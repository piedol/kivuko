# How Kivuko works

Kivuko is two Rust programs in one app bundle: a menu-bar app, and a NetworkExtension packet-tunnel provider that runs WireGuard. Both use [objc2](https://github.com/madsmtm/objc2) for the Apple frameworks. There is no Swift or Objective-C in the shipped app; the Swift in `signing/` is a stub that only exists to get provisioning profiles from Xcode.

```text
Kivuko.app                              app/       menu bar, token prompt, VPN configuration
└── Contents/PlugIns/
    └── KivukoTunnel.appex              provider/  NEPacketTunnelProvider + GotaTun (WireGuard)
```

| Crate | What it does |
|---|---|
| `app/` | Menu, Connect and Disconnect, Location, toggles, token prompt, the VPN configuration macOS stores |
| `provider/` | The tunnel: server choice, network settings, GotaTun on the utun, watchdog |
| `nord/` | NordVPN API client: server recommendations, countries, the NordLynx key |
| `shared/` | Bundle IDs, settings and the messages between the app and the tunnel |

## The two processes

The menu-bar app owns the VPN configuration that macOS keeps (System Settings › VPN): the kill switch, the On-Demand rule, and the country and DNS choice. It starts and stops the tunnel and asks it for status.

The tunnel is an `NEPacketTunnelProvider` subclass defined in Rust with `define_class!`. macOS launches it, sandboxed, whenever the VPN starts, including On-Demand starts when the app isn't running. The binary's `main` registers the class and hands over to `NSExtensionMain`.

They talk in two ways:

- `providerConfiguration["settings"]` in the VPN configuration holds the non-secret settings as JSON (`shared::Settings`), so the tunnel can read them on any start.
- `sendProviderMessage` carries JSON requests (`shared::Request`) and status replies (`shared::Status`: server, city, handshake age, bytes in and out, the last problem).

## A connection, step by step

1. You click Connect. The app saves the VPN configuration with On-Demand off, then starts the tunnel with your access token in the `startVPNTunnel` options.
2. The tunnel fetches your NordLynx private key from Nord with the token and saves it in its sandbox container.
3. It asks Nord for 5 recommended WireGuard servers, filtered by country if you chose one, and saves the list.
4. For up to 3 of them, it hands macOS the tunnel settings (addresses, default routes, DNS, MTU), finds the utun file descriptor macOS created, runs GotaTun on it with that server as the only peer, and waits up to 8 seconds for a handshake. The first server that answers wins.
5. When macOS reports the tunnel as connected, the app turns On-Demand on, so macOS restarts the tunnel on its own after sleep or a network change.

On-Demand starts carry no token, so the tunnel uses the key it saved. If Nord's API can't be reached, it falls back to the server list it saved last time.

While connected, a watchdog checks the handshake every 15 seconds. With a 25-second keepalive, WireGuard renews the handshake about every 2 minutes. After 3 minutes without one, the tunnel forces a new handshake and macOS shows "Reconnecting". After 6 minutes, it cancels itself with an error and On-Demand restarts it on a freshly chosen server. On wake from sleep it forces a handshake straight away.

## Decisions

| Topic | Decision | Why |
|---|---|---|
| WireGuard engine | [GotaTun](https://github.com/mullvad/gotatun) 0.10 (`default-features = false`, features `device` and `ring`) | Mullvad's audited Rust fork of BoringTun |
| Tunnel device | Kivuko's own `IpSend`/`IpRecv` adapter on the utun file descriptor (`provider/src/utun.rs`), using tokio's `AsyncFd` and adding or stripping macOS's 4-byte protocol header | The tun crate can't read the MTU of a borrowed descriptor, so its `TunDevice` fails |
| Finding the utun | Scan the open descriptors for the socket whose control ID is `com.apple.net.utun_control` | The same method as [wireguard-apple](https://github.com/WireGuard/wireguard-apple) |
| App extension, not system extension | A packet-tunnel app extension inside the app | A development-signed app extension needs no notarization or Developer ID profile; a system extension does |
| Kill switch | `includeAllNetworks = true`, `excludeLocalNetworks = true` | macOS enforces it, and the local network keeps working |
| Auto-Connect | On-Demand with one `NEOnDemandRuleConnect` for any interface, switched on only after a successful connection | The first start must carry the token |
| Setup errors | A rejected token or a missing key turns On-Demand off and shows the problem in the menu | Retrying can't fix these, and with the kill switch on a retry loop would keep blocking the internet |
| Disconnect | Turns On-Demand off until the next Connect | Otherwise On-Demand reconnects at once |
| Quit | Turns the VPN off, then quits. Auto-Connect connects again when Kivuko next starts | Quitting the VPN app should leave the VPN off |
| DNS | Threat Protection Lite, 103.86.96.108 and 103.86.99.108 (default), or Nord's plain resolvers, 103.86.96.100 and 103.86.99.100; `matchDomains = [""]` | The empty match domain sends every DNS query to these servers |
| Addresses | IPv4 10.5.0.2/32 and an IPv6 ULA /128, with default routes for both | NordLynx gives every client 10.5.0.2 and no IPv6, so IPv6 goes into the tunnel, where Nord drops it, instead of leaking around it |
| MTU | 1420 | WireGuard's default |
| Server choice | In the tunnel, at every start | On-Demand starts happen without the app |
| Logging | Unified log, subsystem = the app's bundle ID | `log stream --predicate 'subsystem == "<prefix>.kivuko"'` |

## Security model

- The access token is a login-keychain item (service `Kivuko`, account `NordVPN token`) that the app creates, so macOS never asks whether the app may read it. The tunnel never reads the keychain: macOS points VPN processes at the system keychain for data-protection items, so a shared keychain group doesn't work. Instead the app passes the token in the start options when you click Connect.
- The tunnel fetches the WireGuard key itself because its own traffic is allowed past the kill switch while the tunnel is starting, and the app's isn't.
- The key and the server list live in the tunnel's sandbox container (`Library/Application Support/state.json`), written with mode 0600 through a temporary file and a rename.
- Nothing secret goes in the VPN configuration. macOS stores it in `/Library/Preferences/com.apple.networkextension.plist`, which every user can read, so it holds only the country and DNS choice.
- The logs record events, such as which server answered and why a start failed, never the token or the key.
- Both bundles are signed with the hardened runtime. Both have the `com.apple.developer.networking.networkextension` entitlement with `packet-tunnel-provider`, plus `application-identifier` and `team-identifier`. The tunnel also has the App Sandbox with network client and server. The app isn't sandboxed.

## The NordVPN API

Nord doesn't document the API a WireGuard client needs, so `nord/` parses leniently and ignores every field it doesn't use.

| Call | Use |
|---|---|
| `GET https://api.nordvpn.com/v1/servers/recommendations?filters[servers_technologies][identifier]=wireguard_udp&limit=5`, plus `filters[country_id]=<id>` for a country | Servers to try. Each server's WireGuard public key is in `technologies[wireguard_udp].metadata[name=public_key]`; the endpoint is the server's `station` address, port 51820 |
| `GET https://api.nordvpn.com/v1/servers/countries` | The Location menu, cached for a week |
| `GET https://api.nordvpn.com/v1/users/services/credentials` with Basic auth `token:<access token>` | The account's NordLynx private key, field `nordlynx_private_key` |

`cargo test -p nord -- --ignored` runs the live checks against the API.

## Signing and the build

`scripts/bundle.sh` does the whole build:

1. It works out the team (`KIVUKO_TEAM`, or the team of the one Apple Development certificate in the login keychain) and the bundle IDs (`<KIVUKO_BUNDLE_PREFIX>.kivuko` and `<KIVUKO_BUNDLE_PREFIX>.kivuko.tunnel`). It passes the IDs to cargo as `KIVUKO_APP_ID` and `KIVUKO_EXT_ID`, which `shared/src/lib.rs` reads with `option_env!`, and to XcodeGen, which fills `${...}` in `signing/project.yml` from the environment.
2. It looks for unexpired development profiles for both bundle IDs in `~/Library/Developer/Xcode/UserData/Provisioning Profiles`. If either is missing, it generates the stub project in `signing/` and builds it with `-allowProvisioningUpdates -allowProvisioningDeviceRegistration`, so Xcode's automatic signing registers the Mac and the IDs and downloads the profiles. It then unregisters and deletes the stub app.
3. It signs with the Apple Development certificate the app's profile was issued for.
4. It builds both binaries in release mode for `aarch64-apple-darwin`, fills `resources/*.plist` and `resources/*.entitlements` with the names and IDs, embeds the profiles, and signs the extension, then the app.

Only one copy of the app may stay registered with macOS. With two copies of the tunnel extension (the same bundle ID), opening either makes macOS remove one extension and stop every running instance of it, which drops the VPN. That is why the script unregisters the stub app and the `dist/` copy, and clears the `com.apple.macl` attribute that a copied bundle can carry: with it, macOS re-registers the app on every launch, which restarts the tunnel.

## Verified

On a MacBook Pro with macOS 26, on 24 October 2026:

| Check | Result |
|---|---|
| Connect | Fetched the key, picked a nearby server from Nord's recommendations and completed the handshake in 0.2 s |
| Exit IP | In the chosen Nord server's range |
| DNS | Only 103.86.96.108 and 103.86.99.108, over the utun; googleadservices, pagead2 and securepubads were blocked |
| IPv6 | No route out, so no leak |
| Kill switch | A request forced out of the Wi-Fi interface (en0) still left through the tunnel |
| Throughput | 212 Mbit/s download; the tunnel used about 26% of one core while it ran |
| Memory | App 26 MB, tunnel 4.2 MB |
| Idle CPU | App 0%; tunnel about 0.4% with background traffic |
