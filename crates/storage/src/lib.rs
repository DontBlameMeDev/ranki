#![expect(
    clippy::excessive_nesting,
    reason = "deck creation writes one hierarchy in a single transaction"
)]
//! Anki-compatible SQLite storage for Ranki.
//!
//! Collections use Anki's `collection.anki2` tables, so files written here can
//! be opened by Anki. Existing collections are read as they are; their schema
//! and collection JSON are never rewritten.
//!
//! # Search syntax
//!
//! [`Collection::find_cards`] accepts a subset of Anki's search bar:
//!
//! - `deck:Name` matches a deck and all of its subdecks.
//! - `tag:Name` matches a note tag.
//! - `note:Name` matches a note model name.
//! - `is:new`, `is:learn`, `is:review`, `is:suspended`, `is:buried`.
//! - Plain words match note fields. `*` matches any number of characters.
//! - Quoted text keeps spaces inside one term; terms are combined with AND.
//!
//! All values are bound as SQL parameters.

mod anki;
mod entities;
mod options;
mod queue;
mod records;
mod reopening;
mod schema;
mod search;
mod tags;

use std::cell::Cell;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::slice;
use std::str;

pub use options::{
    deck_from_json, deck_from_proto, deck_to_json, deck_to_proto,
    preset_from_json, preset_from_proto, preset_to_json, preset_to_proto,
};
pub use ranki_model::DeckPreset;
pub use ranki_model::EffectiveDeck;
pub use ranki_model::StoreError;
pub use ranki_model::stock_models;

pub use anki::deck_kind_config_id;
pub use anki::decode_deck_name;
pub use anki::encode_deck_name;
pub use anki::local_minutes_west;
pub use anki::normal_deck_kind;
pub use anki::protobuf_floats;
pub use anki::with_deck_kind_config_id;
pub use anki::{
    checksum, csum_value, html_to_text_line, legacy_checksum,
    legacy_field_text, sort_field, sort_field_with, strip_html,
    strip_html_preserving_media_filenames,
};
pub use reopening::ReopeningCollection;
pub use tags::canonify as canonify_tags;

pub use records::models_from_json;
pub use records::models_from_tables_with_prefix;
pub use records::models_to_json;

use ranki_model::AddNoteOptions;
use ranki_model::Card;
use ranki_model::CardQueue;
use ranki_model::CardType;
use ranki_model::DEFAULT_DECK_ID;
use ranki_model::Deck;
use ranki_model::DeckStats;
use ranki_model::DeckSteps;
use ranki_model::DeckTotals;
use ranki_model::DuplicateScope;
use ranki_model::Id;
use ranki_model::NewNote;
use ranki_model::Note;
use ranki_model::NoteModel;
use ranki_model::RevLog;
use ranki_model::ReviewDecision;
use ranki_model::StockModelKind;
use ranki_model::Store;
use ranki_model::Timing;
use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::TransactionBehavior;
use rusqlite::functions::FunctionFlags;
use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;
pub use schema::ANKI_DEFAULT_FSRS6_PARAMS_F64;
pub use schema::schema11_default_conf;
pub use schema::schema11_default_deck_config;
use serde_json::Map;
use serde_json::Value as Json;
use serde_json::json;
pub const SEARCH_SYNTAX: &str = "deck:, tag:, note:, is:new/learn/review/due/suspended/buried, plain field text, * wildcard";
const HOME_DECK_SQL: &str = "CASE WHEN c.odid != 0 THEN c.odid ELSE c.did END";
const TRUE_DUE_SQL: &str = "CASE WHEN c.odid != 0 THEN c.odue ELSE c.due END";

/// An Anki collection backed by a SQLite file.
pub struct Collection {
    conn: Connection,
    path: PathBuf,
    created: OnceCell<i64>,
    timing_config: OnceCell<TimingConfig>,
    timing_now: Cell<Option<(i64, Timing)>>,
    options_cache: RefCell<Option<OptionsCache>>,
}

#[derive(Clone, Copy)]
struct TimingConfig {
    creation_offset: Option<i32>,
    rollover: Option<u8>,
    scheduler_v1: bool,
}

struct OptionsCache {
    modified: i64,
    decks: Vec<EffectiveDeck>,
    presets: Vec<DeckPreset>,
}

type QueueOptions = (Vec<EffectiveDeck>, Vec<DeckPreset>);

fn timing_config_from_conf(conf: &str) -> TimingConfig {
    let object: Json = serde_json::from_str(conf).unwrap_or(Json::Null);
    timing_config_from_json(&object)
}

