//! Size-capped, content-free logging for the foreground macOS host.
//!
//! Callers should pass fixed lifecycle or failure descriptions only. Clipboard text,
//! input events, peer identifiers, and other user content must never be recorded.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum size of the active host log file.
pub const MAC_HOST_LOG_MAX_BYTES: u64 = 1024 * 1024;
const MAX_LOG_MESSAGE_CHARS: usize = 512;

/// A serialized writer that truncates its single log file before it exceeds its cap.
#[derive(Debug)]
pub struct BoundedMacHostLog {
    path: PathBuf,
    max_bytes: u64,
    write_lock: Mutex<()>,
}

impl BoundedMacHostLog {
    /// Opens a private log file inside the supplied Application Support directory.
    pub fn open(data_directory: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_limit(data_directory, MAC_HOST_LOG_MAX_BYTES)
    }

    fn open_with_limit(data_directory: impl AsRef<Path>, max_bytes: u64) -> io::Result<Self> {
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Mac host log size limit must be nonzero",
            ));
        }
        let data_directory = data_directory.as_ref();
        ensure_private_log_directory(data_directory)?;
        let path = data_directory.join("host-agent.log");
        let log = Self {
            path,
            max_bytes,
            write_lock: Mutex::new(()),
        };
        let file = log.open_append()?;
        if file.metadata()?.len() > max_bytes {
            file.set_len(0)?;
        }
        Ok(log)
    }

    /// Appends a sanitized lifecycle or failure message without exceeding the file cap.
    pub fn record(&self, message: &str) -> io::Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| io::Error::other("Mac host log lock was poisoned"))?;
        let sanitized: String = message
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_LOG_MESSAGE_CHARS)
            .collect();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let mut record = format!("{timestamp} {sanitized}\n");
        if record.len() as u64 > self.max_bytes {
            let mut boundary = usize::try_from(self.max_bytes).unwrap_or(usize::MAX);
            while !record.is_char_boundary(boundary) {
                boundary = boundary.saturating_sub(1);
            }
            record.truncate(boundary);
        }

        let mut file = self.open_append()?;
        let existing_len = file.metadata()?.len();
        if existing_len.saturating_add(record.len() as u64) > self.max_bytes {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
        }
        file.write_all(record.as_bytes())
    }

    /// Returns the path of this log for diagnostics and tests.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open_append(&self) -> io::Result<File> {
        validate_existing_log_file(&self.path)?;
        let mut options = OpenOptions::new();
        options
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options.open(&self.path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.uid() != effective_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mac host log is not a regular file owned by the current user",
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
}

fn ensure_private_log_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Mac host log path is not a real directory",
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Mac host log directory is owned by another user",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn validate_existing_log_file(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Mac host log path is not a regular file",
        ));
    }
    if metadata.uid() != effective_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Mac host log file is owned by another user",
        ));
    }
    Ok(())
}

fn effective_uid() -> libc::uid_t {
    // SAFETY: geteuid has no pointer arguments or preconditions and returns the caller's UID.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    fn test_directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "racc-mac-host-log-{}-{}",
            std::process::id(),
            NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn log_stays_within_cap_and_truncates_old_records() {
        let directory = test_directory();
        let log = BoundedMacHostLog::open_with_limit(&directory, 64).expect("private test log");
        log.record("first lifecycle record").expect("first record");
        log.record("second lifecycle record")
            .expect("second record");
        log.record("latest lifecycle record")
            .expect("latest record");
        let contents = fs::read_to_string(log.path()).expect("read bounded log");
        assert!(contents.len() <= 64);
        assert!(contents.contains("latest lifecycle record"));
        assert!(!contents.contains("first lifecycle record"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn log_rejects_symlink_files_and_directories() {
        use std::os::unix::fs::symlink;

        let root = test_directory();
        fs::create_dir(&root).expect("create temporary log root");
        let outside = root.join("outside.log");
        fs::write(&outside, b"keep").expect("create outside log");
        let linked_log_directory = root.join("linked-logs");
        symlink(&root, &linked_log_directory).expect("create log directory symlink");
        assert!(BoundedMacHostLog::open(&linked_log_directory).is_err());

        let logs = root.join("logs");
        fs::create_dir(&logs).expect("create log directory");
        let log_link = logs.join("host-agent.log");
        symlink(&outside, &log_link).expect("create log file symlink");
        assert!(BoundedMacHostLog::open(&logs).is_err());
        assert_eq!(fs::read(&outside).expect("read outside file"), b"keep");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_sanitizes_control_characters_and_rejects_zero_cap() {
        let directory = test_directory();
        assert_eq!(
            BoundedMacHostLog::open_with_limit(&directory, 0)
                .expect_err("zero cap is invalid")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        let log = BoundedMacHostLog::open_with_limit(&directory, 256).expect("private test log");
        log.record("first\nsecond\tthird")
            .expect("sanitized record");
        let contents = fs::read_to_string(log.path()).expect("read log");
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.contains("firstsecondthird"));
        let _ = fs::remove_dir_all(directory);
    }
}
