//! Same-device D3D11 video-processor conversion from capture BGRA to pooled NV12.
//!
//! All pixel conversion and scaling remain on the GPU. The only CPU work is submitting the video
//! processor command and retaining bounded texture leases until Media Foundation emits output.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use racc_capture::windows::WindowsGpuTexture;
use racc_capture::{GpuFrame, PixelFormat};
use windows::core::{IUnknown, Interface};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext,
    ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_VIDEO_ENCODER, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};

use crate::pipeline::{conversion_plan, InputPixelFormat, NV12_SURFACE_POOL_CAPACITY};
use crate::{D3D11Nv12Frame, EncodeError, EncoderConfig};

struct SurfacePool {
    device: ID3D11Device,
    textures: Vec<ID3D11Texture2D>,
    free_slots: Mutex<VecDeque<usize>>,
}

/// A lease on one preallocated NV12 surface. Dropping it returns the slot without waiting.
pub struct Nv12Surface {
    pool: Arc<SurfacePool>,
    slot: usize,
    texture: ID3D11Texture2D,
    width: u32,
    height: u32,
    timestamp_us: u64,
}

impl Nv12Surface {
    /// Output surface width.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Output surface height.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Capture timestamp copied from the source frame.
    pub const fn timestamp_us(&self) -> u64 {
        self.timestamp_us
    }

    /// Produces the borrowed Media Foundation input descriptor while keeping this lease alive.
    pub fn as_encoder_frame(&self) -> D3D11Nv12Frame<'_> {
        D3D11Nv12Frame {
            width: self.width,
            height: self.height,
            timestamp_us: self.timestamp_us,
            texture: &self.texture,
        }
    }

    /// Number of surfaces in the fixed conversion pool.
    pub const fn pool_capacity(&self) -> usize {
        NV12_SURFACE_POOL_CAPACITY
    }

    pub(crate) fn device(&self) -> &ID3D11Device {
        &self.pool.device
    }
}

impl Drop for Nv12Surface {
    fn drop(&mut self) {
        let mut free_slots = self
            .pool
            .free_slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        free_slots.push_back(self.slot);
    }
}

/// GPU converter with a bounded pool tied to the capture texture's D3D11 device.
pub struct WindowsNv12Converter {
    pool: Arc<SurfacePool>,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    source_width: u32,
    source_height: u32,
    config: EncoderConfig,
}

impl WindowsNv12Converter {
    /// Creates a bounded BGRA→NV12 conversion path using the device that owns `first_frame`.
    pub fn new(first_frame: &GpuFrame, config: EncoderConfig) -> Result<Self, EncodeError> {
        let source = capture_texture(first_frame)?;
        let plan = conversion_plan(
            InputPixelFormat::Bgra8,
            first_frame.width,
            first_frame.height,
            config,
        )?;
        validate_bgra_texture(source, first_frame.width, first_frame.height)?;
        // SAFETY: GetDevice returns the live D3D11 device owning the capture texture.
        let device = unsafe { source.texture().GetDevice() }
            .map_err(|error| EncodeError::Backend(format!("get capture D3D11 device: {error}")))?;
        // SAFETY: This obtains the immediate context belonging to the capture device. Multithread
        // protection is enabled below before this shared context is used by the conversion worker.
        let context = unsafe { device.GetImmediateContext() }.map_err(|error| {
            EncodeError::Backend(format!("get capture device context: {error}"))
        })?;
        let multithread: ID3D11Multithread = context.cast().map_err(|error| {
            EncodeError::Backend(format!("query D3D11 multithread protection: {error}"))
        })?;
        // SAFETY: The context is live; enabling D3D11's internal serialization is required because
        // capture and encode may submit work from different threads on the same immediate context.
        let _ = unsafe { multithread.SetMultithreadProtected(BOOL(1)) };

        let video_device: ID3D11VideoDevice = device
            .cast()
            .map_err(|error| EncodeError::Backend(format!("query D3D11 video device: {error}")))?;
        let video_context: ID3D11VideoContext = context
            .cast()
            .map_err(|error| EncodeError::Backend(format!("query D3D11 video context: {error}")))?;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
            InputWidth: plan.source_width,
            InputHeight: plan.source_height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
            OutputWidth: plan.output_width,
            OutputHeight: plan.output_height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: The validated descriptor is nonzero, bounded and copied by D3D11.
        let enumerator =
            unsafe { video_device.CreateVideoProcessorEnumerator(&content) }.map_err(|error| {
                EncodeError::Backend(format!("create D3D11 video processor enumerator: {error}"))
            })?;
        // SAFETY: Enumerator is a live interface created for the content descriptor above.
        let processor =
            unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }.map_err(|error| {
                EncodeError::Backend(format!("create D3D11 video processor: {error}"))
            })?;
        let pool = Arc::new(create_surface_pool(device, config)?);
        Ok(Self {
            pool,
            context,
            video_device,
            video_context,
            enumerator,
            processor,
            source_width: first_frame.width,
            source_height: first_frame.height,
            config,
        })
    }

    /// Converts one capture BGRA frame to NV12 on the same D3D11 device.
    pub fn convert(&mut self, frame: &GpuFrame) -> Result<Nv12Surface, EncodeError> {
        if frame.pixel_format != PixelFormat::Bgra8
            || frame.width != self.source_width
            || frame.height != self.source_height
        {
            return Err(EncodeError::InvalidFrame(
                "capture format or dimensions changed; rebuild the NV12 converter".to_owned(),
            ));
        }
        let source = capture_texture(frame)?;
        validate_bgra_texture(source, frame.width, frame.height)?;
        // SAFETY: GetDevice returns the live owner of the source texture.
        let source_device = unsafe { source.texture().GetDevice() }
            .map_err(|error| EncodeError::Backend(format!("get source D3D11 device: {error}")))?;
        if !same_device(&self.pool.device, &source_device)? {
            return Err(EncodeError::CrossDevice);
        }
        let surface = acquire_surface(self.pool.clone(), self.config, frame.capture_ts_us)?;
        convert_bgra_to_nv12(
            &self.video_device,
            &self.video_context,
            &self.enumerator,
            &self.processor,
            source.texture(),
            &surface.texture,
        )?;
        // Retain the context as an explicit lifetime and make the shared device relationship clear.
        let _ = &self.context;
        Ok(surface)
    }
}

