//! Windows DXGI Desktop Duplication backend.
//!
//! Construction is inert. Output enumeration occurs only from `enumerate_displays`; desktop
//! pixels are acquired only after explicit `start` followed by `poll_event` calls from the caller's
//! capture thread. The crate never launches a service/helper or elevates itself.

mod edid;

use std::collections::{HashMap, VecDeque};
use std::mem::size_of;
use std::sync::Arc;
use std::time::{Duration, Instant};

use racc_topology::{stable_display_id, Display, DisplayFlags, DisplayId, DisplayIdentity};
use windows::core::{Error as WinError, Interface, HRESULT};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{BOOL, E_ACCESSDENIED, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext,
    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutput5,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_MORE_DATA, DXGI_ERROR_SESSION_DISCONNECTED,
    DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTDUPL_POINTER_SHAPE_INFO, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
    DXGI_OUTPUT_DESC,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

use crate::{
    signal_to_event, AccessLostKind, CaptureBackend, CaptureCondition, CaptureError,
    CaptureErrorKind, CaptureEvent, CaptureParams, CaptureRecovery, CapturedDisplay,
    CursorBlendMode, CursorPosition, CursorShape, DamageSummary, DisplayIdentitySource, GpuFrame,
    GpuResource, PixelFormat, RecoveryAction, TexturePool,
};

const MAX_OUTPUTS_PER_ADAPTER: u32 = 32;
const MAX_ADAPTERS: u32 = 16;
const MAX_CURSOR_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_DAMAGE_RECTS: usize = 4096;
const MAX_ACQUIRE_WAIT: Duration = Duration::from_millis(16);

/// Call near process startup, before querying monitor geometry, to request per-monitor DPI v2.
///
/// The app/helper should call this before creating windows. `enumerate_displays` also attempts it
/// as a late safety net, but Windows can reject late awareness changes after HWND creation.
pub fn set_process_dpi_awareness_v2() -> Result<(), CaptureError> {
    // SAFETY: This passes the documented pseudo-handle constant to a Windows API that stores a
    // process-wide DPI-awareness mode; no borrowed pointer or handle lifetime is involved.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .map_err(|error| CaptureError::Platform(format!("set per-monitor DPI awareness: {error}")))
}

/// GPU-resident D3D11 texture allocated with `D3D11_USAGE_DEFAULT` and no CPU access flags.
#[derive(Debug)]
pub struct WindowsGpuTexture {
    id: u64,
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    cpu_access_flags: u32,
}
impl WindowsGpuTexture {
    /// Returns the D3D11 texture reference for encoder interop.
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }
    /// Returns the allocated texture width.
    pub const fn width(&self) -> u32 {
        self.width
    }
    /// Returns the allocated texture height.
    pub const fn height(&self) -> u32 {
        self.height
    }
    /// Returns CPU access flags; pooled streaming surfaces are always zero.
    pub const fn cpu_access_flags(&self) -> u32 {
        self.cpu_access_flags
    }
}
impl GpuResource for WindowsGpuTexture {
    fn resource_id(&self) -> u64 {
        self.id
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct OutputInfo {
    captured: CapturedDisplay,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
}

struct VideoScaler {
    device: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

struct ActiveCapture {
    display_id: DisplayId,
    duplication: IDXGIOutputDuplication,
    context: ID3D11DeviceContext,
    pool: TexturePool,
    width: u32,
    height: u32,
    started_at: Instant,
    last_frame_at: Option<Instant>,
    dropped_queue_frames: u64,
    params: CaptureParams,
    scaler: Option<VideoScaler>,
    origin_x: i32,
    origin_y: i32,
    dirty_rects: Vec<RECT>,
    move_rects: Vec<DXGI_OUTDUPL_MOVE_RECT>,
}

/// Explicitly started DXGI Desktop Duplication backend.
pub struct WindowsCaptureBackend {
    outputs: Vec<OutputInfo>,
    active: Option<ActiveCapture>,
    selected_display: Option<DisplayId>,
    params: CaptureParams,
    recovery: CaptureRecovery,
    retry_at: Option<Instant>,
    pending: VecDeque<CaptureEvent>,
    next_texture_id: u64,
    duplicate_output1: bool,
}
impl Default for WindowsCaptureBackend {
    fn default() -> Self {
        Self::new()
    }
}
impl WindowsCaptureBackend {
    /// Constructs an inert backend. No display query or capture starts here.
    pub fn new() -> Self {
        Self {
            outputs: Vec::new(),
            active: None,
            selected_display: None,
            params: CaptureParams::default(),
            recovery: CaptureRecovery::default(),
            retry_at: None,
            pending: VecDeque::with_capacity(4),
            next_texture_id: 1,
            duplicate_output1: false,
        }
    }

    /// Indicates whether the active output was opened with `DuplicateOutput1`.
    pub const fn used_duplicate_output1(&self) -> bool {
        self.duplicate_output1
    }

    /// Returns cumulative dropped frames for the active capture session.
    pub fn dropped_frames(&self) -> u64 {
        self.active.as_ref().map_or(0, |active| {
            active
                .pool
                .dropped_newest()
                .saturating_add(active.dropped_queue_frames)
        })
    }

    fn find_output(&self, display_id: DisplayId) -> Option<&OutputInfo> {
        self.outputs
            .iter()
            .find(|entry| entry.captured.display.id() == display_id)
    }

    fn open_active(
        &mut self,
        display_id: DisplayId,
        params: CaptureParams,
    ) -> Result<ActiveCapture, CaptureError> {
        let output_info = self
            .find_output(display_id)
            .ok_or(CaptureError::DisplayUnavailable(display_id))?;
        let (source_width, source_height) = output_info.captured.display.size();
        let (width, height) = crate::bounded_capture_dimensions(
            source_width,
            source_height,
            params.max_width,
            params.max_height,
        );
        if width == 0 || height == 0 {
            return Err(CaptureError::Platform(
                "DXGI output has zero dimensions".to_owned(),
            ));
        }
        let adapter = output_info.adapter.clone();
        let output = output_info.output.clone();
        let (origin_x, origin_y) = output_info.captured.display.origin();
        let mut device = None;
        let mut context = None;
        let mut feature_level = D3D_FEATURE_LEVEL_11_0;
        // SAFETY: The adapter is a live COM reference owned by OutputInfo. The output slots are
        // writable locals, the feature-level slice is valid for the call, and returned interfaces
        // are immediately wrapped in windows' reference-counted RAII types.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                None,
                D3D11_CREATE_DEVICE_FLAG(
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT.0 | D3D11_CREATE_DEVICE_VIDEO_SUPPORT.0,
                ),
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
        }
        .map_err(|error| CaptureError::Platform(format!("create D3D11 device: {error}")))?;
        let device =
            device.ok_or_else(|| CaptureError::Platform("D3D11 returned no device".to_owned()))?;
        let context = context.ok_or_else(|| {
            CaptureError::Platform("D3D11 returned no immediate context".to_owned())
        })?;

        let scaler = if (width, height) != (source_width, source_height) {
            Some(create_video_scaler(
                &device,
                &context,
                source_width,
                source_height,
                width,
                height,
            )?)
        } else {
            None
        };
        let duplication = create_duplication(&output, &device)?;
        let mut resources: Vec<Arc<dyn GpuResource>> = Vec::with_capacity(3);
        for _ in 0..3 {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut texture = None;
            // SAFETY: `desc` is fully initialized with nonzero dimensions; the output pointer is a
            // valid local; no initial CPU data is supplied; D3D11 owns the resulting texture.
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(
                |error| CaptureError::Platform(format!("allocate GPU frame texture: {error}")),
            )?;
            let texture = texture
                .ok_or_else(|| CaptureError::Platform("D3D11 returned no texture".to_owned()))?;
            let id = self.next_texture_id;
            self.next_texture_id = self.next_texture_id.wrapping_add(1).max(1);
            resources.push(Arc::new(WindowsGpuTexture {
                id,
                texture,
                width,
                height,
                cpu_access_flags: 0,
            }));
        }
        let pool = TexturePool::new(resources)?;
        self.duplicate_output1 = duplication.1;
        Ok(ActiveCapture {
            display_id,
            duplication: duplication.0,
            context,
            pool,
            width,
            height,
            started_at: Instant::now(),
            last_frame_at: None,
            dropped_queue_frames: 0,
            params,
            scaler,
            origin_x,
            origin_y,
            dirty_rects: vec![RECT::default(); MAX_DAMAGE_RECTS],
            move_rects: vec![DXGI_OUTDUPL_MOVE_RECT::default(); MAX_DAMAGE_RECTS],
        })
    }

    fn fail_native(&mut self, error: WinError) -> Option<CaptureEvent> {
        let condition = classify_hresult(error.code());
        let decision = self.recovery.observe(condition);
        self.active = None;
        match decision.action {
            RecoveryAction::RetryAfter(delay) => self.retry_at = Some(Instant::now() + delay),
            RecoveryAction::Reenumerate => {
                self.selected_display = None;
                self.retry_at = None;
            }
            RecoveryAction::Stop | RecoveryAction::NoFrame => {
                self.selected_display = None;
                self.retry_at = None;
            }
        }
        decision.signal.map(signal_to_event)
    }

    fn try_recover(&mut self) -> Result<Option<CaptureEvent>, CaptureError> {
        let Some(display_id) = self.selected_display else {
            return Ok(None);
        };
        let Some(retry_at) = self.retry_at else {
            return Ok(None);
        };
        if Instant::now() < retry_at {
            return Ok(None);
        }
        self.retry_at = None;
        if let Err(error) = self.enumerate_displays() {
            self.retry_at = Some(Instant::now() + Duration::from_millis(1_000));
            return Err(error);
        }
        if self.find_output(display_id).is_none() {
            self.selected_display = None;
            let _ = self.recovery.observe(CaptureCondition::DisplayRemoved);
            return Ok(Some(CaptureEvent::DisplayLost));
        }
        match self.open_active(display_id, self.params) {
            Ok(active) => {
                self.active = Some(active);
                let signal = self.recovery.recovered().signal;
                Ok(signal.map(signal_to_event))
            }
            Err(_error) => {
                let decision = self.recovery.observe(CaptureCondition::Other);
                let delay = match decision.action {
                    RecoveryAction::RetryAfter(delay) => delay,
                    _ => Duration::from_millis(1_000),
                };
                self.retry_at = Some(Instant::now() + delay);
                Ok(decision.signal.map(signal_to_event))
            }
        }
    }
}

impl CaptureBackend for WindowsCaptureBackend {
    fn enumerate_displays(&mut self) -> Result<Vec<CapturedDisplay>, CaptureError> {
        // SAFETY: The awareness pseudo-handle is a documented constant; late changes can be
        // rejected safely and are reported by the caller's chosen startup policy.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        // SAFETY: CreateDXGIFactory1 returns an owned COM interface; windows wraps its refcount.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
            .map_err(|error| CaptureError::Platform(format!("create DXGI factory: {error}")))?;
        let display_config = active_display_config_metadata();
        let mut found = Vec::new();
        for adapter_index in 0..MAX_ADAPTERS {
            // SAFETY: Enumeration uses a bounded monotonically increasing index and returns an
            // owned COM interface. An error means the adapter list is exhausted.
            let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
                Ok(adapter) => adapter,
                Err(_) => break,
            };
            // SAFETY: Adapter description is written into a value returned by the binding.
            let adapter_desc = unsafe { adapter.GetDesc1() }
                .map_err(|error| CaptureError::Platform(format!("read DXGI adapter: {error}")))?;
            let adapter_name = wide_to_string(&adapter_desc.Description);
            let adapter_id = format!(
                "{:08x}:{:08x}",
                adapter_desc.AdapterLuid.HighPart, adapter_desc.AdapterLuid.LowPart
            );
            for output_index in 0..MAX_OUTPUTS_PER_ADAPTER {
                // SAFETY: Output index is bounded; error means there are no more outputs.
                let output = match unsafe { adapter.EnumOutputs(output_index) } {
                    Ok(output) => output,
                    Err(_) => break,
                };
                // SAFETY: GetDesc returns a fully initialized output description.
                let desc = unsafe { output.GetDesc() }.map_err(|error| {
                    CaptureError::Platform(format!("read DXGI output: {error}"))
                })?;
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                if let Some(info) = build_output_info(
                    adapter.clone(),
                    output,
                    desc,
                    &adapter_name,
                    &adapter_id,
                    &display_config,
                )? {
                    found.push(info);
                }
            }
        }
        self.outputs = found;
        Ok(self
            .outputs
            .iter()
            .map(|entry| entry.captured.clone())
            .collect())
    }

    fn start(&mut self, display_id: DisplayId, params: CaptureParams) -> Result<(), CaptureError> {
        self.stop();
        self.params = params;
        let active = self.open_active(display_id, params)?;
        self.active = Some(active);
        self.selected_display = Some(display_id);
        self.retry_at = None;
        self.recovery.recovered();
        Ok(())
    }

    fn migrate(&mut self, display_id: DisplayId) -> Result<(), CaptureError> {
        if self.active.is_none() {
            return Err(CaptureError::NotStarted);
        }
        // Releasing the previous duplication does not terminate the caller's capture thread.
        self.active = None;
        self.retry_at = None;
        let active = self.open_active(display_id, self.params)?;
        self.active = Some(active);
        self.selected_display = Some(display_id);
        self.recovery.recovered();
        Ok(())
    }

    fn stop(&mut self) {
        self.active = None;
        self.selected_display = None;
        self.retry_at = None;
        self.pending.clear();
    }

    fn poll_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, CaptureError> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        if self.active.is_none() {
            if let Some(event) = self.try_recover()? {
                return Ok(Some(event));
            }
        }
        let Some(active) = self.active.as_mut() else {
            return Ok(None);
        };
        let wait = timeout
            .min(active.params.acquire_timeout)
            .min(MAX_ACQUIRE_WAIT);
        match acquire_copy_and_metadata(active, wait, &mut self.pending) {
            Ok(()) => Ok(self.pending.pop_front()),
            Err(NativeCaptureFailure::Os(error)) => Ok(self.fail_native(error)),
            Err(NativeCaptureFailure::InvalidFrame) => {
                Ok(Some(CaptureEvent::Error(CaptureErrorKind::InvalidFrame)))
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct DisplayConfigMetadata {
    monitor_path: Option<String>,
    friendly_name: Option<String>,
    refresh_mhz: u32,
}

fn build_output_info(
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    desc: DXGI_OUTPUT_DESC,
    adapter_name: &str,
    adapter_id: &str,
    display_config: &HashMap<String, DisplayConfigMetadata>,
) -> Result<Option<OutputInfo>, CaptureError> {
    let x = desc.DesktopCoordinates.left;
    let y = desc.DesktopCoordinates.top;
    let width = desc.DesktopCoordinates.right.saturating_sub(x) as u32;
    let height = desc.DesktopCoordinates.bottom.saturating_sub(y) as u32;
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let gdi_name = wide_to_string(&desc.DeviceName);
    let metadata = display_config.get(&normalize_gdi_name(&gdi_name));
    let (id, identity_source) = match metadata.and_then(|entry| entry.monitor_path.as_deref()) {
        Some(path) if !path.is_empty() => {
            let identity = edid::identity_from_monitor_path(path).ok_or_else(|| {
                CaptureError::Platform("derive display identity from monitor path".to_owned())
            })?;
            let source = if identity.used_edid {
                DisplayIdentitySource::EdidAndConnector
            } else {
                DisplayIdentitySource::ConnectorPathOnly
            };
            (identity.display_id, source)
        }
        _ => {
            let identity = DisplayIdentity::new(*b"UNK", 0, 0, gdi_name.clone(), None);
            let id = stable_display_id(&identity).map_err(|error| {
                CaptureError::Platform(format!("derive display identity: {error}"))
            })?;
            (id, DisplayIdentitySource::GdiDeviceNameFallback)
        }
    };
    let display_name = metadata
        .and_then(|entry| entry.friendly_name.as_deref())
        .filter(|name| !name.is_empty())
        .unwrap_or(&gdi_name);
    let (scale_milli, primary) = monitor_metadata(desc);
    let refresh_mhz = metadata.map_or(0, |entry| entry.refresh_mhz);
    let display = Display::new(
        id,
        display_name,
        x,
        y,
        width,
        height,
        scale_milli,
        refresh_mhz,
        DisplayFlags::new(primary, true, true, false),
    );
    let captured = CapturedDisplay {
        display,
        backend_handle: gdi_name,
        adapter_id: format!("{adapter_id} ({adapter_name})"),
        adapter_model: adapter_name.to_owned(),
        identity_source,
    };
    Ok(Some(OutputInfo {
        captured,
        adapter,
        output,
    }))
}

fn normalize_gdi_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn active_display_config_metadata() -> HashMap<String, DisplayConfigMetadata> {
    const MAX_ACTIVE_PATHS: u32 = 256;
    const MAX_MODE_INFOS: u32 = 4096;
    let mut path_count = 0u32;
    let mut mode_count = 0u32;
    // SAFETY: Both count pointers refer to initialized writable locals, and flags request only
    // active paths. A failed or overlarge query degrades to the documented GDI-name fallback.
    let sizes = unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
    };
    if sizes.is_err()
        || path_count == 0
        || path_count > MAX_ACTIVE_PATHS
        || mode_count > MAX_MODE_INFOS
    {
        return HashMap::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![Default::default(); mode_count as usize];
    // SAFETY: The arrays are initialized and sized to the bounded counts returned above. No
    // topology is changed; this reads the active display configuration only.
    let result = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
    };
    if result.is_err() || path_count as usize > paths.len() {
        return HashMap::new();
    }
    let mut metadata = HashMap::with_capacity(path_count as usize);
    for path in paths.into_iter().take(path_count as usize) {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: The header identifies this active path's source adapter/id; `source` is a
        // correctly sized writable structure and the API only fills its fields.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } < 0 {
            continue;
        }
        let gdi_name = normalize_gdi_name(&wide_to_string(&source.viewGdiDeviceName));
        if gdi_name.is_empty() {
            continue;
        }
        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: The header identifies this active path's target adapter/id; `target` is a
        // correctly sized writable structure and the API only fills its fields.
        // SAFETY: `target.header` identifies this path's live target; `target` is initialized
        // writable storage of the structure size above.
        let target_available = unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } >= 0;
        let monitor_path = target_available
            .then(|| wide_to_string(&target.monitorDevicePath))
            .filter(|path| !path.is_empty());
        let friendly_name = target_available
            .then(|| wide_to_string(&target.monitorFriendlyDeviceName))
            .filter(|name| !name.is_empty());
        let refresh = path.targetInfo.refreshRate;
        let refresh_mhz = if refresh.Denominator == 0 {
            0
        } else {
            ((u64::from(refresh.Numerator) * 1000) / u64::from(refresh.Denominator))
                .min(u64::from(u32::MAX)) as u32
        };
        metadata.insert(
            gdi_name,
            DisplayConfigMetadata {
                monitor_path,
                friendly_name,
                refresh_mhz,
            },
        );
    }
    metadata
}

fn monitor_metadata(desc: DXGI_OUTPUT_DESC) -> (u16, bool) {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: Monitor is supplied by a live DXGI output description; `info` is correctly sized
    // writable storage for MONITORINFO.
    let got_info = unsafe { GetMonitorInfoW(desc.Monitor, &mut info) }.as_bool();
    let primary = got_info && info.dwFlags & MONITORINFOF_PRIMARY != 0;
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    // SAFETY: The DXGI output owns the monitor handle for the duration of this query, and both
    // output pointers are valid locals. Failure falls back to the documented 100% metadata value.
    if unsafe { GetDpiForMonitor(desc.Monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_err()
    {
        dpi_x = 96;
    }
    let scale = ((u64::from(dpi_x.max(1)) * 1000 + 48) / 96).min(u64::from(u16::MAX)) as u16;
    (scale, primary)
}

fn create_video_scaler(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
) -> Result<VideoScaler, CaptureError> {
    let video_device: ID3D11VideoDevice = device
        .cast()
        .map_err(|error| CaptureError::Platform(format!("query D3D11 video device: {error}")))?;
    let video_context: ID3D11VideoContext = context
        .cast()
        .map_err(|error| CaptureError::Platform(format!("query D3D11 video context: {error}")))?;
    let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        },
        InputWidth: source_width,
        InputHeight: source_height,
        OutputFrameRate: DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        },
        OutputWidth: output_width,
        OutputHeight: output_height,
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    // SAFETY: The content descriptor has nonzero bounded dimensions and specifies progressive
    // 30 fps input/output; D3D11 copies the descriptor during object creation.
    let enumerator =
        unsafe { video_device.CreateVideoProcessorEnumerator(&content) }.map_err(|error| {
            CaptureError::Platform(format!("create D3D11 video processor enumerator: {error}"))
        })?;
    // SAFETY: The enumerator is a live interface returned for the content descriptor above.
    let processor =
        unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }.map_err(|error| {
            CaptureError::Platform(format!("create D3D11 video processor: {error}"))
        })?;
    Ok(VideoScaler {
        device: video_device,
        context: video_context,
        enumerator,
        processor,
    })
}

