#![allow(unsafe_code)]
//! ScreenCaptureKit NV12 capture adapter.

use super::{
    control::{
        CaptureStallWatchdog, MacCaptureController, MacCaptureNotice, MacCapturePath,
        MacCaptureSource,
    },
    screen_recording_access, MacDisplay, ScreenRecordingAccess,
};
use crate::bounded_capture_dimensions;
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread, DefinedClass,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString};
#[allow(deprecated)]
use objc2_core_graphics::{
    kCGDisplayStreamShowCursor, CGDisplayStream, CGDisplayStreamFrameAvailableHandler,
    CGDisplayStreamFrameStatus, CGDisplayStreamUpdate, CGError,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreateWithIOSurface, CVPixelBufferGetHeight,
    CVPixelBufferGetIOSurface, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_io_surface::IOSurfaceRef;
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};
use std::ptr::NonNull;
use std::{
    collections::VecDeque,
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

const NV12_VIDEO_RANGE: u32 = 0x3432_3076;
const MAX_PENDING_NOTICES: usize = 16;
/// A 30 fps capture source is considered stalled after two seconds without a valid frame.
const CAPTURE_FRAME_STALL_TIMEOUT: Duration = Duration::from_secs(2);
// M9 captures the Mac pointer in the video and suppresses separate cursor overlays.
const SHOW_CURSOR_IN_VIDEO: bool = true;

/// macOS host capture defaults, starting at 720p30.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacCaptureConfig {
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
}
impl Default for MacCaptureConfig {
    fn default() -> Self {
        Self {
            max_width: 1280,
            max_height: 720,
            fps: 30,
        }
    }
}

/// Typed failure from starting or controlling macOS screen capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MacCaptureError {
    PermissionMissing,
    AlreadyStarted,
    NotStarted,
    DisplayUnavailable,
    Platform(String),
}
impl fmt::Display for MacCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionMissing => f.write_str("Screen Recording permission is required"),
            Self::AlreadyStarted => f.write_str("macOS display capture is already started"),
            Self::NotStarted => f.write_str("macOS display capture has not started"),
            Self::DisplayUnavailable => f.write_str("selected macOS display is unavailable"),
            Self::Platform(message) => write!(f, "macOS capture failed: {message}"),
        }
    }
}
impl std::error::Error for MacCaptureError {}

/// Retained NV12 CVPixelBuffer handed directly to a native macOS encoder.
pub struct MacCapturedFrame {
    pixel_buffer: CFRetained<CVPixelBuffer>,
    pub native_display_id: u32,
    pub width: u32,
    pub height: u32,
    pub capture_ts_us: u64,
}
impl fmt::Debug for MacCapturedFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacCapturedFrame")
            .field("native_display_id", &self.native_display_id)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("capture_ts_us", &self.capture_ts_us)
            .finish_non_exhaustive()
    }
}
impl MacCapturedFrame {
    /// Borrows the retained IOSurface-backed buffer without a CPU pixel copy.
    pub fn pixel_buffer(&self) -> &CVPixelBuffer {
        &self.pixel_buffer
    }
}
// SAFETY: The retained CoreVideo frame is immutable capture output transferred by ownership.
unsafe impl Send for MacCapturedFrame {}

/// Event produced by the macOS capture adapter.
pub enum MacCaptureEvent {
    Frame(MacCapturedFrame),
    Lifecycle(MacCaptureNotice),
    Error(String),
}
impl fmt::Debug for MacCaptureEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(frame) => f.debug_tuple("Frame").field(frame).finish(),
            Self::Lifecycle(event) => f.debug_tuple("Lifecycle").field(event).finish(),
            Self::Error(message) => f.debug_tuple("Error").field(message).finish(),
        }
    }
}

