#![expect(clippy::expect_used, reason = "test fixtures panic on failure")]
#![expect(
    clippy::shadow_reuse,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "fixtures mirror the stored Anki shapes"
)]
//! Storage tests for Anki deck presets, effective decks, and parent creation.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use ranki_model::DeckKind;
use ranki_model::LeechAction;
use ranki_model::Store;
use ranki_storage::Collection;
use rusqlite::Connection;
use rusqlite::params;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

fn fixture() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("collection.anki2");
    Collection::create(&path).expect("create");
    (directory, path)
}

fn open(path: &Path) -> Connection {
    Connection::open(path).expect("open")
}

fn varint(field: u32, value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut tag = u64::from(field).saturating_mul(8);
    while tag >= 0x80 {
        bytes.push(u8::try_from(tag & 0x7f).expect("byte") | 0x80);
        tag >>= 7;
    }
    bytes.push(u8::try_from(tag).expect("byte"));
    let mut rest = value;
    while rest >= 0x80 {
        bytes.push(u8::try_from(rest & 0x7f).expect("byte") | 0x80);
        rest >>= 7;
    }
    bytes.push(u8::try_from(rest).expect("byte"));
    bytes
}

fn f32_field(field: u32, value: f32) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut tag = u64::from(field).saturating_mul(8).saturating_add(5);
    while tag >= 0x80 {
        bytes.push(u8::try_from(tag & 0x7f).expect("byte") | 0x80);
        tag >>= 7;
    }
    bytes.push(u8::try_from(tag).expect("byte"));
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes
}

fn float_slice(field: u32, values: &[f32]) -> Vec<u8> {
    let mut payload = Vec::new();
    for value in values {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    nested(field, &payload)
}

fn nested(field: u32, bytes: &[u8]) -> Vec<u8> {
    let mut tag = u64::from(field).saturating_mul(8).saturating_add(2);
    let mut out = Vec::new();
    while tag >= 0x80 {
        out.push(u8::try_from(tag & 0x7f).expect("byte") | 0x80);
        tag >>= 7;
    }
    out.push(u8::try_from(tag).expect("byte"));
    let mut length = u64::try_from(bytes.len()).expect("length");
    while length >= 0x80 {
        out.push(u8::try_from(length & 0x7f).expect("byte") | 0x80);
        length >>= 7;
    }
    out.push(u8::try_from(length).expect("byte"));
    out.extend_from_slice(bytes);
    out
}

fn schema18(path: &Path) -> Connection {
    let _ = fs::remove_file(path);
    let conn = open(path);
    conn.execute_batch(
        "CREATE TABLE col (
            id INTEGER PRIMARY KEY, crt INTEGER NOT NULL, mod INTEGER NOT NULL,
            scm INTEGER NOT NULL, ver INTEGER NOT NULL, dty INTEGER NOT NULL,
            usn INTEGER NOT NULL, ls INTEGER NOT NULL, conf TEXT NOT NULL,
            models TEXT NOT NULL, decks TEXT NOT NULL, dconf TEXT NOT NULL,
            tags TEXT NOT NULL
        );
        CREATE TABLE decks (
            id INTEGER PRIMARY KEY, name TEXT NOT NULL, mtime_secs INTEGER NOT NULL,
            usn INTEGER NOT NULL, common BLOB NOT NULL, kind BLOB NOT NULL
        );
        CREATE TABLE deck_config (
            id INTEGER PRIMARY KEY, name TEXT NOT NULL, mtime_secs INTEGER NOT NULL,
            usn INTEGER NOT NULL, config BLOB NOT NULL
        );
        CREATE TABLE notes (
            id INTEGER PRIMARY KEY, guid TEXT NOT NULL, mid INTEGER NOT NULL,
            mod INTEGER NOT NULL, usn INTEGER NOT NULL, tags TEXT NOT NULL,
            flds TEXT NOT NULL, sfld INTEGER NOT NULL, csum INTEGER NOT NULL,
            flags INTEGER NOT NULL, data TEXT NOT NULL
        );",
    )
    .expect("schema");
    conn.execute(
        "INSERT INTO col VALUES (1, 0, 0, 0, 18, 0, 0, 0, ?1, ?2, ?3, ?4, '{}')",
        params![
            json!({"loadBalancerEnabled": true}).to_string(),
            json!({"77": {"id": 77, "name": "Dead"}}).to_string(),
            json!({"77": {"id": 77, "name": "Dead", "conf": 77}}).to_string(),
            json!({"77": {"id": 77, "name": "Dead"}}).to_string(),
        ],
    )
    .expect("col");
    conn
}

