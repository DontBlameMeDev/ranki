use crate::error::{Result, SyncError};
use crate::media;
use crate::state::SyncState;
use crate::transport::{
    SyncMethod, SyncResponse, SyncTransport, meta_client_version,
    normalize_endpoint,
};
use crate::wire::{self, MAX_COLLECTION_BYTES, SYNC_VERSION};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::Builder;

const CLOCK_SKEW_SECONDS: i64 = 300;
const MAX_REDIRECTS: u8 = 3;
const MEDIA_DOWNLOAD_WORKERS: usize = 8;

/// Checkpoint attempts before an upload gives up on folding the log in.
const CHECKPOINT_TRIES: u8 = 3;
const CHECKPOINT_PAUSE: Duration = Duration::from_millis(50);

/// Username and password used only for a single login call.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    username: String,
    password: String,
}

impl Credentials {
    /// Build credentials from caller-provided strings.
    #[must_use]
    pub fn new(
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Collection metadata returned by `sync/meta`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteStatus {
    /// Server collection modification time in milliseconds.
    pub mod_time: i64,
    /// Server schema modification time in milliseconds.
    pub schema_mod_time: i64,
    /// Server USN.
    pub usn: i32,
    /// Server Unix time in seconds.
    pub server_time: i64,
    /// Whether the remote collection has no cards.
    pub empty: bool,
    /// Server media USN.
    pub media_usn: i32,
    /// Optional server message.
    pub message: Option<String>,
}

/// What [`SyncClient::sync`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    /// No host key and no credentials were supplied.
    Disabled,
    /// Both sides already match the last successful sync.
    NoChanges,
    /// The local collection was uploaded.
    Uploaded,
    /// The remote collection replaced the local one.
    Downloaded,
    /// A one-way sync would risk data loss.
    Conflict {
        /// Reason the automatic sync stopped.
        reason: String,
    },
    /// Transient failures remain after bounded retries.
    RetryLater {
        /// Reason the sync should be retried later.
        reason: String,
    },
}

impl SyncOutcome {
    /// Whether the collection sync left both copies in step, so media can
    /// follow.
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        matches!(self, Self::NoChanges | Self::Uploaded | Self::Downloaded)
    }
}

/// How far a media refresh has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaProgress {
    /// Media files downloaded so far.
    pub done: usize,
    /// Media files this refresh found missing locally.
    pub total: usize,
}

/// Receives media progress from a background thread.
pub type MediaProgressFn = Arc<dyn Fn(MediaProgress) + Send + Sync>;

/// Minimal `AnkiWeb` v11 collection sync client.
///
/// The client stores only the host key and sync metadata in its state file.
/// [`SyncClient::sync_media`] adds or replaces local media files. Media is
/// never deleted.
pub struct SyncClient<T: SyncTransport> {
    transport: T,
    state: SyncState,
    collection: PathBuf,
    media_dir: PathBuf,
    state_path: PathBuf,
    media_progress: Option<MediaProgressFn>,
    expected_local_mod: Option<i64>,
}

