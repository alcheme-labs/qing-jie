use std::cell::{Cell, RefCell};

use objc2::{define_class, msg_send, rc::{Retained, Weak}, runtime::{AnyObject, ProtocolObject, Sel}, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;
use crate::localization as loc;

const TOOLBAR_HEIGHT: f64 = 100.0;
const COLORS: [(f64, f64, f64); 6] = [
    (1.0, 0.20, 0.23), (1.0, 0.78, 0.12), (0.15, 0.80, 0.38),
    (0.15, 0.48, 1.0), (1.0, 1.0, 1.0), (0.08, 0.08, 0.08),
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tool { Pen, Rectangle, Ellipse, Arrow }

struct Stroke {
    tool: Tool,
    color: usize,
    width: f64,
    points: Vec<NSPoint>,
}

struct Document {
    image: Retained<NSImage>,
    size: NSSize,
    strokes: Vec<Stroke>,
    active: Option<Stroke>,
}

impl Document {
    fn from_png(data: &NSData, _mtm: MainThreadMarker) -> Result<Self, String> {
        let bitmap = NSBitmapImageRep::imageRepWithData(data).ok_or("Could not read the screenshot")?;
        let size = NSSize::new(bitmap.pixelsWide() as f64, bitmap.pixelsHigh() as f64);
        if size.width < 1.0 || size.height < 1.0 { return Err("The screenshot size is invalid".into()); }
        let image = NSImage::initWithSize(NSImage::alloc(), size);
        bitmap.setSize(size);
        image.addRepresentation(&bitmap);
        Ok(Self { image, size, strokes: vec![], active: None })
    }

    // Both the view and bitmap export use this exact renderer in original-pixel coordinates.
    fn draw(&self, destination: NSRect) {
        NSGraphicsContext::saveGraphicsState_class();
        NSBezierPath::clipRect(destination);
        let transform = NSAffineTransform::new();
        transform.translateXBy_yBy(destination.origin.x, destination.origin.y);
        transform.scaleXBy_yBy(destination.size.width / self.size.width, destination.size.height / self.size.height);
        transform.concat();
        self.image.drawInRect_fromRect_operation_fraction(
            NSRect::new(NSPoint::new(0.0, 0.0), self.size), NSRect::ZERO,
            NSCompositingOperation::Copy, 1.0,
        );
        for stroke in self.strokes.iter().chain(self.active.iter()) { stroke.draw(); }
        NSGraphicsContext::restoreGraphicsState_class();
    }

    fn bitmap(&self) -> Result<Retained<NSBitmapImageRep>, String> {
        // Null planes ask AppKit to own the buffer. Dimensions are from the decoded screenshot.
        let bitmap = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), self.size.width as isize,
                self.size.height as isize, 8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
            )
        }.ok_or("Not enough memory to render the screenshot")?;
        let bitmap = bitmap.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace()).ok_or("Could not create an sRGB image")?;
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap).ok_or("Could not prepare image rendering")?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        self.draw(NSRect::new(NSPoint::new(0.0, 0.0), self.size));
        NSGraphicsContext::restoreGraphicsState_class();
        Ok(bitmap)
    }
}