fn capture_texture(frame: &GpuFrame) -> Result<&WindowsGpuTexture, EncodeError> {
    frame
        .texture()
        .as_any()
        .downcast_ref::<WindowsGpuTexture>()
        .ok_or_else(|| {
            EncodeError::InvalidFrame("capture frame is not a Windows D3D11 texture".to_owned())
        })
}

fn validate_bgra_texture(
    source: &WindowsGpuTexture,
    width: u32,
    height: u32,
) -> Result<(), EncodeError> {
    if source.width() != width || source.height() != height || source.cpu_access_flags() != 0 {
        return Err(EncodeError::InvalidFrame(
            "capture texture dimensions or CPU access flags are invalid".to_owned(),
        ));
    }
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `desc` is writable local storage and the capture wrapper owns a live texture.
    unsafe { source.texture().GetDesc(&mut desc) };
    if desc.Width != width
        || desc.Height != height
        || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
        || desc.CPUAccessFlags != 0
        || desc.BindFlags & D3D11_BIND_RENDER_TARGET.0 as u32 == 0
    {
        return Err(EncodeError::InvalidFrame(
            "expected a GPU-resident BGRA render-target texture".to_owned(),
        ));
    }
    Ok(())
}

fn create_surface_pool(
    device: ID3D11Device,
    config: EncoderConfig,
) -> Result<SurfacePool, EncodeError> {
    let mut textures = Vec::with_capacity(NV12_SURFACE_POOL_CAPACITY);
    for _ in 0..NV12_SURFACE_POOL_CAPACITY {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: config.width(),
            Height: config.height(),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: The bounded descriptor is valid and the output slot is writable local storage.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(|error| {
            EncodeError::Backend(format!("allocate NV12 pool surface: {error}"))
        })?;
        textures.push(texture.ok_or_else(|| {
            EncodeError::Backend("D3D11 returned no NV12 pool surface".to_owned())
        })?);
    }
    Ok(SurfacePool {
        device,
        textures,
        free_slots: Mutex::new((0..NV12_SURFACE_POOL_CAPACITY).collect()),
    })
}

fn acquire_surface(
    pool: Arc<SurfacePool>,
    config: EncoderConfig,
    timestamp_us: u64,
) -> Result<Nv12Surface, EncodeError> {
    let slot = pool
        .free_slots
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .pop_front()
        .ok_or(EncodeError::SurfacePoolExhausted)?;
    let texture = pool
        .textures
        .get(slot)
        .cloned()
        .ok_or_else(|| EncodeError::Backend("NV12 pool slot is invalid".to_owned()))?;
    Ok(Nv12Surface {
        pool,
        slot,
        texture,
        width: config.width(),
        height: config.height(),
        timestamp_us,
    })
}

fn convert_bgra_to_nv12(
    video_device: &ID3D11VideoDevice,
    video_context: &ID3D11VideoContext,
    enumerator: &ID3D11VideoProcessorEnumerator,
    processor: &ID3D11VideoProcessor,
    source: &ID3D11Texture2D,
    destination: &ID3D11Texture2D,
) -> Result<(), EncodeError> {
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
    // SAFETY: Both textures were allocated/acquired on this processor's device and use the
    // documented BGRA input and NV12 output bindings. Views select mip zero and array slice zero.
    unsafe {
        video_device
            .CreateVideoProcessorInputView(source, enumerator, &input_desc, Some(&mut input))
            .map_err(|error| EncodeError::Backend(format!("create BGRA input view: {error}")))?;
        video_device
            .CreateVideoProcessorOutputView(
                destination,
                enumerator,
                &output_desc,
                Some(&mut output),
            )
            .map_err(|error| EncodeError::Backend(format!("create NV12 output view: {error}")))?;
    }
    let input =
        input.ok_or_else(|| EncodeError::Backend("D3D11 returned no input view".to_owned()))?;
    let output =
        output.ok_or_else(|| EncodeError::Backend("D3D11 returned no output view".to_owned()))?;
    let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: BOOL(1),
        pInputSurface: std::mem::ManuallyDrop::new(Some(input)),
        ..Default::default()
    };
    // SAFETY: The stream owns the live input view through the synchronous blit; output view and
    // processor are live and the device context has multithread protection enabled.
    let result = unsafe {
        video_context.VideoProcessorBlt(processor, &output, 0, std::slice::from_ref(&stream))
    };
    // SAFETY: Exactly one input-view COM reference was placed in the ManuallyDrop option above.
    unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
    result.map_err(|error| EncodeError::Backend(format!("D3D11 BGRA-to-NV12 blit: {error}")))
}

pub(crate) fn same_device(left: &ID3D11Device, right: &ID3D11Device) -> Result<bool, EncodeError> {
    let left_unknown: IUnknown = left
        .cast()
        .map_err(|error| EncodeError::Backend(format!("query encoder device identity: {error}")))?;
    let right_unknown: IUnknown = right
        .cast()
        .map_err(|error| EncodeError::Backend(format!("query input device identity: {error}")))?;
    Ok(Interface::as_raw(&left_unknown) == Interface::as_raw(&right_unknown))
}
