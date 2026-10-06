use crate::{
    fragment_count, slice_frame_into, BindPolicy, ForceKeyframe, FrameData, NetError, Pacer,
    Reassembler, ReassemblyEvent, SenderFrame,
};
use racc_proto::CursorUpdate;
use socket2::SockRef;
use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Default fixed frame interval for the 30 fps stream.
pub const DEFAULT_FRAME_INTERVAL_US: u64 = 33_333;
/// Portion of a frame interval reserved for sending that frame's packets.
pub const PACING_FRACTION_PERCENT: u8 = 60;
/// Maximum number of complete frames waiting to start packet transmission.
pub const SEND_QUEUE_MAX_FRAMES: usize = 2;
/// Requested UDP send socket buffer size.
pub const SEND_BUFFER_BYTES: usize = 1024 * 1024;
/// Requested UDP receive socket buffer size.
pub const RECEIVE_BUFFER_BYTES: usize = 2 * 1024 * 1024;
/// Maximum pause between UDP receive polls, in milliseconds.
pub const RECEIVE_POLL_INTERVAL_MS: u64 = 5;
const RECEIVE_BUFFER_LEN: usize = 2048;
const RECEIVER_EVENT_CAPACITY: usize = 64;
const FORCE_EVENT_CAPACITY: usize = 2;

/// Injectable sleeping abstraction for the real sender loop.
pub trait Sleeper: Send + Sync + 'static {
    /// Sleeps for the requested duration without busy-spinning.
    fn sleep(&self, duration: Duration);
}

/// Production sleeper backed by the standard thread sleep function.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// Result of one real paced UDP frame transmission.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoSendMetrics {
    /// Frame identifier.
    pub frame_id: u32,
    /// Number of UDP datagrams written.
    pub datagrams_sent: usize,
    /// Datagram bytes written, including headers.
    pub bytes_sent: usize,
    /// Time from the first to the last datagram write, in microseconds.
    pub send_duration_us: u64,
    /// Largest observed gap between adjacent datagram writes, in microseconds.
    pub max_inter_datagram_gap_us: u64,
    /// Largest actual sleep interval observed by the pacer, in microseconds.
    pub observed_sleep_granularity_us: u64,
}

/// Event emitted by a real UDP receiver worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoTransportEvent {
    /// An ordered encoded frame is ready for decode.
    FrameReady(FrameData),
    /// A keyframe should be requested on the TCP control channel.
    NeedKeyframe(u16),
    /// Cursor metadata arrived from the expected host IP.
    Cursor(CursorUpdate),
}

/// Snapshot of socket-wrapper filtering and bounded-event drops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoReceiverCounters {
    /// Datagrams from an unexpected source IP.
    pub unexpected_peer_datagrams: u64,
    /// Reassembly events dropped because the bounded consumer channel was full.
    pub event_channel_drops: u64,
}

/// Shared bounded queue for frames which have not yet begun transmission.
struct SenderQueue {
    state: Mutex<SenderQueueState>,
    changed: Condvar,
}

struct SenderQueueState {
    queue: VecDeque<PendingSend>,
    closed: bool,
}

struct PendingSend {
    frame: SenderFrame,
    result: SyncSender<Result<VideoSendMetrics, NetError>>,
}

