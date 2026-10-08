//! Bounded cursor-shape cache and display-to-rendered-video geometry for the live viewer.
use std::collections::VecDeque;
use std::sync::Arc;

use racc_proto::{CursorBlendMode, CursorShape, CursorUpdate, MAX_CURSOR_BYTES, MAX_CURSOR_DIM};
use racc_topology::RenderedRect;

const MAX_CACHED_SHAPES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CursorTarget {
    pub epoch: u16,
    pub display_id: u32,
    pub display_width: u32,
    pub display_height: u32,
}

#[derive(Clone, Debug)]
struct CachedShape {
    shape_id: u32,
    width: u32,
    height: u32,
    hotspot_x: u32,
    hotspot_y: u32,
    blend_mode: CursorBlendMode,
    revision: u64,
    rgba: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub(crate) struct CursorRender {
    pub shape_id: u32,
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub blend_mode: CursorBlendMode,
    pub revision: u64,
    pub rgba: Arc<[u8]>,
    pub x: u32,
    pub y: u32,
    pub display_width: u32,
    pub display_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CursorRect {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Default)]
pub(crate) struct CursorOverlayCache {
    target: Option<CursorTarget>,
    position: Option<CursorUpdate>,
    shapes: VecDeque<CachedShape>,
    next_revision: u64,
}

impl CursorOverlayCache {
    /// Changes the active epoch and selected display, dropping stale position metadata.
    pub(crate) fn set_target(&mut self, target: Option<CursorTarget>) {
        if self.target != target {
            self.target = target;
            self.position = None;
        }
    }

    /// Validates, converts, and caches one bounded cursor bitmap by shape ID.
    ///
    /// The only allocation is on a shape update, never during frame preparation.
    pub(crate) fn insert_shape(&mut self, shape: CursorShape) -> bool {
        let Some((width, height)) = checked_shape_dimensions(&shape) else {
            return false;
        };
        if !valid_pixels(shape.blend_mode, &shape.bgra) {
            return false;
        }

        let mut rgba = Vec::with_capacity(shape.bgra.len());
        for bgra in shape.bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
        }

        self.next_revision = self.next_revision.wrapping_add(1).max(1);
        let cached = CachedShape {
            shape_id: shape.shape_id,
            width,
            height,
            hotspot_x: u32::from(shape.hotspot_x),
            hotspot_y: u32::from(shape.hotspot_y),
            blend_mode: shape.blend_mode,
            revision: self.next_revision,
            rgba: rgba.into(),
        };
        if let Some(index) = self
            .shapes
            .iter()
            .position(|entry| entry.shape_id == shape.shape_id)
        {
            self.shapes.remove(index);
        } else if self.shapes.len() == MAX_CACHED_SHAPES {
            self.shapes.pop_front();
        }
        self.shapes.push_back(cached);
        true
    }

    /// Accepts a cursor position only for the active stream epoch.
    pub(crate) fn set_position(&mut self, position: CursorUpdate) -> bool {
        let Some(target) = self.target else {
            return false;
        };
        if position.epoch != target.epoch {
            return false;
        }
        self.position = Some(position);
        true
    }

    /// Resolves visible cursor metadata for the exact frame epoch and display.
    pub(crate) fn for_frame(&self, epoch: u16, display_id: u32) -> Option<CursorRender> {
        let target = self.target?;
        let position = self.position?;
        if target.epoch != epoch
            || target.display_id != display_id
            || position.epoch != epoch
            || !position.visible
            || target.display_width == 0
            || target.display_height == 0
            || position.x < 0
            || position.y < 0
            || position.x as u32 >= target.display_width
            || position.y as u32 >= target.display_height
        {
            return None;
        }
        let shape = self
            .shapes
            .iter()
            .find(|shape| shape.shape_id == position.shape_id)?;
        Some(CursorRender {
            shape_id: shape.shape_id,
            width: shape.width,
            height: shape.height,
            hotspot_x: shape.hotspot_x,
            hotspot_y: shape.hotspot_y,
            blend_mode: shape.blend_mode,
            revision: shape.revision,
            rgba: Arc::clone(&shape.rgba),
            x: position.x as u32,
            y: position.y as u32,
            display_width: target.display_width,
            display_height: target.display_height,
        })
    }

