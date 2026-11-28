#![expect(clippy::expect_used, reason = "test setup panics on failure")]
//! Integration tests for the Anki-compatible collection store.

use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use chrono::TimeZone;
use ranki_core::FsrsScheduler;
use ranki_model::AddNoteOptions;
use ranki_model::ApplyReviewOptions;
use ranki_model::Card;
use ranki_model::CardQueue;
use ranki_model::CardType;
use ranki_model::DeckStats;
use ranki_model::DeckSteps;
use ranki_model::DuplicateScope;
use ranki_model::LeechAction;
use ranki_model::NewNote;
use ranki_model::Rating;
use ranki_model::RevLog;
use ranki_model::RevLogKind;
use ranki_model::ReviewDecision;
use ranki_model::Store;
use ranki_model::StoreError;
use ranki_storage::Collection;
use ranki_storage::ReopeningCollection;
use ranki_storage::legacy_checksum;
use rusqlite::Connection;
use rusqlite::OpenFlags;
use serde_json::Value;
use serde_json::json;
use sha1::Digest;
use sha1::Sha1;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("collection.anki2");
    Collection::create(&path).expect("create");
    (directory, path)
}

/// Keeps legacy day-number tests on the original fixed-length day rule.
fn pin_v1_timing(path: &Path) {
    let conn = Connection::open(path).expect("write connection");
    let raw_conf: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let mut value: Value = serde_json::from_str(&raw_conf).expect("conf JSON");
    value
        .as_object_mut()
        .expect("conf object")
        .insert(String::from("schedVer"), json!(1));
    let v1_conf = value.to_string();
    conn.execute(
        "UPDATE col SET conf = ?1 WHERE id = 1",
        rusqlite::params![v1_conf],
    )
    .expect("pin v1");
    let stored_raw: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("stored conf");
    let stored_value: Value =
        serde_json::from_str(&stored_raw).expect("stored conf JSON");
    assert_eq!(
        stored_value.get("schedVer").and_then(Value::as_i64),
        Some(1)
    );
}

/// Adds the modern Anki config table and leaves `col.conf` empty.
fn write_schema18_config(path: &Path, values: &[(&str, Value)]) {
    let conn = Connection::open(path).expect("write connection");
    conn.execute(
        "CREATE TABLE config (KEY text NOT NULL PRIMARY KEY, usn integer NOT NULL, \
         mtime_secs integer NOT NULL, val blob NOT NULL) WITHOUT ROWID",
        [],
    )
    .expect("create config table");
    for (key, value) in values {
        let bytes = serde_json::to_vec(value).expect("config value");
        conn.execute(
            "INSERT INTO config (key, usn, mtime_secs, val) VALUES (?1, 0, 0, ?2)",
            rusqlite::params![key, bytes],
        )
        .expect("write config value");
    }
    conn.execute("UPDATE col SET conf = '{}' WHERE id = 1", [])
        .expect("clear legacy config");
}

fn read_only(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("read only")
}

fn now_secs() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock");
    i64::try_from(elapsed.as_secs()).expect("timestamp")
}

fn note(fields: &[&str], tags: &[&str]) -> NewNote {
    NewNote {
        model_id: 1,
        deck_id: 1,
        fields: fields.iter().map(|value| (*value).into()).collect(),
        tags: tags.iter().map(|value| (*value).into()).collect(),
    }
}

#[test]
fn due_queries_use_their_queue_indexes() {
    let (_directory, path) = fixture();
    let store = Collection::open(&path).expect("open");
    let _ = store.due_cards(None, 1, now_secs()).expect("due");
    let conn = read_only(&path);
    let plan = |sql: &str, parameters: &[i64]| -> Vec<String> {
        conn.prepare(sql)
            .expect("prepare")
            .query_map(rusqlite::params_from_iter(parameters), |row| row.get(3))
            .expect("query")
            .map(|row| row.expect("row"))
            .collect()
    };
    // Learning cards order by due and time window, new cards by template then
    // position. Both walk an index instead of sorting the whole queue.
    let learning = plan(
        "EXPLAIN QUERY PLAN SELECT id FROM cards WHERE queue = ?1 AND due <= ?2 \
         ORDER BY due, id LIMIT ?3",
        &[1, i64::MAX, 20],
    );
    assert!(
        learning
            .iter()
            .any(|step| step.contains("idx_ranki_cards_queue_due")),
        "{learning:?}"
    );
    let new = plan(
        "EXPLAIN QUERY PLAN SELECT id FROM cards WHERE queue = ?1 \
         ORDER BY ord, due, id LIMIT ?2",
        &[0, 20],
    );
    assert!(
        new.iter()
            .any(|step| step.contains("idx_ranki_cards_queue_ord")),
        "{new:?}"
    );
}
#[test]
fn create_writes_anki_schema_and_default_json() {
    let (_directory, path) = fixture();
    let conn = read_only(&path);
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
        )
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect();
    for table in ["cards", "col", "graves", "notes", "revlog"] {
        assert!(tables.iter().any(|name| name == table), "missing {table}");
    }
    let indexes: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect();
    assert!(indexes.contains(&"ix_cards_sched".to_owned()));
    assert!(indexes.contains(&"ix_notes_csum".to_owned()));
    let (configuration, models, decks, dconf, tags): (
        String,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT conf, models, decks, dconf, tags FROM col WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("col row");
    for value in [&configuration, &models, &decks, &dconf, &tags] {
        let parsed: Value = serde_json::from_str(value).expect("valid JSON");
        assert!(parsed.is_object());
    }
    let model_json: Value = serde_json::from_str(&models).expect("models");
    assert_eq!(model_json["1"]["name"], "Basic");
    assert_eq!(model_json["1"]["flds"][0]["name"], "Front");
    assert_eq!(model_json["1"]["tmpls"][0]["qfmt"], "{{Front}}");
    let deck_json: Value = serde_json::from_str(&decks).expect("decks");
    assert_eq!(deck_json["1"]["name"], "Default");
}

#[test]
fn deck_list_ignores_huge_or_malformed_models_column() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "UPDATE col SET models = ?1 WHERE id = 1",
            [format!("{{{}", "x".repeat(5_000_000))],
        )
        .expect("inflate models");
    }
    let store = Collection::open(&path).expect("open");
    let decks = store.decks().expect("decks");
    assert_eq!(decks.len(), 1);
    assert_eq!(decks[0].name, "Default");
}

#[test]
fn model_list_ignores_huge_or_malformed_decks_column() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "UPDATE col SET decks = ?1 WHERE id = 1",
            [format!("{{{}", "x".repeat(5_000_000))],
        )
        .expect("inflate decks");
    }
    let store = Collection::open(&path).expect("open");
    let models = store.models().expect("models");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "Basic");
}

#[test]
fn unused_columns_still_use_the_same_text_conversion() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "UPDATE col SET decks = CAST('{\"1\":{\"name\":\"Blob Deck\"}}' AS BLOB), \
             models = 123 WHERE id = 1",
        )
        .expect("convert columns");
    }
    let store = Collection::open(&path).expect("open");
    assert_eq!(store.decks().expect("decks")[0].name, "Blob Deck");
    assert!(store.models().is_err());
}
#[test]
fn deck_and_model_lists_keep_id_order() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "UPDATE col SET decks = '{\"9\":{\"name\":\"Nine\"},\"2\":{\"name\":\"Two\"}}', \
             models = '{\"9\":{\"name\":\"Nine\"},\"2\":{\"name\":\"Two\"}}' WHERE id = 1",
        )
        .expect("reorder JSON");
    }
    let store = Collection::open(&path).expect("open");
    let decks: Vec<i64> = store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.id)
        .collect();
    let models: Vec<i64> = store
        .models()
        .expect("models")
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(decks, [2, 9]);
    assert_eq!(models, [2, 9]);
}

#[test]
fn created_collection_declares_its_legacy_schema_version() {
    let (_directory, path) = fixture();
    let conn = read_only(&path);
    let version: i64 = conn
        .query_row("SELECT ver FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("version");
    assert_eq!(version, 11);
    let has_modern_tables: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'notetypes')",
            [],
            |row| row.get(0),
        )
        .expect("modern tables");
    assert!(!has_modern_tables);
}

#[test]
fn review_and_day_learning_dues_use_days_since_collection_creation() {
    let (_directory, path) = fixture();
    let created = 1_685_412_000_i64;
    let now = created.saturating_add(86_400 * 2);
    let today = 2_i64;
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [created])
            .expect("set crt");
        let rows = [
            (21_i64, 2_i64, today),
            (22, 2, today.saturating_add(1)),
            (23, 3, today),
            (24, 3, today.saturating_add(1)),
            (25, 1, now),
            (26, 1, now.saturating_add(1)),
        ];
        for (id, queue, due) in rows {
            conn.execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, \
                 reps, lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, 0, 0, ?2, ?3, \
                 ?4, 1, 0, 0, 0, 0, 0, 0, 0, '')",
                [id, 2_i64, queue, due],
            )
            .expect("insert card");
        }
    }
    let store = Collection::open(&path).expect("open");
    pin_v1_timing(&path);
    assert_eq!(store.collection_created().expect("created"), created);
    let due_ids: Vec<_> = store
        .due_cards(None, 50, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(due_ids, [25, 26, 21, 23]);
    assert!(!due_ids.contains(&22));
    assert!(!due_ids.contains(&24));

    let card = store.card(21).expect("card").expect("exists");
    let decision = FsrsScheduler::default().schedule(
        &card,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );
    let expected = today.saturating_add(i64::from(decision.card.interval));
    assert_eq!(decision.card.due, expected);
}

#[test]
fn due_forecast_buckets_current_overdue_learning_and_future_cards() {
    let (_directory, path) = fixture();
    let created = 1_685_412_000_i64;
    let now = created.saturating_add(172_800);
    let day_secs = 86_400_i64;
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [created])
            .expect("set crt");
        let rows = [
            (31_i64, 2_i64, 1_i64),
            (32, 2, 2),
            (33, 3, 2),
            (34, 1, now),
            (35, 1, created.saturating_add(day_secs.saturating_mul(5))),
            (36, 2, 3),
            (37, 2, 3),
            (38, 3, 4),
            (39, 2, 8),
            (40, 2, 9),
            (41, 0, 3),
            (42, -1, 2),
        ];
        for (id, queue, due) in rows {
            conn.execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, \
                 reps, lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, 0, 0, 2, ?2, \
                 ?3, 1, 0, 0, 0, 0, 0, 0, 0, '')",
                [id, queue, due],
            )
            .expect("insert card");
        }
    }
    let store = Collection::open(&path).expect("open");
    pin_v1_timing(&path);
    assert_eq!(
        store.due_forecast(7, now).expect("forecast"),
        [4, 2, 1, 0, 0, 0, 1]
    );
    assert_eq!(
        store.due_forecast(0, now).expect("empty forecast"),
        Vec::<usize>::new()
    );
}

#[test]
fn create_rejects_existing_collection_and_open_rejects_missing_file() {
    let (_directory, path) = fixture();
    assert!(matches!(
        Collection::create(&path),
        Err(StoreError::InvalidData(_))
    ));
    let missing = path.with_file_name("missing.anki2");
    assert!(matches!(
        Collection::open(&missing),
        Err(StoreError::Backend(_))
    ));
}

#[test]
fn add_note_stores_anki_fields_tags_sort_field_and_checksum() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["<b>hello</b>", "world"], &["one", "two"]))
        .expect("add note");
    let card = store.card(ids[0]).expect("card").expect("card exists");
    let saved = store
        .note(card.note_id)
        .expect("note")
        .expect("note exists");
    assert_eq!(saved.fields, ["<b>hello</b>", "world"]);
    assert_eq!(saved.tags, ["one", "two"]);
    assert_eq!(saved.sort_field, "hello");
    let digest = Sha1::digest(b"hello");
    let unsigned =
        u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    let expected = if unsigned > 2_147_483_647 {
        i32::try_from(
            i64::from(unsigned)
                .saturating_sub(i64::from(u32::MAX))
                .saturating_sub(1),
        )
        .expect("checksum")
    } else {
        i32::try_from(unsigned).expect("checksum")
    };
    assert_eq!(saved.checksum, expected);
    let other_ids = store
        .add_note(&note(&["other", "world"], &[]))
        .expect("add");
    let other_card = store.card(other_ids[0]).expect("card").expect("exists");
    let other = store
        .note(other_card.note_id)
        .expect("note")
        .expect("exists");
    assert_ne!(saved.guid, other.guid);
    assert!(!saved.guid.is_empty() && !other.guid.is_empty());
    assert!(saved.guid.is_ascii());
}

#[test]
fn tags_lists_distinct_tags_from_every_note() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let _ = store
        .add_note(&note(&["first", "back"], &["japan", "verb"]))
        .expect("add first");
    let _ = store
        .add_note(&note(&["second", "back"], &["verb", "noun"]))
        .expect("add second");
    assert_eq!(store.tags().expect("tags"), ["japan", "noun", "verb"]);

    let reopened = ReopeningCollection::new(&path);
    assert_eq!(reopened.tags().expect("tags"), ["japan", "noun", "verb"]);
}

#[test]
fn add_note_creates_one_card_per_template() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        let models: String = conn
            .query_row("SELECT models FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("models");
        let mut model_json: Value =
            serde_json::from_str(&models).expect("parse");
        let template = model_json["1"]["tmpls"][0].clone();
        let templates =
            model_json["1"]["tmpls"].as_array_mut().expect("templates");
        templates.push(template);
        conn.execute(
            "UPDATE col SET models = ?1 WHERE id = 1",
            [serde_json::to_string(&model_json).expect("serialize")],
        )
        .expect("update");
    }
    let mut store = Collection::open(&path).expect("open");
    assert_eq!(store.models().expect("models")[0].templates.len(), 2);
    let ids = store.add_note(&note(&["front", "back"], &[])).expect("add");
    assert_eq!(ids.len(), 2);
    let cards: Vec<_> = ids
        .iter()
        .map(|id| store.card(*id).expect("card").expect("exists"))
        .collect();
    assert_eq!(cards[0].ordinal, 0);
    assert_eq!(cards[1].ordinal, 1);
    assert_eq!(cards[0].note_id, cards[1].note_id);
    assert_ne!(cards[0].due, cards[1].due);
}

