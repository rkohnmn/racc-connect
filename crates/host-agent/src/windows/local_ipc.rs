//! Windows named-pipe transport for bounded host-agent local IPC.
//!
//! The pipe accepts only local interactive users, administrators, and SYSTEM;
//! remote pipe clients are rejected by the kernel. Message parsing and bounds
//! are enforced by the platform-neutral core IPC module.

use crate::control_server::TailscaleAllowlistAuthorizer;
use crate::local_ipc::{serve_local_ipc_connection, LocalIpcHandler};
use racc_core::ipc::{
    write_server_message, AllowlistEntry, HelperState, HostStatus, IpcEvent, IpcFailureCode,
    IpcRequest, IpcRequestHandler, IpcResponse, IpcServerMessage, PeerAction, PendingPeer,
    MAX_IPC_FRAME_BYTES, MAX_IPC_TEXT_BYTES, MAX_PEER_LIST_ENTRIES,
};
use racc_core::{HostRuntime, IpcClient};
#[cfg(test)]
use racc_identity::SystemProcessRunner;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    HLOCAL, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, NAMED_PIPE_MODE,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Live request handler shared by the Windows foreground host helper.
pub struct WindowsHostIpcHandler {
    runtime: Arc<Mutex<HostRuntime>>,
    authorizer: Arc<Mutex<TailscaleAllowlistAuthorizer>>,
    hosting_enabled: Arc<AtomicBool>,
    last_pending: BTreeMap<String, PendingPeer>,
}

impl WindowsHostIpcHandler {
    /// Creates a client handler backed by shared host and allowlist state.
    pub fn new(
        runtime: Arc<Mutex<HostRuntime>>,
        authorizer: Arc<Mutex<TailscaleAllowlistAuthorizer>>,
        hosting_enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            runtime,
            authorizer,
            hosting_enabled,
            last_pending: BTreeMap::new(),
        }
    }

    fn pending_snapshot(&self) -> Vec<PendingPeer> {
        lock(&self.authorizer).pending_peers()
    }

    fn update_pending_baseline(&mut self) -> Vec<PendingPeer> {
        let peers = self.pending_snapshot();
        self.last_pending = peers
            .iter()
            .cloned()
            .map(|peer| (peer.node_id.clone(), peer))
            .collect();
        peers
    }

    fn finish_runtime_pending(&self, node_id: &str) {
        let _ = lock(&self.runtime).finish_pending_decision(node_id);
    }

    fn valid_peer_texts(peers: &[PendingPeer]) -> bool {
        peers.iter().all(|peer| {
            peer.node_id.len() <= MAX_IPC_TEXT_BYTES
                && peer.display_name.len() <= MAX_IPC_TEXT_BYTES
                && peer.login_name.len() <= MAX_IPC_TEXT_BYTES
        })
    }

    fn approved_peers(&self) -> Result<Vec<AllowlistEntry>, IpcResponse> {
        let peers = lock(&self.authorizer).approved_peers();
        if peers.len() > MAX_PEER_LIST_ENTRIES
            || peers.iter().any(|peer| {
                peer.node_id.len() > MAX_IPC_TEXT_BYTES
                    || peer.display_name.len() > MAX_IPC_TEXT_BYTES
                    || peer.login_name.len() > MAX_IPC_TEXT_BYTES
            })
        {
            return Err(unavailable());
        }
        let response = IpcResponse::Allowlist {
            peers: peers.clone(),
        };
        if !response_fits_frame(&response) {
            return Err(unavailable());
        }
        Ok(peers)
    }
}