#[test]
fn schema11_presets_and_decks_decode_non_default_values() {
    let (_directory, path) = fixture();
    {
        let conn = open(&path);
        let config = json!({
            "id": 2,
            "name": "Strict",
            "mod": 12,
            "usn": 3,
            "maxTaken": 45,
            "autoplay": false,
            "timer": 1,
            "replayq": false,
            "new": {"bury": true, "delays": [0.5, 3.25], "initialFactor": 2650, "ints": [2, 7, 0], "order": 0, "perDay": 11},
            "rev": {"bury": true, "ease4": 2.4, "hardFactor": 1.1, "ivlFct": 1.7, "maxIvl": 999, "perDay": 33},
            "lapse": {"delays": [5.5], "leechAction": 0, "leechFails": 4, "minInt": 3, "mult": 0.6},
            "newMix": 2,
            "newPerDayMinimum": 4,
            "interdayLearningMix": 1,
            "reviewOrder": 3,
            "newSortOrder": 2,
            "newGatherPriority": 5,
            "buryInterdayLearning": true,
            "stopTimerOnAnswer": true,
            "secondsToShowQuestion": 2.5,
            "secondsToShowAnswer": 1.5,
            "questionAction": 1,
            "answerAction": 4,
            "waitForAudio": false,
            "easyDaysPercentages": [1.0, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7],
            "fsrsWeights": [4.0],
            "fsrsParams5": [5.0],
            "fsrsParams6": [6.0],
            "desiredRetention": 0.87,
            "sm2Retention": 0.85,
            "weightSearch": "search",
            "ignoreRevlogsBeforeDate": "2024-01-01",
            "futureKey": {"kept": true}
        });
        let decks = json!({
            "1": {"id": 1, "name": "Default", "conf": 1},
            "2": {"id": 2, "name": "Study", "conf": 2, "desc": "Full deck", "dyn": 0, "extendNew": 2, "extendRev": 3, "newToday": [4, 5], "revToday": [6, 7], "newLimit": 15, "reviewLimit": 60, "newLimitToday": [4, 5], "reviewLimitToday": [6, 7], "desiredRetention": 83, "md": true, "future": "kept"},
            "3": {"id": 3, "name": "Filtered", "conf": 2, "dyn": 1}
        });
        conn.execute(
            "UPDATE col SET dconf = ?1, decks = ?2 WHERE id = 1",
            params![json!({"2": config}).to_string(), decks.to_string()],
        )
        .expect("metadata");
    }
    let store = Collection::open(&path).expect("open");
    let preset = store.preset(2).expect("preset").expect("found");
    assert_eq!(preset.name, "Strict");
    assert_eq!(preset.learning_steps, [0.5, 3.25]);
    assert_eq!(preset.relearning_steps, [5.5]);
    assert_eq!(preset.graduating_interval_good, 2);
    assert_eq!(preset.graduating_interval_easy, 7);
    assert!((preset.initial_ease - 2.65).abs() < 0.001);
    assert_eq!(preset.easy_multiplier, 2.4);
    assert_eq!(preset.hard_multiplier, 1.1);
    assert_eq!(preset.interval_multiplier, 1.7);
    assert_eq!(preset.lapse_multiplier, 0.6);
    assert_eq!(preset.maximum_interval, 999);
    assert_eq!(preset.minimum_lapse_interval, 3);
    assert_eq!(preset.new_per_day, 11);
    assert_eq!(preset.reviews_per_day, 33);
    assert_eq!(preset.new_per_day_minimum, 4);
    assert!(
        preset.bury_new && preset.bury_reviews && preset.bury_interday_learning
    );
    assert_eq!(preset.leech_action, LeechAction::Suspend);
    assert_eq!(preset.leech_threshold, 4);
    assert_eq!(preset.new_card_insert_order, 1);
    assert_eq!(preset.new_card_gather_priority, 5);
    assert_eq!(preset.new_card_sort_order, 2);
    assert_eq!(preset.new_mix, 2);
    assert_eq!(preset.review_order, 3);
    assert_eq!(preset.interday_learning_mix, 1);
    assert_eq!(preset.fsrs_params(), [6.0]);
    assert_eq!(preset.desired_retention, 0.87);
    assert_eq!(preset.historical_retention, 0.85);
    assert_eq!(preset.easy_days_percentages[1], 1.2);
    assert!(preset.load_balancer_enabled);
    assert!(!preset.wait_for_audio && preset.stop_timer_on_answer);
    assert_eq!(preset.seconds_to_show_question, 2.5);
    assert_eq!(preset.answer_action, 4);
    let decoded: Value =
        serde_json::from_str(preset.raw_json.as_deref().expect("raw config"))
            .expect("raw JSON");
    assert_eq!(decoded["futureKey"]["kept"], true);

    let decks = store.effective_decks().expect("decks");
    let study = decks.iter().find(|deck| deck.id == 2).expect("study");
    assert_eq!(study.description, "Full deck");
    assert_eq!(study.preset_id, 2);
    assert_eq!(study.overrides.extend_new, 2);
    assert_eq!(study.overrides.new_today.day, 5);
    assert_eq!(
        study.overrides.new_today.today, 0,
        "day 4 count is stale at day 6"
    );
    assert_eq!(study.overrides.new_today.limit, Some(4));
    assert_eq!(study.overrides.new_limit, Some(15));
    assert_eq!(study.overrides.review_limit, Some(60));
    assert_eq!(
        study.common.new_studied, 0,
        "an earlier-day count is not current"
    );
    assert_eq!(study.common.last_day_studied, 6);
    assert!(study.markdown_description);
    assert!((study.desired_retention(&preset) - 0.83).abs() < 0.001);
    let raw: Value =
        serde_json::from_str(study.raw_json.as_deref().expect("raw"))
            .expect("raw deck");
    assert_eq!(raw["future"], "kept");
    let filtered = decks.iter().find(|deck| deck.id == 3).expect("filtered");
    assert_eq!(filtered.kind, DeckKind::Filtered);
}

