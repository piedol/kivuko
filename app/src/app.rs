//! App lifecycle, the menu-bar item, and the connect flow:
//! save the VPN configuration → fetch the WireGuard key → start the tunnel.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplicationActivationPolicy, NSColor,
    NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem, NSModalResponse, NSSecureTextField, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_foundation::{NSNotificationCenter, NSPoint, NSRect, NSSize, NSString, NSURL};
use objc2_network_extension::{NEVPNStatus, NEVPNStatusDidChangeNotification};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use shared::{Dns, Status};

use crate::extension::{self, State as Tunnel};
use crate::prefs::{self, Place};
use crate::util::{self, ns};
use crate::{countries, keychain, ui, update_ui, vpn};

mod tag {
    pub const CONNECT: isize = 1;
    pub const DISCONNECT: isize = 2;
    pub const FASTEST: isize = 3;
    pub const KILL_SWITCH: isize = 4;
    pub const AUTO_CONNECT: isize = 5;
    pub const BLOCK_ADS: isize = 6;
    pub const LOGIN: isize = 7;
    pub const QUIT: isize = 8;
    pub const TOKEN: isize = 11;
    pub const ALLOW_TUNNEL: isize = 12;
    pub const ACTIVATE_TUNNEL: isize = 13;
    /// Check for Updates…, Update to Kivuko X…, or the install in progress (release builds).
    pub const UPDATE: isize = 14;
    pub const CHECK_UPDATES: isize = 15;
    pub const COUNTRY: isize = 100_000;
}

/// Something the user can fix, shown as an orange row in the menu.
#[derive(Clone, Debug, PartialEq)]
enum Problem {
    NoToken,
    TokenRejected,
    ConnectFailed(String),
}

struct Shell {
    item: Retained<NSStatusItem>,
    /// The status row and the Connect/Disconnect row, updated in place while the menu is
    /// open (rebuilding an open menu closes it).
    header: Option<Retained<NSMenuItem>>,
    action: Option<Retained<NSMenuItem>>,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
    static PROBLEM: RefCell<Option<Problem>> = const { RefCell::new(None) };
    static STATUS: RefCell<Option<Status>> = const { RefCell::new(None) };
    static MENU_OPEN: Cell<bool> = const { Cell::new(false) };
    /// Settings changed while connected: start again once the tunnel is down.
    static RECONNECT: Cell<bool> = const { Cell::new(false) };
    /// Auto-Connect at launch waits for macOS's first real status (it reports Invalid
    /// for a moment after loading, even while the tunnel is up).
    static LAUNCH_CHECK: Cell<bool> = const { Cell::new(false) };
    /// A Connect that came before macOS had activated the system extension. It runs once
    /// the tunnel is active.
    static CONNECT_WHEN_READY: Cell<bool> = const { Cell::new(false) };
    /// The VPN configuration is being saved again for the system extension.
    static ADOPTING: Cell<bool> = const { Cell::new(false) };
    /// A modal alert (the token prompt) is open.
    static MODAL: Cell<bool> = const { Cell::new(false) };
    /// How far an update has got with turning the VPN off (see `ready_for_update`).
    static UPDATE_STOP: Cell<UpdateStop> = const { Cell::new(UpdateStop::No) };
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("Kivuko must start on the main thread");
    let app = ui::app(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    ui::set_app_delegate(mtm);
    app.run();
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("UI work must run on the main thread")
}

/// A main menu nobody sees, so ⌘V and friends work in the token field.
fn install_main_menu(mtm: MainThreadMarker) {
    let item = |title: &str, action: objc2::runtime::Sel, key: &str| unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(action), &ns(key))
    };
    let main = NSMenu::new(mtm);
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Edit"));
    edit.addItem(&item("Undo", sel!(undo:), "z"));
    edit.addItem(&item("Cut", sel!(cut:), "x"));
    edit.addItem(&item("Copy", sel!(copy:), "c"));
    edit.addItem(&item("Paste", sel!(paste:), "v"));
    edit.addItem(&item("Select All", sel!(selectAll:), "a"));
    for (title, sub) in [("Kivuko", NSMenu::new(mtm)), ("Edit", edit)] {
        let top = item(title, sel!(submenuAction:), "");
        top.setSubmenu(Some(&sub));
        main.addItem(&top);
    }
    ui::app(mtm).setMainMenu(Some(&main));
}

