//! Windows Media Foundation H.264 hardware encoder using a D3D11 NV12 input surface.
//!
//! Unsafe operations are isolated in this platform binding. The probe uploads a generated test
//! pattern to a D3D11 texture; it never enumerates monitors or captures desktop pixels.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::ptr::null_mut;
use std::slice;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{implement, IUnknown, Interface, VARIANT};
use windows::Win32::Foundation::{BOOL, HMODULE, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_BIND_VIDEO_ENCODER, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::{
    eAVEncH264VProfile_Main, CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonRealTime,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncVideoForceKeyFrame,
    CODECAPI_AVLowLatencyMode, ICodecAPI, IMFActivate, IMFAsyncCallback, IMFAsyncCallback_Impl,
    IMFAsyncResult, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFSample, IMFShutdown,
    IMFTransform, MEError, METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Video, MFStartup, MFTEnumEx, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFASYNC_CALLBACK_QUEUE_STANDARD,
    MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_DATA_BUFFER_INCOMPLETE, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE,
    MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

use racc_capture::windows::WindowsGpuTexture;
use racc_capture::GpuFrame;

use crate::probe::{validate_frame_count, AnnexBFile, ProbeReport};
use crate::windows_video_processor::{same_device, Nv12Surface, WindowsNv12Converter};
use crate::{
    annex_b_contains_idr, normalize_annex_b, D3D11Nv12Frame, EncodeError, EncodedPacket, Encoder,
    EncoderConfig, EncoderInput, EncoderKind, MAX_ENCODED_PACKET_BYTES,
};

static MF_STARTED: OnceLock<Result<(), String>> = OnceLock::new();
const MAX_MFT_CANDIDATES: u32 = 64;
const MAX_OUTPUTS_PER_INPUT: usize = 8;
const DEFAULT_OUTPUT_BUFFER_BYTES: u32 = 2 * 1024 * 1024;
const RATE_CONTROL_WINDOW_MS: u64 = 250;
const ASYNC_EVENT_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_ASYNC_EVENTS_PER_WAIT: usize = 256;

/// Codec API settings accepted by the active Media Foundation MFT.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppliedCodecSettings {
    /// Low-latency mode was available and enabled.
    pub low_latency: bool,
    /// Real-time mode was available and enabled.
    pub real_time: bool,
    /// B-picture count was explicitly set to zero.
    pub b_frames_disabled: bool,
    /// The codec accepted a rate-control buffer sized for 250 ms (bytes).
    pub small_rate_control_buffer: bool,
}

/// Active Media Foundation hardware H.264 encoder.
pub struct MediaFoundationH264Encoder {
    transform: IMFTransform,
    _activation: IMFActivate,
    _device_manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    config: EncoderConfig,
    output_stream_flags: u32,
    output_buffer_size: u32,
    applied: AppliedCodecSettings,
    async_events: Option<AsyncMftEvents>,
    encoded_outputs: u64,
    pending_surfaces: VecDeque<Nv12Surface>,
    _com_apartment: ComApartment,
}

impl MediaFoundationH264Encoder {
    /// Enumerates hardware MFTs and configures the first usable one on the supplied D3D11 device.
    pub fn new(device: &ID3D11Device, config: EncoderConfig) -> Result<Self, EncodeError> {
        startup_media_foundation()?;
        let com_apartment = ComApartment::initialize()?;
        let (device_manager, reset_token) = create_device_manager(device)?;
        let activations = enumerate_hardware_encoders()?;
        let mut failures = Vec::new();
        for activation in activations.usable_clones()? {
            match activate_and_configure(&activation, &device_manager, reset_token, config) {
                Ok((transform, flags, buffer_size, applied, async_events)) => {
                    return Ok(Self {
                        transform,
                        _activation: activation,
                        _device_manager: device_manager,
                        device: device.clone(),
                        config,
                        output_stream_flags: flags,
                        output_buffer_size: buffer_size,
                        applied,
                        async_events,
                        encoded_outputs: 0,
                        pending_surfaces: VecDeque::with_capacity(
                            crate::pipeline::NV12_SURFACE_POOL_CAPACITY,
                        ),
                        _com_apartment: com_apartment,
                    });
                }
                Err(error) => failures.push(error),
            }
        }
        Err(EncodeError::NoHardwareEncoder(if failures.is_empty() {
            "no hardware MFT supports NV12 input and H.264 output".to_owned()
        } else {
            failures.join("; ")
        }))
    }

    /// Creates an MFT bound to the device that owns the capture frame.
    pub fn new_for_capture(frame: &GpuFrame, config: EncoderConfig) -> Result<Self, EncodeError> {
        let wrapper = frame
            .texture()
            .as_any()
            .downcast_ref::<WindowsGpuTexture>()
            .ok_or_else(|| {
                EncodeError::InvalidFrame("capture frame is not a Windows D3D11 texture".to_owned())
            })?;
        // SAFETY: The texture wrapper retains the live capture texture and GetDevice returns its owner.
        let device = unsafe { wrapper.texture().GetDevice() }
            .map_err(|error| EncodeError::Backend(format!("get capture D3D11 device: {error}")))?;
        Self::new(&device, config)
    }

    /// Converts a capture frame on its owning device, then submits the pooled NV12 surface to MF.
    pub fn encode_capture_frame(
        &mut self,
        converter: &mut WindowsNv12Converter,
        frame: &GpuFrame,
    ) -> Result<Option<EncodedPacket>, EncodeError> {
        let surface = converter.convert(frame)?;
        self.encode_surface(surface)
    }

    /// Submits an owned pooled surface and retains it until its corresponding output is emitted.
    pub fn encode_surface(
        &mut self,
        surface: Nv12Surface,
    ) -> Result<Option<EncodedPacket>, EncodeError> {
        if !same_device(&self.device, surface.device())? {
            return Err(EncodeError::CrossDevice);
        }
        let timestamp_us = surface.timestamp_us();
        let packet = self.encode_texture(surface.as_encoder_frame())?;
        self.pending_surfaces.push_back(surface);
        if let Some(packet) = packet.as_ref() {
            self.release_surface_leases_through(packet.timestamp_us);
        }
        let _ = timestamp_us;
        Ok(packet)
    }

    fn release_surface_leases_through(&mut self, timestamp_us: u64) {
        while self
            .pending_surfaces
            .front()
            .is_some_and(|surface| surface.timestamp_us() <= timestamp_us)
        {
            self.pending_surfaces.pop_front();
        }
    }

    /// Requests an IDR on the next input frame through the standard codec API property.
    pub fn request_keyframe(&self) -> Result<(), EncodeError> {
        let codec: ICodecAPI = self
            .transform
            .cast()
            .map_err(|error| EncodeError::Backend(format!("MFT lacks ICodecAPI: {error}")))?;
        set_codec_u32(&codec, &CODECAPI_AVEncVideoForceKeyFrame, 1, true).map(|_| ())
    }
    /// Codec properties that the active MFT accepted.
    pub const fn applied_settings(&self) -> AppliedCodecSettings {
        self.applied
    }

    /// Encodes one same-device D3D11 NV12 surface.
    pub fn encode_texture(
        &mut self,
        input: D3D11Nv12Frame<'_>,
    ) -> Result<Option<EncodedPacket>, EncodeError> {
        validate_texture(input, self.config)?;
        // SAFETY: The texture is live and GetDevice returns its actual D3D11 owner.
        let source_device = unsafe { input.texture.GetDevice() }
            .map_err(|error| EncodeError::Backend(format!("get NV12 input device: {error}")))?;
        if !same_device(&self.device, &source_device)? {
            return Err(EncodeError::CrossDevice);
        }
        if let Some(events) = &mut self.async_events {
            events.wait_for_input()?;
        }
        let sample = create_input_sample(&input)?;
        // SAFETY: The configured input stream accepts NV12, and the sample keeps its same-device
        // D3D11 surface alive while ProcessInput retains the sample.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }
            .map_err(|error| EncodeError::Backend(format!("MF ProcessInput: {error}")))?;
        self.drain_output(input.timestamp_us, false)
    }

    /// Drains delayed output after the final input.
    pub fn finish_stream(&mut self) -> Result<Option<EncodedPacket>, EncodeError> {
        // SAFETY: The transform is live and this requests the standard end-of-input drain.
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(|error| EncodeError::Backend(format!("MF drain: {error}")))?;
        let output = self.drain_output(0, true)?;
        self.pending_surfaces.clear();
        Ok(output)
    }

    fn drain_output(
        &mut self,
        fallback_timestamp_us: u64,
        draining: bool,
    ) -> Result<Option<EncodedPacket>, EncodeError> {
        let mut combined = Vec::with_capacity(64 * 1024);
        let mut timestamp_us = fallback_timestamp_us;

        let mut output_count = 0usize;
        let mut incomplete = true;
        while incomplete && output_count < MAX_OUTPUTS_PER_INPUT {
            if output_count == 0 {
                if let Some(events) = &mut self.async_events {
                    if !events.wait_for_output(draining)? {
                        return Ok(None);
                    }
                }
            }
            let (sample, flags) = self.process_output_sample()?;
            let Some(sample) = sample else { break };
            if output_count == 0 {
                timestamp_us = sample_timestamp_us(&sample).unwrap_or(fallback_timestamp_us);
            }

            let bytes = read_sample_bytes(&sample)?;
            if combined
                .len()
                .checked_add(bytes.len())
                .is_none_or(|size| size > MAX_ENCODED_PACKET_BYTES)
            {
                return Err(EncodeError::InvalidBitstream(
                    "MF output exceeds access-unit bound".to_owned(),
                ));
            }
            combined.extend_from_slice(&bytes);
            output_count += 1;
            incomplete = flags & (MFT_OUTPUT_DATA_BUFFER_INCOMPLETE.0 as u32) != 0;
        }
        if incomplete {
            return Err(EncodeError::InvalidBitstream(
                "MF produced too many output buffers".to_owned(),
            ));
        }
        if combined.is_empty() {
            return Ok(None);
        }
        self.encoded_outputs = self.encoded_outputs.saturating_add(1);
        let bytes = normalize_annex_b(&combined)?;
        let keyframe = annex_b_contains_idr(&bytes);
        Ok(Some(EncodedPacket {
            bytes,
            timestamp_us,
            keyframe,
        }))
    }

    fn process_output_sample(&self) -> Result<(Option<IMFSample>, u32), EncodeError> {
        let caller_sample = if self.output_stream_flags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
            == 0
        {
            let size = self
                .output_buffer_size
                .clamp(DEFAULT_OUTPUT_BUFFER_BYTES, MAX_ENCODED_PACKET_BYTES as u32);
            let sample = unsafe { MFCreateSample() }.map_err(|error| {
                EncodeError::Backend(format!("create MF output sample: {error}"))
            })?;
            let buffer = unsafe { MFCreateMemoryBuffer(size) }.map_err(|error| {
                EncodeError::Backend(format!("create MF output buffer: {error}"))
            })?;
            // SAFETY: The Media Foundation buffer remains owned by the sample.
            unsafe { sample.AddBuffer(&buffer) }.map_err(|error| {
                EncodeError::Backend(format!("attach MF output buffer: {error}"))
            })?;
            Some(sample)
        } else {
            None
        };
        let mut output = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(caller_sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };
        let mut status = 0u32;
        // SAFETY: The output slice and status are writable locals; returned COM fields are taken below.
        let result = unsafe {
            self.transform
                .ProcessOutput(0, slice::from_mut(&mut output), &mut status)
        };
        // SAFETY: ProcessOutput initialized both ManuallyDrop fields; taking transfers ownership once.
        let sample = unsafe { ManuallyDrop::take(&mut output.pSample) };
        // SAFETY: Any returned event collection is a COM option and must be released after the call.
        let events = unsafe { ManuallyDrop::take(&mut output.pEvents) };
        drop(events);
        match result {
            Ok(()) => Ok((sample, output.dwStatus)),
            Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok((None, 0)),
            Err(error) => Err(EncodeError::Backend(format!("MF ProcessOutput: {error}"))),
        }
    }
}

impl Encoder for MediaFoundationH264Encoder {
    fn kind(&self) -> EncoderKind {
        EncoderKind::WindowsMediaFoundationHardware
    }
    fn config(&self) -> EncoderConfig {
        self.config
    }
    fn encode(&mut self, input: EncoderInput<'_>) -> Result<Option<EncodedPacket>, EncodeError> {
        match input {
            EncoderInput::D3D11Nv12(frame) => self.encode_texture(frame),
            EncoderInput::PooledNv12(surface) => self.encode_surface(surface),
            EncoderInput::I420(_) => Err(EncodeError::UnsupportedInput(
                "Media Foundation hardware requires a same-device D3D11 NV12 texture",
            )),
        }
    }

    fn finish(&mut self) -> Result<Option<EncodedPacket>, EncodeError> {
        self.finish_stream()
    }

    fn request_keyframe(&mut self) -> Result<(), EncodeError> {
        MediaFoundationH264Encoder::request_keyframe(self)
    }
}

/// Synthetic NV12 test-pattern probe to an Annex B .h264 file; never captures the desktop.
pub fn run_mf_synthetic_probe(
    path: impl AsRef<std::path::Path>,
    config: EncoderConfig,
    frame_count: u32,
) -> Result<ProbeReport, EncodeError> {
    validate_frame_count(frame_count)?;
    let (device, context) = create_hardware_device()?;
    let hardware = MediaFoundationH264Encoder::new(&device, config)
        .map(|encoder| Box::new(encoder) as Box<dyn Encoder>);
    let mut selection = crate::pipeline::select_encoder(
        config,
        crate::pipeline::InputPixelFormat::Bgra8,
        hardware,
    )?;
    let mut software_readback = if selection.backend == EncoderKind::OpenH264 {
        Some(crate::windows_software_fallback::WindowsI420Readback::new(
            &device, config,
        )?)
    } else {
        None
    };
    let (texture, mut pattern) = create_nv12_pattern_texture(&device, config)?;
    let mut output = AnnexBFile::create(path.as_ref())?;
    selection.encoder.request_keyframe()?;
    let mut frames_encoded = 0u32;
    for frame_index in 0..frame_count {
        fill_nv12_pattern(
            config.width() as usize,
            config.height() as usize,
            frame_index,
            &mut pattern,
        );
        let row_pitch = config.width();
        let depth_pitch = u32::try_from(pattern.len())
            .map_err(|_| EncodeError::InvalidFrame("NV12 pattern too large".to_owned()))?;
        // SAFETY: The same-device default-usage NV12 destination is GPU resident. The initialized
        // test pattern has exact NV12 size and pitch; this only uploads synthetic bytes.
        unsafe {
            context.UpdateSubresource(
                &texture,
                0,
                None,
                pattern.as_ptr().cast(),
                row_pitch,
                depth_pitch,
            )
        };
        let input = D3D11Nv12Frame {
            width: config.width(),
            height: config.height(),
            timestamp_us: u64::from(frame_index) * 1_000_000 / u64::from(config.fps()),
            texture: &texture,
        };
        let packet = if selection.backend == EncoderKind::WindowsMediaFoundationHardware {
            match selection.encoder.encode(EncoderInput::D3D11Nv12(input)) {
                Ok(packet) => packet,
                Err(error) => {
                    selection = crate::pipeline::select_encoder(
                        config,
                        crate::pipeline::InputPixelFormat::Bgra8,
                        Err(error),
                    )?;
                    selection.encoder.request_keyframe()?;
                    if software_readback.is_none() {
                        software_readback =
                            Some(crate::windows_software_fallback::WindowsI420Readback::new(
                                &device, config,
                            )?);
                    }
                    let readback = software_readback.as_mut().ok_or_else(|| {
                        EncodeError::Backend(
                            "OpenH264 synthetic readback did not initialize".to_owned(),
                        )
                    })?;
                    readback.with_i420(input, |i420| {
                        selection.encoder.encode(EncoderInput::I420(i420))
                    })??
                }
            }
        } else {
            let readback = software_readback.as_mut().ok_or_else(|| {
                EncodeError::Backend("OpenH264 synthetic readback did not initialize".to_owned())
            })?;
            readback.with_i420(input, |i420| {
                selection.encoder.encode(EncoderInput::I420(i420))
            })??
        };
        if let Some(packet) = packet {
            output.write_packet(&packet)?;
            frames_encoded = frames_encoded.saturating_add(1);
        }
    }
    if let Some(packet) = selection.encoder.finish()? {
        output.write_packet(&packet)?;
        frames_encoded = frames_encoded.saturating_add(1);
    }
    let bytes_written = output.finish()?;
    if frames_encoded == 0 || bytes_written == 0 {
        return Err(EncodeError::InvalidBitstream(
            "synthetic MF/OpenH264 probe produced no access units".to_owned(),
        ));
    }
    Ok(ProbeReport {
        path: path.as_ref().to_path_buf(),
        backend: selection.backend,
        used_fallback: selection.used_fallback,
        fallback_reason: selection.fallback_reason.clone(),
        width: config.width(),
        height: config.height(),
        fps: config.fps(),
        frames_requested: frame_count,
        frames_encoded,
        bytes_written,
    })
}

struct ComApartment {
    initialized_by_us: bool,
}

impl ComApartment {
    fn initialize() -> Result<Self, EncodeError> {
        // SAFETY: CoInitializeEx accepts a null reserved pointer and does not retain it.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            Ok(Self {
                initialized_by_us: true,
            })
        } else if result == RPC_E_CHANGED_MODE {
            Ok(Self {
                initialized_by_us: false,
            })
        } else {
            Err(EncodeError::Backend(format!(
                "initialize COM MTA: {result:?}"
            )))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized_by_us {
            // SAFETY: This balances the successful CoInitializeEx call on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn startup_media_foundation() -> Result<(), EncodeError> {
    let started = MF_STARTED.get_or_init(|| {
        // SAFETY: MFStartup is process-wide and called once using the SDK version.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|error| error.to_string())
    });
    started
        .as_ref()
        .map(|_| ())
        .map_err(|error| EncodeError::Backend(format!("start Media Foundation: {error}")))
}

struct ActivationList {
    entries: *mut Option<IMFActivate>,
    count: u32,
}

impl ActivationList {
    fn usable_clones(&self) -> Result<Vec<IMFActivate>, EncodeError> {
        if self.count == 0 || self.entries.is_null() {
            return Err(EncodeError::NoHardwareEncoder(
                "Media Foundation returned an empty hardware MFT list".to_owned(),
            ));
        }
        if self.count > MAX_MFT_CANDIDATES {
            return Err(EncodeError::NoHardwareEncoder(format!(
                "Media Foundation returned {} candidates; maximum is {}",
                self.count, MAX_MFT_CANDIDATES
            )));
        }
        // SAFETY: MFTEnumEx returned this many initialized activation options in owned CoTaskMem.
        let entries = unsafe { slice::from_raw_parts(self.entries, self.count as usize) };
        Ok(entries.iter().filter_map(Clone::clone).collect())
    }
}

impl Drop for ActivationList {
    fn drop(&mut self) {
        if self.entries.is_null() {
            return;
        }
        // SAFETY: The pointer and count came directly from MFTEnumEx. Drop each COM option and
        // free the returned CoTaskMem array exactly once.
        unsafe {
            for index in 0..self.count as usize {
                std::ptr::drop_in_place(self.entries.add(index));
            }
            CoTaskMemFree(Some(self.entries.cast()));
        }
    }
}

fn enumerate_hardware_encoders() -> Result<ActivationList, EncodeError> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let mut entries = null_mut();
    let mut count = 0u32;
    // SAFETY: Descriptors and output slots are valid locals; ActivationList owns the returned array.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&input),
            Some(&output),
            &mut entries,
            &mut count,
        )
    }
    .map_err(|error| EncodeError::NoHardwareEncoder(format!("enumerate hardware MFTs: {error}")))?;
    Ok(ActivationList { entries, count })
}