#[test]
fn schema18_ignores_nonempty_json_and_decodes_proto_defaults() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("collection.anki2");
    let conn = schema18(&path);
    let mut config = varint(9, 42);
    config.extend(varint(10, 175));
    config.extend(f32_field(11, 2.7));
    config.extend(float_slice(1, &[0.25, 2.5]));
    config.extend(float_slice(2, &[7.5]));
    config.extend(float_slice(6, &[6.0, 6.5]));
    config.extend(varint(18, 3));
    config.extend(varint(19, 8));
    config.extend(varint(21, 0));
    config.extend(f32_field(37, 0.88));
    config.extend(nested(255, b"future"));
    conn.execute(
        "INSERT INTO deck_config VALUES (2, 'Strict', 5, 2, ?1), (3, 'Empty', 0, 0, ?2)",
        params![config, Vec::<u8>::new()],
    )
    .expect("config rows");

    let mut normal = varint(1, 2);
    normal.extend(nested(4, b"Proto description"));
    normal.extend(varint(7, 12));
    normal.extend(f32_field(10, 0.83));
    let mut common = varint(2, 1);
    common.extend(varint(3, 40));
    common.extend(varint(4, 5));
    common.extend(nested(255, b"common-extra"));
    let mut kind = nested(1, &normal);
    let filtered = nested(2, b"filtered");
    kind.extend(nested(2, &filtered));
    conn.execute(
        "INSERT INTO decks VALUES (2, 'Study', 5, 2, ?1, ?2), (3, 'Filtered\u{001f}Deck', 0, 0, x'', ?3)",
        params![common, nested(1, &normal), nested(2, &filtered)],
    )
    .expect("deck rows");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let presets = store.presets().expect("presets");
    assert_eq!(presets.len(), 2, "dead JSON must not become presets");
    let strict = presets
        .iter()
        .find(|preset| preset.id == 2)
        .expect("strict");
    assert_eq!(strict.learning_steps, [0.25, 2.5]);
    assert_eq!(strict.relearning_steps, [7.5]);
    assert_eq!(strict.fsrs_params(), [6.0, 6.5]);
    assert_eq!(strict.new_per_day, 42);
    assert_eq!(strict.reviews_per_day, 175);
    assert!((strict.initial_ease - 2.7).abs() < 0.001);
    assert_eq!(strict.graduating_interval_good, 3);
    assert_eq!(strict.leech_action, LeechAction::Suspend);
    assert_eq!(strict.desired_retention, 0.88);
    assert_eq!(strict.raw, b"future");

    let empty = presets.iter().find(|preset| preset.id == 3).expect("empty");
    assert_eq!(empty.learning_steps, [1.0, 10.0]);
    assert_eq!(empty.relearning_steps, [10.0]);
    assert_eq!(empty.initial_ease, 2.5);
    assert_eq!(empty.easy_days_percentages, [1.0; 7]);
    assert_eq!(empty.leech_action, LeechAction::TagOnly);

    let decks = store.effective_decks().expect("decks");
    let study = decks.iter().find(|deck| deck.id == 2).expect("study");
    assert_eq!(study.description, "Proto description");
    assert_eq!(study.preset_id, 2);
    assert_eq!(study.overrides.new_limit, Some(12));
    assert_eq!(study.desired_retention(strict), 0.83);
    assert_eq!(study.common.last_day_studied, 40);
    assert_eq!(study.common.new_studied, 5);
    assert_eq!(study.raw_common, b"common-extra");
    let filtered = decks.iter().find(|deck| deck.id == 3).expect("filtered");
    assert_eq!(filtered.name, "Filtered::Deck");
    assert_eq!(filtered.kind, DeckKind::Filtered);

    let mut changed = strict.clone();
    changed.new_per_day = 77;
    changed.fsrs_params_6 = vec![6.25];
    store.write_preset(&changed).expect("write preset");
    let reread = store.preset(2).expect("preset").expect("found");
    assert_eq!(reread.new_per_day, 77);
    assert_eq!(reread.fsrs_params(), [6.25]);
    assert_eq!(reread.raw, b"future");

    let mut changed = study.clone();
    changed.description = "Edited".into();
    changed.overrides.review_limit = Some(31);
    store.write_deck(&changed).expect("write deck");
    let reread = store.effective_decks().expect("decks");
    let study = reread.iter().find(|deck| deck.id == 2).expect("study");
    assert_eq!(study.description, "Edited");
    assert_eq!(study.overrides.review_limit, Some(31));

    let conn = open(&path);
    let json: String = conn
        .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("dead JSON");
    assert_eq!(
        serde_json::from_str::<Value>(&json).expect("JSON")["77"]["name"],
        "Dead"
    );
}

