use crate::error::IdentityError;
use crate::model::{PeerIdentity, SelfNode, StatusSnapshot};
use crate::parse::{parse_status_json, parse_whois_json};
use racc_net::{validate_bind_addr, BindPolicy};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Default deadline for one Tailscale CLI invocation.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
/// Default cache lifetime for status output.
pub const DEFAULT_STATUS_TTL: Duration = Duration::from_secs(1);
/// Default cache lifetime for whois output.
pub const DEFAULT_WHOIS_TTL: Duration = Duration::from_secs(1);
/// Hard maximum output bytes retained from one CLI invocation.
pub const MAX_COMMAND_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_WHOIS_CACHE_ENTRIES: usize = 256;
const PIPE_QUEUE_CHUNKS: usize = 8;
const PIPE_CHUNK_BYTES: usize = 8192;

/// Configures Tailscale process bounds and short-lived caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CliConfig {
    /// Maximum wall-clock duration for one CLI invocation.
    pub command_timeout: Duration,
    /// Maximum combined stdout and stderr retained from one invocation.
    pub max_output_bytes: usize,
    /// Status cache lifetime.
    pub status_ttl: Duration,
    /// Per-address whois cache lifetime.
    pub whois_ttl: Duration,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            max_output_bytes: MAX_COMMAND_OUTPUT_BYTES,
            status_ttl: DEFAULT_STATUS_TTL,
            whois_ttl: DEFAULT_WHOIS_TTL,
        }
    }
}

/// One direct executable invocation; no shell command or interpolated script is represented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRequest {
    /// Executable path resolved by the caller.
    pub executable: PathBuf,
    /// Separate command arguments passed directly to the process.
    pub args: Vec<OsString>,
    /// Explicit minimal environment additions required by the platform.
    pub environment: Vec<(OsString, OsString)>,
}

impl ProcessRequest {
    /// Creates a request with no environment additions.
    pub fn new(executable: PathBuf, args: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            executable,
            args: args.into_iter().collect(),
            environment: Vec::new(),
        }
    }
}

/// Bounded output from one completed process invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    /// Exit code, or None when the process did not exit normally.
    pub exit_code: Option<i32>,
    /// Captured standard output, at most the request's output limit.
    pub stdout: Vec<u8>,
    /// Captured standard error, at most the request's output limit.
    pub stderr: Vec<u8>,
}

/// Failure from the bounded process runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessFailure {
    /// Executable could not be found.
    NotInstalled,
    /// Process exceeded its wall-clock deadline.
    Timeout,
    /// Combined output exceeded the configured cap.
    OutputTooLarge,
    /// Process creation or pipe I/O failed.
    Io(String),
}

/// Injectable boundary for the Tailscale CLI process.
pub trait ProcessRunner: Send + Sync {
    /// Executes the direct process request under timeout and output bounds.
    fn run(
        &self,
        request: &ProcessRequest,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<ProcessOutput, ProcessFailure>;
}

/// Production runner that starts an executable directly, without a shell.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn run(
        &self,
        request: &ProcessRequest,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<ProcessOutput, ProcessFailure> {
        let output_cap = max_output_bytes.min(MAX_COMMAND_OUTPUT_BYTES);
        let mut command = Command::new(&request.executable);
        command
            .args(&request.args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        for name in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for (name, value) in &request.environment {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ProcessFailure::NotInstalled
            } else {
                ProcessFailure::Io(error.kind().to_string())
            }
        })?;
        let stdout = match child.stdout.take() {
            Some(pipe) => pipe,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessFailure::Io("stdout pipe was not created".to_owned()));
            }
        };
        let stderr = match child.stderr.take() {
            Some(pipe) => pipe,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProcessFailure::Io("stderr pipe was not created".to_owned()));
            }
        };
        let (sender, receiver) = mpsc::sync_channel(PIPE_QUEUE_CHUNKS);
        let stdout_thread = spawn_pipe_reader(stdout, false, sender.clone());
        let stderr_thread = spawn_pipe_reader(stderr, true, sender.clone());
        drop(sender);

        let started = Instant::now();
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let mut total = 0_usize;
        let exit_status;
        loop {
            if let Err(error) = drain_pipe_chunks(
                &receiver,
                &mut stdout_bytes,
                &mut stderr_bytes,
                &mut total,
                output_cap,
            ) {
                cleanup_process(&mut child, receiver, stdout_thread, stderr_thread);
                return Err(error);
            }
            if started.elapsed() >= timeout {
                cleanup_process(&mut child, receiver, stdout_thread, stderr_thread);
                return Err(ProcessFailure::Timeout);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_status = status;
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    cleanup_process(&mut child, receiver, stdout_thread, stderr_thread);
                    return Err(ProcessFailure::Io(error.kind().to_string()));
                }
            }
        }

        while let Ok(chunk) = receiver.recv() {
            if let Err(error) = append_chunk(
                chunk,
                &mut stdout_bytes,
                &mut stderr_bytes,
                &mut total,
                output_cap,
            ) {
                cleanup_process(&mut child, receiver, stdout_thread, stderr_thread);
                return Err(error);
            }
        }
        let stdout_join = stdout_thread.join();
        let stderr_join = stderr_thread.join();
        if stdout_join.is_err() || stderr_join.is_err() {
            return Err(ProcessFailure::Io("output reader thread failed".to_owned()));
        }
        Ok(ProcessOutput {
            exit_code: exit_status.code(),
            stdout: stdout_bytes,
            stderr: stderr_bytes,
        })
    }
}

