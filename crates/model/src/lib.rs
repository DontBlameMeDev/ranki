//! Data model shared by storage, scheduling, import/export and the UI.
//!
//! Field names and numeric values mirror Anki's `collection.anki2` schema so
//! records can be read and written without conversion loss.

pub mod cloze;
pub mod config;
mod id;
pub mod image_occlusion;
pub mod occlusion;
pub mod options;
pub mod raw;
mod review;
pub mod stock;
pub use cloze::cloze_markers;
pub use cloze::cloze_ordinal;
pub use cloze::cloze_ordinals;
pub use config::{
    Config, DataPaths, SchedulerConfig, Settings, StudyConfig, SyncConfig,
    SyncSettings, ThemeChoice, UiConfig, UiState, UiView,
};
pub use id::next_id;
pub use image_occlusion::parse_image_occlusion_field;
pub use options::{
    AnkiConfig, ApplyReviewOptions, DayLimit, DeckCommon, DeckKind,
    DeckOverrides, DeckPreset, EffectiveDeck, LeechAction,
};
pub use raw::decode_hex;
pub use raw::encode_hex;
pub use review::review_streak;
use std::collections::BTreeSet;
pub use stock::StockModelKind;
pub use stock::stock_models;

use serde::{Deserialize, Serialize};

/// Anki identifiers are 64-bit integers.
pub type Id = i64;

/// Separator between note fields in `notes.flds`.
pub const FIELD_SEPARATOR: char = '\u{001f}';
pub const DEFAULT_DECK_ID: Id = 1;
pub const DEFAULT_MODEL_ID: Id = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: Id,
    pub guid: String,
    pub model_id: Id,
    /// Seconds since epoch.
    pub modified: i64,
    pub usn: i32,
    pub tags: Vec<String>,
    pub fields: Vec<String>,
    pub sort_field: String,
    pub checksum: i32,
    pub flags: i32,
    pub data: String,
}

/// One card and the note that owns it.
pub type CardWithNote = (Card, Note);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardType {
    New,
    Learning,
    Review,
    Relearning,
}

impl CardType {
    #[must_use]
    pub const fn code(self) -> i64 {
        match self {
            Self::New => 0,
            Self::Learning => 1,
            Self::Review => 2,
            Self::Relearning => 3,
        }
    }

