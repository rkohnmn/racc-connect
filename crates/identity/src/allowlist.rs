use crate::error::IdentityError;
use crate::model::PeerIdentity;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum serialized allowlist size.
pub const MAX_ALLOWLIST_BYTES: usize = 256 * 1024;
/// Maximum approved, rejected, and pending entries combined.
pub const MAX_ALLOWLIST_ENTRIES: usize = 512;
/// Maximum number of peers waiting for host approval.
pub const MAX_PENDING_APPROVALS: usize = 128;
const SCHEMA_VERSION: u8 = 1;
const MAX_ID_BYTES: usize = 512;
const MAX_OWNER_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 128;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Approval state persisted for one stable Tailscale identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalState {
    /// Host policy permits this identity to connect.
    Approved,
    /// Identity is queued for explicit owner approval and remains rejected.
    Pending,
    /// Owner explicitly rejected this identity.
    Rejected,
}

/// Result of checking a peer against the host allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessState {
    /// The peer is approved.
    Approved,
    /// The peer awaits owner review and is not authorized.
    Pending,
    /// The peer was explicitly rejected.
    Rejected,
    /// The peer could not be added to the bounded pending queue.
    Unknown,
}

/// One persisted allowlist record; it contains no addresses, keys, or secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    /// Stable Tailscale node identifier.
    pub node_id: String,
    /// Owner login associated with the node, when reported by whois.
    pub owner_login: Option<String>,
    /// Owner-editable label shown in the approval UI.
    pub label: String,
    /// Unix seconds when this record was first added.
    pub added_at_unix_secs: u64,
    /// Current decision for this identity.
    pub state: ApprovalState,
    /// Unix seconds when this identity was last observed.
    pub last_seen_unix_secs: u64,
}

/// Event generated while loading or checking the allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllowlistEvent {
    /// Invalid or oversized persistence was moved aside; a fresh list is active.
    CorruptFileQuarantined,
    /// A new identity could not enter the bounded pending queue.
    PendingQueueFull,
}

/// Load result returned by an allowlist persistence implementation.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AllowlistLoad {
    /// Valid records loaded from storage.
    pub entries: Vec<AllowlistEntry>,
    /// Recovery events that should be surfaced to the host UI or event log.
    pub events: Vec<AllowlistEvent>,
}

/// Persistence boundary for allowlist data.
pub trait AllowlistStore: Send + Sync {
    /// Loads and validates records, recovering from corruption where possible.
    fn load(&self) -> Result<AllowlistLoad, IdentityError>;
    /// Atomically replaces the saved records.
    fn save(&self, entries: &[AllowlistEntry]) -> Result<(), IdentityError>;
}

/// JSON file implementation of the allowlist persistence boundary.
#[derive(Clone, Debug)]
pub struct FileAllowlistStore {
    path: PathBuf,
}

impl FileAllowlistStore {
    /// Creates a store at an explicit per-user config path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Returns this store's allowlist file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn quarantine_corrupt_file(&self) -> Result<(), IdentityError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| IdentityError::Allowlist("allowlist path has no parent".to_owned()))?;
        let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
        let extension = self.path.extension().unwrap_or_default().to_string_lossy();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        for attempt in 0..100_u32 {
            let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let filename = format!("{stem}.corrupt-{now}-{suffix}-{attempt}.{extension}");
            let destination = parent.join(filename);
            if destination.exists() {
                continue;
            }
            fs::rename(&self.path, destination).map_err(io_error)?;
            return Ok(());
        }
        Err(IdentityError::Allowlist(
            "could not choose a quarantine filename".to_owned(),
        ))
    }
}

#[derive(Serialize, Deserialize)]
struct DiskAllowlist {
    schema_version: u8,
    entries: Vec<AllowlistEntry>,
}

/// Builds the standard Racc Connect allowlist path below a caller-selected config root.
pub fn allowlist_path(config_root: &Path) -> PathBuf {
    config_root.join("RaccConnect").join("allowlist.json")
}