fn cleanup_process(
    child: &mut Child,
    receiver: Receiver<PipeChunk>,
    stdout_thread: JoinHandle<()>,
    stderr_thread: JoinHandle<()>,
) {
    let _ = child.kill();
    let _ = child.wait();
    drop(receiver);
    let stdout_join = stdout_thread.join();
    let stderr_join = stderr_thread.join();
    let _ = (stdout_join, stderr_join);
}
enum PipeChunk {
    Data { stderr: bool, bytes: Vec<u8> },
    Failed(io::ErrorKind),
}

fn spawn_pipe_reader(
    mut pipe: impl Read + Send + 'static,
    stderr: bool,
    sender: SyncSender<PipeChunk>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; PIPE_CHUNK_BYTES];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(length) => {
                    if sender
                        .send(PipeChunk::Data {
                            stderr,
                            bytes: buffer[..length].to_vec(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(PipeChunk::Failed(error.kind()));
                    break;
                }
            }
        }
    })
}

fn drain_pipe_chunks(
    receiver: &Receiver<PipeChunk>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    total: &mut usize,
    cap: usize,
) -> Result<(), ProcessFailure> {
    loop {
        match receiver.try_recv() {
            Ok(chunk) => append_chunk(chunk, stdout, stderr, total, cap)?,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

fn append_chunk(
    chunk: PipeChunk,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    total: &mut usize,
    cap: usize,
) -> Result<(), ProcessFailure> {
    match chunk {
        PipeChunk::Failed(kind) => Err(ProcessFailure::Io(kind.to_string())),
        PipeChunk::Data {
            stderr: is_stderr,
            bytes,
        } => {
            let next = total
                .checked_add(bytes.len())
                .ok_or(ProcessFailure::OutputTooLarge)?;
            if next > cap {
                return Err(ProcessFailure::OutputTooLarge);
            }
            if is_stderr {
                stderr.extend_from_slice(&bytes);
            } else {
                stdout.extend_from_slice(&bytes);
            }
            *total = next;
            Ok(())
        }
    }
}

/// Locates the Tailscale CLI through PATH and documented standard locations.
pub fn locate_tailscale_cli() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["tailscale.exe", "tailscale"]
    } else {
        &["tailscale"]
    };
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            for name in names {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    #[cfg(windows)]
    {
        for variable in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(variable) {
                let candidate = PathBuf::from(root).join("Tailscale").join("tailscale.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for candidate in [
            PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"),
            PathBuf::from("/usr/local/bin/tailscale"),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        for candidate in [
            PathBuf::from("/usr/bin/tailscale"),
            PathBuf::from("/usr/sbin/tailscale"),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// CLI-first Tailscale status and whois client with bounded output and short caches.
pub struct TailscaleClient<R: ProcessRunner = SystemProcessRunner> {
    executable: Option<PathBuf>,
    runner: R,
    config: CliConfig,
    status_cache: Mutex<Option<(Instant, StatusSnapshot)>>,
    whois_cache: Mutex<HashMap<IpAddrKey, (Instant, PeerIdentity)>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct IpAddrKey(std::net::IpAddr);

impl TailscaleClient<SystemProcessRunner> {
    /// Creates a production client and resolves the installed CLI path once.
    pub fn system() -> Self {
        Self::discover(SystemProcessRunner, CliConfig::default())
    }
}

impl<R: ProcessRunner> TailscaleClient<R> {
    /// Creates a client using PATH and platform-standard CLI locations.
    pub fn discover(runner: R, config: CliConfig) -> Self {
        Self::with_optional_executable(locate_tailscale_cli(), runner, config)
    }

    /// Creates a client using an explicit executable path, useful for tests.
    pub fn with_executable(executable: PathBuf, runner: R, config: CliConfig) -> Self {
        Self::with_optional_executable(Some(executable), runner, config)
    }

    fn with_optional_executable(executable: Option<PathBuf>, runner: R, config: CliConfig) -> Self {
        Self {
            executable,
            runner,
            config: CliConfig {
                command_timeout: config.command_timeout.min(Duration::from_secs(30)),
                max_output_bytes: config.max_output_bytes.min(MAX_COMMAND_OUTPUT_BYTES),
                status_ttl: config.status_ttl,
                whois_ttl: config.whois_ttl,
            },
            status_cache: Mutex::new(None),
            whois_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the path selected for the Tailscale CLI, if one was found.
    pub fn executable(&self) -> Option<&Path> {
        self.executable.as_deref()
    }

    /// Runs and parses status output, caching a successful result for the configured TTL.
    pub fn status(&self) -> Result<StatusSnapshot, IdentityError> {
        let now = Instant::now();
        let mut cache = self
            .status_cache
            .lock()
            .map_err(|_| IdentityError::Io("status cache lock poisoned".to_owned()))?;
        if let Some((timestamp, value)) = cache.as_ref() {
            if now.saturating_duration_since(*timestamp) < self.config.status_ttl {
                return Ok(value.clone());
            }
        }
        let output = self.invoke(&["status", "--json"])?;
        let parsed = parse_status_json(&output.stdout)?;
        *cache = Some((Instant::now(), parsed.clone()));
        Ok(parsed)
    }

    /// Returns the local node portion of the cached status response, if present.
    pub fn self_node(&self) -> Result<Option<SelfNode>, IdentityError> {
        Ok(self.status()?.self_node)
    }

    /// Returns the current peer list.
    pub fn peers(&self) -> Result<Vec<crate::model::Peer>, IdentityError> {
        Ok(self.status()?.peers)
    }

    /// Runs whois for a tailnet address, with a short per-address cache.
    pub fn resolve(&self, remote_ip: std::net::IpAddr) -> Result<PeerIdentity, IdentityError> {
        validate_bind_addr(remote_ip, BindPolicy::Tailscale)
            .map_err(|_| IdentityError::AddressNotTailscale)?;
        let now = Instant::now();
        let key = IpAddrKey(remote_ip);
        let mut cache = self
            .whois_cache
            .lock()
            .map_err(|_| IdentityError::Io("whois cache lock poisoned".to_owned()))?;
        if let Some((timestamp, value)) = cache.get(&key) {
            if now.saturating_duration_since(*timestamp) < self.config.whois_ttl {
                return Ok(value.clone());
            }
        }
        let address = remote_ip.to_string();
        let output = self.invoke(&["whois", "--json", address.as_str()])?;
        let parsed = parse_whois_json(&output.stdout)?;
        cache.retain(|_, (timestamp, _)| {
            now.saturating_duration_since(*timestamp) < self.config.whois_ttl
        });
        if cache.len() >= MAX_WHOIS_CACHE_ENTRIES {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, (timestamp, _))| *timestamp)
                .map(|(key, _)| *key)
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Instant::now(), parsed.clone()));
        Ok(parsed)
    }

    /// Returns a production-policy Tailscale bind address, preferring IPv4.
    pub fn self_bind_addr(&self) -> Result<std::net::IpAddr, IdentityError> {
        for family in ["-4", "-6"] {
            if let Ok(output) = self.invoke(&["ip", family]) {
                if let Ok(text) = std::str::from_utf8(&output.stdout) {
                    for candidate in text.split_whitespace() {
                        if let Ok(address) = candidate.parse::<std::net::IpAddr>() {
                            if validate_bind_addr(address, BindPolicy::Tailscale).is_ok()
                                && ((family == "-4") == address.is_ipv4())
                            {
                                return Ok(address);
                            }
                        }
                    }
                }
            }
        }
        if let Some(node) = self.status()?.self_node {
            if let Some(address) = node
                .ipv4
                .filter(|address| validate_bind_addr(*address, BindPolicy::Tailscale).is_ok())
                .or_else(|| {
                    node.ipv6.filter(|address| {
                        validate_bind_addr(*address, BindPolicy::Tailscale).is_ok()
                    })
                })
            {
                return Ok(address);
            }
        }
        Err(IdentityError::NoTailscaleAddress)
    }

    /// Returns the Tailscale CLI version string, if the command succeeds.
    pub fn version(&self) -> Result<String, IdentityError> {
        let output = self.invoke(&["version"])?;
        let text = std::str::from_utf8(&output.stdout).map_err(|_| IdentityError::BadJson)?;
        Ok(text.lines().next().unwrap_or_default().trim().to_owned())
    }

    fn invoke(&self, arguments: &[&str]) -> Result<ProcessOutput, IdentityError> {
        let executable = self.executable.clone().ok_or(IdentityError::NotInstalled)?;
        let request = ProcessRequest::new(executable, arguments.iter().map(OsString::from));
        #[cfg(target_os = "macos")]
        let request = {
            let mut request = request;
            request
                .environment
                .push((OsString::from("TAILSCALE_BE_CLI"), OsString::from("1")));
            request
        };
        let output = self
            .runner
            .run(
                &request,
                self.config.command_timeout,
                self.config.max_output_bytes,
            )
            .map_err(map_process_failure)?;
        if output.exit_code.unwrap_or(-1) != 0 {
            return Err(classify_command_failure(&output));
        }
        Ok(output)
    }
}

fn map_process_failure(error: ProcessFailure) -> IdentityError {
    match error {
        ProcessFailure::NotInstalled => IdentityError::NotInstalled,
        ProcessFailure::Timeout => IdentityError::Timeout,
        ProcessFailure::OutputTooLarge => IdentityError::OutputTooLarge,
        ProcessFailure::Io(message) => IdentityError::Io(message),
    }
}

fn classify_command_failure(output: &ProcessOutput) -> IdentityError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let normalized = stderr.to_ascii_lowercase();
    if normalized.contains("needs login")
        || normalized.contains("logged out")
        || normalized.contains("not logged in")
    {
        IdentityError::NotLoggedIn
    } else if normalized.contains("not running")
        || normalized.contains("backend stopped")
        || normalized.contains("cannot connect to local tailscaled")
    {
        IdentityError::NotRunning
    } else {
        IdentityError::CommandFailed(output.exit_code.unwrap_or(-1))
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn process_runner_child_behavior() {
        match std::env::var("RACC_IDENTITY_RUNNER_CHILD").ok().as_deref() {
            Some("sleep") => thread::sleep(Duration::from_secs(10)),
            Some("large") => {
                let output = "x".repeat(64 * 1024);
                let _ = io::stdout().write_all(output.as_bytes());
                let _ = io::stdout().flush();
                thread::sleep(Duration::from_secs(10));
            }
            _ => {}
        }
    }

    fn child_request(mode: &str) -> ProcessRequest {
        let executable =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("missing-test-exe"));
        let mut request = ProcessRequest::new(
            executable,
            [
                OsString::from("--exact"),
                OsString::from("client::process_tests::process_runner_child_behavior"),
                OsString::from("--nocapture"),
            ],
        );
        request.environment.push((
            OsString::from("RACC_IDENTITY_RUNNER_CHILD"),
            OsString::from(mode),
        ));
        request
    }

    #[test]
    fn system_runner_kills_and_reaps_a_timed_out_process() {
        let started = Instant::now();
        let result =
            SystemProcessRunner.run(&child_request("sleep"), Duration::from_millis(30), 1024);
        assert_eq!(result, Err(ProcessFailure::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn system_runner_stops_process_when_combined_output_exceeds_cap() {
        let started = Instant::now();
        let result = SystemProcessRunner.run(&child_request("large"), Duration::from_secs(2), 2048);
        assert_eq!(result, Err(ProcessFailure::OutputTooLarge));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn command_request_keeps_arguments_separate_and_has_no_shell_field() {
        let request = ProcessRequest::new(
            PathBuf::from("tailscale.exe"),
            [
                OsString::from("whois"),
                OsString::from("--json"),
                OsString::from("100.64.0.1"),
            ],
        );
        assert_eq!(request.args.len(), 3);
        assert_eq!(request.args[0], std::ffi::OsStr::new("whois"));
        assert_eq!(request.args[1], std::ffi::OsStr::new("--json"));
        assert_eq!(request.args[2], std::ffi::OsStr::new("100.64.0.1"));
    }

    #[test]
    fn system_runner_reports_missing_executable() {
        let request = ProcessRequest::new(
            PathBuf::from("__racc_identity_missing_executable__"),
            std::iter::empty(),
        );
        let result = SystemProcessRunner.run(&request, Duration::from_secs(1), 1024);
        assert!(matches!(result, Err(ProcessFailure::NotInstalled)));
    }

    #[test]
    fn command_failure_classifies_backend_states_without_echoing_output() {
        let output = ProcessOutput {
            exit_code: Some(1),
            stdout: Vec::new(),
            stderr: b"backend stopped".to_vec(),
        };
        assert_eq!(classify_command_failure(&output), IdentityError::NotRunning);
        let output = ProcessOutput {
            exit_code: Some(2),
            stdout: Vec::new(),
            stderr: b"needs login".to_vec(),
        };
        assert_eq!(
            classify_command_failure(&output),
            IdentityError::NotLoggedIn
        );
    }
}
