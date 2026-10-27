//! Whether the tunnel is ready to start. In a Developer ID build (feature `sysext`) the
//! tunnel is a system extension: the app asks macOS to activate it at every launch, and the
//! first time macOS waits for the user to allow it in System Settings. In a development
//! build the tunnel is an app extension, which macOS loads from the app bundle, so it is
//! always ready.

/// Where the tunnel's activation stands.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(feature = "sysext"), allow(dead_code))]
pub enum State {
    /// The request is with macOS.
    Activating,
    /// macOS is waiting for the user to allow the tunnel in System Settings.
    NeedsApproval,
    Active,
    Failed(String),
}

/// True when the tunnel is a system extension.
pub const SYSTEM_EXTENSION: bool = cfg!(feature = "sysext");

pub fn ready() -> bool {
    state() == State::Active
}

#[cfg(not(feature = "sysext"))]
pub fn state() -> State {
    State::Active
}

#[cfg(not(feature = "sysext"))]
pub fn activate() {}

#[cfg(feature = "sysext")]
pub use sysext::{activate, state};

/// macOS activates system extensions only for apps in /Applications.
pub fn in_applications() -> bool {
    objc2_foundation::NSBundle::mainBundle().bundlePath().to_string().starts_with("/Applications/")
}

/// System Settings where macOS asks to allow the tunnel, and that pane's name.
pub fn approval_pane() -> (&'static str, &'static str) {
    let version = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    if version.majorVersion >= 15 {
        ("x-apple.systempreferences:com.apple.LoginItems-Settings.extension", "General › Login Items & Extensions")
    } else {
        ("x-apple.systempreferences:com.apple.preference.security", "Privacy & Security")
    }
}

#[cfg(feature = "sysext")]
mod sysext {
    use std::cell::{Cell, OnceCell, RefCell};

    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_foundation::{NSError, NSObject, NSObjectProtocol};
    use objc2_system_extensions::{
        OSSystemExtensionErrorCode, OSSystemExtensionErrorDomain, OSSystemExtensionManager,
        OSSystemExtensionProperties, OSSystemExtensionReplacementAction, OSSystemExtensionRequest,
        OSSystemExtensionRequestDelegate, OSSystemExtensionRequestResult,
    };
    use shared::ids;

    use super::State;
    use crate::util::ns;

    thread_local! {
        static STATE: RefCell<State> = const { RefCell::new(State::Activating) };
        /// The requests' delegate. Their delegate property is weak, so this keeps it alive.
        static DELEGATE: OnceCell<Retained<Delegate>> = const { OnceCell::new() };
        /// The request in flight.
        static REQUEST: RefCell<Option<Retained<OSSystemExtensionRequest>>> = const { RefCell::new(None) };
        /// The delegate kept the active tunnel because it's the same version, so the
        /// cancellation that follows means the tunnel is active.
        static KEPT_ACTIVE: Cell<bool> = const { Cell::new(false) };
    }

    pub fn state() -> State {
        STATE.with(|s| s.borrow().clone())
    }

    fn set(state: State) {
        match &state {
            State::Failed(e) => log::error!("tunnel activation failed: {e}"),
            s => log::info!("tunnel activation: {s:?}"),
        }
        STATE.with(|s| *s.borrow_mut() = state);
        crate::app::extension_changed();
    }

