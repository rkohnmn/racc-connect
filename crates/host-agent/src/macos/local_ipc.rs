//! macOS per-user Unix-domain-socket transport for the local host-agent IPC.
//!
//! The app-side adapter intentionally duplicates the path and ownership checks
//! in `crates/app/src/macos_host_ipc.rs`; the binary crate cannot be a library
//! dependency of the app. Keep `HOST_SOCKET_RELATIVE_PATH` identical there.

use crate::control_server::TailscaleAllowlistAuthorizer;
use crate::local_ipc::{serve_local_ipc_connection, LocalIpcHandler};
use racc_capture::macos::{screen_recording_access, ScreenRecordingAccess};
use racc_core::ipc::{
    write_server_message, AllowlistEntry, HelperState, HostStatus, IpcEvent, IpcFailureCode,
    IpcRequest, IpcRequestHandler, IpcResponse, IpcServerMessage, PeerAction, PendingPeer,
    MAX_IPC_FRAME_BYTES, MAX_IPC_TEXT_BYTES, MAX_PEER_LIST_ENTRIES,
};
use racc_core::{HostRuntime, HostRuntimePhase};
use racc_input::macos::MacQuartzInputInjector;
use std::collections::BTreeMap;
use std::fs::{self, FileType, Metadata, Permissions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Application Support path shared with the app-side connector.
pub const HOST_SOCKET_RELATIVE_PATH: &str =
    "Library/Application Support/RaccConnect/host-agent.sock";
/// Mode required for the app-specific directory.
pub const APP_DIRECTORY_MODE: u32 = 0o700;
/// Mode required for the socket node.
pub const SOCKET_MODE: u32 = 0o600;
/// Bounded number of simultaneous client workers.
pub const MAX_CONNECTION_WORKERS: usize = 4;
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(1);
const MACOS_SUN_PATH_CAPACITY: usize = 104;

/// A bound local IPC listener. Dropping it removes only its own socket node.
pub struct MacLocalIpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
    socket_identity: (u64, u64),
    owner_uid: u32,
}

impl MacLocalIpcServer {
    /// Creates the per-user socket, enforcing a private directory and socket mode.
    pub fn bind_default() -> io::Result<Self> {
        let socket_path = default_socket_path()?;
        check_socket_path_length(&socket_path)?;
        ensure_private_app_directory(&socket_path)?;
        remove_stale_owned_socket(&socket_path, effective_uid() as u32)?;

        let listener = UnixListener::bind(&socket_path)?;
        let owner_uid = effective_uid() as u32;
        if let Err(error) = set_socket_mode(&socket_path, owner_uid) {
            drop(listener);
            remove_stale_owned_socket(&socket_path, owner_uid).ok();
            return Err(error);
        }
        let metadata = fs::symlink_metadata(&socket_path)?;
        validate_socket_metadata(&metadata, owner_uid, Some(SOCKET_MODE))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket_path,
            socket_identity: (metadata.dev(), metadata.ino()),
            owner_uid,
        })
    }

    /// Returns the bound socket path for diagnostics and tests.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Accepts local clients until `stopping` is set.
    ///
    /// The listener polls nonblocking accept, caps workers at
    /// [`MAX_CONNECTION_WORKERS`], gives each stream bounded I/O timeouts, and
    /// joins workers on exit. `make_handler` should return a handler backed by
    /// shared application state if requests need to observe common state.
    pub fn serve_until<F, H>(
        &self,
        stopping: std::sync::Arc<AtomicBool>,
        make_handler: F,
    ) -> io::Result<()>
    where
        F: Fn() -> H + Send + Sync + 'static,
        H: LocalIpcHandler + Send + 'static,
    {
        let make_handler = std::sync::Arc::new(make_handler);
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        let mut accept_error = None;

        while !stopping.load(Ordering::Acquire) {
            reap_finished_workers(&mut workers);
            match self.listener.accept() {
                Ok((mut stream, _peer)) => {
                    if validate_peer_owner(&stream, self.owner_uid).is_err() {
                        drop(stream);
                        continue;
                    }
                    if workers.len() >= MAX_CONNECTION_WORKERS {
                        drop(stream);
                        continue;
                    }
                    if let Err(error) = stream
                        .set_read_timeout(Some(CONNECTION_TIMEOUT))
                        .and_then(|()| stream.set_write_timeout(Some(CONNECTION_TIMEOUT)))
                    {
                        accept_error = Some(error);
                        break;
                    }
                    let worker_stopping = std::sync::Arc::clone(&stopping);
                    let handler_factory = std::sync::Arc::clone(&make_handler);
                    let spawned = thread::Builder::new()
                        .name("racc-macos-local-ipc".to_owned())
                        .spawn(move || {
                            let mut handler = handler_factory();
                            let _ = serve_local_ipc_connection(
                                &mut stream,
                                &mut handler,
                                &worker_stopping,
                            );
                        });
                    match spawned {
                        Ok(worker) => workers.push(worker),
                        Err(error) => {
                            accept_error = Some(error);
                            break;
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    accept_error = Some(error);
                    break;
                }
            }
        }

        for worker in workers {
            let _ = worker.join();
        }
        if let Some(error) = accept_error {
            Err(error)
        } else {
            Ok(())
        }
    }
}

impl Drop for MacLocalIpcServer {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.socket_path) else {
            return;
        };
        if validate_socket_metadata(&metadata, self.owner_uid, None).is_ok()
            && (metadata.dev(), metadata.ino()) == self.socket_identity
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

/// Returns the default per-user Application Support socket path.
pub fn default_socket_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unavailable"))?;
    if !home.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "HOME must be an absolute path",
        ));
    }
    let path = home.join(HOST_SOCKET_RELATIVE_PATH);
    check_socket_path_length(&path)?;
    Ok(path)
}

