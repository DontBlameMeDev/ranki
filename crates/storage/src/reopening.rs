//! Collection access that releases the SQLite file between calls.
//!
//! Anki opens its collection with an exclusive lock and WAL journaling. A
//! connection held open by another process keeps a WAL read lock and makes
//! Anki fail with `DBError: Anki already open, or media currently syncing`.
//! [`ReopeningCollection`] opens the file for one store call at a time, so
//! Anki can take the file while the store is idle.

use crate::Collection;
use ranki_model::AddNoteOptions;
use ranki_model::AnkiConfig;
use ranki_model::ApplyReviewOptions;
use ranki_model::BrowseCard;
use ranki_model::Card;
use ranki_model::CardWithNote;
use ranki_model::Deck;
use ranki_model::DeckPreset;
use ranki_model::DeckStats;
use ranki_model::DeckSteps;
use ranki_model::DeckTotals;
use ranki_model::Id;
use ranki_model::NewNote;
use ranki_model::Note;
use ranki_model::NoteModel;
use ranki_model::RevLog;
use ranki_model::ReviewDecision;
use ranki_model::StockModelKind;
use ranki_model::Store;
use ranki_model::StoreError;
use ranki_model::Timing;
use rusqlite::Connection;
use rusqlite::OpenFlags;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

/// Store backed by a collection file, with no connection held between calls.
#[derive(Debug)]
pub struct ReopeningCollection {
    path: PathBuf,
}

impl ReopeningCollection {
    /// Binds the store to an existing collection file.
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    // Dropping the connection releases the WAL read lock. An explicit
    // checkpoint can fail while another client holds the collection.
    fn read<T>(
        &self,
        call: impl FnOnce(&Collection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let collection = Collection::open(&self.path)?;
        let result = call(&collection);
        drop(collection);
        result
    }

    fn write<T>(
        &self,
        call: impl FnOnce(&mut Collection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut collection = Collection::open(&self.path)?;
        let result = call(&mut collection);
        drop(collection);
        result
    }
}

macro_rules! read_methods {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)+) => {
        $(
            fn $name(&self, $($arg: $ty),*) -> $ret {
                self.read(|store| Store::$name(store, $($arg),*))
            }
        )+
    };
}

macro_rules! write_methods {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)+) => {
        $(
            fn $name(&mut self, $($arg: $ty),*) -> $ret {
                self.write(|store| Store::$name(store, $($arg),*))
            }
        )+
    };
}

impl Store for ReopeningCollection {
    read_methods! {
        decks() -> Result<Vec<Deck>, StoreError>;
        models() -> Result<Vec<NoteModel>, StoreError>;
        tags() -> Result<Vec<String>, StoreError>;
        find_cards(query: &str) -> Result<Vec<Id>, StoreError>;
        due_cards(deck_id: Option<Id>, limit: usize, now_secs: i64)
            -> Result<Vec<Card>, StoreError>;
        card(id: Id) -> Result<Option<Card>, StoreError>;
        deck_steps(deck_id: Id) -> Result<DeckSteps, StoreError>;
        note(id: Id) -> Result<Option<Note>, StoreError>;
        browse_cards(query: &str) -> Result<Vec<BrowseCard>, StoreError>;
        review_history() -> Result<Vec<RevLog>, StoreError>;
        due_forecast(days: usize, now_secs: i64) -> Result<Vec<usize>, StoreError>;
        stats(deck_id: Option<Id>, now_secs: i64) -> Result<DeckStats, StoreError>;
        deck_stats(now_secs: i64) -> Result<DeckTotals, StoreError>;
        collection_created() -> Result<i64, StoreError>;
        anki_config() -> Result<AnkiConfig, StoreError>;
        deck_presets() -> Result<Vec<DeckPreset>, StoreError>;
        timing(now_secs: i64) -> Result<Timing, StoreError>;
        max_taken_secs(deck_id: Id) -> Result<u32, StoreError>;
        review_count_since(cutoff_ms: i64) -> Result<u32, StoreError>;
        last_review_secs(card_id: Id) -> Result<Option<i64>, StoreError>;
        learn_ahead_secs() -> Result<i64, StoreError>;
    }
    fn cards_with_notes(
        &self,
        ids: &[Id],
    ) -> Result<Vec<CardWithNote>, StoreError> {
        self.read(|store| {
            let mut notes: HashMap<Id, Option<Note>> = HashMap::new();
            let mut loaded = Vec::with_capacity(ids.len());
            for id in ids {
                let Some(card) = Store::card(store, *id)? else {
                    continue;
                };
                let cached_note = if let Some(note) = notes.get(&card.note_id) {
                    note.clone()
                } else {
                    let stored_note = Store::note(store, card.note_id)?;
                    let _ = notes.insert(card.note_id, stored_note.clone());
                    stored_note
                };
                let Some(note) = cached_note else {
                    continue;
                };
                loaded.push((card, note));
            }
            Ok(loaded)
        })
    }

    write_methods! {
        create_deck(name: &str) -> Result<Deck, StoreError>;
        remove_deck(id: Id) -> Result<(), StoreError>;
        rename_deck(id: Id, name: &str) -> Result<Deck, StoreError>;
        ensure_model(kind: StockModelKind) -> Result<NoteModel, StoreError>;
        set_rollover(hour: u8) -> Result<(), StoreError>;
        write_preset(preset: &DeckPreset) -> Result<(), StoreError>;
        add_note(note: &NewNote) -> Result<Vec<Id>, StoreError>;
        add_note_with_options(note: &NewNote, options: &AddNoteOptions)
            -> Result<Vec<Id>, StoreError>;
        update_note(note: &Note, deck_id: Id) -> Result<(), StoreError>;
        remove_card(id: Id) -> Result<(), StoreError>;
        apply_review(decision: &ReviewDecision) -> Result<Id, StoreError>;
        apply_review_with_options(decision: &ReviewDecision, options: &ApplyReviewOptions)
            -> Result<Id, StoreError>;
        update_card(card: &Card, now_secs: i64) -> Result<(), StoreError>;
        undo_review(before: &Card, revlog_id: Id) -> Result<bool, StoreError>;
        undo_review_with_note(before: &Card, before_note: Option<&Note>, revlog_id: Id)
            -> Result<bool, StoreError>;
    }

    /// Reads the stamp with a fail-fast connection so the one-second UI poll
    /// cannot wait on Anki's exclusive lock.
    ///
    /// # Errors
    ///
    /// Returns an error when the collection cannot be read.
    fn collection_modified(&self) -> Result<i64, StoreError> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(crate::backend)?;
        connection
            .busy_timeout(Duration::ZERO)
            .map_err(crate::backend)?;
        connection
            .query_row("SELECT mod FROM col WHERE id = 1", [], |row| row.get(0))
            .map_err(crate::backend)
    }
}