impl SenderQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(SenderQueueState {
                queue: VecDeque::with_capacity(SEND_QUEUE_MAX_FRAMES),
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn enqueue(&self, pending: PendingSend) -> Result<Option<ForceKeyframe>, NetError> {
        let mut state = self.state.lock().map_err(|_| NetError::Poisoned)?;
        if state.closed {
            return Err(NetError::QueueClosed);
        }
        let dropped = if state.queue.len() >= SEND_QUEUE_MAX_FRAMES {
            state.queue.pop_front()
        } else {
            None
        };
        let signal = dropped.as_ref().map(|oldest| ForceKeyframe {
            epoch: oldest.frame.epoch,
            dropped_frame_id: oldest.frame.frame_id,
        });
        if let Some(oldest) = dropped {
            let _ = oldest.result.send(Err(NetError::QueueDropped));
        }
        state.queue.push_back(pending);
        self.changed.notify_one();
        Ok(signal)
    }

    fn dequeue(&self) -> Result<Option<PendingSend>, NetError> {
        let mut state = self.state.lock().map_err(|_| NetError::Poisoned)?;
        loop {
            if let Some(pending) = state.queue.pop_front() {
                return Ok(Some(pending));
            }
            if state.closed {
                return Ok(None);
            }
            state = self.changed.wait(state).map_err(|_| NetError::Poisoned)?;
        }
    }

    fn close(&self) -> Result<(), NetError> {
        let mut state = self.state.lock().map_err(|_| NetError::Poisoned)?;
        state.closed = true;
        for pending in state.queue.drain(..) {
            let _ = pending.result.send(Err(NetError::QueueClosed));
        }
        self.changed.notify_all();
        Ok(())
    }
}

/// UDP video sender with one paced worker and a two-frame latest-wins queue.
pub struct VideoSender {
    queue: Arc<SenderQueue>,
    force_keyframes: Receiver<ForceKeyframe>,
    force_keyframe_tx: SyncSender<ForceKeyframe>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl VideoSender {
    /// Binds a sender to an allowed local interface and validates its target IP.
    pub fn bind(
        local_addr: SocketAddr,
        target: SocketAddr,
        policy: BindPolicy,
        frame_interval_us: u64,
    ) -> Result<Self, NetError> {
        crate::validate_bind_addr(local_addr.ip(), policy)?;
        crate::validate_bind_addr(target.ip(), policy)?;
        let socket = UdpSocket::bind(local_addr)?;
        Self::from_socket(socket, target, policy, frame_interval_us)
    }

    /// Creates a sender from a bound UDP socket.
    ///
    /// This constructor validates both the socket's local address and the target IP.
    pub fn from_socket(
        socket: UdpSocket,
        target: SocketAddr,
        policy: BindPolicy,
        frame_interval_us: u64,
    ) -> Result<Self, NetError> {
        Self::from_socket_with_sleeper(
            socket,
            target,
            policy,
            frame_interval_us,
            Arc::new(ThreadSleeper),
        )
    }

    /// Creates a sender with an injected sleeper for deterministic worker tests.
    pub fn from_socket_with_sleeper(
        socket: UdpSocket,
        target: SocketAddr,
        policy: BindPolicy,
        frame_interval_us: u64,
        sleeper: Arc<dyn Sleeper>,
    ) -> Result<Self, NetError> {
        let local = socket.local_addr()?;
        crate::validate_bind_addr(local.ip(), policy)?;
        crate::validate_bind_addr(target.ip(), policy)?;
        SockRef::from(&socket).set_send_buffer_size(SEND_BUFFER_BYTES)?;
        let queue = Arc::new(SenderQueue::new());
        let worker_queue = Arc::clone(&queue);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (force_keyframe_tx, force_keyframes) = mpsc::sync_channel(FORCE_EVENT_CAPACITY);
        let worker_force_tx = force_keyframe_tx.clone();
        let worker = thread::Builder::new()
            .name("racc-video-send".to_owned())
            .spawn(move || {
                let mut pacer = Pacer::new();
                let clock = Instant::now();
                while let Ok(Some(pending)) = worker_queue.dequeue() {
                    let force = ForceKeyframe {
                        epoch: pending.frame.epoch,
                        dropped_frame_id: pending.frame.frame_id,
                    };
                    let result = {
                        let mut context = FrameSendContext {
                            socket: &socket,
                            target,
                            frame_interval_us,
                            pacer: &mut pacer,
                            clock,
                            sleeper: sleeper.as_ref(),
                            cancelled: &worker_cancelled,
                        };
                        send_one_frame(&mut context, pending.frame)
                    };
                    if matches!(result, Err(NetError::Closed)) {
                        let _ = worker_force_tx.try_send(force);
                    }
                    let _ = pending.result.send(result);
                }
            })?;
        Ok(Self {
            queue,
            force_keyframes,
            force_keyframe_tx,
            cancelled,
            worker: Some(worker),
        })
    }

    /// Enqueues and waits for one complete frame transmission.
    pub fn send_frame(&self, frame: SenderFrame) -> Result<VideoSendMetrics, NetError> {
        fragment_count(frame.bytes.len())?;
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let pending = PendingSend {
            frame,
            result: result_tx,
        };
        let force = self.queue.enqueue(pending)?;
        if let Some(signal) = force {
            let _ = self.force_keyframe_tx.try_send(signal);
        }
        result_rx.recv().map_err(|_| NetError::QueueClosed)?
    }

    /// Returns the next encoder keyframe signal caused by queue backpressure.
    pub fn try_force_keyframe(&self) -> Option<ForceKeyframe> {
        self.force_keyframes.try_recv().ok()
    }

    /// Stops the worker after queued frames drain and joins it.
    pub fn close(&mut self) -> Result<(), NetError> {
        self.cancelled.store(true, Ordering::Release);
        self.queue.close()?;
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| NetError::ThreadPanicked)?;
        }
        Ok(())
    }
}

