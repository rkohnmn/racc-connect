//! Native window visibility and current display work-area adapters.
//!
//! All OS interaction is kept in this thin module. The UI continues to use iced
//! window IDs and sends only visibility state through its normal update loop.

use crate::settings::ScreenWorkArea;
use iced::window::Window;

/// Reads current display work areas in the same desktop coordinate convention used by the app.
///
/// An empty vector means the platform API could not provide display geometry; callers should use
/// the bounded origin fallback in `restore_geometry_for_displays`.
pub(crate) fn current_screen_work_areas() -> Vec<ScreenWorkArea> {
    platform::screen_work_areas()
}

/// Hides or shows the native window represented by an iced window handle.
///
/// The caller must execute this from the iced window update callback so platform UI APIs remain
/// on the window's owning thread.
pub(crate) fn set_visible(window: &dyn Window, visible: bool) -> Result<(), String> {
    let handle = window
        .window_handle()
        .map_err(|_| "the native window handle is unavailable".to_owned())?;
    platform::set_visible(handle.as_raw(), visible)
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use iced::window::raw_window_handle::RawWindowHandle;
    use std::{mem::size_of, ptr::null};
    use windows_sys::Win32::{
        Foundation::{LPARAM, RECT},
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_RESTORE},
        },
    };

    pub(super) fn screen_work_areas() -> Vec<ScreenWorkArea> {
        let mut screens = Vec::new();
        // SAFETY: the callback receives a pointer to `screens`, which remains alive and uniquely
        // borrowed until EnumDisplayMonitors returns; Windows invokes the callback synchronously.
        let succeeded = unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                null(),
                Some(collect_monitor),
                (&mut screens as *mut Vec<ScreenWorkArea>) as LPARAM,
            )
        };
        if succeeded == 0 {
            Vec::new()
        } else {
            screens
        }
    }

    unsafe extern "system" fn collect_monitor(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        // SAFETY: EnumDisplayMonitors receives this exact non-null Vec pointer as its LPARAM and
        // invokes callbacks synchronously, so this exclusive borrow cannot outlive the call.
        let screens = unsafe { &mut *(data as *mut Vec<ScreenWorkArea>) };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `monitor` is supplied by EnumDisplayMonitors and `info` is writable and sized.
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return 1;
        }
        let work = info.rcWork;
        let mut dpi_x = 96_u32;
        let mut dpi_y = 96_u32;
        // SAFETY: `monitor` is the live monitor handle supplied by EnumDisplayMonitors and both
        // DPI output pointers refer to writable local integers.
        if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } != 0 {
            dpi_x = 96;
            dpi_y = 96;
        }
        let scale_x = f64::from(dpi_x.max(1)) / 96.0;
        let scale_y = f64::from(dpi_y.max(1)) / 96.0;
        let width = (i64::from(work.right) - i64::from(work.left)) as f64 / scale_x;
        let height = (i64::from(work.bottom) - i64::from(work.top)) as f64 / scale_y;
        let (Some(x), Some(y), Some(width), Some(height)) = (
            logical_i32(f64::from(work.left) / scale_x),
            logical_i32(f64::from(work.top) / scale_y),
            logical_u32(width),
            logical_u32(height),
        ) else {
            return 1;
        };
        screens.push(ScreenWorkArea {
            x,
            y,
            width,
            height,
        });
        1
    }

    fn logical_i32(value: f64) -> Option<i32> {
        (value.is_finite() && value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX))
            .then(|| value.round() as i32)
    }

    fn logical_u32(value: f64) -> Option<u32> {
        (value.is_finite() && value > 0.0 && value <= f64::from(u32::MAX))
            .then(|| value.round() as u32)
            .filter(|value| *value > 0)
    }

    pub(super) fn set_visible(handle: RawWindowHandle, visible: bool) -> Result<(), String> {
        let RawWindowHandle::Win32(handle) = handle else {
            return Err("iced returned a non-Windows native window handle".to_owned());
        };
        let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
        // SAFETY: iced supplied a live Win32 HWND on the window's owning UI thread.
        unsafe { ShowWindow(hwnd, if visible { SW_RESTORE } else { SW_HIDE }) };
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use iced::window::raw_window_handle::RawWindowHandle;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSScreen, NSView};
    use objc2_foundation::{MainThreadMarker, NSRect};

    pub(super) fn screen_work_areas() -> Vec<ScreenWorkArea> {
        let Some(main_thread) = MainThreadMarker::new() else {
            return Vec::new();
        };
        let frames: Vec<(NSRect, NSRect)> = NSScreen::screens(main_thread)
            .iter()
            .map(|screen| (screen.frame(), screen.visibleFrame()))
            .collect();
        let Some(desktop_top) = frames
            .iter()
            .map(|(frame, _)| frame.origin.y + frame.size.height)
            .reduce(f64::max)
        else {
            return Vec::new();
        };
        frames
            .into_iter()
            .filter_map(|(_, visible)| {
                let x = finite_i32(visible.origin.x)?;
                // AppKit uses a bottom-left origin; iced/winit desktop coordinates grow downward.
                let y = finite_i32(desktop_top - (visible.origin.y + visible.size.height))?;
                let width = finite_u32(visible.size.width)?;
                let height = finite_u32(visible.size.height)?;
                Some(ScreenWorkArea {
                    x,
                    y,
                    width,
                    height,
                })
            })
            .collect()
    }

    fn finite_i32(value: f64) -> Option<i32> {
        (value.is_finite() && value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX))
            .then(|| value.round() as i32)
    }

    fn finite_u32(value: f64) -> Option<u32> {
        (value.is_finite() && value > 0.0 && value <= f64::from(u32::MAX))
            .then(|| value.round() as u32)
            .filter(|value| *value > 0)
    }

    pub(super) fn set_visible(handle: RawWindowHandle, visible: bool) -> Result<(), String> {
        let RawWindowHandle::AppKit(handle) = handle else {
            return Err("iced returned a non-AppKit native window handle".to_owned());
        };
        let Some(_) = MainThreadMarker::new() else {
            return Err("the AppKit window operation is outside the main thread".to_owned());
        };
        // SAFETY: the AppKit raw-window-handle contract guarantees ns_view is a live NSView.
        // Retaining it here extends the object lifetime while its parent NSWindow is queried.
        let view = unsafe { Retained::<NSView>::retain(handle.ns_view.as_ptr().cast()) }
            .ok_or_else(|| "the AppKit view could not be retained".to_owned())?;
        let window = view
            .window()
            .ok_or_else(|| "the AppKit view is not attached to a window".to_owned())?;
        if visible {
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderOut(None);
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::*;
    use iced::window::raw_window_handle::RawWindowHandle;

    pub(super) fn screen_work_areas() -> Vec<ScreenWorkArea> {
        Vec::new()
    }

    pub(super) fn set_visible(_handle: RawWindowHandle, _visible: bool) -> Result<(), String> {
        Err("native window hiding is not supported on this platform".to_owned())
    }
}