impl IpcRequestHandler for WindowsHostIpcHandler {
    fn handle(&mut self, request: IpcRequest) -> IpcResponse {
        match request {
            IpcRequest::GetStatus => {
                let status = lock(&self.runtime).status().clone();
                let helper_state = if !self.hosting_enabled.load(Ordering::Acquire) {
                    HelperState::Stopped
                } else {
                    match status.phase {
                        racc_core::HostRuntimePhase::WaitingForTailscale => {
                            HelperState::NeedsAttention
                        }
                        racc_core::HostRuntimePhase::BindReady
                        | racc_core::HostRuntimePhase::ViewerConnected => HelperState::Running,
                        racc_core::HostRuntimePhase::Stopped => HelperState::Stopped,
                    }
                };
                IpcResponse::Status {
                    status: HostStatus {
                        hosting_enabled: self.hosting_enabled.load(Ordering::Acquire),
                        helper_state,
                        connected_viewers: u32::from(status.viewer_connected),
                        pending_approvals: u32::try_from(self.pending_snapshot().len())
                            .unwrap_or(u32::MAX),
                        screen_recording_granted: None,
                        accessibility_granted: None,
                    },
                }
            }
            IpcRequest::SetHostingEnabled { .. } => unavailable(),
            IpcRequest::ListAllowlist => match self.approved_peers() {
                Ok(peers) => IpcResponse::Allowlist { peers },
                Err(response) => response,
            },
            IpcRequest::Approve { node_id } => {
                let is_pending = lock(&self.authorizer)
                    .pending_peers()
                    .iter()
                    .any(|peer| peer.node_id == node_id);
                if !is_pending {
                    return peer_not_found();
                }
                if lock(&self.authorizer)
                    .approve_pending(&node_id, unix_seconds())
                    .is_err()
                {
                    return unavailable();
                }
                self.finish_runtime_pending(&node_id);
                IpcResponse::ActionApplied {
                    action: PeerAction::Approve,
                    node_id,
                }
            }
            IpcRequest::Reject { node_id } => {
                let is_pending = lock(&self.authorizer)
                    .pending_peers()
                    .iter()
                    .any(|peer| peer.node_id == node_id);
                if !is_pending {
                    return peer_not_found();
                }
                if lock(&self.authorizer)
                    .reject_pending(&node_id, unix_seconds())
                    .is_err()
                {
                    return unavailable();
                }
                self.finish_runtime_pending(&node_id);
                IpcResponse::ActionApplied {
                    action: PeerAction::Reject,
                    node_id,
                }
            }
            IpcRequest::Remove { node_id } => {
                let is_approved = lock(&self.authorizer)
                    .approved_peers()
                    .iter()
                    .any(|peer| peer.node_id == node_id);
                if !is_approved {
                    return peer_not_found();
                }
                if lock(&self.authorizer).remove_approved(&node_id).is_err() {
                    return unavailable();
                }
                IpcResponse::ActionApplied {
                    action: PeerAction::Remove,
                    node_id,
                }
            }
            IpcRequest::SubscribePending => {
                let peers = self.update_pending_baseline();
                if !Self::valid_peer_texts(&peers) || peers.len() > MAX_PEER_LIST_ENTRIES {
                    unavailable()
                } else {
                    let response = IpcResponse::PendingSnapshot { peers };
                    if response_fits_frame(&response) {
                        response
                    } else {
                        unavailable()
                    }
                }
            }
        }
    }
}

impl LocalIpcHandler for WindowsHostIpcHandler {
    fn next_pending_event(&mut self, timeout: Duration) -> Option<IpcEvent> {
        thread::sleep(timeout);
        let current = self.pending_snapshot();
        if let Some(peer) = current.iter().find(|peer| {
            self.last_pending.get(&peer.node_id) != Some(*peer)
                && peer.node_id.len() <= MAX_IPC_TEXT_BYTES
                && peer.display_name.len() <= MAX_IPC_TEXT_BYTES
                && peer.login_name.len() <= MAX_IPC_TEXT_BYTES
        }) {
            self.last_pending.insert(peer.node_id.clone(), peer.clone());
            return Some(IpcEvent::PendingAdded { peer: peer.clone() });
        }
        if let Some(node_id) = self
            .last_pending
            .keys()
            .find(|node_id| !current.iter().any(|peer| &peer.node_id == *node_id))
            .cloned()
        {
            self.last_pending.remove(&node_id);
            return Some(IpcEvent::PendingRemoved { node_id });
        }
        None
    }
}

fn response_fits_frame(response: &IpcResponse) -> bool {
    let mut frame = Vec::new();
    write_server_message(&mut frame, &IpcServerMessage::Response(response.clone())).is_ok()
        && frame.len() <= MAX_IPC_FRAME_BYTES + 4
}

fn unavailable() -> IpcResponse {
    IpcResponse::Failure {
        code: IpcFailureCode::Unavailable,
    }
}