    /// Drops all shape and position state when a peer session ends.
    pub(crate) fn clear_session(&mut self) {
        self.target = None;
        self.position = None;
        self.shapes.clear();
    }

    #[cfg(test)]
    fn contains_shape(&self, shape_id: u32) -> bool {
        self.shapes.iter().any(|shape| shape.shape_id == shape_id)
    }

    #[cfg(test)]
    fn cached_shape_count(&self) -> usize {
        self.shapes.len()
    }
}

/// Scales physical cursor geometry into the exact aspect-preserved video rectangle.
pub(crate) fn rendered_cursor_rect(
    cursor: &CursorRender,
    video: RenderedRect,
) -> Option<CursorRect> {
    if cursor.display_width == 0 || cursor.display_height == 0 {
        return None;
    }
    let (x, y) = video.origin();
    let (video_width, video_height) = video.size();
    let scale_x = video_width as f32 / cursor.display_width as f32;
    let scale_y = video_height as f32 / cursor.display_height as f32;
    let anchor_x = x as f32 + cursor.x as f32 * scale_x;
    let anchor_y = y as f32 + cursor.y as f32 * scale_y;
    Some(CursorRect {
        left: anchor_x - cursor.hotspot_x as f32 * scale_x,
        top: anchor_y - cursor.hotspot_y as f32 * scale_y,
        width: cursor.width as f32 * scale_x,
        height: cursor.height as f32 * scale_y,
    })
}

fn checked_shape_dimensions(shape: &CursorShape) -> Option<(u32, u32)> {
    let width = usize::from(shape.width);
    let height = usize::from(shape.height);
    if width == 0
        || height == 0
        || width > MAX_CURSOR_DIM
        || height > MAX_CURSOR_DIM
        || shape.hotspot_x >= shape.width
        || shape.hotspot_y >= shape.height
    {
        return None;
    }
    let expected = width.checked_mul(height)?.checked_mul(4)?;
    if expected > MAX_CURSOR_BYTES || shape.bgra.len() != expected {
        return None;
    }
    Some((width as u32, height as u32))
}

fn valid_pixels(mode: CursorBlendMode, bgra: &[u8]) -> bool {
    bgra.chunks_exact(4).all(|pixel| {
        let [blue, green, red, alpha] = [pixel[0], pixel[1], pixel[2], pixel[3]];
        match mode {
            CursorBlendMode::PremultipliedAlpha => blue <= alpha && green <= alpha && red <= alpha,
            CursorBlendMode::WindowsMaskedColor => alpha == 0 || alpha == u8::MAX,
            CursorBlendMode::WindowsAndXor => {
                (alpha == 0 || alpha == u8::MAX)
                    && (blue == 0 || blue == u8::MAX)
                    && blue == green
                    && green == red
            }
        }
    })
}

