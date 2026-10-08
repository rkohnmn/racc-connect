//! Single-instance ownership and local show-window handoff.

use std::{
    collections::hash_map::DefaultHasher,
    fs::{File, OpenOptions},
    hash::{Hash, Hasher},
    io,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

/// Cloneable receiver for show-window requests from another app process.
#[derive(Clone)]
pub struct ShowRequests(Arc<Mutex<Receiver<()>>>);

impl ShowRequests {
    /// Returns whether at least one process asked the current app to come forward.
    pub fn take_pending(&self) -> bool {
        let Ok(receiver) = self.0.lock() else {
            return false;
        };
        let mut pending = false;
        loop {
            match receiver.try_recv() {
                Ok(()) => pending = true,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return pending,
            }
        }
    }
}

/// Owns the per-user app lock and local handoff listener.
pub struct SingleInstance {
    _lock_file: File,
    requests: ShowRequests,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SingleInstance {
    /// Acquires single-instance ownership, or signals the existing process to show its window.
    ///
    /// Ok(None) means a show request was delivered to the already-running process.
    pub fn acquire(settings_path: &Path) -> io::Result<Option<Self>> {
        let parent = settings_path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let lock_path = parent.join("racc-connect.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;

        match lock_file.try_lock() {
            Ok(()) => {
                let (sender, receiver) = mpsc::channel();
                let shutdown = Arc::new(AtomicBool::new(false));
                let worker = platform::start_listener(settings_path, sender, shutdown.clone())?;
                Ok(Some(Self {
                    _lock_file: lock_file,
                    requests: ShowRequests(Arc::new(Mutex::new(receiver))),
                    shutdown,
                    worker: Some(worker),
                }))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if platform::request_show(settings_path)? {
                    Ok(None)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "the existing app process did not accept a show-window request",
                    ))
                }
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    /// Returns a cloneable handle the UI can poll for show-window requests.
    pub fn show_requests(&self) -> ShowRequests {
        self.requests.clone()
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // The lock file is dropped after this method, so another process cannot take ownership
        // before the local handoff endpoint has shut down.
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::{
        ffi::OsStr,
        os::windows::ffi::OsStrExt,
        ptr::{null, null_mut},
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, GetLastError, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING,
            GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
        },
        Storage::FileSystem::{
            CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING,
            PIPE_ACCESS_INBOUND,
        },
        System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_NOWAIT,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        },
    };

    const SHOW_WINDOW: u8 = 1;

    pub(super) fn start_listener(
        settings_path: &Path,
        sender: Sender<()>,
        shutdown: Arc<AtomicBool>,
    ) -> io::Result<JoinHandle<()>> {
        let name = pipe_name(settings_path);
        // SAFETY: name is a NUL-terminated UTF-16 pipe path; null security attributes request
        // the Windows default ACL. The pipe rejects remote clients and carries only a show signal.
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                8,
                0,
                null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        // HANDLE is transferred to exactly one worker, which closes it after the polling loop.
        let raw_pipe = pipe as usize;
        Ok(thread::spawn(move || {
            let pipe = raw_pipe as HANDLE;
            while !shutdown.load(Ordering::Acquire) {
                // SAFETY: pipe is the live, exclusively owned named-pipe handle created above.
                let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) } != 0;
                if connected {
                    // In PIPE_NOWAIT mode, success only makes a disconnected instance
                    // available. A client is connected only after ERROR_PIPE_CONNECTED.
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                // SAFETY: GetLastError reads the calling thread's Win32 error state.
                let error = unsafe { GetLastError() };
                if error == ERROR_PIPE_CONNECTED {
                    loop {
                        if shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        let mut command = 0_u8;
                        let mut read = 0_u32;
                        // SAFETY: pipe is connected and command/read are valid writable buffers.
                        let result = unsafe {
                            ReadFile(
                                pipe,
                                (&mut command as *mut u8).cast(),
                                1,
                                &mut read,
                                null_mut(),
                            )
                        };
                        if result != 0 && read == 1 {
                            if command == SHOW_WINDOW {
                                let _ = sender.send(());
                            }
                            break;
                        }
                        // SAFETY: GetLastError reads the calling thread's Win32 error state.
                        let read_error = unsafe { GetLastError() };
                        if read_error == ERROR_NO_DATA {
                            thread::sleep(Duration::from_millis(10));
                        } else {
                            break;
                        }
                    }
                    // SAFETY: pipe is the live server handle; disconnect prepares it for reuse.
                    let _ = unsafe { DisconnectNamedPipe(pipe) };
                    continue;
                }
                if error == ERROR_NO_DATA {
                    // The client may write its one-byte request and close before this
                    // nonblocking listener observes ERROR_PIPE_CONNECTED. Drain any
                    // buffered request before disconnecting this completed instance.
                    let mut command = 0_u8;
                    let mut read = 0_u32;
                    // SAFETY: pipe is the live server handle; command/read are writable buffers.
                    let received = unsafe {
                        ReadFile(
                            pipe,
                            (&mut command as *mut u8).cast(),
                            1,
                            &mut read,
                            null_mut(),
                        )
                    } != 0
                        && read == 1;
                    if received && command == SHOW_WINDOW {
                        let _ = sender.send(());
                    }
                    // SAFETY: pipe is the live server handle exclusively owned by this worker.
                    let _ = unsafe { DisconnectNamedPipe(pipe) };
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                if error != ERROR_PIPE_LISTENING {
                    eprintln!("single-instance pipe listener stopped after ConnectNamedPipe error {error}");
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            // SAFETY: this worker exclusively owns the handle and closes it exactly once.
            let _ = unsafe { CloseHandle(pipe) };
        }))
    }

    pub(super) fn request_show(settings_path: &Path) -> io::Result<bool> {
        let name = pipe_name(settings_path);
        let mut last_error = 0;
        for _ in 0..40 {
            // SAFETY: name is a NUL-terminated UTF-16 pipe name. The client requests write-only
            // access to the local endpoint; no network interface is involved.
            let pipe = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    null(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                )
            };
            if pipe != INVALID_HANDLE_VALUE {
                let command = SHOW_WINDOW;
                let mut written = 0_u32;
                // SAFETY: pipe is a valid client handle and command/written are valid buffers.
                let write_succeeded = unsafe {
                    WriteFile(
                        pipe,
                        (&command as *const u8).cast(),
                        1,
                        &mut written,
                        null_mut(),
                    )
                } != 0;
                // SAFETY: Capture this thread's WriteFile failure before CloseHandle can change it.
                let write_error = if write_succeeded {
                    0
                } else {
                    unsafe { GetLastError() }
                };
                // SAFETY: this client owns the handle returned by CreateFileW.
                let _ = unsafe { CloseHandle(pipe) };
                if write_succeeded && written == 1 {
                    return Ok(true);
                }
                if write_succeeded {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "short write while sending show-window request",
                    ));
                }
                return Err(io::Error::from_raw_os_error(write_error as i32));
            }
            // SAFETY: GetLastError reads the current thread's CreateFileW failure status.
            last_error = unsafe { GetLastError() };
            thread::sleep(Duration::from_millis(25));
        }
        Err(io::Error::from_raw_os_error(last_error as i32))
    }

    fn pipe_name(settings_path: &Path) -> Vec<u16> {
        let mut hasher = DefaultHasher::new();
        settings_path
            .to_string_lossy()
            .to_lowercase()
            .hash(&mut hasher);
        let name = format!(r"\\.\pipe\LOCAL\racc-connect-{:016x}", hasher.finish());
        OsStr::new(&name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
        time::{Duration, Instant},
    };

    const SHOW_WINDOW: u8 = 1;

    pub(super) fn start_listener(
        settings_path: &Path,
        sender: Sender<()>,
        shutdown: Arc<AtomicBool>,
    ) -> io::Result<JoinHandle<()>> {
        let socket_path = socket_path(settings_path);
        remove_stale_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        if let Err(error) =
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(&socket_path);
            return Err(error);
        }
        if let Err(error) = listener.set_nonblocking(true) {
            let _ = std::fs::remove_file(&socket_path);
            return Err(error);
        }

        Ok(thread::spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => handle_client(&mut stream, &sender, &shutdown),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
            if std::fs::symlink_metadata(&socket_path)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
            {
                let _ = std::fs::remove_file(socket_path);
            }
        }))
    }

    pub(super) fn request_show(settings_path: &Path) -> io::Result<bool> {
        let socket_path = socket_path(settings_path);
        for _ in 0..40 {
            match UnixStream::connect(&socket_path) {
                Ok(mut stream) => return stream.write_all(&[SHOW_WINDOW]).map(|()| true),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    }

    fn handle_client(stream: &mut UnixStream, sender: &Sender<()>, shutdown: &AtomicBool) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(250);
        while !shutdown.load(Ordering::Acquire) && Instant::now() < deadline {
            let mut command = [0_u8; 1];
            match stream.read(&mut command) {
                Ok(1) if command[0] == SHOW_WINDOW => {
                    let _ = sender.send(());
                    return;
                }
                Ok(0) | Ok(_) => return,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
    }

    fn remove_stale_socket(path: &Path) -> io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "single-instance endpoint exists and is not a socket",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn socket_path(settings_path: &Path) -> std::path::PathBuf {
        let mut hasher = DefaultHasher::new();
        settings_path.to_string_lossy().hash(&mut hasher);
        settings_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("racc-connect-{:016x}.sock", hasher.finish()))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::*;

    pub(super) fn start_listener(
        _settings_path: &Path,
        _sender: Sender<()>,
        _shutdown: Arc<AtomicBool>,
    ) -> io::Result<JoinHandle<()>> {
        Ok(thread::spawn(|| {}))
    }

    pub(super) fn request_show(_settings_path: &Path) -> io::Result<bool> {
        Ok(false)
    }
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn temporary_settings_path() -> (PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("racc-instance-{}-{nonce}", std::process::id()));
        (directory.clone(), directory.join("settings.json"))
    }

    #[test]
    fn second_launch_signals_existing_and_does_not_take_ownership() {
        let (directory, settings_path) = temporary_settings_path();
        let first = SingleInstance::acquire(&settings_path)
            .expect("start primary instance")
            .expect("first process owns the app");

        let second = SingleInstance::acquire(&settings_path).expect("signal existing process");
        assert!(second.is_none());

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut show_requested = false;
        while !show_requested && std::time::Instant::now() < deadline {
            show_requested = first.show_requests().take_pending();
            if !show_requested {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(show_requested, "primary process receives the show request");

        assert!(platform::request_show(&settings_path).expect("send another show request"));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut second_show_requested = false;
        while !second_show_requested && std::time::Instant::now() < deadline {
            second_show_requested = first.show_requests().take_pending();
            if !second_show_requested {
                thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(
            second_show_requested,
            "primary process receives a show request after reusing the pipe"
        );

        drop(first);
        std::fs::remove_dir_all(directory).expect("remove temporary test directory");
    }

    #[test]
    fn show_requests_coalesce_without_losing_a_pending_restore() {
        let (sender, receiver) = mpsc::channel();
        let requests = ShowRequests(Arc::new(Mutex::new(receiver)));
        sender.send(()).expect("send request");
        sender.send(()).expect("send request");
        assert!(requests.take_pending());
        assert!(!requests.take_pending());
    }
}
