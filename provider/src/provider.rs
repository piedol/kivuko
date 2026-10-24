//! The `NEPacketTunnelProvider` subclass macOS loads from our Info.plist
//! (`NEProviderClasses` → `TunnelProvider`). It translates between NetworkExtension's
//! callbacks and the tunnel controller in tunnel.rs.

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AllocAnyThread, Message, define_class};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSLocalizedDescriptionKey, NSNumber, NSString};
use objc2_network_extension::{
    NEDNSSettings, NEIPv4Route, NEIPv4Settings, NEIPv6Route, NEIPv6Settings, NEPacketTunnelNetworkSettings,
    NEPacketTunnelProvider, NEProvider, NEProviderStopReason, NETunnelProvider, NETunnelProviderProtocol,
};
use shared::{Dns, Request, Settings, ids, keys, net};
use tokio::sync::oneshot;

use crate::tunnel;

define_class!(
    #[unsafe(super(NEPacketTunnelProvider, NETunnelProvider, NEProvider, NSObject))]
    #[name = "TunnelProvider"]
    pub struct TunnelProvider;

    impl TunnelProvider {
        #[unsafe(method(startTunnelWithOptions:completionHandler:))]
        fn start_tunnel(
            &self,
            options: Option<&NSDictionary<NSString, NSObject>>,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let key = options
                .and_then(|o| o.objectForKey(&NSString::from_str(keys::PRIVATE_KEY)))
                .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()));
            log::info!("start requested by {}", if options.is_some() { "the app" } else { "macOS" });
            tunnel::start(Handle(self.retain()), key, self.settings(), Done(completion.copy()));
        }

        #[unsafe(method(stopTunnelWithReason:completionHandler:))]
        fn stop_tunnel(&self, reason: NEProviderStopReason, completion: &DynBlock<dyn Fn()>) {
            log::info!("stop requested (reason {})", reason.0);
            tunnel::stop(Stopped(completion.copy()));
        }

        #[unsafe(method(handleAppMessage:completionHandler:))]
        fn handle_app_message(&self, data: &NSData, completion: Option<&DynBlock<dyn Fn(*mut NSData)>>) {
            let request = shared::decode::<Request>(&data.to_vec());
            tunnel::message(request, completion.map(|c| Reply(c.copy())));
        }

        #[unsafe(method(sleepWithCompletionHandler:))]
        fn sleep(&self, completion: &DynBlock<dyn Fn()>) {
            completion.call(());
        }

        #[unsafe(method(wake))]
        fn wake(&self) {
            tunnel::wake();
        }
    }
);

impl TunnelProvider {
    /// Settings the app stored in the VPN configuration (country, DNS).
    fn settings(&self) -> Settings {
        let proto = unsafe { self.protocolConfiguration() };
        proto
            .downcast_ref::<NETunnelProviderProtocol>()
            .and_then(|p| unsafe { p.providerConfiguration() })
            .and_then(|d| d.objectForKey(&NSString::from_str(keys::SETTINGS)))
            .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()))
            .and_then(|json| shared::decode(json.as_bytes()))
            .unwrap_or_default()
    }
}

/// The provider, handed to the tunnel's worker threads. NetworkExtension's provider methods
/// may be called from any thread.
#[derive(Clone)]
pub struct Handle(Retained<TunnelProvider>);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// Point macOS's tunnel at `server`: addresses, default routes and DNS. Resolves when
    /// macOS has applied them (or failed to).
    pub fn apply(&self, server: &nord::Server, dns: Dns) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        let done = RcBlock::new(move |err: *mut NSError| {
            let result = match unsafe { err.as_ref() } {
                None => Ok(()),
                Some(e) => Err(e.localizedDescription().to_string()),
            };
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(result);
            }
        });
        let settings = network_settings(server, dns);
        unsafe { self.0.setTunnelNetworkSettings_completionHandler(Some(&settings), Some(&done)) };
        rx
    }

    /// Tear the tunnel down with an error. If On-Demand is on, macOS starts it again, which
    /// picks a fresh server.
    pub fn cancel(&self, message: &str) {
        unsafe { self.0.cancelTunnelWithError(Some(&ns_error(message))) };
    }

    /// Shows "Reconnecting" in macOS while true.
    pub fn set_reasserting(&self, on: bool) {
        unsafe { self.0.setReasserting(on) };
    }
}

fn strings(items: &[&str]) -> Retained<NSArray<NSString>> {
    NSArray::from_retained_slice(&items.iter().map(|s| NSString::from_str(s)).collect::<Vec<_>>())
}

fn network_settings(server: &nord::Server, dns: Dns) -> Retained<NEPacketTunnelNetworkSettings> {
    unsafe {
        let s = NEPacketTunnelNetworkSettings::initWithTunnelRemoteAddress(
            NEPacketTunnelNetworkSettings::alloc(),
            &NSString::from_str(&server.station),
        );
        let v4 = NEIPv4Settings::initWithAddresses_subnetMasks(
            NEIPv4Settings::alloc(),
            &strings(&[net::IPV4_ADDRESS]),
            &strings(&["255.255.255.255"]),
        );
        v4.setIncludedRoutes(Some(&NSArray::from_retained_slice(&[NEIPv4Route::defaultRoute()])));
        s.setIPv4Settings(Some(&v4));
        let v6 = NEIPv6Settings::initWithAddresses_networkPrefixLengths(
            NEIPv6Settings::alloc(),
            &strings(&[net::IPV6_ADDRESS]),
            &NSArray::from_retained_slice(&[NSNumber::new_u8(128)]),
        );
        v6.setIncludedRoutes(Some(&NSArray::from_retained_slice(&[NEIPv6Route::defaultRoute()])));
        s.setIPv6Settings(Some(&v6));
        let d = NEDNSSettings::initWithServers(NEDNSSettings::alloc(), &strings(&dns.servers()));
        // An empty match domain sends every DNS query to these servers.
        d.setMatchDomains(Some(&strings(&[""])));
        s.setDNSSettings(Some(&d));
        s.setMTU(Some(&NSNumber::new_u16(net::MTU)));
        s
    }
}

pub fn ns_error(message: &str) -> Retained<NSError> {
    let text = NSString::from_str(message);
    let value: &AnyObject = &text;
    let info = NSDictionary::from_slices(&[unsafe { NSLocalizedDescriptionKey }], &[value]);
    unsafe { NSError::errorWithDomain_code_userInfo(&NSString::from_str(ids::PROVIDER_BUNDLE), 1, Some(&info)) }
}

/// startTunnel's completion handler.
pub struct Done(RcBlock<dyn Fn(*mut NSError)>);
unsafe impl Send for Done {}

impl Done {
    pub fn finish(self, error: Option<&str>) {
        match error {
            None => self.0.call((std::ptr::null_mut(),)),
            Some(m) => {
                let e = ns_error(m);
                self.0.call((Retained::as_ptr(&e) as *mut NSError,));
            }
        }
    }
}

/// stopTunnel's completion handler.
pub struct Stopped(RcBlock<dyn Fn()>);
unsafe impl Send for Stopped {}

impl Stopped {
    pub fn finish(self) {
        self.0.call(());
    }
}

/// handleAppMessage's completion handler.
pub struct Reply(RcBlock<dyn Fn(*mut NSData)>);
unsafe impl Send for Reply {}

impl Reply {
    pub fn send(self, bytes: &[u8]) {
        let data = NSData::with_bytes(bytes);
        self.0.call((Retained::as_ptr(&data) as *mut NSData,));
    }
}
