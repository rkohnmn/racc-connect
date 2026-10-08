use std::ffi::c_void;
use std::ptr::{null, null_mut, NonNull};
use std::sync::Mutex;

use objc2_core_foundation::{
    CFBoolean, CFMutableDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    kCMBlockBufferAssureMemoryNowFlag, CMBlockBuffer, CMFormatDescription, CMSampleBuffer,
    CMSampleTimingInfo, CMTime, CMTimeFlags, CMVideoFormatDescriptionCreateFromH264ParameterSets,
};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::{
    kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder, VTDecodeFrameFlags,
    VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
    VTSessionCopyProperty,
};

use crate::{
    validate_reset, DecodeError, DecodedFrame, Decoder, DecoderKind, EncodedAccessUnit,
    VideoToolboxBitstream, MAX_ENCODED_ACCESS_UNIT_BYTES, MAX_NV12_FRAME_BYTES,
};

type NativeDecompressionSession = CFRetained<VTDecompressionSession>;

struct RawFrame {
    width: u32,
    height: u32,
    y: Vec<u8>,
    uv: Vec<u8>,
}

struct DecodeCallbackState {
    output: Option<Result<RawFrame, String>>,
}

impl DecodeCallbackState {
    fn new() -> Self {
        Self { output: None }
    }
}

/// Hardware-required macOS H.264 decoder backed by VideoToolbox.
///
/// The callback copies bounded NV12 planes into the shared decoded-frame contract. A future
/// renderer can replace this CPU copy with an IOSurface/Metal texture handoff.
pub struct VideoToolboxDecoder {
    session: Option<NativeDecompressionSession>,
    callback_state: Box<Mutex<DecodeCallbackState>>,
    bitstream: VideoToolboxBitstream,
    active_sps: Option<Vec<u8>>,
    active_pps: Option<Vec<u8>>,
    epoch: Option<u16>,
    width: u32,
    height: u32,
}

impl VideoToolboxDecoder {
    /// Creates an unconfigured decoder. Configure it with a successful H.264 stream reset first.
    pub fn new() -> Self {
        Self {
            session: None,
            callback_state: Box::new(Mutex::new(DecodeCallbackState::new())),
            bitstream: VideoToolboxBitstream::new(),
            active_sps: None,
            active_pps: None,
            epoch: None,
            width: 0,
            height: 0,
        }
    }

    fn invalidate_session(&mut self) {
        if let Some(session) = self.session.take() {
            // SAFETY: The session is uniquely held by this decoder. Invalidation ends callbacks
            // before this decoder can replace or release its callback state.
            unsafe { session.invalidate() };
        }
    }

    fn create_session(&mut self, sps: &[u8], pps: &[u8]) -> Result<(), DecodeError> {
        let format = create_h264_format_description(sps, pps)?;
        let specification = CFMutableDictionary::<CFString, CFType>::with_capacity(1);
        // SAFETY: VideoToolbox exports this immutable CFString as the hardware requirement key.
        let hardware_key =
            unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder };
        specification.set(hardware_key, CFBoolean::new(true));
        let output_attributes = CFMutableDictionary::<CFString, CFType>::with_capacity(1);
        // SAFETY: CoreVideo exports this immutable CFString as the pixel-format dictionary key.
        let pixel_format_key = unsafe { kCVPixelBufferPixelFormatTypeKey };
        output_attributes.set(
            pixel_format_key,
            &CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32),
        );

        let callback_refcon = (&*self.callback_state as *const Mutex<DecodeCallbackState>)
            .cast_mut()
            .cast::<c_void>();
        let callback_record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(decompression_output_callback),
            decompressionOutputRefCon: callback_refcon,
        };
        let mut output = null_mut();
        // SAFETY: format, specification, and destination attributes are retained CF objects with
        // the framework-required types. callback_refcon remains valid until session invalidation.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                Some(specification.as_opaque()),
                Some(output_attributes.as_opaque()),
                &callback_record,
                NonNull::from(&mut output),
            )
        };
        if status != 0 {
            return Err(DecodeError::Backend(format!(
                "VideoToolbox hardware decoder creation returned OSStatus {status}"
            )));
        }
        let output = NonNull::new(output).ok_or_else(|| {
            DecodeError::Backend("VideoToolbox succeeded without returning a decoder".to_owned())
        })?;
        // SAFETY: VTDecompressionSessionCreate returns a non-null +1 retained session.
        let session = unsafe { CFRetained::from_raw(output) };
        if !read_hardware_acceleration(&session)? {
            // SAFETY: The just-created session is live and uniquely owned here.
            unsafe { session.invalidate() };
            return Err(DecodeError::Backend(
                "VideoToolbox did not report hardware decode".to_owned(),
            ));
        }
        self.invalidate_session();
        self.session = Some(session);
        self.active_sps = Some(sps.to_vec());
        self.active_pps = Some(pps.to_vec());
        Ok(())
    }

    fn native_session(&self) -> Result<&NativeDecompressionSession, DecodeError> {
        self.session
            .as_ref()
            .ok_or(DecodeError::MissingCodecConfiguration)
    }
}

