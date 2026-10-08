use racc_core::{
    HostConnectionId, HostPeerAuthorization, HostRuntime, HostRuntimeError, HostRuntimeEvent,
};
use racc_identity::{
    AccessState, Allowlist, ApprovalState, IdentityError, PeerIdentity, ProcessRunner,
    SystemProcessRunner, TailscaleClient,
};
use racc_net::{BindPolicy, ControlConn, ControlError, ControlListener, ControlSettings};
use racc_proto::{ControlMessage, Hello, HelloStatus};
use racc_session::HostAction;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Maximum accepted control sockets, including connections performing whois.
pub const MAX_CONTROL_CONNECTIONS: usize = 16;
/// Maximum accepted TCP connection attempts from one Tailscale peer in a fixed window.
pub const MAX_CONNECTION_ATTEMPTS_PER_PEER_PER_MINUTE: usize = 20;
/// Maximum distinct peers retained by the attempt limiter.
pub const MAX_TRACKED_ATTEMPT_PEERS: usize = 4096;
const CONNECTION_ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
/// Maximum time a newly accepted peer may wait before sending Hello.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll interval used to make active control handlers observe stop requests.
pub const CONTROL_STOP_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Maximum number of externally queued control messages per active viewer.
pub const MAX_OUTBOUND_CONTROL_MESSAGES: usize = 8;
const OUTBOUND_DRAIN_PER_TICK: usize = 8;

struct QueuedControlMessage {
    message: ControlMessage,
    completion: Option<SyncSender<Result<(), ControlSendError>>>,
}

/// Runtime operations required by the control transport adapter.
///
/// `HostRuntime` implements this directly. The trait also allows deterministic
/// loopback tests without weakening the production runtime's Tailscale checks.
pub trait HostRuntimeAdapter: Send + 'static {
    /// Processes the first Hello after the identity adapter has authorized it.
    fn on_hello(
        &mut self,
        connection_id: HostConnectionId,
        remote_addr: SocketAddr,
        hello: &Hello,
        authorization: HostPeerAuthorization,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError>;

    /// Processes a message received after a successful handshake.
    fn on_control(
        &mut self,
        connection_id: HostConnectionId,
        remote_addr: SocketAddr,
        message: ControlMessage,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError>;

    /// Notifies the runtime that the authorized control socket closed.
    fn on_disconnect(
        &mut self,
        connection_id: HostConnectionId,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError>;
}

impl HostRuntimeAdapter for HostRuntime {
    fn on_hello(
        &mut self,
        connection_id: HostConnectionId,
        remote_addr: SocketAddr,
        hello: &Hello,
        authorization: HostPeerAuthorization,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        HostRuntime::on_hello(self, connection_id, remote_addr, hello, authorization)
    }

    fn on_control(
        &mut self,
        connection_id: HostConnectionId,
        remote_addr: SocketAddr,
        message: ControlMessage,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        HostRuntime::on_control(self, connection_id, remote_addr, message, now_us)
    }

    fn on_disconnect(
        &mut self,
        connection_id: HostConnectionId,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        HostRuntime::on_disconnect(self, connection_id, now_us)
    }
}

/// Performs a whois lookup and checks the host's persistent allowlist.
pub trait PeerAuthorizer: Send + 'static {
    /// Resolves one remote Tailscale IP and returns a fail-closed decision.
    fn authorize(
        &mut self,
        remote_ip: IpAddr,
        now_unix_secs: u64,
    ) -> Result<HostPeerAuthorization, IdentityError>;

    /// Persists approval for a pending peer when the adapter supports owner decisions.
    fn approve_pending(
        &mut self,
        _peer_key: &str,
        _now_unix_secs: u64,
    ) -> Result<(), IdentityError> {
        Err(IdentityError::Allowlist(
            "peer approval is unavailable in this adapter".to_owned(),
        ))
    }
}

/// Production whois plus persisted-allowlist adapter.
pub struct TailscaleAllowlistAuthorizer<R: ProcessRunner = SystemProcessRunner> {
    client: TailscaleClient<R>,
    allowlist: Allowlist,
}

impl TailscaleAllowlistAuthorizer<SystemProcessRunner> {
    /// Opens the per-user allowlist and discovers the system Tailscale CLI.
    pub fn open_config_root(config_root: &Path) -> Result<Self, IdentityError> {
        Ok(Self {
            client: TailscaleClient::system(),
            allowlist: Allowlist::open_in_config_root(config_root)?,
        })
    }
}

impl TailscaleAllowlistAuthorizer<SystemProcessRunner> {
    #[cfg(test)]
    pub(crate) fn from_allowlist_for_test(allowlist: Allowlist) -> Self {
        Self {
            client: TailscaleClient::system(),
            allowlist,
        }
    }

    #[cfg(test)]
    pub(crate) fn seed_peer_for_test(
        &mut self,
        identity: &PeerIdentity,
        approved: bool,
        now_unix_secs: u64,
    ) -> Result<(), IdentityError> {
        if approved {
            self.allowlist.approve(
                identity,
                identity.node_name.as_deref().unwrap_or("Test peer"),
                now_unix_secs,
            )
        } else {
            self.allowlist.check(identity, now_unix_secs).map(|_| ())
        }
    }
}

impl<R: ProcessRunner + 'static> TailscaleAllowlistAuthorizer<R> {
    /// Persists approval for a currently pending Tailscale node ID.
    pub fn approve_pending(
        &mut self,
        peer_key: &str,
        now_unix_secs: u64,
    ) -> Result<(), IdentityError> {
        let entry = self
            .allowlist
            .pending()
            .find(|entry| entry.node_id == peer_key)
            .cloned()
            .ok_or_else(|| IdentityError::Allowlist("peer is not pending approval".to_owned()))?;
        let identity = PeerIdentity {
            node_id: entry.node_id,
            owner_login: entry.owner_login,
            tags: Vec::new(),
            node_name: Some(entry.label.clone()),
            addresses: Vec::new(),
        };
        self.allowlist
            .approve(&identity, &entry.label, now_unix_secs)
    }

    /// Returns only approved peers in the stable local IPC shape.
    pub fn approved_peers(&self) -> Vec<racc_core::AllowlistEntry> {
        self.allowlist
            .entries()
            .iter()
            .filter(|entry| entry.state == ApprovalState::Approved)
            .map(|entry| racc_core::AllowlistEntry {
                node_id: entry.node_id.clone(),
                display_name: entry.label.clone(),
                login_name: entry.owner_login.clone().unwrap_or_default(),
            })
            .collect()
    }

    /// Returns pending peers in allowlist order for snapshots and IPC events.
    pub fn pending_peers(&self) -> Vec<racc_core::PendingPeer> {
        self.allowlist
            .pending()
            .map(|entry| racc_core::PendingPeer {
                node_id: entry.node_id.clone(),
                display_name: entry.label.clone(),
                login_name: entry.owner_login.clone().unwrap_or_default(),
            })
            .collect()
    }

    /// Persists rejection of a peer that is currently awaiting approval.
    pub fn reject_pending(
        &mut self,
        peer_key: &str,
        now_unix_secs: u64,
    ) -> Result<(), IdentityError> {
        let entry = self
            .allowlist
            .pending()
            .find(|entry| entry.node_id == peer_key)
            .cloned()
            .ok_or_else(|| IdentityError::Allowlist("peer is not pending approval".to_owned()))?;
        let identity = identity_from_allowlist(&entry);
        self.allowlist
            .reject(&identity, &entry.label, now_unix_secs)
    }

    /// Removes a peer only if it is currently approved.
    pub fn remove_approved(&mut self, peer_key: &str) -> Result<(), IdentityError> {
        let entry = self
            .allowlist
            .entries()
            .iter()
            .find(|entry| entry.node_id == peer_key && entry.state == ApprovalState::Approved)
            .cloned()
            .ok_or_else(|| IdentityError::Allowlist("peer is not approved".to_owned()))?;
        let identity = identity_from_allowlist(&entry);
        if self.allowlist.remove(&identity)? {
            Ok(())
        } else {
            Err(IdentityError::Allowlist("peer is not approved".to_owned()))
        }
    }
}

