use std::{
    cell::{Cell, RefCell},
    sync::{Arc, Mutex, mpsc::{Receiver, TryRecvError}},
    time::{Duration, Instant},
};

use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::{Retained, Weak}, runtime::{AnyObject, ProtocolObject, Sel}, sel};
use objc2_app_kit::*;
use objc2_foundation::*;

use crate::{gif_capture::{self, Animation, Area, CLIP_LIMIT, DisplayFilter, MANUAL_LIMIT, Mode, ScreenRun, StreamEvent}, mac::{self, CaptureFile}};
use crate::localization as loc;

const PANEL_SIZE: NSSize = NSSize::new(438.0, 100.0);
const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(4);
const MANUAL_DELAY: u16 = 50;

struct Region {
    area: Area,
    filter: DisplayFilter,
}

struct TakingFrames {
    region: Region,
    run: ScreenRun,
    started: Option<Instant>,
    finish_requested: bool,
    got_frame: bool,
}

enum Phase {
    Selecting,
    Preparing { area: Area, reply: Receiver<Result<DisplayFilter, String>>, since: Instant },
    Ready(Region),
    Taking(TakingFrames),
    Draft { file: CaptureFile, frames: usize },
    Closed,
}

pub(crate) struct GifState {
    mode: Mode,
    screen_id: u32,
    screen_frame: NSRect,
    scale: f64,
    outline: Retained<SelectionWindow>,
    panel: Retained<NSPanel>,
    status: Retained<NSTextField>,
    hint: Retained<NSTextField>,
    primary: Retained<NSButton>,
    save: Retained<NSButton>,
    recording: Arc<Mutex<Option<Animation>>>,
    phase: RefCell<Phase>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    saving: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = GifState]
    pub(crate) struct GifWindow;

    unsafe impl NSObjectProtocol for GifWindow {}
    unsafe impl NSWindowDelegate for GifWindow {
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _window: &NSWindow) -> bool {
            self.cancel();
            false
        }
    }

    impl GifWindow {
        #[unsafe(method(gifPrimary:))]
        fn primary_action(&self, _sender: Option<&AnyObject>) { self.primary(); }

        #[unsafe(method(gifSave:))]
        fn save_action(&self, _sender: Option<&AnyObject>) { self.finish(); }

        #[unsafe(method(gifCancel:))]
        fn cancel_action(&self, _sender: Option<&AnyObject>) { self.cancel(); }

        #[unsafe(method(pollGif:))]
        fn poll_action(&self, _timer: &NSTimer) { self.poll(); }
    }
);

impl GifWindow {
    pub(crate) fn new(mode: Mode, shortcut: String, mtm: MainThreadMarker) -> Result<Retained<Self>, &'static str> {
        let cursor = NSEvent::mouseLocation();
        let screens = NSScreen::screens(mtm);
        let screen = screens.iter().find(|screen| contains(screen.frame(), cursor))
            .or_else(|| NSScreen::mainScreen(mtm)).ok_or("No display found.")?;
        let screen_id = display_id(&screen).ok_or("Could not identify the current display.")?;
        let screen_frame = screen.frame();
        let scale = screen.backingScaleFactor();