impl<T: SyncTransport> SyncClient<T> {
    /// Load local state and bind the client to a collection and media directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the state file is unreadable or the stored
    /// endpoint is not a usable HTTPS URL.
    #[expect(
        clippy::too_many_arguments,
        reason = "paths and transport are supplied together"
    )]
    pub fn new(
        transport: T,
        collection: &Path,
        media_dir: &Path,
        state_path: &Path,
    ) -> Result<Self> {
        let mut transport = transport;
        let state = SyncState::load(state_path)?;
        transport.set_endpoint(&state.endpoint)?;
        transport.set_host_key(&state.host_key);
        Ok(Self {
            transport,
            state,
            collection: collection.to_path_buf(),
            media_dir: media_dir.to_path_buf(),
            state_path: state_path.to_path_buf(),
            media_progress: None,
            expected_local_mod: None,
        })
    }

    /// Transport used by this client.
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Reports media refresh progress to a background caller.
    pub fn set_media_progress(&mut self, report: MediaProgressFn) {
        self.media_progress = Some(report);
    }

    /// Current persisted state.
    #[must_use]
    pub const fn state(&self) -> &SyncState {
        &self.state
    }

    /// Collection path supplied by the caller.
    #[must_use]
    pub fn collection(&self) -> &Path {
        &self.collection
    }

    /// Media directory supplied by the caller. Full download adds or replaces
    /// files here; it never deletes them.
    #[must_use]
    pub fn media_dir(&self) -> &Path {
        &self.media_dir
    }

    /// Exchange credentials for a host key and persist only that key.
    ///
    /// # Errors
    ///
    /// Returns an error when the login request fails or the state file cannot
    /// be written.
    pub fn login(&mut self, credentials: &Credentials) -> Result<()> {
        self.transport.set_host_key("");
        let request = HostKeyRequest {
            u: &credentials.username,
            p: &credentials.password,
        };
        let body = wire::frame_json(&request)?;
        let response = self.send(SyncMethod::HostKey, &body)?;
        let bytes = wire::decode(&response)?;
        let reply: HostKeyReply = serde_json::from_slice(&bytes)?;
        if reply.key.is_empty() {
            return Err(SyncError::Auth);
        }
        let key = reply.key;
        self.state.host_key.clone_from(&key);
        self.state.failures = 0;
        self.transport.set_host_key(&key);
        self.state.save(&self.state_path)?;
        Ok(())
    }

    /// Read and check remote collection metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials are missing, the request fails, the
    /// response is invalid, or the clocks differ by more than five minutes.
    pub fn status(&mut self) -> Result<RemoteStatus> {
        self.require_host_key()?;
        let client_version = meta_client_version();
        let request = MetaRequest {
            v: SYNC_VERSION,
            cv: &client_version,
        };
        let body = wire::frame_json(&request)?;
        let response = self.send(SyncMethod::Meta, &body)?;
        let bytes = wire::decode(&response)?;
        let meta: MetaWire = serde_json::from_slice(&bytes)?;
        let message = meta.msg.clone();
        if !meta.flags.cont.is_on() {
            return Err(SyncError::ServerMessage(
                message.unwrap_or_else(|| "server stopped sync".to_owned()),
            ));
        }
        let limit = u64::try_from(CLOCK_SKEW_SECONDS).unwrap_or_default();
        if now_secs()?.abs_diff(meta.ts) > limit {
            return Err(SyncError::ClockSkew);
        }
        Ok(RemoteStatus {
            mod_time: meta.mod_time,
            schema_mod_time: meta.scm,
            usn: meta.usn,
            server_time: meta.ts,
            empty: meta.flags.empty.is_on(),
            media_usn: meta.media_usn,
            message,
        })
    }

    /// Upload the whole local collection. This is an explicit one-way choice.
    ///
    /// # Errors
    ///
    /// Returns an error when the collection cannot be prepared, the upload
    /// fails, or the state file cannot be written.
    pub fn full_upload(&mut self) -> Result<()> {
        self.require_host_key()?;
        let (info, bytes) = self.prepare_collection()?;
        let body = wire::frame(&bytes)?;
        let response = self.send(SyncMethod::Upload, &body)?;
        let reply = wire::decode(&response)?;
        if String::from_utf8_lossy(&reply).trim() != "OK" {
            return Err(SyncError::ServerMessage(
                "upload was not accepted".to_owned(),
            ));
        }
        self.adopt_server_stamps(info.mod_time)?;
        self.record_success()?;
        Ok(())
    }

    /// Download the whole remote collection. This is an explicit one-way choice.
    ///
    /// Media is not part of this call. Run [`Self::sync_media`] afterwards, so
    /// the collection lands without waiting for a large media library.
    ///
    /// # Errors
    ///
    /// Returns an error when the download fails, the temporary collection is
    /// invalid, or the atomic replacement fails.
    pub fn full_download(&mut self) -> Result<()> {
        self.require_host_key()?;
        let expected_local_mod = self.expected_local_mod.take();
        let body = wire::frame(b"{}")?;
        let response = self.send(SyncMethod::Download, &body)?;
        let bytes = wire::decode(&response)?;
        if bytes.is_empty()
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                >= MAX_COLLECTION_BYTES
        {
            return Err(SyncError::TooLarge);
        }
        if let Some(expected) = expected_local_mod
            && read_local_info(&self.collection)?.mod_time != expected
        {
            return Err(SyncError::Conflict(String::from(
                "local collection changed during download",
            )));
        }
        let parent = self.collection.parent().unwrap_or_else(|| Path::new("."));
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
        let mut file = Builder::new()
            .prefix(".ranki-sync-")
            .suffix(".anki2")
            .tempfile_in(parent)?;
        {
            let writer = file.as_file_mut();
            writer.write_all(&bytes)?;
            writer.flush()?;
        }
        let info = validate_download(file.path())?;
        file.as_file().sync_all()?;
        self.backup_collection()?;
        file.persist(&self.collection)
            .map_err(|error| SyncError::Io(error.to_string()))?;
        self.adopt_download_stamps(info);
        self.record_success()?;
        Ok(())
    }

    /// Refreshes local media from `AnkiWeb`.
    ///
    /// Callers run this after the collection sync, so a large media library
    /// never holds up the collection itself. Only changes newer than the last
    /// checkpoint are fetched, unless the media folder is empty and needs a
    /// refill. Each completed group of changes advances the checkpoint, so a
    /// refresh that is closed part way through resumes from there.
    ///
    /// Files already present with the server's hash are left untouched.
    /// Batches, which `AnkiWeb` caps at 25 files, download in parallel when the
    /// transport can clone itself.
    ///
    /// # Errors
    ///
    /// Returns an error when a media request fails, a ZIP cannot be read, or
    /// the state file cannot be written.
    pub fn sync_media(&mut self) -> Result<()> {
        self.require_host_key()?;
        let server_usn = self.media_server_usn()?;
        if server_usn <= 0 {
            return Ok(());
        }
        let empty = self.media_dir_is_empty();
        let matching = server_usn == self.state.last_media_usn;
        if matching && !self.state.media_refill_needed && !empty {
            return Ok(());
        }
        if matching && !self.state.media_refill_needed {
            self.state.media_refill_needed = true;
            self.state.save(&self.state_path)?;
        }
        fs::create_dir_all(&self.media_dir)?;

        let start_usn = if empty
            || self.state.media_refill_needed
            || self.state.media_scan_usn > server_usn
        {
            0
        } else {
            self.state.media_scan_usn
        };
        if start_usn == server_usn {
            self.state.last_media_usn = server_usn;
            self.state.media_scan_usn = server_usn;
            self.state.media_refill_needed = false;
            self.state.save(&self.state_path)?;
            return Ok(());
        }
        if start_usn == 0 {
            self.state.media_scan_usn = 0;
            self.state.media_refill_needed = true;
            self.state.save(&self.state_path)?;
        }

        let missing = self.missing_media(start_usn, server_usn)?;
        let mut plan = DownloadPlan::new(&missing)?;
        self.report_media(MediaProgress {
            done: 0,
            total: plan.total,
        });
        let mut next_group = 0_usize;
        advance_media_checkpoint(self, &plan.groups, &mut next_group)?;
        let workers = (0..MEDIA_DOWNLOAD_WORKERS)
            .filter_map(|_| self.transport.media_download_worker())
            .collect::<Vec<_>>();
        if workers.len() > 1 && plan.bodies.len() > 1 {
            self.download_media_parallel(&mut plan, workers, &mut next_group)?;
        } else {
            self.download_media_sequential(&mut plan, &mut next_group)?;
        }
        self.state.last_media_usn = server_usn;
        self.state.media_scan_usn = server_usn;
        self.state.media_refill_needed = false;
        self.state.save(&self.state_path)?;
        Ok(())
    }

    fn download_media_sequential(
        &mut self,
        plan: &mut DownloadPlan,
        next_group: &mut usize,
    ) -> Result<()> {
        let mut done = 0_usize;
        let bodies = &plan.bodies;
        let batch_groups = &plan.batch_groups;
        let groups = &mut plan.groups;
        for (index, body) in bodies.iter().enumerate() {
            let response = self.send(SyncMethod::MediaDownload, body)?;
            let written = self.extract(&response)?;
            done = done.saturating_add(written);
            self.report_media(MediaProgress {
                done,
                total: plan.total,
            });
            finish_media_batch(
                self,
                groups,
                batch_group(batch_groups, index)?,
                next_group,
            )?;
        }
        Ok(())
    }

    #[expect(
        clippy::excessive_nesting,
        clippy::too_many_arguments,
        reason = "worker setup and result handling stay next to the scope"
    )]
    fn download_media_parallel(
        &mut self,
        plan: &mut DownloadPlan,
        workers: Vec<Box<dyn SyncTransport>>,
        next_group: &mut usize,
    ) -> Result<()> {
        let shared = DownloadShared {
            bodies: &plan.bodies,
            next: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            endpoint: self.state.endpoint.clone(),
        };
        let batch_groups = &plan.batch_groups;
        let groups = &mut plan.groups;
        let total = plan.total;
        let mut done = 0_usize;
        let mut redirected = None;
        thread::scope(|scope| -> Result<()> {
            let (sender, receiver) = sync_channel(workers.len());
            let shared = &shared;
            for transport in workers {
                let worker_sender = sender.clone();
                scope.spawn(move || {
                    download_worker(transport, shared, &worker_sender);
                });
            }
            drop(sender);
            for result in receiver {
                let (index, response, endpoint) = result?;
                let written = self.extract(&response)?;
                done = done.saturating_add(written);
                self.report_media(MediaProgress { done, total });
                finish_media_batch(
                    self,
                    groups,
                    batch_group(batch_groups, index)?,
                    next_group,
                )?;
                if redirected.is_none() && endpoint != self.state.endpoint {
                    redirected = Some(endpoint);
                }
            }
            Ok(())
        })?;

        if let Some(endpoint) = redirected {
            self.state.endpoint = endpoint;
            self.state.save(&self.state_path)?;
        }
        Ok(())
    }

    fn checkpoint_media(&mut self, usn: i32) -> Result<()> {
        if usn <= self.state.media_scan_usn {
            return Ok(());
        }
        self.state.media_scan_usn = usn;
        self.state.save(&self.state_path)
    }

    fn extract(&self, response: &SyncResponse) -> Result<usize> {
        let zip = wire::decode(response)?;
        media::extract_files(&self.media_dir, &zip)
    }

    fn media_server_usn(&mut self) -> Result<i32> {
        let body = wire::frame_json(&media::BeginRequest {
            v: meta_client_version(),
        })?;
        let response = self.send(SyncMethod::MediaBegin, &body)?;
        media::parse_begin(&wire::decode(&response)?)
    }

    /// Collects groups of names that are missing locally or differ from the
    /// server, paired with the highest USN in each manifest batch.
    #[expect(
        clippy::excessive_nesting,
        reason = "the manifest walk keeps its cursor and errors local"
    )]
    fn missing_media(
        &mut self,
        start_usn: i32,
        server_usn: i32,
    ) -> Result<Vec<MediaChangesGroup>> {
        let mut groups = Vec::new();
        let mut last_usn = start_usn;
        while last_usn < server_usn {
            let batch = self.media_changes(last_usn)?;
            if batch.is_empty() {
                break;
            }
            let highest = batch
                .iter()
                .map(|change| change.usn)
                .max()
                .unwrap_or_default();
            if highest <= last_usn {
                return Err(SyncError::Decode(
                    "media changes did not advance".to_owned(),
                ));
            }
            last_usn = highest;
            let names = self.missing_names(&batch)?;
            groups.push(MediaChangesGroup {
                end_usn: highest,
                names,
            });
        }
        Ok(groups)
    }

    fn media_changes(
        &mut self,
        last_usn: i32,
    ) -> Result<Vec<media::MediaChange>> {
        let body = wire::frame_json(&media::ChangesRequest { last_usn })?;
        let response = self.send(SyncMethod::MediaChanges, &body)?;
        media::parse_changes(&wire::decode(&response)?)
    }

    fn missing_names(
        &self,
        batch: &[media::MediaChange],
    ) -> Result<Vec<String>> {
        let names = batch
            .iter()
            .map(|change| self.missing_name(change))
            .collect::<Result<Vec<_>>>()?;
        Ok(names.into_iter().flatten().collect())
    }

    fn missing_name(
        &self,
        change: &media::MediaChange,
    ) -> Result<Option<String>> {
        if change.sha1.is_empty() {
            return Ok(None);
        }
        let Some(name) = media::safe_media_name(&change.fname) else {
            return Ok(None);
        };
        if media::local_matches_sha1(&self.media_dir, name, &change.sha1)? {
            return Ok(None);
        }
        Ok(Some(name.to_owned()))
    }

    fn report_media(&self, progress: MediaProgress) {
        if let Some(report) = &self.media_progress {
            report(progress);
        }
    }

    fn media_dir_is_empty(&self) -> bool {
        match fs::read_dir(&self.media_dir) {
            Ok(mut entries) => entries.next().is_none(),
            Err(error) => error.kind() == ErrorKind::NotFound,
        }
    }

    /// Run one safe automatic sync.
    ///
    /// Credentials are used for login only when no host key is stored. The
    /// method never chooses a one-way sync when both sides changed or when the
    /// local collection may be newer than the server copy.
    ///
    /// # Errors
    ///
    /// Returns an error when login or metadata retrieval fails for a
    /// non-retryable reason.
    pub fn sync(
        &mut self,
        credentials: Option<Credentials>,
    ) -> Result<SyncOutcome> {
        if self.state.host_key.is_empty() {
            match credentials {
                Some(supplied) => self.login(&supplied)?,
                None => return Ok(SyncOutcome::Disabled),
            }
        }
        match self.decide_and_sync() {
            Ok(outcome) => Ok(outcome),
            Err(error) if error.is_retryable() => {
                self.state.failures = self.state.failures.saturating_add(1);
                self.state.save(&self.state_path)?;
                Ok(SyncOutcome::RetryLater {
                    reason: error.to_string(),
                })
            }
            Err(error) => Err(error),
        }
    }

    fn decide_and_sync(&mut self) -> Result<SyncOutcome> {
        let status = self.status()?;
        let local = read_local_info(&self.collection)?;
        self.expected_local_mod = Some(local.mod_time);
        let last_mod = self.state.last_mod;
        let last_scm = self.state.last_scm;

        let Some(last) = last_mod else {
            return self.sync_without_stamp(&status, local);
        };
        let schema_matches = match last_scm {
            Some(scm) => scm == status.schema_mod_time,
            None => local.scm == status.schema_mod_time,
        };
        if !schema_matches {
            return Ok(conflict("the schema changed since the last sync"));
        }
        let local_changed =
            local.mod_time != self.state.last_local_mod.unwrap_or(last);
        let remote_changed = status.mod_time != last;
        if local_changed && remote_changed {
            return Ok(conflict(
                "both collections changed since the last sync",
            ));
        }
        if !local_changed && !remote_changed {
            self.state.failures = 0;
            self.state.last_success = Some(now_secs()?);
            self.state.save(&self.state_path)?;
            return Ok(SyncOutcome::NoChanges);
        }
        if local_changed {
            self.full_upload()?;
            return Ok(SyncOutcome::Uploaded);
        }
        if status.mod_time < local.mod_time {
            return Ok(conflict(
                "local collection is newer than the server copy",
            ));
        }
        self.full_download()?;
        Ok(SyncOutcome::Downloaded)
    }

    fn sync_without_stamp(
        &mut self,
        status: &RemoteStatus,
        local: LocalInfo,
    ) -> Result<SyncOutcome> {
        match (status.empty, local.cards) {
            (_, 0) if status.empty => {
                self.record_success()?;
                Ok(SyncOutcome::NoChanges)
            }
            (false, 0) => {
                self.full_download()?;
                Ok(SyncOutcome::Downloaded)
            }
            _ => Ok(conflict(
                "choose whether to keep the local or AnkiWeb collection",
            )),
        }
    }

    const fn require_host_key(&self) -> Result<()> {
        if self.state.host_key.is_empty() {
            Err(SyncError::NoCredentials)
        } else {
            Ok(())
        }
    }

    /// Adopts the server's own stamps after a full upload or download.
    ///
    /// `AnkiWeb` re-stamps `col.mod` when it stores a collection, so the value
    /// the client sent is not the value the next `sync/meta` reports. Recording
    /// the server's stamp, next to the local one it covers, keeps the next
    /// decision from reading that re-stamp as a second, remote change.
    fn adopt_server_stamps(&mut self, local_mod: i64) -> Result<()> {
        let status = self.status()?;
        self.state.last_mod = Some(status.mod_time);
        self.state.last_scm = Some(status.schema_mod_time);
        self.state.last_usn = status.usn;
        self.state.last_local_mod = Some(local_mod);
        Ok(())
    }
    const fn adopt_download_stamps(&mut self, info: LocalInfo) {
        self.state.last_mod = Some(info.mod_time);
        self.state.last_scm = Some(info.scm);
        self.state.last_usn = info.usn;
        self.state.last_local_mod = Some(info.mod_time);
    }

    fn record_success(&mut self) -> Result<()> {
        self.state.last_success = Some(now_secs()?);
        self.state.failures = 0;
        self.state.save(&self.state_path)
    }

    fn send(
        &mut self,
        method: SyncMethod,
        body: &[u8],
    ) -> Result<SyncResponse> {
        let request = Request { method, body };
        let (response, redirected) = send_transport(
            &mut self.transport,
            &mut self.state.endpoint,
            request,
        )?;
        if redirected {
            self.state.save(&self.state_path)?;
        }
        Ok(response)
    }

    fn prepare_collection(&self) -> Result<(LocalInfo, Vec<u8>)> {
        let mut connection = Connection::open(&self.collection)?;
        register_collations(&connection)?;
        {
            let transaction = connection.transaction()?;
            prepare_transaction(&transaction)?;
            transaction.commit()?;
        }
        checkpoint(&connection)?;
        let info = local_info(&connection)?;
        connection
            .close()
            .map_err(|(_, error)| SyncError::Database(error.to_string()))?;
        let bytes = fs::read(&self.collection)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            >= MAX_COLLECTION_BYTES
        {
            return Err(SyncError::TooLarge);
        }
        Ok((info, bytes))
    }

    fn backup_collection(&self) -> Result<()> {
        if !self.collection.exists() {
            return Ok(());
        }
        let backup = self.collection.with_extension("ranki-sync.bak");
        {
            let connection = Connection::open(&self.collection)?;
            checkpoint(&connection)?;
        }
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        if fs::hard_link(&self.collection, &backup).is_err() {
            fs::copy(&self.collection, &backup)?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct HostKeyRequest<'a> {
    u: &'a str,
    p: &'a str,
}

#[derive(Deserialize)]
struct HostKeyReply {
    key: String,
}

#[derive(Serialize)]
struct MetaRequest<'a> {
    v: u8,
    cv: &'a str,
}