impl AllowlistStore for FileAllowlistStore {
    fn load(&self) -> Result<AllowlistLoad, IdentityError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AllowlistLoad::default())
            }
            Err(error) => return Err(io_error(error)),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take((MAX_ALLOWLIST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        let parsed = if bytes.len() <= MAX_ALLOWLIST_BYTES {
            serde_json::from_slice::<DiskAllowlist>(&bytes).ok()
        } else {
            None
        };
        let Some(disk) = parsed.filter(|disk| {
            disk.schema_version == SCHEMA_VERSION
                && disk.entries.len() <= MAX_ALLOWLIST_ENTRIES
                && validate_entries(&disk.entries).is_ok()
        }) else {
            self.quarantine_corrupt_file()?;
            return Ok(AllowlistLoad {
                entries: Vec::new(),
                events: vec![AllowlistEvent::CorruptFileQuarantined],
            });
        };
        Ok(AllowlistLoad {
            entries: disk.entries,
            events: Vec::new(),
        })
    }

    fn save(&self, entries: &[AllowlistEntry]) -> Result<(), IdentityError> {
        validate_entries(entries)?;
        let disk = DiskAllowlist {
            schema_version: SCHEMA_VERSION,
            entries: entries.to_vec(),
        };
        let bytes = serde_json::to_vec(&disk)
            .map_err(|_| IdentityError::Allowlist("JSON serialization failed".to_owned()))?;
        if bytes.len() > MAX_ALLOWLIST_BYTES {
            return Err(IdentityError::Allowlist(
                "serialized allowlist exceeds its size limit".to_owned(),
            ));
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| IdentityError::Allowlist("allowlist path has no parent".to_owned()))?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = self
            .path
            .with_extension(format!("tmp-{}-{suffix}", std::process::id()));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .map_err(io_error)?;
            file.write_all(&bytes).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            drop(file);
            fs::rename(&temp_path, &self.path).map_err(io_error)?;
            Ok::<(), IdentityError>(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }
}

/// In-memory policy state backed by an injected persistence implementation.
pub struct Allowlist {
    store: Box<dyn AllowlistStore>,
    entries: Vec<AllowlistEntry>,
    events: Vec<AllowlistEvent>,
}

impl Allowlist {
    /// Loads an allowlist through the supplied storage boundary.
    pub fn open(store: impl AllowlistStore + 'static) -> Result<Self, IdentityError> {
        let loaded = store.load()?;
        Ok(Self {
            store: Box::new(store),
            entries: loaded.entries,
            events: loaded.events,
        })
    }

    /// Loads an allowlist JSON file below the caller-supplied per-user config root.
    pub fn open_in_config_root(config_root: &Path) -> Result<Self, IdentityError> {
        Self::open(FileAllowlistStore::new(allowlist_path(config_root)))
    }

    /// Checks a peer and queues new identities for owner approval when capacity permits.
    pub fn check(
        &mut self,
        identity: &PeerIdentity,
        now_unix_secs: u64,
    ) -> Result<AccessState, IdentityError> {
        validate_identity(identity)?;
        if let Some(index) = find_entry(&self.entries, identity) {
            let entry = &mut self.entries[index];
            entry.last_seen_unix_secs = entry.last_seen_unix_secs.max(now_unix_secs);
            let state = access_state(entry.state);
            self.persist()?;
            return Ok(state);
        }
        if self.entries.len() >= MAX_ALLOWLIST_ENTRIES
            || self
                .entries
                .iter()
                .filter(|entry| entry.state == ApprovalState::Pending)
                .count()
                >= MAX_PENDING_APPROVALS
        {
            if !self.events.contains(&AllowlistEvent::PendingQueueFull) {
                self.events.push(AllowlistEvent::PendingQueueFull);
            }
            return Ok(AccessState::Unknown);
        }
        self.entries.push(AllowlistEntry {
            node_id: identity.node_id.clone(),
            owner_login: identity.owner_login.clone(),
            label: identity
                .node_name
                .as_deref()
                .map(|name| bounded(name, MAX_LABEL_BYTES))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Unknown device".to_owned()),
            added_at_unix_secs: now_unix_secs,
            state: ApprovalState::Pending,
            last_seen_unix_secs: now_unix_secs,
        });
        self.persist()?;
        Ok(AccessState::Pending)
    }

    /// Approves an identity and persists its label and timestamps.
    pub fn approve(
        &mut self,
        identity: &PeerIdentity,
        label: &str,
        now_unix_secs: u64,
    ) -> Result<(), IdentityError> {
        self.set_decision(identity, label, now_unix_secs, ApprovalState::Approved)
    }

    /// Rejects an identity and persists the decision.
    pub fn reject(
        &mut self,
        identity: &PeerIdentity,
        label: &str,
        now_unix_secs: u64,
    ) -> Result<(), IdentityError> {
        self.set_decision(identity, label, now_unix_secs, ApprovalState::Rejected)
    }

    /// Removes an identity and returns whether a record existed.
    pub fn remove(&mut self, identity: &PeerIdentity) -> Result<bool, IdentityError> {
        validate_identity(identity)?;
        let previous = self.entries.len();
        self.entries.retain(|entry| !same_identity(entry, identity));
        let removed = self.entries.len() != previous;
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    /// Returns the current records.
    pub fn entries(&self) -> &[AllowlistEntry] {
        &self.entries
    }

    /// Returns recovery and queue-bound events accumulated since opening.
    pub fn events(&self) -> &[AllowlistEvent] {
        &self.events
    }

    /// Returns pending approvals in persisted order.
    pub fn pending(&self) -> impl Iterator<Item = &AllowlistEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.state == ApprovalState::Pending)
    }

    fn set_decision(
        &mut self,
        identity: &PeerIdentity,
        label: &str,
        now_unix_secs: u64,
        state: ApprovalState,
    ) -> Result<(), IdentityError> {
        validate_identity(identity)?;
        let index = find_entry(&self.entries, identity);
        let bounded_label = bounded(label, MAX_LABEL_BYTES);
        match index {
            Some(index) => {
                let entry = &mut self.entries[index];
                if !bounded_label.is_empty() {
                    entry.label = bounded_label;
                }
                entry.state = state;
                entry.last_seen_unix_secs = entry.last_seen_unix_secs.max(now_unix_secs);
            }
            None => {
                if self.entries.len() >= MAX_ALLOWLIST_ENTRIES {
                    return Err(IdentityError::Allowlist(
                        "allowlist entry limit reached".to_owned(),
                    ));
                }
                self.entries.push(AllowlistEntry {
                    node_id: identity.node_id.clone(),
                    owner_login: identity.owner_login.clone(),
                    label: if bounded_label.is_empty() {
                        "Unknown device".to_owned()
                    } else {
                        bounded_label
                    },
                    added_at_unix_secs: now_unix_secs,
                    state,
                    last_seen_unix_secs: now_unix_secs,
                });
            }
        }
        self.persist()
    }

    fn persist(&self) -> Result<(), IdentityError> {
        self.store.save(&self.entries)
    }
}