        let outline: Retained<SelectionWindow> = unsafe { msg_send![SelectionWindow::alloc(mtm),
            initWithContentRect: screen_frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ] };
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), screen_frame.size);
        unsafe { outline.setReleasedWhenClosed(false); }
        outline.setOpaque(false);
        outline.setBackgroundColor(Some(&NSColor::clearColor()));
        outline.setHasShadow(false);
        outline.setLevel(NSStatusWindowLevel + 1);
        outline.setCollectionBehavior(NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::FullScreenAuxiliary);

        let panel: Retained<NSPanel> = unsafe { msg_send![NSPanel::alloc(mtm),
            initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), PANEL_SIZE),
            styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::NonactivatingPanel,
            backing: NSBackingStoreType::Buffered,
            defer: false,
        ] };
        unsafe { panel.setReleasedWhenClosed(false); }
        panel.setFloatingPanel(true);
        panel.setHidesOnDeactivate(false);
        panel.setCollectionBehavior(NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::FullScreenAuxiliary);
        panel.setTitle(&loc::text(if mode == Mode::Clip { "Record Area as GIF" } else { "Build GIF Frame by Frame" }));

        let status = NSTextField::labelWithString(&loc::text("Preparing the capture area…"), mtm);
        status.setFrame(rect(18.0, 62.0, 402.0, 20.0));
        let this = Self::alloc(mtm).set_ivars(GifState {
            mode, screen_id, screen_frame, scale, outline, panel, status,
            hint: NSTextField::labelWithString(ns_string!(""), mtm),
            primary: make_button(mtm, ""), save: make_button(mtm, "Save GIF…"),
            recording: Arc::new(Mutex::new(None)), phase: RefCell::new(Phase::Selecting),
            timer: RefCell::new(None), saving: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let state = this.ivars();
        state.panel.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        let annotation = if mode == Mode::Clip { "Drag to select the area to record" } else { "Drag to select a fixed area for frames" };
        let selector = SelectionView::new(bounds, Weak::from_retained(&this), annotation, mtm);
        state.outline.setContentView(Some(&selector));
        state.outline.setInitialFirstResponder(Some(&selector));

        let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), PANEL_SIZE));
        state.panel.setContentView(Some(&content));
        content.addSubview(&state.status);
        let hint = &state.hint;
        hint.setFrame(rect(18.0, 42.0, 402.0, 18.0));
        hint.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(hint);
        this.set_shortcut_hint(&shortcut);
        this.place(&content, &state.primary, sel!(gifPrimary:), rect(18.0, 8.0, 139.0, 30.0));
        this.place(&content, &state.save, sel!(gifSave:), rect(161.0, 8.0, 122.0, 30.0));
        let cancel = make_button(mtm, "Cancel");
        this.place(&content, &cancel, sel!(gifCancel:), rect(332.0, 8.0, 88.0, 30.0));
        Ok(this)
    }

    fn place(&self, view: &NSView, button: &NSButton, action: Sel, frame: NSRect) {
        // Button actions are declared above and the AppDelegate retains the target.
        unsafe { button.setTarget(Some(self)); button.setAction(Some(action)); }
        button.setFrame(frame);
        view.addSubview(button);
    }

    pub(crate) fn set_shortcut_hint(&self, shortcut: &str) {
        let state = self.ivars();
        let text = if shortcut.is_empty() {
            loc::string(if state.mode == Mode::Clip { "Record up to 12 seconds. Stop at any time." } else { "Each frame uses the same area and plays for 0.5 seconds." })
        } else if state.mode == Mode::Clip {
            loc::format("{shortcut} to start or stop · 12 seconds max", &[("shortcut", &shortcut)])
        } else {
            loc::format("{shortcut} to take a frame · 0.5 seconds per frame", &[("shortcut", &shortcut)])
        };
        state.hint.setStringValue(&NSString::from_str(&text));
    }

    pub(crate) fn show(&self) {
        if self.ivars().saving.get() { return; }
        let selecting = matches!(*self.ivars().phase.borrow(), Phase::Selecting);
        if selecting {
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
            self.ivars().outline.makeKeyAndOrderFront(None);
        } else if !self.is_closed() {
            self.ivars().panel.orderFrontRegardless();
        }
    }

    fn selected(&self, local: NSRect) {
        let state = self.ivars();
        if !matches!(*state.phase.borrow(), Phase::Selecting) { return; }
        let area = Area::from_selection(state.screen_id, state.screen_frame.size.height, state.scale, local);
        let selection = NSRect::new(NSPoint::new(state.screen_frame.origin.x + local.origin.x, state.screen_frame.origin.y + local.origin.y), local.size);
        let screen = NSScreen::screens(self.mtm()).iter().find(|screen| display_id(screen) == Some(state.screen_id));
        let visible = screen.map_or(state.screen_frame, |screen| screen.visibleFrame());
        let below = selection.origin.y - (PANEL_SIZE.height + 36.0);
        let above = selection.origin.y + selection.size.height + 12.0;
        let x = selection.origin.x.clamp(visible.origin.x, (visible.origin.x + visible.size.width - PANEL_SIZE.width).max(visible.origin.x));
        let y = if below >= visible.origin.y { below } else { above };
        let y = y.clamp(visible.origin.y, (visible.origin.y + visible.size.height - PANEL_SIZE.height - 28.0).max(visible.origin.y));
        state.panel.setFrameOrigin(NSPoint::new(x, y));
        state.outline.setIgnoresMouseEvents(true);
        state.outline.setLevel(NSFloatingWindowLevel);
        state.outline.orderOut(None);
        state.outline.orderFrontRegardless();
        state.panel.orderFrontRegardless();
        let windows = [state.panel.windowNumber(), state.outline.windowNumber()].into_iter()
            .filter_map(|number| u32::try_from(number).ok()).collect();
        let reply = gif_capture::display_filter(area, windows);
        state.phase.replace(Phase::Preparing { area, reply, since: Instant::now() });
        self.refresh(None);
        self.start_polling();
    }

    fn start_polling(&self) {
        let state = self.ivars();
        if state.timer.borrow().is_some() { return; }
        // pollGif: is registered above and receives NSTimer.
        let timer = unsafe { NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(0.075, self, sel!(pollGif:), None, true) };
        // This window and timer are created and used on the main thread.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes); }
        state.timer.replace(Some(timer));
    }

    fn stop_polling(&self) {
        if let Some(timer) = self.ivars().timer.take() { timer.invalidate(); }
    }

    pub(crate) fn primary(&self) {
        let state = self.ivars();
        if state.saving.get() { return; }
        let mut phase = state.phase.borrow_mut();
        if let Phase::Taking(run) = &mut *phase {
            if state.mode == Mode::Clip { run.finish_requested = true; }
            return;
        }
        let Phase::Ready(region) = &*phase else { return; };
        let screen_ok = NSScreen::screens(self.mtm()).iter().any(|screen| {
            display_id(&screen) == Some(state.screen_id) && screen.frame() == state.screen_frame && screen.backingScaleFactor() == state.scale
        });
        if !screen_ok {
            drop(phase);
            self.refresh(Some("Display settings changed. Save your frames or select again."));
            return;
        }
        {
            let mut recording = state.recording.lock().expect("GIF writer lock");
            if recording.is_none() {
                match Animation::new(region.area.pixels) {
                    Ok(animation) => *recording = Some(animation),
                    Err(error) => {
                        eprintln!("Could not prepare GIF file: {error}");
                        drop(recording);
                        drop(phase);
                        self.refresh(Some("Could not create the GIF file. Check available disk space."));
                        return;
                    }
                }
            }
            if state.mode == Mode::Manual && recording.as_ref().is_some_and(|animation| animation.count >= MANUAL_LIMIT) {
                return;
            }
        }
        let delay = (state.mode == Mode::Manual).then_some(MANUAL_DELAY);
        let run = ScreenRun::start(region.area, &region.filter, state.recording.clone(), delay);
        match run {
            Ok(run) => {
                let Phase::Ready(region) = std::mem::replace(&mut *phase, Phase::Closed) else { unreachable!() };
                *phase = Phase::Taking(TakingFrames { region, run, started: None, finish_requested: false, got_frame: false });
                drop(phase);
                self.refresh(None);
                self.start_polling();
            }
            Err(error) => {
                eprintln!("Could not start GIF capture: {error}");
                drop(phase);
                self.refresh(Some("Could not start capture. Check permissions and try again."));
            }
        }
    }

    fn poll(&self) {
        let state = self.ivars();
        let mut phase = state.phase.borrow_mut();
        let now = Instant::now();
        if let Phase::Preparing { area, reply, since } = &*phase {
            match reply.try_recv() {
                Ok(Ok(filter)) => { *phase = Phase::Ready(Region { area: *area, filter }); }
                Ok(Err(error)) => {
                    eprintln!("Could not prepare display filter: {error}");
                    drop(phase);
                    self.cancel();
                    mac::alert(self.mtm(), "Could Not Prepare GIF Capture", "Check the display and Screen Recording permission, then select an area again.");
                    return;
                }
                Err(TryRecvError::Empty) if now.duration_since(*since) < START_TIMEOUT => return,
                Err(_) => {
                    drop(phase);
                    self.cancel();
                    mac::alert(self.mtm(), "Could Not Prepare GIF Capture", "The display timed out. Select an area again.");
                    return;
                }
            }
            drop(phase);
            self.stop_polling();
            self.refresh(None);
            return;
        }
        let Phase::Taking(taking) = &mut *phase else { return; };
        let mut notice = None;
        let mut write_failed = false;
        let mut done = false;
        for event in taking.run.events.try_iter() {
            match event {
                StreamEvent::Started => taking.started = Some(now),
                StreamEvent::Frame => {
                    taking.got_frame = true;
                    if state.mode == Mode::Manual { taking.finish_requested = true; }
                }
                StreamEvent::Limit => taking.finish_requested = true,
                StreamEvent::Stopped => { taking.run.has_stopped = true; done = true; }
                StreamEvent::Failed(error) => {
                    eprintln!("GIF capture stopped: {error}");
                    notice = Some("Capture interrupted. Check Screen Recording permission.");
                    done = true;
                }
                StreamEvent::WriteFailed(error) => {
                    eprintln!("Could not write GIF frames: {error}");
                    notice = Some("Could not write the GIF. Check available disk space.");
                    write_failed = true;
                    done = true;
                }
            }
        }
        if let Some(started) = taking.started {
            if state.mode == Mode::Clip && now.duration_since(started) >= CLIP_LIMIT {
                taking.finish_requested = true;
            }
            if state.mode == Mode::Manual && !taking.got_frame && now.duration_since(started) >= START_TIMEOUT {
                taking.finish_requested = true;
            }
            if taking.finish_requested { taking.run.stop(); }
        } else if now.duration_since(taking.run.requested_at) >= START_TIMEOUT {
            done = true;
            notice = Some("Timed out waiting for frames. Try again.");
        }
        if taking.run.stop_at.is_some_and(|at| now.saturating_duration_since(at) >= STOP_TIMEOUT) {
            done = true;
            notice = Some("Timed out while ending capture.");
        }
        if done {
            let Phase::Taking(taking) = std::mem::replace(&mut *phase, Phase::Closed) else { unreachable!() };
            let stopped_at = taking.run.stop_at.unwrap_or(now);
            let got_frame = taking.got_frame;
            drop(taking.run);
            *phase = Phase::Ready(taking.region);
            drop(phase);
            self.stop_polling();
            if write_failed {
                state.recording.lock().expect("GIF writer lock").take();
            }
            if state.mode == Mode::Clip && got_frame && !write_failed {
                self.write_draft(stopped_at);
            } else {
                self.refresh(notice.or((!got_frame).then_some("No frame captured. Try again.")));
            }
        } else {
            drop(phase);
            self.refresh(None);
        }
    }

    fn refresh(&self, notice: Option<&str>) {
        let state = self.ivars();
        let count = state.recording.try_lock().ok().and_then(|writer| writer.as_ref().map(|writer| writer.count)).unwrap_or(0);
        let phase = state.phase.borrow();
        let mut button = if state.mode == Mode::Manual { "Take Frame" } else { "Start Recording" };
        let mut ready = false;
        let mut save = false;
        let message = match &*phase {
            Phase::Selecting | Phase::Preparing { .. } => "Preparing the capture area…".into(),
            Phase::Ready(region) => {
                ready = state.mode == Mode::Clip || count < MANUAL_LIMIT;
                save = state.mode == Mode::Manual && count >= 2;
                if state.mode == Mode::Clip {
                    loc::format("Area selected · {width} × {height}", &[("width", &region.area.pixels.0), ("height", &region.area.pixels.1)])
                } else if count >= MANUAL_LIMIT {
                    loc::format("{count} frames · Limit reached. You can save now.", &[("count", &count)])
                } else {
                    loc::format("{count} frames · Take at least two to save", &[("count", &count)])
                }
            }
            Phase::Taking(taking) => {
                if taking.finish_requested {
                    "Finishing capture…".into()
                } else if state.mode == Mode::Manual {
                    "Taking a frame…".into()
                } else {
                    ready = true;
                    button = "Stop Recording";
                    let seconds = taking.started.map_or(0, |at| at.elapsed().as_secs()).min(CLIP_LIMIT.as_secs());
                    loc::format("● Recording   {seconds} / {limit} s", &[("seconds", &seconds), ("limit", &CLIP_LIMIT.as_secs())])
                }
            }
            Phase::Draft { frames, .. } => {
                save = true;
                loc::format("GIF ready · {frames} frames", &[("frames", frames)])
            }
            Phase::Closed => return,
        };
        state.primary.setTitle(&loc::text(button));
        state.primary.setEnabled(ready);
        state.save.setEnabled(save);
        state.status.setStringValue(&loc::text(notice.unwrap_or(&message)));
    }

    fn finish(&self) {
        let state = self.ivars();
        if state.saving.get() { return; }
        let phase = state.phase.borrow();
        if matches!(*phase, Phase::Draft { .. }) {
            drop(phase);
            self.save_draft();
        } else if matches!(*phase, Phase::Ready(_)) && state.mode == Mode::Manual {
            drop(phase);
            let enough_frames = state.recording.lock().expect("GIF writer lock").as_ref().is_some_and(|writer| writer.count >= 2);
            if enough_frames {
                self.write_draft(Instant::now());
            }
        }
    }

    fn write_draft(&self, at: Instant) {
        let state = self.ivars();
        let animation = state.recording.lock().expect("GIF writer lock").take();
        let Some(animation) = animation else { return; };
        let frames = animation.count;
        match animation.finish(at) {
            Ok(file) => {
                state.phase.replace(Phase::Draft { file, frames });
                state.outline.orderOut(None);
                self.refresh(None);
                self.save_draft();
            }
            Err(error) => {
                eprintln!("Could not finish GIF: {error}");
                self.refresh(Some("Could not export the GIF. Check available disk space."));
            }
        }
    }

    fn save_draft(&self) {
        let state = self.ivars();
        let data = {
            let phase = state.phase.borrow();
            let Phase::Draft { file, .. } = &*phase else { return; };
            NSURL::from_path(&file.path, false, None)
                .and_then(|url| NSData::dataWithContentsOfURL_options_error(&url, NSDataReadingOptions::MappedIfSafe).ok())
        };
        let Some(data) = data else {
            self.refresh(Some("Could not read the GIF. Try saving again."));
            return;
        };
        state.saving.set(true);
        state.panel.setLevel(NSNormalWindowLevel);
        let saved = mac::save_image(self.mtm(), &data, "gif", "GIF");
        state.panel.setLevel(NSFloatingWindowLevel);
        state.saving.set(false);
        if saved { self.cancel(); }
    }

    pub(crate) fn is_closed(&self) -> bool { matches!(*self.ivars().phase.borrow(), Phase::Closed) }

    pub(crate) fn cancel(&self) {
        let state = self.ivars();
        if state.saving.get() || self.is_closed() { return; }
        self.stop_polling();
        state.phase.replace(Phase::Closed);
        state.recording.lock().expect("GIF writer lock").take();
        state.outline.orderOut(None);
        state.panel.orderOut(None);
    }
}

