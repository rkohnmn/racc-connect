//! Bounded, platform-neutral protocol for the local host-agent IPC channel.
//!
//! Named-pipe and Unix-socket adapters can implement [`IpcTransport`] without
//! changing these messages. This module does not open listeners or install services.

use std::fmt;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Maximum serialized JSON body size for one local IPC frame.
pub const MAX_IPC_FRAME_BYTES: usize = 64 * 1024;
/// Maximum number of peers returned in one allowlist or pending snapshot.
pub const MAX_PEER_LIST_ENTRIES: usize = 256;
/// Maximum encoded length for an identifier, login name, or display name.
pub const MAX_IPC_TEXT_BYTES: usize = 256;

/// Requests supported by the local app-to-host-agent control channel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IpcRequest {
    /// Retrieve current hosting/helper status.
    GetStatus,
    /// Enable or disable local hosting.
    SetHostingEnabled {
        /// Whether local hosting should accept new viewers.
        enabled: bool,
    },
    /// Retrieve the approved peer list.
    ListAllowlist,
    /// Approve a peer that is awaiting authorization.
    Approve {
        /// Tailscale node identifier of the pending peer.
        node_id: String,
    },
    /// Reject a peer that is awaiting authorization.
    Reject {
        /// Tailscale node identifier of the pending peer.
        node_id: String,
    },
    /// Remove a peer from the approved list.
    Remove {
        /// Tailscale node identifier of the approved peer.
        node_id: String,
    },
    /// Subscribe to pending authorization changes; the response is an initial snapshot.
    SubscribePending,
}

impl IpcRequest {
    fn validate(&self) -> Result<(), IpcError> {
        match self {
            Self::Approve { node_id } | Self::Reject { node_id } | Self::Remove { node_id } => {
                validate_text(node_id, false)
            }
            Self::GetStatus
            | Self::SetHostingEnabled { .. }
            | Self::ListAllowlist
            | Self::SubscribePending => Ok(()),
        }
    }
}

/// Coarse helper lifecycle state exposed to the UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperState {
    /// Hosting is not running.
    Stopped,
    /// The helper is being started.
    Starting,
    /// Capture and host services are active.
    Running,
    /// Capture or the interactive session is recovering.
    Recovering,
    /// User action or an OS permission is required.
    NeedsAttention,
}

/// Small, non-secret status snapshot for the local session panel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostStatus {
    /// Whether the host is configured to accept sessions.
    pub hosting_enabled: bool,
    /// Current helper lifecycle state.
    pub helper_state: HelperState,
    /// Number of currently connected viewers.
    pub connected_viewers: u32,
    /// Number of peers awaiting local approval.
    pub pending_approvals: u32,
}

/// One peer entry shown in the local allowlist UI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowlistEntry {
    /// Tailscale node identifier.
    pub node_id: String,
    /// Human-readable peer name.
    pub display_name: String,
    /// Tailscale login name, when available.
    pub login_name: String,
}

/// One peer awaiting authorization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingPeer {
    /// Tailscale node identifier.
    pub node_id: String,
    /// Human-readable peer name.
    pub display_name: String,
    /// Tailscale login name, when available.
    pub login_name: String,
}

/// Allowlist mutation represented by an [`IpcResponse::ActionApplied`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerAction {
    /// Peer was approved.
    Approve,
    /// Pending peer was rejected.
    Reject,
    /// Approved peer was removed.
    Remove,
}

/// Stable failure categories returned by the host agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcFailureCode {
    /// The requested peer is not in the expected state.
    PeerNotFound,
    /// The host agent cannot perform the operation in its current state.
    Unavailable,
    /// The request is not permitted by host policy.
    NotAllowed,
}

/// Replies to local host-agent requests.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IpcResponse {
    /// Result of [`IpcRequest::GetStatus`].
    Status {
        /// Current host and helper status.
        status: HostStatus,
    },
    /// Result of [`IpcRequest::SetHostingEnabled`].
    HostingEnabled {
        /// Whether local hosting is enabled after the request.
        enabled: bool,
    },
    /// Result of [`IpcRequest::ListAllowlist`].
    Allowlist {
        /// Approved peers.
        peers: Vec<AllowlistEntry>,
    },
    /// Result of an allowlist mutation.
    ActionApplied {
        /// Mutation applied to the peer.
        action: PeerAction,
        /// Tailscale node identifier of the affected peer.
        node_id: String,
    },
    /// Initial pending list returned by [`IpcRequest::SubscribePending`].
    PendingSnapshot {
        /// Peers currently awaiting approval.
        peers: Vec<PendingPeer>,
    },
    /// A request failed for a stable, non-sensitive reason.
    Failure {
        /// Stable error category.
        code: IpcFailureCode,
    },
}