#[test]
fn create_deck_builds_hierarchy_and_filters_subdecks() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("Study").expect("parent");
    let child = store.create_deck("Study::Languages").expect("child");
    let names: Vec<_> = store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.name)
        .collect();
    assert!(names.contains(&"Study".to_owned()));
    assert!(names.contains(&"Study::Languages".to_owned()));
    let mut new_note = note(&["bonjour", "hello"], &[]);
    new_note.deck_id = child.id;
    let ids = store.add_note(&new_note).expect("add");
    let parent_cards = store
        .due_cards(Some(parent.id), 10, now_secs())
        .expect("due");
    assert!(parent_cards.iter().any(|card| card.id == ids[0]));
    let stats = store.stats(Some(parent.id), now_secs()).expect("stats");
    assert_eq!(stats.new_cards, 1);
    let found = store.find_cards("deck:Study").expect("find");
    assert!(found.contains(&ids[0]));
    assert!(store.create_deck("Study").is_err());
    assert!(store.create_deck("Bad::").is_err());
}

#[test]
fn default_deck_is_hidden_only_when_empty_and_childless() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");

    let names: Vec<_> = store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.name)
        .collect();
    assert_eq!(names, ["Default".to_owned()]);

    store.create_deck("Flags").expect("flags");
    let names: Vec<_> = store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.name)
        .collect();
    assert_eq!(names, ["Flags".to_owned()]);

    store
        .add_note(&note(&["front", "back"], &[]))
        .expect("note");
    let names: Vec<_> = store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.name)
        .collect();
    assert!(names.contains(&"Default".to_owned()));

    let (_child_directory, child_path) = fixture();
    let mut child_store = Collection::open(&child_path).expect("open");
    child_store.create_deck("Flags").expect("flags");
    child_store.create_deck("Default::Kids").expect("child");
    let names: Vec<_> = child_store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.name)
        .collect();
    assert!(names.contains(&"Default".to_owned()));
}

#[test]
fn due_cards_respects_queue_and_time_windows() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let today = 0_i64;
    let conn = Connection::open(&path).expect("open");
    conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [now])
        .expect("set crt");
    let rows = [
        (10_i64, 0_i64, 7_i64),
        (11, 1, now),
        (12, 1, now.saturating_add(60)),
        (13, 2, today),
        (14, 2, today.saturating_add(1)),
        (15, 3, today),
        (16, 3, today.saturating_add(1)),
    ];
    for (id, queue, due) in rows {
        conn.execute(
            "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
             lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, 0, 0, 0, ?2, ?3, 0, 0, \
             0, 0, 0, 0, 0, 0, '')",
            [id, queue, due],
        )
        .expect("insert card");
    }
    let store = Collection::open(&path).expect("open");
    let due_ids: Vec<_> = store
        .due_cards(None, 50, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(due_ids, [11, 12, 13, 10, 15]);
    let limited = store.due_cards(None, 2, now).expect("due");
    assert_eq!(limited.len(), 2);
    assert_eq!(
        store.due_cards(None, 0, now).expect("due"),
        [] as [ranki_model::Card; 0]
    );
}

#[test]
fn stats_count_only_cards_due_at_the_given_time() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let conn = Connection::open(&path).expect("open");
    conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [now])
        .expect("set crt");
    let rows = [
        (10_i64, 0_i64, 7_i64),
        (11, 1, now),
        (12, 1, now.saturating_add(60)),
        (13, 2, 0),
        (14, 2, 1),
        (15, 3, 0),
        (16, 3, 1),
    ];
    for (id, queue, due) in rows {
        conn.execute(
            "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
             lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, 0, 0, 0, ?2, ?3, 0, 0, \
             0, 0, 0, 0, 0, 0, '')",
            [id, queue, due],
        )
        .expect("insert card");
    }
    let store = Collection::open(&path).expect("open");
    let stats = store.stats(None, now).expect("stats");
    assert_eq!(stats.new_cards, 1);
    assert_eq!(stats.learning_cards, 3);
    assert_eq!(stats.review_cards, 1);
    assert_eq!(stats.total_cards, 7);

    let batch = store.deck_stats(now).expect("deck stats");
    assert_eq!(
        batch
            .into_iter()
            .find(|(id, _)| *id == 1)
            .map(|(_, stats)| stats),
        Some(stats)
    );
}

#[test]
fn browse_cards_read_the_sort_field_of_numeric_questions() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    // `notes.sfld` has integer affinity, so this stores an integer.
    let ids = store
        .add_note(&note(&["1942", "answer"], &[]))
        .expect("add");
    let cards = store.browse_cards("").expect("browse");
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].card_id, ids[0]);
    assert_eq!(cards[0].sort_field, "1942");
}

#[test]
fn find_cards_matches_text_wildcards_and_state() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["alpha question", "answer"], &[]))
        .expect("add");
    store
        .add_note(&note(&["beta question", "answer"], &[]))
        .expect("add");
    assert_eq!(store.find_cards("alpha").expect("find").len(), 1);
    assert_eq!(store.find_cards("question").expect("find").len(), 2);
    assert_eq!(store.find_cards("alpha*").expect("find").len(), 1);
    assert_eq!(store.find_cards("QUESTION").expect("find").len(), 2);
    assert_eq!(store.find_cards("ALPHA").expect("find").len(), 1);
    assert_eq!(store.find_cards("is:new").expect("find").len(), 2);
    assert_eq!(store.find_cards("is:review").expect("find"), [] as [i64; 0]);
    assert!(store.find_cards("is:unknown").is_err());
    let quoted = store.find_cards("\"alpha question\"").expect("find");
    assert_eq!(quoted.len(), 1);
}

#[test]
fn find_cards_matches_deck_tag_and_model() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let deck = store.create_deck("Languages::French").expect("deck");
    let mut new_note = note(&["bonjour", "hello"], &["french"]);
    new_note.deck_id = deck.id;
    let ids = store.add_note(&new_note).expect("add");
    assert_eq!(store.find_cards("tag:french").expect("find"), ids);
    assert_eq!(store.find_cards("tag:FRENCH").expect("find"), ids);
    assert_eq!(store.find_cards("note:Basic").expect("find"), ids);
    assert_eq!(
        store.find_cards("tag:english").expect("find"),
        [] as [i64; 0]
    );
    assert_eq!(store.find_cards("deck:Languages").expect("find"), ids);
    assert_eq!(
        store
            .find_cards("deck:Languages::French tag:french")
            .expect("find"),
        ids
    );
}

#[test]
fn find_cards_does_not_interpret_sql_injection() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["secret", "answer"], &["safe"]))
        .expect("add");
    for query in [
        "' OR 1=1 --",
        "deck:Default' OR 1=1 --",
        "tag:safe' OR '1'='1",
    ] {
        assert!(
            store.find_cards(query).expect("no injection").is_empty(),
            "{query}"
        );
    }
    assert_eq!(store.find_cards("secret").expect("normal search").len(), 1);
    assert_eq!(store.find_cards("").expect("empty query").len(), 1);
}

#[test]
fn apply_review_updates_card_log_and_collection_metadata() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["review me", "answer"], &[]))
        .expect("add");
    let card = store.card(ids[0]).expect("card").expect("exists");
    let before = store.collection_modified().expect("modified");
    let mut changed = card.clone();
    changed.queue = CardQueue::Review;
    changed.card_type = CardType::Review;
    changed.due = 42;
    changed.interval = 4;
    let decision = ReviewDecision {
        card: changed,
        expected_modified: card.modified,
        expected_usn: card.usn,
        revlog: RevLog {
            id: 9_000,
            card_id: card.id,
            usn: 0,
            ease: 3,
            interval: 4,
            last_interval: 0,
            factor: 2500,
            time_ms: 500,
            kind: RevLogKind::Review,
        },
    };
    store.apply_review(&decision).expect("apply");
    let saved = store.card(card.id).expect("card").expect("exists");
    assert_eq!(saved.queue, CardQueue::Review);
    assert_eq!(saved.due, 42);
    let log = store.revlog(9_000).expect("revlog").expect("entry");
    assert_eq!(log.card_id, card.id);
    assert_eq!(log.ease, 3);
    assert!(store.collection_modified().expect("modified") > before);
    store.apply_review(&decision).expect("second review");
    assert_eq!(
        store.review_history().expect("history").len(),
        2,
        "review ids are uniquified"
    );
}

#[test]
fn undo_review_restores_card_and_deletes_log() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["undo me", "answer"], &[]))
        .expect("add");
    let now = now_secs();
    let before = store
        .due_cards(None, 1, now)
        .expect("due")
        .pop()
        .expect("due card");
    let decision = FsrsScheduler::default().schedule(
        &before,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );
    store.apply_review(&decision).expect("apply");
    assert_ne!(
        store.card(before.id).expect("card").expect("exists"),
        before
    );
    assert_eq!(store.review_history().expect("history").len(), 1);

    assert!(
        store
            .undo_review(&before, decision.revlog.id)
            .expect("undo")
    );
    let mut restored = store.card(before.id).expect("card").expect("exists");
    assert_eq!(restored.usn, -1, "local undo stays pending sync");
    restored.usn = before.usn;
    assert_eq!(restored, before);
    assert_eq!(
        store.review_history().expect("history"),
        [] as [ranki_model::RevLog; 0]
    );

    let unchanged = store.card(before.id).expect("card").expect("exists");
    assert!(
        !store
            .undo_review(&before, decision.revlog.id)
            .expect("retry")
    );
    assert_eq!(
        store.card(before.id).expect("card").expect("exists"),
        unchanged
    );
}

#[test]
fn grade_undo_grade_uses_the_restored_revision() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["again", "answer"], &[]))
        .expect("add");
    let now = now_secs();
    let before = store
        .due_cards(None, 1, now)
        .expect("due")
        .pop()
        .expect("card");
    let scheduler = FsrsScheduler::default();
    let decision = scheduler.schedule(
        &before,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );
    store.apply_review(&decision).expect("grade");
    assert!(
        store
            .undo_review(&before, decision.revlog.id)
            .expect("undo")
    );
    let restored = store.card(before.id).expect("card").expect("exists");
    let retry = scheduler.schedule(
        &restored,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );
    store.apply_review(&retry).expect("grade after undo");
    assert_eq!(store.review_history().expect("history").len(), 1);
}

#[test]
fn stale_review_writes_are_rejected() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["stale", "answer"], &[]))
        .expect("add");
    let now = now_secs();
    let card = store
        .due_cards(None, 1, now)
        .expect("due")
        .pop()
        .expect("card");
    let decision = FsrsScheduler::default().schedule(
        &card,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );
    let conn = Connection::open(&path).expect("open");
    conn.execute("UPDATE cards SET mod = mod + 10 WHERE id = ?1", [card.id])
        .expect("external write");
    drop(conn);
    assert!(store.apply_review(&decision).is_err());
    assert_eq!(
        store.review_history().expect("history"),
        [] as [ranki_model::RevLog; 0]
    );
}

#[test]
fn repeated_review_gets_a_unique_log_id() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["race", "answer"], &[]))
        .expect("add");
    let card = store
        .due_cards(None, 1, now_secs())
        .expect("due")
        .pop()
        .expect("card");
    let now = card.modified;
    let scheduler = FsrsScheduler::default();
    let first = scheduler.schedule(
        &card,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );
    let second = first.clone();
    store.apply_review(&first).expect("first write");
    store.apply_review(&second).expect("second write");
    assert_eq!(store.review_history().expect("history").len(), 2);
}

#[test]
fn review_of_an_older_card_is_accepted() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["older", "answer"], &[]))
        .expect("add");
    let now = now_secs();
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("UPDATE cards SET mod = ?1", [now.saturating_sub(3600)])
            .expect("age card");
    }
    let card = store
        .due_cards(None, 1, now)
        .expect("due")
        .pop()
        .expect("card");
    let decision = FsrsScheduler::default().schedule(
        &card,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );
    store.apply_review(&decision).expect("apply older card");
    assert_eq!(store.review_history().expect("history").len(), 1);
}

#[test]
fn stats_excludes_future_day_learning_cards() {
    let (_directory, path) = fixture();
    let created = 1_685_412_000_i64;
    let now = created.saturating_add(86_400);
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [created])
            .expect("set crt");
        for (id, due) in [(31_i64, 1_i64), (32, 2)] {
            conn.execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, \
                 reps, lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, 0, 0, 2, 3, \
                 ?2, 1, 0, 0, 0, 0, 0, 0, 0, '')",
                [id, due],
            )
            .expect("insert");
        }
    }
    let store = Collection::open(&path).expect("open");
    pin_v1_timing(&path);
    let stats = store.stats(None, now).expect("stats");
    assert_eq!(stats.total_cards, 2);
    assert_eq!(stats.learning_cards, 1);
}

#[test]
fn due_stats_count_only_cards_due_now() {
    let (_directory, path) = fixture();
    let created = 1_685_412_000_i64;
    let now = created.saturating_add(86_400);
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [created])
            .expect("set crt");
        for (id, queue, due) in [
            (21_i64, 0_i64, 0_i64),
            (22, 1, now),
            (23, 3, 1_i64),
            (24, 2, 2_i64),
            (25, 2, 1_i64),
        ] {
            conn.execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, \
                 reps, lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, 0, 0, ?2, ?3, \
                 ?4, 1, 0, 0, 0, 0, 0, 0, 0, '')",
                [id, 2_i64, queue, due],
            )
            .expect("insert");
        }
    }
    let store = Collection::open(&path).expect("open");
    pin_v1_timing(&path);
    let all = store.stats(None, now).expect("stats");
    let due = store
        .deck_stats(now)
        .expect("deck stats")
        .into_iter()
        .find(|(id, _)| *id == 1)
        .map(|(_, stats)| stats)
        .unwrap_or_default();
    assert_eq!(all.total_cards, 5);
    assert_eq!(all.new_cards, 1);
    assert_eq!(due.new_cards, 1);
    assert_eq!(due.learning_cards, 2);
    assert_eq!(due.review_cards, 1);
    assert_eq!(
        due.new_cards
            .saturating_add(due.learning_cards)
            .saturating_add(due.review_cards),
        4
    );
}

#[test]
fn stats_and_collection_modified_track_writes() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let before = store.collection_modified().expect("modified");
    store.add_note(&note(&["one", "answer"], &[])).expect("add");
    let stats = store.stats(None, now_secs()).expect("stats");
    assert_eq!(stats.new_cards, 1);
    assert_eq!(stats.total_cards, 1);
    assert!(stats.learning_cards == 0 && stats.review_cards == 0);
    assert!(store.collection_modified().expect("modified") > before);
    assert_eq!(
        store
            .stats(Some(999), now_secs())
            .expect("stats")
            .total_cards,
        0
    );
}

#[test]
fn collection_modified_increases_with_consecutive_writes() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let mut previous = store.collection_modified().expect("modified");
    for _ in 0..3 {
        store
            .add_note_with_options(
                &note(&["one", "answer"], &[]),
                &AddNoteOptions::allowing_duplicates(),
            )
            .expect("add");
        let current = store.collection_modified().expect("modified");
        assert!(current > previous, "{current} <= {previous}");
        previous = current;
    }
}