impl Stroke {
    fn draw(&self) {
        let Some(&start) = self.points.first() else { return };
        let end = *self.points.last().unwrap();
        let (r, g, b) = COLORS[self.color];
        NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).set();
        let path = match self.tool {
            Tool::Rectangle => NSBezierPath::bezierPathWithRect(normalized_rect(start, end)),
            Tool::Ellipse => NSBezierPath::bezierPathWithOvalInRect(normalized_rect(start, end)),
            Tool::Pen | Tool::Arrow => {
                let path = NSBezierPath::bezierPath();
                path.moveToPoint(start);
                for &point in &self.points[1..] { path.lineToPoint(point); }
                if self.tool == Tool::Pen && self.points.len() == 1 {
                    NSBezierPath::bezierPathWithOvalInRect(rect(start.x - self.width / 2.0, start.y - self.width / 2.0, self.width, self.width)).fill();
                }
                if self.tool == Tool::Arrow {
                    let length = (end.x - start.x).hypot(end.y - start.y);
                    if length > 0.1 {
                        let head = (self.width * 4.0).max(10.0).min(length * 0.45);
                        let ux = (end.x - start.x) / length;
                        let uy = (end.y - start.y) / length;
                        path.moveToPoint(NSPoint::new(end.x - head * ux - head * 0.55 * uy, end.y - head * uy + head * 0.55 * ux));
                        path.lineToPoint(end);
                        path.lineToPoint(NSPoint::new(end.x - head * ux + head * 0.55 * uy, end.y - head * uy - head * 0.55 * ux));
                    }
                }
                path
            }
        };
        path.setLineWidth(self.width);
        path.setLineCapStyle(NSLineCapStyle::Round);
        path.setLineJoinStyle(NSLineJoinStyle::Round);
        path.stroke();
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn color_swatch(index: usize, name: &str) -> Option<Retained<NSImage>> {
    // A 2x, non-template image keeps the actual palette color in every button state.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), 40, 40, 8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
        )
    }?.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace())?;
    // AppKit owns this interleaved RGBA buffer; initialize transparent pixels outside the circle.
    unsafe { std::ptr::write_bytes(bitmap.bitmapData(), 0, bitmap.bytesPerRow() as usize * 40); }
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let circle = NSBezierPath::bezierPathWithOvalInRect(rect(4.0, 4.0, 32.0, 32.0));
    let (r, g, b) = COLORS[index];
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).setFill();
    circle.fill();
    NSColor::colorWithSRGBRed_green_blue_alpha(0.45, 0.45, 0.45, 1.0).setStroke();
    circle.setLineWidth(1.5);
    circle.stroke();
    NSGraphicsContext::restoreGraphicsState_class();
    let size = NSSize::new(20.0, 20.0);
    bitmap.setSize(size);
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    image.addRepresentation(&bitmap);
    image.setTemplate(false);
    image.setAccessibilityDescription(Some(&loc::text(name)));
    Some(image)
}

fn normalized_rect(a: NSPoint, b: NSPoint) -> NSRect {
    rect(a.x.min(b.x), a.y.min(b.y), (a.x - b.x).abs(), (a.y - b.y).abs())
}

fn fitted_rect(bounds: NSRect, size: NSSize) -> NSRect {
    let scale = ((bounds.size.width - 32.0).max(1.0) / size.width)
        .min((bounds.size.height - 32.0).max(1.0) / size.height).min(1.0);
    let width = size.width * scale;
    let height = size.height * scale;
    rect(bounds.origin.x + (bounds.size.width - width) / 2.0,
         bounds.origin.y + (bounds.size.height - height) / 2.0, width, height)
}

fn image_point(point: NSPoint, frame: NSRect, size: NSSize, clamp: bool) -> Option<NSPoint> {
    let x = (point.x - frame.origin.x) * size.width / frame.size.width;
    let y = (point.y - frame.origin.y) * size.height / frame.size.height;
    if !clamp && (x < 0.0 || y < 0.0 || x > size.width || y > size.height) { return None; }
    Some(NSPoint::new(x.clamp(0.0, size.width), y.clamp(0.0, size.height)))
}

