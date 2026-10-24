//! A small client for the parts of the NordVPN API that a NordLynx (WireGuard) client needs:
//! recommended servers, the country list, and the account's NordLynx private key.
//!
//! The endpoints are the ones Nord's own apps and community tools use; none of them is a
//! documented public API, so parsing is lenient and every field we don't need is ignored.

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};

const API: &str = "https://api.nordvpn.com/v1";
/// WireGuard listens on this port on every Nord server.
pub const WIREGUARD_PORT: u16 = 51820;

#[derive(Debug)]
pub enum Error {
    /// No connection, DNS failure, timeout or TLS error.
    Network(String),
    /// The token was rejected.
    Unauthorized,
    /// The API answered with something we couldn't use.
    BadResponse(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Network(e) => write!(f, "couldn't reach NordVPN: {e}"),
            Error::Unauthorized => write!(f, "NordVPN didn't accept the access token"),
            Error::BadResponse(e) => write!(f, "unexpected answer from NordVPN: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// A WireGuard-capable server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Server {
    /// "United States #8348"
    pub name: String,
    /// "us8348.nordvpn.com"
    pub hostname: String,
    /// The server's IPv4 address, used as the WireGuard endpoint.
    pub station: String,
    /// Current load in percent.
    pub load: u8,
    pub country: String,
    pub country_code: String,
    pub country_id: u32,
    pub city: String,
    /// The server's WireGuard public key (base64).
    pub public_key: String,
}

impl Server {
    /// "ip:51820"
    pub fn endpoint(&self) -> Option<std::net::SocketAddr> {
        let ip: std::net::IpAddr = self.station.parse().ok()?;
        Some(std::net::SocketAddr::new(ip, WIREGUARD_PORT))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Country {
    pub id: u32,
    pub name: String,
    /// ISO code, e.g. "US".
    pub code: String,
    pub server_count: u32,
}

/// HTTP client with short timeouts; the tunnel can't wait long for the API.
pub struct Client {
    agent: ureq::Agent,
}

impl Default for Client {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

impl Client {
    pub fn new(timeout: Duration) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .user_agent(concat!("Kivuko/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Client { agent }
    }

    fn get_json(&self, url: &str, auth: Option<&str>) -> Result<serde_json::Value, Error> {
        let mut req = self.agent.get(url);
        if let Some(a) = auth {
            req = req.header("Authorization", a);
        }
        match req.call() {
            Ok(mut resp) => resp.body_mut().read_json().map_err(|e| Error::BadResponse(e.to_string())),
            Err(ureq::Error::StatusCode(401 | 403)) => Err(Error::Unauthorized),
            Err(ureq::Error::StatusCode(code)) => Err(Error::BadResponse(format!("HTTP {code}"))),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// Nord's recommended WireGuard servers, best first. Recommendations depend on where the
    /// request comes from, so call this before the tunnel is up.
    pub fn recommendations(&self, country_id: Option<u32>, limit: u32) -> Result<Vec<Server>, Error> {
        let mut url = format!(
            "{API}/servers/recommendations?filters%5Bservers_technologies%5D%5Bidentifier%5D=wireguard_udp&limit={limit}"
        );
        if let Some(id) = country_id {
            url.push_str(&format!("&filters%5Bcountry_id%5D={id}"));
        }
        let servers = parse_servers(&self.get_json(&url, None)?);
        if servers.is_empty() {
            return Err(Error::BadResponse("no WireGuard servers in the recommendations".into()));
        }
        Ok(servers)
    }

    /// Every country with servers, sorted by name.
    pub fn countries(&self) -> Result<Vec<Country>, Error> {
        let v = self.get_json(&format!("{API}/servers/countries"), None)?;
        let countries = parse_countries(&v);
        if countries.is_empty() {
            return Err(Error::BadResponse("empty country list".into()));
        }
        Ok(countries)
    }

    /// The account's NordLynx private key (base64), fetched with an access token from
    /// Nord Account › Set up NordVPN manually.
    pub fn private_key(&self, token: &str) -> Result<String, Error> {
        let token = token.trim();
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("token:{token}"));
        let v = self.get_json(&format!("{API}/users/services/credentials"), Some(&format!("Basic {basic}")))?;
        let key = v.get("nordlynx_private_key").and_then(|k| k.as_str()).unwrap_or_default();
        if is_wireguard_key(key) {
            Ok(key.to_string())
        } else if v.get("errors").is_some() {
            Err(Error::Unauthorized)
        } else {
            Err(Error::BadResponse("no NordLynx key in the account credentials".into()))
        }
    }
}

/// A WireGuard key is 32 bytes, base64-encoded (44 characters ending in '=').
pub fn is_wireguard_key(s: &str) -> bool {
    s.len() == 44
        && base64::engine::general_purpose::STANDARD
            .decode(s)
            .is_ok_and(|b| b.len() == 32)
}

/// Catches an obviously wrong paste (empty, a few characters, spaces). Nord's API is the
/// real check: a wrong token gets "Unauthorized".
pub fn looks_like_token(s: &str) -> bool {
    let s = s.trim();
    (32..=512).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

pub fn parse_servers(v: &serde_json::Value) -> Vec<Server> {
    let Some(list) = v.as_array() else { return Vec::new() };
    list.iter().filter_map(parse_server).collect()
}

fn parse_server(s: &serde_json::Value) -> Option<Server> {
    if s.get("status").and_then(|x| x.as_str()).is_some_and(|st| st != "online") {
        return None;
    }
    let public_key = s
        .get("technologies")?
        .as_array()?
        .iter()
        .find(|t| t.get("identifier").and_then(|i| i.as_str()) == Some("wireguard_udp"))?
        .get("metadata")?
        .as_array()?
        .iter()
        .find(|m| m.get("name").and_then(|n| n.as_str()) == Some("public_key"))?
        .get("value")?
        .as_str()?
        .to_string();
    if !is_wireguard_key(&public_key) {
        return None;
    }
    let country = s.get("locations")?.as_array()?.first()?.get("country")?;
    let station = s.get("station")?.as_str()?.to_string();
    station.parse::<std::net::Ipv4Addr>().ok()?;
    Some(Server {
        name: s.get("name")?.as_str()?.to_string(),
        hostname: s.get("hostname")?.as_str()?.to_string(),
        station,
        load: s.get("load").and_then(|l| l.as_u64()).unwrap_or(0).min(100) as u8,
        country: country.get("name")?.as_str()?.to_string(),
        country_code: country.get("code").and_then(|c| c.as_str()).unwrap_or_default().to_string(),
        country_id: country.get("id").and_then(|c| c.as_u64()).unwrap_or(0) as u32,
        city: country
            .get("city")
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string(),
        public_key,
    })
}

pub fn parse_countries(v: &serde_json::Value) -> Vec<Country> {
    let Some(list) = v.as_array() else { return Vec::new() };
    let mut out: Vec<Country> = list
        .iter()
        .filter_map(|c| {
            Some(Country {
                id: c.get("id")?.as_u64()? as u32,
                name: c.get("name")?.as_str()?.to_string(),
                code: c.get("code").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                server_count: c.get("serverCount").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            })
        })
        .filter(|c| c.server_count > 0)
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn server_json(status: &str, key: &str) -> serde_json::Value {
        serde_json::json!([{
            "name": "United States #8348", "hostname": "us8348.nordvpn.com",
            "station": "138.199.52.11", "load": 23, "status": status,
            "locations": [{"country": {"id": 228, "name": "United States", "code": "US",
                                       "city": {"name": "New York"}}}],
            "technologies": [
                {"identifier": "openvpn_udp", "metadata": []},
                {"identifier": "wireguard_udp", "metadata": [{"name": "public_key", "value": key}]}
            ]
        }])
    }

    #[test]
    fn parses_a_wireguard_server() {
        let servers = parse_servers(&server_json("online", KEY));
        assert_eq!(servers.len(), 1);
        let s = &servers[0];
        assert_eq!(s.hostname, "us8348.nordvpn.com");
        assert_eq!((s.country_id, s.city.as_str(), s.load), (228, "New York", 23));
        assert_eq!(s.endpoint().unwrap().to_string(), "138.199.52.11:51820");
    }

    #[test]
    fn skips_offline_servers_and_bad_keys() {
        assert!(parse_servers(&server_json("maintenance", KEY)).is_empty());
        assert!(parse_servers(&server_json("online", "not-a-key")).is_empty());
    }

    #[test]
    fn parses_countries_sorted_and_skips_empty_ones() {
        let v = serde_json::json!([
            {"id": 2, "name": "Zambia", "code": "ZM", "serverCount": 3},
            {"id": 1, "name": "Albania", "code": "AL", "serverCount": 5},
            {"id": 3, "name": "Nowhere", "code": "NW", "serverCount": 0}
        ]);
        let names: Vec<_> = parse_countries(&v).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["Albania", "Zambia"]);
    }

    #[test]
    fn recognises_keys_and_tokens() {
        assert!(is_wireguard_key(KEY));
        assert!(!is_wireguard_key("short"));
        assert!(looks_like_token(&"a1".repeat(32)));
        assert!(looks_like_token(" Abc123_-.xyzAbc123_-.xyzAbc123_-.x "));
        assert!(!looks_like_token("ab12cd!?x"));
        assert!(!looks_like_token(&"a b".repeat(20)));
    }

    /// Talks to the real API: `cargo test -p nord -- --ignored`.
    #[test]
    #[ignore]
    fn live_recommendations_and_countries() {
        let c = Client::default();
        let servers = c.recommendations(None, 3).expect("recommendations");
        assert!(!servers.is_empty());
        let countries = c.countries().expect("countries");
        assert!(countries.len() > 50);
        let us = countries.iter().find(|c| c.code == "US").expect("US");
        let in_us = c.recommendations(Some(us.id), 2).expect("US servers");
        assert!(in_us.iter().all(|s| s.country_code == "US"));
    }
}