struct SharedState {
    latest_frame: Option<MacCapturedFrame>,
    notices: VecDeque<MacCaptureEvent>,
    origin: Instant,
    watchdog: CaptureStallWatchdog,
}
struct Shared {
    state: Mutex<SharedState>,
    ready: Condvar,
}
impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(SharedState {
                latest_frame: None,
                notices: VecDeque::new(),
                origin: Instant::now(),
                watchdog: CaptureStallWatchdog::default(),
            }),
            ready: Condvar::new(),
        }
    }
    fn notice(&self, event: MacCaptureEvent) {
        let mut state = lock_unpoisoned(&self.state);
        if state.notices.len() == MAX_PENDING_NOTICES {
            state.notices.pop_front();
        }
        state.notices.push_back(event);
        self.ready.notify_one();
    }
    fn frame(&self, frame: MacCapturedFrame) {
        let mut state = lock_unpoisoned(&self.state);
        if state.watchdog.frame_received(frame.capture_ts_us) {
            let id = frame.native_display_id;
            if state.notices.len() == MAX_PENDING_NOTICES {
                state.notices.pop_front();
            }
            state
                .notices
                .push_back(MacCaptureEvent::Lifecycle(MacCaptureNotice::Recovered {
                    display_id: id,
                }));
        }
        state.latest_frame = Some(frame);
        self.ready.notify_one();
    }
    fn access_lost(&self, id: u32) {
        let mut state = lock_unpoisoned(&self.state);
        if state.watchdog.interrupt() {
            if state.notices.len() == MAX_PENDING_NOTICES {
                state.notices.pop_front();
            }
            state
                .notices
                .push_back(MacCaptureEvent::Lifecycle(MacCaptureNotice::AccessLost {
                    display_id: id,
                }));
        }
        self.ready.notify_one();
    }
    fn check_stalled(&self, display_id: u32) {
        let now_us = u64::try_from(lock_unpoisoned(&self.state).origin.elapsed().as_micros())
            .unwrap_or(u64::MAX);
        let mut state = lock_unpoisoned(&self.state);
        if state
            .watchdog
            .check_stalled(now_us, CAPTURE_FRAME_STALL_TIMEOUT)
        {
            if state.notices.len() == MAX_PENDING_NOTICES {
                state.notices.pop_front();
            }
            state
                .notices
                .push_back(MacCaptureEvent::Lifecycle(MacCaptureNotice::FrameStalled {
                    display_id,
                }));
            self.ready.notify_one();
        }
    }
    fn capture_started(&self) {
        let now_us = u64::try_from(lock_unpoisoned(&self.state).origin.elapsed().as_micros())
            .unwrap_or(u64::MAX);
        lock_unpoisoned(&self.state).watchdog.start(now_us);
    }
    fn capture_stopped(&self) {
        lock_unpoisoned(&self.state).watchdog.stop();
    }
    fn activity(&self, timestamp_us: u64) {
        lock_unpoisoned(&self.state).watchdog.activity(timestamp_us);
    }
    fn timestamp_us(&self) -> u64 {
        u64::try_from(lock_unpoisoned(&self.state).origin.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

#[derive(Clone)]
struct OutputIvars {
    shared: Arc<Shared>,
    display_id: u32,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = OutputIvars]
    struct CaptureOutput;

    // SAFETY: CaptureOutput is initialized as an NSObject subclass with valid ivars before use.
    unsafe impl NSObjectProtocol for CaptureOutput {}
    // SAFETY: The output method borrows callback arguments only for the callback and retains the pixel buffer before returning.
    unsafe impl SCStreamOutput for CaptureOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        #[allow(non_snake_case)]
        // SAFETY: ScreenCaptureKit invokes this method with valid arguments for the callback duration.
        unsafe fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            output_type: SCStreamOutputType,
        ) {
            if output_type != SCStreamOutputType::Screen {
                return;
            }
            // SAFETY: ScreenCaptureKit supplies a valid sample buffer during this callback.
            let Some(pixel_buffer) = (unsafe { sample.image_buffer() }) else {
                self.ivars().shared.notice(MacCaptureEvent::Error(
                    "screen sample had no image buffer".to_owned(),
                ));
                return;
            };
            let width = CVPixelBufferGetWidth(&pixel_buffer);
            let height = CVPixelBufferGetHeight(&pixel_buffer);
            let format = CVPixelBufferGetPixelFormatType(&pixel_buffer);
            // SAFETY: The retained surface is used only to verify zero-copy IOSurface backing.
            let surface = CVPixelBufferGetIOSurface(Some(&pixel_buffer));
            if format != NV12_VIDEO_RANGE || surface.is_none() {
                self.ivars().shared.notice(MacCaptureEvent::Error(
                    "capture did not provide IOSurface-backed 420v NV12".to_owned(),
                ));
                return;
            }
            let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
                return;
            };
            if width == 0 || height == 0 {
                return;
            }
            let elapsed = lock_unpoisoned(&self.ivars().shared.state)
                .origin
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            self.ivars().shared.frame(MacCapturedFrame {
                pixel_buffer,
                native_display_id: self.ivars().display_id,
                width,
                height,
                capture_ts_us: elapsed,
            });
        }
    }
    // SAFETY: Delegate callbacks only update the synchronized shared lifecycle state.
    unsafe impl SCStreamDelegate for CaptureOutput {
        #[unsafe(method(stream:didStopWithError:))]
        #[allow(non_snake_case)]
        // SAFETY: The callback error is intentionally not dereferenced; the shared state is synchronized.
        unsafe fn stream_didStopWithError(&self, _stream: &SCStream, _error: &NSError) {
            self.ivars().shared.access_lost(self.ivars().display_id);
        }
    }
);

