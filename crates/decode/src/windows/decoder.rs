//! Synchronous Media Foundation H.264 decoder producing bounded CPU NV12.
use crate::{
    validate_reset, DecodeError, DecodedFrame, Decoder, DecoderKind, EncodedAccessUnit,
    MAX_ENCODED_ACCESS_UNIT_BYTES, MAX_NV12_FRAME_BYTES,
};
use racc_proto::StreamReset;
use std::{
    collections::VecDeque,
    mem::ManuallyDrop,
    ptr::{drop_in_place, null_mut},
    slice,
    sync::OnceLock,
};
use windows::core::{IUnknown, Interface};
use windows::Win32::{
    Foundation::{HMODULE, RPC_E_CHANGED_MODE},
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::{
            D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING,
        },
        Dxgi::{Common::DXGI_FORMAT_NV12, IDXGIAdapter},
    },
    Media::MediaFoundation::{
        IMF2DBuffer, IMF2DBuffer2, IMFActivate, IMFDXGIBuffer, IMFDXGIDeviceManager,
        IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MF2DBuffer_LockFlags_Read,
        MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
        MFMediaType_Video, MFStartup, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
        MFVideoInterlace_Progressive, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG,
        MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
        MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_DATA_BUFFER_INCOMPLETE, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
        MFT_REGISTER_TYPE_INFO, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_DEFAULT_STRIDE,
        MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
        MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MF_VERSION,
    },
    System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED},
};
static MF_STARTED: OnceLock<Result<(), String>> = OnceLock::new();
const MAX_MFT_CANDIDATES: u32 = 64;
const MAX_OUTPUTS: usize = 8;
const MAX_PENDING: usize = 8;
const MAX_OUTPUT_BYTES: usize = MAX_NV12_FRAME_BYTES + 1080 * 512;
const MAX_DXGI_ARRAY_SLICES: u32 = 64;
const FRAME_TIME: i64 = 333_333;

