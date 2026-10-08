//! Shared design tokens and pure layout calculations for the desktop shell.

use racc_topology::{letterbox_rect, CoordinateError, RenderedRect};

/// Fixed design tokens used throughout the shell.
pub mod tokens {
    use iced::Color;

    /// Device rail width in logical pixels.
    pub const DEVICE_RAIL_WIDTH: f32 = 72.0;
    /// Device sidebar width in logical pixels.
    pub const DEVICE_SIDEBAR_WIDTH: f32 = 264.0;
    /// Telemetry sidebar width in logical pixels.
    pub const TELEMETRY_WIDTH: f32 = 300.0;
    /// Collapsed sidebar width in logical pixels.
    pub const COLLAPSED_SIDEBAR_WIDTH: f32 = 48.0;
    /// Device tile width and height in the rail.
    pub const DEVICE_TILE_SIZE: f32 = 40.0;
    /// Width of the selected-device marker inside each rail tile.
    pub const RAIL_INDICATOR_WIDTH: f32 = 3.0;
    /// Monogram tile size in the device directory.
    pub const HOME_DEVICE_AVATAR_SIZE: f32 = 56.0;
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
    /// Maximum width for a centered empty-session card.
    pub const EMPTY_STATE_MAX_WIDTH: f32 = 560.0;
    /// Small corner radius.
    pub const RADIUS_SMALL: f32 = 6.0;
    /// Standard corner radius.
    pub const RADIUS_MEDIUM: f32 = 8.0;
    /// Card corner radius.
    pub const RADIUS_LARGE: f32 = 10.0;
    /// Deep graphite background for the device rail.
    pub const RAIL: Color = Color::from_rgb(0.055, 0.060, 0.080);
    /// Secondary device and telemetry sidebar background.
    pub const SIDEBAR: Color = Color::from_rgb(0.090, 0.098, 0.133);
    /// Main workspace background, separated clearly from both sidebars.
    pub const MAIN: Color = Color::from_rgb(0.067, 0.075, 0.105);
    /// Raised card background for lists and event rows.
    pub const CARD: Color = Color::from_rgb(0.141, 0.153, 0.200);
    /// Elevated surface for grouped controls and compact panels.
    pub const SURFACE: Color = Color::from_rgb(0.114, 0.125, 0.169);
    /// Hover fill for interactive rows and buttons.
    pub const HOVER: Color = Color::from_rgb(0.169, 0.184, 0.239);
    /// Near-black video canvas that frames letterboxed content.
    pub const VIDEO_FRAME: Color = Color::from_rgb(0.031, 0.039, 0.059);
    /// Muted purple wash for badges and low-emphasis selected surfaces.
    pub const ACCENT_WASH: Color = Color::from_rgb(0.141, 0.125, 0.224);
    /// Selected row background, distinct from hover and raised cards.
    pub const SELECTED: Color = Color::from_rgb(0.188, 0.169, 0.290);
    /// Primary text color.
    pub const TEXT: Color = Color::from_rgb(0.957, 0.961, 0.988);
    /// Secondary text color, tuned to stay readable on the dark surfaces.
    pub const MUTED: Color = Color::from_rgb(0.690, 0.710, 0.770);
    /// Bright blue-purple accent for labels, indicators and borders.
    pub const ACCENT: Color = Color::from_rgb(0.698, 0.659, 0.990);
    /// Dark blue-purple fill for active/pressed controls with light text.
    pub const ACCENT_FILL: Color = Color::from_rgb(0.302, 0.267, 0.541);
    /// Positive state color for online indicators.
    pub const ONLINE: Color = Color::from_rgb(0.345, 0.839, 0.635);
    /// Error/disconnected state color for readable status indicators.
    pub const OFFLINE: Color = Color::from_rgb(0.953, 0.540, 0.565);
    /// Dark destructive-action fill for light button labels.
    pub const DANGER: Color = Color::from_rgb(0.608, 0.188, 0.251);
    /// Low-emphasis border used to separate nested dark surfaces.
    pub const BORDER: Color = Color::from_rgb(0.160, 0.176, 0.227);
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
        assert_eq!(wide.workspace, 964.0);
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

    #[test]
    fn dark_theme_text_and_active_fills_keep_strong_contrast() {
        fn luminance(color: iced::Color) -> f64 {
            fn linear(channel: f32) -> f64 {
                let channel = f64::from(channel);
                if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                }
            }

            0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
        }

        fn contrast(foreground: iced::Color, background: iced::Color) -> f64 {
            let (lighter, darker) = {
                let first = luminance(foreground);
                let second = luminance(background);
                if first >= second {
                    (first, second)
                } else {
                    (second, first)
                }
            };
            (lighter + 0.05) / (darker + 0.05)
        }

        for surface in [tokens::MAIN, tokens::SIDEBAR, tokens::SURFACE, tokens::CARD] {
            assert!(contrast(tokens::TEXT, surface) >= 7.0);
            assert!(contrast(tokens::MUTED, surface) >= 7.0);
            assert!(contrast(tokens::ACCENT, surface) >= 7.0);
        }
        assert!(contrast(tokens::TEXT, tokens::ACCENT_FILL) >= 4.5);
        assert!(contrast(tokens::TEXT, tokens::DANGER) >= 4.5);
    }
}