impl CaptureOutput {
    fn new(shared: Arc<Shared>, display_id: u32) -> Retained<Self> {
        let allocated = Self::alloc().set_ivars(OutputIvars { shared, display_id });
        // SAFETY: This NSObject subclass initializes its ivars before calling its superclass init.
        unsafe { msg_send![super(allocated), init] }
    }
}

struct ActiveScreenCaptureKit {
    stream: Retained<SCStream>,
    _output: Retained<CaptureOutput>,
    _queue: DispatchRetained<DispatchQueue>,
}
type DisplayHandler =
    dyn Fn(CGDisplayStreamFrameStatus, u64, *mut IOSurfaceRef, *const CGDisplayStreamUpdate);
struct ActiveCGDisplayStream {
    stream: CFRetained<CGDisplayStream>,
    _handler: RcBlock<DisplayHandler>,
    _queue: DispatchRetained<DispatchQueue>,
    stopped: std::sync::mpsc::Receiver<()>,
    stopping: Arc<AtomicBool>,
}
enum ActiveCapture {
    ScreenCaptureKit(ActiveScreenCaptureKit),
    CGDisplayStream(ActiveCGDisplayStream),
}
struct AppleSource {
    config: MacCaptureConfig,
    shared: Arc<Shared>,
    active: Option<ActiveCapture>,
    source_size: Option<(u32, u32)>,
}
impl AppleSource {
    fn new(config: MacCaptureConfig, shared: Arc<Shared>) -> Self {
        Self {
            config,
            shared,
            active: None,
            source_size: None,
        }
    }
    fn start_screen_capture_kit(&mut self, display_id: u32) -> Result<(), String> {
        let content = shareable_content()?;
        // SAFETY: The returned display array is retained for the selection loop.
        let displays = unsafe { content.displays() };
        let mut selected = None;
        for index in 0..displays.count() {
            let display = displays.objectAtIndex(index);
            // SAFETY: The display object is retained and this is a scalar getter.
            if unsafe { display.displayID() } == display_id {
                selected = Some(display);
                break;
            }
        }
        let display =
            selected.ok_or_else(|| "display is absent from ScreenCaptureKit".to_owned())?;
        // SAFETY: Width and height are scalar queries on the retained display object.
        let source_width = unsafe { display.width() };
        // SAFETY: Width and height are scalar queries on the retained display object.
        let source_height = unsafe { display.height() };
        let (width, height) = bounded_capture_dimensions(
            source_width as u32,
            source_height as u32,
            self.config.max_width.clamp(1, 1920),
            self.config.max_height.clamp(1, 1080),
        );
        let (width, height) = (width & !1, height & !1);
        if width == 0 || height == 0 || self.config.fps != 30 {
            return Err("capture requires positive even dimensions at 30 fps".to_owned());
        }
        let excluded = NSArray::<SCWindow>::new();
        // SAFETY: The display and an empty, valid window array are retained for filter creation.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &excluded,
            )
        };
        // SAFETY: new is the documented NSObject initializer for SCStreamConfiguration.
        let configuration = unsafe { SCStreamConfiguration::new() };
        // SAFETY: The dimensions are bounded and even, capture rate is fixed at 30, the Mac
        // cursor is intentionally baked into the image per M9, and audio capture remains disabled.
        unsafe {
            configuration.setWidth(width as usize);
            configuration.setHeight(height as usize);
            configuration.setMinimumFrameInterval(CMTime::new(1, 30));
            configuration.setPixelFormat(NV12_VIDEO_RANGE);
            configuration.setShowsCursor(SHOW_CURSOR_IN_VIDEO);
            configuration.setQueueDepth(3);
            configuration.setCapturesAudio(false);
        }
        let output = CaptureOutput::new(self.shared.clone(), display_id);
        let output_protocol: &ProtocolObject<dyn SCStreamOutput> =
            ProtocolObject::from_ref(&*output);
        let delegate_protocol: &ProtocolObject<dyn SCStreamDelegate> =
            ProtocolObject::from_ref(&*output);
        // SAFETY: Filter, configuration, and delegate objects remain retained during initialization.
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &configuration,
                Some(delegate_protocol),
            )
        };
        let queue = DispatchQueue::new("com.racc-connect.screen-capture", None);
        // SAFETY: Queue and output are retained in ActiveStream until stream stop completes.
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    output_protocol,
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|_| "ScreenCaptureKit rejected the screen output".to_owned())?;
        }
        wait_for_stream_completion(|completion| {
            // SAFETY: The stream is retained through the asynchronous start completion.
            unsafe {
                stream.startCaptureWithCompletionHandler(Some(completion));
            }
        })?;
        self.active = Some(ActiveCapture::ScreenCaptureKit(ActiveScreenCaptureKit {
            stream,
            _output: output,
            _queue: queue,
        }));
        Ok(())
    }
    #[allow(deprecated)]
    fn start_cg_display_stream(&mut self, display_id: u32) -> Result<(), String> {
        let (width, height) = self
            .source_size
            .ok_or_else(|| "capture display size was not set".to_owned())?;
        let (width, height) = bounded_capture_dimensions(
            width,
            height,
            self.config.max_width.clamp(1, 1920),
            self.config.max_height.clamp(1, 1080),
        );
        let (width, height) = (width & !1, height & !1);
        if width == 0 || height == 0 || self.config.fps != 30 {
            return Err("CGDisplayStream requires positive even dimensions at 30 fps".to_owned());
        }
        let queue = DispatchQueue::new("com.racc-connect.cg-display-stream", None);
        let (stopped_tx, stopped_rx) = std::sync::mpsc::sync_channel(1);
        let shared = self.shared.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let callback_stopping = Arc::clone(&stopping);
        let handler = RcBlock::new(
            move |status: CGDisplayStreamFrameStatus,
                  _display_time: u64,
                  surface_ptr: *mut IOSurfaceRef,
                  _update: *const CGDisplayStreamUpdate| {
                if status == CGDisplayStreamFrameStatus::Stopped {
                    if !callback_stopping.load(Ordering::Acquire) {
                        shared.access_lost(display_id);
                    }
                    let _ = stopped_tx.try_send(());
                    return;
                }
                if status == CGDisplayStreamFrameStatus::FrameIdle
                    || status == CGDisplayStreamFrameStatus::FrameBlank
                {
                    // CoreGraphics explicitly reports idle callbacks when the display is
                    // unchanged; they prove the stream is alive without producing a new image.
                    shared.activity(shared.timestamp_us());
                    return;
                }
                if status != CGDisplayStreamFrameStatus::FrameComplete {
                    return;
                }
                let Some(surface_ptr) = NonNull::new(surface_ptr) else {
                    return;
                };
                // SAFETY: CoreGraphics provides this IOSurface only for the callback; retain it before returning.
                let surface = unsafe { CFRetained::<IOSurfaceRef>::retain(surface_ptr) };
                let mut raw_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
                // SAFETY: The retained IOSurface is valid and raw_buffer is a writable output slot.
                let result = unsafe {
                    CVPixelBufferCreateWithIOSurface(
                        None,
                        &surface,
                        None,
                        NonNull::from(&mut raw_buffer),
                    )
                };
                if result != 0 {
                    shared.notice(MacCaptureEvent::Error(format!(
                        "CVPixelBufferCreateWithIOSurface failed ({result})"
                    )));
                    return;
                }
                let Some(raw_buffer) = NonNull::new(raw_buffer) else {
                    shared.notice(MacCaptureEvent::Error(
                        "CoreVideo returned a null pixel buffer".to_owned(),
                    ));
                    return;
                };
                // SAFETY: CoreVideo returned the pixel buffer at +1 ownership on successful creation.
                let pixel_buffer = unsafe { CFRetained::<CVPixelBuffer>::from_raw(raw_buffer) };
                let pixel_format = CVPixelBufferGetPixelFormatType(&pixel_buffer);
                if pixel_format != NV12_VIDEO_RANGE {
                    shared.notice(MacCaptureEvent::Error(
                        "CGDisplayStream did not preserve 420v NV12".to_owned(),
                    ));
                    return;
                }
                let (Ok(frame_width), Ok(frame_height)) = (
                    u32::try_from(CVPixelBufferGetWidth(&pixel_buffer)),
                    u32::try_from(CVPixelBufferGetHeight(&pixel_buffer)),
                ) else {
                    return;
                };
                if frame_width == 0 || frame_height == 0 {
                    return;
                }
                let elapsed = lock_unpoisoned(&shared.state)
                    .origin
                    .elapsed()
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64;
                shared.frame(MacCapturedFrame {
                    pixel_buffer,
                    native_display_id: display_id,
                    width: frame_width,
                    height: frame_height,
                    capture_ts_us: elapsed,
                });
            },
        );
        // SAFETY: CoreGraphics owns the static property key, the dictionary retains its values,
        // and the callback block and dispatch queue are kept alive in ActiveCGDisplayStream.
        let show_cursor_key = unsafe { kCGDisplayStreamShowCursor };
        let properties = CFDictionary::<CFString, CFBoolean>::from_slices(
            &[show_cursor_key],
            &[CFBoolean::new(SHOW_CURSOR_IN_VIDEO)],
        );
        // SAFETY: Display ID came from the active CoreGraphics display list; dimensions are bounded,
        // even, and nonzero; 420v is supported; and the retained queue/block outlive the stream.
        let stream = unsafe {
            CGDisplayStream::with_dispatch_queue(
                display_id,
                width as usize,
                height as usize,
                NV12_VIDEO_RANGE as i32,
                Some(properties.as_ref()),
                &queue,
                RcBlock::as_ptr(&handler) as CGDisplayStreamFrameAvailableHandler,
            )
        }
        .ok_or_else(|| "CoreGraphics could not create a display stream".to_owned())?;
        let started = CGDisplayStream::start(Some(&stream));
        if started != CGError::Success {
            return Err(format!("CGDisplayStream start failed ({})", started.0));
        }
        self.active = Some(ActiveCapture::CGDisplayStream(ActiveCGDisplayStream {
            stream,
            _handler: handler,
            _queue: queue,
            stopped: stopped_rx,
            stopping,
        }));
        Ok(())
    }
    #[allow(deprecated)]
    fn stop_active(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        match active {
            ActiveCapture::ScreenCaptureKit(active) => {
                if let Err(error) = wait_for_stream_completion(|completion| {
                    // SAFETY: Stream, output delegate, and queue live through the stop completion.
                    unsafe {
                        active
                            .stream
                            .stopCaptureWithCompletionHandler(Some(completion));
                    }
                }) {
                    self.shared.notice(MacCaptureEvent::Error(format!(
                        "ScreenCaptureKit did not finish its stop operation; resources remain retained: {error}"
                    )));
                    self.active = Some(ActiveCapture::ScreenCaptureKit(active));
                }
            }
            ActiveCapture::CGDisplayStream(active) => {
                active.stopping.store(true, Ordering::Release);
                let status = CGDisplayStream::stop(Some(&active.stream));
                if status != CGError::Success
                    || active.stopped.recv_timeout(Duration::from_secs(5)).is_err()
                {
                    self.shared.notice(MacCaptureEvent::Error("CGDisplayStream did not finish its stop callback; resources remain retained".to_owned()));
                    self.active = Some(ActiveCapture::CGDisplayStream(active));
                }
            }
        }
    }
}
impl MacCaptureSource for AppleSource {
    fn start(&mut self, display_id: u32) -> Result<MacCapturePath, String> {
        if self.active.is_some() {
            return Err("capture is already started".to_owned());
        }
        match self.start_screen_capture_kit(display_id) {
            Ok(()) => Ok(MacCapturePath::ScreenCaptureKit),
            Err(screen_error) => match self.start_cg_display_stream(display_id) {
                Ok(()) => Ok(MacCapturePath::CGDisplayStream),
                Err(cg_error) => Err(format!("ScreenCaptureKit failed: {screen_error}; CGDisplayStream fallback failed: {cg_error}")),
            },
        }
    }
    fn migrate(&mut self, display_id: u32) -> Result<(), String> {
        if self.active.is_none() {
            return Err("capture has not started".to_owned());
        }
        self.stop_active();
        if self.active.is_some() {
            return Err("previous stream has not completed shutdown".to_owned());
        }
        self.start(display_id).map(|_| ())
    }
    fn stop(&mut self) {
        self.stop_active();
    }
}
impl Drop for AppleSource {
    fn drop(&mut self) {
        self.stop_active();
    }
}