/// An update sent after a client has subscribed to pending authorization changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum IpcEvent {
    /// A peer started awaiting approval.
    PendingAdded {
        /// Newly pending peer.
        peer: PendingPeer,
    },
    /// A previously pending peer is no longer awaiting approval.
    PendingRemoved {
        /// Tailscale node identifier of the removed pending peer.
        node_id: String,
    },
}

/// Server-to-client message. Pending updates may follow a subscription response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "message",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum IpcServerMessage {
    /// One request response.
    Response(IpcResponse),
    /// One asynchronous subscription event.
    Event(IpcEvent),
}

impl IpcServerMessage {
    fn validate(&self) -> Result<(), IpcError> {
        match self {
            Self::Response(response) => response.validate(),
            Self::Event(IpcEvent::PendingAdded { peer }) => validate_pending_peer(peer),
            Self::Event(IpcEvent::PendingRemoved { node_id }) => validate_text(node_id, false),
        }
    }
}

impl IpcResponse {
    fn validate(&self) -> Result<(), IpcError> {
        match self {
            Self::Status { .. } | Self::HostingEnabled { .. } | Self::Failure { .. } => Ok(()),
            Self::Allowlist { peers } => {
                validate_list_len(peers.len())?;
                peers.iter().try_for_each(validate_allowlist_entry)
            }
            Self::ActionApplied { node_id, .. } => validate_text(node_id, false),
            Self::PendingSnapshot { peers } => {
                validate_list_len(peers.len())?;
                peers.iter().try_for_each(validate_pending_peer)
            }
        }
    }
}

fn validate_allowlist_entry(peer: &AllowlistEntry) -> Result<(), IpcError> {
    validate_text(&peer.node_id, false)?;
    validate_text(&peer.display_name, false)?;
    validate_text(&peer.login_name, true)
}

fn validate_pending_peer(peer: &PendingPeer) -> Result<(), IpcError> {
    validate_text(&peer.node_id, false)?;
    validate_text(&peer.display_name, false)?;
    validate_text(&peer.login_name, true)
}

fn validate_list_len(len: usize) -> Result<(), IpcError> {
    if len > MAX_PEER_LIST_ENTRIES {
        Err(IpcError::TooManyPeers)
    } else {
        Ok(())
    }
}

fn validate_text(value: &str, allow_empty: bool) -> Result<(), IpcError> {
    if (!allow_empty && value.trim().is_empty()) || value.len() > MAX_IPC_TEXT_BYTES {
        Err(IpcError::InvalidMessage)
    } else {
        Ok(())
    }
}

/// A byte-stream transport such as a named pipe or Unix domain socket.
///
/// Implementations only provide the stream. Framing, validation, and parsing
/// remain in this platform-neutral module.
pub trait IpcTransport: Read + Write {}

impl<T: Read + Write> IpcTransport for T {}

/// Synchronous request/reply client for the local agent protocol.
pub struct IpcClient<T> {
    transport: T,
}

impl<T: IpcTransport> IpcClient<T> {
    /// Creates a client over an already-connected byte stream.
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Sends one request and waits for its reply.
    pub fn request(&mut self, request: IpcRequest) -> Result<IpcResponse, IpcError> {
        write_request(&mut self.transport, &request)?;
        match read_server_message(&mut self.transport)? {
            IpcServerMessage::Response(response) => Ok(response),
            IpcServerMessage::Event(_) => Err(IpcError::UnexpectedMessage),
        }
    }

    /// Reads one asynchronous event after a pending subscription is established.
    pub fn next_event(&mut self) -> Result<IpcEvent, IpcError> {
        match read_server_message(&mut self.transport)? {
            IpcServerMessage::Event(event) => Ok(event),
            IpcServerMessage::Response(_) => Err(IpcError::UnexpectedMessage),
        }
    }

    /// Returns the underlying transport for platform-specific lifecycle control.
    pub fn into_inner(self) -> T {
        self.transport
    }
}

/// Request handler implemented by the host-agent application layer.
pub trait IpcRequestHandler {
    /// Applies one validated request and returns its response.
    fn handle(&mut self, request: IpcRequest) -> IpcResponse;
}

