use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};
use std::sync::Mutex;

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFMutableDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    kCMVideoCodecType_H264, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
};
use objc2_core_video::{
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_Main_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl, VTCompressionSession,
    VTEncodeInfoFlags, VTSessionCopyProperty, VTSessionSetProperty,
};

use crate::{
    videotoolbox_avcc_to_annex_b, EncodeError, EncodedPacket, Encoder, EncoderConfig, EncoderInput,
    EncoderKind, FRAME_RATE, MAX_ENCODED_PACKET_BYTES,
};

type NativeCompressionSession = CFRetained<VTCompressionSession>;

struct CallbackState {
    output: VecDeque<Result<EncodedPacket, String>>,
}

impl CallbackState {
    fn new() -> Self {
        Self {
            output: VecDeque::with_capacity(2),
        }
    }
}

/// Hardware-required H.264 encoder backed by a VideoToolbox compression session.
///
/// Capture should pass the ScreenCaptureKit NV12 pixel buffer directly. This backend performs no
/// CPU color conversion and is only available on macOS targets.
pub struct VideoToolboxH264Encoder {
    session: Option<NativeCompressionSession>,
    callback_state: Box<Mutex<CallbackState>>,
    config: EncoderConfig,
    last_timestamp_us: Option<u64>,
    keyframe_pending: bool,
    low_latency_specification_accepted: bool,
}

impl VideoToolboxH264Encoder {
    /// Creates a hardware-only VideoToolbox encoder at the fixed 30 fps rate.
    pub fn new(config: EncoderConfig) -> Result<Self, EncodeError> {
        let callback_state = Box::new(Mutex::new(CallbackState::new()));
        let callback_refcon = (&*callback_state as *const Mutex<CallbackState>)
            .cast_mut()
            .cast::<c_void>();
        let (session, low_latency_specification_accepted) = match create_session(
            config,
            callback_refcon,
            true,
        ) {
            Ok(session) => (session, true),
            Err(low_latency_error) => match create_session(config, callback_refcon, false) {
                Ok(session) => (session, false),
                Err(hardware_error) => {
                    return Err(EncodeError::NoHardwareEncoder(format!(
                            "VideoToolbox hardware-only session failed (low latency: {low_latency_error}; hardware retry: {hardware_error})"
                        )));
                }
            },
        };

        configure_session(&session, config)?;
        if !read_hardware_acceleration(&session)? {
            return Err(EncodeError::NoHardwareEncoder(
                "VideoToolbox did not report hardware acceleration".to_owned(),
            ));
        }
        // SAFETY: The new session is retained, live, and has not been invalidated.
        let status = unsafe { session.prepare_to_encode_frames() };
        if status != 0 {
            return Err(EncodeError::Backend(format!(
                "VideoToolbox prepare-to-encode failed with OSStatus {status}"
            )));
        }
        Ok(Self {
            session: Some(session),
            callback_state,
            config,
            last_timestamp_us: None,
            keyframe_pending: true,
            low_latency_specification_accepted,
        })
    }

    /// Updates the live target bitrate without replacing the native pixel-buffer session.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<(), EncodeError> {
        let config = EncoderConfig::new(self.config.width(), self.config.height(), bitrate_bps)?;
        configure_session(self.native_session()?, config)?;
        self.config = config;
        Ok(())
    }
    /// Whether the low-latency rate-control encoder specification was accepted.
    pub const fn low_latency_specification_accepted(&self) -> bool {
        self.low_latency_specification_accepted
    }

    /// Hardware use was required and verified during construction.
    pub const fn uses_hardware_acceleration(&self) -> bool {
        true
    }

    fn native_session(&self) -> Result<&NativeCompressionSession, EncodeError> {
        self.session
            .as_ref()
            .ok_or_else(|| EncodeError::Backend("VideoToolbox session is closed".to_owned()))
    }
}

impl Encoder for VideoToolboxH264Encoder {
    fn kind(&self) -> EncoderKind {
        EncoderKind::MacVideoToolbox
    }

    fn config(&self) -> EncoderConfig {
        self.config
    }