fn peer_not_found() -> IpcResponse {
    IpcResponse::Failure {
        code: IpcFailureCode::PeerNotFound,
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Local pipe name shared by the Windows helper and user-session app.
pub const LOCAL_IPC_PIPE: &str = r"\\.\pipe\racc-connect-host";
/// Read/write operation deadline for an individual pipe transfer.
const PIPE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_BUFFER_BYTES: u32 = 4096;
const PIPE_POLL_MS: u32 = 100;
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Maximum number of simultaneous app and pending-subscription pipe clients.
pub const MAX_LOCAL_IPC_CLIENTS: usize = 4;

/// Runs a bounded multi-client named-pipe server until the owner requests shutdown.
///
/// Each accepted client gets a handler backed by shared host state. A pending
/// subscription occupies one of the bounded workers for at most 60 seconds.
pub fn run_local_ipc_server<F, H>(make_handler: F, stopping: Arc<AtomicBool>) -> io::Result<()>
where
    F: Fn() -> H + Send + Sync + 'static,
    H: LocalIpcHandler + Send + 'static,
{
    let make_handler = Arc::new(make_handler);
    let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut server_error = None;

    while !stopping.load(Ordering::Acquire) {
        reap_finished_workers(&mut workers);
        if workers.len() >= MAX_LOCAL_IPC_CLIENTS {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        let pipe = match create_server_pipe() {
            Ok(pipe) => pipe,
            Err(error) => {
                server_error = Some(error);
                break;
            }
        };
        match connect_server_pipe(pipe.0, &stopping) {
            Ok(true) => {
                let worker_stopping = Arc::clone(&stopping);
                let handler_factory = Arc::clone(&make_handler);
                match thread::Builder::new()
                    .name("racc-local-ipc-client".to_owned())
                    .spawn(move || {
                        let mut stream = PipeStream::server(pipe, Arc::clone(&worker_stopping));
                        let mut handler = handler_factory();
                        let _ =
                            serve_local_ipc_connection(&mut stream, &mut handler, &worker_stopping);
                    }) {
                    Ok(worker) => workers.push(worker),
                    Err(error) => {
                        server_error = Some(error);
                        break;
                    }
                }
            }
            Ok(false) => break,
            Err(error) if stopping.load(Ordering::Acquire) => {
                let _ = error;
                break;
            }
            Err(error) => {
                server_error = Some(error);
                break;
            }
        }
    }

    for worker in workers {
        let _ = worker.join();
    }
    if let Some(error) = server_error {
        Err(error)
    } else {
        Ok(())
    }
}

fn reap_finished_workers(workers: &mut Vec<thread::JoinHandle<()>>) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            let _ = worker.join();
        } else {
            index += 1;
        }
    }
}

/// Connects the safe core IPC client to this process's host-agent pipe.
pub fn connect_local_ipc_client() -> io::Result<IpcClient<PipeStream>> {
    connect_local_ipc_transport().map(IpcClient::new)
}

/// Connects a raw bounded transport, useful when the caller needs custom protocol flow.
pub fn connect_local_ipc_transport() -> io::Result<PipeStream> {
    let name = wide_null(LOCAL_IPC_PIPE);
    let deadline = Instant::now() + PIPE_TRANSFER_TIMEOUT;
    loop {
        // SAFETY: The pipe path is a static NUL-terminated UTF-16 name. The handle is
        // immediately wrapped for unique ownership. Overlapped mode is paired with an
        // OVERLAPPED event for every read and write below.
        let opened = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                FILE_CREATION_DISPOSITION(OPEN_EXISTING.0),
                FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_OVERLAPPED.0),
                HANDLE::default(),
            )
        };
        match opened {
            Ok(pipe) => return Ok(PipeStream::client(OwnedHandle(pipe))),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(to_io_error(error));
                }
                // SAFETY: The pipe name is valid and NUL-terminated. This bounded wait
                // is only used to retry while all bounded server instances are busy.
                let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), PIPE_POLL_MS) };
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Returns the explicit pipe DACL in SDDL form for documentation and tests.
pub fn local_pipe_security_descriptor_sddl() -> &'static str {
    PIPE_SDDL
}

