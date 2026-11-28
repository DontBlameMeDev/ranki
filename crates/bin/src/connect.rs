//! AnkiConnect-compatible JSON-RPC server.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use unicode_normalization::UnicodeNormalization;

use ranki_core::FsrsScheduler;
use ranki_model::AddNoteOptions;
use ranki_model::Card;
use ranki_model::CardQueue;
use ranki_model::CardType;
use ranki_model::Deck;
use ranki_model::DuplicateScope;
use ranki_model::Id;
use ranki_model::Note;
use ranki_model::NoteModel;
use ranki_model::Rating;
use ranki_model::Store;
use ranki_storage::Collection;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use tiny_http::Header;
use tiny_http::Request;
use tiny_http::Response;
use tiny_http::Server;

use crate::CliError;
use crate::CliResult;
use crate::answer_card;
use crate::error::{ErrorCode, InterfaceError};
const API_VERSION: u32 = 6;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const WORKER_COUNT: usize = 4;
static MEDIA_STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const EMPTY_PARAMS: Value = Value::Null;

#[derive(Debug)]
struct RpcError(InterfaceError);

impl From<InterfaceError> for RpcError {
    fn from(error: InterfaceError) -> Self {
        Self(error)
    }
}

impl From<ranki_model::StoreError> for RpcError {
    fn from(error: ranki_model::StoreError) -> Self {
        Self(error.into())
    }
}

impl From<CliError> for RpcError {
    fn from(error: CliError) -> Self {
        Self(error.into())
    }
}

impl From<String> for RpcError {
    fn from(error: String) -> Self {
        Self(InterfaceError::message(error))
    }
}

impl From<&str> for RpcError {
    fn from(error: &str) -> Self {
        Self(InterfaceError::message(error))
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<RpcError> for InterfaceError {
    fn from(error: RpcError) -> Self {
        error.0
    }
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    action: String,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    result: Value,
    error: Option<String>,
    #[serde(rename = "errorCode", skip_serializing_if = "Option::is_none")]
    error_code: Option<&'static str>,
}

impl RpcResponse {
    const fn ok(result: Value) -> Self {
        Self {
            result,
            error: None,
            error_code: None,
        }
    }

    fn error(source: impl Into<InterfaceError>) -> Self {
        let mapped = source.into();
        let code = mapped.code();
        let (_, message) = mapped.into_parts();
        Self {
            result: Value::Null,
            error: Some(message),
            error_code: Some(code.as_str()),
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the HTTP entry point takes its collection, media directory, port, and scheduler"
)]
pub fn serve(
    path: &Path,
    port: u16,
    media_dir: &Path,
    scheduler: &FsrsScheduler,
) -> CliResult<()> {
    let address = ("127.0.0.1", port);
    let server = Server::http(address).map_err(|error| {
        CliError::Message(format!("cannot listen on 127.0.0.1:{port}: {error}"))
    })?;
    let store = Arc::new(Mutex::new(Collection::open(path)?));
    serve_incoming(&server, &store, media_dir, scheduler)
}

#[expect(
    clippy::too_many_arguments,
    reason = "worker setup shares the server's explicit inputs"
)]
#[expect(
    clippy::excessive_nesting,
    reason = "each worker owns a long-lived receive loop"
)]
fn serve_incoming(
    server: &Server,
    store: &Arc<Mutex<Collection>>,
    media_dir: &Path,
    scheduler: &FsrsScheduler,
) -> CliResult<()> {
    let header =
        Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .map_err(|()| {
                CliError::Message(String::from("invalid HTTP header"))
            })?;
    let mut senders = Vec::with_capacity(WORKER_COUNT);
    for _ in 0..WORKER_COUNT {
        let (sender, receiver) = mpsc::channel();
        senders.push(sender);
        let worker_store = Arc::clone(store);
        let worker_media_dir = media_dir.to_path_buf();
        let worker_scheduler = scheduler.clone();
        let worker_header = header.clone();
        drop(thread::spawn(move || {
            for request in receiver {
                respond(
                    &worker_store,
                    &worker_media_dir,
                    request,
                    &worker_scheduler,
                    &worker_header,
                );
            }
        }));
    }

    let mut next = 0usize;
    for request in server.incoming_requests() {
        let sender = senders.get(next).ok_or_else(|| {
            CliError::Message(String::from("connect worker unavailable"))
        })?;
        if sender.send(request).is_err() {
            return Err(CliError::Message(String::from(
                "connect worker stopped",
            )));
        }
        next = next.saturating_add(1) % WORKER_COUNT;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "response dispatch shares the server's explicit inputs"
)]
fn respond(
    store: &Arc<Mutex<Collection>>,
    media_dir: &Path,
    mut request: Request,
    scheduler: &FsrsScheduler,
    header: &Header,
) {
    let response = read_and_dispatch(store, media_dir, &mut request, scheduler);
    let body = serde_json::to_string(&response).unwrap_or_else(|error| {
        let fallback =
            RpcResponse::error(format!("cannot encode response: {error}"));
        serde_json::to_string(&fallback).unwrap_or_else(|_| {
            String::from(r#"{"result":null,"error":"cannot encode response"}"#)
        })
    });
    let _ = request
        .respond(Response::from_string(body).with_header(header.clone()));
}

#[expect(
    clippy::too_many_arguments,
    reason = "request dispatch shares the server's explicit inputs"
)]
fn read_and_dispatch(
    store: &Arc<Mutex<Collection>>,
    media_dir: &Path,
    request: &mut Request,
    scheduler: &FsrsScheduler,
) -> RpcResponse {
    let mut body = Vec::new();
    let limit = u64::try_from(MAX_BODY_BYTES).unwrap_or(u64::MAX);
    if let Err(error) = request
        .as_reader()
        .take(limit.saturating_add(1))
        .read_to_end(&mut body)
    {
        return RpcResponse::error(format!("cannot read request: {error}"));
    }
    if body.len() > MAX_BODY_BYTES {
        return RpcResponse::error(format!(
            "request body too large (limit {MAX_BODY_BYTES} bytes)"
        ));
    }
    let Ok(mut guard) = store.lock() else {
        return RpcResponse::error("collection lock poisoned");
    };
    handle_body(&mut guard, media_dir, &body, scheduler)
}

