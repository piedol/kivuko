//! The VPN configuration macOS keeps for us (System Settings › VPN), and the connection to
//! the tunnel extension. Everything here runs on the main thread.

use std::cell::RefCell;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSString};
use objc2_network_extension::{
    NEOnDemandRule, NEOnDemandRuleConnect, NEOnDemandRuleInterfaceType, NETunnelProviderManager,
    NETunnelProviderProtocol, NETunnelProviderSession, NEVPNStatus,
};
use shared::{Request, Status, ids, keys};

use crate::prefs::Prefs;
use crate::util::{ns, once};

thread_local! {
    static MANAGER: RefCell<Option<Retained<NETunnelProviderManager>>> = const { RefCell::new(None) };
}

fn manager() -> Option<Retained<NETunnelProviderManager>> {
    MANAGER.with(|m| m.borrow().clone())
}

fn error_text(err: *mut NSError) -> Option<String> {
    unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string())
}

/// Find our saved configuration, if there is one.
pub fn load(then: impl FnOnce() + 'static) {
    let then = RefCell::new(Some(then));
    let block = RcBlock::new(move |list: *mut NSArray<NETunnelProviderManager>, err: *mut NSError| {
        if let Some(e) = error_text(err) {
            log::error!("couldn't read VPN configurations: {e}");
        }
        let ours = unsafe { list.as_ref() }.and_then(|list| {
            list.iter().find(|m| {
                unsafe { m.protocolConfiguration() }
                    .and_then(|p| p.downcast::<NETunnelProviderProtocol>().ok())
                    .and_then(|p| unsafe { p.providerBundleIdentifier() })
                    .is_some_and(|id| id.to_string() == ids::PROVIDER_BUNDLE)
            })
        });
        MANAGER.with(|m| *m.borrow_mut() = ours);
        if let Some(f) = then.borrow_mut().take() {
            f();
        }
    });
    unsafe { NETunnelProviderManager::loadAllFromPreferencesWithCompletionHandler(&block) };
}

pub fn exists() -> bool {
    manager().is_some()
}

pub fn status() -> NEVPNStatus {
    manager().map_or(NEVPNStatus::Invalid, |m| unsafe { m.connection().status() })
}

pub fn on_demand() -> bool {
    manager().is_some_and(|m| unsafe { m.isOnDemandEnabled() })
}

/// Seconds since the tunnel came up.
pub fn connected_secs() -> Option<u64> {
    let m = manager()?;
    let date = unsafe { m.connection().connectedDate() }?;
    Some((-date.timeIntervalSinceNow()).max(0.0) as u64)
}

/// Create or update the configuration from the preferences, with On-Demand on or off, and
/// save it. macOS asks the user to allow a VPN configuration the first time.
pub fn save(prefs: &Prefs, on_demand: bool, then: impl FnOnce(Result<(), String>) + 'static) {
    let m = manager().unwrap_or_else(|| unsafe { NETunnelProviderManager::new() });
    unsafe {
        let proto = NETunnelProviderProtocol::new();
        proto.setProviderBundleIdentifier(Some(&ns(ids::PROVIDER_BUNDLE)));
        proto.setServerAddress(Some(&ns("NordVPN (NordLynx)")));
        proto.setIncludeAllNetworks(prefs.kill_switch);
        proto.setExcludeLocalNetworks(true);
        let settings = String::from_utf8(shared::encode(&prefs.tunnel_settings())).unwrap_or_default();
        let value: &AnyObject = &ns(&settings);
        let config = NSDictionary::from_slices(&[&*ns(keys::SETTINGS)], &[value]);
        proto.setProviderConfiguration(Some(&config));
        m.setProtocolConfiguration(Some(&proto));
        m.setLocalizedDescription(Some(&ns(ids::VPN_DESCRIPTION)));
        m.setEnabled(true);
        let rule = NEOnDemandRuleConnect::new();
        rule.setInterfaceTypeMatch(NEOnDemandRuleInterfaceType::Any);
        let rule: Retained<NEOnDemandRule> = Retained::into_super(rule);
        m.setOnDemandRules(Some(&NSArray::from_retained_slice(&[rule])));
        m.setOnDemandEnabled(on_demand);
    }
    let saved = m.clone();
    let done = RcBlock::new(once(move |err: *mut NSError| {
        if let Some(e) = error_text(err) {
            log::error!("couldn't save the VPN configuration: {e}");
            return then(Err(e));
        }
        MANAGER.with(|cell| *cell.borrow_mut() = Some(saved.clone()));
        // A saved configuration must be reloaded before it can be started.
        let reloaded = RcBlock::new(once(move |err: *mut NSError| match error_text(err) {
            Some(e) => then(Err(e)),
            None => then(Ok(())),
        }));
        unsafe { saved.loadFromPreferencesWithCompletionHandler(&reloaded) };
    }));
    unsafe { m.saveToPreferencesWithCompletionHandler(Some(&done)) };
}

fn session() -> Option<Retained<NETunnelProviderSession>> {
    let m = manager()?;
    unsafe { m.connection() }.downcast::<NETunnelProviderSession>().ok()
}

/// Start the tunnel. `key` hands the tunnel a fresh WireGuard key; without it the tunnel
/// uses the one it saved last time.
pub fn start(key: Option<&str>) -> Result<(), String> {
    let session = session().ok_or("Kivuko isn't set up yet.")?;
    let names: Vec<Retained<NSString>> = key.map(|_| ns(keys::PRIVATE_KEY)).into_iter().collect();
    let values: Vec<Retained<NSString>> = key.map(ns).into_iter().collect();
    let names: Vec<&NSString> = names.iter().map(|n| &**n).collect();
    let values: Vec<&AnyObject> = values.iter().map(|v| -> &AnyObject { v }).collect();
    let options = NSDictionary::from_slices(&names, &values);
    unsafe { session.startTunnelWithOptions_andReturnError(Some(&options)) }
        .map_err(|e| e.localizedDescription().to_string())
}

pub fn stop() {
    if let Some(s) = session() {
        unsafe { s.stopTunnel() };
    }
}

/// Ask the running tunnel for its status.
pub fn request_status(then: impl FnOnce(Option<Status>) + 'static) {
    let Some(s) = session() else { return then(None) };
    if !matches!(status(), NEVPNStatus::Connected | NEVPNStatus::Reasserting | NEVPNStatus::Connecting) {
        return then(None);
    }
    let then = std::rc::Rc::new(RefCell::new(Some(then)));
    let reply = then.clone();
    let handler = RcBlock::new(move |data: *mut NSData| {
        let status = unsafe { data.as_ref() }.and_then(|d| shared::decode::<Status>(&d.to_vec()));
        if let Some(f) = reply.borrow_mut().take() {
            f(status);
        }
    });
    let message = NSData::with_bytes(&shared::encode(&Request::Status));
    let sent = unsafe { s.sendProviderMessage_returnError_responseHandler(&message, None, Some(&handler)) };
    if !sent
        && let Some(f) = then.borrow_mut().take()
    {
        f(None);
    }
}

/// Why the tunnel last stopped, when it stopped because of an error.
pub fn last_error(then: impl FnOnce(Option<String>) + 'static) {
    let Some(m) = manager() else { return then(None) };
    let done = RcBlock::new(once(move |err: *mut NSError| then(error_text(err))));
    unsafe { m.connection().fetchLastDisconnectErrorWithCompletionHandler(&done) };
}
