use std::{
    fs::File,
    io::{BufWriter, Write},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc::{self, Receiver, Sender}},
    time::{Duration, Instant},
};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, rc::Retained, runtime::ProtocolObject};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::*;
use objc2_foundation::*;
use objc2_screen_capture_kit::*;

use crate::mac::CaptureFile;

pub(crate) const CLIP_LIMIT: Duration = Duration::from_secs(12);
pub(crate) const CLIP_FPS: u16 = 10;
pub(crate) const MANUAL_LIMIT: usize = 60;
const MAX_EDGE: f64 = 720.0;
const CLIP_FRAMES: usize = CLIP_LIMIT.as_secs() as usize * CLIP_FPS as usize;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode { Clip, Manual }

#[derive(Clone, Copy)]
pub(crate) struct Area {
    pub display: u32,
    // Top-left relative to this display, in logical points as required by ScreenCaptureKit.
    pub source: NSRect,
    pub pixels: (u16, u16),
}

impl Area {
    pub fn from_selection(display: u32, screen_height: f64, scale: f64, selection: NSRect) -> Self {
        let width = selection.size.width * scale;
        let height = selection.size.height * scale;
        let factor = (MAX_EDGE / width.max(height)).min(1.0);
        let even_pixels = |size: f64| (size * factor / 2.0).floor().max(1.0) as u16 * 2;
        Self {
            display,
            source: NSRect::new(NSPoint::new(selection.origin.x, screen_height - selection.origin.y - selection.size.height), selection.size),
            pixels: (even_pixels(width), even_pixels(height)),
        }
    }
}

pub(crate) struct DisplayFilter(Retained<SCContentFilter>);

// ScreenCaptureKit produces this filter on its completion queue. We move it
// once to the controller and never mutate the filter after construction.
unsafe impl Send for DisplayFilter {}

pub(crate) fn display_filter(area: Area, own_windows: Vec<u32>) -> Receiver<Result<DisplayFilter, String>> {
    let (send, receive) = mpsc::channel();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let result = match unsafe { content.as_ref() } {
            Some(content) => filter_for_content(content, area, &own_windows),
            None => Err(native_error(error, "Could not read the current display")),
        };
        let _ = send.send(result);
    });
    // The framework copies and invokes the completion block with live objects.
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(false, true, &handler); }
    receive
}

fn filter_for_content(content: &SCShareableContent, area: Area, own_windows: &[u32]) -> Result<DisplayFilter, String> {
    // All inputs come from the current shareable-content result, on its callback queue.
    unsafe {
        let displays = content.displays();
        let display = displays.iter().find(|display| display.displayID() == area.display).ok_or("The display was disconnected. Select an area again.")?;
        let applications = content.applications();
        let pid = std::process::id() as i32;
        let filter = if let Some(app) = applications.iter().find(|app| app.processID() == pid) {
            let excluded = NSArray::from_slice(&[&*app]);
            SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(SCContentFilter::alloc(), &display, &excluded, &NSArray::new())
        } else {
            let windows = content.windows();
            let excluded: Vec<_> = windows.iter().filter(|window| own_windows.contains(&window.windowID())).collect();
            let excluded = NSArray::from_retained_slice(&excluded);
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &excluded)
        };
        Ok(DisplayFilter(filter))
    }
}

pub(crate) struct Animation {
    writer: gif::Encoder<BufWriter<File>>,
    file: CaptureFile,
    pixels: (u16, u16),
    pending: Option<(gif::Frame<'static>, Instant)>,
    first: Option<Instant>,
    pub count: usize,
}

impl Animation {
    pub fn new(pixels: (u16, u16)) -> Result<Self, String> {
        let file = CaptureFile::new("recording.gif").map_err(|error| error.to_string())?;
        let output = File::create(&file.path).map_err(|error| error.to_string())?;
        let mut writer = gif::Encoder::new(BufWriter::new(output), pixels.0, pixels.1, &[]).map_err(|error| error.to_string())?;
        writer.set_repeat(gif::Repeat::Infinite).map_err(|error| error.to_string())?;
        Ok(Self { writer, file, pixels, pending: None, first: None, count: 0 })
    }

    fn push(&mut self, rgb: &[u8], now: Instant, delay: Option<u16>) -> Result<bool, String> {
        let limit = if delay.is_some() { MANUAL_LIMIT } else { CLIP_FRAMES };
        if self.count >= limit || (delay.is_none() && self.first.is_some_and(|first| now.duration_since(first) >= CLIP_LIMIT)) {
            return Ok(false);
        }
        let mut frame = gif::Frame::from_rgb_speed(self.pixels.0, self.pixels.1, rgb, 20);
        if let Some(delay) = delay {
            frame.delay = delay;
            self.writer.write_frame(&frame).map_err(|error| error.to_string())?;
        } else {
            // ScreenCaptureKit can skip unchanged frames. The time until the next
            // image goes on the previous frame, preserving pauses on playback.
            self.write_pending(now)?;
            self.pending = Some((frame, now));
            self.first.get_or_insert(now);
        }
        self.count += 1;
        Ok(true)
    }