fn identity_from_allowlist(entry: &racc_identity::AllowlistEntry) -> PeerIdentity {
    PeerIdentity {
        node_id: entry.node_id.clone(),
        owner_login: entry.owner_login.clone(),
        tags: Vec::new(),
        node_name: Some(entry.label.clone()),
        addresses: Vec::new(),
    }
}

impl<R: ProcessRunner + 'static> PeerAuthorizer for TailscaleAllowlistAuthorizer<R> {
    fn authorize(
        &mut self,
        remote_ip: IpAddr,
        now_unix_secs: u64,
    ) -> Result<HostPeerAuthorization, IdentityError> {
        let identity = self.client.resolve(remote_ip)?;
        let access = self.allowlist.check(&identity, now_unix_secs)?;
        Ok(match access {
            AccessState::Approved => HostPeerAuthorization::Approved {
                peer_key: identity.node_id,
            },
            AccessState::Pending => HostPeerAuthorization::Pending {
                peer_key: identity.node_id,
                label: identity
                    .node_name
                    .unwrap_or_else(|| "Unknown device".to_owned()),
            },
            AccessState::Rejected | AccessState::Unknown => HostPeerAuthorization::Rejected,
        })
    }

    fn approve_pending(&mut self, peer_key: &str, now_unix_secs: u64) -> Result<(), IdentityError> {
        TailscaleAllowlistAuthorizer::approve_pending(self, peer_key, now_unix_secs)
    }
}

/// Authenticated control-plane item handed to platform or session workers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostAdapterEvent {
    /// A viewer completed the authorized Hello handshake and supplied its UDP listener port.
    ViewerAccepted {
        /// Adapter-assigned connection identifier.
        connection_id: HostConnectionId,
        /// Remote Tailscale socket address used for both channels.
        remote_addr: SocketAddr,
        /// Viewer UDP port for video datagrams.
        video_udp_port: u16,
    },
    /// One runtime action or status event generated by an authenticated peer.
    Runtime {
        /// Adapter-assigned connection identifier.
        connection_id: HostConnectionId,
        /// Remote socket address.
        remote_addr: SocketAddr,
        /// Runtime event; session actions remain typed and are not falsely executed here.
        event: HostRuntimeEvent,
    },
    /// An authenticated message awaiting an input or clipboard worker.
    AuthenticatedMessage {
        /// Adapter-assigned connection identifier.
        connection_id: HostConnectionId,
        /// Remote socket address.
        remote_addr: SocketAddr,
        /// Input and text clipboard messages are surfaced without claiming to inject or sync.
        message: ControlMessage,
    },
    /// Whois or allowlist lookup failed; the peer remains unauthorized.
    IdentityCheckFailed {
        /// Remote socket address.
        remote_addr: SocketAddr,
        /// Typed local identity error. No error detail is sent to the peer.
        error: IdentityError,
    },
    /// Listener accept or worker creation failed.
    ServerError {
        /// Bounded diagnostic text intended for local logs or status UI.
        message: String,
    },
}

type EventCallback = Arc<dyn Fn(HostAdapterEvent) + Send + Sync + 'static>;

struct Shared<R, A> {
    stopping: Arc<AtomicBool>,
    active_connections: AtomicUsize,
    next_connection_id: AtomicU64,
    runtime: Arc<Mutex<R>>,
    authorizer: Arc<Mutex<A>>,
    callback: EventCallback,
    outbound: Arc<Mutex<HashMap<HostConnectionId, SyncSender<QueuedControlMessage>>>>,
    started_at: Instant,
    bind_policy: BindPolicy,
    attempt_limiter: Mutex<ConnectionAttemptLimiter>,
}