#[test]
fn schema18_create_deck_writes_tables_when_json_is_nonempty() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("collection.anki2");
    let conn = schema18(&path);
    conn.execute(
        "INSERT INTO deck_config VALUES (1, 'Default', 0, 0, x'')",
        [],
    )
    .expect("default preset");
    drop(conn);

    let mut store = Collection::open(&path).expect("open");
    let child = store.create_deck("Parent::Child").expect("create");
    assert_eq!(child.name, "Parent::Child");
    let decks = store.effective_decks().expect("decks");
    assert!(decks.iter().any(|deck| deck.name == "Parent"));
    assert!(decks.iter().any(|deck| deck.name == "Parent::Child"));
    let conn = open(&path);
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM decks", [], |row| row.get(0))
        .expect("count");
    assert_eq!(rows, 2);
    let json: String = conn
        .query_row("SELECT decks FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("dead JSON");
    assert_eq!(
        serde_json::from_str::<Value>(&json).expect("JSON")["77"]["name"],
        "Dead"
    );
}

#[test]
fn create_deck_creates_parents_and_matches_their_casing() {
    let (_directory, path) = fixture();
    let mut store = Collection::open(&path).expect("open");
    store.create_deck("Alpha").expect("parent");
    let child = store.create_deck("alpha::Beta::Gamma").expect("child");
    assert_eq!(child.name, "Alpha::Beta::Gamma");
    let names: Vec<String> = store
        .decks()
        .expect("decks")
        .into_iter()
        .map(|deck| deck.name)
        .collect();
    assert!(names.contains(&"Alpha".to_owned()));
    assert!(names.contains(&"Alpha::Beta".to_owned()));
    assert!(names.contains(&"Alpha::Beta::Gamma".to_owned()));
    assert!(store.create_deck("ALPHA::BETA::GAMMA").is_err());
}