/// Reads one request, invokes the handler, and writes its response.
///
/// The function handles one exchange so platform adapters can own connection
/// loops, authorization, and shutdown behavior.
pub fn serve_one<T: IpcTransport, H: IpcRequestHandler>(
    transport: &mut T,
    handler: &mut H,
) -> Result<(), IpcError> {
    let request = read_request(transport)?;
    let response = handler.handle(request);
    write_server_message(transport, &IpcServerMessage::Response(response))
}

/// Writes one validated request frame.
pub fn write_request<T: Write>(transport: &mut T, request: &IpcRequest) -> Result<(), IpcError> {
    request.validate()?;
    write_json_frame(transport, request)
}

/// Reads and validates one request frame.
pub fn read_request<T: Read>(transport: &mut T) -> Result<IpcRequest, IpcError> {
    let value = read_json_value(transport)?;
    reject_unknown_request_fields(&value)?;
    let request: IpcRequest =
        serde_json::from_value(value).map_err(|_| IpcError::InvalidMessage)?;
    request.validate()?;
    Ok(request)
}

/// Writes one validated server message frame.
pub fn write_server_message<T: Write>(
    transport: &mut T,
    message: &IpcServerMessage,
) -> Result<(), IpcError> {
    message.validate()?;
    write_json_frame(transport, message)
}

/// Reads and validates one server message frame.
pub fn read_server_message<T: Read>(transport: &mut T) -> Result<IpcServerMessage, IpcError> {
    let value = read_json_value(transport)?;
    reject_unknown_server_fields(&value)?;
    let message: IpcServerMessage =
        serde_json::from_value(value).map_err(|_| IpcError::InvalidMessage)?;
    message.validate()?;
    Ok(message)
}

fn write_json_frame<T: Write, M: Serialize>(
    transport: &mut T,
    message: &M,
) -> Result<(), IpcError> {
    let body = serde_json::to_vec(message).map_err(|_| IpcError::InvalidMessage)?;
    if body.is_empty() {
        return Err(IpcError::EmptyFrame);
    }
    if body.len() > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge { actual: body.len() });
    }
    let length =
        u32::try_from(body.len()).map_err(|_| IpcError::FrameTooLarge { actual: body.len() })?;
    transport
        .write_all(&length.to_le_bytes())
        .and_then(|()| transport.write_all(&body))
        .map_err(IpcError::from_io)?;
    Ok(())
}

fn read_json_value<T: Read>(transport: &mut T) -> Result<serde_json::Value, IpcError> {
    let mut prefix = [0; 4];
    transport
        .read_exact(&mut prefix)
        .map_err(IpcError::from_io)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 {
        return Err(IpcError::EmptyFrame);
    }
    if length > MAX_IPC_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge { actual: length });
    }
    let mut body = vec![0; length];
    transport.read_exact(&mut body).map_err(IpcError::from_io)?;
    serde_json::from_slice(&body).map_err(|_| IpcError::InvalidMessage)
}

fn ensure_fields(value: &serde_json::Value, allowed: &[&str]) -> Result<(), IpcError> {
    let object = value.as_object().ok_or(IpcError::InvalidMessage)?;
    if object.keys().all(|field| allowed.contains(&field.as_str())) {
        Ok(())
    } else {
        Err(IpcError::InvalidMessage)
    }
}

fn discriminator<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, IpcError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(IpcError::InvalidMessage)
}

fn reject_unknown_request_fields(value: &serde_json::Value) -> Result<(), IpcError> {
    match discriminator(value, "type")? {
        "get_status" | "list_allowlist" | "subscribe_pending" => ensure_fields(value, &["type"]),
        "set_hosting_enabled" => ensure_fields(value, &["type", "enabled"]),
        "approve" | "reject" | "remove" => ensure_fields(value, &["type", "node_id"]),
        _ => Err(IpcError::InvalidMessage),
    }
}

fn reject_unknown_server_fields(value: &serde_json::Value) -> Result<(), IpcError> {
    ensure_fields(value, &["message", "body"])?;
    let body = value.get("body").ok_or(IpcError::InvalidMessage)?;
    match discriminator(value, "message")? {
        "response" => reject_unknown_response_fields(body),
        "event" => match discriminator(body, "type")? {
            "pending_added" => {
                ensure_fields(body, &["type", "peer"])?;
                ensure_fields(
                    body.get("peer").ok_or(IpcError::InvalidMessage)?,
                    &["node_id", "display_name", "login_name"],
                )
            }
            "pending_removed" => ensure_fields(body, &["type", "node_id"]),
            _ => Err(IpcError::InvalidMessage),
        },
        _ => Err(IpcError::InvalidMessage),
    }
}