fn create_device_manager(
    device: &ID3D11Device,
) -> Result<(IMFDXGIDeviceManager, u32), EncodeError> {
    let mut reset_token = 0u32;
    let mut manager = None;
    // SAFETY: Output pointers are writable locals; the returned manager is a COM RAII object.
    unsafe { MFCreateDXGIDeviceManager(&mut reset_token, &mut manager) }
        .map_err(|error| EncodeError::Backend(format!("create DXGI device manager: {error}")))?;
    let manager = manager
        .ok_or_else(|| EncodeError::Backend("MF returned no DXGI device manager".to_owned()))?;
    // SAFETY: Device and manager are live interfaces, and the token came from that manager.
    unsafe { manager.ResetDevice(device, reset_token) }
        .map_err(|error| EncodeError::Backend(format!("bind D3D11 device manager: {error}")))?;
    Ok((manager, reset_token))
}

fn activate_and_configure(
    activation: &IMFActivate,
    device_manager: &IMFDXGIDeviceManager,
    _reset_token: u32,
    config: EncoderConfig,
) -> Result<
    (
        IMFTransform,
        u32,
        u32,
        AppliedCodecSettings,
        Option<AsyncMftEvents>,
    ),
    String,
> {
    // SAFETY: Activation is a live IMFActivate from MFTEnumEx; windows-rs wraps the transform.
    let transform: IMFTransform = unsafe { activation.ActivateObject() }
        .map_err(|error| format!("activate transform: {error}"))?;
    // SAFETY: GetAttributes returns an owned attribute store for this live transform.
    let attributes = unsafe { transform.GetAttributes() }
        .map_err(|error| format!("get transform attributes: {error}"))?;
    // SAFETY: MF_TRANSFORM_ASYNC is a standard scalar attribute on the live store.
    let is_async = unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or_default() != 0;
    if is_async {
        // Async MFTs remain locked until the host opts in to the event-driven processing model.
        // SAFETY: Set the documented unlock flag before any IMFTransform processing calls.
        unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
            .map_err(|error| format!("unlock asynchronous MFT: {error}"))?;
    }
    // SAFETY: ProcessMessage stores the manager pointer on this live transform; manager stays owned.
    unsafe {
        transform
            .ProcessMessage(
                MFT_MESSAGE_SET_D3D_MANAGER,
                Interface::as_raw(device_manager) as usize,
            )
            .map_err(|error| format!("bind D3D manager to MFT: {error}"))?;
    }
    // Configure ICodecAPI properties before output-type negotiation, as encoder MFTs may lock
    // these controls once media types are committed. Each property setter below reads the value
    // back; a successful HRESULT alone does not prove that an MFT applied the requested value.
    let applied = configure_codec_properties(&transform, config)
        .map_err(|error| format!("configure low-latency properties: {error}"))?;
    let output_type = create_media_type(config, &MFVideoFormat_H264, true)
        .map_err(|error| format!("create H.264 output type: {error}"))?;
    // SAFETY: Set the live H.264 output type first; encoder MFTs derive the accepted raw inputs
    // from the chosen compressed output type.
    unsafe { transform.SetOutputType(0, &output_type, 0) }
        .map_err(|error| format!("set H.264 output type: {error}"))?;
    let input_type = create_media_type(config, &MFVideoFormat_NV12, false)
        .map_err(|error| format!("create NV12 input type: {error}"))?;
    // SAFETY: The live configured NV12 type is passed to input stream zero after output negotiation.
    unsafe { transform.SetInputType(0, &input_type, 0) }
        .map_err(|error| format!("set NV12 input type: {error}"))?;
    // SAFETY: The transform has configured input/output types and the D3D11 device manager.
    unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0) }
        .map_err(|error| format!("begin MFT streaming: {error}"))?;
    // SAFETY: Notify the configured MFT that the first sample is about to be processed. This is
    // optional for synchronous transforms and required for asynchronous transforms.
    unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0) }
        .map_err(|error| format!("start MFT stream: {error}"))?;
    // SAFETY: The configured transform fills a local output description.
    let output_info = unsafe { transform.GetOutputStreamInfo(0) }
        .map_err(|error| format!("get output stream info: {error}"))?;
    let buffer_size = output_info
        .cbSize
        .clamp(DEFAULT_OUTPUT_BUFFER_BYTES, MAX_ENCODED_PACKET_BYTES as u32);
    let async_events = if is_async {
        // SAFETY: Async transforms expose this interface for their input/output credit events.
        let generator: IMFMediaEventGenerator = transform
            .cast()
            .map_err(|error| format!("async MFT lacks IMFMediaEventGenerator: {error}"))?;
        Some(
            AsyncMftEvents::new(generator)
                .map_err(|error| format!("start async event callback: {error}"))?,
        )
    } else {
        None
    };
    Ok((
        transform,
        output_info.dwFlags,
        buffer_size,
        applied,
        async_events,
    ))
}