#[derive(Deserialize)]
struct MetaWire {
    #[serde(rename = "mod")]
    mod_time: i64,
    scm: i64,
    usn: i32,
    ts: i64,
    #[serde(flatten)]
    flags: MetaFlags,
    #[serde(default)]
    msg: Option<String>,
    #[serde(rename = "media_usn", default)]
    media_usn: i32,
}

#[derive(Clone, Copy, Deserialize)]
struct MetaFlags {
    #[serde(default)]
    empty: SyncFlag,
    #[serde(default = "cont_true")]
    cont: SyncFlag,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(from = "bool")]
enum SyncFlag {
    On,
    #[default]
    Off,
}

impl From<bool> for SyncFlag {
    fn from(value: bool) -> Self {
        if value { Self::On } else { Self::Off }
    }
}

impl SyncFlag {
    const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

const fn cont_true() -> SyncFlag {
    SyncFlag::On
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LocalInfo {
    mod_time: i64,
    scm: i64,
    usn: i32,
    cards: u64,
}

/// One framed request, ready to send.
#[derive(Clone, Copy)]
struct Request<'a> {
    method: SyncMethod,
    body: &'a [u8],
}

fn send_transport<T: SyncTransport + ?Sized>(
    transport: &mut T,
    endpoint: &mut String,
    request: Request<'_>,
) -> Result<(SyncResponse, bool)> {
    let starting_endpoint = endpoint.clone();
    let mut redirects = 0_u8;
    loop {
        let response = attempt_transport(transport, request)?;
        if response.status != 308 {
            ensure_success(&response, request.method)?;
            return Ok((response, *endpoint != starting_endpoint));
        }
        follow_redirect(transport, endpoint, &response)?;
        redirects = redirects.saturating_add(1);
        if redirects > MAX_REDIRECTS {
            return Err(SyncError::TooManyRedirects);
        }
    }
}

fn follow_redirect<T: SyncTransport + ?Sized>(
    transport: &mut T,
    endpoint: &mut String,
    response: &SyncResponse,
) -> Result<()> {
    let location = response.header("location").ok_or_else(|| {
        SyncError::Network("redirect response has no location".to_owned())
    })?;
    let redirected_endpoint = normalize_endpoint(location)?;
    transport.set_endpoint(&redirected_endpoint)?;
    *endpoint = redirected_endpoint;
    Ok(())
}

fn attempt_transport<T: SyncTransport + ?Sized>(
    transport: &mut T,
    request: Request<'_>,
) -> Result<SyncResponse> {
    let policy = transport.retry_policy();
    let mut attempt = 0_u8;
    let Request { method, body } = request;
    loop {
        let result = match method {
            SyncMethod::HostKey => transport.host_key(body),
            SyncMethod::Meta => transport.meta(body),
            SyncMethod::Upload => transport.upload(body),
            SyncMethod::Download => transport.download(body),
            SyncMethod::MediaBegin => transport.media_begin(body),
            SyncMethod::MediaChanges => transport.media_changes(body),
            SyncMethod::MediaDownload => transport.media_download(body),
        };
        let retry =
            is_retryable_result(&result, method) && attempt < policy.retries;
        if !retry {
            return result;
        }
        let delay = policy
            .backoff
            .saturating_mul(u32::from(attempt).saturating_add(1));
        sleep(delay);
        attempt = attempt.saturating_add(1);
    }
}

/// One manifest batch of files to download, tagged with its last USN.
struct MediaChangesGroup {
    end_usn: i32,
    names: Vec<String>,
}

/// Download state for one manifest batch.
struct MediaGroup {
    end_usn: i32,
    remaining: usize,
    done: bool,
}

/// Media batches and their framed requests for one refresh.
struct DownloadPlan {
    bodies: Vec<Vec<u8>>,
    batch_groups: Vec<usize>,
    groups: Vec<MediaGroup>,
    total: usize,
}

impl DownloadPlan {
    #[expect(
        clippy::excessive_nesting,
        reason = "each manifest group builds its own framed batches"
    )]
    fn new(missing: &[MediaChangesGroup]) -> Result<Self> {
        let mut bodies = Vec::new();
        let mut batch_groups = Vec::new();
        let mut groups = Vec::new();
        let mut total = 0_usize;
        for (index, group) in missing.iter().enumerate() {
            let count = group.names.len().div_ceil(media::DOWNLOAD_BATCH);
            for files in group.names.chunks(media::DOWNLOAD_BATCH) {
                bodies.push(wire::frame_json(&media::DownloadRequest {
                    files: files.to_vec(),
                })?);
                batch_groups.push(index);
            }
            total = total.saturating_add(group.names.len());
            groups.push(MediaGroup {
                end_usn: group.end_usn,
                remaining: count,
                done: count == 0,
            });
        }
        Ok(Self {
            bodies,
            batch_groups,
            groups,
            total,
        })
    }
}

