//! Kivuko: a lightweight NordVPN client for macOS. This is the menu-bar app; the WireGuard
//! tunnel itself runs in an app extension (provider/) that macOS starts on its own.

mod app;
mod countries;
mod keychain;
mod prefs;
mod ui;
mod util;
mod vpn;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("Kivuko is built for Apple silicon Macs.");

fn main() {
    let _ = oslog::OsLogger::new(shared::ids::LOG_SUBSYSTEM).level_filter(log::LevelFilter::Info).init();
    app::run();
}