    fn encode(&mut self, input: EncoderInput<'_>) -> Result<Option<EncodedPacket>, EncodeError> {
        let EncoderInput::MacPixelBuffer(pixel_buffer, timestamp_us) = input else {
            return Err(EncodeError::UnsupportedInput(
                "VideoToolbox requires a native macOS NV12 CVPixelBuffer",
            ));
        };
        let width = CVPixelBufferGetWidth(pixel_buffer);
        let height = CVPixelBufferGetHeight(pixel_buffer);
        if width != self.config.width() as usize || height != self.config.height() as usize {
            return Err(EncodeError::InvalidFrame(
                "CoreVideo buffer dimensions do not match the configured encoder".to_owned(),
            ));
        }
        let pixel_format = CVPixelBufferGetPixelFormatType(pixel_buffer);
        if pixel_format != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            && pixel_format != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        {
            return Err(EncodeError::UnsupportedInput(
                "VideoToolbox expects an 8-bit bi-planar NV12 pixel buffer",
            ));
        }
        if self
            .last_timestamp_us
            .is_some_and(|previous| timestamp_us <= previous)
        {
            return Err(EncodeError::InvalidFrame(
                "VideoToolbox timestamps must increase monotonically".to_owned(),
            ));
        }

        let value = i64::try_from(timestamp_us).map_err(|_| {
            EncodeError::InvalidFrame("presentation timestamp exceeds CMTime".to_owned())
        })?;
        let pts = CMTime {
            value,
            timescale: 1_000_000,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        let duration = CMTime {
            value: 1,
            timescale: FRAME_RATE as i32,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        let frame_properties = if self.keyframe_pending {
            // SAFETY: VideoToolbox exports this immutable CFString as the per-frame keyframe key.
            let force_keyframe_key = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            Some(CFDictionary::<CFString, CFType>::from_slices(
                &[force_keyframe_key],
                &[CFBoolean::new(true)],
            ))
        } else {
            None
        };
        let frame_properties = frame_properties.as_ref().map(|value| value.as_opaque());
        let mut flags = VTEncodeInfoFlags::empty();
        // SAFETY: The pixel buffer remains borrowed and valid for this call. Dimensions, format,
        // and strictly increasing timestamp were checked above. The frame token is never
        // dereferenced; the output callback recovers it as an integer.
        let status = unsafe {
            self.native_session()?.encode_frame(
                pixel_buffer,
                pts,
                duration,
                frame_properties,
                timestamp_us as usize as *mut c_void,
                &mut flags,
            )
        };
        if status != 0 {
            return Err(EncodeError::Backend(format!(
                "VideoToolbox encode failed with OSStatus {status}"
            )));
        }
        self.last_timestamp_us = Some(timestamp_us);
        self.keyframe_pending = false;

        // Complete through this frame so the shared synchronous Encoder contract can return the
        // packet from its dedicated encode worker.
        // SAFETY: pts is the timestamp just submitted to this retained session.
        let status = unsafe { self.native_session()?.complete_frames(pts) };
        if status != 0 {
            return Err(EncodeError::Backend(format!(
                "VideoToolbox frame completion failed with OSStatus {status}"
            )));
        }
        let mut state = self
            .callback_state
            .lock()
            .map_err(|_| EncodeError::Backend("VideoToolbox callback state poisoned".to_owned()))?;
        match state.output.pop_front() {
            Some(Ok(packet)) => Ok(Some(packet)),
            Some(Err(message)) => Err(EncodeError::Backend(message)),
            None => Ok(None),
        }
    }

    fn request_keyframe(&mut self) -> Result<(), EncodeError> {
        self.keyframe_pending = true;
        Ok(())
    }
}

impl Drop for VideoToolboxH264Encoder {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // SAFETY: Invalidating the uniquely owned session stops callbacks before state drops.
            unsafe { session.invalidate() };
        }
    }
}