fn display_id(screen: &NSScreen) -> Option<u32> {
    screen.deviceDescription().objectForKey(ns_string!("NSScreenNumber"))?
        .downcast::<NSNumber>().ok().map(|value| value.unsignedIntValue())
}

fn make_button(mtm: MainThreadMarker, title: &str) -> Retained<NSButton> {
    // Target and action are assigned when the button is placed.
    unsafe { NSButton::buttonWithTitle_target_action(&loc::text(title), None, None, mtm) }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn contains(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y && point.y < rect.origin.y + rect.size.height
}

define_class!(
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    struct SelectionWindow;

    impl SelectionWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool { true }
    }
);

struct SelectionState {
    owner: Weak<GifWindow>,
    instruction: Retained<NSTextField>,
    start: Cell<Option<NSPoint>>,
    box_rect: Cell<Option<NSRect>>,
    selected: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SelectionState]
    struct SelectionView;

    impl SelectionView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }

        #[unsafe(method(drawRect:))]
        fn draw(&self, dirty: NSRect) {
            NSRectFillUsingOperation(dirty, NSCompositingOperation::Clear);
            if !self.ivars().selected.get() {
                let shade = NSBezierPath::bezierPathWithRect(self.bounds());
                if let Some(selection) = self.ivars().box_rect.get() { shade.appendBezierPathWithRect(selection); }
                shade.setWindingRule(NSWindingRule::EvenOdd);
                NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.30).setFill();
                shade.fill();
            }
            if let Some(selection) = self.ivars().box_rect.get() {
                NSColor::controlAccentColor().setStroke();
                let path = NSBezierPath::bezierPathWithRect(selection);
                path.setLineWidth(2.0);
                path.stroke();
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().instruction.setHidden(true);
            self.ivars().start.set(Some(self.point(event)));
            self.ivars().box_rect.set(None);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) { self.resize_crop(event); }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.resize_crop(event);
            self.ivars().start.set(None);
            if let Some(area) = self.ivars().box_rect.get().filter(|area| area.size.width >= 8.0 && area.size.height >= 8.0) {
                self.ivars().selected.set(true);
                self.setNeedsDisplay(true);
                if let Some(owner) = self.ivars().owner.load() { owner.selected(area); }
            } else {
                self.ivars().box_rect.set(None);
                self.ivars().instruction.setHidden(false);
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == 53 && let Some(owner) = self.ivars().owner.load() { owner.cancel(); }
        }
    }
);

