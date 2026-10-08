/// A platform-specific autostart location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutostartTarget {
    /// Current user's `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` entry.
    WindowsRunKey { value_name: String },
    /// Per-user LaunchAgent plist label.
    MacLaunchAgent {
        label: String,
        path: std::path::PathBuf,
    },
}

/// A reversible write plan; an OS adapter owns applying it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutostartPlan {
    /// Destination to write or remove.
    pub target: AutostartTarget,
    /// Value or plist contents for enable; absent when disabling.
    pub contents: Option<String>,
}

/// Minimal adapter for a system registry or filesystem implementation.
pub trait AutostartBackend {
    /// Adapter-specific error type.
    type Error;

    /// Writes/replaces or removes the target atomically where the platform allows.
    fn apply(&mut self, plan: &AutostartPlan) -> Result<(), Self::Error>;
}

/// Executes the plan through an injected adapter, enabling fake-writer tests.
pub fn apply_autostart_plan<B: AutostartBackend>(
    backend: &mut B,
    plan: &AutostartPlan,
) -> Result<(), B::Error> {
    backend.apply(plan)
}

/// Fixed Windows Run value and macOS LaunchAgent names owned by this app.
pub const AUTOSTART_ENTRY_NAME: &str = "RaccConnect";
/// Stable per-user LaunchAgent label owned by this app.
pub const AUTOSTART_LAUNCH_AGENT_LABEL: &str = "com.racc.connect";

/// Builds the current platform's per-user sign-in entry without applying it.
pub fn current_platform_autostart_plan(
    executable: &std::path::Path,
    enabled: bool,
) -> std::io::Result<AutostartPlan> {
    #[cfg(target_os = "windows")]
    {
        let executable = executable.to_string_lossy();
        Ok(AutostartPlan {
            target: AutostartTarget::WindowsRunKey {
                value_name: AUTOSTART_ENTRY_NAME.to_owned(),
            },
            contents: enabled.then(|| windows_run_value(&executable)),
        })
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "current user's home directory is unavailable",
                )
            })?;
        let executable = executable.to_string_lossy();
        Ok(mac_launch_agent_plan(
            &home,
            AUTOSTART_LAUNCH_AGENT_LABEL,
            &executable,
            enabled,
        ))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (executable, enabled);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "current-user autostart is supported only on Windows and macOS",
        ))
    }
}

/// Applies the current user's autostart preference on the supported desktop OS.
/// This is the only entry point that performs OS writes and should be called only
/// after the user changes the preference in Settings.
pub fn apply_current_platform_autostart(enabled: bool) -> std::io::Result<()> {
    let executable = std::env::current_exe()?;
    let plan = current_platform_autostart_plan(&executable, enabled)?;
    #[cfg(target_os = "windows")]
    {
        WindowsRunKeyBackend.apply(&plan)
    }
    #[cfg(target_os = "macos")]
    {
        MacLaunchAgentBackend.apply(&plan)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = plan;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "current-user autostart is supported only on Windows and macOS",
        ))
    }
}

#[cfg(target_os = "windows")]
struct WindowsRunKeyBackend;

#[cfg(target_os = "windows")]
impl AutostartBackend for WindowsRunKeyBackend {
    type Error = std::io::Error;

    fn apply(&mut self, plan: &AutostartPlan) -> Result<(), Self::Error> {
        use std::process::Command;

        let AutostartTarget::WindowsRunKey { value_name } = &plan.target else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Windows autostart backend received a non-Windows target",
            ));
        };
        if value_name != AUTOSTART_ENTRY_NAME {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to modify an autostart value not owned by this app",
            ));
        }
        let mut command = Command::new("reg.exe");
        match &plan.contents {
            Some(contents) => {
                command.args([
                    "add",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                    "/v",
                    value_name,
                    "/t",
                    "REG_SZ",
                    "/d",
                    contents,
                    "/f",
                ]);
            }
            None => {
                command.args([
                    "delete",
                    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                    "/v",
                    value_name,
                    "/f",
                ]);
            }
        }
        let status = command.status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "reg.exe failed to apply the current-user autostart entry ({status})"
            )))
        }
    }
}

#[cfg(target_os = "macos")]
struct MacLaunchAgentBackend;

#[cfg(target_os = "macos")]
impl AutostartBackend for MacLaunchAgentBackend {
    type Error = std::io::Error;

    fn apply(&mut self, plan: &AutostartPlan) -> Result<(), Self::Error> {
        use std::fs::{self, OpenOptions};
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let AutostartTarget::MacLaunchAgent { label, path } = &plan.target else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "macOS autostart backend received a non-macOS target",
            ));
        };
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is unavailable")
            })?;
        let expected = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{AUTOSTART_LAUNCH_AGENT_LABEL}.plist"));
        if label != AUTOSTART_LAUNCH_AGENT_LABEL || path != &expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "refusing to modify a LaunchAgent not owned by this app",
            ));
        }
        let domain = launchctl_gui_domain()?;
        let service = format!("{domain}/{label}");
        let Some(contents) = &plan.contents else {
            run_launchctl(&["disable", &service])?;
            return match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
        };

        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "LaunchAgent has no parent",
            )
        })?;
        fs::create_dir_all(parent)?;
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{AUTOSTART_LAUNCH_AGENT_LABEL}.plist.{}-{sequence}.tmp",
            std::process::id()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            run_launchctl(&["enable", &service])?;
            if !launchctl_service_loaded(&service)? {
                run_launchctl(&["bootstrap", &domain, path.to_string_lossy().as_ref()])?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(target_os = "macos")]
