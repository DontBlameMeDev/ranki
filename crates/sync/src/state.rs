use crate::error::{Result, SyncError};
use crate::transport::DEFAULT_ENDPOINT;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::fs::File;
#[cfg(unix)]
use std::fs::Permissions;
use std::io::{ErrorKind, Result as IoResult, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const STATE_VERSION: u8 = 1;
#[cfg(unix)]
const STATE_MODE: u32 = 0o600;
/// Persisted sync state.
///
/// Only the host key and sync metadata are stored. Passwords never reach this
/// file. The `Debug` implementation redacts the host key.
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct SyncState {
    /// State format version.
    pub version: u8,
    /// Current `AnkiWeb` endpoint.
    pub endpoint: String,
    /// Long-lived host key.
    pub host_key: String,
    /// Last successful remote collection modification time in milliseconds.
    pub last_mod: Option<i64>,
    /// Local collection modification time the last successful sync covered.
    ///
    /// `AnkiWeb` stamps its own `mod` on an upload or download, so comparing
    /// the local file with `last_mod` alone reads every server re-stamp as a
    /// local change too.
    pub last_local_mod: Option<i64>,
    /// Last successful remote schema modification time in milliseconds.
    pub last_scm: Option<i64>,
    /// Last server USN observed during a successful sync.
    pub last_usn: i32,
    /// Last media USN observed during a successful sync.
    /// Last media USN observed during a successful sync.
    pub last_media_usn: i32,
    /// Last media change USN whose files are all present locally.
    ///
    /// A refresh can stop part way through a large library. The next run starts
    /// from this stamp instead of walking the whole manifest again.
    #[serde(default)]
    pub media_scan_usn: i32,
    /// A media refill started and has not completed.
    #[serde(default)]
    pub media_refill_needed: bool,
    /// Last successful sync time in Unix seconds.
    pub last_success: Option<i64>,
    /// Number of consecutive failed attempts.
    pub failures: u32,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            host_key: String::new(),
            last_mod: None,
            last_local_mod: None,
            last_scm: None,
            last_usn: 0,
            last_media_usn: 0,
            media_scan_usn: 0,
            media_refill_needed: false,
            last_success: None,
            failures: 0,
        }
    }
}

impl fmt::Debug for SyncState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncState")
            .field("version", &self.version)
            .field("endpoint", &self.endpoint)
            .field("host_key", &"<redacted>")
            .field("last_mod", &self.last_mod)
            .field("last_local_mod", &self.last_local_mod)
            .field("last_scm", &self.last_scm)
            .field("last_usn", &self.last_usn)
            .field("last_media_usn", &self.last_media_usn)
            .field("media_scan_usn", &self.media_scan_usn)
            .field("media_refill_needed", &self.media_refill_needed)
            .field("last_success", &self.last_success)
            .field("failures", &self.failures)
            .finish()
    }
}

impl SyncState {
    /// Reads the state file, returning defaults when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or has an unknown version.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(SyncError::State(error.to_string())),
        };
        let state: Self = serde_json::from_str(&text)
            .map_err(|error| SyncError::State(error.to_string()))?;
        if state.version != STATE_VERSION {
            return Err(SyncError::State(
                "unsupported state version".to_owned(),
            ));
        }
        Ok(state)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let mut file = tempfile::Builder::new()
            .prefix(".ranki-sync-")
            .suffix(".json")
            .tempfile_in(parent)?;
        restrict_permissions(file.as_file())?;
        {
            let writer = file.as_file_mut();
            serde_json::to_writer(&mut *writer, self)
                .map_err(|error| SyncError::State(error.to_string()))?;
            writer.flush()?;
            writer.sync_all()?;
        }
        file.persist(path)
            .map_err(|error| SyncError::State(error.to_string()))?;
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_permissions(file: &File) -> IoResult<()> {
    file.set_permissions(Permissions::from_mode(STATE_MODE))
}

#[cfg(not(unix))]
fn restrict_permissions(_: &File) -> IoResult<()> {
    Ok(())
}

/// Stores a host key atomically while preserving the rest of the state.
///
/// # Errors
///
/// Returns an error when the state file cannot be read or written.
pub fn set_host_key(path: &Path, key: &str) -> Result<()> {
    let mut state = SyncState::load(path)?;
    key.clone_into(&mut state.host_key);
    state.save(path)
}

/// Clears the host key while keeping the last sync stamps.
///
/// # Errors
///
/// Returns an error when an existing state file cannot be read or written.
pub fn clear_host_key(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut state = SyncState::load(path)?;
    state.host_key.clear();
    state.save(path)
}