#[expect(
    clippy::too_many_arguments,
    reason = "request dispatch shares the server's explicit inputs"
)]
fn handle_body(
    store: &mut Collection,
    media_dir: &Path,
    body: &[u8],
    scheduler: &FsrsScheduler,
) -> RpcResponse {
    let request: RpcRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(error) => {
            return RpcResponse::error(format!("invalid JSON: {error}"));
        }
    };
    if request.version.is_some_and(|version| version > API_VERSION) {
        return RpcResponse::error(format!(
            "unsupported protocol version: {}",
            request.version.unwrap_or_default()
        ));
    }
    let params = request.params.as_ref().unwrap_or(&EMPTY_PARAMS);
    match dispatch(store, media_dir, &request.action, params, scheduler) {
        Ok(result) => RpcResponse::ok(result),
        Err(error) => RpcResponse::error(error),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "dispatch shares one entry point for every action"
)]
fn dispatch(
    store: &mut Collection,
    media_dir: &Path,
    action: &str,
    params: &Value,
    scheduler: &FsrsScheduler,
) -> Result<Value, RpcError> {
    match action {
        "version" => Ok(json!(API_VERSION)),
        "deckNames" => Ok(json!(names(
            store.decks()?.iter().map(|deck| deck.name.as_str())
        ))),
        "modelNames" => Ok(json!(names(
            store.models()?.iter().map(|model| model.name.as_str())
        ))),
        "modelFieldNames" => model_field_names(store, params),
        "addNote" => add_note(store, media_dir, params),
        "findCards" => {
            Ok(json!(store.find_cards(param_str(params, "query")?)?))
        }
        "cardsInfo" => cards_info(store, params),
        "answerCards" => answer_cards(store, params, scheduler),
        "createDeck" => create_deck(store, params),
        "renameDeck" => rename_deck(store, params),
        "deleteDeck" => delete_deck(store, params),
        "suspend" => {
            let suspend = params
                .get("suspend")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            set_suspended(store, params, suspend)
        }
        "unsuspend" => {
            set_suspended(store, params, false)?;
            Ok(Value::Null)
        }
        "changeDeck" => change_deck(store, params),
        _ => Err(InterfaceError::new(
            ErrorCode::Unsupported,
            format!("unsupported action: {action}"),
        )
        .into()),
    }
}

fn names<'a>(values: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    values.collect()
}

fn member<'a>(value: &'a Value, key: &str) -> Result<&'a Value, RpcError> {
    Ok(value
        .get(key)
        .ok_or_else(|| format!("missing parameter: {key}"))?)
}

fn param_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    Ok(member(params, key)?
        .as_str()
        .ok_or_else(|| format!("invalid parameter: {key}"))?)
}

fn param_ids(params: &Value, key: &str) -> Result<Vec<Id>, RpcError> {
    let values = member(params, key)?
        .as_array()
        .ok_or_else(|| format!("invalid parameter: {key}"))?;
    values
        .iter()
        .map(|value| {
            value.as_i64().ok_or_else(|| {
                RpcError::from(format!("invalid card id: {value}"))
            })
        })
        .collect()
}

fn find_deck<'a>(decks: &'a [Deck], name: &str) -> Option<&'a Deck> {
    decks
        .iter()
        .find(|deck| deck.name.eq_ignore_ascii_case(name))
}

fn find_model<'a>(
    models: &'a [NoteModel],
    name: &str,
) -> Option<&'a NoteModel> {
    models
        .iter()
        .find(|model| model.name.eq_ignore_ascii_case(name))
}

fn model_field_names(
    store: &Collection,
    params: &Value,
) -> Result<Value, RpcError> {
    let requested = param_str(params, "modelName")?;
    let models = store.models()?;
    let model = find_model(&models, requested)
        .ok_or_else(|| format!("model not found: {requested}"))?;
    Ok(json!(model.fields))
}

#[expect(
    clippy::too_many_lines,
    reason = "addNote validates fields and coordinates staged media with the store"
)]
fn add_note(
    store: &mut Collection,
    media_dir: &Path,
    params: &Value,
) -> Result<Value, RpcError> {
    let note = member(params, "note")?;
    let deck_name = member(note, "deckName")?
        .as_str()
        .ok_or("invalid parameter: deckName")?;
    let model_name = member(note, "modelName")?
        .as_str()
        .ok_or("invalid parameter: modelName")?;
    let fields = member(note, "fields")?
        .as_object()
        .ok_or("invalid parameter: fields")?;

    let decks = store.deck_list()?;
    let models = store.models()?;
    let deck = find_deck(&decks, deck_name)
        .ok_or_else(|| format!("deck not found: {deck_name}"))?;
    let model = find_model(&models, model_name)
        .ok_or_else(|| format!("model not found: {model_name}"))?;

    for key in fields.keys() {
        if !model
            .fields
            .iter()
            .any(|name| name.eq_ignore_ascii_case(key))
        {
            return Err(format!("unknown field: {key}").into());
        }
    }
    let mut ordered: Vec<String> = model
        .fields
        .iter()
        .map(|name| {
            fields
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .and_then(|(_, value)| value.as_str())
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let options = note_options(note, &decks, deck.id)?;
    let media = prepare_media(note, &model.fields)?;
    apply_media(&media, &mut ordered);
    if ordered.first().is_none_or(|value| value.trim().is_empty()) {
        return Err("empty question".into());
    }
    let tags = note_tags(note)?;
    let (stage, staged) = stage_media(&media, media_dir)?;
    let published = match publish_media(&media, &staged, &stage, media_dir) {
        Ok(published) => published,
        Err(error) => {
            cleanup_stage(&stage);
            return Err(error);
        }
    };
    let ids = match store.add_note_with_options(
        &ranki_model::NewNote {
            model_id: model.id,
            deck_id: deck.id,
            fields: ordered,
            tags,
        },
        &options,
    ) {
        Ok(ids) => ids,
        Err(error) => {
            let rollback = rollback_media(&published)
                .err()
                .map(|detail| format!("; media rollback failed: {detail}"))
                .unwrap_or_default();
            return Err(format!("{error}{rollback}").into());
        }
    };
    let note_id = ids
        .first()
        .copied()
        .map(|card_id| store.card(card_id))
        .transpose()?
        .flatten()
        .map(|card| card.note_id);
    if let Err(error) = finish_media(&published) {
        return Err(media_cleanup_error(error, note_id));
    }
    Ok(note_id.map_or(Value::Null, |id| json!(id)))
}

fn note_options(
    note: &Value,
    decks: &[Deck],
    deck_id: Id,
) -> Result<AddNoteOptions, RpcError> {
    let Some(options) = note.get("options") else {
        return Ok(AddNoteOptions::default());
    };
    let object = options.as_object().ok_or("invalid parameter: options")?;
    let allow_duplicate = match object.get("allowDuplicate") {
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(
                "option parameter \"allowDuplicate\" must be boolean".into()
            );
        }
        None => false,
    };
    let mut duplicate_scope = DuplicateScope::new_collection();
    if object
        .get("duplicateScope")
        .and_then(Value::as_str)
        .unwrap_or("collection")
        == "deck"
    {
        duplicate_scope.use_deck = true;
        duplicate_scope.deck_id = Some(deck_id);
    }
    if let Some(scoped) = object.get("duplicateScopeOptions") {
        let scoped_options = scoped
            .as_object()
            .ok_or("invalid parameter: duplicateScopeOptions")?;
        match scoped_options.get("deckName") {
            Some(Value::String(name)) if !name.trim().is_empty() => {
                let deck = find_deck(decks, name)
                    .ok_or_else(|| format!("deck not found: {name}"))?;
                duplicate_scope.deck_id = Some(deck.id);
            }
            Some(Value::String(_)) | None => {}
            Some(_) => {
                return Err(
                    "invalid parameter: duplicateScopeOptions.deckName".into(),
                );
            }
        }
        match scoped_options.get("checkChildren") {
            Some(Value::Bool(value)) => duplicate_scope.check_children = *value,
            Some(_) => {
                return Err(
                    "option parameter \"duplicateScopeOptions.checkChildren\" must be boolean"
                        .into(),
                );
            }
            None => {}
        }
        match scoped_options.get("checkAllModels") {
            Some(Value::Bool(value)) => {
                duplicate_scope.check_all_models = *value;
            }
            Some(_) => {
                return Err(
                    "option parameter \"duplicateScopeOptions.checkAllModels\" must be boolean"
                        .into(),
                );
            }
            None => {}
        }
    }
    Ok(AddNoteOptions {
        allow_duplicate,
        duplicate_scope,
    })
}

struct MediaAttachment {
    filename: String,
    bytes: Vec<u8>,
    marker: String,
    field_indices: Vec<usize>,
}

#[expect(
    clippy::excessive_nesting,
    reason = "media attachment parsing mirrors AnkiConnect's nested option shape"
)]
fn prepare_media(
    note: &Value,
    field_names: &[String],
) -> Result<Vec<MediaAttachment>, RpcError> {
    let mut attachments = Vec::new();
    for (key, is_picture) in
        [("audio", false), ("video", false), ("picture", true)]
    {
        let Some(value) = note.get(key) else {
            continue;
        };
        let entries: Vec<&Value> = value
            .as_array()
            .map_or_else(|| vec![value], |array| array.iter().collect());
        for entry in entries {
            let filename = param_str(entry, "filename")?;
            let targets = entry
                .get("fields")
                .and_then(Value::as_array)
                .ok_or("invalid parameter: media fields")?;
            if targets.is_empty() {
                continue;
            }
            let bytes = if let Some(data) =
                entry.get("data").and_then(Value::as_str)
            {
                decode_base64(data)?
            } else if let Some(path) = entry.get("path").and_then(Value::as_str)
            {
                fs::read(path)
                    .map_err(|error| format!("cannot read media: {error}"))?
            } else {
                return Err(
                    "media requires a \"data\" or \"path\" field".into()
                );
            };
            let safe_filename = safe_media_filename(filename)?;
            let marker = if is_picture {
                format!(
                    "<img src=\"{}\">",
                    escape_html_attribute(&safe_filename)
                )
            } else {
                format!("[sound:{safe_filename}]")
            };
            let mut field_indices = Vec::new();
            for target in targets {
                let name =
                    target.as_str().ok_or("invalid parameter: media field")?;
                if let Some(index) = field_names
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case(name))
                {
                    field_indices.push(index);
                }
            }
            attachments.push(MediaAttachment {
                filename: safe_filename,
                bytes,
                marker,
                field_indices,
            });
        }
    }
    Ok(attachments)
}