fn reject_unknown_response_fields(value: &serde_json::Value) -> Result<(), IpcError> {
    match discriminator(value, "type")? {
        "status" => {
            ensure_fields(value, &["type", "status"])?;
            ensure_fields(
                value.get("status").ok_or(IpcError::InvalidMessage)?,
                &[
                    "hosting_enabled",
                    "helper_state",
                    "connected_viewers",
                    "pending_approvals",
                ],
            )
        }
        "hosting_enabled" => ensure_fields(value, &["type", "enabled"]),
        "allowlist" => {
            ensure_fields(value, &["type", "peers"])?;
            for peer in value
                .get("peers")
                .and_then(serde_json::Value::as_array)
                .ok_or(IpcError::InvalidMessage)?
            {
                ensure_fields(peer, &["node_id", "display_name", "login_name"])?;
            }
            Ok(())
        }
        "action_applied" => ensure_fields(value, &["type", "action", "node_id"]),
        "pending_snapshot" => {
            ensure_fields(value, &["type", "peers"])?;
            for peer in value
                .get("peers")
                .and_then(serde_json::Value::as_array)
                .ok_or(IpcError::InvalidMessage)?
            {
                ensure_fields(peer, &["node_id", "display_name", "login_name"])?;
            }
            Ok(())
        }
        "failure" => ensure_fields(value, &["type", "code"]),
        _ => Err(IpcError::InvalidMessage),
    }
}

/// Errors from framing, validation, and message exchange.
#[derive(Debug)]
pub enum IpcError {
    /// The underlying stream failed or ended mid-frame.
    Io(io::ErrorKind),
    /// A zero-length frame was received or emitted.
    EmptyFrame,
    /// A frame exceeded [`MAX_IPC_FRAME_BYTES`].
    FrameTooLarge {
        /// Claimed or encoded body length.
        actual: usize,
    },
    /// The JSON body was malformed, contained unknown fields, or failed validation.
    InvalidMessage,
    /// A peer list exceeded [`MAX_PEER_LIST_ENTRIES`].
    TooManyPeers,
    /// A response/event arrived where a different server message was expected.
    UnexpectedMessage,
}

impl IpcError {
    fn from_io(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

impl fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(formatter, "local IPC stream error: {kind}"),
            Self::EmptyFrame => formatter.write_str("local IPC frame is empty"),
            Self::FrameTooLarge { actual } => write!(
                formatter,
                "local IPC frame has {actual} bytes; limit is {MAX_IPC_FRAME_BYTES}"
            ),
            Self::InvalidMessage => formatter.write_str("local IPC message is invalid"),
            Self::TooManyPeers => write!(
                formatter,
                "local IPC peer list exceeds {MAX_PEER_LIST_ENTRIES} entries"
            ),
            Self::UnexpectedMessage => {
                formatter.write_str("unexpected local IPC message for this operation")
            }
        }
    }
}