struct CanvasState {
    document: RefCell<Option<Document>>,
    tool: Cell<Tool>,
    color: Cell<usize>,
    width: Cell<f64>,
    controller: RefCell<Option<Weak<EditorController>>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = CanvasState]
    struct Canvas;

    impl Canvas {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.12, 0.13, 0.15, 1.0).set();
            NSBezierPath::bezierPathWithRect(self.bounds()).fill();
            if let Some(document) = self.ivars().document.borrow().as_ref() {
                document.draw(fitted_rect(self.bounds(), document.size));
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if let Some(window) = self.window() { window.makeFirstResponder(Some(self)); }
            let mut slot = self.ivars().document.borrow_mut();
            let Some(doc) = slot.as_mut() else { return };
            let frame = fitted_rect(self.bounds(), doc.size);
            let location = self.convertPoint_fromView(event.locationInWindow(), None);
            if let Some(point) = image_point(location, frame, doc.size, false) {
                doc.active = Some(Stroke {
                    tool: self.ivars().tool.get(), color: self.ivars().color.get(),
                    width: self.ivars().width.get() * doc.size.width / frame.size.width,
                    points: vec![point],
                });
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) { self.update_stroke(event, false); }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) { self.update_stroke(event, true); }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let controller = self.ivars().controller.borrow().as_ref().and_then(Weak::load);
            if let Some(controller) = controller {
                match event.keyCode() {
                    53 => controller.close(),
                    36 | 76 => controller.confirm(),
                    6 if event.modifierFlags().contains(NSEventModifierFlags::Command) => self.undo(),
                    _ => { /* Drawing canvas has no text input. */ }
                }
            }
        }
    }
);

impl Canvas {
    fn new(document: Document, frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CanvasState {
            document: RefCell::new(Some(document)), tool: Cell::new(Tool::Pen),
            color: Cell::new(0), width: Cell::new(3.0), controller: RefCell::new(None),
        });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn update_stroke(&self, event: &NSEvent, finish: bool) {
        let mut slot = self.ivars().document.borrow_mut();
        let Some(doc) = slot.as_mut() else { return };
        let location = self.convertPoint_fromView(event.locationInWindow(), None);
        let point = image_point(location, fitted_rect(self.bounds(), doc.size), doc.size, true).unwrap();
        if let Some(stroke) = doc.active.as_mut() {
            if stroke.tool == Tool::Pen {
                let last = stroke.points.last().unwrap();
                if (point.x - last.x).hypot(point.y - last.y) > 0.25 { stroke.points.push(point); }
            } else { stroke.points.truncate(1); stroke.points.push(point); }
        }
        if finish && let Some(stroke) = doc.active.take() { doc.strokes.push(stroke); }
        self.setNeedsDisplay(true);
    }

    fn undo(&self) {
        if let Some(doc) = self.ivars().document.borrow_mut().as_mut() {
            if doc.active.take().is_none() { doc.strokes.pop(); }
        }
        self.setNeedsDisplay(true);
    }
}

pub(crate) struct EditorState {
    window: Retained<NSWindow>,
    canvas: Retained<Canvas>,
    buttons: RefCell<Vec<Retained<NSButton>>>,
    closed: Cell<bool>,
    busy: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = EditorState]
    pub(crate) struct EditorController;

    unsafe impl NSObjectProtocol for EditorController {}
    unsafe impl NSWindowDelegate for EditorController {
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _window: &NSWindow) -> bool { !self.ivars().busy.get() }

        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _notification: &NSNotification) {
            self.ivars().closed.set(true);
            self.ivars().canvas.ivars().document.borrow_mut().take();
        }
    }

    impl EditorController {
        #[unsafe(method(selectTool:))]
        fn select_tool(&self, sender: &NSButton) {
            self.ivars().canvas.ivars().tool.set([Tool::Pen, Tool::Rectangle, Tool::Ellipse, Tool::Arrow][sender.tag() as usize]);
            self.refresh_buttons();
        }

        #[unsafe(method(selectColor:))]
        fn select_color(&self, sender: &NSButton) {
            self.ivars().canvas.ivars().color.set(sender.tag() as usize - 10);
            self.refresh_buttons();
        }

        #[unsafe(method(selectWidth:))]
        fn select_width(&self, sender: &NSButton) {
            self.ivars().canvas.ivars().width.set([2.0, 3.0, 6.0][sender.tag() as usize - 20]);
            self.refresh_buttons();
        }

        #[unsafe(method(undo:))]
        fn undo_action(&self, _sender: Option<&AnyObject>) { self.ivars().canvas.undo(); }

        #[unsafe(method(confirm:))]
        fn confirm_action(&self, _sender: Option<&AnyObject>) { self.confirm(); }

        #[unsafe(method(cancel:))]
        fn cancel_action(&self, _sender: Option<&AnyObject>) { self.close(); }

        #[unsafe(method(save:))]
        fn save_action(&self, _sender: Option<&AnyObject>) { self.save(); }
    }
);

