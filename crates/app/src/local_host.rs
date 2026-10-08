//! Background app client for the current user's host-agent.
//!
//! Requests and the pending-event subscription run on worker threads. The UI
//! receives bounded status and peer metadata snapshots only.

use racc_core::ipc::{
    AllowlistEntry, HostStatus, IpcEvent, IpcFailureCode, IpcRequest, IpcResponse, IpcTransport,
    PendingPeer,
};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 16;

/// Current local host-agent state. Missing status means the agent did not answer
/// this refresh; persisted preferences are never used as runtime state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalHostSnapshot {
    /// Whether the latest full refresh succeeded.
    pub connected: bool,
    /// Host/helper state, present only after a successful refresh.
    pub status: Option<HostStatus>,
    /// Approved peers returned by the host's allowlist.
    pub allowlist: Vec<AllowlistEntry>,
    /// Safe user-facing connection or protocol error, when unavailable.
    pub error: Option<String>,
}

/// A local host-agent action requested by the app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalHostCommand {
    /// Ask the helper to accept or stop accepting new viewers.
    SetHostingEnabled(bool),
    /// Add a pending peer to the Tailscale allowlist.
    Approve(String),
    /// Reject a pending peer.
    Reject(String),
    /// Remove an approved peer from the allowlist.
    Remove(String),
}

impl LocalHostCommand {
    fn request(&self) -> IpcRequest {
        match self {
            Self::SetHostingEnabled(enabled) => IpcRequest::SetHostingEnabled { enabled: *enabled },
            Self::Approve(node_id) => IpcRequest::Approve {
                node_id: node_id.clone(),
            },
            Self::Reject(node_id) => IpcRequest::Reject {
                node_id: node_id.clone(),
            },
            Self::Remove(node_id) => IpcRequest::Remove {
                node_id: node_id.clone(),
            },
        }
    }

    fn success_message(&self) -> String {
        match self {
            Self::SetHostingEnabled(true) => "Local hosting enabled".to_owned(),
            Self::SetHostingEnabled(false) => "Local hosting disabled".to_owned(),
            Self::Approve(_) => "Peer approved".to_owned(),
            Self::Reject(_) => "Peer rejected".to_owned(),
            Self::Remove(_) => "Peer removed from the allowlist".to_owned(),
        }
    }
}

/// Worker updates polled by the UI's existing telemetry tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalHostEvent {
    /// New host/helper and allowlist snapshot.
    Snapshot(LocalHostSnapshot),
    /// Initial pending-approval set from a persistent subscription.
    PendingSnapshot(Vec<PendingPeer>),
    /// Incremental pending-approval update from the persistent subscription.
    PendingEvent(IpcEvent),
    /// Result of a user-requested command.
    CommandFinished {
        /// The action that was sent.
        command: LocalHostCommand,
        /// User-facing result text.
        result: Result<String, String>,
    },
}

/// Handle to the nonblocking local host-agent worker.
pub struct LocalHostWorker {
    commands: SyncSender<LocalHostCommand>,
    events: Receiver<LocalHostEvent>,
}

impl LocalHostWorker {
    /// Starts workers that refresh status and maintain a pending-peer subscription.
    pub fn start() -> Self {
        Self::start_with(Box::new(PlatformRequester), true)
    }

    fn start_with(requester: Box<dyn LocalHostRequester>, subscribe: bool) -> Self {
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_CAPACITY);
        let worker_events = event_tx.clone();
        let spawn_result = thread::Builder::new()
            .name("racc-local-host-ipc".to_owned())
            .spawn(move || run_worker(requester, command_rx, worker_events));
        if let Err(error) = spawn_result {
            let _ = event_tx.try_send(LocalHostEvent::Snapshot(LocalHostSnapshot {
                error: Some(format!(
                    "Could not start the local host-agent worker: {error}"
                )),
                ..LocalHostSnapshot::default()
            }));
        }
        if subscribe {
            let subscription_events = event_tx.clone();
            let spawn_result = thread::Builder::new()
                .name("racc-local-host-pending".to_owned())
                .spawn(move || run_pending_subscription(subscription_events));
            if let Err(error) = spawn_result {
                let _ = event_tx.try_send(LocalHostEvent::Snapshot(LocalHostSnapshot {
                    error: Some(format!(
                        "Could not start the local approval listener: {error}"
                    )),
                    ..LocalHostSnapshot::default()
                }));
            }
        }
        Self {
            commands: command_tx,
            events: event_rx,
        }
    }

    /// Queues a command without waiting for IPC or a pipe/socket connection.
    pub fn send(&self, command: LocalHostCommand) -> Result<(), &'static str> {
        match self.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("Local host-agent command queue is busy."),
            Err(TrySendError::Disconnected(_)) => Err("Local host-agent worker is unavailable."),
        }
    }

    /// Receives one worker update without blocking the UI thread.
    pub fn try_recv(&self) -> Result<LocalHostEvent, TryRecvError> {
        self.events.try_recv()
    }
}

