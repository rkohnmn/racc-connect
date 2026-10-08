//! Widget-local pointer geometry for the live native video surface.
use iced::{Point, Rectangle};
use racc_topology::RenderedRect;

/// Returns the same letterboxed content rectangle used by the video shader for a widget layout.
pub(crate) fn rendered_video_rect(
    bounds: Rectangle,
    stream_width: u32,
    stream_height: u32,
) -> Option<RenderedRect> {
    let width = bounds.width.round().max(1.0) as u32;
    let height = bounds.height.round().max(1.0) as u32;
    crate::design::video_rect(width, height, stream_width.max(1), stream_height.max(1))
        .ok()
        .flatten()
        .or_else(|| RenderedRect::new(0, 0, width, height).ok())
}

/// Converts a window-global iced point into coordinates local to the exact video widget.
pub(crate) fn local_point(bounds: Rectangle, point: Point) -> (i64, i64) {
    let x = (point.x - bounds.x).round();
    let y = (point.y - bounds.y).round();
    (x as i64, y as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_input::{ViewerControlMode, ViewerInputEvent, ViewerInputReducer, ViewerMouseInput};

    #[test]
    fn pointer_rect_matches_shader_letterbox_and_rejects_bars() {
        let bounds = Rectangle {
            x: 100.0,
            y: 40.0,
            width: 1_000.0,
            height: 600.0,
        };
        let rect = rendered_video_rect(bounds, 1_920, 1_080).expect("video area");
        assert_eq!(rect.origin(), (0, 18));
        assert_eq!(rect.size(), (1_000, 563));
        assert_eq!(local_point(bounds, Point::new(600.0, 340.0)), (500, 300));

        let mut reducer = ViewerInputReducer::new(9, 4).expect("valid target");
        reducer.set_connected(true).expect("valid state");
        reducer.set_focused(true).expect("valid state");
        reducer
            .set_mode(ViewerControlMode::RemoteDesktop)
            .expect("valid state");
        let outside = reducer
            .mouse_event(ViewerMouseInput::Move {
                px: 500,
                py: 0,
                rendered_rect: rect,
            })
            .expect("letterbox hover is rejected");
        assert!(outside.is_empty());
        let inside = reducer
            .mouse_event(ViewerMouseInput::Move {
                px: 500,
                py: 300,
                rendered_rect: rect,
            })
            .expect("valid video point");
        assert!(matches!(
            inside.as_slice(),
            [ViewerInputEvent::Input(racc_proto::InputEvent {
                event: racc_proto::InputEventKind::MouseMoveAbs { u, v },
                ..
            })] if *u > 30_000 && *u < 36_000 && *v > 30_000 && *v < 36_000
        ));
    }
}
