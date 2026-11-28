//! Store-backed state for the touch-first mobile screens.

#![expect(
    clippy::excessive_nesting,
    clippy::too_many_arguments,
    reason = "store loading and background sync keep their inputs explicit"
)]

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ranki_core::{FsrsScheduler, from_settings};
use ranki_model::{
    Card, CardQueue, Config, DataPaths, Deck, DeckStats, Id, NewNote, Note,
    NoteModel, Rating, RevLog, RevLogKind, Settings, Store, ThemeChoice,
    Timing, UiState, UiView,
};
use ranki_storage::ReopeningCollection;
use ranki_sync::{
    Credentials, DEFAULT_ENDPOINT, HttpTransport, RetryPolicy, SyncClient,
    SyncOutcome, SyncState, SyncTransport, TransportConfig, clear_host_key,
};
use std::result::Result as StdResult;
use std::sync::mpsc;
use std::thread;

use crate::mobile::card::{sides, sides_text};
use crate::mobile::error::Result;
use ranki_core::scheduler_for_card;
use ranki_model::config::parse;

const BROWSE_ROW_LIMIT: usize = 200;
const FORECAST_DAYS: usize = 7;

const MAX_ANSWER_TIME: Duration = Duration::from_secs(60);

/// Full-screen destination shown on a phone.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Decks,
    Review,
    Add,
    Browse,
    Statistics,
    Settings,
}

impl From<UiView> for Screen {
    fn from(view: UiView) -> Self {
        match view {
            UiView::Decks => Self::Decks,
            UiView::Browse => Self::Browse,
            UiView::Review => Self::Review,
            UiView::AddCard => Self::Add,
            UiView::Settings => Self::Settings,
            UiView::Statistics => Self::Statistics,
        }
    }
}

impl From<Screen> for UiView {
    fn from(screen: Screen) -> Self {
        match screen {
            Screen::Decks => Self::Decks,
            Screen::Review => Self::Review,
            Screen::Add => Self::AddCard,
            Screen::Browse => Self::Browse,
            Screen::Statistics => Self::Statistics,
            Screen::Settings => Self::Settings,
        }
    }
}

/// What the review screen is showing for the current card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewPhase {
    #[default]
    Question,
    Answer,
    Waiting,
    Summary,
}

/// One row of the decks list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeckRow {
    pub id: Option<Id>,
    pub name: String,
    pub stats: DeckStats,
    pub due: usize,
}

/// One row of the flat browser list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrowseRow {
    pub card_id: Id,
    pub front: String,
    pub back: String,
    pub deck: String,
    pub meta: String,
}

/// The last persisted grade, kept so undo can restore its answer time.
#[derive(Clone, Debug)]
pub struct LastGrade {
    pub card: Card,
    pub log_id: Id,
    /// Focused time the answer took.
    pub spent: Duration,
}

/// State of one review session.
#[derive(Clone, Debug, Default)]
pub struct ReviewState {
    pub queue: Vec<Card>,
    pub pending: Vec<Card>,
    pub current: Option<Card>,
    pub note: Option<Note>,
    pub model: Option<NoteModel>,
    pub phase: ReviewPhase,
    pub answered: usize,
    /// The last persisted grade, kept for undo.
    pub last: Option<LastGrade>,
    /// Focused time on the current card, used as the answer time.
    pub focused: Duration,
    pub intervals: [i32; 4],
    pub summary: String,
    /// Start of the last focus tick.
    tick: Option<Instant>,
}

/// State of the add-note form.
#[derive(Clone, Debug, Default)]
pub struct AddState {
    pub decks: Vec<Deck>,
    pub deck_index: usize,
    pub models: Vec<NoteModel>,
    pub model_index: usize,
    pub fields: Vec<String>,
    pub tags: String,
    pub preview: String,
    /// Note being edited, or `None` while adding a new note.
    pub editing: Option<Id>,
}

/// Deck create, rename, and remove controls shown on the decks screen.
#[derive(Clone, Debug, Default)]
pub struct DeckDraft {
    pub name: String,
    /// Deck being renamed, or `None` while creating one.
    pub renaming: Option<Id>,
    /// Deck waiting for delete confirmation.
    pub confirm_remove: Option<Id>,
}

/// State of the flat browser screen.
#[derive(Clone, Debug, Default)]
pub struct BrowseState {
    pub query: String,
    /// Text being typed; the query runs when the field is committed.
    pub draft: String,
    pub rows: Vec<BrowseRow>,
    pub total: usize,
    pub page_limit: usize,
}

/// Review totals and the due forecast shown on the statistics screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Statistics {
    pub reviews_today: usize,
    pub millis_today: u64,
    /// `Again`, `Hard`, `Good`, and `Easy` counts for today.
    pub ratings_today: [usize; 4],
    /// Current review streak, using the shared due-day rules.
    pub streak: usize,
    /// Cards due on each of the next seven days, starting today.
    pub forecast: Vec<usize>,
}

impl Statistics {
    /// Percentage of today's answers that were not Again.
    #[must_use]
    pub fn accuracy_percent(&self) -> Option<u8> {
        if self.reviews_today == 0 {
            return None;
        }
        let again = self.ratings_today.first().copied().unwrap_or_default();
        let correct = self.reviews_today.saturating_sub(again);
        let percent = correct
            .saturating_mul(100)
            .checked_div(self.reviews_today)?;
        u8::try_from(percent).ok()
    }
}

/// Config files the mobile app shares with the desktop build.
#[derive(Debug)]
struct Persistence {
    dir: PathBuf,
    config: Config,
}

/// Synchronous `AnkiWeb` state shown in settings.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the contract fixes these independent sync flags"
)]
#[derive(Clone, Debug, Default)]
pub struct SyncUiState {
    pub username: String,
    pub password: String,
    pub signed_in: bool,
    pub busy: bool,
    pub status: String,
    pub conflict: bool,
}
#[derive(Clone, Debug)]
enum SyncKind {
    SignIn { username: String, password: String },
    Sync,
    Conflict { keep_remote: bool },
}

#[derive(Debug)]
struct SyncJob {
    kind: SyncKind,
    results: mpsc::Receiver<StdResult<SyncOutcome, String>>,
}

/// Tone of a short status message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastKind {
    #[default]
    Success,
    Error,
}

/// A short status message shown above the bottom tabs.
#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub created: Instant,
}

impl Default for Toast {
    fn default() -> Self {
        Self {
            kind: ToastKind::default(),
            text: String::new(),
            created: Instant::now(),
        }
    }
}

