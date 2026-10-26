//! The packet-tunnel app extension. macOS launches this binary (sandboxed) when the VPN
//! starts, instantiates `TunnelProvider` and calls it to start and stop the tunnel.

mod provider;
mod store;
mod tunnel;
mod utun;

use objc2::ClassType;

use std::ffi::{CString, c_char, c_int};

unsafe extern "C" {
    /// Foundation's entry point for app extensions.
    fn NSExtensionMain(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

fn main() {
    let _ = oslog::OsLogger::new(shared::ids::LOG_SUBSYSTEM).level_filter(log::LevelFilter::Info).init();
    log::info!("tunnel extension {} starting", env!("CARGO_PKG_VERSION"));
    // Register the class before NetworkExtension looks it up by name.
    let _ = provider::TunnelProvider::class();
    let args: Vec<CString> = std::env::args().filter_map(|a| CString::new(a).ok()).collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let code = unsafe { NSExtensionMain(args.len() as c_int, argv.as_mut_ptr()) };
    std::process::exit(code);
}