/// Notification delivered by a single outstanding IMFMediaEventGenerator callback.
#[derive(Clone, Copy, Debug)]
struct MftAsyncNotification {
    event_type: u32,
    status: windows::core::HRESULT,
}

type MftAsyncEventResult = Result<MftAsyncNotification, String>;

/// COM callback for IMFMediaEventGenerator::BeginGetEvent.
#[implement(IMFAsyncCallback)]
struct MftEventCallback {
    sender: SyncSender<MftAsyncEventResult>,
}

impl IMFAsyncCallback_Impl for MftEventCallback_Impl {
    fn GetParameters(&self, flags: *mut u32, queue: *mut u32) -> windows::core::Result<()> {
        if flags.is_null() || queue.is_null() {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_POINTER,
                "null IMFAsyncCallback::GetParameters output",
            ));
        }
        // SAFETY: COM provides valid writable out-pointers; null pointers were rejected above.
        unsafe {
            *flags = 0;
            *queue = MFASYNC_CALLBACK_QUEUE_STANDARD;
        }
        Ok(())
    }

    fn Invoke(&self, async_result: Option<&IMFAsyncResult>) -> windows::core::Result<()> {
        let event_result = (|| {
            let async_result = async_result.ok_or_else(|| "missing async result".to_owned())?;
            // The generator is passed as callback state, avoiding a generator<->callback cycle.
            // SAFETY: The callback result was supplied by Media Foundation for our registration.
            let state = unsafe { async_result.GetState() }
                .map_err(|error| format!("get async event state: {error}"))?;
            let generator: IMFMediaEventGenerator = state
                .cast()
                .map_err(|error| format!("cast async event generator state: {error}"))?;
            // SAFETY: EndGetEvent pairs with the BeginGetEvent that triggered this callback.
            let event = unsafe { generator.EndGetEvent(async_result) }
                .map_err(|error| format!("complete async event retrieval: {error}"))?;
            // SAFETY: The live event returns scalar type and status values.
            let event_type = unsafe { event.GetType() }
                .map_err(|error| format!("read async event type: {error}"))?;
            // SAFETY: GetStatus reads the status associated with this event.
            let status = unsafe { event.GetStatus() }
                .map_err(|error| format!("read async event status: {error}"))?;
            Ok(MftAsyncNotification { event_type, status })
        })();
        let _ = self.sender.send(event_result);
        Ok(())
    }
}