/// How long a toast stays on screen.
const TOAST_TTL: Duration = Duration::from_secs(4);

/// Everything the mobile UI needs, including the reopening store.
#[derive(Debug)]
pub struct AppState {
    pub store: ReopeningCollection,
    pub settings: Settings,
    pub scheduler: FsrsScheduler,
    pub screen: Screen,
    pub decks: Vec<DeckRow>,
    pub deck_query: String,
    pub selected_deck: Option<Id>,
    pub review_scope: Option<Id>,
    pub review: ReviewState,
    pub add: AddState,
    pub deck_draft: DeckDraft,
    pub browse: BrowseState,
    pub sync: SyncUiState,
    pub statistics: Statistics,
    pub toast: Option<Toast>,
    files: Option<Persistence>,
    sync_job: Option<SyncJob>,
}

impl AppState {
    /// Builds the state from an opened collection and resolved settings.
    ///
    /// File persistence stays off; [`Self::open`] wires the app's config
    /// directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the persisted sync state cannot be read.
    pub fn new(store: ReopeningCollection, settings: Settings) -> Result<Self> {
        Self::build(store, settings, None)
    }

    /// Builds the state from the app's data paths and config directory.
    ///
    /// Reads `config.toml` and `ui-state.toml` from `config_dir`, lets a saved
    /// theme win over the config file as the desktop app does, and keeps
    /// writing both files there.
    ///
    /// # Errors
    ///
    /// Returns an error when the persisted sync state cannot be read.
    pub fn open(
        store: ReopeningCollection,
        paths: DataPaths,
        config_dir: &Path,
    ) -> Result<Self> {
        let config = read_config(config_dir);
        let mut settings = Settings::resolve(&config, paths);
        let ui_state = read_ui_state(config_dir);
        if let Some(theme) = ui_state.theme {
            settings.theme = theme;
        }
        settings.ui_state = ui_state;
        Self::build(
            store,
            settings,
            Some(Persistence {
                dir: config_dir.to_path_buf(),
                config,
            }),
        )
    }

    fn build(
        store: ReopeningCollection,
        settings: Settings,
        files: Option<Persistence>,
    ) -> Result<Self> {
        let scheduler = from_settings(&settings);
        let review_all = settings.ui_state.review_all_decks;
        let deck_query = settings.ui_state.deck_query.clone();
        let selected_deck = settings.ui_state.selected_deck;
        let browse_query = settings.ui_state.browse_query.clone();
        let sync_username = settings.sync.username.clone().unwrap_or_default();
        let screen =
            dev_screen().unwrap_or_else(|| settings.ui_state.view.into());
        let mut state = Self {
            store,
            settings,
            scheduler,
            screen,
            decks: Vec::new(),
            deck_query,
            selected_deck,
            review_scope: if review_all { None } else { selected_deck },
            review: ReviewState::default(),
            add: AddState::default(),
            deck_draft: DeckDraft::default(),
            browse: BrowseState {
                query: browse_query.clone(),
                draft: browse_query,
                rows: Vec::new(),
                total: 0,
                page_limit: BROWSE_ROW_LIMIT,
            },
            sync: SyncUiState {
                username: sync_username,
                ..SyncUiState::default()
            },
            statistics: Statistics::default(),
            toast: None,
            files,
            sync_job: None,
        };
        state.refresh()?;
        if state.screen == Screen::Review {
            state.start_review(state.selected_deck)?;
        }
        apply_dev_hooks(&mut state);
        Ok(state)
    }

    /// Reloads collection-backed lists and the local sign-in state.
    ///
    /// Store failures are reported as toasts so the UI stays usable.
    ///
    /// Local data still loads when sync state cannot be read.
    ///
    /// # Errors
    ///
    /// Always returns `Ok`; the result is kept for existing callers.
    pub fn refresh(&mut self) -> Result<()> {
        if let Err(error) = self.load_sync_state() {
            self.fail(format!("Sync state: {error}"));
        }
        self.load_decks();
        self.load_models();
        self.refresh_browse();
        if self.screen == Screen::Statistics {
            self.load_statistics();
        }
        Ok(())
    }

    /// Navigates to a screen. Entering review starts a fresh session.
    ///
    /// # Errors
    ///
    /// Returns an error when the sync state file cannot be read.
    pub fn set_screen(&mut self, screen: Screen) -> Result<()> {
        if screen == Screen::Review {
            return self.start_review(self.selected_deck);
        }
        self.screen = screen;
        let refreshed = self.refresh();
        self.persist_ui_state();
        refreshed
    }

    /// Replaces the decks filter text.
    pub fn set_deck_query(&mut self, value: String) {
        self.deck_query = value;
        self.persist_ui_state();
    }

    /// Starts a review session for one deck, or all decks when `deck` is none.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn start_review(&mut self, deck: Option<Id>) -> Result<()> {
        self.load_decks();
        let scope = if let Some(id) = deck
            && self.decks.iter().any(|row| row.id == Some(id))
        {
            self.selected_deck = Some(id);
            Some(id)
        } else {
            self.selected_deck = None;
            None
        };
        self.review_scope = scope;
        self.screen = Screen::Review;
        self.review = ReviewState::default();
        self.review.phase = if dev_reveal() {
            ReviewPhase::Answer
        } else {
            ReviewPhase::Question
        };