impl Default for VideoToolboxDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for VideoToolboxDecoder {
    fn kind(&self) -> DecoderKind {
        DecoderKind::VideoToolbox
    }

    fn configure(&mut self, reset: racc_proto::StreamReset) -> Result<(), DecodeError> {
        validate_reset(reset)?;
        self.invalidate_session();
        self.bitstream = VideoToolboxBitstream::new();
        self.active_sps = None;
        self.active_pps = None;
        self.epoch = Some(reset.epoch);
        self.width = u32::from(reset.width);
        self.height = u32::from(reset.height);
        Ok(())
    }

    fn decode(
        &mut self,
        access_unit: &EncodedAccessUnit,
    ) -> Result<Option<DecodedFrame>, DecodeError> {
        let expected_epoch = self.epoch.ok_or(DecodeError::MissingCodecConfiguration)?;
        if expected_epoch != access_unit.epoch {
            return Err(DecodeError::WrongEpoch {
                expected: expected_epoch,
                received: access_unit.epoch,
            });
        }
        let sample = self.bitstream.sample(access_unit)?;
        if !access_unit.has_vcl() {
            return Ok(None);
        }
        let parameter_sets_changed = sample.sps != self.active_sps || sample.pps != self.active_pps;
        if (self.session.is_none() || parameter_sets_changed) && !access_unit.is_keyframe() {
            return Ok(None);
        }
        if self.session.is_none() || parameter_sets_changed {
            let sps = sample
                .sps
                .as_deref()
                .ok_or(DecodeError::MissingCodecConfiguration)?;
            let pps = sample
                .pps
                .as_deref()
                .ok_or(DecodeError::MissingCodecConfiguration)?;
            self.create_session(sps, pps)?;
        }
        if sample.avcc.len() > MAX_ENCODED_ACCESS_UNIT_BYTES {
            return Err(DecodeError::InvalidBitstream(
                "AVCC sample exceeds the configured bound",
            ));
        }

        let format = create_h264_format_description(
            self.active_sps
                .as_deref()
                .ok_or(DecodeError::MissingCodecConfiguration)?,
            self.active_pps
                .as_deref()
                .ok_or(DecodeError::MissingCodecConfiguration)?,
        )?;
        let sample_buffer = create_sample_buffer(&sample.avcc, &format, access_unit.frame_id)?;
        let mut state = self
            .callback_state
            .lock()
            .map_err(|_| DecodeError::Backend("VideoToolbox callback state poisoned".to_owned()))?;
        state.output = None;
        drop(state);

        let mut info_flags = VTDecodeInfoFlags::empty();
        // SAFETY: sample_buffer and session are retained for the duration of this call. With no
        // asynchronous decode flags set, VideoToolbox completes the callback before returning.
        let status = unsafe {
            self.native_session()?.decode_frame(
                &sample_buffer,
                VTDecodeFrameFlags::empty(),
                null_mut(),
                &mut info_flags,
            )
        };
        if status != 0 {
            return Err(DecodeError::Backend(format!(
                "VideoToolbox decode returned OSStatus {status}"
            )));
        }
        let output = self
            .callback_state
            .lock()
            .map_err(|_| DecodeError::Backend("VideoToolbox callback state poisoned".to_owned()))?
            .output
            .take();
        let Some(output) = output else {
            return Ok(None);
        };
        let frame = output.map_err(DecodeError::Backend)?;
        if frame.width != self.width || frame.height != self.height {
            return Err(DecodeError::InvalidFrame(
                "VideoToolbox output dimensions do not match the active stream reset",
            ));
        }
        Ok(Some(DecodedFrame::new(
            expected_epoch,
            access_unit.frame_id,
            access_unit.capture_ts_us,
            frame.width,
            frame.height,
            frame.y,
            frame.uv,
        )?))
    }

    fn flush(&mut self) -> Result<(), DecodeError> {
        self.invalidate_session();
        self.active_sps = None;
        self.active_pps = None;
        Ok(())
    }
}

impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        self.invalidate_session();
    }
}

