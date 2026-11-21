use std::{cell::RefCell, io::Read, os::unix::fs::DirBuilderExt, path::PathBuf, process::{Child, Command, Stdio}};
use crate::editor::EditorController;
use crate::{gif_capture::Mode as GifMode, gif_window::GifWindow};
use crate::localization as loc;
use crate::{preferences::{self, SettingsWindow}, shortcut};

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use objc2::{define_class, msg_send, rc::Retained, runtime::{AnyObject, ProtocolObject, Sel}, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

struct Capture {
    child: Child,
    file: CaptureFile,
}

// Only the selected region is written here. No shared clipboard is touched before confirmation.
pub(crate) struct CaptureFile { directory: PathBuf, pub(crate) path: PathBuf }

impl CaptureFile {
    pub(crate) fn new(filename: &str) -> std::io::Result<Self> {
        let directory = std::env::temp_dir().join(format!("qingjie-{}", NSUUID::UUID().UUIDString()));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        Ok(Self { path: directory.join(filename), directory })
    }
}

impl Drop for CaptureFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

#[derive(Default)]
pub(crate) struct State {
    status: Option<Retained<NSStatusItem>>,
    message: Option<Retained<NSMenuItem>>,
    capture: Option<Capture>,
    editor: Option<Retained<EditorController>>,
    gif: Option<Retained<GifWindow>>,
    timer: Option<Retained<NSTimer>>,
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    settings: Option<Retained<SettingsWindow>>,
    modal: bool,
}

thread_local! {
    static DELEGATE: RefCell<Option<Retained<AppDelegate>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<State>]
    pub(crate) struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            preferences::apply_login_default();
            if std::env::args().any(|arg| arg == "--settings") { self.show_settings(); }
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            self.stop_capture();
        }
    }

    impl AppDelegate {
        #[unsafe(method(capture:))]
        fn capture_action(&self, _sender: Option<&AnyObject>) {
            self.start_capture();
        }

        #[unsafe(method(recordGif:))]
        fn record_gif(&self, _sender: Option<&AnyObject>) { self.start_gif(GifMode::Clip); }

        #[unsafe(method(manualGif:))]
        fn manual_gif(&self, _sender: Option<&AnyObject>) { self.start_gif(GifMode::Manual); }

        #[unsafe(method(pollCapture:))]
        fn poll_capture(&self, _timer: &NSTimer) {
            self.finish_if_ready();
        }

        #[unsafe(method(saveImage:))]
        fn save_image(&self, _sender: Option<&AnyObject>) {
            self.save_clipboard();
        }

        #[unsafe(method(openPermissions:))]
        fn open_permissions(&self, _sender: Option<&AnyObject>) {
            open_permissions();
        }

        #[unsafe(method(showSettings:))]
        fn settings_action(&self, _sender: Option<&AnyObject>) { self.show_settings(); }

        #[unsafe(method(showHelp:))]
        fn show_help(&self, _sender: Option<&AnyObject>) {
            let entry = self.active_hotkey().map_or_else(
                || loc::string("Choose Capture and Annotate from the menu"),
                |key| loc::format("Press {shortcut}", &[("shortcut", &shortcut::label(key))]),
            );
            let help = loc::format("{entry}, drag to select an area, and release to edit.\nUse the pen, rectangle, oval, or arrow. Click Copy or press Enter, then press ⌘V in another app.\nPress Esc to cancel, ⌘Z to undo, or ⌘S to save.\n\nOpen Settings from the menu to change your shortcut or launch at login.", &[("entry", &entry)]);
            alert(self.mtm(), "Qingjie", &help);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            self.stop_capture();
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(State::default()));
        // SAFETY: NSObject's initializer is called exactly once on this allocated subclass.
        unsafe { msg_send![super(this), init] }
    }

    fn setup(&self) {
        let mtm = self.mtm();
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let heading = NSMenuItem::new(mtm);
        heading.setEnabled(false);
        menu.addItem(&heading);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Capture and Annotate…", sel!(capture:));
        self.add_item(&menu, "Save Clipboard Image…", sel!(saveImage:));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Record Area as GIF…", sel!(recordGif:));
        self.add_item(&menu, "Build GIF Frame by Frame…", sel!(manualGif:));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Settings…", sel!(showSettings:));
        self.add_item(&menu, "Screen Recording Settings…", sel!(openPermissions:));
        self.add_item(&menu, "Help", sel!(showHelp:));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        self.add_item(&menu, "Quit Qingjie", sel!(quit:));

        let status = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0);
        status.setMenu(Some(&menu));
        if let Some(button) = status.button(mtm) {
            if let Some(icon) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("viewfinder"), Some(&loc::text("Qingjie screenshot")),
            ) {
                icon.setTemplate(true);
                button.setImage(Some(&icon));
            } else {
                button.setTitle(&loc::text("Q"));
            }
        }
        {
            let mut state = self.ivars().borrow_mut();
            state.status = Some(status);
            state.message = Some(heading);
        }

        GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
            // The Carbon callback runs on the main event loop. Never touch AppKit on another thread.
            if event.state == HotKeyState::Pressed && MainThreadMarker::new().is_some() {
                let app = DELEGATE.with(|slot| slot.borrow().clone());
                if let Some(app) = app { app.handle_hotkey(event.id()); }
            }
        }));
        if let Err(error) = self.register_hotkey(shortcut::configured_hotkey()) {
            eprintln!("Screenshot shortcut unavailable: {error}");
        }
        self.update_shortcut_label();
    }

    fn handle_hotkey(&self, id: u32) {
        if !self.active_hotkey().is_some_and(|key| key.id() == id) { return; }
        let settings = self.ivars().borrow().settings.clone();
        if let Some(settings) = settings.filter(|settings| settings.is_recording()) {
            settings.cancel_recording();
            return;
        }
        let gif = self.ivars().borrow().gif.clone();
        if let Some(gif) = gif.filter(|gif| !gif.is_closed()) {
            gif.primary();
            return;
        }
        self.start_capture();
    }

    pub(crate) fn active_hotkey(&self) -> Option<HotKey> {
        self.ivars().borrow().hotkey
    }

    pub(crate) fn set_hotkey(&self, key: HotKey) -> Result<Option<&'static str>, String> {
        let note = self.register_hotkey(key)?;
        shortcut::save_hotkey(key);
        self.update_shortcut_label();
        Ok(note)
    }

    fn register_hotkey(&self, key: HotKey) -> Result<Option<&'static str>, String> {
        let mut state = self.ivars().borrow_mut();
        let previous = state.hotkey;
        if previous == Some(key) { return Ok(None); }
        shortcut::check_system_shortcut(key)?;
        if state.manager.is_none() {
            state.manager = Some(GlobalHotKeyManager::new().map_err(|error| {
                eprintln!("Could not initialize the shortcut manager: {error}");
                "Global shortcuts are unavailable. Reopen Qingjie and try again."
            })?);
        }
        let manager = state.manager.as_ref().expect("shortcut manager was initialized");
        // Register first. A conflict must leave the previous registration and preference intact.
        manager.register(key).map_err(|error| {
            eprintln!("Could not register the screenshot shortcut: {error}");
            "This shortcut may be in use. Try a different combination."
        })?;
        let note = previous.and_then(|old| manager.unregister(old).err()).map(|error| {
            eprintln!("Could not release the old screenshot shortcut: {error}");
            "The new shortcut is active. Reopen Qingjie to release the old one."
        });
        state.hotkey = Some(key);
        Ok(note)
    }

    fn update_shortcut_label(&self) {
        let label = self.active_hotkey().map_or_else(
            || loc::string("Qingjie · Capture from the menu or change your shortcut"),
            |key| loc::format("Qingjie · {shortcut} to capture an area", &[("shortcut", &shortcut::label(key))]),
        );
        self.set_message(&label);
        if let Some(button) = self.ivars().borrow().status.as_ref().and_then(|item| item.button(self.mtm())) {
            button.setToolTip(Some(&NSString::from_str(&label)));
        }
        if let Some(gif) = self.ivars().borrow().gif.as_ref() {
            gif.set_shortcut_hint(&self.active_hotkey().map(shortcut::label).unwrap_or_default());
        }
    }

    fn show_settings(&self) {
        let window = self.ivars().borrow().settings.clone();
        let window = window.unwrap_or_else(|| {
            let window = SettingsWindow::new(self);
            self.ivars().borrow_mut().settings = Some(window.clone());
            window
        });
        window.show();
    }

    fn add_item(&self, menu: &NSMenu, title: &str, action: Sel) {
        // SAFETY: each selector names an action declared above; DELEGATE retains this target for the run loop.
        let item = unsafe {
            let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()), &loc::text(title), Some(action), ns_string!(""),
            );
            item.setTarget(Some(self));
            item
        };
        menu.addItem(&item);
    }

    fn set_message(&self, text: &str) {
        if let Some(item) = &self.ivars().borrow().message {
            item.setTitle(&loc::text(text));
        }
    }

    fn start_capture(&self) {
        if !self.begin_action() {
            return;
        }
        self.start_capture_inner();
        self.ivars().borrow_mut().modal = false;
    }

    fn begin_action(&self) -> bool {
        let mut state = self.ivars().borrow_mut();
        if state.modal || state.capture.is_some() { return false; }
        if let Some(gif) = state.gif.as_ref().filter(|gif| !gif.is_closed()) {
            let gif = gif.clone();
            drop(state);
            gif.show();
            return false;
        }
        state.gif.take();
        if let Some(editor) = state.editor.as_ref() {
            if !editor.is_closed() {
                let editor = editor.clone();
                drop(state);
                editor.show();
                return false;
            }
        }
        state.editor.take();
        state.modal = true;
        true
    }

    fn request_screen_access(&self) -> bool {
        // SAFETY: these documented CoreGraphics permission APIs take no pointers.
        if !unsafe { CGPreflightScreenCaptureAccess() } {
            unsafe { CGRequestScreenCaptureAccess() };
            if !unsafe { CGPreflightScreenCaptureAccess() } {
                self.set_message("Screen recording permission needed");
                alert(self.mtm(), "Allow Screen Recording", "Turn on Qingjie in System Settings → Privacy & Security → Screen & System Audio Recording, then quit and reopen Qingjie.\n\nQingjie uses this permission to capture the area you select.");
                return false;
            }
        }
        true
    }

    fn start_gif(&self, mode: GifMode) {
        if !self.begin_action() { return; }
        if self.request_screen_access() {
            let label = self.active_hotkey().map(shortcut::label).unwrap_or_default();
            match GifWindow::new(mode, label, self.mtm()) {
                Ok(gif) => {
                    self.ivars().borrow_mut().gif = Some(gif.clone());
                    gif.show();
                }
                Err(error) => alert(self.mtm(), "Could Not Open GIF Capture", error),
            }
        }
        self.ivars().borrow_mut().modal = false;
    }

    fn start_capture_inner(&self) {
        if !self.request_screen_access() { return; }
        let file = match CaptureFile::new("selection.png") {
            Ok(file) => file,
            Err(error) => { alert(self.mtm(), "Could Not Prepare Screenshot", &error.to_string()); return; }
        };
        match Command::new("/usr/sbin/screencapture")
            .args(["-i", "-s", "-x", "-t", "png"]).arg(&file.path)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()
        {
            Ok(child) => {
                self.ivars().borrow_mut().capture = Some(Capture { child, file });
                self.set_message("Selecting an area · Esc to cancel");
                // SAFETY: pollCapture: accepts NSTimer; timer and target are retained until completion.
                let timer = unsafe {
                    NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                        0.1, self, sel!(pollCapture:), None, true,
                    )
                };
                self.ivars().borrow_mut().timer = Some(timer);
            }
            Err(error) => alert(self.mtm(), "Could Not Start Screenshot", &error.to_string()),
        }
    }

    fn finish_if_ready(&self) {
        let completed = {
            let mut state = self.ivars().borrow_mut();
            let Some(capture) = state.capture.as_mut() else { return };
            match capture.child.try_wait() {
                Ok(None) => return,
                Ok(Some(status)) => Ok(status),
                Err(error) => Err(error),
            }
        };
        let mut capture = {
            let mut state = self.ivars().borrow_mut();
            if let Some(timer) = state.timer.take() { timer.invalidate(); }
            state.capture.take().expect("capture is owned by the main thread")
        };
        let status = match completed {
            Ok(status) => status,
            Err(error) => {
                let _ = capture.child.kill();
                let _ = capture.child.wait();
                self.set_message("Screenshot failed. Try again.");
                alert(self.mtm(), "Screenshot Failed", &error.to_string());
                return;
            }
        };
        let mut stderr = String::new();
        if let Some(mut pipe) = capture.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        match classify_result(status.success(), capture.file.path.is_file(), &stderr) {
            CaptureResult::Ready => {
                let result = std::fs::read(&capture.file.path);
                drop(capture); // Remove the private on-disk copy before opening the editor.
                match result.map_err(|error| error.to_string()).and_then(|bytes| {
                    EditorController::new(&NSData::with_bytes(&bytes), self.mtm())
                }) {
                    Ok(editor) => {
                        self.ivars().borrow_mut().editor = Some(editor.clone());
                        self.update_shortcut_label();
                        editor.show();
                    }
                    Err(error) => alert(self.mtm(), "Could Not Open Screenshot", &error),
                }
            }
            CaptureResult::Cancelled => self.update_shortcut_label(),
            CaptureResult::Failed => {
                self.set_message("Capture incomplete. Try again.");
                alert(self.mtm(), "Capture Incomplete", "No image was captured. Check Screen Recording permission and try again. The clipboard is unchanged.");
            }
        }
    }

    fn stop_capture(&self) {
        let mut state = self.ivars().borrow_mut();
        if let Some(timer) = state.timer.take() { timer.invalidate(); }
        if let Some(mut capture) = state.capture.take() {
            let _ = capture.child.kill();
            let _ = capture.child.wait();
        }
        let editor = state.editor.take();
        let gif = state.gif.take();
        drop(state);
        if let Some(editor) = editor { editor.close(); }
        if let Some(gif) = gif { gif.cancel(); }
    }

    fn save_clipboard(&self) {
        if !self.begin_action() { return; }
        self.save_clipboard_inner();
        self.ivars().borrow_mut().modal = false;
    }

    fn save_clipboard_inner(&self) {
        let Some(data) = clipboard_png() else {
            alert(self.mtm(), "No Image on the Clipboard", "Capture and copy an image before saving it from the clipboard.");
            return;
        };
        if save_png(self.mtm(), &data) { self.set_message("Image saved"); }
    }
}

