use core::fmt;
use core::num::NonZeroU32;

use racc_proto::{DisplayInfo, ProtoError, TopologyAnnounce, MAX_DISPLAYS, MAX_NAME_BYTES};

/// A validated, nonzero display identifier. Zero is reserved by the wire format.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DisplayId(NonZeroU32);

impl DisplayId {
    /// Creates an identifier, returning None for the reserved zero value.
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the nonzero wire value.
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    pub(crate) const fn from_hash(hash: u32) -> Self {
        let value = if hash == 0 { 1 } else { hash };
        match NonZeroU32::new(value) {
            Some(value) => Self(value),
            None => Self(NonZeroU32::MIN),
        }
    }
}

impl TryFrom<u32> for DisplayId {
    type Error = TopologyError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(TopologyError::ZeroDisplayId)
    }
}

/// Read-only display attributes that are kept separate from wire flag bits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisplayFlags {
    pub(crate) primary: bool,
    pub(crate) active: bool,
    pub(crate) available: bool,
    pub(crate) hdr: bool,
}

impl DisplayFlags {
    /// Creates flags for primary, OS-active, available, and read-only HDR label state.
    pub const fn new(primary: bool, active: bool, available: bool, hdr: bool) -> Self {
        Self {
            primary,
            active,
            available,
            hdr,
        }
    }

    /// Returns whether this is the primary display.
    pub const fn primary(self) -> bool {
        self.primary
    }

    /// Returns whether the OS reports the display as active.
    pub const fn active(self) -> bool {
        self.active
    }

    /// Returns whether the display is available for capture.
    pub const fn available(self) -> bool {
        self.available
    }

    /// Returns the optional read-only HDR label.
    pub const fn hdr(self) -> bool {
        self.hdr
    }

    pub(crate) const fn bits(self) -> u8 {
        (self.primary as u8)
            | ((self.active as u8) << 1)
            | ((self.available as u8) << 2)
            | ((self.hdr as u8) << 3)
    }

    pub(crate) fn from_bits(bits: u8) -> Result<Self, TopologyError> {
        if bits & !0x0f != 0 {
            return Err(TopologyError::InvalidDisplayFlags(bits));
        }
        Ok(Self::new(
            bits & 0x01 != 0,
            bits & 0x02 != 0,
            bits & 0x04 != 0,
            bits & 0x08 != 0,
        ))
    }
}

/// One display in physical pixels, independent of its wire representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Display {
    pub(crate) id: DisplayId,
    pub(crate) name: String,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    pub(crate) scale_milli: u16,
    pub(crate) refresh_mhz: u32,
    pub(crate) flags: DisplayFlags,
}

impl Display {
    /// Creates a display value. Topology validation is performed by Topology::new.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: DisplayId,
        name: impl Into<String>,
        x: i32,
        y: i32,
        width_px: u32,
        height_px: u32,
        scale_milli: u16,
        refresh_mhz: u32,
        flags: DisplayFlags,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            x,
            y,
            width_px,
            height_px,
            scale_milli,
            refresh_mhz,
            flags,
        }
    }

    /// Returns this display's stable identifier.
    pub const fn id(&self) -> DisplayId {
        self.id
    }

    /// Returns the display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the physical-pixel virtual-desktop origin.
    pub const fn origin(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// Returns the physical-pixel size.
    pub const fn size(&self) -> (u32, u32) {
        (self.width_px, self.height_px)
    }

    /// Returns the scale in thousandths, where 1500 represents 150 percent.
    pub const fn scale_milli(&self) -> u16 {
        self.scale_milli
    }

    /// Returns refresh rate in thousandths of a hertz.
    pub const fn refresh_mhz(&self) -> u32 {
        self.refresh_mhz
    }

    /// Returns the display flags.
    pub const fn flags(&self) -> DisplayFlags {
        self.flags
    }

    /// Returns the inclusive rightmost physical-pixel coordinate as i64.
    pub fn rightmost_pixel_x(&self) -> Option<i64> {
        i64::from(self.x).checked_add(i64::from(self.width_px).checked_sub(1)?)
    }

    /// Returns the inclusive bottommost physical-pixel coordinate as i64.
    pub fn bottommost_pixel_y(&self) -> Option<i64> {
        i64::from(self.y).checked_add(i64::from(self.height_px).checked_sub(1)?)
    }
}

/// A validated and deterministically ordered set of displays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Topology {
    revision: u32,
    displays: Vec<Display>,
    streamed_display_id: Option<DisplayId>,
}