/// Called from `applicationDidFinishLaunching:`.
pub fn launched(mtm: MainThreadMarker) {
    install_main_menu(mtm);
    build_status_item(mtm);
    countries::refresh_async();
    // Developer ID builds: the system extension must be active before the VPN can start.
    extension::activate();
    let center = NSNotificationCenter::defaultCenter();
    unsafe {
        center.addObserver_selector_name_object(
            &ui::controller(mtm),
            sel!(vpnStatusChanged:),
            Some(NEVPNStatusDidChangeNotification),
            None,
        );
    }
    vpn::load(move || {
        log::info!("launched; VPN configuration {}", if vpn::exists() { "found" } else { "not set up yet" });
        refresh();
        // Auto-Connect also means connecting when Kivuko starts (Quit turns the VPN off).
        if prefs::get().auto_connect && vpn::exists() {
            LAUNCH_CHECK.with(|c| c.set(true));
            launch_auto_connect(false);
            // If macOS never reports a status change, decide after a few seconds.
            util::main_after(3.0, || launch_auto_connect(true));
        }
        adopt_configuration();
    });
    update_ui::launched();
    // `KIVUKO_SHOW_MENU=<seconds>` opens the menu that long after launch, for README
    // screenshots.
    if let Some(delay) = std::env::var("KIVUKO_SHOW_MENU").ok().map(|v| v.parse::<f64>().unwrap_or(6.0)) {
        util::main_after(delay, || {
            let item = SHELL.with(|sh| sh.borrow().as_ref().map(|s| s.item.clone()));
            if let Some(button) = item.and_then(|i| i.button(self::mtm())) {
                unsafe { button.performClick(None) };
            }
        });
    }
}

fn build_status_item(mtm: MainThreadMarker) {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.setDelegate(Some(&ui::menu_delegate(mtm)));
    item.setMenu(Some(&menu));
    SHELL.with(|sh| *sh.borrow_mut() = Some(Shell { item, header: None, action: None }));
    refresh_icon();
}

fn set_problem(p: Option<Problem>) {
    if let Some(p) = &p {
        log::warn!("problem: {p:?}");
    }
    PROBLEM.with(|c| *c.borrow_mut() = p);
    refresh();
}

/// Update the icon, and the menu's live rows if it's open. A closed menu is rebuilt the
/// next time it opens.
pub fn refresh() {
    refresh_icon();
    if MENU_OPEN.with(Cell::get) {
        update_header();
        update_action();
    }
}

fn refresh_icon() {
    let mtm = mtm();
    let status = vpn::status();
    let (symbol, dim, tip) = match status {
        NEVPNStatus::Connected => ("checkmark.shield", false, "Kivuko · connected"),
        NEVPNStatus::Connecting | NEVPNStatus::Reasserting => ("shield", true, "Kivuko · connecting"),
        NEVPNStatus::Disconnecting => ("shield", true, "Kivuko · disconnecting"),
        _ => ("shield.slash", false, "Kivuko · not connected"),
    };
    SHELL.with(|sh| {
        let sh = sh.borrow();
        let Some(sh) = sh.as_ref() else { return };
        let Some(button) = sh.item.button(mtm) else { return };
        if let Some(img) = ui::symbol(symbol, 0.0) {
            img.setTemplate(true);
            button.setImage(Some(&img));
        }
        button.setAppearsDisabled(dim);
        button.setToolTip(Some(&ns(tip)));
    });
}

/// Auto-Connect when Kivuko starts: connect only if the tunnel is really down.
fn launch_auto_connect(give_up_waiting: bool) {
    if !LAUNCH_CHECK.with(Cell::get) {
        return;
    }
    match vpn::status() {
        NEVPNStatus::Disconnected => {}
        NEVPNStatus::Invalid if give_up_waiting => {}
        NEVPNStatus::Invalid => return,
        _ => {
            LAUNCH_CHECK.with(|c| c.set(false));
            return adopt_configuration();
        }
    }
    LAUNCH_CHECK.with(|c| c.set(false));
    log::info!("connecting at launch (Auto-Connect)");
    connect();
}

/// The system extension's activation moved on (Developer ID builds).
#[cfg_attr(not(feature = "sysext"), allow(dead_code))]
pub fn extension_changed() {
    if extension::ready() {
        if CONNECT_WHEN_READY.with(|c| c.replace(false)) {
            connect();
        } else {
            adopt_configuration();
        }
    }
    refresh();
}