/// Bounded TCP control server for the host runtime.
///
/// Production `bind` accepts only Tailscale addresses. It checks whois and the
/// allowlist before calling `HostRuntime::on_hello`; all later runtime actions
/// are delivered as typed events for the absent capture/encode workers.
pub struct HostControlServer<R: HostRuntimeAdapter, A: PeerAuthorizer> {
    local_addr: SocketAddr,
    stopping: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
    outbound: Arc<Mutex<HashMap<HostConnectionId, SyncSender<QueuedControlMessage>>>>,
    authorizer: Arc<Mutex<A>>,
    _runtime: std::marker::PhantomData<fn() -> (R, A)>,
}

impl<R: HostRuntimeAdapter, A: PeerAuthorizer> HostControlServer<R, A> {
    /// Binds a production listener to a Tailscale address only.
    pub fn bind(
        address: SocketAddr,
        settings: ControlSettings,
        runtime: Arc<Mutex<R>>,
        authorizer: Arc<Mutex<A>>,
        callback: impl Fn(HostAdapterEvent) + Send + Sync + 'static,
    ) -> Result<Self, HostControlServerError> {
        Self::bind_with_policy(
            address,
            settings,
            runtime,
            authorizer,
            Arc::new(callback),
            BindPolicy::Tailscale,
        )
    }

    /// Returns the actual bound address, including an assigned port.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Returns a bounded sender for control messages generated by capture,
    /// encoder, telemetry, or quality workers after the handshake.
    pub fn control_sender(&self) -> ControlSendHandle {
        ControlSendHandle {
            outbound: Arc::clone(&self.outbound),
        }
    }

    /// Persists an owner approval for a peer that previously appeared as pending.
    pub fn approve_peer(&self, peer_key: &str, now_unix_secs: u64) -> Result<(), IdentityError> {
        lock(&self.authorizer).approve_pending(peer_key, now_unix_secs)
    }

    /// Stops accepting connections and asks active handlers to close.
    ///
    /// Active handlers poll the stop flag at a bounded socket-read interval.
    /// A whois subprocess already in progress may finish before its handler exits.
    pub fn stop(&mut self) -> Result<(), HostControlServerError> {
        self.stopping.store(true, Ordering::Release);
        lock(&self.outbound).clear();
        let Some(accept_thread) = self.accept_thread.take() else {
            return Ok(());
        };
        if !accept_thread.is_finished() {
            let wake = TcpStream::connect_timeout(&self.local_addr, Duration::from_millis(500));
            if wake.is_err() && !accept_thread.is_finished() {
                // Retain the handle so shutdown can be retried if the OS refuses this wake-up.
                self.accept_thread = Some(accept_thread);
                return Err(HostControlServerError::StopWakeFailed);
            }
        }
        accept_thread
            .join()
            .map_err(|_| HostControlServerError::ThreadPanicked)
    }

    fn bind_with_policy(
        address: SocketAddr,
        settings: ControlSettings,
        runtime: Arc<Mutex<R>>,
        authorizer: Arc<Mutex<A>>,
        callback: EventCallback,
        policy: BindPolicy,
    ) -> Result<Self, HostControlServerError> {
        let listener = ControlListener::bind(address, policy, bounded_settings(settings))?;
        let local_addr = listener.local_addr()?;
        let stopping = Arc::new(AtomicBool::new(false));
        let outbound = Arc::new(Mutex::new(HashMap::new()));
        let shared = Arc::new(Shared {
            stopping: Arc::clone(&stopping),
            active_connections: AtomicUsize::new(0),
            next_connection_id: AtomicU64::new(1),
            runtime,
            authorizer: Arc::clone(&authorizer),
            callback,
            outbound: Arc::clone(&outbound),
            started_at: Instant::now(),
            bind_policy: policy,
            attempt_limiter: Mutex::new(ConnectionAttemptLimiter::default()),
        });
        let accept_thread = thread::Builder::new()
            .name("racc-control-accept".to_owned())
            .spawn(move || accept_loop(listener, shared))?;
        Ok(Self {
            local_addr,
            stopping,
            accept_thread: Some(accept_thread),
            outbound,
            authorizer,
            _runtime: std::marker::PhantomData,
        })
    }
}

/// Cloneable handle for sending bounded control messages to an authenticated
/// viewer from a worker other than the TCP read loop.
#[derive(Clone)]
pub struct ControlSendHandle {
    outbound: Arc<Mutex<HashMap<HostConnectionId, SyncSender<QueuedControlMessage>>>>,
}

impl ControlSendHandle {
    /// Queues a control message for an active authorized viewer.
    ///
    /// Success means the bounded queue accepted it; concurrent disconnect or shutdown can still discard it.
    pub fn send(
        &self,
        connection_id: HostConnectionId,
        message: ControlMessage,
    ) -> Result<(), ControlSendError> {
        let sender = lock(&self.outbound)
            .get(&connection_id)
            .cloned()
            .ok_or(ControlSendError::NoSuchConnection)?;
        sender
            .try_send(QueuedControlMessage {
                message,
                completion: None,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => ControlSendError::QueueFull,
                TrySendError::Disconnected(_) => ControlSendError::ConnectionClosed,
            })
    }

    /// Queues a control message and waits until the TCP handler writes it.
    ///
    /// A timeout has an ambiguous delivery outcome. Callers that gate another transport on this
    /// message must treat any error as a failed send and must not start that transport.
    pub fn send_confirmed(
        &self,
        connection_id: HostConnectionId,
        message: ControlMessage,
        timeout: Duration,
    ) -> Result<(), ControlSendError> {
        let sender = lock(&self.outbound)
            .get(&connection_id)
            .cloned()
            .ok_or(ControlSendError::NoSuchConnection)?;
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        sender
            .try_send(QueuedControlMessage {
                message,
                completion: Some(completion_tx),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => ControlSendError::QueueFull,
                TrySendError::Disconnected(_) => ControlSendError::ConnectionClosed,
            })?;
        match completion_rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ControlSendError::DeliveryTimeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ControlSendError::ConnectionClosed),
        }
    }
}

