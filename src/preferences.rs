use std::cell::Cell;

use global_hotkey::hotkey::HotKey;
use objc2::{ClassType, DefinedClass, MainThreadOnly, define_class, extern_class, msg_send, rc::{Retained, Weak}, runtime::{ProtocolObject, Sel}, sel};
use objc2_app_kit::*;
use objc2_foundation::*;

use crate::{mac::AppDelegate, shortcut};
use crate::localization as loc;

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

extern_class!(
    #[unsafe(super(NSObject))]
    struct SMAppService;
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginStatus { Off, Enabled, RequiresApproval, Unavailable }

impl SMAppService {
    fn main_app() -> Retained<Self> {
        // SMAppService.mainAppService is available on the app's minimum macOS version (13).
        unsafe { msg_send![Self::class(), mainAppService] }
    }

    fn status(&self) -> LoginStatus {
        let status: isize = unsafe { msg_send![self, status] };
        match status {
            0 => LoginStatus::Off,
            1 => LoginStatus::Enabled,
            2 => LoginStatus::RequiresApproval,
            _ => LoginStatus::Unavailable,
        }
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), Retained<NSError>> {
        // Both public selectors use BOOL with an NSError** out parameter.
        unsafe {
            if enabled { msg_send![self, registerAndReturnError: _] }
            else { msg_send![self, unregisterAndReturnError: _] }
        }
    }
}

pub(crate) fn login_status() -> LoginStatus {
    SMAppService::main_app().status()
}

pub(crate) fn apply_login_default() {
    if NSBundle::mainBundle().bundleIdentifier().is_none() { return; }
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = ns_string!("DidApplyLoginDefault");
    if defaults.boolForKey(key) { return; }
    // One-time default. The system's login item is the authoritative on/off state.
    // Removing or disabling it later, in either UI, must survive a relaunch.
    defaults.setBool_forKey(true, key);
    let service = SMAppService::main_app();
    // On a fresh signed app, macOS can return NotFound until its first registration.
    if matches!(service.status(), LoginStatus::Off | LoginStatus::Unavailable)
        && let Err(error) = service.set_enabled(true)
    {
        eprintln!("Could not enable launch at login: {}", error.localizedDescription());
    }
}

pub(crate) struct SettingsState {
    app: Weak<AppDelegate>,
    recording: Cell<bool>,
    login: Retained<NSButton>,
    login_status: Retained<NSTextField>,
    system_settings: Retained<NSButton>,
    current_key: Retained<NSTextField>,
    record: Retained<NSButton>,
    shortcut_status: Retained<NSTextField>,
    reset: Retained<NSButton>,
}

define_class!(
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SettingsState]
    pub(crate) struct SettingsWindow;

    unsafe impl NSObjectProtocol for SettingsWindow {}
    unsafe impl NSWindowDelegate for SettingsWindow {
        #[unsafe(method(windowDidBecomeKey:))]
        fn became_key(&self, _notification: &NSNotification) { self.refresh_login(None); }

        #[unsafe(method(windowDidResignKey:))]
        fn resigned_key(&self, _notification: &NSNotification) { self.cancel_recording(); }
    }

    impl SettingsWindow {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if self.is_recording() && event.r#type() == NSEventType::KeyDown {
                if event.isARepeat() { return; }
                if event.keyCode() == 53 {
                    self.cancel_recording();
                } else {
                    match shortcut::from_event(event) {
                        Ok(hotkey) => self.apply_shortcut(hotkey),
                        Err(message) => self.set_shortcut_status(message),
                    }
                }
                return;
            }
            // All other events keep the standard NSWindow behavior.
            unsafe { msg_send![super(self), sendEvent: event] }
        }

        #[unsafe(method(toggleLogin:))]
        fn toggle_login(&self, _sender: &NSButton) {
            self.cancel_recording();
            let service = SMAppService::main_app();
            let status = service.status();
            let enabled = !matches!(status, LoginStatus::Enabled | LoginStatus::RequiresApproval);
            let result = service.set_enabled(enabled);
            let effective = service.status();
            let error = result.err().map(|error| {
                eprintln!("Could not change launch at login: {}", error.localizedDescription());
                if enabled { "Could not enable launch at login. Check System Settings." }
                else { "Could not turn it off. Check Login Items in System Settings." }
            });
            self.refresh_login(if effective == LoginStatus::RequiresApproval && enabled { None } else { error });
        }

        #[unsafe(method(openLoginItems:))]
        fn open_login_items(&self, _sender: &NSButton) {
            // Documented ServiceManagement entry to the system's login-item settings.
            unsafe { msg_send![SMAppService::class(), openSystemSettingsLoginItems] }
        }

        #[unsafe(method(recordShortcut:))]
        fn record_shortcut(&self, _sender: &NSButton) {
            if self.is_recording() {
                self.cancel_recording();
            } else {
                self.ivars().recording.set(true);
                self.ivars().record.setTitle(&loc::text("Cancel Edit"));
                self.set_shortcut_status("Press a new shortcut or press Esc to cancel.");
            }
        }

        #[unsafe(method(resetShortcut:))]
        fn reset_shortcut(&self, _sender: &NSButton) {
            self.cancel_recording();
            self.apply_shortcut(shortcut::default_hotkey());
        }
    }
);

impl SettingsWindow {
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn verify_login_toggle(&self) {
        self.refresh_login(None);
        assert_eq!(self.ivars().login.state(), 1);
        for expected in [0, 1, 0] {
            // Invoke the control's installed target/action in this test bundle.
            unsafe { self.ivars().login.performClick(None); }
            assert_eq!(self.ivars().login.state(), expected);
        }
    }

