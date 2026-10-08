use crate::supervisor::{
    SessionChange, SessionId, Supervisor, SupervisorAction, SupervisorInput, SupervisorLog,
};
use ::windows::core::{PCWSTR, PWSTR};
use ::windows::Win32::Foundation::{
    CloseHandle, BOOL, HANDLE, HLOCAL, NO_ERROR, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use ::windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use ::windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use ::windows::Win32::Security::SECURITY_ATTRIBUTES;
use ::windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use ::windows::Win32::System::RemoteDesktop::{
    WTSGetActiveConsoleSessionId, WTSQueryUserToken, WTSSESSION_NOTIFICATION,
};
use ::windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE, SERVICE_CONTROL_SHUTDOWN,
    SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STOPPED,
    SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
use ::windows::Win32::System::Threading::{
    CreateEventW, CreateProcessAsUserW, GetExitCodeProcess, OpenEventW, SetEvent,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, STARTUPINFOW,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT,
    WTS_SESSION_LOCK, WTS_SESSION_LOGOFF, WTS_SESSION_LOGON, WTS_SESSION_UNLOCK,
};
use std::collections::VecDeque;
use std::error::Error;
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const SERVICE_NAME: &str = "RaccHostAgent";
const SERVICE_EVENT_QUEUE_CAPACITY: usize = 64;
const SERVICE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const SESSION_RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
const HELPER_STOP_GRACE: Duration = Duration::from_secs(5);
const INVALID_SESSION_ID: u32 = u32::MAX;
const EVENT_NAME_PREFIX: &str = "Global\\RaccHostStop-";
const EVENT_SECURITY_DESCRIPTOR: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x00100000;;;IU)";

/// Enters the Windows Service Control Manager dispatcher for the host service.
pub fn run_service_dispatcher() -> Result<(), Box<dyn Error>> {
    let mut name = wide_null(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: `name` and `table` remain alive and NUL-terminated for the duration
    // of the synchronous dispatcher call. The second entry is the required null sentinel.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr())? };
    Ok(())
}

/// Starts a bounded waiter for the service-owned event passed to a helper process.
pub fn start_stop_event_listener(event_name: &str) -> io::Result<Receiver<()>> {
    validate_stop_event_name(event_name)?;
    let event_name = event_name.to_owned();
    let (signal_tx, signal_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("racc-service-stop-event".to_owned())
        .spawn(move || {
            let event_name_wide = wide_null(&event_name);
            // SAFETY: The event name is NUL-terminated and came from our own service
            // command line. The ACL grants the interactive user synchronize-only access.
            let event = unsafe {
                OpenEventW(
                    SYNCHRONIZATION_SYNCHRONIZE,
                    BOOL(0),
                    PCWSTR(event_name_wide.as_ptr()),
                )
            };
            let event = match event {
                Ok(event) => OwnedHandle(event),
                Err(error) => {
                    let _ = signal_tx.send(());
                    service_debug(&format!("helper could not open its stop event: {error}"));
                    return;
                }
            };
            loop {
                // SAFETY: `event` is a valid owned event handle until this thread exits.
                let wait = unsafe { WaitForSingleObject(event.0, 250) };
                if wait == WAIT_OBJECT_0 {
                    let _ = signal_tx.send(());
                    return;
                }
                if wait == WAIT_TIMEOUT {
                    continue;
                }
                let _ = signal_tx.send(());
                service_debug("helper stop-event wait failed; requesting shutdown");
                return;
            }
        })?;
    Ok(signal_rx)
}

fn validate_stop_event_name(name: &str) -> io::Result<()> {
    let suffix = name.strip_prefix(EVENT_NAME_PREFIX).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid service stop event name",
        )
    })?;
    if suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid service stop event name",
        ));
    }
    Ok(())
}

#[derive(Debug)]
enum ServiceControl {
    Stop,
    Session(SessionChange),
}

