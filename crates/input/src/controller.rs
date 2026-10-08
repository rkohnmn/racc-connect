//! Host-side event validation, session gate, and stuck-input protection.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use racc_proto::{InputEvent, InputEventKind};
use racc_topology::{
    map_to_host_pixel, CoordinateError, DisplayId, HostPhysicalPixel, Topology,
    VirtualDesktopBounds,
};

use crate::hid_to_windows_scancode;

/// Generous per-session ceiling for inbound input events.
pub const MAX_INPUT_EVENTS_PER_SECOND: u32 = 2_000;
const VALID_MODIFIER_MASK: u8 = 0x0f;

/// Windows absolute-pointer API selected by the host's validated local setting.
///
/// The virtual-desktop SendInput method remains the provisional default until the owner
/// measures corner and center accuracy on the target monitors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PointerInjectionMethod {
    /// Send an absolute SendInput move normalized to the complete virtual desktop.
    #[default]
    VirtualDesktopAbsolute,
    /// Place the system pointer at the validated host screen pixel with SetCursorPos.
    SetCursorPos,
}

impl PointerInjectionMethod {
    /// Parses the exact values accepted for RACC_POINTER_INJECTION_METHOD.
    pub fn from_setting(value: &str) -> Option<Self> {
        match value {
            "virtual-desktop-absolute" => Some(Self::VirtualDesktopAbsolute),
            "set-cursor-pos" => Some(Self::SetCursorPos),
            _ => None,
        }
    }
}

/// Validated host operation passed to a platform injector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputCommand {
    /// Press or release a HID key after mapping it to a Windows scan identity.
    Key {
        /// USB HID keyboard-page usage, validated against the bounded mapping table.
        hid_usage: u16,
        /// True for key down, false for key up.
        pressed: bool,
    },
    /// Move to an absolute host physical pixel with the matching virtual-desktop bounds.
    MouseMoveAbsolute {
        /// Absolute host physical pixel on the selected display.
        pixel: HostPhysicalPixel,
        /// Full host virtual-desktop physical bounds, including any negative origin.
        virtual_desktop: VirtualDesktopBounds,
    },
    /// Move by raw relative device units.
    MouseMoveRelative {
        /// Horizontal delta.
        dx: i16,
        /// Vertical delta.
        dy: i16,
    },
    /// Press or release a supported mouse button (1 through 5).
    MouseButton {
        /// Protocol button number.
        button: u8,
        /// True for down, false for up.
        pressed: bool,
    },
    /// Apply a wheel delta in 1/120 notch units.
    Wheel {
        /// Horizontal wheel delta.
        dx: i16,
        /// Vertical wheel delta.
        dy: i16,
    },
}

/// OS or fake backend interface for already-validated input commands.
pub trait InputInjector {
    /// Injects one command. Implementations must not log its contents.
    fn inject(&mut self, command: InputCommand) -> Result<(), InputError>;
}

