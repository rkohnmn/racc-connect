//! macOS keyboard injection adapter. The OS calls are isolated in this module.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::{Arc, RwLock};

use racc_topology::{HostDisplayGeometry, HostPhysicalPixel};

use crate::{hid_to_macos_keycode, InputCommand, InputError, InputInjector, MacDisplayGeometry};

/// One association between wire-pixel bounds and host logical point geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacDisplayMap {
    geometry: MacDisplayGeometry,
}

impl MacDisplayMap {
    /// Creates a validated pixel-to-point mapping for one active display.
    pub fn new(
        pixel_origin: (i32, i32),
        pixel_size: (u32, u32),
        points: HostDisplayGeometry,
    ) -> Result<Self, InputError> {
        let geometry = MacDisplayGeometry::new(pixel_origin, pixel_size, points)
            .map_err(InputError::Coordinate)?;
        Ok(Self { geometry })
    }

    fn map(self, pixel: HostPhysicalPixel) -> Result<racc_topology::MacPoint, InputError> {
        self.geometry
            .map_pixel(pixel)
            .map_err(InputError::Coordinate)
    }

    fn contains(self, pixel: HostPhysicalPixel) -> bool {
        self.map(pixel).is_ok()
    }
}

/// Quartz input injector. Construct with current physical and logical display geometry.
///
/// Accessibility trust is checked before each event. This adapter never requests permission or
/// posts an event during construction.
#[derive(Clone, Debug, Default)]
pub struct MacQuartzInputInjector {
    displays: Arc<RwLock<Vec<MacDisplayMap>>>,
    wheel_remainder_x: i32,
    wheel_remainder_y: i32,
}

impl MacQuartzInputInjector {
    /// Creates the injector with a snapshot of validated display mappings.
    pub fn new(displays: Vec<MacDisplayMap>) -> Self {
        Self {
            displays: Arc::new(RwLock::new(displays)),
            wheel_remainder_x: 0,
            wheel_remainder_y: 0,
        }
    }

    /// Replaces the validated display mappings after a display-topology change.
    pub fn replace_display_maps(&self, displays: Vec<MacDisplayMap>) -> Result<(), InputError> {
        let mut current = self
            .displays
            .write()
            .map_err(|_| InputError::OsInjectionFailed)?;
        *current = displays;
        Ok(())
    }

    /// Returns whether macOS currently trusts this process for Accessibility control.
    pub fn accessibility_trusted() -> bool {
        // SAFETY: AXIsProcessTrusted has no parameters, does not retain pointers, and only returns
        // the OS's current Accessibility trust state.
        unsafe { AXIsProcessTrusted() != 0 }
    }
}