struct HandlerContext {
    sender: SyncSender<ServiceControl>,
    queue_overflowed: Arc<AtomicBool>,
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let result = std::panic::catch_unwind(run_service_main);
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => service_debug(&format!("service adapter failed: {error}")),
        Err(_) => service_debug("service adapter panicked; caught at the FFI boundary"),
    }
}

fn run_service_main() -> Result<(), Box<dyn Error>> {
    let (sender, receiver) = mpsc::sync_channel(SERVICE_EVENT_QUEUE_CAPACITY);
    let queue_overflowed = Arc::new(AtomicBool::new(false));
    let context = Box::new(HandlerContext {
        sender,
        queue_overflowed: Arc::clone(&queue_overflowed),
    });
    let context_ptr = (&*context as *const HandlerContext).cast::<c_void>();
    let service_name = wide_null(SERVICE_NAME);
    // SAFETY: `service_name` is NUL-terminated. `context` stays alive until this
    // function exits, after the SCM dispatcher has stopped invoking its handler.
    let status_handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            PCWSTR(service_name.as_ptr()),
            Some(service_control_handler),
            Some(context_ptr),
        )?
    };
    let _status_guard = ServiceStatusGuard(status_handle);
    report_service_status(status_handle, SERVICE_START_PENDING, 0, 10_000)?;
    report_service_status(
        status_handle,
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE,
        0,
    )?;

    let started_at = Instant::now();
    let mut supervisor = Supervisor::new();
    let mut helper: Option<HelperProcess> = None;
    let first_session = active_console_session();
    let initial_actions = supervisor.handle(
        elapsed_ms(started_at),
        SupervisorInput::ServiceStarted {
            active_session: first_session,
        },
    );
    apply_actions(initial_actions, &mut supervisor, &mut helper, started_at)?;

    let mut last_session_reconcile = Instant::now();
    let mut stopping = false;
    while !stopping {
        if queue_overflowed.swap(false, Ordering::AcqRel) {
            service_debug(
                "SCM event queue overflowed; stopping the helper and service fail-closed",
            );
            let actions = supervisor.handle(
                elapsed_ms(started_at),
                SupervisorInput::ServiceStopRequested,
            );
            apply_actions(actions, &mut supervisor, &mut helper, started_at)?;
            stopping = true;
            continue;
        }

        match receiver.recv_timeout(SERVICE_POLL_INTERVAL) {
            Ok(ServiceControl::Stop) => {
                let actions = supervisor.handle(
                    elapsed_ms(started_at),
                    SupervisorInput::ServiceStopRequested,
                );
                apply_actions(actions, &mut supervisor, &mut helper, started_at)?;
                stopping = true;
            }
            Ok(ServiceControl::Session(change)) => {
                let actions = supervisor.handle(
                    elapsed_ms(started_at),
                    SupervisorInput::SessionChanged(change),
                );
                apply_actions(actions, &mut supervisor, &mut helper, started_at)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let actions = supervisor.handle(
                    elapsed_ms(started_at),
                    SupervisorInput::ServiceStopRequested,
                );
                apply_actions(actions, &mut supervisor, &mut helper, started_at)?;
                stopping = true;
            }
        }

        let helper_exit = match helper.as_mut() {
            Some(process) => process.poll_exit()?,
            None => None,
        };
        if let Some(exit_code) = helper_exit {
            if let Some(exited) = helper.take() {
                let generation = exited.generation;
                drop(exited);
                let actions = supervisor.handle(
                    elapsed_ms(started_at),
                    SupervisorInput::HelperExited {
                        generation,
                        exit_code,
                    },
                );
                apply_actions(actions, &mut supervisor, &mut helper, started_at)?;
            }
        }

        let actions = supervisor.handle(elapsed_ms(started_at), SupervisorInput::Tick);
        apply_actions(actions, &mut supervisor, &mut helper, started_at)?;

        if last_session_reconcile.elapsed() >= SESSION_RECONCILE_INTERVAL {
            last_session_reconcile = Instant::now();
            let active_session = active_console_session();
            if supervisor.desired_session() != active_session {
                let actions = supervisor.handle(
                    elapsed_ms(started_at),
                    SupervisorInput::SessionChanged(SessionChange::FastUserSwitch {
                        active_session,
                    }),
                );
                apply_actions(actions, &mut supervisor, &mut helper, started_at)?;
            }
        }
    }

    report_service_status(status_handle, SERVICE_STOP_PENDING, 1, 10_000)?;
    if let Some(mut child) = helper.take() {
        child.stop();
    }
    report_service_status(status_handle, SERVICE_STOPPED, 0, 0)?;
    Ok(())
}