    /// Unknown values are treated as new, matching Anki's tolerant readers.
    #[must_use]
    pub const fn from_code(code: i64) -> Self {
        match code {
            1 => Self::Learning,
            2 => Self::Review,
            3 => Self::Relearning,
            _ => Self::New,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardQueue {
    New,
    Learning,
    Review,
    DayLearning,
    Suspended,
    Buried,
    ManuallyBuried,
    Preview,
}

impl CardQueue {
    #[must_use]
    pub const fn code(self) -> i64 {
        match self {
            Self::New => 0,
            Self::Learning => 1,
            Self::Review => 2,
            Self::DayLearning => 3,
            Self::Suspended => -1,
            Self::Buried => -2,
            Self::ManuallyBuried => -3,
            Self::Preview => 4,
        }
    }

    /// Unknown values are treated as suspended so they are never shown by accident.
    #[must_use]
    pub const fn from_code(code: i64) -> Self {
        match code {
            0 => Self::New,
            1 => Self::Learning,
            2 => Self::Review,
            3 => Self::DayLearning,
            -2 => Self::Buried,
            -3 => Self::ManuallyBuried,
            4 => Self::Preview,
            _ => Self::Suspended,
        }
    }

    #[must_use]
    pub const fn is_due(self) -> bool {
        matches!(
            self,
            Self::New | Self::Learning | Self::Review | Self::DayLearning
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: Id,
    pub note_id: Id,
    pub deck_id: Id,
    pub ordinal: u16,
    /// Seconds since epoch.
    pub modified: i64,
    pub usn: i32,
    pub card_type: CardType,
    pub queue: CardQueue,
    /// New cards: position. Learning cards: Unix seconds. Review cards: day number.
    pub due: i64,
    pub interval: i32,
    pub factor: u32,
    pub reps: u32,
    pub lapses: u32,
    pub left: u32,
    pub original_due: i64,
    pub original_deck_id: Id,
    pub flags: i32,
    pub data: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}

impl Rating {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Again => 1,
            Self::Hard => 2,
            Self::Good => 3,
            Self::Easy => 4,
        }
    }

    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Again),
            2 => Some(Self::Hard),
            3 => Some(Self::Good),
            4 => Some(Self::Easy),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevLogKind {
    Learn,
    Review,
    Relearn,
    Filtered,
    Manual,
    Rescheduled,
}

impl RevLogKind {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Learn => 0,
            Self::Review => 1,
            Self::Relearn => 2,
            Self::Filtered => 3,
            Self::Manual => 4,
            Self::Rescheduled => 5,
        }
    }

    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Review,
            2 => Self::Relearn,
            3 => Self::Filtered,
            4 => Self::Manual,
            5 => Self::Rescheduled,
            _ => Self::Learn,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevLog {
    pub id: Id,
    pub card_id: Id,
    pub usn: i32,
    pub ease: u8,
    pub interval: i32,
    pub last_interval: i32,
    pub factor: u32,
    /// Milliseconds spent on the review.
    pub time_ms: u32,
    pub kind: RevLogKind,
}

/// Learning and relearning step delays from an Anki deck preset, in seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeckSteps {
    pub learning: Vec<u32>,
    pub relearning: Vec<u32>,
}

impl Default for DeckSteps {
    fn default() -> Self {
        Self {
            learning: vec![60, 600],
            relearning: vec![600],
        }
    }
}

/// Anki's day boundary for one moment in time.
///
/// `creation_offset` and `current_offset` are minutes west of UTC, including
/// daylight saving time. A missing rollover selects the original scheduler's
/// fixed 86400-second days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub now: i64,
    pub days_elapsed: u32,
    pub next_day_at: i64,
}

impl Timing {
    #[expect(
        clippy::too_many_arguments,
        reason = "Anki timing needs both offsets, the rollover and two timestamps"
    )]
    #[must_use]
    pub fn new(
        now: i64,
        collection_created: i64,
        creation_offset: Option<i32>,
        current_offset: i32,
        rollover_hour: Option<u8>,
    ) -> Self {
        let Some(hour) = rollover_hour else {
            return Self::v1(now, collection_created);
        };
        let offset = offset_secs(current_offset);
        let now_local = now.saturating_sub(offset);
        let now_day = now_local.div_euclid(SECONDS_PER_DAY);
        let rollover_secs = i64::from(hour.min(23)).saturating_mul(3_600);
        let today_rollover = now_day
            .saturating_mul(SECONDS_PER_DAY)
            .saturating_add(rollover_secs)
            .saturating_add(offset);
        let next_day_at = if today_rollover <= now {
            today_rollover.saturating_add(SECONDS_PER_DAY)
        } else {
            today_rollover
        };
        let rolled_over = now_local
            .saturating_sub(now_day.saturating_mul(SECONDS_PER_DAY))
            >= rollover_secs;
        let days = creation_offset.map_or_else(
            || {
                let created_day = now_local
                    .saturating_sub(now.saturating_sub(collection_created))
                    .div_euclid(SECONDS_PER_DAY);
                let created_rollover = created_day
                    .saturating_mul(SECONDS_PER_DAY)
                    .saturating_add(rollover_secs)
                    .saturating_add(offset);
                now.saturating_sub(created_rollover)
                    .max(0)
                    .div_euclid(SECONDS_PER_DAY)
            },
            |creation_minutes| {
                let created_local = collection_created
                    .saturating_sub(offset_secs(creation_minutes));
                let created_day = created_local.div_euclid(SECONDS_PER_DAY);
                now_day
                    .saturating_sub(created_day)
                    .saturating_sub(i64::from(!rolled_over))
            },
        );
        Self {
            now,
            days_elapsed: u32::try_from(days.max(0)).unwrap_or(u32::MAX),
            next_day_at,
        }
    }

    /// The original scheduler's fixed-length days.
    #[must_use]
    pub fn v1(now: i64, collection_created: i64) -> Self {
        let days = now
            .saturating_sub(collection_created)
            .max(0)
            .div_euclid(SECONDS_PER_DAY);
        Self {
            now,
            days_elapsed: u32::try_from(days).unwrap_or(u32::MAX),
            next_day_at: collection_created.saturating_add(
                days.saturating_add(1).saturating_mul(SECONDS_PER_DAY),
            ),
        }
    }

    /// Seconds left before the next day starts.
    #[must_use]
    pub fn secs_until_rollover(&self) -> u32 {
        let left = self.next_day_at.saturating_sub(self.now).max(0);
        u32::try_from(left).unwrap_or(u32::MAX)
    }

    /// Whole Anki days between a timestamp and the next rollover.
    #[must_use]
    pub fn elapsed_days_since(&self, timestamp: i64) -> u32 {
        let elapsed = self.next_day_at.saturating_sub(timestamp).max(0);
        u32::try_from(elapsed.div_euclid(SECONDS_PER_DAY)).unwrap_or(u32::MAX)
    }

    /// Due day for a review interval, relative to the collection's day zero.
    #[must_use]
    pub fn due_from_days(&self, interval: i32) -> i64 {
        i64::from(self.days_elapsed)
            .saturating_add(i64::from(interval))
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
    }

    /// True when a queue-1 or queue-3 `due` value is a Unix timestamp.
    #[must_use]
    pub const fn is_unix_due(due: i64) -> bool {
        due > 1_000_000_000
    }
}