fn scale_texture(
    scaler: &VideoScaler,
    source: &ID3D11Texture2D,
    destination: &ID3D11Texture2D,
) -> Result<(), WinError> {
    let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPIV {
                MipSlice: 0,
                ArraySlice: 0,
            },
        },
    };
    let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };
    let mut input = None;
    let mut output = None;
    // SAFETY: The source and destination textures were created/acquired on this device. The views
    // select mip zero and array slice zero and are used only for this synchronous processor call.
    unsafe {
        scaler.device.CreateVideoProcessorInputView(
            source,
            &scaler.enumerator,
            &input_desc,
            Some(&mut input),
        )?;
        scaler.device.CreateVideoProcessorOutputView(
            destination,
            &scaler.enumerator,
            &output_desc,
            Some(&mut output),
        )?;
    }
    let input = input.ok_or_else(|| WinError::from(E_ACCESSDENIED))?;
    let output = output.ok_or_else(|| WinError::from(E_ACCESSDENIED))?;
    let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: BOOL(1),
        pInputSurface: std::mem::ManuallyDrop::new(Some(input)),
        ..Default::default()
    };
    // SAFETY: Stream points to the live input view and has no past/future surfaces. The single
    // output view is live until the processor call completes.
    let result = unsafe {
        scaler.context.VideoProcessorBlt(
            &scaler.processor,
            &output,
            0,
            std::slice::from_ref(&stream),
        )
    };
    // D3D11_VIDEO_PROCESSOR_STREAM intentionally uses ManuallyDrop for COM pointers; release the
    // cloned input view after the synchronous VideoProcessorBlt call.
    // SAFETY: The stream's pInputSurface was initialized above with exactly one owned Option.
    unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
    result
}

