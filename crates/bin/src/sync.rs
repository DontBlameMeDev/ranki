#![expect(
    clippy::redundant_pub_crate,
    reason = "helpers are shared only with the crate root"
)]
//! `AnkiWeb` sync wiring for the command line binary.
//!
//! Credentials come from the environment or a private host-key file. They are
//! never stored in config, printed, or included in a `Debug` implementation.

use std::env;
use std::fmt;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

use ranki_model::Config;
use ranki_model::Settings;
use ranki_sync::Credentials;
use ranki_sync::HttpTransport;
#[cfg(test)]
use ranki_sync::MockTransport;
use ranki_sync::RetryPolicy;
use ranki_sync::SyncClient;
use ranki_sync::SyncError;
use ranki_sync::SyncOutcome;
use ranki_sync::SyncResponse;
use ranki_sync::SyncState;
use ranki_sync::SyncTransport;
use ranki_sync::TransportConfig;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

#[cfg(unix)]
const PRIVATE_BITS: u32 = 0o077;

/// Credentials loaded for one process. Passwords are only held for login.
pub(super) struct SyncAuth {
    host_key: Option<String>,
    login: Option<Credentials>,
}

/// Environment values used by [`load_auth`], kept separate for testing.
pub(super) struct SyncEnv {
    pub username: Option<String>,
    pub password: Option<String>,
    pub hkey: Option<String>,
}

/// Transport that builds its HTTP client on the first sync request.
pub(super) struct LazyTransport<T> {
    factory: Box<dyn FnMut() -> T + Send>,
    transport: Option<T>,
    retry_policy: RetryPolicy,
}

impl<T> LazyTransport<T> {
    fn new(
        factory: impl FnMut() -> T + Send + 'static,
        retry_policy: RetryPolicy,
    ) -> Self {
        Self {
            factory: Box::new(factory),
            transport: None,
            retry_policy,
        }
    }

    fn transport(&mut self) -> &mut T {
        let factory = &mut self.factory;
        self.transport.get_or_insert_with(factory)
    }
}

/// HTTP transport with the configured retry settings, built lazily.
#[must_use]
pub(super) fn lazy_http_transport(
    config: &Config,
) -> LazyTransport<HttpTransport> {
    let retry_policy = RetryPolicy {
        retries: config.sync.retries,
        backoff: Duration::from_millis(config.sync.backoff_ms),
    };
    let transport = TransportConfig {
        retry_policy,
        ..TransportConfig::default()
    };
    LazyTransport::new(
        move || {
            HttpTransport::with_config(ranki_sync::DEFAULT_ENDPOINT, transport)
        },
        retry_policy,
    )
}

impl<T: SyncTransport> SyncTransport for LazyTransport<T> {
    fn host_key(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.transport().host_key(body)
    }

    fn meta(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.transport().meta(body)
    }

    fn upload(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.transport().upload(body)
    }

    fn download(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.transport().download(body)
    }

    fn media_begin(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.transport().media_begin(body)
    }

    fn media_changes(
        &mut self,
        body: &[u8],
    ) -> ranki_sync::Result<SyncResponse> {
        self.transport().media_changes(body)
    }

    fn media_download(
        &mut self,
        body: &[u8],
    ) -> ranki_sync::Result<SyncResponse> {
        self.transport().media_download(body)
    }

    fn media_download_worker(
        &self,
    ) -> Option<Box<dyn ranki_sync::SyncTransport>> {
        self.transport
            .as_ref()
            .and_then(SyncTransport::media_download_worker)
    }

    fn set_endpoint(&mut self, endpoint: &str) -> ranki_sync::Result<()> {
        self.transport().set_endpoint(endpoint)
    }

    fn set_host_key(&mut self, key: &str) {
        self.transport().set_host_key(key);
    }

    fn retry_policy(&self) -> RetryPolicy {
        self.retry_policy
    }
}

/// Reads the credential environment variables without ever echoing them.
#[must_use]
pub(super) fn read_env() -> SyncEnv {
    SyncEnv {
        username: env::var("RANKI_SYNC_USERNAME").ok().and_then(non_empty),
        password: env::var("RANKI_SYNC_PASSWORD").ok().and_then(non_empty),
        hkey: env::var("RANKI_SYNC_HKEY").ok().and_then(non_empty),
    }
}

fn read_state(path: &Path) -> Option<SyncState> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<SyncState>(&text).ok())
}

