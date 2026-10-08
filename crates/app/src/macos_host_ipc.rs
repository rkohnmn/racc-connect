//! App-side connector for the macOS per-user host-agent Unix socket.
//!
//! Keep `HOST_SOCKET_RELATIVE_PATH` byte-for-byte identical to the constant in
//! `crates/host-agent/src/macos/local_ipc.rs`. This crate cannot depend on the
//! host-agent binary crate.

use racc_core::ipc::IpcClient;
use std::fs::{self, FileType, Metadata};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// Application Support path shared with the host-agent listener.
pub const HOST_SOCKET_RELATIVE_PATH: &str =
    "Library/Application Support/RaccConnect/host-agent.sock";
/// Required directory mode for local IPC.
pub const APP_DIRECTORY_MODE: u32 = 0o700;
/// Required socket-node mode for local IPC.
pub const SOCKET_MODE: u32 = 0o600;
const MACOS_SUN_PATH_CAPACITY: usize = 104;

/// Connects to the current user's host-agent using verified path and owner metadata.
pub fn connect_local_host_agent() -> io::Result<IpcClient<UnixStream>> {
    let socket_path = default_socket_path()?;
    connect_at(&socket_path).map(IpcClient::new)
}

/// Returns the per-user Application Support socket path.
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

fn connect_at(socket_path: &Path) -> io::Result<UnixStream> {
    check_socket_path_length(socket_path)?;
    let uid = effective_uid() as u32;
    validate_home_chain(socket_path, uid)?;
    let metadata = fs::symlink_metadata(socket_path)?;
    validate_socket_metadata(&metadata, uid, Some(SOCKET_MODE))?;
    let identity = (metadata.dev(), metadata.ino());
    let stream = UnixStream::connect(socket_path)?;
    validate_peer_owner(&stream, uid)?;
    let post_connect = fs::symlink_metadata(socket_path)?;
    validate_socket_metadata(&post_connect, uid, Some(SOCKET_MODE))?;
    if (post_connect.dev(), post_connect.ino()) != identity {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "host-agent socket changed during connect",
        ));
    }
    Ok(stream)
}

fn validate_home_chain(socket_path: &Path, uid: u32) -> io::Result<()> {
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
            io::ErrorKind::InvalidInput,
            "local IPC path is not a Unix-domain socket",
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
            "local IPC server is owned by another user",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn connector_verifies_private_directory_and_socket_before_client_exchange() {
        let root = PathBuf::from(format!("/tmp/rc-app-ipc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let support = home.join("Library/Application Support");
        let app_dir = support.join("RaccConnect");
        fs::create_dir_all(&app_dir)
            .unwrap_or_else(|error| panic!("create app directory: {error}"));
        fs::set_permissions(&app_dir, fs::Permissions::from_mode(APP_DIRECTORY_MODE))
            .unwrap_or_else(|error| panic!("secure app directory: {error}"));
        let socket_path = app_dir.join("host-agent.sock");
        let listener = UnixListener::bind(&socket_path)
            .unwrap_or_else(|error| panic!("bind test socket: {error}"));
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(SOCKET_MODE))
            .unwrap_or_else(|error| panic!("secure test socket: {error}"));
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .unwrap_or_else(|error| panic!("accept client: {error}"));
            use std::io::{Read, Write};
            let mut prefix = [0; 4];
            stream
                .read_exact(&mut prefix)
                .unwrap_or_else(|error| panic!("read request length: {error}"));
            let len = u32::from_le_bytes(prefix) as usize;
            let mut request = vec![0; len];
            stream
                .read_exact(&mut request)
                .unwrap_or_else(|error| panic!("read request: {error}"));
            let body = br#"{"message":"response","body":{"type":"status","status":{"hosting_enabled":true,"helper_state":"running","connected_viewers":0,"pending_approvals":0}}}"#;
            stream
                .write_all(&(body.len() as u32).to_le_bytes())
                .unwrap_or_else(|error| panic!("write response length: {error}"));
            stream
                .write_all(body)
                .unwrap_or_else(|error| panic!("write response: {error}"));
        });
        let client_stream =
            connect_at(&socket_path).unwrap_or_else(|error| panic!("connect local IPC: {error}"));
        let mut client = IpcClient::new(client_stream);
        let response = client.request(racc_core::ipc::IpcRequest::GetStatus);
        assert!(matches!(
            response,
            Ok(racc_core::ipc::IpcResponse::Status { .. })
        ));
        assert!(server.join().is_ok());
        let _ = fs::remove_dir_all(root);
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

    #[test]
    fn connector_rejects_regular_file_and_unsafe_modes() {
        let root = PathBuf::from(format!("/tmp/rc-app-check-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let app_dir = home.join("Library/Application Support/RaccConnect");
        fs::create_dir_all(&app_dir)
            .unwrap_or_else(|error| panic!("create app directory: {error}"));
        fs::set_permissions(&app_dir, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("make app directory insecure: {error}"));
        let socket_path = app_dir.join("host-agent.sock");
        assert_eq!(
            connect_at(&socket_path)
                .expect_err("insecure directory rejected")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::set_permissions(&app_dir, fs::Permissions::from_mode(APP_DIRECTORY_MODE))
            .unwrap_or_else(|error| panic!("secure app directory: {error}"));
        fs::write(&socket_path, b"unchanged")
            .unwrap_or_else(|error| panic!("create sentinel: {error}"));
        assert_eq!(
            connect_at(&socket_path)
                .expect_err("regular file rejected")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read(&socket_path).unwrap_or_default(), b"unchanged");
        let _ = fs::remove_dir_all(root);
    }
}
