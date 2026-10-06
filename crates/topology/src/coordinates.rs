use core::fmt;

use crate::{Display, VirtualDesktopBounds};
use racc_proto::MAX_CURSOR_DIM;

/// Integer rectangle occupied by rendered video in device pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderedRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl RenderedRect {
    /// Creates a nonzero rectangle.
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, CoordinateError> {
        if width == 0 || height == 0 {
            return Err(CoordinateError::InvalidRect);
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    /// Returns the top-left offset in device pixels.
    pub const fn origin(self) -> (u32, u32) {
        (self.x, self.y)
    }

    /// Returns the rendered rectangle size in device pixels.
    pub const fn size(self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Pointer behavior for positions outside the rendered video rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerPolicy {
    /// Clamp outside positions to the nearest video edge.
    Clamp,
    /// Reject positions outside the video rectangle.
    Reject,
}

/// Fixed-point normalized pointer position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NormalizedPointer {
    /// Horizontal coordinate in 0..=65535.
    pub u: u16,
    /// Vertical coordinate in 0..=65535.
    pub v: u16,
}

/// Host physical pixel coordinates in virtual-desktop space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostPhysicalPixel {
    /// Horizontal host pixel coordinate.
    pub x: i64,
    /// Vertical host pixel coordinate.
    pub y: i64,
}

/// Windows MOUSEEVENTF_VIRTUALDESK absolute coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowsAbsolute {
    /// Horizontal absolute value in 0..=65535.
    pub x: u16,
    /// Vertical absolute value in 0..=65535.
    pub y: u16,
}

/// Host-local logical display rectangle, expressed in macOS points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostDisplayGeometry {
    x_pt: f64,
    y_pt: f64,
    width_pt: f64,
    height_pt: f64,
}

impl HostDisplayGeometry {
    /// Creates logical display geometry from host OS point measurements.
    pub fn new(
        x_pt: f64,
        y_pt: f64,
        width_pt: f64,
        height_pt: f64,
    ) -> Result<Self, CoordinateError> {
        if !x_pt.is_finite()
            || !y_pt.is_finite()
            || !width_pt.is_finite()
            || !height_pt.is_finite()
            || width_pt <= 0.0
            || height_pt <= 0.0
        {
            return Err(CoordinateError::InvalidHostGeometry);
        }
        Ok(Self {
            x_pt,
            y_pt,
            width_pt,
            height_pt,
        })
    }
}

/// A macOS host point position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacPoint {
    /// Horizontal position in points.
    pub x_pt: f64,
    /// Vertical position in points.
    pub y_pt: f64,
}

/// Host cursor bitmap dimensions and hotspot in bitmap pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorBitmapGeometry {
    width_px: u32,
    height_px: u32,
    hotspot_x_px: u32,
    hotspot_y_px: u32,
}

impl CursorBitmapGeometry {
    /// Creates cursor geometry using the bounded v0 cursor dimensions.
    pub fn new(
        width_px: u32,
        height_px: u32,
        hotspot_x_px: u32,
        hotspot_y_px: u32,
    ) -> Result<Self, CoordinateError> {
        if width_px == 0
            || height_px == 0
            || width_px > MAX_CURSOR_DIM as u32
            || height_px > MAX_CURSOR_DIM as u32
            || hotspot_x_px >= width_px
            || hotspot_y_px >= height_px
        {
            return Err(CoordinateError::InvalidCursorGeometry);
        }
        Ok(Self {
            width_px,
            height_px,
            hotspot_x_px,
            hotspot_y_px,
        })
    }
}

/// Device-pixel rectangle for drawing a scaled cursor bitmap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorDrawRect {
    /// Horizontal top-left position; may be outside the rendered video rectangle.
    pub x: i64,
    /// Vertical top-left position; may be outside the rendered video rectangle.
    pub y: i64,
    /// Scaled cursor width, at least one pixel.
    pub width: u32,
    /// Scaled cursor height, at least one pixel.
    pub height: u32,
}