fn create_h264_format_description(
    sps: &[u8],
    pps: &[u8],
) -> Result<CFRetained<CMFormatDescription>, DecodeError> {
    let mut pointers = [
        NonNull::new(sps.as_ptr().cast_mut()).ok_or(DecodeError::MissingCodecConfiguration)?,
        NonNull::new(pps.as_ptr().cast_mut()).ok_or(DecodeError::MissingCodecConfiguration)?,
    ];
    let mut sizes = [sps.len(), pps.len()];
    let mut output: *const CMFormatDescription = null();
    // SAFETY: parameter-set pointers refer to live slices for this call and sizes match those
    // slices. CoreMedia parses the bytes but does not mutate them.
    let status = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            None,
            2,
            NonNull::from(&mut pointers[0]),
            NonNull::from(&mut sizes[0]),
            4,
            NonNull::from(&mut output),
        )
    };
    if status != 0 {
        return Err(DecodeError::Backend(format!(
            "H.264 format description creation returned OSStatus {status}"
        )));
    }
    let output = NonNull::new(output.cast_mut()).ok_or_else(|| {
        DecodeError::Backend("CoreMedia returned a null H.264 format description".to_owned())
    })?;
    // SAFETY: The create function returns a +1 retained format description.
    Ok(unsafe { CFRetained::from_raw(output) })
}

fn create_sample_buffer(
    avcc: &[u8],
    format: &CMFormatDescription,
    frame_id: u32,
) -> Result<CFRetained<CMSampleBuffer>, DecodeError> {
    if avcc.is_empty() || avcc.len() > MAX_ENCODED_ACCESS_UNIT_BYTES {
        return Err(DecodeError::InvalidBitstream(
            "AVCC sample length is outside its bound",
        ));
    }
    let mut block_output = null_mut();
    // SAFETY: A null memory block requests framework allocation; the block length is bounded and
    // the output pointer is a valid out slot.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            null_mut(),
            avcc.len(),
            None,
            null(),
            0,
            avcc.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block_output),
        )
    };
    if status != 0 {
        return Err(DecodeError::Backend(format!(
            "CoreMedia block-buffer creation returned OSStatus {status}"
        )));
    }
    let block_output = NonNull::new(block_output)
        .ok_or_else(|| DecodeError::Backend("CoreMedia returned a null block buffer".to_owned()))?;
    // SAFETY: The create function returns a +1 retained block buffer.
    let block = unsafe { CFRetained::<CMBlockBuffer>::from_raw(block_output) };
    let source = NonNull::new(avcc.as_ptr().cast_mut().cast::<c_void>())
        .ok_or(DecodeError::InvalidBitstream("AVCC source buffer is null"))?;
    // SAFETY: source points to the full immutable AVCC slice; CoreMedia only copies avcc.len bytes.
    let status = unsafe { CMBlockBuffer::replace_data_bytes(source, &block, 0, avcc.len()) };
    if status != 0 {
        return Err(DecodeError::Backend(format!(
            "CoreMedia sample copy returned OSStatus {status}"
        )));
    }

    let pts = CMTime {
        value: i64::from(frame_id),
        timescale: 30,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    };
    let duration = CMTime {
        value: 1,
        timescale: 30,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    };
    let timing = CMSampleTimingInfo {
        duration,
        presentationTimeStamp: pts,
        decodeTimeStamp: CMTime {
            value: 0,
            timescale: 0,
            flags: CMTimeFlags::empty(),
            epoch: 0,
        },
    };
    let sample_size = avcc.len();
    let mut sample_output = null_mut();
    // SAFETY: Each array pointer references one live timing/size entry, and the block and format
    // are retained until CoreMedia has created the ready sample buffer.
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(format),
            1,
            1,
            &timing,
            1,
            &sample_size,
            NonNull::from(&mut sample_output),
        )
    };
    if status != 0 {
        return Err(DecodeError::Backend(format!(
            "CoreMedia sample-buffer creation returned OSStatus {status}"
        )));
    }
    let sample_output = NonNull::new(sample_output).ok_or_else(|| {
        DecodeError::Backend("CoreMedia returned a null sample buffer".to_owned())
    })?;
    // SAFETY: The create function returns a +1 retained sample buffer.
    Ok(unsafe { CFRetained::from_raw(sample_output) })
}

fn read_hardware_acceleration(session: &NativeDecompressionSession) -> Result<bool, DecodeError> {
    let mut raw_value: *mut c_void = null_mut();
    // SAFETY: raw_value is a valid out slot for the retained CFBoolean property value.
    let status = unsafe {
        VTSessionCopyProperty(
            session,
            kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
            None,
            (&mut raw_value as *mut *mut c_void).cast(),
        )
    };
    if status != 0 {
        return Err(DecodeError::Backend(format!(
            "VideoToolbox hardware-use property returned OSStatus {status}"
        )));
    }
    let raw_value = NonNull::new(raw_value).ok_or_else(|| {
        DecodeError::Backend("VideoToolbox hardware-use property is null".to_owned())
    })?;
    // SAFETY: This documented property returns a retained CFBoolean.
    let value = unsafe { CFRetained::<CFBoolean>::from_raw(raw_value.cast()) };
    Ok(value.as_bool())
}

