use crate::{
    diff_topologies, letterbox_rect, map_to_host_pixel, normalize_pointer, windows_absolute,
    Display, DisplayFlags, DisplayId, HostPhysicalPixel, NormalizedPointer, PointerPolicy,
    RenderedRect, Topology,
};
use proptest::prelude::*;

fn one_display(id: u32, x: i32, y: i32, width: u32, height: u32) -> Display {
    Display::new(
        DisplayId::new(id).unwrap_or_else(|| unreachable!()),
        format!("display-{id}"),
        x,
        y,
        width,
        height,
        1000,
        60_000,
        DisplayFlags::new(true, true, true, false),
    )
}

fn round_half_up(numerator: u64, denominator: u64) -> u64 {
    (numerator * 2 + denominator) / (denominator * 2)
}

proptest! {
    #[test]
    fn host_pixels_stay_inside_display_and_are_monotonic(
        width in 1u32..=100_000,
        height in 1u32..=100_000,
        x in any::<i32>(),
        y in any::<i32>(),
        u1 in any::<u16>(),
        u2 in any::<u16>(),
        v1 in any::<u16>(),
        v2 in any::<u16>(),
    ) {
        let display = one_display(1, x, y, width, height);
        for (u, v) in [(0, 0), (u16::MAX, u16::MAX), (u1, v1), (u2, v2)] {
            let point = map_to_host_pixel(&display, NormalizedPointer { u, v }).expect("bounded mapping");
            prop_assert!(point.x >= i64::from(x));
            prop_assert!(point.x < i64::from(x) + i64::from(width));
            prop_assert!(point.y >= i64::from(y));
            prop_assert!(point.y < i64::from(y) + i64::from(height));
        }
        let (low_u, high_u) = (u1.min(u2), u1.max(u2));
        let (low_v, high_v) = (v1.min(v2), v1.max(v2));
        let left = map_to_host_pixel(&display, NormalizedPointer { u: low_u, v: low_v }).expect("map");
        let right = map_to_host_pixel(&display, NormalizedPointer { u: high_u, v: high_v }).expect("map");
        prop_assert!(left.x <= right.x);
        prop_assert!(left.y <= right.y);
    }

    #[test]
    fn letterbox_fits_is_centered_and_preserves_aspect_within_rounding(
        cw in 1u32..=20_000,
        ch in 1u32..=20_000,
        sw in 1u32..=20_000,
        sh in 1u32..=20_000,
    ) {
        let rect = letterbox_rect(cw, ch, sw, sh).expect("valid sizes").expect("nonzero container");
        let (x, y) = rect.origin();
        let (width, height) = rect.size();
        prop_assert!(width > 0 && height > 0);
        prop_assert!(x + width <= cw);
        prop_assert!(y + height <= ch);
        prop_assert!(x.abs_diff(cw - x - width) <= 1);
        prop_assert!(y.abs_diff(ch - y - height) <= 1);
        let lhs = u64::from(width) * u64::from(sh);
        let rhs = u64::from(height) * u64::from(sw);
        prop_assert!(lhs.abs_diff(rhs) <= u64::from(sw.max(sh)));
    }

    #[test]
    fn normalize_then_map_error_is_bounded_by_render_to_display_scale(
        rw in 1u32..=4096,
        rh in 1u32..=4096,
        dw in 1u32..=100_000,
        dh in 1u32..=100_000,
        dx in 0u32..=4096,
        dy in 0u32..=4096,
    ) {
        let rect = RenderedRect::new(7, 13, rw, rh).expect("valid rect");
        let px_offset = dx % (rw + 1);
        let py_offset = dy % (rh + 1);
        let pointer = normalize_pointer(
            rect,
            i64::from(7 + px_offset),
            i64::from(13 + py_offset),
            PointerPolicy::Clamp,
        ).expect("normalization").expect("clamped");
        let display = one_display(1, -400, 21, dw, dh);
        let mapped = map_to_host_pixel(&display, pointer).expect("host point");
        let intended_x = round_half_up(u64::from(px_offset) * u64::from(dw - 1), u64::from(rw));
        let intended_y = round_half_up(u64::from(py_offset) * u64::from(dh - 1), u64::from(rh));
        let x_tolerance = u64::from(dw).div_ceil(u64::from(rw)) + 1;
        let y_tolerance = u64::from(dh).div_ceil(u64::from(rh)) + 1;
        prop_assert!(mapped.x.abs_diff(-400 + intended_x as i64) <= x_tolerance);
        prop_assert!(mapped.y.abs_diff(21 + intended_y as i64) <= y_tolerance);
    }

    #[test]
    fn windows_virtual_absolute_is_bounded_and_monotonic(
        width in 1u32..=100_000,
        height in 1u32..=100_000,
        x1 in any::<u16>(),
        x2 in any::<u16>(),
        y1 in any::<u16>(),
        y2 in any::<u16>(),
    ) {
        let display = one_display(1, -1920, -1080, width, height);
        let topology = Topology::new(1, vec![display.clone()], None).expect("valid topology");
        let bounds = topology.virtual_desktop_bounds().expect("bounds").expect("display");
        let to_pixel = |value: u16, extent: u32, origin: i32| -> i64 {
            let offset = if extent == 1 { 0 } else {
                round_half_up(u64::from(value) * u64::from(extent - 1), u64::from(u16::MAX)) as i64
            };
            i64::from(origin) + offset
        };
        let a = windows_absolute(HostPhysicalPixel { x: to_pixel(x1, width, -1920), y: to_pixel(y1, height, -1080) }, bounds).expect("absolute");
        let b = windows_absolute(HostPhysicalPixel { x: to_pixel(x2, width, -1920), y: to_pixel(y2, height, -1080) }, bounds).expect("absolute");
        prop_assert!(u32::from(a.x) <= 65_535 && u32::from(a.y) <= 65_535);
        prop_assert!(u32::from(b.x) <= 65_535 && u32::from(b.y) <= 65_535);
        if x1 <= x2 { prop_assert!(a.x <= b.x); } else { prop_assert!(a.x >= b.x); }
        if y1 <= y2 { prop_assert!(a.y <= b.y); } else { prop_assert!(a.y >= b.y); }
    }

    #[test]
    fn topology_diff_counts_match_generated_display_sets(old_mask in any::<u16>(), new_mask in any::<u16>()) {
        let make = |mask: u16, rev| {
            let displays = (0..16u32)
                .filter(|bit| mask & (1 << bit) != 0)
                .map(|bit| Display::new(DisplayId::new(bit + 1).expect("nonzero"), format!("display-{}", bit + 1), (bit as i32) * 2000, 0, 1920, 1080, 1000, 60_000, DisplayFlags::new(false, true, true, false)))
                .collect();
            Topology::new(rev, displays, None).expect("generated valid topology")
        };
        let old = make(old_mask, 1);
        let new = make(new_mask, 2);
        let diff = diff_topologies(&old, &new);
        prop_assert_eq!(diff.added.len(), (new_mask & !old_mask).count_ones() as usize);
        prop_assert_eq!(diff.removed.len(), (old_mask & !new_mask).count_ones() as usize);
        prop_assert!(diff.changed.is_empty());
        prop_assert_eq!(diff.is_empty(), old_mask == new_mask);
    }

    #[test]
    fn proto_conversion_roundtrips_generated_topologies(
        id in 1u32..=u32::MAX,
        x in any::<i32>(),
        y in any::<i32>(),
        width in 1u32..=u32::MAX,
        height in 1u32..=u32::MAX,
        scale in any::<u16>(),
    ) {
        let topology = Topology::new(4, vec![Display::new(DisplayId::new(id).expect("nonzero"), format!("display-{id}"), x, y, width, height, scale, 60_000, DisplayFlags::new(true, true, true, false))], Some(DisplayId::new(id).expect("nonzero")))
            .expect("valid topology");
        let wire = topology.to_proto().expect("wire conversion");
        prop_assert_eq!(Topology::from_proto(&wire).expect("domain conversion"), topology);
    }
}
