//! Bounded helper-side input dispatch for authenticated control events.

use racc_core::HostConnectionId;
use racc_input::{InputInjectionController, InputInjector};
use racc_proto::InputEvent;
use racc_topology::{DisplayId, Topology};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Maximum number of control input events waiting for the helper injection worker.
pub(super) const INPUT_QUEUE_CAPACITY: usize = 256;
const INPUT_WORKER_POLL: Duration = Duration::from_millis(5);

/// Snapshot that authorizes input for one authenticated connection and stream reset.
#[derive(Clone)]
pub(super) struct HostInputSession {
    connection_id: HostConnectionId,
    epoch: u16,
    display_id: DisplayId,
    topology: Topology,
}

impl HostInputSession {
    /// Creates a session snapshot from the reset and topology announced by the host runtime.
    pub(super) fn new(
        connection_id: HostConnectionId,
        epoch: u16,
        display_id: DisplayId,
        topology: Topology,
    ) -> Self {
        Self {
            connection_id,
            epoch,
            display_id,
            topology,
        }
    }

    /// Builds a replacement input snapshot only when its selected display remains compatible.
    pub(super) fn with_updated_topology(
        &self,
        previous: &Topology,
        topology: Topology,
    ) -> Option<Self> {
        if racc_topology::requires_stream_reset(
            &racc_topology::diff_topologies(previous, &topology),
            Some(self.display_id),
        ) || !topology
            .displays()
            .iter()
            .any(|display| display.id() == self.display_id && display.flags().available())
        {
            return None;
        }
        Some(Self::new(
            self.connection_id,
            self.epoch,
            self.display_id,
            topology,
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InputQueueError {
    Inactive,
    Full,
    Stopped,
}

struct QueuedInput {
    connection_id: HostConnectionId,
    event: InputEvent,
    generation: u64,
}

struct WorkerControl {
    desired: Mutex<Option<HostInputSession>>,
    session_generation: AtomicU64,
    event_generation: AtomicU64,
    blocked: AtomicBool,
    release_requested: AtomicBool,
    stopping: AtomicBool,
}

/// A cloneable, nonblocking endpoint safe to use from a control-network callback.
#[derive(Clone)]
pub(super) struct HostInputHandle {
    sender: SyncSender<QueuedInput>,
    control: Arc<WorkerControl>,
}

impl HostInputHandle {
    /// Tries to queue one event without waiting for injection or queue space.
    pub(super) fn try_enqueue(
        &self,
        connection_id: HostConnectionId,
        event: InputEvent,
    ) -> Result<(), InputQueueError> {
        if self.control.stopping.load(Ordering::Acquire) {
            return Err(InputQueueError::Stopped);
        }
        if self.control.blocked.load(Ordering::Acquire) {
            return Err(InputQueueError::Inactive);
        }
        let queued = QueuedInput {
            connection_id,
            event,
            generation: self.control.event_generation.load(Ordering::Acquire),
        };
        match self.sender.try_send(queued) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                // Discard the old backlog and release held input before accepting newer events.
                self.control.event_generation.fetch_add(1, Ordering::AcqRel);
                self.control
                    .release_requested
                    .store(true, Ordering::Release);
                Err(InputQueueError::Full)
            }
            Err(TrySendError::Disconnected(_)) => Err(InputQueueError::Stopped),
        }
    }

    /// Blocks new input and requests release without locking or waiting on the injector.
    pub(super) fn request_deactivate_nonblocking(&self) {
        self.control.blocked.store(true, Ordering::Release);
        self.control.event_generation.fetch_add(1, Ordering::AcqRel);
        self.control
            .release_requested
            .store(true, Ordering::Release);
    }

    /// Replaces authorization after a successfully delivered StreamReset.
    pub(super) fn activate_session(&self, session: HostInputSession) {
        self.control.blocked.store(true, Ordering::Release);
        self.control.event_generation.fetch_add(1, Ordering::AcqRel);
        {
            let mut desired = lock(&self.control.desired);
            *desired = Some(session);
            self.control
                .session_generation
                .fetch_add(1, Ordering::AcqRel);
        }
        self.control
            .release_requested
            .store(true, Ordering::Release);
        self.control.blocked.store(false, Ordering::Release);
    }

    /// Clears the active session and ensures its queued events are discarded.
    pub(super) fn deactivate(&self) {
        self.request_deactivate_nonblocking();
        let mut desired = lock(&self.control.desired);
        *desired = None;
        self.control
            .session_generation
            .fetch_add(1, Ordering::AcqRel);
    }
}

/// Routes an authorized runtime input action into the bounded helper worker.
///
/// Actions without a connection identity are ignored because they cannot be
/// associated with the authenticated viewer session required by the worker.
pub(super) fn route_runtime_input_action(
    handle: &HostInputHandle,
    connection_id: Option<HostConnectionId>,
    event: InputEvent,
) {
    if let Some(connection_id) = connection_id {
        let _ = handle.try_enqueue(connection_id, event);
    }
}
/// Owns the helper input worker and joins it after requesting release on shutdown.
pub(super) struct HostInputWorker<I: InputInjector + Send + 'static> {
    handle: HostInputHandle,
    join: Option<JoinHandle<()>>,
    marker: std::marker::PhantomData<I>,
}

impl<I: InputInjector + Send + 'static> HostInputWorker<I> {
    /// Starts a bounded input worker using the production queue capacity.
    pub(super) fn new(injector: I) -> std::io::Result<Self> {
        Self::with_capacity(injector, INPUT_QUEUE_CAPACITY)
    }