fn batch_group(groups: &[usize], index: usize) -> Result<usize> {
    groups.get(index).copied().ok_or_else(|| {
        SyncError::Decode("media batch has no manifest group".to_owned())
    })
}

/// State shared by the media download workers.
struct DownloadShared<'a> {
    bodies: &'a [Vec<u8>],
    next: AtomicUsize,
    failed: AtomicBool,
    endpoint: String,
}

type DownloadResult = Result<(usize, SyncResponse, String)>;

fn download_worker(
    mut transport: Box<dyn SyncTransport>,
    shared: &DownloadShared<'_>,
    sender: &SyncSender<DownloadResult>,
) {
    let mut endpoint = shared.endpoint.clone();
    while !shared.failed.load(Ordering::Relaxed) {
        let index = shared.next.fetch_add(1, Ordering::Relaxed);
        let Some(body) = shared.bodies.get(index) else {
            break;
        };
        let request = Request {
            method: SyncMethod::MediaDownload,
            body,
        };
        let sent = match send_transport(&mut *transport, &mut endpoint, request)
        {
            Ok((response, _)) => {
                sender.send(Ok((index, response, endpoint.clone())))
            }
            Err(error) => {
                shared.failed.store(true, Ordering::Relaxed);
                sender.send(Err(error))
            }
        };
        if sent.is_err() {
            break;
        }
    }
}