fn find_entry(entries: &[AllowlistEntry], identity: &PeerIdentity) -> Option<usize> {
    entries
        .iter()
        .position(|entry| same_identity(entry, identity))
}

fn same_identity(entry: &AllowlistEntry, identity: &PeerIdentity) -> bool {
    entry.node_id == identity.node_id && entry.owner_login == identity.owner_login
}

fn access_state(state: ApprovalState) -> AccessState {
    match state {
        ApprovalState::Approved => AccessState::Approved,
        ApprovalState::Pending => AccessState::Pending,
        ApprovalState::Rejected => AccessState::Rejected,
    }
}

fn validate_identity(identity: &PeerIdentity) -> Result<(), IdentityError> {
    if identity.node_id.is_empty() || identity.node_id.len() > MAX_ID_BYTES {
        return Err(IdentityError::InvalidIdentity);
    }
    if identity
        .owner_login
        .as_ref()
        .is_some_and(|owner| owner.len() > MAX_OWNER_BYTES)
    {
        return Err(IdentityError::InvalidIdentity);
    }
    Ok(())
}

fn validate_entries(entries: &[AllowlistEntry]) -> Result<(), IdentityError> {
    if entries.len() > MAX_ALLOWLIST_ENTRIES
        || entries
            .iter()
            .filter(|entry| entry.state == ApprovalState::Pending)
            .count()
            > MAX_PENDING_APPROVALS
    {
        return Err(IdentityError::Allowlist(
            "allowlist count exceeds configured bounds".to_owned(),
        ));
    }
    for entry in entries {
        if entry.node_id.is_empty()
            || entry.node_id.len() > MAX_ID_BYTES
            || entry
                .owner_login
                .as_ref()
                .is_some_and(|owner| owner.len() > MAX_OWNER_BYTES)
            || entry.label.len() > MAX_LABEL_BYTES
        {
            return Err(IdentityError::Allowlist(
                "allowlist field exceeds configured bounds".to_owned(),
            ));
        }
    }
    Ok(())
}

fn bounded(value: &str, maximum: usize) -> String {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}