/// Bounded, single-consumer event pump for asynchronous transform input/output notifications.
///
/// Exactly one BeginGetEvent operation is outstanding. Its callback writes to a capacity-one
/// channel; the encoder rearms only after receiving the previous event, so neither side can build
/// an unbounded queue.
struct AsyncMftEvents {
    generator: IMFMediaEventGenerator,
    callback: IMFAsyncCallback,
    state: IUnknown,
    receiver: Receiver<MftAsyncEventResult>,
    need_input: bool,
    have_output: bool,
}

impl AsyncMftEvents {
    fn new(generator: IMFMediaEventGenerator) -> Result<Self, windows::core::Error> {
        let state: IUnknown = generator.cast()?;
        let (sender, receiver) = sync_channel(1);
        let callback: IMFAsyncCallback = MftEventCallback { sender }.into();
        let events = Self {
            generator,
            callback,
            state,
            receiver,
            need_input: false,
            have_output: false,
        };
        events.arm_event()?;
        Ok(events)
    }

    fn arm_event(&self) -> Result<(), windows::core::Error> {
        // SAFETY: This registers one callback and retains the live callback and state interfaces.
        unsafe { self.generator.BeginGetEvent(&self.callback, &self.state) }
    }

    fn next_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<MftAsyncNotification>, EncodeError> {
        let received = match self.receiver.recv_timeout(timeout) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => return Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(EncodeError::Backend(
                    "async MFT event callback channel disconnected".to_owned(),
                ))
            }
        };
        self.arm_event().map_err(|error| {
            EncodeError::Backend(format!("rearm async MFT event callback: {error}"))
        })?;
        received.map(Some).map_err(|error| {
            EncodeError::Backend(format!("async MFT event callback failed: {error}"))
        })
    }

    fn wait_for_input(&mut self) -> Result<(), EncodeError> {
        if self.need_input {
            self.need_input = false;
            return Ok(());
        }
        self.wait_for(METransformNeedInput.0 as u32, false)
            .map(|_| ())
    }

    fn wait_for_output(&mut self, draining: bool) -> Result<bool, EncodeError> {
        if self.have_output {
            self.have_output = false;
            return Ok(true);
        }
        self.wait_for(METransformHaveOutput.0 as u32, draining)
    }

    fn wait_for(&mut self, target: u32, draining: bool) -> Result<bool, EncodeError> {
        let deadline = Instant::now() + ASYNC_EVENT_TIMEOUT;
        let mut events_seen = 0usize;
        let mut event_types = [0u32; 8];
        let mut event_type_count = 0usize;
        while Instant::now() < deadline && events_seen < MAX_ASYNC_EVENTS_PER_WAIT {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(notification) = self.next_event(remaining)? else {
                break;
            };
            events_seen += 1;
            if event_type_count < event_types.len() {
                event_types[event_type_count] = notification.event_type;
                event_type_count += 1;
            }
            if notification.event_type == MEError.0 as u32 {
                return Err(EncodeError::Backend(format!(
                    "asynchronous MFT reported MEError with status {:?}",
                    notification.status
                )));
            }
            if notification.status.is_err() {
                return Err(EncodeError::Backend(format!(
                    "asynchronous MFT event {} failed: {:?}",
                    notification.event_type, notification.status
                )));
            }
            if notification.event_type == METransformNeedInput.0 as u32 {
                if target == notification.event_type {
                    return Ok(true);
                }
                self.need_input = true;
            } else if notification.event_type == METransformHaveOutput.0 as u32 {
                if target == notification.event_type {
                    return Ok(true);
                }
                self.have_output = true;
            } else if notification.event_type == METransformDrainComplete.0 as u32 {
                if draining {
                    return Ok(false);
                }
                return Err(EncodeError::Backend(
                    "async MFT completed a drain before a drain was requested".to_owned(),
                ));
            }
        }
        let awaited = if target == METransformNeedInput.0 as u32 {
            "METransformNeedInput"
        } else {
            "METransformHaveOutput"
        };
        Err(EncodeError::Backend(format!(
            "asynchronous MFT did not signal {awaited} within {} ms ({} events consumed: {:?})",
            ASYNC_EVENT_TIMEOUT.as_millis(),
            events_seen,
            &event_types[..event_type_count]
        )))
    }
}