fn advance_media_checkpoint<T: SyncTransport>(
    client: &mut SyncClient<T>,
    groups: &[MediaGroup],
    next_group: &mut usize,
) -> Result<()> {
    while let Some(group) = groups.get(*next_group) {
        if !group.done {
            break;
        }
        client.checkpoint_media(group.end_usn)?;
        *next_group = next_group.saturating_add(1);
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "checkpoint state travels with the media refresh"
)]
fn finish_media_batch<T: SyncTransport>(
    client: &mut SyncClient<T>,
    groups: &mut [MediaGroup],
    group_index: usize,
    next_group: &mut usize,
) -> Result<()> {
    let Some(group) = groups.get_mut(group_index) else {
        return Err(SyncError::Decode(
            "media batch has no manifest group".to_owned(),
        ));
    };
    group.remaining = group.remaining.saturating_sub(1);
    if group.remaining == 0 {
        group.done = true;
    }
    advance_media_checkpoint(client, groups, next_group)
}

fn conflict(reason: &str) -> SyncOutcome {
    SyncOutcome::Conflict {
        reason: reason.to_owned(),
    }
}

fn is_retryable_result(
    result: &Result<SyncResponse>,
    method: SyncMethod,
) -> bool {
    match result {
        Ok(response) => {
            is_server_error(response.status) && method != SyncMethod::Upload
        }
        Err(error) => error.is_retryable() && method != SyncMethod::Upload,
    }
}