fn launchctl_gui_domain() -> std::io::Result<String> {
    use std::process::{Command, Stdio};

    let output = Command::new("id")
        .arg("-u")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            "could not determine the current user ID",
        ));
    }
    let uid = String::from_utf8(output.stdout)
        .map_err(|_| std::io::Error::other("current user ID was not valid UTF-8"))?;
    let uid = uid.trim();
    if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(std::io::Error::other("current user ID was malformed"));
    }
    Ok(format!("gui/{uid}"))
}

#[cfg(target_os = "macos")]
fn launchctl_service_loaded(service: &str) -> std::io::Result<bool> {
    use std::process::{Command, Stdio};

    let status = Command::new("launchctl")
        .arg("print")
        .arg(service)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

#[cfg(target_os = "macos")]
fn run_launchctl(arguments: &[&str]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};

    let status = Command::new("launchctl")
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "launchctl {} failed with status {}",
            arguments.first().copied().unwrap_or("command"),
            status.code().unwrap_or(-1)
        )))
    }
}

/// Quotes an executable path for the Windows Run value and appends no user data.
pub fn windows_run_value(executable: &str) -> String {
    format!("\"{}\"", executable.replace('"', "\\\""))
}

/// Generates a LaunchAgent plist for a single app executable.
pub fn mac_launch_agent_plist(label: &str, executable: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><false/></dict></plist>\n",
        xml_escape(label),
        xml_escape(executable),
    )
}

/// Builds a macOS LaunchAgent write/removal plan under the current user's Library.
pub fn mac_launch_agent_plan(
    home: &std::path::Path,
    label: &str,
    executable: &str,
    enabled: bool,
) -> AutostartPlan {
    let path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"));
    AutostartPlan {
        target: AutostartTarget::MacLaunchAgent {
            label: label.to_owned(),
            path,
        },
        contents: enabled.then(|| mac_launch_agent_plist(label, executable)),
    }
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[derive(Default)]
    struct FakeBackend(Vec<AutostartPlan>);
    impl AutostartBackend for FakeBackend {
        type Error = ();
        fn apply(&mut self, plan: &AutostartPlan) -> Result<(), Self::Error> {
            self.0.push(plan.clone());
            Ok(())
        }
    }

    #[test]
    fn fake_backend_observes_reversible_enable_and_disable_plans() {
        let mut fake = FakeBackend::default();
        let enabled = AutostartPlan {
            target: AutostartTarget::WindowsRunKey {
                value_name: "RaccConnect".to_owned(),
            },
            contents: Some(windows_run_value(
                "C:\\Program Files\\Racc Connect\\racc-app.exe",
            )),
        };
        let disabled = AutostartPlan {
            target: enabled.target.clone(),
            contents: None,
        };
        apply_autostart_plan(&mut fake, &enabled).expect("enable");
        apply_autostart_plan(&mut fake, &disabled).expect("disable");
        assert_eq!(fake.0, vec![enabled, disabled]);
    }

    #[test]
    fn fake_backend_applies_and_removes_owned_windows_and_mac_entries() {
        let mut fake = FakeBackend::default();
        let windows = AutostartPlan {
            target: AutostartTarget::WindowsRunKey {
                value_name: AUTOSTART_ENTRY_NAME.to_owned(),
            },
            contents: Some(windows_run_value(
                "C:\\Program Files\\Racc Connect\\racc-app.exe",
            )),
        };
        let mac = mac_launch_agent_plan(
            Path::new("/Users/example"),
            AUTOSTART_LAUNCH_AGENT_LABEL,
            "/Applications/Racc Connect.app/Contents/MacOS/racc-app",
            true,
        );
        let mut disabled_mac = mac.clone();
        disabled_mac.contents = None;
        apply_autostart_plan(&mut fake, &windows).expect("fake Windows enable");
        apply_autostart_plan(&mut fake, &mac).expect("fake macOS enable");
        apply_autostart_plan(&mut fake, &disabled_mac).expect("fake macOS disable");
        assert_eq!(fake.0, vec![windows, mac, disabled_mac]);
    }

    #[test]
    fn launch_agent_escapes_xml_and_records_user_app() {
        let plan = mac_launch_agent_plan(
            Path::new("/Users/example"),
            "com.racc.connect",
            "/Applications/Racc Connect.app/Contents/MacOS/racc-app",
            true,
        );
        let Some(contents) = plan.contents else {
            panic!("enabled plan contains plist");
        };
        assert!(contents.contains("com.racc.connect"));
        assert!(contents
            .contains("<string>/Applications/Racc Connect.app/Contents/MacOS/racc-app</string>"));
        assert!(!contents.contains("&"));
    }

    #[test]
    fn run_key_value_quotes_path() {
        assert_eq!(
            windows_run_value("C:\\Program Files\\racc-app.exe"),
            "\"C:\\Program Files\\racc-app.exe\""
        );
    }
}
