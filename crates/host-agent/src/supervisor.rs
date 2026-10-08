//! Pure helper-process supervisor policy for the Windows host service.
//!
//! This module performs no I/O. The service adapter translates `SupervisorAction`
//! values into process and timer operations, then feeds resulting events back.

use std::collections::VecDeque;

pub type SessionId = u32;

const FAILURE_WINDOW_MS: u64 = 60_000;
const CRASH_LOOP_FAILURE_LIMIT: usize = 5;
const CRASH_LOOP_COOLDOWN_MS: u64 = 60_000;
const RESTART_BASE_DELAY_MS: u64 = 1_000;
const RESTART_MAX_DELAY_MS: u64 = 30_000;

/// Events reported by Windows session notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChange {
    Logon {
        session_id: SessionId,
    },
    Logoff {
        session_id: SessionId,
    },
    Lock {
        session_id: SessionId,
    },
    Unlock {
        session_id: SessionId,
    },
    /// The active console session changed, including a fast user switch.
    FastUserSwitch {
        active_session: Option<SessionId>,
    },
}

/// Inputs from the service adapter and helper process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorInput {
    ServiceStarted { active_session: Option<SessionId> },
    ServiceStopRequested,
    SessionChanged(SessionChange),
    HelperExited { generation: u64, exit_code: i32 },
    Tick,
}

/// Why the supervisor requested an orderly helper stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    SessionChanged,
    SessionLocked,
    SessionEnded,
    ServiceStopping,
}

/// Actions for the Windows service adapter to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorAction {
    StartHelper {
        session_id: SessionId,
        generation: u64,
    },
    StopHelper {
        session_id: SessionId,
        generation: u64,
        reason: StopReason,
    },
    ScheduleRestart {
        session_id: SessionId,
        at_ms: u64,
    },
    Log(SupervisorLog),
}

/// Structured lifecycle messages for the service adapter's logger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorLog {
    ServiceStarted,
    ServiceStopRequested,
    HelperStartRequested {
        session_id: SessionId,
    },
    HelperStopRequested {
        reason: StopReason,
    },
    HelperExited {
        exit_code: i32,
    },
    RestartScheduled {
        delay_ms: u64,
    },
    CrashLoopCooldown {
        failures_in_window: usize,
        duration_ms: u64,
    },
    IgnoredStaleHelperExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelperState {
    Stopped,
    Running {
        session_id: SessionId,
        generation: u64,
    },
    Stopping {
        session_id: SessionId,
        generation: u64,
    },
    RestartScheduled {
        session_id: SessionId,
        at_ms: u64,
    },
}

/// Deterministic, platform-neutral policy for supervising one capture helper.
///
/// Time is supplied by the caller in monotonic milliseconds. Every emitted
/// deadline uses that same clock so tests can drive the state machine without
/// sleeping or starting a child process.
#[derive(Debug, Clone)]
pub struct Supervisor {
    service_running: bool,
    desired_session: Option<SessionId>,
    session_locked: bool,
    helper: HelperState,
    generation: u64,
    failure_times_ms: VecDeque<u64>,
    cooldown_until_ms: Option<u64>,
    last_now_ms: Option<u64>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    /// Creates a stopped supervisor with no active console session.
    pub fn new() -> Self {
        Self {
            service_running: false,
            desired_session: None,
            session_locked: false,
            helper: HelperState::Stopped,
            generation: 0,
            failure_times_ms: VecDeque::new(),
            cooldown_until_ms: None,
            last_now_ms: None,
        }
    }