/// Typed validation, session, rate-limit, coordinate, and injection errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    /// No authorized input session is active.
    NoAuthorizedSession,
    /// The incoming events epoch is not the active stream epoch.
    StaleEpoch {
        /// Epoch carried by the input event.
        received: u16,
        /// Current authorized epoch.
        active: u16,
    },
    /// Absolute motion targets a display other than the active display.
    StaleDisplay {
        /// Display id carried by the input event.
        received: u32,
        /// Current active display id.
        active: u32,
    },
    /// Absolute motion names a display absent from the current topology.
    UnknownDisplay(u32),
    /// Absolute motion targets a display that is no longer available.
    DisplayUnavailable(u32),
    /// Topology does not contain valid available virtual-desktop bounds.
    InvalidTopology,
    /// A pointer coordinate could not be mapped safely.
    Coordinate(CoordinateError),
    /// A key event contains modifier bits outside the four defined modifiers.
    InvalidModifiers(u8),
    /// A keyboard-page usage is not in the bounded host mapping table.
    UnsupportedHidUsage(u16),
    /// A mouse button number falls outside 1 through 5.
    InvalidMouseButton(u8),
    /// Input event rate exceeded the configured per-second ceiling.
    RateLimited,
    /// Required operating-system permission for this injection is absent.
    PermissionMissing,
    /// The platform injection API did not accept the complete operation.
    OsInjectionFailed,
    /// The bounded fake backend command record is full.
    FakeRecordingLimit,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAuthorizedSession => {
                formatter.write_str("no authorized input session is active")
            }
            Self::StaleEpoch { received, active } => {
                write!(
                    formatter,
                    "stale input epoch {received}; active epoch is {active}"
                )
            }
            Self::StaleDisplay { received, active } => {
                write!(
                    formatter,
                    "stale input display {received}; active display is {active}"
                )
            }
            Self::UnknownDisplay(id) => write!(formatter, "input display {id} is not in topology"),
            Self::DisplayUnavailable(id) => write!(formatter, "input display {id} is unavailable"),
            Self::InvalidTopology => formatter.write_str("topology has no usable virtual desktop"),
            Self::Coordinate(error) => {
                write!(formatter, "input coordinate mapping failed: {error}")
            }
            Self::InvalidModifiers(bits) => write!(formatter, "invalid modifier bits 0x{bits:02x}"),
            Self::UnsupportedHidUsage(usage) => {
                write!(formatter, "unsupported HID usage 0x{usage:04x}")
            }
            Self::InvalidMouseButton(button) => write!(formatter, "invalid mouse button {button}"),
            Self::RateLimited => formatter.write_str("input event rate limit exceeded"),
            Self::PermissionMissing => {
                formatter.write_str("required operating-system permission is missing")
            }
            Self::OsInjectionFailed => {
                formatter.write_str("the operating system rejected input injection")
            }
            Self::FakeRecordingLimit => {
                formatter.write_str("fake input record reached its fixed limit")
            }
        }
    }
}

impl Error for InputError {}

/// Fixed-window input rate limiter with an injectable monotonic millisecond clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputRateLimiter {
    window_start_ms: Option<u64>,
    accepted_in_window: u32,
    max_events_per_second: u32,
}

impl Default for InputRateLimiter {
    fn default() -> Self {
        Self::new(MAX_INPUT_EVENTS_PER_SECOND)
    }
}

impl InputRateLimiter {
    /// Creates a rate limiter. A zero ceiling is clamped to one event per second.
    pub const fn new(max_events_per_second: u32) -> Self {
        Self {
            window_start_ms: None,
            accepted_in_window: 0,
            max_events_per_second: if max_events_per_second == 0 {
                1
            } else {
                max_events_per_second
            },
        }
    }

