#![allow(dead_code, unused_imports)]

#[path = "../src/editor.rs"]
mod editor;
#[path = "../src/mac.rs"]
mod mac;
#[path = "../src/preferences.rs"]
mod preferences;
#[path = "../src/shortcut.rs"]
mod shortcut;
#[path = "../src/gif_capture.rs"]
mod gif_capture;
#[path = "../src/gif_window.rs"]
mod gif_window;
#[path = "../src/localization.rs"]
mod localization;

fn main() {
    let mtm = objc2_foundation::MainThreadMarker::new().unwrap();
    objc2::rc::autoreleasepool(|_| {
        objc2_app_kit::NSApplication::sharedApplication(mtm);
        let args: Vec<_> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("--verify-settings") => mac::verify_native_settings(mtm, &args[1]),
            Some("--verify-screen-fixture") => verify_screen_fixture(&args[1]),
            _ => editor::verify_native_pipeline(mtm),
        }
    });
}

// This opt-in integration probe samples only a target test window with known colors.
fn verify_screen_fixture(fixture_dir: &str) {
    use std::{fs, path::Path, sync::{Arc, Mutex}, time::{Duration, Instant}};
    use gif_capture::{Animation, Area, ScreenRun, StreamEvent, display_filter};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" { fn CGPreflightScreenCaptureAccess() -> bool; }
    assert!(unsafe { CGPreflightScreenCaptureAccess() }, "Screen Recording is not authorized for the current test process");
    let dir = Path::new(fixture_dir);
    let geometry = fs::read_to_string(dir.join("geometry")).unwrap();
    let parts: Vec<f64> = geometry.split_whitespace().map(|value| value.parse().unwrap()).collect();
    let area = Area::from_selection(parts[0] as u32, parts[1], parts[2], NSRect::new(NSPoint::new(parts[3], parts[4]), NSSize::new(parts[5], parts[6])));
    let filter = display_filter(area, vec![]).recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    let wait = |run: &ScreenRun, until: fn(&StreamEvent) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let event = run.events.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("native frame or stop timed out");
            match event {
                StreamEvent::Failed(error) | StreamEvent::WriteFailed(error) => panic!("screen fixture: {error}"),
                event if until(&event) => break,
                _ => {}
            }
        }
    };
    let stop = |run: &mut ScreenRun| {
        run.stop();
        wait(run, |event| matches!(event, StreamEvent::Stopped));
        run.has_stopped = true;
    };
    let process = |file: &mac::CaptureFile, expected: &[[u8; 3]]| {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoded = options.read_info(fs::File::open(&file.path).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), area.pixels);
        let mut frames = Vec::new();
        while let Some(frame) = decoded.read_next_frame().unwrap() {
            let color = expected.iter().position(|color| frame.buffer.chunks_exact(4).all(|pixel| {
                pixel[..3].iter().zip(color).all(|(found, wanted)| found.abs_diff(*wanted) < 30)
            })).expect("screen crop contains pixels outside its known fixture colors");
            frames.push((color, frame.delay));
        }
        frames
    };
    let red = [205, 42, 47];
    let blue = [41, 88, 208];
    let writer = Arc::new(Mutex::new(Some(Animation::new(area.pixels).unwrap())));
    for (index, color) in ["red", "blue"].iter().enumerate() {
        fs::write(dir.join("color"), color).unwrap();
        std::thread::sleep(Duration::from_millis(230));
        let mut take = ScreenRun::start(area, &filter, writer.clone(), Some(50)).unwrap();
        wait(&take, |event| matches!(event, StreamEvent::Started));
        if writer.lock().unwrap().as_ref().unwrap().count == index {
            wait(&take, |event| matches!(event, StreamEvent::Frame));
        }
        stop(&mut take);
        assert_eq!(writer.lock().unwrap().as_ref().unwrap().count, index + 1);
    }
    let file = writer.lock().unwrap().take().unwrap().finish(Instant::now()).unwrap();
    assert_eq!(process(&file, &[red, blue]), vec![(0, 50), (1, 50)]);
    fs::copy(&file.path, dir.join("verified-manual.gif")).unwrap();
    fs::write(dir.join("color"), "red").unwrap();
    std::thread::sleep(Duration::from_millis(230));
    *writer.lock().unwrap() = Some(Animation::new(area.pixels).unwrap());
    let mut clip = ScreenRun::start(area, &filter, writer.clone(), None).unwrap();
    wait(&clip, |event| matches!(event, StreamEvent::Started));
    if writer.lock().unwrap().as_ref().unwrap().count == 0 {
        wait(&clip, |event| matches!(event, StreamEvent::Frame));
    }
    let started = Instant::now();
    std::thread::sleep(Duration::from_millis(320));
    fs::write(dir.join("color"), "blue").unwrap();
    std::thread::sleep(Duration::from_millis(320));
    stop(&mut clip);
    let file = writer.lock().unwrap().take().unwrap().finish(Instant::now()).unwrap();
    let frames = process(&file, &[red, blue]);
    assert_eq!(frames.first().unwrap().0, 0);
    assert_eq!(frames.last().unwrap().0, 1);
    assert!(frames.iter().filter(|(color, _)| *color == 0).map(|(_, delay)| u32::from(*delay)).sum::<u32>() >= 30);
    assert!(started.elapsed() < Duration::from_secs(5));
    fs::copy(&file.path, dir.join("verified-clip.gif")).unwrap();
    println!("ScreenCaptureKit fixture: {:?} RGB crop, manual and recorded changes decoded in order, clip retained a 320 ms static hold", area.pixels);
}