fn create_server_pipe() -> io::Result<OwnedHandle> {
    let name = wide_null(LOCAL_IPC_PIPE);
    let sddl = wide_null(PIPE_SDDL);
    let mut descriptor = PSECURITY_DESCRIPTOR(ptr::null_mut());
    // SAFETY: The SDDL string is valid and NUL-terminated; the API writes the
    // allocated descriptor pointer, which is freed by SecurityDescriptor.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            ptr::addr_of_mut!(descriptor),
            None,
        )
    }
    .map_err(to_io_error)?;
    let descriptor = SecurityDescriptor(descriptor.0);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let mode = NAMED_PIPE_MODE(
        PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
    );
    let access = FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX.0 | FILE_FLAG_OVERLAPPED.0);
    // SAFETY: Name and security descriptor live through the call. Each pipe is a
    // byte-stream instance, asynchronous, bounded by protocol framing, and rejects
    // remote clients. The returned handle is immediately owned.
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            access,
            mode,
            MAX_LOCAL_IPC_CLIENTS as u32,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&attributes),
        )
    };
    if pipe == INVALID_HANDLE_VALUE || pipe.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(pipe))
}

fn connect_server_pipe(pipe: HANDLE, stopping: &AtomicBool) -> io::Result<bool> {
    let event = create_event()?;
    let mut operation = OVERLAPPED {
        hEvent: event.0,
        ..Default::default()
    };
    // SAFETY: The pipe is owned by the caller. `operation` and its live event stay
    // valid until the pending operation is completed or explicitly canceled/reaped.
    let result = unsafe { ConnectNamedPipe(pipe, Some(&mut operation)) };
    if result.is_ok()
        || result
            .as_ref()
            .is_err_and(|error| win32_code(error) == ERROR_PIPE_CONNECTED.0)
    {
        return Ok(true);
    }
    let error = result
        .err()
        .map(to_io_error)
        .unwrap_or_else(|| io::Error::other("named pipe connection failed"));
    if error.raw_os_error() != Some(ERROR_IO_PENDING.0 as i32) {
        return Err(error);
    }

    loop {
        if stopping.load(Ordering::Acquire) {
            cancel_and_reap(pipe, &operation);
            return Ok(false);
        }
        // SAFETY: `event` is live and owned until after this wait.
        match unsafe { WaitForSingleObject(event.0, PIPE_POLL_MS) } {
            WAIT_OBJECT_0 => {
                let mut transferred = 0;
                // SAFETY: The overlapped connect has signaled; its storage remains live.
                unsafe { GetOverlappedResult(pipe, &operation, &mut transferred, false) }
                    .map_err(to_io_error)?;
                return Ok(true);
            }
            WAIT_TIMEOUT => {}
            _ => return Err(io::Error::last_os_error()),
        }
    }
}

pub struct PipeStream {
    pipe: OwnedHandle,
    stopping: Option<Arc<AtomicBool>>,
}

impl PipeStream {
    fn server(pipe: OwnedHandle, stopping: Arc<AtomicBool>) -> Self {
        Self {
            pipe,
            stopping: Some(stopping),
        }
    }

    fn client(pipe: OwnedHandle) -> Self {
        Self {
            pipe,
            stopping: None,
        }
    }