fn create_duplication(
    output: &IDXGIOutput,
    device: &ID3D11Device,
) -> Result<(IDXGIOutputDuplication, bool), CaptureError> {
    let formats = [DXGI_FORMAT_B8G8R8A8_UNORM];
    // SAFETY: Interface casts query COM for supported interfaces, and `device` is a live D3D11
    // device created for this output's adapter. DuplicateOutput1 uses one initialized format.
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        if let Ok(duplication) = unsafe { output5.DuplicateOutput1(device, 0, &formats) } {
            return Ok((duplication, true));
        }
    }
    // SAFETY: The same live output/device pair is passed through DXGI's documented fallback API.
    let output1: IDXGIOutput1 = output
        .cast()
        .map_err(|error| CaptureError::Platform(format!("output lacks IDXGIOutput1: {error}")))?;
    // SAFETY: `device` belongs to the same adapter as `output`; returned duplication is RAII-owned.
    unsafe { output1.DuplicateOutput(device) }
        .map(|duplication| (duplication, false))
        .map_err(|error| CaptureError::Platform(format!("create desktop duplication: {error}")))
}

enum NativeCaptureFailure {
    Os(WinError),
    InvalidFrame,
}

fn acquire_copy_and_metadata(
    active: &mut ActiveCapture,
    timeout: Duration,
    pending: &mut VecDeque<CaptureEvent>,
) -> Result<(), NativeCaptureFailure> {
    let timeout_ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut desktop_resource: Option<IDXGIResource> = None;
    // SAFETY: Both output pointers refer to initialized local storage; timeout is capped at 16 ms.
    let acquire_started = Instant::now();
    let acquired = unsafe {
        active
            .duplication
            .AcquireNextFrame(timeout_ms, &mut info, &mut desktop_resource)
    };
    if let Err(error) = acquired {
        if error.code() == DXGI_ERROR_WAIT_TIMEOUT {
            return Ok(());
        }
        return Err(NativeCaptureFailure::Os(error));
    }
    let acquired_at = Instant::now();
    let acquire_wait_us = duration_micros(acquired_at.saturating_duration_since(acquire_started));
    let release = AcquiredFrame {
        duplication: active.duplication.clone(),
    };
    if info.ProtectedContentMaskedOut.as_bool() {
        queue_pending(
            pending,
            CaptureEvent::AccessLost(AccessLostKind::ProtectedContent),
        );
        drop(release);
        return Ok(());
    }
    if info.PointerShapeBufferSize > 0 {
        if let Some(shape) = read_cursor_shape(&active.duplication, info.PointerShapeBufferSize)? {
            queue_pending(pending, CaptureEvent::CursorShape(shape));
        }
    }
    if info.LastMouseUpdateTime != 0 {
        queue_pending(
            pending,
            CaptureEvent::CursorMoved(CursorPosition {
                x: map_output_relative_coordinate(info.PointerPosition.Position.x, active.origin_x),
                y: map_output_relative_coordinate(info.PointerPosition.Position.y, active.origin_y),
                visible: info.PointerPosition.Visible.as_bool(),
            }),
        );
    }
    if info.LastPresentTime != 0 {
        let Some(lease) = active.pool.try_acquire() else {
            drop(release);
            return Ok(());
        };
        let resource = desktop_resource.ok_or(NativeCaptureFailure::InvalidFrame)?;
        // SAFETY: The acquired DXGI resource is owned for this frame and cast only to the D3D11
        // texture interface supplied by Desktop Duplication. The pool texture has matching dimensions.
        let source: ID3D11Texture2D = resource.cast().map_err(NativeCaptureFailure::Os)?;
        let destination = lease
            .resource()
            .as_any()
            .downcast_ref::<WindowsGpuTexture>()
            .ok_or(NativeCaptureFailure::InvalidFrame)?;
        let copy_scale_started = Instant::now();
        if let Some(scaler) = active.scaler.as_ref() {
            scale_texture(scaler, &source, &destination.texture)
                .map_err(NativeCaptureFailure::Os)?;
        } else {
            // SAFETY: Both resources are live D3D11 textures from the same adapter/device and have
            // identical dimensions/format; CopyResource is asynchronous and performs no CPU mapping.
            unsafe { active.context.CopyResource(&destination.texture, &source) };
        }
        let copy_scale_submit_us = duration_micros(copy_scale_started.elapsed());
        let frame_interval_us = active.last_frame_at.map_or(0, |previous| {
            duration_micros(acquired_at.saturating_duration_since(previous))
        });
        let damage = read_damage(active)?;
        let frame = GpuFrame {
            lease,
            display_id: active.display_id,
            width: active.width,
            height: active.height,
            pixel_format: PixelFormat::Bgra8,
            capture_ts_us: duration_micros(
                acquired_at.saturating_duration_since(active.started_at),
            ),
            damage,
            acquire_wait_us,
            copy_scale_submit_us,
            frame_interval_us,
            dropped_frames: active
                .pool
                .dropped_newest()
                .saturating_add(active.dropped_queue_frames),
        };
        if queue_pending(pending, CaptureEvent::Frame(frame)) {
            active.last_frame_at = Some(acquired_at);
        } else {
            active.dropped_queue_frames = active.dropped_queue_frames.saturating_add(1);
        }
    }
    drop(release);
    Ok(())
}