const SECONDS_PER_DAY: i64 = 86_400;

fn offset_secs(minutes_west: i32) -> i64 {
    i64::from(minutes_west.clamp(-1_380, 1_380)).saturating_mul(60)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deck {
    pub id: Id,
    pub name: String,
    pub description: String,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum NoteKind {
    #[default]
    Normal,
    Cloze,
}

impl NoteKind {
    /// Anki's `type` value for the note kind.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Cloze => 1,
        }
    }

    /// Unknown values are treated as normal, matching Anki's tolerant readers.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Cloze,
            _ => Self::Normal,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardTemplate {
    pub name: String,
    pub question_format: String,
    pub answer_format: String,
    #[serde(default)]
    pub ord: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteModel {
    pub id: Id,
    pub name: String,
    pub fields: Vec<String>,
    pub templates: Vec<CardTemplate>,
    pub css: String,
    #[serde(default)]
    pub sort_field_index: u16,
    #[serde(default)]
    pub kind: NoteKind,
    #[serde(default)]
    pub original_stock_kind: Option<i32>,
    #[serde(default)]
    pub field_tags: Vec<Option<u32>>,
    /// Ordinals parallel to `templates`; missing `ord` values use the position.
    #[serde(default)]
    pub template_ordinals: Vec<u16>,
    /// Raw schema-18 protobuf config kept for export round trips.
    #[serde(default)]
    pub raw_config: Option<Vec<u8>>,
    /// Full schema-11 model JSON, kept so unknown keys and field or template
    /// settings survive a read/write round trip.
    #[serde(default)]
    pub raw_json: Option<String>,
}

impl NoteModel {
    /// True for cloze note types, including Image Occlusion.
    #[must_use]
    pub const fn is_cloze(&self) -> bool {
        matches!(self.kind, NoteKind::Cloze)
    }

    /// True when Anki's original stock kind identifies Image Occlusion.
    #[must_use]
    pub fn is_image_occlusion(&self) -> bool {
        self.original_stock_kind
            == Some(stock::ORIGINAL_STOCK_KIND_IMAGE_OCCLUSION)
    }

    /// Finds the template for a card ordinal.
    ///
    /// Cloze note types use their first template for every ordinal. Older
    /// models without ordinals fall back to the template position.
    #[must_use]
    pub fn template_for(&self, ordinal: u16) -> Option<&CardTemplate> {
        if self.is_cloze() {
            return self.templates.first();
        }
        self.templates
            .iter()
            .find(|template| template.ord == ordinal)
            .or_else(|| self.templates.get(usize::from(ordinal)))
    }

    /// Ordinals of the cards Anki generates for a note with these fields.
    #[must_use]
    pub fn card_ordinals(&self, fields: &[String]) -> Vec<u16> {
        if self.is_cloze() {
            return cloze_ordinals(fields).into_iter().collect();
        }
        let mut used = BTreeSet::new();
        let mut ordinals = self
            .templates
            .iter()
            .enumerate()
            .map(|(position, template)| {
                let declared = self
                    .template_ordinals
                    .get(position)
                    .copied()
                    .unwrap_or(template.ord);
                unique_ordinal(declared, position, &mut used)
            })
            .collect::<Vec<_>>();
        ordinals.sort_unstable();
        ordinals
    }
}
fn unique_ordinal(
    declared: u16,
    position: usize,
    used: &mut BTreeSet<u16>,
) -> u16 {
    if used.insert(declared) {
        return declared;
    }
    let fallback = u16::try_from(position)
        .unwrap_or(u16::MAX)
        .min(u16::MAX.saturating_sub(1));
    let mut ordinal = fallback;
    while used.contains(&ordinal) {
        ordinal = ordinal.saturating_add(1);
    }
    let _ = used.insert(ordinal);
    ordinal
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewNote {
    pub model_id: Id,
    pub deck_id: Id,
    pub fields: Vec<String>,
    pub tags: Vec<String>,
}

/// Options for the store's first-field duplicate check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AddNoteOptions {
    pub allow_duplicate: bool,
    pub duplicate_scope: DuplicateScope,
}

/// Selects which existing notes take part in a duplicate check.
#[expect(
    clippy::struct_excessive_bools,
    reason = "AnkiConnect exposes three independent duplicate-scope flags"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuplicateScope {
    /// Check only notes with cards in the selected deck.
    pub use_deck: bool,
    /// Deck to check. With `use_deck`, `None` matches no existing note.
    pub deck_id: Option<Id>,
    pub check_children: bool,
    pub check_all_models: bool,
}

impl AddNoteOptions {
    /// Creates options that permit a duplicate on the first field.
    #[must_use]
    pub const fn allowing_duplicates() -> Self {
        Self {
            allow_duplicate: true,
            duplicate_scope: DuplicateScope::new_collection(),
        }
    }
}

impl DuplicateScope {
    /// Creates the default collection-wide scope for the note's model.
    #[must_use]
    pub const fn new_collection() -> Self {
        Self {
            use_deck: false,
            deck_id: None,
            check_children: false,
            check_all_models: false,
        }
    }
}

/// A card after scheduling, ready to be persisted together with its log entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewDecision {
    pub card: Card,
    pub revlog: RevLog,
    /// Card modification stamp the decision was computed from.
    pub expected_modified: i64,
    /// Card sync revision the decision was computed from.
    pub expected_usn: i32,
}

/// Card counts for every deck in a collection, in deck order.
pub type DeckTotals = Vec<(Id, DeckStats)>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeckStats {
    pub new_cards: usize,
    pub learning_cards: usize,
    pub review_cards: usize,
    pub total_cards: usize,
}

/// One card matched by a browse search, with what the table needs to group
/// and order it before the card and note are read.
#[derive(Clone, Debug)]
pub struct BrowseCard {
    pub card_id: Id,
    pub deck_id: Id,
    /// Anki's sort field for the note: the first field with formatting
    /// removed. Stores that cannot read it leave this empty.
    pub sort_field: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("storage failure: {0}")]
    Backend(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

/// Synchronous collection access. Implementations must be atomic per call.
pub trait Store {
    /// Lists the decks shown to the user. An empty default deck with no
    /// subdecks is omitted, as in Anki.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn decks(&self) -> Result<Vec<Deck>, StoreError>;

    /// Creates a deck and returns it.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the name is invalid or the write fails.
    fn create_deck(&mut self, name: &str) -> Result<Deck, StoreError>;

    /// Deletes a deck and every deck below it in the `A::B` hierarchy.
    ///
    /// Cards in those decks are deleted with their review logs. Notes are
    /// deleted when no cards remain, and grave rows are written for every
    /// deleted card, note, and deck. Media files are left in place because
    /// Anki's collection schema has no media ownership table and a file can
    /// be shared by notes outside the deleted subtree. The Default deck
    /// (id 1) cannot be deleted.
    ///
    /// Implementations that cannot delete decks return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the deck is missing, is the Default deck,
    /// or the write fails.
    fn remove_deck(&mut self, id: Id) -> Result<(), StoreError> {
        Err(StoreError::Unsupported(format!(
            "remove_deck is not supported for deck {id}"
        )))
    }

    /// Renames a deck and every subdeck, preserving all ids.
    ///
    /// Names use Anki's `A::B` hierarchy. An empty name, an empty name
    /// segment, or a name that collides with another deck is rejected. The
    /// collection's `col.decks` JSON is updated in the same write.
    ///
    /// Implementations that cannot rename decks return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the deck is missing, the name is invalid,
    /// or the write fails.
    fn rename_deck(&mut self, id: Id, name: &str) -> Result<Deck, StoreError> {
        Err(StoreError::Unsupported(format!(
            "rename_deck is not supported for deck {id} to {name}"
        )))
    }

    /// Lists note models with their templates.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn models(&self) -> Result<Vec<NoteModel>, StoreError>;

    /// Lists distinct tags already used by notes in the collection.
    ///
    /// Implementations without tag storage return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn tags(&self) -> Result<Vec<String>, StoreError> {
        Err(StoreError::Unsupported(
            "tags are not supported by this store".to_owned(),
        ))
    }
    /// Installs a stock note type and returns the stored model.
    ///
    /// Returns the collection's existing model when it already has that stock
    /// type, so the call is safe to repeat.
    ///
    /// Implementations that cannot install models return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the write fails.
    fn ensure_model(
        &mut self,
        kind: StockModelKind,
    ) -> Result<NoteModel, StoreError> {
        Err(StoreError::Unsupported(format!(
            "ensure_model is not supported for {}",
            kind.name()
        )))
    }

    /// Finds card ids matching an Anki-style search query.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the query is invalid or the search fails.
    fn find_cards(&self, query: &str) -> Result<Vec<Id>, StoreError>;

    /// Finds cards matching a search query with the deck and sort field the
    /// browser groups and orders its table by.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the query is invalid or the search fails.
    fn browse_cards(&self, query: &str) -> Result<Vec<BrowseCard>, StoreError>;

    /// Returns cards due in a deck, including subdecks.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    #[expect(
        clippy::too_many_arguments,
        reason = "the store contract fixes these arguments"
    )]
    fn due_cards(
        &self,
        deck_id: Option<Id>,
        limit: usize,
        now_secs: i64,
    ) -> Result<Vec<Card>, StoreError>;

    /// Returns Anki's learn-ahead window in seconds, `collapseTime`.
    ///
    /// Stores without collection config use the default of 1200 seconds.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the collection cannot be read.
    fn learn_ahead_secs(&self) -> Result<i64, StoreError> {
        Ok(1200)
    }

    /// Returns the collection-wide Anki config, including the rollover hour.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn anki_config(&self) -> Result<AnkiConfig, StoreError> {
        Ok(AnkiConfig::default())
    }

    /// Sets the collection-wide Anki day rollover hour, `0..=23`.
    ///
    /// Stores that cannot write collection config return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if `hour` is invalid or the write fails.
    fn set_rollover(&mut self, hour: u8) -> Result<(), StoreError> {
        Err(StoreError::Unsupported(format!(
            "set_rollover is not supported for hour {hour}"
        )))
    }

    /// Returns the learning and relearning steps configured for a deck.
    ///
    /// Implementations without deck presets return
    /// [`StoreError::Unsupported`], and callers fall back to Anki's defaults.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection metadata cannot be read.
    fn deck_steps(&self, _deck_id: Id) -> Result<DeckSteps, StoreError> {
        Err(StoreError::Unsupported(
            "deck steps are not supported by this store".to_owned(),
        ))
    }

    /// Returns the answer-time cap of the preset used by a deck.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection metadata cannot be read.
    fn max_taken_secs(&self, _deck_id: Id) -> Result<u32, StoreError> {
        Ok(60)
    }

    /// Returns the Anki deck presets available in this collection.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection metadata cannot be read.
    fn deck_presets(&self) -> Result<Vec<DeckPreset>, StoreError> {
        Ok(Vec::new())
    }

    /// Writes one Anki deck preset, replacing it when the id already exists.
    ///
    /// Stores that cannot write preset metadata return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the preset is invalid or the write fails.
    fn write_preset(&mut self, preset: &DeckPreset) -> Result<(), StoreError> {
        Err(StoreError::Unsupported(format!(
            "write_preset is not supported for preset {}",
            preset.id
        )))
    }

    /// Returns the time of a card's last review log, in Unix seconds.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn last_review_secs(
        &self,
        _card_id: Id,
    ) -> Result<Option<i64>, StoreError> {
        Ok(None)
    }

    /// Loads one card by id.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn card(&self, id: Id) -> Result<Option<Card>, StoreError>;

    /// Loads one note by id.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn note(&self, id: Id) -> Result<Option<Note>, StoreError>;
    /// Loads cards and their notes in one store call.
    ///
    /// Missing cards or notes are skipped. Stores may override this to avoid
    /// reopening or requerying the collection for every id.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the collection cannot be read.
    #[expect(
        clippy::excessive_nesting,
        reason = "batch loading skips missing cards and notes in one loop"
    )]
    fn cards_with_notes(
        &self,
        ids: &[Id],
    ) -> Result<Vec<CardWithNote>, StoreError> {
        let mut loaded = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(card) = self.card(*id)? else {
                continue;
            };
            let Some(note) = self.note(card.note_id)? else {
                continue;
            };
            loaded.push((card, note));
        }
        Ok(loaded)
    }

    /// Adds a note and its generated cards.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the note is invalid or the write fails.
    fn add_note(&mut self, note: &NewNote) -> Result<Vec<Id>, StoreError>;

    /// Adds a note with explicit duplicate-check options.
    ///
    /// Stores that do not implement scoped checks fall back to [`Self::add_note`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the note is invalid or the write fails.
    fn add_note_with_options(
        &mut self,
        note: &NewNote,
        _options: &AddNoteOptions,
    ) -> Result<Vec<Id>, StoreError> {
        self.add_note(note)
    }

    /// Replaces fields, tags, note type, and deck of an existing note.
    ///
    /// Card ids and review history are preserved. Every card of the note
    /// moves to `deck_id`. Implementations that cannot edit notes return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the note, model, or deck is missing, the
    /// field count does not match the model, or the write fails.
    fn update_note(
        &mut self,
        _note: &Note,
        _deck_id: Id,
    ) -> Result<(), StoreError> {
        Err(StoreError::Unsupported(
            "update_note is not supported by this store".to_owned(),
        ))
    }

    /// Deletes one card, its review log, and its note when no cards remain.
    ///
    /// Implementations that cannot delete cards return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the card is missing or the write fails.
    fn remove_card(&mut self, id: Id) -> Result<(), StoreError> {
        Err(StoreError::Unsupported(format!(
            "remove_card is not supported for card {id}"
        )))
    }
    /// Persists a scheduled review and its log entry atomically.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the card does not exist or the write fails.
    fn apply_review(
        &mut self,
        decision: &ReviewDecision,
    ) -> Result<Id, StoreError>;

    /// Persists a review with leech-marking options.
    ///
    /// Stores that do not handle leech options fall back to [`Self::apply_review`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the card does not exist or the write fails.
    fn apply_review_with_options(
        &mut self,
        decision: &ReviewDecision,
        _options: &ApplyReviewOptions,
    ) -> Result<Id, StoreError> {
        self.apply_review(decision)
    }

    /// Writes card state without creating a review log.
    ///
    /// The card's stored modification time and sync revision must still match
    /// `card`, so a stale edit is rejected.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the card changed or the write fails.
    fn update_card(
        &mut self,
        card: &Card,
        now_secs: i64,
    ) -> Result<(), StoreError> {
        let _ = (card, now_secs);
        Err(StoreError::Unsupported(
            "update_card is not supported by this store".to_owned(),
        ))
    }

    /// Reverses one persisted review: deletes the review log and restores the
    /// card row to the snapshot taken before that review.
    ///
    /// Returns `Ok(false)` when the log is already gone, so a retried undo is
    /// not an error. Implementations that cannot reverse writes return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the card is missing or the write fails.
    fn undo_review(
        &mut self,
        before: &Card,
        _revlog_id: Id,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Unsupported(format!(
            "undo_review is not supported for card {}",
            before.id
        )))
    }

    /// Reverses a review and restores note tags changed by leech marking.
    ///
    /// `before_note` is the note snapshot taken before the review, when leech
    /// marking changed its tags. Stores without note undo fall back to
    /// [`Self::undo_review`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the card or note cannot be restored.
    #[expect(
        clippy::too_many_arguments,
        reason = "undo needs the card, note snapshot, and review log id"
    )]
    fn undo_review_with_note(
        &mut self,
        before: &Card,
        _before_note: Option<&Note>,
        revlog_id: Id,
    ) -> Result<bool, StoreError> {
        self.undo_review(before, revlog_id)
    }

    /// Returns every review log, ordered by card and then by log id.
    ///
    /// Implementations without a review history return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn review_history(&self) -> Result<Vec<RevLog>, StoreError> {
        Err(StoreError::Unsupported(
            "review_history is not supported by this store".to_owned(),
        ))
    }
    /// Counts review logs newer than `cutoff_ms`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the collection cannot be read.
    fn review_count_since(&self, cutoff_ms: i64) -> Result<u32, StoreError> {
        let count = self
            .review_history()?
            .iter()
            .filter(|log| log.id > cutoff_ms)
            .count();
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    /// Counts scheduled cards for each of the next `days` Anki days.
    ///
    /// The first entry is today. It includes overdue cards and learning cards
    /// due before the next day rollover. New cards have no scheduled day and
    /// are excluded.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn due_forecast(
        &self,
        _days: usize,
        _now_secs: i64,
    ) -> Result<Vec<usize>, StoreError> {
        Err(StoreError::Unsupported(
            "due_forecast is not supported by this store".to_owned(),
        ))
    }

    /// Returns card counts for a deck.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn stats(
        &self,
        deck_id: Option<Id>,
        now_secs: i64,
    ) -> Result<DeckStats, StoreError>;

    /// Returns due card counts for every deck at `now_secs`, each including
    /// its subdecks.
    ///
    /// Implementations without a batch query return
    /// [`StoreError::Unsupported`].
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn deck_stats(&self, _now_secs: i64) -> Result<DeckTotals, StoreError> {
        Err(StoreError::Unsupported(
            "deck_stats is not supported by this store".to_owned(),
        ))
    }
    /// Returns the collection modification timestamp in seconds.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    fn collection_modified(&self) -> Result<i64, StoreError>;

    /// Returns the collection creation time in Unix seconds.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the collection cannot be read.
    fn collection_created(&self) -> Result<i64, StoreError>;

    /// Returns Anki's current day boundary information.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection metadata cannot be read.
    fn timing(&self, now_secs: i64) -> Result<Timing, StoreError> {
        Ok(Timing::v1(now_secs, self.collection_created()?))
    }
}