/// The first time a Developer ID build runs, save the VPN configuration again, so it points
/// at the system extension. The tunnel keeps its bundle ID, so the settings don't change.
/// A Connect saves it anyway, so this waits while one is on its way.
fn adopt_configuration() {
    let waiting = LAUNCH_CHECK.with(Cell::get) || CONNECT_WHEN_READY.with(Cell::get) || ADOPTING.with(Cell::get);
    if !extension::SYSTEM_EXTENSION
        || waiting
        || !extension::ready()
        || !vpn::exists()
        || prefs::get().saved_for_system_extension
    {
        return;
    }
    log::info!("saving the VPN configuration for the system extension");
    ADOPTING.with(|a| a.set(true));
    vpn::save(&prefs::get(), vpn::on_demand(), |result| {
        ADOPTING.with(|a| a.set(false));
        if let Err(e) = result {
            set_problem(Some(Problem::ConnectFailed(format!("macOS didn't save the VPN configuration: {e}"))));
        }
        refresh();
    });
}

pub fn vpn_status_changed() {
    let status = vpn::status();
    log::info!("VPN status {}", status.0);
    launch_auto_connect(false);
    match status {
        NEVPNStatus::Connected => {
            if matches!(PROBLEM.with(|p| p.borrow().clone()), Some(Problem::ConnectFailed(_))) {
                set_problem(None);
            }
            // The tunnel now has its key, so On-Demand starts will work.
            if prefs::get().auto_connect {
                vpn::set_on_demand(true, refresh);
            }
            fetch_status();
        }
        NEVPNStatus::Disconnected => {
            STATUS.with(|s| *s.borrow_mut() = None);
            if RECONNECT.with(|r| r.replace(false)) {
                start_tunnel(None);
            } else {
                vpn::last_error(|e| {
                    let Some(e) = e else { return };
                    if shared::is_setup_error(&e) {
                        // Retrying can't fix these; with the kill switch on, a retry loop
                        // would also keep blocking the internet. Stop until the user acts.
                        vpn::set_on_demand(false, refresh);
                        let token_problem = e.contains("access token");
                        set_problem(Some(if token_problem && keychain::has_token() {
                            Problem::TokenRejected
                        } else if token_problem {
                            Problem::NoToken
                        } else {
                            Problem::ConnectFailed(e)
                        }));
                    } else if vpn::on_demand() {
                        // Auto-Connect is already bringing it back; not worth a warning.
                        log::info!("tunnel stopped ({e}); Auto-Connect will reconnect");
                    } else {
                        set_problem(Some(Problem::ConnectFailed(e)));
                    }
                });
            }
        }
        _ => {}
    }
    refresh();
}

// ---------------------------------------------------------------- connecting

/// Connect, setting up whatever's missing on the way.
pub fn connect() {
    if matches!(vpn::status(), NEVPNStatus::Connected | NEVPNStatus::Connecting | NEVPNStatus::Reasserting) {
        return refresh();
    }
    if !extension::ready() {
        log::info!("connect waits until macOS has activated the tunnel");
        CONNECT_WHEN_READY.with(|c| c.set(true));
        return refresh();
    }
    let Some(token) = keychain::token() else {
        ask_for_token();
        return;
    };
    // On-Demand stays off until the tunnel is up (see vpn_status_changed): a start macOS
    // makes on its own can't carry the token, so the first start must come from here.
    let p = prefs::get();
    vpn::save(&p, false, move |result| match result {
        Ok(()) => start_tunnel(Some(token)),
        Err(e) => set_problem(Some(Problem::ConnectFailed(format!("macOS didn't save the VPN configuration: {e}")))),
    });
}

fn start_tunnel(token: Option<String>) {
    match vpn::start(token.as_deref()) {
        Ok(()) => {
            if matches!(PROBLEM.with(|p| p.borrow().clone()), Some(Problem::ConnectFailed(_))) {
                PROBLEM.with(|p| *p.borrow_mut() = None);
            }
        }
        Err(e) => PROBLEM.with(|p| *p.borrow_mut() = Some(Problem::ConnectFailed(e))),
    }
    refresh();
}

/// Disconnect and stay disconnected: On-Demand off until the next Connect.
fn disconnect(then: impl FnOnce() + 'static) {
    vpn::set_on_demand(false, move || {
        vpn::stop();
        then();
    });
}

/// Quit turns the VPN off too.
pub fn quit() {
    let terminate = || ui::app(mtm()).terminate(None);
    let active = !matches!(vpn::status(), NEVPNStatus::Disconnected | NEVPNStatus::Invalid);
    if vpn::exists() && (active || vpn::on_demand()) {
        disconnect(terminate);
    } else {
        terminate();
    }
}