fn queue_pending(pending: &mut VecDeque<CaptureEvent>, event: CaptureEvent) -> bool {
    // `poll_event` drains before acquisition, and this queue is preallocated for the maximum of
    // shape, position, and frame events from one acquire. Avoid growing it on the frame path.
    if pending.len() < pending.capacity() {
        pending.push_back(event);
        true
    } else {
        false
    }
}

struct AcquiredFrame {
    duplication: IDXGIOutputDuplication,
}
impl Drop for AcquiredFrame {
    fn drop(&mut self) {
        // SAFETY: This guard is created only after AcquireNextFrame succeeds and is dropped once;
        // DXGI requires ReleaseFrame before the next acquisition.
        let _ = unsafe { self.duplication.ReleaseFrame() };
    }
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn map_output_relative_coordinate(relative: i32, origin: i32) -> i32 {
    origin.saturating_add(relative)
}

fn read_damage(active: &mut ActiveCapture) -> Result<DamageSummary, NativeCaptureFailure> {
    let mut dirty_bytes = (active.dirty_rects.len() * size_of::<RECT>()) as u32;
    // SAFETY: The fixed initialized RECT buffer is bounded to 4096 entries, and byte capacity
    // matches its exact allocation. DXGI writes no more than the declared capacity.
    let dirty = unsafe {
        active.duplication.GetFrameDirtyRects(
            dirty_bytes,
            active.dirty_rects.as_mut_ptr(),
            &mut dirty_bytes,
        )
    };
    if let Err(error) = dirty {
        if error.code() == DXGI_ERROR_MORE_DATA {
            return Ok(DamageSummary::Full);
        }
        return Err(NativeCaptureFailure::Os(error));
    }
    let mut moved_bytes = (active.move_rects.len() * size_of::<DXGI_OUTDUPL_MOVE_RECT>()) as u32;
    // SAFETY: The fixed initialized move-rect buffer is bounded to 4096 entries, and byte capacity
    // matches its allocation; DXGI reports overflow instead of writing beyond it.
    let moved = unsafe {
        active.duplication.GetFrameMoveRects(
            moved_bytes,
            active.move_rects.as_mut_ptr(),
            &mut moved_bytes,
        )
    };
    if let Err(error) = moved {
        if error.code() == DXGI_ERROR_MORE_DATA {
            return Ok(DamageSummary::Full);
        }
        return Err(NativeCaptureFailure::Os(error));
    }
    Ok(DamageSummary::Rectangles {
        dirty: (dirty_bytes as usize / size_of::<RECT>()).min(usize::from(u16::MAX)) as u16,
        moved: (moved_bytes as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>())
            .min(usize::from(u16::MAX)) as u16,
    })
}

fn read_cursor_shape(
    duplication: &IDXGIOutputDuplication,
    requested: u32,
) -> Result<Option<CursorShape>, NativeCaptureFailure> {
    let length = (requested as usize).min(MAX_CURSOR_SOURCE_BYTES);
    if requested as usize > MAX_CURSOR_SOURCE_BYTES {
        return Ok(None);
    }
    let mut buffer = vec![0u8; length];
    let mut required = 0u32;
    let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
    // SAFETY: The pointer buffer is initialized, bounded to 1 MiB, and its exact byte length is
    // supplied. The result struct is writable local storage.
    unsafe {
        duplication.GetFramePointerShape(
            length as u32,
            buffer.as_mut_ptr().cast(),
            &mut required,
            &mut info,
        )
    }
    .map_err(NativeCaptureFailure::Os)?;
    let width = info.Width;
    let source_height = info.Height;
    if width == 0 || source_height == 0 || required as usize > buffer.len() {
        return Ok(None);
    }
    let (height, mode) = if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32 {
        if source_height % 2 != 0 {
            return Ok(None);
        }
        (source_height / 2, CursorBlendMode::WindowsAndXor)
    } else if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32 {
        (source_height, CursorBlendMode::WindowsMaskedColor)
    } else if info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 as u32 {
        (source_height, CursorBlendMode::PremultipliedAlpha)
    } else {
        return Ok(None);
    };
    let Some(output_len) = cursor_bgra_output_len(width, height) else {
        return Ok(None);
    };
    let pitch = info.Pitch as usize;
    let bgra = if mode == CursorBlendMode::WindowsAndXor {
        if (required as usize) < pitch.saturating_mul(source_height as usize) {
            return Ok(None);
        }
        let Some(pixels) = monochrome_to_bgra(&buffer, width, height, pitch) else {
            return Ok(None);
        };
        pixels
    } else {
        let row_bytes = width as usize * 4;
        if pitch < row_bytes || (required as usize) < pitch.saturating_mul(height as usize) {
            return Ok(None);
        }
        let mut pixels = vec![0u8; output_len];
        for y in 0..height as usize {
            let source = &buffer[y * pitch..y * pitch + row_bytes];
            let destination = &mut pixels[y * row_bytes..(y + 1) * row_bytes];
            destination.copy_from_slice(source);
        }
        pixels
    };
    let hotspot_x = (info.HotSpot.x.max(0) as u32).min(width.saturating_sub(1));
    let hotspot_y = (info.HotSpot.y.max(0) as u32).min(height.saturating_sub(1));
    Ok(Some(CursorShape {
        width,
        height,
        hotspot_x,
        hotspot_y,
        bgra8: bgra.into(),
        blend_mode: mode,
    }))
}

fn cursor_bgra_output_len(width: u32, height: u32) -> Option<usize> {
    let output_len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    (output_len <= MAX_CURSOR_SOURCE_BYTES).then_some(output_len)
}
fn monochrome_to_bgra(source: &[u8], width: u32, height: u32, pitch: usize) -> Option<Vec<u8>> {
    let output_len = cursor_bgra_output_len(width, height)?;
    if width == 0
        || height == 0
        || pitch < (width as usize).div_ceil(8)
        || source.len() < pitch.checked_mul(height as usize)?.checked_mul(2)?
    {
        return None;
    }

    let mut bgra = vec![0u8; output_len];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let mask = 0x80 >> (x % 8);
            let and_bit = source[y * pitch + x / 8] & mask != 0;
            let xor_bit = source[(y + height as usize) * pitch + x / 8] & mask != 0;
            let xor = if xor_bit { 255 } else { 0 };
            let offset = (y * width as usize + x) * 4;
            bgra[offset..offset + 4].copy_from_slice(&[
                xor,
                xor,
                xor,
                if and_bit { 255 } else { 0 },
            ]);
        }
    }
    Some(bgra)
}

