//! Portable mapping from macOS wire-pixel topology to the OS's logical point geometry.

use racc_topology::{
    map_to_macos_points, CoordinateError, HostDisplayGeometry, HostPhysicalPixel, MacPoint,
    NormalizedPointer,
};

/// Pixel and point rectangles for one macOS display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacDisplayGeometry {
    pixel_origin: (i32, i32),
    pixel_size: (u32, u32),
    points: HostDisplayGeometry,
}

impl MacDisplayGeometry {
    /// Creates geometry using the physical display rectangle and OS-reported point rectangle.
    pub fn new(
        pixel_origin: (i32, i32),
        pixel_size: (u32, u32),
        points: HostDisplayGeometry,
    ) -> Result<Self, CoordinateError> {
        if pixel_size.0 == 0 || pixel_size.1 == 0 {
            return Err(CoordinateError::ZeroDisplaySize);
        }
        Ok(Self {
            pixel_origin,
            pixel_size,
            points,
        })
    }

    /// Converts an in-display physical pixel to the matching point position.
    pub fn map_pixel(self, pixel: HostPhysicalPixel) -> Result<MacPoint, CoordinateError> {
        let x0 = i64::from(self.pixel_origin.0);
        let y0 = i64::from(self.pixel_origin.1);
        let dx = pixel
            .x
            .checked_sub(x0)
            .ok_or(CoordinateError::ArithmeticOverflow)?;
        let dy = pixel
            .y
            .checked_sub(y0)
            .ok_or(CoordinateError::ArithmeticOverflow)?;
        if dx < 0
            || dy < 0
            || dx >= i64::from(self.pixel_size.0)
            || dy >= i64::from(self.pixel_size.1)
        {
            return Err(CoordinateError::HostPixelOutsideVirtualDesktop);
        }
        let pointer = NormalizedPointer {
            u: axis_normalized(dx as u64, self.pixel_size.0)?,
            v: axis_normalized(dy as u64, self.pixel_size.1)?,
        };
        map_to_macos_points(self.points, pointer)
    }
}

fn axis_normalized(offset: u64, size: u32) -> Result<u16, CoordinateError> {
    if size == 0 || offset >= u64::from(size) {
        return Err(CoordinateError::ZeroDisplaySize);
    }
    if size == 1 {
        return Ok(0);
    }
    let denominator = u64::from(size - 1);
    let numerator = u128::from(offset.min(denominator)) * u128::from(u16::MAX);
    let rounded = numerator + u128::from(denominator / 2);
    u16::try_from(rounded / u128::from(denominator))
        .map_err(|_| CoordinateError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retina_pixel_edges_map_to_os_reported_points() {
        let geometry = MacDisplayGeometry::new(
            (0, 0),
            (3024, 1964),
            HostDisplayGeometry::new(0.0, 0.0, 1512.0, 982.0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            geometry
                .map_pixel(HostPhysicalPixel { x: 0, y: 0 })
                .unwrap(),
            MacPoint {
                x_pt: 0.0,
                y_pt: 0.0
            }
        );
        assert_eq!(
            geometry
                .map_pixel(HostPhysicalPixel { x: 3023, y: 1963 })
                .unwrap(),
            MacPoint {
                x_pt: 1512.0,
                y_pt: 982.0
            }
        );
    }

    #[test]
    fn negative_origin_secondary_display_uses_its_logical_rectangle() {
        let geometry = MacDisplayGeometry::new(
            (-1920, 0),
            (1920, 1080),
            HostDisplayGeometry::new(-1440.0, 0.0, 1440.0, 810.0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            geometry
                .map_pixel(HostPhysicalPixel { x: -1920, y: 0 })
                .unwrap(),
            MacPoint {
                x_pt: -1440.0,
                y_pt: 0.0
            }
        );
        assert_eq!(
            geometry
                .map_pixel(HostPhysicalPixel { x: -1, y: 1079 })
                .unwrap(),
            MacPoint {
                x_pt: 0.0,
                y_pt: 810.0
            }
        );
    }

    #[test]
    fn rejects_pixels_outside_the_selected_display() {
        let geometry = MacDisplayGeometry::new(
            (100, 100),
            (100, 50),
            HostDisplayGeometry::new(0.0, 0.0, 100.0, 50.0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            geometry.map_pixel(HostPhysicalPixel { x: 99, y: 100 }),
            Err(CoordinateError::HostPixelOutsideVirtualDesktop)
        );
    }
}