#[test]
fn collection_modified_saturates_at_the_end_of_the_range() {
    let (_directory, path) = fixture();
    let conn = Connection::open(&path).expect("open");
    conn.execute("UPDATE col SET mod = ?1 WHERE id = 1", [i64::MAX - 1])
        .expect("seed mod");
    drop(conn);
    let mut store = Collection::open(&path).expect("open");
    for _ in 0..2 {
        store
            .add_note_with_options(
                &note(&["one", "answer"], &[]),
                &AddNoteOptions::allowing_duplicates(),
            )
            .expect("add");
        assert_eq!(
            store.collection_modified().expect("modified"),
            i64::MAX,
            "saturating bump past i64::MAX - 1 stays at the limit"
        );
    }
}

#[test]
fn checkpoint_and_close_leave_a_readable_collection() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["durable", "answer"], &[]))
        .expect("add");
    store.checkpoint().expect("checkpoint");
    assert_eq!(store.path(), path);
    store.close().expect("close");
    let reopened = Collection::open(&path).expect("reopen");
    assert_eq!(reopened.find_cards("durable").expect("find").len(), 1);
}

#[test]
fn open_does_not_rewrite_existing_collection_json() {
    let (_directory, path) = fixture();
    let conn = Connection::open(&path).expect("open");
    conn.execute(
        "UPDATE col SET conf = ?1 WHERE id = 1",
        ["{\"nextPos\": 77, \"custom\": true}"],
    )
    .expect("update");
    drop(conn);
    let store = Collection::open(&path).expect("open");
    store.checkpoint().expect("checkpoint");
    drop(store);
    let conn = read_only(&path);
    let configuration: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let parsed: Value = serde_json::from_str(&configuration).expect("JSON");
    assert_eq!(parsed["nextPos"], 77);
    assert_eq!(parsed["custom"], true);
}

#[test]
fn reads_a_real_anki_collection_without_touching_the_original() {
    let Some(source) = env::var_os("RANKI_TEST_COLLECTION")
        .map(PathBuf::from)
        .filter(|path| path.exists())
    else {
        eprintln!("skipping: real Anki collection not present");
        return;
    };
    let original = fs::read(&source).expect("read original");
    let original_hash = Sha1::digest(&original);
    let directory = tempfile::tempdir().expect("tempdir");
    let copy = directory.path().join("collection.anki2");
    fs::copy(&source, &copy).expect("copy");
    let store = Collection::open(&copy).expect("open copy");
    let conn = read_only(&copy);
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect();
    for table in ["cards", "col", "graves", "notes", "revlog"] {
        assert!(tables.iter().any(|name| name == table), "missing {table}");
    }
    let decks = store.decks().expect("decks");
    assert!(decks.len() > 1);
    assert!(!decks.iter().any(|deck| deck.name == "Default"));
    assert!(decks.iter().any(|deck| deck.name.contains("::")));
    let first_card: i64 = conn
        .query_row("SELECT id FROM cards ORDER BY id LIMIT 1", [], |row| {
            row.get(0)
        })
        .expect("card id");
    assert!(store.card(first_card).expect("card").is_some());
    assert!(store.models().expect("models").len() > 1);
    let due_now = {
        let crt: i64 = conn
            .query_row("SELECT crt FROM col WHERE id = 1", [], |row| row.get(0))
            .expect("crt");
        let maximum_due: i64 = conn
            .query_row(
                "SELECT MAX(due) FROM cards WHERE queue IN (2, 3)",
                [],
                |row| row.get(0),
            )
            .expect("maximum due");
        crt.saturating_add(maximum_due.saturating_mul(86_400))
    };
    assert_ne!(
        store.due_cards(None, 5, due_now).expect("due"),
        [] as [ranki_model::Card; 0]
    );
    drop(conn);
    drop(store);
    assert_eq!(
        Sha1::digest(fs::read(&source).expect("read again")),
        original_hash
    );
}

#[test]
fn ids_are_unique_across_storage_core_and_manual_logs() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let note_ids = store
        .add_note(&note(&["front", "back"], &[]))
        .expect("add note");
    let card_id = note_ids.last().copied().expect("card id");
    let card = store.card(card_id).expect("card").expect("card exists");
    let start = store.collection_created().expect("collection start");
    let decision = FsrsScheduler::default().schedule(
        &card,
        Rating::Good,
        now_secs(),
        start,
    );
    let manual_id = ranki_model::next_id(now_secs().saturating_mul(1_000));

    let mut ids = note_ids;
    ids.push(decision.revlog.id);
    ids.push(manual_id);
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
}

#[test]
fn rename_deck_renames_subdecks_and_preserves_ids() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("Study").expect("parent");
    let child = store.create_deck("Study::Languages").expect("child");
    let grandchild = store
        .create_deck("Study::Languages::French")
        .expect("grandchild");
    let mut entry = note(&["bonjour", "hello"], &[]);
    entry.deck_id = grandchild.id;
    let card_id = store.add_note(&entry).expect("add")[0];

    let renamed = store.rename_deck(parent.id, "Learning").expect("rename");
    assert_eq!(renamed.id, parent.id);
    assert_eq!(renamed.name, "Learning");

    let decks = store.decks().expect("decks");
    let names: Vec<_> = decks.iter().map(|deck| deck.name.as_str()).collect();
    assert!(names.contains(&"Learning"));
    assert!(names.contains(&"Learning::Languages"));
    assert!(names.contains(&"Learning::Languages::French"));
    assert!(!names.iter().any(|name| name.starts_with("Study")));
    for expected in [&child, &grandchild] {
        assert!(decks.iter().any(|deck| deck.id == expected.id));
    }
    let card = store.card(card_id).expect("card").expect("exists");
    assert_eq!(card.deck_id, grandchild.id);

    let conn = read_only(&path);
    let decks_json: String = conn
        .query_row("SELECT decks FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("decks json");
    let parsed: Value = serde_json::from_str(&decks_json).expect("valid JSON");
    let deck_name = |id: i64| {
        parsed
            .get(id.to_string())
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(deck_name(parent.id), "Learning");
    assert_eq!(deck_name(child.id), "Learning::Languages");
    assert_eq!(deck_name(grandchild.id), "Learning::Languages::French");
    let index: String = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'ix_cards_sched'",
            [],
            |row| row.get(0),
        )
        .expect("index");
    assert_eq!(index, "ix_cards_sched");
}

#[test]
fn rename_deck_rejects_empty_and_colliding_names() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("A").expect("parent");
    store.create_deck("A::B").expect("child");
    store.create_deck("B").expect("other");
    assert!(store.rename_deck(parent.id, "  ").is_err());
    assert!(store.rename_deck(parent.id, "A::").is_err());
    assert!(store.rename_deck(parent.id, "B").is_err());
    assert!(store.rename_deck(999, "Renamed").is_err());

    let source = store.create_deck("Q").expect("source");
    store.create_deck("Q::Z").expect("source child");
    store.create_deck("Z::Z").expect("collision");
    assert!(store.rename_deck(source.id, "Z").is_err());
    assert!(
        store
            .decks()
            .expect("decks")
            .iter()
            .any(|deck| deck.name == "Q")
    );
}

#[test]
fn remove_deck_deletes_subtree_cards_notes_revlog_and_writes_graves() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("Work").expect("parent");
    let child = store.create_deck("Work::Notes").expect("child");
    let mut entry = note(&["question", "answer"], &["tag"]);
    entry.deck_id = child.id;
    let card_id = store.add_note(&entry).expect("add")[0];
    let card = store.card(card_id).expect("card").expect("exists");
    let note_id = card.note_id;
    let log_id = 9_002_i64;
    let decision = ReviewDecision {
        card: card.clone(),
        expected_modified: card.modified,
        expected_usn: card.usn,
        revlog: RevLog {
            id: log_id,
            card_id,
            usn: 0,
            ease: 3,
            interval: 4,
            last_interval: 0,
            factor: 2500,
            time_ms: 500,
            kind: RevLogKind::Review,
        },
    };
    store.apply_review(&decision).expect("review");

    store.remove_deck(parent.id).expect("remove");

    assert_eq!(store.decks().expect("decks").len(), 1);
    assert!(store.card(card_id).expect("card").is_none());
    assert!(store.note(note_id).expect("note").is_none());
    assert!(store.revlog(log_id).expect("revlog").is_none());
    let conn = read_only(&path);
    let mut statement = conn
        .prepare("SELECT oid, type FROM graves ORDER BY type, oid")
        .expect("prepare");
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .expect("query");
    let graves: Vec<_> = rows.map(|row| row.expect("row")).collect();
    assert!(graves.contains(&(card_id, 0)), "{graves:?}");
    assert!(graves.contains(&(note_id, 1)), "{graves:?}");
    assert!(graves.contains(&(parent.id, 2)), "{graves:?}");
    assert!(graves.contains(&(child.id, 2)), "{graves:?}");
    let decks_json: String = conn
        .query_row("SELECT decks FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("decks json");
    let parsed: Value = serde_json::from_str(&decks_json).expect("JSON");
    assert!(parsed.get(parent.id.to_string()).is_none());
    assert!(parsed.get(child.id.to_string()).is_none());
    assert!(parsed.get("1").is_some());
}

#[test]
fn remove_deck_protects_default_and_rejects_unknown_decks() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    assert!(matches!(
        store.remove_deck(1),
        Err(StoreError::InvalidData(_))
    ));
    assert!(matches!(
        store.remove_deck(999),
        Err(StoreError::NotFound(_))
    ));
    assert!(
        store
            .decks()
            .expect("decks")
            .iter()
            .any(|deck| deck.id == 1)
    );
}

#[test]
fn remove_deck_keeps_notes_that_still_have_cards() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        let models: String = conn
            .query_row("SELECT models FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("models");
        let mut model_json: Value =
            serde_json::from_str(&models).expect("parse");
        let template = model_json["1"]["tmpls"][0].clone();
        model_json["1"]["tmpls"]
            .as_array_mut()
            .expect("templates")
            .push(template);
        conn.execute(
            "UPDATE col SET models = ?1 WHERE id = 1",
            [serde_json::to_string(&model_json).expect("serialize")],
        )
        .expect("update");
    }
    let mut store = Collection::open(&path).expect("open");
    let source = store.create_deck("Source").expect("source");
    let other = store.create_deck("Other").expect("other");
    let mut entry = note(&["front", "back"], &[]);
    entry.deck_id = source.id;
    let cards = store.add_note(&entry).expect("add");
    assert_eq!(cards.len(), 2);
    let note_id = store.card(cards[0]).expect("card").expect("exists").note_id;

    let mut moved = store.card(cards[1]).expect("card").expect("exists");
    moved.deck_id = other.id;
    let decision = ReviewDecision {
        card: moved.clone(),
        expected_modified: moved.modified,
        expected_usn: moved.usn,
        revlog: RevLog {
            id: 9_003_i64,
            card_id: cards[1],
            usn: 0,
            ease: 3,
            interval: 0,
            last_interval: 0,
            factor: 2500,
            time_ms: 0,
            kind: RevLogKind::Manual,
        },
    };
    store.apply_review(&decision).expect("move");
    store.remove_deck(source.id).expect("remove");

    assert!(store.card(cards[0]).expect("card").is_none());
    assert_eq!(
        store.card(cards[1]).expect("card").expect("exists").deck_id,
        other.id
    );
    assert!(store.note(note_id).expect("note").is_some());
}

#[test]
fn rename_and_remove_work_with_modern_deck_tables() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "UPDATE col SET ver = 18, decks = '{}' WHERE id = 1;
             CREATE TABLE decks (
                 id integer primary key,
                 name text not null,
                 mtime_secs integer not null,
                 usn integer not null,
                 common blob not null,
                 kind integer not null
             );
             INSERT INTO decks VALUES (10, 'Modern', 0, 0, x'', 0);
             INSERT INTO decks VALUES (11, 'Modern\u{001f}Child', 0, 0, x'', 0);",
        )
        .expect("modern schema");
    }
    let mut store = Collection::open(&path).expect("open");
    let parent = store
        .decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.name == "Modern")
        .expect("parent");
    let renamed = store.rename_deck(parent.id, "Current").expect("rename");
    assert_eq!(renamed.name, "Current");
    let names: Vec<_> = store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.name)
        .collect();
    assert!(names.contains(&"Current::Child".to_owned()));

    let conn = read_only(&path);
    let raw: String = conn
        .query_row("SELECT name FROM decks WHERE id = 11", [], |row| row.get(0))
        .expect("encoded name");
    assert_eq!(raw, "Current\u{001f}Child");
    drop(conn);

    store.remove_deck(parent.id).expect("remove");
    let conn = read_only(&path);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM decks", [], |row| row.get(0))
        .expect("count");
    assert_eq!(count, 0);
}

#[test]
fn deck_stats_matches_per_deck_stats_for_a_hierarchy() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let root = store.create_deck("Root").expect("root");
    let child = store.create_deck("Root::Child").expect("child");
    let leaf = store.create_deck("Root::Child::Leaf").expect("leaf");
    for (index, deck_id) in
        [root.id, child.id, leaf.id, 1].into_iter().enumerate()
    {
        store
            .add_note(&NewNote {
                model_id: 1,
                deck_id,
                fields: vec![format!("front {index}"), "back".to_owned()],
                tags: Vec::new(),
            })
            .expect("add");
    }
    let batch = store.deck_stats(now_secs()).expect("deck stats");
    let decks = store.decks().expect("decks");
    assert_eq!(batch.len(), decks.len());
    for (id, stats) in batch {
        assert_eq!(
            stats,
            store.stats(Some(id), now_secs()).expect("stats"),
            "deck {id}"
        );
    }
    let root_stats = store
        .deck_stats(now_secs())
        .expect("deck stats")
        .into_iter()
        .find(|(id, _)| *id == root.id)
        .expect("root entry");
    assert_eq!(root_stats.1.total_cards, 3);
    assert_eq!(root_stats.1.new_cards, 3);
}

#[test]
fn deck_stats_counts_children_of_names_ending_in_a_colon() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("A:").expect("parent");
    let child = store.create_deck("A:::B").expect("child");
    store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: child.id,
            fields: vec!["front".to_owned(), "back".to_owned()],
            tags: Vec::new(),
        })
        .expect("add");
    let batch = store.deck_stats(now_secs()).expect("deck stats");
    let totals = batch
        .iter()
        .find(|(id, _)| *id == parent.id)
        .map(|(_, stats)| *stats)
        .expect("parent entry");
    assert_eq!(
        totals,
        store.stats(Some(parent.id), now_secs()).expect("stats")
    );
    assert_eq!(totals.new_cards, 1);
}