fn classify_hresult(code: HRESULT) -> CaptureCondition {
    if code == DXGI_ERROR_WAIT_TIMEOUT {
        CaptureCondition::WaitTimeout
    } else if code == DXGI_ERROR_ACCESS_LOST {
        CaptureCondition::DxgiAccessLost
    } else if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
        CaptureCondition::DeviceLost
    } else if code == DXGI_ERROR_UNSUPPORTED {
        CaptureCondition::Unsupported
    } else if code == DXGI_ERROR_SESSION_DISCONNECTED {
        CaptureCondition::SessionDisconnected
    } else if code == E_ACCESSDENIED {
        CaptureCondition::AccessDenied
    } else {
        CaptureCondition::Other
    }
}

fn wide_to_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

/// Explicitly documented placeholder for a future WGC capture implementation.
///
/// M5a keeps DXGI Desktop Duplication as the primary path. Windows.Graphics.Capture is deferred;
/// it may be considered if a hybrid-GPU configuration cannot create duplication on its display
/// adapter. WGC's secure-desktop limitation would remain.
pub struct WindowsGraphicsCaptureFallback;

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::HRESULT;

    #[test]
    fn hresult_mapping_covers_dxgi_access_device_timeout_and_access_denied() {
        assert_eq!(
            classify_hresult(DXGI_ERROR_ACCESS_LOST),
            CaptureCondition::DxgiAccessLost
        );
        assert_eq!(
            classify_hresult(DXGI_ERROR_DEVICE_REMOVED),
            CaptureCondition::DeviceLost
        );
        assert_eq!(
            classify_hresult(DXGI_ERROR_DEVICE_RESET),
            CaptureCondition::DeviceLost
        );
        assert_eq!(
            classify_hresult(DXGI_ERROR_WAIT_TIMEOUT),
            CaptureCondition::WaitTimeout
        );
        assert_eq!(
            classify_hresult(E_ACCESSDENIED),
            CaptureCondition::AccessDenied
        );
        assert_eq!(
            classify_hresult(DXGI_ERROR_SESSION_DISCONNECTED),
            CaptureCondition::SessionDisconnected
        );
        assert_eq!(
            classify_hresult(DXGI_ERROR_UNSUPPORTED),
            CaptureCondition::Unsupported
        );
        assert_eq!(
            classify_hresult(HRESULT(0x80004005u32 as i32)),
            CaptureCondition::Other
        );
    }

    #[test]
    fn cursor_coordinates_add_negative_virtual_desktop_origin_safely() {
        assert_eq!(map_output_relative_coordinate(11, -1920), -1909);
        assert_eq!(map_output_relative_coordinate(i32::MAX, 20), i32::MAX);
        assert_eq!(map_output_relative_coordinate(i32::MIN, -20), i32::MIN);
    }

    #[test]
    fn monochrome_shape_conversion_keeps_and_xor_planes_and_rejects_truncation() {
        let source = [0b1000_0000, 0b0100_0000];
        let bgra = monochrome_to_bgra(&source, 2, 1, 1).expect("well-shaped mono planes");
        assert_eq!(bgra.len(), 8);
        assert_eq!(&bgra[..4], &[0, 0, 0, 255]);
        assert_eq!(&bgra[4..], &[255, 255, 255, 0]);
        assert!(monochrome_to_bgra(&source[..1], 2, 1, 1).is_none());
        assert!(monochrome_to_bgra(&[], u32::MAX, u32::MAX, usize::MAX).is_none());
    }

    #[test]
    fn cursor_shape_output_size_is_checked_before_allocation() {
        assert_eq!(cursor_bgra_output_len(1, 1), Some(4));
        assert_eq!(
            cursor_bgra_output_len(512, 512),
            Some(MAX_CURSOR_SOURCE_BYTES)
        );
        assert_eq!(cursor_bgra_output_len(513, 512), None);
        assert_eq!(cursor_bgra_output_len(u32::MAX, u32::MAX), None);
    }
}