    /// Ask macOS to activate the tunnel. If it's already active, the request completes at
    /// once. The delegate's callbacks run on the main queue.
    pub fn activate() {
        let mtm = MainThreadMarker::new().expect("activate the tunnel from the main thread");
        let delegate = DELEGATE.with(|d| {
            d.get_or_init(|| {
                let this = Delegate::alloc(mtm).set_ivars(());
                unsafe { msg_send![super(this), init] }
            })
            .clone()
        });
        KEPT_ACTIVE.with(|k| k.set(false));
        STATE.with(|s| *s.borrow_mut() = State::Activating);
        let request = unsafe {
            OSSystemExtensionRequest::activationRequestForExtension_queue(
                &ns(ids::PROVIDER_BUNDLE),
                dispatch2::DispatchQueue::main(),
            )
        };
        unsafe { request.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
        log::info!("asking macOS to activate the tunnel {}", ids::PROVIDER_BUNDLE);
        REQUEST.with(|r| *r.borrow_mut() = Some(request.clone()));
        unsafe { OSSystemExtensionManager::sharedManager().submitRequest(&request) };
    }

    /// Whether a callback is about the request in flight, not one an earlier attempt left.
    fn current(request: &OSSystemExtensionRequest) -> bool {
        REQUEST.with(|r| r.borrow().as_deref().is_some_and(|r| std::ptr::eq(r, request)))
    }

    /// The request is over: forget it, then report where the tunnel stands.
    fn finish(request: &OSSystemExtensionRequest, state: State) {
        if current(request) {
            REQUEST.with(|r| *r.borrow_mut() = None);
            set(state);
        }
    }

    fn version(p: &OSSystemExtensionProperties) -> String {
        unsafe { format!("{} ({})", p.bundleShortVersion(), p.bundleVersion()) }
    }

    /// The menu's words for a failed request.
    fn describe(error: &NSError) -> String {
        if error.domain().to_string() != unsafe { OSSystemExtensionErrorDomain }.to_string() {
            return error.localizedDescription().to_string();
        }
        match OSSystemExtensionErrorCode(error.code()) {
            OSSystemExtensionErrorCode::UnsupportedParentBundleLocation => {
                "macOS starts the VPN only for Kivuko in the Applications folder".into()
            }
            OSSystemExtensionErrorCode::ForbiddenBySystemPolicy => "This Mac's management settings block it".into(),
            OSSystemExtensionErrorCode::CodeSignatureInvalid | OSSystemExtensionErrorCode::ValidationFailed => {
                "macOS didn't accept the tunnel's signature".into()
            }
            OSSystemExtensionErrorCode::AuthorizationRequired => "An administrator has to allow it".into(),
            _ => error.localizedDescription().to_string(),
        }
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements; no Drop impl.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[name = "TunnelActivationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl OSSystemExtensionRequestDelegate for Delegate {
            /// macOS asks this when another version of the tunnel is active, as after an
            /// update. A different version replaces it; the same version (only asked in
            /// System Extension developer mode) stays.
            #[unsafe(method(request:actionForReplacingExtension:withExtension:))]
            fn replace(
                &self,
                _request: &OSSystemExtensionRequest,
                existing: &OSSystemExtensionProperties,
                ext: &OSSystemExtensionProperties,
            ) -> OSSystemExtensionReplacementAction {
                let (old, new) = (version(existing), version(ext));
                if old == new {
                    log::info!("tunnel {old} is already active; keeping it");
                    KEPT_ACTIVE.with(|k| k.set(true));
                    OSSystemExtensionReplacementAction::Cancel
                } else {
                    log::info!("replacing tunnel {old} with {new}");
                    OSSystemExtensionReplacementAction::Replace
                }
            }

            /// The request stays open until the user allows the tunnel, then finishes.
            #[unsafe(method(requestNeedsUserApproval:))]
            fn needs_approval(&self, request: &OSSystemExtensionRequest) {
                if current(request) {
                    set(State::NeedsApproval);
                }
            }

            #[unsafe(method(request:didFinishWithResult:))]
            fn finished(&self, request: &OSSystemExtensionRequest, result: OSSystemExtensionRequestResult) {
                let state = if result == OSSystemExtensionRequestResult::WillCompleteAfterReboot {
                    State::Failed("Restart your Mac to finish installing the VPN".into())
                } else {
                    State::Active
                };
                finish(request, state);
            }

            #[unsafe(method(request:didFailWithError:))]
            fn failed(&self, request: &OSSystemExtensionRequest, error: &NSError) {
                let canceled = OSSystemExtensionErrorCode(error.code()) == OSSystemExtensionErrorCode::RequestCanceled;
                let kept = KEPT_ACTIVE.with(|k| k.replace(false));
                finish(request, if canceled && kept { State::Active } else { State::Failed(describe(error)) });
            }
        }
    );
}