impl Drop for AsyncMftEvents {
    fn drop(&mut self) {
        if let Ok(shutdown) = self.generator.cast::<IMFShutdown>() {
            // SAFETY: This shuts down the async transform's own event queue before it is released.
            let _ = unsafe { shutdown.Shutdown() };
        }
    }
}

fn create_media_type(
    config: EncoderConfig,
    subtype: &windows::core::GUID,
    h264_output: bool,
) -> Result<windows::Win32::Media::MediaFoundation::IMFMediaType, EncodeError> {
    // SAFETY: MFCreateMediaType initializes a valid COM media type.
    let media_type = unsafe { MFCreateMediaType() }
        .map_err(|error| EncodeError::Backend(format!("create media type: {error}")))?;
    let frame_size = (u64::from(config.width()) << 32) | u64::from(config.height());
    let frame_rate = (u64::from(config.fps()) << 32) | 1;
    // SAFETY: media_type is live, and the attribute keys and values remain valid per call.
    unsafe {
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|error| EncodeError::Backend(format!("set major type: {error}")))?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|error| EncodeError::Backend(format!("set subtype: {error}")))?;
        media_type
            .SetUINT64(&MF_MT_FRAME_SIZE, frame_size)
            .map_err(|error| EncodeError::Backend(format!("set frame size: {error}")))?;
        media_type
            .SetUINT64(&MF_MT_FRAME_RATE, frame_rate)
            .map_err(|error| EncodeError::Backend(format!("set frame rate: {error}")))?;
        media_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
            .map_err(|error| EncodeError::Backend(format!("set pixel aspect ratio: {error}")))?;
        media_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|error| EncodeError::Backend(format!("set progressive mode: {error}")))?;
        if h264_output {
            media_type
                .SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate_bps())
                .map_err(|error| EncodeError::Backend(format!("set average bitrate: {error}")))?;
            media_type
                .SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)
                .map_err(|error| EncodeError::Backend(format!("set Main profile: {error}")))?;
        } else {
            media_type
                .SetUINT32(&MF_SA_D3D11_AWARE, 1)
                .map_err(|error| EncodeError::Backend(format!("set D3D11 awareness: {error}")))?;
        }
    }
    Ok(media_type)
}