fn ensure_success(response: &SyncResponse, method: SyncMethod) -> Result<()> {
    if (200..300).contains(&response.status) {
        Ok(())
    } else {
        Err(map_status(response.status, &response.body, method))
    }
}

fn prepare_transaction(transaction: &Transaction<'_>) -> Result<()> {
    if table_exists(transaction, "graves")? {
        transaction.execute("DELETE FROM graves", [])?;
    }
    for table in ["notes", "cards", "revlog"] {
        if table_exists(transaction, table)? {
            let statement =
                format!("UPDATE {table} SET usn = 0 WHERE usn = -1");
            transaction.execute(&statement, [])?;
        }
    }
    let now = now_ms()?;
    transaction.execute(
        "UPDATE col SET usn = usn + 1, scm = ?1, ls = ?1 WHERE id = 1",
        params![now],
    )?;
    Ok(())
}

/// Folds the write-ahead log into the collection file before it is read.
///
/// A blocked checkpoint leaves committed rows in the log only, and the file
/// read that follows would then carry an older collection. The window keeps
/// its own connection open, so a checkpoint can be briefly blocked by a
/// reader; retry a few times before calling the upload unsafe.
fn checkpoint(connection: &Connection) -> Result<()> {
    for attempt in 0..CHECKPOINT_TRIES {
        let busy: i64 = connection.query_row(
            "PRAGMA wal_checkpoint(TRUNCATE)",
            [],
            |row| row.get(0),
        )?;
        if busy == 0 {
            return Ok(());
        }
        if attempt.saturating_add(1) < CHECKPOINT_TRIES {
            sleep(CHECKPOINT_PAUSE);
        }
    }
    Err(SyncError::Database("wal checkpoint was busy".to_owned()))
}

