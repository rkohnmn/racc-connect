//! Bounded D3D11 NV12 readback for the Windows OpenH264 software fallback.
//!
//! This module copies plane bytes into reusable I420 buffers. It does no color conversion.

use std::slice;

use windows::core::Interface;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

use racc_capture::windows::WindowsGpuTexture;
use racc_capture::GpuFrame;

use crate::windows_video_processor::same_device;
use crate::{D3D11Nv12Frame, EncodeError, EncoderConfig, I420Frame, MAX_I420_BYTES};

const MAX_STAGING_BYTES: usize = MAX_I420_BYTES * 2;

/// Serialized, fixed-buffer NV12 readback into OpenH264's planar I420 layout.
///
/// Create one instance per software encoder. Its staging texture and Y/U/V buffers are reused for
/// every call. `with_i420` holds exclusive access to those buffers until the synchronous closure
/// returns, so a caller cannot observe or overwrite a plane while encoding it.
pub struct WindowsI420Readback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: ID3D11Texture2D,
    config: EncoderConfig,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl WindowsI420Readback {
    /// Creates the readback bridge on the same device as a captured D3D11 frame.
    pub fn new_for_capture(frame: &GpuFrame, config: EncoderConfig) -> Result<Self, EncodeError> {
        let wrapper = frame
            .texture()
            .as_any()
            .downcast_ref::<WindowsGpuTexture>()
            .ok_or_else(|| {
                EncodeError::InvalidFrame("capture frame is not a Windows D3D11 texture".to_owned())
            })?;
        // SAFETY: The capture wrapper retains this live texture; GetDevice returns its owner.
        let device = unsafe { wrapper.texture().GetDevice() }
            .map_err(|error| EncodeError::Backend(format!("get capture D3D11 device: {error}")))?;
        Self::new(&device, config)
    }

    /// Creates one bounded NV12 staging texture and fixed-size I420 plane buffers.
    pub fn new(device: &ID3D11Device, config: EncoderConfig) -> Result<Self, EncodeError> {
        let width = config.width() as usize;
        let height = config.height() as usize;
        let y_len = width
            .checked_mul(height)
            .ok_or_else(|| EncodeError::InvalidConfig("I420 frame size overflow".to_owned()))?;
        let chroma_len = y_len / 4;
        // SAFETY: `device` is live and returns an owned reference to its immediate context.
        let context = unsafe { device.GetImmediateContext() }.map_err(|error| {
            EncodeError::Backend(format!("get D3D11 readback context: {error}"))
        })?;
        let multithread: ID3D11Multithread = context.cast().map_err(|error| {
            EncodeError::Backend(format!("query D3D11 multithread protection: {error}"))
        })?;
        // SAFETY: This is the live immediate context from `device`; D3D11's internal lock
        // serializes readback commands with capture and conversion work sharing the context.
        let _ = unsafe { multithread.SetMultithreadProtected(BOOL(1)) };

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
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        // SAFETY: The validated config supplies nonzero even dimensions within 1080p; the
        // staging descriptor has the required NV12 layout and read-only CPU access.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(|error| {
            EncodeError::Backend(format!("create NV12 readback staging texture: {error}"))
        })?;
        let staging = staging.ok_or_else(|| {
            EncodeError::Backend("D3D11 returned no NV12 staging texture".to_owned())
        })?;

        Ok(Self {
            device: device.clone(),
            context,
            staging,
            config,
            y: vec![0; y_len],
            u: vec![0; chroma_len],
            v: vec![0; chroma_len],
        })
    }

    /// Reads one same-device NV12 frame and synchronously exposes reusable I420 planes.
    ///
    /// The callback must finish all use of the borrowed frame before returning. Its return value
    /// cannot borrow the scratch planes, which prevents a later call from invalidating it.
    pub fn with_i420<R>(
        &mut self,
        frame: D3D11Nv12Frame<'_>,
        encode: impl FnOnce(&I420Frame<'_>) -> R,
    ) -> Result<R, EncodeError> {
        validate_input_texture(frame, self.config)?;
        // SAFETY: `frame.texture` is a live D3D11 texture; GetDevice returns the device that owns
        // it, which is compared by COM identity before CopyResource is submitted.
        let frame_device = unsafe { frame.texture.GetDevice() }
            .map_err(|error| EncodeError::Backend(format!("get NV12 texture device: {error}")))?;
        if !same_device(&self.device, &frame_device)? {
            return Err(EncodeError::CrossDevice);
        }

        // SAFETY: Both textures are validated, single-sample NV12 2D resources of identical
        // dimensions on the same D3D11 device. The destination is the private staging surface.
        unsafe { self.context.CopyResource(&self.staging, frame.texture) };
        // SAFETY: The staging texture was created with CPU read access, and exclusive `&mut self`
        // ensures this one staging surface is never mapped concurrently by this bridge.
        let mut mapped_data = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped_data))
        }
        .map_err(|error| EncodeError::Backend(format!("map NV12 staging texture: {error}")))?;
        let mapped = MappedNv12::new(&self.context, &self.staging, mapped_data);
        if mapped.data.pData.is_null() {
            return Err(EncodeError::InvalidFrame(
                "D3D11 returned a null NV12 readback pointer".to_owned(),
            ));
        }
        let row_pitch = mapped.data.RowPitch as usize;
        let width = frame.width as usize;
        let height = frame.height as usize;
        let mapped_len = validate_nv12_layout(width, height, row_pitch)?;
        // SAFETY: Map returned a live pointer for the validated single-sample NV12 staging
        // resource. `mapped_len` is the checked row-pitch footprint of its Y and UV planes, and
        // `MappedNv12` keeps the resource mapped until this borrow is no longer used.
        let source = unsafe { slice::from_raw_parts(mapped.data.pData.cast::<u8>(), mapped_len) };
        let copy_result = copy_nv12_to_i420(
            source,
            width,
            height,
            row_pitch,
            &mut self.y,
            &mut self.u,
            &mut self.v,
        );
        drop(mapped);
        copy_result?;

        let i420 = I420Frame::new(
            frame.width,
            frame.height,
            frame.timestamp_us,
            &self.y,
            &self.u,
            &self.v,
        )?;
        Ok(encode(&i420))
    }
}

