//! Platform-independent lifecycle model shared by the macOS capture driver and its fakes.
use std::collections::VecDeque;

/// Active macOS display-capture implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacCapturePath {
    ScreenCaptureKit,
    CGDisplayStream,
}

/// Lifecycle event that carries no platform-specific frame memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacCaptureNotice {
    Started {
        path: MacCapturePath,
        display_id: u32,
    },
    DisplayReconfigured {
        display_id: u32,
    },
    DisplayRemoved {
        display_id: u32,
    },
    AccessLost {
        display_id: u32,
    },
    Recovered {
        display_id: u32,
    },
}

/// Source operations required by the testable lifecycle controller.
pub trait MacCaptureSource {
    fn start(&mut self, display_id: u32) -> Result<MacCapturePath, String>;
    fn migrate(&mut self, display_id: u32) -> Result<(), String>;
    fn stop(&mut self);
}

/// Lifecycle controller independent of macOS APIs and frame representation.
pub struct MacCaptureController<S> {
    source: S,
    selected_display: Option<u32>,
    interrupted: bool,
    notices: VecDeque<MacCaptureNotice>,
}

impl<S: MacCaptureSource> MacCaptureController<S> {
    /// Creates a stopped controller around a real or fake source.
    pub fn new(source: S) -> Self {
        Self {
            source,
            selected_display: None,
            interrupted: false,
            notices: VecDeque::new(),
        }
    }

    /// Starts capture after the caller has checked Screen Recording permission.
    pub fn start(&mut self, display_id: u32) -> Result<(), String> {
        if self.selected_display.is_some() {
            return Err("capture is already started".to_owned());
        }
        let path = self.source.start(display_id)?;
        self.selected_display = Some(display_id);
        self.interrupted = false;
        self.notices
            .push_back(MacCaptureNotice::Started { path, display_id });
        Ok(())
    }

    /// Reconfigures a running source for another display.
    pub fn migrate(&mut self, display_id: u32) -> Result<(), String> {
        if self.selected_display.is_none() {
            return Err("capture has not started".to_owned());
        }
        self.source.migrate(display_id)?;
        self.selected_display = Some(display_id);
        self.interrupted = false;
        self.notices
            .push_back(MacCaptureNotice::DisplayReconfigured { display_id });
        Ok(())
    }

    /// Records an OS interruption once until a valid frame is received.
    pub fn access_lost(&mut self) -> bool {
        let Some(display_id) = self.selected_display else {
            return false;
        };
        if !self.interrupted {
            self.interrupted = true;
            self.notices
                .push_back(MacCaptureNotice::AccessLost { display_id });
        }
        true
    }

    /// Marks a valid frame as recovery after an interruption.
    pub fn frame_received(&mut self) -> bool {
        let Some(display_id) = self.selected_display else {
            return false;
        };
        if self.interrupted {
            self.interrupted = false;
            self.notices
                .push_back(MacCaptureNotice::Recovered { display_id });
        }
        true
    }

    /// Stops and reports removal of the selected display.
    pub fn display_removed(&mut self) -> bool {
        let Some(display_id) = self.selected_display.take() else {
            return false;
        };
        self.source.stop();
        self.interrupted = false;
        self.notices
            .push_back(MacCaptureNotice::DisplayRemoved { display_id });
        true
    }

    /// Stops capture and clears the selected display.
    pub fn stop(&mut self) {
        self.source.stop();
        self.selected_display = None;
        self.interrupted = false;
    }

    /// Returns the active native display identifier.
    pub const fn selected_display(&self) -> Option<u32> {
        self.selected_display
    }
    /// Removes the oldest pending lifecycle event.
    pub fn poll_notice(&mut self) -> Option<MacCaptureNotice> {
        self.notices.pop_front()
    }
    /// Mutably borrows the driver for platform configuration before a lifecycle call.
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }
    /// Borrows the driver for fake assertions and diagnostics.
    pub fn source(&self) -> &S {
        &self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        selected: Option<u32>,
        migrations: usize,
        stops: usize,
    }
    impl MacCaptureSource for Fake {
        fn start(&mut self, id: u32) -> Result<MacCapturePath, String> {
            self.selected = Some(id);
            Ok(MacCapturePath::ScreenCaptureKit)
        }
        fn migrate(&mut self, id: u32) -> Result<(), String> {
            self.selected = Some(id);
            self.migrations += 1;
            Ok(())
        }
        fn stop(&mut self) {
            self.selected = None;
            self.stops += 1;
        }
    }
    #[test]
    fn fake_lifecycle_covers_start_migrate_loss_recovery_and_removal() {
        let mut c = MacCaptureController::new(Fake::default());
        assert!(c.migrate(2).is_err());
        assert!(c.start(41).is_ok());
        assert_eq!(
            c.poll_notice(),
            Some(MacCaptureNotice::Started {
                path: MacCapturePath::ScreenCaptureKit,
                display_id: 41
            })
        );
        assert!(c.access_lost());
        assert!(c.access_lost());
        assert_eq!(
            c.poll_notice(),
            Some(MacCaptureNotice::AccessLost { display_id: 41 })
        );
        assert!(c.frame_received());
        assert_eq!(
            c.poll_notice(),
            Some(MacCaptureNotice::Recovered { display_id: 41 })
        );
        assert!(c.migrate(42).is_ok());
        assert_eq!(
            c.poll_notice(),
            Some(MacCaptureNotice::DisplayReconfigured { display_id: 42 })
        );
        assert_eq!(c.source().migrations, 1);
        assert!(c.display_removed());
        assert_eq!(
            c.poll_notice(),
            Some(MacCaptureNotice::DisplayRemoved { display_id: 42 })
        );
        assert_eq!(c.source().stops, 1);
    }
}