impl EditorController {
    pub(crate) fn new(data: &NSData, mtm: MainThreadMarker) -> Result<Retained<Self>, String> {
        let document = Document::from_png(data, mtm)?;
        let size = document.size;
        let screen = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame().size).unwrap_or(NSSize::new(1280.0, 800.0));
        let width = (size.width + 32.0).max(720.0).min(screen.width - 60.0);
        let height = (size.height + TOOLBAR_HEIGHT + 32.0).max(360.0).min(screen.height - 80.0);
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect(0.0, 0.0, width, height),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered, false)
        };
        // Rust retains the window; AppKit must not release it a second time on close.
        unsafe { window.setReleasedWhenClosed(false) };
        let title = loc::format("Qingjie · Annotate {width} × {height}", &[("width", &(size.width as usize)), ("height", &(size.height as usize))]);
        window.setTitle(&NSString::from_str(&title));
        window.setContentMinSize(NSSize::new(720.0, 320.0));
        window.setLevel(3); // NSFloatingWindowLevel; stays visible without covering system dialogs.
        window.setCollectionBehavior(NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::FullScreenAuxiliary);
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, width, height));
        window.setContentView(Some(&content));
        let canvas = Canvas::new(document, rect(0.0, TOOLBAR_HEIGHT, width, height - TOOLBAR_HEIGHT), mtm);
        canvas.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        content.addSubview(&canvas);
        let this = Self::alloc(mtm).set_ivars(EditorState {
            window, canvas, buttons: RefCell::new(vec![]), closed: Cell::new(false), busy: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        *this.ivars().canvas.ivars().controller.borrow_mut() = Some(Weak::from_retained(&this));
        this.ivars().window.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this.make_toolbar(&content, width);
        this.ivars().window.center();
        Ok(this)
    }

    fn button(&self, parent: &NSView, title: &str, frame: NSRect, tag: isize, action: Sel, toggle: bool) -> Retained<NSButton> {
        let button = NSButton::initWithFrame(NSButton::alloc(self.mtm()), frame);
        button.setTitle(&loc::text(title));
        button.setBezelStyle(NSBezelStyle::Push);
        button.setButtonType(if toggle { NSButtonType::PushOnPushOff } else { NSButtonType::MomentaryPushIn });
        button.setTag(tag);
        // All selectors are declared above. AppDelegate owns the controller while the window is open.
        unsafe { button.setTarget(Some(self)); button.setAction(Some(action)); }
        parent.addSubview(&button);
        if toggle { self.ivars().buttons.borrow_mut().push(button.clone()); }
        button
    }

    fn make_toolbar(&self, content: &NSView, width: f64) {
        for (index, title) in ["Pen", "Rect", "Oval", "Arrow"].iter().enumerate() {
            self.button(content, title, rect(12.0 + index as f64 * 57.0, 56.0, 55.0, 30.0), index as isize, sel!(selectTool:), true);
        }
        for (index, name) in ["Red", "Yellow", "Green", "Blue", "White", "Black"].iter().enumerate() {
            let button = self.button(content, name, rect(253.0 + index as f64 * 34.0, 56.0, 32.0, 30.0), 10 + index as isize, sel!(selectColor:), true);
            button.setToolTip(Some(&loc::text(name)));
            if let Some(icon) = color_swatch(index, name) {
                button.setImage(Some(&icon));
                button.setImagePosition(NSCellImagePosition::ImageOnly);
            }
        }
        for (index, title) in ["Fine", "Med.", "Bold"].iter().enumerate() {
            self.button(content, title, rect(478.0 + index as f64 * 44.0, 56.0, 42.0, 30.0), 20 + index as isize, sel!(selectWidth:), true);
        }
        let label = NSTextField::labelWithString(&loc::text("Enter to copy · Esc to cancel · ⌘Z to undo"), self.mtm());
        label.setFrame(rect(18.0, 15.0, 275.0, 22.0));
        label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&label);
        for (index, (title, action, key, modifiers)) in [
            ("Undo", sel!(undo:), "z", NSEventModifierFlags::Command),
            ("Save As…", sel!(save:), "s", NSEventModifierFlags::Command),
            ("Cancel", sel!(cancel:), "\u{1b}", NSEventModifierFlags::empty()),
            ("Copy", sel!(confirm:), "\r", NSEventModifierFlags::empty()),
        ].iter().enumerate() {
            let button = self.button(content, title, rect(width - 392.0 + index as f64 * 95.0, 10.0, 91.0, 32.0), -1, *action, false);
            button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);
            button.setKeyEquivalent(&NSString::from_str(key));
            button.setKeyEquivalentModifierMask(*modifiers);
        }
        self.refresh_buttons();
    }

    fn refresh_buttons(&self) {
        let canvas = self.ivars().canvas.ivars();
        for button in self.ivars().buttons.borrow().iter() {
            let tag = button.tag();
            let selected = match tag {
                0..=3 => [Tool::Pen, Tool::Rectangle, Tool::Ellipse, Tool::Arrow][tag as usize] == canvas.tool.get(),
                10..=15 => tag as usize - 10 == canvas.color.get(),
                20..=22 => [2.0, 3.0, 6.0][tag as usize - 20] == canvas.width.get(),
                _ => false,
            };
            button.setState(if selected { 1 } else { 0 });
        }
        self.ivars().window.makeFirstResponder(Some(&self.ivars().canvas));
    }

    pub(crate) fn is_closed(&self) -> bool { self.ivars().closed.get() }

    pub(crate) fn show(&self) {
        if self.is_closed() || self.ivars().busy.get() { return; }
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
        self.ivars().window.makeKeyAndOrderFront(None);
        self.ivars().window.makeFirstResponder(Some(&self.ivars().canvas));
    }

    pub(crate) fn close(&self) {
        if !self.ivars().busy.get() { self.ivars().window.close(); }
    }

    fn export(&self, kind: NSBitmapImageFileType) -> Result<Retained<NSData>, String> {
        let slot = self.ivars().canvas.ivars().document.borrow();
        let doc = slot.as_ref().ok_or("The editor is closed")?;
        let bitmap = doc.bitmap()?;
        encode(&bitmap, kind)
    }

    fn confirm(&self) {
        if self.is_closed() || self.ivars().busy.replace(true) { return; }
        let result = (|| {
            let slot = self.ivars().canvas.ivars().document.borrow();
            let bitmap = slot.as_ref().ok_or("The editor is closed")?.bitmap()?;
            copy_bitmap(&bitmap, &NSPasteboard::generalPasteboard())
        })();
        match result {
            Ok(()) => { self.ivars().busy.set(false); self.close(); }
            Err(message) => { crate::mac::alert(self.mtm(), "Could Not Copy", &message); self.ivars().busy.set(false); }
        }
    }

    fn save(&self) {
        if self.is_closed() || self.ivars().busy.replace(true) { return; }
        match self.export(NSBitmapImageFileType::PNG) {
            Ok(data) => {
                self.ivars().window.setLevel(0);
                crate::mac::save_png(self.mtm(), &data);
                self.ivars().window.setLevel(3);
            }
            Err(message) => crate::mac::alert(self.mtm(), "Could Not Save Image", &message),
        }
        self.ivars().busy.set(false);
    }
}

