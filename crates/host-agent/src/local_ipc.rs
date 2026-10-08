//! Connection-level dispatch shared by platform IPC transports.
//!
//! The operating-system adapter owns connection lifecycle and access control;
//! this module only applies the bounded core protocol to an already-connected
//! byte stream.

use racc_core::ipc::{
    read_request, write_server_message, IpcError, IpcEvent, IpcRequest, IpcRequestHandler,
    IpcServerMessage, IpcTransport,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Maximum wait between pending-authorization events while a client subscribes.
pub const PENDING_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Maximum lifetime of one pending subscription before the app reconnects.
pub const MAX_PENDING_SUBSCRIPTION_DURATION: Duration = Duration::from_secs(60);

/// A host IPC backend that can provide the initial response and later pending events.
pub trait LocalIpcHandler: IpcRequestHandler {
    /// Waits up to `timeout` for the next pending-authorization event.
    ///
    /// Returning `None` means the interval elapsed without an event. Event
    /// sources should bound this wait so shutdown remains responsive.
    fn next_pending_event(&mut self, timeout: Duration) -> Option<IpcEvent>;
}

/// Serves one request and streams later events for `SubscribePending`.
///
/// The caller must enforce the platform pipe ACL before invoking this function.
/// Frames use the bounds and validation rules in `racc-core::ipc`.
pub fn serve_local_ipc_connection<T, H>(
    transport: &mut T,
    handler: &mut H,
    stopping: &AtomicBool,
) -> Result<(), IpcError>
where
    T: IpcTransport,
    H: LocalIpcHandler,
{
    let request = read_request(transport)?;
    let requested_subscription = matches!(request, IpcRequest::SubscribePending);
    let response = handler.handle(request);
    let subscribed = requested_subscription
        && matches!(
            response,
            racc_core::ipc::IpcResponse::PendingSnapshot { .. }
        );
    write_server_message(transport, &IpcServerMessage::Response(response))?;
    if !subscribed {
        return Ok(());
    }

    let subscription_deadline = Instant::now() + MAX_PENDING_SUBSCRIPTION_DURATION;
    while !stopping.load(Ordering::Acquire) && Instant::now() < subscription_deadline {
        if let Some(event) = handler.next_pending_event(PENDING_EVENT_POLL_INTERVAL) {
            write_server_message(transport, &IpcServerMessage::Event(event))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_core::ipc::{
        AllowlistEntry, HelperState, HostStatus, IpcClient, IpcResponse, PeerAction, PendingPeer,
    };
    use std::collections::VecDeque;
    use std::io::{self, Read, Write};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread;

    struct MemoryEndpoint {
        incoming: Receiver<Vec<u8>>,
        outgoing: Sender<Vec<u8>>,
        buffered: VecDeque<u8>,
    }

    impl Read for MemoryEndpoint {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if output.is_empty() {
                return Ok(0);
            }
            while self.buffered.is_empty() {
                let chunk = self.incoming.recv().map_err(|_| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "memory IPC closed")
                })?;
                self.buffered.extend(chunk);
            }
            let count = output.len().min(self.buffered.len());
            for byte in &mut output[..count] {
                if let Some(value) = self.buffered.pop_front() {
                    *byte = value;
                }
            }
            Ok(count)
        }
    }

    impl Write for MemoryEndpoint {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.outgoing
                .send(input.to_vec())
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "memory IPC closed"))?;
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn memory_pair() -> (MemoryEndpoint, MemoryEndpoint) {
        let (client_tx, server_rx) = mpsc::channel();
        let (server_tx, client_rx) = mpsc::channel();
        (
            MemoryEndpoint {
                incoming: client_rx,
                outgoing: client_tx,
                buffered: VecDeque::new(),
            },
            MemoryEndpoint {
                incoming: server_rx,
                outgoing: server_tx,
                buffered: VecDeque::new(),
            },
        )
    }

    struct FakeHandler {
        events: VecDeque<IpcEvent>,
        hosting: bool,
    }

    impl IpcRequestHandler for FakeHandler {
        fn handle(&mut self, request: IpcRequest) -> IpcResponse {
            match request {
                IpcRequest::GetStatus => IpcResponse::Status {
                    status: HostStatus {
                        hosting_enabled: self.hosting,
                        helper_state: HelperState::Running,
                        connected_viewers: 0,
                        pending_approvals: 1,
                        screen_recording_granted: None,
                        accessibility_granted: None,
                    },
                },
                IpcRequest::SetHostingEnabled { enabled } => {
                    self.hosting = enabled;
                    IpcResponse::HostingEnabled { enabled }
                }
                IpcRequest::ListAllowlist => IpcResponse::Allowlist {
                    peers: vec![AllowlistEntry {
                        node_id: "peer-approved".to_owned(),
                        display_name: "Approved PC".to_owned(),
                        login_name: "owner@example.invalid".to_owned(),
                    }],
                },
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
                IpcRequest::SubscribePending => IpcResponse::PendingSnapshot {
                    peers: vec![PendingPeer {
                        node_id: "peer-pending".to_owned(),
                        display_name: "New PC".to_owned(),
                        login_name: String::new(),
                    }],
                },
            }
        }
    }

    impl LocalIpcHandler for FakeHandler {
        fn next_pending_event(&mut self, timeout: Duration) -> Option<IpcEvent> {
            if self.events.is_empty() {
                thread::sleep(timeout.min(Duration::from_millis(5)));
            }
            self.events.pop_front()
        }
    }

    fn run_request(request: IpcRequest, handler: FakeHandler) -> IpcResponse {
        let (client_transport, mut server_transport) = memory_pair();
        let stop = std::sync::Arc::new(AtomicBool::new(true));
        let server_stop = std::sync::Arc::clone(&stop);
        let server = thread::spawn(move || {
            let mut handler = handler;
            serve_local_ipc_connection(&mut server_transport, &mut handler, &server_stop)
        });
        let mut client = IpcClient::new(client_transport);
        let response = client.request(request);
        assert!(server.join().is_ok_and(|result| result.is_ok()));
        response.unwrap_or_else(|error| panic!("local IPC request failed: {error}"))
    }

    #[test]
    fn dispatcher_routes_all_request_kinds() {
        let handler = || FakeHandler {
            events: VecDeque::new(),
            hosting: false,
        };
        assert!(matches!(
            run_request(IpcRequest::GetStatus, handler()),
            IpcResponse::Status { .. }
        ));
        assert_eq!(
            run_request(IpcRequest::SetHostingEnabled { enabled: true }, handler()),
            IpcResponse::HostingEnabled { enabled: true }
        );
        assert!(
            matches!(run_request(IpcRequest::ListAllowlist, handler()), IpcResponse::Allowlist { peers } if peers.len() == 1)
        );
        assert!(matches!(
            run_request(
                IpcRequest::Approve {
                    node_id: "p1".to_owned()
                },
                handler()
            ),
            IpcResponse::ActionApplied {
                action: PeerAction::Approve,
                ..
            }
        ));
        assert!(matches!(
            run_request(
                IpcRequest::Reject {
                    node_id: "p2".to_owned()
                },
                handler()
            ),
            IpcResponse::ActionApplied {
                action: PeerAction::Reject,
                ..
            }
        ));
        assert!(matches!(
            run_request(
                IpcRequest::Remove {
                    node_id: "p3".to_owned()
                },
                handler()
            ),
            IpcResponse::ActionApplied {
                action: PeerAction::Remove,
                ..
            }
        ));
    }

    #[test]
    fn subscription_sends_snapshot_then_pending_events_and_stops_cleanly() {
        let (client_transport, mut server_transport) = memory_pair();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let server_stop = std::sync::Arc::clone(&stop);
        let server = thread::spawn(move || {
            let mut handler = FakeHandler {
                events: VecDeque::from([IpcEvent::PendingAdded {
                    peer: PendingPeer {
                        node_id: "peer-next".to_owned(),
                        display_name: "Second PC".to_owned(),
                        login_name: String::new(),
                    },
                }]),
                hosting: true,
            };
            serve_local_ipc_connection(&mut server_transport, &mut handler, &server_stop)
        });
        let mut client = IpcClient::new(client_transport);
        assert!(matches!(
            client.request(IpcRequest::SubscribePending),
            Ok(IpcResponse::PendingSnapshot { peers }) if peers.len() == 1
        ));
        assert_eq!(
            client
                .next_event()
                .unwrap_or_else(|error| panic!("pending event failed: {error}")),
            IpcEvent::PendingAdded {
                peer: PendingPeer {
                    node_id: "peer-next".to_owned(),
                    display_name: "Second PC".to_owned(),
                    login_name: String::new(),
                }
            }
        );
        stop.store(true, Ordering::Release);
        drop(client);
        assert!(server.join().is_ok_and(|result| result.is_ok()));
    }
}
