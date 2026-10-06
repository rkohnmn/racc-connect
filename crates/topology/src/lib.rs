//! Display-topology domain model and its protocol boundary.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod coordinates;
mod diff;
mod domain;
mod identity;

pub use coordinates::{
    letterbox_rect, map_cursor_to_rect, map_to_host_pixel, map_to_macos_points, normalize_pointer,
    windows_absolute, CoordinateError, CursorBitmapGeometry, CursorDrawRect, HostDisplayGeometry,
    HostPhysicalPixel, MacPoint, NormalizedPointer, PointerPolicy, RenderedRect, WindowsAbsolute,
};
pub use diff::{diff_topologies, requires_stream_reset, DisplayChanges, DisplayDiff, TopologyDiff};
pub use domain::{
    is_revision_newer, ApplyTopologyOutcome, Display, DisplayFlags, DisplayId, Topology,
    TopologyError, VirtualDesktopBounds,
};
pub use identity::{assign_display_ids, stable_display_id, DisplayIdentity, IdentityError};

/// Topology and coordinate definitions that do not depend on platform APIs.
pub const TOPOLOGY_MODULE_VERSION: u8 = 1;

#[cfg(test)]
mod coordinate_tables;

#[cfg(test)]
mod property_tests;

#[cfg(test)]
mod error_tests;