/// Windows Media Foundation H.264 decoder. D3D11-aware hardware MFTs are tried first with a
/// DXGI device manager and bounded NV12 staging readback; the synchronous CPU-output MFT path is
/// used when hardware negotiation or D3D11 surface output is unavailable. Asynchronous MFTs are
/// not supported by this decoder.
pub struct MediaFoundationDecoder {
    transform: Option<IMFTransform>,
    activation: Option<IMFActivate>,
    dxva: Option<DxvaDevice>,
    staging: Option<StagingTexture>,
    _com: ComApartment,
    reset: Option<StreamReset>,
    kind: DecoderKind,
    out_flags: u32,
    out_size: u32,
    stride: Option<i32>,
    configured: bool,
    waiting: bool,
    last: Option<u32>,
    pending: VecDeque<Pending>,
}
impl MediaFoundationDecoder {
    /// Initializes COM and Media Foundation on the calling decode thread.
    pub fn new() -> Result<Self, DecodeError> {
        startup()?;
        Ok(Self {
            transform: None,
            activation: None,
            dxva: None,
            staging: None,
            _com: ComApartment::init()?,
            reset: None,
            kind: DecoderKind::MediaFoundation,
            out_flags: 0,
            out_size: 0,
            stride: None,
            configured: false,
            waiting: true,
            last: None,
            pending: VecDeque::with_capacity(MAX_PENDING),
        })
    }
    fn select(&mut self, reset: StreamReset) -> Result<(), DecodeError> {
        let mut failures = Vec::new();
        let hardware_flags =
            MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        match (create_dxva_device(), enumerate(hardware_flags)) {
            (Ok(dxva), Ok(list)) => match list.clones() {
                Ok(candidates) => {
                    let (selected, rejected) = select_candidate(candidates, |activation| {
                        configure(activation, reset, Some(&dxva.manager))
                    });
                    failures.extend(rejected);
                    if let Some((activation, configured)) = selected {
                        self.transform = Some(configured.transform);
                        self.activation = Some(activation);
                        self.dxva = Some(dxva);
                        self.staging = None;
                        self.kind = decoder_kind_for_path(true);
                        self.out_flags = configured.flags;
                        self.out_size = configured.size;
                        self.stride = configured.stride;
                        return Ok(());
                    }
                }
                Err(error) if failures.len() < 8 => failures.push(error.to_string()),
                Err(_) => {}
            },
            (Err(error), _) | (_, Err(error)) if failures.len() < 8 => failures.push(error),
            (Err(_), _) | (_, Err(_)) => {}
        }

        // A CPU-accessible output does not prove software decoding. This fallback is labeled as
        // the synchronous CPU-output MFT path and is used only after D3D11 negotiation failed.
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let list = enumerate(flags).map_err(|error| {
            DecodeError::Backend(format!("no Media Foundation H.264 decoder: {error}"))
        })?;
        let candidates = list.clones()?;
        let (selected, rejected) =
            select_candidate(candidates, |activation| configure(activation, reset, None));
        failures.extend(rejected);
        if let Some((activation, configured)) = selected {
            self.transform = Some(configured.transform);
            self.activation = Some(activation);
            self.dxva = None;
            self.staging = None;
            self.kind = decoder_kind_for_path(false);
            self.out_flags = configured.flags;
            self.out_size = configured.size;
            self.stride = configured.stride;
            return Ok(());
        }
        let why = if failures.is_empty() {
            "no D3D11-aware hardware MFT or synchronous CPU-output MFT was found".to_owned()
        } else {
            failures.join("; ")
        };
        Err(DecodeError::Backend(format!(
            "no Media Foundation H.264 decoder: {why}"
        )))
    }
    fn submit(
        &mut self,
        a: &EncodedAccessUnit,
        track: bool,
    ) -> Result<Option<DecodedFrame>, DecodeError> {
        if a.bytes.len() > MAX_ENCODED_ACCESS_UNIT_BYTES {
            return Err(DecodeError::InvalidBitstream("access unit exceeds 8 MiB"));
        }
        let n = u32::try_from(a.bytes.len())
            .map_err(|_| DecodeError::InvalidBitstream("access unit size overflow"))?;
        // SAFETY: Buffer size is bounded by the validated input limit.
        let b = unsafe { MFCreateMemoryBuffer(n) }.map_err(|e| mferr("create input buffer", e))?;
        write_buffer(&b, &a.bytes)?;
        // SAFETY: The factories return owned COM interfaces.
        let s = unsafe { MFCreateSample() }.map_err(|e| mferr("create input sample", e))?;
        // SAFETY: The live sample retains the live buffer.
        unsafe { s.AddBuffer(&b) }.map_err(|e| mferr("attach input buffer", e))?;
        let time = i64::from(a.frame_id)
            .checked_mul(FRAME_TIME)
            .ok_or(DecodeError::InvalidBitstream("timestamp overflow"))?;
        // SAFETY: Sample is live; timestamps use 100 ns units.
        unsafe {
            s.SetSampleTime(time)
                .map_err(|e| mferr("set sample time", e))?;
            s.SetSampleDuration(FRAME_TIME)
                .map_err(|e| mferr("set sample duration", e))?;
        }
        if track {
            if self.pending.len() >= MAX_PENDING {
                return Err(DecodeError::Backend("pending decode bound exceeded".into()));
            }
            self.pending.push_back(Pending {
                time,
                epoch: a.epoch,
                id: a.frame_id,
                capture: a.capture_ts_us,
            });
        }
        let t = self
            .transform
            .as_ref()
            .ok_or(DecodeError::InvalidConfig("decoder is not configured"))?;
        // SAFETY: Configured H.264 stream accepts this live sample.
        if let Err(e) = unsafe { t.ProcessInput(0, &s, 0) } {
            if track {
                self.pending.pop_back();
            }
            return Err(mferr("ProcessInput", e));
        }
        self.drain()
    }
    fn drain(&mut self) -> Result<Option<DecodedFrame>, DecodeError> {
        let mut latest = None;
        for _ in 0..MAX_OUTPUTS {
            let (s, status) = self.output()?;
            let Some(s) = s else { break };
            if let Some(f) = self.copy_frame(&s)? {
                latest = Some(f)
            }
            if status & (MFT_OUTPUT_DATA_BUFFER_INCOMPLETE.0 as u32) == 0 {
                break;
            }
        }
        Ok(latest)
    }
    fn output(&mut self) -> Result<(Option<IMFSample>, u32), DecodeError> {
        let caller = if self.out_flags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) == 0 {
            // SAFETY: Output sample and allocation are capped by MAX_OUTPUT_BYTES.
            let s = unsafe { MFCreateSample() }.map_err(|e| mferr("create output sample", e))?;
            let b = unsafe { MFCreateMemoryBuffer(self.out_size) }
                .map_err(|e| mferr("create output buffer", e))?;
            // SAFETY: The live sample retains the bounded buffer.
            unsafe { s.AddBuffer(&b) }.map_err(|e| mferr("attach output buffer", e))?;
            Some(s)
        } else {
            None
        };
        let mut o = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(caller),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };
        let mut status = 0;
        let t = self
            .transform
            .as_ref()
            .ok_or(DecodeError::InvalidConfig("decoder is not configured"))?;
        // SAFETY: Output record and status are writable locals for one stream.
        let result = unsafe { t.ProcessOutput(0, std::slice::from_mut(&mut o), &mut status) };
        // SAFETY: ProcessOutput initializes these fields; take each returned COM option once.
        let sample = unsafe { ManuallyDrop::take(&mut o.pSample) };
        // SAFETY: Any returned event collection is released after the call.
        let events = unsafe { ManuallyDrop::take(&mut o.pEvents) };
        drop(events);
        match result {
            Ok(()) => Ok((sample, o.dwStatus)),
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok((None, 0)),
            Err(e) => Err(mferr("ProcessOutput", e)),
        }
    }
    fn copy_frame(&mut self, s: &IMFSample) -> Result<Option<DecodedFrame>, DecodeError> {
        let meta = metadata(&mut self.pending, s);
        let Some(meta) = meta else { return Ok(None) };
        let r = self
            .reset
            .ok_or(DecodeError::InvalidConfig("decoder is not configured"))?;
        let (w, h) = (u32::from(r.width), u32::from(r.height));
        if let Some(dxva) = self.dxva.as_ref() {
            if let Ok(buffer) = single_sample_buffer(s) {
                if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
                    let (y, uv) = read_dxgi_nv12(dxva, &dxgi, &mut self.staging, w, h)?;
                    return DecodedFrame::new(meta.epoch, meta.id, meta.capture, w, h, y, uv)
                        .map(Some);
                }
            }
            // Some registered transforms accept the D3D manager but still output CPU-accessible
            // samples. Preserve the frame; the reported kind identifies the selected MFT class,
            // which is documented not to prove DXVA use.
        }
        // SAFETY: Sample is a live output from a CPU-accessible Media Foundation transform.
        let b = unsafe { s.ConvertToContiguousBuffer() }
            .map_err(|e| mferr("convert output buffer", e))?;
        let (y, uv) = match b.cast::<IMF2DBuffer2>() {
            Ok(two) => read_2d(&two, w, h)?,
            Err(_) => read_linear(&b, self.stride, w, h)?,
        };
        DecodedFrame::new(meta.epoch, meta.id, meta.capture, w, h, y, uv).map(Some)
    }
}
impl Decoder for MediaFoundationDecoder {
    fn kind(&self) -> DecoderKind {
        self.kind
    }
    fn configure(&mut self, r: StreamReset) -> Result<(), DecodeError> {
        self.transform = None;
        self.activation = None;
        self.dxva = None;
        self.staging = None;
        self.reset = None;
        self.configured = false;
        self.waiting = true;
        self.last = None;
        self.pending.clear();
        self.out_flags = 0;
        self.out_size = 0;
        self.stride = None;
        validate_reset(r)?;
        self.select(r)?;
        self.reset = Some(r);
        Ok(())
    }
    fn decode(&mut self, a: &EncodedAccessUnit) -> Result<Option<DecodedFrame>, DecodeError> {
        let r = self.reset.ok_or(DecodeError::InvalidConfig(
            "decoder has not been configured",
        ))?;
        if a.epoch != r.epoch {
            return Err(DecodeError::WrongEpoch {
                expected: r.epoch,
                received: a.epoch,
            });
        }
        if let Some(p) = self.last {
            if a.frame_id <= p {
                return Err(DecodeError::OutOfOrderFrame {
                    previous: p,
                    received: a.frame_id,
                });
            }
        }
        self.last = Some(a.frame_id);
        if a.config && !a.has_parameter_sets() {
            return Err(DecodeError::InvalidBitstream(
                "config flag is set without SPS/PPS",
            ));
        }
        if a.has_parameter_sets() {
            self.configured = true
        }
        if a.is_keyframe() && !self.configured {
            self.waiting = true;
            return Err(DecodeError::MissingCodecConfiguration);
        }
        if self.waiting && !a.is_keyframe() {
            if !a.has_parameter_sets() {
                return Err(DecodeError::AwaitingKeyframe);
            }
            return self.submit(a, false);
        }
        if a.is_keyframe() {
            self.waiting = false
        }
        self.submit(a, a.has_vcl())
    }
    fn flush(&mut self) -> Result<(), DecodeError> {
        self.pending.clear();
        self.configured = false;
        self.waiting = true;
        self.last = None;
        if let Some(t) = &self.transform {
            // SAFETY: Live decoder receives the standard flush command.
            unsafe { t.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) }
                .map_err(|e| mferr("flush decoder", e))?;
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Pending {
    time: i64,
    epoch: u16,
    id: u32,
    capture: u32,
}
struct Configured {
    transform: IMFTransform,
    flags: u32,
    size: u32,
    stride: Option<i32>,
}
struct DxvaDevice {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    manager: IMFDXGIDeviceManager,
}
struct StagingTexture {
    source: D3D11_TEXTURE2D_DESC,
    texture: ID3D11Texture2D,
}
struct MappedTexture<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
    subresource: u32,
}
impl Drop for MappedTexture<'_> {
    fn drop(&mut self) {
        // SAFETY: This guard is constructed only after Map succeeds and unmaps that exact resource.
        unsafe { self.context.Unmap(self.texture, self.subresource) };
    }
}
fn metadata(q: &mut VecDeque<Pending>, s: &IMFSample) -> Option<Pending> {
    // SAFETY: The live output sample may contain its corresponding timestamp.
    if let Ok(t) = unsafe { s.GetSampleTime() } {
        if let Some(i) = q.iter().position(|v| v.time == t) {
            return q.remove(i);
        }
    }
    q.pop_front()
}
fn configure(
    a: &IMFActivate,
    r: StreamReset,
    dxva_manager: Option<&IMFDXGIDeviceManager>,
) -> Result<Configured, String> {
    // SAFETY: Activation is live and returns an owned transform.
    let t: IMFTransform = unsafe { a.ActivateObject() }.map_err(|e| e.to_string())?;
    // SAFETY: The live transform returns an owned attributes interface.
    let attrs = unsafe { t.GetAttributes() }.map_err(|e| e.to_string())?;
    // SAFETY: MF_TRANSFORM_ASYNC is a standard scalar attribute.
    if unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0 {
        return Err("asynchronous MFT unsupported".into());
    }
    if let Some(manager) = dxva_manager {
        // A transform must explicitly advertise D3D11 awareness before receiving a device manager.
        // SAFETY: MF_SA_D3D11_AWARE is a standard scalar transform attribute.
        let d3d11_aware = unsafe { attrs.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) != 0;
        if !hardware_candidate_eligible(d3d11_aware, true) {
            return Err("hardware MFT is not D3D11-aware".into());
        }
        // SAFETY: The manager remains owned by MediaFoundationDecoder for the transform lifetime.
        unsafe {
            t.ProcessMessage(
                MFT_MESSAGE_SET_D3D_MANAGER,
                Interface::as_raw(manager) as usize,
            )
        }
        .map_err(|e| format!("set DXGI device manager: {e}"))?;
    }
    let input = make_type(r, &MFVideoFormat_H264)?;
    // SAFETY: Stream zero receives a live H.264 media type.
    unsafe { t.SetInputType(0, &input, 0) }.map_err(|e| format!("input type: {e}"))?;
    let output = make_type(r, &MFVideoFormat_NV12)?;
    // SAFETY: Stream zero receives a live NV12 media type.
    unsafe { t.SetOutputType(0, &output, 0) }.map_err(|e| format!("output type: {e}"))?;
    // SAFETY: The configured synchronous transform receives standard stream notifications.
    unsafe {
        t.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .map_err(|e| e.to_string())?;
        t.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
            .map_err(|e| e.to_string())?;
    }
    // SAFETY: The configured transform reports its output allocation.
    let info = unsafe { t.GetOutputStreamInfo(0) }.map_err(|e| e.to_string())?;
    let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
    if dxva_manager.is_some() && !hardware_candidate_eligible(true, provides_samples) {
        return Err("D3D11 decoder does not provide its surface-backed output samples".into());
    }
    let pixels = usize::from(r.width)
        .checked_mul(usize::from(r.height))
        .ok_or("NV12 dimension overflow")?;
    let min = pixels.checked_add(pixels / 2).ok_or("NV12 size overflow")?;
    let size = usize::try_from(info.cbSize).unwrap_or(min).max(min);
    if size > MAX_OUTPUT_BYTES {
        return Err("output allocation exceeds NV12 bound".into());
    }
    // SAFETY: The live transform returns the negotiated output media type.
    let current: IMFMediaType = unsafe { t.GetOutputCurrentType(0) }.map_err(|e| e.to_string())?;
    // SAFETY: Default stride is an optional scalar attribute.
    let stride = unsafe { current.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
        .ok()
        .map(|v| v as i32);
    Ok(Configured {
        transform: t,
        flags: info.dwFlags,
        size: u32::try_from(size).map_err(|_| "output buffer size overflow")?,
        stride,
    })
}
fn hardware_candidate_eligible(d3d11_aware: bool, provides_samples: bool) -> bool {
    d3d11_aware && provides_samples
}
fn decoder_kind_for_path(dxgi_surface_backed: bool) -> DecoderKind {
    if dxgi_surface_backed {
        DecoderKind::WindowsMediaFoundationHardwareMft
    } else {
        DecoderKind::WindowsMediaFoundationSynchronousMft
    }
}
fn select_candidate<C, T>(
    candidates: Vec<C>,
    mut configure_candidate: impl FnMut(&C) -> Result<T, String>,
) -> (Option<(C, T)>, Vec<String>) {
    let mut failures = Vec::new();
    for candidate in candidates {
        match configure_candidate(&candidate) {
            Ok(configured) => return (Some((candidate, configured)), failures),
            Err(error) if failures.len() < 8 => failures.push(error),
            Err(_) => {}
        }
    }
    (None, failures)
}
fn create_dxva_device() -> Result<DxvaDevice, String> {
    let (mut device, mut context) = (None, None);
    let levels = [D3D_FEATURE_LEVEL_11_0];
    let flags = D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0);
    // SAFETY: The adapter is selected by the system; output slots and feature levels are valid.
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
    .map_err(|error| format!("create D3D11 decode device: {error}"))?;
    let device = device.ok_or_else(|| "D3D11 returned no decode device".to_owned())?;
    let context = context.ok_or_else(|| "D3D11 returned no immediate context".to_owned())?;
    let (mut token, mut manager) = (0, None);
    // SAFETY: Both output locations are writable; Media Foundation returns an owned COM manager.
    unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }
        .map_err(|error| format!("create DXGI device manager: {error}"))?;
    let manager = manager.ok_or_else(|| "Media Foundation returned no DXGI manager".to_owned())?;
    // SAFETY: The live manager receives the live D3D11 device and its own reset token.
    unsafe { manager.ResetDevice(&device, token) }
        .map_err(|error| format!("bind D3D11 device to DXGI manager: {error}"))?;
    Ok(DxvaDevice {
        device,
        context,
        manager,
    })
}
fn single_sample_buffer(sample: &IMFSample) -> Result<IMFMediaBuffer, DecodeError> {
    // SAFETY: The output sample is live and returns an initialized count.
    let count = unsafe { sample.GetBufferCount() }
        .map_err(|error| mferr("get decoder output buffer count", error))?;
    if count != 1 {
        return Err(DecodeError::InvalidFrame(
            "DXVA output sample must contain one NV12 surface",
        ));
    }
    // SAFETY: Index zero is valid because the sample count was checked above.
    unsafe { sample.GetBufferByIndex(0) }
        .map_err(|error| mferr("get decoder output surface", error))
}
fn read_dxgi_nv12(
    dxva: &DxvaDevice,
    buffer: &IMFDXGIBuffer,
    staging: &mut Option<StagingTexture>,
    width: u32,
    height: u32,
) -> Result<(Vec<u8>, Vec<u8>), DecodeError> {
    // SAFETY: The sample buffer is a live Media Foundation DXGI surface wrapper.
    let subresource = unsafe { buffer.GetSubresourceIndex() }
        .map_err(|error| mferr("get DXVA output subresource", error))?;
    let mut raw_texture = null_mut();
    // SAFETY: GetResource writes a COM interface pointer for the requested ID3D11Texture2D.
    unsafe { buffer.GetResource(&ID3D11Texture2D::IID, &mut raw_texture) }
        .map_err(|error| mferr("get DXVA output texture", error))?;
    if raw_texture.is_null() {
        return Err(DecodeError::InvalidFrame("null DXVA output texture"));
    }
    // SAFETY: GetResource succeeded and transferred one owned reference to this interface pointer.
    let source = unsafe { ID3D11Texture2D::from_raw(raw_texture) };
    let mut source_desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: GetDesc initializes the writable descriptor for the live texture.
    unsafe { source.GetDesc(&mut source_desc) };
    validate_dxgi_output(&source_desc, subresource, width, height)?;

    // Verify this surface came from the manager's device before issuing a D3D11 copy.
    // SAFETY: Both live textures/devices expose owned ID3D11Device interfaces.
    let source_device =
        unsafe { source.GetDevice() }.map_err(|error| mferr("get DXVA surface device", error))?;
    let source_identity = source_device
        .cast::<IUnknown>()
        .map_err(|error| mferr("query DXVA surface device identity", error))?;
    let expected_identity = dxva
        .device
        .cast::<IUnknown>()
        .map_err(|error| mferr("query decode device identity", error))?;
    if Interface::as_raw(&source_identity) != Interface::as_raw(&expected_identity) {
        return Err(DecodeError::InvalidFrame(
            "DXVA output surface belongs to another D3D11 device",
        ));
    }

    let staging_texture = match staging.as_ref() {
        Some(cached) if cached.source == source_desc => cached.texture.clone(),
        _ => {
            let mut desc = source_desc;
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut texture = None;
            // SAFETY: Descriptor is copied from the source and changed to a bounded readback target.
            unsafe { dxva.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
                .map_err(|error| mferr("create NV12 staging texture", error))?;
            let texture = texture.ok_or(DecodeError::InvalidFrame(
                "D3D11 returned no staging texture",
            ))?;
            *staging = Some(StagingTexture {
                source: source_desc,
                texture: texture.clone(),
            });
            texture
        }
    };
    // SAFETY: The cached staging texture mirrors the source subresource layout and both belong to
    // the verified same device. One subresource is copied; this decode context is thread-confined.
    unsafe {
        dxva.context.CopySubresourceRegion(
            &staging_texture,
            subresource,
            0,
            0,
            0,
            &source,
            subresource,
            None,
        )
    };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: The staging resource is CPU-readable and Map initializes the local mapping record.
    unsafe {
        dxva.context.Map(
            &staging_texture,
            subresource,
            D3D11_MAP_READ,
            0,
            Some(&mut mapped),
        )
    }
    .map_err(|error| mferr("map NV12 staging texture", error))?;
    let _mapped = MappedTexture {
        context: &dxva.context,
        texture: &staging_texture,
        subresource,
    };
    let pitch = usize::try_from(mapped.RowPitch)
        .map_err(|_| DecodeError::InvalidFrame("DXVA row pitch overflow"))?;
    let length = nv12_readback_length(pitch, width, height)?;
    if mapped.pData.is_null() {
        return Err(DecodeError::InvalidFrame(
            "invalid DXVA mapped NV12 surface",
        ));
    }
    // SAFETY: D3D11 Map keeps pData valid for the documented NV12 row extent and the checked pitch
    // and height bound this read. The guard unmaps the texture on every return path.
    let bytes = unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), length) };
    copy_planes(bytes, 0, pitch, width, height)
}
fn validate_dxgi_output(
    desc: &D3D11_TEXTURE2D_DESC,
    subresource: u32,
    width: u32,
    height: u32,
) -> Result<(), DecodeError> {
    if desc.Width != width
        || desc.Height != height
        || desc.Format != DXGI_FORMAT_NV12
        || desc.SampleDesc.Count != 1
        || desc.MipLevels != 1
        || desc.ArraySize == 0
        || desc.ArraySize > MAX_DXGI_ARRAY_SLICES
        || subresource >= desc.ArraySize
    {
        return Err(DecodeError::InvalidFrame(
            "unsupported DXVA NV12 texture layout",
        ));
    }
    let minimum_pitch =
        usize::try_from(width).map_err(|_| DecodeError::InvalidFrame("DXVA width overflow"))?;
    let minimum_bytes = minimum_pitch
        .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
        .and_then(|luma| luma.checked_add(luma / 2))
        .ok_or(DecodeError::InvalidFrame("DXVA output size overflow"))?;
    if minimum_bytes > MAX_NV12_FRAME_BYTES {
        return Err(DecodeError::InvalidFrame("DXVA frame exceeds maximum"));
    }
    Ok(())
}
fn nv12_readback_length(pitch: usize, width: u32, height: u32) -> Result<usize, DecodeError> {
    let width =
        usize::try_from(width).map_err(|_| DecodeError::InvalidFrame("DXVA width overflow"))?;
    if pitch < width {
        return Err(DecodeError::InvalidFrame(
            "DXVA row pitch is smaller than width",
        ));
    }
    let height =
        usize::try_from(height).map_err(|_| DecodeError::InvalidFrame("DXVA height overflow"))?;
    let rows = height
        .checked_add(height / 2)
        .ok_or(DecodeError::InvalidFrame("DXVA plane row count overflow"))?;
    pitch
        .checked_mul(rows)
        .filter(|length| *length <= MAX_OUTPUT_BYTES)
        .ok_or(DecodeError::InvalidFrame(
            "DXVA readback exceeds the output bound",
        ))
}
fn make_type(r: StreamReset, subtype: &windows::core::GUID) -> Result<IMFMediaType, String> {
    // SAFETY: Factory returns a new owned media type.
    let m = unsafe { MFCreateMediaType() }.map_err(|e| e.to_string())?;
    let size = (u64::from(r.width) << 32) | u64::from(r.height);
    let rate = (u64::from(r.fps) << 32) | 1;
    // SAFETY: Fixed GUID and bounded scalar attributes on a live media type.
    unsafe {
        m.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        m.SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|e| e.to_string())?;
        m.SetUINT64(&MF_MT_FRAME_SIZE, size)
            .map_err(|e| e.to_string())?;
        m.SetUINT64(&MF_MT_FRAME_RATE, rate)
            .map_err(|e| e.to_string())?;
        m.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
            .map_err(|e| e.to_string())?;
        m.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| e.to_string())?;
    }
    Ok(m)
}
fn enumerate(flags: MFT_ENUM_FLAG) -> Result<Activations, String> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let mut a = Activations {
        entries: null_mut(),
        count: 0,
    };
    // SAFETY: Type descriptors and writable output slots are valid; RAII list frees returned memory.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            flags,
            Some(&input),
            Some(&output),
            &mut a.entries,
            &mut a.count,
        )
    }
    .map_err(|e| format!("enumerate MFTs: {e}"))?;
    Ok(a)
}
struct Activations {
    entries: *mut Option<IMFActivate>,
    count: u32,
}
impl Activations {
    fn clones(&self) -> Result<Vec<IMFActivate>, DecodeError> {
        if self.count == 0 {
            return Ok(Vec::new());
        }
        if self.entries.is_null() {
            return Err(DecodeError::Backend(
                "MF returned null activation list".into(),
            ));
        }
        if self.count > MAX_MFT_CANDIDATES {
            return Err(DecodeError::Backend(format!(
                "MF returned {} decoder candidates; limit is {MAX_MFT_CANDIDATES}",
                self.count
            )));
        }
        // SAFETY: MFTEnumEx returned count initialized activation options in this owned array.
        let entries = unsafe { slice::from_raw_parts(self.entries, self.count as usize) };
        Ok(entries.iter().filter_map(Clone::clone).collect())
    }
}
impl Drop for Activations {
    fn drop(&mut self) {
        if self.entries.is_null() {
            return;
        }
        // SAFETY: Pointer/count came from MFTEnumEx; release each option and the CoTaskMem array once.
        unsafe {
            for i in 0..self.count as usize {
                drop_in_place(self.entries.add(i));
            }
            CoTaskMemFree(Some(self.entries.cast()));
        }
    }
}
struct Lock<'a> {
    buffer: &'a IMFMediaBuffer,
}
impl Drop for Lock<'_> {
    fn drop(&mut self) {
        // SAFETY: Guard is created only after successful Lock and unlocks exactly once.
        let _ = unsafe { self.buffer.Unlock() };
    }
}
struct Lock2d<'a> {
    buffer: &'a IMF2DBuffer,
}
impl Drop for Lock2d<'_> {
    fn drop(&mut self) {
        // SAFETY: Guard is created only after successful Lock2DSize and unlocks exactly once.
        let _ = unsafe { self.buffer.Unlock2D() };
    }
}
fn write_buffer(b: &IMFMediaBuffer, bytes: &[u8]) -> Result<(), DecodeError> {
    let mut data = null_mut();
    let mut cap = 0u32;
    // SAFETY: Output pointers are writable locals for this live buffer.
    unsafe { b.Lock(&mut data, Some(&mut cap), None) }.map_err(|e| mferr("lock input", e))?;
    let guard = Lock { buffer: b };
    if data.is_null() || usize::try_from(cap).unwrap_or(0) < bytes.len() {
        drop(guard);
        return Err(DecodeError::Backend("MF input buffer is undersized".into()));
    }
    // SAFETY: Locked buffer capacity is at least the bounded input length.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len()) };
    let n = u32::try_from(bytes.len())
        .map_err(|_| DecodeError::InvalidBitstream("input size overflow"))?;
    // SAFETY: Written length is within the locked buffer capacity.
    unsafe { b.SetCurrentLength(n) }.map_err(|e| mferr("set input length", e))?;
    drop(guard);
    Ok(())
}
fn read_2d(b: &IMF2DBuffer2, w: u32, h: u32) -> Result<(Vec<u8>, Vec<u8>), DecodeError> {
    let (mut row0, mut pitch, mut base, mut len) = (null_mut(), 0i32, null_mut(), 0u32);
    // SAFETY: Live 2D buffer and writable output slots.
    unsafe {
        b.Lock2DSize(
            MF2DBuffer_LockFlags_Read,
            &mut row0,
            &mut pitch,
            &mut base,
            &mut len,
        )
    }
    .map_err(|e| mferr("lock 2D output", e))?;
    let guard = Lock2d { buffer: b };
    if row0.is_null() || base.is_null() || pitch <= 0 {
        drop(guard);
        return Err(DecodeError::InvalidFrame(
            "unsupported MF NV12 pointer or stride",
        ));
    }
    let len =
        usize::try_from(len).map_err(|_| DecodeError::InvalidFrame("output length overflow"))?;
    if len > MAX_OUTPUT_BYTES {
        drop(guard);
        return Err(DecodeError::InvalidFrame("MF output exceeds bound"));
    }
    // SAFETY: Lock2DSize returns row0 and base pointers into one locked allocation.
    let origin = unsafe { row0.offset_from(base) };
    let origin = usize::try_from(origin)
        .map_err(|_| DecodeError::InvalidFrame("scanline precedes buffer"))?;
    // SAFETY: MF reports this readable length, held until the guard unlocks.
    let src = unsafe { slice::from_raw_parts(base, len) };
    let result = copy_planes(src, origin, pitch as usize, w, h);
    drop(guard);
    result
}
fn read_linear(
    b: &IMFMediaBuffer,
    stride: Option<i32>,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, Vec<u8>), DecodeError> {
    let (mut data, mut cap, mut len) = (null_mut(), 0u32, 0u32);
    // SAFETY: Lock output pointers are writable locals for this live buffer.
    unsafe { b.Lock(&mut data, Some(&mut cap), Some(&mut len)) }
        .map_err(|e| mferr("lock linear output", e))?;
    let guard = Lock { buffer: b };
    if data.is_null() {
        drop(guard);
        return Err(DecodeError::InvalidFrame("null MF output buffer"));
    }
    let len =
        usize::try_from(len).map_err(|_| DecodeError::InvalidFrame("output length overflow"))?;
    if len > MAX_OUTPUT_BYTES || usize::try_from(cap).unwrap_or(0) < len {
        drop(guard);
        return Err(DecodeError::InvalidFrame("MF output exceeds bound"));
    }
    let rows = usize::try_from(h).unwrap_or(0) + usize::try_from(h / 2).unwrap_or(0);
    let pitch = match stride {
        Some(s) if s > 0 => s as usize,
        Some(_) => {
            drop(guard);
            return Err(DecodeError::InvalidFrame(
                "negative NV12 stride unsupported",
            ));
        }
        None if rows > 0 && len % rows == 0 => len / rows,
        None => usize::try_from(w).map_err(|_| DecodeError::InvalidFrame("width overflow"))?,
    };
    // SAFETY: Lock reports len readable bytes until guard unlocks.
    let src = unsafe { slice::from_raw_parts(data, len) };
    let result = copy_planes(src, 0, pitch, w, h);
    drop(guard);
    result
}
fn copy_planes(
    src: &[u8],
    origin: usize,
    pitch: usize,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, Vec<u8>), DecodeError> {
    let row = usize::try_from(w).map_err(|_| DecodeError::InvalidFrame("width overflow"))?;
    let rows = usize::try_from(h).map_err(|_| DecodeError::InvalidFrame("height overflow"))?;
    if row == 0 || rows < 2 || rows % 2 != 0 || pitch < row {
        return Err(DecodeError::InvalidFrame(
            "invalid NV12 dimensions or stride",
        ));
    }
    let uvrows = rows / 2;
    let uv = origin
        .checked_add(
            pitch
                .checked_mul(rows)
                .ok_or(DecodeError::InvalidFrame("plane offset overflow"))?,
        )
        .ok_or(DecodeError::InvalidFrame("plane offset overflow"))?;
    let yend = origin
        .checked_add(
            (rows - 1)
                .checked_mul(pitch)
                .ok_or(DecodeError::InvalidFrame("luma bounds overflow"))?,
        )
        .and_then(|v| v.checked_add(row))
        .ok_or(DecodeError::InvalidFrame("luma bounds overflow"))?;
    let uvend = uv
        .checked_add(
            (uvrows - 1)
                .checked_mul(pitch)
                .ok_or(DecodeError::InvalidFrame("chroma bounds overflow"))?,
        )
        .and_then(|v| v.checked_add(row))
        .ok_or(DecodeError::InvalidFrame("chroma bounds overflow"))?;
    if yend > src.len() || uvend > src.len() {
        return Err(DecodeError::InvalidFrame("short MF NV12 output"));
    }
    let n = row
        .checked_mul(rows)
        .ok_or(DecodeError::InvalidFrame("luma allocation overflow"))?;
    if n.saturating_add(n / 2) > MAX_NV12_FRAME_BYTES {
        return Err(DecodeError::InvalidFrame("NV12 frame exceeds maximum"));
    }
    let (mut y, mut chroma) = (vec![0; n], vec![0; n / 2]);
    for r in 0..rows {
        let (s, d) = (origin + r * pitch, r * row);
        y[d..d + row].copy_from_slice(&src[s..s + row])
    }
    for r in 0..uvrows {
        let (s, d) = (uv + r * pitch, r * row);
        chroma[d..d + row].copy_from_slice(&src[s..s + row])
    }
    Ok((y, chroma))
}
struct ComApartment {
    owned: bool,
}
impl ComApartment {
    fn init() -> Result<Self, DecodeError> {
        // SAFETY: CoInitializeEx accepts a null reserved pointer and retains nothing.
        let r = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if r.is_ok() {
            Ok(Self { owned: true })
        } else if r == RPC_E_CHANGED_MODE {
            Ok(Self { owned: false })
        } else {
            Err(DecodeError::Backend(format!("initialize COM: {r:?}")))
        }
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: Balances this guard's successful CoInitializeEx exactly once.
            unsafe { CoUninitialize() };
        }
    }
}
fn startup() -> Result<(), DecodeError> {
    let r = MF_STARTED.get_or_init(|| {
        // SAFETY: MFStartup is process-wide and called once with the SDK version.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string())
    });
    r.as_ref()
        .map(|_| ())
        .map_err(|e| DecodeError::Backend(format!("start Media Foundation: {e}")))
}
fn mferr(label: &str, e: windows::core::Error) -> DecodeError {
    DecodeError::Backend(format!("{label}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dxva_candidate_requires_d3d11_awareness_and_surface_samples() {
        assert!(hardware_candidate_eligible(true, true));
        assert!(!hardware_candidate_eligible(false, true));
        assert!(!hardware_candidate_eligible(true, false));
        assert!(!hardware_candidate_eligible(false, false));
    }

    #[test]
    fn candidate_selection_tries_fallback_after_dxva_rejection() {
        let (selected, rejected) = select_candidate(vec!["first", "second"], |candidate| {
            if *candidate == "second" {
                Ok(DecoderKind::WindowsMediaFoundationHardwareMft)
            } else {
                Err("candidate lacks usable D3D11 output".to_owned())
            }
        });
        assert_eq!(
            selected,
            Some(("second", DecoderKind::WindowsMediaFoundationHardwareMft))
        );
        assert_eq!(rejected, ["candidate lacks usable D3D11 output"]);

        let (selected, _) = select_candidate(vec!["cpu-fallback"], |candidate| {
            if *candidate == "cpu-fallback" {
                Ok(decoder_kind_for_path(false))
            } else {
                Err("unusable".to_owned())
            }
        });
        assert_eq!(
            selected,
            Some((
                "cpu-fallback",
                DecoderKind::WindowsMediaFoundationSynchronousMft
            ))
        );
    }

    #[test]
    fn dxva_readback_bound_accounts_for_padded_rows() {
        assert_eq!(nv12_readback_length(672, 640, 480), Ok(483_840));
        assert!(nv12_readback_length(639, 640, 480).is_err());
        assert!(nv12_readback_length(4096, 1920, 1080).is_err());
    }

    #[test]
    fn dxva_output_requires_matching_bounded_nv12_surface() {
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: 640,
            Height: 480,
            MipLevels: 1,
            ArraySize: 8,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..D3D11_TEXTURE2D_DESC::default()
        };
        assert_eq!(validate_dxgi_output(&desc, 7, 640, 480), Ok(()));
        assert!(validate_dxgi_output(&desc, 8, 640, 480).is_err());
        desc.ArraySize = MAX_DXGI_ARRAY_SLICES + 1;
        assert!(validate_dxgi_output(&desc, 0, 640, 480).is_err());
        desc.ArraySize = 1;
        desc.Width = 1920;
        assert!(validate_dxgi_output(&desc, 0, 640, 480).is_err());
    }

    #[test]
    fn copies_padded_nv12_rows() {
        let s = [
            1, 2, 3, 4, 90, 90, 5, 6, 7, 8, 90, 90, 9, 10, 11, 12, 90, 90, 13, 14, 15, 16, 90, 90,
            17, 18, 19, 20, 90, 90, 21, 22, 23, 24, 90, 90,
        ];
        let (y, uv) = copy_planes(&s, 0, 6, 4, 4).expect("valid padded NV12");
        assert_eq!(y, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(uv, [17, 18, 19, 20, 21, 22, 23, 24]);
    }
    #[test]
    fn rejects_short_stride_and_allocation() {
        assert!(copy_planes(&[0; 24], 0, 3, 4, 4).is_err());
        assert!(copy_planes(&[0; 23], 0, 4, 4, 4).is_err());
    }
    #[test]
    fn respects_scanline_origin() {
        let s = [
            0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
            23, 24,
        ];
        let (y, uv) = copy_planes(&s, 2, 4, 4, 4).expect("valid origin");
        assert_eq!(y, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(uv, [17, 18, 19, 20, 21, 22, 23, 24]);
    }
}
