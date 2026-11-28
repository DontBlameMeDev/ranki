//! Built-in MCP stdio server over the same store the CLI uses.
#![expect(
    clippy::unused_async_trait_impl,
    reason = "rmcp's tool_handler macro generates async ServerHandler methods"
)]

use std::borrow::Cow;
use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::MutexGuard;

use ranki_core::FsrsScheduler;
use ranki_model::Rating;
use ranki_model::Store;
use ranki_model::{DataPaths, Id};
use ranki_storage::Collection;
use rmcp::ErrorData;
use rmcp::Json;
use rmcp::ServiceExt;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResponse;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::tool;
use rmcp::tool_handler;
use rmcp::tool_router;
use rmcp::transport::stdio;
use serde::Deserialize;
use serde::Serialize;
use tokio::runtime::Builder;

use crate::CliError;
use crate::CliResult;
use crate::error::{ErrorCode, InterfaceError};

const MAX_DUE_LIMIT: usize = 200;

const MAX_BATCH_IDS: usize = 200;
const MAX_ELAPSED_MS: u64 = 4_294_967_295;
const MAX_OFFSET: usize = 100_000;
/// Protocol errors stay protocol errors; operational failures become tool errors.
enum ToolError {
    Protocol(ErrorData),
    Call(InterfaceError),
}

impl ToolError {
    fn call(message: impl Into<String>) -> Self {
        Self::Call(InterfaceError::message(message))
    }
}

impl From<ErrorData> for ToolError {
    fn from(error: ErrorData) -> Self {
        Self::Protocol(error)
    }
}

impl From<ranki_model::StoreError> for ToolError {
    fn from(error: ranki_model::StoreError) -> Self {
        Self::Call(error.into())
    }
}

impl From<CliError> for ToolError {
    fn from(error: CliError) -> Self {
        Self::Call(error.into())
    }
}

impl From<ranki_format::FormatError> for ToolError {
    fn from(error: ranki_format::FormatError) -> Self {
        Self::Call(error.into())
    }
}

impl IntoCallToolResult for ToolError {
    fn into_call_tool_result(self) -> Result<CallToolResponse, ErrorData> {
        match self {
            Self::Protocol(error) => Err(error),
            Self::Call(error) => {
                let (code, message) = error.into_parts();
                let text = tool_message(code, message);
                Ok(CallToolResult::error(vec![ContentBlock::text(text)]).into())
            }
        }
    }
}

fn tool_message(code: ErrorCode, message: String) -> String {
    if code == ErrorCode::Message {
        message
    } else {
        format!("[{}] {message}", code.as_str())
    }
}

fn invalid(message: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(Cow::Owned(message.into()), None)
}

fn require_confirm(confirm: bool) -> Result<(), ToolError> {
    if !confirm {
        return Err(ToolError::call(
            "this tool requires confirm: true to make changes",
        ));
    }
    Ok(())
}

fn validate_limit(limit: usize) -> Result<usize, ErrorData> {
    if limit == 0 || limit > MAX_DUE_LIMIT {
        return Err(invalid("limit must be between 1 and 200"));
    }
    Ok(limit)
}

fn validate_query(query: &str) -> Result<(), ErrorData> {
    if query.trim().is_empty() {
        return Err(invalid("query must not be empty"));
    }
    Ok(())
}

fn validate_fields(fields: &[String]) -> Result<(), ErrorData> {
    if fields.is_empty()
        || fields.first().is_none_or(|first| first.trim().is_empty())
    {
        return Err(invalid("first field must not be empty"));
    }
    Ok(())
}

fn validate_card_ids(ids: &[Id]) -> Result<(), ErrorData> {
    if ids.is_empty() || ids.len() > MAX_BATCH_IDS {
        return Err(invalid("card_ids must contain between 1 and 200 ids"));
    }
    Ok(())
}

fn validate_due_offset(offset: usize) -> Result<usize, ErrorData> {
    if offset > MAX_OFFSET {
        return Err(invalid(format!(
            "offset must be between 0 and {MAX_OFFSET}"
        )));
    }
    Ok(offset)
}

fn page_items<T>(
    items: Vec<T>,
    offset: usize,
    limit: usize,
) -> (Vec<T>, Option<usize>, bool) {
    let total = items.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    let has_more = end < total;
    (
        items.into_iter().skip(start).take(limit).collect(),
        has_more.then_some(end),
        has_more,
    )
}

fn validate_deck_id(store: &Collection, id: Id) -> Result<(), ToolError> {
    if store.deck_list()?.iter().any(|deck| deck.id == id) {
        Ok(())
    } else {
        Err(ranki_model::StoreError::NotFound(format!("deck {id}")).into())
    }
}

fn resolve_model_id(
    store: &mut Collection,
    model_id: Option<Id>,
    model_name: Option<&str>,
) -> Result<Id, ToolError> {
    match (model_id, model_name) {
        (Some(id), None) => Ok(id),
        (None, Some(name)) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(invalid("model_name must not be empty").into());
            }
            let models = store.models()?;
            if let Some(model) = models
                .iter()
                .find(|model| model.name.eq_ignore_ascii_case(trimmed))
            {
                return Ok(model.id);
            }
            let kind = ranki_model::StockModelKind::from_name(trimmed)
                .ok_or_else(|| {
                    ToolError::call(format!("unknown note type: {trimmed}"))
                })?;
            Ok(store.ensure_model(kind)?.id)
        }
        (Some(_), Some(_)) => {
            Err(invalid("use either model_id or model_name, not both").into())
        }
        (None, None) => {
            Err(invalid("model_id or model_name is required").into())
        }
    }
}

fn resolve_deck_id(
    store: &Collection,
    deck_id: Option<Id>,
    deck_name: Option<&str>,
) -> Result<Option<Id>, ToolError> {
    match (deck_id, deck_name) {
        (Some(id), None) => Ok(Some(id)),
        (None, Some(name)) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(invalid("deck_name must not be empty").into());
            }
            let decks = store.deck_list()?;
            let deck = decks
                .iter()
                .find(|deck| deck.name.eq_ignore_ascii_case(trimmed))
                .ok_or_else(|| {
                    ToolError::call(format!("deck not found: {trimmed}"))
                })?;
            Ok(Some(deck.id))
        }
        (Some(_), Some(_)) => {
            Err(invalid("use either deck_id or deck_name, not both").into())
        }
        (None, None) => Ok(None),
    }
}

fn validate_deck_name(name: &str) -> Result<&str, ErrorData> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.split("::").any(|part| part.trim().is_empty())
    {
        return Err(invalid("invalid deck name"));
    }
    Ok(trimmed)
}

fn validate_path(raw: &str) -> Result<PathBuf, ErrorData> {
    let path = Path::new(raw);
    if raw.is_empty() || !path.is_relative() {
        return Err(invalid(
            "path must be a relative path inside the collection directory",
        ));
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => {
                return Err(invalid("path must not contain '.' or '..'"));
            }
        }
    }
    if result.file_name().is_none() {
        return Err(invalid("path must name a file"));
    }
    Ok(result)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListDecksInput {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateDeckInput {
    #[schemars(
        description = "Deck name, with :: between parent and child decks"
    )]
    #[schemars(length(min = 1))]
    name: String,
    #[schemars(description = "Set true to apply the change")]
    #[schemars(extend("const" = true))]
    confirm: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenameDeckInput {
    #[schemars(description = "Id of the deck to rename")]
    id: Id,
    #[schemars(description = "New deck name, with :: between name parts")]
    #[schemars(length(min = 1))]
    name: String,
    #[schemars(description = "Set true to apply the change")]
    #[schemars(extend("const" = true))]
    confirm: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RemoveDeckInput {
    #[schemars(description = "Id of the deck to remove")]
    id: Id,
    #[schemars(description = "Set true to apply the change")]
    #[schemars(extend("const" = true))]
    confirm: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DueCardsInput {
    #[schemars(description = "Limit results to this deck and its subdecks")]
    deck_id: Option<Id>,
    #[schemars(description = "Maximum cards to return")]
    #[schemars(range(min = 1, max = 200))]
    #[serde(default = "default_limit")]
    limit: usize,
    #[schemars(description = "Skip this many cards; at most 100000")]
    #[schemars(range(min = 0, max = 100_000))]
    #[serde(default)]
    offset: usize,
    #[schemars(description = "Unix seconds; defaults to server clock")]
    now_secs: Option<i64>,
    #[schemars(
        description = "Include each card's note text; set false for cards only"
    )]
    #[schemars(extend("default" = true))]
    #[serde(default = "default_include_notes")]
    include_notes: bool,
}

