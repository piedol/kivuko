//! What the menu-bar app and the tunnel extension agree on: identifiers, the settings the app
//! hands the tunnel, and the messages they exchange.

use serde::{Deserialize, Serialize};

/// Bundle identifiers and names. A rename starts here and in scripts/bundle.sh.
/// Kivuko is Swahili for a crossing or ferry.
pub mod ids {
    pub const TEAM: &str = "ABCDE12345";
    pub const APP_NAME: &str = "Kivuko";
    pub const APP_BUNDLE: &str = "top.piedol.kivuko";
    pub const PROVIDER_BUNDLE: &str = "top.piedol.kivuko.tunnel";
    /// Unified-log subsystem for both processes.
    pub const LOG_SUBSYSTEM: &str = "top.piedol.kivuko";
    /// The Nord access token's item in the login keychain. The app creates it, so macOS
    /// never asks whether the app may read it. The tunnel never reads it: the app hands the
    /// token over when you click Connect. (On macOS, VPN apps and extensions are pointed
    /// at the system keychain for data-protection items, so a shared keychain group fails.)
    pub const KEYCHAIN_SERVICE: &str = "Kivuko";
    pub const KEYCHAIN_ACCOUNT: &str = "NordVPN token";
    /// The description macOS shows in System Settings › VPN.
    pub const VPN_DESCRIPTION: &str = "Kivuko";
}

/// Tunnel addressing. NordLynx hands every client 10.5.0.2; IPv6 gets a private address so
/// IPv6 traffic enters the tunnel (where Nord drops it) instead of leaking around it.
pub mod net {
    pub const IPV4_ADDRESS: &str = "10.5.0.2";
    pub const IPV6_ADDRESS: &str = "fdc6:9a1d:4e7b::2";
    pub const MTU: u16 = 1420;
    pub const KEEPALIVE_SECS: u16 = 25;
}

/// Key names in the dictionaries macOS stores and passes for us.
pub mod keys {
    /// `providerConfiguration["settings"]`: JSON-encoded [`super::Settings`].
    pub const SETTINGS: &str = "settings";
    /// `startVPNTunnel(options:)["token"]`: the Nord access token. The tunnel uses it to
    /// fetch the WireGuard key itself, since its own traffic passes the kill switch.
    pub const TOKEN: &str = "token";
}

/// Which DNS servers the tunnel hands to macOS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Dns {
    /// Nord's Threat Protection Lite: blocks ads, trackers and known malware domains.
    #[default]
    BlockAds,
    /// Nord's plain resolvers.
    Plain,
}

impl Dns {
    pub fn servers(self) -> [&'static str; 2] {
        match self {
            Dns::BlockAds => ["103.86.96.108", "103.86.99.108"],
            Dns::Plain => ["103.86.96.100", "103.86.99.100"],
        }
    }
}

/// Non-secret settings, stored in the VPN configuration so the tunnel can read them when
/// macOS starts it on its own (On-Demand).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Connect within this country (Nord country id). None = the fastest server anywhere.
    pub country_id: Option<u32>,
    /// Shown in the menu while the tunnel is starting.
    pub country_name: Option<String>,
    pub dns: Dns,
}

/// App → tunnel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Request {
    Status,
    /// Replace the stored private key (after the user changes their token).
    SetKey { key: String },
}

/// Extension → app.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    /// "United States #8348"
    pub server: Option<String>,
    pub hostname: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub city: Option<String>,
    /// Seconds since the Unix epoch when traffic first flowed.
    pub connected_since: Option<u64>,
    pub last_handshake_secs: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// The last thing that went wrong, in words for the menu.
    pub problem: Option<String>,
}

/// Start errors that won't fix themselves by retrying (the app turns Auto-Connect off).
/// The tunnel's messages for these mention one of the two phrases below.
pub fn is_setup_error(message: &str) -> bool {
    message.contains("access token") || message.contains("WireGuard key")
}

pub fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Option<T> {
    serde_json::from_slice(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let r = Request::SetKey { key: "k".into() };
        assert_eq!(decode::<Request>(&encode(&r)), Some(r));
        assert_eq!(decode::<Request>(br#"{"type":"status"}"#), Some(Request::Status));
    }

    #[test]
    fn settings_fill_missing_fields() {
        let s: Settings = decode(b"{}").unwrap();
        assert_eq!(s.dns, Dns::BlockAds);
        assert_eq!(s.country_id, None);
        let s: Settings = decode(br#"{"dns":"plain","country_id":228}"#).unwrap();
        assert_eq!((s.dns, s.country_id), (Dns::Plain, Some(228)));
    }
}
