//! Minimal `AnkiWeb` v11 sync for Ranki.
//!
//! [`SyncClient`] implements login, metadata checks, full upload, full download,
//! and a conservative automatic [`SyncClient::sync`]. It refuses one-way
//! changes when both sides changed or when the local collection may be newer.
//! [`SyncClient::sync_media`] refreshes local media on its own, so a large
//! media library never holds up the collection. Media refreshes checkpoint their
//! manifest cursor, so a later run resumes after the last completed group. Local
//! media is never deleted.
//!
//! HTTP lives behind the object-safe [`SyncTransport`] trait. [`HttpTransport`]
//! uses a blocking client, sends the `anki-sync` header fields `v`, `k`, `c`,
//! and `s`, disables redirects, and applies bounded timeouts and retries.
//! [`MockTransport`] provides scripted responses and failures for tests.
//!
//! Requests and responses are raw zstd-framed bytes. The client owns framing,
//! decompression, status checks, and payload limits. The caller supplies a state
//! file inside Ranki's app data directory. It stores only the host key and sync
//! metadata, with mode `0600` on Unix. Credentials are passed to [`SyncClient::login`] or
//! [`SyncClient::sync`] and are never written to disk or diagnostics.
//!
//! Wire details follow `docs/ankiweb-sync-research.md`. Delta sync and merge
//! behavior are intentionally out of scope. Callers must choose
//! [`SyncClient::full_upload`] or [`SyncClient::full_download`] explicitly when
//! [`SyncOutcome::Conflict`] is returned.

mod client;
mod error;
mod media;
mod mock;
mod state;
mod transport;
mod wire;

pub use client::{
    Credentials, MediaProgress, MediaProgressFn, RemoteStatus, SyncClient,
    SyncOutcome,
};
pub use error::{Result, SyncError};
pub use mock::{MockResponse, MockTransport, RecordedRequest};
pub use state::{SyncState, clear_host_key, set_host_key};
pub use transport::{
    DEFAULT_ENDPOINT, HttpTransport, ORIGINAL_SIZE_HEADER, RetryPolicy,
    SYNC_PLATFORM, SyncMethod, SyncResponse, SyncTransport, TransportConfig,
    client_version, meta_client_version,
};

#[cfg(test)]
mod tests;