    /// Applies one service, session, process, or timer event and returns the
    /// process/timer/log operations that the adapter should perform.
    pub fn handle(&mut self, now_ms: u64, input: SupervisorInput) -> Vec<SupervisorAction> {
        let now_ms = self.monotonic_now(now_ms);
        match input {
            SupervisorInput::ServiceStarted { active_session } => {
                if self.service_running {
                    return Vec::new();
                }
                self.service_running = true;
                self.desired_session = active_session;
                self.session_locked = false;
                let mut actions = vec![SupervisorAction::Log(SupervisorLog::ServiceStarted)];
                actions.extend(self.reconcile(now_ms, StopReason::SessionChanged));
                actions
            }
            SupervisorInput::ServiceStopRequested => self.stop_service(),
            SupervisorInput::SessionChanged(change) => self.handle_session_change(now_ms, change),
            SupervisorInput::HelperExited {
                generation,
                exit_code,
            } => self.handle_helper_exit(now_ms, generation, exit_code),
            SupervisorInput::Tick => self.handle_tick(now_ms),
        }
    }

    /// The session the service currently intends to host, if any.
    pub fn desired_session(&self) -> Option<SessionId> {
        self.desired_session
    }

    /// Whether the service has not received a stop request.
    pub fn service_running(&self) -> bool {
        self.service_running
    }

    /// Generation token for the currently running or stopping helper.
    /// It lets the adapter discard exit notifications from an older process.
    pub fn helper_generation(&self) -> Option<u64> {
        match self.helper {
            HelperState::Running { generation, .. } | HelperState::Stopping { generation, .. } => {
                Some(generation)
            }
            HelperState::Stopped | HelperState::RestartScheduled { .. } => None,
        }
    }

    fn monotonic_now(&mut self, now_ms: u64) -> u64 {
        let effective = self.last_now_ms.map_or(now_ms, |last| last.max(now_ms));
        self.last_now_ms = Some(effective);
        effective
    }

    fn stop_service(&mut self) -> Vec<SupervisorAction> {
        if !self.service_running {
            return Vec::new();
        }
        self.service_running = false;
        self.desired_session = None;
        self.session_locked = false;
        let mut actions = vec![SupervisorAction::Log(SupervisorLog::ServiceStopRequested)];
        actions.extend(self.reconcile(
            self.last_now_ms.unwrap_or_default(),
            StopReason::ServiceStopping,
        ));
        actions
    }

    fn handle_session_change(
        &mut self,
        now_ms: u64,
        change: SessionChange,
    ) -> Vec<SupervisorAction> {
        if !self.service_running {
            return Vec::new();
        }
        let reason = match change {
            SessionChange::Logon { session_id } => {
                self.desired_session = Some(session_id);
                self.session_locked = false;
                StopReason::SessionChanged
            }
            SessionChange::Logoff { session_id } => {
                if self.desired_session != Some(session_id) {
                    return Vec::new();
                }
                self.desired_session = None;
                self.session_locked = false;
                StopReason::SessionEnded
            }
            SessionChange::Lock { session_id } => {
                if self.desired_session != Some(session_id) {
                    return Vec::new();
                }
                self.session_locked = true;
                StopReason::SessionLocked
            }
            SessionChange::Unlock { session_id } => {
                if self
                    .desired_session
                    .is_some_and(|active| active != session_id)
                {
                    return Vec::new();
                }
                self.desired_session = Some(session_id);
                self.session_locked = false;
                StopReason::SessionChanged
            }
            SessionChange::FastUserSwitch { active_session } => {
                self.desired_session = active_session;
                self.session_locked = false;
                StopReason::SessionChanged
            }
        };
        self.reconcile(now_ms, reason)
    }

    fn handle_helper_exit(
        &mut self,
        now_ms: u64,
        generation: u64,
        exit_code: i32,
    ) -> Vec<SupervisorAction> {
        let expected_stop = matches!(
            self.helper,
            HelperState::Stopping {
                generation: current,
                ..
            } if current == generation
        );
        let unexpected_exit = matches!(
            self.helper,
            HelperState::Running {
                generation: current,
                ..
            } if current == generation
        );
        if !expected_stop && !unexpected_exit {
            return vec![SupervisorAction::Log(SupervisorLog::IgnoredStaleHelperExit)];
        }

        self.helper = HelperState::Stopped;
        let mut actions = vec![SupervisorAction::Log(SupervisorLog::HelperExited {
            exit_code,
        })];
        if unexpected_exit {
            actions.extend(self.record_failure_and_schedule(now_ms));
        } else {
            actions.extend(self.reconcile(now_ms, StopReason::SessionChanged));
        }
        actions
    }