trait LocalHostRequester: Send {
    fn request(&mut self, request: IpcRequest) -> Result<IpcResponse, String>;
}

struct PlatformRequester;

type LocalHostClient = racc_core::ipc::IpcClient<Box<dyn IpcTransport + Send>>;

fn connect_local_host_client() -> Result<LocalHostClient, String> {
    #[cfg(target_os = "windows")]
    {
        let client =
            crate::windows_host_ipc::connect_local_host_agent().map_err(describe_local_error)?;
        Ok(racc_core::ipc::IpcClient::new(Box::new(
            client.into_inner(),
        )))
    }
    #[cfg(target_os = "macos")]
    {
        let client =
            crate::macos_host_ipc::connect_local_host_agent().map_err(describe_local_error)?;
        Ok(racc_core::ipc::IpcClient::new(Box::new(
            client.into_inner(),
        )))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err("Local host-agent IPC is supported on Windows and macOS only.".to_owned())
    }
}

impl LocalHostRequester for PlatformRequester {
    fn request(&mut self, request: IpcRequest) -> Result<IpcResponse, String> {
        connect_local_host_client()?
            .request(request)
            .map_err(|error| error.to_string())
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn describe_local_error(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            "The local host-agent is not running.".to_owned()
        }
        std::io::ErrorKind::PermissionDenied => {
            "Access to the local host-agent was denied.".to_owned()
        }
        _ => "Could not connect to the local host-agent.".to_owned(),
    }
}