/// Whether a previous login left a host key in the local sync state.
#[must_use]
pub(super) fn has_stored_key(settings: &Settings) -> bool {
    read_state(&settings.paths.sync_state)
        .is_some_and(|state| !state.host_key.is_empty())
}

/// Chooses a stored host key or a one-time username and password login.
#[must_use]
pub(super) fn load_auth(
    settings: &Settings,
    env: &SyncEnv,
) -> Option<SyncAuth> {
    let host_key = env.hkey.clone().or_else(|| {
        settings.sync.hkey_file.as_deref().and_then(read_hkey_file)
    });
    let username = env
        .username
        .clone()
        .or_else(|| settings.sync.username.clone())
        .and_then(non_empty);
    let login = match (username, env.password.clone().and_then(non_empty)) {
        (Some(name), Some(password)) => Some(Credentials::new(name, password)),
        _ => None,
    };
    if host_key.is_none() && login.is_none() {
        return None;
    }
    Some(SyncAuth { host_key, login })
}

/// Loads sync credentials only when a command can actually sync.
///
/// Disabled sync and ordinary commands never read the environment or the
/// state file. Auto sync is enabled only with credentials, stored or supplied.
pub(super) fn startup_credentials(
    settings: &Settings,
    manual: bool,
) -> (Option<SyncAuth>, bool) {
    credentials_for(settings, manual, read_env, || has_stored_key(settings))
}

#[expect(
    clippy::too_many_arguments,
    reason = "settings, mode and both input seams stay explicit for tests"
)]
fn credentials_for<F, G>(
    settings: &Settings,
    manual: bool,
    env: F,
    stored_key: G,
) -> (Option<SyncAuth>, bool)
where
    F: FnOnce() -> SyncEnv,
    G: FnOnce() -> bool,
{
    let auto_enabled = settings.sync.enabled && settings.sync.auto;
    if !manual && !auto_enabled {
        return (None, false);
    }
    let auth = load_auth(settings, &env());
    let automatic = auto_enabled && !manual && (auth.is_some() || stored_key());
    (auth, automatic)
}

/// Adapts an injected transport to the generic sync client.
pub(super) struct TransportRef<'a>(pub &'a mut dyn SyncTransport);

impl SyncTransport for TransportRef<'_> {
    fn host_key(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.host_key(body)
    }

    fn meta(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.meta(body)
    }

    fn upload(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.upload(body)
    }

    fn download(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.download(body)
    }

    fn media_begin(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.media_begin(body)
    }

    fn media_changes(
        &mut self,
        body: &[u8],
    ) -> ranki_sync::Result<SyncResponse> {
        self.0.media_changes(body)
    }

    fn media_download(
        &mut self,
        body: &[u8],
    ) -> ranki_sync::Result<SyncResponse> {
        self.0.media_download(body)
    }

    fn media_download_worker(
        &self,
    ) -> Option<Box<dyn ranki_sync::SyncTransport>> {
        self.0.media_download_worker()
    }

    fn set_endpoint(&mut self, endpoint: &str) -> ranki_sync::Result<()> {
        self.0.set_endpoint(endpoint)
    }

    fn set_host_key(&mut self, key: &str) {
        self.0.set_host_key(key);
    }

    fn retry_policy(&self) -> RetryPolicy {
        self.0.retry_policy()
    }
}

/// What the explicit `ranki sync` command should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SyncMode {
    Safe,
    Status,
    FullUpload,
    FullDownload,
}

/// Errors from an explicit `ranki sync` command.
#[derive(Debug, Error)]
pub(super) enum ManualSyncError {
    #[error(
        "no sync credentials are available. Set RANKI_SYNC_HKEY, set RANKI_SYNC_USERNAME and RANKI_SYNC_PASSWORD, or pass --sync-hkey-file <path>"
    )]
    NoCredentials,
    #[error("sync conflict: {0}")]
    Conflict(String),
    #[error("sync will retry later: {0}")]
    RetryLater(String),
    #[error(transparent)]
    Sync(#[from] SyncError),
}

/// Collection result from an explicit sync, with any later media failure.
#[derive(Debug)]
pub(super) struct ManualSyncReport {
    pub text: String,
    pub media_error: Option<SyncError>,
}

impl fmt::Display for ManualSyncReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)?;
        if let Some(error) = &self.media_error {
            write!(formatter, "\nwarning: media sync failed: {error}")?;
        }
        Ok(())
    }
}