#[test]
fn apply_reviews_writes_a_batch_atomically() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let mut decisions = Vec::with_capacity(100);
    for index in 0..100_u32 {
        let ids = store
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![format!("card {index}"), "back".to_owned()],
                tags: Vec::new(),
            })
            .expect("add");
        let card = store.card(ids[0]).expect("card").expect("exists");
        let mut changed = card.clone();
        changed.queue = CardQueue::Review;
        changed.card_type = CardType::Review;
        changed.due = i64::from(index);
        changed.interval = 4;
        decisions.push(ReviewDecision {
            card: changed,
            expected_modified: card.modified,
            expected_usn: card.usn,
            revlog: RevLog {
                id: 20_000 + i64::from(index),
                card_id: card.id,
                usn: 0,
                ease: 3,
                interval: 4,
                last_interval: 0,
                factor: 2500,
                time_ms: 400,
                kind: RevLogKind::Review,
            },
        });
    }
    store.apply_reviews(&decisions).expect("batch");
    for (index, decision) in decisions.iter().enumerate() {
        let saved =
            store.card(decision.card.id).expect("card").expect("exists");
        assert_eq!(saved.queue, CardQueue::Review);
        assert_eq!(saved.interval, 4);
        assert_eq!(saved.due, i64::from(u32::try_from(index).expect("index")));
        let log = store
            .revlog(20_000 + i64::try_from(index).expect("index"))
            .expect("revlog")
            .expect("entry");
        assert_eq!(log.card_id, decision.card.id);
    }

    let mut failing = decisions[0].clone();
    failing.revlog.id = 90_000;
    failing.card.interval = 9;
    let mut missing = failing.clone();
    missing.card.id = -1;
    missing.revlog.id = 90_001;
    assert!(store.apply_reviews(&[failing, missing]).is_err());
    let saved = store
        .card(decisions[0].card.id)
        .expect("card")
        .expect("exists");
    assert_eq!(saved.interval, 4, "a failed batch writes nothing");
    assert!(store.revlog(90_000).expect("revlog").is_none());
    assert!(store.revlog(90_001).expect("revlog").is_none());
}

#[test]
fn stats_counts_boundaries_within_a_deck_subtree() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let conn = Connection::open(&path).expect("open");
    conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [now])
        .expect("set crt");
    drop(conn);
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("Study").expect("parent");
    let child = store.create_deck("Study::Kids").expect("child");

    let conn = Connection::open(&path).expect("open");
    for (id, did, queue, due) in [
        (10_i64, parent.id, 0_i64, 7_i64),
        (11, parent.id, 1, now),
        (12, parent.id, 1, now.saturating_add(1)),
        (13, parent.id, 2, 0),
        (14, parent.id, 2, 1),
        (15, parent.id, 3, 0),
        (20, child.id, 0, 7),
        (21, child.id, 1, now),
        (22, child.id, 1, now.saturating_add(1)),
        (23, child.id, 2, 0),
        (24, child.id, 3, 1),
    ] {
        conn.execute(
            "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
             lapses, left, odue, odid, flags, data) VALUES (?1, 1, ?2, 0, 0, 0, 0, ?3, ?4, 0, 0, \
             0, 0, 0, 0, 0, 0, '')",
            [id, did, queue, due],
        )
        .expect("insert card");
    }
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let all = store.stats(Some(parent.id), now).expect("parent stats");
    assert_eq!(all.new_cards, 2);
    assert_eq!(all.learning_cards, 5);
    assert_eq!(all.review_cards, 2);
    assert_eq!(all.total_cards, 11);

    let subtree = store.stats(Some(child.id), now).expect("child stats");
    assert_eq!(subtree.new_cards, 1);
    assert_eq!(subtree.learning_cards, 2);
    assert_eq!(subtree.review_cards, 1);
    assert_eq!(subtree.total_cards, 5);

    let collection = store.stats(None, now).expect("collection stats");
    assert_eq!(collection, all);
    assert_eq!(
        store.stats(Some(999), now).expect("missing deck"),
        DeckStats::default()
    );
}

#[test]
fn due_cards_probe_keeps_priority_and_all_new_order() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let today = 0_i64;
    let conn = Connection::open(&path).expect("open");
    conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [now])
        .expect("set crt");
    for (id, queue, due) in [(10_i64, 0_i64, 7_i64), (11, 0, 3), (30, 2, today)]
    {
        conn.execute(
            "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
             lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, 0, 0, 0, ?2, ?3, 0, 0, \
             0, 0, 0, 0, 0, 0, '')",
            [id, queue, due],
        )
        .expect("insert card");
    }
    conn.execute("DELETE FROM cards WHERE id = 30", [])
        .expect("remove review card");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let due: Vec<_> = store
        .due_cards(None, 2, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(due, [11, 10]);

    let conn = Connection::open(&path).expect("open");
    conn.execute(
        "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
         lapses, left, odue, odid, flags, data) VALUES (30, 1, 1, 0, 0, 0, 0, 2, 0, 0, 0, 0, \
         0, 0, 0, 0, 0, '')",
        [],
    )
    .expect("insert review card");
    drop(conn);
    let due: Vec<_> = store
        .due_cards(None, 2, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(due, [11, 30]);
}

#[test]
fn due_cards_order_same_day_reviews_like_anki() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let conn = Connection::open(&path).expect("open");
    conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [now])
        .expect("set crt");
    // Anki breaks a shared due day with fnvhash(id, mod).
    for (id, modified) in [
        (101_i64, 1_700_000_000_i64),
        (102, 1_700_000_000),
        (103, 1_700_000_500),
        (104, 1_699_999_999),
        (105, 1_700_000_000),
    ] {
        conn.execute(
            "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
             lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, 0, ?2, 0, 2, 2, 0, 1, 2500, \
             1, 0, 0, 0, 0, 0, '')",
            [id, modified],
        )
        .expect("insert card");
    }
    drop(conn);
    let store = Collection::open(&path).expect("open");
    let order: Vec<_> = store
        .due_cards(None, 10, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(order, [101, 103, 105, 104, 102]);
}

#[test]
fn due_cards_order_new_cards_by_template_then_position() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let conn = Connection::open(&path).expect("open");
    for (id, ord, position) in [(10_i64, 1_i64, 0_i64), (11, 0, 1), (12, 0, 0)]
    {
        conn.execute(
            "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
             lapses, left, odue, odid, flags, data) VALUES (?1, 1, 1, ?2, 0, 0, 0, 0, ?3, 0, 0, 0, \
             0, 0, 0, 0, 0, '')",
            [id, ord, position],
        )
        .expect("insert card");
    }
    drop(conn);
    let store = Collection::open(&path).expect("open");
    let order: Vec<_> = store
        .due_cards(None, 10, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(order, [12, 11, 10]);
}

#[test]
fn apply_review_reports_stale_and_missing_cards() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["review", "answer"], &[]))
        .expect("add");
    let now = now_secs();
    let card = store
        .due_cards(None, 1, now)
        .expect("due")
        .pop()
        .expect("card");
    let decision = FsrsScheduler::default().schedule(
        &card,
        Rating::Good,
        now,
        store.collection_created().expect("created"),
    );

    let mut stale = decision.clone();
    stale.expected_modified = card.modified.saturating_add(1);
    let error = store.apply_review(&stale).expect_err("stale review");
    assert!(
        matches!(&error, StoreError::InvalidData(message) if message == "card changed since it was loaded"),
        "{error:?}"
    );

    let mut missing = decision;
    missing.card.id = -1;
    let error = store.apply_review(&missing).expect_err("missing review");
    assert!(
        matches!(&error, StoreError::InvalidData(message) if message == "card changed since it was loaded"),
        "{error:?}"
    );
    assert_eq!(store.review_history().expect("history"), [] as [RevLog; 0]);
}

#[test]
fn reopening_store_leaves_the_collection_free_for_anki() {
    let (_directory, path) = fixture();
    {
        let connection = Connection::open(&path).expect("wal setup");
        connection
            .pragma_update(None, "journal_mode", "wal")
            .expect("wal mode");
    }

    let anki_open = || -> rusqlite::Result<()> {
        let connection = Connection::open(&path)?;
        connection.busy_timeout(Duration::ZERO)?;
        connection.pragma_update(None, "locking_mode", "exclusive")?;
        connection.pragma_update(None, "journal_mode", "wal")?;
        Ok(())
    };

    let held = Collection::open(&path).expect("held collection");
    let _ = held.decks().expect("held decks");
    assert!(
        anki_open().is_err(),
        "a held WAL connection must block Anki's exclusive open"
    );
    drop(held);

    let mut store = ReopeningCollection::new(&path);
    let _ = store.decks().expect("reopening decks");
    anki_open().expect("Anki opens while the store is idle");

    let before = store.collection_modified().expect("before write");
    let deck = store.create_deck("After Anki").expect("create deck");
    assert_eq!(deck.name, "After Anki");
    assert!(store.collection_modified().expect("after write") >= before);
}

#[test]
fn reopening_store_sees_external_wal_commits() {
    let (_directory, path) = fixture();
    let anki = Connection::open(&path).expect("external connection");
    anki.pragma_update(None, "journal_mode", "wal")
        .expect("wal mode");

    let store = ReopeningCollection::new(&path);
    let before = store.collection_modified().expect("before commit");
    let changed = before.saturating_add(10);
    anki.execute("UPDATE col SET mod = ?1 WHERE id = 1", [changed])
        .expect("external commit");

    assert_eq!(store.collection_modified().expect("after commit"), changed);
}

#[test]
fn polling_fails_fast_while_anki_holds_the_collection() {
    let (_directory, path) = fixture();
    let anki = Connection::open(&path).expect("external connection");
    anki.busy_timeout(Duration::ZERO).expect("busy timeout");
    anki.pragma_update(None, "locking_mode", "exclusive")
        .expect("exclusive mode");
    anki.pragma_update(None, "journal_mode", "wal")
        .expect("wal mode");

    let store = ReopeningCollection::new(&path);
    let started = Instant::now();
    assert!(store.collection_modified().is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn deck_steps_read_legacy_preset() {
    let (_directory, path) = fixture();
    let connection = Connection::open(&path).expect("open");
    let dconf = json!({
        "1": {
            "id": 1,
            "new": {"delays": [5.0, 20.0]},
            "lapse": {"delays": [0.5]},
        }
    });
    connection
        .execute(
            "UPDATE col SET decks = ?1, dconf = ?2 WHERE id = 1",
            rusqlite::params![
                json!({
                    "1": {"id": 1, "conf": 1},
                    "2": {"id": 2, "name": "Child", "conf": 0},
                })
                .to_string(),
                dconf.to_string(),
            ],
        )
        .expect("write preset");
    drop(connection);

    let store = Collection::open(&path).expect("open store");
    assert_eq!(
        store.deck_steps(1).expect("steps"),
        DeckSteps {
            learning: vec![300, 1_200],
            relearning: vec![30],
        }
    );
    let expected = DeckSteps {
        learning: vec![300, 1_200],
        relearning: vec![30],
    };
    assert_eq!(store.deck_steps(2).expect("child steps"), expected);
}

#[test]
fn deck_kind_rewrite_updates_length_for_a_large_preset_id() {
    let kind = ranki_storage::normal_deck_kind(2);
    let rewritten =
        ranki_storage::with_deck_kind_config_id(&kind, 702).unwrap();
    assert_eq!(ranki_storage::deck_kind_config_id(&rewritten), Some(702));
}

#[test]
fn deck_steps_read_schema18_preset() {
    let (_directory, path) = fixture();
    let connection = Connection::open(&path).expect("open");
    connection
        .execute_batch(
            "UPDATE col SET decks = '{}', dconf = '{}' WHERE id = 1;
             CREATE TABLE decks (id INTEGER PRIMARY KEY, name TEXT, mtime_secs INTEGER,
                 usn INTEGER, common BLOB, kind BLOB);
             CREATE TABLE deck_config (id INTEGER PRIMARY KEY, name TEXT, mtime_secs INTEGER,
                 usn INTEGER, config BLOB);",
        )
        .expect("create tables");
    let mut config = pb_floats(1, &[5.0, 20.0]);
    config.extend(pb_floats(2, &[0.5]));
    connection
        .execute(
            "INSERT INTO decks (id, name, mtime_secs, usn, common, kind)
             VALUES (42, 'Custom', 0, 0, ?, ?)",
            rusqlite::params![
                Vec::<u8>::new(),
                ranki_storage::normal_deck_kind(7),
            ],
        )
        .expect("insert deck");
    connection
        .execute(
            "INSERT INTO deck_config (id, name, mtime_secs, usn, config)
             VALUES (7, 'Custom', 0, 0, ?)",
            [config],
        )
        .expect("insert preset");
    drop(connection);

    let store = Collection::open(&path).expect("open store");
    assert_eq!(
        store.deck_steps(42).expect("steps"),
        DeckSteps {
            learning: vec![300, 1_200],
            relearning: vec![30],
        }
    );
}

#[test]
fn add_note_enforces_first_field_rules_with_explicit_allowance() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    assert!(matches!(
        store.add_note(&note(&["<b> </b>", "back"], &[])),
        Err(StoreError::InvalidData(_))
    ));
    store
        .add_note(&note(&["Question", "back"], &[]))
        .expect("first note");
    let duplicate = note(&["<b>Question</b>", "back"], &[]);
    assert!(matches!(
        store.add_note(&duplicate),
        Err(StoreError::InvalidData(_))
    ));
    store
        .add_note_with_options(
            &duplicate,
            &AddNoteOptions::allowing_duplicates(),
        )
        .expect("explicitly allowed duplicate");
}

#[test]
fn duplicate_detection_uses_the_stored_checksum_input() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["<b>x", "back"], &[]))
        .expect("markup note");
    assert!(store.add_note(&note(&["x", "back"], &[])).is_err());

    store
        .add_note(&note(&["<img src=\"sound.png\">y", "back"], &[]))
        .expect("media note");
    assert!(
        store
            .add_note(&note(&[" sound.png y", "back"], &[]))
            .is_err()
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one test covers each duplicate-scope flag"
)]
fn duplicate_scope_controls_deck_model_and_invalid_deck_checks() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let other = store.create_deck("Other").expect("deck");
    store
        .add_note(&note(&["Scoped", "back"], &[]))
        .expect("first note");

    let options = AddNoteOptions {
        duplicate_scope: DuplicateScope {
            use_deck: true,
            deck_id: Some(other.id),
            ..DuplicateScope::default()
        },
        ..AddNoteOptions::default()
    };
    store
        .add_note_with_options(&note(&["Scoped", "back"], &[]), &options)
        .expect("different deck is not a duplicate");
    assert!(store.add_note(&note(&["Scoped", "back"], &[])).is_err());

    let _parent_deck = store.create_deck("Parent").expect("parent deck");
    let child_deck = store.create_deck("Parent::Child").expect("child deck");
    let parent = store
        .decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.name == "Parent")
        .expect("parent deck");
    store
        .add_note(&NewNote {
            deck_id: child_deck.id,
            ..note(&["Child scoped", "back"], &[])
        })
        .expect("child note");
    let mut child_scope = DuplicateScope {
        use_deck: true,
        deck_id: Some(parent.id),
        ..DuplicateScope::default()
    };
    child_scope.check_children = true;
    assert!(
        store
            .add_note_with_options(
                &NewNote {
                    deck_id: parent.id,
                    ..note(&["Child scoped", "back"], &[])
                },
                &AddNoteOptions {
                    duplicate_scope: child_scope,
                    ..AddNoteOptions::default()
                },
            )
            .is_err()
    );
    child_scope.check_children = false;
    assert!(
        store
            .add_note_with_options(
                &NewNote {
                    deck_id: parent.id,
                    ..note(&["Child scoped", "back"], &[])
                },
                &AddNoteOptions {
                    duplicate_scope: child_scope,
                    ..AddNoteOptions::default()
                },
            )
            .is_ok()
    );

    store
        .add_note(&note(&["All models", "back"], &[]))
        .expect("note for model scope");
    let note_id = store
        .find_cards("All models")
        .expect("find")
        .first()
        .and_then(|card_id| store.card(*card_id).expect("card"))
        .expect("note")
        .note_id;
    Connection::open(&path)
        .expect("open sqlite")
        .execute("UPDATE notes SET mid = 99 WHERE id = ?1", [note_id])
        .expect("change model");
    let all_models = AddNoteOptions {
        duplicate_scope: DuplicateScope {
            check_all_models: true,
            ..DuplicateScope::default()
        },
        ..AddNoteOptions::default()
    };
    assert!(
        store
            .add_note_with_options(
                &note(&["All models", "back"], &[]),
                &all_models
            )
            .is_err()
    );
    assert!(store.add_note(&note(&["All models", "back"], &[])).is_ok());

    let missing_deck = AddNoteOptions {
        duplicate_scope: DuplicateScope {
            use_deck: true,
            deck_id: None,
            ..DuplicateScope::default()
        },
        ..AddNoteOptions::default()
    };
    assert!(
        store
            .add_note_with_options(
                &note(&["Scoped", "back"], &[]),
                &missing_deck
            )
            .is_ok()
    );
}