/// Why an asynchronous outbound control message could not be queued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlSendError {
    /// No authenticated control connection has this id.
    NoSuchConnection,
    /// The fixed outbound queue is full; the producer must apply backpressure.
    QueueFull,
    /// The TCP handler has already closed its receive side or is closing concurrently.
    ConnectionClosed,
    /// The TCP handler did not report the write before the caller's deadline.
    DeliveryTimeout,
}

impl std::fmt::Display for ControlSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoSuchConnection => "no active control connection",
            Self::QueueFull => "outbound control queue is full",
            Self::ConnectionClosed => "control connection is closed",
            Self::DeliveryTimeout => "control message write timed out",
        })
    }
}

impl std::error::Error for ControlSendError {}

impl<R: HostRuntimeAdapter, A: PeerAuthorizer> Drop for HostControlServer<R, A> {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
impl<R: HostRuntimeAdapter, A: PeerAuthorizer> HostControlServer<R, A> {
    fn bind_loopback_for_test(
        address: SocketAddr,
        settings: ControlSettings,
        runtime: Arc<Mutex<R>>,
        authorizer: Arc<Mutex<A>>,
        callback: impl Fn(HostAdapterEvent) + Send + Sync + 'static,
    ) -> Result<Self, HostControlServerError> {
        Self::bind_with_policy(
            address,
            settings,
            runtime,
            authorizer,
            Arc::new(callback),
            BindPolicy::TestOnlyLoopback,
        )
    }
}

/// Typed failure from server setup or shutdown.
#[derive(Debug)]
pub enum HostControlServerError {
    /// The control listener or TCP connection failed.
    Control(ControlError),
    /// The accept thread could not be started.
    ThreadStart(std::io::Error),
    /// The local wake-up connection failed during shutdown.
    StopWakeFailed,
    /// The accept thread panicked.
    ThreadPanicked,
}

impl std::fmt::Display for HostControlServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Control(error) => write!(formatter, "control server failed: {error}"),
            Self::ThreadStart(error) => write!(formatter, "control accept thread failed: {error}"),
            Self::StopWakeFailed => {
                formatter.write_str("could not wake the control listener to stop")
            }
            Self::ThreadPanicked => formatter.write_str("control accept thread panicked"),
        }
    }
}

impl std::error::Error for HostControlServerError {}

impl From<ControlError> for HostControlServerError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

impl From<std::io::Error> for HostControlServerError {
    fn from(error: std::io::Error) -> Self {
        Self::ThreadStart(error)
    }
}

fn bounded_settings(mut settings: ControlSettings) -> ControlSettings {
    settings.read_timeout = Some(
        settings
            .read_timeout
            .filter(|timeout| !timeout.is_zero())
            .unwrap_or(CONTROL_STOP_POLL_INTERVAL)
            .min(CONTROL_STOP_POLL_INTERVAL),
    );
    settings
}

fn accept_loop<R: HostRuntimeAdapter, A: PeerAuthorizer>(
    listener: ControlListener,
    shared: Arc<Shared<R, A>>,
) {
    let mut connection_workers = Vec::with_capacity(MAX_CONTROL_CONNECTIONS);
    while !shared.stopping.load(Ordering::Acquire) {
        reap_connection_workers(&mut connection_workers, &shared.callback);
        let (connection, remote_addr) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) => {
                if !shared.stopping.load(Ordering::Acquire) {
                    emit(
                        &shared.callback,
                        HostAdapterEvent::ServerError {
                            message: bounded_error(&error.to_string()),
                        },
                    );
                }
                break;
            }
        };
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        if racc_net::validate_bind_addr(remote_addr.ip(), shared.bind_policy).is_err()
            || !lock(&shared.attempt_limiter).allow(remote_addr.ip(), Instant::now())
        {
            // Reject outside the Tailscale bind policy and excess peer attempts before
            // allocating a handler thread or performing an identity lookup.
            drop(connection);
            continue;
        }
        let reserved = shared.active_connections.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| (current < MAX_CONTROL_CONNECTIONS).then_some(current + 1),
        );
        if reserved.is_err() {
            // The socket is dropped immediately when the bounded worker limit is reached.
            continue;
        }
        let worker_shared = Arc::clone(&shared);
        let next = shared.next_connection_id.fetch_add(1, Ordering::Relaxed);
        let connection_id = next.max(1);
        let spawned = thread::Builder::new()
            .name(format!("racc-control-{connection_id}"))
            .spawn(move || {
                let _active = ActiveConnectionGuard(&worker_shared.active_connections);
                let connection_shared = Arc::clone(&worker_shared);
                handle_connection(connection, remote_addr, connection_id, connection_shared);
            });
        match spawned {
            Ok(worker) => connection_workers.push(worker),
            Err(error) => {
                shared.active_connections.fetch_sub(1, Ordering::AcqRel);
                emit(
                    &shared.callback,
                    HostAdapterEvent::ServerError {
                        message: bounded_error(&error.to_string()),
                    },
                );
            }
        }
    }

    // Each handler observes `stopping` on the bounded control read interval and
    // performs its disconnect callback before returning. Retaining and joining
    // every worker makes `HostControlServer::stop` a true drain boundary: callers
    // may safely release or replace the runtime only after it returns.
    for worker in connection_workers {
        if worker.join().is_err() {
            emit(
                &shared.callback,
                HostAdapterEvent::ServerError {
                    message: "control connection worker panicked during shutdown".to_owned(),
                },
            );
        }
    }
}

fn reap_connection_workers(workers: &mut Vec<JoinHandle<()>>, callback: &EventCallback) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            if worker.join().is_err() {
                emit(
                    callback,
                    HostAdapterEvent::ServerError {
                        message: "control connection worker panicked".to_owned(),
                    },
                );
            }
        } else {
            index += 1;
        }
    }
}