fn configure_codec_properties(
    transform: &IMFTransform,
    config: EncoderConfig,
) -> Result<AppliedCodecSettings, EncodeError> {
    let codec: ICodecAPI = transform
        .cast()
        .map_err(|error| EncodeError::Backend(format!("MFT lacks ICodecAPI: {error}")))?;
    let b_frames_disabled = set_codec_u32(&codec, &CODECAPI_AVEncMPVDefaultBPictureCount, 0, true)?;
    if !b_frames_disabled {
        return Err(EncodeError::Backend(
            "MFT cannot guarantee B-frame count zero".to_owned(),
        ));
    }
    let low_latency = set_codec_bool(&codec, &CODECAPI_AVLowLatencyMode, true, false)?;
    let real_time = set_codec_bool(&codec, &CODECAPI_AVEncCommonRealTime, true, false)?;
    let window_bytes = u64::from(config.bitrate_bps()) * RATE_CONTROL_WINDOW_MS / 8000;
    let small_rate_control_buffer = u32::try_from(window_bytes)
        .ok()
        .map(|bits| set_codec_u32(&codec, &CODECAPI_AVEncCommonBufferSize, bits, false))
        .transpose()?
        .unwrap_or(false);
    Ok(AppliedCodecSettings {
        low_latency,
        real_time,
        b_frames_disabled,
        small_rate_control_buffer,
    })
}