#[test]
fn tags_are_canonified_and_registered_in_col_tags() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(
            &["front", "back"],
            &[
                " Foo  BAR ",
                "foo::bar",
                "\u{3000}Ten\u{3000}",
                "X::Y",
                "X",
                "x::z",
                "a\u{0001}b",
            ],
        ))
        .expect("add note");
    let card = store.find_cards("").expect("find")[0];
    let saved = store.card(card).expect("card").expect("exists");
    let note = store.note(saved.note_id).expect("note").expect("exists");
    assert_eq!(
        note.tags,
        ["ab", "BAR", "Foo", "Foo::bar", "Ten", "X", "X::Y", "X::z"]
    );
    assert_eq!(store.tags().expect("tags"), note.tags);

    let conn = read_only(&path);
    let raw: String = conn
        .query_row("SELECT tags FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("col tags");
    let registry: Value = serde_json::from_str(&raw).expect("parse col tags");
    let names: Vec<&str> = registry
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(names.len(), 8);
    assert!(names.contains(&"Foo::bar"));
}

#[test]
fn tags_are_registered_in_schema18_tags_table() {
    let (_directory, path) = fixture();
    {
        let conn = Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE tags (
                 tag TEXT NOT NULL PRIMARY KEY,
                 usn INTEGER NOT NULL,
                 collapsed INTEGER NOT NULL
             );
             UPDATE col SET ver = 18 WHERE id = 1;",
        )
        .expect("schema-18 tags table");
    }
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(
            &["front", "back"],
            &["Existing", "existing::child", "EXISTING::sibling"],
        ))
        .expect("add note");
    let card = store.find_cards("").expect("find")[0];
    let saved = store.card(card).expect("card").expect("exists");
    let note = store.note(saved.note_id).expect("note").expect("exists");
    assert_eq!(
        note.tags,
        ["Existing", "Existing::child", "Existing::sibling"]
    );
    assert_eq!(store.tags().expect("tags"), note.tags);

    let conn = read_only(&path);
    let mut statement = conn
        .prepare("SELECT tag FROM tags ORDER BY tag")
        .expect("prepare");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query");
    let registered: Vec<String> = rows.map(|row| row.expect("row")).collect();
    assert_eq!(registered, note.tags);
}

#[test]
fn update_note_preserves_identity_flags_data_and_card_state() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["old", "back"], &["keep"]))
        .expect("add note");
    let card = store.card(ids[0]).expect("card").expect("exists");
    let connection = Connection::open(&path).expect("open sqlite");
    connection
        .execute(
            "UPDATE notes SET guid = 'stable', flags = 7, data = 'meta' WHERE id = ?1",
            [card.note_id],
        )
        .expect("seed identity");
    connection
        .execute(
            "UPDATE cards SET ivl = 42, reps = 9, queue = 2 WHERE id = ?1",
            [card.id],
        )
        .expect("seed scheduling");
    drop(connection);

    let mut edited = store.note(card.note_id).expect("note").expect("exists");
    edited.fields = vec![String::from("new"), String::from("back")];
    edited.guid = String::from("wrong");
    edited.flags = 0;
    edited.data = String::from("ignored");
    store.update_note(&edited, 1).expect("update");

    let saved = store.note(card.note_id).expect("note").expect("exists");
    assert_eq!(saved.guid, "stable");
    assert_eq!(saved.fields, ["new", "back"]);
    assert_eq!(saved.flags, 7);
    assert_eq!(saved.data, "meta");
    assert_eq!(saved.tags, ["keep"]);
    let saved_card = store.card(card.id).expect("card").expect("exists");
    assert_eq!(saved_card.interval, 42);
    assert_eq!(saved_card.reps, 9);
    assert_eq!(saved_card.queue, ranki_model::CardQueue::Review);
}

#[test]
fn normalize_note_text_can_be_disabled() {
    let (_directory, path) = fixture();
    Connection::open(&path)
        .expect("open sqlite")
        .execute(
            "UPDATE col SET conf = ?1 WHERE id = 1",
            [json!({"normalize_note_text": false}).to_string()],
        )
        .expect("disable normalization");
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["e\u{301}", "back"], &[]))
        .expect("add note");
    let card = store.card(ids[0]).expect("card").expect("exists");
    let saved = store.note(card.note_id).expect("note").expect("exists");
    assert_eq!(saved.fields[0], "e\u{301}");
}

#[test]
fn legacy_checksums_still_detect_duplicates() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let raw = "<img src=\"sound.png\">y";
    let ids = store
        .add_note(&note(&[raw, "back"], &[]))
        .expect("add note");
    let card = store.card(ids[0]).expect("card").expect("exists");
    Connection::open(&path)
        .expect("open sqlite")
        .execute(
            "UPDATE notes SET csum = ?1 WHERE id = ?2",
            rusqlite::params![
                ranki_storage::csum_value(legacy_checksum(raw)),
                card.note_id
            ],
        )
        .expect("write legacy checksum");
    assert!(
        store
            .add_note(&note(&["<img src=\"other.png\">y", "back"], &[]))
            .is_err()
    );
}

#[test]
fn reopening_store_forwards_duplicate_options() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["Question", "back"], &[]))
        .expect("first note");
    let mut reopened = ReopeningCollection::new(&path);
    reopened
        .add_note_with_options(
            &note(&["Question", "back"], &[]),
            &AddNoteOptions::allowing_duplicates(),
        )
        .expect("explicit duplicate through reopening store");
}

#[test]
fn tag_decoder_uses_anki_separators() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store
        .add_note(&note(&["front", "back"], &["one\u{a0}two"]))
        .expect("add note");
    let card = store.find_cards("").expect("find")[0];
    let saved_card = store.card(card).expect("card").expect("exists");
    let saved = store
        .note(saved_card.note_id)
        .expect("note")
        .expect("exists");
    assert_eq!(saved.tags, ["one\u{a0}two"]);
    assert_eq!(store.tags().expect("tags"), ["one\u{a0}two"]);
}

fn pb_floats(field: u32, values: &[f32]) -> Vec<u8> {
    let mut bytes = vec![u8::try_from((field << 3) | 2).expect("tag")];
    bytes.push(
        u8::try_from(values.len().checked_mul(4).expect("packed length"))
            .expect("packed length"),
    );
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

#[test]
fn scheduler_v1_upgrade_fixes_cards_and_revlogs() {
    let (_directory, path) = fixture();
    let card_id = {
        let mut store = Collection::open(&path).expect("open");
        let ids = store
            .add_note(&note(&["Front?", "Answer!"], &[]))
            .expect("note");
        ids[0]
    };
    let conn = Connection::open(&path).expect("write connection");
    let conf_text: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let mut conf_json: Value =
        serde_json::from_str(&conf_text).expect("conf JSON");
    conf_json["schedVer"] = json!(1);
    conf_json
        .as_object_mut()
        .expect("conf object")
        .remove("creationOffset");
    let created = 1_672_531_200_i64;
    conn.execute(
        "UPDATE col SET crt = ?1 WHERE id = 1",
        rusqlite::params![created],
    )
    .expect("set creation stamp");
    let v1_conf = conf_json.to_string();
    conn.execute(
        "UPDATE col SET conf = ?1 WHERE id = 1",
        rusqlite::params![v1_conf],
    )
    .expect("set v1");
    conn.execute(
        "UPDATE cards SET type = 2, queue = 1 WHERE id = ?1",
        rusqlite::params![card_id],
    )
    .expect("v1 relearning card");
    for (id, ease, kind) in [(1_i64, 2_i64, 0_i64), (2, 3, 2), (3, 1, 1)] {
        conn.execute(
            "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type) \
             VALUES (?1, ?2, -1, ?3, 1, 0, 2500, 100, ?4)",
            rusqlite::params![id, card_id, ease, kind],
        )
        .expect("revlog");
    }
    drop(conn);

    let store = Collection::open(&path).expect("upgrade on open");
    let card = store.card(card_id).expect("card").expect("card present");
    assert_eq!(card.card_type, CardType::Relearning);
    let history = store.review_history().expect("history");
    let eases: Vec<u8> = history.iter().map(|log| log.ease).collect();
    assert_eq!(eases, vec![3, 4, 1]);
    let conn = read_only(&path);
    let upgraded_text: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let upgraded: Value =
        serde_json::from_str(&upgraded_text).expect("conf JSON");
    assert_eq!(upgraded["schedVer"], 2);
    assert_eq!(upgraded["sched2021"], true);
    assert_eq!(
        upgraded["creationOffset"],
        ranki_storage::local_minutes_west(created).expect("offset")
    );
}

#[test]
fn ordinary_card_writes_do_not_bump_schema_modification() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["Front?", "Answer!"], &[]))
        .expect("note");
    let card = store.card(ids[0]).expect("card").expect("card present");
    let timing = store.timing(now_secs()).expect("timing");
    let conn = read_only(&path);
    let before: i64 = conn
        .query_row("SELECT scm FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("scm");
    drop(conn);
    let decision = FsrsScheduler::default()
        .schedule_timed(&card, Rating::Good, &timing, None, 0)
        .expect("decision");
    store.apply_review(&decision).expect("apply");
    let conn = read_only(&path);
    let after: i64 = conn
        .query_row("SELECT scm FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("scm");
    assert_eq!(before, after);
}

#[test]
fn card_updates_write_state_without_a_manual_log() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["Front?", "Answer!"], &[]))
        .expect("note");
    let mut card = store.card(ids[0]).expect("card").expect("card present");
    let now = now_secs() + 10;
    card.queue = CardQueue::Suspended;
    store.update_card(&card, now).expect("update");
    let stored = store.card(card.id).expect("card").expect("card present");
    assert_eq!(stored.queue, CardQueue::Suspended);
    assert_eq!(stored.modified, now);
    assert_eq!(stored.usn, -1);
    assert_eq!(store.review_history().expect("history").len(), 0);
}

#[test]
fn scheduler_v2_collections_use_rollover_timing() {
    let (_directory, path) = fixture();
    let now = 1_700_000_000_i64;
    let created = now - 20 * 86_400;
    let v2_conf = json!({
        "schedVer": 2,
        "creationOffset": 0,
        "rollover": 4,
    })
    .to_string();
    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE col SET crt = ?1, conf = ?2 WHERE id = 1",
        rusqlite::params![created, v2_conf],
    )
    .expect("set v2 timing metadata");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let timing = Store::timing(&store, now).expect("timing");
    let expected = ranki_model::Timing::new(
        now,
        created,
        Some(0),
        ranki_storage::local_minutes_west(now).expect("offset"),
        Some(4),
    );
    assert_eq!(timing.days_elapsed, expected.days_elapsed);
    assert_eq!(timing.next_day_at, expected.next_day_at);
}

#[test]
fn scheduler_v2_without_rollover_uses_ankis_four_am_default() {
    let (_directory, path) = fixture();
    let now = 1_700_000_000_i64;
    let created = now - 20 * 86_400;
    let v2_conf = json!({"schedVer": 2, "creationOffset": 0}).to_string();
    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE col SET crt = ?1, conf = ?2 WHERE id = 1",
        rusqlite::params![created, v2_conf],
    )
    .expect("set v2 metadata without rollover");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let timing = Store::timing(&store, now).expect("timing");
    let expected = ranki_model::Timing::new(
        now,
        created,
        Some(0),
        ranki_storage::local_minutes_west(now).expect("offset"),
        Some(4),
    );
    assert_eq!(timing.days_elapsed, expected.days_elapsed);
    assert_eq!(timing.next_day_at, expected.next_day_at);
}

#[test]
fn a_fresh_collection_uses_ankis_default_day_boundary() {
    let (_directory, path) = fixture();
    let store = Collection::open(&path).expect("open");
    let now = now_secs();
    let created = store.collection_created().expect("created");
    let timing = Store::timing(&store, now).expect("timing");
    let expected = ranki_model::Timing::new(
        now,
        created,
        Some(ranki_storage::local_minutes_west(created).expect("offset")),
        ranki_storage::local_minutes_west(now).expect("offset"),
        Some(4),
    );
    assert_eq!(timing.days_elapsed, expected.days_elapsed);
    assert_eq!(timing.next_day_at, expected.next_day_at);
}