unsafe extern "system" fn service_control_handler(
    control: u32,
    event_type: u32,
    event_data: *mut c_void,
    context: *mut c_void,
) -> u32 {
    if context.is_null() {
        return ::windows::Win32::Foundation::ERROR_INVALID_PARAMETER.0;
    }
    // SAFETY: The SCM passes the exact context pointer registered by
    // `run_service_main`; its Box remains live until dispatcher shutdown.
    let context = unsafe { &*context.cast::<HandlerContext>() };
    let control = match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => Some(ServiceControl::Stop),
        SERVICE_CONTROL_SESSIONCHANGE => session_control(event_type, event_data),
        SERVICE_CONTROL_INTERROGATE => return NO_ERROR.0,
        _ => return ::windows::Win32::Foundation::ERROR_CALL_NOT_IMPLEMENTED.0,
    };
    let Some(control) = control else {
        return NO_ERROR.0;
    };
    match context.sender.try_send(control) {
        Ok(()) => NO_ERROR.0,
        Err(TrySendError::Full(_)) => {
            context.queue_overflowed.store(true, Ordering::Release);
            NO_ERROR.0
        }
        Err(TrySendError::Disconnected(_)) => NO_ERROR.0,
    }
}

fn session_control(event_type: u32, event_data: *mut c_void) -> Option<ServiceControl> {
    if event_data.is_null() {
        return None;
    }
    // SAFETY: SCM supplies a WTSSESSION_NOTIFICATION structure for
    // SERVICE_CONTROL_SESSIONCHANGE. Read unaligned to avoid relying on caller alignment.
    let notification = unsafe { ptr::read_unaligned(event_data.cast::<WTSSESSION_NOTIFICATION>()) };
    if usize::try_from(notification.cbSize).ok()? < std::mem::size_of::<WTSSESSION_NOTIFICATION>() {
        return None;
    }
    let session_id = notification.dwSessionId;
    let change = match event_type {
        WTS_SESSION_LOGON => SessionChange::Logon { session_id },
        WTS_SESSION_LOGOFF => SessionChange::Logoff { session_id },
        WTS_SESSION_LOCK => SessionChange::Lock { session_id },
        WTS_SESSION_UNLOCK => SessionChange::Unlock { session_id },
        WTS_CONSOLE_CONNECT
        | WTS_CONSOLE_DISCONNECT
        | WTS_REMOTE_CONNECT
        | WTS_REMOTE_DISCONNECT => SessionChange::FastUserSwitch {
            active_session: active_console_session(),
        },
        _ => return None,
    };
    Some(ServiceControl::Session(change))
}

fn active_console_session() -> Option<SessionId> {
    // SAFETY: This API reads the active console session id and has no pointer arguments.
    let session_id = unsafe { WTSGetActiveConsoleSessionId() };
    (session_id != INVALID_SESSION_ID).then_some(session_id)
}

