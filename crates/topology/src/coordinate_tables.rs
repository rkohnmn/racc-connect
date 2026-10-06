use crate::{
    letterbox_rect, map_to_host_pixel, normalize_pointer, windows_absolute, Display, DisplayFlags,
    DisplayId, HostPhysicalPixel, NormalizedPointer, PointerPolicy, Topology, WindowsAbsolute,
};

fn display(id: u32, x: i32, y: i32, width: u32, height: u32, scale: u16) -> Display {
    Display::new(
        DisplayId::new(id).unwrap_or_else(|| unreachable!()),
        format!("D{id}"),
        x,
        y,
        width,
        height,
        scale,
        60_000,
        DisplayFlags::new(false, true, true, false),
    )
}

fn center_pixel(rect: crate::RenderedRect) -> (i64, i64) {
    let (x, y) = rect.origin();
    let (width, height) = rect.size();
    (i64::from(x + width / 2), i64::from(y + height / 2))
}

#[test]
fn explicit_display_topology_host_pixel_vectors() {
    let cases = [
        (
            display(1, 0, 0, 1920, 1080, 1000),
            HostPhysicalPixel { x: 960, y: 540 },
        ),
        (
            display(2, -1920, 0, 1920, 1080, 1000),
            HostPhysicalPixel { x: -960, y: 540 },
        ),
        (
            display(3, 0, -1080, 1920, 1080, 1000),
            HostPhysicalPixel { x: 960, y: -540 },
        ),
        (
            display(4, 0, 0, 3840, 2160, 1500),
            HostPhysicalPixel { x: 1920, y: 1080 },
        ),
        (
            display(5, 3840, 0, 1920, 1080, 1000),
            HostPhysicalPixel { x: 4800, y: 540 },
        ),
        (
            display(6, 2560, -1440, 2560, 1440, 1250),
            HostPhysicalPixel { x: 3840, y: -720 },
        ),
    ];
    for (screen, expected) in cases {
        assert_eq!(
            map_to_host_pixel(&screen, NormalizedPointer { u: 32768, v: 32768 }),
            Ok(expected)
        );
    }
}

#[test]
fn windows_absolute_vectors_for_negative_side_by_side_and_stacked_displays() {
    let side_by_side = Topology::new(
        1,
        vec![
            display(1, 0, 0, 1920, 1080, 1000),
            display(2, -1920, 0, 1920, 1080, 1000),
        ],
        None,
    )
    .expect("valid side-by-side topology");
    let bounds = side_by_side
        .virtual_desktop_bounds()
        .expect("bounds")
        .expect("available displays");
    for (pixel, expected) in [
        (
            HostPhysicalPixel { x: -960, y: 540 },
            WindowsAbsolute { x: 16388, y: 32798 },
        ),
        (
            HostPhysicalPixel { x: 960, y: 540 },
            WindowsAbsolute { x: 49164, y: 32798 },
        ),
    ] {
        assert_eq!(windows_absolute(pixel, bounds), Ok(expected));
    }

    let stacked = Topology::new(
        2,
        vec![
            display(3, 0, -1080, 1920, 1080, 1000),
            display(4, 0, 0, 1920, 1080, 1000),
        ],
        None,
    )
    .expect("valid stacked topology");
    let bounds = stacked
        .virtual_desktop_bounds()
        .expect("bounds")
        .expect("available displays");
    for (pixel, expected) in [
        (
            HostPhysicalPixel { x: 960, y: -540 },
            WindowsAbsolute { x: 32785, y: 16391 },
        ),
        (
            HostPhysicalPixel { x: 960, y: 540 },
            WindowsAbsolute { x: 32785, y: 49174 },
        ),
    ] {
        assert_eq!(windows_absolute(pixel, bounds), Ok(expected));
    }
}

#[test]
fn three_wide_windows_virtual_desktop_corners_and_centers() {
    let topology = Topology::new(
        1,
        vec![
            display(1, -1920, 0, 1920, 1080, 1000),
            display(2, 0, 0, 1920, 1080, 1000),
            display(3, 1920, 0, 1920, 1080, 1000),
        ],
        None,
    )
    .expect("valid three-wide topology");
    let bounds = topology
        .virtual_desktop_bounds()
        .expect("bounds")
        .expect("available displays");
    for (pixel, expected) in [
        (
            HostPhysicalPixel { x: -1920, y: 0 },
            WindowsAbsolute { x: 0, y: 0 },
        ),
        (
            HostPhysicalPixel { x: 3839, y: 0 },
            WindowsAbsolute { x: u16::MAX, y: 0 },
        ),
        (
            HostPhysicalPixel { x: -1920, y: 1079 },
            WindowsAbsolute { x: 0, y: u16::MAX },
        ),
        (
            HostPhysicalPixel { x: 3839, y: 1079 },
            WindowsAbsolute {
                x: u16::MAX,
                y: u16::MAX,
            },
        ),
        (
            HostPhysicalPixel { x: -960, y: 540 },
            WindowsAbsolute { x: 10924, y: 32798 },
        ),
        (
            HostPhysicalPixel { x: 960, y: 540 },
            WindowsAbsolute { x: 32773, y: 32798 },
        ),
        (
            HostPhysicalPixel { x: 2880, y: 540 },
            WindowsAbsolute { x: 54622, y: 32798 },
        ),
    ] {
        assert_eq!(windows_absolute(pixel, bounds), Ok(expected));
    }
}

#[test]
fn stream_480p_and_720p_rectangles_on_1080p_and_4k_displays_keep_center_mapping() {
    let cases = [
        (1920, 1080, 854, 480, (0, 0, 1920, 1079)),
        (1920, 1080, 1280, 720, (0, 0, 1920, 1080)),
        (3840, 2160, 854, 480, (0, 1, 3840, 2158)),
        (3840, 2160, 1280, 720, (0, 0, 3840, 2160)),
    ];
    for (display_w, display_h, stream_w, stream_h, expected_rect) in cases {
        let screen = display(1, -300, 75, display_w, display_h, 1000);
        let rect = letterbox_rect(display_w, display_h, stream_w, stream_h)
            .expect("valid stream")
            .expect("nonzero container");
        let (x, y) = rect.origin();
        let (width, height) = rect.size();
        assert_eq!((x, y, width, height), expected_rect);
        let (px, py) = center_pixel(rect);
        let normalized = normalize_pointer(rect, px, py, PointerPolicy::Clamp)
            .expect("normalization")
            .expect("clamped");
        let result = map_to_host_pixel(&screen, normalized).expect("host mapping");
        let expected = map_to_host_pixel(&screen, NormalizedPointer { u: 32768, v: 32768 })
            .expect("stream-independent host mapping");
        assert!(result.x.abs_diff(expected.x) <= 2);
        assert!(result.y.abs_diff(expected.y) <= 2);
    }
}