impl Drop for VideoSender {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// UDP receiver that filters source IPs, reassembles on one thread, and emits
/// through a bounded event channel.
pub struct VideoReceiver {
    events: Receiver<VideoTransportEvent>,
    epoch_commands: SyncSender<u16>,
    stop: Arc<AtomicBool>,
    unexpected_peer_datagrams: Arc<AtomicU64>,
    event_channel_drops: Arc<AtomicU64>,
    local_addr: SocketAddr,
    reassembly_stats: Arc<Mutex<crate::ReassemblyStats>>,
    worker: Option<JoinHandle<()>>,
}

impl VideoReceiver {
    /// Binds a video listener after applying the selected production or test policy.
    pub fn bind(
        address: SocketAddr,
        expected_host_ip: IpAddr,
        epoch: u16,
        policy: BindPolicy,
    ) -> Result<Self, NetError> {
        crate::validate_bind_addr(address.ip(), policy)?;
        crate::validate_bind_addr(expected_host_ip, policy)?;
        let socket = UdpSocket::bind(address)?;
        SockRef::from(&socket).set_recv_buffer_size(RECEIVE_BUFFER_BYTES)?;
        let local_addr = socket.local_addr()?;
        socket.set_read_timeout(Some(Duration::from_millis(RECEIVE_POLL_INTERVAL_MS)))?;
        let (event_tx, events) = mpsc::sync_channel(RECEIVER_EVENT_CAPACITY);
        let (epoch_tx, epoch_commands) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let unexpected_peer_datagrams = Arc::new(AtomicU64::new(0));
        let event_channel_drops = Arc::new(AtomicU64::new(0));
        let worker_stop = Arc::clone(&stop);
        let unexpected_counter = Arc::clone(&unexpected_peer_datagrams);
        let drop_counter = Arc::clone(&event_channel_drops);
        let reassembly_stats = Arc::new(Mutex::new(crate::ReassemblyStats::default()));
        let worker_stats = Arc::clone(&reassembly_stats);
        let worker = thread::Builder::new()
            .name("racc-video-recv".to_owned())
            .spawn(move || {
                receive_loop(
                    socket,
                    ReceiverContext {
                        expected_host_ip,
                        epoch,
                        epoch_commands,
                        events: event_tx,
                        stop: worker_stop,
                        unexpected_peer_datagrams: unexpected_counter,
                        event_channel_drops: drop_counter,
                        shared_stats: worker_stats,
                    },
                );
            })?;
        Ok(Self {
            events,
            epoch_commands: epoch_tx,
            stop,
            unexpected_peer_datagrams,
            event_channel_drops,
            local_addr,
            reassembly_stats,
            worker: Some(worker),
        })
    }

    /// Returns the bound local address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.local_addr)
    }