#[expect(
    clippy::excessive_nesting,
    reason = "each attachment may target several fields"
)]
fn apply_media(attachments: &[MediaAttachment], fields: &mut [String]) {
    for attachment in attachments {
        for index in &attachment.field_indices {
            if let Some(value) = fields.get_mut(*index) {
                value.push_str(&attachment.marker);
            }
        }
    }
}

fn create_stage_dir(media_dir: &Path) -> Result<PathBuf, RpcError> {
    loop {
        let sequence = MEDIA_STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = media_dir
            .join(format!(".ranki-stage-{}-{sequence}", process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "cannot create media staging directory: {error}"
                )
                .into());
            }
        }
    }
}

fn stage_media(
    attachments: &[MediaAttachment],
    media_dir: &Path,
) -> Result<(PathBuf, Vec<PathBuf>), RpcError> {
    if attachments.is_empty() {
        return Ok((PathBuf::new(), Vec::new()));
    }
    fs::create_dir_all(media_dir)
        .map_err(|error| format!("cannot create media directory: {error}"))?;
    let stage = create_stage_dir(media_dir)?;
    let mut staged = Vec::new();
    for (index, attachment) in attachments.iter().enumerate() {
        let path = stage.join(index.to_string());
        if let Err(error) = fs::write(&path, &attachment.bytes) {
            cleanup_stage(&stage);
            return Err(format!("cannot stage media: {error}").into());
        }
        staged.push(path);
    }
    Ok((stage, staged))
}

struct PublishedMedia {
    stage: PathBuf,
    backups: Vec<(PathBuf, Option<PathBuf>)>,
}

#[expect(
    clippy::excessive_nesting,
    reason = "media publication backs up each existing target before replacing it"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "publication needs attachments, staged files, and both directories"
)]
fn publish_media(
    attachments: &[MediaAttachment],
    staged: &[PathBuf],
    stage: &Path,
    media_dir: &Path,
) -> Result<PublishedMedia, RpcError> {
    let mut published = PublishedMedia {
        stage: stage.to_path_buf(),
        backups: Vec::new(),
    };
    for (index, (attachment, path)) in
        attachments.iter().zip(staged).enumerate()
    {
        let target = media_dir.join(&attachment.filename);
        if target.is_dir() {
            let detail = rollback_media(&published).err().unwrap_or_default();
            return Err(format!(
                "cannot write media: target is a directory; {detail}"
            )
            .into());
        }
        let backup = if target.exists() {
            let backup_path = stage.join(format!("backup-{index}"));
            if let Err(error) = fs::rename(&target, &backup_path) {
                let detail =
                    rollback_media(&published).err().unwrap_or_default();
                return Err(
                    format!("cannot back up media: {error}; {detail}").into()
                );
            }
            Some(backup_path)
        } else {
            None
        };
        if let Err(error) = fs::rename(path, &target) {
            if let Some(backup_path) = &backup {
                let _ = fs::rename(backup_path, &target);
            }
            let detail = rollback_media(&published).err().unwrap_or_default();
            return Err(format!("cannot write media: {error}; {detail}").into());
        }
        published.backups.push((target, backup));
    }
    Ok(published)
}