        let now = now_secs();
        let limit = self.settings.review_batch_size.max(1);
        match self.store.due_cards(scope, limit, now) {
            Ok(cards) => self.review.queue = cards,
            Err(error) => self.fail(format!("Could not load cards: {error}")),
        }
        self.take_next_card();
        if self.review.current.is_none()
            && self.review.phase != ReviewPhase::Summary
        {
            self.review.phase = ReviewPhase::Summary;
            self.review.summary = if self.review.answered == 0 {
                String::from("No cards are due right now.")
            } else {
                format!(
                    "Reviewed {} cards. Session complete.",
                    self.review.answered
                )
            };
        }
        self.persist_ui_state();
        Ok(())
    }

    /// Moves the current card from question to answer.
    pub fn reveal(&mut self) {
        if self.screen != Screen::Review || self.review.current.is_none() {
            return;
        }
        if self.review.phase == ReviewPhase::Question {
            self.review.phase = ReviewPhase::Answer;
        }
    }

    /// Schedules and persists a grade, then advances the session.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn rate(&mut self, rating: Rating) -> Result<()> {
        if self.sync.busy
            || self.screen != Screen::Review
            || self.review.phase != ReviewPhase::Answer
        {
            return Ok(());
        }
        let Some(mut before) = self.review.current.clone() else {
            return Ok(());
        };
        if let Ok(Some(fresh)) = self.store.card(before.id) {
            before = fresh;
        }
        let now = now_secs();
        let timing = self.timing(now);
        let last_review =
            self.store.last_review_secs(before.id).unwrap_or(None);
        let scheduler =
            scheduler_for_card(&self.scheduler, &self.store, &before);
        let answer_ms =
            u32::try_from(self.review.focused.as_millis()).unwrap_or(u32::MAX);
        let Some(decision) = scheduler.schedule_timed(
            &before,
            rating,
            &timing,
            last_review,
            answer_ms,
        ) else {
            self.fail(format!("Card {} is not due", before.id));
            return Ok(());
        };
        let log_id = match self.store.apply_review(&decision) {
            Ok(id) => id,
            Err(error) => {
                self.fail(format!("Could not save review: {error}"));
                return Ok(());
            }
        };

        self.review.answered = self.review.answered.saturating_add(1);
        self.review.last = Some(LastGrade {
            card: before,
            log_id,
            spent: self.review.focused,
        });
        self.review.focused = Duration::ZERO;
        if matches!(
            decision.card.queue,
            CardQueue::Learning | CardQueue::DayLearning
        ) {
            self.review.pending.push(decision.card);
        }
        self.take_next_card();
        if self.review.current.is_some()
            && self.review.phase != ReviewPhase::Summary
        {
            self.review.phase = ReviewPhase::Question;
        }
        self.load_decks();
        if self.review.current.is_none()
            && self.review.phase != ReviewPhase::Summary
        {
            if self.review.pending.is_empty() {
                self.review.phase = ReviewPhase::Summary;
                self.review.summary = format!(
                    "Reviewed {} cards. Session complete.",
                    self.review.answered
                );
            } else {
                self.review.phase = ReviewPhase::Waiting;
                self.review.summary =
                    String::from("Next card is waiting for its learning step.");
            }
        }
        Ok(())
    }

    /// Seconds until the next pending learning card is due, if any.
    #[must_use]
    pub fn pending_delay_secs(&self) -> Option<i64> {
        if self.screen != Screen::Review
            || self.review.phase != ReviewPhase::Waiting
        {
            return None;
        }
        let now = now_secs();
        self.review
            .pending
            .iter()
            .filter(|card| {
                matches!(
                    card.queue,
                    CardQueue::Learning | CardQueue::DayLearning
                )
            })
            .map(|card| card.due.saturating_sub(now).max(0))
            .min()
    }

    /// Advances when a pending learning card becomes due.
    pub fn poll_pending(&mut self) {
        if self.screen != Screen::Review
            || self.review.phase != ReviewPhase::Waiting
        {
            return;
        }
        self.take_next_card();
        if self.review.current.is_some()
            && self.review.note.is_some()
            && self.review.model.is_some()
            && self.review.phase != ReviewPhase::Summary
        {
            self.review.phase = ReviewPhase::Question;
            self.review.summary.clear();
        }
    }

    /// Leaves review and returns to the decks list.
    pub fn end_review(&mut self) {
        self.review.queue.clear();
        self.review.pending.clear();
        self.review.current = None;
        self.review.note = None;
        self.review.model = None;
        self.review.last = None;
        self.review.phase = ReviewPhase::Question;
        self.review.summary.clear();
        self.review.intervals = [0; 4];
        self.review.focused = Duration::ZERO;
        self.review.tick = None;
        self.screen = Screen::Decks;
        self.load_decks();
        self.persist_ui_state();
    }

    /// Reverses the last persisted grade and restores that card.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn undo(&mut self) -> Result<()> {
        if self.sync.busy || self.screen != Screen::Review {
            return Ok(());
        }
        let Some(last) = self.review.last.take() else {
            self.fail(String::from("Nothing to undo"));
            return Ok(());
        };
        let LastGrade {
            card: before,
            log_id,
            spent,
        } = last;
        match self.store.undo_review(&before, log_id) {
            Ok(true) => {}
            Ok(false) => {
                self.fail(String::from("That review is already gone"));
                return Ok(());
            }
            Err(error) => {
                self.review.last = Some(LastGrade {
                    card: before,
                    log_id,
                    spent,
                });
                self.fail(format!("Could not undo review: {error}"));
                return Ok(());
            }
        }

        self.review.answered = self.review.answered.saturating_sub(1);
        self.review.pending.retain(|card| card.id != before.id);
        if let Some(current) = self.review.current.take()
            && current.id != before.id
        {
            self.review.queue.insert(0, current);
        }
        self.review.current = Some(before);
        self.review.focused = spent;
        self.review.phase = if dev_reveal() {
            ReviewPhase::Answer
        } else {
            ReviewPhase::Question
        };
        self.review.summary.clear();
        self.load_current();
        self.load_decks();
        Ok(())
    }

    /// Replaces one note field and refreshes the add preview.
    pub fn set_add_field(&mut self, index: usize, value: String) {
        let updated = self.add.fields.get_mut(index).is_some_and(|slot| {
            *slot = value;
            true
        });
        if updated {
            self.update_preview();
        }
    }

    /// Selects the deck used by new notes.
    pub const fn set_add_deck(&mut self, index: usize) {
        if index < self.add.decks.len() {
            self.add.deck_index = index;
        }
    }

    /// Selects the note type used by new notes.
    pub fn set_add_model(&mut self, index: usize) {
        if self.add.editing.is_some()
            || index >= self.add.models.len()
            || index == self.add.model_index
        {
            return;
        }
        let old_fields = self.add.fields.clone();
        let old_names: Vec<String> = self
            .add
            .models
            .get(self.add.model_index)
            .map_or_else(Vec::new, |model| model.fields.clone());
        let Some(new_names) =
            self.add.models.get(index).map(|model| model.fields.clone())
        else {
            return;
        };
        self.add.fields = new_names
            .iter()
            .map(|name| {
                old_names
                    .iter()
                    .position(|old| old.eq_ignore_ascii_case(name))
                    .and_then(|position| old_fields.get(position))
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        self.add.model_index = index;
        self.update_preview();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "creating and editing a note share one draft path"
    )]
    /// Saves the draft as a new note, or updates the note being edited.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn save_note(&mut self) -> Result<()> {
        if self.sync.busy {
            return Ok(());
        }
        let Some(deck) = self.add.decks.get(self.add.deck_index).cloned()
        else {
            self.fail(String::from("Choose a deck first"));
            return Ok(());
        };
        let Some(model) = self.add.models.get(self.add.model_index).cloned()
        else {
            self.fail(String::from("Choose a note type first"));
            return Ok(());
        };
        if model.is_image_occlusion() {
            self.fail("Image occlusion notes cannot be created here");
            return Ok(());
        }
        if self.add.fields.len() != model.fields.len() {
            self.add.fields.resize(model.fields.len(), String::new());
        }
        let mut tags = Vec::new();
        for tag in self.add.tags.split_whitespace() {
            if !tags.iter().any(|known| known == tag) {
                tags.push(tag.to_owned());
            }
        }
        if model.is_cloze() && model.card_ordinals(&self.add.fields).is_empty()
        {
            self.fail(String::from(
                "Cloze notes need a {{c1::...}} deletion before saving",
            ));
            return Ok(());
        }
        if self.add.fields.iter().all(|value| value.trim().is_empty()) {
            self.fail(String::from("Enter text in at least one field"));
            return Ok(());
        }
        let fields = self.add.fields.clone();
        if let Some(note_id) = self.add.editing {
            let mut note = match self.store.note(note_id) {
                Ok(Some(note)) => note,
                Ok(None) => {
                    self.fail(String::from("That note no longer exists"));
                    return Ok(());
                }
                Err(error) => {
                    self.fail(format!("Could not load note: {error}"));
                    return Ok(());
                }
            };
            note.model_id = model.id;
            note.fields = fields;
            note.tags = tags;
            match self.store.update_note(&note, deck.id) {
                Ok(()) => {
                    self.add.editing = None;
                    self.reset_draft();
                    self.set_screen(Screen::Browse)?;
                    self.say("Note updated");
                }
                Err(error) => {
                    self.fail(format!("Could not update note: {error}"));
                }
            }
            return Ok(());
        }
        let note = NewNote {
            model_id: model.id,
            deck_id: deck.id,
            fields,
            tags,
        };
        match self.store.add_note(&note) {
            Ok(ids) => {
                let count = ids.len();
                self.reset_draft();
                self.refresh()?;
                self.say(format!("Added {count} cards"));
            }
            Err(error) => self.fail(format!("Could not add note: {error}")),
        }
        Ok(())
    }

    /// Opens the note behind a browser row in the add form.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn start_edit(&mut self, card_id: Id) -> Result<()> {
        if self.add.editing.is_some() {
            self.fail("Finish or cancel the current edit first");
            return Ok(());
        }
        let card = match self.store.card(card_id) {
            Ok(Some(card)) => card,
            Ok(None) => {
                self.fail(String::from("That card no longer exists"));
                return Ok(());
            }
            Err(error) => {
                self.fail(format!("Could not load card: {error}"));
                return Ok(());
            }
        };
        let note = match self.store.note(card.note_id) {
            Ok(Some(note)) => note,
            Ok(None) => {
                self.fail(String::from("That note no longer exists"));
                return Ok(());
            }
            Err(error) => {
                self.fail(format!("Could not load note: {error}"));
                return Ok(());
            }
        };
        let Some(model_index) = self
            .add
            .models
            .iter()
            .position(|model| model.id == note.model_id)
        else {
            self.fail(String::from("This note type is not in the collection"));
            return Ok(());
        };
        if self
            .add
            .models
            .get(model_index)
            .is_some_and(NoteModel::is_image_occlusion)
        {
            self.fail(String::from(
                "Image occlusion notes cannot be edited here",
            ));
            return Ok(());
        }
        let Some(deck_index) = self
            .add
            .decks
            .iter()
            .position(|deck| deck.id == card.deck_id)
        else {
            self.fail(String::from("That deck is not in this collection"));
            return Ok(());
        };
        self.add.editing = Some(note.id);
        self.add.model_index = model_index;
        self.add.deck_index = deck_index;
        self.add.fields.clone_from(&note.fields);
        self.add.tags = note.tags.join(" ");
        self.update_preview();
        self.screen = Screen::Add;
        self.persist_ui_state();
        Ok(())
    }

    /// Drops the edit draft and returns to the browser.
    pub fn cancel_edit(&mut self) {
        if self.add.editing.take().is_none() {
            return;
        }
        self.reset_draft();
        self.screen = Screen::Browse;
        self.persist_ui_state();
    }

    /// Saves the deck name draft, creating a deck or renaming one.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn save_deck(&mut self) -> Result<()> {
        let name = self.deck_draft.name.trim().to_owned();
        if name.is_empty() {
            self.fail(String::from("Deck name cannot be empty"));
            return Ok(());
        }
        match self.deck_draft.renaming {
            Some(id) => match self.store.rename_deck(id, &name) {
                Ok(_) => {
                    self.deck_draft.name.clear();
                    self.deck_draft.renaming = None;
                    self.refresh()?;
                    self.say("Deck renamed");
                }
                Err(error) => {
                    self.fail(format!("Could not rename deck: {error}"));
                }
            },
            None => match self.store.create_deck(&name) {
                Ok(_) => {
                    self.deck_draft.name.clear();
                    self.refresh()?;
                    self.say("Deck created");
                }
                Err(error) => {
                    self.fail(format!("Could not create deck: {error}"));
                }
            },
        }
        Ok(())
    }

    /// Fills the deck name field with an existing name for renaming.
    pub fn start_deck_rename(&mut self, id: Id) {
        let Some(row) = self.decks.iter().find(|row| row.id == Some(id)) else {
            return;
        };
        self.deck_draft.name.clone_from(&row.name);
        self.deck_draft.renaming = Some(id);
        self.deck_draft.confirm_remove = None;
    }

    /// Clears the deck create and rename draft.
    pub fn cancel_deck_draft(&mut self) {
        self.deck_draft.name.clear();
        self.deck_draft.renaming = None;
    }

    /// Asks for confirmation before removing a deck.
    pub const fn request_remove_deck(&mut self, id: Id) {
        self.deck_draft.confirm_remove = Some(id);
    }

    /// Removes the confirmed deck with its cards and notes.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn confirm_remove_deck(&mut self) -> Result<()> {
        let Some(id) = self.deck_draft.confirm_remove.take() else {
            return Ok(());
        };
        match self.store.remove_deck(id) {
            Ok(()) => {
                if self.review_scope == Some(id) {
                    self.review_scope = None;
                }
                self.deck_draft.name.clear();
                self.deck_draft.renaming = None;
                self.refresh()?;
                self.say("Deck removed");
            }
            Err(error) => {
                self.deck_draft.confirm_remove = Some(id);
                self.fail(format!("Could not remove deck: {error}"));
            }
        }
        Ok(())
    }

    /// Dismisses the deck removal confirmation.
    pub const fn cancel_remove_deck(&mut self) {
        self.deck_draft.confirm_remove = None;
    }

    fn reset_draft(&mut self) {
        self.add.fields.iter_mut().for_each(String::clear);
        self.add.tags.clear();
        self.update_preview();
    }

    /// Runs a store search and rebuilds the flat browser list.
    ///
    /// # Errors
    ///
    /// Store failures are reported as toasts; this call still succeeds.
    pub fn set_browse_query(&mut self, value: String) -> Result<()> {
        self.browse.query.clone_from(&value);
        self.browse.draft = value;
        self.refresh_browse();
        self.persist_ui_state();
        Ok(())
    }

    /// Signs in to `AnkiWeb` and starts one background sync.
    ///
    /// # Errors
    ///
    /// Network and state failures are reported as toasts; this call succeeds.
    pub fn sign_in(&mut self) -> Result<()> {
        if self.sync.busy {
            return Ok(());
        }
        let username = self.sync.username.trim().to_owned();
        let password = self.sync.password.clone();
        if username.is_empty() || password.is_empty() {
            self.fail(String::from("Enter your AnkiWeb username and password"));
            return Ok(());
        }

        self.sync.conflict = false;
        self.sync.busy = true;
        self.sync.status = String::from("Signing in to AnkiWeb");
        self.start_sync_job(SyncKind::SignIn { username, password });
        Ok(())
    }

    /// Starts one background automatic sync.
    /// # Errors
    ///
    /// Never propagates failures; they are reported as toasts.
    pub fn sync_now(&mut self) -> Result<()> {
        if self.sync.busy {
            return Ok(());
        }
        if !self.sync.signed_in {
            self.fail(String::from("Sign in before syncing"));
            return Ok(());
        }
        self.sync.busy = true;
        self.sync.status = String::from("Syncing with AnkiWeb");
        self.start_sync_job(SyncKind::Sync);
        Ok(())
    }

    /// Removes the stored host key.
    ///
    /// # Errors
    ///
    /// State write failures are reported as toasts; this call succeeds.
    pub fn sign_out(&mut self) -> Result<()> {
        if self.sync.busy {
            return Ok(());
        }
        if let Err(error) = clear_host_key(&self.settings.paths.sync_state) {
            self.fail(format!("Could not sign out: {error}"));
            return Ok(());
        }
        self.sync.signed_in = false;
        self.sync.password.clear();
        self.sync.conflict = false;
        self.sync.status = String::from("Signed out of AnkiWeb");
        self.settings.sync.enabled = false;
        self.settings.sync.username = None;
        self.persist_sync();
        self.say("Signed out of AnkiWeb");
        Ok(())
    }

    /// Resolves a sync conflict with an explicit one-way choice.
    ///
    /// # Errors
    ///
    /// Network failures are reported as toasts. This call succeeds.
    pub fn resolve_conflict(&mut self, keep_remote: bool) -> Result<()> {
        if self.sync.busy || !self.sync.conflict {
            return Ok(());
        }
        self.sync.busy = true;
        self.sync.status = if keep_remote {
            String::from("Downloading the remote collection")
        } else {
            String::from("Uploading the local collection")
        };
        self.start_sync_job(SyncKind::Conflict { keep_remote });
        Ok(())
    }
    pub(crate) fn poll_sync(&mut self) -> Result<()> {
        let Some(job) = self.sync_job.take() else {
            return Ok(());
        };
        let result = match job.results.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                self.sync_job = Some(job);
                return Ok(());
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(String::from("sync worker stopped"))
            }
        };
        match (&job.kind, result) {
            (SyncKind::SignIn { username, .. }, Ok(outcome)) => {
                self.sync.signed_in = true;
                self.settings.sync.enabled = true;
                self.settings.sync.username = Some(username.clone());
                self.sync.password.clear();
                self.persist_sync();
                self.finish_sync(&outcome);
                self.load_sync_state()?;
            }
            (SyncKind::SignIn { .. }, Err(error)) => {
                self.sync.busy = false;
                let message = format!("AnkiWeb sign-in failed: {error}");
                self.sync.status.clone_from(&message);
                self.fail(message);
                self.load_sync_state()?;
            }
            (SyncKind::Sync | SyncKind::Conflict { .. }, Ok(outcome)) => {
                self.finish_sync(&outcome);
                self.refresh()?;
            }
            (SyncKind::Sync | SyncKind::Conflict { .. }, Err(error)) => {
                self.sync.busy = false;
                let message = if matches!(&job.kind, SyncKind::Conflict { .. })
                {
                    format!("Could not resolve sync conflict: {error}")
                } else {
                    format!("Sync failed: {error}")
                };
                self.sync.status.clone_from(&message);
                self.fail(message);
            }
        }
        Ok(())
    }

    fn start_sync_job(&mut self, kind: SyncKind) {
        let paths = self.settings.paths.clone();
        let retries = self.settings.sync.retries;
        let backoff_ms = self.settings.sync.backoff_ms;
        let (sender, results) = mpsc::channel();
        let worker_kind = kind.clone();
        thread::spawn(move || {
            let result = match worker_kind {
                SyncKind::SignIn { username, password } => {
                    Self::login_and_sync(
                        &paths.collection,
                        &paths.media_dir,
                        &paths.sync_state,
                        retries,
                        backoff_ms,
                        &username,
                        &password,
                    )
                }
                SyncKind::Sync => Self::run_sync(
                    &paths.collection,
                    &paths.media_dir,
                    &paths.sync_state,
                    retries,
                    backoff_ms,
                ),
                SyncKind::Conflict { keep_remote } => Self::run_conflict(
                    &paths.collection,
                    &paths.media_dir,
                    &paths.sync_state,
                    retries,
                    backoff_ms,
                    keep_remote,
                ),
            };
            let _ = sender.send(result.map_err(|error| error.to_string()));
        });
        self.sync_job = Some(SyncJob { kind, results });
    }

    /// Expires the current toast and returns the remaining display time.
    pub fn poll_toast(&mut self, now: Instant) -> Option<Duration> {
        let toast = self.toast.as_ref()?;
        let remaining = toast
            .created
            .checked_add(TOAST_TTL)
            .and_then(|deadline| deadline.checked_duration_since(now));
        if remaining.is_some_and(|wait| !wait.is_zero()) {
            return remaining;
        }
        self.toast = None;
        None
    }

    /// Shows a success toast.
    pub fn say(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            kind: ToastKind::Success,
            text: text.into(),
            created: Instant::now(),
        });
    }

    /// Selects the theme and saves it with the rest of the UI state.
    pub fn set_theme(&mut self, theme: ThemeChoice) {
        if self.settings.theme == theme {
            return;
        }
        self.settings.theme = theme;
        self.persist_ui_state();
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            kind: ToastKind::Error,
            text: text.into(),
            created: Instant::now(),
        });
    }

    fn timing(&self, now: i64) -> Timing {
        self.store.timing(now).unwrap_or_else(|_| {
            let created = self.store.collection_created().unwrap_or_default();
            Timing::v1(now, created)
        })
    }

    /// Credits focused time on the current card to its answer time.
    pub fn tick_focus(&mut self, focused: bool) {
        let now = Instant::now();
        let Some(last) = self.review.tick else {
            self.review.tick = Some(now);
            return;
        };
        let elapsed = now.saturating_duration_since(last);
        self.review.tick = Some(now);
        self.review.focused =
            focused_delta(elapsed, focused, self.review.focused);
    }

    fn ui_state(&self) -> UiState {
        let mut state = self.settings.ui_state.clone();
        state.view = self.screen.into();
        state.theme = Some(self.settings.theme);
        state.selected_deck = self.selected_deck;
        state.deck_query.clone_from(&self.deck_query);
        state.browse_query.clone_from(&self.browse.query);
        state.review_all_decks = self.review_scope.is_none();
        state
    }

    fn persist_ui_state(&mut self) {
        let Some(dir) = self.files.as_ref().map(|files| files.dir.clone())
        else {
            return;
        };
        let state = self.ui_state();
        if state == self.settings.ui_state {
            return;
        }
        let result = write_ui_state(&dir, &state);
        self.settings.ui_state = state;
        if let Err(error) = result {
            self.fail(format!("Could not save UI state: {error}"));
        }
    }

    fn persist_sync(&mut self) {
        let Some(files) = self.files.as_mut() else {
            return;
        };
        files.config.sync.enabled = self.settings.sync.enabled;
        files
            .config
            .sync
            .username
            .clone_from(&self.settings.sync.username);
        let result = write_config(&files.dir, &files.config);
        if let Err(error) = result {
            self.fail(format!("Could not save sync settings: {error}"));
        }
    }

    fn load_statistics(&mut self) {
        let now = now_secs();
        let timing = self.timing(now);
        match self.store.review_history() {
            Ok(history) => self.statistics = summarize(&history, &timing),
            Err(error) => {
                self.statistics = summarize(&[], &timing);
                self.fail(format!("Could not load review history: {error}"));
            }
        }
        self.statistics.forecast = self
            .store
            .due_forecast(FORECAST_DAYS, now)
            .unwrap_or_default();
    }

    fn load_sync_state(&mut self) -> Result<()> {
        let state = SyncState::load(&self.settings.paths.sync_state)?;
        self.sync.signed_in = !state.host_key.is_empty();
        Ok(())
    }

    fn load_decks(&mut self) {
        let now = now_secs();
        let decks = match self.store.decks() {
            Ok(decks) => decks,
            Err(error) => {
                self.fail(format!("Could not load decks: {error}"));
                return;
            }
        };
        let totals = self.store.deck_stats(now).map_or_else(
            |_| {
                decks
                    .iter()
                    .map(|deck| {
                        let stats = self
                            .store
                            .stats(Some(deck.id), now)
                            .unwrap_or_default();
                        (deck.id, stats)
                    })
                    .collect::<HashMap<_, _>>()
            },
            |totals| totals.into_iter().collect::<HashMap<_, _>>(),
        );
        let selected = self
            .selected_deck
            .filter(|id| decks.iter().any(|deck| deck.id == *id))
            .or_else(|| decks.first().map(|deck| deck.id));
        let add_deck_id = self
            .add
            .decks
            .get(self.add.deck_index)
            .map(|deck| deck.id)
            .or(selected);
        self.decks = decks
            .iter()
            .map(|deck| {
                let stats = totals.get(&deck.id).copied().unwrap_or_default();
                DeckRow {
                    id: Some(deck.id),
                    name: deck.name.clone(),
                    stats,
                    due: stats
                        .new_cards
                        .saturating_add(stats.learning_cards)
                        .saturating_add(stats.review_cards),
                }
            })
            .collect();
        self.selected_deck = selected;
        self.add.decks = decks;
        self.add.deck_index = add_deck_id
            .and_then(|id| self.add.decks.iter().position(|deck| deck.id == id))
            .unwrap_or(0);
    }

    fn load_models(&mut self) {
        let models = match self.store.models() {
            Ok(models) => models,
            Err(error) => {
                self.fail(format!("Could not load note types: {error}"));
                return;
            }
        };
        let current_id = self
            .add
            .models
            .get(self.add.model_index)
            .map(|model| model.id);
        self.add.models = models;
        self.add.model_index = current_id
            .and_then(|id| {
                self.add.models.iter().position(|model| model.id == id)
            })
            .unwrap_or(0);
        let len = self
            .add
            .models
            .get(self.add.model_index)
            .map_or(0, |model| model.fields.len());
        if self.add.fields.len() != len {
            self.add.fields.resize(len, String::new());
        }
        self.update_preview();
    }

    /// Shows another page of browse results.
    pub fn load_more(&mut self) {
        self.browse.page_limit =
            self.browse.page_limit.saturating_add(BROWSE_ROW_LIMIT);
        self.refresh_browse();
    }

    fn refresh_browse(&mut self) {
        let ids = match self.store.find_cards(&self.browse.query) {
            Ok(ids) => ids,
            Err(error) => {
                self.fail(format!("Could not search cards: {error}"));
                self.browse.rows.clear();
                self.browse.total = 0;
                return;
            }
        };
        self.browse.total = ids.len();
        let decks = self.decks.clone();
        let models = self.add.models.clone();
        let limit = self.browse.page_limit.max(1);
        let mut rows = Vec::with_capacity(ids.len().min(limit));
        let visible = ids.len().min(limit);
        let visible_ids = ids.get(..visible).unwrap_or(&ids);
        let loaded = match self.store.cards_with_notes(visible_ids) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.fail(format!("Could not load cards: {error}"));
                return;
            }
        };
        for (card, note) in loaded {
            let Some(model) =
                models.iter().find(|model| model.id == note.model_id)
            else {
                continue;
            };
            let (question, answer) = sides_text(model, &note, card.ordinal);
            let repeats_front =
                model.template_for(card.ordinal).is_some_and(|template| {
                    template.answer_format.contains("{{FrontSide}}")
                });
            let back = browse_answer(&question, &answer, repeats_front);
            let deck = decks
                .iter()
                .find(|row| row.id == Some(card.deck_id))
                .map_or_else(
                    || String::from("Unknown deck"),
                    |row| row.name.clone(),
                );
            rows.push(BrowseRow {
                card_id: card.id,
                front: question,
                back,
                deck,
                meta: format!("{} / {}", model.name, queue_name(card.queue)),
            });
        }
        self.browse.rows = rows;
    }

    pub(crate) fn take_next_card(&mut self) {
        self.review.current = None;
        self.review.note = None;
        self.review.model = None;
        self.review.intervals = [0; 4];
        let now = now_secs();
        let due = self.review.pending.iter().position(|card| {
            matches!(card.queue, CardQueue::Learning | CardQueue::DayLearning)
                && card.due <= now
        });
        let next = if let Some(index) = due {
            Some(self.review.pending.remove(index))
        } else if self.review.queue.is_empty() {
            None
        } else {
            Some(self.review.queue.remove(0))
        };
        if let Some(card) = next {
            self.review.current = Some(card);
            self.review.summary.clear();
            self.load_current();
        }
    }

    fn load_current(&mut self) {
        let Some(card) = self.review.current.clone() else {
            return;
        };
        let note = match self.store.note(card.note_id) {
            Ok(note) => note,
            Err(error) => {
                self.fail(format!("Could not load note: {error}"));
                None
            }
        };
        let model = note.as_ref().and_then(|loaded| {
            self.store.models().ok().and_then(|models| {
                models.into_iter().find(|model| model.id == loaded.model_id)
            })
        });
        if note.is_none() || model.is_none() {
            self.review.summary = String::from("A card could not be loaded.");
            self.review.phase = ReviewPhase::Summary;
        }
        let now = now_secs();
        let timing = self.timing(now);
        let last_review = self.store.last_review_secs(card.id).unwrap_or(None);
        let scheduler = scheduler_for_card(&self.scheduler, &self.store, &card);
        self.review.intervals =
            scheduler.preview_all_timed(&card, &timing, last_review);
        self.review.note = note;
        self.review.model = model;
    }

    fn update_preview(&mut self) {
        let Some(model) = self.add.models.get(self.add.model_index).cloned()
        else {
            self.add.preview.clear();
            return;
        };
        let note = Note {
            id: 0,
            guid: String::new(),
            model_id: model.id,
            modified: 0,
            usn: 0,
            tags: Vec::new(),
            fields: self.add.fields.clone(),
            sort_field: self.add.fields.first().cloned().unwrap_or_default(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        let (question, _) = sides(&model, &note, 0);
        self.add.preview = question;
    }

    fn sync_client(
        collection: &Path,
        media_dir: &Path,
        state_path: &Path,
        retries: u8,
        backoff_ms: u64,
    ) -> Result<SyncClient<HttpTransport>> {
        let transport = HttpTransport::with_config(
            DEFAULT_ENDPOINT,
            TransportConfig {
                retry_policy: RetryPolicy {
                    retries,
                    backoff: Duration::from_millis(backoff_ms),
                },
                ..TransportConfig::default()
            },
        );
        Ok(SyncClient::new(
            transport, collection, media_dir, state_path,
        )?)
    }

    fn login_and_sync(
        collection: &Path,
        media_dir: &Path,
        state_path: &Path,
        retries: u8,
        backoff_ms: u64,
        username: &str,
        password: &str,
    ) -> Result<SyncOutcome> {
        let mut client = Self::sync_client(
            collection, media_dir, state_path, retries, backoff_ms,
        )?;
        client.login(&Credentials::new(username, password))?;
        let outcome = client.sync(None)?;
        Self::refresh_media(&mut client, &outcome)?;
        Ok(outcome)
    }

    /// Fetches media after a sync that left both copies in step.
    fn refresh_media<T: SyncTransport>(
        client: &mut SyncClient<T>,
        outcome: &SyncOutcome,
    ) -> Result<()> {
        if outcome.is_settled() {
            client.sync_media()?;
        }
        Ok(())
    }

    fn run_sync(
        collection: &Path,
        media_dir: &Path,
        state_path: &Path,
        retries: u8,
        backoff_ms: u64,
    ) -> Result<SyncOutcome> {
        let mut client = Self::sync_client(
            collection, media_dir, state_path, retries, backoff_ms,
        )?;
        let outcome = client.sync(None)?;
        Self::refresh_media(&mut client, &outcome)?;
        Ok(outcome)
    }

    fn run_conflict(
        collection: &Path,
        media_dir: &Path,
        state_path: &Path,
        retries: u8,
        backoff_ms: u64,
        keep_remote: bool,
    ) -> Result<SyncOutcome> {
        let mut client = Self::sync_client(
            collection, media_dir, state_path, retries, backoff_ms,
        )?;
        if keep_remote {
            client.full_download()?;
            client.sync_media()?;
            Ok(SyncOutcome::Downloaded)
        } else {
            client.full_upload()?;
            client.sync_media()?;
            Ok(SyncOutcome::Uploaded)
        }
    }

    fn finish_sync(&mut self, outcome: &SyncOutcome) {
        self.sync.busy = false;
        self.sync.conflict = false;
        let (status, error, conflict) = match outcome {
            SyncOutcome::Disabled => {
                (String::from("Sync did not start"), true, false)
            }
            SyncOutcome::NoChanges => {
                (String::from("Already up to date"), false, false)
            }
            SyncOutcome::Uploaded => {
                (String::from("Collection uploaded"), false, false)
            }
            SyncOutcome::Downloaded => {
                (String::from("Collection downloaded"), false, false)
            }
            SyncOutcome::Conflict { reason } => {
                (format!("Sync conflict: {reason}"), true, true)
            }
            SyncOutcome::RetryLater { reason } => {
                (format!("Sync failed: {reason}"), true, false)
            }
        };
        self.sync.status = status;
        self.sync.conflict = conflict;
        if error {
            self.fail(self.sync.status.clone());
        } else {
            self.say(self.sync.status.clone());
        }
    }
}

/// Answer time credited per tick, capped so idle screens cannot skew it.
pub(crate) fn focused_delta(
    elapsed: Duration,
    focused: bool,
    total: Duration,
) -> Duration {
    if !focused {
        return total;
    }
    total.saturating_add(elapsed).min(MAX_ANSWER_TIME)
}

/// Opens click-only screens for headless visual checks.
fn apply_dev_hooks(state: &mut AppState) {
    if env::var("RANKI_MOBILE_DEV_EDIT").is_ok_and(|value| value == "1")
        && let Some(card_id) = state.browse.rows.first().map(|row| row.card_id)
    {
        let _ = state.start_edit(card_id);
    }
    if env::var("RANKI_MOBILE_DEV_CONFIRM_DECK").is_ok_and(|value| value == "1")
        && let Some(id) = state.decks.first().and_then(|row| row.id)
    {
        state.request_remove_deck(id);
    }
}

fn dev_screen() -> Option<Screen> {
    match env::var("RANKI_MOBILE_DEV_VIEW")
        .unwrap_or_default()
        .as_str()
    {
        "review" => Some(Screen::Review),
        "add" => Some(Screen::Add),
        "browse" => Some(Screen::Browse),
        "statistics" => Some(Screen::Statistics),
        "settings" => Some(Screen::Settings),
        "decks" => Some(Screen::Decks),
        _ => None,
    }
}

fn dev_reveal() -> bool {
    env::var("RANKI_MOBILE_DEV_REVEAL").is_ok_and(|value| value == "1")
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

pub(crate) fn summarize(history: &[RevLog], timing: &Timing) -> Statistics {
    let today = i64::from(timing.days_elapsed);
    let day_of = |seconds| {
        today.saturating_sub(i64::from(timing.elapsed_days_since(seconds)))
    };
    let mut stats = Statistics::default();
    for log in history {
        if matches!(log.kind, RevLogKind::Manual | RevLogKind::Rescheduled) {
            continue;
        }
        let day = day_of(log.id.checked_div(1_000).unwrap_or_default());
        if day > today {
            continue;
        }
        if day != today {
            continue;
        }
        stats.reviews_today = stats.reviews_today.saturating_add(1);
        stats.millis_today =
            stats.millis_today.saturating_add(u64::from(log.time_ms));
        let rating = usize::from(log.ease.saturating_sub(1).min(3));
        if let Some(slot) = stats.ratings_today.get_mut(rating) {
            *slot = slot.saturating_add(1);
        }
    }
    stats.streak = ranki_model::review_streak(history, day_of, today);
    stats
}

fn read_config(dir: &Path) -> Config {
    let path = dir.join("config.toml");
    match fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == ErrorKind::NotFound => Config::default(),
        Err(error) => {
            eprintln!(
                "ranki mobile: config: cannot read {}: {error}",
                path.display()
            );
            Config::default()
        }
    }
}

fn write_config(dir: &Path, config: &Config) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let text = toml::to_string(config).map_err(io::Error::other)?;
    fs::write(dir.join("config.toml"), text)
}