#[derive(Default)]
struct ConnectionAttemptLimiter {
    windows: HashMap<IpAddr, AttemptWindow>,
}

#[derive(Clone, Copy)]
struct AttemptWindow {
    started_at: Instant,
    attempts: usize,
}

impl ConnectionAttemptLimiter {
    fn allow(&mut self, peer: IpAddr, now: Instant) -> bool {
        self.windows.retain(|_, window| {
            now.checked_duration_since(window.started_at)
                .is_none_or(|elapsed| elapsed < CONNECTION_ATTEMPT_WINDOW)
        });
        if let Some(window) = self.windows.get_mut(&peer) {
            if window.attempts >= MAX_CONNECTION_ATTEMPTS_PER_PEER_PER_MINUTE {
                return false;
            }
            window.attempts += 1;
            return true;
        }
        if self.windows.len() >= MAX_TRACKED_ATTEMPT_PEERS {
            return false;
        }
        self.windows.insert(
            peer,
            AttemptWindow {
                started_at: now,
                attempts: 1,
            },
        );
        true
    }
}

struct ActiveConnectionGuard<'a>(&'a AtomicUsize);

impl Drop for ActiveConnectionGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle_connection<R: HostRuntimeAdapter, A: PeerAuthorizer>(
    mut connection: ControlConn,
    remote_addr: SocketAddr,
    connection_id: HostConnectionId,
    shared: Arc<Shared<R, A>>,
) {
    if racc_net::validate_bind_addr(remote_addr.ip(), shared.bind_policy).is_err() {
        return;
    }
    let hello = match receive_hello(&mut connection, &shared.stopping) {
        Some(hello) => hello,
        None => return,
    };
    let video_udp_port = hello.video_udp_port;
    let authorization = match lock(&shared.authorizer).authorize(remote_addr.ip(), unix_seconds()) {
        Ok(authorization) => authorization,
        Err(error) => {
            emit(
                &shared.callback,
                HostAdapterEvent::IdentityCheckFailed { remote_addr, error },
            );
            HostPeerAuthorization::Rejected
        }
    };
    let hello_events = {
        let mut runtime = lock(&shared.runtime);
        runtime.on_hello(connection_id, remote_addr, &hello, authorization)
    };
    let Ok(hello_events) = hello_events else {
        return;
    };
    let accepted = hello_events.iter().any(|event| {
        matches!(
            event,
            HostRuntimeEvent::SendControl {
                connection_id: target,
                message: ControlMessage::HelloAck(ack),
            } if *target == connection_id && ack.status == HelloStatus::Ok
        ) || matches!(
            event,
            HostRuntimeEvent::SessionAction {
                connection_id: Some(target),
                action: HostAction::SendControl(ControlMessage::HelloAck(ack)),
            } if *target == connection_id && ack.status == HelloStatus::Ok
        )
    });
    if !accepted {
        let _ = dispatch_runtime_events(
            &mut connection,
            remote_addr,
            connection_id,
            hello_events,
            &shared.callback,
        );
        return;
    }
    let (outbound_sender, outbound_receiver) = mpsc::sync_channel(MAX_OUTBOUND_CONTROL_MESSAGES);
    {
        let mut outbound = lock(&shared.outbound);
        if shared.stopping.load(Ordering::Acquire) {
            drop(outbound);
            drop(outbound_receiver);
            let _ = dispatch_runtime_events(
                &mut connection,
                remote_addr,
                connection_id,
                vec![HostRuntimeEvent::CloseConnection(connection_id)],
                &shared.callback,
            );
            notify_disconnect(&mut connection, remote_addr, connection_id, &shared);
            return;
        }
        outbound.insert(connection_id, outbound_sender);
    }
    let outbound_registration = OutboundRegistration {
        connection_id,
        outbound: Arc::clone(&shared.outbound),
    };

    if dispatch_runtime_events(
        &mut connection,
        remote_addr,
        connection_id,
        hello_events,
        &shared.callback,
    ) {
        drop(outbound_receiver);
        drop(outbound_registration);
        notify_disconnect(&mut connection, remote_addr, connection_id, &shared);
        return;
    }

    emit(
        &shared.callback,
        HostAdapterEvent::ViewerAccepted {
            connection_id,
            remote_addr,
            video_udp_port,
        },
    );

    while !shared.stopping.load(Ordering::Acquire) {
        if flush_outbound(&mut connection, &outbound_receiver) {
            break;
        }
        let message = match connection.recv() {
            Ok(message) => message,
            Err(ControlError::Timeout) => continue,
            Err(_) => break,
        };
        match message {
            message @ ControlMessage::InputEvent(_)
            | message @ ControlMessage::ClipboardUpdate(_)
            | message @ ControlMessage::ClipboardSyncControl(_) => {
                emit(
                    &shared.callback,
                    HostAdapterEvent::AuthenticatedMessage {
                        connection_id,
                        remote_addr,
                        message,
                    },
                );
            }
            message => {
                let goodbye = matches!(message, ControlMessage::Goodbye(_));
                let now_us = elapsed_us(shared.started_at);
                let events = {
                    let mut runtime = lock(&shared.runtime);
                    runtime.on_control(connection_id, remote_addr, message, now_us)
                };
                let Ok(events) = events else {
                    break;
                };
                if dispatch_runtime_events(
                    &mut connection,
                    remote_addr,
                    connection_id,
                    events,
                    &shared.callback,
                ) || goodbye
                {
                    break;
                }
            }
        }
    }
    drop(outbound_receiver);
    drop(outbound_registration);
    notify_disconnect(&mut connection, remote_addr, connection_id, &shared);
}

struct OutboundRegistration {
    connection_id: HostConnectionId,
    outbound: Arc<Mutex<HashMap<HostConnectionId, SyncSender<QueuedControlMessage>>>>,
}

impl Drop for OutboundRegistration {
    fn drop(&mut self) {
        lock(&self.outbound).remove(&self.connection_id);
    }
}

