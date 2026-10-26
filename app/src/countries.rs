//! Nord's country list for the Location menu, cached for a week in
//! ~/Library/Caches/Kivuko/countries.json.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

static LIST: Mutex<Vec<nord::Country>> = Mutex::new(Vec::new());
static FETCHING: AtomicBool = AtomicBool::new(false);
const MAX_AGE: Duration = Duration::from_secs(7 * 86_400);

fn cache() -> std::path::PathBuf {
    crate::util::home().join("Library/Caches/Kivuko/countries.json")
}

pub fn list() -> Vec<nord::Country> {
    LIST.lock().unwrap().clone()
}

/// Load the cache now; fetch a fresh list in the background when it's old or missing.
/// Without any list, a failed fetch is retried with growing pauses (up to 10 minutes):
/// at launch the network may not be up yet, or the kill switch may be holding traffic while
/// the tunnel restarts.
pub fn refresh_async() {
    let path = cache();
    if let Some(cached) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Vec<nord::Country>>(&b).ok())
    {
        *LIST.lock().unwrap() = cached;
    }
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < MAX_AGE);
    if fresh && !LIST.lock().unwrap().is_empty() {
        return;
    }
    if FETCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let mut pause = Duration::from_secs(5);
        loop {
            match nord::Client::default().countries() {
                Ok(list) => {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if let Err(e) = std::fs::write(&path, serde_json::to_vec(&list).unwrap_or_default()) {
                        log::warn!("couldn't cache countries: {e}");
                    }
                    *LIST.lock().unwrap() = list;
                    break;
                }
                // An old list is still usable; try again at the next launch.
                Err(e) if !LIST.lock().unwrap().is_empty() => {
                    log::warn!("couldn't refresh countries: {e}");
                    break;
                }
                Err(e) => {
                    log::warn!("couldn't load countries: {e}; retrying in {} s", pause.as_secs());
                    std::thread::sleep(pause);
                    pause = (pause * 3).min(Duration::from_secs(600));
                }
            }
        }
        FETCHING.store(false, Ordering::Release);
    });
}