    /// Records one event timestamp or returns `RateLimited` when the current window is full.
    pub fn check(&mut self, now_ms: u64) -> Result<(), InputError> {
        let Some(start) = self.window_start_ms else {
            self.window_start_ms = Some(now_ms);
            self.accepted_in_window = 1;
            return Ok(());
        };
        if now_ms < start || now_ms.saturating_sub(start) >= 1_000 {
            self.window_start_ms = Some(now_ms);
            self.accepted_in_window = 1;
            return Ok(());
        }
        if self.accepted_in_window >= self.max_events_per_second {
            return Err(InputError::RateLimited);
        }
        self.accepted_in_window += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveSession {
    epoch: u16,
    display_id: DisplayId,
}

/// Authorization and release tracker around a platform injector.
///
/// Call this on a dedicated helper input worker. Network threads should enqueue bounded work
/// instead of calling a potentially blocking OS API directly.
pub struct InputInjectionController<I: InputInjector> {
    injector: I,
    session: Option<ActiveSession>,
    pressed_keys: BTreeSet<u16>,
    pressed_buttons: BTreeSet<u8>,
    rate_limiter: InputRateLimiter,
}

impl<I: InputInjector> InputInjectionController<I> {
    /// Creates an inactive controller with the standard 2,000 event/second ceiling.
    pub fn new(injector: I) -> Self {
        Self::with_rate_limit(injector, MAX_INPUT_EVENTS_PER_SECOND)
    }

    /// Creates an inactive controller with a configurable ceiling (primarily for tests).
    pub fn with_rate_limit(injector: I, max_events_per_second: u32) -> Self {
        Self {
            injector,
            session: None,
            pressed_keys: BTreeSet::new(),
            pressed_buttons: BTreeSet::new(),
            rate_limiter: InputRateLimiter::new(max_events_per_second),
        }
    }

    /// Opens an input session only when the caller confirms authorization.
    pub fn begin_session(
        &mut self,
        epoch: u16,
        display_id: DisplayId,
        authorized: bool,
    ) -> Result<(), InputError> {
        let release_result = self.release_all();
        self.session = None;
        release_result?;
        if !authorized {
            return Err(InputError::NoAuthorizedSession);
        }
        self.session = Some(ActiveSession { epoch, display_id });
        self.rate_limiter = InputRateLimiter::new(self.rate_limiter.max_events_per_second);
        Ok(())
    }

    /// Changes the selected stream target after releasing held keys and buttons.
    pub fn switch_target(&mut self, epoch: u16, display_id: DisplayId) -> Result<(), InputError> {
        let Some(_) = self.session else {
            return Err(InputError::NoAuthorizedSession);
        };
        self.release_all()?;
        self.session = Some(ActiveSession { epoch, display_id });
        Ok(())
    }

    /// Validates and processes one protocol input event for an active authorized session.
    pub fn process_event(
        &mut self,
        event: InputEvent,
        topology: &Topology,
        now_ms: u64,
    ) -> Result<(), InputError> {
        let session = self.session.ok_or(InputError::NoAuthorizedSession)?;
        if event.epoch != session.epoch {
            return Err(InputError::StaleEpoch {
                received: event.epoch,
                active: session.epoch,
            });
        }

        let (command, tracked_key, tracked_button) = match event.event {
            InputEventKind::MouseMoveAbs { u, v } => {
                if event.display_id != session.display_id.get() {
                    return Err(InputError::StaleDisplay {
                        received: event.display_id,
                        active: session.display_id.get(),
                    });
                }
                let display = topology
                    .displays()
                    .iter()
                    .find(|display| display.id() == session.display_id)
                    .ok_or(InputError::UnknownDisplay(event.display_id))?;
                if !display.flags().available() {
                    return Err(InputError::DisplayUnavailable(event.display_id));
                }
                let bounds = topology
                    .virtual_desktop_bounds()
                    .map_err(|_| InputError::InvalidTopology)?
                    .ok_or(InputError::InvalidTopology)?;
                let pixel = map_to_host_pixel(display, racc_topology::NormalizedPointer { u, v })
                    .map_err(InputError::Coordinate)?;
                (
                    InputCommand::MouseMoveAbsolute {
                        pixel,
                        virtual_desktop: bounds,
                    },
                    None,
                    None,
                )
            }
            InputEventKind::MouseMoveRel { dx, dy } => {
                (InputCommand::MouseMoveRelative { dx, dy }, None, None)
            }
            InputEventKind::MouseButton { button, pressed } => {
                if !(1..=5).contains(&button) {
                    return Err(InputError::InvalidMouseButton(button));
                }
                (
                    InputCommand::MouseButton { button, pressed },
                    None,
                    Some(button),
                )
            }
            InputEventKind::Wheel { dx, dy } => (InputCommand::Wheel { dx, dy }, None, None),
            InputEventKind::Key {
                hid_usage,
                pressed,
                modifiers,
            } => {
                if modifiers & !VALID_MODIFIER_MASK != 0 {
                    return Err(InputError::InvalidModifiers(modifiers));
                }
                hid_to_windows_scancode(hid_usage)
                    .ok_or(InputError::UnsupportedHidUsage(hid_usage))?;
                (
                    InputCommand::Key { hid_usage, pressed },
                    Some((hid_usage, pressed)),
                    None,
                )
            }
        };

        if let Some((usage, pressed)) = tracked_key {
            if self.pressed_keys.contains(&usage) == pressed {
                return Ok(());
            }
        }
        if let Some(button) = tracked_button {
            if self.pressed_buttons.contains(&button)
                == matches!(command, InputCommand::MouseButton { pressed: true, .. })
            {
                return Ok(());
            }
        }
        self.rate_limiter.check(now_ms)?;
        self.injector.inject(command)?;

        if let Some((usage, pressed)) = tracked_key {
            if pressed {
                self.pressed_keys.insert(usage);
            } else {
                self.pressed_keys.remove(&usage);
            }
        }
        if let Some(button) = tracked_button {
            if matches!(command, InputCommand::MouseButton { pressed: true, .. }) {
                self.pressed_buttons.insert(button);
            } else {
                self.pressed_buttons.remove(&button);
            }
        }
        Ok(())
    }

    /// Releases every key and mouse button accepted in this session.
    ///
    /// A failed release remains tracked so the caller may retry. All releases are attempted even
    /// if one operation fails. This method intentionally bypasses the inbound rate limiter.
    pub fn release_all(&mut self) -> Result<(), InputError> {
        let mut first_error = None;
        let buttons: Vec<u8> = self.pressed_buttons.iter().copied().collect();
        for button in buttons {
            match self.injector.inject(InputCommand::MouseButton {
                button,
                pressed: false,
            }) {
                Ok(()) => {
                    self.pressed_buttons.remove(&button);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            };
        }

        let keys: Vec<u16> = self.pressed_keys.iter().copied().collect();
        for usage in keys.into_iter().rev() {
            match self.injector.inject(InputCommand::Key {
                hid_usage: usage,
                pressed: false,
            }) {
                Ok(()) => {
                    self.pressed_keys.remove(&usage);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            };
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Releases tracked input and closes authorization. Safe to call repeatedly.
    pub fn deactivate(&mut self) -> Result<(), InputError> {
        let result = self.release_all();
        self.session = None;
        result
    }

    /// Releases tracked input after video is paused.
    pub fn pause(&mut self) -> Result<(), InputError> {
        self.deactivate()
    }

    /// Releases tracked input after the control connection is lost.
    pub fn control_lost(&mut self) -> Result<(), InputError> {
        self.deactivate()
    }

    /// Releases tracked input after the authorized session ends.
    pub fn end_session(&mut self) -> Result<(), InputError> {
        self.deactivate()
    }

    /// Returns an immutable reference to the wrapped injector.
    pub const fn injector(&self) -> &I {
        &self.injector
    }

    /// Returns a mutable reference to the wrapped injector, useful for draining fake records.
    pub fn injector_mut(&mut self) -> &mut I {
        &mut self.injector
    }
}

impl<I: InputInjector> Drop for InputInjectionController<I> {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeInputInjector;
    use racc_topology::{Display, DisplayFlags, Topology};

    #[test]
    fn pointer_injection_setting_accepts_only_supported_methods() {
        assert_eq!(
            PointerInjectionMethod::default(),
            PointerInjectionMethod::VirtualDesktopAbsolute
        );
        assert_eq!(
            PointerInjectionMethod::from_setting("virtual-desktop-absolute"),
            Some(PointerInjectionMethod::VirtualDesktopAbsolute)
        );
        assert_eq!(
            PointerInjectionMethod::from_setting("set-cursor-pos"),
            Some(PointerInjectionMethod::SetCursorPos)
        );
        for invalid in ["", "VirtualDesktopAbsolute", "set-cursorpos", "anything"] {
            assert_eq!(PointerInjectionMethod::from_setting(invalid), None);
        }
    }

    fn display_id(value: u32) -> DisplayId {
        DisplayId::new(value).unwrap_or_else(|| unreachable!("test id is nonzero"))
    }

    fn topology() -> Topology {
        let display = Display::new(
            display_id(7),
            "left monitor",
            -1920,
            0,
            1920,
            1080,
            1000,
            60_000,
            DisplayFlags::new(false, true, true, false),
        );
        Topology::new(1, vec![display], Some(display_id(7)))
            .unwrap_or_else(|_| unreachable!("test topology is valid"))
    }

    fn key(usage: u16, pressed: bool, modifiers: u8) -> InputEvent {
        InputEvent {
            epoch: 4,
            display_id: 7,
            event: InputEventKind::Key {
                hid_usage: usage,
                pressed,
                modifiers,
            },
        }
    }

    fn mouse_button(button: u8, pressed: bool) -> InputEvent {
        InputEvent {
            epoch: 4,
            display_id: 7,
            event: InputEventKind::MouseButton { button, pressed },
        }
    }

    #[test]
    fn validates_events_and_injects_only_known_hid_buttons_and_modifier_bits() {
        let mut controller = InputInjectionController::new(FakeInputInjector::default());
        controller
            .begin_session(4, display_id(7), true)
            .expect("authorized");
        let topology = topology();

        assert_eq!(
            controller.process_event(key(0xffff, true, 0), &topology, 0),
            Err(InputError::UnsupportedHidUsage(0xffff))
        );
        assert_eq!(
            controller.process_event(key(0x04, true, 0x10), &topology, 0),
            Err(InputError::InvalidModifiers(0x10))
        );
        assert_eq!(
            controller.process_event(mouse_button(6, true), &topology, 0),
            Err(InputError::InvalidMouseButton(6))
        );
        assert!(controller.injector().commands().is_empty());

        controller
            .process_event(key(0x04, true, 1), &topology, 1)
            .expect("valid key");
        assert_eq!(controller.injector().commands().len(), 1);
    }

    #[test]
    fn maps_negative_origin_absolute_pointer_using_topology_bounds() {
        let mut controller = InputInjectionController::new(FakeInputInjector::default());
        controller
            .begin_session(4, display_id(7), true)
            .expect("authorized");
        let topology = topology();
        controller
            .process_event(
                InputEvent {
                    epoch: 4,
                    display_id: 7,
                    event: InputEventKind::MouseMoveAbs { u: 0, v: 0 },
                },
                &topology,
                0,
            )
            .expect("top-left pointer");
        controller
            .process_event(
                InputEvent {
                    epoch: 4,
                    display_id: 7,
                    event: InputEventKind::MouseMoveAbs {
                        u: u16::MAX,
                        v: u16::MAX,
                    },
                },
                &topology,
                1,
            )
            .expect("bottom-right pointer");
        assert_eq!(
            controller.injector().commands(),
            &[
                InputCommand::MouseMoveAbsolute {
                    pixel: HostPhysicalPixel { x: -1920, y: 0 },
                    virtual_desktop: topology
                        .virtual_desktop_bounds()
                        .expect("bounds")
                        .expect("desktop"),
                },
                InputCommand::MouseMoveAbsolute {
                    pixel: HostPhysicalPixel { x: -1, y: 1079 },
                    virtual_desktop: topology
                        .virtual_desktop_bounds()
                        .expect("bounds")
                        .expect("desktop"),
                }
            ]
        );
    }

    #[test]
    fn absolute_pointer_rejects_stale_epoch_and_display() {
        let mut controller = InputInjectionController::new(FakeInputInjector::default());
        controller
            .begin_session(4, display_id(7), true)
            .expect("authorized");
        let topology = topology();
        let stale_epoch = InputEvent {
            epoch: 3,
            display_id: 7,
            event: InputEventKind::MouseMoveAbs { u: 0, v: 0 },
        };
        assert!(matches!(
            controller.process_event(stale_epoch, &topology, 0),
            Err(InputError::StaleEpoch { .. })
        ));
        let stale_display = InputEvent {
            epoch: 4,
            display_id: 8,
            event: InputEventKind::MouseMoveAbs { u: 0, v: 0 },
        };
        assert!(matches!(
            controller.process_event(stale_display, &topology, 0),
            Err(InputError::StaleDisplay { .. })
        ));
        assert!(controller.injector().commands().is_empty());
    }

    #[test]
    fn releases_pressed_keys_and_buttons_on_pause_control_loss_and_end() {
        for stop in [0, 1, 2] {
            let mut controller = InputInjectionController::new(FakeInputInjector::default());
            controller
                .begin_session(4, display_id(7), true)
                .expect("authorized");
            let topology = topology();
            controller
                .process_event(key(0x04, true, 0), &topology, 0)
                .expect("key down");
            controller
                .process_event(mouse_button(1, true), &topology, 1)
                .expect("button down");
            match stop {
                0 => controller.pause(),
                1 => controller.control_lost(),
                _ => controller.end_session(),
            }
            .expect("release succeeds");
            assert_eq!(
                controller.injector().commands(),
                &[
                    InputCommand::Key {
                        hid_usage: 0x04,
                        pressed: true,
                    },
                    InputCommand::MouseButton {
                        button: 1,
                        pressed: true,
                    },
                    InputCommand::MouseButton {
                        button: 1,
                        pressed: false,
                    },
                    InputCommand::Key {
                        hid_usage: 0x04,
                        pressed: false,
                    },
                ]
            );
            assert_eq!(
                controller.process_event(key(0x04, false, 0), &topology, 2),
                Err(InputError::NoAuthorizedSession)
            );
        }
    }

    #[test]
    fn duplicate_key_transitions_are_idempotent_and_rate_limit_is_enforced() {
        let mut controller =
            InputInjectionController::with_rate_limit(FakeInputInjector::default(), 1);
        controller
            .begin_session(4, display_id(7), true)
            .expect("authorized");
        let topology = topology();
        controller
            .process_event(key(0x04, true, 0), &topology, 100)
            .expect("first");
        controller
            .process_event(key(0x04, true, 0), &topology, 101)
            .expect("duplicate down ignored");
        assert_eq!(controller.injector().commands().len(), 1);
        assert_eq!(
            controller.process_event(
                InputEvent {
                    epoch: 4,
                    display_id: 7,
                    event: InputEventKind::MouseMoveRel { dx: 1, dy: 1 },
                },
                &topology,
                102
            ),
            Err(InputError::RateLimited)
        );
        controller
            .process_event(
                InputEvent {
                    epoch: 4,
                    display_id: 7,
                    event: InputEventKind::MouseMoveRel { dx: 1, dy: 1 },
                },
                &topology,
                1_100,
            )
            .expect("next window");
    }

    #[test]
    fn failed_release_is_reported_and_remains_retryable() {
        let mut controller = InputInjectionController::new(FakeInputInjector::default());
        controller
            .begin_session(4, display_id(7), true)
            .expect("authorized");
        controller
            .process_event(key(0x04, true, 0), &topology(), 0)
            .expect("key down");
        controller.injector_mut().fail_next();
        assert_eq!(controller.pause(), Err(InputError::OsInjectionFailed));
        assert_eq!(controller.injector().commands().len(), 1);
        assert_eq!(controller.release_all(), Ok(()));
        assert_eq!(controller.injector().commands().len(), 2);
        assert!(matches!(
            controller.injector().commands()[1],
            InputCommand::Key { pressed: false, .. }
        ));
    }

    #[test]
    fn unauthorized_session_cannot_inject() {
        let mut controller = InputInjectionController::new(FakeInputInjector::default());
        assert_eq!(
            controller.begin_session(4, display_id(7), false),
            Err(InputError::NoAuthorizedSession)
        );
        assert_eq!(
            controller.process_event(key(0x04, true, 0), &topology(), 0),
            Err(InputError::NoAuthorizedSession)
        );
    }
}