fn flush_outbound(connection: &mut ControlConn, outbound: &Receiver<QueuedControlMessage>) -> bool {
    for _ in 0..OUTBOUND_DRAIN_PER_TICK {
        match outbound.try_recv() {
            Ok(queued) => {
                let result = connection
                    .send(&queued.message)
                    .map_err(|_| ControlSendError::ConnectionClosed);
                if let Some(completion) = queued.completion {
                    let _ = completion.send(result);
                }
                if result.is_err() {
                    return true;
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => return true,
        }
    }
    false
}

fn notify_disconnect<R: HostRuntimeAdapter, A: PeerAuthorizer>(
    connection: &mut ControlConn,
    remote_addr: SocketAddr,
    connection_id: HostConnectionId,
    shared: &Shared<R, A>,
) {
    let events = {
        let mut runtime = lock(&shared.runtime);
        runtime.on_disconnect(connection_id, elapsed_us(shared.started_at))
    };
    if let Ok(events) = events {
        let _ = dispatch_runtime_events(
            connection,
            remote_addr,
            connection_id,
            events,
            &shared.callback,
        );
    }
}
fn receive_hello(connection: &mut ControlConn, stopping: &AtomicBool) -> Option<Hello> {
    let deadline = Instant::now() + HELLO_TIMEOUT;
    loop {
        if stopping.load(Ordering::Acquire) || Instant::now() >= deadline {
            return None;
        }
        match connection.recv() {
            Ok(ControlMessage::Hello(hello)) => return Some(hello),
            Ok(_) => return None,
            Err(ControlError::Timeout) => {}
            Err(_) => return None,
        }
    }
}

fn dispatch_runtime_events(
    connection: &mut ControlConn,
    remote_addr: SocketAddr,
    connection_id: HostConnectionId,
    events: Vec<HostRuntimeEvent>,
    callback: &EventCallback,
) -> bool {
    let mut close = false;
    for event in events {
        match &event {
            HostRuntimeEvent::SendControl {
                connection_id: target,
                message,
            } if *target == connection_id && connection.send(message).is_err() => close = true,
            HostRuntimeEvent::SendControl {
                connection_id: target,
                ..
            } if *target == connection_id => {}
            HostRuntimeEvent::SessionAction {
                connection_id: Some(target),
                action: HostAction::SendControl(message),
            } if *target == connection_id && connection.send(message).is_err() => close = true,
            HostRuntimeEvent::SessionAction {
                connection_id: Some(target),
                action: HostAction::SendControl(_),
            } if *target == connection_id => {}
            HostRuntimeEvent::CloseConnection(target) if *target == connection_id => close = true,
            _ => {}
        }
        if close {
            break;
        }
        // Runtime callbacks are emitted after any direct control write succeeds. The foreground
        // adapter can then safely start the matching UDP epoch when it receives a StreamReset.
        emit(
            callback,
            HostAdapterEvent::Runtime {
                connection_id,
                remote_addr,
                event,
            },
        );
    }
    close
}

fn emit(callback: &EventCallback, event: HostAdapterEvent) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(event)));
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn elapsed_us(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn bounded_error(message: &str) -> String {
    const MAX_ERROR_BYTES: usize = 256;
    let mut end = message.len().min(MAX_ERROR_BYTES);
    while !message.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    message[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_proto::{ClipboardUpdate, HelloAck, OsType, Pong};
    use std::sync::mpsc;

    struct FakeAuthorizer {
        order: Arc<Mutex<Vec<&'static str>>>,
        decision: HostPeerAuthorization,
    }

    impl PeerAuthorizer for FakeAuthorizer {
        fn authorize(
            &mut self,
            _remote_ip: IpAddr,
            _now_unix_secs: u64,
        ) -> Result<HostPeerAuthorization, IdentityError> {
            lock(&self.order).push("authorize");
            Ok(self.decision.clone())
        }
    }

    struct FakeRuntime {
        order: Arc<Mutex<Vec<&'static str>>>,
        disconnected: bool,
    }

    impl HostRuntimeAdapter for FakeRuntime {
        fn on_hello(
            &mut self,
            connection_id: HostConnectionId,
            _remote_addr: SocketAddr,
            _hello: &Hello,
            authorization: HostPeerAuthorization,
        ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
            lock(&self.order).push("hello");
            let status = if matches!(authorization, HostPeerAuthorization::Approved { .. }) {
                HelloStatus::Ok
            } else {
                HelloStatus::NotAuthorized
            };
            Ok(vec![HostRuntimeEvent::SessionAction {
                connection_id: Some(connection_id),
                action: HostAction::SendControl(ControlMessage::HelloAck(HelloAck {
                    protocol_version: 1,
                    status,
                    device_name: "test-host".to_owned(),
                    os: OsType::Windows,
                    app_version: "test".to_owned(),
                    codecs: 1,
                    max_height: 1080,
                    features: 1,
                    host_cpu_cores: 4,
                })),
            }])
        }

        fn on_control(
            &mut self,
            _connection_id: HostConnectionId,
            _remote_addr: SocketAddr,
            _message: ControlMessage,
            _now_us: u64,
        ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
            Ok(Vec::new())
        }

        fn on_disconnect(
            &mut self,
            _connection_id: HostConnectionId,
            _now_us: u64,
        ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
            self.disconnected = true;
            Ok(Vec::new())
        }
    }

    fn hello() -> Hello {
        Hello {
            protocol_version: 1,
            device_name: "test-viewer".to_owned(),
            os: OsType::Windows,
            app_version: "test".to_owned(),
            video_udp_port: 44000,
            codecs: 1,
            max_height: 720,
            features: 1,
        }
    }

    #[test]
    fn outbound_control_queue_rejects_messages_at_its_fixed_capacity() {
        let (outbound_sender, outbound_receiver) =
            mpsc::sync_channel(MAX_OUTBOUND_CONTROL_MESSAGES);
        let mut outbound = HashMap::new();
        outbound.insert(5, outbound_sender);
        let handle = ControlSendHandle {
            outbound: Arc::new(Mutex::new(outbound)),
        };

        for nonce in 0..MAX_OUTBOUND_CONTROL_MESSAGES as u64 {
            assert!(handle
                .send(
                    5,
                    ControlMessage::Pong(Pong {
                        nonce,
                        echo_ts_us: nonce,
                    }),
                )
                .is_ok());
        }
        assert_eq!(
            handle.send(
                5,
                ControlMessage::Pong(Pong {
                    nonce: 99,
                    echo_ts_us: 99,
                }),
            ),
            Err(ControlSendError::QueueFull)
        );
        assert_eq!(
            outbound_receiver.try_iter().count(),
            MAX_OUTBOUND_CONTROL_MESSAGES
        );
    }

    #[test]
    fn connection_attempts_are_limited_per_peer_and_expire_after_one_minute() {
        let now = Instant::now();
        let peer = IpAddr::from([100, 64, 0, 1]);
        let other_peer = IpAddr::from([100, 64, 0, 2]);
        let mut limiter = ConnectionAttemptLimiter::default();

        for _ in 0..MAX_CONNECTION_ATTEMPTS_PER_PEER_PER_MINUTE {
            assert!(limiter.allow(peer, now));
        }
        assert!(!limiter.allow(peer, now));
        assert!(limiter.allow(other_peer, now));
        assert!(limiter.allow(peer, now + CONNECTION_ATTEMPT_WINDOW));
    }

    #[test]
    fn connection_attempt_limiter_bounds_distinct_peer_state() {
        let now = Instant::now();
        let mut limiter = ConnectionAttemptLimiter::default();
        for index in 0..MAX_TRACKED_ATTEMPT_PEERS {
            let peer = IpAddr::V4(std::net::Ipv4Addr::from(index as u32));
            assert!(limiter.allow(peer, now));
        }
        assert_eq!(limiter.windows.len(), MAX_TRACKED_ATTEMPT_PEERS);
        assert!(!limiter.allow(IpAddr::V4(std::net::Ipv4Addr::from(u32::MAX)), now));
        assert!(limiter.allow(
            IpAddr::V4(std::net::Ipv4Addr::from(u32::MAX)),
            now + CONNECTION_ATTEMPT_WINDOW
        ));
        assert!(limiter.windows.len() <= MAX_TRACKED_ATTEMPT_PEERS);
    }

    #[test]
    fn production_bind_rejects_loopback_even_with_fake_dependencies() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(Mutex::new(FakeRuntime {
            order: Arc::clone(&order),
            disconnected: false,
        }));
        let authorizer = Arc::new(Mutex::new(FakeAuthorizer {
            order,
            decision: HostPeerAuthorization::Approved {
                peer_key: "test-node".to_owned(),
            },
        }));
        let result = HostControlServer::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            ControlSettings::default(),
            runtime,
            authorizer,
            |_| {},
        );
        assert!(matches!(result, Err(HostControlServerError::Control(_))));
    }

    #[test]
    fn whois_allowlist_precedes_hello_and_authenticated_clipboard_is_forwarded() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(Mutex::new(FakeRuntime {
            order: Arc::clone(&order),
            disconnected: false,
        }));
        let authorizer = Arc::new(Mutex::new(FakeAuthorizer {
            order: Arc::clone(&order),
            decision: HostPeerAuthorization::Approved {
                peer_key: "test-node".to_owned(),
            },
        }));
        let (event_tx, event_rx) = mpsc::channel();
        let server = HostControlServer::bind_loopback_for_test(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            ControlSettings::default(),
            Arc::clone(&runtime),
            authorizer,
            move |event| {
                let _ = event_tx.send(event);
            },
        );
        assert!(server.is_ok());
        let mut server = match server {
            Ok(server) => server,
            Err(_) => return,
        };
        let address = server.local_addr();
        let client = racc_net::connect_control(
            address,
            BindPolicy::TestOnlyLoopback,
            ControlSettings {
                read_timeout: Some(Duration::from_secs(2)),
                ..ControlSettings::default()
            },
        );
        assert!(client.is_ok());
        let mut client = match client {
            Ok(client) => client,
            Err(_) => return,
        };
        assert!(client.send(&ControlMessage::Hello(hello())).is_ok());
        assert!(matches!(
            client.recv(),
            Ok(ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Ok,
                ..
            }))
        ));
        assert_eq!(lock(&order).as_slice(), &["authorize", "hello"]);
        let connection_id = event_rx
            .recv_timeout(Duration::from_secs(1))
            .ok()
            .and_then(|event| match event {
                HostAdapterEvent::Runtime {
                    connection_id,
                    event:
                        HostRuntimeEvent::SessionAction {
                            action:
                                HostAction::SendControl(ControlMessage::HelloAck(HelloAck {
                                    status: HelloStatus::Ok,
                                    ..
                                })),
                            ..
                        },
                    ..
                } => Some(connection_id),
                _ => None,
            })
            .expect("accepted handshake event");
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_secs(1)),
            Ok(HostAdapterEvent::ViewerAccepted {
                connection_id: accepted_id,
                video_udp_port: 44000,
                ..
            }) if accepted_id == connection_id
        ));
        let sender = server.control_sender();
        assert_eq!(
            sender.send(
                99,
                ControlMessage::Pong(Pong {
                    nonce: 1,
                    echo_ts_us: 2,
                })
            ),
            Err(ControlSendError::NoSuchConnection)
        );
        assert!(sender
            .send_confirmed(
                connection_id,
                ControlMessage::Pong(Pong {
                    nonce: 9,
                    echo_ts_us: 123,
                }),
                Duration::from_secs(2),
            )
            .is_ok());
        assert!(matches!(
            client.recv(),
            Ok(ControlMessage::Pong(Pong {
                nonce: 9,
                echo_ts_us: 123,
            }))
        ));

        let update = ClipboardUpdate {
            seq: 3,
            origin: racc_proto::ClipboardOrigin::Viewer,
            logical_clock: racc_proto::LogicalClock {
                version: racc_proto::CLIPBOARD_LOGICAL_CLOCK_VERSION,
                counter: 3,
            },
            text: "safe text".to_owned(),
        };
        assert!(client
            .send(&ControlMessage::ClipboardSyncControl(
                racc_proto::ClipboardSyncControl { enabled: true },
            ))
            .is_ok());
        assert!(client
            .send(&ControlMessage::ClipboardUpdate(update.clone()))
            .is_ok());
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut bridge = crate::clipboard_bridge::HostClipboardBridge::new(connection_id);
        let mut fake_host_clipboard = Vec::new();
        let mut enabled = false;
        let mut forwarded = false;
        while Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(HostAdapterEvent::AuthenticatedMessage {
                    connection_id: received_connection,
                    message: ControlMessage::ClipboardSyncControl(control),
                    ..
                }) if received_connection == connection_id => {
                    bridge
                        .set_enabled(control.enabled)
                        .expect("explicit session opt-in");
                    enabled = control.enabled;
                }
                Ok(HostAdapterEvent::AuthenticatedMessage {
                    connection_id: received_connection,
                    message: ControlMessage::ClipboardUpdate(received),
                    ..
                }) if received_connection == connection_id && received == update && enabled => {
                    match bridge.receive_remote(received) {
                        Ok(racc_clipboard::RemoteChangeResult::Apply(applied)) => {
                            fake_host_clipboard = applied.bytes;
                            forwarded = true;
                            break;
                        }
                        _ => break,
                    }
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(forwarded);
        assert_eq!(fake_host_clipboard, b"safe text");
        assert_eq!(
            bridge.local_change(&fake_host_clipboard),
            racc_clipboard::LocalChangeResult::EchoSuppressed
        );
        assert_eq!(
            bridge.local_change(b"host response"),
            racc_clipboard::LocalChangeResult::Queued
        );
        let outbound = bridge
            .next_outbound(0)
            .ok()
            .flatten()
            .expect("host clipboard update is ready");
        assert!(sender.send(connection_id, outbound).is_ok());
        bridge.confirm_outbound_queued();
        assert!(matches!(
            client.recv(),
            Ok(ControlMessage::ClipboardUpdate(racc_proto::ClipboardUpdate {
                origin: racc_proto::ClipboardOrigin::Host,
                text,
                ..
            })) if text == "host response"
        ));

        assert!(server.stop().is_ok());
        assert_eq!(
            sender.send(
                connection_id,
                ControlMessage::Pong(Pong {
                    nonce: 10,
                    echo_ts_us: 124
                })
            ),
            Err(ControlSendError::NoSuchConnection)
        );
        // `stop` joins the accept loop, which in turn joins every active
        // handler after its disconnect callback has run.
        assert!(lock(&runtime).disconnected);
    }
    #[test]
    fn local_allowlist_operations_list_reject_and_remove_only_expected_states() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let config_root = std::env::temp_dir().join(format!(
            "racc-host-agent-allowlist-ops-{}-{unique}",
            std::process::id()
        ));
        let mut authorizer = match TailscaleAllowlistAuthorizer::open_config_root(&config_root) {
            Ok(authorizer) => authorizer,
            Err(error) => panic!("open temporary allowlist: {error}"),
        };
        let approved = PeerIdentity {
            node_id: "node-approved".to_owned(),
            owner_login: Some("owner@example.test".to_owned()),
            tags: Vec::new(),
            node_name: Some("Approved PC".to_owned()),
            addresses: Vec::new(),
        };
        let pending = PeerIdentity {
            node_id: "node-pending".to_owned(),
            owner_login: Some("owner@example.test".to_owned()),
            tags: Vec::new(),
            node_name: Some("New PC".to_owned()),
            addresses: Vec::new(),
        };
        assert!(authorizer
            .allowlist
            .approve(&approved, "Approved PC", 10)
            .is_ok());
        assert_eq!(
            authorizer.allowlist.check(&pending, 11),
            Ok(AccessState::Pending)
        );
        assert_eq!(
            authorizer.approved_peers(),
            vec![racc_core::AllowlistEntry {
                node_id: "node-approved".to_owned(),
                display_name: "Approved PC".to_owned(),
                login_name: "owner@example.test".to_owned(),
            }]
        );
        assert_eq!(authorizer.pending_peers().len(), 1);
        assert!(authorizer.reject_pending("node-pending", 12).is_ok());
        assert!(authorizer.reject_pending("node-pending", 13).is_err());
        assert!(authorizer.remove_approved("node-approved").is_ok());
        assert!(authorizer.remove_approved("node-approved").is_err());
        drop(authorizer);
        assert!(std::fs::remove_dir_all(config_root).is_ok());
    }

    #[test]
    fn explicit_approval_changes_only_a_pending_allowlist_entry() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let config_root = std::env::temp_dir().join(format!(
            "racc-host-agent-allowlist-{}-{unique}",
            std::process::id()
        ));
        let mut authorizer = match TailscaleAllowlistAuthorizer::open_config_root(&config_root) {
            Ok(authorizer) => authorizer,
            Err(error) => panic!("open temporary allowlist: {error}"),
        };
        let identity = PeerIdentity {
            node_id: "test-node-approval".to_owned(),
            owner_login: Some("owner@example.test".to_owned()),
            tags: Vec::new(),
            node_name: Some("test viewer".to_owned()),
            addresses: Vec::new(),
        };
        assert_eq!(
            authorizer.allowlist.check(&identity, 1),
            Ok(AccessState::Pending)
        );
        assert!(authorizer.approve_pending("test-node-approval", 2).is_ok());
        assert_eq!(
            authorizer.allowlist.check(&identity, 3),
            Ok(AccessState::Approved)
        );
        drop(authorizer);
        assert!(std::fs::remove_dir_all(config_root).is_ok());
    }
}