#[test]
fn schema18_config_table_supplies_due_counts() {
    let (_directory, path) = fixture();
    let now = chrono::Local
        .with_ymd_and_hms(2023, 11, 14, 12, 0, 0)
        .single()
        .expect("local timestamp")
        .timestamp();
    let created = now - 20 * 86_400;
    let current_offset =
        ranki_storage::local_minutes_west(now).expect("offset");
    let rollover = 13;
    write_schema18_config(
        &path,
        &[
            ("schedVer", json!(2)),
            ("creationOffset", json!(current_offset)),
            ("rollover", json!(rollover)),
        ],
    );
    let expected = ranki_model::Timing::new(
        now,
        created,
        Some(current_offset),
        current_offset,
        Some(rollover),
    );
    assert_eq!(expected.days_elapsed, 19);

    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE col SET crt = ?1 WHERE id = 1",
        rusqlite::params![created],
    )
    .expect("set creation time");
    for id in 0..17_i64 {
        conn.execute(
            "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, \
             reps, lapses, left, odue, odid, flags, data) \
             VALUES (?1, 1, 1, 0, 0, 0, 2, 2, ?2, 0, 0, 0, 0, 0, 0, 0, 0, '')",
            rusqlite::params![1_000 + id, expected.days_elapsed],
        )
        .expect("insert due card");
    }
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let legacy_conf: String = Connection::open(&path)
        .expect("read connection")
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("legacy config unchanged");
    assert_eq!(legacy_conf, "{}");
    assert_eq!(Store::timing(&store, now).expect("timing"), expected);
    assert_eq!(store.stats(None, now).expect("stats").review_cards, 17);
    assert_eq!(store.due_cards(None, 20, now).expect("due").len(), 17);
}

#[test]
fn last_review_time_ignores_non_answer_logs() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["Front?", "Answer!"], &[]))
        .expect("note");
    let card_id = ids[0];
    let conn = Connection::open(&path).expect("write connection");
    for (id, ease, kind, factor) in [
        (1_000_000_i64, 3_i64, 1_i64, 2_500_i64),
        (2_000_000, 0, 5, 0),
        (3_000_000, 4, 3, 0),
    ] {
        conn.execute(
            "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type) \
             VALUES (?1, ?2, -1, ?3, 1, 0, ?4, 100, ?5)",
            rusqlite::params![id, card_id, ease, factor, kind],
        )
        .expect("revlog");
    }
    drop(conn);
    assert_eq!(
        Store::last_review_secs(&store, card_id).expect("last review"),
        Some(1_000)
    );
}

#[test]
fn timing_metadata_is_cached_until_an_upgrade() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    pin_v1_timing(&path);
    let now = 1_700_000_000_i64;
    let created = store.collection_created().expect("created");
    let before = Store::timing(&store, now).expect("v1 timing");
    assert_eq!(before, ranki_model::Timing::v1(now, created));

    assert!(store.upgrade_scheduler_v2().expect("upgrade"));
    let after = Store::timing(&store, now).expect("v2 timing");
    assert_ne!(after.next_day_at, before.next_day_at);
}

#[test]
fn repeated_timing_calls_reuse_cached_configuration() {
    let (_directory, path) = fixture();
    let store = Collection::open(&path).expect("open");
    let now = 1_700_000_000_i64;
    let before = Store::timing(&store, now).expect("timing");

    let conn = Connection::open(&path).expect("write connection");
    let text: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let mut config: Value = serde_json::from_str(&text).expect("conf JSON");
    config
        .as_object_mut()
        .expect("conf object")
        .insert(String::from("rollover"), json!(12));
    conn.execute(
        "UPDATE col SET conf = ?1 WHERE id = 1",
        rusqlite::params![config.to_string()],
    )
    .expect("write rollover");
    drop(conn);

    let after = Store::timing(&store, now).expect("cached timing");
    assert_eq!(after.next_day_at, before.next_day_at);
}

#[test]
fn timing_cache_is_keyed_by_exact_now() {
    let (_directory, path) = fixture();
    let store = Collection::open(&path).expect("open");
    let now = 1_700_000_000_i64;
    let before = Store::timing(&store, now).expect("timing");

    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE col SET crt = ?1 WHERE id = 1",
        rusqlite::params![now - 86_400],
    )
    .expect("change creation stamp");
    drop(conn);

    let same = Store::timing(&store, now).expect("cached timing");
    assert_eq!(same, before);
    let later = Store::timing(&store, now + 1).expect("recomputed timing");
    assert_eq!(later.now, now + 1);
}

#[test]
fn malformed_conf_does_not_start_an_upgrade() {
    let (_directory, path) = fixture();
    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE col SET conf = ?1 WHERE id = 1",
        rusqlite::params!["not json"],
    )
    .expect("write malformed conf");
    drop(conn);

    let mut store = Collection::open(&path).expect("open");
    assert!(!store.upgrade_scheduler_v2().expect("no upgrade"));
}

#[test]
fn reopening_store_batches_cards_and_counts_reviews() {
    let (_directory, path) = fixture();
    let mut collection = Collection::open(&path).expect("open");
    let ids = collection
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("front"), String::from("back")],
            tags: Vec::new(),
        })
        .expect("add note");
    let card = collection.card(ids[0]).expect("card").expect("card");
    drop(collection);

    let store = ReopeningCollection::new(&path);
    let loaded = store
        .cards_with_notes(&[card.id, card.id.saturating_add(1_000)])
        .expect("batch load");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0.id, card.id);
    assert_eq!(loaded[0].1.id, card.note_id);

    let connection = Connection::open(&path).expect("write connection");
    connection
        .execute(
            "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type)
             VALUES (100, ?1, 0, 3, 1, 0, 2500, 1000, 0)",
            [card.id],
        )
        .expect("insert review");
    connection
        .execute(
            "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type)
             VALUES (200, ?1, 0, 3, 1, 0, 2500, 1000, 0)",
            [card.id],
        )
        .expect("insert reviews");
    drop(connection);
    assert_eq!(store.review_count_since(150).expect("count"), 1);
}
fn set_conf_values(path: &Path, values: &[(&str, Value)]) {
    let conn = Connection::open(path).expect("write connection");
    let raw: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let mut parsed: Value = serde_json::from_str(&raw).expect("conf JSON");
    let object = parsed.as_object_mut().expect("conf object");
    for (key, value) in values {
        let _ = object.insert((*key).to_owned(), value.clone());
    }
    let stored = parsed.to_string();
    conn.execute(
        "UPDATE col SET conf = ?1 WHERE id = 1",
        rusqlite::params![stored],
    )
    .expect("store conf");
}

#[expect(
    clippy::too_many_arguments,
    reason = "the fixture inserts one complete card row"
)]
fn insert_queue_card(
    conn: &Connection,
    id: i64,
    note_id: i64,
    deck_id: i64,
    card_type: i64,
    queue: i64,
    due: i64,
) {
    conn.execute(
        "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
         lapses, left, odue, odid, flags, data) VALUES (?1, ?2, ?3, 0, 0, 0, ?4, ?5, ?6, 1, \
         2500, 0, 0, 0, 0, 0, 0, '')",
        rusqlite::params![id, note_id, deck_id, card_type, queue, due],
    )
    .expect("insert card");
}

#[test]
fn due_cards_applies_remaining_daily_limits_and_parent_limits() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("Parent").expect("parent");
    let child = store.create_deck("Parent::Child").expect("child");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_per_day = 2;
    preset.reviews_per_day = 5;
    store.write_preset(&preset).expect("write preset");
    {
        let conn = Connection::open(&path).expect("write connection");
        for id in 10..15 {
            insert_queue_card(&conn, id, id, child.id, 0, 0, id);
        }
        for id in 20..23 {
            insert_queue_card(&conn, id, id, child.id, 2, 2, 0);
        }
    }

    let ids: Vec<_> = store
        .due_cards(Some(child.id), 50, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(ids.len(), 5, "two new and three review cards remain");
    assert_eq!(ids.iter().filter(|id| **id < 15).count(), 2);
    assert_eq!(ids.iter().filter(|id| **id >= 20).count(), 3);

    let mut parent_deck = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == parent.id)
        .expect("parent deck");
    parent_deck.overrides.new_limit = Some(1);
    store.write_deck(&parent_deck).expect("write parent");
    set_conf_values(&path, &[("applyAllParentLimits", json!(true))]);
    let ids = store
        .due_cards(Some(child.id), 50, now)
        .expect("limited by parent");
    assert_eq!(
        ids.iter()
            .filter(|card| card.queue == CardQueue::New)
            .count(),
        1
    );

    set_conf_values(&path, &[("newCardsIgnoreReviewLimit", json!(true))]);
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.reviews_per_day = 0;
    store.write_preset(&preset).expect("zero review limit");
    let ids = store
        .due_cards(Some(child.id), 50, now)
        .expect("new ignores review limit");
    assert!(ids.iter().any(|card| card.queue == CardQueue::New));
    assert!(ids.iter().all(|card| card.queue != CardQueue::Review));
}

#[test]
fn answering_updates_today_counters_and_the_next_batch() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_per_day = 1;
    store.write_preset(&preset).expect("write preset");
    let ids = store
        .add_note(&note(&["Front?", "Answer!"], &[]))
        .expect("note");
    let card = store.card(ids[0]).expect("card").expect("present");
    let timing = store.timing(now).expect("timing");
    let decision = FsrsScheduler::default()
        .schedule_timed(&card, Rating::Good, &timing, None, 0)
        .expect("decision");
    store.apply_review(&decision).expect("apply");

    let deck = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == 1)
        .expect("default deck");
    assert_eq!(deck.common.new_studied, 1);
    assert_eq!(deck.overrides.new_today.today, 1);
    assert!(
        store
            .due_cards(None, 20, now)
            .expect("due")
            .iter()
            .all(|card| card.queue != CardQueue::New)
    );
}

#[test]
fn scheduled_siblings_are_buried_and_restored_after_rollover() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.bury_new = true;
    store.write_preset(&preset).expect("write preset");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 1, 1, 0, 0, 1);
        insert_queue_card(&conn, 11, 1, 1, 0, 0, 2);
        insert_queue_card(&conn, 12, 2, 1, 0, -3, 3);
    }
    let first = store
        .due_cards(None, 20, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect::<Vec<_>>();
    assert_eq!(first, [10], "only the first sibling is gathered");

    let card = store.card(10).expect("card").expect("present");
    let timing = store.timing(now).expect("timing");
    let decision = FsrsScheduler::default()
        .schedule_timed(&card, Rating::Good, &timing, None, 0)
        .expect("decision");
    store.apply_review(&decision).expect("apply");
    let queue: i64 = {
        let conn = read_only(&path);
        conn.query_row("SELECT queue FROM cards WHERE id = 11", [], |row| {
            row.get(0)
        })
        .expect("buried sibling")
    };
    assert_eq!(queue, -2);

    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE col SET crt = ?1 WHERE id = 1",
        rusqlite::params![now.saturating_sub(86_400 * 2)],
    )
    .expect("roll over a day");
    drop(conn);
    let store = Collection::open(&path).expect("reopen after rollover");
    let _ = store.due_cards(None, 20, now).expect("after rollover");
    let conn = read_only(&path);
    let restored: i64 = conn
        .query_row("SELECT queue FROM cards WHERE id = 11", [], |row| {
            row.get(0)
        })
        .expect("restored sibling");
    let manual: i64 = conn
        .query_row("SELECT queue FROM cards WHERE id = 12", [], |row| {
            row.get(0)
        })
        .expect("manual burial");
    assert_eq!(restored, 0);
    assert_eq!(manual, 0, "Anki restores user buries at rollover too");
    let last: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let parsed: Value = serde_json::from_str(&last).expect("conf JSON");
    assert!(
        parsed
            .get("lastUnburied")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            > 0
    );
}

#[test]
fn filtered_cards_count_in_their_home_deck_on_original_due() {
    let (_directory, path) = fixture();
    let now = now_secs();
    pin_v1_timing(&path);
    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, \
         lapses, left, odue, odid, flags, data) VALUES (10, 1, 2, 0, 0, 0, 2, 2, 90, 1, 2500, \
         0, 0, 0, 0, 1, 0, ''), (11, 2, 2, 0, 0, 0, 2, 2, 90, 1, 2500, 0, 0, 0, 5, 1, 0, ''), \
         (12, 3, 1, 0, 0, 0, 2, 2, 0, 1, 2500, 0, 0, 0, 0, 0, 0, '')",
        [],
    )
    .expect("insert filtered cards");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let stats = store.stats(Some(1), now).expect("home deck stats");
    assert_eq!(
        stats.review_cards, 2,
        "filtered due day and home review count"
    );
    assert_eq!(stats.total_cards, 3);
    let totals = store.deck_stats(now).expect("deck stats");
    assert_eq!(
        totals
            .into_iter()
            .find(|(id, _)| *id == 1)
            .map(|(_, stats)| stats),
        Some(stats)
    );
    assert_eq!(
        store.due_forecast(7, now).expect("forecast")[0],
        2,
        "only cards due on the original day are in today's bucket"
    );
}

#[test]
fn is_due_and_type_filters_use_queues_and_learn_ahead() {
    let (_directory, path) = fixture();
    let now = now_secs();
    set_conf_values(&path, &[("collapseTime", json!(1200))]);
    let conn = Connection::open(&path).expect("write connection");
    conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [now])
        .expect("set creation");
    for id in 10..=16 {
        conn.execute(
            "INSERT INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data) \
             VALUES (?1, ?2, 1, 0, 0, '', 'front', 'front', 0, 0, '')",
            rusqlite::params![id, format!("guid-{id}")],
        )
        .expect("insert note");
    }
    insert_queue_card(&conn, 10, 10, 1, 2, -1, 0);
    insert_queue_card(&conn, 11, 11, 1, 3, -1, 0);
    insert_queue_card(&conn, 12, 12, 1, 2, 2, 0);
    insert_queue_card(&conn, 13, 13, 1, 1, 1, now.saturating_add(600));
    insert_queue_card(&conn, 14, 14, 1, 1, 1, now.saturating_add(3_600));
    insert_queue_card(&conn, 15, 15, 1, 0, 0, 1);
    insert_queue_card(&conn, 16, 16, 1, 1, 4, now.saturating_add(300));
    drop(conn);

    let store = Collection::open(&path).expect("open");
    assert_eq!(store.learn_ahead_secs().expect("learn ahead"), 1200);
    assert_eq!(
        store.find_cards("is:review").expect("review type"),
        [10, 11, 12]
    );
    assert_eq!(
        store.find_cards("is:learn").expect("learning types"),
        [11, 13, 14, 16]
    );
    assert_eq!(store.find_cards("is:due").expect("due cards"), [12, 13, 16]);
}