fn apply_actions(
    initial: Vec<SupervisorAction>,
    supervisor: &mut Supervisor,
    helper: &mut Option<HelperProcess>,
    started_at: Instant,
) -> Result<(), Box<dyn Error>> {
    let mut actions = VecDeque::from(initial);
    while let Some(action) = actions.pop_front() {
        match action {
            SupervisorAction::StartHelper {
                session_id,
                generation,
            } => match HelperProcess::launch(session_id, generation) {
                Ok(child) => {
                    service_debug(&format!("started capture helper for session {session_id}"));
                    *helper = Some(child);
                }
                Err(error) => {
                    service_debug(&format!("could not start capture helper: {error}"));
                    actions.extend(supervisor.handle(
                        elapsed_ms(started_at),
                        SupervisorInput::HelperExited {
                            generation,
                            exit_code: -1,
                        },
                    ));
                }
            },
            SupervisorAction::StopHelper {
                generation, reason, ..
            } => {
                if helper
                    .as_ref()
                    .is_some_and(|child| child.generation == generation)
                {
                    if let Some(mut child) = helper.take() {
                        child.stop();
                    }
                }
                actions.extend(supervisor.handle(
                    elapsed_ms(started_at),
                    SupervisorInput::HelperExited {
                        generation,
                        exit_code: stop_reason_exit_code(reason),
                    },
                ));
            }
            SupervisorAction::ScheduleRestart { .. } => {}
            SupervisorAction::Log(log) => log_supervisor_event(log),
        }
    }
    Ok(())
}

