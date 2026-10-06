use crate::{Display, DisplayId, Topology};

/// Set of fields that changed on one display.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisplayChanges {
    /// The human-readable name changed.
    pub name: bool,
    /// The virtual-desktop origin changed.
    pub origin: bool,
    /// The physical size changed.
    pub size: bool,
    /// The scale factor changed.
    pub scale: bool,
    /// The refresh rate changed.
    pub refresh: bool,
    /// The primary flag changed.
    pub primary: bool,
    /// The OS-active flag changed.
    pub active: bool,
    /// The availability flag changed.
    pub available: bool,
    /// The read-only HDR label changed.
    pub hdr: bool,
}

impl DisplayChanges {
    /// Returns true when no compared display field changed.
    pub const fn is_empty(self) -> bool {
        !(self.name
            || self.origin
            || self.size
            || self.scale
            || self.refresh
            || self.primary
            || self.active
            || self.available
            || self.hdr)
    }
}

/// One changed display and its exact before/after values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayDiff {
    /// Stable id of the changed display.
    pub id: DisplayId,
    /// Original display value.
    pub before: Display,
    /// Updated display value.
    pub after: Display,
    /// Exact set of changed fields.
    pub changes: DisplayChanges,
}

/// Changes between two validated topology snapshots.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TopologyDiff {
    /// Displays present only in the new topology, in id order.
    pub added: Vec<Display>,
    /// Displays present only in the old topology, in id order.
    pub removed: Vec<Display>,
    /// Same-id displays whose fields changed, in id order.
    pub changed: Vec<DisplayDiff>,
    /// Whether the currently streamed display id changed.
    pub active_changed: bool,
    /// Whether the primary display id changed.
    pub primary_changed: bool,
}

impl TopologyDiff {
    /// Returns true when no topology state changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && !self.active_changed
            && !self.primary_changed
    }
}

/// Computes exact display-set and field differences between two topologies.
pub fn diff_topologies(old: &Topology, new: &Topology) -> TopologyDiff {
    let mut diff = TopologyDiff {
        active_changed: old.streamed_display_id() != new.streamed_display_id(),
        primary_changed: old.primary_display_id() != new.primary_display_id(),
        ..TopologyDiff::default()
    };

    let old_displays = old.displays();
    let new_displays = new.displays();
    let mut old_index = 0usize;
    let mut new_index = 0usize;

    while old_index < old_displays.len() && new_index < new_displays.len() {
        let before = &old_displays[old_index];
        let after = &new_displays[new_index];
        if before.id() < after.id() {
            diff.removed.push(before.clone());
            old_index += 1;
        } else if after.id() < before.id() {
            diff.added.push(after.clone());
            new_index += 1;
        } else {
            let changes = compare_display(before, after);
            if !changes.is_empty() {
                diff.changed.push(DisplayDiff {
                    id: before.id(),
                    before: before.clone(),
                    after: after.clone(),
                    changes,
                });
            }
            old_index += 1;
            new_index += 1;
        }
    }

    diff.removed
        .extend(old_displays[old_index..].iter().cloned());
    diff.added.extend(new_displays[new_index..].iter().cloned());
    diff
}

/// Determines whether the current stream needs a reset after a topology diff.
pub fn requires_stream_reset(diff: &TopologyDiff, current_display: Option<DisplayId>) -> bool {
    let Some(current_display) = current_display else {
        return false;
    };
    if diff
        .removed
        .iter()
        .any(|display| display.id() == current_display)
    {
        return true;
    }
    diff.changed.iter().any(|change| {
        change.id == current_display
            && (change.changes.size
                || change.changes.refresh
                || change.changes.scale
                || !change.after.flags().available())
    })
}