/// Save changed preferences into the VPN configuration; reconnect if the tunnel is up.
fn apply_prefs() {
    if !vpn::exists() {
        refresh();
        return;
    }
    let p = prefs::get();
    let active = matches!(vpn::status(), NEVPNStatus::Connected | NEVPNStatus::Connecting | NEVPNStatus::Reasserting);
    vpn::save(&p, vpn::on_demand(), move |result| {
        if let Err(e) = result {
            set_problem(Some(Problem::ConnectFailed(e)));
        } else if active {
            RECONNECT.with(|r| r.set(true));
            vpn::stop();
        }
        refresh();
    });
}

fn open_url(url: &str) {
    if let Some(u) = NSURL::URLWithString(&ns(url)) {
        NSWorkspace::sharedWorkspace().openURL(&u);
    }
}

/// Run a modal alert. An update doesn't replace the app while one is open.
fn run_modal(alert: &NSAlert) -> NSModalResponse {
    MODAL.with(|m| m.set(true));
    let response = alert.runModal();
    MODAL.with(|m| m.set(false));
    response
}

/// Ask for the Nord access token and save it in the login keychain.
fn ask_for_token() {
    let mtm = mtm();
    let rejected = PROBLEM.with(|p| *p.borrow() == Some(Problem::TokenRejected));
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(if rejected { "Replace Your NordVPN Token" } else { "Add Your NordVPN Token" }));
    alert.setInformativeText(&ns(
        "Kivuko uses it to get your WireGuard key from NordVPN, and keeps it in your login keychain.\n\n\
         In Nord Account, open NordVPN › Advanced settings › Access token › Get Access token, \
         then copy the token and paste it here.",
    ));
    let field = NSSecureTextField::initWithFrame(
        NSSecureTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 24.0)),
    );
    field.setPlaceholderString(Some(&ns("Paste your access token")));
    alert.setAccessoryView(Some(&field));
    alert.addButtonWithTitle(&ns("Save and Connect"));
    alert.addButtonWithTitle(&ns("Cancel"));
    alert.addButtonWithTitle(&ns("Open Nord Account"));
    alert.window().setInitialFirstResponder(Some(&field));
    #[allow(deprecated)]
    ui::app(mtm).activateIgnoringOtherApps(true);
    let response = run_modal(&alert);
    if response == NSAlertSecondButtonReturn {
        if !keychain::has_token() {
            set_problem(Some(Problem::NoToken));
        }
        return;
    }
    if response != NSAlertFirstButtonReturn {
        open_url("https://my.nordaccount.com/");
        return;
    }
    let token = field.stringValue().to_string();
    if !nord::looks_like_token(&token) {
        let oops = NSAlert::new(mtm);
        oops.setMessageText(&ns("That doesn't look like a NordVPN token"));
        oops.setInformativeText(&ns("Copy the whole access token from Nord Account and paste it in, with no spaces."));
        run_modal(&oops);
        return ask_for_token();
    }
    if let Err(e) = keychain::set_token(&token) {
        set_problem(Some(Problem::ConnectFailed(format!("Couldn't save the token in your keychain: {e}"))));
        return;
    }
    set_problem(None);
    connect();
}

// ---------------------------------------------------------------- menu

fn menu_item(mtm: MainThreadMarker, title: &str, tag: isize) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &ns(title),
            Some(sel!(command:)),
            &NSString::new(),
        )
    };
    let target = ui::controller(mtm);
    unsafe { item.setTarget(Some(&target)) };
    item.setTag(tag);
    item
}

fn toggle(mtm: MainThreadMarker, title: &str, tag: isize, on: bool, tip: &str) -> Retained<NSMenuItem> {
    let item = menu_item(mtm, title, tag);
    item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    item.setToolTip(Some(&ns(tip)));
    item
}

pub fn menu_did_close() {
    MENU_OPEN.with(|o| o.set(false));
}

pub fn menu_will_open(menu: &NSMenu) {
    MENU_OPEN.with(|o| o.set(true));
    build_menu(menu);
    fetch_status();
}

/// Ask the tunnel which server it's on; update the menu when it answers.
fn fetch_status() {
    vpn::request_status(|status| {
        STATUS.with(|s| *s.borrow_mut() = Some(status));
        update_header();
    });
}