fn rollback_media(published: &PublishedMedia) -> Result<(), String> {
    let mut errors = Vec::new();
    for (target, backup) in published.backups.iter().rev() {
        if target.is_file()
            && let Err(error) = fs::remove_file(target)
        {
            errors.push(format!("remove {}: {error}", target.display()));
        }
        if let Some(backup_path) = backup
            && let Err(error) = fs::rename(backup_path, target)
        {
            errors.push(format!("restore {}: {error}", target.display()));
        }
    }
    if let Err(error) = cleanup_stage_result(&published.stage) {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn media_cleanup_error(
    error: impl fmt::Display,
    note_id: Option<Id>,
) -> RpcError {
    let note =
        note_id.map_or_else(|| String::from("unknown"), |id| id.to_string());
    format!(
        "{error}; note was created but media cleanup failed (note id: {note})"
    )
    .into()
}

fn finish_media(published: &PublishedMedia) -> Result<(), RpcError> {
    cleanup_stage_result(&published.stage).map_err(RpcError::from)
}

fn cleanup_stage(stage: &Path) {
    let _ = cleanup_stage_result(stage);
}

fn cleanup_stage_result(stage: &Path) -> Result<(), String> {
    if stage.as_os_str().is_empty() || !stage.exists() {
        return Ok(());
    }
    fs::remove_dir_all(stage)
        .map_err(|error| format!("remove {}: {error}", stage.display()))
}

fn safe_media_filename(filename: &str) -> Result<String, RpcError> {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    let stripped: String = base
        .chars()
        .filter(|character| {
            !character.is_ascii_control()
                && !matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
        .collect();
    let safe: String = stripped.nfc().collect();
    if safe.is_empty() || safe == "." || safe == ".." {
        return Err(format!("invalid media filename: {filename}").into());
    }
    Ok(safe)
}

fn escape_html_attribute(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[expect(
    clippy::excessive_nesting,
    reason = "base64 padding is checked inside the byte decoder"
)]
fn decode_base64(input: &str) -> Result<Vec<u8>, RpcError> {
    let mut output = Vec::new();
    let mut accumulator = 0u32;
    let mut bits = 0usize;
    let mut padding = 0usize;
    for byte in input.bytes() {
        if padding > 0 && !matches!(byte, b'=' | b' ' | b'\n' | b'\r' | b'\t') {
            return Err("invalid base64 media data".into());
        }
        let value = match byte {
            b'=' => {
                if padding >= 2 {
                    return Err("invalid base64 media data".into());
                }
                padding = padding.saturating_add(1);
                continue;
            }
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            b'A'..=b'Z' => byte.saturating_sub(b'A'),
            b'a'..=b'z' => byte.saturating_sub(b'a').saturating_add(26),
            b'0'..=b'9' => byte.saturating_sub(b'0').saturating_add(52),
            b'+' => 62,
            b'/' => 63,
            _ => return Err("invalid base64 media data".into()),
        };
        accumulator = accumulator
            .saturating_mul(64)
            .saturating_add(u32::from(value));
        bits = bits.saturating_add(6);
        if bits >= 8 {
            let shift = bits.saturating_sub(8);
            let decoded = u8::try_from((accumulator >> shift) & 0xff)
                .map_err(|_| RpcError::from("invalid base64 media data"))?;
            output.push(decoded);
            bits = shift;
            accumulator &= (1u32 << bits).wrapping_sub(1);
        }
    }
    Ok(output)
}

fn note_tags(note: &Value) -> Result<Vec<String>, RpcError> {
    match note.get("tags") {
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| RpcError::from("invalid parameter: tags"))
            })
            .collect(),
        Some(_) => Err("invalid parameter: tags".into()),
        None => Ok(Vec::new()),
    }
}

fn field_values(
    model: Option<&NoteModel>,
    note: Option<&Note>,
) -> Map<String, Value> {
    let Some(entry) = model else {
        return Map::new();
    };
    let Some(record) = note else {
        return Map::new();
    };
    let mut fields = Map::new();
    for (ord, (name, value)) in
        entry.fields.iter().zip(record.fields.iter()).enumerate()
    {
        let order = i64::try_from(ord).unwrap_or_default();
        fields.insert(name.clone(), json!({"value": value, "order": order}));
    }
    fields
}

#[expect(
    clippy::too_many_arguments,
    reason = "card info needs the store, id and cached lookups"
)]
fn card_info(
    store: &Collection,
    id: Id,
    decks: &[Deck],
    models: &[NoteModel],
) -> Result<Value, RpcError> {
    let Some(card) = store.card(id)? else {
        return Ok(json!({}));
    };
    let note = store.note(card.note_id)?;
    let model = note
        .as_ref()
        .and_then(|entry| models.iter().find(|item| item.id == entry.model_id));
    let deck = decks.iter().find(|entry| entry.id == card.deck_id);
    let question = note
        .as_ref()
        .and_then(|entry| entry.fields.first())
        .cloned()
        .unwrap_or_default();
    let answer = note
        .as_ref()
        .map_or(String::new(), |entry| entry.fields.join(" / "));
    Ok(json!({
        "cardId": card.id,
        "fields": field_values(model, note.as_ref()),
        "fieldOrder": model.map_or_else(Vec::new, |entry| entry.fields.clone()),
        "question": question,
        "answer": answer,
        "modelName": model.map_or(String::new(), |entry| entry.name.clone()),
        "ord": card.ordinal,
        "deckName": deck.map_or(String::new(), |entry| entry.name.clone()),
        "css": model.map_or(String::new(), |entry| entry.css.clone()),
        "factor": card.factor,
        "interval": card.interval,
        "note": card.note_id,
        "type": card.card_type.code(),
        "queue": card.queue.code(),
        "due": card.due,
        "reps": card.reps,
        "lapses": card.lapses,
        "left": card.left,
        "mod": card.modified,
    }))
}

fn cards_info(store: &Collection, params: &Value) -> Result<Value, RpcError> {
    let ids = param_ids(params, "cards")?;
    let decks = store.decks()?;
    let models = store.models()?;
    let mut result = Vec::with_capacity(ids.len());
    for id in ids {
        result.push(card_info(store, id, &decks, &models)?);
    }
    Ok(json!(result))
}

fn answer_cards(
    store: &mut Collection,
    params: &Value,
    scheduler: &FsrsScheduler,
) -> Result<Value, RpcError> {
    let answers = member(params, "answers")?
        .as_array()
        .ok_or("invalid parameter: answers")?;
    let mut pending = Vec::with_capacity(answers.len());
    for answer in answers {
        let card_id = member(answer, "cardId")?
            .as_i64()
            .ok_or("invalid parameter: cardId")?;
        let ease = member(answer, "ease")?
            .as_i64()
            .and_then(|ease| u8::try_from(ease).ok())
            .and_then(Rating::from_code)
            .ok_or("invalid parameter: ease")?;
        pending.push((card_id, ease));
    }
    let now = crate::now_secs()?;
    let mut result = Vec::with_capacity(pending.len());
    for (card_id, ease) in pending {
        let card = store.card(card_id)?;
        if card.is_none_or(|entry| !entry.queue.is_due()) {
            result.push(false);
            continue;
        }
        answer_card(store, card_id, ease, now, scheduler, 0)?;
        result.push(true);
    }
    Ok(json!(result))
}

fn create_deck(
    store: &mut Collection,
    params: &Value,
) -> Result<Value, RpcError> {
    let name = param_str(params, "deck")?;
    Ok(json!(store.create_deck(name)?.id))
}

fn required_string<'a>(
    params: &'a Value,
    key: &str,
) -> Result<&'a str, RpcError> {
    let value = params.get(key).ok_or_else(|| {
        InterfaceError::new(
            ErrorCode::InvalidData,
            format!("missing parameter: {key}"),
        )
    })?;
    value.as_str().ok_or_else(|| {
        InterfaceError::new(
            ErrorCode::InvalidData,
            format!("invalid parameter: {key}"),
        )
        .into()
    })
}

fn deck_id_param(store: &Collection, params: &Value) -> Result<Id, RpcError> {
    if let Some(id) = params.get("id").and_then(Value::as_i64) {
        return Ok(id);
    }
    let name = required_string(params, "deck")?;
    let decks = store.decks()?;
    let deck = find_deck(&decks, name).ok_or_else(|| {
        ranki_model::StoreError::NotFound(format!("deck not found: {name}"))
    })?;
    Ok(deck.id)
}

fn rename_deck(
    store: &mut Collection,
    params: &Value,
) -> Result<Value, RpcError> {
    let id = deck_id_param(store, params)?;
    let new_name = required_string(params, "newName")?;
    store.rename_deck(id, new_name)?;
    Ok(Value::Null)
}