    fn handle_tick(&mut self, now_ms: u64) -> Vec<SupervisorAction> {
        let scheduled = match self.helper {
            HelperState::RestartScheduled { session_id, at_ms } if now_ms >= at_ms => {
                Some(session_id)
            }
            _ => None,
        };
        if let Some(session_id) = scheduled {
            self.helper = HelperState::Stopped;
            if self.should_run_session() == Some(session_id) {
                return self.start_or_reschedule(now_ms, session_id);
            }
        }
        self.reconcile(now_ms, StopReason::SessionChanged)
    }

    fn record_failure_and_schedule(&mut self, now_ms: u64) -> Vec<SupervisorAction> {
        while self
            .failure_times_ms
            .front()
            .is_some_and(|failed_at| now_ms.saturating_sub(*failed_at) >= FAILURE_WINDOW_MS)
        {
            self.failure_times_ms.pop_front();
        }
        self.failure_times_ms.push_back(now_ms);
        let failure_count = self.failure_times_ms.len();
        let Some(session_id) = self.should_run_session() else {
            self.helper = HelperState::Stopped;
            return Vec::new();
        };

        if failure_count > CRASH_LOOP_FAILURE_LIMIT {
            let until_ms = now_ms.saturating_add(CRASH_LOOP_COOLDOWN_MS);
            self.cooldown_until_ms = Some(until_ms);
            let mut actions = vec![SupervisorAction::Log(SupervisorLog::CrashLoopCooldown {
                failures_in_window: failure_count,
                duration_ms: CRASH_LOOP_COOLDOWN_MS,
            })];
            actions.extend(self.schedule_restart(now_ms, session_id, until_ms));
            return actions;
        }

        let shift = u32::try_from(failure_count.saturating_sub(1)).unwrap_or(u32::MAX);
        let delay_ms = RESTART_BASE_DELAY_MS
            .saturating_mul(1_u64.checked_shl(shift.min(63)).unwrap_or(u64::MAX))
            .min(RESTART_MAX_DELAY_MS);
        let at_ms = now_ms.saturating_add(delay_ms);
        self.schedule_restart(now_ms, session_id, at_ms)
    }

    fn reconcile(&mut self, now_ms: u64, reason: StopReason) -> Vec<SupervisorAction> {
        let wanted = self.should_run_session();
        match self.helper {
            HelperState::Running {
                session_id,
                generation,
            } if wanted != Some(session_id) => {
                self.helper = HelperState::Stopping {
                    session_id,
                    generation,
                };
                vec![
                    SupervisorAction::Log(SupervisorLog::HelperStopRequested { reason }),
                    SupervisorAction::StopHelper {
                        session_id,
                        generation,
                        reason,
                    },
                ]
            }
            HelperState::Running { .. } | HelperState::Stopping { .. } => Vec::new(),
            HelperState::RestartScheduled { session_id, .. } => match wanted {
                None => {
                    self.helper = HelperState::Stopped;
                    Vec::new()
                }
                Some(wanted_session) if wanted_session != session_id => {
                    self.helper = HelperState::Stopped;
                    self.start_or_reschedule(now_ms, wanted_session)
                }
                Some(_) => Vec::new(),
            },
            HelperState::Stopped => wanted.map_or_else(Vec::new, |session_id| {
                self.start_or_reschedule(now_ms, session_id)
            }),
        }
    }

    fn should_run_session(&self) -> Option<SessionId> {
        if self.service_running && !self.session_locked {
            self.desired_session
        } else {
            None
        }
    }

    fn start_or_reschedule(&mut self, now_ms: u64, session_id: SessionId) -> Vec<SupervisorAction> {
        if let Some(until_ms) = self.cooldown_until_ms.filter(|until| *until > now_ms) {
            return self.schedule_restart(now_ms, session_id, until_ms);
        }
        self.cooldown_until_ms = None;
        self.generation = self.generation.wrapping_add(1).max(1);
        let generation = self.generation;
        self.helper = HelperState::Running {
            session_id,
            generation,
        };
        vec![
            SupervisorAction::Log(SupervisorLog::HelperStartRequested { session_id }),
            SupervisorAction::StartHelper {
                session_id,
                generation,
            },
        ]
    }