impl std::error::Error for IpcError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread;

    struct MemoryEndpoint {
        incoming: Receiver<Vec<u8>>,
        outgoing: Sender<Vec<u8>>,
        buffered: VecDeque<u8>,
    }

    impl Read for MemoryEndpoint {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if buffer.is_empty() {
                return Ok(0);
            }
            while self.buffered.is_empty() {
                let chunk = self.incoming.recv().map_err(|_| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "memory transport closed")
                })?;
                self.buffered.extend(chunk);
            }
            let count = buffer.len().min(self.buffered.len());
            for slot in &mut buffer[..count] {
                if let Some(byte) = self.buffered.pop_front() {
                    *slot = byte;
                }
            }
            Ok(count)
        }
    }

    impl Write for MemoryEndpoint {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.outgoing.send(buffer.to_vec()).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "memory transport closed")
            })?;
            Ok(buffer.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn memory_pair() -> (MemoryEndpoint, MemoryEndpoint) {
        let (left_tx, left_rx) = mpsc::channel();
        let (right_tx, right_rx) = mpsc::channel();
        (
            MemoryEndpoint {
                incoming: left_rx,
                outgoing: right_tx,
                buffered: VecDeque::new(),
            },
            MemoryEndpoint {
                incoming: right_rx,
                outgoing: left_tx,
                buffered: VecDeque::new(),
            },
        )
    }

    struct TestHandler;
    impl IpcRequestHandler for TestHandler {
        fn handle(&mut self, request: IpcRequest) -> IpcResponse {
            match request {
                IpcRequest::GetStatus => IpcResponse::Status {
                    status: HostStatus {
                        hosting_enabled: true,
                        helper_state: HelperState::Running,
                        connected_viewers: 1,
                        pending_approvals: 2,
                    },
                },
                IpcRequest::Approve { node_id } => IpcResponse::ActionApplied {
                    action: PeerAction::Approve,
                    node_id,
                },
                IpcRequest::SubscribePending => IpcResponse::PendingSnapshot {
                    peers: vec![PendingPeer {
                        node_id: "node-1".to_owned(),
                        display_name: "Office PC".to_owned(),
                        login_name: "user@example.invalid".to_owned(),
                    }],
                },
                _ => IpcResponse::Failure {
                    code: IpcFailureCode::Unavailable,
                },
            }
        }
    }

    #[test]
    fn client_and_server_round_trip_request_and_response() {
        let (client_stream, mut server_stream) = memory_pair();
        let server = thread::spawn(move || serve_one(&mut server_stream, &mut TestHandler));
        let mut client = IpcClient::new(client_stream);
        let response = client
            .request(IpcRequest::GetStatus)
            .unwrap_or_else(|error| panic!("request failed: {error}"));
        assert_eq!(
            response,
            IpcResponse::Status {
                status: HostStatus {
                    hosting_enabled: true,
                    helper_state: HelperState::Running,
                    connected_viewers: 1,
                    pending_approvals: 2,
                }
            }
        );
        assert!(server.join().is_ok_and(|result| result.is_ok()));
    }

    #[test]
    fn pending_subscription_returns_snapshot_then_events() {
        let (client_stream, mut server_stream) = memory_pair();
        let server = thread::spawn(move || {
            serve_one(&mut server_stream, &mut TestHandler)?;
            write_server_message(
                &mut server_stream,
                &IpcServerMessage::Event(IpcEvent::PendingRemoved {
                    node_id: "node-1".to_owned(),
                }),
            )
        });
        let mut client = IpcClient::new(client_stream);
        let snapshot = client
            .request(IpcRequest::SubscribePending)
            .unwrap_or_else(|error| panic!("subscribe failed: {error}"));
        assert!(matches!(snapshot, IpcResponse::PendingSnapshot { peers } if peers.len() == 1));
        assert_eq!(
            client
                .next_event()
                .unwrap_or_else(|error| panic!("event failed: {error}")),
            IpcEvent::PendingRemoved {
                node_id: "node-1".to_owned()
            }
        );
        assert!(server.join().is_ok_and(|result| result.is_ok()));
    }

    #[test]
    fn malformed_truncated_oversized_and_trailing_data_are_rejected() {
        let truncated = vec![3, 0, 0, 0, b'{'];
        assert!(matches!(
            read_request(&mut truncated.as_slice()),
            Err(IpcError::Io(_))
        ));
        let oversized = ((MAX_IPC_FRAME_BYTES as u32) + 1).to_le_bytes().to_vec();
        assert!(matches!(
            read_request(&mut oversized.as_slice()),
            Err(IpcError::FrameTooLarge { .. })
        ));
        let trailing_body = br#"{"type":"get_status"}{"type":"list_allowlist"}"#;
        let mut trailing = (trailing_body.len() as u32).to_le_bytes().to_vec();
        trailing.extend_from_slice(trailing_body);
        assert!(matches!(
            read_request(&mut trailing.as_slice()),
            Err(IpcError::InvalidMessage)
        ));
        let mut unknown_body = br#"{"type":"get_status","secret":"not-accepted"}"#.to_vec();
        let mut unknown = (unknown_body.len() as u32).to_le_bytes().to_vec();
        unknown.append(&mut unknown_body);
        assert!(matches!(
            read_request(&mut unknown.as_slice()),
            Err(IpcError::InvalidMessage)
        ));
    }

    #[test]
    fn outgoing_requests_and_responses_enforce_bounds() {
        let request = IpcRequest::Approve {
            node_id: " ".to_owned(),
        };
        let mut output = Vec::new();
        assert!(matches!(
            write_request(&mut output, &request),
            Err(IpcError::InvalidMessage)
        ));
        let response = IpcServerMessage::Response(IpcResponse::Allowlist {
            peers: (0..=MAX_PEER_LIST_ENTRIES)
                .map(|index| AllowlistEntry {
                    node_id: format!("node-{index}"),
                    display_name: "Peer".to_owned(),
                    login_name: String::new(),
                })
                .collect(),
        });
        assert!(matches!(
            write_server_message(&mut output, &response),
            Err(IpcError::TooManyPeers)
        ));
    }
}