fn set_codec_bool(
    codec: &ICodecAPI,
    property: &windows::core::GUID,
    value: bool,
    required: bool,
) -> Result<bool, EncodeError> {
    let expected = VARIANT::from(value);
    set_codec_value(codec, property, &expected, required, "BOOL")
}

fn set_codec_u32(
    codec: &ICodecAPI,
    property: &windows::core::GUID,
    value: u32,
    required: bool,
) -> Result<bool, EncodeError> {
    let expected = VARIANT::from(value);
    set_codec_value(codec, property, &expected, required, "UINT32")
}

fn set_codec_value(
    codec: &ICodecAPI,
    property: &windows::core::GUID,
    expected: &VARIANT,
    required: bool,
    value_kind: &str,
) -> Result<bool, EncodeError> {
    // ICodecAPI's support/modifiability probes can report S_FALSE as success through HRESULT
    // wrappers. Attempt the write and verify the value instead of treating a successful probe as
    // proof that the setting took effect.
    // SAFETY: codec is a live ICodecAPI interface and expected remains alive for the call.
    if let Err(error) = unsafe { codec.SetValue(property, expected) } {
        if required {
            return Err(EncodeError::Backend(format!(
                "required {value_kind} codec property could not be set: {error}"
            )));
        }
        return Ok(false);
    }
    // SAFETY: The live codec interface returns an owned RAII VARIANT for this property.
    let actual = match unsafe { codec.GetValue(property) } {
        Ok(value) => value,
        Err(error) if required => {
            return Err(EncodeError::Backend(format!(
                "required {value_kind} codec property could not be read back: {error}"
            )));
        }
        Err(_) => return Ok(false),
    };
    if &actual != expected {
        if required {
            return Err(EncodeError::Backend(format!(
                "required {value_kind} codec property did not retain the requested value"
            )));
        }
        return Ok(false);
    }
    Ok(true)
}
fn create_input_sample(input: &D3D11Nv12Frame<'_>) -> Result<IMFSample, EncodeError> {
    // SAFETY: The live D3D11 texture implements the requested IID and MF holds its COM reference.
    let buffer =
        unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, input.texture, 0, BOOL(0)) }
            .map_err(|error| EncodeError::Backend(format!("wrap D3D11 surface: {error}")))?;
    // SAFETY: MFCreateSample returns an owned COM sample.
    let sample = unsafe { MFCreateSample() }
        .map_err(|error| EncodeError::Backend(format!("create input sample: {error}")))?;
    let timestamp_100ns = input
        .timestamp_us
        .checked_mul(10)
        .and_then(|timestamp| i64::try_from(timestamp).ok())
        .ok_or_else(|| EncodeError::InvalidFrame("timestamp exceeds MF range".to_owned()))?;
    // SAFETY: The sample retains its texture buffer and scalar timestamps are valid for the calls.
    unsafe {
        sample
            .AddBuffer(&buffer)
            .map_err(|error| EncodeError::Backend(format!("attach D3D11 surface: {error}")))?;
        sample
            .SetSampleTime(timestamp_100ns)
            .map_err(|error| EncodeError::Backend(format!("set sample time: {error}")))?;
        sample
            .SetSampleDuration(10_000_000 / i64::from(crate::FRAME_RATE))
            .map_err(|error| EncodeError::Backend(format!("set sample duration: {error}")))?;
    }
    Ok(sample)
}

fn validate_texture(input: D3D11Nv12Frame<'_>, config: EncoderConfig) -> Result<(), EncodeError> {
    if input.width != config.width() || input.height != config.height() {
        return Err(EncodeError::InvalidFrame(
            "D3D11 texture dimensions mismatch".to_owned(),
        ));
    }
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: desc is writable local storage and texture is a live ID3D11Texture2D.
    unsafe { input.texture.GetDesc(&mut desc) };
    if desc.Width != input.width
        || desc.Height != input.height
        || desc.Format != DXGI_FORMAT_NV12
        || desc.CPUAccessFlags != 0
        || desc.BindFlags & D3D11_BIND_VIDEO_ENCODER.0 as u32 == 0
    {
        return Err(EncodeError::InvalidFrame(
            "expected GPU-resident NV12 with VIDEO_ENCODER binding and no CPU access".to_owned(),
        ));
    }
    Ok(())
}

fn sample_timestamp_us(sample: &IMFSample) -> Option<u64> {
    // SAFETY: This reads a scalar timestamp from a live output sample.
    let timestamp = unsafe { sample.GetSampleTime() }.ok()?;
    u64::try_from(timestamp / 10).ok()
}

struct BufferUnlock<'a>(&'a windows::Win32::Media::MediaFoundation::IMFMediaBuffer);