    /// Blocks until an event is available or the supplied timeout elapses.
    pub fn recv_event(&self, timeout: Duration) -> Result<VideoTransportEvent, NetError> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(event),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(NetError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(NetError::Closed),
        }
    }

    /// Requests that the receiver drop current fragments and use the given epoch.
    pub fn reset_epoch(&self, epoch: u16) -> Result<(), NetError> {
        self.epoch_commands
            .try_send(epoch)
            .map_err(|_| NetError::QueueClosed)
    }

    /// Returns the source filter and bounded event-channel counters.
    pub fn counters(&self) -> VideoReceiverCounters {
        VideoReceiverCounters {
            unexpected_peer_datagrams: self.unexpected_peer_datagrams.load(Ordering::Relaxed),
            event_channel_drops: self.event_channel_drops.load(Ordering::Relaxed),
        }
    }

    /// Returns the worker's latest bounded reassembly counters and loss estimate.
    pub fn reassembly_stats(&self) -> Result<crate::ReassemblyStats, NetError> {
        self.reassembly_stats
            .lock()
            .map(|stats| *stats)
            .map_err(|_| NetError::Poisoned)
    }

    /// Stops polling and joins the receiver worker.
    pub fn close(&mut self) -> Result<(), NetError> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| NetError::ThreadPanicked)?;
        }
        Ok(())
    }
}

impl Drop for VideoReceiver {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

struct FrameSendContext<'a> {
    socket: &'a UdpSocket,
    target: SocketAddr,
    frame_interval_us: u64,
    pacer: &'a mut Pacer,
    clock: Instant,
    sleeper: &'a dyn Sleeper,
    cancelled: &'a AtomicBool,
}

fn send_one_frame(
    context: &mut FrameSendContext<'_>,
    frame: SenderFrame,
) -> Result<VideoSendMetrics, NetError> {
    let count = fragment_count(frame.bytes.len())?;
    let mut datagrams = Vec::new();
    datagrams
        .try_reserve_exact(count)
        .map_err(|_| NetError::AllocationFailed)?;
    for _ in 0..count {
        let mut datagram = Vec::new();
        datagram
            .try_reserve_exact(racc_proto::MAX_DATAGRAM)
            .map_err(|_| NetError::AllocationFailed)?;
        datagrams.push(datagram);
    }
    slice_frame_into(&frame, &mut datagrams)?;
    let now_us = elapsed_us(context.clock);
    let schedule = context.pacer.schedule(
        now_us,
        count,
        context.frame_interval_us,
        PACING_FRACTION_PERCENT,
    );
    let mut metrics = VideoSendMetrics {
        frame_id: frame.frame_id,
        datagrams_sent: 0,
        bytes_sent: 0,
        send_duration_us: 0,
        max_inter_datagram_gap_us: 0,
        observed_sleep_granularity_us: 0,
    };
    let mut first_send: Option<Instant> = None;
    let mut prior_send: Option<Instant> = None;
    for (index, datagram) in datagrams.iter().enumerate() {
        if context.cancelled.load(Ordering::Acquire) {
            return Err(NetError::Closed);
        }
        let Some(target_us) = schedule.send_at_us.get(index).copied() else {
            return Err(NetError::BufferCount);
        };
        let before = elapsed_us(context.clock);
        if target_us > before {
            let sleep_start = Instant::now();
            context
                .sleeper
                .sleep(Duration::from_micros(target_us - before));
            let actual_sleep = u64::try_from(sleep_start.elapsed().as_micros()).unwrap_or(u64::MAX);
            metrics.observed_sleep_granularity_us =
                metrics.observed_sleep_granularity_us.max(actual_sleep);
        }
        if context.cancelled.load(Ordering::Acquire) {
            return Err(NetError::Closed);
        }
        let sent_at = Instant::now();
        let bytes_sent = context.socket.send_to(datagram, context.target)?;
        if bytes_sent != datagram.len() {
            return Err(NetError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "partial UDP datagram write",
            )));
        }
        if let Some(previous) = prior_send {
            let gap =
                u64::try_from(sent_at.duration_since(previous).as_micros()).unwrap_or(u64::MAX);
            metrics.max_inter_datagram_gap_us = metrics.max_inter_datagram_gap_us.max(gap);
        }
        prior_send = Some(sent_at);
        first_send.get_or_insert(sent_at);
        metrics.datagrams_sent = metrics.datagrams_sent.saturating_add(1);
        metrics.bytes_sent = metrics.bytes_sent.saturating_add(bytes_sent);
    }
    if let (Some(first), Some(last)) = (first_send, prior_send) {
        metrics.send_duration_us =
            u64::try_from(last.duration_since(first).as_micros()).unwrap_or(u64::MAX);
    }
    Ok(metrics)
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