impl Topology {
    /// Validates and sorts displays by id before creating a topology.
    pub fn new(
        revision: u32,
        mut displays: Vec<Display>,
        streamed_display_id: Option<DisplayId>,
    ) -> Result<Self, TopologyError> {
        let candidate = Self {
            revision,
            displays: {
                displays.sort_by_key(Display::id);
                displays
            },
            streamed_display_id,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    /// Returns the topology revision.
    pub const fn revision(&self) -> u32 {
        self.revision
    }

    /// Returns displays in deterministic ascending-id order.
    pub fn displays(&self) -> &[Display] {
        &self.displays
    }

    /// Returns the currently streamed display, if any.
    pub const fn streamed_display_id(&self) -> Option<DisplayId> {
        self.streamed_display_id
    }

    /// Returns the primary display id, if exactly one primary is present.
    pub fn primary_display_id(&self) -> Option<DisplayId> {
        self.displays
            .iter()
            .find(|display| display.flags.primary)
            .map(Display::id)
    }

    /// Validates all bounded topology invariants.
    pub fn validate(&self) -> Result<(), TopologyError> {
        if self.displays.len() > MAX_DISPLAYS {
            return Err(TopologyError::TooManyDisplays {
                actual: self.displays.len(),
                maximum: MAX_DISPLAYS,
            });
        }

        let mut primary_count = 0usize;
        for (index, display) in self.displays.iter().enumerate() {
            if display.width_px == 0 || display.height_px == 0 {
                return Err(TopologyError::ZeroDisplaySize(display.id));
            }
            let name_bytes = display.name.len();
            if name_bytes > MAX_NAME_BYTES {
                return Err(TopologyError::NameTooLong {
                    bytes: name_bytes,
                    maximum: MAX_NAME_BYTES,
                });
            }
            if self.displays[..index]
                .iter()
                .any(|earlier| earlier.id == display.id)
            {
                return Err(TopologyError::DuplicateDisplayId(display.id));
            }
            if display.flags.primary {
                primary_count += 1;
            }
        }

        if primary_count > 1 {
            return Err(TopologyError::MultiplePrimaryDisplays);
        }
        if let Some(active_id) = self.streamed_display_id {
            if !self.displays.iter().any(|display| display.id == active_id) {
                return Err(TopologyError::StreamedDisplayNotFound(active_id));
            }
        }
        Ok(())
    }

    /// Converts validated domain data to the v0 wire topology structure.
    pub fn to_proto(&self) -> Result<TopologyAnnounce, TopologyError> {
        self.validate()?;
        let mut displays = Vec::with_capacity(self.displays.len());
        for display in &self.displays {
            displays.push(DisplayInfo {
                display_id: display.id.get(),
                name: display.name.clone(),
                x: display.x,
                y: display.y,
                width_px: display.width_px,
                height_px: display.height_px,
                scale_milli: display.scale_milli,
                refresh_mhz: display.refresh_mhz,
                flags: display.flags.bits(),
            });
        }
        Ok(TopologyAnnounce {
            topology_rev: self.revision,
            active_display_id: self.streamed_display_id.map_or(0, DisplayId::get),
            displays,
        })
    }

    /// Validates and converts a received v0 wire topology.
    pub fn from_proto(value: &TopologyAnnounce) -> Result<Self, TopologyError> {
        if value.displays.len() > MAX_DISPLAYS {
            return Err(TopologyError::TooManyDisplays {
                actual: value.displays.len(),
                maximum: MAX_DISPLAYS,
            });
        }

        let mut displays = Vec::with_capacity(value.displays.len());
        for wire in &value.displays {
            let id = DisplayId::try_from(wire.display_id)?;
            if wire.width_px == 0 || wire.height_px == 0 {
                return Err(TopologyError::ZeroDisplaySize(id));
            }
            let name_bytes = wire.name.len();
            if name_bytes > MAX_NAME_BYTES {
                return Err(TopologyError::NameTooLong {
                    bytes: name_bytes,
                    maximum: MAX_NAME_BYTES,
                });
            }
            displays.push(Display::new(
                id,
                wire.name.clone(),
                wire.x,
                wire.y,
                wire.width_px,
                wire.height_px,
                wire.scale_milli,
                wire.refresh_mhz,
                DisplayFlags::from_bits(wire.flags)?,
            ));
        }

        let streamed_display_id = if value.active_display_id == 0 {
            None
        } else {
            Some(DisplayId::try_from(value.active_display_id)?)
        };
        Self::new(value.topology_rev, displays, streamed_display_id)
    }

    /// Applies an incoming topology only when its revision is serial-newer.
    pub fn apply_update(&mut self, incoming: Self) -> ApplyTopologyOutcome {
        if is_revision_newer(incoming.revision, self.revision) {
            *self = incoming;
            ApplyTopologyOutcome::Applied
        } else {
            ApplyTopologyOutcome::IgnoredStale
        }
    }

    /// Computes bounds of all available displays; gaps are included.
    pub fn virtual_desktop_bounds(&self) -> Result<Option<VirtualDesktopBounds>, TopologyError> {
        let mut left = i64::MAX;
        let mut top = i64::MAX;
        let mut right_exclusive = i64::MIN;
        let mut bottom_exclusive = i64::MIN;
        let mut any = false;

        for display in self
            .displays
            .iter()
            .filter(|display| display.flags.available)
        {
            any = true;
            let x = i64::from(display.x);
            let y = i64::from(display.y);
            let right = x
                .checked_add(i64::from(display.width_px))
                .ok_or(TopologyError::BoundsOverflow)?;
            let bottom = y
                .checked_add(i64::from(display.height_px))
                .ok_or(TopologyError::BoundsOverflow)?;
            left = left.min(x);
            top = top.min(y);
            right_exclusive = right_exclusive.max(right);
            bottom_exclusive = bottom_exclusive.max(bottom);
        }

        if !any {
            return Ok(None);
        }

        let width = right_exclusive
            .checked_sub(left)
            .ok_or(TopologyError::BoundsOverflow)?;
        let height = bottom_exclusive
            .checked_sub(top)
            .ok_or(TopologyError::BoundsOverflow)?;
        let width = u32::try_from(width).map_err(|_| TopologyError::BoundsOverflow)?;
        let height = u32::try_from(height).map_err(|_| TopologyError::BoundsOverflow)?;

        Ok(Some(VirtualDesktopBounds {
            left,
            top,
            width,
            height,
        }))
    }
}

/// Result of applying a newer topology revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyTopologyOutcome {
    /// The incoming topology replaced the stored value.
    Applied,
    /// The incoming topology was equal, stale, or serial-ambiguous.
    IgnoredStale,
}

/// Physical-pixel bounds of the available virtual desktop. Gaps are included.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualDesktopBounds {
    left: i64,
    top: i64,
    width: u32,
    height: u32,
}