#[test]
fn schema11_model_json_round_trips_unknown_settings() {
    let (_directory, path) = fixture();
    {
        let conn = open(&path);
        let models = json!({
            "1": {
                "id": 1, "name": "Basic", "type": 0, "mod": 9, "usn": 2,
                "sortf": 0, "did": 1, "css": ".card {}", "future": "kept",
                "flds": [{"name": "Front", "ord": 0, "sticky": true, "font": "Courier", "size": 14, "description": "Prompt"}],
                "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}", "afmt": "{{Back}}", "bqfmt": "browser"}],
                "req": [[0, "any", [0]]]
            }
        });
        conn.execute(
            "UPDATE col SET models = ?1 WHERE id = 1",
            params![models.to_string()],
        )
        .expect("models");
    }
    let store = Collection::open(&path).expect("open");
    let models = store.models().expect("models");
    let json = ranki_storage::models_to_json(&models);
    let entry = &json["1"];
    assert_eq!(entry["future"], "kept");
    assert_eq!(entry["mod"], 9);
    assert_eq!(entry["flds"][0]["sticky"], true);
    assert_eq!(entry["flds"][0]["font"], "Courier");
    assert_eq!(entry["flds"][0]["size"], 14);
    assert_eq!(entry["flds"][0]["description"], "Prompt");
    assert_eq!(entry["tmpls"][0]["bqfmt"], "browser");
    assert_eq!(entry["req"][0][2][0], 0);
}

#[test]
fn new_collections_use_the_full_shared_schema11_defaults() {
    let (_directory, path) = fixture();
    let conn = open(&path);
    let conf: String = conn
        .query_row("SELECT conf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("conf");
    let conf: Value = serde_json::from_str(&conf).expect("conf JSON");
    assert_eq!(conf["sched2021"], true);
    assert_eq!(conf["dayLearnFirst"], false);
    assert_eq!(conf["collapseTime"], 1200);
    assert_eq!(conf["timeLim"], 0);
    assert_eq!(conf["estTimes"], true);
    assert_eq!(conf["dueCounts"], true);
    assert_eq!(conf["curDeck"], 1);
    assert!(conf["curModel"].is_null());
    assert_eq!(conf["nextPos"], 1);
    assert_eq!(
        conf["creationOffset"],
        ranki_storage::local_minutes_west(
            conn.query_row::<i64, _, _>(
                "SELECT crt FROM col WHERE id = 1",
                [],
                |row| row.get(0)
            )
            .unwrap(),
        )
        .unwrap()
    );
    assert_eq!(
        ranki_storage::schema11_default_conf(None)["creationOffset"],
        Value::Null
    );
}

#[test]
fn schema11_preset_write_keeps_new_card_order_mapping() {
    let (_directory, path) = fixture();
    {
        let conn = open(&path);
        let config = json!({
            "2": {"id": 2, "name": "Random", "mod": 0, "usn": 0,
                  "new": {"order": 0, "delays": [1.0], "ints": [1, 4, 0]}}
        });
        conn.execute(
            "UPDATE col SET dconf = ?1 WHERE id = 1",
            params![config.to_string()],
        )
        .expect("config");
    }
    let store = Collection::open(&path).expect("open");
    let mut preset = store.preset(2).expect("preset").expect("found");
    assert_eq!(preset.new_card_insert_order, 1);
    store.write_preset(&preset).expect("write random");
    {
        let conn = open(&path);
        let text: String = conn
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("config");
        let value: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(value["2"]["new"]["order"], 0);
    }
    preset.new_card_insert_order = 0;
    store.write_preset(&preset).expect("write due");
    let conn = open(&path);
    let text: String = conn
        .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("config");
    let value: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(value["2"]["new"]["order"], 1);
}

#[test]
fn schema18_i64_preset_ids_survive_and_filtered_kind_is_not_rewrapped() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("collection.anki2");
    let conn = schema18(&path);
    let id = 4_294_967_300_i64;
    conn.execute(
        "INSERT INTO deck_config VALUES (?1, 'Wide', 0, 0, ?2)",
        params![id, varint(9, 7)],
    )
    .expect("preset");
    let payload = varint(1, u64::try_from(id).expect("id"));
    conn.execute(
        "INSERT INTO decks VALUES (11, 'Wide', 0, 0, x'', ?1)",
        params![nested(1, &payload)],
    )
    .expect("normal deck");
    conn.execute(
        "INSERT INTO decks VALUES (10, 'Filtered', 0, 0, x'', ?1)",
        params![nested(2, &payload)],
    )
    .expect("deck");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let preset = store.preset(id).expect("preset").expect("found");
    assert_eq!(preset.id, id);
    assert_eq!(preset.new_per_day, 7);
    let deck = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == 10)
        .expect("filtered deck");
    assert_eq!(deck.kind, DeckKind::Filtered);
    assert_eq!(deck.preset_id, 0);
    let wide = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == 11)
        .expect("normal deck");
    assert_eq!(wide.preset_id, id);
    assert_eq!(deck.raw_kind.as_deref(), Some(payload.as_slice()));
    let mut changed = deck;
    changed.name = "Renamed".into();
    store.write_deck(&changed).expect("write deck");
    let reread = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == 10)
        .expect("filtered deck");
    assert_eq!(reread.name, "Renamed");
    assert_eq!(reread.kind, DeckKind::Filtered);
    assert_eq!(reread.raw_kind.as_deref(), Some(payload.as_slice()));
    let conn = open(&path);
    let kind: Vec<u8> = conn
        .query_row("SELECT kind FROM decks WHERE id = 10", [], |row| row.get(0))
        .expect("kind");
    assert_eq!(kind, nested(2, &payload));

    let mut preset = preset;
    preset.new_per_day = 9;
    store.write_preset(&preset).expect("write preset");
    assert_eq!(
        store
            .preset(id)
            .expect("preset")
            .expect("found")
            .new_per_day,
        9
    );
}