/// Applies one encoded cursor pixel using the same integer semantics as the GPU compositor.
#[cfg(test)]
pub(crate) fn composite_rgb(
    mode: CursorBlendMode,
    destination: [u8; 3],
    source_rgba: [u8; 4],
) -> Option<[u8; 3]> {
    let [red, green, blue, alpha] = source_rgba;
    match mode {
        CursorBlendMode::PremultipliedAlpha => {
            let inverse_alpha = u16::from(u8::MAX - alpha);
            Some([
                (u16::from(red) + (u16::from(destination[0]) * inverse_alpha + 127) / 255).min(255)
                    as u8,
                (u16::from(green) + (u16::from(destination[1]) * inverse_alpha + 127) / 255)
                    .min(255) as u8,
                (u16::from(blue) + (u16::from(destination[2]) * inverse_alpha + 127) / 255).min(255)
                    as u8,
            ])
        }
        CursorBlendMode::WindowsMaskedColor => match alpha {
            0 => Some([red, green, blue]),
            255 => Some([
                destination[0] ^ red,
                destination[1] ^ green,
                destination[2] ^ blue,
            ]),
            _ => None,
        },
        CursorBlendMode::WindowsAndXor if alpha == 0 || alpha == 255 => {
            let and_mask = alpha;
            Some([
                (destination[0] & and_mask) ^ red,
                (destination[1] & and_mask) ^ green,
                (destination[2] & and_mask) ^ blue,
            ])
        }
        CursorBlendMode::WindowsAndXor => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(shape_id: u32, mode: CursorBlendMode, bgra: [u8; 4]) -> CursorShape {
        CursorShape {
            shape_id,
            width: 1,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            blend_mode: mode,
            bgra: bgra.to_vec(),
        }
    }

    fn cursor_cache() -> CursorOverlayCache {
        let mut cache = CursorOverlayCache::default();
        cache.set_target(Some(CursorTarget {
            epoch: 7,
            display_id: 3,
            display_width: 1920,
            display_height: 1080,
        }));
        cache
    }

    #[test]
    fn caches_all_modes_with_bounded_rgba_conversion() {
        let mut cache = cursor_cache();
        for (id, mode, bgra) in [
            (1, CursorBlendMode::PremultipliedAlpha, [0, 0, 128, 128]),
            (2, CursorBlendMode::WindowsMaskedColor, [30, 20, 10, 255]),
            (3, CursorBlendMode::WindowsAndXor, [255, 255, 255, 0]),
        ] {
            assert!(cache.insert_shape(shape(id, mode, bgra)));
            cache.set_position(CursorUpdate {
                epoch: 7,
                shape_id: id,
                x: 20,
                y: 30,
                visible: true,
            });
            let render = cache.for_frame(7, 3).expect("shape resolves");
            assert_eq!(render.blend_mode, mode);
            assert_eq!(render.rgba.len(), 4);
            assert_eq!(render.rgba.as_ref(), &[bgra[2], bgra[1], bgra[0], bgra[3]]);
        }
    }

    #[test]
    fn rejects_oversized_malformed_and_mode_invalid_shapes() {
        let mut cache = cursor_cache();
        let mut invalid = shape(1, CursorBlendMode::PremultipliedAlpha, [0, 0, 0, 0]);
        invalid.width = (MAX_CURSOR_DIM + 1) as u16;
        assert!(!cache.insert_shape(invalid));

        let mut invalid = shape(2, CursorBlendMode::PremultipliedAlpha, [0, 0, 200, 100]);
        assert!(!cache.insert_shape(invalid.clone()));
        invalid.blend_mode = CursorBlendMode::WindowsMaskedColor;
        invalid.bgra = vec![0, 0, 0, 127];
        assert!(!cache.insert_shape(invalid.clone()));
        invalid.blend_mode = CursorBlendMode::WindowsAndXor;
        invalid.bgra = vec![0, 255, 0, 0];
        assert!(!cache.insert_shape(invalid));

        let mut wrong_length = shape(3, CursorBlendMode::PremultipliedAlpha, [0, 0, 0, 0]);
        wrong_length.bgra.clear();
        assert!(!cache.insert_shape(wrong_length));
    }

    #[test]
    fn shape_cache_is_bounded_and_evicts_oldest_entry() {
        let mut cache = cursor_cache();
        for id in 0..=(MAX_CACHED_SHAPES as u32) {
            assert!(cache.insert_shape(shape(
                id,
                CursorBlendMode::PremultipliedAlpha,
                [0, 0, 0, 0]
            )));
        }
        assert_eq!(cache.cached_shape_count(), MAX_CACHED_SHAPES);
        assert!(!cache.contains_shape(0));
        assert!(cache.contains_shape(MAX_CACHED_SHAPES as u32));
    }

    #[test]
    fn stale_epoch_display_and_hidden_positions_never_render() {
        let mut cache = cursor_cache();
        cache.insert_shape(shape(5, CursorBlendMode::PremultipliedAlpha, [0, 0, 0, 0]));
        assert!(!cache.set_position(CursorUpdate {
            epoch: 6,
            shape_id: 5,
            x: 20,
            y: 30,
            visible: true,
        }));
        assert!(cache.for_frame(7, 3).is_none());

        assert!(cache.set_position(CursorUpdate {
            epoch: 7,
            shape_id: 5,
            x: 20,
            y: 30,
            visible: true,
        }));
        assert!(cache.for_frame(8, 3).is_none());
        assert!(cache.for_frame(7, 4).is_none());
        assert!(cache.for_frame(7, 3).is_some());

        assert!(cache.set_position(CursorUpdate {
            epoch: 7,
            shape_id: 5,
            x: 1920,
            y: 30,
            visible: true,
        }));
        assert!(cache.for_frame(7, 3).is_none());
        assert!(cache.set_position(CursorUpdate {
            epoch: 7,
            shape_id: 5,
            x: 20,
            y: 30,
            visible: false,
        }));
        assert!(cache.for_frame(7, 3).is_none());
    }

    #[test]
    fn target_change_clears_position_but_keeps_bounded_shape_cache() {
        let mut cache = cursor_cache();
        cache.insert_shape(shape(5, CursorBlendMode::PremultipliedAlpha, [0, 0, 0, 0]));
        cache.set_position(CursorUpdate {
            epoch: 7,
            shape_id: 5,
            x: 20,
            y: 30,
            visible: true,
        });
        cache.set_target(Some(CursorTarget {
            epoch: 8,
            display_id: 4,
            display_width: 2560,
            display_height: 1440,
        }));
        assert!(cache.for_frame(8, 4).is_none());
        assert!(cache.contains_shape(5));
    }

    #[test]
    fn shape_revision_does_not_reuse_texture_key_after_session_reset() {
        let mut cache = cursor_cache();
        let shape = shape(5, CursorBlendMode::PremultipliedAlpha, [0, 0, 0, 0]);
        assert!(cache.insert_shape(shape.clone()));
        assert!(cache.set_position(CursorUpdate {
            epoch: 7,
            shape_id: 5,
            x: 20,
            y: 30,
            visible: true,
        }));
        let first_revision = cache.for_frame(7, 3).expect("first cursor").revision;

        cache.clear_session();
        cache.set_target(Some(CursorTarget {
            epoch: 8,
            display_id: 4,
            display_width: 2560,
            display_height: 1440,
        }));
        assert!(cache.insert_shape(shape));
        assert!(cache.set_position(CursorUpdate {
            epoch: 8,
            shape_id: 5,
            x: 20,
            y: 30,
            visible: true,
        }));
        let second_revision = cache.for_frame(8, 4).expect("reconnected cursor").revision;
        assert_ne!(first_revision, second_revision);
    }

    #[test]
    fn cursor_bitmap_and_hotspot_scale_with_the_rendered_display_rect() {
        let cursor = CursorRender {
            shape_id: 1,
            width: 32,
            height: 24,
            hotspot_x: 4,
            hotspot_y: 5,
            blend_mode: CursorBlendMode::PremultipliedAlpha,
            revision: 1,
            rgba: Arc::from([0u8; 4]),
            x: 960,
            y: 540,
            display_width: 1920,
            display_height: 1080,
        };
        let rect = RenderedRect::new(0, 18, 1000, 563).expect("valid video rect");
        let output = rendered_cursor_rect(&cursor, rect).expect("visible cursor geometry");
        assert!((output.left - 497.9167).abs() < 0.01);
        assert!((output.top - 296.8935).abs() < 0.01);
        assert!((output.width - 16.6667).abs() < 0.01);
        assert!((output.height - 12.5111).abs() < 0.01);
    }

    #[test]
    fn each_blend_mode_has_explicit_compositing_semantics() {
        assert_eq!(
            composite_rgb(
                CursorBlendMode::PremultipliedAlpha,
                [100, 40, 0],
                [128, 0, 0, 128]
            ),
            Some([178, 20, 0])
        );
        assert_eq!(
            composite_rgb(
                CursorBlendMode::WindowsMaskedColor,
                [0x12, 0x34, 0x56],
                [0xa1, 0xb2, 0xc3, 0]
            ),
            Some([0xa1, 0xb2, 0xc3])
        );
        assert_eq!(
            composite_rgb(
                CursorBlendMode::WindowsMaskedColor,
                [0x12, 0x34, 0x56],
                [0x0f, 0xf0, 0xaa, 255]
            ),
            Some([0x1d, 0xc4, 0xfc])
        );
        assert_eq!(
            composite_rgb(
                CursorBlendMode::WindowsAndXor,
                [0x12, 0x34, 0x56],
                [255, 0, 255, 0]
            ),
            Some([255, 0, 255])
        );
        assert_eq!(
            composite_rgb(
                CursorBlendMode::WindowsAndXor,
                [0x12, 0x34, 0x56],
                [0, 255, 0, 255]
            ),
            Some([0x12, 0xcb, 0x56])
        );
    }
}