/// Returns the current user's private Application Support directory for app data.
pub fn default_app_data_directory() -> io::Result<PathBuf> {
    let socket_path = default_socket_path()?;
    ensure_private_app_directory(&socket_path)?;
    socket_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))
}

fn ensure_private_app_directory(socket_path: &Path) -> io::Result<()> {
    let uid = effective_uid() as u32;
    let app_dir = socket_path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    let support_dir = app_dir.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "app directory has no parent")
    })?;
    let library_dir = support_dir.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "support directory has no parent",
        )
    })?;
    let home_dir = library_dir
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Library has no home parent"))?;

    validate_owned_directory(home_dir, uid, None)?;
    validate_owned_directory(library_dir, uid, None)?;
    validate_owned_directory(support_dir, uid, None)?;
    match fs::create_dir(app_dir) {
        Ok(()) => {
            fs::set_permissions(app_dir, Permissions::from_mode(APP_DIRECTORY_MODE))?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_owned_directory(app_dir, uid, None)?;
            fs::set_permissions(app_dir, Permissions::from_mode(APP_DIRECTORY_MODE))?;
        }
        Err(error) => return Err(error),
    }
    validate_owned_directory(app_dir, uid, Some(APP_DIRECTORY_MODE))
}

fn validate_owned_directory(path: &Path, uid: u32, required_mode: Option<u32>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local IPC parent is not a real directory",
        ));
    }
    if metadata.uid() != uid {
        return Err(permission_error(
            "local IPC directory is owned by another user",
        ));
    }
    if required_mode.is_some_and(|mode| metadata.mode() & 0o777 != mode) {
        return Err(permission_error(
            "local IPC directory permissions are unsafe",
        ));
    }
    Ok(())
}

fn set_socket_mode(path: &Path, uid: u32) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    validate_socket_metadata(&metadata, uid, None)?;
    fs::set_permissions(path, Permissions::from_mode(SOCKET_MODE))?;
    Ok(())
}

fn remove_stale_owned_socket(path: &Path, uid: u32) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_socket_metadata(&metadata, uid, None)?;
    match UnixStream::connect(path) {
        Ok(stream) => {
            drop(stream);
            Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "host-agent socket is already accepting connections",
            ))
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            let current = fs::symlink_metadata(path)?;
            validate_socket_metadata(&current, uid, None)?;
            if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "host-agent socket changed while checking stale path",
                ));
            }
            fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

fn validate_socket_metadata(
    metadata: &Metadata,
    uid: u32,
    required_mode: Option<u32>,
) -> io::Result<()> {
    validate_socket_type(&metadata.file_type())?;
    if metadata.uid() != uid {
        return Err(permission_error(
            "local IPC socket is owned by another user",
        ));
    }
    if required_mode.is_some_and(|mode| metadata.mode() & 0o777 != mode) {
        return Err(permission_error("local IPC socket permissions are unsafe"));
    }
    Ok(())
}

fn validate_socket_type(file_type: &FileType) -> io::Result<()> {
    if file_type.is_socket() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing to replace a non-socket local IPC path",
        ))
    }
}