fn timing_config_from_json(object: &Json) -> TimingConfig {
    let creation_offset = object
        .get("creationOffset")
        .and_then(Json::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let rollover = object
        .get("rollover")
        .and_then(Json::as_u64)
        .and_then(|value| u8::try_from(value).ok());
    let scheduler_v1 = object
        .get("schedVer")
        .and_then(Json::as_i64)
        .is_none_or(|version| version < 2);
    TimingConfig {
        creation_offset,
        rollover,
        scheduler_v1,
    }
}

fn timing_config(conn: &Connection) -> Result<TimingConfig, StoreError> {
    if records::table_exists(conn, "config")? {
        let value = |key: &str| -> Result<Option<Json>, StoreError> {
            let stored: Option<SqlValue> = conn
                .query_row(
                    "SELECT val FROM config WHERE key = ?1",
                    [key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(backend)?;
            Ok(match stored {
                Some(SqlValue::Blob(bytes)) => {
                    serde_json::from_slice(&bytes).ok()
                }
                Some(SqlValue::Text(text)) => serde_json::from_str(&text).ok(),
                Some(SqlValue::Integer(number)) => Some(Json::from(number)),
                _ => None,
            })
        };
        return Ok(TimingConfig {
            creation_offset: value("creationOffset")?
                .and_then(|json| json.as_i64())
                .and_then(|number| i32::try_from(number).ok()),
            rollover: value("rollover")?
                .and_then(|json| json.as_u64())
                .and_then(|number| u8::try_from(number).ok()),
            scheduler_v1: value("schedVer")?
                .and_then(|json| json.as_i64())
                .is_none_or(|version| version < 2),
        });
    }
    Ok(timing_config_from_conf(&conf_text(conn)?))
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "SQLite may store conf as text or blob"
)]
pub(crate) fn conf_text(conn: &Connection) -> Result<String, StoreError> {
    let value: SqlValue = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .map_err(backend)?;
    Ok(match value {
        SqlValue::Text(text) => text,
        SqlValue::Blob(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        _ => String::new(),
    })
}

/// True when a legacy collection still declares the original scheduler.
fn legacy_scheduler_v1(conn: &Connection) -> Result<bool, StoreError> {
    let raw = conf_text(conn)?;
    let object: Json = serde_json::from_str(&raw).unwrap_or(Json::Null);
    Ok(object.is_object()
        && object
            .get("schedVer")
            .and_then(Json::as_i64)
            .is_none_or(|version| version < 2))
}

fn ensure_due_indexes(conn: &Connection) -> Result<(), StoreError> {
    if !records::table_exists(conn, "cards")? {
        return Ok(());
    }
    let existing: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' \
             AND name IN ('idx_ranki_cards_queue_due', 'idx_ranki_cards_queue_ord', \
                          'idx_ranki_cards_new_gather')",
            [],
            |row| row.get(0),
        )
        .map_err(backend)?;
    if existing == 3 {
        return Ok(());
    }
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_info('cards')")
        .map_err(backend)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(backend)?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(backend)?;
    if !["queue", "due", "id", "ord", "did"]
        .into_iter()
        .all(|name| columns.contains(name))
    {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_ranki_cards_queue_due ON cards (queue, due, id); \
         CREATE INDEX IF NOT EXISTS idx_ranki_cards_queue_ord ON cards (queue, ord, due); \
         CREATE INDEX IF NOT EXISTS idx_ranki_cards_new_gather ON cards (queue, did, due, ord)",
    )
    .map_err(backend)
}

/// Upgrades the stored scheduler state from Anki's original scheduler.
///
/// Returns `true` when rows were changed. The connection must be writable;
/// callers that import a collection can run this before creating their store.
///
/// # Errors
/// Returns an error when the collection cannot be read or written.
pub fn upgrade_scheduler_v2(conn: &mut Connection) -> Result<bool, StoreError> {
    let transaction = conn.transaction().map_err(backend)?;
    if records::table_exists(&transaction, "config")?
        || !legacy_scheduler_v1(&transaction)?
        || !records::table_exists(&transaction, "cards")?
        || !records::table_exists(&transaction, "revlog")?
        || !column_exists(&transaction, "revlog", "ease")?
    {
        return Ok(false);
    }
    transaction
        .execute(
            "UPDATE revlog SET ease = ease + 1 WHERE ease IN (2, 3) AND type IN (0, 2)",
            [],
        )
        .map_err(backend)?;
    transaction
        .execute(
            "UPDATE cards SET type = 3 WHERE type = 2 AND queue IN (1, 3)",
            [],
        )
        .map_err(backend)?;
    let mut conf_json: Json =
        serde_json::from_str(&conf_text(&transaction)?).unwrap_or(Json::Null);
    if let Some(object) = conf_json.as_object_mut() {
        let _ = object.insert(String::from("schedVer"), json!(2));
        let _ = object.insert(String::from("sched2021"), json!(true));
        if object.get("creationOffset").is_none() {
            let created: i64 = transaction
                .query_row("SELECT crt FROM col WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .map_err(backend)?;
            let offset = anki::local_minutes_west(created)?;
            let _ =
                object.insert(String::from("creationOffset"), json!(offset));
        }
        if object.get("rollover").is_none() {
            let _ = object.insert(String::from("rollover"), json!(4));
        }
    }
    let conf_value = serde_json::to_string(&conf_json).map_err(backend)?;
    transaction
        .execute(
            "UPDATE col SET conf = ?1, scm = ?2 WHERE id = 1",
            params![conf_value, anki::now_ms()?],
        )
        .map_err(backend)?;
    transaction.commit().map_err(backend)?;
    Ok(true)
}

impl Collection {
    /// Creates a new Anki collection at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file already exists or SQLite cannot create
    /// the schema and the default collection row.
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        if path.exists() {
            return Err(StoreError::InvalidData(format!(
                "collection already exists: {}",
                path.display()
            )));
        }
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(backend)?;
        register_collations(&conn)?;
        register_functions(&conn)?;
        configure_connection(&conn)?;
        let transaction = conn.transaction().map_err(backend)?;
        for statement in schema::CREATE_STATEMENTS {
            transaction.execute(statement, []).map_err(backend)?;
        }
        let now_secs = anki::now_secs()?;
        let creation_offset = anki::local_minutes_west(now_secs)?;
        let col = schema::default_col(
            now_secs,
            anki::now_ms()?,
            Some(creation_offset),
        );
        let text = |key: &str| -> Result<String, StoreError> {
            let value =
                col.get(key).ok_or_else(|| invalid("missing col field"))?;
            serde_json::to_string(value)
                .map_err(|error| invalid(error.to_string()))
        };
        transaction
            .execute(
                "INSERT INTO col (id, crt, mod, scm, ver, dty, usn, ls, conf, models, decks, dconf, \
                 tags) VALUES (1, ?1, ?2, ?3, ?4, 0, 0, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    col.get("crt").and_then(Json::as_i64).unwrap_or(now_secs),
                    col.get("mod").and_then(Json::as_i64).unwrap_or_default(),
                    col.get("scm").and_then(Json::as_i64).unwrap_or_default(),
                    col.get("ver").and_then(Json::as_i64).unwrap_or_default(),
                    col.get("ls").and_then(Json::as_i64).unwrap_or_default(),
                    text("conf")?,
                    text("models")?,
                    text("decks")?,
                    text("dconf")?,
                    text("tags")?,
                ],
            )
            .map_err(backend)?;
        transaction.commit().map_err(backend)?;
        Self::open(path)
    }

    /// Opens an existing collection for reading and writing.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is missing, locked, or has no `col`
    /// table.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(backend)?;
        register_collations(&conn)?;
        register_functions(&conn)?;
        configure_connection(&conn)?;
        if !records::table_exists(&conn, "col")? {
            return Err(invalid("not an Anki collection"));
        }
        let _ = upgrade_scheduler_v2(&mut conn)?;
        ensure_due_indexes(&conn)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            created: OnceCell::new(),
            timing_config: OnceCell::new(),
            timing_now: Cell::new(None),
            options_cache: RefCell::new(None),
        })
    }

    /// Returns the collection's file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads all deck presets from the active storage layout.
    ///
    /// # Errors
    /// Returns an error when collection metadata cannot be read.
    pub fn presets(&self) -> Result<Vec<DeckPreset>, StoreError> {
        options::presets(&self.conn)
    }

    /// Reads one deck preset by id.
    ///
    /// # Errors
    /// Returns an error when collection metadata cannot be read.
    pub fn preset(&self, id: Id) -> Result<Option<DeckPreset>, StoreError> {
        Ok(self.presets()?.into_iter().find(|preset| preset.id == id))
    }

    /// Reads deck identity and overrides from the active storage layout.
    ///
    /// # Errors
    /// Returns an error when collection metadata cannot be read.
    pub fn effective_decks(&self) -> Result<Vec<EffectiveDeck>, StoreError> {
        options::effective_decks(&self.conn)
    }

    /// Reads the deck and preset state used by the queue builder.
    ///
    /// The cache follows `col.mod`, so local writes and external commits both
    /// invalidate it.
    pub(crate) fn queue_options(&self) -> Result<QueueOptions, StoreError> {
        let modified: i64 = self
            .conn
            .query_row("SELECT mod FROM col WHERE id = 1", [], |row| row.get(0))
            .map_err(backend)?;
        if self
            .options_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| cache.modified == modified)
        {
            let borrowed = self.options_cache.borrow();
            let Some(cached) = borrowed.as_ref() else {
                return Err(invalid("options cache disappeared"));
            };
            return Ok((cached.decks.clone(), cached.presets.clone()));
        }
        let decks = options::effective_decks(&self.conn)?;
        let presets = options::presets(&self.conn).unwrap_or_default();
        let _ = self.options_cache.borrow_mut().replace(OptionsCache {
            modified,
            decks: decks.clone(),
            presets: presets.clone(),
        });
        Ok((decks, presets))
    }

    /// Writes one preset back to the active storage layout.
    ///
    /// # Errors
    /// Returns an error when the preset cannot be written.
    pub fn write_preset(&self, preset: &DeckPreset) -> Result<(), StoreError> {
        options::write_preset(&self.conn, preset)
    }

    /// Writes one deck back to the active storage layout.
    ///
    /// # Errors
    /// Returns an error when the deck cannot be written.
    pub fn write_deck(&self, deck: &EffectiveDeck) -> Result<(), StoreError> {
        options::write_deck(&self.conn, deck)
    }

    /// Installs a stock note type and returns the stored model.
    ///
    /// The existing model is returned when the collection already has that
    /// stock type. Schema-18 collections that keep note types in normalized
    /// tables get rows there instead of `col.models`.
    fn install_model(
        &mut self,
        kind: StockModelKind,
    ) -> Result<NoteModel, StoreError> {
        let existing = self.model_list()?;
        if let Some(model) = existing
            .iter()
            .find(|model| StockModelKind::of(model) == Some(kind))
        {
            return Ok(model.clone());
        }
        let mut model = kind.definition();
        model.id = fresh_model_id(&self.conn, &existing)?;
        let col = records::read_col(&self.conn)?;
        let stamp = col_stamp(&col)?;
        let tables = models_in_tables(&self.conn, &col)?;
        let models = if tables {
            None
        } else {
            Some(models_json_installing(&col.models, &model)?)
        };
        let transaction = self.conn.transaction().map_err(backend)?;
        if let Some(json) = models.as_deref() {
            transaction
                .execute(
"UPDATE col SET mod = ?1, scm = ?2, models = ?3 WHERE id = 1",
params![stamp.modified, stamp.scm, json],
                )
                .map_err(backend)?;
        } else {
            transaction
                .execute(
                    "UPDATE col SET mod = ?1, scm = ?2 WHERE id = 1",
                    params![stamp.modified, stamp.scm],
                )
                .map_err(backend)?;
            insert_model_tables(&transaction, &model, &stamp)?;
        }
        transaction.commit().map_err(backend)?;
        Ok(model)
    }

    /// Returns the collection creation time in Unix seconds.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read.
    pub fn collection_created(&self) -> Result<i64, StoreError> {
        if let Some(created) = self.created.get() {
            return Ok(*created);
        }
        let created = self
            .conn
            .query_row("SELECT crt FROM col WHERE id = 1", [], |row| row.get(0))
            .map_err(backend)?;
        let _ = self.created.set(created);
        Ok(created)
    }

    /// Returns Anki's day boundary for `now_secs`.
    ///
    /// # Errors
    /// Returns an error when the collection metadata cannot be read.
    pub fn timing(&self, now_secs: i64) -> Result<Timing, StoreError> {
        if let Some((cached_now, cached)) = self.timing_now.get().as_ref()
            && *cached_now == now_secs
        {
            return Ok(*cached);
        }
        let config = if let Some(config) = self.timing_config.get() {
            *config
        } else {
            let config = timing_config(&self.conn)?;
            let _ = self.timing_config.set(config);
            config
        };
        let created = self.collection_created()?;
        let timing = if config.scheduler_v1 {
            Timing::v1(now_secs, created)
        } else {
            Timing::new(
                now_secs,
                created,
                config.creation_offset,
                anki::local_minutes_west(now_secs)?,
                config.rollover.or(Some(4)),
            )
        };
        self.timing_now.set(Some((now_secs, timing)));
        Ok(timing)
    }

    /// Returns Anki's learn-ahead window in seconds, from `collapseTime`.
    ///
    /// # Errors
    ///
    /// Returns an error when the collection config cannot be read.
    pub fn learn_ahead_secs(&self) -> Result<i64, StoreError> {
        queue::learn_ahead_secs(&self.conn)
    }

    /// Runs the scheduler-v1 upgrade on this collection.
    ///
    /// # Errors
    /// Returns an error when the collection cannot be written.
    pub fn upgrade_scheduler_v2(&mut self) -> Result<bool, StoreError> {
        let upgraded = upgrade_scheduler_v2(&mut self.conn)?;
        let _ = self.timing_config.take();
        let _ = self.timing_now.take();
        Ok(upgraded)
    }

    /// Checkpoints the write-ahead log, if the collection uses one.
    ///
    /// # Errors
    ///
    /// Returns an error when SQLite cannot report or checkpoint the journal.
    pub fn checkpoint(&self) -> Result<(), StoreError> {
        let mode: String = self
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(backend)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Ok(());
        }
        let busy: i64 = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(backend)?;
        if busy != 0 {
            return Err(invalid("checkpoint blocked by another writer"));
        }
        Ok(())
    }

    /// Checkpoints and closes the collection.
    ///
    /// # Errors
    ///
    /// Returns an error when the checkpoint or the SQLite close fails.
    pub fn close(self) -> Result<(), StoreError> {
        self.checkpoint()?;
        let Self { conn, .. } = self;
        conn.close().map_err(|(_, error)| backend(error))
    }

    /// Returns a single review log entry by id.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read.
    pub fn revlog(&self, id: Id) -> Result<Option<RevLog>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, cid, usn, ease, ivl, lastIvl, factor, time, type FROM revlog WHERE id = \
                 ?1",
                [id],
                records::revlog_from_row,
            )
            .optional()
            .map_err(backend)
    }

    /// Persists several scheduled reviews and their log entries in one
    /// transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when any card does not exist or the write fails. A
    /// failed batch writes nothing.
    pub fn apply_reviews(
        &mut self,
        decisions: &[ReviewDecision],
    ) -> Result<(), StoreError> {
        let today = self.timing(anki::now_secs()?)?.days_elapsed;
        let transaction = self.conn.transaction().map_err(backend)?;
        let usn = update_col(&transaction, false)?;
        for decision in decisions {
            write_review(&transaction, usn, decision, today)?;
        }
        transaction.commit().map_err(backend)?;
        Ok(())
    }

    /// Lists every deck, including an empty Default deck.
    ///
    /// # Errors
    /// Returns an error when the collection cannot be read.
    pub fn deck_list(&self) -> Result<Vec<Deck>, StoreError> {
        deck_list_from(&self.conn)
    }

    fn model_list(&self) -> Result<Vec<NoteModel>, StoreError> {
        let col = records::read_col(&self.conn)?;
        if models_in_tables(&self.conn, &col)? {
            return records::models_from_tables(&self.conn);
        }
        records::models_from_json(&col.models)
    }

    /// Lists the deck and every subdeck below it in the `A::B` hierarchy.
    /// Returns an empty list when the deck is missing.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the collection cannot be read.
    pub fn deck_subtree(&self, deck_id: Id) -> Result<Vec<Id>, StoreError> {
        let decks = self.deck_list()?;
        let Some(deck) = decks.iter().find(|deck| deck.id == deck_id) else {
            return Ok(Vec::new());
        };
        Ok(deck_subtree_ids(&decks, &deck.name))
    }

    fn deck_filter(
        &self,
        deck_id: Option<Id>,
    ) -> Result<DeckFilter, StoreError> {
        let Some(id) = deck_id else {
            return Ok(DeckFilter {
                params: Vec::new(),
                missing: false,
            });
        };
        let ids = self.deck_subtree(id)?;
        if ids.is_empty() {
            return Ok(DeckFilter {
                params: Vec::new(),
                missing: true,
            });
        }
        Ok(DeckFilter {
            params: ids.into_iter().map(SqlValue::Integer).collect(),
            missing: false,
        })
    }

    /// Builds a search query with the current day and learn-ahead cutoff.
    fn search_query_timed(
        &self,
        query: &str,
    ) -> Result<search::SearchQuery, StoreError> {
        let tokens = search::tokenize(query)?;
        let decks = if search::needs_decks(&tokens) {
            self.deck_list()?
        } else {
            Vec::new()
        };
        let models = if search::needs_models(&tokens) {
            self.model_list()?
        } else {
            Vec::new()
        };
        let (today, learn_cutoff) = if search::needs_timing(&tokens) {
            let now = anki::now_secs()?;
            let timing = self.timing(now)?;
            (
                i64::from(timing.days_elapsed),
                now.saturating_add(queue::learn_ahead_secs(&self.conn)?),
            )
        } else {
            (0, 0)
        };
        search::build(&tokens, &decks, &models, today, learn_cutoff)
    }

    /// Returns up to `limit` matching card ids starting at `offset`.
    ///
    /// # Errors
    /// Returns an error when the query is invalid, a page bound is too large,
    /// or the database cannot be read.
    #[expect(
        clippy::too_many_arguments,
        reason = "page bounds stay explicit at the call site"
    )]
    pub fn find_cards_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Id>, StoreError> {
        let limit_value =
            i64::try_from(limit).map_err(|_| invalid("limit too large"))?;
        let offset_value =
            i64::try_from(offset).map_err(|_| invalid("offset too large"))?;
        let search = self.search_query_timed(query)?;
        let sql = format!(
            "SELECT c.id FROM cards c JOIN notes n ON n.id = c.nid WHERE {} \
             ORDER BY c.id LIMIT ? OFFSET ?",
            search.clause
        );
        let mut parameters = search.params;
        parameters.push(SqlValue::Integer(limit_value));
        parameters.push(SqlValue::Integer(offset_value));
        let mut statement = self.conn.prepare(&sql).map_err(backend)?;
        let rows = statement
            .query_map(params_from_iter(parameters), |row| row.get::<_, Id>(0))
            .map_err(backend)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(backend)?);
        }
        Ok(ids)
    }
    /// Returns the deck of the first card for a note, if it has one.
    ///
    /// # Errors
    /// Returns an error when the database cannot be read.
    pub fn note_deck_id(&self, note_id: Id) -> Result<Option<Id>, StoreError> {
        self.conn
            .query_row(
                "SELECT did FROM cards WHERE nid = ?1 LIMIT 1",
                [note_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)
    }

    /// Replaces a note's fields, tags, and note type.
    ///
    /// Cards move only when `deck_id` is set. `expected_modified` rejects the
    /// write inside the transaction when another writer changed the note.
    ///
    /// # Errors
    /// Returns an error when the note or model is missing, the field count does
    /// not match the model, the edit would change the card set, a supplied
    /// revision is stale, or the write fails.
    #[expect(
        clippy::too_many_arguments,
        reason = "the revision and deck move are independent write guards"
    )]
    pub fn update_note_checked(
        &mut self,
        note: &Note,
        deck_id: Option<Id>,
        expected_modified: Option<i64>,
    ) -> Result<(), StoreError> {
        self.write_note(note, deck_id, expected_modified)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "note updates keep validation and writes in one transaction"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "the revision and deck move are independent write guards"
    )]
    fn write_note(
        &mut self,
        note: &Note,
        deck_id: Option<Id>,
        expected_modified: Option<i64>,
    ) -> Result<(), StoreError> {
        let model = self
            .model_list()?
            .into_iter()
            .find(|model| model.id == note.model_id)
            .ok_or_else(|| {
                StoreError::NotFound(format!("model {}", note.model_id))
            })?;
        if note.fields.is_empty() || note.fields.len() != model.fields.len() {
            return Err(invalid("field count does not match model"));
        }
        if let Some(target) = deck_id
            && !self.deck_subtree(target)?.contains(&target)
        {
            return Err(StoreError::NotFound(format!("deck {target}")));
        }
        let normalize_text = normalize_note_text(&self.conn)?;
        let mut fields = note.fields.clone();
        for field in &mut fields {
            anki::normalize_field(field, normalize_text);
        }
        let first = fields.first().map_or("", String::as_str);
        let transaction = self.conn.transaction().map_err(backend)?;
        let stored_modified: Option<i64> = transaction
            .query_row(
                "SELECT mod FROM notes WHERE id = ?1",
                [note.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(current_modified) = stored_modified else {
            return Err(StoreError::NotFound(format!("note {}", note.id)));
        };
        if expected_modified
            .is_some_and(|expected| expected != current_modified)
        {
            return Err(invalid("note changed; read it again"));
        }
        let mut expected = model.card_ordinals(&fields);
        expected.sort_unstable();
        let mut statement = transaction
            .prepare("SELECT ord FROM cards WHERE nid = ?1 ORDER BY ord")
            .map_err(backend)?;
        let rows = statement
            .query_map([note.id], |row| row.get::<_, i64>(0))
            .map_err(backend)?;
        let mut stored = Vec::new();
        for row in rows {
            stored.push(
                u16::try_from(row.map_err(backend)?)
                    .map_err(|_| invalid("card ordinal out of range"))?,
            );
        }
        if expected != stored {
            return Err(invalid("editing would change the card set"));
        }
        drop(statement);
        let usn = update_col(&transaction, false)?;
        let tags = tags::canonify(&note.tags, &transaction, usn)?;
        let modified =
            anki::now_secs()?.max(current_modified.saturating_add(1));
        let sort_field = anki::sort_field_with(
            &fields,
            usize::from(model.sort_field_index),
            normalize_text,
        );
        let checksum = anki::checksum_with(first, normalize_text);
        transaction
            .execute(
                "UPDATE notes SET mid = ?1, mod = ?2, usn = ?3, tags = ?4, flds = ?5, \
                 sfld = ?6, csum = ?7 WHERE id = ?8",
                params![
                    note.model_id,
                    modified,
                    usn,
                    anki::encode_tags(&tags),
                    anki::encode_fields(&fields),
                    sort_field,
                    anki::csum_value(checksum),
                    note.id,
                ],
            )
            .map_err(backend)?;
        if let Some(target) = deck_id {
            transaction
                .execute(
                    "UPDATE cards SET did = ?1, mod = ?2, usn = ?3 WHERE nid = ?4",
                    params![target, modified, usn, note.id],
                )
                .map_err(backend)?;
        }
        transaction.commit().map_err(backend)
    }
}

impl Store for Collection {
    fn decks(&self) -> Result<Vec<Deck>, StoreError> {
        let decks = self.deck_list()?;
        if decks.len() <= 1 {
            return Ok(decks);
        }
        let Some(default) =
            decks.iter().find(|deck| deck.id == DEFAULT_DECK_ID)
        else {
            return Ok(decks);
        };
        let child_prefix = format!("{}::", default.name);
        if decks
            .iter()
            .any(|deck| deck.name.starts_with(&child_prefix))
        {
            return Ok(decks);
        }
        let occupied = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM cards WHERE did = ?1)",
                [DEFAULT_DECK_ID],
                |row| row.get::<_, bool>(0),
            )
            .map_err(backend)?;
        if occupied {
            return Ok(decks);
        }
        Ok(decks
            .into_iter()
            .filter(|deck| deck.id != DEFAULT_DECK_ID)
            .collect())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one transaction creates the whole deck hierarchy"
    )]
    fn create_deck(&mut self, name: &str) -> Result<Deck, StoreError> {
        let requested = validate_deck_name(name)?;
        let decks = self.deck_list()?;
        if decks
            .iter()
            .any(|deck| deck.name.eq_ignore_ascii_case(requested))
        {
            return Err(invalid(format!("deck already exists: {requested}")));
        }
        let plan = deck_creation_plan(&decks, requested);
        let col = records::read_col(&self.conn)?;
        let stamp = col_stamp(&col)?;
        let modern = uses_deck_table(&self.conn, &col)?;
        let transaction = self.conn.transaction().map_err(backend)?;
        let mut next_id = fresh_id(&transaction, "notes")?;
        let mut json_decks = if modern {
            None
        } else {
            let value: Json = serde_json::from_str(&col.decks)
                .map_err(|error| invalid(error.to_string()))?;
            if !value.is_object() {
                return Err(invalid("decks is not an object"));
            }
            Some(value)
        };
        let mut created = Vec::new();
        for (deck_name, existing) in &plan {
            if existing.is_some() {
                continue;
            }
            let id = next_id;
            next_id = next_id.saturating_add(1);
            if modern {
                transaction
                    .execute(
                        "INSERT INTO decks (id, name, mtime_secs, usn, common, kind) VALUES (?1, ?2, \
                         ?3, ?4, ?5, ?6)",
                        params![
                            id,
                            anki::encode_deck_name(deck_name),
                            stamp.now_secs,
                            stamp.usn,
                            Vec::<u8>::new(),
                            anki::normal_deck_kind(1),
                        ],
                    )
                    .map_err(backend)?;
            } else if let Some(object) =
                json_decks.as_mut().and_then(Json::as_object_mut)
            {
                object.insert(
                    id.to_string(),
                    json!({"id": id, "name": deck_name, "usn": stamp.usn, "mod": stamp.now_secs, "conf": 1, "desc": "", "dyn": 0, "collapsed": false, "browserCollapsed": false, "lrnToday": [0, 0], "revToday": [0, 0], "newToday": [0, 0], "timeToday": [0, 0], "extendNew": 0, "extendRev": 0}),
                );
            }
            created.push((id, deck_name.clone()));
        }
        if modern {
            transaction
                .execute(
                    "UPDATE col SET mod = ?1, scm = ?2 WHERE id = 1",
                    params![stamp.modified, stamp.scm],
                )
                .map_err(backend)?;
        } else {
            let decks_json = serde_json::to_string(&json_decks)
                .map_err(|error| invalid(error.to_string()))?;
            transaction
                .execute(
                    "UPDATE col SET mod = ?1, scm = ?2, decks = ?3 WHERE id = 1",
                    params![stamp.modified, stamp.scm, decks_json],
                )
                .map_err(backend)?;
        }
        transaction.commit().map_err(backend)?;
        let (id, name) = created.pop().ok_or_else(|| {
            StoreError::InvalidData("deck hierarchy already exists".into())
        })?;
        Ok(Deck {
            id,
            name,
            description: String::new(),
        })
    }

    fn remove_deck(&mut self, id: Id) -> Result<(), StoreError> {
        if id == DEFAULT_DECK_ID {
            return Err(invalid("the Default deck cannot be removed"));
        }
        let transaction = self.conn.transaction().map_err(backend)?;
        let decks = deck_list_from(&transaction)?;
        let Some(root) = decks.iter().find(|deck| deck.id == id) else {
            return Err(StoreError::NotFound(format!("deck {id}")));
        };
        let subtree = deck_subtree_ids(&decks, &root.name);
        let (card_ids, note_ids) = cards_in_decks(&transaction, &subtree)?;
        delete_cards(&transaction, &card_ids)?;
        let deleted_notes = delete_orphan_notes(&transaction, &note_ids)?;
        reset_original_decks(&transaction, &subtree)?;

        let col = records::read_col(&transaction)?;
        let stamp = col_stamp(&col)?;
        let modern = uses_deck_table(&transaction, &col)?;
        let decks_json = if modern {
            None
        } else {
            Some(decks_json_without(&col.decks, &subtree)?)
        };
        write_col_metadata(&transaction, &stamp, decks_json.as_deref())?;
        if modern {
            delete_from_deck_table(&transaction, &subtree)?;
        }
        let graves = Graves {
            cards: card_ids,
            notes: deleted_notes,
            decks: subtree,
        };
        write_graves(&transaction, &stamp, &graves)?;
        transaction.commit().map_err(backend)
    }

    fn rename_deck(&mut self, id: Id, name: &str) -> Result<Deck, StoreError> {
        let new_name = validate_deck_name(name)?;
        let transaction = self.conn.transaction().map_err(backend)?;
        let decks = deck_list_from(&transaction)?;
        let changes = rename_plan(&decks, id, new_name)?;
        let description = decks
            .iter()
            .find(|deck| deck.id == id)
            .map_or(String::new(), |deck| deck.description.clone());
        let col = records::read_col(&transaction)?;
        let stamp = col_stamp(&col)?;
        let modern = uses_deck_table(&transaction, &col)?;
        let decks_json = if modern {
            None
        } else {
            Some(renamed_decks_json(&col.decks, &changes, &stamp)?)
        };
        write_col_metadata(&transaction, &stamp, decks_json.as_deref())?;
        if modern {
            rename_in_deck_table(&transaction, &changes, &stamp)?;
        }
        transaction.commit().map_err(backend)?;
        Ok(Deck {
            id,
            name: new_name.into(),
            description,
        })
    }

    fn models(&self) -> Result<Vec<NoteModel>, StoreError> {
        self.model_list()
    }

    fn tags(&self) -> Result<Vec<String>, StoreError> {
        tags::all(&self.conn)
    }

    fn ensure_model(
        &mut self,
        kind: StockModelKind,
    ) -> Result<NoteModel, StoreError> {
        self.install_model(kind)
    }

    fn find_cards(&self, query: &str) -> Result<Vec<Id>, StoreError> {
        let search = self.search_query_timed(query)?;
        let sql = format!(
            "SELECT c.id FROM cards c JOIN notes n ON n.id = c.nid WHERE {} ORDER BY c.id",
            search.clause
        );
        let mut statement = self.conn.prepare(&sql).map_err(backend)?;
        let rows = statement
            .query_map(params_from_iter(search.params), |row| {
                row.get::<_, Id>(0)
            })
            .map_err(backend)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(backend)?);
        }
        Ok(ids)
    }

    fn browse_cards(
        &self,
        query: &str,
    ) -> Result<Vec<ranki_model::BrowseCard>, StoreError> {
        let search = self.search_query_timed(query)?;
        let sql = format!(
            "SELECT c.id, c.did, n.sfld FROM cards c JOIN notes n ON n.id = c.nid \
             WHERE {} ORDER BY c.id",
            search.clause
        );
        let mut statement = self.conn.prepare(&sql).map_err(backend)?;
        let rows = statement
            .query_map(params_from_iter(search.params), |row| {
                Ok(ranki_model::BrowseCard {
                    card_id: row.get(0)?,
                    deck_id: row.get(1)?,
                    // `sfld` has integer affinity, so a numeric-looking sort
                    // field comes back as a number, not text.
                    sort_field: records::text(row.get::<_, SqlValue>(2)?),
                })
            })
            .map_err(backend)?;
        let mut cards = Vec::new();
        for row in rows {
            cards.push(row.map_err(backend)?);
        }
        Ok(cards)
    }

    fn due_cards(
        &self,
        deck_id: Option<Id>,
        limit: usize,
        now_secs: i64,
    ) -> Result<Vec<Card>, StoreError> {
        let timing = self.timing(now_secs)?;
        let real_today = self.timing(anki::now_secs()?)?.days_elapsed;
        let (decks, presets) = self.queue_options()?;
        queue::gather_due_cards(
            &self.conn, deck_id, limit, now_secs, timing, real_today, &decks,
            &presets,
        )
    }

    fn card(&self, id: Id) -> Result<Option<Card>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, lapses, \
                 left, odue, odid, flags, data FROM cards WHERE id = ?1",
                [id],
                records::card_from_row,
            )
            .optional()
            .map_err(backend)
    }

    fn note(&self, id: Id) -> Result<Option<ranki_model::Note>, StoreError> {
        self.conn
            .query_row(
                "SELECT id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data FROM notes \
                 WHERE id = ?1",
                [id],
                records::note_from_row,
            )
            .optional()
            .map_err(backend)
    }

    fn add_note(&mut self, note: &NewNote) -> Result<Vec<Id>, StoreError> {
        self.add_note_with_options(note, &AddNoteOptions::default())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "note writes keep validation and card generation in one transaction"
    )]
    fn add_note_with_options(
        &mut self,
        note: &NewNote,
        options: &AddNoteOptions,
    ) -> Result<Vec<Id>, StoreError> {
        let model = self
            .model_list()?
            .into_iter()
            .find(|model| model.id == note.model_id)
            .ok_or_else(|| {
                StoreError::NotFound(format!("model {}", note.model_id))
            })?;
        if note.fields.is_empty() || note.fields.len() != model.fields.len() {
            return Err(invalid("field count does not match model"));
        }
        if !self.deck_subtree(note.deck_id)?.contains(&note.deck_id) {
            return Err(StoreError::NotFound(format!("deck {}", note.deck_id)));
        }
        let normalize_text = normalize_note_text(&self.conn)?;
        let mut fields = note.fields.clone();
        for field in &mut fields {
            anki::normalize_field(field, normalize_text);
        }
        let first = fields.first().map_or("", String::as_str);
        let checksum = anki::checksum_with(first, normalize_text);
        let original_first = note.fields.first().map_or("", String::as_str);
        let legacy_sum = anki::legacy_checksum(original_first);
        let stripped_first = anki::strip_html_preserving_media_filenames_with(
            first,
            normalize_text,
        );
        let legacy_first = anki::legacy_field_text(original_first);
        if stripped_first.trim().is_empty() {
            return Err(invalid("empty question"));
        }
        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        if !options.allow_duplicate
            && duplicate_exists(
                &transaction,
                note.model_id,
                checksum,
                legacy_sum,
                &stripped_first,
                &legacy_first,
                normalize_text,
                &options.duplicate_scope,
            )?
        {
            return Err(invalid("duplicate"));
        }
        let id = fresh_id(&transaction, "notes")?;
        let guid = anki::guid(id)?;
        let modified = anki::now_secs()?;
        let sort_field = anki::sort_field_with(
            &fields,
            usize::from(model.sort_field_index),
            normalize_text,
        );
        let usn = update_col(&transaction, false)?;
        let tags = tags::canonify(&note.tags, &transaction, usn)?;
        let next_position = next_position(&transaction)?;
        let random_insert = options::effective_decks(&transaction)
            .ok()
            .into_iter()
            .flatten()
            .find(|deck| deck.id == note.deck_id)
            .and_then(|deck| {
                options::presets(&transaction)
                    .ok()?
                    .into_iter()
                    .find(|preset| preset.id == deck.preset_id)
            })
            .is_some_and(|preset| preset.new_card_insert_order == 1);
        transaction
            .execute(
                "INSERT INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, '')",
                params![
                    id,
                    guid,
                    note.model_id,
                    modified,
                    usn,
                    anki::encode_tags(&tags),
                    anki::encode_fields(&fields),
                    sort_field,
                    anki::csum_value(checksum),
                ],
            )
            .map_err(backend)?;
        let ordinals = model.card_ordinals(&fields);
        let mut cards: Vec<Id> = Vec::with_capacity(ordinals.len());
        for (offset, ordinal) in ordinals.into_iter().enumerate() {
            let card_id = fresh_id(&transaction, "cards")?;
            let due = queue::new_card_position(
                next_position,
                i64::try_from(offset).unwrap_or_default(),
                random_insert,
            );
            transaction
                .execute(
                    "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, \
                     reps, lapses, left, odue, odid, flags, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
                     0, 0, ?7, 0, 0, 0, 0, 0, 0, 0, 0, ?8)",
                    params![
                        card_id,
                        id,
                        note.deck_id,
                        ordinal,
                        modified,
                        usn,
                        due,
                        format!("{{\"pos\":{due}}}"),
                    ],
                )
                .map_err(backend)?;
            cards.push(card_id);
        }
        set_next_position(
            &transaction,
            next_position
                .saturating_add(i64::try_from(cards.len()).unwrap_or_default()),
        )?;
        transaction.commit().map_err(backend)?;
        Ok(cards)
    }

    fn update_note(
        &mut self,
        note: &Note,
        deck_id: Id,
    ) -> Result<(), StoreError> {
        self.write_note(note, Some(deck_id), None)
    }

    fn remove_card(&mut self, id: Id) -> Result<(), StoreError> {
        let Some(card) = self.card(id)? else {
            return Err(StoreError::NotFound(format!("card {id}")));
        };
        let transaction = self.conn.transaction().map_err(backend)?;
        delete_cards(&transaction, &[id])?;
        let deleted_notes = delete_orphan_notes(&transaction, &[card.note_id])?;
        let col = records::read_col(&transaction)?;
        let stamp = col_stamp(&col)?;
        write_col_metadata(&transaction, &stamp, None)?;
        write_graves(
            &transaction,
            &stamp,
            &Graves {
                cards: vec![id],
                notes: deleted_notes,
                decks: Vec::new(),
            },
        )?;
        transaction.commit().map_err(backend)
    }

    fn apply_review(
        &mut self,
        decision: &ReviewDecision,
    ) -> Result<Id, StoreError> {
        let today = self.timing(anki::now_secs()?)?.days_elapsed;
        let transaction = self.conn.transaction().map_err(backend)?;
        let current: Option<(i64, i32)> = transaction
            .query_row(
                "SELECT mod, usn FROM cards WHERE id = ?1",
                [decision.card.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(backend)?;
        if current != Some((decision.expected_modified, decision.expected_usn))
        {
            return Err(invalid("card changed since it was loaded"));
        }
        let usn = update_col(&transaction, false)?;
        let log_id = write_review(&transaction, usn, decision, today)?;
        transaction.commit().map_err(backend)?;
        Ok(log_id)
    }

    fn update_card(
        &mut self,
        card: &Card,
        now_secs: i64,
    ) -> Result<(), StoreError> {
        let transaction = self.conn.transaction().map_err(backend)?;
        let current: Option<(i64, i32)> = transaction
            .query_row(
                "SELECT mod, usn FROM cards WHERE id = ?1",
                [card.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(backend)?;
        if current != Some((card.modified, card.usn)) {
            return Err(invalid("card changed since it was loaded"));
        }
        let usn = update_col(&transaction, false)?;
        let updated = transaction
            .execute(
                "UPDATE cards SET did = ?1, mod = ?2, usn = ?3, type = ?4, queue = ?5, due = ?6, \
                 ivl = ?7, factor = ?8, reps = ?9, lapses = ?10, left = ?11, odue = ?12, odid = \
                 ?13, flags = ?14, data = ?15 WHERE id = ?16",
                params![
                    card.deck_id,
                    now_secs,
                    usn,
                    card.card_type.code(),
                    card.queue.code(),
                    card.due,
                    card.interval,
                    card.factor,
                    card.reps,
                    card.lapses,
                    card.left,
                    card.original_due,
                    card.original_deck_id,
                    card.flags,
                    card.data,
                    card.id,
                ],
            )
            .map_err(backend)?;
        if updated == 0 {
            return Err(StoreError::NotFound(format!("card {}", card.id)));
        }
        transaction.commit().map_err(backend)?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "undo restores the card, buried siblings and counters in one transaction"
    )]
    fn undo_review(
        &mut self,
        before: &Card,
        revlog_id: Id,
    ) -> Result<bool, StoreError> {
        let today = self.timing(anki::now_secs()?)?.days_elapsed;
        let answer_millis: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM revlog WHERE id = ?1 AND cid = ?2 AND id = \
                 (SELECT MAX(id) FROM revlog WHERE cid = ?2)",
                params![revlog_id, before.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let Some(millis) = answer_millis else {
            return Ok(false);
        };
        let answer_day = self.timing(millis.div_euclid(1_000))?.days_elapsed;
        let transaction = self.conn.transaction().map_err(backend)?;
        let usn = update_col(&transaction, false)?;
        transaction
            .execute(
                "DELETE FROM revlog WHERE id = ?1 AND cid = ?2",
                params![revlog_id, before.id],
            )
            .map_err(backend)?;
        let updated = transaction
            .execute(
                "UPDATE cards SET did = ?1, mod = ?2, usn = ?3, type = ?4, queue = ?5, due = ?6, \
                 ivl = ?7, factor = ?8, reps = ?9, lapses = ?10, left = ?11, odue = ?12, odid = \
                 ?13, flags = ?14, data = ?15 WHERE id = ?16",
                params![
                    before.deck_id,
                    before.modified,
                    usn,
                    before.card_type.code(),
                    before.queue.code(),
                    before.due,
                    before.interval,
                    before.factor,
                    before.reps,
                    before.lapses,
                    before.left,
                    before.original_due,
                    before.original_deck_id,
                    before.flags,
                    before.data,
                    before.id,
                ],
            )
            .map_err(backend)?;
        if updated == 0 {
            return Err(StoreError::NotFound(format!("card {}", before.id)));
        }
        let mut statement = transaction
            .prepare(
                "SELECT id, type, due FROM cards WHERE nid = ?1 AND queue = -2 AND id != ?2 AND mod = ?3",
            )
            .map_err(backend)?;
        let rows = statement
            .query_map(
                params![before.note_id, before.id, millis.div_euclid(1_000)],
                |row| {
                    Ok((
                        row.get::<_, Id>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(backend)?;
        let mut siblings = Vec::new();
        for row in rows {
            let (id, card_type, due) = row.map_err(backend)?;
            siblings.push((
                id,
                queue::restore_queue(CardType::from_code(card_type), due),
            ));
        }
        drop(statement);
        for (id, queue) in siblings {
            transaction
                .execute(
                    "UPDATE cards SET queue = ?1 WHERE id = ?2",
                    params![queue, id],
                )
                .map_err(backend)?;
        }
        let home = if before.original_deck_id == 0 {
            before.deck_id
        } else {
            before.original_deck_id
        };
        let (new_delta, review_delta) = match before.queue {
            CardQueue::New => (-1_i64, 0_i64),
            CardQueue::Review | CardQueue::DayLearning => (0, -1),
            CardQueue::Learning
            | CardQueue::Suspended
            | CardQueue::Buried
            | CardQueue::ManuallyBuried
            | CardQueue::Preview => (0, 0),
        };
        if answer_day == today {
            adjust_deck_counts(
                &transaction,
                home,
                today,
                new_delta,
                review_delta,
            )?;
        }
        transaction.commit().map_err(backend)?;
        Ok(true)
    }

    fn stats(
        &self,
        deck_id: Option<Id>,
        now_secs: i64,
    ) -> Result<DeckStats, StoreError> {
        let filter = self.deck_filter(deck_id)?;
        if filter.missing {
            return Ok(DeckStats::default());
        }
        let today = i64::from(self.timing(now_secs)?.days_elapsed);
        let learn_cutoff =
            now_secs.saturating_add(queue::learn_ahead_secs(&self.conn)?);
        let (deck_and, deck_where) = if filter.params.is_empty() {
            (String::new(), String::new())
        } else {
            let list = placeholders(3, filter.params.len());
            (
                format!("AND {HOME_DECK_SQL} IN ({list})"),
                format!("WHERE {HOME_DECK_SQL} IN ({list})"),
            )
        };
        let sql = format!(
            "SELECT \
             (SELECT COUNT(*) FROM cards c WHERE c.queue = 0 {deck_and}), \
             (SELECT COUNT(*) FROM cards c WHERE ((c.queue = 1 AND {TRUE_DUE_SQL} <= ?1) OR \
              (c.queue = 3 AND {TRUE_DUE_SQL} <= ?2)) {deck_and}), \
             (SELECT COUNT(*) FROM cards c WHERE c.queue = 2 AND {TRUE_DUE_SQL} <= ?2 {deck_and}), \
             (SELECT COUNT(*) FROM cards c {deck_where})"
        );
        let mut parameters =
            vec![SqlValue::Integer(learn_cutoff), SqlValue::Integer(today)];
        parameters.extend(filter.params);
        let mut statement = self.conn.prepare_cached(&sql).map_err(backend)?;
        let counts: (i64, i64, i64, i64) = statement
            .query_row(params_from_iter(parameters), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(backend)?;
        Ok(stats_from_counts((
            Some(counts.0),
            Some(counts.1),
            Some(counts.2),
            counts.3,
        )))
    }

    fn due_forecast(
        &self,
        days: usize,
        now_secs: i64,
    ) -> Result<Vec<usize>, StoreError> {
        let mut counts = vec![0_usize; days];
        if days == 0 {
            return Ok(counts);
        }
        let timing = self.timing(now_secs)?;
        let today = i64::from(timing.days_elapsed);
        let last = i64::try_from(days)
            .map_err(|_| invalid("forecast too long"))?
            .saturating_sub(1);
        let horizon = today
            .checked_add(last)
            .ok_or_else(|| invalid("forecast overflow"))?;
        let learn_cutoff =
            now_secs.saturating_add(queue::learn_ahead_secs(&self.conn)?);
        let sql = format!(
            "SELECT CASE WHEN c.queue IN (1, 4) OR {TRUE_DUE_SQL} < ?1 THEN ?1 ELSE {TRUE_DUE_SQL} END AS due_day, \
             COUNT(*) FROM cards c WHERE (c.queue IN (1, 4) AND {TRUE_DUE_SQL} <= ?2) \
             OR (c.queue IN (2, 3) AND {TRUE_DUE_SQL} <= ?3) GROUP BY due_day"
        );
        let mut statement = self.conn.prepare(&sql).map_err(backend)?;
        let rows = statement
            .query_map(params![today, learn_cutoff, horizon], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(backend)?;
        for row in rows {
            let (day, count) = row.map_err(backend)?;
            let offset = usize::try_from(day.saturating_sub(today))
                .unwrap_or(usize::MAX);
            record_forecast_count(&mut counts, offset, count);
        }
        Ok(counts)
    }

    fn deck_stats(&self, now_secs: i64) -> Result<DeckTotals, StoreError> {
        let decks = self.deck_list()?;
        let today = i64::from(self.timing(now_secs)?.days_elapsed);
        let learn_cutoff =
            now_secs.saturating_add(queue::learn_ahead_secs(&self.conn)?);
        let counts = deck_card_counts(&self.conn, today, learn_cutoff)?;
        let ids_by_name = decks
            .iter()
            .map(|deck| (deck.name.as_str(), deck.id))
            .collect::<HashMap<_, _>>();
        let mut totals: HashMap<Id, DeckStats> = HashMap::new();
        for deck in &decks {
            let subdeck = counts.get(&deck.id).copied().unwrap_or_default();
            let ids = deck_and_ancestor_ids(deck, &ids_by_name);
            add_deck_totals(&mut totals, &ids, subdeck);
        }
        Ok(decks
            .iter()
            .map(|deck| {
                (deck.id, totals.get(&deck.id).copied().unwrap_or_default())
            })
            .collect())
    }
    fn review_history(&self) -> Result<Vec<RevLog>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, cid, usn, ease, ivl, lastIvl, factor, time, type \
                 FROM revlog ORDER BY cid, id",
            )
            .map_err(backend)?;
        let rows = statement
            .query_map([], records::revlog_from_row)
            .map_err(backend)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(backend)
    }

    fn review_count_since(&self, cutoff_ms: i64) -> Result<u32, StoreError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM revlog WHERE id > ?1",
                [cutoff_ms],
                |row| row.get(0),
            )
            .map_err(backend)?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    fn deck_steps(&self, deck_id: Id) -> Result<DeckSteps, StoreError> {
        read_deck_steps(&self.conn, deck_id)
    }

    fn max_taken_secs(&self, deck_id: Id) -> Result<u32, StoreError> {
        let decks = self.effective_decks()?;
        let Some(preset_id) = decks
            .iter()
            .find(|deck| deck.id == deck_id)
            .map(|deck| deck.preset_id)
        else {
            return Ok(60);
        };
        Ok(self
            .preset(preset_id)?
            .map_or(60, |preset| preset.max_taken_secs))
    }

    fn last_review_secs(&self, card_id: Id) -> Result<Option<i64>, StoreError> {
        self.conn
            .query_row(
                "SELECT MAX(id) FROM revlog WHERE cid = ?1 AND ease BETWEEN 1 AND 4 \
                 AND (type != 3 OR factor != 0)",
                [card_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map(|stamp| stamp.map(|millis| millis.div_euclid(1_000)))
            .map_err(backend)
    }

    fn collection_created(&self) -> Result<i64, StoreError> {
        Self::collection_created(self)
    }

    fn timing(&self, now_secs: i64) -> Result<Timing, StoreError> {
        Self::timing(self, now_secs)
    }

    fn learn_ahead_secs(&self) -> Result<i64, StoreError> {
        Self::learn_ahead_secs(self)
    }

    fn collection_modified(&self) -> Result<i64, StoreError> {
        Ok(read_stamps(&self.conn)?.modified)
    }
}

fn read_deck_steps(
    conn: &Connection,
    deck_id: Id,
) -> Result<DeckSteps, StoreError> {
    if let Some(steps) = modern_deck_steps(conn, deck_id)? {
        return Ok(steps);
    }
    if let Some(steps) = legacy_deck_steps(conn, deck_id)? {
        return Ok(steps);
    }
    Ok(DeckSteps::default())
}

#[expect(
    clippy::excessive_nesting,
    clippy::shadow_reuse,
    reason = "deck lookup keeps Anki's metadata fallbacks together"
)]
fn legacy_deck_steps(
    conn: &Connection,
    deck_id: Id,
) -> Result<Option<DeckSteps>, StoreError> {
    let raw: Option<(String, String)> = conn
        .query_row("SELECT decks, dconf FROM col WHERE id = 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
        .map_err(backend)?;
    let Some((decks, dconf)) = raw else {
        return Ok(None);
    };
    let decks: Json = serde_json::from_str(&decks)
        .map_err(|error| invalid(error.to_string()))?;
    let dconf: Json = serde_json::from_str(&dconf)
        .map_err(|error| invalid(error.to_string()))?;
    let Some(deck) = decks.get(deck_id.to_string()).or_else(|| {
        decks.as_object().and_then(|entries| {
            entries.values().find(|entry| {
                entry.get("id").and_then(Json::as_i64) == Some(deck_id)
            })
        })
    }) else {
        return Ok(None);
    };
    let config_id = deck
        .get("conf")
        .and_then(Json::as_i64)
        .filter(|id| *id > 0)
        .unwrap_or(1);
    let Some(config) = dconf.get(config_id.to_string()) else {
        return Ok(None);
    };
    let learning = config
        .get("new")
        .and_then(|new| new.get("delays"))
        .and_then(Json::as_array);
    let relearning = config
        .get("lapse")
        .and_then(|lapse| lapse.get("delays"))
        .and_then(Json::as_array);
    if learning.is_none() && relearning.is_none() {
        return Ok(None);
    }
    Ok(Some(DeckSteps {
        learning: steps_from_json_minutes(learning.unwrap_or(&Vec::new())),
        relearning: steps_from_json_minutes(relearning.unwrap_or(&Vec::new())),
    }))
}

#[expect(
    clippy::shadow_reuse,
    reason = "decoded blobs replace their optional query results"
)]
fn modern_deck_steps(
    conn: &Connection,
    deck_id: Id,
) -> Result<Option<DeckSteps>, StoreError> {
    if !records::table_exists(conn, "decks")?
        || !records::table_exists(conn, "deck_config")?
    {
        return Ok(None);
    }
    let kind: Option<Vec<u8>> = conn
        .query_row("SELECT kind FROM decks WHERE id = ?1", [deck_id], |row| {
            row.get(0)
        })
        .optional()
        .map_err(backend)?;
    let Some(kind) = kind else {
        return Ok(None);
    };
    let Some(config_id) = anki::deck_kind_config_id(&kind) else {
        return Ok(None);
    };
    let config: Option<Vec<u8>> = conn
        .query_row(
            "SELECT config FROM deck_config WHERE id = ?1",
            [config_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend)?;
    let Some(config) = config else {
        return Ok(None);
    };
    Ok(Some(DeckSteps {
        learning: steps_from_float_minutes(&anki::protobuf_floats(&config, 1)),
        relearning: steps_from_float_minutes(&anki::protobuf_floats(
            &config, 2,
        )),
    }))
}

fn steps_from_json_minutes(values: &[Json]) -> Vec<u32> {
    values
        .iter()
        .filter_map(Json::as_f64)
        .filter_map(minutes_to_seconds)
        .collect()
}

fn steps_from_float_minutes(values: &[f32]) -> Vec<u32> {
    values
        .iter()
        .copied()
        .map(f64::from)
        .filter_map(minutes_to_seconds)
        .collect()
}

fn minutes_to_seconds(minutes: f64) -> Option<u32> {
    let seconds = (minutes * 60.0).round();
    if !(1.0..=f64::from(u32::MAX)).contains(&seconds) {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "seconds are checked against the u32 range"
    )]
    let value = seconds as u32;
    Some(value)
}

fn configure_connection(conn: &Connection) -> Result<(), StoreError> {
    // Anki expects WAL. NORMAL avoids an fsync on every grade.
    let journal: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(backend)?;
    if !journal.eq_ignore_ascii_case("wal") {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(backend)?;
    }
    let synchronous: i64 = conn
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(backend)?;
    if synchronous != 1 {
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(backend)?;
    }
    Ok(())
}

/// Registers the case-insensitive collation used by schema-18 table indexes.
///
/// # Errors
///
/// Returns an error when SQLite cannot install the collation.
pub fn register_collations(conn: &Connection) -> Result<(), StoreError> {
    conn.create_collation("unicase", |left, right| {
        unicase::UniCase::new(left).cmp(&unicase::UniCase::new(right))
    })
    .map_err(backend)
}

/// Registers Anki's `fnvhash` SQL function, which orders cards that share a
/// due day. Anki hashes each integer argument's little-endian bytes.
fn register_functions(conn: &Connection) -> Result<(), StoreError> {
    conn.create_scalar_function(
        "fnvhash",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let id: i64 = ctx.get(0)?;
            let modified: i64 = ctx.get(1)?;
            Ok(fnv_hash(id, modified))
        },
    )
    .map_err(backend)
}

/// FNV-1a over both values, compared as Anki's signed `i64`.
pub(crate) fn fnv_hash(first: i64, second: i64) -> i64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for value in [first, second] {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    i64::from_ne_bytes(hash.to_ne_bytes())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the duplicate check needs both checksums, both field texts, and scope inputs"
)]
fn duplicate_exists(
    conn: &Connection,
    model_id: Id,
    checksum: i32,
    legacy_sum: i32,
    first_field: &str,
    legacy_first: &str,
    normalize_text: bool,
    scope: &DuplicateScope,
) -> Result<bool, StoreError> {
    let mut sql = String::from(
        "SELECT id, flds FROM notes WHERE (csum = ?1 OR csum = ?2)",
    );
    let mut parameters = vec![
        SqlValue::Integer(anki::csum_value(checksum)),
        SqlValue::Integer(anki::csum_value(legacy_sum)),
    ];
    if !scope.check_all_models {
        let _ = write!(sql, " AND mid = ?3");
        parameters.push(SqlValue::Integer(model_id));
    }

    let allowed_decks = if scope.use_deck {
        let Some(deck_id) = scope.deck_id else {
            return Ok(false);
        };
        if scope.check_children {
            let decks = deck_list_from(conn)?;
            let Some(name) = decks.iter().find(|deck| deck.id == deck_id)
            else {
                return Ok(false);
            };
            deck_subtree_ids(&decks, &name.name)
        } else {
            vec![deck_id]
        }
    } else {
        Vec::new()
    };

    let mut statement = conn.prepare(&sql).map_err(backend)?;
    let rows = statement
        .query_map(params_from_iter(parameters), |row| {
            Ok((row.get::<_, Id>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(backend)?;
    for row in rows {
        let (id, fields) = row.map_err(backend)?;
        let note_fields = anki::decode_fields(&fields);
        let other = note_fields.first().map_or("", String::as_str);
        let current = anki::strip_html_preserving_media_filenames_with(
            other,
            normalize_text,
        );
        if current != first_field
            && anki::legacy_field_text(other) != legacy_first
        {
            continue;
        }
        if scope.use_deck && !note_has_card_in(conn, id, &allowed_decks)? {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

fn note_has_card_in(
    conn: &Connection,
    note_id: Id,
    deck_ids: &[Id],
) -> Result<bool, StoreError> {
    let mut statement = conn
        .prepare("SELECT did FROM cards WHERE nid = ?1")
        .map_err(backend)?;
    let rows = statement
        .query_map([note_id], |row| row.get::<_, Id>(0))
        .map_err(backend)?;
    for row in rows {
        if deck_ids.contains(&row.map_err(backend)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn backend(error: impl Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

pub(crate) fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidData(message.into())
}

struct DeckFilter {
    params: Vec<SqlValue>,
    missing: bool,
}

type Counts = (Option<i64>, Option<i64>, Option<i64>, i64);

type DeckRenames = Vec<(Id, String)>;
type CardsAndNotes = (Vec<Id>, Vec<Id>);

struct ColStamp {
    usn: i32,
    modified: i64,
    scm: i64,
    now_secs: i64,
}

struct Graves {
    cards: Vec<Id>,
    notes: Vec<Id>,
    decks: Vec<Id>,
}

fn fresh_id(conn: &Connection, table: &str) -> Result<Id, StoreError> {
    let highest: Option<Id> = conn
        .query_row(&format!("SELECT MAX(id) FROM {table}"), [], |row| {
            row.get(0)
        })
        .map_err(backend)?;
    let mut id = anki::next_id()?;
    if highest.is_some_and(|maximum| id <= maximum) {
        id = highest.unwrap_or(id).saturating_add(1);
    }
    loop {
        let exists: bool = conn
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"),
                [id],
                |row| row.get(0),
            )
            .map_err(backend)?;
        if !exists {
            return Ok(id);
        }
        id = id.saturating_add(1);
    }
}

fn fresh_revlog_id(conn: &Connection, preferred: Id) -> Result<Id, StoreError> {
    let highest: Option<Id> = conn
        .query_row("SELECT MAX(id) FROM revlog", [], |row| row.get(0))
        .map_err(backend)?;
    Ok(highest.map_or(preferred, |maximum| {
        preferred.max(maximum.saturating_add(1))
    }))
}

fn fresh_model_id(
    conn: &Connection,
    existing: &[NoteModel],
) -> Result<Id, StoreError> {
    let mut id = fresh_id(conn, "notes")?;
    while existing.iter().any(|entry| entry.id == id) {
        id = id.saturating_add(1);
    }
    Ok(id)
}

/// Adds one model to `col.models` JSON and returns the new document.
fn models_json_installing(
    json: &str,
    model: &NoteModel,
) -> Result<String, StoreError> {
    let mut object: Map<String, Json> =
        serde_json::from_str(json).unwrap_or_default();
    if let Some(entry) = models_to_json(slice::from_ref(model)).as_object() {
        object.extend(entry.clone());
    }
    serde_json::to_string(&Json::Object(object))
        .map_err(|error| invalid(error.to_string()))
}

/// True when the collection keeps its note types in schema-18 tables.
fn models_in_tables(
    conn: &Connection,
    col: &records::ColData,
) -> Result<bool, StoreError> {
    if !records::table_exists(conn, "notetypes")? {
        return Ok(false);
    }
    Ok(col.ver >= 18)
}

fn column_exists(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, StoreError> {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(backend)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(backend)?;
    for row in rows {
        if row.map_err(backend)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Writes one note type into the schema-18 normalized tables.
fn insert_model_tables(
    conn: &Connection,
    model: &NoteModel,
    stamp: &ColStamp,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO notetypes (id, name, mtime_secs, usn, config) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            model.id,
            model.name,
            stamp.now_secs,
            stamp.usn,
            records::notetype_config_for(model),
        ],
    )
    .map_err(backend)?;
    for (index, name) in model.fields.iter().enumerate() {
        let ord = i64::try_from(index).unwrap_or_default();
        let tag = model.field_tags.get(index).copied().flatten();
        conn.execute(
            "INSERT INTO fields (ntid, ord, name, config) VALUES (?1, ?2, ?3, ?4)",
            params![model.id, ord, name, records::field_config_for(model, index, tag)],
        )
        .map_err(backend)?;
    }
    let stamped = column_exists(conn, "templates", "mtime_secs")?;
    for (position, template) in model.templates.iter().enumerate() {
        let ord = i64::from(template.ord);
        let config = records::template_config_for(
            model,
            position,
            &template.question_format,
            &template.answer_format,
        );
        if stamped {
            conn.execute(
                "INSERT INTO templates (ntid, ord, name, mtime_secs, usn, config) VALUES (?1, \
                 ?2, ?3, ?4, ?5, ?6)",
                params![
                    model.id,
                    ord,
                    template.name,
                    stamp.now_secs,
                    stamp.usn,
                    config
                ],
            )
            .map_err(backend)?;
        } else {
            conn.execute(
                "INSERT INTO templates (ntid, ord, name, config) VALUES (?1, ?2, ?3, ?4)",
                params![model.id, ord, template.name, config],
            )
            .map_err(backend)?;
        }
    }
    Ok(())
}

/// Writes a note type into normalized tables with explicit import stamps.
///
/// # Errors
///
/// Returns an error when a normalized row cannot be written.
#[expect(
    clippy::too_many_arguments,
    reason = "import passes explicit connection, note type, and stamps"
)]
pub fn insert_model_tables_for_import(
    conn: &Connection,
    model: &NoteModel,
    mtime_secs: i64,
    usn: i32,
) -> Result<(), StoreError> {
    let stamp = ColStamp {
        usn,
        modified: mtime_secs,
        scm: mtime_secs,
        now_secs: mtime_secs,
    };
    insert_model_tables(conn, model, &stamp)
}

fn placeholders(start: i64, count: usize) -> String {
    (0..count)
        .map(|index| {
            let number =
                start.saturating_add(i64::try_from(index).unwrap_or_default());
            format!("?{number}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn deck_list_from(conn: &Connection) -> Result<Vec<Deck>, StoreError> {
    let col = records::read_col(conn)?;
    if uses_deck_table(conn, &col)? {
        return records::decks_from_tables(conn);
    }
    records::decks_from_json(&col.decks)
}

/// Counts cards due at `now_secs` per deck id in one grouped query.
fn deck_card_counts(
    conn: &Connection,
    today: i64,
    learn_cutoff: i64,
) -> Result<HashMap<Id, DeckStats>, StoreError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {HOME_DECK_SQL}, SUM(CASE WHEN c.queue = 0 THEN 1 ELSE 0 END), \
             SUM(CASE WHEN (c.queue = 1 AND {TRUE_DUE_SQL} <= ?1) OR (c.queue = 3 AND {TRUE_DUE_SQL} <= ?2) THEN 1 ELSE 0 END), \
             SUM(CASE WHEN c.queue = 2 AND {TRUE_DUE_SQL} <= ?2 THEN 1 ELSE 0 END), COUNT(*) FROM cards c GROUP BY \
             {HOME_DECK_SQL}",
        ))
        .map_err(backend)?;
    let rows = statement
        .query_map(params![learn_cutoff, today], |row| {
            Ok((
                row.get::<_, Id>(0)?,
                (row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?),
            ))
        })
        .map_err(backend)?;
    let mut counts = HashMap::new();
    for row in rows {
        let (id, tally) = row.map_err(backend)?;
        let _ = counts.insert(id, stats_from_counts(tally));
    }
    Ok(counts)
}

/// Adds one deck's counts to the deck and each id in `ids`.
fn add_deck_totals(
    totals: &mut HashMap<Id, DeckStats>,
    ids: &[Id],
    subdeck: DeckStats,
) {
    for id in ids {
        add_deck_stats(totals.entry(*id).or_default(), subdeck);
    }
}

/// Ids of a deck followed by each ancestor deck that is named in the map.
fn deck_and_ancestor_ids(
    deck: &Deck,
    ids_by_name: &HashMap<&str, Id>,
) -> Vec<Id> {
    let mut ids = vec![deck.id];
    for (index, _) in deck.name.char_indices() {
        if !deck
            .name
            .get(index..)
            .is_some_and(|tail| tail.starts_with("::"))
        {
            continue;
        }
        let Some(&ancestor) = ids_by_name.get(&deck.name[..index]) else {
            continue;
        };
        ids.push(ancestor);
    }
    ids
}

const fn add_deck_stats(total: &mut DeckStats, counts: DeckStats) {
    total.new_cards = total.new_cards.saturating_add(counts.new_cards);
    total.learning_cards =
        total.learning_cards.saturating_add(counts.learning_cards);
    total.review_cards = total.review_cards.saturating_add(counts.review_cards);
    total.total_cards = total.total_cards.saturating_add(counts.total_cards);
}
fn record_forecast_count(counts: &mut [usize], offset: usize, count: i64) {
    if let Some(slot) = counts.get_mut(offset) {
        *slot = usize::try_from(count).unwrap_or(usize::MAX);
    }
}

fn stats_from_counts(counts: Counts) -> DeckStats {
    DeckStats {
        new_cards: usize::try_from(counts.0.unwrap_or_default())
            .unwrap_or_default(),
        learning_cards: usize::try_from(counts.1.unwrap_or_default())
            .unwrap_or_default(),
        review_cards: usize::try_from(counts.2.unwrap_or_default())
            .unwrap_or_default(),
        total_cards: usize::try_from(counts.3).unwrap_or_default(),
    }
}

/// Ids of a deck and its subdecks, in deck-list order.
pub(crate) fn deck_subtree_ids(decks: &[Deck], root_name: &str) -> Vec<Id> {
    let Some(root) = decks.iter().find(|deck| deck.name == root_name) else {
        return Vec::new();
    };
    let prefix = format!("{}::", root.name);
    decks
        .iter()
        .filter(|deck| deck.id == root.id || deck.name.starts_with(&prefix))
        .map(|deck| deck.id)
        .collect()
}

/// Names of every deck needed for `name`, with the id of existing decks.
///
/// Existing ancestors keep their stored spelling, so `alpha::Beta` joins an
/// existing `Alpha` instead of creating a second parent.
fn deck_creation_plan(decks: &[Deck], name: &str) -> Vec<(String, Option<Id>)> {
    let mut canonical = String::new();
    let mut plan = Vec::new();
    for part in name.split("::") {
        let candidate = if canonical.is_empty() {
            part.to_owned()
        } else {
            format!("{canonical}::{part}")
        };
        if let Some(existing) = decks
            .iter()
            .find(|deck| deck.name.eq_ignore_ascii_case(&candidate))
        {
            canonical.clone_from(&existing.name);
            plan.push((canonical.clone(), Some(existing.id)));
        } else {
            canonical = candidate;
            plan.push((canonical.clone(), None));
        }
    }
    plan
}

fn validate_deck_name(name: &str) -> Result<&str, StoreError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.split("::").any(|part| part.trim().is_empty())
    {
        Err(invalid("invalid deck name"))
    } else {
        Ok(trimmed)
    }
}

fn uses_deck_table(
    conn: &Connection,
    col: &records::ColData,
) -> Result<bool, StoreError> {
    if !records::table_exists(conn, "decks")? {
        return Ok(false);
    }
    Ok(col.ver >= 18)
}

/// Keeps a stamp at least one past `previous` so writes in the same
/// millisecond stay ordered. Saturation at [`i64::MAX`] keeps a collection
/// writable at the end of the range instead of panicking.
fn next_stamp(previous: i64, now: i64) -> i64 {
    now.max(previous.saturating_add(1))
}

fn col_stamp(col: &records::ColData) -> Result<ColStamp, StoreError> {
    stamp_from(&Stamps {
        modified: col.modified,
        scm: col.scm,
    })
}

fn stamp_from(stamps: &Stamps) -> Result<ColStamp, StoreError> {
    let now = anki::now_ms()?;
    Ok(ColStamp {
        usn: -1,
        modified: next_stamp(stamps.modified, now),
        scm: next_stamp(stamps.scm, now),
        now_secs: anki::now_secs()?,
    })
}

fn write_col_metadata(
    conn: &Connection,
    stamp: &ColStamp,
    decks_json: Option<&str>,
) -> Result<(), StoreError> {
    if let Some(decks) = decks_json {
        conn.execute(
            "UPDATE col SET mod = ?1, scm = ?2, decks = ?3 WHERE id = 1",
            params![stamp.modified, stamp.scm, decks],
        )
        .map_err(backend)?;
    } else {
        conn.execute(
            "UPDATE col SET mod = ?1, scm = ?2 WHERE id = 1",
            params![stamp.modified, stamp.scm],
        )
        .map_err(backend)?;
    }
    Ok(())
}

fn sql_params(ids: &[Id]) -> Vec<SqlValue> {
    ids.iter().copied().map(SqlValue::Integer).collect()
}

fn cards_in_decks(
    conn: &Connection,
    deck_ids: &[Id],
) -> Result<CardsAndNotes, StoreError> {
    if deck_ids.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let list = placeholders(1, deck_ids.len());
    let mut statement = conn
        .prepare(&format!("SELECT id, nid FROM cards WHERE did IN ({list})"))
        .map_err(backend)?;
    let rows = statement
        .query_map(params_from_iter(sql_params(deck_ids)), |row| {
            Ok((row.get::<_, Id>(0)?, row.get::<_, Id>(1)?))
        })
        .map_err(backend)?;
    let mut cards = Vec::new();
    let mut notes = Vec::new();
    for row in rows {
        let (card_id, note_id) = row.map_err(backend)?;
        cards.push(card_id);
        if !notes.contains(&note_id) {
            notes.push(note_id);
        }
    }
    Ok((cards, notes))
}

fn delete_cards(conn: &Connection, card_ids: &[Id]) -> Result<(), StoreError> {
    if card_ids.is_empty() {
        return Ok(());
    }
    let list = placeholders(1, card_ids.len());
    conn.execute(
        &format!("DELETE FROM revlog WHERE cid IN ({list})"),
        params_from_iter(sql_params(card_ids)),
    )
    .map_err(backend)?;
    conn.execute(
        &format!("DELETE FROM cards WHERE id IN ({list})"),
        params_from_iter(sql_params(card_ids)),
    )
    .map_err(backend)?;
    Ok(())
}

fn delete_orphan_notes(
    conn: &Connection,
    note_ids: &[Id],
) -> Result<Vec<Id>, StoreError> {
    let mut deleted = Vec::new();
    for note_id in note_ids {
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM cards WHERE nid = ?1",
                [*note_id],
                |row| row.get(0),
            )
            .map_err(backend)?;
        if remaining == 0 {
            conn.execute("DELETE FROM notes WHERE id = ?1", [*note_id])
                .map_err(backend)?;
            deleted.push(*note_id);
        }
    }
    Ok(deleted)
}

fn reset_original_decks(
    conn: &Connection,
    deck_ids: &[Id],
) -> Result<(), StoreError> {
    if deck_ids.is_empty() {
        return Ok(());
    }
    let list = placeholders(1, deck_ids.len());
    conn.execute(
        &format!("UPDATE cards SET odid = 0 WHERE odid IN ({list})"),
        params_from_iter(sql_params(deck_ids)),
    )
    .map(|_| ())
    .map_err(backend)
}

fn delete_from_deck_table(
    conn: &Connection,
    deck_ids: &[Id],
) -> Result<(), StoreError> {
    if deck_ids.is_empty() {
        return Ok(());
    }
    let list = placeholders(1, deck_ids.len());
    conn.execute(
        &format!("DELETE FROM decks WHERE id IN ({list})"),
        params_from_iter(sql_params(deck_ids)),
    )
    .map(|_| ())
    .map_err(backend)
}

fn decks_json_without(json: &str, ids: &[Id]) -> Result<String, StoreError> {
    let mut value: Json = serde_json::from_str(json)
        .map_err(|error| invalid(error.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("decks is not an object"))?;
    for id in ids {
        object.remove(&id.to_string());
    }
    serde_json::to_string(&value).map_err(|error| invalid(error.to_string()))
}

fn renamed_decks_json(
    json: &str,
    changes: &DeckRenames,
    stamp: &ColStamp,
) -> Result<String, StoreError> {
    let mut value: Json = serde_json::from_str(json)
        .map_err(|error| invalid(error.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("decks is not an object"))?;
    for (deck_id, deck_name) in changes {
        let key = deck_id.to_string();
        let entry = object.get_mut(&key).ok_or_else(|| {
            StoreError::NotFound(format!(
                "deck {deck_id} missing from col.decks"
            ))
        })?;
        let fields = entry
            .as_object_mut()
            .ok_or_else(|| invalid("deck entry is not an object"))?;
        fields.insert("name".into(), json!(deck_name));
        fields.insert("usn".into(), json!(stamp.usn));
        fields.insert("mod".into(), json!(stamp.now_secs));
    }
    serde_json::to_string(&value).map_err(|error| invalid(error.to_string()))
}

fn rename_in_deck_table(
    conn: &Connection,
    changes: &DeckRenames,
    stamp: &ColStamp,
) -> Result<(), StoreError> {
    for (deck_id, deck_name) in changes {
        let updated = conn
            .execute(
                "UPDATE decks SET name = ?1, mtime_secs = ?2, usn = ?3 WHERE id = ?4",
                params![
                    anki::encode_deck_name(deck_name),
                    stamp.now_secs,
                    stamp.usn,
                    deck_id,
                ],
            )
            .map_err(backend)?;
        if updated == 0 {
            return Err(StoreError::NotFound(format!("deck {deck_id}")));
        }
    }
    Ok(())
}

fn rename_plan(
    decks: &[Deck],
    id: Id,
    new_name: &str,
) -> Result<DeckRenames, StoreError> {
    let root = decks
        .iter()
        .find(|deck| deck.id == id)
        .ok_or_else(|| StoreError::NotFound(format!("deck {id}")))?;
    let prefix = format!("{}::", root.name);
    let mut changes = Vec::with_capacity(decks.len());
    for deck in decks {
        if deck.id == id {
            changes.push((deck.id, new_name.to_owned()));
        } else if let Some(suffix) = deck.name.strip_prefix(&prefix) {
            changes.push((deck.id, format!("{new_name}::{suffix}")));
        }
    }
    let mut seen: HashSet<String> = HashSet::with_capacity(decks.len());
    for deck in decks {
        let name = changes
            .iter()
            .find(|(changed_id, _)| *changed_id == deck.id)
            .map_or(deck.name.as_str(), |(_, name)| name.as_str());
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(invalid(format!("deck already exists: {name}")));
        }
    }
    Ok(changes)
}

fn write_graves(
    conn: &Connection,
    stamp: &ColStamp,
    graves: &Graves,
) -> Result<(), StoreError> {
    for (kind, ids) in [
        (0_i64, graves.cards.as_slice()),
        (1_i64, graves.notes.as_slice()),
        (2_i64, graves.decks.as_slice()),
    ] {
        for id in ids {
            conn.execute(
                "DELETE FROM graves WHERE oid = ?1 AND type = ?2",
                params![id, kind],
            )
            .map_err(backend)?;
            conn.execute(
                "INSERT INTO graves (oid, type, usn) VALUES (?1, ?2, ?3)",
                params![id, kind, stamp.usn],
            )
            .map_err(backend)?;
        }
    }
    Ok(())
}
fn update_col(conn: &Connection, bump_scm: bool) -> Result<i32, StoreError> {
    let stamps = read_stamps(conn)?;
    let stamp = stamp_from(&stamps)?;
    let scm = if bump_scm { stamp.scm } else { stamps.scm };
    conn.execute(
        "UPDATE col SET mod = ?1, scm = ?2 WHERE id = 1",
        params![stamp.modified, scm],
    )
    .map_err(backend)?;
    Ok(stamp.usn)
}

/// Collection stamps that every write bumps.
struct Stamps {
    modified: i64,
    scm: i64,
}

/// Reads the stamps without decoding the note types, decks, and configuration
/// JSON that the same row carries.
fn read_stamps(conn: &Connection) -> Result<Stamps, StoreError> {
    conn.query_row("SELECT mod, scm FROM col WHERE id = 1", [], |row| {
        Ok(Stamps {
            modified: row.get(0)?,
            scm: row.get(1)?,
        })
    })
    .map_err(backend)
}

#[expect(
    clippy::too_many_arguments,
    reason = "counter updates need the deck, day and both signed deltas"
)]
/// Adds a signed delta to a deck's today counters, and to each parent deck.
fn adjust_deck_counts(
    conn: &Connection,
    deck_id: Id,
    today: u32,
    new_delta: i64,
    review_delta: i64,
) -> Result<(), StoreError> {
    let mut decks = options::effective_decks(conn)?;
    let schema_modified = read_stamps(conn)?.scm;
    let Some(root) = decks.iter().find(|deck| deck.id == deck_id).cloned()
    else {
        return Ok(());
    };
    let mut affected = vec![root.id];
    let mut parent = root.name.rsplit_once("::").map(|entry| entry.0);
    while let Some(name) = parent {
        let Some(id) = decks
            .iter()
            .find(|deck| deck.name == name)
            .map(|deck| deck.id)
        else {
            break;
        };
        affected.push(id);
        parent = decks
            .iter()
            .find(|deck| deck.id == id)
            .and_then(|deck| deck.name.rsplit_once("::"))
            .map(|entry| entry.0);
    }
    for deck in &mut decks {
        if !affected.contains(&deck.id) {
            continue;
        }
        if deck.common.last_day_studied != today {
            deck.common.new_studied = 0;
            deck.common.review_studied = 0;
            deck.common.learning_studied = 0;
            deck.common.milliseconds_studied = 0;
            deck.common.last_day_studied = today;
            deck.overrides.new_today.today = 0;
            deck.overrides.review_today.today = 0;
        }
        let new_count = i64::from(deck.common.new_studied)
            .saturating_add(new_delta)
            .clamp(0, i64::from(i32::MAX));
        let review_count = i64::from(deck.common.review_studied)
            .saturating_add(review_delta)
            .clamp(0, i64::from(i32::MAX));
        deck.common.new_studied = i32::try_from(new_count).unwrap_or(i32::MAX);
        deck.common.review_studied =
            i32::try_from(review_count).unwrap_or(i32::MAX);
        deck.overrides.new_today.today = u32::try_from(new_count).unwrap_or(0);
        deck.overrides.review_today.today =
            u32::try_from(review_count).unwrap_or(0);
        options::write_deck(conn, deck)?;
    }
    conn.execute("UPDATE col SET scm = ?1 WHERE id = 1", [schema_modified])
        .map_err(backend)?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "burial needs the connection, decision, source queue and stamp"
)]
fn bury_siblings_after_answer(
    conn: &Connection,
    decision: &ReviewDecision,
    from_queue: CardQueue,
    usn: i32,
) -> Result<(), StoreError> {
    let home = if decision.card.original_deck_id == 0 {
        decision.card.deck_id
    } else {
        decision.card.original_deck_id
    };
    let home_preset = options::effective_decks(conn)?
        .into_iter()
        .find(|deck| deck.id == home)
        .map(|deck| deck.preset_id);
    let Some(preset_id) = home_preset else {
        return Ok(());
    };
    let Some(preset) = options::presets(conn)?
        .into_iter()
        .find(|preset| preset.id == preset_id)
    else {
        return Ok(());
    };
    let order = match from_queue {
        CardQueue::Learning | CardQueue::Preview => 0,
        CardQueue::DayLearning => 1,
        CardQueue::Review => 2,
        CardQueue::New => 3,
        CardQueue::Suspended
        | CardQueue::Buried
        | CardQueue::ManuallyBuried => {
            return Ok(());
        }
    };
    let mut queues = Vec::new();
    if preset.bury_new {
        queues.push((0_i64, 0_i64));
    }
    if preset.bury_reviews && order <= 2 {
        queues.push((2, 2));
    }
    if preset.bury_interday_learning && order <= 1 {
        queues.push((1, 3));
        queues.push((3, 3));
    }
    for (card_type, queue) in queues {
        conn.execute(
            "UPDATE cards SET queue = -2, mod = ?5, usn = ?6 WHERE nid = ?1 AND id != ?2 AND type = ?3 AND queue = ?4",
            params![
                decision.card.note_id,
                decision.card.id,
                card_type,
                queue,
                decision.card.modified,
                usn
            ],
            )
        .map_err(backend)?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "a review write needs the transaction, stamp, decision and day"
)]
/// Writes one scheduled card and its review log entry inside `transaction`.
fn write_review(
    transaction: &Transaction<'_>,
    usn: i32,
    decision: &ReviewDecision,
    today: u32,
) -> Result<Id, StoreError> {
    let card = &decision.card;
    let previous: Option<(i64, i64, i64)> = transaction
        .query_row(
            "SELECT queue, did, odid FROM cards WHERE id = ?1",
            [card.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(backend)?;
    let (from_queue, home) = previous.map_or(
        (CardQueue::New, card.deck_id),
        |(queue, did, odid)| {
            (
                CardQueue::from_code(queue),
                if odid == 0 { did } else { odid },
            )
        },
    );
    let updated = transaction
        .execute(
            "UPDATE cards SET did = ?1, mod = ?2, usn = ?3, type = ?4, queue = ?5, due = ?6, \
             ivl = ?7, factor = ?8, reps = ?9, lapses = ?10, left = ?11, odue = ?12, odid = \
             ?13, flags = ?14, data = ?15 WHERE id = ?16",
            params![
                card.deck_id,
                card.modified,
                usn,
                card.card_type.code(),
                card.queue.code(),
                card.due,
                card.interval,
                card.factor,
                card.reps,
                card.lapses,
                card.left,
                card.original_due,
                card.original_deck_id,
                card.flags,
                card.data,
                card.id,
            ],
        )
        .map_err(backend)?;
    if updated == 0 {
        return Err(StoreError::NotFound(format!("card {}", card.id)));
    }
    let log = &decision.revlog;
    let log_id = fresh_revlog_id(transaction, log.id)?;
    transaction
        .execute(
            "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                log_id,
                card.id,
                usn,
                log.ease,
                log.interval,
                log.last_interval,
                log.factor,
                log.time_ms,
                log.kind.code(),
            ],
        )
        .map_err(backend)?;
    let (new_delta, review_delta) = match from_queue {
        CardQueue::New => (1_i64, 0_i64),
        CardQueue::Review | CardQueue::DayLearning => (0, 1),
        CardQueue::Learning
        | CardQueue::Suspended
        | CardQueue::Buried
        | CardQueue::ManuallyBuried
        | CardQueue::Preview => (0, 0),
    };
    adjust_deck_counts(transaction, home, today, new_delta, review_delta)?;
    bury_siblings_after_answer(transaction, decision, from_queue, usn)?;
    Ok(log_id)
}

/// Reads the next new-card position.
///
/// Schema-18 collections keep configuration in the `config` table and leave
/// `col.conf` empty, so both paths are supported.
fn next_position(conn: &Connection) -> Result<i64, StoreError> {
    if records::table_exists(conn, "config")? {
        let stored: Option<SqlValue> = conn
            .query_row(
                "SELECT val FROM config WHERE key = 'nextPos'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        let position = match stored {
            Some(SqlValue::Blob(bytes)) => parse_position(&bytes),
            Some(SqlValue::Text(text)) => text.parse().ok(),
            Some(SqlValue::Integer(value)) => Some(value),
            _ => None,
        };
        return Ok(position.unwrap_or(1));
    }
    let col = records::read_col(conn)?;
    let config = parse_conf(&col.conf)?;
    Ok(config.get("nextPos").and_then(Json::as_i64).unwrap_or(1))
}

fn set_next_position(
    transaction: &Transaction<'_>,
    position: i64,
) -> Result<(), StoreError> {
    if records::table_exists(transaction, "config")? {
        return transaction
            .execute(
                "INSERT INTO config (key, usn, mtime_secs, val) \
                 VALUES ('nextPos', -1, ?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET val = excluded.val, \
                 mtime_secs = excluded.mtime_secs, usn = excluded.usn",
                params![anki::now_secs()?, position.to_string().into_bytes()],
            )
            .map(|_| ())
            .map_err(backend);
    }
    let col = records::read_col(transaction)?;
    let mut config = parse_conf(&col.conf)?;
    let object = config
        .as_object_mut()
        .ok_or_else(|| invalid("conf is not an object"))?;
    object.insert("nextPos".into(), json!(position));
    let config_json = serde_json::to_string(&config)
        .map_err(|error| invalid(error.to_string()))?;
    transaction
        .execute("UPDATE col SET conf = ?1 WHERE id = 1", [config_json])
        .map(|_| ())
        .map_err(backend)
}

/// Parses `col.conf`, treating an empty blob as an empty object.
pub(crate) fn parse_conf(conf: &str) -> Result<Json, StoreError> {
    if conf.trim().is_empty() {
        return Ok(Json::Object(serde_json::Map::new()));
    }
    serde_json::from_str(conf).map_err(|error| invalid(error.to_string()))
}

/// Anki's `normalize_note_text` preference, enabled by default.
fn normalize_note_text(conn: &Connection) -> Result<bool, StoreError> {
    if records::table_exists(conn, "config")? {
        let raw: Option<Vec<u8>> = conn
            .query_row(
                "SELECT val FROM config WHERE key = 'normalize_note_text'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend)?;
        return Ok(raw
            .and_then(|value| serde_json::from_slice(&value).ok())
            .unwrap_or(true));
    }
    let col = records::read_col(conn)?;
    let config = parse_conf(&col.conf)?;
    Ok(config
        .get("normalize_note_text")
        .and_then(Json::as_bool)
        .unwrap_or(true))
}
/// Reads a decimal position stored as UTF-8 bytes in the config table.
fn parse_position(bytes: &[u8]) -> Option<i64> {
    str::from_utf8(bytes).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use ranki_model::NewNote;
    use ranki_model::RevLog;
    use ranki_model::RevLogKind;
    use ranki_model::ReviewDecision;
    use ranki_model::Store;

    use super::Collection;

    #[test]
    fn review_history_returns_logs_in_card_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let mut collection = Collection::create(&path).unwrap();
        let ids = collection
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![String::from("front"), String::from("back")],
                tags: Vec::new(),
            })
            .unwrap();
        let card = collection.card(ids[0]).unwrap().unwrap();
        let log = RevLog {
            id: 1_700_000_000_000,
            card_id: card.id,
            usn: 0,
            ease: 3,
            interval: 1,
            last_interval: 0,
            factor: 2_500,
            time_ms: 100,
            kind: RevLogKind::Review,
        };
        collection
            .apply_review(&ReviewDecision {
                card: card.clone(),
                expected_modified: card.modified,
                expected_usn: card.usn,
                revlog: log.clone(),
            })
            .unwrap();
        let history = collection.review_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, log.id);
        assert_eq!(history[0].card_id, log.card_id);
        assert_eq!(history[0].kind, RevLogKind::Review);
    }

    #[test]
    fn update_note_keeps_cards_and_moves_them_to_the_chosen_deck() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let mut collection = Collection::create(&path).unwrap();
        let ids = collection
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![String::from("front"), String::from("back")],
                tags: Vec::new(),
            })
            .unwrap();
        let card = collection.card(ids[0]).unwrap().unwrap();
        let mut note = collection.note(card.note_id).unwrap().unwrap();
        note.fields = vec![String::from("new front"), String::from("new back")];
        note.tags = vec![String::from("edited")];
        let other = collection.create_deck("Other").unwrap();

        collection.update_note(&note, other.id).unwrap();

        let saved = collection.note(note.id).unwrap().unwrap();
        assert_eq!(saved.fields, note.fields);
        assert_eq!(saved.tags, note.tags);
        let saved_card = collection.card(card.id).unwrap().unwrap();
        assert_eq!(saved_card.deck_id, other.id);
        assert_eq!(saved_card.reps, card.reps);
    }

    #[test]
    fn remove_card_deletes_its_log_and_orphan_note() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let mut collection = Collection::create(&path).unwrap();
        let ids = collection
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![String::from("front"), String::from("back")],
                tags: Vec::new(),
            })
            .unwrap();
        let card = collection.card(ids[0]).unwrap().unwrap();
        let note_id = card.note_id;
        let log = RevLog {
            id: 1_700_000_000_000,
            card_id: card.id,
            usn: 0,
            ease: 3,
            interval: 1,
            last_interval: 0,
            factor: 2_500,
            time_ms: 100,
            kind: RevLogKind::Review,
        };
        collection
            .apply_review(&ReviewDecision {
                card: card.clone(),
                expected_modified: card.modified,
                expected_usn: card.usn,
                revlog: log,
            })
            .unwrap();

        collection.remove_card(card.id).unwrap();

        assert!(collection.card(card.id).unwrap().is_none());
        assert!(collection.note(note_id).unwrap().is_none());
        assert_eq!(collection.review_history().unwrap().len(), 0);
        let grave: i64 = collection
            .conn
            .query_row(
                "SELECT COUNT(*) FROM graves WHERE oid = ?1 AND type = 0",
                [card.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(grave, 1);
    }

    #[test]
    fn removing_one_cloze_card_keeps_the_shared_note() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let mut collection = Collection::create(&path).unwrap();
        let model = collection
            .ensure_model(ranki_model::StockModelKind::Cloze)
            .unwrap();
        let ids = collection
            .add_note(&NewNote {
                model_id: model.id,
                deck_id: 1,
                fields: vec![
                    String::from("{{c1::one}} {{c2::two}}"),
                    String::new(),
                ],
                tags: Vec::new(),
            })
            .unwrap();
        assert_eq!(ids.len(), 2);

        collection.remove_card(ids[0]).unwrap();

        assert!(collection.card(ids[0]).unwrap().is_none());
        assert!(collection.card(ids[1]).unwrap().is_some());
        let card = collection.card(ids[1]).unwrap().unwrap();
        assert!(collection.note(card.note_id).unwrap().is_some());
    }

    #[test]
    fn update_note_rejects_cloze_edits_that_change_the_card_set() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let mut collection = Collection::create(&path).unwrap();
        let model = collection
            .ensure_model(ranki_model::StockModelKind::Cloze)
            .unwrap();
        let ids = collection
            .add_note(&NewNote {
                model_id: model.id,
                deck_id: 1,
                fields: vec![
                    String::from("{{c1::one}} {{c2::two}}"),
                    String::new(),
                ],
                tags: Vec::new(),
            })
            .unwrap();
        let card = collection.card(ids[0]).unwrap().unwrap();
        let mut note = collection.note(card.note_id).unwrap().unwrap();
        note.fields = vec![String::from("{{c1::one}}"), String::new()];

        assert!(collection.update_note(&note, 1).is_err());
        assert_eq!(
            collection.note(note.id).unwrap().unwrap().fields,
            vec![String::from("{{c1::one}} {{c2::two}}"), String::new()]
        );
    }

    fn assert_due_indexes(collection: &Collection) {
        for name in [
            "idx_ranki_cards_queue_due",
            "idx_ranki_cards_queue_ord",
            "idx_ranki_cards_new_gather",
        ] {
            let count: i64 = collection
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing index {name}");
        }
    }

    #[test]
    fn find_cards_page_returns_requested_page_in_id_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let mut collection = Collection::create(&path).unwrap();
        let mut ids = Vec::new();
        for index in 0..5 {
            ids.extend(
                collection
                    .add_note(&NewNote {
                        model_id: 1,
                        deck_id: 1,
                        fields: vec![
                            format!("paged {index}"),
                            String::from("back"),
                        ],
                        tags: Vec::new(),
                    })
                    .unwrap(),
            );
        }
        ids.sort_unstable();

        let first = collection.find_cards_page("paged", 2, 0).unwrap();
        let second = collection.find_cards_page("paged", 2, 2).unwrap();

        assert_eq!(first.as_slice(), &ids[..2]);
        assert_eq!(second.as_slice(), &ids[2..4]);
    }

    #[test]
    fn open_recreates_due_indexes_before_due_cards() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let collection = Collection::create(&path).unwrap();
        assert_due_indexes(&collection);

        collection
            .conn
            .execute_batch(
                "DROP INDEX IF EXISTS idx_ranki_cards_queue_due; \
                 DROP INDEX IF EXISTS idx_ranki_cards_queue_ord",
            )
            .unwrap();
        drop(collection);

        let collection = Collection::open(&path).unwrap();
        assert_due_indexes(&collection);
        assert_eq!(
            collection
                .due_cards(None, 10, super::anki::now_secs().unwrap())
                .unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn note_deck_id_reads_a_card_and_handles_a_missing_note() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collection.anki2");
        let mut collection = Collection::create(&path).unwrap();
        let deck = collection.create_deck("Other").unwrap();
        let cards = collection
            .add_note(&NewNote {
                model_id: 1,
                deck_id: deck.id,
                fields: vec![String::from("front"), String::from("back")],
                tags: Vec::new(),
            })
            .unwrap();
        let note_id = collection.card(cards[0]).unwrap().unwrap().note_id;

        assert_eq!(collection.note_deck_id(note_id).unwrap(), Some(deck.id));
        assert_eq!(collection.note_deck_id(i64::MAX).unwrap(), None);
    }
}