const fn default_limit() -> usize {
    20
}

const fn default_include_notes() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindCardsInput {
    #[schemars(description = "Anki-style search query")]
    #[schemars(length(min = 1))]
    query: String,
    #[schemars(description = "Maximum ids to return")]
    #[schemars(range(min = 1, max = 200))]
    #[serde(default = "default_limit")]
    limit: usize,
    #[schemars(description = "Skip this many matches")]
    #[serde(default)]
    offset: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetCardInput {
    #[schemars(description = "Id of the card to read")]
    id: Id,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetCardsInput {
    #[schemars(description = "Card ids to read at once, up to 200")]
    #[schemars(length(min = 1, max = 200))]
    card_ids: Vec<Id>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetNoteInput {
    #[schemars(description = "Id of the note to read")]
    id: Id,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddNoteInput {
    #[schemars(
        description = "Id of an installed note type; use this or model_name"
    )]
    model_id: Option<Id>,
    #[schemars(
        description = "Installed or stock note type name; use this or model_id"
    )]
    model_name: Option<String>,
    #[schemars(description = "Id of the target deck; use this or deck_name")]
    deck_id: Option<Id>,
    #[schemars(
        description = "Target deck name, matched without case; use this or deck_id"
    )]
    deck_name: Option<String>,
    #[schemars(
        description = "Field values in note type order; the first field must not be blank"
    )]
    #[schemars(length(min = 1))]
    fields: Vec<String>,
    #[schemars(description = "Tags to attach to the note")]
    #[serde(default)]
    tags: Vec<String>,
    #[schemars(description = "Allow a duplicate first field when true")]
    allow_duplicate: Option<bool>,
    #[schemars(
        description = "Check duplicates in the collection or only in the deck"
    )]
    duplicate_scope: Option<DuplicateScopeInput>,
    #[schemars(description = "When checking a deck, include its subdecks")]
    check_children: Option<bool>,
    #[schemars(description = "When checking a deck, ignore the note type")]
    check_all_models: Option<bool>,
    #[schemars(description = "Set true to add the note")]
    #[schemars(extend("const" = true))]
    confirm: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum DuplicateScopeInput {
    Collection,
    Deck,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateNoteInput {
    #[schemars(description = "Id of the note to update")]
    id: Id,
    #[schemars(
        description = "Reject the edit when the note changed since it was read"
    )]
    expected_modified: Option<i64>,
    #[schemars(description = "Replace all field values in note type order")]
    fields: Option<Vec<String>>,
    #[schemars(description = "Replace all tags on the note")]
    tags: Option<Vec<String>>,
    #[schemars(description = "Id of the replacement note type")]
    model_id: Option<Id>,
    #[schemars(
        description = "Name of the replacement installed or stock note type"
    )]
    model_name: Option<String>,
    #[schemars(description = "Id of the target deck; use this or deck_name")]
    deck_id: Option<Id>,
    #[schemars(
        description = "Target deck name, matched without case; use this or deck_id"
    )]
    deck_name: Option<String>,
    #[schemars(description = "Set true to apply the change")]
    #[schemars(extend("const" = true))]
    confirm: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AnswerCardInput {
    #[schemars(description = "Id of the card to answer")]
    card_id: Id,
    #[schemars(description = "Review rating for this answer")]
    rating: RatingInput,
    #[schemars(description = "Time spent on the answer in milliseconds")]
    #[schemars(default, range(min = 0, max = 4_294_967_295_u64))]
    #[serde(default)]
    elapsed_ms: u64,
    #[schemars(description = "Unix seconds; defaults to server clock")]
    now_secs: Option<i64>,
    #[schemars(description = "Set true to record the review")]
    #[schemars(extend("const" = true))]
    confirm: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum RatingInput {
    Again,
    Hard,
    Good,
    Easy,
}

impl From<RatingInput> for Rating {
    fn from(value: RatingInput) -> Self {
        match value {
            RatingInput::Again => Self::Again,
            RatingInput::Hard => Self::Hard,
            RatingInput::Good => Self::Good,
            RatingInput::Easy => Self::Easy,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StatsInput {
    #[schemars(description = "Limit counts to this deck and its subdecks")]
    deck_id: Option<Id>,
    #[schemars(description = "Unix seconds; defaults to server clock")]
    now_secs: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PackageInput {
    #[schemars(
        description = "Relative package path inside the collection directory"
    )]
    #[schemars(length(min = 1))]
    path: String,
    #[schemars(description = "Set true to run the package operation")]
    #[schemars(extend("const" = true))]
    confirm: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DeckView {
    id: Id,
    name: String,
    description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CardTypeView {
    New,
    Learning,
    Review,
    Relearning,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CardQueueView {
    New,
    Learning,
    Review,
    DayLearning,
    Suspended,
    Buried,
    Preview,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CardView {
    id: Id,
    note_id: Id,
    deck_id: Id,
    ordinal: u16,
    modified: i64,
    usn: i32,
    card_type: CardTypeView,
    queue: CardQueueView,
    due: i64,
    interval: i32,
    factor: u32,
    reps: u32,
    lapses: u32,
    left: u32,
    original_due: i64,
    original_deck_id: Id,
    flags: i32,
    data: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct NoteView {
    id: Id,
    guid: String,
    model_id: Id,
    modified: i64,
    usn: i32,
    tags: Vec<String>,
    fields: Vec<String>,
    sort_field: String,
    checksum: i32,
    flags: i32,
    data: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum RevLogKindView {
    Learn,
    Review,
    Relearn,
    Filtered,
    Manual,
    Rescheduled,
}

#[derive(Debug, Serialize, JsonSchema)]
struct RevLogView {
    id: Id,
    card_id: Id,
    usn: i32,
    ease: u8,
    interval: i32,
    last_interval: i32,
    factor: u32,
    time_ms: u32,
    kind: RevLogKindView,
}

#[expect(
    clippy::struct_field_names,
    reason = "the MCP schema fixes these field names"
)]
#[derive(Debug, Serialize, JsonSchema)]
struct StatsView {
    new_cards: usize,
    learning_cards: usize,
    review_cards: usize,
    total_cards: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DeckList {
    decks: Vec<DeckView>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ModelView {
    id: Option<Id>,
    name: String,
    fields: Vec<String>,
    templates: Vec<String>,
}

fn model_view(model: &ranki_model::NoteModel) -> ModelView {
    ModelView {
        id: (model.id != 0).then_some(model.id),
        name: model.name.clone(),
        fields: model.fields.clone(),
        templates: model
            .templates
            .iter()
            .map(|template| template.name.clone())
            .collect(),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
struct ModelList {
    models: Vec<ModelView>,
    stock_models: Vec<ModelView>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct DueCardView {
    #[serde(flatten)]
    card: CardView,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<NoteView>,
}
#[derive(Debug, Serialize, JsonSchema)]
struct DueCards {
    cards: Vec<DueCardView>,
    now_secs: i64,
    next_offset: Option<usize>,
    has_more: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
#[expect(
    clippy::struct_field_names,
    reason = "the MCP schema fixes this field name"
)]
struct CardIds {
    card_ids: Vec<Id>,
    next_offset: Option<usize>,
    has_more: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CardAndNote {
    card: Option<CardView>,
    note: Option<NoteView>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct CardList {
    cards: Vec<CardAndNote>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct NoteCards {
    card_ids: Vec<Id>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ReviewResult {
    card: CardView,
    revlog: RevLogView,
}

#[derive(Debug, Serialize, JsonSchema)]
struct StatsResult {
    stats: StatsView,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ImportResult {
    path: String,
    notes_added: usize,
    cards_added: usize,
    media_count: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ExportResult {
    path: String,
    bytes: u64,
}
fn deck_view(deck: &ranki_model::Deck) -> DeckView {
    DeckView {
        id: deck.id,
        name: deck.name.clone(),
        description: deck.description.clone(),
    }
}

fn card_view(card: &ranki_model::Card) -> CardView {
    let card_type = match card.card_type {
        ranki_model::CardType::New => CardTypeView::New,
        ranki_model::CardType::Learning => CardTypeView::Learning,
        ranki_model::CardType::Review => CardTypeView::Review,
        ranki_model::CardType::Relearning => CardTypeView::Relearning,
    };
    let queue_view = match card.queue {
        ranki_model::CardQueue::New => CardQueueView::New,
        ranki_model::CardQueue::Learning => CardQueueView::Learning,
        ranki_model::CardQueue::Review => CardQueueView::Review,
        ranki_model::CardQueue::DayLearning => CardQueueView::DayLearning,
        ranki_model::CardQueue::Suspended => CardQueueView::Suspended,
        ranki_model::CardQueue::Buried
        | ranki_model::CardQueue::ManuallyBuried => CardQueueView::Buried,
        ranki_model::CardQueue::Preview => CardQueueView::Preview,
    };
    CardView {
        id: card.id,
        note_id: card.note_id,
        deck_id: card.deck_id,
        ordinal: card.ordinal,
        modified: card.modified,
        usn: card.usn,
        card_type,
        queue: queue_view,
        due: card.due,
        interval: card.interval,
        factor: card.factor,
        reps: card.reps,
        lapses: card.lapses,
        left: card.left,
        original_due: card.original_due,
        original_deck_id: card.original_deck_id,
        flags: card.flags,
        data: card.data.clone(),
    }
}

fn note_view(note: &ranki_model::Note) -> NoteView {
    NoteView {
        id: note.id,
        guid: note.guid.clone(),
        model_id: note.model_id,
        modified: note.modified,
        usn: note.usn,
        tags: note.tags.clone(),
        fields: note.fields.clone(),
        sort_field: note.sort_field.clone(),
        checksum: note.checksum,
        flags: note.flags,
        data: note.data.clone(),
    }
}

const fn kind_view(kind: ranki_model::RevLogKind) -> RevLogKindView {
    match kind {
        ranki_model::RevLogKind::Learn => RevLogKindView::Learn,
        ranki_model::RevLogKind::Review => RevLogKindView::Review,
        ranki_model::RevLogKind::Relearn => RevLogKindView::Relearn,
        ranki_model::RevLogKind::Filtered => RevLogKindView::Filtered,
        ranki_model::RevLogKind::Manual => RevLogKindView::Manual,
        ranki_model::RevLogKind::Rescheduled => RevLogKindView::Rescheduled,
    }
}

const fn revlog_view(log: &ranki_model::RevLog) -> RevLogView {
    RevLogView {
        id: log.id,
        card_id: log.card_id,
        usn: log.usn,
        ease: log.ease,
        interval: log.interval,
        last_interval: log.last_interval,
        factor: log.factor,
        time_ms: log.time_ms,
        kind: kind_view(log.kind),
    }
}

struct RankiMcp {
    collection: Mutex<Collection>,
    paths: DataPaths,
    root: PathBuf,
    scheduler: FsrsScheduler,
}

impl RankiMcp {
    fn new(
        collection: Collection,
        paths: DataPaths,
        scheduler: FsrsScheduler,
    ) -> Self {
        let parent =
            collection.path().parent().unwrap_or_else(|| Path::new("."));
        let root =
            fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        Self {
            collection: Mutex::new(collection),
            paths,
            root,
            scheduler,
        }
    }

    fn store(&self) -> Result<MutexGuard<'_, Collection>, ToolError> {
        self.collection
            .lock()
            .map_err(|_| ToolError::call("collection lock poisoned"))
    }

    fn file(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let relative = validate_path(raw)?;
        let candidate = self.root.join(relative);
        let parent = candidate
            .parent()
            .ok_or_else(|| ToolError::call("path has no parent directory"))?;
        let canonical_parent = parent.canonicalize().map_err(|error| {
            ToolError::call(format!("cannot resolve path: {error}"))
        })?;
        let name = candidate
            .file_name()
            .ok_or_else(|| ToolError::call("path must name a file"))?;
        let resolved = canonical_parent.join(name);
        if !resolved.starts_with(&self.root) {
            return Err(ToolError::call(
                "path is outside the collection directory",
            ));
        }
        if fs::symlink_metadata(&resolved)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(ToolError::call(format!(
                "package path must not be a symlink: {raw}"
            )));
        }
        Ok(resolved)
    }

    fn media_dir(&self) -> PathBuf {
        self.paths.media_dir.clone()
    }
}

#[tool_router]
impl RankiMcp {
    #[tool(
        title = "List decks",
        description = "List decks in the active collection",
        annotations(
            title = "List decks",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn list_decks(
        &self,
        Parameters(_): Parameters<ListDecksInput>,
    ) -> Result<Json<DeckList>, ToolError> {
        let decks = self.store()?.decks()?;
        Ok(Json(DeckList {
            decks: decks.iter().map(deck_view).collect(),
        }))
    }

    #[tool(
        title = "List models",
        description = "List installed note types and available stock note types",
        annotations(
            title = "List models",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn list_models(
        &self,
        Parameters(_): Parameters<ListDecksInput>,
    ) -> Result<Json<ModelList>, ToolError> {
        let models = self.store()?.models()?;
        Ok(Json(ModelList {
            models: models.iter().map(model_view).collect(),
            stock_models: ranki_model::stock_models()
                .iter()
                .map(model_view)
                .collect(),
        }))
    }

    #[tool(
        title = "Create deck",
        description = "Create a deck in the active collection",
        annotations(
            title = "Create deck",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn create_deck(
        &self,
        Parameters(input): Parameters<CreateDeckInput>,
    ) -> Result<Json<DeckView>, ToolError> {
        require_confirm(input.confirm)?;
        let name = validate_deck_name(&input.name)?;
        let deck = self.store()?.create_deck(name)?;
        Ok(Json(deck_view(&deck)))
    }

    #[tool(
        title = "Rename deck",
        description = "Rename a deck and its subdecks",
        annotations(
            title = "Rename deck",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn rename_deck(
        &self,
        Parameters(input): Parameters<RenameDeckInput>,
    ) -> Result<Json<DeckView>, ToolError> {
        require_confirm(input.confirm)?;
        let name = validate_deck_name(&input.name)?;
        let deck = self.store()?.rename_deck(input.id, name)?;
        Ok(Json(deck_view(&deck)))
    }

    #[tool(
        title = "Remove deck",
        description = "Remove a deck, its subdecks, and their cards",
        annotations(
            title = "Remove deck",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn remove_deck(
        &self,
        Parameters(input): Parameters<RemoveDeckInput>,
    ) -> Result<Json<DeckView>, ToolError> {
        require_confirm(input.confirm)?;
        let mut store = self.store()?;
        let deck = store
            .deck_list()?
            .into_iter()
            .find(|deck| deck.id == input.id)
            .ok_or_else(|| {
                ToolError::from(ranki_model::StoreError::NotFound(format!(
                    "deck {}",
                    input.id
                )))
            })?;
        store.remove_deck(input.id)?;
        Ok(Json(deck_view(&deck)))
    }

    #[tool(
        title = "Due cards",
        description = "List due cards with their note text",
        annotations(
            title = "Due cards",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn due_cards(
        &self,
        Parameters(input): Parameters<DueCardsInput>,
    ) -> Result<Json<DueCards>, ToolError> {
        let limit = validate_limit(input.limit)?;
        let offset = validate_due_offset(input.offset)?;
        let now = match input.now_secs {
            Some(value) => value,
            None => crate::now_secs()?,
        };
        let store = self.store()?;
        if let Some(id) = input.deck_id {
            validate_deck_id(&store, id)?;
        }
        let fetched = store.due_cards(
            input.deck_id,
            offset.saturating_add(limit).saturating_add(1),
            now,
        )?;
        let (cards, next_offset, has_more) = page_items(fetched, offset, limit);
        let notes = if input.include_notes {
            cards
                .iter()
                .map(|card| store.note(card.note_id))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            (0..cards.len()).map(|_| None).collect()
        };
        Ok(Json(DueCards {
            cards: cards
                .iter()
                .zip(notes)
                .map(|(card, note)| DueCardView {
                    card: card_view(card),
                    note: note.as_ref().map(note_view),
                })
                .collect(),
            now_secs: now,
            next_offset,
            has_more,
        }))
    }

    #[tool(
        title = "Find cards",
        description = "Find card ids with an Anki search query",
        annotations(
            title = "Find cards",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn find_cards(
        &self,
        Parameters(input): Parameters<FindCardsInput>,
    ) -> Result<Json<CardIds>, ToolError> {
        validate_query(&input.query)?;
        let limit = validate_limit(input.limit)?;
        let offset = input.offset;
        let store = self.store()?;
        let mut found = store.find_cards_page(
            &input.query,
            limit.saturating_add(1),
            offset,
        )?;
        let has_more = found.len() > limit;
        found.truncate(limit);
        Ok(Json(CardIds {
            card_ids: found,
            next_offset: has_more.then_some(offset.saturating_add(limit)),
            has_more,
        }))
    }

    #[tool(
        title = "Get card",
        description = "Read one card and its note",
        annotations(
            title = "Get card",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn get_card(
        &self,
        Parameters(input): Parameters<GetCardInput>,
    ) -> Result<Json<CardAndNote>, ToolError> {
        let store = self.store()?;
        let card = store.card(input.id)?.ok_or_else(|| {
            ToolError::from(ranki_model::StoreError::NotFound(format!(
                "card {}",
                input.id
            )))
        })?;
        let note = store.note(card.note_id)?;
        Ok(Json(CardAndNote {
            card: Some(card_view(&card)),
            note: note.as_ref().map(note_view),
        }))
    }

    #[tool(
        title = "Get cards",
        description = "Read up to 200 cards and their notes by id",
        annotations(
            title = "Get cards",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn get_cards(
        &self,
        Parameters(input): Parameters<GetCardsInput>,
    ) -> Result<Json<CardList>, ToolError> {
        validate_card_ids(&input.card_ids)?;
        let store = self.store()?;
        let cards = input
            .card_ids
            .iter()
            .map(|id| -> Result<CardAndNote, ToolError> {
                let card = store.card(*id)?;
                let note = match &card {
                    Some(existing) => store.note(existing.note_id)?,
                    None => None,
                };
                Ok(CardAndNote {
                    card: card.as_ref().map(card_view),
                    note: note.as_ref().map(note_view),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Json(CardList { cards }))
    }

    #[tool(
        title = "Get note",
        description = "Read one note by id",
        annotations(
            title = "Get note",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn get_note(
        &self,
        Parameters(input): Parameters<GetNoteInput>,
    ) -> Result<Json<NoteView>, ToolError> {
        let note = self.store()?.note(input.id)?.ok_or_else(|| {
            ToolError::from(ranki_model::StoreError::NotFound(format!(
                "note {}",
                input.id
            )))
        })?;
        Ok(Json(note_view(&note)))
    }

    #[tool(
        title = "Add note",
        description = "Add a note and its generated cards",
        annotations(
            title = "Add note",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn add_note(
        &self,
        Parameters(input): Parameters<AddNoteInput>,
    ) -> Result<Json<NoteCards>, ToolError> {
        require_confirm(input.confirm)?;
        validate_fields(&input.fields)?;
        let mut store = self.store()?;
        let deck_id =
            resolve_deck_id(&store, input.deck_id, input.deck_name.as_deref())?
                .ok_or_else(|| invalid("deck_id or deck_name is required"))?;
        let model_id = resolve_model_id(
            &mut store,
            input.model_id,
            input.model_name.as_deref(),
        )?;
        let note = ranki_model::NewNote {
            model_id,
            deck_id,
            fields: input.fields,
            tags: input.tags,
        };
        let duplicate_scope = match input
            .duplicate_scope
            .unwrap_or(DuplicateScopeInput::Collection)
        {
            DuplicateScopeInput::Collection => {
                ranki_model::DuplicateScope::new_collection()
            }
            DuplicateScopeInput::Deck => ranki_model::DuplicateScope {
                use_deck: true,
                deck_id: Some(deck_id),
                check_children: input.check_children.unwrap_or(false),
                check_all_models: input.check_all_models.unwrap_or(false),
            },
        };
        let options = ranki_model::AddNoteOptions {
            allow_duplicate: input.allow_duplicate.unwrap_or(false),
            duplicate_scope,
        };
        let card_ids = store.add_note_with_options(&note, &options)?;
        Ok(Json(NoteCards { card_ids }))
    }

    #[tool(
        title = "Update note",
        description = "Update a note's fields, tags, note type, or deck",
        annotations(
            title = "Update note",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn update_note(
        &self,
        Parameters(input): Parameters<UpdateNoteInput>,
    ) -> Result<Json<NoteView>, ToolError> {
        require_confirm(input.confirm)?;
        if let Some(fields) = &input.fields {
            validate_fields(fields)?;
        }
        let mut store = self.store()?;
        let mut note = store.note(input.id)?.ok_or_else(|| {
            ToolError::from(ranki_model::StoreError::NotFound(format!(
                "note {}",
                input.id
            )))
        })?;
        if input
            .expected_modified
            .is_some_and(|expected| expected != note.modified)
        {
            return Err(ToolError::call("note changed; read it again"));
        }
        if let Some(fields) = input.fields {
            note.fields = fields;
        }
        if let Some(tags) = input.tags {
            note.tags = tags;
        }
        if input.model_id.is_some() || input.model_name.is_some() {
            note.model_id = resolve_model_id(
                &mut store,
                input.model_id,
                input.model_name.as_deref(),
            )?;
        }
        match resolve_deck_id(
            &store,
            input.deck_id,
            input.deck_name.as_deref(),
        )? {
            Some(deck_id) => store.update_note(&note, deck_id)?,
            None => store.update_note_checked(&note, None, None)?,
        }
        let updated = store.note(input.id)?.ok_or_else(|| {
            ToolError::from(ranki_model::StoreError::NotFound(format!(
                "note {}",
                input.id
            )))
        })?;
        Ok(Json(note_view(&updated)))
    }

    #[tool(
        title = "Answer card",
        description = "Answer a card and record a review",
        annotations(
            title = "Answer card",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn answer_card(
        &self,
        Parameters(input): Parameters<AnswerCardInput>,
    ) -> Result<Json<ReviewResult>, ToolError> {
        require_confirm(input.confirm)?;
        let answer_ms = u32::try_from(input.elapsed_ms).map_err(|_| {
            invalid(format!("elapsed_ms must be at most {MAX_ELAPSED_MS}"))
        })?;
        let now = match input.now_secs {
            Some(value) => value,
            None => crate::now_secs()?,
        };
        let mut store = self.store()?;
        let card = store.card(input.card_id)?.ok_or_else(|| {
            ToolError::from(ranki_model::StoreError::NotFound(format!(
                "card {}",
                input.card_id
            )))
        })?;
        let timing = store.timing(now)?;
        let last_review_secs = store.last_review_secs(input.card_id)?;
        let scheduler =
            ranki_core::scheduler_for_card(&self.scheduler, &*store, &card);
        let decision = scheduler
            .schedule_timed(
                &card,
                Rating::from(input.rating),
                &timing,
                last_review_secs,
                answer_ms,
            )
            .ok_or_else(|| {
                ToolError::call(format!("card {} is not due", input.card_id))
            })?;
        store.apply_review(&decision)?;
        Ok(Json(ReviewResult {
            card: card_view(&decision.card),
            revlog: revlog_view(&decision.revlog),
        }))
    }

    #[tool(
        title = "Deck stats",
        description = "Return card counts for a deck",
        annotations(
            title = "Deck stats",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn stats(
        &self,
        Parameters(input): Parameters<StatsInput>,
    ) -> Result<Json<StatsResult>, ToolError> {
        let now = match input.now_secs {
            Some(value) => value,
            None => crate::now_secs()?,
        };
        let store = self.store()?;
        if let Some(id) = input.deck_id {
            validate_deck_id(&store, id)?;
        }
        let stats = store.stats(input.deck_id, now)?;
        Ok(Json(StatsResult {
            stats: StatsView {
                new_cards: stats.new_cards,
                learning_cards: stats.learning_cards,
                review_cards: stats.review_cards,
                total_cards: stats.total_cards,
            },
        }))
    }

    #[tool(
        title = "Import package",
        description = "Import an Anki package from the collection directory",
        annotations(
            title = "Import package",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn import_apkg(
        &self,
        Parameters(input): Parameters<PackageInput>,
    ) -> Result<Json<ImportResult>, ToolError> {
        require_confirm(input.confirm)?;
        let path = self.file(&input.path)?;
        if !path.is_file() {
            return Err(ToolError::call(format!(
                "package not found: {}",
                input.path
            )));
        }
        self.store()?.checkpoint()?;
        let report = ranki_format::import_apkg(
            &path,
            &self.paths.collection,
            &self.media_dir(),
        )?;
        Ok(Json(ImportResult {
            path: input.path,
            notes_added: report.notes,
            cards_added: report.cards,
            media_count: report.media,
        }))
    }

    #[tool(
        title = "Export package",
        description = "Export the collection to a new Anki package",
        annotations(
            title = "Export package",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn export_apkg(
        &self,
        Parameters(input): Parameters<PackageInput>,
    ) -> Result<Json<ExportResult>, ToolError> {
        require_confirm(input.confirm)?;
        let path = self.file(&input.path)?;
        if path.exists() {
            return Err(ToolError::call(format!(
                "export target already exists: {}",
                input.path
            )));
        }
        {
            let store = self.store()?;
            store.checkpoint()?;
        }
        ranki_format::export_apkg(
            &self.paths.collection,
            &self.media_dir(),
            &path,
        )?;
        let bytes = fs::metadata(&path)
            .map_err(|error| ToolError::call(error.to_string()))?
            .len();
        Ok(Json(ExportResult {
            path: input.path,
            bytes,
        }))
    }
}

#[tool_handler(
    name = "Ranki",
    instructions = "Ranki manages an Anki-compatible collection. Use list_models to find note types, due_cards to study, and confirm: true for writes. Package paths are relative to the collection directory."
)]
impl rmcp::ServerHandler for RankiMcp {}

pub fn collection_path(paths: &DataPaths) -> CliResult<PathBuf> {
    let path = paths.collection.clone();
    fs::canonicalize(&path).map_err(|error| {
        CliError::Message(format!(
            "collection not found: {}: {error}",
            path.display()
        ))
    })
}

pub fn serve(paths: &DataPaths, scheduler: FsrsScheduler) -> CliResult<()> {
    let path = collection_path(paths)?;
    let resolved_paths = DataPaths {
        collection: path.clone(),
        ..paths.clone()
    };
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Message(error.to_string()))?;
    runtime.block_on(async move {
        let collection = Collection::open(&path)?;
        let server = RankiMcp::new(collection, resolved_paths, scheduler);
        let service = server.serve(stdio()).await.map_err(|error| {
            CliError::Message(format!("mcp server: {error}"))
        })?;
        service.waiting().await.map_err(|error| {
            CliError::Message(format!("mcp server: {error}"))
        })?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::IntoCallToolResult;
    use super::Json;
    use super::RankiMcp;
    use super::ToolError;
    use super::card_view;
    use super::page_items;
    use super::validate_card_ids;
    use super::validate_deck_name;
    use super::validate_due_offset;
    use super::validate_fields;
    use super::validate_limit;
    use super::validate_path;
    use super::validate_query;
    use crate::error::ErrorCode;
    use ranki_model::Id;
    use ranki_model::Store;
    use ranki_storage::Collection;
    use rmcp::ServerHandler;
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::schemars::schema_for;
    use serde_json::json;
    use tempfile::TempDir;

    use super::AddNoteInput;
    use super::AnswerCardInput;
    use super::CreateDeckInput;
    use super::DueCardsInput;
    use super::FindCardsInput;
    use super::GetCardInput;
    use super::GetCardsInput;
    use super::GetNoteInput;
    use super::ListDecksInput;
    use super::PackageInput;
    use super::RatingInput;
    use super::RemoveDeckInput;
    use super::RenameDeckInput;
    use super::StatsInput;
    use super::UpdateNoteInput;

    fn ok<T>(result: Result<Json<T>, ToolError>) -> T {
        result
            .map(|value| value.0)
            .ok()
            .unwrap_or_else(|| panic!("tool failed"))
    }

    fn assert_not_found(error: ToolError) {
        let ToolError::Call(mapped) = error else {
            panic!("missing records must be tool errors");
        };
        assert_eq!(mapped.code(), ErrorCode::NotFound);
    }

    fn assert_schema_descriptions(value: &serde_json::Value) {
        for (name, property) in
            value["properties"].as_object().into_iter().flatten()
        {
            assert!(
                property.get("description").is_some(),
                "{name} needs a schema description"
            );
        }
    }

    fn add_input(front: &str) -> AddNoteInput {
        AddNoteInput {
            model_id: Some(1),
            model_name: None,
            deck_id: Some(1),
            deck_name: None,
            fields: vec![front.to_owned(), String::from("Back")],
            tags: Vec::new(),
            allow_duplicate: None,
            duplicate_scope: None,
            check_children: None,
            check_all_models: None,
            confirm: true,
        }
    }

    fn find_input(query: &str) -> FindCardsInput {
        FindCardsInput {
            query: query.to_owned(),
            limit: 20,
            offset: 0,
        }
    }

    fn due_input() -> DueCardsInput {
        DueCardsInput {
            deck_id: None,
            limit: 20,
            offset: 0,
            now_secs: Some(1_000_000),
            include_notes: true,
        }
    }

    fn answer_input(card_id: Id) -> AnswerCardInput {
        AnswerCardInput {
            card_id,
            rating: RatingInput::Good,
            elapsed_ms: 1234,
            now_secs: None,
            confirm: true,
        }
    }

    fn add_card(server: &RankiMcp, front: &str) -> Id {
        ok(server.add_note(Parameters(add_input(front)))).card_ids[0]
    }

    fn mcp_server() -> (TempDir, RankiMcp) {
        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        let collection = Collection::create(&paths.collection).unwrap();
        let scheduler =
            ranki_core::from_settings(&ranki_model::Settings::default());
        let server = RankiMcp::new(collection, paths, scheduler);
        (directory, server)
    }

    #[test]
    fn tool_errors_keep_the_source_error_code() {
        let error = ToolError::from(ranki_model::StoreError::NotFound(
            String::from("card"),
        ));
        let ToolError::Call(mapped) = error else {
            panic!("store failure must be a tool call error");
        };
        assert_eq!(mapped.code(), ErrorCode::NotFound);
    }

    #[test]
    fn tool_metadata_is_discoverable() {
        let (_directory, server) = mcp_server();
        let info = <RankiMcp as ServerHandler>::get_info(&server);
        assert_eq!(info.server_info.name, "Ranki");
        assert!(
            info.instructions
                .as_deref()
                .is_some_and(|text| text.contains("confirm: true"))
        );

        let tools = RankiMcp::tool_router().list_all();
        assert_eq!(tools.len(), 16);
        for tool in tools {
            assert!(tool.title.is_some(), "{} needs a title", tool.name);
            assert!(
                tool.output_schema.is_some(),
                "{} needs an outputSchema",
                tool.name
            );
            let annotations = tool.annotations.as_ref().unwrap();
            assert!(annotations.title.is_some(), "{} needs a title", tool.name);
            assert_eq!(annotations.open_world_hint, Some(false));
        }
    }

    #[test]
    fn input_schemas_close_unknown_fields() {
        for schema in [
            schema_for!(ListDecksInput),
            schema_for!(DueCardsInput),
            schema_for!(FindCardsInput),
            schema_for!(GetCardsInput),
            schema_for!(GetNoteInput),
            schema_for!(AddNoteInput),
            schema_for!(UpdateNoteInput),
            schema_for!(CreateDeckInput),
            schema_for!(RenameDeckInput),
            schema_for!(RemoveDeckInput),
            schema_for!(AnswerCardInput),
            schema_for!(StatsInput),
            schema_for!(PackageInput),
        ] {
            let value = serde_json::to_value(schema).unwrap();
            assert_eq!(value["additionalProperties"], json!(false));
            assert_eq!(value["type"], json!("object"));
            assert_schema_descriptions(&value);
        }
    }

    #[test]
    fn write_tools_require_const_true() {
        for schema in [
            schema_for!(AddNoteInput),
            schema_for!(UpdateNoteInput),
            schema_for!(CreateDeckInput),
            schema_for!(RenameDeckInput),
            schema_for!(RemoveDeckInput),
            schema_for!(AnswerCardInput),
            schema_for!(PackageInput),
        ] {
            let value = serde_json::to_value(schema).unwrap();
            assert_eq!(value["properties"]["confirm"]["const"], json!(true));
            assert!(
                value["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("confirm"))
            );
        }
    }

    #[test]
    fn query_limit_offset_and_page_schemas_match_the_design() {
        let value = serde_json::to_value(schema_for!(FindCardsInput)).unwrap();
        assert_eq!(value["properties"]["query"]["minLength"], json!(1));
        assert_eq!(value["properties"]["limit"]["minimum"], json!(1));
        assert_eq!(value["properties"]["limit"]["maximum"], json!(200));
        assert_eq!(value["properties"]["limit"]["default"], json!(20));

        let value = serde_json::to_value(schema_for!(DueCardsInput)).unwrap();
        assert_eq!(value["properties"]["limit"]["default"], json!(20));
        assert_eq!(
            value["properties"]["include_notes"]["default"],
            json!(true)
        );
        assert_eq!(value["properties"]["offset"]["maximum"], json!(100_000));

        let value = serde_json::to_value(schema_for!(AnswerCardInput)).unwrap();
        assert_eq!(
            value["properties"]["elapsed_ms"]["maximum"],
            json!(4_294_967_295_u64)
        );

        let value = serde_json::to_value(schema_for!(GetCardsInput)).unwrap();
        assert_eq!(value["properties"]["card_ids"]["maxItems"], json!(200));

        let value = serde_json::to_value(schema_for!(PackageInput)).unwrap();
        assert_eq!(value["properties"]["path"]["minLength"], json!(1));

        let value = serde_json::to_value(schema_for!(AddNoteInput)).unwrap();
        assert!(
            serde_json::to_string(&value)
                .unwrap()
                .contains("collection")
        );
    }

    #[test]
    fn limit_offset_card_ids_and_fields_validate_bounds() {
        assert_eq!(validate_limit(20).unwrap(), 20);
        assert!(validate_limit(0).is_err());
        assert!(validate_limit(201).is_err());
        assert_eq!(validate_due_offset(0).unwrap(), 0);
        assert!(validate_due_offset(100_001).is_err());
        assert_eq!(page_items(vec![1, 2, 3], 2, 2), (vec![3], None, false));
        assert_eq!(
            page_items(vec![1, 2, 3], 0, 2),
            (vec![1, 2], Some(2), true)
        );
        assert!(validate_card_ids(&[1]).is_ok());
        assert!(validate_card_ids(&[]).is_err());
        assert!(validate_card_ids(&vec![1; 201]).is_err());
        assert!(validate_fields(&[String::from("Front")]).is_ok());
        assert!(validate_fields(&[String::new()]).is_err());
        assert!(
            validate_fields(&[String::from("Front"), String::new()]).is_ok()
        );
    }

    #[test]
    fn query_and_path_validation_reject_unsafe_values() {
        assert!(validate_query("deck:Default").is_ok());
        assert!(validate_query("  ").is_err());
        assert_eq!(
            validate_path("backup.apkg").unwrap(),
            PathBuf::from("backup.apkg")
        );
        assert!(validate_path("../other.apkg").is_err());
        assert!(validate_path("/tmp/other.apkg").is_err());
        assert!(validate_path("").is_err());
        assert!(validate_path(".").is_err());
    }

    #[test]
    fn unknown_fields_fail_deserialization() {
        let result = serde_json::from_value::<AddNoteInput>(json!({
            "model_id": 1, "deck_id": 1, "fields": ["a"], "confirm": true, "extra": 1,
        }));
        assert!(result.is_err());
    }

    #[test]
    fn false_confirmation_deserializes_but_is_rejected_by_the_tool() {
        let parsed: AddNoteInput = serde_json::from_value(json!({
            "model_id": 1, "deck_id": 1, "fields": ["a"], "confirm": false,
        }))
        .unwrap();
        assert!(!parsed.confirm);
    }

    #[test]
    fn rating_enum_is_lowercase() {
        let parsed: AnswerCardInput = serde_json::from_value(json!({
            "card_id": 1, "rating": "good", "confirm": true,
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(parsed.rating).unwrap(), json!("good"));
        assert!(
            serde_json::from_value::<AnswerCardInput>(json!({
                "card_id": 1, "rating": "very-good", "confirm": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn queue_mapping_follows_the_stored_queue() {
        let (_directory, server) = mcp_server();
        let card_id = add_card(&server, "Queue");
        let mut card = server
            .store()
            .unwrap_or_else(|_| panic!("store lock poisoned"))
            .card(card_id)
            .unwrap()
            .unwrap();
        for (queue, expected) in [
            (ranki_model::CardQueue::New, "new"),
            (ranki_model::CardQueue::Learning, "learning"),
            (ranki_model::CardQueue::Review, "review"),
            (ranki_model::CardQueue::DayLearning, "day_learning"),
            (ranki_model::CardQueue::Suspended, "suspended"),
            (ranki_model::CardQueue::Buried, "buried"),
            (ranki_model::CardQueue::ManuallyBuried, "buried"),
            (ranki_model::CardQueue::Preview, "preview"),
        ] {
            card.queue = queue;
            let value = serde_json::to_value(card_view(&card)).unwrap();
            assert_eq!(value["queue"], json!(expected));
        }
    }

    #[test]
    fn deck_name_validation_rejects_blank_and_broken_hierarchies() {
        assert_eq!(validate_deck_name(" A::B ").unwrap(), "A::B");
        assert!(validate_deck_name("").is_err());
        assert!(validate_deck_name("A::").is_err());
        assert!(validate_deck_name("::B").is_err());
    }

    #[test]
    fn deck_tools_create_rename_and_remove() {
        let (_directory, server) = mcp_server();
        let created = ok(server.create_deck(Parameters(CreateDeckInput {
            name: String::from("Study::Languages"),
            confirm: true,
        })));
        assert_eq!(created.name, "Study::Languages");

        let renamed = ok(server.rename_deck(Parameters(RenameDeckInput {
            id: created.id,
            name: String::from("Learning"),
            confirm: true,
        })));
        assert_eq!(renamed.id, created.id);
        assert_eq!(renamed.name, "Learning");

        let rejected = server
            .remove_deck(Parameters(RemoveDeckInput {
                id: created.id,
                confirm: false,
            }))
            .err()
            .unwrap_or_else(|| panic!("unconfirmed removal must fail"));
        let ToolError::Call(error) = rejected else {
            panic!("unconfirmed removal must be a tool error");
        };
        assert_eq!(error.code(), ErrorCode::Message);

        let removed = ok(server.remove_deck(Parameters(RemoveDeckInput {
            id: created.id,
            confirm: true,
        })));
        assert_eq!(removed.id, created.id);
        let decks = ok(server.list_decks(Parameters(ListDecksInput {}))).decks;
        assert!(!decks.iter().any(|deck| deck.id == created.id));
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end MCP note workflow"
    )]
    #[test]
    fn note_tools_create_read_update_answer_and_report_cards() {
        let (_directory, server) = mcp_server();
        let deck = ok(server.create_deck(Parameters(CreateDeckInput {
            name: String::from("Study"),
            confirm: true,
        })));
        let mut input = add_input("Front");
        input.deck_id = None;
        input.deck_name = Some(String::from("study"));
        input.model_id = None;
        input.model_name = Some(String::from("Basic"));
        let added = ok(server.add_note(Parameters(input)));
        assert_eq!(added.card_ids.len(), 1);
        let card_id = added.card_ids[0];

        let found = ok(server.find_cards(Parameters(find_input("Front"))));
        assert_eq!(found.card_ids, vec![card_id]);
        assert!(!found.has_more);
        assert_eq!(found.next_offset, None);

        let due = ok(server.due_cards(Parameters(due_input())));
        assert_eq!(due.now_secs, 1_000_000);
        assert_eq!(due.cards[0].card.id, card_id);
        assert_eq!(
            due.cards[0].note.as_ref().unwrap().fields,
            vec!["Front", "Back"]
        );

        let without_notes = ok(server.due_cards(Parameters(DueCardsInput {
            include_notes: false,
            ..due_input()
        })));
        assert!(without_notes.cards[0].note.is_none());

        let read =
            ok(server.get_card(Parameters(GetCardInput { id: card_id })));
        let note_id = read.note.unwrap().id;
        assert_eq!(read.card.unwrap().id, card_id);

        let note =
            ok(server.get_note(Parameters(GetNoteInput { id: note_id })));
        assert_eq!(note.fields, vec!["Front", "Back"]);

        let updated = ok(server.update_note(Parameters(UpdateNoteInput {
            id: note_id,
            expected_modified: Some(note.modified),
            fields: Some(vec![String::from("Edited"), String::from("Back")]),
            tags: Some(vec![String::from("edited")]),
            model_id: None,
            model_name: None,
            deck_id: None,
            deck_name: None,
            confirm: true,
        })));
        assert_eq!(updated.id, note_id);
        assert_eq!(updated.fields, vec!["Edited", "Back"]);
        assert_eq!(updated.tags, vec!["edited"]);

        let stale = server
            .update_note(Parameters(UpdateNoteInput {
                id: note_id,
                expected_modified: Some(note.modified.saturating_sub(1)),
                fields: None,
                tags: None,
                model_id: None,
                model_name: None,
                deck_id: None,
                deck_name: None,
                confirm: true,
            }))
            .err()
            .unwrap();
        let ToolError::Call(error) = stale else {
            panic!("stale edits must be tool errors");
        };
        assert_eq!(error.to_string(), "note changed; read it again");

        let batch = ok(server.get_cards(Parameters(GetCardsInput {
            card_ids: vec![card_id, 999],
        })));
        assert_eq!(batch.cards.len(), 2);
        assert!(batch.cards[0].card.is_some());
        assert!(batch.cards[1].card.is_none());

        let reviewed =
            ok(server.answer_card(Parameters(answer_input(card_id))));
        assert_eq!(reviewed.card.id, card_id);
        assert_eq!(
            serde_json::to_value(reviewed.revlog.ease).unwrap(),
            json!(3)
        );
        assert_eq!(reviewed.revlog.time_ms, 1234);

        let stats = ok(server.stats(Parameters(StatsInput {
            deck_id: Some(deck.id),
            now_secs: Some(1_000_000),
        })));
        assert_eq!(stats.stats.total_cards, 1);
        assert_eq!(stats.stats.new_cards, 0);
    }

    #[test]
    fn duplicate_options_are_exposed() {
        let (_directory, server) = mcp_server();
        assert!(server.add_note(Parameters(add_input("Dup"))).is_ok());
        assert!(server.add_note(Parameters(add_input("Dup"))).is_err());
        let mut allowed = add_input("Dup");
        allowed.allow_duplicate = Some(true);
        assert!(server.add_note(Parameters(allowed)).is_ok());
        let mut scoped = add_input("Scoped");
        scoped.duplicate_scope = Some(super::DuplicateScopeInput::Deck);
        scoped.check_children = Some(true);
        scoped.check_all_models = Some(true);
        assert!(server.add_note(Parameters(scoped)).is_ok());
    }

    #[test]
    fn paginated_results_report_continuation() {
        let (_directory, server) = mcp_server();
        add_card(&server, "Page one");
        add_card(&server, "Page two");
        add_card(&server, "Page three");

        let first = ok(server.find_cards(Parameters(FindCardsInput {
            limit: 2,
            ..find_input("Page")
        })));
        assert_eq!(first.card_ids.len(), 2);
        assert!(first.has_more);
        assert_eq!(first.next_offset, Some(2));

        let second = ok(server.find_cards(Parameters(FindCardsInput {
            offset: 2,
            ..find_input("Page")
        })));
        assert_eq!(second.card_ids.len(), 1);
        assert!(!second.has_more);
        assert_eq!(second.next_offset, None);

        let first = ok(server.due_cards(Parameters(DueCardsInput {
            limit: 2,
            ..due_input()
        })));
        assert_eq!(first.cards.len(), 2);
        assert!(first.has_more);
        assert_eq!(first.next_offset, Some(2));

        let second = ok(server.due_cards(Parameters(DueCardsInput {
            offset: 2,
            ..due_input()
        })));
        assert_eq!(second.cards.len(), 1);
        assert!(!second.has_more);
    }

    #[test]
    fn write_tools_reject_invalid_arguments_without_writing() {
        let (_directory, server) = mcp_server();
        let refused = server
            .add_note(Parameters(AddNoteInput {
                confirm: false,
                ..add_input("Front")
            }))
            .err()
            .unwrap();
        let ToolError::Call(error) = refused else {
            panic!("unconfirmed writes must be tool errors");
        };
        assert_eq!(error.code(), ErrorCode::Message);

        assert!(
            server
                .add_note(Parameters(AddNoteInput {
                    fields: vec![String::new()],
                    ..add_input("Front")
                }))
                .is_err()
        );
        assert!(
            server
                .add_note(Parameters(AddNoteInput {
                    deck_id: None,
                    deck_name: None,
                    ..add_input("Front")
                }))
                .is_err()
        );
        assert!(
            server
                .add_note(Parameters(AddNoteInput {
                    deck_name: Some(String::from("missing")),
                    ..add_input("Front")
                }))
                .is_err()
        );
        assert!(
            server
                .find_cards(Parameters(FindCardsInput {
                    query: String::new(),
                    ..find_input("Front")
                }))
                .is_err()
        );
        assert!(
            server
                .due_cards(Parameters(DueCardsInput {
                    limit: 0,
                    ..due_input()
                }))
                .is_err()
        );
        assert!(
            server
                .due_cards(Parameters(DueCardsInput {
                    offset: 100_001,
                    ..due_input()
                }))
                .is_err()
        );
        assert!(
            server
                .answer_card(Parameters(AnswerCardInput {
                    elapsed_ms: 4_294_967_296,
                    ..answer_input(999)
                }))
                .is_err()
        );
    }

    #[test]
    fn empty_default_deck_and_blank_extra_fields_are_supported() {
        let (_directory, server) = mcp_server();
        ok(server.create_deck(Parameters(CreateDeckInput {
            name: String::from("Study"),
            confirm: true,
        })));
        let mut input = add_input("Default front");
        input.deck_id = None;
        input.deck_name = Some(String::from("default"));
        input.fields[1] = String::new();
        let card_id = ok(server.add_note(Parameters(input))).card_ids[0];

        let due = ok(server.due_cards(Parameters(DueCardsInput {
            deck_id: Some(1),
            ..due_input()
        })));
        assert_eq!(due.cards[0].card.id, card_id);
        let stats = ok(server.stats(Parameters(StatsInput {
            deck_id: Some(1),
            now_secs: Some(1_000_000),
        })));
        assert_eq!(stats.stats.total_cards, 1);

        let page = ok(server.find_cards(Parameters(FindCardsInput {
            offset: 200_000,
            ..find_input("Default")
        })));
        assert_eq!(page.card_ids, Vec::<Id>::new());
        assert!(!page.has_more);
    }

    #[test]
    fn missing_ids_and_decks_are_not_found() {
        let (_directory, server) = mcp_server();
        assert_not_found(
            server
                .get_card(Parameters(GetCardInput { id: 999 }))
                .err()
                .unwrap(),
        );
        assert_not_found(
            server
                .get_note(Parameters(GetNoteInput { id: 999 }))
                .err()
                .unwrap(),
        );
        assert_not_found(
            server
                .update_note(Parameters(UpdateNoteInput {
                    id: 999,
                    expected_modified: None,
                    fields: None,
                    tags: None,
                    model_id: None,
                    model_name: None,
                    deck_id: Some(1),
                    deck_name: None,
                    confirm: true,
                }))
                .err()
                .unwrap(),
        );
        assert_not_found(
            server
                .due_cards(Parameters(DueCardsInput {
                    deck_id: Some(999),
                    ..due_input()
                }))
                .err()
                .unwrap(),
        );
        assert_not_found(
            server
                .stats(Parameters(StatsInput {
                    deck_id: Some(999),
                    now_secs: Some(1_000_000),
                }))
                .err()
                .unwrap(),
        );
    }

    #[test]
    fn package_tools_export_and_import_inside_the_collection_directory() {
        let (directory, server) = mcp_server();
        add_card(&server, "<img src=\"image.png\"> Front");
        let media_dir = directory.path().join("collection.media");
        fs::create_dir_all(&media_dir).unwrap();
        fs::write(media_dir.join("image.png"), b"png").unwrap();
        let export = PackageInput {
            path: String::from("backup.apkg"),
            confirm: true,
        };
        let exported = ok(server.export_apkg(Parameters(PackageInput {
            path: export.path.clone(),
            confirm: true,
        })));
        assert!(exported.bytes > 0);
        assert!(directory.path().join("backup.apkg").is_file());
        assert!(server.export_apkg(Parameters(export)).err().is_some());

        let path = directory.path().join("other.anki2");
        let collection = Collection::create(&path).unwrap();
        let paths = ranki_model::DataPaths {
            collection: path,
            media_dir: directory.path().join("collection.media"),
            sync_state: directory.path().join("ranki-sync.json"),
        };
        let scheduler =
            ranki_core::from_settings(&ranki_model::Settings::default());
        let other = RankiMcp::new(collection, paths, scheduler);
        let imported = ok(other.import_apkg(Parameters(PackageInput {
            path: String::from("backup.apkg"),
            confirm: true,
        })));
        assert_eq!(imported.notes_added, 1);
        assert_eq!(imported.cards_added, 1);
        assert_eq!(imported.media_count, 1);
        let found = ok(other.find_cards(Parameters(find_input("Front"))));
        assert_eq!(found.card_ids.len(), 1);

        let missing = other
            .import_apkg(Parameters(PackageInput {
                path: String::from("missing.apkg"),
                confirm: true,
            }))
            .err()
            .unwrap();
        let ToolError::Call(error) = missing else {
            panic!("missing packages must be tool errors");
        };
        assert!(error.to_string().contains("package not found"));
    }

    #[test]
    fn protocol_errors_and_call_errors_stay_distinct() {
        let protocol = ToolError::from(super::invalid("bad input"));
        assert!(protocol.into_call_tool_result().is_err());

        let call = ToolError::from(crate::CliError::Store(
            ranki_model::StoreError::NotFound(String::from("card")),
        ));
        let response = call.into_call_tool_result().unwrap();
        let super::CallToolResponse::Complete(result) = response else {
            panic!("call errors must be complete tool responses");
        };
        let response = serde_json::to_value(result).unwrap();
        assert_eq!(response["isError"], json!(true));
        assert!(
            response["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("[not_found]")
        );

        let format = ToolError::from(ranki_format::FormatError::new("bad"));
        let ToolError::Call(mapped) = format else {
            panic!("format failures must be tool errors");
        };
        assert_eq!(mapped.code(), ErrorCode::Format);

        let plain = ToolError::call("plain message");
        let response = plain.into_call_tool_result().unwrap();
        let super::CallToolResponse::Complete(result) = response else {
            panic!("plain errors must be complete tool responses");
        };
        let response = serde_json::to_value(result).unwrap();
        assert_eq!(response["content"][0]["text"], json!("plain message"));
    }

    #[test]
    fn collection_path_resolves_existing_files_and_reports_missing_ones() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        fs::write(&path, b"").unwrap();
        let paths = ranki_model::DataPaths {
            collection: path.clone(),
            media_dir: directory.path().join("collection.media"),
            sync_state: directory.path().join("ranki-sync.json"),
        };
        assert_eq!(
            super::collection_path(&paths).unwrap(),
            fs::canonicalize(&path).unwrap()
        );

        let missing = ranki_model::DataPaths {
            collection: directory.path().join("missing.anki2"),
            ..paths
        };
        assert!(super::collection_path(&missing).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn package_paths_work_through_a_symlinked_app_directory() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let collection_path = real.join("collection.anki2");
        let collection = Collection::create(&collection_path).unwrap();
        let link = directory.path().join("link");
        symlink(&real, &link).unwrap();
        let paths = ranki_model::DataPaths {
            collection: link.join("collection.anki2"),
            media_dir: link.join("collection.media"),
            sync_state: link.join("ranki-sync.json"),
        };
        let scheduler =
            ranki_core::from_settings(&ranki_model::Settings::default());
        let server = RankiMcp::new(collection, paths, scheduler);

        assert!(
            server.file("backup.apkg").is_ok(),
            "package path was rejected through the symlinked app directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn package_paths_reject_a_final_component_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        let collection = Collection::create(&paths.collection).unwrap();
        let scheduler =
            ranki_core::from_settings(&ranki_model::Settings::default());
        let server = RankiMcp::new(collection, paths, scheduler);

        let outside = directory.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let target = outside.join("secret.apkg");
        fs::write(&target, b"outside").unwrap();
        symlink(&target, directory.path().join("escape.apkg")).unwrap();

        assert!(server.file("escape.apkg").is_err());
        let ToolError::Call(error) = server
            .export_apkg(Parameters(PackageInput {
                path: String::from("escape.apkg"),
                confirm: true,
            }))
            .err()
            .unwrap()
        else {
            panic!("symlink export must be a tool error");
        };
        assert!(error.to_string().contains("must not be a symlink"));
        assert_eq!(fs::read(&target).unwrap(), b"outside");
    }
}
