//! Small, deterministic helpers for polling platform display topology.

use racc_topology::{Display, Topology, TopologyError};
use std::time::Duration;

/// Maximum delay between platform display-set observations while hosting.
pub(super) const TOPOLOGY_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Returns a newer topology when the platform's active display list changed.
pub(super) fn changed_topology(
    current: &Topology,
    displays: Vec<Display>,
) -> Result<Option<Topology>, TopologyError> {
    let candidate = Topology::new(current.revision(), displays, None)?;
    if current.displays() == candidate.displays() {
        return Ok(None);
    }
    let revision = current.revision().wrapping_add(1).max(1);
    Topology::new(revision, candidate.displays().to_vec(), None).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_topology::{DisplayFlags, DisplayId};

    fn display(id: u32, x: i32) -> Display {
        Display::new(
            DisplayId::new(id).unwrap_or_else(|| unreachable!("test display id")),
            format!("Display {id}"),
            x,
            0,
            1920,
            1080,
            1000,
            60_000,
            DisplayFlags::new(id == 1, true, true, false),
        )
    }

    #[test]
    fn unchanged_displays_do_not_advance_revision() {
        let current = Topology::new(7, vec![display(1, 0)], None)
            .unwrap_or_else(|_| unreachable!("test topology"));
        assert_eq!(changed_topology(&current, vec![display(1, 0)]), Ok(None));
    }

    #[test]
    fn display_add_remove_and_geometry_change_advance_revision_once() {
        let current = Topology::new(7, vec![display(1, 0)], None)
            .unwrap_or_else(|_| unreachable!("test topology"));
        let updated = changed_topology(&current, vec![display(1, -1920), display(2, 0)])
            .unwrap_or_else(|_| unreachable!("valid display update"))
            .unwrap_or_else(|| unreachable!("changed display list"));
        assert_eq!(updated.revision(), 8);
        assert_eq!(updated.displays().len(), 2);
        let wrapped = Topology::new(u32::MAX, vec![display(1, 0)], None)
            .unwrap_or_else(|_| unreachable!("test topology"));
        let updated = changed_topology(&wrapped, vec![display(1, 1)])
            .unwrap_or_else(|_| unreachable!("valid display update"))
            .unwrap_or_else(|| unreachable!("changed display list"));
        assert_eq!(updated.revision(), 1);
    }
}