fn read_local_info(path: &Path) -> Result<LocalInfo> {
    if !path.exists() {
        return Ok(LocalInfo::default());
    }
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    register_collations(&connection)?;
    local_info(&connection)
}

fn register_collations(connection: &Connection) -> Result<()> {
    connection.create_collation("unicase", |left, right| {
        left.to_lowercase().cmp(&right.to_lowercase())
    })?;
    Ok(())
}

fn local_info(connection: &Connection) -> Result<LocalInfo> {
    let (mod_time, scm, usn): (i64, i64, i32) = connection.query_row(
        "SELECT mod, scm, usn FROM col WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let cards = if table_exists(connection, "cards")? {
        let count: i64 =
            connection.query_row("SELECT count(*) FROM cards", [], |row| {
                row.get(0)
            })?;
        u64::try_from(count).unwrap_or_default()
    } else {
        0
    };
    Ok(LocalInfo {
        mod_time,
        scm,
        usn,
        cards,
    })
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool> {
    let found = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()?;
    Ok(found.is_some())
}

fn validate_download(path: &Path) -> Result<LocalInfo> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    register_collations(&connection)?;
    let check: String =
        connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if !check.eq_ignore_ascii_case("ok") {
        return Err(SyncError::Database(
            "downloaded collection failed quick_check".to_owned(),
        ));
    }
    let info = local_info(&connection)?;
    connection.execute("UPDATE col SET ls = mod WHERE id = 1", [])?;
    connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    })?;
    connection
        .close()
        .map_err(|(_, error)| SyncError::Database(error.to_string()))?;
    Ok(info)
}

