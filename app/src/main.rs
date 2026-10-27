//! Kivuko: a NordVPN client for macOS. This is the menu-bar app. The WireGuard tunnel runs
//! in a NetworkExtension provider (provider/) that macOS starts: an app extension in
//! development builds, a system extension in Developer ID builds (feature `sysext`).

mod app;
mod countries;
mod extension;
mod keychain;
mod prefs;
mod ui;
// Shared by piedol's Mac apps and copied as it is, so it keeps their formatting.
#[rustfmt::skip]
mod update;
mod update_ui;
mod util;
mod vpn;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("Kivuko is built for Apple silicon Macs.");

fn main() {
    let _ = oslog::OsLogger::new(shared::ids::LOG_SUBSYSTEM).level_filter(log::LevelFilter::Info).init();
    app::run();
}