fn run_worker(
    mut requester: Box<dyn LocalHostRequester>,
    commands: Receiver<LocalHostCommand>,
    events: SyncSender<LocalHostEvent>,
) {
    let mut next_refresh = Instant::now();
    loop {
        let wait = next_refresh.saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(command) => {
                let result = run_command(requester.as_mut(), &command);
                let _ = events.try_send(LocalHostEvent::CommandFinished { command, result });
                publish_snapshot(requester.as_mut(), &events);
                next_refresh = Instant::now() + REFRESH_INTERVAL;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                publish_snapshot(requester.as_mut(), &events);
                next_refresh = Instant::now() + REFRESH_INTERVAL;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn run_pending_subscription(events: SyncSender<LocalHostEvent>) {
    loop {
        let _ = run_pending_subscription_connection(&events);
        thread::sleep(Duration::from_secs(1));
    }
}

fn run_pending_subscription_connection(events: &SyncSender<LocalHostEvent>) -> Result<(), String> {
    let mut client = connect_local_host_client()?;
    let pending = match client.request(IpcRequest::SubscribePending) {
        Ok(IpcResponse::PendingSnapshot { peers }) => peers,
        Ok(response) => return Err(unexpected_response("pending approvals", response)),
        Err(error) => return Err(error.to_string()),
    };
    events
        .send(LocalHostEvent::PendingSnapshot(pending))
        .map_err(|_| "The app stopped receiving host-agent events.".to_owned())?;
    loop {
        let event = client.next_event().map_err(|error| error.to_string())?;
        events
            .send(LocalHostEvent::PendingEvent(event))
            .map_err(|_| "The app stopped receiving host-agent events.".to_owned())?;
    }
}

fn publish_snapshot(requester: &mut dyn LocalHostRequester, events: &SyncSender<LocalHostEvent>) {
    let snapshot = refresh_snapshot(requester).unwrap_or_else(|error| LocalHostSnapshot {
        error: Some(error),
        ..LocalHostSnapshot::default()
    });
    let _ = events.try_send(LocalHostEvent::Snapshot(snapshot));
}

fn refresh_snapshot(requester: &mut dyn LocalHostRequester) -> Result<LocalHostSnapshot, String> {
    let status = match requester.request(IpcRequest::GetStatus)? {
        IpcResponse::Status { status } => status,
        response => return Err(unexpected_response("status", response)),
    };
    let allowlist = match requester.request(IpcRequest::ListAllowlist)? {
        IpcResponse::Allowlist { peers } => peers,
        response => return Err(unexpected_response("allowlist", response)),
    };
    Ok(LocalHostSnapshot {
        connected: true,
        status: Some(status),
        allowlist,
        error: None,
    })
}

fn run_command(
    requester: &mut dyn LocalHostRequester,
    command: &LocalHostCommand,
) -> Result<String, String> {
    let response = requester.request(command.request())?;
    match response {
        IpcResponse::HostingEnabled { enabled } if matches!(command, LocalHostCommand::SetHostingEnabled(expected) if *expected == enabled) => {
            Ok(command.success_message())
        }
        IpcResponse::ActionApplied { action, node_id } => {
            let matches_command = match (command, action) {
                (LocalHostCommand::Approve(expected), racc_core::ipc::PeerAction::Approve)
                | (LocalHostCommand::Reject(expected), racc_core::ipc::PeerAction::Reject)
                | (LocalHostCommand::Remove(expected), racc_core::ipc::PeerAction::Remove) => {
                    expected == &node_id
                }
                _ => false,
            };
            if matches_command {
                Ok(command.success_message())
            } else {
                Err("The host-agent returned an unexpected command response.".to_owned())
            }
        }
        response => Err(unexpected_response("command", response)),
    }
}

fn unexpected_response(operation: &str, response: IpcResponse) -> String {
    match response {
        IpcResponse::Failure { code } => match code {
            IpcFailureCode::PeerNotFound => {
                "The peer is no longer awaiting this action.".to_owned()
            }
            IpcFailureCode::Unavailable => {
                "The host-agent cannot perform this action now.".to_owned()
            }
            IpcFailureCode::NotAllowed => "The host-agent policy rejected this action.".to_owned(),
        },
        _ => format!("The host-agent returned an unexpected {operation} response."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_core::ipc::{HelperState, PeerAction};

    struct FakeRequester {
        requests: Vec<IpcRequest>,
    }

    impl LocalHostRequester for FakeRequester {
        fn request(&mut self, request: IpcRequest) -> Result<IpcResponse, String> {
            self.requests.push(request.clone());
            Ok(match request {
                IpcRequest::GetStatus => IpcResponse::Status {
                    status: HostStatus {
                        hosting_enabled: true,
                        helper_state: HelperState::Running,
                        connected_viewers: 2,
                        pending_approvals: 1,
                        screen_recording_granted: None,
                        accessibility_granted: None,
                    },
                },
                IpcRequest::ListAllowlist => IpcResponse::Allowlist {
                    peers: vec![AllowlistEntry {
                        node_id: "approved-node".to_owned(),
                        display_name: "Studio PC".to_owned(),
                        login_name: "owner@example.invalid".to_owned(),
                    }],
                },
                IpcRequest::SetHostingEnabled { enabled } => {
                    IpcResponse::HostingEnabled { enabled }
                }
                IpcRequest::Approve { node_id } => IpcResponse::ActionApplied {
                    action: PeerAction::Approve,
                    node_id,
                },
                IpcRequest::Reject { node_id } => IpcResponse::ActionApplied {
                    action: PeerAction::Reject,
                    node_id,
                },
                IpcRequest::Remove { node_id } => IpcResponse::ActionApplied {
                    action: PeerAction::Remove,
                    node_id,
                },
                IpcRequest::SubscribePending => IpcResponse::PendingSnapshot { peers: Vec::new() },
            })
        }
    }

    #[test]
    fn refresh_uses_agent_status_and_allowlist_snapshots() {
        let mut requester = FakeRequester { requests: vec![] };
        let snapshot = refresh_snapshot(&mut requester)
            .unwrap_or_else(|error| panic!("refresh fake host status: {error}"));
        assert!(snapshot.connected);
        assert_eq!(
            snapshot
                .status
                .as_ref()
                .map(|status| status.connected_viewers),
            Some(2)
        );
        assert_eq!(snapshot.allowlist[0].node_id, "approved-node");
        assert_eq!(
            requester.requests,
            vec![IpcRequest::GetStatus, IpcRequest::ListAllowlist]
        );
    }

    #[test]
    fn actions_are_sent_to_the_agent_and_match_their_reply() {
        let cases = [
            (
                LocalHostCommand::SetHostingEnabled(false),
                "Local hosting disabled",
            ),
            (
                LocalHostCommand::Approve("peer-a".to_owned()),
                "Peer approved",
            ),
            (
                LocalHostCommand::Reject("peer-b".to_owned()),
                "Peer rejected",
            ),
            (
                LocalHostCommand::Remove("peer-c".to_owned()),
                "Peer removed from the allowlist",
            ),
        ];
        for (command, expected) in cases {
            let mut requester = FakeRequester { requests: vec![] };
            assert_eq!(
                run_command(&mut requester, &command).as_deref(),
                Ok(expected)
            );
            assert_eq!(requester.requests, vec![command.request()]);
        }
    }

    #[test]
    fn unavailable_refresh_has_no_stale_runtime_status() {
        struct Offline;
        impl LocalHostRequester for Offline {
            fn request(&mut self, _request: IpcRequest) -> Result<IpcResponse, String> {
                Err("The local host-agent is not running.".to_owned())
            }
        }

        let error = refresh_snapshot(&mut Offline).unwrap_err();
        assert_eq!(error, "The local host-agent is not running.");
        let unavailable = LocalHostSnapshot {
            error: Some(error),
            ..LocalHostSnapshot::default()
        };
        assert!(!unavailable.connected);
        assert!(unavailable.status.is_none());
        assert!(unavailable.allowlist.is_empty());
    }
}