fn read_ui_state(dir: &Path) -> UiState {
    let path = dir.join("ui-state.toml");
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
            eprintln!(
                "ranki mobile: ui state: cannot read {}: {error}",
                path.display()
            );
            UiState::default()
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => UiState::default(),
        Err(error) => {
            eprintln!(
                "ranki mobile: ui state: cannot read {}: {error}",
                path.display()
            );
            UiState::default()
        }
    }
}

fn write_ui_state(dir: &Path, state: &UiState) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let text = toml::to_string(state).map_err(io::Error::other)?;
    fs::write(dir.join("ui-state.toml"), text)
}

pub(crate) fn browse_answer(
    question: &str,
    answer: &str,
    repeats_front: bool,
) -> String {
    if repeats_front {
        answer
            .strip_prefix(question)
            .unwrap_or(answer)
            .trim_start()
            .to_owned()
    } else {
        answer.to_owned()
    }
}

const fn queue_name(queue: CardQueue) -> &'static str {
    match queue {
        CardQueue::New => "New",
        CardQueue::Learning | CardQueue::DayLearning => "Learning",
        CardQueue::Review => "Review",
        CardQueue::Suspended => "Suspended",
        CardQueue::Buried | CardQueue::ManuallyBuried => "Buried",
        CardQueue::Preview => "Preview",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ranki_model::DataPaths;
    use ranki_storage::Collection;

    #[test]
    fn sync_job_polls_without_blocking_the_ui() {
        let directory = tempfile::tempdir().unwrap();
        let paths = DataPaths::new(directory.path());
        let _collection = Collection::create(&paths.collection).unwrap();
        let settings = Settings {
            paths,
            ..Settings::default()
        };
        let mut state = AppState::new(
            ReopeningCollection::new(&settings.paths.collection),
            settings,
        )
        .unwrap();
        let (sender, results) = mpsc::channel();
        state.sync.busy = true;
        state.sync.status = String::from("Syncing with AnkiWeb");
        state.sync_job = Some(SyncJob {
            kind: SyncKind::Sync,
            results,
        });

        state.poll_sync().unwrap();
        assert!(state.sync.busy);
        assert!(state.sync_job.is_some());

        sender.send(Ok(SyncOutcome::NoChanges)).unwrap();
        state.poll_sync().unwrap();
        assert!(!state.sync.busy);
        assert_eq!(state.sync.status, "Already up to date");
        assert!(state.sync_job.is_none());
    }

    #[test]
    fn toasts_expire_after_their_lifetime() {
        let directory = tempfile::tempdir().unwrap();
        let paths = DataPaths::new(directory.path());
        let _collection = Collection::create(&paths.collection).unwrap();
        let settings = Settings {
            paths,
            ..Settings::default()
        };
        let mut state = AppState::new(
            ReopeningCollection::new(&settings.paths.collection),
            settings,
        )
        .unwrap();
        state.say("Saved");
        let start = state.toast.as_ref().unwrap().created;
        assert!(state.poll_toast(start).is_some());
        assert!(state.toast.is_some());
        assert!(state.poll_toast(start + TOAST_TTL).is_none());
        assert!(state.toast.is_none());
    }
}