    fn with_capacity(injector: I, capacity: usize) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(capacity.max(1));
        let control = Arc::new(WorkerControl {
            desired: Mutex::new(None),
            session_generation: AtomicU64::new(0),
            event_generation: AtomicU64::new(0),
            blocked: AtomicBool::new(true),
            release_requested: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        });
        let thread_control = Arc::clone(&control);
        let join = thread::Builder::new()
            .name("racc-host-input".to_owned())
            .spawn(move || run_worker(receiver, thread_control, injector))?;
        Ok(Self {
            handle: HostInputHandle { sender, control },
            join: Some(join),
            marker: std::marker::PhantomData,
        })
    }

    /// Returns a cloneable endpoint for the network callback and foreground lifecycle.
    pub(super) fn handle(&self) -> HostInputHandle {
        self.handle.clone()
    }

    /// Requests release and joins the worker. Safe to call repeatedly.
    pub(super) fn shutdown(&mut self) {
        self.handle.deactivate();
        self.handle.control.stopping.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl<I: InputInjector + Send + 'static> Drop for HostInputWorker<I> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_worker<I: InputInjector>(
    receiver: Receiver<QueuedInput>,
    control: Arc<WorkerControl>,
    injector: I,
) {
    let started_at = Instant::now();
    let mut controller = InputInjectionController::new(injector);
    let mut active: Option<HostInputSession> = None;
    let mut observed_session_generation = u64::MAX;

    loop {
        let queued = match receiver.recv_timeout(INPUT_WORKER_POLL) {
            Ok(input) => Some(input),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if control.blocked.load(Ordering::Acquire) {
            if controller.deactivate().is_err() {
                control.release_requested.store(true, Ordering::Release);
            }
            active = None;
            observed_session_generation = control.session_generation.load(Ordering::Acquire);
            if control.stopping.load(Ordering::Acquire) {
                break;
            }
            continue;
        }

        let generation = control.session_generation.load(Ordering::Acquire);
        if generation != observed_session_generation {
            let desired = lock(&control.desired).clone();
            let Some(session) = desired else {
                if controller.deactivate().is_err() {
                    control.release_requested.store(true, Ordering::Release);
                }
                active = None;
                observed_session_generation = generation;
                continue;
            };
            if controller
                .begin_session(session.epoch, session.display_id, true)
                .is_err()
            {
                control.release_requested.store(true, Ordering::Release);
                active = None;
                continue;
            }
            active = Some(session);
            observed_session_generation = generation;
        }

        if control.release_requested.swap(false, Ordering::AcqRel)
            && controller.release_all().is_err()
        {
            control.release_requested.store(true, Ordering::Release);
        }

        let Some(queued) = queued else {
            if control.stopping.load(Ordering::Acquire) {
                break;
            }
            continue;
        };
        if control.blocked.load(Ordering::Acquire)
            || queued.generation != control.event_generation.load(Ordering::Acquire)
        {
            continue;
        }
        let Some(session) = active.as_ref() else {
            continue;
        };
        if queued.connection_id != session.connection_id
            || queued.event.epoch != session.epoch
            || queued.event.display_id != session.display_id.get()
        {
            continue;
        }
        let now_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let _ = controller.process_event(queued.event, &session.topology, now_ms);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_input::{InputCommand, InputError, InputInjector};
    use racc_proto::InputEventKind;
    use racc_topology::{Display, DisplayFlags};
    use std::sync::Condvar;
    use std::time::{Duration, Instant};

    #[derive(Clone, Default)]
    struct RecordingInjector(Arc<Mutex<Vec<InputCommand>>>);

    impl InputInjector for RecordingInjector {
        fn inject(&mut self, command: InputCommand) -> Result<(), InputError> {
            lock(&self.0).push(command);
            Ok(())
        }
    }

    fn topology() -> (Topology, DisplayId) {
        let id = DisplayId::new(7).unwrap_or_else(|| unreachable!("test id"));
        let display = Display::new(
            id,
            "test display",
            -1920,
            0,
            1920,
            1080,
            1000,
            60_000,
            DisplayFlags::new(true, true, true, false),
        );
        (
            Topology::new(1, vec![display], None).unwrap_or_else(|_| unreachable!("test topology")),
            id,
        )
    }

    fn session(connection_id: HostConnectionId, epoch: u16) -> HostInputSession {
        let (topology, display_id) = topology();
        HostInputSession::new(connection_id, epoch, display_id, topology)
    }

    fn key(epoch: u16, display_id: u32, pressed: bool) -> InputEvent {
        InputEvent {
            epoch,
            display_id,
            event: InputEventKind::Key {
                hid_usage: 0x04,
                pressed,
                modifiers: 0,
            },
        }
    }

    fn wait_for_len(
        commands: &Arc<Mutex<Vec<InputCommand>>>,
        expected: usize,
    ) -> Vec<InputCommand> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = lock(commands).clone();
            if snapshot.len() >= expected || Instant::now() >= deadline {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn only_connection_current_epoch_and_display_inject() {
        let injector = RecordingInjector::default();
        let commands = Arc::clone(&injector.0);
        let worker = HostInputWorker::with_capacity(injector, 8).expect("worker thread");
        let handle = worker.handle();
        handle.activate_session(session(11, 4));

        // Messages carry the connection id assigned only after HostControlServer authorization.
        assert_eq!(handle.try_enqueue(12, key(4, 7, true)), Ok(()));
        assert_eq!(handle.try_enqueue(11, key(3, 7, true)), Ok(()));
        assert_eq!(handle.try_enqueue(11, key(4, 8, true)), Ok(()));
        thread::sleep(Duration::from_millis(30));
        assert!(lock(&commands).is_empty());

        assert_eq!(handle.try_enqueue(11, key(4, 7, true)), Ok(()));
        assert_eq!(
            wait_for_len(&commands, 1),
            vec![InputCommand::Key {
                hid_usage: 0x04,
                pressed: true,
            }]
        );
    }

    #[test]
    fn topology_rebase_preserves_session_when_selected_display_remains_compatible() {
        let (previous, display_id) = topology();
        let session = HostInputSession::new(11, 4, display_id, previous.clone());
        let updated = Topology::new(
            2,
            vec![
                Display::new(
                    display_id,
                    "renamed display",
                    -2560,
                    0,
                    1920,
                    1080,
                    1000,
                    60_000,
                    DisplayFlags::new(true, true, true, false),
                ),
                Display::new(
                    DisplayId::new(8).unwrap_or_else(|| unreachable!("test display id")),
                    "new display",
                    -640,
                    1080,
                    1280,
                    720,
                    1000,
                    60_000,
                    DisplayFlags::new(false, true, true, false),
                ),
            ],
            None,
        )
        .unwrap_or_else(|_| unreachable!("test topology"));

        let rebased = session
            .with_updated_topology(&previous, updated.clone())
            .unwrap_or_else(|| unreachable!("compatible selected display"));
        assert_eq!(rebased.connection_id, 11);
        assert_eq!(rebased.epoch, 4);
        assert_eq!(rebased.display_id, display_id);
        assert_eq!(rebased.topology, updated);
    }

    #[test]
    fn topology_rebase_waits_for_reset_when_selected_display_geometry_changes() {
        let (previous, display_id) = topology();
        let session = HostInputSession::new(11, 4, display_id, previous.clone());
        let updated = Topology::new(
            2,
            vec![Display::new(
                display_id,
                "test display",
                -1920,
                0,
                1280,
                720,
                1000,
                60_000,
                DisplayFlags::new(true, true, true, false),
            )],
            None,
        )
        .unwrap_or_else(|_| unreachable!("test topology"));
        assert!(session.with_updated_topology(&previous, updated).is_none());
    }

    #[test]
    fn runtime_input_action_uses_the_bounded_worker_and_requires_a_connection() {
        let injector = RecordingInjector::default();
        let commands = Arc::clone(&injector.0);
        let worker = HostInputWorker::with_capacity(injector, 8).expect("worker thread");
        let handle = worker.handle();
        handle.activate_session(session(11, 4));

        route_runtime_input_action(&handle, None, key(4, 7, true));
        thread::sleep(Duration::from_millis(30));
        assert!(lock(&commands).is_empty());

        route_runtime_input_action(&handle, Some(11), key(4, 7, true));
        assert_eq!(
            wait_for_len(&commands, 1),
            vec![InputCommand::Key {
                hid_usage: 0x04,
                pressed: true,
            }]
        );
    }
    #[test]
    fn deactivation_and_shutdown_release_held_keys_and_buttons() {
        let injector = RecordingInjector::default();
        let commands = Arc::clone(&injector.0);
        let mut worker = HostInputWorker::with_capacity(injector, 8).expect("worker thread");
        let handle = worker.handle();
        handle.activate_session(session(11, 4));
        assert_eq!(handle.try_enqueue(11, key(4, 7, true)), Ok(()));
        assert_eq!(
            handle.try_enqueue(
                11,
                InputEvent {
                    epoch: 4,
                    display_id: 7,
                    event: InputEventKind::MouseButton {
                        button: 1,
                        pressed: true,
                    },
                }
            ),
            Ok(())
        );
        assert_eq!(wait_for_len(&commands, 2).len(), 2);

        // Pause, switch/reset and disconnect use this release-before-reactivation path.
        handle.deactivate();
        let released = wait_for_len(&commands, 4);
        assert_eq!(
            released[2],
            InputCommand::MouseButton {
                button: 1,
                pressed: false,
            }
        );
        assert_eq!(
            released[3],
            InputCommand::Key {
                hid_usage: 0x04,
                pressed: false,
            }
        );

        handle.activate_session(session(11, 5));
        assert_eq!(handle.try_enqueue(11, key(5, 7, true)), Ok(()));
        assert_eq!(
            wait_for_len(&commands, 5)[4],
            InputCommand::Key {
                hid_usage: 0x04,
                pressed: true,
            }
        );
        worker.shutdown();
        assert_eq!(
            lock(&commands)[5],
            InputCommand::Key {
                hid_usage: 0x04,
                pressed: false,
            }
        );
    }

    #[derive(Default)]
    struct BlockingState {
        commands: Vec<InputCommand>,
        entered: bool,
        unblock: bool,
    }

    #[derive(Clone, Default)]
    struct BlockingInjector(Arc<(Mutex<BlockingState>, Condvar)>);

    impl InputInjector for BlockingInjector {
        fn inject(&mut self, command: InputCommand) -> Result<(), InputError> {
            let (mutex, condition) = &*self.0;
            let mut state = lock(mutex);
            state.commands.push(command);
            if matches!(command, InputCommand::MouseMoveRelative { dx: 1, dy: 1 }) {
                state.entered = true;
                condition.notify_all();
                while !state.unblock {
                    state = condition
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
            Ok(())
        }
    }

    #[test]
    fn deactivation_releases_even_when_normal_queue_is_saturated() {
        let injector = BlockingInjector::default();
        let state = Arc::clone(&injector.0);
        let mut worker = HostInputWorker::with_capacity(injector, 1).expect("worker thread");
        let handle = worker.handle();
        handle.activate_session(session(11, 4));
        assert_eq!(handle.try_enqueue(11, key(4, 7, true)), Ok(()));
        wait_for_blocking_commands(&state, 1);

        let relative = |dx| InputEvent {
            epoch: 4,
            display_id: 7,
            event: InputEventKind::MouseMoveRel { dx, dy: dx },
        };
        assert_eq!(handle.try_enqueue(11, relative(1)), Ok(()));
        wait_for_injector_block(&state);
        assert_eq!(handle.try_enqueue(11, relative(2)), Ok(()));
        assert_eq!(
            handle.try_enqueue(11, relative(3)),
            Err(InputQueueError::Full)
        );
        handle.request_deactivate_nonblocking();
        {
            let (mutex, condition) = &*state;
            lock(mutex).unblock = true;
            condition.notify_all();
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let commands = lock(&state.0).commands.clone();
            if commands.iter().any(|command| {
                matches!(
                    command,
                    InputCommand::Key {
                        hid_usage: 0x04,
                        pressed: false
                    }
                )
            }) {
                assert!(!commands.iter().any(|command| matches!(
                    command,
                    InputCommand::MouseMoveRelative { dx: 2, .. }
                )));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not release after deactivation"
            );
            thread::sleep(Duration::from_millis(2));
        }
        worker.shutdown();
    }

    fn wait_for_injector_block(state: &Arc<(Mutex<BlockingState>, Condvar)>) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (mutex, condition) = &**state;
        let mut locked = lock(mutex);
        while !locked.entered {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "injector did not block");
            let (next, timeout) = condition
                .wait_timeout(locked, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locked = next;
            assert!(
                !timeout.timed_out() || locked.entered,
                "injector did not block"
            );
        }
    }

    fn wait_for_blocking_commands(state: &Arc<(Mutex<BlockingState>, Condvar)>, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = lock(&state.0).commands.clone();
            if snapshot.len() >= expected || Instant::now() >= deadline {
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}
