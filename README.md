<p align="center">
  <img src="assets/logo.png" width="128" alt="Kivuko icon">
</p>

<h1 align="center">Kivuko</h1>

<p align="center">
  A small, open-source NordVPN client for the Mac menu bar.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-14%2B-1c1c1e?logo=apple" alt="macOS 14 or later">
  <img src="https://img.shields.io/badge/Apple%20silicon-required-1c1c1e" alt="Apple silicon required">
  <img src="https://img.shields.io/badge/WireGuard-GotaTun-1c1c1e" alt="WireGuard through GotaTun">
  <a href="LICENSE.md"><img src="https://img.shields.io/badge/license-PolyForm%20Noncommercial-1c1c1e" alt="License: PolyForm Noncommercial 1.0.0"></a>
</p>

<p align="center">
  <img src="docs/images/showcase.png" width="880" alt="Kivuko's menu while connected, with Location, Kill Switch, Auto-Connect and Block Ads and Trackers">
</p>

Kivuko connects your Mac to NordVPN over WireGuard (NordLynx) from a single menu in the menu bar. It has a kill switch that macOS itself enforces, reconnects on its own after sleep and network changes, and can block ads and trackers through NordVPN's DNS. The menu bar app uses about 26 MB of memory and the tunnel about 4 MB.

It's written in Rust. The tunnel runs [GotaTun](https://github.com/mullvad/gotatun), Mullvad's audited Rust implementation of WireGuard, inside a macOS packet-tunnel extension, so the VPN keeps working when the app isn't running.

<p align="center"><sub><em>Kivuko</em> (kee-VOO-koh) is Swahili for a crossing, or the ferry that carries you over.</sub></p>

> [!NOTE]
> Kivuko is an independent project. It is not made, endorsed or supported by Nord Security. You need your own NordVPN subscription.

## Contents

- [Features](#features)
- [Requirements](#requirements)
- [Install](#install)
- [How it works](#how-it-works)
- [Privacy and security](#privacy-and-security)
- [Resource use](#resource-use)
- [Development](#development)
- [License](#license)

## Features

- **Connect and Disconnect** sit at the top of the menu. Quitting Kivuko turns the VPN off too.
- **Location** lists every country NordVPN serves, with your recent choices first. **Fastest Server** uses NordVPN's recommendation for where you are.
- **Kill Switch.** While the VPN is on or reconnecting, macOS blocks traffic that would leave outside the tunnel. Your local network keeps working.
- **Auto-Connect** reconnects on any network, after sleep and when Kivuko starts, until you click Disconnect.
- **Block Ads and Trackers** uses NordVPN's Threat Protection Lite DNS. Turn it off to use NordVPN's plain DNS.
- **Recovery.** If a server doesn't answer within 8 seconds, Kivuko tries the next one. If the connection goes quiet, it renews the WireGuard handshake, and if that fails, macOS restarts the tunnel on a new server.
- **No IPv6 leaks.** IPv6 traffic goes into the tunnel too, where NordVPN drops it, instead of slipping around it.

## Requirements

- A Mac with Apple silicon and macOS 14 or later (tested on macOS 26)
- A NordVPN subscription
- A paid [Apple Developer Program](https://developer.apple.com/programs/) membership. macOS only loads VPN extensions signed through a paid account, so Kivuko can't be distributed as a ready-made download; you build and sign it yourself.
- Xcode signed in with that account, [XcodeGen](https://github.com/yonaskolb/XcodeGen) and Rust 1.95 or later

## Install

The full guide is [docs/setup.md](docs/setup.md). In short:

1. Clone the repository and pick a bundle ID prefix of your own, such as `com.yourname`:

   ```bash
   git clone https://github.com/piedol/kivuko.git
   ```

   ```bash
   cd kivuko && cp signing/local.env.example signing/local.env
   ```

   Set `KIVUKO_BUNDLE_PREFIX` in `signing/local.env`, and `KIVUKO_TEAM` if you belong to more than one developer team. `scripts/bundle.sh --check` shows what will be signed.

2. Build, sign and install. On the first run, Xcode registers your Mac and the two bundle IDs and downloads development profiles:

   ```bash
   scripts/bundle.sh --install
   ```

3. Get a NordVPN access token from [Nord Account](https://my.nordaccount.com/) › NordVPN › Advanced settings › Access token.
4. Click the shield in the menu bar, choose **Set Up and Connect**, paste the token, and click **Allow** when macOS asks to add a VPN configuration.

Development profiles last a year. Run `scripts/bundle.sh --install` again before they expire. Keep only one copy of Kivuko on your Mac: when two copies of the tunnel extension are registered, opening one makes macOS stop the other's tunnel, which drops the VPN.

## How it works

```text
Kivuko.app                              app/       menu bar app: menu, token prompt, VPN configuration
└── Contents/PlugIns/
    └── KivukoTunnel.appex              provider/  packet-tunnel extension: server choice, GotaTun, watchdog
```

- The menu bar app manages the VPN configuration macOS keeps in System Settings › VPN, including the kill switch, the On-Demand rule and the DNS choice.
- The tunnel is a NetworkExtension packet-tunnel provider. macOS starts it whenever the VPN starts, including automatic reconnects while the app isn't running.
- On each start, the tunnel asks NordVPN for recommended WireGuard servers, gives macOS the addresses, routes and DNS, and runs GotaTun on the tunnel interface macOS creates.

[docs/architecture.md](docs/architecture.md) explains the design decisions, the connection sequence and the watchdog.

## Privacy and security

- Your NordVPN access token is stored in your login keychain. When you click Connect, the app hands it to the tunnel, which uses it to fetch your WireGuard key from NordVPN.
- The tunnel keeps the WireGuard key in its own sandbox container, readable only by your user (mode 0600).
- macOS's VPN configuration is readable by every user on the Mac, so Kivuko stores nothing secret there: only your country and DNS choice.
- The logs record events, never the token or the key.
- Kivuko talks only to NordVPN's API and the VPN server you're connected to. There's no analytics or telemetry.

[docs/setup.md](docs/setup.md#what-kivuko-stores) lists every file Kivuko writes and how to remove them.

## Resource use

Measured on a MacBook Pro with macOS 26:

| | Memory | CPU |
|---|---|---|
| Menu bar app | 26 MB | 0% when idle |
| Tunnel | 4 MB | Under 1% with background traffic; about a quarter of one core at 200 Mbit/s and above |

The measured download speed through a NordVPN server was 212 Mbit/s.

## Development

| Path | What it does |
|---|---|
| `app/` | The menu, Connect and Disconnect, Location, toggles, the token prompt and the VPN configuration |
| `provider/` | The tunnel: server choice, network settings, GotaTun on the utun interface, the watchdog |
| `nord/` | The NordVPN API client: server recommendations, countries and the NordLynx key |
| `shared/` | Bundle IDs, settings and the messages between the app and the tunnel |
| `signing/` | A stub Xcode project that exists only to get development signing |
| `scripts/bundle.sh` | Build, sign and install |

```bash
cargo test                            # unit tests
cargo test -p nord -- --ignored       # live checks against NordVPN's API
cargo clippy --workspace --all-targets -- -D warnings
```

## License

[PolyForm Noncommercial 1.0.0](LICENSE.md). You may use, change and share Kivuko for any noncommercial purpose.

WireGuard is a registered trademark of Jason A. Donenfeld. NordVPN and NordLynx are trademarks of Nord Security, which is not affiliated with this project.