fn map_status(status: u16, body: &[u8], method: SyncMethod) -> SyncError {
    let message = server_message(body);
    match status {
        401 | 403 => SyncError::Auth,
        400 => SyncError::DatabaseCheck(message),
        409 => SyncError::Conflict(message),
        501 => SyncError::ClientTooOld,
        500..=599 => SyncError::Server { status, message },
        _ => {
            if method == SyncMethod::Upload && message.is_empty() {
                SyncError::ServerMessage("upload was not accepted".to_owned())
            } else {
                SyncError::ServerMessage(message)
            }
        }
    }
}

fn server_message(body: &[u8]) -> String {
    String::from_utf8_lossy(body)
        .trim()
        .chars()
        .take(256)
        .collect()
}

fn is_server_error(status: u16) -> bool {
    (500..600).contains(&status)
}

fn unix_duration() -> Result<Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| SyncError::State(error.to_string()))
}

fn now_secs() -> Result<i64> {
    let seconds = unix_duration()?.as_secs();
    i64::try_from(seconds)
        .map_err(|_| SyncError::State("clock is out of range".to_owned()))
}

fn now_ms() -> Result<i64> {
    let millis = unix_duration()?.as_millis();
    i64::try_from(millis)
        .map_err(|_| SyncError::State("clock is out of range".to_owned()))
}