/// Coordinate mapping errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinateError {
    /// Stream dimensions must be nonzero.
    ZeroStreamSize,
    /// A rendered rectangle must have nonzero dimensions.
    InvalidRect,
    /// A display passed to coordinate math has zero width or height.
    ZeroDisplaySize,
    /// Virtual desktop bounds have zero width or height.
    InvalidVirtualDesktop,
    /// A host pixel is outside the supplied virtual desktop bounds.
    HostPixelOutsideVirtualDesktop,
    /// Cursor size or hotspot does not satisfy the bounded wire geometry.
    InvalidCursorGeometry,
    /// Host-provided point geometry is not finite and positive.
    InvalidHostGeometry,
    /// Checked coordinate arithmetic or representation conversion overflowed.
    ArithmeticOverflow,
}

impl fmt::Display for CoordinateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroStreamSize => "stream dimensions must be nonzero",
            Self::InvalidRect => "rendered rectangle dimensions must be nonzero",
            Self::ZeroDisplaySize => "display dimensions must be nonzero",
            Self::InvalidVirtualDesktop => "virtual desktop dimensions must be nonzero",
            Self::HostPixelOutsideVirtualDesktop => "host pixel is outside virtual desktop bounds",
            Self::InvalidCursorGeometry => "cursor geometry is invalid",
            Self::InvalidHostGeometry => "host point geometry is invalid",
            Self::ArithmeticOverflow => "coordinate arithmetic overflow",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CoordinateError {}

/// Computes an aspect-preserving rectangle centered in a device-pixel container.
///
/// Stream dimensions must be nonzero. A zero-sized container returns None.
pub fn letterbox_rect(
    container_width: u32,
    container_height: u32,
    stream_width: u32,
    stream_height: u32,
) -> Result<Option<RenderedRect>, CoordinateError> {
    if stream_width == 0 || stream_height == 0 {
        return Err(CoordinateError::ZeroStreamSize);
    }
    if container_width == 0 || container_height == 0 {
        return Ok(None);
    }

    let cw = u64::from(container_width);
    let ch = u64::from(container_height);
    let sw = u64::from(stream_width);
    let sh = u64::from(stream_height);
    let width_limited_left = cw
        .checked_mul(sh)
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let width_limited_right = ch
        .checked_mul(sw)
        .ok_or(CoordinateError::ArithmeticOverflow)?;

    let (render_width, render_height) = if width_limited_left <= width_limited_right {
        (
            cw,
            round_ratio(
                cw.checked_mul(sh)
                    .ok_or(CoordinateError::ArithmeticOverflow)?,
                sw,
            )?
            .max(1),
        )
    } else {
        (
            round_ratio(
                ch.checked_mul(sw)
                    .ok_or(CoordinateError::ArithmeticOverflow)?,
                sh,
            )?
            .max(1),
            ch,
        )
    };

    let width = u32::try_from(render_width).map_err(|_| CoordinateError::ArithmeticOverflow)?;
    let height = u32::try_from(render_height).map_err(|_| CoordinateError::ArithmeticOverflow)?;
    let x = (container_width - width) / 2;
    let y = (container_height - height) / 2;
    Ok(Some(RenderedRect::new(x, y, width, height)?))
}

/// Normalizes a device-pixel pointer against the rendered video rectangle.
///
/// The right and bottom edges are exclusive for Reject and map to 65535 for Clamp.
pub fn normalize_pointer(
    rect: RenderedRect,
    px: i64,
    py: i64,
    policy: PointerPolicy,
) -> Result<Option<NormalizedPointer>, CoordinateError> {
    if rect.width == 0 || rect.height == 0 {
        return Err(CoordinateError::InvalidRect);
    }

    let left = i128::from(rect.x);
    let top = i128::from(rect.y);
    let right = left + i128::from(rect.width);
    let bottom = top + i128::from(rect.height);
    let px_wide = i128::from(px);
    let py_wide = i128::from(py);
    let inside = px_wide >= left && px_wide < right && py_wide >= top && py_wide < bottom;
    if policy == PointerPolicy::Reject && !inside {
        return Ok(None);
    }

    let dx = (px_wide - left).clamp(0, i128::from(rect.width));
    let dy = (py_wide - top).clamp(0, i128::from(rect.height));
    let dx = u64::try_from(dx).map_err(|_| CoordinateError::ArithmeticOverflow)?;
    let dy = u64::try_from(dy).map_err(|_| CoordinateError::ArithmeticOverflow)?;
    let u = ratio_to_u16(dx, u64::from(rect.width))?;
    let v = ratio_to_u16(dy, u64::from(rect.height))?;
    Ok(Some(NormalizedPointer { u, v }))
}