fn create_session(
    config: EncoderConfig,
    callback_refcon: *mut c_void,
    request_low_latency: bool,
) -> Result<NativeCompressionSession, String> {
    let specification = CFMutableDictionary::<CFString, CFType>::with_capacity(2);
    // SAFETY: VideoToolbox exports these immutable CFStrings as encoder specification keys.
    let hardware_key =
        unsafe { kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder };
    specification.set(hardware_key, CFBoolean::new(true));
    if request_low_latency {
        // SAFETY: This is the immutable VideoToolbox low-latency specification key.
        let low_latency_key = unsafe { kVTVideoEncoderSpecification_EnableLowLatencyRateControl };
        specification.set(low_latency_key, CFBoolean::new(true));
    }
    let mut output = null_mut();
    // SAFETY: The dictionary contains the documented VideoToolbox CF types. The callback is
    // static, refcon points to a stable mutex owned until after invalidation, and output is valid.
    let status = unsafe {
        VTCompressionSession::create(
            None,
            config.width() as i32,
            config.height() as i32,
            kCMVideoCodecType_H264,
            Some(specification.as_opaque()),
            None,
            None,
            Some(compression_output_callback),
            callback_refcon,
            NonNull::from(&mut output),
        )
    };
    if status != 0 {
        return Err(format!("session creation returned OSStatus {status}"));
    }
    let output = NonNull::new(output)
        .ok_or_else(|| "VideoToolbox succeeded without returning a session".to_owned())?;
    // SAFETY: VTCompressionSessionCreate returns a non-null +1 retained reference.
    Ok(unsafe { CFRetained::from_raw(output) })
}

fn configure_session(
    session: &NativeCompressionSession,
    config: EncoderConfig,
) -> Result<(), EncodeError> {
    // SAFETY: These are immutable framework-owned VideoToolbox CFString property keys.
    let realtime_key = unsafe { kVTCompressionPropertyKey_RealTime };
    let no_reordering_key = unsafe { kVTCompressionPropertyKey_AllowFrameReordering };
    let average_bitrate_key = unsafe { kVTCompressionPropertyKey_AverageBitRate };
    let data_rate_limits_key = unsafe { kVTCompressionPropertyKey_DataRateLimits };
    let frame_rate_key = unsafe { kVTCompressionPropertyKey_ExpectedFrameRate };
    let keyframe_interval_key = unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval };
    let profile_key = unsafe { kVTCompressionPropertyKey_ProfileLevel };
    // SAFETY: This is the immutable framework-owned Main AutoLevel profile constant.
    let main_profile = unsafe { kVTProfileLevel_H264_Main_AutoLevel };

    set_bool(session, realtime_key, true)?;
    set_bool(session, no_reordering_key, false)?;
    set_number(
        session,
        average_bitrate_key,
        CFNumber::new_i32(config.bitrate_bps() as i32),
    )?;
    // Permit up to a 2x bitrate burst in a one-second window for IDR frames while retaining
    // the configured average bitrate target.
    let max_bytes_per_second = CFNumber::new_i32((config.bitrate_bps() / 4) as i32);
    let window_seconds = CFNumber::new_i32(1);
    let data_rate_limits =
        CFArray::<CFNumber>::from_objects(&[&max_bytes_per_second, &window_seconds]);
    set_property(session, data_rate_limits_key, &data_rate_limits)?;
    set_number(
        session,
        frame_rate_key,
        CFNumber::new_f32(FRAME_RATE as f32),
    )?;
    set_number(
        session,
        keyframe_interval_key,
        CFNumber::new_i32((FRAME_RATE * 60 * 60) as i32),
    )?;
    set_property(session, profile_key, main_profile)
}

fn set_bool(
    session: &NativeCompressionSession,
    key: &'static CFString,
    value: bool,
) -> Result<(), EncodeError> {
    set_property(session, key, CFBoolean::new(value))
}

fn set_number(
    session: &NativeCompressionSession,
    key: &'static CFString,
    value: CFRetained<CFNumber>,
) -> Result<(), EncodeError> {
    set_property(session, key, &value)
}

fn set_property(
    session: &NativeCompressionSession,
    key: &'static CFString,
    value: &impl AsRef<CFType>,
) -> Result<(), EncodeError> {
    let value = value.as_ref();
    // SAFETY: The session is live; private call sites pair each CF property key with its documented
    // boolean, number, or profile CF value.
    let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    if status == 0 {
        Ok(())
    } else {
        Err(EncodeError::Backend(format!(
            "setting VideoToolbox property failed with OSStatus {status}"
        )))
    }
}

