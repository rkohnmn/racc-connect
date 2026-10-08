//! Conservative policy shared by the macOS host adapter and its fake lifecycle checks.

use racc_proto::CursorUpdate;
use racc_session::{bounded_stream_dimensions_with_height, StreamDimensions};

/// Until sustained hardware tests pass, the Mac host advertises and serves at most 720p30.
pub(crate) const MAC_HOST_DEFAULT_MAX_HEIGHT: u16 = 720;

/// Computes the stream output dimensions used by both capture and the host state machine.
pub(crate) fn stream_dimensions(width: u32, height: u32) -> Option<StreamDimensions> {
    bounded_stream_dimensions_with_height(width, height, MAC_HOST_DEFAULT_MAX_HEIGHT)
}

/// Converts successfully sent UDP payload bytes over an elapsed microsecond interval to kbps.
pub(crate) fn measured_bitrate_kbps(sent_bytes: u64, elapsed_us: u64) -> u32 {
    if elapsed_us == 0 {
        return 0;
    }
    u32::try_from(u128::from(sent_bytes).saturating_mul(8_000) / u128::from(elapsed_us))
        .unwrap_or(u32::MAX)
}

/// Creates a protocol update that explicitly suppresses the viewer cursor for Mac capture.
pub(crate) const fn hidden_cursor_update(epoch: u16) -> CursorUpdate {
    CursorUpdate {
        epoch,
        shape_id: 0,
        x: 0,
        y: 0,
        visible: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_stream_default_is_capped_at_720_and_preserves_physical_topology() {
        assert_eq!(
            stream_dimensions(3840, 2160),
            Some(StreamDimensions {
                width: 1280,
                height: 720,
            })
        );
        assert_eq!(
            stream_dimensions(2880, 1800),
            Some(StreamDimensions {
                width: 1152,
                height: 720,
            })
        );
        assert_eq!(
            stream_dimensions(1280, 720),
            Some(StreamDimensions {
                width: 1280,
                height: 720
            })
        );
    }

    #[test]
    fn measured_bitrate_uses_payload_bytes_and_handles_zero_or_saturation() {
        assert_eq!(measured_bitrate_kbps(1_000_000, 1_000_000), 8_000);
        assert_eq!(measured_bitrate_kbps(0, 1_000_000), 0);
        assert_eq!(measured_bitrate_kbps(1_000, 0), 0);
        assert_eq!(measured_bitrate_kbps(u64::MAX, 1), u32::MAX);
    }

    #[test]
    fn mac_host_suppresses_separate_cursor_for_each_stream_epoch() {
        let update = hidden_cursor_update(9);
        assert_eq!(update.epoch, 9);
        assert_eq!(update.shape_id, 0);
        assert!(!update.visible);
    }
}
