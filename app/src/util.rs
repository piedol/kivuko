//! Small helpers shared across modules.

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2_foundation::NSString;

/// Run `f` on the main queue after `secs` seconds.
#[cfg(debug_assertions)]
pub fn main_after(secs: f64, f: impl FnOnce() + Send + 'static) {
    let when = dispatch2::DispatchTime::NOW.time((secs * 1_000_000_000.0) as i64);
    let _ = dispatch2::DispatchQueue::main().after(when, f);
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

/// Wrap an FnOnce so it can be called from a block that AppKit types as Fn.
pub fn once<A>(f: impl FnOnce(A) + 'static) -> impl Fn(A) + 'static {
    let cell = std::cell::RefCell::new(Some(f));
    move |a| {
        if let Some(f) = cell.borrow_mut().take() {
            f(a)
        }
    }
}

/// "12 min", "2 h 14 min", "3 days".
pub fn duration(secs: u64) -> String {
    match secs {
        0..60 => "less than a minute".into(),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => match (secs / 3600, (secs % 3600) / 60) {
            (h, 0) => format!("{h} h"),
            (h, m) => format!("{h} h {m} min"),
        },
        _ => match secs / 86_400 {
            1 => "1 day".into(),
            d => format!("{d} days"),
        },
    }
}

/// "840 KB", "12.4 MB", "3.1 GB".
pub fn bytes(n: u64) -> String {
    let n = n as f64;
    if n < 1e6 {
        format!("{:.0} KB", n / 1e3)
    } else if n < 1e9 {
        format!("{:.1} MB", n / 1e6)
    } else {
        format!("{:.1} GB", n / 1e9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_and_sizes() {
        assert_eq!(duration(30), "less than a minute");
        assert_eq!(duration(125), "2 min");
        assert_eq!(duration(3600), "1 h");
        assert_eq!(duration(8040), "2 h 14 min");
        assert_eq!(duration(200_000), "2 days");
        assert_eq!(bytes(840_000), "840 KB");
        assert_eq!(bytes(12_400_000), "12.4 MB");
        assert_eq!(bytes(3_100_000_000), "3.1 GB");
    }
}