/// Maps normalized coordinates to a host display's physical pixel space.
pub fn map_to_host_pixel(
    display: &Display,
    pointer: NormalizedPointer,
) -> Result<HostPhysicalPixel, CoordinateError> {
    if display.width_px == 0 || display.height_px == 0 {
        return Err(CoordinateError::ZeroDisplaySize);
    }
    let offset_x = normalized_pixel_offset(pointer.u, display.width_px)?;
    let offset_y = normalized_pixel_offset(pointer.v, display.height_px)?;
    let x = i64::from(display.x)
        .checked_add(i64::from(offset_x))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let y = i64::from(display.y)
        .checked_add(i64::from(offset_y))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    Ok(HostPhysicalPixel { x, y })
}

/// Maps a host physical pixel to virtual-desktop absolute coordinates.
///
/// The Windows API's internal rounding and DPI behavior remain unverified.
pub fn windows_absolute(
    pixel: HostPhysicalPixel,
    bounds: VirtualDesktopBounds,
) -> Result<WindowsAbsolute, CoordinateError> {
    if bounds.width() == 0 || bounds.height() == 0 {
        return Err(CoordinateError::InvalidVirtualDesktop);
    }

    let left = bounds.left();
    let top = bounds.top();
    let right = bounds
        .rightmost_pixel_x()
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let bottom = bounds
        .bottommost_pixel_y()
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    if pixel.x < left || pixel.x > right || pixel.y < top || pixel.y > bottom {
        return Err(CoordinateError::HostPixelOutsideVirtualDesktop);
    }

    let x_relative = u64::try_from(
        pixel
            .x
            .checked_sub(left)
            .ok_or(CoordinateError::ArithmeticOverflow)?,
    )
    .map_err(|_| CoordinateError::ArithmeticOverflow)?;
    let y_relative = u64::try_from(
        pixel
            .y
            .checked_sub(top)
            .ok_or(CoordinateError::ArithmeticOverflow)?,
    )
    .map_err(|_| CoordinateError::ArithmeticOverflow)?;
    Ok(WindowsAbsolute {
        x: virtual_axis_to_u16(x_relative, bounds.width())?,
        y: virtual_axis_to_u16(y_relative, bounds.height())?,
    })
}

/// Maps normalized video coordinates to host-supplied macOS point geometry.
pub fn map_to_macos_points(
    geometry: HostDisplayGeometry,
    pointer: NormalizedPointer,
) -> Result<MacPoint, CoordinateError> {
    let u = f64::from(pointer.u) / f64::from(u16::MAX);
    let v = f64::from(pointer.v) / f64::from(u16::MAX);
    let x_pt = geometry.x_pt + u * geometry.width_pt;
    let y_pt = geometry.y_pt + v * geometry.height_pt;
    if !x_pt.is_finite() || !y_pt.is_finite() {
        return Err(CoordinateError::ArithmeticOverflow);
    }
    Ok(MacPoint { x_pt, y_pt })
}

