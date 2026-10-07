//! Shared design tokens and pure layout calculations for the desktop shell.

use racc_topology::{letterbox_rect, CoordinateError, RenderedRect};

/// Fixed design tokens used throughout the shell.
pub mod tokens {
    use iced::Color;

    /// Device rail width in logical pixels.
    pub const DEVICE_RAIL_WIDTH: f32 = 72.0;
    /// Device sidebar width in logical pixels.
    pub const DEVICE_SIDEBAR_WIDTH: f32 = 240.0;
    /// Telemetry sidebar width in logical pixels.
    pub const TELEMETRY_WIDTH: f32 = 280.0;
    /// Collapsed sidebar width in logical pixels.
    pub const COLLAPSED_SIDEBAR_WIDTH: f32 = 48.0;
    /// Device tile width and height in the rail.
    pub const DEVICE_TILE_SIZE: f32 = 48.0;
    /// Minimum useful workspace width in logical pixels.
    pub const MIN_WORKSPACE_WIDTH: f32 = 360.0;
    /// Minimum window width in logical pixels.
    pub const MIN_WINDOW_WIDTH: f32 = 1050.0;
    /// Minimum window height in logical pixels.
    pub const MIN_WINDOW_HEIGHT: f32 = 640.0;
    /// Standard border width.
    pub const BORDER_WIDTH: f32 = 1.0;
    /// Packet loss fraction that receives a warning color in the UI.
    pub const LOSS_WARNING_FRACTION: f64 = 0.02;
    /// Focus ring width.
    pub const FOCUS_RING_WIDTH: f32 = 2.0;
    /// Standard spacing unit.
    pub const SPACE_1: f32 = 4.0;
    /// Compact spacing token.
    pub const SPACE_2: f32 = 8.0;
    /// Medium spacing token.
    pub const SPACE_3: f32 = 12.0;
    /// Large spacing token.
    pub const SPACE_4: f32 = 16.0;
    /// Body text size.
    pub const BODY_SIZE: f32 = 14.0;
    /// Compact metadata text size.
    pub const META_SIZE: f32 = 12.0;
    /// Section label size.
    pub const SECTION_SIZE: f32 = 12.0;
    /// Header text size.
    pub const HEADER_SIZE: f32 = 20.0;
    /// Large workspace title size.
    pub const TITLE_SIZE: f32 = 22.0;
    /// Metric value size.
    pub const METRIC_SIZE: f32 = 20.0;
    /// Small corner radius.
    pub const RADIUS_SMALL: f32 = 6.0;
    /// Standard corner radius.
    pub const RADIUS_MEDIUM: f32 = 8.0;
    /// Card corner radius.
    pub const RADIUS_LARGE: f32 = 10.0;
    /// Dark rail background.
    pub const RAIL: Color = Color::from_rgb(0.075, 0.082, 0.105);
    /// Secondary sidebar background.
    pub const SIDEBAR: Color = Color::from_rgb(0.105, 0.115, 0.145);
    /// Main workspace background.
    pub const MAIN: Color = Color::from_rgb(0.075, 0.085, 0.115);
    /// Raised card background.
    pub const CARD: Color = Color::from_rgb(0.14, 0.155, 0.19);
    /// Slightly elevated surface for compact control groups.
    pub const SURFACE: Color = Color::from_rgb(0.12, 0.13, 0.165);
    /// Near-black video canvas that frames letterboxed content.
    pub const VIDEO_FRAME: Color = Color::from_rgb(0.045, 0.052, 0.070);
    /// Subtle accent wash for status chips and focus areas.
    pub const ACCENT_WASH: Color = Color::from_rgb(0.15, 0.14, 0.24);
    /// Selected row background.
    pub const SELECTED: Color = Color::from_rgb(0.22, 0.20, 0.36);
    /// Primary text color.
    pub const TEXT: Color = Color::from_rgb(0.94, 0.95, 0.98);
    /// Secondary text color.
    pub const MUTED: Color = Color::from_rgb(0.61, 0.64, 0.70);
    /// Blue-purple accent.
    pub const ACCENT: Color = Color::from_rgb(0.48, 0.40, 0.95);
    /// Positive state color.
    pub const ONLINE: Color = Color::from_rgb(0.31, 0.78, 0.57);
    /// Error/disconnected state color.
    pub const OFFLINE: Color = Color::from_rgb(0.83, 0.38, 0.42);
    /// Destructive-action color.
    pub const DANGER: Color = Color::from_rgb(0.86, 0.30, 0.36);
    /// Subtle border color.
    pub const BORDER: Color = Color::from_rgb(0.22, 0.24, 0.29);
}

/// Computed widths for the four main shell regions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionWidths {
    /// Device rail width.
    pub rail: f32,
    /// Device sidebar width.
    pub device_sidebar: f32,
    /// Session workspace width.
    pub workspace: f32,
    /// Telemetry sidebar width.
    pub telemetry: f32,
}

/// Computes the minimum-respecting region widths for the current window.
pub fn region_widths(
    window_width: f32,
    device_collapsed: bool,
    telemetry_collapsed: bool,
) -> RegionWidths {
    let rail = tokens::DEVICE_RAIL_WIDTH;
    let device_sidebar = if device_collapsed {
        tokens::COLLAPSED_SIDEBAR_WIDTH
    } else {
        tokens::DEVICE_SIDEBAR_WIDTH
    };
    let telemetry = if telemetry_collapsed {
        tokens::COLLAPSED_SIDEBAR_WIDTH
    } else {
        tokens::TELEMETRY_WIDTH
    };
    let minimum = rail + device_sidebar + telemetry + tokens::MIN_WORKSPACE_WIDTH;
    let workspace = (window_width.max(minimum) - rail - device_sidebar - telemetry)
        .max(tokens::MIN_WORKSPACE_WIDTH);
    RegionWidths {
        rail,
        device_sidebar,
        workspace,
        telemetry,
    }
}

/// Computes an aspect-preserving video rectangle using the shared topology math.
pub fn video_rect(
    container_width: u32,
    container_height: u32,
    stream_width: u32,
    stream_height: u32,
) -> Result<Option<RenderedRect>, CoordinateError> {
    letterbox_rect(
        container_width,
        container_height,
        stream_width,
        stream_height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_widths_preserve_the_minimum_workspace_and_collapse_tokens() {
        let open = region_widths(700.0, false, false);
        assert_eq!(open.workspace, tokens::MIN_WORKSPACE_WIDTH);
        let collapsed = region_widths(700.0, true, true);
        assert_eq!(collapsed.device_sidebar, tokens::COLLAPSED_SIDEBAR_WIDTH);
        assert_eq!(collapsed.telemetry, tokens::COLLAPSED_SIDEBAR_WIDTH);
        assert_eq!(collapsed.workspace, 532.0);
        let wide = region_widths(1600.0, false, false);
        assert_eq!(wide.workspace, 1008.0);
    }

    #[test]
    fn video_layout_uses_shared_letterbox_geometry() {
        let rect = video_rect(1000, 600, 1920, 1080)
            .expect("valid dimensions")
            .expect("visible area");
        assert_eq!(rect.origin(), (0, 18));
        assert_eq!(rect.size(), (1000, 563));
        assert!(video_rect(1000, 600, 0, 1080).is_err());
        assert!(video_rect(0, 0, 1920, 1080)
            .expect("empty container is valid")
            .is_none());
    }
}