fn stop_reason_exit_code(reason: crate::supervisor::StopReason) -> i32 {
    match reason {
        crate::supervisor::StopReason::ServiceStopping => 0,
        crate::supervisor::StopReason::SessionChanged => 10,
        crate::supervisor::StopReason::SessionLocked => 11,
        crate::supervisor::StopReason::SessionEnded => 12,
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

struct ServiceStatusGuard(::windows::Win32::System::Services::SERVICE_STATUS_HANDLE);

impl Drop for ServiceStatusGuard {
    fn drop(&mut self) {
        let _ = report_service_status(self.0, SERVICE_STOPPED, 0, 0);
    }
}
fn report_service_status(
    handle: ::windows::Win32::System::Services::SERVICE_STATUS_HANDLE,
    state: ::windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE,
    accepted_controls: u32,
    wait_hint_ms: u32,
) -> Result<(), ::windows::core::Error> {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted_controls,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: u32::from(state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING),
        dwWaitHint: wait_hint_ms,
    };
    // SAFETY: `handle` was returned by RegisterServiceCtrlHandlerExW and `status`
    // is a fully initialized SERVICE_STATUS for this service.
    unsafe { SetServiceStatus(handle, &status) }
}

fn log_supervisor_event(log: SupervisorLog) {
    service_debug(&format!("supervisor: {log:?}"));
}

fn service_debug(message: &str) {
    // OutputDebugString is intentionally the only current service diagnostic sink.
    // Durable rotating logs and Event Log registration remain an M6 integration gap.
    let message = wide_null(&format!("racc-host-agent: {message}"));
    // SAFETY: `message` is NUL-terminated and remains live for the synchronous call.
    unsafe {
        ::windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(PCWSTR(message.as_ptr()))
    };
}

struct HelperProcess {
    session_id: SessionId,
    generation: u64,
    process: OwnedHandle,
    stop_event: OwnedHandle,
}

impl HelperProcess {
    fn launch(session_id: SessionId, generation: u64) -> Result<Self, Box<dyn Error>> {
        let mut user_token = HANDLE::default();
        // SAFETY: WTSQueryUserToken writes one token handle for the selected logon
        // session. This succeeds only when the service has the required LocalSystem rights.
        unsafe { WTSQueryUserToken(session_id, &mut user_token)? };
        let user_token = OwnedHandle(user_token);
        let mut environment = ptr::null_mut();
        // SAFETY: `user_token` is a valid token returned by WTSQueryUserToken and the
        // output pointer is writable. The environment block is released by EnvBlock.
        unsafe { CreateEnvironmentBlock(&mut environment, user_token.0, false)? };
        let environment = EnvBlock(environment);

        let event_name = new_stop_event_name()?;
        let event_name_wide = wide_null(&event_name);
        let mut security_descriptor =
            ::windows::Win32::Security::PSECURITY_DESCRIPTOR(ptr::null_mut());
        let sddl = wide_null(EVENT_SECURITY_DESCRIPTOR);
        // SAFETY: The SDDL and output pointer are valid. The descriptor is freed by
        // SecurityDescriptor after CreateEventW has copied the security attributes.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                ptr::addr_of_mut!(security_descriptor),
                None,
            )?
        };
        let security_descriptor = SecurityDescriptor(security_descriptor.0);
        let event_attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security_descriptor.0,
            bInheritHandle: BOOL(0),
        };
        // SAFETY: Attributes point to a valid descriptor, event parameters are bounded,
        // and the returned handle is immediately wrapped for ownership.
        let stop_event = OwnedHandle(unsafe {
            CreateEventW(
                Some(&event_attributes),
                BOOL(1),
                BOOL(0),
                PCWSTR(event_name_wide.as_ptr()),
            )?
        });

        let executable = std::env::current_exe()?;
        let executable_wide = wide_null_os(executable.as_os_str());
        let working_directory = executable
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let working_directory_wide = wide_null_os(working_directory.as_os_str());
        let stop_argument = wide_null(&event_name);
        let mut command_line = Vec::with_capacity(executable_wide.len() + stop_argument.len() + 64);
        append_quoted_argument(
            &mut command_line,
            &executable_wide[..executable_wide.len() - 1],
        );
        command_line.extend(" helper --service-stop-event ".encode_utf16());
        append_quoted_argument(&mut command_line, &stop_argument[..stop_argument.len() - 1]);
        command_line.push(0);

        let mut desktop = wide_null("WinSta0\\Default");
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut process_info = ::windows::Win32::System::Threading::PROCESS_INFORMATION::default();
        // SAFETY: All input buffers are NUL-terminated and remain live through the call;
        // the session token and environment block are valid; output is writable. The
        // child is launched on the interactive default desktop, never the secure desktop.
        unsafe {
            CreateProcessAsUserW(
                user_token.0,
                PCWSTR(executable_wide.as_ptr()),
                PWSTR(command_line.as_mut_ptr()),
                None,
                None,
                BOOL(0),
                CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
                Some(environment.0.cast_const()),
                PCWSTR(working_directory_wide.as_ptr()),
                &startup,
                &mut process_info,
            )?
        };
        let _thread = OwnedHandle(process_info.hThread);
        Ok(Self {
            session_id,
            generation,
            process: OwnedHandle(process_info.hProcess),
            stop_event,
        })
    }

    fn poll_exit(&mut self) -> Result<Option<i32>, ::windows::core::Error> {
        // SAFETY: The process handle is valid and owned until HelperProcess is dropped.
        let wait = unsafe { WaitForSingleObject(self.process.0, 0) };
        if wait == WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait == WAIT_TIMEOUT {
            return Err(::windows::core::Error::from_win32());
        }
        let mut exit_code = 0_u32;
        // SAFETY: `process` is a valid signaled process handle and `exit_code` is writable.
        unsafe { GetExitCodeProcess(self.process.0, &mut exit_code)? };
        Ok(Some(exit_code as i32))
    }

    fn stop(&mut self) {
        // SAFETY: The event handle is valid. If the helper has exited, setting it may fail;
        // we still wait briefly and terminate only if the process remains alive.
        let _ = unsafe { SetEvent(self.stop_event.0) };
        // SAFETY: Process handle is owned and valid.
        let wait = unsafe {
            WaitForSingleObject(
                self.process.0,
                u32::try_from(HELPER_STOP_GRACE.as_millis()).unwrap_or(u32::MAX),
            )
        };
        if wait == WAIT_TIMEOUT {
            // SAFETY: This is the service-owned helper process handle. This is only used
            // after its explicit stop event failed to end it within the grace interval.
            if unsafe { ::windows::Win32::System::Threading::TerminateProcess(self.process.0, 1) }
                .is_ok()
            {
                // SAFETY: The process handle remains owned and is valid after termination.
                let _ = unsafe { WaitForSingleObject(self.process.0, 2_000) };
            }
        }
        service_debug(&format!(
            "stopped capture helper for session {}",
            self.session_id
        ));
    }
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: This wrapper uniquely owns this handle and closes it exactly once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

