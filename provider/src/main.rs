//! The packet-tunnel provider. macOS launches this binary (sandboxed) when the VPN starts,
//! instantiates `TunnelProvider` and calls it to start and stop the tunnel.
//!
//! The default build is an app extension, for development signing. With the `sysext`
//! feature it is a system extension, which Developer ID builds need. macOS runs that one as
//! root once the app has activated it.

mod provider;
mod store;
mod tunnel;
mod utun;

use objc2::ClassType;

fn main() {
    let _ = oslog::OsLogger::new(shared::ids::LOG_SUBSYSTEM).level_filter(log::LevelFilter::Info).init();
    log::info!("tunnel {} {} starting", KIND, env!("CARGO_PKG_VERSION"));
    // Register the class before NetworkExtension looks it up by name.
    let _ = provider::TunnelProvider::class();
    run()
}

#[cfg(not(feature = "sysext"))]
const KIND: &str = "app extension";

#[cfg(not(feature = "sysext"))]
fn run() -> ! {
    use std::ffi::{CString, c_char, c_int};

    unsafe extern "C" {
        /// Foundation's entry point for app extensions.
        fn NSExtensionMain(argc: c_int, argv: *mut *mut c_char) -> c_int;
    }

    let args: Vec<CString> = std::env::args().filter_map(|a| CString::new(a).ok()).collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let code = unsafe { NSExtensionMain(args.len() as c_int, argv.as_mut_ptr()) };
    std::process::exit(code);
}

#[cfg(feature = "sysext")]
const KIND: &str = "system extension";

#[cfg(feature = "sysext")]
fn run() -> ! {
    // NetworkExtension finds the provider class through NEProviderClasses in Info.plist and
    // calls it on the main queue.
    objc2::rc::autoreleasepool(|_| unsafe { objc2_network_extension::NEProvider::startSystemExtensionMode() });
    dispatch2::dispatch_main()
}