fn shareable_content() -> Result<Retained<SCShareableContent>, String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let completion = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            if !error.is_null() || content.is_null() {
                let _ = sender.send(None);
                return;
            }
            let Some(content) = NonNull::new(content) else {
                let _ = sender.send(None);
                return;
            };
            // SAFETY: ScreenCaptureKit lends this object for the callback; retaining extends its lifetime.
            let retained = unsafe { Retained::retain(content.as_ptr()) };
            let _ = sender.send(retained);
        },
    );
    // SAFETY: ScreenCaptureKit retains this heap-backed completion block until its callback.
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&completion);
    }
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "timed out querying ScreenCaptureKit displays".to_owned())?
        .ok_or_else(|| "ScreenCaptureKit could not enumerate shareable content".to_owned())
}

fn wait_for_stream_completion(
    start: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let completion = RcBlock::new(move |error: *mut NSError| {
        let _ = sender.send(error.is_null());
    });
    start(&completion);
    match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(true) => Ok(()),
        Ok(false) => Err("ScreenCaptureKit operation returned an error".to_owned()),
        Err(_) => Err("timed out waiting for ScreenCaptureKit".to_owned()),
    }
}

/// macOS-only capture adapter. Its frames retain CVPixelBuffer and do not masquerade as D3D GpuFrame.
pub struct MacCaptureBackend {
    controller: MacCaptureController<AppleSource>,
    shared: Arc<Shared>,
}
impl MacCaptureBackend {
    /// Creates an idle backend; construction does not enumerate displays or request permission.
    pub fn new(config: MacCaptureConfig) -> Self {
        let shared = Arc::new(Shared::new());
        Self {
            controller: MacCaptureController::new(AppleSource::new(config, shared.clone())),
            shared,
        }
    }