    fn schedule_restart(
        &mut self,
        now_ms: u64,
        session_id: SessionId,
        at_ms: u64,
    ) -> Vec<SupervisorAction> {
        let at_ms = at_ms.max(now_ms);
        self.helper = HelperState::RestartScheduled { session_id, at_ms };
        let delay_ms = at_ms.saturating_sub(now_ms);
        vec![
            SupervisorAction::Log(SupervisorLog::RestartScheduled { delay_ms }),
            SupervisorAction::ScheduleRestart { session_id, at_ms },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(supervisor: &mut Supervisor, at_ms: u64, session_id: SessionId) -> u64 {
        let actions = supervisor.handle(
            at_ms,
            SupervisorInput::ServiceStarted {
                active_session: Some(session_id),
            },
        );
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::StartHelper { session_id: started, .. } if *started == session_id
        )));
        supervisor.helper_generation().expect("helper should start")
    }

    fn scheduled_at(actions: &[SupervisorAction]) -> u64 {
        actions
            .iter()
            .find_map(|action| match action {
                SupervisorAction::ScheduleRestart { at_ms, .. } => Some(*at_ms),
                _ => None,
            })
            .expect("restart deadline should be emitted")
    }

    #[test]
    fn service_start_and_duplicate_logon_start_one_helper() {
        let mut supervisor = Supervisor::new();
        let generation = start(&mut supervisor, 0, 7);

        assert!(supervisor
            .handle(
                1,
                SupervisorInput::SessionChanged(SessionChange::Logon { session_id: 7 }),
            )
            .is_empty());
        assert_eq!(supervisor.helper_generation(), Some(generation));
    }

    #[test]
    fn session_switch_stops_old_helper_then_starts_new_session() {
        let mut supervisor = Supervisor::new();
        let old_generation = start(&mut supervisor, 0, 3);
        let actions = supervisor.handle(
            10,
            SupervisorInput::SessionChanged(SessionChange::FastUserSwitch {
                active_session: Some(9),
            }),
        );
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::StopHelper { session_id: 3, generation, .. }
                if *generation == old_generation
        )));
        assert!(supervisor.helper_generation().is_some());

        let actions = supervisor.handle(
            20,
            SupervisorInput::HelperExited {
                generation: old_generation,
                exit_code: 0,
            },
        );
        assert!(actions
            .iter()
            .any(|action| matches!(action, SupervisorAction::StartHelper { session_id: 9, .. })));
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::Log(SupervisorLog::HelperExited { exit_code: 0 })
        )));
        assert_eq!(supervisor.desired_session(), Some(9));
    }

    #[test]
    fn lock_stops_helper_and_unlock_restarts_it() {
        let mut supervisor = Supervisor::new();
        let generation = start(&mut supervisor, 0, 2);
        let lock_actions = supervisor.handle(
            5,
            SupervisorInput::SessionChanged(SessionChange::Lock { session_id: 2 }),
        );
        assert!(lock_actions.iter().any(|action| matches!(
            action,
            SupervisorAction::StopHelper {
                reason: StopReason::SessionLocked,
                ..
            }
        )));

        assert!(supervisor
            .handle(
                6,
                SupervisorInput::SessionChanged(SessionChange::Unlock { session_id: 2 }),
            )
            .is_empty());
        let actions = supervisor.handle(
            7,
            SupervisorInput::HelperExited {
                generation,
                exit_code: 0,
            },
        );
        assert!(actions
            .iter()
            .any(|action| matches!(action, SupervisorAction::StartHelper { session_id: 2, .. })));
    }

    #[test]
    fn logoff_stops_helper_without_scheduling_a_restart() {
        let mut supervisor = Supervisor::new();
        let generation = start(&mut supervisor, 0, 5);
        let actions = supervisor.handle(
            10,
            SupervisorInput::SessionChanged(SessionChange::Logoff { session_id: 5 }),
        );
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::StopHelper {
                reason: StopReason::SessionEnded,
                ..
            }
        )));
        let actions = supervisor.handle(
            11,
            SupervisorInput::HelperExited {
                generation,
                exit_code: 0,
            },
        );
        assert!(!actions
            .iter()
            .any(|action| matches!(action, SupervisorAction::StartHelper { .. })));
        assert!(!actions
            .iter()
            .any(|action| matches!(action, SupervisorAction::ScheduleRestart { .. })));
    }

    #[test]
    fn unexpected_exit_uses_exponential_backoff_and_tick_restarts() {
        let mut supervisor = Supervisor::new();
        let generation = start(&mut supervisor, 10, 4);
        let actions = supervisor.handle(
            10,
            SupervisorInput::HelperExited {
                generation,
                exit_code: 11,
            },
        );
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::ScheduleRestart {
                session_id: 4,
                at_ms: 1_010
            }
        )));
        assert!(supervisor
            .handle(1_009, SupervisorInput::Tick)
            .iter()
            .all(|action| !matches!(action, SupervisorAction::StartHelper { .. })));
        let actions = supervisor.handle(1_010, SupervisorInput::Tick);
        assert!(actions
            .iter()
            .any(|action| matches!(action, SupervisorAction::StartHelper { session_id: 4, .. })));

        let next_generation = supervisor.helper_generation().expect("restarted helper");
        let actions = supervisor.handle(
            1_011,
            SupervisorInput::HelperExited {
                generation: next_generation,
                exit_code: 12,
            },
        );
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::ScheduleRestart { at_ms: 3_011, .. }
        )));
    }

    #[test]
    fn sixth_failure_in_the_window_enters_cooldown_until_deadline() {
        let mut supervisor = Supervisor::new();
        let mut now_ms = 0;
        let mut generation = start(&mut supervisor, now_ms, 1);
        let mut sixth_failure_actions = Vec::new();

        for failure in 1..=6 {
            let actions = supervisor.handle(
                now_ms,
                SupervisorInput::HelperExited {
                    generation,
                    exit_code: 1,
                },
            );
            if failure == 6 {
                sixth_failure_actions = actions;
                break;
            }
            let at_ms = scheduled_at(&actions);
            let actions = supervisor.handle(at_ms, SupervisorInput::Tick);
            generation = supervisor
                .helper_generation()
                .expect("backoff should restart");
            assert!(actions
                .iter()
                .any(|action| matches!(action, SupervisorAction::StartHelper { .. })));
            now_ms = at_ms;
        }

        assert!(sixth_failure_actions.iter().any(|action| matches!(
            action,
            SupervisorAction::Log(SupervisorLog::CrashLoopCooldown {
                failures_in_window: 6,
                duration_ms: CRASH_LOOP_COOLDOWN_MS,
            })
        )));
        let cooldown_until = scheduled_at(&sixth_failure_actions);
        assert!(supervisor
            .handle(cooldown_until - 1, SupervisorInput::Tick)
            .iter()
            .all(|action| !matches!(action, SupervisorAction::StartHelper { .. })));
        assert!(supervisor
            .handle(cooldown_until, SupervisorInput::Tick)
            .iter()
            .any(|action| matches!(action, SupervisorAction::StartHelper { .. })));
    }

    #[test]
    fn a_failure_exactly_sixty_seconds_old_is_outside_the_crash_window() {
        let mut supervisor = Supervisor::new();
        let mut now_ms = 0;
        let mut generation = start(&mut supervisor, now_ms, 1);
        for _ in 0..5 {
            let actions = supervisor.handle(
                now_ms,
                SupervisorInput::HelperExited {
                    generation,
                    exit_code: 1,
                },
            );
            let at_ms = scheduled_at(&actions);
            supervisor.handle(at_ms, SupervisorInput::Tick);
            generation = supervisor
                .helper_generation()
                .expect("backoff should restart");
            now_ms = at_ms;
        }

        // Move to a time that removes the first crash from the rolling window.
        let target_ms = FAILURE_WINDOW_MS;
        let actions = supervisor.handle(
            target_ms,
            SupervisorInput::HelperExited {
                generation,
                exit_code: 1,
            },
        );
        assert!(!actions.iter().any(|action| matches!(
            action,
            SupervisorAction::Log(SupervisorLog::CrashLoopCooldown { .. })
        )));
        assert_eq!(supervisor.failure_times_ms.len(), CRASH_LOOP_FAILURE_LIMIT);
        assert!(target_ms >= now_ms);
    }

    #[test]
    fn service_stop_requests_active_helper_stop() {
        let mut supervisor = Supervisor::new();
        let generation = start(&mut supervisor, 0, 6);
        let actions = supervisor.handle(1, SupervisorInput::ServiceStopRequested);
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::StopHelper {
                session_id: 6,
                generation: stopped_generation,
                reason: StopReason::ServiceStopping,
            } if *stopped_generation == generation
        )));
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::Log(SupervisorLog::ServiceStopRequested)
        )));
    }

    #[test]
    fn service_stop_cancels_backoff_and_never_restarts() {
        let mut supervisor = Supervisor::new();
        let generation = start(&mut supervisor, 0, 6);
        let crash = supervisor.handle(
            1,
            SupervisorInput::HelperExited {
                generation,
                exit_code: 1,
            },
        );
        assert!(crash
            .iter()
            .any(|action| matches!(action, SupervisorAction::ScheduleRestart { .. })));
        let stop = supervisor.handle(2, SupervisorInput::ServiceStopRequested);
        assert!(stop.iter().all(|action| !matches!(
            action,
            SupervisorAction::StartHelper { .. }
                | SupervisorAction::ScheduleRestart { .. }
                | SupervisorAction::StopHelper { .. }
        )));
        assert!(supervisor
            .handle(u64::MAX, SupervisorInput::Tick)
            .iter()
            .all(|action| !matches!(action, SupervisorAction::StartHelper { .. })));
        assert!(!supervisor.service_running());
    }

    #[test]
    fn stale_helper_exit_cannot_stop_or_restart_a_new_generation() {
        let mut supervisor = Supervisor::new();
        let first_generation = start(&mut supervisor, 0, 8);
        let switch = supervisor.handle(
            1,
            SupervisorInput::SessionChanged(SessionChange::FastUserSwitch {
                active_session: Some(10),
            }),
        );
        assert!(switch
            .iter()
            .any(|action| matches!(action, SupervisorAction::StopHelper { .. })));
        let actions = supervisor.handle(
            2,
            SupervisorInput::HelperExited {
                generation: first_generation,
                exit_code: 0,
            },
        );
        assert!(actions
            .iter()
            .any(|action| matches!(action, SupervisorAction::StartHelper { .. })));
        let new_generation = supervisor.helper_generation().expect("new helper");
        let actions = supervisor.handle(
            3,
            SupervisorInput::HelperExited {
                generation: first_generation,
                exit_code: 1,
            },
        );
        assert_eq!(supervisor.helper_generation(), Some(new_generation));
        assert!(actions.iter().any(|action| matches!(
            action,
            SupervisorAction::Log(SupervisorLog::IgnoredStaleHelperExit)
        )));
    }

    #[test]
    fn fast_switch_to_no_console_session_stops_without_restarting() {
        let mut supervisor = Supervisor::new();
        let generation = start(&mut supervisor, 0, 12);
        supervisor.handle(
            1,
            SupervisorInput::SessionChanged(SessionChange::FastUserSwitch {
                active_session: None,
            }),
        );
        let actions = supervisor.handle(
            2,
            SupervisorInput::HelperExited {
                generation,
                exit_code: 0,
            },
        );
        assert!(actions
            .iter()
            .all(|action| !matches!(action, SupervisorAction::StartHelper { .. })));
    }
}