    fn write_pending(&mut self, end: Instant) -> Result<(), String> {
        if let Some((mut frame, at)) = self.pending.take() {
            let gap = end.saturating_duration_since(at).min(CLIP_LIMIT);
            frame.delay = (gap.as_millis().div_ceil(10) as u16).max(2);
            self.writer.write_frame(&frame).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn finish(mut self, at: Instant) -> Result<CaptureFile, String> {
        let at = self.first.map_or(at, |first| at.min(first + CLIP_LIMIT));
        self.write_pending(at)?;
        self.writer.into_inner().and_then(|mut file| Ok(file.flush()?)).map_err(|error| error.to_string())?;
        Ok(self.file)
    }
}

pub(crate) enum StreamEvent { Started, Frame, Limit, Stopped, Failed(String), WriteFailed(String) }

struct OutputState {
    recording: Arc<Mutex<Option<Animation>>>,
    accept: Arc<AtomicBool>,
    manual_delay: Option<u16>,
    pixels: (u16, u16),
    events: Sender<StreamEvent>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    #[ivars = OutputState]
    struct FrameOutput;

    unsafe impl NSObjectProtocol for FrameOutput {}

    unsafe impl SCStreamOutput for FrameOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn sample(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen || !self.ivars().accept.load(Ordering::Acquire) { return; }
            if !has_frame(sample) { return; }
            let time = Instant::now();
            let Some(pixels) = copy_frame(sample, self.ivars().pixels) else { return; };
            let mut recording = self.ivars().recording.lock().expect("GIF writer lock");
            if !self.ivars().accept.load(Ordering::Acquire) { return; }
            let Some(animation) = recording.as_mut() else { return; };
            match animation.push(&pixels, time, self.ivars().manual_delay) {
                Ok(true) => {
                    if self.ivars().manual_delay.is_some() {
                        self.ivars().accept.store(false, Ordering::Release);
                    }
                    let _ = self.ivars().events.send(StreamEvent::Frame);
                }
                Ok(false) => {
                    self.ivars().accept.store(false, Ordering::Release);
                    let _ = self.ivars().events.send(StreamEvent::Limit);
                }
                Err(error) => {
                    self.ivars().accept.store(false, Ordering::Release);
                    let _ = self.ivars().events.send(StreamEvent::WriteFailed(error));
                }
            }
        }
    }

    unsafe impl SCStreamDelegate for FrameOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn failed(&self, _stream: &SCStream, error: &NSError) {
            self.ivars().accept.store(false, Ordering::Release);
            let _ = self.ivars().events.send(StreamEvent::Failed(error.localizedDescription().to_string()));
        }
    }
);

fn has_frame(sample: &CMSampleBuffer) -> bool {
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else { return false; };
    // ScreenCaptureKit supplies CF dictionaries with CFNumber status values.
    // The attachment array stays retained while it is viewed as Foundation objects.
    let values = unsafe { &*std::ptr::from_ref(&*attachments).cast::<NSArray<NSDictionary<NSString, NSNumber>>>() };
    let status = values.firstObject().and_then(|record| record.objectForKey(unsafe { SCStreamFrameInfoStatus }));
    status.is_some_and(|status| matches!(SCFrameStatus(status.integerValue()), SCFrameStatus::Complete | SCFrameStatus::Started))
}

fn copy_frame(sample: &CMSampleBuffer, pixels: (u16, u16)) -> Option<Vec<u8>> {
    let image = unsafe { sample.image_buffer()? };
    let (width, height) = (usize::from(pixels.0), usize::from(pixels.1));
    if CVPixelBufferGetPixelFormatType(&image) != kCVPixelFormatType_32BGRA
        || CVPixelBufferGetWidth(&image) != width || CVPixelBufferGetHeight(&image) != height
        || CVPixelBufferIsPlanar(&image)
    { return None; }
    let flags = CVPixelBufferLockFlags::ReadOnly;
    if unsafe { CVPixelBufferLockBaseAddress(&image, flags) } != 0 { return None; }
    let rgb = (|| {
        let base = CVPixelBufferGetBaseAddress(&image).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(&image);
        let len = stride.checked_mul(height.checked_sub(1)?)?.checked_add(width * 4)?;
        if base.is_null() || stride < width * 4 || len > CVPixelBufferGetDataSize(&image) { return None; }
        let mut rgb = Vec::with_capacity(width * height * 3);
        // The locked, interleaved buffer has at least the validated row span.
        let bytes = unsafe { std::slice::from_raw_parts(base, len) };
        for row in 0..height {
            for pixel in bytes[row * stride..row * stride + width * 4].chunks_exact(4) {
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
        }
        Some(rgb)
    })();
    let _ = unsafe { CVPixelBufferUnlockBaseAddress(&image, flags) };
    rgb
}

pub(crate) struct ScreenRun {
    stream: Retained<SCStream>,
    _output: Retained<FrameOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    accept: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    pub events: Receiver<StreamEvent>,
    send: Sender<StreamEvent>,
    pub requested_at: Instant,
    pub stop_at: Option<Instant>,
    pub has_stopped: bool,
}

impl ScreenRun {
    pub fn start(area: Area, filter: &DisplayFilter, recording: Arc<Mutex<Option<Animation>>>, manual_delay: Option<u16>) -> Result<Self, String> {
        let (send, events) = mpsc::channel();
        let accept = Arc::new(AtomicBool::new(true));
        let cancelled = Arc::new(AtomicBool::new(false));
        let output = FrameOutput::alloc().set_ivars(OutputState {
            recording, accept: accept.clone(), manual_delay, pixels: area.pixels, events: send.clone(),
        });
        let output: Retained<FrameOutput> = unsafe { msg_send![super(output), init] };
        let queue = DispatchQueue::new("com.taiyi.qingjie.gif.samples", None);
        // Source geometry comes from an on-screen selection and output is capped to MAX_EDGE.
        let stream = unsafe {
            let config = SCStreamConfiguration::new();
            config.setSourceRect(area.source);
            config.setWidth(area.pixels.0.into());
            config.setHeight(area.pixels.1.into());
            config.setScalesToFit(true);
            config.setMinimumFrameInterval(CMTime::new(1, CLIP_FPS.into()));
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setShowsCursor(false);
            config.setCapturesAudio(false);
            config.setQueueDepth(3);
            let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter.0, &config, Some(ProtocolObject::from_ref(&*output)));
            stream.addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&queue))
                .map_err(|error| error.localizedDescription().to_string())?;
            stream
        };
        let retained_stream = stream.clone();
        let start_send = send.clone();
        let abandoned = cancelled.clone();
        let started = RcBlock::new(move |error: *mut NSError| {
            if abandoned.load(Ordering::Acquire) {
                // Closing during startup may beat the first stop request. Stop again
                // after native startup if the controller has already let this run go.
                if error.is_null() { unsafe { retained_stream.stopCaptureWithCompletionHandler(None); } }
            } else if error.is_null() {
                let _ = start_send.send(StreamEvent::Started);
            } else {
                let _ = start_send.send(StreamEvent::Failed(native_error(error, "Could not start screen capture")));
            }
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&started)); }
        Ok(Self { stream, _output: output, _queue: queue, accept, cancelled, events, send, requested_at: Instant::now(), stop_at: None, has_stopped: false })
    }

    pub fn stop(&mut self) {
        if self.stop_at.is_some() { return; }
        self.stop_at = Some(Instant::now());
        self.accept.store(false, Ordering::Release);
        let send = self.send.clone();
        let stopped = RcBlock::new(move |error: *mut NSError| {
            let event = if error.is_null() { StreamEvent::Stopped } else { StreamEvent::Failed(native_error(error, "Could not stop screen capture")) };
            let _ = send.send(event);
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&stopped)); }
    }
}