fn io_error(error: std::io::Error) -> IdentityError {
    IdentityError::Io(error.kind().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(label: &str) -> PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "racc-identity-{label}-{}-{unique}",
            std::process::id()
        ));
        assert!(fs::create_dir_all(&path).is_ok());
        path
    }

    fn identity(id: &str, owner: Option<&str>, name: Option<&str>) -> PeerIdentity {
        PeerIdentity {
            node_id: id.to_owned(),
            owner_login: owner.map(str::to_owned),
            tags: vec!["tag:viewer".to_owned()],
            node_name: name.map(str::to_owned),
            addresses: Vec::new(),
        }
    }

    #[test]
    fn new_identity_is_pending_then_can_be_approved_rejected_and_removed() {
        let path = scratch_dir("policy");
        let result = Allowlist::open(FileAllowlistStore::new(allowlist_path(&path)));
        assert!(result.is_ok());
        let mut list = match result {
            Ok(value) => value,
            Err(_) => return,
        };
        let peer = identity("node-1", Some("owner-a"), Some("Desk"));
        assert_eq!(list.check(&peer, 100), Ok(AccessState::Pending));
        assert_eq!(list.pending().count(), 1);
        assert_eq!(list.check(&peer, 101), Ok(AccessState::Pending));
        assert_eq!(list.entries()[0].last_seen_unix_secs, 101);
        assert!(list.approve(&peer, "Approved desk", 102).is_ok());
        assert_eq!(list.check(&peer, 103), Ok(AccessState::Approved));
        assert!(list.reject(&peer, "Rejected desk", 104).is_ok());
        assert_eq!(list.check(&peer, 105), Ok(AccessState::Rejected));
        assert_eq!(list.remove(&peer), Ok(true));
        assert_eq!(list.remove(&peer), Ok(false));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn pending_approval_queue_is_bounded_and_rejects_overflow() {
        let root = scratch_dir("queue");
        let result = Allowlist::open(FileAllowlistStore::new(allowlist_path(&root)));
        assert!(result.is_ok());
        let mut list = match result {
            Ok(value) => value,
            Err(_) => return,
        };
        for index in 0..MAX_PENDING_APPROVALS {
            let peer = identity(&format!("node-{index}"), Some("owner-a"), None);
            assert_eq!(list.check(&peer, index as u64), Ok(AccessState::Pending));
        }
        let overflow = identity("node-overflow", Some("owner-a"), None);
        assert_eq!(list.check(&overflow, 999), Ok(AccessState::Unknown));
        assert_eq!(list.pending().count(), MAX_PENDING_APPROVALS);
        assert_eq!(list.entries().len(), MAX_PENDING_APPROVALS);
        assert!(list.events().contains(&AllowlistEvent::PendingQueueFull));
        let _ = fs::remove_dir_all(root);
    }
    #[test]
    fn corrupt_json_is_quarantined_and_returns_an_event() {
        let root = scratch_dir("corrupt");
        let file_path = allowlist_path(&root);
        assert!(fs::create_dir_all(file_path.parent().unwrap_or(&root)).is_ok());
        assert!(fs::write(&file_path, b"{not-json").is_ok());
        let result = Allowlist::open(FileAllowlistStore::new(file_path.clone()));
        assert!(result.is_ok());
        let list = match result {
            Ok(value) => value,
            Err(_) => return,
        };
        assert_eq!(list.entries().len(), 0);
        assert_eq!(list.events(), &[AllowlistEvent::CorruptFileQuarantined]);
        let parent = file_path.parent().unwrap_or(&root);
        assert!(fs::read_dir(parent)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".corrupt-")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_replacement_leaves_only_complete_json() {
        let root = scratch_dir("atomic");
        let path = allowlist_path(&root);
        let store = FileAllowlistStore::new(path.clone());
        assert!(store.save(&[]).is_ok());
        let first = fs::read(&path).unwrap_or_default();
        assert!(store.save(&[]).is_ok());
        let second = fs::read(&path).unwrap_or_default();
        assert_eq!(first, second);
        assert!(serde_json::from_slice::<DiskAllowlist>(&second).is_ok());
        let parent = path.parent().unwrap_or(&root);
        assert!(!fs::read_dir(parent)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".tmp-")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn file_format_contains_only_identity_label_timestamps_and_state() {
        let root = scratch_dir("format");
        let path = allowlist_path(&root);
        let store = FileAllowlistStore::new(path.clone());
        let peer = identity("node-opaque", Some("owner-a"), Some("Desk"));
        let entry = AllowlistEntry {
            node_id: peer.node_id,
            owner_login: peer.owner_login,
            label: "Desk".to_owned(),
            added_at_unix_secs: 1,
            state: ApprovalState::Approved,
            last_seen_unix_secs: 2,
        };
        assert!(store.save(&[entry]).is_ok());
        let json = fs::read_to_string(&path).unwrap_or_default();
        assert!(!json.contains("100.64."));
        assert!(!json.contains("address"));
        assert!(json.contains("node-opaque"));
        let _ = fs::remove_dir_all(root);
    }
}