pub(crate) fn save_png(mtm: MainThreadMarker, data: &NSData) -> bool {
    save_image(mtm, data, "png", "Screenshot")
}

pub(crate) fn save_image(mtm: MainThreadMarker, data: &NSData, extension: &str, name: &str) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let panel = NSSavePanel::savePanel(mtm);
    let name = loc::string(name);
    panel.setTitle(Some(&NSString::from_str(&loc::format("Save {name}", &[("name", &name)]))));
    panel.setCanCreateDirectories(true);
    panel.setCanSelectHiddenExtension(true);
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[&*NSString::from_str(extension)])));
    panel.setAllowsOtherFileTypes(false);
    let date = NSDateFormatter::new();
    date.setDateFormat(Some(ns_string!("yyyy-MM-dd HH.mm.ss")));
    panel.setNameFieldStringValue(&NSString::from_str(&format!("{name} {}.{extension}", date.stringFromDate(&NSDate::now()))));
    if panel.runModal() == NSModalResponseOK && let Some(url) = panel.URL() {
        match data.writeToURL_options_error(&url, NSDataWritingOptions::Atomic) {
            Ok(()) => return true,
            Err(error) => alert(mtm, "Could Not Save", &loc::format("{error}\nThe image is still available. You can try saving again.", &[("error", &error.localizedDescription())])),
        }
    }
    false
}