fn encode(bitmap: &NSBitmapImageRep, kind: NSBitmapImageFileType) -> Result<Retained<NSData>, String> {
    // The empty property dictionary requests the native encoder's defaults.
    unsafe { bitmap.representationUsingType_properties(kind, &NSDictionary::new()) }.ok_or("Could not encode the image".into())
}

fn copy_bitmap(bitmap: &NSBitmapImageRep, pasteboard: &NSPasteboard) -> Result<(), String> {
    // Eagerly encode both types before replacing the pasteboard; no file promises or deferred owners.
    let png = encode(bitmap, NSBitmapImageFileType::PNG)?;
    let tiff = encode(bitmap, NSBitmapImageFileType::TIFF)?;
    let item = NSPasteboardItem::new();
    if !item.setData_forType(&png, ns_string!("public.png")) || !item.setData_forType(&tiff, ns_string!("public.tiff")) {
        return Err("Could not prepare the clipboard image. Try again or save a file.".into());
    }
    let objects = NSArray::from_slice(&[ProtocolObject::<dyn NSPasteboardWriting>::from_ref(&*item)]);
    pasteboard.clearContents();
    if !pasteboard.writeObjects(&objects) { return Err("Could not write to the clipboard. Try again or save a file.".into()); }
    Ok(())
}