/// Runs one automatic collection sync. Media is left to explicit `ranki sync`
/// and the window's background refresh, so ordinary commands never wait on a
/// large media library. A missing credential is a silent no-op in the sync
/// client, which leaves any existing state-file host key usable.
///
/// # Errors
/// Returns sync errors for login, transport, or local file failures.
pub(super) fn sync_once<T: SyncTransport>(
    transport: T,
    settings: &Settings,
    auth: Option<&SyncAuth>,
) -> Result<SyncOutcome, SyncError> {
    let mut client = build_client(transport, settings, auth)?;
    client.sync(auth.and_then(|credentials| credentials.login.clone()))
}

/// Refreshes media after a collection sync that left both copies in step.
fn refresh_media<T: SyncTransport>(
    client: &mut SyncClient<T>,
    outcome: &SyncOutcome,
) -> Result<(), SyncError> {
    if outcome.is_settled() {
        client.sync_media()?;
    }
    Ok(())
}

/// Performs the explicit `ranki sync` operation and returns its display text.
///
/// Explicit sync needs credentials or a stored host key, even for status.
/// Safe sync conflicts and retries are errors so the command exits nonzero.
///
/// # Errors
/// Returns an error when credentials are missing, a safe sync needs a user
/// decision or retry, or a request or local file operation fails.
#[expect(
    clippy::too_many_arguments,
    reason = "transport, settings, credentials and mode are distinct inputs"
)]
pub(super) fn manual_sync<T: SyncTransport>(
    transport: T,
    settings: &Settings,
    auth: Option<&SyncAuth>,
    mode: SyncMode,
) -> Result<ManualSyncReport, ManualSyncError> {
    if auth.is_none() && !has_stored_key(settings) {
        return Err(ManualSyncError::NoCredentials);
    }
    let mut client = build_client(transport, settings, auth)?;
    match mode {
        SyncMode::Status => {
            let info = client.status()?;
            Ok(ManualSyncReport {
                text: format!(
                    "remote mod={} scm={} usn={} empty={}",
                    info.mod_time, info.schema_mod_time, info.usn, info.empty
                ),
                media_error: None,
            })
        }
        SyncMode::FullUpload => {
            client.full_upload()?;
            Ok(ManualSyncReport {
                text: String::from("uploaded"),
                media_error: client.sync_media().err(),
            })
        }
        SyncMode::FullDownload => {
            client.full_download()?;
            Ok(ManualSyncReport {
                text: String::from("downloaded"),
                media_error: client.sync_media().err(),
            })
        }
        SyncMode::Safe => {
            let outcome = client
                .sync(auth.and_then(|credentials| credentials.login.clone()))?;
            match &outcome {
                SyncOutcome::Conflict { reason } => {
                    return Err(ManualSyncError::Conflict(reason.clone()));
                }
                SyncOutcome::RetryLater { reason } => {
                    return Err(ManualSyncError::RetryLater(reason.clone()));
                }
                SyncOutcome::Disabled
                | SyncOutcome::NoChanges
                | SyncOutcome::Uploaded
                | SyncOutcome::Downloaded => {}
            }
            Ok(ManualSyncReport {
                text: outcome_text(&outcome),
                media_error: refresh_media(&mut client, &outcome).err(),
            })
        }
    }
}

fn build_client<T: SyncTransport>(
    transport: T,
    settings: &Settings,
    auth: Option<&SyncAuth>,
) -> Result<SyncClient<T>, SyncError> {
    let state_path = settings.paths.sync_state.clone();
    if let Some(key) =
        auth.and_then(|credentials| credentials.host_key.as_deref())
    {
        ranki_sync::set_host_key(&state_path, key)
            .map_err(|error| SyncError::State(error.to_string()))?;
    }
    SyncClient::new(
        transport,
        &settings.paths.collection,
        &settings.paths.media_dir,
        &state_path,
    )
}

/// Prints a warning only for outcomes that need user attention.
pub(super) fn warn_outcome(outcome: &SyncOutcome) {
    match outcome {
        SyncOutcome::Conflict { reason } => {
            eprintln!("ranki: sync: conflict: {reason}");
        }
        SyncOutcome::RetryLater { reason } => {
            eprintln!("ranki: sync: retry later: {reason}");
        }
        SyncOutcome::Disabled
        | SyncOutcome::NoChanges
        | SyncOutcome::Uploaded
        | SyncOutcome::Downloaded => {}
    }
}

