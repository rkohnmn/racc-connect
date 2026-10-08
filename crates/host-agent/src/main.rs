pub mod clipboard_bridge;
pub mod control_server;
#[cfg(any(windows, target_os = "macos", test))]
mod cursor_sender;
#[cfg(any(windows, target_os = "macos", test))]
mod input_worker;
mod local_ipc;
#[cfg(any(windows, test))]
mod stream_dispatch;
pub mod supervisor;
mod topology_watch;

#[cfg(target_os = "macos")]
mod foreground_macos;
#[cfg(windows)]
mod foreground_windows;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_bounded_log;
#[cfg(target_os = "macos")]
mod macos_host_policy;
#[cfg(any(target_os = "macos", test))]
mod macos_system_metrics;
#[cfg(windows)]
pub mod windows;

use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("status") => match print_tailscale_status() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("host-agent status unavailable: {error}");
                ExitCode::from(1)
            }
        },
        Some("service") => run_service_command(),
        Some("helper") => run_helper_command(),
        Some("install") => print_service_script_guidance("install-service.ps1"),
        Some("uninstall") => print_service_script_guidance("uninstall-service.ps1"),
        Some("console") | Some("host") | Some("start") => run_host_command(),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(command) => {
            eprintln!("unknown command: {command}");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    println!("Usage: racc-host-agent <status|service|helper|console|install|uninstall>");
    println!("  status  Show the local Tailscale identity and address");
    println!("  service Run as the Windows SCM lifecycle supervisor");
    println!("  helper  Internal interactive-session host process (service launched)");
    println!("  console Run the host listener in this foreground session");
    println!("  install Print instructions for the human-run service script");
    println!("  uninstall Print instructions for the human-run service script");
}

#[cfg(windows)]
fn run_service_command() -> ExitCode {
    match windows::run_service_dispatcher() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("could not connect to the Windows Service Control Manager: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn run_service_command() -> ExitCode {
    eprintln!("the Windows service command is available on Windows only");
    ExitCode::from(1)
}

#[cfg(windows)]
fn run_helper_command() -> ExitCode {
    let mut arguments = std::env::args().skip(2);
    if arguments.next().as_deref() != Some("--service-stop-event") {
        eprintln!("helper must be launched by the service supervisor");
        return ExitCode::from(2);
    }
    let Some(event_name) = arguments.next() else {
        eprintln!("helper is missing its service stop event");
        return ExitCode::from(2);
    };
    if arguments.next().is_some() {
        eprintln!("helper received unexpected arguments");
        return ExitCode::from(2);
    }
    let stop_signal = match windows::start_stop_event_listener(&event_name) {
        Ok(receiver) => receiver,
        Err(error) => {
            eprintln!("helper could not monitor the service stop event: {error}");
            return ExitCode::from(1);
        }
    };
    match foreground_windows::run_foreground_host_with_stop_event(stop_signal) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("interactive host helper stopped: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn run_helper_command() -> ExitCode {
    eprintln!("the interactive host helper is available on Windows only");
    ExitCode::from(1)
}

fn print_service_script_guidance(script: &str) -> ExitCode {
    println!("No service configuration was changed.");
    println!("Review and run scripts/{script} manually from an elevated PowerShell terminal.");
    println!("The scripts support --dry-run; the host-agent binary never registers or removes a service.");
    ExitCode::SUCCESS
}
fn print_tailscale_status() -> Result<(), racc_identity::IdentityError> {
    let client = racc_identity::TailscaleClient::system();
    let address = client.self_bind_addr()?;
    let self_node = client.self_node()?;
    let name = self_node
        .as_ref()
        .and_then(|node| node.display_name.as_deref())
        .unwrap_or("unknown device");
    let online = self_node.as_ref().is_some_and(|node| node.online);
    println!("Tailscale: {}", if online { "online" } else { "offline" });
    println!("Device: {name}");
    println!("Address: {address}");
    Ok(())
}

#[cfg(windows)]
fn run_host_command() -> ExitCode {
    match foreground_windows::run_foreground_host() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("foreground host stopped: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(target_os = "macos")]
fn run_host_command() -> ExitCode {
    match foreground_macos::run_foreground_host() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("foreground Mac host stopped: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_host_command() -> ExitCode {
    eprintln!("the foreground host command is currently available on Windows and macOS only");
    ExitCode::from(1)
}