    fn transfer(&mut self, buffer: &mut [u8], writing: bool) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let event = create_event()?;
        let mut operation = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        let mut transferred = 0_u32;
        let result = if writing {
            // SAFETY: The pipe and event are valid; `buffer` remains borrowed and
            // stable until overlapped completion is reaped below.
            unsafe {
                WriteFile(
                    self.pipe.0,
                    Some(buffer),
                    Some(&mut transferred),
                    Some(&mut operation),
                )
            }
        } else {
            // SAFETY: The pipe and event are valid; the writable buffer remains
            // borrowed and stable until overlapped completion is reaped below.
            unsafe {
                ReadFile(
                    self.pipe.0,
                    Some(buffer),
                    Some(&mut transferred),
                    Some(&mut operation),
                )
            }
        };
        if let Err(error) = result {
            if win32_code(&error) != ERROR_IO_PENDING.0 {
                return Err(to_io_error(error));
            }
            let deadline = Instant::now() + PIPE_TRANSFER_TIMEOUT;
            loop {
                if self
                    .stopping
                    .as_ref()
                    .is_some_and(|stop| stop.load(Ordering::Acquire))
                    || Instant::now() >= deadline
                {
                    cancel_and_reap(self.pipe.0, &operation);
                    return Err(io::Error::new(
                        if self
                            .stopping
                            .as_ref()
                            .is_some_and(|stop| stop.load(Ordering::Acquire))
                        {
                            io::ErrorKind::Interrupted
                        } else {
                            io::ErrorKind::TimedOut
                        },
                        "named pipe transfer canceled or timed out",
                    ));
                }
                // SAFETY: Event is valid for this pending operation and remains live.
                match unsafe { WaitForSingleObject(event.0, PIPE_POLL_MS) } {
                    WAIT_OBJECT_0 => {
                        // SAFETY: The operation signaled and its storage remains live.
                        unsafe {
                            GetOverlappedResult(self.pipe.0, &operation, &mut transferred, false)
                        }
                        .map_err(to_io_error)?;
                        break;
                    }
                    WAIT_TIMEOUT => {}
                    _ => return Err(io::Error::last_os_error()),
                }
            }
        }
        usize::try_from(transferred).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "named pipe transfer size overflow",
            )
        })
    }
}

impl Read for PipeStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.transfer(output, false)
    }
}

impl Write for PipeStream {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.transfer(&mut input.to_vec(), true)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeStream {
    fn drop(&mut self) {
        // SAFETY: The handle is still live and uniquely owned. DisconnectNamedPipe
        // is best-effort because client disconnect is expected after each exchange.
        let _ = unsafe { DisconnectNamedPipe(self.pipe.0) };
    }
}

struct OwnedHandle(HANDLE);

// SAFETY: Windows kernel handles can be used from another thread. This wrapper
// transfers unique ownership when moved; it is not Sync, and only its owner may
// issue operations or close the handle.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: This wrapper uniquely owns the valid handle and closes it once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

struct SecurityDescriptor(*mut std::ffi::c_void);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This pointer came from ConvertStringSecurityDescriptor... and
            // uses the LocalFree allocator; it is released exactly once.
            let _ = unsafe { windows::Win32::Foundation::LocalFree(HLOCAL(self.0)) };
        }
    }
}

fn create_event() -> io::Result<OwnedHandle> {
    // SAFETY: No name or custom security attributes are needed. The event handle
    // is immediately wrapped and closed after the associated operation completes.
    unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .map(OwnedHandle)
        .map_err(to_io_error)
}

fn cancel_and_reap(pipe: HANDLE, operation: &OVERLAPPED) {
    // SAFETY: `operation` belongs to this handle and remains valid until the
    // cancellation is reaped. ERROR_NOT_FOUND means the operation already ended.
    let _ = unsafe { CancelIoEx(pipe, Some(operation)) };
    let mut transferred = 0;
    // SAFETY: The operation storage is live and cancellation has been requested.
    let _ = unsafe { GetOverlappedResult(pipe, operation, &mut transferred, true) };
}

fn wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win32_code(error: &windows::core::Error) -> u32 {
    error.code().0 as u32 & 0xffff
}