fn delete_deck(
    store: &mut Collection,
    params: &Value,
) -> Result<Value, RpcError> {
    let id = deck_id_param(store, params)?;
    store.remove_deck(id)?;
    Ok(Value::Null)
}

fn set_suspended(
    store: &mut Collection,
    params: &Value,
    suspend: bool,
) -> Result<Value, RpcError> {
    for id in param_ids(params, "cards")? {
        let Some(mut card) = store.card(id)? else {
            continue;
        };
        card.queue = if suspend {
            CardQueue::Suspended
        } else {
            queue_for(&card)
        };
        store.update_card(&card, crate::now_secs()?)?;
    }
    Ok(json!(suspend))
}

const fn queue_for(card: &Card) -> CardQueue {
    match card.card_type {
        CardType::New => CardQueue::New,
        CardType::Review => CardQueue::Review,
        CardType::Learning | CardType::Relearning => {
            if ranki_model::Timing::is_unix_due(card.due) {
                CardQueue::Learning
            } else {
                CardQueue::DayLearning
            }
        }
    }
}

fn change_deck(
    store: &mut Collection,
    params: &Value,
) -> Result<Value, RpcError> {
    let deck_name = param_str(params, "deck")?;
    let decks = store.decks()?;
    let deck = find_deck(&decks, deck_name)
        .ok_or_else(|| format!("deck not found: {deck_name}"))?;
    for id in param_ids(params, "cards")? {
        let Some(mut card) = store.card(id)? else {
            continue;
        };
        card.deck_id = deck.id;
        card.original_deck_id = 0;
        store.update_card(&card, crate::now_secs()?)?;
    }
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {

    use serde_json::Value;
    use serde_json::json;
    use std::fs;
    use std::io::Read;
    use std::io::Write;
    use std::net::TcpStream;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;
    use tiny_http::Server;
    use tiny_http::TestRequest;

    use super::FsrsScheduler;
    use super::dispatch;
    use super::handle_body;
    use crate::answer_card;
    use ranki_model::Rating;
    use ranki_model::Store;
    use ranki_storage::Collection;

    use super::RpcResponse;
    use crate::error::ErrorCode;
    fn fixture() -> (TempDir, Collection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.anki2");
        (dir, Collection::create(&path).unwrap())
    }

    fn scheduler() -> FsrsScheduler {
        ranki_core::from_settings(&ranki_model::Settings::default())
    }

    fn media() -> &'static Path {
        Path::new("unused-media")
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the helper builds a JSON-RPC envelope from an owned payload"
    )]
    fn call(store: &mut Collection, action: &str, params: Value) -> Value {
        let body = serde_json::to_vec(&json!({
            "action": action, "version": 6, "params": params,
        }))
        .unwrap();
        let response = handle_body(store, media(), &body, &scheduler());
        assert_eq!(response.error, None);
        response.result
    }

    fn note(deck: &str) -> Value {
        json!({
            "deckName": deck,
            "modelName": "Basic",
            "fields": {"Front": "Question", "Back": "Answer"},
            "tags": ["test"],
        })
    }

    fn add_card(store: &mut Collection) -> i64 {
        call(store, "addNote", json!({"note": note("Default")}))
            .as_i64()
            .unwrap()
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "the helper builds an owned JSON-RPC envelope"
    )]
    fn call_media(
        dir: &TempDir,
        store: &mut Collection,
        payload: Value,
    ) -> RpcResponse {
        let body = serde_json::to_vec(&json!({
            "action": "addNote", "version": 6, "params": payload,
        }))
        .unwrap();
        handle_body(
            store,
            &dir.path().join("collection.media"),
            &body,
            &scheduler(),
        )
    }

    #[test]
    fn version_returns_six() {
        let (_dir, mut store) = fixture();
        assert_eq!(call(&mut store, "version", json!({})), json!(6));
    }

    #[test]
    fn version_response_bytes_are_the_same_without_params() {
        let (_dir, mut store) = fixture();
        let bodies = [
            br#"{"action":"version","version":6}"#.as_slice(),
            br#"{"action":"version","version":6,"params":{}}"#.as_slice(),
            br#"{"action":"version","version":6,"params":null}"#.as_slice(),
        ];
        let bytes: Vec<String> = bodies
            .iter()
            .map(|body| {
                let response =
                    handle_body(&mut store, media(), body, &scheduler());
                serde_json::to_string(&response).unwrap()
            })
            .collect();
        assert!(bytes.iter().all(|text| text == &bytes[0]));
        assert_eq!(bytes[0], r#"{"result":6,"error":null}"#);
    }

    #[test]
    fn deck_names_are_returned() {
        let (_dir, mut store) = fixture();
        assert_eq!(
            call(&mut store, "deckNames", json!({})),
            json!(["Default"])
        );
    }

    #[test]
    fn model_names_are_returned() {
        let (_dir, mut store) = fixture();
        assert_eq!(call(&mut store, "modelNames", json!({})), json!(["Basic"]));
    }

    #[test]
    fn model_field_names_follow_field_order() {
        let (_dir, mut store) = fixture();
        assert_eq!(
            call(&mut store, "modelFieldNames", json!({"modelName": "basic"})),
            json!(["Front", "Back"])
        );
    }

    #[test]
    fn create_deck_returns_an_id() {
        let (_dir, mut store) = fixture();
        let id = call(&mut store, "createDeck", json!({"deck": "A::B"}))
            .as_i64()
            .unwrap();
        assert!(id > 0);
        assert!(
            call(&mut store, "deckNames", json!({}))
                .as_array()
                .unwrap()
                .contains(&json!("A::B"))
        );
    }

    #[test]
    fn add_note_returns_the_note_id() {
        let (_dir, mut store) = fixture();
        let id = add_card(&mut store);
        assert!(id > 0);
        let card_id = store.find_cards("").unwrap().first().copied().unwrap();
        let note_id = store.card(card_id).unwrap().unwrap().note_id;
        assert_eq!(id, note_id);
        assert_eq!(
            call(&mut store, "findCards", json!({"query": ""}))
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn add_note_rejects_unknown_fields() {
        let (_dir, mut store) = fixture();
        let mut payload = note("Default");
        payload["fields"]["Missing"] = json!("x");
        let error = dispatch(
            &mut store,
            media(),
            "addNote",
            &json!({"note": payload}),
            &scheduler(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn find_cards_matches_fields() {
        let (_dir, mut store) = fixture();
        let id = add_card(&mut store);
        assert_eq!(
            call(&mut store, "findCards", json!({"query": "Question"})),
            json!([store
                .find_cards("Question")
                .unwrap()
                .first()
                .copied()
                .unwrap()])
        );
        assert!(id > 0, "id was {id}");
    }

    #[test]
    fn cards_info_returns_objects_for_existing_and_missing_cards() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        let info =
            call(&mut store, "cardsInfo", json!({"cards": [card_id, 999]}));
        let entries = info.as_array().unwrap();
        assert_eq!(entries[1], json!({}));
        assert_eq!(entries[0]["cardId"], json!(card_id));
        assert_eq!(entries[0]["deckName"], json!("Default"));
        assert_eq!(entries[0]["fields"]["Front"]["value"], json!("Question"));
    }

    #[test]
    fn answer_cards_reports_which_cards_existed() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        assert_eq!(
            call(
                &mut store,
                "answerCards",
                json!({"answers": [
                    {"cardId": card_id, "ease": 3}, {"cardId": 999, "ease": 3}
                ]})
            ),
            json!([true, false])
        );
        assert_eq!(store.card(card_id).unwrap().unwrap().reps, 1);
    }

    #[test]
    fn answer_cards_rejects_bad_ease() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let error = dispatch(
            &mut store,
            media(),
            "answerCards",
            &json!({"answers": [{"cardId": 1, "ease": 9}]}),
            &scheduler(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("ease"));
    }

    #[test]
    fn suspend_and_unsuspend_change_the_queue() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        assert_eq!(
            call(
                &mut store,
                "suspend",
                json!({"cards": [card_id], "suspend": true})
            ),
            json!(true)
        );
        assert_eq!(store.card(card_id).unwrap().unwrap().queue.code(), -1);
        assert_eq!(
            call(&mut store, "unsuspend", json!({"cards": [card_id]})),
            json!(null)
        );
        assert_eq!(store.card(card_id).unwrap().unwrap().queue.code(), 0);
    }

    #[test]
    fn change_deck_moves_the_card() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        let deck_id = call(&mut store, "createDeck", json!({"deck": "Other"}))
            .as_i64()
            .unwrap();
        assert_eq!(
            call(
                &mut store,
                "changeDeck",
                json!({"cards": [card_id], "deck": "Other"})
            ),
            json!(null)
        );
        assert_eq!(store.card(card_id).unwrap().unwrap().deck_id, deck_id);
    }

    #[test]
    fn unknown_actions_and_bad_json_report_errors() {
        let (_dir, mut store) = fixture();
        let response = handle_body(
            &mut store,
            media(),
            br#"{"action":"nope","version":6,"params":{}}"#,
            &scheduler(),
        );
        assert_eq!(response.result, Value::Null);
        assert_eq!(response.error.as_deref(), Some("unsupported action: nope"));
        let response =
            handle_body(&mut store, media(), b"not json", &scheduler());
        assert!(response.error.unwrap_or_default().contains("invalid JSON"));
    }

    #[test]
    fn answer_card_helper_is_reused_by_the_http_api() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        let decision = answer_card(
            &mut store,
            card_id,
            Rating::Good,
            crate::now_secs().unwrap(),
            &scheduler(),
            0,
        )
        .unwrap();
        assert_eq!(decision.card.id, card_id);
    }

    #[test]
    fn transport_errors_keep_a_stable_code() {
        let (_dir, mut store) = fixture();
        let error =
            dispatch(&mut store, media(), "cardInfo", &json!({}), &scheduler())
                .unwrap_err();
        assert_eq!(error.0.code(), ErrorCode::Unsupported);

        let response = RpcResponse::error(ranki_model::StoreError::NotFound(
            "card".into(),
        ));
        assert_eq!(response.error_code, Some("not_found"));
        let response =
            RpcResponse::error(ranki_format::FormatError::new("bad"));
        assert_eq!(response.error_code, Some("format"));
        assert_eq!(
            serde_json::to_value(response).unwrap()["errorCode"],
            json!("format")
        );
    }
    #[test]
    fn rename_deck_and_delete_deck_update_the_deck_list() {
        let (_dir, mut store) = fixture();
        let id = call(&mut store, "createDeck", json!({"deck": "Old::Child"}))
            .as_i64()
            .unwrap();
        assert_eq!(
            call(
                &mut store,
                "renameDeck",
                json!({"deck": "Old::Child", "newName": "New::Child"})
            ),
            json!(null)
        );
        let names = call(&mut store, "deckNames", json!({}));
        assert!(names.as_array().unwrap().contains(&json!("New::Child")));
        assert!(!names.as_array().unwrap().contains(&json!("Old::Child")));

        assert_eq!(
            call(&mut store, "deleteDeck", json!({"deck": "new::child"})),
            json!(null)
        );
        let names = call(&mut store, "deckNames", json!({}));
        assert!(!names.as_array().unwrap().contains(&json!("New::Child")));
        assert!(store.decks().unwrap().iter().all(|deck| deck.id != id));
    }

    #[test]
    fn deck_actions_report_stable_error_codes() {
        let (_dir, mut store) = fixture();
        let cases = [
            (
                br#"{"action":"deleteDeck","version":6,"params":{"deck":"Missing"}}"#.as_slice(),
                Some("not_found"),
            ),
            (
                br#"{"action":"deleteDeck","version":6,"params":{"deck":"Default"}}"#.as_slice(),
                Some("invalid_data"),
            ),
            (
                br#"{"action":"renameDeck","version":6,"params":{"id":999}}"#.as_slice(),
                Some("invalid_data"),
            ),
            (
                br#"{"action":"renameDeck","version":6,"params":{"id":999,"newName":"New"}}"#.as_slice(),
                Some("not_found"),
            ),
        ];
        for (body, expected) in cases {
            let response = handle_body(&mut store, media(), body, &scheduler());
            assert_eq!(response.result, Value::Null);
            assert_eq!(response.error_code, expected, "{body:?}");
        }
    }

    #[test]
    fn malformed_parameters_report_precise_errors() {
        let (_dir, mut store) = fixture();
        let request = json!({
            "action": "version", "version": 7, "params": {},
        });
        let body = serde_json::to_vec(&request).unwrap();
        let response = handle_body(&mut store, media(), &body, &scheduler());
        assert_eq!(
            response.error.as_deref(),
            Some("unsupported protocol version: 7")
        );

        let cases = [
            (
                json!({"action":"findCards","params":{"query":42}}),
                "invalid parameter: query",
            ),
            (
                json!({"action":"cardsInfo","params":{"cards":"1"}}),
                "invalid parameter: cards",
            ),
            (
                json!({"action":"cardsInfo","params":{"cards":[1,"x"]}}),
                "invalid card id: \"x\"",
            ),
            (
                json!({"action":"modelFieldNames","params":{"modelName":"Missing"}}),
                "model not found: Missing",
            ),
            (
                json!({"action":"addNote","params":{}}),
                "missing parameter: note",
            ),
            (
                json!({"action":"addNote","params":{"note":{"deckName":"Default","modelName":"Basic","fields":{}}}}),
                "empty question",
            ),
            (
                json!({"action":"addNote","params":{"note":{"deckName":"Missing","modelName":"Basic","fields":{"Front":"Q"}}}}),
                "deck not found: Missing",
            ),
            (
                json!({"action":"answerCards","params":{}}),
                "missing parameter: answers",
            ),
            (
                json!({"action":"answerCards","params":{"answers":{}}}),
                "invalid parameter: answers",
            ),
            (
                json!({"action":"suspend","params":{"cards":[1,"x"]}}),
                "invalid card id: \"x\"",
            ),
        ];
        for (request, expected) in cases {
            let body = serde_json::to_vec(&request).unwrap();
            let response =
                handle_body(&mut store, media(), &body, &scheduler());
            assert_eq!(response.error.as_deref(), Some(expected), "{request}");
        }
    }
    #[test]
    fn add_note_rejects_duplicates_unless_explicitly_allowed() {
        let (dir, mut store) = fixture();
        add_card(&mut store);
        let mut payload = note("Default");
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert!(response.error.unwrap_or_default().contains("duplicate"));

        payload["options"] = json!({"allowDuplicate": true});
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert_eq!(response.error, None);
        assert!(response.result.as_i64().is_some_and(|id| id > 0));
    }

    #[test]
    fn add_note_supports_deck_duplicate_scope() {
        let (dir, mut store) = fixture();
        add_card(&mut store);
        let other = call(&mut store, "createDeck", json!({"deck": "Other"}))
            .as_i64()
            .unwrap();
        assert!(other > 0);

        let mut scoped = note("Other");
        scoped["options"] = json!({"duplicateScope": "deck"});
        let response = call_media(&dir, &mut store, json!({"note": scoped}));
        assert_eq!(response.error, None, "{response:?}");

        let global = note("Other");
        let response = call_media(&dir, &mut store, json!({"note": global}));
        assert!(response.error.unwrap_or_default().contains("duplicate"));

        let unknown = json!({
            "duplicateScope": "global",
        });
        let mut unknown_payload = note("Other");
        unknown_payload["options"] = unknown;
        let response =
            call_media(&dir, &mut store, json!({"note": unknown_payload}));
        assert!(response.error.unwrap_or_default().contains("duplicate"));
    }

    #[test]
    fn add_note_attaches_media_bytes_and_references() {
        let (dir, mut store) = fixture();
        let mut payload = note("Default");
        payload["picture"] = json!({
            "filename": "pic.png",
            "fields": ["Front"],
            "data": "cGl4ZWxz",
        });
        payload["audio"] = json!([{
            "filename": "voice.mp3",
            "fields": ["Back"],
            "data": "YXVkaW8=",
        }]);
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert_eq!(response.error, None, "{response:?}");

        let media = dir.path().join("collection.media");
        assert_eq!(fs::read(media.join("pic.png")).unwrap(), b"pixels");
        assert_eq!(fs::read(media.join("voice.mp3")).unwrap(), b"audio");

        let card = store.find_cards("").expect("find")[0];
        let saved_card = store.card(card).expect("card").expect("exists");
        let saved = store
            .note(saved_card.note_id)
            .expect("note")
            .expect("exists");
        assert_eq!(saved.fields[0], "Question<img src=\"pic.png\">");
        assert_eq!(saved.fields[1], "Answer[sound:voice.mp3]");
    }

    #[test]
    fn add_note_escapes_and_normalizes_media_filenames() {
        let (dir, mut store) = fixture();
        let mut payload = note("Default");
        payload["picture"] = json!({
            "filename": "a&b'.png",
            "fields": ["Front"],
            "data": "cGl4ZWxz",
        });
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert_eq!(response.error, None, "{response:?}");
        let media = dir.path().join("collection.media");
        assert_eq!(fs::read(media.join("a&b'.png")).unwrap(), b"pixels");
        let card = store.find_cards("").expect("find")[0];
        let saved_card = store.card(card).expect("card").expect("exists");
        let saved = store
            .note(saved_card.note_id)
            .expect("note")
            .expect("exists");
        assert_eq!(saved.fields[0], "Question<img src=\"a&amp;b&#39;.png\">");

        let mut payload = note("Default");
        payload["fields"]["Front"] = json!("Second");
        payload["picture"] = json!({
            "filename": "e\u{301}.png",
            "fields": ["Front"],
            "data": "cGl4ZWxz",
        });
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert_eq!(response.error, None, "{response:?}");
        assert_eq!(fs::read(media.join("é.png")).unwrap(), b"pixels");
        let card = store.find_cards("Second").expect("find")[0];
        let saved_card = store.card(card).expect("card").expect("exists");
        let saved = store
            .note(saved_card.note_id)
            .expect("note")
            .expect("exists");
        assert_eq!(saved.fields[0], "Second<img src=\"é.png\">");
    }

    #[test]
    fn add_note_sanitizes_media_filenames() {
        let (dir, mut store) = fixture();
        let mut payload = note("Default");
        payload["picture"] = json!({
            "filename": "../bad:<name>.png",
            "fields": ["Front"],
            "data": "cGl4ZWxz",
        });
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert_eq!(response.error, None, "{response:?}");

        let media = dir.path().join("collection.media");
        assert_eq!(fs::read(media.join("badname.png")).unwrap(), b"pixels");
        assert!(!dir.path().join("badname.png").exists());
    }

    #[test]
    fn rejected_duplicate_leaves_no_media_behind() {
        let (dir, mut store) = fixture();
        add_card(&mut store);
        let media = dir.path().join("collection.media");
        fs::create_dir_all(&media).expect("media dir");
        fs::write(media.join("duplicate.mp3"), b"old").expect("existing media");
        let mut payload = note("Default");
        payload["audio"] = json!({
            "filename": "duplicate.mp3",
            "fields": ["Back"],
            "data": "YXVkaW8=",
        });
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert!(response.error.unwrap_or_default().contains("duplicate"));
        assert_eq!(fs::read(media.join("duplicate.mp3")).unwrap(), b"old");
        let staged: Vec<_> = fs::read_dir(&media)
            .expect("media dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with(".ranki-")
            })
            .collect();
        assert!(staged.is_empty(), "{staged:?}");
    }

    #[test]
    fn repeated_media_filename_keeps_the_last_attachment() {
        let (dir, mut store) = fixture();
        let mut payload = note("Default");
        payload["picture"] = json!([
            {"filename": "same.png", "fields": ["Front"], "data": "Zmlyc3Q="},
            {"filename": "same.png", "fields": ["Front"], "data": "c2Vjb25k"},
        ]);
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert_eq!(response.error, None, "{response:?}");
        let media = dir.path().join("collection.media");
        assert_eq!(fs::read(media.join("same.png")).unwrap(), b"second");
        assert_eq!(fs::read_dir(&media).unwrap().count(), 1);
    }

    #[test]
    fn failed_media_rename_removes_staged_files() {
        let (dir, mut store) = fixture();
        let media = dir.path().join("collection.media");
        fs::create_dir_all(media.join("collision.png"))
            .expect("blocking directory");
        let mut payload = note("Default");
        payload["picture"] = json!({
            "filename": "collision.png",
            "fields": ["Front"],
            "data": "cGl4ZWxz",
        });
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert!(
            response
                .error
                .unwrap_or_default()
                .contains("cannot write media")
        );
        let staged: Vec<_> = fs::read_dir(&media)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with(".ranki-")
            })
            .collect();
        assert!(staged.is_empty(), "{staged:?}");
    }
    #[test]
    fn media_filename_filters_invalid_characters_before_nfc() {
        assert_eq!(
            super::safe_media_filename("e\u{0001}\u{301}.png")
                .ok()
                .as_deref(),
            Some("é.png")
        );
    }
    #[test]
    fn unsuspend_restores_learning_queue_from_the_due_value() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        let mut card = store.card(card_id).unwrap().unwrap();
        card.card_type = ranki_model::CardType::Learning;
        card.queue = ranki_model::CardQueue::Learning;
        card.due = 1_700_000_100;
        card.left = 2;
        store
            .update_card(&card, crate::now_secs().unwrap())
            .unwrap();

        call(&mut store, "suspend", json!({"cards": [card_id]}));
        assert_eq!(
            store.card(card_id).unwrap().unwrap().queue,
            ranki_model::CardQueue::Suspended
        );
        call(&mut store, "unsuspend", json!({"cards": [card_id]}));
        assert_eq!(
            store.card(card_id).unwrap().unwrap().queue,
            ranki_model::CardQueue::Learning
        );

        let mut day_card = store.card(card_id).unwrap().unwrap();
        day_card.due = 12;
        store
            .update_card(&day_card, crate::now_secs().unwrap() + 1)
            .unwrap();
        call(&mut store, "suspend", json!({"cards": [card_id]}));
        call(&mut store, "unsuspend", json!({"cards": [card_id]}));
        assert_eq!(
            store.card(card_id).unwrap().unwrap().queue,
            ranki_model::CardQueue::DayLearning
        );
        assert_eq!(store.review_history().unwrap().len(), 0);
    }

    #[test]
    fn answering_a_suspended_card_creates_no_log() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        call(&mut store, "suspend", json!({"cards": [card_id]}));
        let result = call(
            &mut store,
            "answerCards",
            json!({"answers": [{"cardId": card_id, "ease": 3}]}),
        );
        assert_eq!(result, json!([false]));
        assert_eq!(store.review_history().unwrap().len(), 0);
    }

    #[test]
    fn duplicate_scope_unknown_deck_name_is_rejected_before_staging() {
        let (dir, mut store) = fixture();
        let mut payload = note("Default");
        payload["options"] =
            json!({"duplicateScopeOptions": {"deckName": "Missing"}});
        payload["picture"] = json!({
            "filename": "pic.png",
            "fields": ["Front"],
            "data": "cGl4ZWxz",
        });
        let response = call_media(&dir, &mut store, json!({"note": payload}));
        assert!(
            response
                .error
                .unwrap_or_default()
                .contains("deck not found: Missing")
        );
        assert!(!dir.path().join("collection.media").exists());
        assert_eq!(store.find_cards("").unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn duplicate_scope_resolves_an_empty_default_deck() {
        let (dir, mut store) = fixture();
        store.create_deck("Study").unwrap();
        let mut payload = note("Default");
        payload["options"] = json!({
            "duplicateScope": "deck",
            "duplicateScopeOptions": {"deckName": "Default"}
        });

        let first = call_media(&dir, &mut store, json!({"note": payload}));
        assert!(first.result.is_i64(), "{first:?}");
        let second = call_media(&dir, &mut store, json!({"note": payload}));
        assert!(
            second
                .error
                .as_deref()
                .is_some_and(|error| error.contains("duplicate")),
            "{second:?}"
        );
        payload["options"] = json!({
            "duplicateScope": "deck",
            "duplicateScopeOptions": {"deckName": " "}
        });
        let blank = call_media(&dir, &mut store, json!({"note": payload}));
        assert!(
            blank
                .error
                .as_deref()
                .is_some_and(|error| error.contains("duplicate")),
            "{blank:?}"
        );
    }

    #[test]
    fn answer_cards_validates_all_answers_before_applying_any() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let first = store.find_cards("").unwrap()[0];
        let mut second = note("Default");
        second["fields"]["Front"] = json!("Second question");
        call(&mut store, "addNote", json!({"note": second}));
        let second_id = store.find_cards("Second").unwrap()[0];

        let invalid = [
            json!({"answers": [
                {"cardId": first, "ease": 3},
                {"cardId": second_id, "ease": 9}
            ]}),
            json!({"answers": [
                {"cardId": first, "ease": 3},
                {"cardId": "x", "ease": 3}
            ]}),
        ];
        for params in invalid {
            let error = dispatch(
                &mut store,
                media(),
                "answerCards",
                &params,
                &scheduler(),
            )
            .unwrap_err();
            assert!(error.to_string().contains("invalid parameter"), "{error}");
            assert_eq!(store.card(first).unwrap().unwrap().reps, 0);
            assert_eq!(store.card(second_id).unwrap().unwrap().reps, 0);
        }
    }

    #[test]
    fn valid_answer_followed_by_bad_ease_commits_nothing() {
        let (_dir, mut store) = fixture();
        add_card(&mut store);
        let card_id = store.find_cards("").unwrap()[0];
        let error = dispatch(
            &mut store,
            media(),
            "answerCards",
            &json!({"answers": [
                {"cardId": card_id, "ease": 3},
                {"cardId": card_id, "ease": 9}
            ]}),
            &scheduler(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("ease"), "{error}");
        assert_eq!(store.card(card_id).unwrap().unwrap().reps, 0);
    }
    #[test]
    fn base64_media_rejects_trailing_junk_after_padding() {
        assert_eq!(super::decode_base64("YQ== \n").ok(), Some(b"a".to_vec()));
        for input in ["YQ==junk", "YQ=Zg==", "YQ==="] {
            assert!(super::decode_base64(input).is_err(), "{input}");
        }
    }

    #[test]
    fn media_cleanup_failure_reports_the_created_note_id() {
        let error = super::media_cleanup_error("remove stage failed", Some(42));
        let text = error.to_string();
        assert!(text.contains("42"), "{text}");
        assert!(
            text.contains("note was created but media cleanup failed"),
            "{text}"
        );
    }

    #[test]
    fn oversized_request_bodies_are_rejected() {
        let (_dir, raw_store) = fixture();
        let body: &'static str = Box::leak(
            "x".repeat(super::MAX_BODY_BYTES.saturating_add(1))
                .into_boxed_str(),
        );
        let mut request: tiny_http::Request =
            TestRequest::new().with_body(body).into();
        let store = Arc::new(Mutex::new(raw_store));
        let response = super::read_and_dispatch(
            &store,
            media(),
            &mut request,
            &scheduler(),
        );
        assert!(response.error.unwrap_or_default().contains("too large"));
        assert_eq!(response.result, Value::Null);
    }

    #[expect(
        clippy::excessive_nesting,
        reason = "the concurrency test drives two clients inside a scoped server thread"
    )]
    #[test]
    fn stalled_request_body_does_not_block_other_clients() {
        let (_dir, raw_store) = fixture();
        let store = Arc::new(Mutex::new(raw_store));
        let server = Server::http(("127.0.0.1", 0)).unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let scheduler = scheduler();

        thread::scope(|scope| {
            let runner = scope.spawn(|| {
                super::serve_incoming(&server, &store, media(), &scheduler)
            });

            let mut stalled = TcpStream::connect(addr).unwrap();
            stalled
                .write_all(
                    b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4096\r\n\r\n",
                )
                .unwrap();
            thread::sleep(Duration::from_millis(100));

            let body = r#"{"action":"version","version":6}"#;
            let request = format!(
                "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let mut client = TcpStream::connect(addr).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client.write_all(request.as_bytes()).unwrap();

            let mut received = Vec::new();
            let mut buffer = [0u8; 256];
            while !String::from_utf8_lossy(&received).contains("\"result\":6") {
                let count = client.read(&mut buffer).unwrap();
                assert!(count > 0, "connection closed before a response");
                received.extend_from_slice(&buffer[..count]);
            }

            drop(stalled);
            server.unblock();
            assert!(runner.join().unwrap().is_ok());
        });
    }
}