/// Maps a cursor position and hotspot into device pixels for drawing.
pub fn map_cursor_to_rect(
    display: &Display,
    rendered: RenderedRect,
    cursor_x: i32,
    cursor_y: i32,
    bitmap: CursorBitmapGeometry,
) -> Result<CursorDrawRect, CoordinateError> {
    if display.width_px == 0 || display.height_px == 0 {
        return Err(CoordinateError::ZeroDisplaySize);
    }
    if rendered.width == 0 || rendered.height == 0 {
        return Err(CoordinateError::InvalidRect);
    }

    let scaled_x = scale_signed(cursor_x, rendered.width, display.width_px)?;
    let scaled_y = scale_signed(cursor_y, rendered.height, display.height_px)?;
    let hotspot_x = scale_unsigned(bitmap.hotspot_x_px, rendered.width, display.width_px)?;
    let hotspot_y = scale_unsigned(bitmap.hotspot_y_px, rendered.height, display.height_px)?;
    let width = scale_unsigned(bitmap.width_px, rendered.width, display.width_px)?.max(1);
    let height = scale_unsigned(bitmap.height_px, rendered.height, display.height_px)?.max(1);
    let x = i64::from(rendered.x)
        .checked_add(scaled_x)
        .and_then(|value| value.checked_sub(i64::from(hotspot_x)))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let y = i64::from(rendered.y)
        .checked_add(scaled_y)
        .and_then(|value| value.checked_sub(i64::from(hotspot_y)))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    Ok(CursorDrawRect {
        x,
        y,
        width,
        height,
    })
}