fn compare_display(before: &Display, after: &Display) -> DisplayChanges {
    let before_flags = before.flags();
    let after_flags = after.flags();
    DisplayChanges {
        name: before.name() != after.name(),
        origin: before.origin() != after.origin(),
        size: before.size() != after.size(),
        scale: before.scale_milli() != after.scale_milli(),
        refresh: before.refresh_mhz() != after.refresh_mhz(),
        primary: before_flags.primary() != after_flags.primary(),
        active: before_flags.active() != after_flags.active(),
        available: before_flags.available() != after_flags.available(),
        hdr: before_flags.hdr() != after_flags.hdr(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DisplayFlags;

    fn id(raw: u32) -> DisplayId {
        DisplayId::new(raw).unwrap_or_else(|| unreachable!())
    }

    fn display(
        raw: u32,
        x: i32,
        width: u32,
        refresh: u32,
        scale: u16,
        flags: DisplayFlags,
    ) -> Display {
        Display::new(
            id(raw),
            format!("D{raw}"),
            x,
            0,
            width,
            1080,
            scale,
            refresh,
            flags,
        )
    }

    #[test]
    fn identical_topology_has_empty_diff() {
        let topology = Topology::new(
            4,
            vec![display(
                2,
                0,
                1920,
                60000,
                1000,
                DisplayFlags::new(true, true, true, false),
            )],
            Some(id(2)),
        )
        .expect("valid topology");
        assert!(diff_topologies(&topology, &topology).is_empty());
    }

    #[test]
    fn reports_exact_added_removed_and_changed_fields() {
        let old = Topology::new(
            1,
            vec![
                display(
                    1,
                    0,
                    1920,
                    60000,
                    1000,
                    DisplayFlags::new(true, true, true, false),
                ),
                display(
                    3,
                    1920,
                    1920,
                    60000,
                    1000,
                    DisplayFlags::new(false, true, true, false),
                ),
            ],
            Some(id(3)),
        )
        .expect("valid old topology");
        let new = Topology::new(
            2,
            vec![
                display(
                    1,
                    -1920,
                    1280,
                    75000,
                    1500,
                    DisplayFlags::new(false, true, true, true),
                ),
                display(
                    2,
                    0,
                    1920,
                    60000,
                    1000,
                    DisplayFlags::new(true, true, true, false),
                ),
            ],
            Some(id(2)),
        )
        .expect("valid new topology");

        let diff = diff_topologies(&old, &new);
        assert_eq!(
            diff.removed.iter().map(Display::id).collect::<Vec<_>>(),
            vec![id(3)]
        );
        assert_eq!(
            diff.added.iter().map(Display::id).collect::<Vec<_>>(),
            vec![id(2)]
        );
        assert!(diff.active_changed);
        assert!(diff.primary_changed);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(
            diff.changed[0].changes,
            DisplayChanges {
                origin: true,
                size: true,
                scale: true,
                refresh: true,
                primary: true,
                hdr: true,
                ..DisplayChanges::default()
            }
        );
    }

    #[test]
    fn stream_reset_is_required_only_for_stream_geometry_or_availability() {
        let old = Topology::new(
            1,
            vec![display(
                1,
                0,
                1920,
                60000,
                1000,
                DisplayFlags::new(true, true, true, false),
            )],
            Some(id(1)),
        )
        .expect("valid old topology");
        let moved = Topology::new(
            2,
            vec![display(
                1,
                100,
                1920,
                60000,
                1000,
                DisplayFlags::new(true, true, true, false),
            )],
            Some(id(1)),
        )
        .expect("valid moved topology");
        let resized = Topology::new(
            3,
            vec![display(
                1,
                0,
                1280,
                60000,
                1000,
                DisplayFlags::new(true, true, true, false),
            )],
            Some(id(1)),
        )
        .expect("valid resized topology");

        assert!(!requires_stream_reset(
            &diff_topologies(&old, &moved),
            Some(id(1))
        ));
        assert!(requires_stream_reset(
            &diff_topologies(&old, &resized),
            Some(id(1))
        ));
        assert!(!requires_stream_reset(
            &diff_topologies(&old, &resized),
            None
        ));

        let unavailable = Topology::new(
            4,
            vec![display(
                1,
                0,
                1920,
                60000,
                1000,
                DisplayFlags::new(true, true, false, false),
            )],
            Some(id(1)),
        )
        .expect("valid topology");
        assert!(requires_stream_reset(
            &diff_topologies(&old, &unavailable),
            Some(id(1))
        ));
    }
}