fn to_io_error(error: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(win32_code(&error) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_core::ipc::IpcRequest;
    use racc_identity::{Allowlist, PeerIdentity};
    use racc_proto::{HelloAck, HelloStatus, OsType, PROTOCOL_VERSION};
    use racc_session::HostConfig;
    use racc_topology::Topology;
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    fn make_handler() -> (WindowsHostIpcHandler, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "racc-host-ipc-handler-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let allowlist = Allowlist::open_in_config_root(&root)
            .unwrap_or_else(|error| panic!("open temporary allowlist: {error}"));
        let authorizer =
            TailscaleAllowlistAuthorizer::<SystemProcessRunner>::from_allowlist_for_test(allowlist);
        let runtime = HostRuntime::new(
            HelloAck {
                protocol_version: PROTOCOL_VERSION,
                status: HelloStatus::Ok,
                device_name: "Test host".to_owned(),
                os: OsType::Windows,
                app_version: "test".to_owned(),
                codecs: 1,
                max_height: 1080,
                features: 1,
                host_cpu_cores: 4,
            },
            Topology::new(1, Vec::new(), None)
                .unwrap_or_else(|error| panic!("create test topology: {error}")),
            racc_identity::DEFAULT_CONTROL_PORT,
            HostConfig::default(),
        )
        .unwrap_or_else(|error| panic!("create host runtime: {error}"));
        let runtime = Arc::new(Mutex::new(runtime));
        lock(&runtime)
            .update_tailscale_address(Some(IpAddr::from([100, 64, 0, 1])))
            .unwrap_or_else(|error| panic!("set test bind address: {error}"));
        let authorizer = Arc::new(Mutex::new(authorizer));
        let handler =
            WindowsHostIpcHandler::new(runtime, authorizer, Arc::new(AtomicBool::new(true)));
        (handler, root)
    }

    #[test]
    fn real_handler_reports_status_and_rejects_unavailable_host_toggle() {
        let (mut handler, root) = make_handler();
        assert!(matches!(
            handler.handle(IpcRequest::GetStatus),
            IpcResponse::Status {
                status: HostStatus {
                    hosting_enabled: true,
                    helper_state: HelperState::Running,
                    connected_viewers: 0,
                    pending_approvals: 0,
                    screen_recording_granted: None,
                    accessibility_granted: None,
                }
            }
        ));
        assert_eq!(
            handler.handle(IpcRequest::SetHostingEnabled { enabled: false }),
            unavailable()
        );
        assert!(matches!(
            handler.handle(IpcRequest::GetStatus),
            IpcResponse::Status {
                status: HostStatus {
                    hosting_enabled: true,
                    ..
                }
            }
        ));
        drop(handler);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn real_handler_snapshots_and_mutates_persistent_peer_states() {
        let (mut handler, root) = make_handler();
        let pending_identity = PeerIdentity {
            node_id: "pending-node".to_owned(),
            owner_login: Some("owner@example.test".to_owned()),
            tags: Vec::new(),
            node_name: Some("Pending PC".to_owned()),
            addresses: Vec::new(),
        };
        let approved_identity = PeerIdentity {
            node_id: "approved-node".to_owned(),
            owner_login: Some("owner@example.test".to_owned()),
            tags: Vec::new(),
            node_name: Some("Approved PC".to_owned()),
            addresses: Vec::new(),
        };
        {
            let mut authorizer = lock(&handler.authorizer);
            assert!(authorizer
                .seed_peer_for_test(&pending_identity, false, 10)
                .is_ok());
            assert!(authorizer
                .seed_peer_for_test(&approved_identity, true, 11)
                .is_ok());
        }
        assert!(matches!(
            handler.handle(IpcRequest::ListAllowlist),
            IpcResponse::Allowlist { peers } if peers.len() == 1 && peers[0].node_id == "approved-node"
        ));
        assert!(matches!(
            handler.handle(IpcRequest::SubscribePending),
            IpcResponse::PendingSnapshot { peers } if peers.len() == 1 && peers[0].node_id == "pending-node"
        ));
        assert_eq!(
            handler.handle(IpcRequest::Approve {
                node_id: "pending-node".to_owned()
            }),
            IpcResponse::ActionApplied {
                action: PeerAction::Approve,
                node_id: "pending-node".to_owned()
            }
        );
        assert!(matches!(
            handler.next_pending_event(Duration::ZERO),
            Some(IpcEvent::PendingRemoved { node_id }) if node_id == "pending-node"
        ));
        assert_eq!(
            handler.handle(IpcRequest::Remove {
                node_id: "approved-node".to_owned()
            }),
            IpcResponse::ActionApplied {
                action: PeerAction::Remove,
                node_id: "approved-node".to_owned()
            }
        );
        assert_eq!(
            handler.handle(IpcRequest::Reject {
                node_id: "missing-node".to_owned()
            }),
            peer_not_found()
        );
        drop(handler);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pipe_acl_and_modes_are_local_and_restrictive() {
        assert_eq!(
            local_pipe_security_descriptor_sddl(),
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)"
        );
        let mode =
            PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0;
        assert_ne!(mode & PIPE_REJECT_REMOTE_CLIENTS.0, 0);
        assert!(LOCAL_IPC_PIPE.starts_with(r"\\.\pipe\"));
    }
}