impl InputInjector for MacQuartzInputInjector {
    fn inject(&mut self, command: InputCommand) -> Result<(), InputError> {
        if !Self::accessibility_trusted() {
            return Err(InputError::PermissionMissing);
        }
        match command {
            InputCommand::Key { hid_usage, pressed } => {
                let keycode = hid_to_macos_keycode(hid_usage)
                    .ok_or(InputError::UnsupportedHidUsage(hid_usage))?;
                // SAFETY: The key code is a bounded value from our table. A null source asks
                // Quartz to use the current event source; the returned Create-rule event is owned.
                let event = unsafe {
                    CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, pressed as u8)
                };
                post_event(event)
            }
            InputCommand::MouseMoveAbsolute { pixel, .. } => {
                let displays = self
                    .displays
                    .read()
                    .map_err(|_| InputError::OsInjectionFailed)?;
                let display = displays
                    .iter()
                    .copied()
                    .find(|item| item.contains(pixel))
                    .ok_or(InputError::Coordinate(
                        racc_topology::CoordinateError::HostPixelOutsideVirtualDesktop,
                    ))?;
                let point = display.map(pixel)?;
                // SAFETY: Point is finite and derives from validated display geometry. The returned
                // Create-rule event is owned and released by `post_event`.
                let event = unsafe {
                    CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        CG_EVENT_MOUSE_MOVED,
                        cg_point(point),
                        CG_MOUSE_BUTTON_LEFT,
                    )
                };
                post_event(event)
            }
            InputCommand::MouseMoveRelative { dx, dy } => {
                let point = current_mouse_location()?;
                // SAFETY: The movement delta is bounded by the wire i16 fields. The event starts
                // at the current cursor location, and setting the documented deltas mutates it.
                let event = unsafe {
                    let event = CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        CG_EVENT_MOUSE_MOVED,
                        point,
                        CG_MOUSE_BUTTON_LEFT,
                    );
                    if !event.is_null() {
                        CGEventSetIntegerValueField(event, CG_MOUSE_EVENT_DELTA_X, i64::from(dx));
                        CGEventSetIntegerValueField(event, CG_MOUSE_EVENT_DELTA_Y, i64::from(dy));
                    }
                    event
                };
                post_event(event)
            }
            InputCommand::MouseButton { button, pressed } => {
                let (event_type, quartz_button) = match (button, pressed) {
                    (1, true) => (CG_EVENT_LEFT_MOUSE_DOWN, 0),
                    (1, false) => (CG_EVENT_LEFT_MOUSE_UP, 0),
                    (2, true) => (CG_EVENT_RIGHT_MOUSE_DOWN, 1),
                    (2, false) => (CG_EVENT_RIGHT_MOUSE_UP, 1),
                    (3, true) => (CG_EVENT_OTHER_MOUSE_DOWN, 2),
                    (3, false) => (CG_EVENT_OTHER_MOUSE_UP, 2),
                    (4, true) => (CG_EVENT_OTHER_MOUSE_DOWN, 3),
                    (4, false) => (CG_EVENT_OTHER_MOUSE_UP, 3),
                    (5, true) => (CG_EVENT_OTHER_MOUSE_DOWN, 4),
                    (5, false) => (CG_EVENT_OTHER_MOUSE_UP, 4),
                    _ => return Err(InputError::InvalidMouseButton(button)),
                };
                let point = current_mouse_location()?;
                // SAFETY: Button/event values are selected from the bounded match above. Point is
                // the current cursor location; `post_event` releases the returned owned event.
                let event = unsafe {
                    CGEventCreateMouseEvent(std::ptr::null_mut(), event_type, point, quartz_button)
                };
                post_event(event)
            }
            InputCommand::Wheel { dx, dy } => {
                self.wheel_remainder_x = self.wheel_remainder_x.saturating_add(i32::from(dx));
                self.wheel_remainder_y = self.wheel_remainder_y.saturating_add(i32::from(dy));
                let ticks_x = self.wheel_remainder_x / 120;
                let ticks_y = self.wheel_remainder_y / 120;
                self.wheel_remainder_x -= ticks_x * 120;
                self.wheel_remainder_y -= ticks_y * 120;
                if ticks_x == 0 && ticks_y == 0 {
                    return Ok(());
                }
                // SAFETY: At most the accepted bounded event deltas are converted to line units.
                // The Create-rule event is owned and released by `post_event`.
                let event = unsafe {
                    CGEventCreateScrollWheelEvent(
                        std::ptr::null_mut(),
                        CG_SCROLL_EVENT_UNIT_LINE,
                        2,
                        ticks_y,
                        ticks_x,
                    )
                };
                post_event(event)
            }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn CGEventCreate(source: *mut c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
    fn CGEventCreateKeyboardEvent(
        source: *mut c_void,
        virtual_key: u16,
        key_down: u8,
    ) -> *mut c_void;
    fn CGEventCreateMouseEvent(
        source: *mut c_void,
        event_type: u32,
        point: CGPoint,
        button: u32,
    ) -> *mut c_void;
    fn CGEventCreateScrollWheelEvent(
        source: *mut c_void,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
    ) -> *mut c_void;
    fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}

const CG_EVENT_MOUSE_MOVED: u32 = 5;
const CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
const CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const CG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
const CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
const CG_EVENT_OTHER_MOUSE_UP: u32 = 26;
const CG_MOUSE_BUTTON_LEFT: u32 = 0;
const CG_SCROLL_EVENT_UNIT_LINE: u32 = 1;
const CG_EVENT_TAP_HID: u32 = 0;
const CG_MOUSE_EVENT_DELTA_X: u32 = 4;
const CG_MOUSE_EVENT_DELTA_Y: u32 = 5;

fn cg_point(point: racc_topology::MacPoint) -> CGPoint {
    CGPoint {
        x: point.x_pt,
        y: point.y_pt,
    }
}

fn current_mouse_location() -> Result<CGPoint, InputError> {
    // SAFETY: A null event source requests a new current-system event object. The returned +1
    // reference is released below after reading its immutable location.
    let event = unsafe { CGEventCreate(std::ptr::null_mut()) };
    if event.is_null() {
        return Err(InputError::OsInjectionFailed);
    }
    // SAFETY: `event` is a valid retained CGEvent returned by CGEventCreate.
    let point = unsafe { CGEventGetLocation(event) };
    // SAFETY: Balance the Create-rule ownership after extracting the value-type point.
    unsafe {
        CFRelease(event);
    }
    if point.x.is_finite() && point.y.is_finite() {
        Ok(point)
    } else {
        Err(InputError::OsInjectionFailed)
    }
}
fn post_event(event: *mut c_void) -> Result<(), InputError> {
    if event.is_null() {
        return Err(InputError::OsInjectionFailed);
    }
    // SAFETY: `event` is a non-null, newly-created CGEvent. CGEventPost is synchronous; releasing
    // the Create-rule reference after posting balances the native object on all successful paths.
    unsafe {
        CGEventPost(CG_EVENT_TAP_HID, event);
        CFRelease(event);
    }
    Ok(())
}