struct ReceiverContext {
    expected_host_ip: IpAddr,
    epoch: u16,
    epoch_commands: Receiver<u16>,
    events: SyncSender<VideoTransportEvent>,
    stop: Arc<AtomicBool>,
    unexpected_peer_datagrams: Arc<AtomicU64>,
    event_channel_drops: Arc<AtomicU64>,
    shared_stats: Arc<Mutex<crate::ReassemblyStats>>,
}

fn receive_loop(socket: UdpSocket, context: ReceiverContext) {
    let ReceiverContext {
        expected_host_ip,
        epoch,
        epoch_commands,
        events,
        stop,
        unexpected_peer_datagrams,
        event_channel_drops,
        shared_stats,
    } = context;
    let clock = Instant::now();
    let mut reassembler = Reassembler::new(epoch);
    let mut buffer = [0u8; RECEIVE_BUFFER_LEN];
    while !stop.load(Ordering::Acquire) {
        while let Ok(new_epoch) = epoch_commands.try_recv() {
            let reset_events = reassembler.reset(new_epoch, elapsed_us(clock));
            emit_events(
                reset_events,
                &events,
                &mut reassembler,
                elapsed_us(clock),
                &event_channel_drops,
            );
        }
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                if source.ip() != expected_host_ip {
                    unexpected_peer_datagrams.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let Some(datagram) = buffer.get(..length) else {
                    continue;
                };
                let parsed_events = reassembler.push_datagram(datagram, elapsed_us(clock));
                emit_events(
                    parsed_events,
                    &events,
                    &mut reassembler,
                    elapsed_us(clock),
                    &event_channel_drops,
                );
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => break,
        }
        let poll_now = elapsed_us(clock);
        let poll_events = reassembler.poll(poll_now);
        emit_events(
            poll_events,
            &events,
            &mut reassembler,
            poll_now,
            &event_channel_drops,
        );
        if let Ok(mut snapshot) = shared_stats.lock() {
            *snapshot = reassembler.stats(poll_now);
        }
    }
}

fn emit_events(
    source: Vec<ReassemblyEvent>,
    events: &SyncSender<VideoTransportEvent>,
    reassembler: &mut Reassembler,
    now_us: u64,
    event_channel_drops: &AtomicU64,
) {
    for event in source {
        let mapped = match event {
            ReassemblyEvent::FrameReady(frame) => VideoTransportEvent::FrameReady(frame),
            ReassemblyEvent::NeedKeyframe(epoch) => VideoTransportEvent::NeedKeyframe(epoch),
            ReassemblyEvent::Cursor(cursor) => VideoTransportEvent::Cursor(cursor),
        };
        match events.try_send(mapped) {
            Ok(()) => {}
            Err(TrySendError::Full(VideoTransportEvent::FrameReady(_))) => {
                event_channel_drops.fetch_add(1, Ordering::Relaxed);
                let _ = reassembler.force_keyframe_request(now_us);
            }
            Err(TrySendError::Full(_)) => {
                event_channel_drops.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m2_socket_and_thread_constants_match_the_transport_contract() {
        assert_eq!(DEFAULT_FRAME_INTERVAL_US, 33_333);
        assert_eq!(PACING_FRACTION_PERCENT, 60);
        assert_eq!(SEND_QUEUE_MAX_FRAMES, 2);
        assert_eq!(SEND_BUFFER_BYTES, 1024 * 1024);
        assert_eq!(RECEIVE_BUFFER_BYTES, 2 * 1024 * 1024);
        assert_eq!(RECEIVE_POLL_INTERVAL_MS, 5);
        assert_eq!(RECEIVE_BUFFER_LEN, 2048);
        assert_eq!(RECEIVER_EVENT_CAPACITY, 64);
        assert_eq!(FORCE_EVENT_CAPACITY, 2);
        assert_eq!(crate::THREAD_JOIN_TIMEOUT_MS, 1000);
    }
}