/// Title and subtitle of the status row.
fn header_text() -> (String, String) {
    let p = prefs::get();
    let place = p.country.as_ref().map(|c| c.name.clone());
    let down = matches!(vpn::status(), NEVPNStatus::Disconnected | NEVPNStatus::Invalid);
    if down && !extension::ready() {
        let title = if vpn::exists() { "Not Connected" } else { "Not Set Up" };
        let detail = match extension::state() {
            Tunnel::NeedsApproval => "Waiting for you to allow the VPN in System Settings",
            Tunnel::Failed(_) => "macOS hasn't started Kivuko's VPN",
            _ => "Starting Kivuko's VPN",
        };
        return (title.into(), detail.into());
    }
    if !vpn::exists() {
        return ("Not Set Up".into(), "Connect once to install the VPN".into());
    }
    match vpn::status() {
        NEVPNStatus::Connected => {
            let status = STATUS.with(|s| s.borrow().clone());
            let time = vpn::connected_secs().map(|s| if s < 60 { "just now".to_string() } else { util::duration(s) });
            let detail = match (status.as_ref().and_then(|s| s.server.clone()), time) {
                (Some(server), Some(t)) => format!("{server} · {t}"),
                (Some(server), None) => server,
                (None, Some(t)) if t == "just now" => "Just now".into(),
                (None, Some(t)) => format!("For {t}"),
                (None, None) => String::new(),
            };
            ("Connected".into(), detail)
        }
        NEVPNStatus::Connecting => (
            "Connecting…".into(),
            place.map_or("Finding the fastest server".into(), |c| format!("Finding a server in {c}")),
        ),
        NEVPNStatus::Reasserting => ("Reconnecting…".into(), "The connection dropped; finding a way back".into()),
        NEVPNStatus::Disconnecting => ("Disconnecting…".into(), String::new()),
        _ => ("Not Connected".into(), "Your traffic isn't going through the VPN".into()),
    }
}