fn round_ratio(numerator: u64, denominator: u64) -> Result<u64, CoordinateError> {
    if denominator == 0 {
        return Err(CoordinateError::ArithmeticOverflow);
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounds_up = remainder
        .checked_mul(2)
        .ok_or(CoordinateError::ArithmeticOverflow)?
        >= denominator;
    if rounds_up {
        quotient
            .checked_add(1)
            .ok_or(CoordinateError::ArithmeticOverflow)
    } else {
        Ok(quotient)
    }
}

fn ratio_to_u16(numerator: u64, denominator: u64) -> Result<u16, CoordinateError> {
    let scaled = numerator
        .checked_mul(u64::from(u16::MAX))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let rounded = round_ratio(scaled, denominator)?;
    u16::try_from(rounded).map_err(|_| CoordinateError::ArithmeticOverflow)
}

fn normalized_pixel_offset(value: u16, dimension: u32) -> Result<u32, CoordinateError> {
    let extent = u64::from(dimension.saturating_sub(1));
    let numerator = u64::from(value)
        .checked_mul(extent)
        .and_then(|number| number.checked_mul(2))
        .and_then(|number| number.checked_add(u64::from(u16::MAX)))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let denominator = u64::from(u16::MAX) * 2;
    let offset = numerator / denominator;
    u32::try_from(offset).map_err(|_| CoordinateError::ArithmeticOverflow)
}

fn virtual_axis_to_u16(relative: u64, dimension: u32) -> Result<u16, CoordinateError> {
    if dimension == 1 {
        return Ok(0);
    }
    let span = u64::from(dimension - 1);
    let scaled = relative
        .checked_mul(u64::from(u16::MAX))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let rounded = round_ratio(scaled, span)?;
    u16::try_from(rounded).map_err(|_| CoordinateError::ArithmeticOverflow)
}

fn scale_unsigned(
    value: u32,
    render_extent: u32,
    display_extent: u32,
) -> Result<u32, CoordinateError> {
    let numerator = u64::from(value)
        .checked_mul(u64::from(render_extent))
        .ok_or(CoordinateError::ArithmeticOverflow)?;
    let rounded = round_ratio(numerator, u64::from(display_extent))?;
    u32::try_from(rounded).map_err(|_| CoordinateError::ArithmeticOverflow)
}

fn scale_signed(
    value: i32,
    render_extent: u32,
    display_extent: u32,
) -> Result<i64, CoordinateError> {
    let numerator = i128::from(value) * i128::from(render_extent);
    let denominator = i128::from(display_extent);
    if denominator == 0 {
        return Err(CoordinateError::ZeroDisplaySize);
    }
    let magnitude = numerator.unsigned_abs();
    let divisor = u128::try_from(denominator).map_err(|_| CoordinateError::ArithmeticOverflow)?;
    let quotient = magnitude / divisor;
    let remainder = magnitude % divisor;
    let rounded = if remainder.saturating_mul(2) >= divisor {
        quotient.saturating_add(1)
    } else {
        quotient
    };
    let rounded = i64::try_from(rounded).map_err(|_| CoordinateError::ArithmeticOverflow)?;
    if numerator < 0 {
        rounded
            .checked_neg()
            .ok_or(CoordinateError::ArithmeticOverflow)
    } else {
        Ok(rounded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DisplayFlags, DisplayId, Topology};

    fn display(id: u32, x: i32, y: i32, width: u32, height: u32, scale: u16) -> Display {
        Display::new(
            DisplayId::new(id).unwrap_or_else(|| unreachable!()),
            format!("D{id}"),
            x,
            y,
            width,
            height,
            scale,
            60000,
            DisplayFlags::new(false, true, true, false),
        )
    }

    fn rect(result: Result<Option<RenderedRect>, CoordinateError>) -> RenderedRect {
        result.expect("valid rectangle").expect("nonzero container")
    }

    #[test]
    fn letterbox_table_has_explicit_integer_rectangles() {
        let cases = [
            (1920, 1080, 1920, 1080, (0, 0, 1920, 1080)),
            (1920, 1080, 640, 480, (240, 0, 1440, 1080)),
            (1080, 1920, 16, 9, (0, 656, 1080, 608)),
            (1, 1, 16, 9, (0, 0, 1, 1)),
            (16, 9, 1, u32::MAX, (7, 0, 1, 9)),
            (
                u32::MAX,
                u32::MAX,
                u32::MAX,
                u32::MAX,
                (0, 0, u32::MAX, u32::MAX),
            ),
        ];
        for (cw, ch, sw, sh, expected) in cases {
            assert_eq!(
                rect(letterbox_rect(cw, ch, sw, sh)).size_and_origin(),
                expected
            );
        }
        assert_eq!(letterbox_rect(0, 100, 1, 1), Ok(None));
        assert_eq!(
            letterbox_rect(100, 100, 0, 1),
            Err(CoordinateError::ZeroStreamSize)
        );
    }

    #[test]
    fn normalization_rejects_or_clamps_the_right_and_bottom_edges() {
        let video = RenderedRect::new(10, 20, 100, 50).expect("valid rectangle");
        assert_eq!(
            normalize_pointer(video, 60, 45, PointerPolicy::Reject),
            Ok(Some(NormalizedPointer { u: 32768, v: 32768 }))
        );
        assert_eq!(
            normalize_pointer(video, 110, 70, PointerPolicy::Reject),
            Ok(None)
        );
        assert_eq!(
            normalize_pointer(video, 110, 70, PointerPolicy::Clamp),
            Ok(Some(NormalizedPointer {
                u: u16::MAX,
                v: u16::MAX
            }))
        );
        assert_eq!(
            normalize_pointer(video, i64::MIN, i64::MAX, PointerPolicy::Clamp),
            Ok(Some(NormalizedPointer { u: 0, v: u16::MAX }))
        );
    }

    #[test]
    fn host_pixel_endpoints_center_and_one_pixel_display_are_exact() {
        let screen = display(1, -1920, -100, 1920, 1080, 1000);
        assert_eq!(
            map_to_host_pixel(&screen, NormalizedPointer { u: 0, v: 0 }),
            Ok(HostPhysicalPixel { x: -1920, y: -100 })
        );
        assert_eq!(
            map_to_host_pixel(
                &screen,
                NormalizedPointer {
                    u: u16::MAX,
                    v: u16::MAX
                }
            ),
            Ok(HostPhysicalPixel { x: -1, y: 979 })
        );
        let single = display(2, 17, -9, 1, 1, 1000);
        assert_eq!(
            map_to_host_pixel(&single, NormalizedPointer { u: 32768, v: 32768 }),
            Ok(HostPhysicalPixel { x: 17, y: -9 })
        );
    }

    #[test]
    fn virtual_bounds_include_negative_origins_gaps_and_only_available_displays() {
        let topology = Topology::new(
            1,
            vec![
                display(1, -1920, 0, 1920, 1080, 1000),
                display(2, 0, 0, 1920, 1080, 1000),
                Display::new(
                    DisplayId::new(3).expect("nonzero"),
                    "unavailable",
                    9000,
                    0,
                    1920,
                    1080,
                    1000,
                    60000,
                    DisplayFlags::new(false, false, false, false),
                ),
            ],
            None,
        )
        .expect("valid topology");
        let bounds = topology
            .virtual_desktop_bounds()
            .expect("bounds")
            .expect("available displays");
        assert_eq!(
            (bounds.left(), bounds.top(), bounds.width(), bounds.height()),
            (-1920, 0, 3840, 1080)
        );

        let overflow = Topology::new(
            2,
            vec![
                display(4, i32::MIN, 0, 1, 1, 1000),
                display(5, i32::MAX, 0, 1, 1, 1000),
            ],
            None,
        )
        .expect("valid topology with far-apart displays");
        assert_eq!(
            overflow.virtual_desktop_bounds(),
            Err(crate::TopologyError::BoundsOverflow)
        );
    }

    #[test]
    fn windows_absolute_maps_two_display_virtual_desktop_corners() {
        let bounds = Topology::new(
            1,
            vec![
                display(1, -1920, 0, 1920, 1080, 1000),
                display(2, 0, 0, 1920, 1080, 1000),
            ],
            None,
        )
        .expect("valid topology")
        .virtual_desktop_bounds()
        .expect("bounds")
        .expect("available displays");
        let corners = [
            (
                HostPhysicalPixel { x: -1920, y: 0 },
                WindowsAbsolute { x: 0, y: 0 },
            ),
            (
                HostPhysicalPixel { x: 1919, y: 0 },
                WindowsAbsolute { x: u16::MAX, y: 0 },
            ),
            (
                HostPhysicalPixel { x: -1920, y: 1079 },
                WindowsAbsolute { x: 0, y: u16::MAX },
            ),
            (
                HostPhysicalPixel { x: 1919, y: 1079 },
                WindowsAbsolute {
                    x: u16::MAX,
                    y: u16::MAX,
                },
            ),
        ];
        for (pixel, expected) in corners {
            assert_eq!(windows_absolute(pixel, bounds), Ok(expected));
        }
    }

    #[test]
    fn macos_point_mapping_uses_host_points_and_negative_origins() {
        let retina = HostDisplayGeometry::new(0.0, 0.0, 1920.0, 1080.0).expect("valid geometry");
        let center = map_to_macos_points(retina, NormalizedPointer { u: 32768, v: 32768 })
            .expect("mapped center");
        assert!((center.x_pt - 960.0146486610208).abs() < 0.000001);
        assert!((center.y_pt - 540.0082398718242).abs() < 0.000001);
        let secondary =
            HostDisplayGeometry::new(-1280.0, 0.0, 1280.0, 800.0).expect("valid geometry");
        let point = map_to_macos_points(
            secondary,
            NormalizedPointer {
                u: u16::MAX,
                v: u16::MAX,
            },
        )
        .expect("mapped point");
        assert_eq!(
            point,
            MacPoint {
                x_pt: 0.0,
                y_pt: 800.0
            }
        );
    }

    #[test]
    fn cursor_bitmap_and_hotspot_are_scaled_with_documented_rounding() {
        let screen = display(1, 0, 0, 1920, 1080, 1000);
        let rect = RenderedRect::new(100, 50, 960, 540).expect("valid rectangle");
        let bitmap = CursorBitmapGeometry::new(32, 32, 4, 6).expect("valid cursor");
        assert_eq!(
            map_cursor_to_rect(&screen, rect, 960, 540, bitmap),
            Ok(CursorDrawRect {
                x: 578,
                y: 317,
                width: 16,
                height: 16,
            })
        );
    }

    trait RectExpected {
        fn size_and_origin(self) -> (u32, u32, u32, u32);
    }

    impl RectExpected for RenderedRect {
        fn size_and_origin(self) -> (u32, u32, u32, u32) {
            (self.x, self.y, self.width, self.height)
        }
    }
}