impl VirtualDesktopBounds {
    /// Returns the left edge in host physical pixels.
    pub const fn left(self) -> i64 {
        self.left
    }

    /// Returns the top edge in host physical pixels.
    pub const fn top(self) -> i64 {
        self.top
    }

    /// Returns the width in physical pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the height in physical pixels.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Returns the inclusive rightmost pixel coordinate.
    pub fn rightmost_pixel_x(self) -> Option<i64> {
        self.left.checked_add(i64::from(self.width).checked_sub(1)?)
    }

    /// Returns the inclusive bottommost pixel coordinate.
    pub fn bottommost_pixel_y(self) -> Option<i64> {
        self.top.checked_add(i64::from(self.height).checked_sub(1)?)
    }
}

/// Typed validation and conversion errors for topology data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopologyError {
    /// A wire display id used the reserved zero value.
    ZeroDisplayId,
    /// Two displays used the same id.
    DuplicateDisplayId(DisplayId),
    /// A display width or height was zero.
    ZeroDisplaySize(DisplayId),
    /// The display vector exceeded the protocol limit.
    TooManyDisplays {
        /// Number of supplied displays.
        actual: usize,
        /// Protocol maximum.
        maximum: usize,
    },
    /// A UTF-8 display name exceeded the wire byte limit.
    NameTooLong {
        /// Encoded UTF-8 byte length.
        bytes: usize,
        /// Maximum encoded length.
        maximum: usize,
    },
    /// More than one display was marked primary.
    MultiplePrimaryDisplays,
    /// The streamed display id did not appear in the display list.
    StreamedDisplayNotFound(DisplayId),
    /// Reserved wire flag bits were set.
    InvalidDisplayFlags(u8),
    /// An arithmetic operation or bounds representation overflowed.
    BoundsOverflow,
    /// Protocol encoding or decoding rejected the value.
    Protocol(ProtoError),
}