fn update_header() {
    let header = SHELL.with(|sh| sh.borrow().as_ref().and_then(|s| s.header.clone()));
    let Some(header) = header else { return };
    let (title, subtitle) = header_text();
    let (symbol, color) = match vpn::status() {
        NEVPNStatus::Connected => ("checkmark.shield.fill", NSColor::systemGreenColor()),
        _ => ("shield", NSColor::secondaryLabelColor()),
    };
    header.setImage(ui::state_symbol(symbol, color).as_deref());
    header.setTitle(&ns(&title));
    header.setSubtitle((!subtitle.is_empty()).then(|| ns(&subtitle)).as_deref());
    if let Some(s) = STATUS.with(|s| s.borrow().clone()) {
        let place = [s.city.clone(), s.hostname.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let data = format!("Received {} · sent {}", util::bytes(s.rx_bytes), util::bytes(s.tx_bytes));
        header.setToolTip(Some(&ns(&format!("{place}\n{data}"))));
    }
}

/// The one action that matters right now: Connect, Disconnect, Stop Connecting.
fn update_action() {
    let action = SHELL.with(|sh| sh.borrow().as_ref().and_then(|s| s.action.clone()));
    let Some(action) = action else { return };
    let (title, tag) = match vpn::status() {
        NEVPNStatus::Connected | NEVPNStatus::Reasserting => ("Disconnect", tag::DISCONNECT),
        NEVPNStatus::Connecting => ("Stop Connecting", tag::DISCONNECT),
        NEVPNStatus::Disconnecting => ("Disconnecting…", 0),
        _ if !vpn::exists() => ("Set Up and Connect", tag::CONNECT),
        _ => ("Connect", tag::CONNECT),
    };
    action.setTitle(&ns(title));
    action.setTag(tag);
    // Connect needs an active tunnel; Disconnect always works.
    action.setEnabled(tag != 0 && (tag != tag::CONNECT || extension::ready()));
}

fn build_menu(menu: &NSMenu) {
    let mtm = mtm();
    let p = prefs::get();
    let status = vpn::status();
    menu.removeAllItems();

    // Status, then the one action that matters right now.
    let header = menu_item(mtm, "", 0);
    unsafe { header.setAction(None) };
    menu.addItem(&header);
    SHELL.with(|sh| {
        if let Some(s) = sh.borrow_mut().as_mut() {
            s.header = Some(header.clone());
        }
    });
    update_header();

    let action = menu_item(mtm, "", 0);
    menu.addItem(&action);
    SHELL.with(|sh| {
        if let Some(s) = sh.borrow_mut().as_mut() {
            s.action = Some(action.clone());
        }
    });
    update_action();

    // Where to connect.
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let location = menu_item(mtm, "Location", 0);
    unsafe { location.setAction(None) };
    location.setSubtitle(Some(&ns(p.country.as_ref().map_or("Fastest server", |c| c.name.as_str()))));
    location.setSubmenu(Some(&location_menu(mtm, &p)));
    menu.addItem(&location);

    // Protection settings. Changing one while connected reconnects.
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&NSMenuItem::sectionHeaderWithTitle(&ns("Protection"), mtm));
    menu.addItem(&toggle(
        mtm,
        "Kill Switch",
        tag::KILL_SWITCH,
        p.kill_switch,
        "Blocks all internet traffic outside the VPN while it's on or reconnecting.\nYour local network still works.",
    ));
    menu.addItem(&toggle(
        mtm,
        "Auto-Connect",
        tag::AUTO_CONNECT,
        p.auto_connect,
        "Connects when Kivuko starts, on any network, and after sleep or a dropped connection, until you click Disconnect.",
    ));
    menu.addItem(&toggle(
        mtm,
        "Block Ads and Trackers",
        tag::BLOCK_ADS,
        p.dns == Dns::BlockAds,
        "Uses NordVPN's Threat Protection Lite DNS, which blocks ads, trackers and known malware sites.",
    ));

    // Problems, only while they exist. Each row fixes its own problem.
    let mut problems = Vec::new();
    let tunnel = extension::state();
    if tunnel != Tunnel::Active && !extension::in_applications() {
        let i = menu_item(mtm, "Move Kivuko to Applications", 0);
        unsafe { i.setAction(None) };
        i.setSubtitle(Some(&ns("macOS starts its VPN only from the Applications folder")));
        problems.push(i);
    } else if tunnel == Tunnel::NeedsApproval {
        let (_, pane) = extension::approval_pane();
        let i = menu_item(mtm, "Allow Kivuko's VPN in System Settings…", tag::ALLOW_TUNNEL);
        i.setSubtitle(Some(&ns(pane)));
        i.setToolTip(Some(&ns("macOS asks once before it runs Kivuko's VPN. Connect works once you allow it.")));
        problems.push(i);
    } else if let Tunnel::Failed(e) = tunnel {
        let i = menu_item(mtm, "Couldn't Start Kivuko's VPN", tag::ACTIVATE_TUNNEL);
        i.setSubtitle(Some(&ns(&e)));
        i.setToolTip(Some(&ns(&format!("{e}\n\nClick to try again."))));
        problems.push(i);
    }
    match PROBLEM.with(|p| p.borrow().clone()) {
        Some(Problem::NoToken) => {
            let i = menu_item(mtm, "Add Your NordVPN Token…", tag::TOKEN);
            i.setSubtitle(Some(&ns("Needed to get your WireGuard key")));
            problems.push(i);
        }
        Some(Problem::TokenRejected) => {
            let i = menu_item(mtm, "Replace Your NordVPN Token…", tag::TOKEN);
            i.setSubtitle(Some(&ns("NordVPN didn't accept the saved token")));
            problems.push(i);
        }
        Some(Problem::ConnectFailed(e)) if status != NEVPNStatus::Connected => {
            let i = menu_item(mtm, "Couldn't Connect", tag::CONNECT);
            i.setSubtitle(Some(&ns(&e)));
            i.setToolTip(Some(&ns(&format!("{e}\n\nClick to try again."))));
            problems.push(i);
        }
        _ => {}
    }
    if !problems.is_empty() {
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        for item in problems {
            item.setImage(ui::state_symbol("exclamationmark.triangle.fill", NSColor::systemOrangeColor()).as_deref());
            menu.addItem(&item);
        }
    }

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&toggle(mtm, "Launch at Login", tag::LOGIN, login_item_enabled(), "Start Kivuko when you log in."));
    if update_ui::offered() {
        menu.addItem(&toggle(
            mtm,
            "Check for Updates Automatically",
            tag::CHECK_UPDATES,
            p.check_updates,
            "Once a day, Kivuko asks GitHub whether there's a newer version.",
        ));
        let (title, subtitle, enabled) = update_ui::menu_item();
        let update = menu_item(mtm, &title, tag::UPDATE);
        update.setSubtitle(subtitle.map(|s| ns(&s)).as_deref());
        update.setEnabled(enabled);
        menu.addItem(&update);
    }
    let quit = menu_item(mtm, "Quit Kivuko", tag::QUIT);
    quit.setKeyEquivalent(&ns("q"));
    quit.setToolTip(Some(&ns("Turns the VPN off and quits.")));
    menu.addItem(&quit);
}