impl SelectionView {
    fn new(bounds: NSRect, owner: Weak<GifWindow>, prompt: &str, mtm: MainThreadMarker) -> Retained<Self> {
        let instruction = loc::format("{prompt} · Esc to cancel", &[("prompt", &loc::string(prompt))]);
        let instruction = NSTextField::labelWithString(&NSString::from_str(&instruction), mtm);
        instruction.setFont(Some(&NSFont::boldSystemFontOfSize(16.0)));
        instruction.setTextColor(Some(&NSColor::whiteColor()));
        instruction.setFrame(rect(28.0, bounds.size.height - 96.0, 480.0, 32.0));
        let this = Self::alloc(mtm).set_ivars(SelectionState { owner, instruction, start: Cell::new(None), box_rect: Cell::new(None), selected: Cell::new(false) });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: bounds] };
        this.addSubview(&this.ivars().instruction);
        this
    }

    fn point(&self, event: &NSEvent) -> NSPoint {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let size = self.bounds().size;
        NSPoint::new(point.x.round().clamp(0.0, size.width), point.y.round().clamp(0.0, size.height))
    }

    fn resize_crop(&self, event: &NSEvent) {
        let Some(start) = self.ivars().start.get() else { return; };
        let end = self.point(event);
        self.ivars().box_rect.set(Some(rect(start.x.min(end.x), start.y.min(end.y), (start.x - end.x).abs(), (start.y - end.y).abs())));
        self.setNeedsDisplay(true);
    }
}