impl Drop for BufferUnlock<'_> {
    fn drop(&mut self) {
        // SAFETY: Constructed only after successful Lock and dropped exactly once.
        let _ = unsafe { self.0.Unlock() };
    }
}

fn read_sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, EncodeError> {
    // SAFETY: MF returns a retained contiguous output-buffer interface.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(|error| EncodeError::Backend(format!("get output buffer: {error}")))?;
    let mut pointer = null_mut();
    let mut maximum = 0u32;
    let mut current = 0u32;
    // SAFETY: Output pointers are writable locals; BufferUnlock balances this successful lock.
    unsafe { buffer.Lock(&mut pointer, Some(&mut maximum), Some(&mut current)) }
        .map_err(|error| EncodeError::Backend(format!("lock output buffer: {error}")))?;
    let _unlock = BufferUnlock(&buffer);
    if current > maximum || current as usize > MAX_ENCODED_PACKET_BYTES {
        return Err(EncodeError::InvalidBitstream(
            "MF output exceeds packet bound".to_owned(),
        ));
    }
    if current == 0 {
        return Ok(Vec::new());
    }
    if pointer.is_null() {
        return Err(EncodeError::InvalidBitstream(
            "MF returned null output bytes".to_owned(),
        ));
    }
    // SAFETY: Lock guarantees current initialized bytes at pointer until _unlock is dropped.
    let bytes = unsafe { slice::from_raw_parts(pointer, current as usize) };
    Ok(bytes.to_vec())
}

fn create_hardware_device() -> Result<(ID3D11Device, ID3D11DeviceContext), EncodeError> {
    let mut device = None;
    let mut context = None;
    let levels = [D3D_FEATURE_LEVEL_11_0];
    let flags = D3D11_CREATE_DEVICE_FLAG(
        D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0,
    );
    // SAFETY: Null adapter/software select the default hardware adapter; output slots and feature
    // data are valid locals, and returned interfaces use windows-rs COM RAII wrappers.
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            flags,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|error| EncodeError::NoHardwareEncoder(format!("create D3D11 device: {error}")))?;
    let device =
        device.ok_or_else(|| EncodeError::Backend("D3D11 returned no device".to_owned()))?;
    let context =
        context.ok_or_else(|| EncodeError::Backend("D3D11 returned no context".to_owned()))?;
    Ok((device, context))
}

fn create_nv12_pattern_texture(
    device: &ID3D11Device,
    config: EncoderConfig,
) -> Result<(ID3D11Texture2D, Vec<u8>), EncodeError> {
    let width = config.width();
    let height = config.height();
    let bytes = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .and_then(|pixels| pixels.checked_mul(3))
        .map(|value| value / 2)
        .ok_or_else(|| EncodeError::InvalidFrame("NV12 size overflow".to_owned()))?;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_VIDEO_ENCODER.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: Descriptor dimensions are validated and bounded; the output slot is writable. The
    // GPU texture has no CPU access flags and is allocated on the same device as the MFT manager.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .map_err(|error| EncodeError::Backend(format!("create NV12 texture: {error}")))?;
    let texture =
        texture.ok_or_else(|| EncodeError::Backend("D3D11 returned no texture".to_owned()))?;
    Ok((texture, vec![0u8; bytes]))
}

fn fill_nv12_pattern(width: usize, height: usize, frame_index: u32, bytes: &mut [u8]) {
    const Y_BARS: [u8; 8] = [235, 210, 170, 145, 106, 81, 41, 16];
    const UV_BARS: [(u8, u8); 8] = [
        (128, 128),
        (16, 146),
        (166, 16),
        (54, 34),
        (202, 222),
        (90, 240),
        (240, 110),
        (128, 128),
    ];
    let y_len = width.saturating_mul(height);
    if width == 0 || height == 0 || bytes.len() != y_len.saturating_add(y_len / 2) {
        return;
    }
    let band_width = (width / Y_BARS.len()).max(1);
    for row in 0..height {
        for column in 0..width {
            bytes[row * width + column] = Y_BARS[(column / band_width).min(Y_BARS.len() - 1)];
        }
    }
    let marker_width = (width / 12).max(2);
    let marker_height = (height / 16).max(2);
    let marker_x = (frame_index as usize * width / 60) % width.saturating_sub(marker_width).max(1);
    let marker_y = height.saturating_sub(marker_height + 4);
    for row in marker_y..(marker_y + marker_height).min(height) {
        for column in marker_x..(marker_x + marker_width).min(width) {
            bytes[row * width + column] = 16;
        }
    }
    let uv = &mut bytes[y_len..];
    let chroma_band_width = (width / UV_BARS.len()).max(2) & !1;
    for row in 0..height / 2 {
        for pair in (0..width).step_by(2) {
            let (u, v) = UV_BARS[(pair / chroma_band_width).min(UV_BARS.len() - 1)];
            let offset = row * width + pair;
            uv[offset] = u;
            uv[offset + 1] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_pattern_is_bounded_and_animates() {
        let mut a = vec![0u8; 64 * 48 * 3 / 2];
        let mut b = vec![0u8; a.len()];
        fill_nv12_pattern(64, 48, 0, &mut a);
        fill_nv12_pattern(64, 48, 1, &mut b);
        assert!(a[..64 * 48].contains(&235));
        assert!(a[64 * 48..].iter().any(|value| *value != 0));
        assert_ne!(a, b);
        fill_nv12_pattern(63, 48, 0, &mut a);
    }

    #[test]
    fn activation_count_limit_rejects_oversized_lists() {
        let list = ActivationList {
            entries: null_mut(),
            count: MAX_MFT_CANDIDATES + 1,
        };
        assert!(list.usable_clones().is_err());
    }
}
