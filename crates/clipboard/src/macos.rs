//! macOS NSPasteboard adapter for the shared text-only clipboard policy.
//!
//! The adapter accesses only `NSPasteboardTypeString`. Objective-C objects are held in
//! objc2 `Retained` RAII wrappers and released when the call returns.

use crate::{
    macos_poll::PasteboardText, ClipboardAdapterError, ClipboardErrorKind, ClipboardOperation,
    MAX_CLIPBOARD_BYTES,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardType, NSPasteboardTypeString};
use objc2_foundation::{NSString, NSUTF8StringEncoding};

/// Text-only adapter for the user's general macOS pasteboard.
#[derive(Clone, Copy, Debug, Default)]
pub struct MacPasteboard;

impl MacPasteboard {
    /// Creates a lightweight adapter without contacting the pasteboard.
    pub const fn new() -> Self {
        Self
    }
}

impl PasteboardText for MacPasteboard {
    type Error = ClipboardAdapterError;

    fn change_count(&mut self) -> Result<i64, Self::Error> {
        let pasteboard = NSPasteboard::generalPasteboard();
        Ok(pasteboard.changeCount() as i64)
    }

    fn read_text_bounded(&mut self, maximum_bytes: usize) -> Result<Option<Vec<u8>>, Self::Error> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let Some(value) = pasteboard.stringForType(string_type()) else {
            return Ok(None);
        };
        let limit = maximum_bytes.min(MAX_CLIPBOARD_BYTES);
        let byte_len = value.lengthOfBytesUsingEncoding(NSUTF8StringEncoding);
        if byte_len > limit {
            return Err(ClipboardAdapterError {
                operation: ClipboardOperation::Read,
                kind: ClipboardErrorKind::TooLarge,
            });
        }
        let text = value.to_string();
        if text.len() > limit {
            return Err(ClipboardAdapterError {
                operation: ClipboardOperation::Read,
                kind: ClipboardErrorKind::TooLarge,
            });
        }
        Ok(Some(text.into_bytes()))
    }

    fn write_text(&mut self, text: &str) -> Result<(), Self::Error> {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Err(ClipboardAdapterError {
                operation: ClipboardOperation::Write,
                kind: ClipboardErrorKind::TooLarge,
            });
        }
        let pasteboard = NSPasteboard::generalPasteboard();
        let _new_change_count = pasteboard.clearContents();
        let value = NSString::from_str(text);
        if pasteboard.setString_forType(&value, string_type()) {
            Ok(())
        } else {
            Err(ClipboardAdapterError {
                operation: ClipboardOperation::Write,
                kind: ClipboardErrorKind::Platform(0),
            })
        }
    }
}

fn string_type() -> &'static NSPasteboardType {
    // SAFETY: AppKit exports this immutable UTI constant for the process lifetime.
    unsafe { NSPasteboardTypeString }
}