fn location_menu(mtm: MainThreadMarker, p: &prefs::Prefs) -> Retained<NSMenu> {
    let sub = NSMenu::new(mtm);
    sub.setAutoenablesItems(false);
    let current = p.country.as_ref().map(|c| c.id);
    let fastest = menu_item(mtm, "Fastest Server", tag::FASTEST);
    fastest.setSubtitle(Some(&ns("NordVPN's pick for where you are")));
    fastest.setState(if current.is_none() { NSControlStateValueOn } else { NSControlStateValueOff });
    sub.addItem(&fastest);
    let country_item = |c: &Place| {
        let i = menu_item(mtm, &c.name, tag::COUNTRY + c.id as isize);
        i.setState(if current == Some(c.id) { NSControlStateValueOn } else { NSControlStateValueOff });
        i
    };
    if !p.recent.is_empty() {
        sub.addItem(&NSMenuItem::separatorItem(mtm));
        sub.addItem(&NSMenuItem::sectionHeaderWithTitle(&ns("Recent"), mtm));
        for c in &p.recent {
            sub.addItem(&country_item(c));
        }
    }
    let all = countries::list();
    sub.addItem(&NSMenuItem::separatorItem(mtm));
    sub.addItem(&NSMenuItem::sectionHeaderWithTitle(&ns("All Countries"), mtm));
    if all.is_empty() {
        countries::refresh_async();
        let i = menu_item(mtm, "Loading countries…", 0);
        i.setEnabled(false);
        sub.addItem(&i);
    }
    for c in all {
        sub.addItem(&country_item(&Place { id: c.id, name: c.name }));
    }
    sub
}