fn read_hardware_acceleration(session: &NativeCompressionSession) -> Result<bool, EncodeError> {
    let mut raw_value: *mut c_void = null_mut();
    // SAFETY: raw_value is an out slot for the retained CFBoolean value of this property.
    // SAFETY: VideoToolbox exports this immutable CFString as the hardware-use property key.
    let hardware_use_key =
        unsafe { kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder };
    let status = unsafe {
        VTSessionCopyProperty(
            session,
            hardware_use_key,
            None,
            (&mut raw_value as *mut *mut c_void).cast(),
        )
    };
    if status != 0 {
        return Err(EncodeError::NoHardwareEncoder(format!(
            "VideoToolbox hardware-use property returned OSStatus {status}"
        )));
    }
    let raw_value = NonNull::new(raw_value).ok_or_else(|| {
        EncodeError::NoHardwareEncoder("hardware-use property is null".to_owned())
    })?;
    // SAFETY: This property is documented as a retained CFBoolean, and ownership transfers here.
    let value = unsafe { CFRetained::<CFBoolean>::from_raw(raw_value.cast()) };
    Ok(value.as_bool())
}

unsafe extern "C-unwind" fn compression_output_callback(
    refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: i32,
    _info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut objc2_core_media::CMSampleBuffer,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if refcon.is_null() {
            return;
        }
        // SAFETY: The refcon is the stable callback-state pointer installed at creation, and the
        // encoder invalidates the session before dropping the Box that owns it.
        let state = unsafe { &*refcon.cast::<Mutex<CallbackState>>() };
        let result = if status != 0 {
            Err(format!(
                "VideoToolbox output callback returned OSStatus {status}"
            ))
        } else if sample_buffer.is_null() {
            Err("VideoToolbox returned an empty compressed sample".to_owned())
        } else {
            // SAFETY: VideoToolbox lends a valid sample buffer for the callback duration.
            let sample = unsafe { &*sample_buffer };
            encode_sample(sample, source_frame_refcon as usize as u64)
        };
        if let Ok(mut state) = state.lock() {
            if state.output.len() == 2 {
                state.output.pop_front();
            }
            state.output.push_back(result);
        }
    }));
}

fn encode_sample(
    sample: &objc2_core_media::CMSampleBuffer,
    timestamp_us: u64,
) -> Result<EncodedPacket, String> {
    // SAFETY: The borrowed sample buffer is valid during the callback; the wrapper retains the
    // referenced block buffer before returning it.
    let data = unsafe { sample.data_buffer() }
        .ok_or_else(|| "VideoToolbox sample has no compressed data buffer".to_owned())?;
    // SAFETY: CoreMedia owns this live block buffer and reports the total bounded sample length.
    let length = unsafe { data.data_length() };
    if length == 0 || length > MAX_ENCODED_PACKET_BYTES {
        return Err("VideoToolbox produced an empty or oversized H.264 sample".to_owned());
    }
    let mut avcc = vec![0_u8; length];
    let destination = NonNull::new(avcc.as_mut_ptr().cast::<c_void>())
        .ok_or_else(|| "AVCC allocation returned a null pointer".to_owned())?;
    // SAFETY: The destination has exactly length bytes and CoreMedia copies no more than that
    // range from its block buffer.
    let status = unsafe { data.copy_data_bytes(0, length, destination) };
    if status != 0 {
        return Err(format!(
            "copying VideoToolbox sample returned OSStatus {status}"
        ));
    }

    // SAFETY: The wrapper retains the format description for its local lifetime.
    let description = unsafe { sample.format_description() }
        .ok_or_else(|| "VideoToolbox sample has no format description".to_owned())?;
    let sps = copy_parameter_set(&description, 0);
    let pps = copy_parameter_set(&description, 1);
    let (packet, _) =
        videotoolbox_avcc_to_annex_b(&avcc, 4, timestamp_us, sps.as_deref(), pps.as_deref())
            .map_err(|error| error.to_string())?;
    Ok(packet)
}

fn copy_parameter_set(
    description: &objc2_core_media::CMFormatDescription,
    index: usize,
) -> Option<Vec<u8>> {
    let mut pointer = std::ptr::null();
    let mut length = 0usize;
    let mut header_length = 0_i32;
    // SAFETY: description is a live borrowed format description and each output pointer is valid.
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            description,
            index,
            &mut pointer,
            &mut length,
            null_mut(),
            &mut header_length,
        )
    };
    if status != 0 || pointer.is_null() || length == 0 || length > 64 * 1024 {
        return None;
    }
    // SAFETY: CoreMedia returns a pointer borrowed from the retained description; length is
    // bounded and the bytes are copied before that description is released.
    Some(unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec())
}