struct MappedNv12<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
    data: D3D11_MAPPED_SUBRESOURCE,
}

impl<'a> MappedNv12<'a> {
    fn new(
        context: &'a ID3D11DeviceContext,
        texture: &'a ID3D11Texture2D,
        data: D3D11_MAPPED_SUBRESOURCE,
    ) -> Self {
        Self {
            context,
            texture,
            data,
        }
    }
}

impl Drop for MappedNv12<'_> {
    fn drop(&mut self) {
        // SAFETY: This guard is constructed only after a successful Map of subresource zero and
        // is dropped exactly once on every return path.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

fn validate_input_texture(
    frame: D3D11Nv12Frame<'_>,
    config: EncoderConfig,
) -> Result<(), EncodeError> {
    if frame.width != config.width() || frame.height != config.height() {
        return Err(EncodeError::InvalidFrame(
            "D3D11 NV12 dimensions do not match readback configuration".to_owned(),
        ));
    }
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `desc` is writable local storage and the frame contains a live D3D11 texture.
    unsafe { frame.texture.GetDesc(&mut desc) };
    if desc.Width != frame.width
        || desc.Height != frame.height
        || desc.Format != DXGI_FORMAT_NV12
        || desc.MipLevels != 1
        || desc.ArraySize != 1
        || desc.SampleDesc.Count != 1
        || desc.Usage != D3D11_USAGE_DEFAULT
        || desc.CPUAccessFlags != 0
    {
        return Err(EncodeError::InvalidFrame(
            "expected a GPU-resident, single-sample NV12 texture".to_owned(),
        ));
    }
    Ok(())
}

fn validate_nv12_layout(
    width: usize,
    height: usize,
    row_pitch: usize,
) -> Result<usize, EncodeError> {
    if width < 2 || height < 2 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(EncodeError::InvalidFrame(
            "NV12 dimensions must be even and at least two pixels".to_owned(),
        ));
    }
    if width > crate::MAX_WIDTH as usize || height > crate::MAX_HEIGHT as usize {
        return Err(EncodeError::InvalidFrame(
            "NV12 dimensions exceed the 1080p bound".to_owned(),
        ));
    }
    if row_pitch < width {
        return Err(EncodeError::InvalidFrame(
            "D3D11 NV12 row pitch is shorter than the visible row".to_owned(),
        ));
    }
    let rows = height
        .checked_add(height / 2)
        .ok_or_else(|| EncodeError::InvalidFrame("NV12 row count overflow".to_owned()))?;
    let required_len = row_pitch
        .checked_mul(rows)
        .ok_or_else(|| EncodeError::InvalidFrame("NV12 readback size overflow".to_owned()))?;
    if required_len > MAX_STAGING_BYTES {
        return Err(EncodeError::InvalidFrame(
            "D3D11 NV12 readback exceeds the staging byte bound".to_owned(),
        ));
    }
    Ok(required_len)
}

