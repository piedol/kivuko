//! What the tunnel keeps between starts, in its sandbox container (mode 0600):
//! the NordLynx private key and the last good server list, for when Nord's API can't be
//! reached. macOS's VPN configuration is world-readable, so the key can't live there.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub private_key: Option<String>,
    pub servers: Vec<nord::Server>,
}

fn path() -> PathBuf {
    let home = objc2_foundation::NSHomeDirectory().to_string();
    PathBuf::from(home).join("Library/Application Support/state.json")
}

pub fn load() -> Store {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(store: &Store) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("json.tmp");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(&serde_json::to_vec(store).unwrap_or_default()));
    match written.and_then(|_| std::fs::rename(&tmp, &p)) {
        Ok(()) => {}
        Err(e) => log::error!("couldn't save state: {e}"),
    }
}