fn clipboard_png() -> Option<Retained<NSData>> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let data = pasteboard.dataForType(ns_string!("public.png"))
        .or_else(|| pasteboard.dataForType(ns_string!("public.tiff")))?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&data)?;
    // SAFETY: an empty property dictionary is valid for PNG encoding.
    unsafe { bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
}

pub(crate) fn alert(mtm: MainThreadMarker, title: &str, message: &str) {
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let dialog = NSAlert::new(mtm);
    dialog.setMessageText(&loc::text(title));
    dialog.setInformativeText(&loc::text(message));
    dialog.addButtonWithTitle(&loc::text("OK"));
    dialog.runModal();
}

fn open_permissions() {
    if let Some(url) = NSURL::URLWithString(ns_string!("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CaptureResult { Ready, Cancelled, Failed }

fn classify_result(success: bool, has_file: bool, stderr: &str) -> CaptureResult {
    if success && has_file { CaptureResult::Ready }
    else if !has_file && stderr.trim().is_empty() { CaptureResult::Cancelled }
    else { CaptureResult::Failed }
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("AppKit must run on the main thread");
    if std::env::args().any(|arg| arg == "--diagnose") {
        println!("qingjie {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH);
        println!("screen_capture_authorized={}", unsafe { CGPreflightScreenCaptureAccess() });
        println!("login_item={:?}", preferences::login_status());
        println!("shortcut={}", shortcut::label(shortcut::configured_hotkey()));
        if let Some(language) = NSBundle::mainBundle().preferredLocalizations().firstObject() {
            println!("language={language}");
        }
        println!("capture_tool_present={}", std::path::Path::new("/usr/sbin/screencapture").is_file());
        return;
    }
    objc2::rc::autoreleasepool(|_| {
        if let Some(identifier) = NSBundle::mainBundle().bundleIdentifier() {
            let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier);
            if running.iter().any(|other| other.processIdentifier() != std::process::id() as i32) {
                return;
            }
        }
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let delegate = AppDelegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        DELEGATE.with(|slot| *slot.borrow_mut() = Some(delegate.clone()));
        delegate.setup();
        app.run();
        delegate.stop_capture();
        DELEGATE.with(|slot| slot.borrow_mut().take());
    });
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn verify_native_settings(mtm: MainThreadMarker, stage: &str) {
    use global_hotkey::hotkey::{Code, Modifiers};
    let id = NSBundle::mainBundle().bundleIdentifier().unwrap().to_string();
    assert!(id == "com.taiyi.qingjie.settingsqa", "manual verification requires an isolated test bundle");
    NSApplication::sharedApplication(mtm).finishLaunching();
    let initial = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT), Code::F19);
    let blocked = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT), Code::F18);
    let changed = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT), Code::F17);
    let app = AppDelegate::new(mtm);
    if stage == "first" {
        assert!(matches!(preferences::login_status(), preferences::LoginStatus::Off | preferences::LoginStatus::Unavailable));
        preferences::apply_login_default();
        assert_eq!(preferences::login_status(), preferences::LoginStatus::Enabled);
        let window = SettingsWindow::new(&app);
        window.verify_login_toggle();
        assert_eq!(preferences::login_status(), preferences::LoginStatus::Off);
        app.set_hotkey(initial).unwrap();
        assert!(app.set_hotkey(blocked).is_err());
        assert_eq!(app.active_hotkey(), Some(initial));
        assert_eq!(shortcut::configured_hotkey(), initial);
        app.set_hotkey(changed).unwrap();
        assert_eq!(shortcut::configured_hotkey(), changed);
    } else {
        preferences::apply_login_default();
        assert_eq!(preferences::login_status(), preferences::LoginStatus::Off);
        assert_eq!(shortcut::configured_hotkey(), changed);
        app.register_hotkey(shortcut::configured_hotkey()).unwrap();
        assert_eq!(app.active_hotkey(), Some(changed));
    }
    println!("native settings: {stage} passed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn capture_requires_a_successful_new_file() {
        assert_eq!(classify_result(true, true, ""), CaptureResult::Ready);
        assert_eq!(classify_result(false, true, ""), CaptureResult::Failed);
        assert_eq!(classify_result(true, false, ""), CaptureResult::Cancelled);
        assert_eq!(classify_result(false, false, ""), CaptureResult::Cancelled);
        assert_eq!(classify_result(false, false, "permission denied"), CaptureResult::Failed);
    }

    #[test]
    fn capture_file_is_private_unique_and_removed_on_drop() {
        let first = CaptureFile::new("selection.png").unwrap();
        let second = CaptureFile::new("selection.png").unwrap();
        assert_ne!(first.directory, second.directory);
        let directory = first.directory.clone();
        assert_eq!(std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::write(&first.path, b"selected screen content").unwrap();
        drop(first);
        assert!(!directory.exists());
        let empty_directory = second.directory.clone();
        drop(second);
        assert!(!empty_directory.exists());
    }
}
