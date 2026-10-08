//! Deterministic, bounded fake input injector for controller and loopback tests.

use crate::{InputCommand, InputError, InputInjector};

/// Maximum number of commands recorded by a fake backend.
pub const MAX_FAKE_INPUT_COMMANDS: usize = 4_096;

/// Fake backend that records commands without touching the operating system.
#[derive(Clone, Debug, Default)]
pub struct FakeInputInjector {
    commands: Vec<InputCommand>,
    fail_next: bool,
}

impl FakeInputInjector {
    /// Returns all commands accepted by the fake backend.
    pub fn commands(&self) -> &[InputCommand] {
        &self.commands
    }

    /// Removes and returns the recorded commands.
    pub fn take_commands(&mut self) -> Vec<InputCommand> {
        std::mem::take(&mut self.commands)
    }

    /// Causes the next call to `inject` to fail, for release-retry tests.
    pub fn fail_next(&mut self) {
        self.fail_next = true;
    }
}

impl InputInjector for FakeInputInjector {
    fn inject(&mut self, command: InputCommand) -> Result<(), InputError> {
        if self.fail_next {
            self.fail_next = false;
            return Err(InputError::OsInjectionFailed);
        }
        if self.commands.len() >= MAX_FAKE_INPUT_COMMANDS {
            return Err(InputError::FakeRecordingLimit);
        }
        self.commands.push(command);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_without_platform_side_effects_and_can_fail_once() {
        let mut fake = FakeInputInjector::default();
        fake.fail_next();
        assert_eq!(
            fake.inject(InputCommand::MouseMoveRelative { dx: 2, dy: -3 }),
            Err(InputError::OsInjectionFailed)
        );
        fake.inject(InputCommand::MouseMoveRelative { dx: 2, dy: -3 })
            .expect("retry");
        assert_eq!(
            fake.commands(),
            &[InputCommand::MouseMoveRelative { dx: 2, dy: -3 }]
        );
    }
}