fn validate_peer_owner(stream: &UnixStream, expected_uid: u32) -> io::Result<()> {
    let mut peer_uid: libc::uid_t = 0;
    let mut peer_gid: libc::gid_t = 0;
    // SAFETY: the stream owns a live Unix socket descriptor and both output
    // pointers refer to initialized, writable credential-sized values.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_uid, &mut peer_gid) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if peer_uid != expected_uid {
        return Err(permission_error(
            "local IPC client is owned by another user",
        ));
    }
    Ok(())
}

fn check_socket_path_length(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    if path.as_os_str().as_bytes().len() >= MACOS_SUN_PATH_CAPACITY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local IPC socket path exceeds macOS Unix-domain socket limit",
        ));
    }
    Ok(())
}

fn permission_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid has no pointer arguments or preconditions and returns the caller's UID.
    unsafe { libc::geteuid() }
}

fn reap_finished_workers(workers: &mut Vec<JoinHandle<()>>) {
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

/// Live IPC request handler for the foreground macOS host.
pub struct MacHostIpcHandler {
    runtime: Arc<Mutex<HostRuntime>>,
    authorizer: Arc<Mutex<TailscaleAllowlistAuthorizer>>,
    hosting_enabled: Arc<AtomicBool>,
    last_pending: BTreeMap<String, PendingPeer>,
}

impl MacHostIpcHandler {
    /// Creates a handler backed by the same runtime and allowlist as host control.
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

    fn approved_peers(&self) -> Result<Vec<AllowlistEntry>, IpcResponse> {
        let peers = lock(&self.authorizer).approved_peers();
        if peers.len() > MAX_PEER_LIST_ENTRIES || !valid_allowlist_texts(&peers) {
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

impl IpcRequestHandler for MacHostIpcHandler {
    fn handle(&mut self, request: IpcRequest) -> IpcResponse {
        match request {
            IpcRequest::GetStatus => {
                let host_status = lock(&self.runtime).status().clone();
                let hosting_enabled = self.hosting_enabled.load(Ordering::Acquire);
                let helper_state = if !hosting_enabled {
                    HelperState::Stopped
                } else {
                    match host_status.phase {
                        HostRuntimePhase::WaitingForTailscale => HelperState::NeedsAttention,
                        HostRuntimePhase::BindReady | HostRuntimePhase::ViewerConnected => {
                            HelperState::Running
                        }
                        HostRuntimePhase::Stopped => HelperState::Stopped,
                    }
                };
                IpcResponse::Status {
                    status: HostStatus {
                        hosting_enabled,
                        helper_state,
                        connected_viewers: u32::from(host_status.viewer_connected),
                        pending_approvals: u32::try_from(self.pending_snapshot().len())
                            .unwrap_or(u32::MAX),
                        screen_recording_granted: Some(matches!(
                            screen_recording_access(),
                            ScreenRecordingAccess::Granted
                        )),
                        accessibility_granted: Some(MacQuartzInputInjector::accessibility_trusted()),
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
                if peers.len() > MAX_PEER_LIST_ENTRIES || !valid_pending_texts(&peers) {
                    return unavailable();
                }
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

impl LocalIpcHandler for MacHostIpcHandler {
    fn next_pending_event(&mut self, timeout: Duration) -> Option<IpcEvent> {
        if !timeout.is_zero() {
            thread::sleep(timeout.min(crate::local_ipc::PENDING_EVENT_POLL_INTERVAL));
        }
        let current = self.pending_snapshot();
        if let Some(peer) = current.iter().find(|peer| {
            self.last_pending.get(&peer.node_id) != Some(*peer) && valid_pending_peer(peer)
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

fn valid_allowlist_texts(peers: &[AllowlistEntry]) -> bool {
    peers.iter().all(|peer| {
        peer.node_id.len() <= MAX_IPC_TEXT_BYTES
            && peer.display_name.len() <= MAX_IPC_TEXT_BYTES
            && peer.login_name.len() <= MAX_IPC_TEXT_BYTES
    })
}

fn valid_pending_peer(peer: &PendingPeer) -> bool {
    peer.node_id.len() <= MAX_IPC_TEXT_BYTES
        && peer.display_name.len() <= MAX_IPC_TEXT_BYTES
        && peer.login_name.len() <= MAX_IPC_TEXT_BYTES
}

fn valid_pending_texts(peers: &[PendingPeer]) -> bool {
    peers.iter().all(valid_pending_peer)
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

#[cfg(test)]
mod handler_tests {
    use super::*;
    use crate::control_server::TailscaleAllowlistAuthorizer;
    use racc_core::ipc::{IpcRequest, PeerAction};
    use racc_identity::{Allowlist, PeerIdentity, SystemProcessRunner};
    use racc_proto::{HelloAck, HelloStatus, OsType, PROTOCOL_VERSION};
    use racc_session::HostConfig;
    use racc_topology::Topology;
    use std::net::IpAddr;
    use std::sync::atomic::AtomicU64;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn make_handler() -> (
        MacHostIpcHandler,
        Arc<Mutex<TailscaleAllowlistAuthorizer>>,
        PathBuf,
    ) {
        let root = std::env::temp_dir().join(format!(
            "racc-mac-host-ipc-handler-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let allowlist = Allowlist::open_in_config_root(&root)
            .unwrap_or_else(|error| panic!("open temporary allowlist: {error}"));
        let authorizer = Arc::new(Mutex::new(TailscaleAllowlistAuthorizer::<
            SystemProcessRunner,
        >::from_allowlist_for_test(allowlist)));
        let mut runtime = HostRuntime::new(
            HelloAck {
                protocol_version: PROTOCOL_VERSION,
                status: HelloStatus::Ok,
                device_name: "Mac test host".to_owned(),
                os: OsType::MacOs,
                app_version: "test".to_owned(),
                codecs: 1,
                max_height: 720,
                features: 1,
                host_cpu_cores: 4,
            },
            Topology::new(1, Vec::new(), None)
                .unwrap_or_else(|error| panic!("create test topology: {error}")),
            racc_identity::DEFAULT_CONTROL_PORT,
            HostConfig::default(),
        )
        .unwrap_or_else(|error| panic!("create host runtime: {error}"));
        runtime
            .update_tailscale_address(Some(IpAddr::from([100, 64, 0, 1])))
            .unwrap_or_else(|error| panic!("set test bind address: {error}"));
        let runtime = Arc::new(Mutex::new(runtime));
        let hosting_enabled = Arc::new(AtomicBool::new(true));
        let handler = MacHostIpcHandler::new(runtime, Arc::clone(&authorizer), hosting_enabled);
        (handler, authorizer, root)
    }

    fn identity(node_id: &str, node_name: &str) -> PeerIdentity {
        PeerIdentity {
            node_id: node_id.to_owned(),
            owner_login: Some("owner@example.test".to_owned()),
            tags: Vec::new(),
            node_name: Some(node_name.to_owned()),
            addresses: Vec::new(),
        }
    }

    #[test]
    fn reports_live_status_and_never_fakes_host_toggle_success() {
        let (mut handler, _authorizer, root) = make_handler();
        assert!(matches!(
            handler.handle(IpcRequest::GetStatus),
            IpcResponse::Status {
                status: HostStatus {
                    hosting_enabled: true,
                    helper_state: HelperState::Running,
                    connected_viewers: 0,
                    pending_approvals: 0,
                    screen_recording_granted: Some(_),
                    accessibility_granted: Some(_),
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
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn allowlist_requests_mutate_shared_authorizer_and_emit_pending_diffs() {
        let (mut handler, authorizer, root) = make_handler();
        {
            let mut authorizer = lock(&authorizer);
            assert!(authorizer
                .seed_peer_for_test(&identity("pending-node", "Pending PC"), false, 10)
                .is_ok());
            assert!(authorizer
                .seed_peer_for_test(&identity("approved-node", "Approved PC"), true, 11)
                .is_ok());
        }
        assert!(matches!(
            handler.handle(IpcRequest::ListAllowlist),
            IpcResponse::Allowlist { peers }
                if peers.len() == 1 && peers[0].node_id == "approved-node"
        ));
        assert!(matches!(
            handler.handle(IpcRequest::SubscribePending),
            IpcResponse::PendingSnapshot { peers }
                if peers.len() == 1 && peers[0].node_id == "pending-node"
        ));
        {
            let mut authorizer = lock(&authorizer);
            assert!(authorizer
                .seed_peer_for_test(&identity("new-node", "New PC"), false, 12)
                .is_ok());
        }
        assert!(matches!(
            handler.next_pending_event(Duration::ZERO),
            Some(IpcEvent::PendingAdded { peer }) if peer.node_id == "new-node"
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
            handler.handle(IpcRequest::Reject {
                node_id: "new-node".to_owned()
            }),
            IpcResponse::ActionApplied {
                action: PeerAction::Reject,
                node_id: "new-node".to_owned()
            }
        );
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
            handler.handle(IpcRequest::Remove {
                node_id: "missing-node".to_owned()
            }),
            peer_not_found()
        );
        drop(handler);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_ipc_lists_fail_the_text_or_frame_bounds() {
        let oversized_entry = AllowlistEntry {
            node_id: "x".repeat(MAX_IPC_TEXT_BYTES + 1),
            display_name: "Test peer".to_owned(),
            login_name: String::new(),
        };
        assert!(!valid_allowlist_texts(&[oversized_entry]));

        let peers = (0..MAX_PEER_LIST_ENTRIES)
            .map(|_| AllowlistEntry {
                node_id: "n".repeat(MAX_IPC_TEXT_BYTES),
                display_name: "d".repeat(MAX_IPC_TEXT_BYTES),
                login_name: "l".repeat(MAX_IPC_TEXT_BYTES),
            })
            .collect();
        let response = IpcResponse::Allowlist { peers };
        assert!(!response_fits_frame(&response));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicU64;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_root() -> PathBuf {
        let path = PathBuf::from(format!(
            "/tmp/rc-ipc-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap_or_else(|error| panic!("create temp root: {error}"));
        path
    }

    fn make_support_tree(root: &Path) -> PathBuf {
        let home = root.join("home");
        let library = home.join("Library");
        let support = library.join("Application Support");
        fs::create_dir_all(&support).unwrap_or_else(|error| panic!("create support tree: {error}"));
        support
    }

    #[test]
    fn app_directory_is_created_private_and_owned() {
        let root = temp_root();
        let support = make_support_tree(&root);
        let app_dir = support.join("RaccConnect");
        let socket = app_dir.join("host-agent.sock");
        ensure_private_app_directory(&socket)
            .unwrap_or_else(|error| panic!("secure app dir: {error}"));
        let metadata =
            fs::symlink_metadata(&app_dir).unwrap_or_else(|error| panic!("stat app dir: {error}"));
        assert_eq!(metadata.uid(), effective_uid());
        assert_eq!(metadata.permissions().mode() & 0o777, APP_DIRECTORY_MODE);
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("remove temp root: {error}"));
    }

    #[test]
    fn stale_owned_socket_is_removed_but_active_socket_is_preserved() {
        let root = temp_root();
        let support = make_support_tree(&root);
        let socket = support.join("stale.sock");
        let listener =
            UnixListener::bind(&socket).unwrap_or_else(|error| panic!("bind temp socket: {error}"));
        drop(listener);
        remove_stale_owned_socket(&socket, effective_uid())
            .unwrap_or_else(|error| panic!("remove stale: {error}"));
        assert!(!socket.exists());

        let active = support.join("active.sock");
        let listener = UnixListener::bind(&active)
            .unwrap_or_else(|error| panic!("bind active socket: {error}"));
        fs::set_permissions(&active, Permissions::from_mode(SOCKET_MODE))
            .unwrap_or_else(|error| panic!("chmod active socket: {error}"));
        let error = remove_stale_owned_socket(&active, effective_uid())
            .expect_err("active socket must remain");
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        drop(listener);
        fs::remove_file(active).unwrap_or_else(|error| panic!("remove active socket: {error}"));
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("remove temp root: {error}"));
    }

    #[test]
    fn refuses_regular_file_at_socket_path_and_wrong_owner_metadata() {
        let root = temp_root();
        let socket = root.join("not-a-socket");
        fs::write(&socket, b"keep").unwrap_or_else(|error| panic!("write sentinel: {error}"));
        let error =
            remove_stale_owned_socket(&socket, effective_uid()).expect_err("regular file rejected");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&socket).unwrap_or_default(), b"keep");

        let listener_path = root.join("socket");
        let listener = UnixListener::bind(&listener_path)
            .unwrap_or_else(|error| panic!("bind socket: {error}"));
        let metadata = fs::symlink_metadata(&listener_path)
            .unwrap_or_else(|error| panic!("stat socket: {error}"));
        let wrong_owner = effective_uid().wrapping_add(1);
        let error = validate_socket_metadata(&metadata, wrong_owner, None)
            .expect_err("foreign owner rejected");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        drop(listener);
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("remove temp root: {error}"));
    }

    #[test]
    fn accepts_only_same_user_socket_peers() {
        let (server, _client) = UnixStream::pair().expect("create Unix socket pair");
        assert!(validate_peer_owner(&server, effective_uid() as u32).is_ok());
        assert_eq!(
            validate_peer_owner(&server, effective_uid().wrapping_add(1) as u32)
                .expect_err("foreign peer rejected")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