#[test]
fn random_new_card_insert_order_uses_anki_positions() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_card_insert_order = 1;
    store.write_preset(&preset).expect("write preset");
    set_conf_values(&path, &[("nextPos", json!(5))]);

    let ids = store
        .add_note(&note(&["Front?", "Answer!"], &[]))
        .expect("note");
    let positions: Vec<_> = ids
        .into_iter()
        .map(|id| store.card(id).expect("card").expect("present").due)
        .collect();
    assert_ne!(positions, [] as [i64; 0]);
    assert!(
        positions.iter().all(|position| *position == 180),
        "Anki's seeded position for nextPos 5"
    );
}

#[test]
fn external_option_commit_invalidates_the_queue_cache() {
    let (_directory, path) = fixture();
    let now = now_secs();
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 10, 1, 0, 0, 1);
        insert_queue_card(&conn, 11, 11, 1, 0, 0, 2);
    }
    let store = Collection::open(&path).expect("open");
    let warm = store.due_cards(None, 20, now).expect("warm cache");
    assert_eq!(warm.len(), 2);

    let conn = Connection::open(&path).expect("external writer");
    let dconf: String = conn
        .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("dconf");
    let modified: i64 = conn
        .query_row("SELECT mod FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("mod");
    let mut value: Value = serde_json::from_str(&dconf).expect("dconf JSON");
    let preset = value
        .as_object_mut()
        .expect("preset object")
        .get_mut("1")
        .expect("default preset");
    preset["new"]["perDay"] = json!(0);
    let stored = value.to_string();
    conn.execute(
        "UPDATE col SET dconf = ?1, mod = ?2 WHERE id = 1",
        rusqlite::params![stored, modified.saturating_add(1)],
    )
    .expect("external commit");
    drop(conn);

    let after = store
        .due_cards(None, 20, now)
        .expect("after external commit");
    assert!(after.is_empty(), "the cached preset must be refreshed");
}

#[test]
fn today_limit_day_number_is_not_a_count() {
    let (_directory, path) = fixture();
    let now = now_secs();
    pin_v1_timing(&path);
    {
        let conn = Connection::open(&path).expect("write connection");
        for id in 10..13 {
            insert_queue_card(&conn, id, id, 1, 0, 0, id);
        }
    }
    let set_limit = |day: i64| {
        let conn = Connection::open(&path).expect("write connection");
        let raw: String = conn
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("decks");
        let mut decks: Value = serde_json::from_str(&raw).expect("decks JSON");
        decks["1"]["newLimitToday"] = json!([0, day]);
        let stored = decks.to_string();
        conn.execute(
            "UPDATE col SET decks = ?1 WHERE id = 1",
            rusqlite::params![stored],
        )
        .expect("store decks");
    };
    set_limit(0);
    let store = Collection::open(&path).expect("open");
    assert!(
        store
            .due_cards(None, 20, now)
            .expect("zero today limit")
            .is_empty(),
        "a zero limit on today must stop new cards"
    );
    set_limit(5);
    let store = Collection::open(&path).expect("reopen");
    assert_eq!(store.due_cards(None, 20, now).expect("other day").len(), 3);
}

#[test]
fn missing_review_ignore_flag_defaults_to_false() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_per_day = 2;
    preset.reviews_per_day = 0;
    store.write_preset(&preset).expect("write preset");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 10, 1, 0, 0, 1);
        insert_queue_card(&conn, 11, 11, 1, 0, 0, 2);
    }
    let default = store.due_cards(None, 20, now).expect("default flags");
    assert!(
        default.iter().all(|card| card.queue != CardQueue::New),
        "new cards stay capped by the zero review limit"
    );
    set_conf_values(&path, &[("newCardsIgnoreReviewLimit", json!(true))]);
    let store = Collection::open(&path).expect("reopen");
    let ignored = store.due_cards(None, 20, now).expect("ignore flag");
    assert_eq!(ignored.len(), 2);
}

#[test]
fn review_order_deck_then_day_uses_deck_preorder() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.review_order = 2;
    store.write_preset(&preset).expect("write preset");
    let parent = store.create_deck("Parent").expect("parent");
    let child = store.create_deck("Parent::Child").expect("child");
    let grandchild = store
        .create_deck("Parent::Child::Grandchild")
        .expect("grandchild");
    let child2 = store.create_deck("Parent::Child2").expect("child2");
    {
        let conn = Connection::open(&path).expect("write connection");
        for (index, deck) in [parent.id, child.id, grandchild.id, child2.id]
            .into_iter()
            .enumerate()
        {
            let id = i64::try_from(index).expect("id") + 20;
            insert_queue_card(&conn, id, id, deck, 2, 2, 0);
        }
    }
    let ids: Vec<_> = store
        .due_cards(Some(parent.id), 20, now)
        .expect("due")
        .into_iter()
        .map(|card| card.id)
        .collect();
    assert_eq!(ids, [20, 21, 22, 23], "Child precedes Child2 in preorder");
}

#[test]
fn undo_restores_scheduled_burials_but_not_manual_burials() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.bury_new = true;
    store.write_preset(&preset).expect("write preset");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 1, 1, 0, 0, 1);
        insert_queue_card(&conn, 11, 1, 1, 0, 0, 2);
        insert_queue_card(&conn, 12, 2, 1, 0, -3, 3);
        insert_queue_card(&conn, 13, 2, 1, 0, 0, 4);
    }
    let before = store.card(10).expect("card").expect("present");
    let timing = store.timing(now).expect("timing");
    let decision = FsrsScheduler::default()
        .schedule_timed(&before, Rating::Good, &timing, None, 0)
        .expect("decision");
    let mut decision = decision;
    decision.revlog.id = decision.card.modified.saturating_mul(1_000);
    let log_id = store.apply_review(&decision).expect("apply");
    let queue = |id| {
        let conn = read_only(&path);
        conn.query_row("SELECT queue FROM cards WHERE id = ?1", [id], |row| {
            row.get::<_, i64>(0)
        })
        .expect("queue")
    };
    assert_eq!(queue(11), -2, "scheduled sibling burial");
    assert_eq!(queue(12), -3, "manual burial survives the answer");
    assert!(store.undo_review(&before, log_id).expect("undo"));
    assert_eq!(queue(11), 0, "undo restores scheduled burial");
    assert_eq!(queue(12), -3, "undo does not unbury manual state");
}

#[test]
fn unknown_deck_id_has_no_due_cards() {
    let (_directory, path) = fixture();
    let now = now_secs();
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 10, 1, 0, 0, 1);
    }
    let store = Collection::open(&path).expect("open");
    assert_eq!(
        store.due_cards(Some(999), 20, now).expect("missing deck"),
        [] as [ranki_model::Card; 0]
    );
}

#[test]
fn intraday_learning_keeps_attempted_cards_ahead() {
    let (_directory, path) = fixture();
    let now = now_secs();
    {
        let conn = Connection::open(&path).expect("write connection");
        for (id, due, reps) in [(10, now.saturating_add(1), 1), (11, now, 0)] {
            conn.execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, \
                 reps, lapses, left, odue, odid, flags, data) VALUES (?1, ?2, 1, 0, 0, 0, 1, 1, \
                 ?3, 0, 2500, ?4, 0, 1, 0, 0, 0, '')",
                rusqlite::params![id, id, due, reps],
            )
            .expect("insert learning card");
        }
    }
    let store = Collection::open(&path).expect("open");
    let due = store.due_cards(None, 1, now).expect("due");
    assert_eq!(
        due.into_iter().map(|card| card.id).collect::<Vec<_>>(),
        [10]
    );
}

#[test]
fn sibling_burial_invalidates_a_stale_review_decision() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.bury_new = true;
    store.write_preset(&preset).expect("preset");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 1, 1, 0, 0, 1);
        insert_queue_card(&conn, 11, 1, 1, 0, 0, 2);
    }
    let sibling = store.card(11).expect("sibling").expect("present");
    let timing = store.timing(now).expect("timing");
    let mut stale = FsrsScheduler::default()
        .schedule_timed(&sibling, Rating::Good, &timing, None, 0)
        .expect("decision");
    stale.revlog.id = decision_time(&stale, now);
    let first = store.card(10).expect("card").expect("present");
    let mut decision = FsrsScheduler::default()
        .schedule_timed(&first, Rating::Good, &timing, None, 0)
        .expect("decision");
    decision.revlog.id = decision_time(&decision, now);
    store.apply_review(&decision).expect("apply first answer");
    assert!(
        store.apply_review(&stale).is_err(),
        "the buried sibling's old decision is stale"
    );
}

#[must_use]
pub const fn decision_time(decision: &ReviewDecision, fallback: i64) -> i64 {
    let secs = if decision.card.modified > 0 {
        decision.card.modified
    } else {
        fallback
    };
    secs.saturating_mul(1_000)
}

#[test]
fn undo_does_not_overwrite_later_burial_state() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.bury_new = true;
    store.write_preset(&preset).expect("preset");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 1, 1, 0, 0, 1);
        insert_queue_card(&conn, 11, 1, 1, 0, 0, 2);
        insert_queue_card(&conn, 12, 1, 1, 0, -2, 3);
    }
    let card = store.card(10).expect("card").expect("present");
    let timing = store.timing(now).expect("timing");
    let mut decision = FsrsScheduler::default()
        .schedule_timed(&card, Rating::Good, &timing, None, 0)
        .expect("decision");
    decision.revlog.id = decision_time(&decision, now);
    let log_id = store.apply_review(&decision).expect("apply");
    {
        let conn = Connection::open(&path).expect("write connection");
        conn.execute(
            "UPDATE cards SET queue = -3, mod = ?1 WHERE id = 11",
            rusqlite::params![now.saturating_add(10)],
        )
        .expect("later manual burial");
    }
    assert!(store.undo_review(&card, log_id).expect("undo"));
    let queue = |id| {
        let conn = read_only(&path);
        conn.query_row("SELECT queue FROM cards WHERE id = ?1", [id], |row| {
            row.get::<_, i64>(0)
        })
        .expect("queue")
    };
    assert_eq!(queue(11), -3, "a later manual burial is untouched");
    assert_eq!(queue(12), -2, "an earlier burial is untouched");
}

#[test]
fn undoing_an_older_answer_keeps_today_counter() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let older_ids = store
        .add_note(&note(&["Older?", "Answer!"], &[]))
        .expect("older note");
    let newer_ids = store
        .add_note(&note(&["Newer?", "Answer!"], &[]))
        .expect("newer note");
    let older = store
        .card(older_ids[0])
        .expect("older card")
        .expect("present");
    let newer = store
        .card(newer_ids[0])
        .expect("newer card")
        .expect("present");
    let timing = store.timing(now).expect("timing");
    let mut first = FsrsScheduler::default()
        .schedule_timed(&older, Rating::Good, &timing, None, 0)
        .expect("older decision");
    first.revlog.id = now.saturating_sub(86_400 * 3).saturating_mul(1_000);
    let first_log = store.apply_review(&first).expect("older answer");
    {
        let conn = Connection::open(&path).expect("write connection");
        conn.execute(
            "UPDATE col SET crt = ?1 WHERE id = 1",
            rusqlite::params![now.saturating_sub(86_400 * 2)],
        )
        .expect("roll to a later study day");
    }
    let mut store = Collection::open(&path).expect("reopen after rollover");

    let mut second = FsrsScheduler::default()
        .schedule_timed(&newer, Rating::Good, &timing, None, 0)
        .expect("newer decision");
    second.revlog.id = decision_time(&second, now);
    store.apply_review(&second).expect("newer answer");

    let deck = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == 1)
        .expect("deck");
    let counted_day = deck.common.last_day_studied;
    let counted = deck.common.new_studied;
    assert!(counted > 0, "the newer answer charged today's counter");

    assert!(
        store
            .undo_review(&older, first_log)
            .expect("undo older answer")
    );
    let deck = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == 1)
        .expect("deck");
    assert_eq!(deck.common.new_studied, counted, "newer count is unchanged");
    assert_eq!(
        deck.common.last_day_studied, counted_day,
        "study day unchanged"
    );
}

#[test]
fn due_gather_scans_past_skipped_candidates() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("Parent").expect("parent");
    let child = store.create_deck("Parent::Child").expect("child");
    let mut child_deck = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == child.id)
        .expect("child deck");
    child_deck.overrides.new_limit = Some(0);
    store.write_deck(&child_deck).expect("zero child limit");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_card_gather_priority = 2;
    store.write_preset(&preset).expect("highest position first");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 10, child.id, 0, 0, 10);
        insert_queue_card(&conn, 11, 11, parent.id, 0, 0, 1);
    }
    let due = store
        .due_cards(Some(parent.id), 1, now)
        .expect("scan past child");
    assert_eq!(
        due.into_iter().map(|card| card.id).collect::<Vec<_>>(),
        [11]
    );

    let mut preset = store.presets().expect("presets")[0].clone();
    preset.bury_new = true;
    store.write_preset(&preset).expect("bury new");
    let store = Collection::open(&path).expect("reopen");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 20, 100, 1, 0, 0, 1);
        insert_queue_card(&conn, 21, 100, 1, 0, 0, 2);
        insert_queue_card(&conn, 22, 101, 1, 0, 0, 3);
    }
    let due = store
        .due_cards(None, 2, now)
        .expect("scan past buried siblings");
    let ids: Vec<_> = due.into_iter().map(|card| card.id).collect();
    assert!(
        ids.contains(&22),
        "later eligible new card survives: {ids:?}"
    );
}

#[test]
fn caller_timestamp_does_not_persist_rollover_state() {
    let (_directory, path) = fixture();
    let now = now_secs();
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 10, 1, 0, -2, 1);
    }
    let store = Collection::open(&path).expect("open");
    let _ = store
        .due_cards(None, 20, now.saturating_add(86_400 * 5))
        .expect("future read view");
    let conn = read_only(&path);
    let queue: i64 = conn
        .query_row("SELECT queue FROM cards WHERE id = 10", [], |row| {
            row.get(0)
        })
        .expect("queue");
    assert_eq!(queue, -2, "a caller timestamp must not unbury cards");
    let stored_conf: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let parsed: Value = serde_json::from_str(&stored_conf).expect("conf JSON");
    assert!(parsed.get("lastUnburied").is_none());
}

#[test]
fn ancestor_decks_contribute_limits_not_cards() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let parent = store.create_deck("Parent").expect("parent");
    let child = store.create_deck("Parent::Child").expect("child");
    set_conf_values(&path, &[("applyAllParentLimits", json!(true))]);
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 10, parent.id, 0, 0, 1);
        insert_queue_card(&conn, 11, 11, child.id, 0, 0, 2);
    }
    let store = Collection::open(&path).expect("reopen");
    let due = store
        .due_cards(Some(child.id), 10, now)
        .expect("child candidates");
    assert_eq!(
        due.into_iter().map(|card| card.id).collect::<Vec<_>>(),
        [11]
    );
}

