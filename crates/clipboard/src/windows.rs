//! Windows clipboard access through Unicode text and change notifications.
//!
//! This adapter never polls. A message-only window receives
//! `WM_CLIPBOARDUPDATE`; consumers can coalesce notifications and call
//! [`WindowsClipboard::read_text`] when notified.

use std::{
    cell::RefCell,
    mem::size_of,
    slice,
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::Duration,
};

use windows::{
    core::{Error as WindowsError, PCWSTR},
    Win32::{
        Foundation::{
            GetLastError, GlobalFree, ERROR_ACCESS_DENIED, ERROR_CLASS_ALREADY_EXISTS, HANDLE,
            HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
        },
        Graphics::Gdi::HBRUSH,
        System::{
            DataExchange::{
                AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
                OpenClipboard, RemoveClipboardFormatListener, SetClipboardData,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
            Ole::CF_UNICODETEXT,
            Threading::GetCurrentThreadId,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassW, TranslateMessage,
            HMENU, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE, WM_CLOSE,
            WM_DESTROY, WM_QUIT, WNDCLASSW,
        },
    },
};

use crate::{
    retry_contended, ClipboardAdapterError, ClipboardErrorKind, ClipboardOperation,
    CLIPBOARD_CONTENTION_ATTEMPTS, MAX_CLIPBOARD_BYTES,
};

const ERROR_CLIPBOARD_FORMAT_NOT_AVAILABLE: u32 = 0x0200;
const MAX_WINDOWS_TEXT_UNITS: usize = MAX_CLIPBOARD_BYTES * 2 + 1;
const MAX_WINDOWS_GLOBAL_BYTES: usize = (MAX_WINDOWS_TEXT_UNITS + 1) * size_of::<u16>();
const RETRY_DELAYS_MS: [u64; CLIPBOARD_CONTENTION_ATTEMPTS - 1] = [5, 10, 20, 40];
const WINDOW_CLASS_NAME: [u16; 32] = [
    82, 97, 99, 99, 67, 111, 110, 110, 101, 99, 116, 67, 108, 105, 112, 98, 111, 97, 114, 100, 76,
    105, 115, 116, 101, 110, 101, 114, 87, 105, 110, 0,
];

thread_local! {
    static CHANGE_NOTIFIER: RefCell<Option<SyncSender<()>>> = const { RefCell::new(None) };
}

/// Windows Unicode-text clipboard adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsClipboard;

impl WindowsClipboard {
    /// Creates a zero-sized adapter for the current interactive user session.
    pub const fn new() -> Self {
        Self
    }

    /// Starts a message-only window that reports clipboard change notifications.
    ///
    /// The returned receiver has capacity one, so repeated OS notifications
    /// coalesce while a consumer is processing the prior change. Access-denied
    /// errors are surfaced so callers can explain secure-desktop/session limits.
    pub fn start_listener(&self) -> Result<ClipboardListener, ClipboardAdapterError> {
        let (notification_tx, notifications) = mpsc::sync_channel(1);
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("racc-clipboard-listener".to_owned())
            .spawn(move || listener_thread(notification_tx, ready_tx))
            .map_err(|error| ClipboardAdapterError {
                operation: ClipboardOperation::Listen,
                kind: ClipboardErrorKind::Platform(error.raw_os_error().unwrap_or(0) as u32),
            })?;

        match ready_rx.recv() {
            Ok(Ok((window, thread_id))) => Ok(ClipboardListener {
                notifications,
                window,
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(ClipboardAdapterError {
                    operation: ClipboardOperation::Listen,
                    kind: ClipboardErrorKind::Platform(0),
                })
            }
        }
    }

    /// Reads bounded `CF_UNICODETEXT`, normalizing Windows line endings to LF.
    ///
    /// Returns `None` when the clipboard has no Unicode text format. The returned
    /// UTF-8 bytes are capped at [`MAX_CLIPBOARD_BYTES`].
    pub fn read_text(&self) -> Result<Option<Vec<u8>>, ClipboardAdapterError> {
        with_open_clipboard(ClipboardOperation::Read, read_open_clipboard)
    }

    /// Writes UTF-8 text as bounded `CF_UNICODETEXT`, converting LF to CRLF.
    pub fn write_text(&self, bytes: &[u8]) -> Result<(), ClipboardAdapterError> {
        let wide = encode_windows_text(bytes)?;
        with_open_clipboard(ClipboardOperation::Write, || write_open_clipboard(&wide))
    }
}

/// Owner of a Windows clipboard notification window.
pub struct ClipboardListener {
    notifications: Receiver<()>,
    window: isize,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl ClipboardListener {
    /// Returns the coalescing notification receiver.
    pub fn notifications(&self) -> &Receiver<()> {
        &self.notifications
    }
}

impl Drop for ClipboardListener {
    fn drop(&mut self) {
        let window = HWND(self.window as *mut std::ffi::c_void);
        // SAFETY: The handle was returned by CreateWindowExW and remains owned by
        // the listener thread until it processes WM_CLOSE or exits.
        if unsafe { PostMessageW(window, WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_err() {
            // SAFETY: The thread ID belongs to the listener thread and its message
            // queue exists because the thread created a window and entered GetMessageW.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn listener_thread(
    notification_tx: SyncSender<()>,
    ready_tx: mpsc::Sender<Result<(isize, u32), ClipboardAdapterError>>,
) {
    CHANGE_NOTIFIER.with(|slot| *slot.borrow_mut() = Some(notification_tx));
    let window = match create_listener_window() {
        Ok(window) => window,
        Err(error) => {
            CHANGE_NOTIFIER.with(|slot| *slot.borrow_mut() = None);
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    // SAFETY: This is called on the window's owning thread, and the returned ID
    // is used only to post a shutdown message to that same thread.
    let thread_id = unsafe { GetCurrentThreadId() };
    if ready_tx.send(Ok((window.0 as isize, thread_id))).is_err() {
        // SAFETY: The just-created window is still owned by this thread.
        let _ = unsafe { RemoveClipboardFormatListener(window) };
        // SAFETY: Destroying this thread's own window is valid here.
        let _ = unsafe { DestroyWindow(window) };
        CHANGE_NOTIFIER.with(|slot| *slot.borrow_mut() = None);
        return;
    }

    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is a valid writable MSG and the null HWND requests
        // the current thread's complete message queue.
        let result = unsafe { GetMessageW(&mut message, HWND::default(), 0, 0) };
        if result.0 <= 0 {
            break;
        }
        // SAFETY: `message` was populated by GetMessageW and remains valid.
        let _ = unsafe { TranslateMessage(&message) };
        // SAFETY: DispatchMessageW consumes the message removed from this thread's queue.
        let _ = unsafe { DispatchMessageW(&message) };
    }

    // WM_CLOSE normally unregisters and destroys the window. These calls also
    // clean up if the thread exits because GetMessageW reported an error.
    // SAFETY: The listener window was created on this thread; failing cleanup
    // calls are ignored because WM_CLOSE may already have destroyed it.
    let _ = unsafe { RemoveClipboardFormatListener(window) };
    // SAFETY: See the preceding cleanup invariant; DestroyWindow reports stale handles.
    let _ = unsafe { DestroyWindow(window) };
    CHANGE_NOTIFIER.with(|slot| *slot.borrow_mut() = None);
}

fn create_listener_window() -> Result<HWND, ClipboardAdapterError> {
    // SAFETY: A null module name asks Windows for the current executable module.
    let module = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map_err(|error| map_windows_error(ClipboardOperation::Listen, &error))?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: HINSTANCE(module.0),
        hbrBackground: HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(WINDOW_CLASS_NAME.as_ptr()),
        ..WNDCLASSW::default()
    };
    // SAFETY: `class` and its nul-terminated class name remain valid for the call.
    let atom = unsafe { RegisterClassW(&class) };
    if atom == 0 {
        // SAFETY: GetLastError is read immediately after RegisterClassW failed.
        let code = unsafe { GetLastError() }.0;
        if code != ERROR_CLASS_ALREADY_EXISTS.0 {
            return Err(native_error(ClipboardOperation::Listen, code));
        }
    }

    // SAFETY: The class is registered above, HWND_MESSAGE requests a non-visible
    // message-only window, and all parameters use null/default optional handles.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(WINDOW_CLASS_NAME.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU::default(),
            HINSTANCE(module.0),
            None,
        )
    }
    .map_err(|error| map_windows_error(ClipboardOperation::Listen, &error))?;

    // SAFETY: `window` is a live HWND created above on this thread.
    if let Err(error) = unsafe { AddClipboardFormatListener(window) } {
        // SAFETY: The window was just created on this thread and registration failed.
        let _ = unsafe { DestroyWindow(window) };
        return Err(map_windows_error(ClipboardOperation::Listen, &error));
    }
    Ok(window)
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLIPBOARDUPDATE => {
            CHANGE_NOTIFIER.with(|slot| {
                if let Ok(sender) = slot.try_borrow() {
                    if let Some(sender) = sender.as_ref() {
                        let _ = sender.try_send(());
                    }
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: This callback is executing for `window`, a live HWND owned
            // by the listener thread.
            let _ = unsafe { RemoveClipboardFormatListener(window) };
            // SAFETY: WM_CLOSE is the normal destruction path for this window.
            let _ = unsafe { DestroyWindow(window) };
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: Posting WM_QUIT terminates GetMessageW on this window thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            // SAFETY: Unhandled messages are forwarded unchanged to the system procedure.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

fn with_open_clipboard<T>(
    operation: ClipboardOperation,
    body: impl FnOnce() -> Result<T, ClipboardAdapterError>,
) -> Result<T, ClipboardAdapterError> {
    let guard = OpenClipboardGuard::open(operation)?;
    let result = body();
    let close_result = guard.close();
    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

struct OpenClipboardGuard {
    open: bool,
    operation: ClipboardOperation,
}

impl OpenClipboardGuard {
    fn open(operation: ClipboardOperation) -> Result<Self, ClipboardAdapterError> {
        let opened = retry_contended(
            CLIPBOARD_CONTENTION_ATTEMPTS,
            || {
                // SAFETY: Null owner is permitted when opening the clipboard for
                // reading/writing; the clipboard is closed by this guard.
                unsafe { OpenClipboard(HWND::default()) }
            },
            |error| win32_code(error) == ERROR_ACCESS_DENIED.0,
            |attempt| {
                let delay = RETRY_DELAYS_MS
                    .get(attempt)
                    .copied()
                    .unwrap_or(RETRY_DELAYS_MS[RETRY_DELAYS_MS.len() - 1]);
                thread::sleep(Duration::from_millis(delay));
            },
        );
        opened.map_err(|error| map_windows_error(operation, &error))?;
        Ok(Self {
            open: true,
            operation,
        })
    }

    fn close(mut self) -> Result<(), ClipboardAdapterError> {
        // SAFETY: This guard represents exactly one successful OpenClipboard call.
        let result = unsafe { CloseClipboard() };
        if result.is_ok() {
            self.open = false;
        }
        result.map_err(|error| map_windows_error(self.operation, &error))
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        if self.open {
            // SAFETY: This guard owns the matching successful OpenClipboard call.
            let _ = unsafe { CloseClipboard() };
        }
    }
}

fn read_open_clipboard() -> Result<Option<Vec<u8>>, ClipboardAdapterError> {
    let format = u32::from(CF_UNICODETEXT.0);
    // SAFETY: The clipboard is open for this function's duration.
    let handle = match unsafe { GetClipboardData(format) } {
        Ok(handle) => HGLOBAL(handle.0),
        Err(error) if win32_code(&error) == ERROR_CLIPBOARD_FORMAT_NOT_AVAILABLE => {
            return Ok(None)
        }
        Err(error) if error.code().0 == 0 => return Ok(None),
        Err(error) => return Err(map_windows_error(ClipboardOperation::Read, &error)),
    };

    // SAFETY: `handle` is the HGLOBAL returned by GetClipboardData while the
    // clipboard is open. Its size is bounded before creating any slice.
    let size = unsafe { GlobalSize(handle) };
    if size < size_of::<u16>() || size > MAX_WINDOWS_GLOBAL_BYTES || size % size_of::<u16>() != 0 {
        return Err(ClipboardAdapterError {
            operation: ClipboardOperation::Read,
            kind: ClipboardErrorKind::TooLarge,
        });
    }
    // SAFETY: The handle is valid and locked only for the bounded conversion below.
    let pointer = unsafe { GlobalLock(handle) };
    if pointer.is_null() {
        // SAFETY: Read the Win32 error immediately after GlobalLock failed.
        return Err(native_error(
            ClipboardOperation::Read,
            unsafe { GetLastError() }.0,
        ));
    }

    // SAFETY: GlobalSize established an even, bounded allocation size. GlobalLock
    // keeps the movable handle fixed until the matching GlobalUnlock below.
    let decoded = unsafe {
        let units = slice::from_raw_parts(pointer.cast::<u16>(), size / size_of::<u16>());
        decode_windows_text(units)
    };
    // SAFETY: `pointer` was obtained by the matching GlobalLock above.
    let _ = unsafe { GlobalUnlock(handle) };
    decoded.map(Some)
}

fn decode_windows_text(units: &[u16]) -> Result<Vec<u8>, ClipboardAdapterError> {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    if end > MAX_WINDOWS_TEXT_UNITS {
        return Err(ClipboardAdapterError {
            operation: ClipboardOperation::Read,
            kind: ClipboardErrorKind::TooLarge,
        });
    }
    let mut text = String::new();
    text.try_reserve_exact(end.min(MAX_CLIPBOARD_BYTES))
        .map_err(|_| too_large_error(ClipboardOperation::Read))?;
    let mut pending_cr = false;
    for decoded in char::decode_utf16(units[..end].iter().copied()) {
        let character = decoded.map_err(|_| ClipboardAdapterError {
            operation: ClipboardOperation::Read,
            kind: ClipboardErrorKind::InvalidUtf16,
        })?;
        if pending_cr {
            push_bounded(&mut text, '\n')?;
            pending_cr = false;
            if character == '\n' {
                continue;
            }
        }
        if character == '\r' {
            pending_cr = true;
        } else {
            push_bounded(&mut text, character)?;
        }
    }
    if pending_cr {
        push_bounded(&mut text, '\n')?;
    }
    Ok(text.into_bytes())
}

fn push_bounded(text: &mut String, character: char) -> Result<(), ClipboardAdapterError> {
    if text.len().saturating_add(character.len_utf8()) > MAX_CLIPBOARD_BYTES {
        return Err(too_large_error(ClipboardOperation::Read));
    }
    text.push(character);
    Ok(())
}

fn encode_windows_text(bytes: &[u8]) -> Result<Vec<u16>, ClipboardAdapterError> {
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(too_large_error(ClipboardOperation::Write));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ClipboardAdapterError {
        operation: ClipboardOperation::Write,
        kind: ClipboardErrorKind::InvalidUtf8,
    })?;
    let capacity = text
        .len()
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| too_large_error(ClipboardOperation::Write))?;
    let mut units = Vec::new();
    units
        .try_reserve_exact(capacity)
        .map_err(|_| too_large_error(ClipboardOperation::Write))?;
    let windows_text = crate::to_windows_newlines(text);
    let capacity = windows_text
        .len()
        .checked_add(1)
        .ok_or_else(|| too_large_error(ClipboardOperation::Write))?;
    units
        .try_reserve_exact(capacity)
        .map_err(|_| too_large_error(ClipboardOperation::Write))?;
    for character in windows_text.chars() {
        let mut encoded = [0_u16; 2];
        units.extend_from_slice(character.encode_utf16(&mut encoded));
    }
    units.push(0);
    if units.len() > MAX_WINDOWS_TEXT_UNITS + 1 {
        return Err(too_large_error(ClipboardOperation::Write));
    }
    Ok(units)
}

fn write_open_clipboard(units: &[u16]) -> Result<(), ClipboardAdapterError> {
    let bytes = units
        .len()
        .checked_mul(size_of::<u16>())
        .filter(|size| *size <= MAX_WINDOWS_GLOBAL_BYTES)
        .ok_or_else(|| too_large_error(ClipboardOperation::Write))?;
    // SAFETY: GMEM_MOVEABLE is required for SetClipboardData. The allocation
    // size is derived from bounded UTF-16 units and ownership stays local until transfer.
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }
        .map_err(|error| map_windows_error(ClipboardOperation::Write, &error))?;
    let mut allocation = OwnedGlobal {
        handle,
        transferred: false,
    };
    // SAFETY: The allocation is a valid movable HGLOBAL returned above.
    let pointer = unsafe { GlobalLock(handle) };
    if pointer.is_null() {
        // SAFETY: Read the Win32 error immediately after GlobalLock failed.
        return Err(native_error(
            ClipboardOperation::Write,
            unsafe { GetLastError() }.0,
        ));
    }
    // SAFETY: Destination is locked for `bytes`; source has the same number of
    // initialized u16 values. The buffers are distinct and properly aligned.
    unsafe {
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer.cast::<u16>(), units.len());
    }
    // SAFETY: `pointer` is the matching GlobalLock result. Its zero return is
    // expected when the lock count reaches zero, so the status is not treated as failure.
    let _ = unsafe { GlobalUnlock(handle) };

    // SAFETY: The clipboard is open. EmptyClipboard transfers no memory from our allocation.
    unsafe { EmptyClipboard() }
        .map_err(|error| map_windows_error(ClipboardOperation::Write, &error))?;
    // SAFETY: `handle` is a movable global allocation containing nul-terminated
    // UTF-16. Windows takes ownership only if SetClipboardData succeeds.
    unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), HANDLE(handle.0)) }
        .map_err(|error| map_windows_error(ClipboardOperation::Write, &error))?;
    allocation.transferred = true;
    Ok(())
}

struct OwnedGlobal {
    handle: HGLOBAL,
    transferred: bool,
}

impl Drop for OwnedGlobal {
    fn drop(&mut self) {
        if !self.transferred {
            // SAFETY: Ownership remains with this guard unless SetClipboardData succeeded.
            let _ = unsafe { GlobalFree(self.handle) };
        }
    }
}

fn too_large_error(operation: ClipboardOperation) -> ClipboardAdapterError {
    ClipboardAdapterError {
        operation,
        kind: ClipboardErrorKind::TooLarge,
    }
}

fn map_windows_error(operation: ClipboardOperation, error: &WindowsError) -> ClipboardAdapterError {
    native_error(operation, win32_code(error))
}

fn win32_code(error: &WindowsError) -> u32 {
    (error.code().0 as u32) & 0xffff
}

fn native_error(operation: ClipboardOperation, code: u32) -> ClipboardAdapterError {
    let kind = if code == ERROR_ACCESS_DENIED.0 {
        ClipboardErrorKind::AccessDenied {
            secure_desktop_likely: true,
        }
    } else {
        ClipboardErrorKind::Platform(code)
    };
    ClipboardAdapterError { operation, kind }
}
