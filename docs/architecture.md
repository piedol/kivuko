# How Kivuko works

Kivuko.app is a menu bar app, and its bundle contains a NetworkExtension packet-tunnel provider that runs WireGuard. The app and the tunnel are written in Rust and use [objc2](https://github.com/madsmtm/objc2) for the Apple frameworks. The shipped app contains no Swift or Objective-C. The Swift in `signing/` is a stub for getting provisioning profiles from Xcode.

The tunnel is packaged in two ways. The development build, signed for your own Macs, has it as an app extension. The Developer ID release has it as a system extension.

```text
Development build: scripts/bundle.sh
Kivuko.app                                         app/       menu bar, token prompt, VPN configuration
└── Contents/PlugIns/
    └── KivukoTunnel.appex                         provider/  NEPacketTunnelProvider + GotaTun (WireGuard)

Release build: scripts/bundle.sh --release
Kivuko.app                                         app/       the same, plus the system extension's activation
└── Contents/Library/SystemExtensions/
    └── top.piedol.kivuko.tunnel.systemextension   provider/  the same tunnel, built with the sysext feature
```

| Crate | What it does |
|---|---|
| `app/` | Menu, Connect and Disconnect, Location, toggles, token prompt, the VPN configuration macOS stores |
| `provider/` | The tunnel: server choice, network settings, GotaTun on the utun, watchdog |
| `nord/` | NordVPN API client: server recommendations, countries, the NordLynx key |
| `shared/` | Bundle IDs, settings and the messages between the app and the tunnel |

## The two processes

The menu-bar app owns the VPN configuration that macOS keeps (System Settings › VPN): the kill switch, the On-Demand rule, and the country and DNS choice. It starts and stops the tunnel and asks it for status.

The tunnel is an `NEPacketTunnelProvider` subclass defined in Rust with `define_class!`. macOS launches it, sandboxed, whenever the VPN starts, including On-Demand starts when the app isn't running. The binary's `main` registers the class. In the development build it then hands over to `NSExtensionMain`. In the release build it calls `NEProvider.startSystemExtensionMode()` and then `dispatch_main()`, and macOS runs it as root.

They talk in two ways:

- `providerConfiguration["settings"]` in the VPN configuration holds the non-secret settings as JSON (`shared::Settings`), so the tunnel can read them on any start.
- `sendProviderMessage` carries JSON requests (`shared::Request`) and status replies (`shared::Status`: server, city, handshake age, bytes in and out, the last problem).

## The system extension

In the release build (the cargo feature `sysext` on `app` and `provider`), the app asks macOS to activate the tunnel before the VPN can start:

1. At launch, the app submits an `OSSystemExtensionRequest` activation request for the tunnel's bundle ID. If that version is already active, the request completes at once.
2. The first time, macOS waits for the user to allow the tunnel. Until then Connect is disabled and the menu shows **Allow Kivuko's VPN in System Settings…**, which opens General › Login Items & Extensions (Privacy & Security on macOS 14). The request completes when the user allows it.
3. If a different version is active, as after an update, the request's delegate tells macOS to replace it.
4. macOS activates system extensions for apps in /Applications and nowhere else. From another folder, the menu shows **Move Kivuko to Applications**.
5. The first time a release build finds a VPN configuration, it saves the configuration again, so it points at the system extension. The tunnel has the same bundle ID in both builds, so the settings don't change. The app records this as `saved_for_system_extension` in its settings file.

A Connect that comes before the tunnel is active, such as Auto-Connect at launch, waits and runs when activation completes.

## A connection, step by step

1. You click Connect. The app saves the VPN configuration with On-Demand off, then starts the tunnel with your access token in the `startVPNTunnel` options.
2. The tunnel fetches your NordLynx private key from Nord with the token and saves it in its sandbox container.
3. It asks Nord for 5 recommended WireGuard servers, filtered by country if you chose one, and saves the list.
4. For up to 3 of them, it hands macOS the tunnel settings (addresses, default routes, DNS, MTU), finds the utun file descriptor macOS created, runs GotaTun on it with that server as the only peer, and waits up to 8 seconds for a handshake. The first server that answers wins.
5. When macOS reports the tunnel as connected, the app turns On-Demand on, so macOS restarts the tunnel after sleep or a network change.

On-Demand starts carry no token, so the tunnel uses the key it saved. If Nord's API can't be reached, it falls back to the server list it saved last time.

While connected, a watchdog checks the handshake every 15 seconds. With a 25-second keepalive, WireGuard renews the handshake about every 2 minutes. After 3 minutes without a handshake, the tunnel forces a new handshake and macOS shows "Reconnecting". After 6 minutes, it cancels itself with an error and On-Demand restarts it on a freshly chosen server. On wake from sleep it forces a handshake straight away.

## Decisions

| Topic | Decision | Why |
|---|---|---|
| WireGuard engine | [GotaTun](https://github.com/mullvad/gotatun) 0.10 (`default-features = false`, features `device` and `ring`) | Mullvad's Rust fork of BoringTun |
| Tunnel device | Kivuko's own `IpSend`/`IpRecv` adapter on the utun file descriptor (`provider/src/utun.rs`), using tokio's `AsyncFd` and adding or stripping macOS's 4-byte protocol header | The tun crate can't read the MTU of a borrowed descriptor, so its `TunDevice` fails |
| Finding the utun | Scan the open descriptors for the socket whose control ID is `com.apple.net.utun_control` | The same method as [wireguard-apple](https://github.com/WireGuard/wireguard-apple) |
| Tunnel packaging | An app extension in the development build, a system extension in the release build | macOS runs a Network Extension app extension only in App Store apps and development-signed builds, so a Developer ID download needs a system extension. The development build keeps the app extension, which needs no Developer ID certificate or notarization and runs from any folder without an approval in System Settings |
| System extension updates | The activation delegate replaces an active tunnel whose version differs (`CFBundleShortVersionString` or `CFBundleVersion`) and keeps an active tunnel of the same version | The build number is the commit count, so a build from a later commit replaces the tunnel |
| Kill switch | `includeAllNetworks = true`, `excludeLocalNetworks = true` | macOS enforces it, and the local network keeps working |
| Auto-Connect | On-Demand with an `NEOnDemandRuleConnect` rule for any interface, switched on only after a successful connection | The first start must carry the token |
| Setup errors | A rejected token or a missing key turns On-Demand off and shows the problem in the menu | Retrying can't fix these, and with the kill switch on a retry loop would keep blocking the internet |
| Disconnect | Turns On-Demand off until the next Connect | Otherwise On-Demand reconnects at once |
| Quit | Turns the VPN off, then quits. Auto-Connect connects again when Kivuko next starts | Quitting the VPN app should leave the VPN off |
| DNS | Threat Protection Lite, 103.86.96.108 and 103.86.99.108 (default), or Nord's standard resolvers, 103.86.96.100 and 103.86.99.100. Both use `matchDomains = [""]` | The empty match domain sends every DNS query to these servers |
| Addresses | IPv4 10.5.0.2/32 and an IPv6 ULA /128, with default routes for both | NordLynx gives every client 10.5.0.2 and no IPv6, so IPv6 goes into the tunnel, where Nord drops it, instead of leaking around it |
| MTU | 1420 | WireGuard's default |
| Server choice | In the tunnel, at every start | On-Demand starts happen without the app |
| Logging | Unified log, subsystem = the app's bundle ID | `log stream --predicate 'subsystem == "<prefix>.kivuko"'` |

## Security model

- The access token is a login-keychain item (service `Kivuko`, account `NordVPN token`) that the app creates, so macOS never asks whether the app may read it. The tunnel never reads the keychain. macOS points VPN processes at the system keychain for data-protection items, so a shared keychain group doesn't work. The app passes the token in the start options when you click Connect.
- The tunnel fetches the WireGuard key because its traffic is allowed past the kill switch while the tunnel is starting, and the app's traffic isn't.
- The key and the server list are stored in the tunnel's sandbox container (`Library/Application Support/state.json`), written with mode 0600 through a temporary file and a rename. The release build's tunnel runs as root, so its container is under `/var/root/Library/Containers/`, and after a switch from the development build it fetches the key again on the first Connect.
- Nothing secret goes in the VPN configuration. macOS stores it in `/Library/Preferences/com.apple.networkextension.plist`, which every user can read, so it holds only the country and DNS choice.
- The logs record events, such as which server answered and why a start failed, never the token or the key.
- Both bundles are signed with the hardened runtime. Both have the `com.apple.developer.networking.networkextension` entitlement with `packet-tunnel-provider`, plus `application-identifier` and `team-identifier`. The tunnel also has the App Sandbox with network client and server. The app isn't sandboxed.
- In the release build, the Network Extension value is `packet-tunnel-provider-systemextension`, and the app also has `com.apple.developer.system-extension.install`. Both bundles have the app group `<team>.<prefix>.kivuko`, because the tunnel's Mach service name, `<team>.<prefix>.kivuko.tunnel`, has to start with an app group they share.

## The NordVPN API

Nord doesn't document the API a WireGuard client needs, so `nord/` parses leniently and ignores fields it doesn't use.

| Call | Use |
|---|---|
| `GET https://api.nordvpn.com/v1/servers/recommendations?filters[servers_technologies][identifier]=wireguard_udp&limit=5`, plus `filters[country_id]=<id>` for a country | Servers to try. Each server's WireGuard public key is in `technologies[wireguard_udp].metadata[name=public_key]`. The endpoint is the server's `station` address, port 51820 |
| `GET https://api.nordvpn.com/v1/servers/countries` | The Location menu, cached for a week |
| `GET https://api.nordvpn.com/v1/users/services/credentials` with Basic auth `token:<access token>` | The account's NordLynx private key, field `nordlynx_private_key` |

`cargo test -p nord -- --ignored` runs the live checks against the API.

## Signing and the build

### The development build

`scripts/bundle.sh` does the whole build:

1. It works out the team (`KIVUKO_TEAM`, or the team of the Apple Development certificates in the login keychain, if they share a team) and the bundle IDs (`<KIVUKO_BUNDLE_PREFIX>.kivuko` and `<KIVUKO_BUNDLE_PREFIX>.kivuko.tunnel`). It passes the IDs to cargo as `KIVUKO_APP_ID` and `KIVUKO_EXT_ID`, which `shared/src/lib.rs` reads with `option_env!`, and to XcodeGen, which fills `${...}` in `signing/project.yml` from the environment.
2. It looks for unexpired development profiles for both bundle IDs in `~/Library/Developer/Xcode/UserData/Provisioning Profiles`. If either is missing, it generates the stub project in `signing/` and builds it with `-allowProvisioningUpdates -allowProvisioningDeviceRegistration`, so Xcode's automatic signing registers the Mac and the IDs and downloads the profiles. It then unregisters and deletes the stub app.
3. It signs with the Apple Development certificate the app's profile was issued for.
4. It builds both binaries in release mode for `aarch64-apple-darwin`, fills `resources/*.plist` and `resources/*.entitlements` with the names and IDs, embeds the profiles, and signs the extension, then the app.

macOS must not have two copies of the development build registered. Their tunnel extensions share a bundle ID, and opening either copy makes macOS remove one of the two extensions and stop every running instance of it, which drops the VPN. That is why the script unregisters the stub app and the `dist/` copy. It also clears the `com.apple.macl` attribute that a copied bundle can carry, because with that attribute macOS re-registers the app on every launch, which restarts the tunnel.

### The release build

`scripts/bundle.sh --release` builds the Developer ID release:

1. It works out the team and bundle IDs as above.
2. It looks for unexpired Developer ID profiles for both bundle IDs that grant `packet-tunnel-provider-systemextension`, and for the app also `com.apple.developer.system-extension.install`. If either is missing, it generates the stub project and archives its `KivukoRelease` scheme, a stub app with a system extension. Archiving signs the stub for development, so Xcode adds the System Extension capability to the app's bundle ID. Exporting the archive with `signing/Release/ExportOptions.plist` (method `developer-id`, automatic signing) and `-allowProvisioningUpdates` makes Xcode create the Developer ID profiles and download them. The script then unregisters and deletes the stub apps.
3. It signs with the Developer ID Application certificate in the login keychain that the profiles list.
4. It builds both binaries with the `sysext` feature and `--remap-path-prefix`, which keeps the build machine's paths out of the binaries. It fills `resources/Provider-Release-Info.plist` and the two `*-Release.entitlements`, embeds the profiles, and signs the system extension, then the app, with the hardened runtime and a secure timestamp.
5. It notarizes the app with `xcrun notarytool submit` and the keychain profile in `KIVUKO_NOTARY_PROFILE` (default `piedol-notary`), staples the ticket, checks it with `stapler validate` and `spctl`, and zips the app as `dist/Kivuko-macOS-arm64.zip`. It doesn't install or open the app.

When Xcode adds the System Extension capability, it also replaces the app's development profile with a new profile that lists the System Extension entitlement. Development builds don't use that entitlement.

The release build's tunnel isn't registered from the app bundle. macOS copies it to `/Library/SystemExtensions` when the app activates it from /Applications. The script still unregisters the `dist/` copy of the app, so macOS doesn't see two apps with the same bundle ID.

## Verified

On a MacBook Pro with macOS 26, on 24 October 2026:

| Check | Result |
|---|---|
| Connect | Fetched the key, picked a nearby server from Nord's recommendations and completed the handshake in 0.2 s |
| Exit IP | In the chosen Nord server's range |
| DNS | Only 103.86.96.108 and 103.86.99.108, over the utun. The googleadservices, pagead2 and securepubads domains were blocked |
| IPv6 | No route out, so no leak |
| Kill switch | A request forced out of the Wi-Fi interface (en0) still left through the tunnel |
| Throughput | 212 Mbit/s download, with the tunnel using about 26% of a CPU core |
| Memory | App 26 MB, tunnel 4.2 MB |
| Idle CPU | App 0%, tunnel about 0.4% with background traffic |