impl Drop for ScreenRun {
    fn drop(&mut self) {
        self.accept.store(false, Ordering::Release);
        self.cancelled.store(true, Ordering::Release);
        if !self.has_stopped {
            // No callback is needed on teardown. The startup handler also checks cancellation.
            unsafe { self.stream.stopCaptureWithCompletionHandler(None); }
        }
    }
}

fn native_error(error: *mut NSError, fallback: &str) -> String {
    // Called while the completion block's NSError is still live.
    unsafe { error.as_ref() }.map_or_else(|| fallback.to_string(), |error| error.localizedDescription().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retina_selection_flips_y_without_scaling_the_source_rect() {
        let selection = NSRect::new(NSPoint::new(125.0, 85.0), NSSize::new(480.0, 240.0));
        let area = Area::from_selection(9, 900.0, 2.0, selection);
        assert_eq!(area.display, 9);
        assert_eq!(area.source, NSRect::new(NSPoint::new(125.0, 575.0), selection.size));
        assert_eq!(area.pixels, (720, 360));
    }

    #[test]
    fn exported_frames_decode_in_order_with_manual_and_recorded_delays() {
        let now = Instant::now();
        for delay in [Some(50), None] {
            let mut recording = Animation::new((20, 10)).unwrap();
            for (at, color) in [(now, [255, 0, 0]), (now + Duration::from_millis(320), [0, 0, 255])] {
                assert!(recording.push(&color.repeat(200), at, delay).unwrap());
            }
            let file = recording.finish(now + Duration::from_millis(720)).unwrap();
            let mut decoder = gif::DecodeOptions::new();
            decoder.set_color_output(gif::ColorOutput::RGBA);
            let mut decoder = decoder.read_info(File::open(&file.path).unwrap()).unwrap();
            assert_eq!((decoder.width(), decoder.height()), (20, 10));
            let first = decoder.read_next_frame().unwrap().unwrap();
            assert_eq!(&first.buffer[..4], &[255, 0, 0, 255]);
            assert_eq!(first.delay, delay.unwrap_or(32));
            let second = decoder.read_next_frame().unwrap().unwrap();
            assert_eq!(&second.buffer[..4], &[0, 0, 255, 255]);
            assert_eq!(second.delay, delay.unwrap_or(40));
            assert!(decoder.read_next_frame().unwrap().is_none());
            let path = file.path.clone();
            drop(file);
            assert!(!path.exists());
        }
    }
}