struct EnvBlock(*mut c_void);
impl Drop for EnvBlock {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This pointer came from CreateEnvironmentBlock and is released once.
            let _ = unsafe { DestroyEnvironmentBlock(self.0) };
        }
    }
}

struct SecurityDescriptor(*mut c_void);
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: This descriptor came from ConvertStringSecurityDescriptor... and is
            // released with the matching LocalFree allocator exactly once.
            let _ = unsafe { ::windows::Win32::Foundation::LocalFree(HLOCAL(self.0)) };
        }
    }
}

fn new_stop_event_name() -> Result<String, ::windows::core::Error> {
    let mut random = [0_u8; 16];
    // SAFETY: BCryptGenRandom writes exactly the provided 16-byte buffer using the
    // system-preferred RNG and does not retain the buffer.
    let status = unsafe { BCryptGenRandom(None, &mut random, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.0 < 0 {
        return Err(::windows::core::Error::from_win32());
    }
    let mut name = String::with_capacity(EVENT_NAME_PREFIX.len() + random.len() * 2);
    name.push_str(EVENT_NAME_PREFIX);
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(&mut name, "{byte:02x}");
    }
    Ok(name)
}

fn append_quoted_argument(command_line: &mut Vec<u16>, argument: &[u16]) {
    command_line.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in argument.iter().copied() {
        if unit == b'\\' as u16 {
            backslashes = backslashes.saturating_add(1);
        } else if unit == b'"' as u16 {
            command_line.extend(std::iter::repeat_n(
                b'\\' as u16,
                backslashes.saturating_mul(2).saturating_add(1),
            ));
            command_line.push(unit);
            backslashes = 0;
        } else {
            command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            command_line.push(unit);
            backslashes = 0;
        }
    }
    command_line.extend(std::iter::repeat_n(
        b'\\' as u16,
        backslashes.saturating_mul(2),
    ));
    command_line.push(b'"' as u16);
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_null_os(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_stop_event_name_requires_private_random_suffix() {
        assert!(
            validate_stop_event_name("Global\\RaccHostStop-0123456789abcdef0123456789ABCDEF")
                .is_ok()
        );
        assert!(
            validate_stop_event_name("Local\\RaccHostStop-0123456789abcdef0123456789abcdef")
                .is_err()
        );
        assert!(validate_stop_event_name("Global\\RaccHostStop-short").is_err());
        assert!(
            validate_stop_event_name("Global\\RaccHostStop-0123456789abcdef0123456789abcdeg")
                .is_err()
        );
    }

    #[test]
    fn process_argument_quoting_preserves_spaces_quotes_and_trailing_slashes() {
        let source: Vec<u16> = r#"C:\folder with space\a"b\"#.encode_utf16().collect();
        let mut quoted = Vec::new();
        append_quoted_argument(&mut quoted, &source);
        let rendered = String::from_utf16(&quoted).expect("valid UTF-16 command line");
        assert_eq!(rendered, r#""C:\folder with space\a\"b\\""#);
    }

    #[test]
    fn session_change_events_translate_to_supervisor_inputs() {
        let notification = WTSSESSION_NOTIFICATION {
            cbSize: std::mem::size_of::<WTSSESSION_NOTIFICATION>() as u32,
            dwSessionId: 17,
        };
        let event_data = (&notification as *const WTSSESSION_NOTIFICATION)
            .cast_mut()
            .cast();
        assert!(matches!(
            session_control(WTS_SESSION_LOCK, event_data),
            Some(ServiceControl::Session(SessionChange::Lock {
                session_id: 17
            }))
        ));
        assert!(matches!(
            session_control(WTS_SESSION_LOGON, event_data),
            Some(ServiceControl::Session(SessionChange::Logon {
                session_id: 17
            }))
        ));
        assert!(session_control(999, event_data).is_none());
        assert!(session_control(WTS_SESSION_LOCK, ptr::null_mut()).is_none());
    }
}
