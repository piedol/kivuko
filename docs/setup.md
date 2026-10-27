# Building and running Kivuko

This guide builds Kivuko from source and signs it with your Apple developer account. To use Kivuko, download the notarized release instead (README › Install). The development build's VPN part is a NetworkExtension app extension, and macOS loads it only when it is signed with a provisioning profile from a paid developer account.

## What you need

- An Apple silicon Mac with macOS 14 or later. Kivuko is tested on macOS 26.
- A paid [Apple Developer Program](https://developer.apple.com/programs/) membership, 99 USD a year. A free Apple Account isn't enough. Apple's [table of supported macOS capabilities](https://developer.apple.com/help/account/reference/supported-capabilities-macos) lists Network Extensions only for Apple Developer Program members and Developer ID.
- Xcode, signed in with that account (Xcode › Settings › Accounts). The Command Line Tools alone aren't enough, because Xcode's automatic signing creates the certificate and profiles.
- [XcodeGen](https://github.com/yonaskolb/XcodeGen): `brew install xcodegen`.
- Rust 1.95 or later, from [rustup](https://rustup.rs). The WireGuard library, GotaTun, needs 1.95.
- A NordVPN subscription.

## 1. Choose your team and bundle IDs

Bundle IDs are unique across all Apple developer accounts. Pick a prefix that's yours, such as `com.yourname`. The app becomes `com.yourname.kivuko` and the tunnel `com.yourname.kivuko.tunnel`. The default prefix, `top.piedol`, is the project's and signs only for its owner.

`scripts/bundle.sh` reads two settings:

| Variable | Default |
|---|---|
| `KIVUKO_BUNDLE_PREFIX` | `top.piedol` |
| `KIVUKO_TEAM` | The team of the Apple Development certificate in your login keychain |

Your team ID is 10 characters, like `ABCDE12345`, and is listed under Membership details at [developer.apple.com/account](https://developer.apple.com/account). The script finds it in the certificate's subject (its Organizational Unit). The code in parentheses after your name in the certificate's name, as in "Apple Development: Your Name (XXXXXXXXXX)", is not the team ID. If you've never built a Mac app with this account, you don't have the certificate yet, so set `KIVUKO_TEAM` and Xcode creates the certificate on the first build. If you have certificates for different teams, the script stops and asks you to set `KIVUKO_TEAM`.

To set them once, copy the example file and edit it. `signing/local.env` is ignored by git, and values already set in the environment win over it.

```bash
cp signing/local.env.example signing/local.env
```

```bash
KIVUKO_TEAM=ABCDE12345
KIVUKO_BUNDLE_PREFIX=com.yourname
```

Check what will be signed:

```bash
scripts/bundle.sh --check
```

This prints the team and both bundle IDs and writes the filled-in Info.plists and entitlements to `dist/check/`. It doesn't build, sign, run Xcode or contact Apple.

## 2. Build and install

```bash
scripts/bundle.sh --install
```

The first run asks Xcode for development signing by building a stub project in `signing/` (`xcodegen`, then `xcodebuild -allowProvisioningUpdates -allowProvisioningDeviceRegistration`). Xcode then:

- creates an Apple Development certificate if you don't have one;
- registers this Mac and the two bundle IDs in your account, with the Network Extensions capability;
- downloads a development provisioning profile for each bundle ID.

The script then builds the app and the tunnel with cargo, assembles `dist/Kivuko.app` with the tunnel at `Contents/PlugIns/KivukoTunnel.appex`, embeds the profiles and signs both with your certificate. With `--install` it replaces `/Applications/Kivuko.app`, removes the copy in `dist/` and opens the app. Without `--install` it stops at `dist/Kivuko.app`.

Don't keep a second copy of Kivuko on your Mac. Two copies of the tunnel extension share a bundle ID, and when you open either copy, macOS removes the other's extension and stops the running tunnel, which drops the VPN. So don't open `dist/Kivuko.app` while another copy is installed.

The build isn't notarized. It doesn't need to be, because it's built on this Mac and macOS never quarantines it. The profiles cover only the Macs registered in your account, so the app won't run on anyone else's.

## 3. Get a NordVPN access token

1. Sign in to [Nord Account](https://my.nordaccount.com/).
2. Open NordVPN › Advanced settings › Access token, and click Get Access token. Nord emails you a code first.
3. Copy the token.

The username and password under manual setup are for OpenVPN. Kivuko doesn't use them.

## 4. Connect

1. Click the shield in the menu bar, then Set Up and Connect.
2. Paste the token and click Save and Connect. Kivuko stores it in your login keychain.
3. macOS asks to add a VPN configuration. Click Allow.

The tunnel fetches your WireGuard key from Nord, picks a server and connects. Once it's connected, Kivuko turns on Auto-Connect, which reconnects on any network and after sleep until you click Disconnect. If Nord later stops accepting the token, the menu shows Replace Your NordVPN Token.

## Every year

Development profiles and Apple Development certificates expire after a year. When the profiles expire, macOS stops loading the app and its tunnel. Run `scripts/bundle.sh --install` again when that happens. The script sees the expired profiles, Xcode renews them (and the certificate, if it has expired), and the script reinstalls the app. Renewal needs an active membership.

To see when the installed profile expires:

```bash
security cms -D -i /Applications/Kivuko.app/Contents/embedded.provisionprofile | plutil -extract ExpirationDate raw -
```

## Logs

The app and the tunnel both log to the unified log under the app's bundle ID. The build script prints the command for your prefix when it finishes. With the default prefix it is:

```bash
log stream --predicate 'subsystem == "top.piedol.kivuko"' --level info
```

Use `log show` with the same predicate and `--last 1h --info` for earlier messages. The logs record events, never the token or the key.

## What Kivuko stores

| What | Where |
|---|---|
| NordVPN access token | Login keychain, service `Kivuko`, account `NordVPN token` |
| WireGuard private key and the last good server list | `~/Library/Containers/<prefix>.kivuko.tunnel/Data/Library/Application Support/state.json`, the tunnel's sandbox container, readable only by you (mode 0600) |
| Kill Switch, Auto-Connect, DNS, location and update-check choices, and the time of the last update check | `~/Library/Application Support/Kivuko/settings.json`, written the first time you change a setting or Kivuko checks for updates |
| Nord's country list | `~/Library/Caches/Kivuko/countries.json`, refreshed weekly |
| An update's download (release build) | `~/Library/Caches/Kivuko/Updates`, removed when the install finishes or fails |
| The VPN configuration | macOS's own store (System Settings › VPN). It holds the country and DNS choice and nothing secret, because every user on the Mac can read it |

## Uninstalling

1. In Kivuko's menu, turn off Launch at Login, then choose Quit Kivuko. Quitting turns the VPN off and stops it reconnecting.
2. In System Settings › VPN, click the info button next to Kivuko and remove the configuration.
3. Move `/Applications/Kivuko.app` to the Trash.
4. To delete the token, search the login keychain in Keychain Access for "Kivuko" and delete the item, or run `security delete-generic-password -s Kivuko -a "NordVPN token"`.
5. Move these folders to the Trash: `~/Library/Application Support/Kivuko`, `~/Library/Caches/Kivuko` and `~/Library/Containers/<prefix>.kivuko.tunnel`.

Quit before you delete the app. If Auto-Connect is still on, macOS keeps trying to start a tunnel that is no longer there, and with the Kill Switch on it blocks traffic while it tries, until you remove the configuration.

## Checks

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test                          # unit tests
cargo test -p nord -- --ignored     # live checks against Nord's API
```

Running `cargo build` or `cargo test` directly uses `com.example.kivuko` placeholders for the bundle IDs. Only `scripts/bundle.sh` passes your own.
