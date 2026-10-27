//! The app's own preferences, in ~/Library/Application Support/Kivuko/settings.json.
//! The tunnel gets the parts it needs through the VPN configuration (see vpn.rs).

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use shared::Dns;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Block all traffic outside the tunnel while it's on or reconnecting.
    pub kill_switch: bool,
    /// Connect whenever there's a network (On-Demand), until you click Disconnect.
    pub auto_connect: bool,
    pub dns: Dns,
    /// None = the fastest server anywhere.
    pub country: Option<Place>,
    /// Last few countries chosen, newest first.
    pub recent: Vec<Place>,
    /// The VPN configuration was last saved by a build whose tunnel is a system extension.
    /// A Developer ID build that finds this false saves the configuration again, so macOS
    /// starts the system extension, not a development build's app extension.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub saved_for_system_extension: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            kill_switch: true,
            auto_connect: true,
            dns: Dns::BlockAds,
            country: None,
            recent: Vec::new(),
            saved_for_system_extension: false,
        }
    }
}

impl Prefs {
    /// What the tunnel reads when it starts.
    pub fn tunnel_settings(&self) -> shared::Settings {
        shared::Settings {
            country_id: self.country.as_ref().map(|c| c.id),
            country_name: self.country.as_ref().map(|c| c.name.clone()),
            dns: self.dns,
        }
    }

    pub fn choose_country(&mut self, place: Option<Place>) {
        if let Some(p) = &place {
            self.recent.retain(|r| r.id != p.id);
            self.recent.insert(0, p.clone());
            self.recent.truncate(5);
        }
        self.country = place;
    }
}

static CURRENT: Mutex<Option<Prefs>> = Mutex::new(None);

pub fn dir() -> PathBuf {
    crate::util::home().join("Library/Application Support/Kivuko")
}

fn path() -> PathBuf {
    dir().join("settings.json")
}

pub fn get() -> Prefs {
    let mut cur = CURRENT.lock().unwrap();
    cur.get_or_insert_with(|| {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    })
    .clone()
}

/// Change preferences and save them at once.
pub fn update(f: impl FnOnce(&mut Prefs)) -> Prefs {
    let mut p = get();
    f(&mut p);
    let _ = std::fs::create_dir_all(dir());
    if let Ok(text) = serde_json::to_vec_pretty(&p) {
        let tmp = path().with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, path());
        }
    }
    *CURRENT.lock().unwrap() = Some(p.clone());
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_countries_stay_unique_newest_first() {
        let mut p = Prefs::default();
        for (id, name) in [(1, "A"), (2, "B"), (1, "A")] {
            p.choose_country(Some(Place { id, name: name.into() }));
        }
        let ids: Vec<_> = p.recent.iter().map(|r| r.id).collect();
        assert_eq!(ids, [1, 2]);
        p.choose_country(None);
        assert_eq!(p.tunnel_settings().country_id, None);
        assert_eq!(p.recent.len(), 2);
    }

    #[test]
    fn defaults_protect() {
        let p: Prefs = serde_json::from_str("{}").unwrap();
        assert!(p.kill_switch && p.auto_connect);
        assert_eq!(p.dns, Dns::BlockAds);
        assert!(!p.saved_for_system_extension);
    }

    #[test]
    fn system_extension_mark_is_written_only_when_set() {
        // Development builds leave settings.json as it was before the mark existed.
        let mut p = Prefs::default();
        assert!(!serde_json::to_string(&p).unwrap().contains("saved_for_system_extension"));
        p.saved_for_system_extension = true;
        let back: Prefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert!(back.saved_for_system_extension);
    }
}