#[test]
fn schema11_deck_write_keeps_day_amount_pair_order() {
    let (_directory, path) = fixture();
    {
        let conn = open(&path);
        let decks = json!({
            "2": {"id": 2, "name": "Study", "conf": 1, "newToday": [12, 3],
                  "revToday": [12, 4], "newLimitToday": [7, 2], "md": true}
        });
        conn.execute(
            "UPDATE col SET decks = ?1 WHERE id = 1",
            params![decks.to_string()],
        )
        .expect("decks");
    }
    let store = Collection::open(&path).expect("open");
    let mut deck = store
        .effective_decks()
        .expect("decks")
        .into_iter()
        .find(|deck| deck.id == 2)
        .expect("study");
    assert_eq!(deck.common.last_day_studied, 12);
    assert_eq!(deck.common.new_studied, 3);
    assert_eq!(deck.overrides.new_today.day, 2);
    assert_eq!(
        deck.overrides.new_today.today, 3,
        "count survives an expired limit"
    );
    assert_eq!(deck.overrides.new_today.limit, Some(7));
    deck.description = "Edited".into();
    store.write_deck(&deck).expect("write");
    let conn = open(&path);
    let text: String = conn
        .query_row("SELECT decks FROM col WHERE id = 1", [], |row| row.get(0))
        .expect("decks");
    let value: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(value["2"]["newToday"], json!([12, 3]));
    assert_eq!(value["2"]["revToday"], json!([12, 4]));
    assert_eq!(value["2"]["newLimitToday"], json!([7, 2]));
    assert_eq!(value["2"]["md"], true);
}

