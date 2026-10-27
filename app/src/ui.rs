//! The single Objective-C controller that AppKit and macOS call back into (app delegate,
//! menu delegate, menu items, VPN status changes, the update window and notification), plus
//! small AppKit helpers.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSColor, NSFontWeightMedium, NSImage, NSImageSymbolConfiguration, NSMenu,
    NSMenuDelegate,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSNotification, NSObject, NSObjectProtocol};
use objc2_user_notifications::{
    UNNotification, UNNotificationPresentationOptions, UNNotificationResponse, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::util::ns;

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "TunnelController"]
    pub struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            crate::app::launched(self.mtm());
        }
    }

    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, menu: &NSMenu) {
            crate::app::menu_will_open(menu);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            crate::app::menu_did_close();
        }
    }

    unsafe impl UNUserNotificationCenterDelegate for Controller {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // Show the banner even while the update window is in front.
            handler.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &block2::DynBlock<dyn Fn()>,
        ) {
            crate::update_ui::notification_response(&response.actionIdentifier().to_string());
            handler.call(());
        }
    }

    impl Controller {
        /// The update window's buttons.
        #[unsafe(method(updateButton:))]
        fn update_button(&self, sender: Option<&AnyObject>) {
            let tag: isize = match sender {
                Some(s) => unsafe { msg_send![s, tag] },
                None => 0,
            };
            crate::update_ui::button(tag);
        }

        #[unsafe(method(command:))]
        fn command(&self, sender: Option<&AnyObject>) {
            let tag: isize = match sender {
                Some(s) => unsafe { msg_send![s, tag] },
                None => 0,
            };
            crate::app::handle_tag(tag);
        }

        #[unsafe(method(vpnStatusChanged:))]
        fn vpn_status_changed(&self, _n: &NSNotification) {
            crate::app::vpn_status_changed();
        }
    }
);

thread_local! {
    static CONTROLLER: OnceCell<Retained<Controller>> = const { OnceCell::new() };
}

/// The single controller instance (target of every menu item).
pub fn controller(mtm: MainThreadMarker) -> Retained<Controller> {
    CONTROLLER.with(|c| {
        c.get_or_init(|| {
            let this = Controller::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

pub fn app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    NSApplication::sharedApplication(mtm)
}

pub fn set_app_delegate(mtm: MainThreadMarker) {
    let c = controller(mtm);
    app(mtm).setDelegate(Some(ProtocolObject::from_ref(&*c)));
}

pub fn menu_delegate(mtm: MainThreadMarker) -> Retained<ProtocolObject<dyn NSMenuDelegate>> {
    ProtocolObject::from_retained(controller(mtm))
}

/// An SF Symbol, optionally sized.
pub fn symbol(name: &str, points: f64) -> Option<Retained<NSImage>> {
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), None)?;
    if points > 0.0 {
        let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(points, unsafe { NSFontWeightMedium });
        return img.imageWithSymbolConfiguration(&cfg).or(Some(img));
    }
    Some(img)
}

/// A coloured state symbol (Photon's vocabulary: green = fine, orange = needs attention).
/// Filled symbols get a white glyph on the colour.
pub fn state_symbol(name: &str, color: Retained<NSColor>) -> Option<Retained<NSImage>> {
    let img = symbol(name, 0.0)?;
    let colors = if name.ends_with(".fill") {
        NSArray::from_retained_slice(&[NSColor::whiteColor(), color])
    } else {
        NSArray::from_retained_slice(&[color])
    };
    let cfg = NSImageSymbolConfiguration::configurationWithPaletteColors(&colors);
    Some(img.imageWithSymbolConfiguration(&cfg).unwrap_or(img))
}