    pub(crate) fn new(app: &AppDelegate) -> Retained<Self> {
        let mtm = app.mtm();
        // Assign the target after initializing the retained window.
        let login = unsafe {
            NSButton::checkboxWithTitle_target_action(&loc::text("Open Qingjie when you log in"), None, None, mtm)
        };
        let window = Self::alloc(mtm).set_ivars(SettingsState {
            app: Weak::new(app),
            recording: Cell::new(false),
            login,
            login_status: label(mtm, "", 12.0, true),
            system_settings: button(mtm, "System Settings…"),
            current_key: label(mtm, "", 16.0, false),
            record: button(mtm, "Change…"),
            shortcut_status: label(mtm, "", 12.0, true),
            reset: button(mtm, "Reset"),
        });
        let bounds = rect(0.0, 0.0, 462.0, 192.0);
        let window: Retained<Self> = unsafe { msg_send![super(window),
            initWithContentRect: bounds,
            styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ] };
        // AppDelegate retains this window so it can be reopened with the same entry point.
        unsafe { window.setReleasedWhenClosed(false); }
        window.setTitle(&loc::text("Qingjie Settings"));
        window.setDelegate(Some(ProtocolObject::from_ref(&*window)));
        window.setLevel(NSFloatingWindowLevel);
        let content = NSView::initWithFrame(NSView::alloc(mtm), bounds);
        window.setContentView(Some(&content));
        let views = window.ivars();

        let heading = label(mtm, "Launch at Login", 14.0, false);
        heading.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
        place(&content, &heading, rect(24.0, 150.0, 232.0, 21.0));
        window.place_button(&content, &views.login, sel!(toggleLogin:), rect(24.0, 119.0, 289.0, 25.0));
        place(&content, &views.login_status, rect(24.0, 94.0, 414.0, 21.0));
        window.place_button(&content, &views.system_settings, sel!(openLoginItems:), rect(305.0, 117.0, 133.0, 29.0));

        let heading = label(mtm, "Shortcut", 14.0, false);
        heading.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
        place(&content, &heading, rect(24.0, 50.0, 104.0, 24.0));
        place(&content, &views.current_key, rect(128.0, 50.0, 130.0, 24.0));
        window.place_button(&content, &views.record, sel!(recordShortcut:), rect(253.0, 47.0, 91.0, 30.0));
        window.place_button(&content, &views.reset, sel!(resetShortcut:), rect(345.0, 47.0, 93.0, 30.0));
        place(&content, &views.shortcut_status, rect(24.0, 20.0, 414.0, 23.0));
        window.center();
        window
    }

    fn place_button(&self, content: &NSView, button: &NSButton, action: Sel, frame: NSRect) {
        // The selectors above take the sending NSButton. Its target is weak.
        unsafe { button.setTarget(Some(self)); button.setAction(Some(action)); }
        place(content, button, frame);
    }

    pub(crate) fn show(&self) {
        self.refresh_login(None);
        self.refresh_shortcut();
        let app = NSApplication::sharedApplication(self.mtm());
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        self.makeKeyAndOrderFront(None);
    }

    pub(crate) fn is_recording(&self) -> bool { self.ivars().recording.get() }

    pub(crate) fn cancel_recording(&self) {
        if self.ivars().recording.replace(false) {
            self.ivars().record.setTitle(&loc::text("Change…"));
            self.refresh_shortcut();
        }
    }

    fn refresh_login(&self, message: Option<&str>) {
        let status = login_status();
        let views = self.ivars();
        views.login.setState(match status {
            LoginStatus::Enabled => 1,
            LoginStatus::RequiresApproval => -1,
            _ => 0,
        });
        views.system_settings.setHidden(message.is_none() && status != LoginStatus::RequiresApproval && status != LoginStatus::Unavailable);
        let message = message.unwrap_or(match status {
            LoginStatus::Enabled => "On. Takes effect the next time you log in.",
            LoginStatus::Off => "Off.",
            LoginStatus::RequiresApproval => "Allow Qingjie to open in System Settings.",
            LoginStatus::Unavailable => "Login item unavailable. Check System Settings.",
        });
        views.login_status.setStringValue(&loc::text(message));
    }

    fn refresh_shortcut(&self) {
        let active = self.ivars().app.load().and_then(|app| app.active_hotkey());
        let configured = active.unwrap_or_else(shortcut::configured_hotkey);
        self.ivars().current_key.setStringValue(&NSString::from_str(&shortcut::label(configured)));
        self.ivars().reset.setEnabled(active != Some(shortcut::default_hotkey()));
        let message = if active.is_some() { "Changes take effect immediately." } else { "Shortcut unavailable. Change it or capture from the menu." };
        self.set_shortcut_status(message);
    }

    fn set_shortcut_status(&self, text: &str) {
        self.ivars().shortcut_status.setStringValue(&loc::text(text));
    }

    fn apply_shortcut(&self, hotkey: HotKey) {
        let Some(app) = self.ivars().app.load() else { return; };
        match app.set_hotkey(hotkey) {
            Ok(note) => {
                self.cancel_recording();
                self.refresh_shortcut();
                self.set_shortcut_status(note.unwrap_or("Saved. You can use the shortcut now."));
            }
            Err(message) => self.set_shortcut_status(&message),
        }
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn place(content: &NSView, view: &NSView, frame: NSRect) {
    view.setFrame(frame);
    content.addSubview(view);
}

fn label(mtm: MainThreadMarker, text: &str, size: f64, secondary: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&loc::text(text), mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(size)));
    if secondary { field.setTextColor(Some(&NSColor::secondaryLabelColor())); }
    field
}

fn button(mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
    // The action and the target are set when the button is attached to the window.
    unsafe { NSButton::buttonWithTitle_target_action(&loc::text(title), None, None, mtm) }
}