    /// Starts capture after a read-only Screen Recording permission preflight.
    pub fn start(&mut self, display: &MacDisplay) -> Result<(), MacCaptureError> {
        if self.controller.selected_display().is_some() {
            return Err(MacCaptureError::AlreadyStarted);
        }
        if display.native_display_id == 0 {
            return Err(MacCaptureError::DisplayUnavailable);
        }
        if screen_recording_access() == ScreenRecordingAccess::Missing {
            self.shared.notice(MacCaptureEvent::Error(
                MacCaptureError::PermissionMissing.to_string(),
            ));
            return Err(MacCaptureError::PermissionMissing);
        }
        self.controller.source_mut().source_size = Some(display.captured.display.size());
        self.controller
            .start(display.native_display_id)
            .map_err(MacCaptureError::Platform)?;
        self.shared.capture_started();
        Ok(())
    }

    /// Switches the running capture source to another display.
    pub fn migrate(&mut self, display: &MacDisplay) -> Result<(), MacCaptureError> {
        if self.controller.selected_display().is_none() {
            return Err(MacCaptureError::NotStarted);
        }
        if display.native_display_id == 0 {
            return Err(MacCaptureError::DisplayUnavailable);
        }
        self.controller.source_mut().source_size = Some(display.captured.display.size());
        self.controller
            .migrate(display.native_display_id)
            .map_err(MacCaptureError::Platform)?;
        self.shared.capture_started();
        Ok(())
    }

    /// Stops capture and releases the active native stream.
    pub fn stop(&mut self) {
        self.controller.stop();
        self.shared.capture_stopped();
    }

    /// Returns the next lifecycle event or latest frame, waiting no longer than timeout.
    pub fn poll_event(&mut self, timeout: Duration) -> Option<MacCaptureEvent> {
        if let Some(notice) = self.controller.poll_notice() {
            return Some(MacCaptureEvent::Lifecycle(notice));
        }
        let mut state = lock_unpoisoned(&self.shared.state);
        if state.latest_frame.is_none() && state.notices.is_empty() && !timeout.is_zero() {
            state = self
                .shared
                .ready
                .wait_timeout(state, timeout)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if let Some(event) = state.notices.pop_front() {
            return Some(event);
        }
        if let Some(frame) = state.latest_frame.take() {
            return Some(MacCaptureEvent::Frame(frame));
        }
        drop(state);
        if let Some(display_id) = self.controller.selected_display() {
            self.shared.check_stalled(display_id);
            if let Some(event) = lock_unpoisoned(&self.shared.state).notices.pop_front() {
                return Some(event);
            }
        }
        None
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
