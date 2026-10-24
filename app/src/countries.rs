//! Nord's country list for the Location menu, cached for a week in
//! ~/Library/Caches/Kivuko/countries.json.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

static LIST: Mutex<Vec<nord::Country>> = Mutex::new(Vec::new());
const MAX_AGE: Duration = Duration::from_secs(7 * 86_400);

fn cache() -> std::path::PathBuf {
    crate::util::home().join("Library/Caches/Kivuko/countries.json")
}

pub fn list() -> Vec<nord::Country> {
    LIST.lock().unwrap().clone()
}

/// Load the cache now; fetch a fresh list in the background when it's old or missing.
pub fn refresh_async() {
    let path = cache();
    if let Some(cached) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Vec<nord::Country>>(&b).ok()) {
        *LIST.lock().unwrap() = cached;
    }
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < MAX_AGE);
    if fresh && !LIST.lock().unwrap().is_empty() {
        return;
    }
    std::thread::spawn(move || match nord::Client::default().countries() {
        Ok(list) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, serde_json::to_vec(&list).unwrap_or_default());
            *LIST.lock().unwrap() = list;
        }
        Err(e) => log::warn!("couldn't refresh countries: {e}"),
    });
}