// Run by tests/native_editor.rs on the main thread, against actual AppKit and pasteboard services.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn verify_native_pipeline(mtm: MainThreadMarker) {
    let original = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), 128, 96, 8, 4, true, false, NSDeviceRGBColorSpace, 0, 0,
        )
    }.unwrap();
    let original = original.bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace()).unwrap();
    // An opaque RGBA fixture written directly; independent of the renderer under test.
    let row = original.bytesPerRow() as usize;
    unsafe {
        let pixels = std::slice::from_raw_parts_mut(original.bitmapData(), row * 96);
        pixels.fill(255);
        pixels[6 * row + 6 * 4 .. 6 * row + 6 * 4 + 4].fill(0);
        pixels[3 * row + 2 * 4 .. 3 * row + 2 * 4 + 4].copy_from_slice(&[0, 0, 0, 255]);
        pixels[91 * row + 124 * 4 .. 91 * row + 124 * 4 + 4].copy_from_slice(&[0, 0, 255, 255]);
    }
    let png = encode(&original, NSBitmapImageFileType::PNG).unwrap();
    std::fs::write("target/native-input.png", png.to_vec()).unwrap();
    let editor = EditorController::new(&png, mtm).unwrap();
    let rgb = |bitmap: &NSBitmapImageRep, x, y| {
        let color = bitmap.colorAtX_y(x, y).unwrap().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).unwrap();
        (color.redComponent(), color.greenComponent(), color.blueComponent())
    };
    {
        let slot = editor.ivars().canvas.ivars().document.borrow();
        let bitmap = slot.as_ref().unwrap().bitmap().unwrap();
        assert_eq!((bitmap.pixelsWide(), bitmap.pixelsHigh()), (128, 96));
        assert!(bitmap.colorAtX_y(6, 6).unwrap().alphaComponent() < 0.05, "transparent pixels must not inherit uninitialized bitmap contents");
        assert!(rgb(&bitmap, 2, 3).0 < 0.05, "top-left original pixel must not flip");
        std::fs::write("target/native-unmarked.png", encode(&bitmap, NSBitmapImageFileType::PNG).unwrap().to_vec()).unwrap();
        let marker = rgb(&bitmap, 124, 91);
        assert!(marker.2 > 0.95 && marker.0 < 0.05, "bottom-right original pixel must survive: {marker:?}");
    }
    let board = NSPasteboard::pasteboardWithUniqueName();
    {
        let mut slot = editor.ivars().canvas.ivars().document.borrow_mut();
        let doc = slot.as_mut().unwrap();
        for (tool, color, points) in [
            (Tool::Rectangle, 0, vec![NSPoint::new(40.0, 30.0), NSPoint::new(10.0, 10.0)]),
            (Tool::Ellipse, 3, vec![NSPoint::new(50.0, 10.0), NSPoint::new(90.0, 40.0)]),
            (Tool::Arrow, 2, vec![NSPoint::new(15.0, 60.0), NSPoint::new(70.0, 60.0)]),
            (Tool::Pen, 0, vec![NSPoint::new(100.0, 65.0), NSPoint::new(110.0, 75.0), NSPoint::new(116.0, 67.0)]),
        ] { doc.strokes.push(Stroke { tool, color, width: 4.0, points }); }
        let bitmap = doc.bitmap().unwrap();
        let red = rgb(&bitmap, 10, 76);
        assert!(red.0 > 0.9 && red.1 < 0.4, "reverse rectangle must render in image coordinates: {red:?}");
        let blue = rgb(&bitmap, 70, 86);
        assert!(blue.2 > 0.9 && blue.0 < 0.3, "ellipse must render: {blue:?}");
        for (x, y) in [(40, 36), (55, 28)] {
            let green = rgb(&bitmap, x, y);
            assert!(green.1 > 0.7 && green.0 < 0.3, "arrow shaft and head must render: {green:?}");
        }
        let pen = rgb(&bitmap, 110, 21);
        assert!(pen.0 > 0.9 && pen.1 < 0.4, "pen must render: {pen:?}");
        copy_bitmap(&bitmap, &board).unwrap();
        for kind in [ns_string!("public.png"), ns_string!("public.tiff")] {
            let decoded = NSBitmapImageRep::imageRepWithData(&board.dataForType(kind).unwrap()).unwrap();
            assert_eq!((decoded.pixelsWide(), decoded.pixelsHigh()), (128, 96));
            assert!(rgb(&decoded, 10, 76).1 < 0.4, "encoded pasteboard data must contain annotations");
        }
        let output = std::path::Path::new("target/native-render.png");
        std::fs::write(output, encode(&bitmap, NSBitmapImageFileType::PNG).unwrap().to_vec()).unwrap();
    }
    editor.ivars().canvas.undo();
    let exported = editor.export(NSBitmapImageFileType::PNG).unwrap();
    let after_undo = NSBitmapImageRep::imageRepWithData(&exported).unwrap();
    assert!(rgb(&after_undo, 110, 21).1 > 0.95, "undo must remove only the last stroke");
    assert!(rgb(&after_undo, 10, 76).1 < 0.4, "undo must preserve earlier shapes");
    editor.close();
    assert!(editor.is_closed());
    assert!(editor.ivars().canvas.ivars().document.borrow().is_none());
    let retained_image = NSBitmapImageRep::imageRepWithData(&board.dataForType(ns_string!("public.png")).unwrap()).unwrap();
    assert!(rgb(&retained_image, 110, 21).1 < 0.4, "clipboard result must remain independent after undo and editor close");
    board.clearContents();
    println!("native AppKit: original pixels, four tools, arrow head, undo, PNG/TIFF pasteboard round-trip, cancellation and image release passed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_canvas_round_trips_original_pixels_and_clamps_drag() {
        let size = NSSize::new(2400.0, 1600.0);
        let frame = fitted_rect(rect(0.0, 100.0, 832.0, 632.0), size);
        assert!((frame.size.width - 800.0).abs() < 1e-8);
        assert!((frame.size.height - 1600.0 / 3.0).abs() < 1e-8);
        let center = NSPoint::new(frame.origin.x + 400.0, frame.origin.y + frame.size.height / 2.0);
        let point = image_point(center, frame, size, false).unwrap();
        assert!((point.x - 1200.0).abs() < 1e-8 && (point.y - 800.0).abs() < 1e-8);
        assert_eq!(image_point(NSPoint::new(-40.0, -40.0), frame, size, false), None);
        assert_eq!(image_point(NSPoint::new(-40.0, 5000.0), frame, size, true), Some(NSPoint::new(0.0, 1600.0)));
    }

    #[test]
    fn shape_bounds_support_every_drag_direction() {
        let expected = rect(10.0, 20.0, 90.0, 60.0);
        for (a, b) in [(NSPoint::new(10.0, 20.0), NSPoint::new(100.0, 80.0)),
                       (NSPoint::new(100.0, 80.0), NSPoint::new(10.0, 20.0)),
                       (NSPoint::new(10.0, 80.0), NSPoint::new(100.0, 20.0))] {
            assert_eq!(normalized_rect(a, b), expected);
        }
    }
}
