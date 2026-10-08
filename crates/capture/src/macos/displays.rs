//! CoreGraphics display enumeration and stable topology metadata for macOS.
#![allow(unsafe_code)]

use std::{
    ffi::{c_char, c_void, CStr},
    fmt,
};

use racc_topology::{
    assign_display_ids, Display, DisplayFlags, DisplayIdentity, HostDisplayGeometry, IdentityError,
};

use crate::{CapturedDisplay, DisplayIdentitySource};

const MAX_ACTIVE_DISPLAYS: usize = 16;
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// One active macOS display with its wire-pixel topology and OS logical point geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct MacDisplay {
    /// Native CoreGraphics display identifier used only by the local capture adapter.
    pub native_display_id: u32,
    /// Cross-platform display metadata in physical pixels.
    pub captured: CapturedDisplay,
    /// Logical rectangle reported by CoreGraphics, retained in points for input injection.
    pub points: HostDisplayGeometry,
}

/// Error returned while enumerating active macOS displays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MacDisplayError {
    /// CoreGraphics failed to enumerate or return valid geometry.
    CoreGraphics(i32),
    /// The active display list exceeded the fixed supported bound.
    TooManyDisplays,
    /// The OS returned invalid geometry or an invalid UUID.
    InvalidDisplay,
    /// Stable display identities could not be assigned.
    Identity(IdentityError),
}

impl fmt::Display for MacDisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoreGraphics(status) => write!(f, "CoreGraphics display query failed ({status})"),
            Self::TooManyDisplays => {
                f.write_str("active display count exceeds the supported limit")
            }
            Self::InvalidDisplay => {
                f.write_str("CoreGraphics returned invalid display geometry or identity")
            }
            Self::Identity(error) => write!(f, "display identity failed: {error}"),
        }
    }
}

impl std::error::Error for MacDisplayError {}