#[test]
fn all_decks_has_no_default_root_cap() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_per_day = 1;
    store.write_preset(&preset).expect("preset");
    let other = store.create_deck("Other").expect("other deck");
    {
        let conn = Connection::open(&path).expect("write connection");
        insert_queue_card(&conn, 10, 10, 1, 0, 0, 1);
        insert_queue_card(&conn, 11, 11, other.id, 0, 0, 2);
    }
    let due = store.due_cards(None, 10, now).expect("all decks");
    let ids: Vec<_> = due.into_iter().map(|card| card.id).collect();
    assert_eq!(ids, [10, 11], "each deck keeps its own budget");
}

#[test]
fn preview_cards_stay_out_of_review_sessions() {
    let (_directory, path) = fixture();
    let now = now_secs();
    {
        let conn = Connection::open(&path).expect("write connection");
        for id in 10..=11 {
            conn.execute(
                "INSERT INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data) \
                 VALUES (?1, ?2, 1, 0, 0, '', 'front', 'front', 0, 0, '')",
                rusqlite::params![id, format!("guid-{id}")],
            )
            .expect("insert note");
        }
        insert_queue_card(&conn, 10, 10, 1, 1, 4, now);
        insert_queue_card(&conn, 11, 11, 1, 0, 0, 1);
    }
    let store = Collection::open(&path).expect("open");
    let due = store.due_cards(None, 10, now).expect("due");
    assert_eq!(
        due.into_iter().map(|card| card.id).collect::<Vec<_>>(),
        [11]
    );
    assert_eq!(
        store.find_cards("is:due").expect("is:due"),
        [10],
        "preview is still visible to Anki-style search"
    );
    let stats = store.stats(Some(1), now).expect("stats");
    assert_eq!(
        stats.learning_cards, 0,
        "preview cannot select a study deck"
    );
}

#[test]
fn stale_today_limit_still_subtracts_todays_studies() {
    let (_directory, path) = fixture();
    let now = now_secs();
    let store = Collection::open(&path).expect("open");
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_per_day = 1;
    store.write_preset(&preset).expect("preset");
    {
        let conn = Connection::open(&path).expect("write connection");
        conn.execute(
            "UPDATE col SET crt = ?1 WHERE id = 1",
            rusqlite::params![now.saturating_sub(86_400 * 2)],
        )
        .expect("day 2 collection");
        let decks = json!({
            "1": {"id": 1, "name": "Default", "conf": 1,
                  "newToday": [2, 1], "revToday": [2, 0],
                  "newLimitToday": [5, 0]}
        });
        conn.execute(
            "UPDATE col SET decks = ?1 WHERE id = 1",
            rusqlite::params![decks.to_string()],
        )
        .expect("stale override with one study today");
        insert_queue_card(&conn, 10, 10, 1, 0, 0, 1);
    }
    let store = Collection::open(&path).expect("reopen");
    let due = store.due_cards(None, 10, now).expect("remaining limit");
    assert!(due.is_empty(), "today's one study still consumes the cap");
}

#[test]
fn reopening_forwards_learn_ahead_from_collection() {
    let (_directory, path) = fixture();
    set_conf_values(&path, &[("collapseTime", json!(0))]);
    let store = ReopeningCollection::new(&path);
    assert_eq!(store.learn_ahead_secs().expect("forwarded"), 0);
}

#[test]
fn browse_is_due_uses_the_same_timing_as_find_cards() {
    let (_directory, path) = fixture();
    let now = now_secs();
    {
        let conn = Connection::open(&path).expect("write connection");
        conn.execute("UPDATE col SET crt = ?1 WHERE id = 1", [now])
            .expect("creation");
        for id in 10..=11 {
            conn.execute(
                "INSERT INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data) \
                 VALUES (?1, ?2, 1, 0, 0, '', 'front', 'front', 0, 0, '')",
                rusqlite::params![id, format!("guid-{id}")],
            )
            .expect("note");
        }
        insert_queue_card(&conn, 10, 10, 1, 2, 2, 0);
        insert_queue_card(&conn, 11, 11, 1, 1, 1, now.saturating_add(600));
    }
    let store = Collection::open(&path).expect("open");
    let ids: Vec<_> = store
        .browse_cards("is:due")
        .expect("browse due")
        .into_iter()
        .map(|card| card.card_id)
        .collect();
    assert_eq!(ids, [10, 11]);
}

fn mark_card_as_review(path: &Path, card_id: i64, lapses: u32) {
    let conn = Connection::open(path).expect("write connection");
    conn.execute(
        "UPDATE cards SET type = 2, queue = 2, lapses = ?1 WHERE id = ?2",
        rusqlite::params![lapses, card_id],
    )
    .expect("review card");
}

fn again_decision(card: &Card, log_id: i64) -> ReviewDecision {
    let mut changed = card.clone();
    changed.lapses = changed.lapses.saturating_add(1);
    ReviewDecision {
        card: changed,
        expected_modified: card.modified,
        expected_usn: card.usn,
        revlog: RevLog {
            id: log_id,
            card_id: card.id,
            usn: 0,
            ease: 1,
            interval: 1,
            last_interval: card.interval.max(1),
            factor: 2500,
            time_ms: 100,
            kind: RevLogKind::Review,
        },
    }
}

fn note_tags(store: &Collection, note_id: i64) -> Vec<String> {
    store
        .note(note_id)
        .expect("note")
        .expect("note exists")
        .tags
}

#[test]
fn review_leech_marks_the_note_without_changing_the_queue() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["leech me", "answer"], &[]))
        .expect("add");
    mark_card_as_review(&path, ids[0], 7);
    let card = store.card(ids[0]).expect("card").expect("exists");
    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE notes SET mod = 1 WHERE id = ?1",
        rusqlite::params![card.note_id],
    )
    .expect("older note stamp");
    drop(conn);
    let before_note = store.note(card.note_id).expect("note").expect("exists");
    let decision = again_decision(&card, 9_100);
    assert_eq!(decision.card.lapses, 8);

    store.apply_review(&decision).expect("apply");

    assert_eq!(
        store.card(ids[0]).expect("card").expect("exists").queue,
        CardQueue::Review
    );
    assert_eq!(note_tags(&store, card.note_id), ["leech"]);
    let marked_note = store.note(card.note_id).expect("note").expect("exists");
    assert!(marked_note.modified > before_note.modified);
    assert_eq!(marked_note.usn, -1);
}

#[test]
fn review_leech_suspends_when_the_preset_action_says_so() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store
        .deck_presets()
        .expect("presets")
        .into_iter()
        .find(|preset| preset.id == 1)
        .expect("default preset");
    preset.leech_action = LeechAction::Suspend;
    store.write_preset(&preset).expect("write preset");
    let ids = store
        .add_note(&note(&["leech me", "answer"], &[]))
        .expect("add");
    mark_card_as_review(&path, ids[0], 7);
    let card = store.card(ids[0]).expect("card").expect("exists");

    store
        .apply_review(&again_decision(&card, 9_200))
        .expect("apply");

    assert_eq!(
        store.card(ids[0]).expect("card").expect("exists").queue,
        CardQueue::Suspended
    );
    assert_eq!(note_tags(&store, card.note_id), ["leech"]);
}

#[test]
fn review_below_the_leech_threshold_does_not_mark_the_note() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["not yet", "answer"], &[]))
        .expect("add");
    mark_card_as_review(&path, ids[0], 6);
    let card = store.card(ids[0]).expect("card").expect("exists");
    let decision = again_decision(&card, 9_300);
    assert_eq!(decision.card.lapses, 7);

    store.apply_review(&decision).expect("apply");

    assert_eq!(note_tags(&store, card.note_id), [] as [String; 0]);
}

#[test]
fn leech_threshold_repeats_at_half_thresholds() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let mut preset = store
        .deck_presets()
        .expect("presets")
        .into_iter()
        .find(|preset| preset.id == 1)
        .expect("default preset");
    preset.leech_threshold = 3;
    store.write_preset(&preset).expect("write preset");

    for (index, (lapses, marked)) in
        [(3, true), (4, false), (5, true), (7, true)]
            .into_iter()
            .enumerate()
    {
        let ids = store
            .add_note(&note(&[&format!("lapse {lapses}"), "answer"], &[]))
            .expect("add");
        mark_card_as_review(&path, ids[0], lapses - 1);
        let card = store.card(ids[0]).expect("card").expect("exists");
        let decision =
            again_decision(&card, 9_400 + i64::try_from(index).expect("id"));
        assert_eq!(decision.card.lapses, lapses);

        store.apply_review(&decision).expect("apply");

        let tags = note_tags(&store, card.note_id);
        assert_eq!(tags.iter().any(|tag| tag == "leech"), marked, "{lapses}");
    }
}

#[test]
fn repeated_leech_marking_does_not_rewrite_the_note() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["repeat leech", "answer"], &[]))
        .expect("add");
    mark_card_as_review(&path, ids[0], 7);
    let card = store.card(ids[0]).expect("card").expect("exists");
    store
        .apply_review(&again_decision(&card, 9_800))
        .expect("first mark");
    let first = store.note(card.note_id).expect("note").expect("exists");

    mark_card_as_review(&path, ids[0], 11);
    let card = store.card(ids[0]).expect("card").expect("exists");
    store
        .apply_review(&again_decision(&card, 9_900))
        .expect("repeat mark");
    let second = store.note(card.note_id).expect("note").expect("exists");

    assert_eq!(second.modified, first.modified);
    assert_eq!(second.usn, first.usn);
    assert_eq!(note_tags(&store, card.note_id), ["leech"]);
}

#[test]
fn disabled_leech_option_skips_marking() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["no leech", "answer"], &[]))
        .expect("add");
    mark_card_as_review(&path, ids[0], 7);
    let card = store.card(ids[0]).expect("card").expect("exists");

    store
        .apply_review_with_options(
            &again_decision(&card, 9_500),
            &ApplyReviewOptions {
                leech_enabled: false,
            },
        )
        .expect("apply");

    assert_eq!(note_tags(&store, card.note_id), [] as [String; 0]);
    assert_eq!(
        store.card(ids[0]).expect("card").expect("exists").queue,
        CardQueue::Review
    );
}

#[test]
fn relearning_again_does_not_mark_a_leech() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["relearn", "answer"], &[]))
        .expect("add");
    let conn = Connection::open(&path).expect("write connection");
    conn.execute(
        "UPDATE cards SET type = 3, queue = 3, lapses = 7 WHERE id = ?1",
        rusqlite::params![ids[0]],
    )
    .expect("relearning card");
    drop(conn);
    let card = store.card(ids[0]).expect("card").expect("exists");

    store
        .apply_review(&again_decision(&card, 9_700))
        .expect("apply");

    assert_eq!(note_tags(&store, card.note_id), [] as [String; 0]);
}

#[test]
fn undo_review_with_note_restores_prior_tags() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    let ids = store
        .add_note(&note(&["undo leech", "answer"], &["keep"]))
        .expect("add");
    mark_card_as_review(&path, ids[0], 7);
    let card = store.card(ids[0]).expect("card").expect("exists");
    let before_note = store.note(card.note_id).expect("note").expect("exists");
    let log_id = store
        .apply_review(&again_decision(&card, 9_600))
        .expect("apply");
    assert_eq!(note_tags(&store, card.note_id), ["keep", "leech"]);

    assert!(
        store
            .undo_review_with_note(&card, Some(&before_note), log_id)
            .expect("undo")
    );

    assert_eq!(
        store.note(card.note_id).expect("note").expect("exists"),
        before_note
    );
}

#[test]
fn set_rollover_persists_in_legacy_conf_and_invalidates_timing_cache() {
    let (_directory, path) = fixture();
    let modified_before: i64;
    {
        let conn = Connection::open(&path).expect("write connection");
        conn.execute(
            "UPDATE col SET conf = ?1 WHERE id = 1",
            rusqlite::params![
                json!({
                    "schedVer": 2,
                    "creationOffset": 0,
                    "rollover": 4,
                    "futureKey": "kept"
                })
                .to_string()
            ],
        )
        .expect("conf");
        modified_before = conn
            .query_row("SELECT mod FROM col WHERE id = 1", [], |row| row.get(0))
            .expect("modification stamp");
    }
    let mut store = Collection::open(&path).expect("open");
    let now = 1_700_000_000_i64;
    let before = store.timing(now).expect("timing");

    store.set_rollover(9).expect("set rollover");
    let modified_after: i64 = {
        let conn = read_only(&path);
        conn.query_row("SELECT mod FROM col WHERE id = 1", [], |row| row.get(0))
            .expect("modification stamp")
    };
    assert!(modified_after > modified_before);

    assert_eq!(store.anki_config().expect("config").rollover, 9);
    let after = store.timing(now).expect("refreshed timing");
    let expected = ranki_model::Timing::new(
        now,
        store.collection_created().expect("created"),
        None,
        ranki_storage::local_minutes_west(now).expect("offset"),
        Some(9),
    );
    assert_ne!(before, after);
    assert_eq!(after.next_day_at, expected.next_day_at);
    let conn = read_only(&path);
    let raw: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let value: Value = serde_json::from_str(&raw).expect("conf JSON");
    assert_eq!(value["rollover"], json!(9));
    assert_eq!(value["futureKey"], json!("kept"));

    store.set_rollover(99).expect("clamp");
    assert_eq!(store.anki_config().expect("clamped").rollover, 23);
}

#[test]
fn set_rollover_persists_in_schema18_config_table() {
    let (_directory, path) = fixture();
    write_schema18_config(
        &path,
        &[("rollover", json!(4)), ("futureKey", json!(1))],
    );
    let reopening = ReopeningCollection::new(&path);
    assert_eq!(reopening.anki_config().expect("config").rollover, 4);

    let mut store = Collection::open(&path).expect("open");
    store.set_rollover(17).expect("set rollover");

    assert_eq!(store.anki_config().expect("config").rollover, 17);
    let conn = read_only(&path);
    let raw: Vec<u8> = conn
        .query_row("SELECT val FROM config WHERE key = 'rollover'", [], |row| {
            row.get(0)
        })
        .expect("rollover value");
    assert_eq!(
        serde_json::from_slice::<Value>(&raw).expect("JSON"),
        json!(17)
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM config WHERE key = 'futureKey'",
            [],
            |row| row.get(0),
        )
        .expect("other keys");
    assert_eq!(count, 1);
}