/// Human-readable result of an automatic sync.
#[must_use]
pub(super) fn outcome_text(outcome: &SyncOutcome) -> String {
    match outcome {
        SyncOutcome::Disabled => String::from("sync disabled"),
        SyncOutcome::NoChanges => String::from("no changes"),
        SyncOutcome::Uploaded => String::from("uploaded"),
        SyncOutcome::Downloaded => String::from("downloaded"),
        SyncOutcome::Conflict { reason } => format!("conflict: {reason}"),
        SyncOutcome::RetryLater { reason } => format!("retry later: {reason}"),
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn read_hkey_file(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        let mode = fs::metadata(path).ok()?.permissions().mode();
        if mode & PRIVATE_BITS != 0 {
            eprintln!(
                "ranki: sync: ignoring {}; host-key files must be mode 0600",
                path.display()
            );
            return None;
        }
    }
    let text = fs::read_to_string(path).ok()?;
    non_empty(text.trim().to_owned())
}

#[cfg(test)]
#[derive(Clone)]
struct SharedMock(Arc<Mutex<MockTransport>>);

#[cfg(test)]
impl SyncTransport for SharedMock {
    fn host_key(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.lock().unwrap().host_key(body)
    }

    fn meta(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.lock().unwrap().meta(body)
    }

    fn upload(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.lock().unwrap().upload(body)
    }

    fn download(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.lock().unwrap().download(body)
    }

    fn media_begin(&mut self, body: &[u8]) -> ranki_sync::Result<SyncResponse> {
        self.0.lock().unwrap().media_begin(body)
    }

    fn media_changes(
        &mut self,
        body: &[u8],
    ) -> ranki_sync::Result<SyncResponse> {
        self.0.lock().unwrap().media_changes(body)
    }

    fn media_download(
        &mut self,
        body: &[u8],
    ) -> ranki_sync::Result<SyncResponse> {
        self.0.lock().unwrap().media_download(body)
    }

    fn set_endpoint(&mut self, endpoint: &str) -> ranki_sync::Result<()> {
        self.0.lock().unwrap().set_endpoint(endpoint)
    }

    fn set_host_key(&mut self, key: &str) {
        self.0.lock().unwrap().set_host_key(key);
    }

    fn retry_policy(&self) -> RetryPolicy {
        self.0.lock().unwrap().retry_policy()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use ranki_model::NewNote;
    use ranki_sync::MockResponse;
    use ranki_sync::MockTransport;
    use ranki_sync::RetryPolicy;
    use ranki_sync::SyncMethod;
    use ranki_sync::SyncTransport;

    use super::LazyTransport;
    use super::ManualSyncError;
    use super::SharedMock;
    use super::SyncAuth;
    use super::SyncEnv;
    use super::SyncMode;
    use super::TransportRef;
    use super::credentials_for;
    use super::lazy_http_transport;
    use super::load_auth;
    use super::manual_sync;
    use super::outcome_text;
    use super::sync_once;
    use ranki_model::Store;

    fn settings(path: &Path) -> ranki_model::Settings {
        ranki_model::Settings {
            paths: ranki_model::DataPaths {
                collection: path.join("collection.anki2"),
                media_dir: path.join("collection.media"),
                sync_state: path.join("ranki-sync.json"),
            },
            sync: ranki_model::SyncSettings {
                enabled: true,
                ..ranki_model::SyncSettings::default()
            },
            ..ranki_model::Settings::default()
        }
    }

    fn now_secs() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .unwrap_or(0)
    }

    fn meta(empty: bool) -> MockResponse {
        MockResponse::ok_json(&serde_json::json!({
            "mod": 1_700_000_000_000_i64,
            "scm": 1_700_000_000_000_i64,
            "usn": 7,
            "ts": now_secs(),
            "empty": empty,
        }))
        .unwrap()
    }

    fn login() -> MockResponse {
        MockResponse::ok_json(&serde_json::json!({"key": "cached-hkey"}))
            .unwrap()
    }

    fn media_begin() -> MockResponse {
        MockResponse::ok_json(&serde_json::json!({
            "data": { "usn": 0, "sk": "cached-hkey" },
            "err": ""
        }))
        .unwrap()
    }

    fn collection_with_cards(path: &Path) {
        let mut collection =
            ranki_storage::Collection::create(&path.join("collection.anki2"))
                .unwrap();
        collection
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![String::from("front"), String::from("back")],
                tags: Vec::new(),
            })
            .unwrap();
        collection.checkpoint().unwrap();
        collection.close().unwrap();
    }

    fn host_key_auth() -> SyncAuth {
        SyncAuth {
            host_key: Some(String::from("stored-hkey")),
            login: None,
        }
    }

    fn seed_state(settings: &ranki_model::Settings) {
        let state = serde_json::json!({
            "version": 1,
            "endpoint": ranki_sync::DEFAULT_ENDPOINT,
            "host_key": "stored-hkey",
            "last_mod": null,
            "last_scm": 1_700_000_000_000_i64,
            "last_usn": 0,
            "last_media_usn": 0,
            "last_success": null,
            "failures": 0,
        });
        fs::write(settings.paths.sync_state.clone(), state.to_string())
            .unwrap();
    }

    #[test]
    fn env_credentials_are_loaded() {
        let settings = settings(Path::new("/tmp/ranki-env"));
        let env = SyncEnv {
            username: Some(String::from("alice")),
            password: Some(String::from("secret")),
            hkey: Some(String::from("env-hkey")),
        };
        let auth = load_auth(&settings, &env).unwrap();
        assert_eq!(auth.host_key.as_deref(), Some("env-hkey"));
        assert!(auth.login.is_some());
    }

    #[test]
    fn username_falls_back_to_config() {
        let mut settings = settings(Path::new("/tmp/ranki-env"));
        settings.sync.username = Some(String::from("config-user"));
        let env = SyncEnv {
            username: None,
            password: Some(String::from("secret")),
            hkey: None,
        };
        assert!(load_auth(&settings, &env).unwrap().login.is_some());
    }

    #[test]
    fn no_credentials_are_skipped() {
        let template = settings(Path::new("/tmp/ranki-env"));
        let env = SyncEnv {
            username: None,
            password: None,
            hkey: None,
        };
        assert!(load_auth(&template, &env).is_none());
        let auth = SyncAuth {
            host_key: None,
            login: None,
        };
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(directory.path());
        let mut transport = MockTransport::new();
        assert_eq!(
            sync_once(TransportRef(&mut transport), &settings, Some(&auth))
                .unwrap(),
            ranki_sync::SyncOutcome::Disabled
        );
        assert_eq!(transport.request_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn host_key_file_must_be_private() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = settings(directory.path());
        let path = directory.path().join("hkey");
        fs::write(&path, "file-hkey\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        settings.sync.hkey_file = Some(path.clone());
        let auth = load_auth(
            &settings,
            &SyncEnv {
                username: None,
                password: None,
                hkey: None,
            },
        )
        .unwrap();
        assert_eq!(auth.host_key.as_deref(), Some("file-hkey"));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load_auth(
                &settings,
                &SyncEnv {
                    username: None,
                    password: None,
                    hkey: None,
                },
            )
            .is_none()
        );
    }

    #[test]
    fn mock_login_caches_only_the_host_key() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(directory.path());
        let env = SyncEnv {
            username: Some(String::from("alice")),
            password: Some(String::from("secret")),
            hkey: None,
        };
        let auth = load_auth(&settings, &env).unwrap();
        let mut transport = MockTransport::new()
            .with_response(SyncMethod::HostKey, login())
            .with_response(SyncMethod::Meta, meta(true));
        sync_once(TransportRef(&mut transport), &settings, Some(&auth))
            .unwrap();
        assert_eq!(transport.requests()[0].method, SyncMethod::HostKey);
        assert_eq!(transport.requests()[0].host_key, "");
        let state = fs::read_to_string(&settings.paths.sync_state).unwrap();
        assert!(state.contains("cached-hkey"));
        assert!(!state.contains("secret"));
    }

    #[test]
    fn mock_upload_waits_for_a_choice_when_remote_is_empty() {
        let directory = tempfile::tempdir().unwrap();
        collection_with_cards(directory.path());
        let settings = settings(directory.path());
        seed_state(&settings);
        let mut transport =
            MockTransport::new().with_response(SyncMethod::Meta, meta(true));
        let outcome = sync_once(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
        )
        .unwrap();
        assert!(matches!(outcome, ranki_sync::SyncOutcome::Conflict { .. }));
        let methods: Vec<SyncMethod> = transport
            .requests()
            .iter()
            .map(|request| request.method)
            .collect();
        assert_eq!(methods, [SyncMethod::Meta]);
    }

    #[test]
    fn mock_download_replaces_an_empty_collection() {
        let source = tempfile::tempdir().unwrap();
        collection_with_cards(source.path());
        let bytes = fs::read(source.path().join("collection.anki2")).unwrap();

        let target = tempfile::tempdir().unwrap();
        let settings = settings(target.path());
        seed_state(&settings);
        let mut transport = MockTransport::new()
            .with_response(SyncMethod::Meta, meta(false))
            .with_response(
                SyncMethod::Download,
                MockResponse::ok_bytes(bytes).unwrap(),
            )
            .with_response(SyncMethod::Meta, meta(false));
        let outcome = sync_once(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
        )
        .unwrap();
        assert_eq!(outcome_text(&outcome), "downloaded");
        assert!(settings.paths.collection.exists());
        let methods: Vec<SyncMethod> = transport
            .requests()
            .iter()
            .map(|request| request.method)
            .collect();
        assert_eq!(methods, [SyncMethod::Meta, SyncMethod::Download]);
    }

    #[test]
    fn mock_conflict_never_overwrites_the_local_collection() {
        let directory = tempfile::tempdir().unwrap();
        collection_with_cards(directory.path());
        let settings = settings(directory.path());
        let before = fs::metadata(&settings.paths.collection).unwrap().len();
        let mut transport =
            MockTransport::new().with_response(SyncMethod::Meta, meta(false));
        let outcome = sync_once(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
        )
        .unwrap();
        assert!(matches!(outcome, ranki_sync::SyncOutcome::Conflict { .. }));
        assert_eq!(transport.request_count(), 1);
        assert_eq!(
            fs::metadata(&settings.paths.collection).unwrap().len(),
            before
        );
    }

    #[test]
    fn manual_status_reads_remote_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(directory.path());
        let mut transport =
            MockTransport::new().with_response(SyncMethod::Meta, meta(false));
        let report = manual_sync(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
            SyncMode::Status,
        )
        .unwrap();
        assert!(report.text.starts_with("remote mod=1700000000000"));
        assert_eq!(transport.request_count(), 1);
    }

    #[test]
    fn manual_safe_sync_refreshes_media() {
        let directory = tempfile::tempdir().unwrap();
        collection_with_cards(directory.path());
        let settings = settings(directory.path());
        fs::write(
            &settings.paths.sync_state,
            serde_json::json!({
                "version": 1,
                "endpoint": ranki_sync::DEFAULT_ENDPOINT,
                "host_key": "stored-hkey",
                "last_mod": 1_700_000_000_000_i64,
                "last_scm": 1_700_000_000_000_i64,
                "last_usn": 0,
                "last_media_usn": 0,
                "last_success": null,
                "failures": 0,
            })
            .to_string(),
        )
        .unwrap();
        let mut transport = MockTransport::new()
            .with_response(SyncMethod::Meta, meta(false))
            .with_response(
                SyncMethod::Upload,
                MockResponse::ok_bytes(b"OK".to_vec()).unwrap(),
            )
            .with_response(SyncMethod::Meta, meta(false))
            .with_response(SyncMethod::MediaBegin, media_begin());
        let report = manual_sync(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
            SyncMode::Safe,
        )
        .unwrap();
        assert_eq!(report.text, "uploaded");
        assert!(report.media_error.is_none());
        let methods: Vec<SyncMethod> = transport
            .requests()
            .iter()
            .map(|request| request.method)
            .collect();
        assert_eq!(
            methods,
            [
                SyncMethod::Meta,
                SyncMethod::Upload,
                SyncMethod::Meta,
                SyncMethod::MediaBegin
            ]
        );
    }

    #[test]
    fn manual_full_upload_is_an_explicit_escape_hatch() {
        let directory = tempfile::tempdir().unwrap();
        collection_with_cards(directory.path());
        let settings = settings(directory.path());
        let mut transport = MockTransport::new()
            .with_response(
                SyncMethod::Upload,
                MockResponse::ok_bytes(b"OK".to_vec()).unwrap(),
            )
            .with_response(SyncMethod::Meta, meta(false))
            .with_response(SyncMethod::MediaBegin, media_begin());
        let report = manual_sync(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
            SyncMode::FullUpload,
        )
        .unwrap();
        assert_eq!(report.text, "uploaded");
        assert!(report.media_error.is_none());
        assert_eq!(transport.requests()[0].method, SyncMethod::Upload);
        assert_eq!(transport.requests()[1].method, SyncMethod::Meta);
    }

    #[test]
    fn data_paths_keep_sync_state_inside_the_app_directory() {
        let paths = ranki_model::DataPaths::new("/data/ranki");
        assert_eq!(
            paths.sync_state,
            PathBuf::from("/data/ranki/ranki-sync.json")
        );
        assert_eq!(
            paths.collection,
            PathBuf::from("/data/ranki/collection.anki2")
        );
    }

    #[test]
    fn stored_host_key_enables_automatic_sync() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(directory.path());
        assert!(!super::has_stored_key(&settings));
        seed_state(&settings);
        assert!(super::has_stored_key(&settings));
    }

    #[test]
    fn lazy_http_transport_reports_the_configured_retry_policy() {
        let mut config = ranki_model::Config::default();
        config.sync.retries = 5;
        config.sync.backoff_ms = 250;
        let transport = lazy_http_transport(&config);
        let policy = transport.retry_policy();
        assert_eq!(policy.retries, 5);
        assert_eq!(policy.backoff, Duration::from_millis(250));
    }

    #[test]
    fn lazy_transport_builds_once_and_forwards_every_request() {
        let mock = MockTransport::new()
            .with_response(SyncMethod::HostKey, MockResponse::error(200, "ok"))
            .with_response(SyncMethod::Meta, MockResponse::error(200, "ok"))
            .with_response(SyncMethod::Upload, MockResponse::error(200, "ok"))
            .with_response(
                SyncMethod::Download,
                MockResponse::error(200, "ok"),
            );
        let shared = Arc::new(Mutex::new(mock));
        let builds = Arc::new(AtomicUsize::new(0));
        let stored = Arc::clone(&shared);
        let counter = Arc::clone(&builds);
        let policy = RetryPolicy {
            retries: 4,
            backoff: Duration::ZERO,
        };
        let mut transport = LazyTransport::new(
            move || {
                counter.fetch_add(1, Ordering::Relaxed);
                SharedMock(Arc::clone(&stored))
            },
            policy,
        );
        assert_eq!(transport.retry_policy(), policy);
        assert_eq!(builds.load(Ordering::Relaxed), 0);
        transport.set_endpoint("https://example.invalid/").unwrap();
        transport.set_host_key("key");
        transport.host_key(b"host-key").unwrap();
        transport.meta(b"meta").unwrap();
        transport.upload(b"upload").unwrap();
        transport.download(b"download").unwrap();
        assert_eq!(builds.load(Ordering::Relaxed), 1);
        drop(transport);
        let mock = shared.lock().unwrap();
        assert_eq!(mock.request_count(), 4);
        let methods: Vec<SyncMethod> = mock
            .requests()
            .iter()
            .map(|request| request.method)
            .collect();
        assert_eq!(
            methods,
            [
                SyncMethod::HostKey,
                SyncMethod::Meta,
                SyncMethod::Upload,
                SyncMethod::Download
            ]
        );
        for request in mock.requests() {
            assert_eq!(request.endpoint, "https://example.invalid/");
            assert_eq!(request.host_key, "key");
        }
    }

    #[test]
    fn disabled_sync_does_not_read_credentials_or_state() {
        let mut template = settings(Path::new("/tmp/ranki-disabled"));
        template.sync.enabled = false;
        let env_reads = Cell::new(0);
        let state_reads = Cell::new(0);
        let (auth, automatic) = credentials_for(
            &template,
            false,
            || {
                env_reads.set(env_reads.get() + 1);
                SyncEnv {
                    username: Some(String::from("alice")),
                    password: Some(String::from("secret")),
                    hkey: None,
                }
            },
            || {
                state_reads.set(state_reads.get() + 1);
                true
            },
        );
        assert!(auth.is_none());
        assert!(!automatic);
        assert_eq!(env_reads.get(), 0);
        assert_eq!(state_reads.get(), 0);
    }

    #[test]
    fn auto_sync_reads_state_only_without_supplied_credentials() {
        let template = settings(Path::new("/tmp/ranki-auto"));
        let env_reads = Cell::new(0);
        let state_reads = Cell::new(0);
        let (auth, automatic) = credentials_for(
            &template,
            false,
            || {
                env_reads.set(env_reads.get() + 1);
                SyncEnv {
                    username: None,
                    password: None,
                    hkey: Some(String::from("env-hkey")),
                }
            },
            || {
                state_reads.set(state_reads.get() + 1);
                false
            },
        );
        assert!(auth.is_some());
        assert!(automatic);
        assert_eq!(env_reads.get(), 1);
        assert_eq!(state_reads.get(), 0);

        let state_reads = Cell::new(0);
        let (auth, automatic) = credentials_for(
            &template,
            false,
            || SyncEnv {
                username: None,
                password: None,
                hkey: None,
            },
            || {
                state_reads.set(state_reads.get() + 1);
                true
            },
        );
        assert!(auth.is_none());
        assert!(automatic);
        assert_eq!(state_reads.get(), 1);
    }

    #[test]
    fn manual_sync_never_starts_an_automatic_round_trip() {
        let template = settings(Path::new("/tmp/ranki-manual"));
        let state_reads = Cell::new(0);
        let (auth, automatic) = credentials_for(
            &template,
            true,
            || SyncEnv {
                username: Some(String::from("alice")),
                password: Some(String::from("secret")),
                hkey: None,
            },
            || {
                state_reads.set(state_reads.get() + 1);
                true
            },
        );
        assert!(auth.is_some());
        assert!(!automatic);
        assert_eq!(state_reads.get(), 0);
    }
    #[test]
    fn manual_sync_without_credentials_has_a_setup_hint() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(directory.path());
        let mut transport = MockTransport::new();
        for mode in [SyncMode::Safe, SyncMode::Status] {
            let error = manual_sync(
                TransportRef(&mut transport),
                &settings,
                None,
                mode,
            )
            .unwrap_err();
            let message = error.to_string();
            assert!(
                [
                    "RANKI_SYNC_HKEY",
                    "RANKI_SYNC_USERNAME",
                    "RANKI_SYNC_PASSWORD",
                    "--sync-hkey-file",
                ]
                .iter()
                .all(|hint| message.contains(hint)),
                "{message}"
            );
        }
        assert_eq!(transport.request_count(), 0);
    }

    #[test]
    fn stored_host_key_allows_explicit_sync_without_new_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(directory.path());
        seed_state(&settings);
        let mut transport =
            MockTransport::new().with_response(SyncMethod::Meta, meta(false));
        let report = manual_sync(
            TransportRef(&mut transport),
            &settings,
            None,
            SyncMode::Status,
        )
        .unwrap();
        assert!(report.text.starts_with("remote mod=1700000000000"));
        assert!(report.media_error.is_none());
    }

    #[test]
    fn manual_safe_conflict_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        collection_with_cards(directory.path());
        let settings = settings(directory.path());
        let mut transport =
            MockTransport::new().with_response(SyncMethod::Meta, meta(false));
        let error = manual_sync(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
            SyncMode::Safe,
        )
        .unwrap_err();
        assert!(matches!(error, ManualSyncError::Conflict(_)));
        assert!(error.to_string().contains("sync conflict"));
    }

    #[test]
    fn manual_safe_retry_later_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(directory.path());
        let mut transport = MockTransport::new()
            .with_failure(SyncMethod::Meta, ranki_sync::SyncError::Timeout);
        let error = manual_sync(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
            SyncMode::Safe,
        )
        .unwrap_err();
        assert!(matches!(error, ManualSyncError::RetryLater(_)));
        assert!(error.to_string().contains("retry later"));
    }

    #[test]
    fn media_failure_keeps_the_collection_success_visible() {
        let directory = tempfile::tempdir().unwrap();
        collection_with_cards(directory.path());
        let settings = settings(directory.path());
        let mut transport = MockTransport::new()
            .with_response(
                SyncMethod::Upload,
                MockResponse::ok_bytes(b"OK".to_vec()).unwrap(),
            )
            .with_response(SyncMethod::Meta, meta(false))
            .with_failure(
                SyncMethod::MediaBegin,
                ranki_sync::SyncError::Network(String::from(
                    "media unavailable",
                )),
            );
        let report = manual_sync(
            TransportRef(&mut transport),
            &settings,
            Some(&host_key_auth()),
            SyncMode::FullUpload,
        )
        .unwrap();
        assert_eq!(report.text, "uploaded");
        assert!(report.media_error.is_some());
        let shown = report.to_string();
        assert!(shown.contains("uploaded"), "{shown}");
        assert!(shown.contains("warning: media sync failed"), "{shown}");
        assert!(shown.contains("media unavailable"), "{shown}");
    }
}