unsafe extern "C-unwind" fn decompression_output_callback(
    refcon: *mut c_void,
    _source_frame_refcon: *mut c_void,
    status: i32,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if refcon.is_null() {
            return;
        }
        // SAFETY: refcon points to the Box-owned callback state, which remains alive until after
        // the decompression session is invalidated.
        let state = unsafe { &*refcon.cast::<Mutex<DecodeCallbackState>>() };
        let output = if status != 0 {
            Err(format!(
                "VideoToolbox output callback returned OSStatus {status}"
            ))
        } else if image_buffer.is_null() {
            Err("VideoToolbox returned no decoded image buffer".to_owned())
        } else {
            // SAFETY: The callback provides a borrowed image buffer that is a CVPixelBuffer because
            // the decoder's destination attributes request the NV12 pixel format.
            let pixel_buffer: &CVPixelBuffer = unsafe { &*image_buffer };
            read_nv12(pixel_buffer)
        };
        if let Ok(mut state) = state.lock() {
            state.output = Some(output);
        }
    }));
}

fn read_nv12(pixel_buffer: &CVPixelBuffer) -> Result<RawFrame, String> {
    let width = CVPixelBufferGetWidth(pixel_buffer);
    let height = CVPixelBufferGetHeight(pixel_buffer);
    if width < 16
        || height < 16
        || width > 1920
        || height > 1080
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        return Err("VideoToolbox returned unsupported NV12 dimensions".to_owned());
    }
    if CVPixelBufferGetPixelFormatType(pixel_buffer)
        != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    {
        return Err("VideoToolbox returned a non-video-range NV12 pixel buffer".to_owned());
    }
    if CVPixelBufferGetPlaneCount(pixel_buffer) != 2 {
        return Err("VideoToolbox NV12 frame has the wrong plane count".to_owned());
    }
    let frame_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_add(pixels / 2))
        .ok_or_else(|| "VideoToolbox frame size overflow".to_owned())?;
    if frame_bytes > MAX_NV12_FRAME_BYTES {
        return Err("VideoToolbox frame exceeds the NV12 allocation bound".to_owned());
    }
    // SAFETY: pixel_buffer is a live VideoToolbox callback buffer and the matching unlock guard
    // is created immediately after a successful lock.
    let lock_status =
        unsafe { CVPixelBufferLockBaseAddress(pixel_buffer, CVPixelBufferLockFlags::ReadOnly) };
    if lock_status != 0 {
        return Err(format!(
            "locking VideoToolbox pixel buffer returned status {lock_status}"
        ));
    }
    let _unlock = PixelBufferUnlock(pixel_buffer);
    let y = copy_plane(pixel_buffer, 0, width, height)?;
    let uv = copy_plane(pixel_buffer, 1, width, height / 2)?;
    Ok(RawFrame {
        width: width as u32,
        height: height as u32,
        y,
        uv,
    })
}

struct PixelBufferUnlock<'a>(&'a CVPixelBuffer);

impl Drop for PixelBufferUnlock<'_> {
    fn drop(&mut self) {
        // SAFETY: This guard is only constructed after a successful lock using the same flags.
        let _ = unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

fn copy_plane(
    pixel_buffer: &CVPixelBuffer,
    plane: usize,
    row_bytes: usize,
    row_count: usize,
) -> Result<Vec<u8>, String> {
    let actual_row_bytes = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, plane);
    let plane_height = CVPixelBufferGetHeightOfPlane(pixel_buffer, plane);
    if actual_row_bytes < row_bytes || plane_height < row_count {
        return Err("VideoToolbox NV12 plane geometry is inconsistent".to_owned());
    }
    let base = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, plane).cast::<u8>();
    if base.is_null() {
        return Err("VideoToolbox NV12 plane has a null base address".to_owned());
    }
    let mut output = vec![0_u8; row_bytes * row_count];
    for row in 0..row_count {
        // SAFETY: the pixel buffer remains locked, base is non-null, and each row lies within the
        // checked plane stride and height. The output row is exactly row_bytes long.
        let source =
            unsafe { std::slice::from_raw_parts(base.add(row * actual_row_bytes), row_bytes) };
        let start = row * row_bytes;
        output[start..start + row_bytes].copy_from_slice(source);
    }
    Ok(output)
}