fn copy_nv12_to_i420(
    source: &[u8],
    width: usize,
    height: usize,
    row_pitch: usize,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) -> Result<(), EncodeError> {
    let required_len = validate_nv12_layout(width, height, row_pitch)?;
    let y_len = width
        .checked_mul(height)
        .ok_or_else(|| EncodeError::InvalidFrame("I420 luma size overflow".to_owned()))?;
    let chroma_len = y_len / 4;
    if source.len() < required_len
        || y.len() != y_len
        || u.len() != chroma_len
        || v.len() != chroma_len
    {
        return Err(EncodeError::InvalidFrame(
            "NV12 source or I420 destination planes have invalid bounds".to_owned(),
        ));
    }

    for row in 0..height {
        let source_start = row * row_pitch;
        let destination_start = row * width;
        y[destination_start..destination_start + width]
            .copy_from_slice(&source[source_start..source_start + width]);
    }

    let uv_start = row_pitch * height;
    let chroma_width = width / 2;
    for row in 0..height / 2 {
        let source_row = uv_start + row * row_pitch;
        let destination_row = row * chroma_width;
        for column in 0..chroma_width {
            let pair = source_row + column * 2;
            u[destination_row + column] = source[pair];
            v[destination_row + column] = source[pair + 1];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{copy_nv12_to_i420, validate_nv12_layout};

    #[test]
    fn copies_padded_nv12_rows_and_deinterleaves_uv_in_plane_order() {
        let source = [
            1, 2, 3, 4, 90, 91, // Y row 0, with padding
            5, 6, 7, 8, 92, 93, // Y row 1, with padding
            9, 10, 11, 12, 94, 95, // Y row 2, with padding
            13, 14, 15, 16, 96, 97, // Y row 3, with padding
            17, 18, 19, 20, 98, 99, // UV row 0, with padding
            21, 22, 23, 24, 100, 101, // UV row 1, with padding
        ];
        let mut y = [0; 16];
        let mut u = [0; 4];
        let mut v = [0; 4];

        copy_nv12_to_i420(&source, 4, 4, 6, &mut y, &mut u, &mut v)
            .expect("valid padded NV12 frame");

        assert_eq!(y, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(u, [17, 19, 21, 23]);
        assert_eq!(v, [18, 20, 22, 24]);
    }

    #[test]
    fn rejects_invalid_dimensions_pitch_and_buffer_bounds() {
        assert!(validate_nv12_layout(3, 2, 4).is_err());
        assert!(validate_nv12_layout(4, 2, 3).is_err());
        assert!(validate_nv12_layout(usize::MAX, 2, usize::MAX).is_err());
        assert!(validate_nv12_layout(4, 2, usize::MAX).is_err());

        let source = [0; 12];
        let mut y = [0; 8];
        let mut u = [0; 2];
        let mut v = [0; 2];
        assert!(copy_nv12_to_i420(&source[..11], 4, 2, 4, &mut y, &mut u, &mut v).is_err());
        assert!(copy_nv12_to_i420(&source, 4, 2, 4, &mut y[..7], &mut u, &mut v).is_err());
    }
}