impl fmt::Display for TopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDisplayId => formatter.write_str("display id zero is reserved"),
            Self::DuplicateDisplayId(id) => write!(formatter, "duplicate display id {}", id.get()),
            Self::ZeroDisplaySize(id) => write!(formatter, "display {} has zero size", id.get()),
            Self::TooManyDisplays { actual, maximum } => {
                write!(formatter, "{actual} displays exceed maximum {maximum}")
            }
            Self::NameTooLong { bytes, maximum } => {
                write!(
                    formatter,
                    "display name has {bytes} bytes; maximum is {maximum}"
                )
            }
            Self::MultiplePrimaryDisplays => formatter.write_str("multiple primary displays"),
            Self::StreamedDisplayNotFound(id) => {
                write!(
                    formatter,
                    "streamed display {} is not in topology",
                    id.get()
                )
            }
            Self::InvalidDisplayFlags(bits) => {
                write!(formatter, "invalid display flags {bits:#04x}")
            }
            Self::BoundsOverflow => formatter.write_str("virtual desktop bounds overflow"),
            Self::Protocol(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TopologyError {}

impl From<ProtoError> for TopologyError {
    fn from(value: ProtoError) -> Self {
        Self::Protocol(value)
    }
}

/// Returns whether candidate follows current under u32 serial arithmetic.
///
/// A difference of exactly 2^31 is ambiguous and is treated as not newer.
pub const fn is_revision_newer(candidate: u32, current: u32) -> bool {
    let distance = candidate.wrapping_sub(current);
    distance != 0 && distance < (1_u32 << 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_proto::{ControlMessage, DisplayInfo};

    fn display(id: u32, name: &str, x: i32, width: u32, flags: DisplayFlags) -> Display {
        Display::new(
            DisplayId::new(id).unwrap_or_else(|| unreachable!()),
            name,
            x,
            0,
            width,
            1080,
            1000,
            60000,
            flags,
        )
    }

    fn wire_display(id: u32, name: &str, flags: u8) -> DisplayInfo {
        DisplayInfo {
            display_id: id,
            name: name.to_owned(),
            x: -1920,
            y: 0,
            width_px: 1920,
            height_px: 1080,
            scale_milli: 1000,
            refresh_mhz: 60000,
            flags,
        }
    }

    #[test]
    fn validates_all_domain_and_wire_errors() {
        let zero_id = TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 0,
            displays: vec![wire_display(0, "D0", 4)],
        };
        assert_eq!(
            Topology::from_proto(&zero_id),
            Err(TopologyError::ZeroDisplayId)
        );

        let duplicate = Topology::new(
            1,
            vec![
                display(
                    1,
                    "D1",
                    0,
                    1920,
                    DisplayFlags::new(false, true, true, false),
                ),
                display(
                    1,
                    "D1 again",
                    1920,
                    1920,
                    DisplayFlags::new(false, true, true, false),
                ),
            ],
            None,
        );
        assert_eq!(
            duplicate,
            Err(TopologyError::DuplicateDisplayId(
                DisplayId::new(1).expect("nonzero")
            ))
        );

        assert_eq!(
            Topology::new(
                1,
                vec![display(
                    1,
                    "zero",
                    0,
                    0,
                    DisplayFlags::new(false, false, true, false)
                )],
                None
            ),
            Err(TopologyError::ZeroDisplaySize(
                DisplayId::new(1).expect("nonzero")
            ))
        );

        let too_many = (1..=MAX_DISPLAYS + 1)
            .map(|id| display(id as u32, "D", id as i32, 1, DisplayFlags::default()))
            .collect();
        assert!(matches!(
            Topology::new(1, too_many, None),
            Err(TopologyError::TooManyDisplays { .. })
        ));

        let long_name = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(matches!(
            Topology::new(
                1,
                vec![display(1, &long_name, 0, 1, DisplayFlags::default())],
                None
            ),
            Err(TopologyError::NameTooLong { .. })
        ));

        assert_eq!(
            Topology::new(
                1,
                vec![
                    display(1, "D1", 0, 1, DisplayFlags::new(true, false, true, false)),
                    display(2, "D2", 1, 1, DisplayFlags::new(true, false, true, false)),
                ],
                None,
            ),
            Err(TopologyError::MultiplePrimaryDisplays)
        );

        let missing_stream = DisplayId::new(99).expect("nonzero");
        assert_eq!(
            Topology::new(1, Vec::new(), Some(missing_stream)),
            Err(TopologyError::StreamedDisplayNotFound(missing_stream))
        );

        let bad_flags = TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 0,
            displays: vec![wire_display(1, "D1", 0x80)],
        };
        assert_eq!(
            Topology::from_proto(&bad_flags),
            Err(TopologyError::InvalidDisplayFlags(0x80))
        );

        let zero_size = TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 0,
            displays: vec![DisplayInfo {
                width_px: 0,
                ..wire_display(1, "D1", 4)
            }],
        };
        assert_eq!(
            Topology::from_proto(&zero_size),
            Err(TopologyError::ZeroDisplaySize(
                DisplayId::new(1).expect("nonzero")
            ))
        );

        let too_many_wire = TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 0,
            displays: (1..=MAX_DISPLAYS + 1)
                .map(|id| wire_display(id as u32, "D", 4))
                .collect(),
        };
        assert!(matches!(
            Topology::from_proto(&too_many_wire),
            Err(TopologyError::TooManyDisplays { .. })
        ));

        let missing_active = TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 99,
            displays: vec![wire_display(1, "D1", 4)],
        };
        assert_eq!(
            Topology::from_proto(&missing_active),
            Err(TopologyError::StreamedDisplayNotFound(
                DisplayId::new(99).expect("nonzero")
            ))
        );
    }

    #[test]
    fn topology_sorts_deterministically_and_bounds_empty_available_set() {
        let topology = Topology::new(
            7,
            vec![
                display(2, "D2", 0, 10, DisplayFlags::default()),
                display(1, "D1", -10, 10, DisplayFlags::default()),
            ],
            None,
        )
        .expect("valid topology");
        assert_eq!(
            topology
                .displays()
                .iter()
                .map(Display::id)
                .collect::<Vec<_>>(),
            vec![
                DisplayId::new(1).expect("nonzero"),
                DisplayId::new(2).expect("nonzero")
            ]
        );
        assert_eq!(topology.virtual_desktop_bounds(), Ok(None));
    }

    #[test]
    fn topology_proto_conversion_has_negative_origin_golden_frame() {
        let topology = Topology::new(
            1,
            vec![display(
                1,
                "D1",
                -1920,
                1920,
                DisplayFlags::new(true, true, true, false),
            )],
            Some(DisplayId::new(1).expect("nonzero")),
        )
        .expect("valid topology");
        let proto = topology.to_proto().expect("wire conversion");
        let mut frame = Vec::new();
        ControlMessage::TopologyAnnounce(proto.clone())
            .encode_frame(&mut frame)
            .expect("encode golden frame");
        assert_eq!(
            frame,
            vec![
                0x28, 0x00, 0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
                0x01, 0x00, 0x00, 0x00, 0x02, 0x44, 0x31, 0x80, 0xf8, 0xff, 0xff, 0x00, 0x00, 0x00,
                0x00, 0x80, 0x07, 0x00, 0x00, 0x38, 0x04, 0x00, 0x00, 0xe8, 0x03, 0x60, 0xea, 0x00,
                0x00, 0x07,
            ]
        );
        assert_eq!(Topology::from_proto(&proto), Ok(topology.clone()));
        assert_eq!(
            ControlMessage::decode_body(&frame[4..]),
            Ok(ControlMessage::TopologyAnnounce(proto))
        );
    }

    #[test]
    fn revision_serial_arithmetic_and_update_ignore_stale_or_ambiguous_values() {
        assert!(is_revision_newer(1, u32::MAX - 1));
        assert!(!is_revision_newer(u32::MAX - 1, 1));
        assert!(!is_revision_newer(5, 5));
        assert!(!is_revision_newer(0x8000_0005, 5));

        let mut current = Topology::new(5, Vec::new(), None).expect("empty topology");
        let stale = Topology::new(4, Vec::new(), None).expect("empty topology");
        let next = Topology::new(6, Vec::new(), None).expect("empty topology");
        assert_eq!(
            current.apply_update(stale),
            ApplyTopologyOutcome::IgnoredStale
        );
        assert_eq!(current.revision(), 5);
        assert_eq!(current.apply_update(next), ApplyTopologyOutcome::Applied);
        assert_eq!(current.revision(), 6);

        let mut wrapping = Topology::new(u32::MAX - 1, Vec::new(), None).expect("topology");
        let after_wrap = Topology::new(1, Vec::new(), None).expect("topology");
        assert_eq!(
            wrapping.apply_update(after_wrap),
            ApplyTopologyOutcome::Applied
        );
    }
}