#[test]
fn schema18_option_writes_bump_collection_mod() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("collection.anki2");
    let conn = schema18(&path);
    conn.execute(
        "INSERT INTO deck_config VALUES (1, 'Preset', 0, 0, ?1)",
        params![Vec::<u8>::new()],
    )
    .expect("preset row");
    conn.execute(
        "INSERT INTO decks VALUES (1, 'Default', 0, 0, ?1, ?2)",
        params![Vec::<u8>::new(), nested(1, &varint(1, 1))],
    )
    .expect("deck row");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let mod_now = || -> i64 {
        let conn = open(&path);
        conn.query_row("SELECT mod FROM col WHERE id = 1", [], |row| row.get(0))
            .expect("mod")
    };
    let before = mod_now();
    let mut preset = store.presets().expect("presets")[0].clone();
    preset.new_per_day = 12;
    store.write_preset(&preset).expect("write preset");
    let after_preset = mod_now();
    assert!(after_preset > before, "schema-18 preset write bumps mod");

    let mut deck = store.effective_decks().expect("decks")[0].clone();
    deck.overrides.new_limit = Some(9);
    store.write_deck(&deck).expect("write deck");
    assert!(mod_now() > after_preset, "schema-18 deck write bumps mod");
}

#[test]
fn schema11_null_today_override_is_unset() {
    let (_directory, path) = fixture();
    {
        let conn = open(&path);
        let decks = json!({
            "1": {"id": 1, "name": "Default", "conf": 1, "newToday": [0, 0], "revToday": [0, 0], "newLimitToday": null}
        });
        conn.execute(
            "UPDATE col SET decks = ?1 WHERE id = 1",
            params![decks.to_string()],
        )
        .expect("decks");
    }
    let store = Collection::open(&path).expect("open");
    let deck = store.effective_decks().expect("decks")[0].clone();
    assert_eq!(deck.overrides.new_today.limit, None);
    assert_eq!(deck.overrides.new_today.day, 0);
    assert_eq!(deck.overrides.new_today.today, 0);
}

#[test]
fn schema18_present_day_limit_defaults_omitted_fields_to_zero() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("collection.anki2");
    let conn = schema18(&path);
    conn.execute(
        "INSERT INTO deck_config VALUES (1, 'Preset', 0, 0, ?1)",
        params![Vec::<u8>::new()],
    )
    .expect("preset row");
    let mut normal = varint(1, 1);
    normal.extend(nested(9, &varint(2, 5)));
    let mut common = varint(3, 5);
    common.extend(varint(4, 2));
    conn.execute(
        "INSERT INTO decks VALUES (1, 'Default', 0, 0, ?1, ?2)",
        params![common, nested(1, &normal)],
    )
    .expect("deck row");
    drop(conn);

    let store = Collection::open(&path).expect("open");
    let deck = store.effective_decks().expect("decks")[0].clone();
    assert_eq!(
        deck.overrides.new_today.limit,
        Some(0),
        "omitted limit is zero"
    );
    assert_eq!(deck.overrides.new_today.day, 5);
    assert_eq!(deck.overrides.new_today.today, 2, "count comes from common");
}

#[test]
fn schema11_combines_days_across_all_four_counters() {
    let (_directory, path) = fixture();
    {
        let conn = open(&path);
        let decks = json!({
            "1": {"id": 1, "name": "Default", "conf": 1,
                  "newToday": [1, 1], "revToday": [1, 1],
                  "lrnToday": [2, 3], "timeToday": [2, 9]}
        });
        conn.execute(
            "UPDATE col SET decks = ?1 WHERE id = 1",
            params![decks.to_string()],
        )
        .expect("decks");
    }
    let store = Collection::open(&path).expect("open");
    let deck = store.effective_decks().expect("decks")[0].clone();
    assert_eq!(deck.common.last_day_studied, 2);
    assert_eq!(deck.common.new_studied, 0);
    assert_eq!(deck.common.review_studied, 0);
    assert_eq!(deck.common.learning_studied, 3);
    assert_eq!(deck.common.milliseconds_studied, 9);

#[test]
fn field_44_wait_for_audio_round_trips_both_values() {
    for (bytes, expected) in [
        (varint(44, 0), false),
        (varint(44, 1), true),
        (Vec::new(), false),
    ] {
        let preset = ranki_storage::preset_from_proto(
            9,
            "Audio".to_owned(),
            0,
            0,
            &bytes,
            true,
        );
        assert_eq!(preset.wait_for_audio, expected);
        let encoded = ranki_storage::preset_to_proto(&preset);
        let decoded = ranki_storage::preset_from_proto(
            9,
            "Audio".to_owned(),
            0,
            0,
            &encoded,
            true,
        );
        assert_eq!(decoded.wait_for_audio, expected);
    }
}