pub fn handle_tag(t: isize) {
    match t {
        tag::CONNECT => connect(),
        tag::DISCONNECT => disconnect(|| {}),
        tag::FASTEST => {
            prefs::update(|p| p.choose_country(None));
            apply_prefs();
        }
        tag::KILL_SWITCH => {
            prefs::update(|p| p.kill_switch = !p.kill_switch);
            apply_prefs();
        }
        tag::AUTO_CONNECT => {
            let on = prefs::update(|p| p.auto_connect = !p.auto_connect).auto_connect;
            // On-Demand follows the switch only while the user wants to be connected.
            if vpn::exists() {
                let active = !matches!(vpn::status(), NEVPNStatus::Disconnected | NEVPNStatus::Invalid);
                vpn::set_on_demand(on && (active || vpn::on_demand()), refresh);
            }
        }
        tag::BLOCK_ADS => {
            prefs::update(|p| p.dns = if p.dns == Dns::BlockAds { Dns::Plain } else { Dns::BlockAds });
            apply_prefs();
        }
        tag::LOGIN => {
            let on = !login_item_enabled();
            if let Err(e) = set_login_item(on) {
                log::error!("couldn't change Launch at Login: {e}");
            }
            refresh();
        }
        tag::QUIT => quit(),
        tag::UPDATE => update_ui::menu_chosen(),
        tag::CHECK_UPDATES => {
            prefs::update(|p| p.check_updates = !p.check_updates);
            update_ui::setting_changed();
        }
        tag::TOKEN => ask_for_token(),
        tag::ALLOW_TUNNEL => open_url(extension::approval_pane().0),
        tag::ACTIVATE_TUNNEL => {
            extension::activate();
            refresh();
        }
        t if t >= tag::COUNTRY => {
            let id = (t - tag::COUNTRY) as u32;
            let name = countries::list()
                .into_iter()
                .find(|c| c.id == id)
                .map(|c| c.name)
                .or_else(|| prefs::get().recent.into_iter().find(|r| r.id == id).map(|r| r.name))
                .unwrap_or_else(|| format!("Country {id}"));
            prefs::update(|p| p.choose_country(Some(Place { id, name })));
            apply_prefs();
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- updates

/// How far an update has got with turning the VPN off.
#[derive(Clone, Copy, PartialEq)]
enum UpdateStop {
    No,
    /// Turning On-Demand off, since then.
    Asked(Instant),
    /// The tunnel was told to stop.
    Sent(Instant),
}

/// How long an update waits for the VPN to go off before it carries on anyway.
const UPDATE_STOP_WAIT: Duration = Duration::from_secs(20);

/// Nothing is half done that quitting would cut short: no connect, disconnect, reconnect or
/// save of the VPN configuration under way, the tunnel's activation settled, and no prompt open.
fn is_idle() -> bool {
    let changing = matches!(vpn::status(), NEVPNStatus::Connecting | NEVPNStatus::Disconnecting);
    let pending = [&LAUNCH_CHECK, &RECONNECT, &ADOPTING, &MODAL].iter().any(|flag| flag.with(Cell::get));
    !changing && !pending && !vpn::saving() && extension::state() != Tunnel::Activating
}

/// What an update does next, in `ready_for_update`.
#[derive(Debug, PartialEq)]
enum UpdateNext {
    Wait,
    StopVpn,
    Replace,
}

/// The decision behind `ready_for_update`, from what it has done so far and where Kivuko and
/// the VPN stand: idle, a VPN configuration exists, the VPN is down, On-Demand is on.
fn update_next(stop: UpdateStop, idle: bool, configured: bool, down: bool, on_demand: bool) -> UpdateNext {
    let waited = |since: Instant| since.elapsed() > UPDATE_STOP_WAIT;
    match stop {
        UpdateStop::Sent(since) if down || waited(since) => UpdateNext::Replace,
        UpdateStop::Asked(since) if waited(since) => UpdateNext::Replace,
        UpdateStop::Sent(_) | UpdateStop::Asked(_) => UpdateNext::Wait,
        UpdateStop::No if !idle => UpdateNext::Wait,
        UpdateStop::No if !configured || (down && !on_demand) => UpdateNext::Replace,
        UpdateStop::No => UpdateNext::StopVpn,
    }
}

/// An update asks this every second, once the download has passed its checks, until it
/// answers yes; then it replaces the app and quits it. Yes means Kivuko is idle and the VPN is
/// off. The first time the VPN is on (or On-Demand is armed), this turns it off the way Quit
/// does, so the new version's tunnel doesn't replace one that's carrying traffic, and the new
/// version's Auto-Connect finds it off and connects again.
pub fn ready_for_update() -> bool {
    let down = matches!(vpn::status(), NEVPNStatus::Disconnected | NEVPNStatus::Invalid);
    let stop = UPDATE_STOP.with(Cell::get);
    match update_next(stop, is_idle(), vpn::exists(), down, vpn::on_demand()) {
        UpdateNext::Wait => false,
        UpdateNext::Replace => true,
        UpdateNext::StopVpn => {
            log::info!("turning the VPN off for the update");
            let since = Instant::now();
            UPDATE_STOP.with(|s| s.set(UpdateStop::Asked(since)));
            disconnect(move || UPDATE_STOP.with(|s| s.set(UpdateStop::Sent(since))));
            false
        }
    }
}

/// Whether an update is turning the VPN off.
pub fn stopping_for_update() -> bool {
    UPDATE_STOP.with(Cell::get) != UpdateStop::No
}

/// The update failed after `ready_for_update` turned the VPN off: turn it on again.
pub fn update_not_installed() {
    if UPDATE_STOP.with(|s| s.replace(UpdateStop::No)) != UpdateStop::No {
        log::info!("the update wasn't installed; connecting again");
        connect();
    }
}

pub fn login_item_enabled() -> bool {
    let status = unsafe { SMAppService::mainAppService().status() };
    status == SMAppServiceStatus::Enabled
}

pub fn set_login_item(on: bool) -> Result<(), String> {
    let service = unsafe { SMAppService::mainAppService() };
    let result =
        if on { unsafe { service.registerAndReturnError() } } else { unsafe { service.unregisterAndReturnError() } };
    result.map_err(|e| e.localizedDescription().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_turns_the_vpn_off_before_it_replaces_the_app() {
        use UpdateNext::*;
        // Busy (connecting, saving, a prompt open): wait.
        assert_eq!(update_next(UpdateStop::No, false, true, true, false), Wait);
        // No VPN set up, or already off with On-Demand off: nothing to turn off.
        assert_eq!(update_next(UpdateStop::No, true, false, false, true), Replace);
        assert_eq!(update_next(UpdateStop::No, true, true, true, false), Replace);
        // Connected, or off with On-Demand armed: turn it off first.
        assert_eq!(update_next(UpdateStop::No, true, true, false, true), StopVpn);
        assert_eq!(update_next(UpdateStop::No, true, true, true, true), StopVpn);
        // Turning On-Demand off, or the tunnel still stopping: wait, even while busy.
        let now = Instant::now();
        assert_eq!(update_next(UpdateStop::Asked(now), false, true, true, false), Wait);
        assert_eq!(update_next(UpdateStop::Sent(now), false, true, false, false), Wait);
        // Down: replace.
        assert_eq!(update_next(UpdateStop::Sent(now), false, true, true, false), Replace);
    }

    #[test]
    fn an_update_stops_waiting_for_the_vpn_after_a_while() {
        let Some(long_ago) = Instant::now().checked_sub(UPDATE_STOP_WAIT + Duration::from_secs(1)) else { return };
        assert_eq!(update_next(UpdateStop::Asked(long_ago), false, true, false, true), UpdateNext::Replace);
        assert_eq!(update_next(UpdateStop::Sent(long_ago), false, true, false, false), UpdateNext::Replace);
    }
}