/// Enumerates current active displays without starting capture or reading pixels.
pub fn enumerate_displays() -> Result<Vec<MacDisplay>, MacDisplayError> {
    let mut ids = [0u32; MAX_ACTIVE_DISPLAYS];
    let mut count = 0u32;
    // SAFETY: `ids` is a writable fixed-size array, `count` is a valid output pointer, and the
    // supplied bound exactly matches the allocated array. No display content is accessed.
    let status =
        unsafe { CGGetActiveDisplayList(MAX_ACTIVE_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    if status != 0 {
        return Err(MacDisplayError::CoreGraphics(status));
    }
    let count = usize::try_from(count).map_err(|_| MacDisplayError::TooManyDisplays)?;
    if count > ids.len() {
        return Err(MacDisplayError::TooManyDisplays);
    }

    let mut raw = Vec::with_capacity(count);
    let mut identities = Vec::with_capacity(count);
    for (ordinal, id) in ids[..count].iter().copied().enumerate() {
        let record = query_display(id, ordinal)?;
        identities.push(record.identity.clone());
        raw.push(record);
    }
    let ids = assign_display_ids(&identities).map_err(MacDisplayError::Identity)?;
    let mut result = Vec::with_capacity(raw.len());
    for (index, record) in raw.into_iter().enumerate() {
        let display_id = *ids.get(index).ok_or(MacDisplayError::InvalidDisplay)?;
        let x = physical_origin(record.bounds.origin.x, record.scale)?;
        let y = physical_origin(record.bounds.origin.y, record.scale)?;
        let display = Display::new(
            display_id,
            format!("Display {}", record.ordinal + 1),
            x,
            y,
            record.width_px,
            record.height_px,
            scale_milli(record.scale)?,
            refresh_mhz(record.refresh_hz),
            DisplayFlags::new(record.is_main, true, true, false),
        );
        result.push(MacDisplay {
            native_display_id: record.native_id,
            captured: CapturedDisplay {
                display,
                backend_handle: record.backend_id,
                adapter_id: "coregraphics".to_owned(),
                adapter_model: "Apple display pipeline".to_owned(),
                identity_source: DisplayIdentitySource::MacDisplayUuid,
            },
            points: HostDisplayGeometry::new(
                record.bounds.origin.x,
                record.bounds.origin.y,
                record.bounds.size.width,
                record.bounds.size.height,
            )
            .map_err(|_| MacDisplayError::InvalidDisplay)?,
        });
    }
    Ok(result)
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[derive(Clone, Copy)]
#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}
#[derive(Clone, Copy)]
#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

struct RawDisplay {
    native_id: u32,
    ordinal: usize,
    bounds: CGRect,
    width_px: u32,
    height_px: u32,
    scale: f64,
    refresh_hz: f64,
    is_main: bool,
    backend_id: String,
    identity: DisplayIdentity,
}

fn query_display(id: u32, ordinal: usize) -> Result<RawDisplay, MacDisplayError> {
    // SAFETY: CoreGraphics value-returning queries accept only the enumerated display identifier.
    let bounds = unsafe { CGDisplayBounds(id) };
    // SAFETY: The display ID came from the bounded active-display list; this query has no output pointer.
    let width_px = u32::try_from(unsafe { CGDisplayPixelsWide(id) })
        .map_err(|_| MacDisplayError::InvalidDisplay)?;
    // SAFETY: The display ID came from the bounded active-display list; this query has no output pointer.
    let height_px = u32::try_from(unsafe { CGDisplayPixelsHigh(id) })
        .map_err(|_| MacDisplayError::InvalidDisplay)?;
    // SAFETY: The display ID came from the bounded active-display list.
    let vendor = unsafe { CGDisplayVendorNumber(id) };
    // SAFETY: The display ID came from the bounded active-display list.
    let model = unsafe { CGDisplayModelNumber(id) };
    // SAFETY: The display ID came from the bounded active-display list.
    let serial = unsafe { CGDisplaySerialNumber(id) };
    // SAFETY: The display ID came from the bounded active-display list.
    let is_main = unsafe { CGDisplayIsMain(id) != 0 };
    if !bounds.origin.x.is_finite()
        || !bounds.origin.y.is_finite()
        || !bounds.size.width.is_finite()
        || !bounds.size.height.is_finite()
        || bounds.size.width <= 0.0
        || bounds.size.height <= 0.0
        || width_px == 0
        || height_px == 0
    {
        return Err(MacDisplayError::InvalidDisplay);
    }
    let scale = width_px as f64 / bounds.size.width;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(MacDisplayError::InvalidDisplay);
    }

    // SAFETY: The display ID came from the bounded active-display list; a non-null result is +1 retained.
    let mode = unsafe { CGDisplayCopyDisplayMode(id) };
    let refresh_hz = if mode.is_null() {
        0.0
    } else {
        // SAFETY: The mode came from CGDisplayCopyDisplayMode and remains retained until release.
        let hz = unsafe { CGDisplayModeGetRefreshRate(mode) };
        // SAFETY: `mode` is a non-null CoreFoundation object returned at +1 ownership.
        unsafe {
            CFRelease(mode.cast());
        }
        hz
    };
    let backend_id = display_uuid(id).ok_or(MacDisplayError::InvalidDisplay)?;
    let identity = DisplayIdentity::new(
        decode_vendor(u16::try_from(vendor).map_err(|_| MacDisplayError::InvalidDisplay)?),
        u16::try_from(model).map_err(|_| MacDisplayError::InvalidDisplay)?,
        serial,
        backend_id.clone(),
        None,
    );
    Ok(RawDisplay {
        native_id: id,
        ordinal,
        bounds,
        width_px,
        height_px,
        scale,
        refresh_hz,
        is_main,
        backend_id,
        identity,
    })
}

fn display_uuid(id: u32) -> Option<String> {
    // SAFETY: CoreGraphics returns a retained CFUUID for a valid display identifier.
    let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(id) };
    if uuid.is_null() {
        return None;
    }
    // SAFETY: `uuid` is a valid retained CFUUID; allocator default is a process-global constant.
    let string = unsafe { CFUUIDCreateString(std::ptr::null(), uuid) };
    // SAFETY: Balance the retained UUID irrespective of string-conversion outcome.
    unsafe {
        CFRelease(uuid);
    }
    if string.is_null() {
        return None;
    }
    let mut buffer = [0 as c_char; 128];
    // SAFETY: The mutable buffer is fixed-size and its exact capacity is supplied. UTF-8 is a
    // documented CF string encoding; success guarantees a terminating NUL within the buffer.
    let success = unsafe {
        CFStringGetCString(
            string,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            CF_STRING_ENCODING_UTF8,
        ) != 0
    };
    // SAFETY: `string` is a retained CFString created by CFUUIDCreateString.
    unsafe {
        CFRelease(string);
    }
    if !success {
        return None;
    }
    // SAFETY: CFStringGetCString reported success, so `buffer` contains a NUL-terminated string.
    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_str().ok()?;
    Some(value.to_owned())
}

fn decode_vendor(code: u16) -> [u8; 3] {
    let letter = |shift: u32| {
        (((code >> shift) & 0x1f_u16) as u8)
            .checked_add(b'A' - 1)
            .unwrap_or(0)
    };
    let vendor = [letter(10), letter(5), letter(0)];
    if vendor.iter().all(u8::is_ascii_uppercase) {
        vendor
    } else {
        *b"UNK"
    }
}

fn physical_origin(origin_pt: f64, scale: f64) -> Result<i32, MacDisplayError> {
    let value = (origin_pt * scale).round();
    if !value.is_finite() || value < i32::MIN as f64 || value > i32::MAX as f64 {
        return Err(MacDisplayError::InvalidDisplay);
    }
    Ok(value as i32)
}

fn scale_milli(scale: f64) -> Result<u16, MacDisplayError> {
    let value = (scale * 1000.0).round();
    if !value.is_finite() || value < 1.0 || value > f64::from(u16::MAX) {
        return Err(MacDisplayError::InvalidDisplay);
    }
    Ok(value as u16)
}

fn refresh_mhz(hz: f64) -> u32 {
    if hz.is_finite() && hz > 0.0 && hz <= f64::from(u32::MAX) / 1000.0 {
        (hz * 1000.0).round() as u32
    } else {
        0
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut u32,
        display_count: *mut u32,
    ) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGDisplayVendorNumber(display: u32) -> u32;
    fn CGDisplayModelNumber(display: u32) -> u32;
    fn CGDisplaySerialNumber(display: u32) -> u32;
    fn CGDisplayIsMain(display: u32) -> u8;
    fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void;
    fn CGDisplayModeGetRefreshRate(mode: *const c_void) -> f64;
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDCreateString(allocator: *const c_void, uuid: *const c_void) -> *const c_void;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut c_char,
        capacity: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(value: *const c_void);
}
