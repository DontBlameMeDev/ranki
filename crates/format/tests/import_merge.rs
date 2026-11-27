#![expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::excessive_nesting,
    clippy::let_and_return,
    clippy::identity_op,
    clippy::indexing_slicing,
    clippy::missing_const_for_fn,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::redundant_clone,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::unwrap_used,
    reason = "test fixtures use fixed identifiers and explicit SQL"
)]

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::slice;

use ranki_format::ImportOptions;
use ranki_format::import_apkg_with_options;
use ranki_model::NoteModel;
use rusqlite::Connection;
use rusqlite::params;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const SCHEMA11: &str = "
CREATE TABLE col (
    id INTEGER PRIMARY KEY, crt INTEGER NOT NULL, mod INTEGER NOT NULL,
    scm INTEGER NOT NULL, ver INTEGER NOT NULL, dty INTEGER NOT NULL,
    usn INTEGER NOT NULL, ls INTEGER NOT NULL, conf TEXT NOT NULL,
    models TEXT NOT NULL, decks TEXT NOT NULL, dconf TEXT NOT NULL,
    tags TEXT NOT NULL
);
CREATE TABLE notes (
    id INTEGER PRIMARY KEY, guid TEXT NOT NULL, mid INTEGER NOT NULL,
    mod INTEGER NOT NULL, usn INTEGER NOT NULL, tags TEXT NOT NULL,
    flds TEXT NOT NULL, sfld TEXT NOT NULL, csum INTEGER NOT NULL,
    flags INTEGER NOT NULL, data TEXT NOT NULL
);
CREATE TABLE cards (
    id INTEGER PRIMARY KEY, nid INTEGER NOT NULL, did INTEGER NOT NULL,
    ord INTEGER NOT NULL, mod INTEGER NOT NULL, usn INTEGER NOT NULL,
    type INTEGER NOT NULL, queue INTEGER NOT NULL, due INTEGER NOT NULL,
    ivl INTEGER NOT NULL, factor INTEGER NOT NULL, reps INTEGER NOT NULL,
    lapses INTEGER NOT NULL, left INTEGER NOT NULL, odue INTEGER NOT NULL,
    odid INTEGER NOT NULL, flags INTEGER NOT NULL, data TEXT NOT NULL
);
CREATE TABLE revlog (
    id INTEGER PRIMARY KEY, cid INTEGER NOT NULL, usn INTEGER NOT NULL,
    ease INTEGER NOT NULL, ivl INTEGER NOT NULL, lastIvl INTEGER NOT NULL,
    factor INTEGER NOT NULL, time INTEGER NOT NULL, type INTEGER NOT NULL
);
CREATE TABLE graves (usn INTEGER NOT NULL, oid INTEGER NOT NULL, type INTEGER NOT NULL);
";

const SCHEMA18_TABLES: &str = "
CREATE TABLE decks (
    id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL COLLATE unicase,
    mtime_secs INTEGER NOT NULL, usn INTEGER NOT NULL,
    common BLOB NOT NULL, kind BLOB NOT NULL
);
CREATE UNIQUE INDEX idx_decks_name ON decks (name);
CREATE TABLE deck_config (
    id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL COLLATE unicase,
    mtime_secs INTEGER NOT NULL, usn INTEGER NOT NULL, config BLOB NOT NULL
);
CREATE TABLE tags (
    tag TEXT PRIMARY KEY COLLATE unicase, usn INTEGER NOT NULL, collapsed INTEGER NOT NULL
);
CREATE TABLE notetypes (
    id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL COLLATE unicase,
    mtime_secs INTEGER NOT NULL, usn INTEGER NOT NULL, config BLOB NOT NULL
);
CREATE UNIQUE INDEX idx_notetypes_name ON notetypes (name);
CREATE TABLE fields (
    ntid INTEGER NOT NULL, ord INTEGER NOT NULL, name TEXT NOT NULL,
    config BLOB NOT NULL, PRIMARY KEY (ntid, ord)
) WITHOUT ROWID;
CREATE TABLE templates (
    ntid INTEGER NOT NULL, ord INTEGER NOT NULL, name TEXT NOT NULL,
    mtime_secs INTEGER NOT NULL, usn INTEGER NOT NULL, config BLOB NOT NULL,
    PRIMARY KEY (ntid, ord)
) WITHOUT ROWID;
";

#[derive(Clone)]
struct Note {
    id: i64,
    guid: &'static str,
    mid: i64,
    modified: i64,
    fields: &'static str,
    tags: &'static str,
}

#[derive(Clone, Copy)]
struct Card {
    id: i64,
    nid: i64,
    did: i64,
    ord: i64,
}

#[derive(Clone, Copy)]
struct Review {
    id: i64,
    cid: i64,
}

struct Fixture {
    dir: TempDir,
    media: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        fs::create_dir_all(&media).unwrap();
        Self { dir, media }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(format!("{name}.anki2"))
    }

    fn collection11(
        &self,
        name: &str,
        models: Value,
        decks: Value,
        presets: Value,
        notes: &[Note],
        cards: &[Card],
        reviews: &[Review],
    ) -> PathBuf {
        let path = self.path(name);
        let _ = fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(SCHEMA11).unwrap();
        connection
            .execute(
                "INSERT INTO col (id, crt, mod, scm, ver, dty, usn, ls, conf, models, decks, dconf, tags)
                 VALUES (1, 0, 10, 999, 11, 0, 3, 0, ?1, ?2, ?3, ?4, ?5)",
                params![
                    json!({"activeDecks": [1], "curDeck": 1, "curModel": 1}).to_string(),
                    models.to_string(),
                    decks.to_string(),
                    presets.to_string(),
                    json!({}).to_string(),
                ],
            )
            .unwrap();
        write_records(&connection, notes, cards, reviews);
        path
    }

    fn collection18(
        &self,
        name: &str,
        preset: &ranki_model::DeckPreset,
        deck: &ranki_model::EffectiveDeck,
        model: &NoteModel,
        notes: &[Note],
        cards: &[Card],
    ) -> PathBuf {
        let path = self.path(name);
        let _ = fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        ranki_storage::register_collations(&connection).unwrap();
        connection.execute_batch(SCHEMA11).unwrap();
        connection.execute_batch(SCHEMA18_TABLES).unwrap();
        connection
            .execute(
                "INSERT INTO col (id, crt, mod, scm, ver, dty, usn, ls, conf, models, decks, dconf, tags)
                 VALUES (1, 0, 10, 999, 18, 0, 3, 0, '{}', '', '', '', '{}')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO deck_config (id, name, mtime_secs, usn, config) VALUES (?1, ?2, 0, 0, ?3)",
                params![
                    preset.id,
                    preset.name,
                    ranki_storage::preset_to_proto(preset),
                ],
            )
            .unwrap();
        let (common, kind) = ranki_storage::deck_to_proto(deck);
        connection
            .execute(
                "INSERT INTO decks (id, name, mtime_secs, usn, common, kind) VALUES (?1, ?2, 0, 0, ?3, ?4)",
                params![
                    deck.id,
                    ranki_storage::encode_deck_name(&deck.name),
                    common,
                    kind,
                ],
            )
            .unwrap();
        ranki_storage::insert_model_tables_for_import(&connection, model, 0, 0)
            .unwrap();
        write_records(&connection, notes, cards, &[]);
        path
    }

    fn package(&self, collection: &Path, name: &str) -> PathBuf {
        let package = self.dir.path().join(format!("{name}.apkg"));
        let file = fs::File::create(&package).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("collection.anki2", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&fs::read(collection).unwrap()).unwrap();
        writer
            .start_file("media", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"{}").unwrap();
        writer.finish().unwrap();
        package
    }

    fn import(
        &self,
        package: &Path,
        name: &str,
    ) -> (PathBuf, ranki_format::ImportReport) {
        self.import_with(package, name, ImportOptions::default())
    }

    fn import_with(
        &self,
        package: &Path,
        name: &str,
        options: ImportOptions,
    ) -> (PathBuf, ranki_format::ImportReport) {
        let collection = self.path(name);
        let report = import_apkg_with_options(
            package,
            &collection,
            &self.media,
            options,
        )
        .unwrap();
        (collection, report)
    }
}

fn write_records(
    connection: &Connection,
    notes: &[Note],
    cards: &[Card],
    reviews: &[Review],
) {
    for note in notes {
        let sort_field =
            note.fields.split('\u{001f}').next().unwrap_or_default();
        connection
            .execute(
                "INSERT INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, 0, 0, '')",
                params![
                    note.id,
                    note.guid,
                    note.mid,
                    note.modified,
                    note.tags,
                    note.fields,
                    sort_field,
                ],
            )
            .unwrap();
    }
    for card in cards {
        connection
            .execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl,
                                    factor, reps, lapses, left, odue, odid, flags, data)
                 VALUES (?1, ?2, ?3, ?4, 0, 0, 0, 0, 1, 0, 2500, 0, 0, 0, 0, 0, 0, '')",
                params![card.id, card.nid, card.did, card.ord],
            )
            .unwrap();
    }
    for review in reviews {
        connection
            .execute(
                "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type)
                 VALUES (?1, ?2, 0, 3, 1, 1, 2500, 100, 0)",
                params![review.id, review.cid],
            )
            .unwrap();
    }
}

fn model(id: i64, name: &str, fields: &[&str]) -> Value {
    let entries = fields
        .iter()
        .enumerate()
        .map(|(ord, field)| json!({"name": field, "ord": ord}))
        .collect::<Vec<_>>();
    json!({
        "id": id,
        "name": name,
        "type": 0,
        "mod": 0,
        "usn": 0,
        "sortf": 0,
        "did": 1,
        "flds": entries,
        "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}", "afmt": "{{Back}}", "did": null}],
        "css": "",
        "req": [],
    })
}

fn add_template(model: &mut Value, name: &str, ord: i64) {
    if let Some(templates) =
        model.get_mut("tmpls").and_then(Value::as_array_mut)
    {
        templates.push(json!({
            "name": name, "ord": ord, "qfmt": "{{Front}}",
            "afmt": "{{Back}}", "did": null
        }));
    }
}

fn deck(id: i64, name: &str, conf: i64) -> Value {
    json!({"id": id, "name": name, "conf": conf, "desc": "", "dyn": 0, "usn": 0, "mod": 0})
}

fn entries(values: &[Value]) -> Value {
    let mut map = serde_json::Map::new();
    for value in values {
        let id = value.get("id").and_then(Value::as_i64).unwrap();
        let _ = map.insert(id.to_string(), value.clone());
    }
    Value::Object(map)
}

fn preset(id: i64, name: &str, learning: &[f32]) -> Value {
    let mut config = ranki_storage::schema11_default_deck_config(0);
    let object = config.as_object_mut().unwrap();
    let _ = object.insert("id".into(), json!(id));
    let _ = object.insert("name".into(), json!(name));
    let new = object.get_mut("new").unwrap().as_object_mut().unwrap();
    let _ = new.insert(
        "delays".into(),
        json!(
            learning
                .iter()
                .map(|value| f64::from(*value))
                .collect::<Vec<_>>()
        ),
    );
    config
}

fn note(
    id: i64,
    guid: &'static str,
    mid: i64,
    modified: i64,
    fields: &'static str,
) -> Note {
    Note {
        id,
        guid,
        mid,
        modified,
        fields,
        tags: "",
    }
}

fn field(connection: &Connection, sql: &str) -> String {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn number(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn ids(connection: &Connection, sql: &str) -> Vec<i64> {
    let mut statement = connection.prepare(sql).unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

fn col_value(connection: &Connection, column: &str) -> Value {
    let sql = format!("SELECT {column} FROM col WHERE id = 1");
    let text: String =
        connection.query_row(&sql, [], |row| row.get(0)).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn guid_updates_follow_mtime_and_detect_conflicts() {
    let fixture = Fixture::new();
    let models = entries(&[
        model(1, "Basic", &["Front", "Back"]),
        model(2, "Other", &["Front"]),
    ]);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    let destination_notes = [
        note(1, "newer", 1, 100, "old\u{001f}back"),
        note(2, "older", 1, 100, "old\u{001f}back"),
        note(3, "equal", 1, 100, "old\u{001f}back"),
        note(4, "conflict", 1, 100, "old\u{001f}back"),
    ];
    let destination = fixture.collection11(
        "mtime-dest",
        models.clone(),
        decks.clone(),
        presets.clone(),
        &destination_notes,
        &[],
        &[],
    );
    let source = fixture.collection11(
        "mtime-source",
        models,
        decks,
        presets,
        &[
            note(11, "newer", 1, 200, "new\u{001f}back"),
            note(12, "older", 1, 50, "stale\u{001f}back"),
            note(13, "equal", 1, 100, "same\u{001f}back"),
            note(14, "conflict", 2, 200, "conflict"),
        ],
        &[],
        &[],
    );
    {
        let source_connection = Connection::open(&source).unwrap();
        source_connection
            .execute(
                "UPDATE notes SET flags = 7, data = 'payload' WHERE guid = 'newer'",
                [],
            )
            .unwrap();
        let destination_connection = Connection::open(&destination).unwrap();
        destination_connection
            .execute(
                "UPDATE notes SET flags = 1, data = 'old' WHERE guid = 'newer'",
                [],
            )
            .unwrap();
    }
    let package = fixture.package(&source, "mtime");
    let (_, report) = fixture.import(&package, "mtime-dest");

    assert_eq!(report.note_outcomes.updated, 1);
    assert_eq!(report.note_outcomes.duplicate, 2);
    assert_eq!(report.note_outcomes.conflicting, 1);
    let connection = Connection::open(fixture.path("mtime-dest")).unwrap();
    assert_eq!(
        field(&connection, "SELECT flds FROM notes WHERE guid = 'newer'"),
        "new\u{001f}back"
    );
    assert_eq!(
        number(&connection, "SELECT mod FROM notes WHERE guid = 'newer'"),
        200
    );
    assert_eq!(
        field(&connection, "SELECT flds FROM notes WHERE guid = 'older'"),
        "old\u{001f}back"
    );
    assert_eq!(
        field(
            &connection,
            "SELECT flds FROM notes WHERE guid = 'conflict'"
        ),
        "old\u{001f}back"
    );
}

#[test]
fn missing_ordinals_import_and_existing_ordinals_dedupe() {
    let fixture = Fixture::new();
    let mut basic = model(1, "Basic", &["Front", "Back"]);
    add_template(&mut basic, "Card 2", 1);
    let models = entries(&[basic]);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "ord-dest",
        models.clone(),
        decks.clone(),
        presets.clone(),
        &[note(1, "kept", 1, 100, "front\u{001f}back")],
        &[Card {
            id: 10,
            nid: 1,
            did: 1,
            ord: 0,
        }],
        &[],
    );
    let source = fixture.collection11(
        "ord-source",
        models,
        decks,
        presets,
        &[note(1, "kept", 1, 100, "front\u{001f}back")],
        &[
            Card {
                id: 20,
                nid: 1,
                did: 1,
                ord: 0,
            },
            Card {
                id: 21,
                nid: 1,
                did: 1,
                ord: 1,
            },
            Card {
                id: 22,
                nid: 999,
                did: 1,
                ord: 0,
            },
        ],
        &[],
    );
    let package = fixture.package(&source, "ord");
    let (collection, report) = fixture.import(&package, "ord-dest");

    assert_eq!(report.cards, 1);
    assert_eq!(report.cards_missing, 1);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        ids(&connection, "SELECT id FROM cards ORDER BY id"),
        vec![10, 21]
    );
}

#[test]
fn original_id_maps_note_type_and_remaps_fields() {
    let fixture = Fixture::new();
    let mut target = model(50, "Basic", &["Front", "Back"]);
    if let Some(object) = target.as_object_mut() {
        let _ = object.insert("sortf".to_owned(), json!(1));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "original-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[note(1, "note", 50, 100, "one\u{001f}two")],
        &[],
        &[],
    );
    let mut source_model = model(1, "Renamed", &["Extra", "Front", "Back"]);
    let object = source_model.as_object_mut().unwrap();
    let _ = object.insert("originalId".into(), json!(50));
    let _ = object.insert("mod".into(), json!(200));
    let template = object
        .get_mut("tmpls")
        .and_then(Value::as_array_mut)
        .and_then(|templates| templates.first_mut())
        .and_then(Value::as_object_mut)
        .unwrap();
    let _ = template.insert("ord".to_owned(), json!(1));
    let source = fixture.collection11(
        "original-source",
        entries(&[source_model]),
        decks,
        presets,
        &[note(1, "note", 1, 200, "extra\u{001f}one\u{001f}two")],
        &[Card {
            id: 20,
            nid: 1,
            did: 1,
            ord: 1,
        }],
        &[],
    );
    let package = fixture.package(&source, "original");
    let (collection, report) = fixture.import_with(
        &package,
        "original-dest",
        ImportOptions {
            merge_notetypes: true,
            ..ImportOptions::default()
        },
    );

    assert_eq!(report.notetype_outcomes.updated, 1);
    let connection = Connection::open(collection).unwrap();
    let models = col_value(&connection, "models");
    let merged = models.get("50").unwrap();
    let fields = merged
        .get("flds")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.get("name").unwrap().as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["Front", "Back", "Extra"]);
    assert_eq!(
        number(&connection, "SELECT mid FROM notes WHERE guid = 'note'"),
        50
    );
    assert_eq!(
        field(&connection, "SELECT flds FROM notes WHERE guid = 'note'"),
        "one\u{001f}two\u{001f}extra"
    );
    assert_eq!(
        field(&connection, "SELECT sfld FROM notes WHERE guid = 'note'"),
        "two"
    );
    assert_eq!(
        number(&connection, "SELECT ord FROM cards WHERE id = 20"),
        0
    );
}

#[test]
fn schema18_source_writes_legacy_metadata_and_tags() {
    let fixture = Fixture::new();
    let mut preset = ranki_storage::preset_from_json(
        700,
        &preset(700, "Fast", &[5.0, 20.0]),
        true,
    );
    preset.mtime_secs = 0;
    let deck = ranki_storage::deck_from_json(
        700,
        &json!({
            "id": 700, "name": "Fast", "conf": 700, "desc": "fast deck", "dyn": 0,
            "newLimit": 42, "md": true
        }),
    );
    let mut model = ranki_storage::models_from_json(
        &entries(&[model(500, "Extra", &["Front", "Back"])][..]).to_string(),
    )
    .unwrap()
    .remove(0);
    model.raw_json = Some(
        json!({
            "id": 500, "name": "Extra", "flds": [{"name": "Front"}, {"name": "Back"}],
            "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}", "afmt": "{{Back}}", "did": 700}]
        })
        .to_string(),
    );
    let mut tagged = note(1, "tagged", 500, 100, "front\u{001f}back");
    tagged.tags = "alpha";
    let source = fixture.collection18(
        "schema18-source",
        &preset,
        &deck,
        &model,
        &[tagged],
        &[Card {
            id: 10,
            nid: 1,
            did: 700,
            ord: 0,
        }],
    );
    let package = fixture.package(&source, "schema18");
    let (collection, _) = fixture.import(&package, "schema18-to-legacy");

    let connection = Connection::open(collection).unwrap();
    let presets = col_value(&connection, "dconf");
    let fast = presets.get("700").unwrap();
    assert_eq!(fast.get("name").unwrap(), "Fast");
    assert_eq!(
        fast.get("new").unwrap().get("delays").unwrap(),
        &json!([5.0, 20.0])
    );
    let decks = col_value(&connection, "decks");
    let imported_deck = decks.get("700").unwrap();
    assert_eq!(imported_deck.get("newLimit").unwrap(), 42);
    assert_eq!(imported_deck.get("desc").unwrap(), "fast deck");
    let tags = col_value(&connection, "tags");
    assert!(tags.get("alpha").is_some());
    let models = col_value(&connection, "models");
    let template = models
        .get("500")
        .unwrap()
        .get("tmpls")
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(template.get("did").unwrap(), 700);
}

#[test]
fn legacy_source_writes_schema18_rows() {
    let fixture = Fixture::new();
    let mut legacy_model = model(500, "Basic", &["Front", "Back"]);
    if let Some(template) = legacy_model
        .get_mut("tmpls")
        .and_then(Value::as_array_mut)
        .and_then(|templates| templates.first_mut())
        && let Some(object) = template.as_object_mut()
    {
        let _ = object.insert("did".to_owned(), json!(700));
    }
    let models = entries(&[legacy_model]);
    let decks = entries(&[
        deck(1, "Default", 1),
        deck(700, "Fast", 700),
        json!({"id": 701, "name": "Filtered", "dyn": 1, "conf": 1}),
    ]);
    let presets = entries(&[
        preset(1, "Default", &[1.0, 10.0]),
        preset(700, "Fast", &[5.0, 20.0]),
    ]);
    let mut tagged = note(1, "tagged", 500, 100, "front\u{001f}back");
    tagged.tags = "alpha";
    let source = fixture.collection11(
        "legacy-source",
        models,
        decks,
        presets,
        &[tagged],
        &[Card {
            id: 10,
            nid: 1,
            did: 700,
            ord: 0,
        }],
        &[],
    );
    let destination = fixture.path("schema18-dest");
    let _ = fs::remove_file(&destination);
    let connection = Connection::open(&destination).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    connection.execute_batch(SCHEMA11).unwrap();
    connection.execute_batch(SCHEMA18_TABLES).unwrap();
    connection
        .execute(
            "INSERT INTO col (id, crt, mod, scm, ver, dty, usn, ls, conf, models, decks, dconf, tags)
             VALUES (1, 0, 10, 999, 18, 0, 3, 0, '{}', '', '', '', '{}')",
            [],
        )
        .unwrap();
    drop(connection);

    let package = fixture.package(&source, "legacy");
    let (collection, _) = fixture.import(&package, "schema18-dest");
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        ids(&connection, "SELECT id FROM deck_config ORDER BY id"),
        vec![1, 700]
    );
    assert_eq!(
        field(&connection, "SELECT name FROM deck_config WHERE id = 700"),
        "Fast"
    );
    assert_eq!(
        ids(&connection, "SELECT id FROM decks ORDER BY id"),
        vec![1, 700, 701]
    );
    assert_eq!(ids(&connection, "SELECT id FROM notetypes"), vec![500]);
    assert_eq!(
        ids(
            &connection,
            "SELECT ord FROM fields WHERE ntid = 500 ORDER BY ord"
        ),
        vec![0, 1]
    );
    assert_eq!(
        ids(&connection, "SELECT ord FROM templates WHERE ntid = 500"),
        vec![0]
    );
    let conf = col_value(&connection, "conf");
    assert_eq!(conf, json!({}));
    let template_config: Vec<u8> = connection
        .query_row("SELECT config FROM templates WHERE ntid = 500", [], |row| {
            row.get(0)
        })
        .unwrap();
    let target_deck = protobuf_varint(&template_config, 5);
    assert_eq!(target_deck, Some(700));
}

#[test]
fn stamps_use_target_usn_and_duplicate_revlog_ids_are_skipped() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "stamp-dest",
        models.clone(),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[Review { id: 5, cid: 99 }],
    );
    let source = fixture.collection11(
        "stamp-source",
        models,
        decks,
        presets,
        &[note(1, "new", 1, 100, "front\u{001f}back")],
        &[Card {
            id: 10,
            nid: 1,
            did: 1,
            ord: 0,
        }],
        &[Review { id: 5, cid: 10 }, Review { id: 6, cid: 10 }],
    );
    let package = fixture.package(&source, "stamp");
    let (collection, report) = fixture.import(&package, "stamp-dest");

    assert_eq!(report.revlogs, 1);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        ids(&connection, "SELECT id FROM revlog ORDER BY id"),
        vec![5, 6]
    );
    assert_eq!(number(&connection, "SELECT usn FROM notes"), 4);
    assert_eq!(number(&connection, "SELECT usn FROM cards"), 4);
    assert_eq!(number(&connection, "SELECT usn FROM col WHERE id = 1"), 4);
    assert_eq!(number(&connection, "SELECT scm FROM col WHERE id = 1"), 999);
    assert!(number(&connection, "SELECT mod FROM col WHERE id = 1") > 10);
}

#[test]
fn update_options_can_force_equal_mtime_updates() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "always-dest",
        models.clone(),
        decks.clone(),
        presets.clone(),
        &[note(1, "same", 1, 100, "old\u{001f}back")],
        &[],
        &[],
    );
    let source = fixture.collection11(
        "always-source",
        models,
        decks,
        presets,
        &[note(1, "same", 1, 100, "forced\u{001f}back")],
        &[],
        &[],
    );
    let package = fixture.package(&source, "always");
    let (collection, report) = fixture.import_with(
        &package,
        "always-dest",
        ImportOptions {
            update_notes: ranki_format::UpdateCondition::Always,
            ..ImportOptions::default()
        },
    );
    assert_eq!(report.note_outcomes.updated, 1);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        field(&connection, "SELECT flds FROM notes WHERE guid = 'same'"),
        "forced\u{001f}back"
    );
}

#[test]
fn import_options_can_disable_scheduling_and_presets() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    let source = fixture.collection11(
        "options-source",
        models,
        decks,
        presets,
        &[note(1, "options", 1, 100, "front\u{001f}back")],
        &[],
        &[],
    );
    let package = fixture.package(&source, "options");
    let options = ImportOptions {
        with_scheduling: false,
        with_deck_configs: false,
        ..ImportOptions::default()
    };
    let (collection, report) =
        fixture.import_with(&package, "options-import", options);
    assert_eq!(report.preset_outcomes.duplicate, 1);
    let connection = Connection::open(collection).unwrap();
    let presets = col_value(&connection, "dconf");
    assert_eq!(presets.as_object().unwrap().len(), 1);
}

fn protobuf_varint(bytes: &[u8], wanted: u64) -> Option<i64> {
    let mut position = 0;
    while position < bytes.len() {
        let tag = read_varint(bytes, &mut position)?;
        let field = tag >> 3;
        match tag & 7 {
            0 => {
                let value = read_varint(bytes, &mut position)?;
                if field == wanted {
                    return i64::try_from(value).ok();
                }
            }
            1 => {
                position = position.checked_add(8)?;
            }
            2 => {
                let length =
                    usize::try_from(read_varint(bytes, &mut position)?).ok()?;
                position = position.checked_add(length)?;
            }
            5 => {
                position = position.checked_add(4)?;
            }
            _ => return None,
        }
    }
    None
}

fn read_varint(bytes: &[u8], position: &mut usize) -> Option<u64> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*position)?;
        *position = position.checked_add(1)?;
        value |=
            u64::from(byte & 0x7f).checked_shl(u32::try_from(shift).ok()?)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

#[test]
fn unknown_protobuf_fields_survive_schema_conversion() {
    let fixture = Fixture::new();
    let raw = vec![0xaa, 0xbb, 0xcc];
    let mut preset = ranki_storage::preset_from_json(
        700,
        &preset(700, "Fast", &[5.0]),
        true,
    );
    preset.raw = raw.clone();
    let deck = ranki_storage::deck_from_json(
        700,
        &json!({"id": 700, "name": "Fast", "conf": 700, "dyn": 0}),
    );
    let mut model = ranki_storage::models_from_json(
        &entries(&[model(500, "Extra", &["Front", "Back"])][..]).to_string(),
    )
    .unwrap()
    .remove(0);
    model.raw_config = Some(raw.clone());
    let source =
        fixture.collection18("raw-source", &preset, &deck, &model, &[], &[]);
    let first = fixture.package(&source, "raw-first");
    let (legacy, _) = fixture.import(&first, "raw-legacy");
    let second = fixture.package(&legacy, "raw-second");
    let destination = fixture.empty_schema18("raw-normalized");
    let imported = fixture.import_with(
        &second,
        "raw-normalized",
        ImportOptions::default(),
    );
    assert_eq!(imported.0, destination);
    let connection = Connection::open(destination).unwrap();
    let config: Vec<u8> = connection
        .query_row("SELECT config FROM deck_config WHERE id = 700", [], |row| {
            row.get(0)
        })
        .unwrap();
    let decoded = ranki_storage::preset_from_proto(
        700,
        "Fast".to_owned(),
        0,
        0,
        &config,
        true,
    );
    assert_eq!(decoded.raw, raw);
    let models =
        ranki_storage::models_from_tables_with_prefix(&connection, "main")
            .unwrap();
    let imported_model = models
        .iter()
        .find(|entry| entry.id == 500)
        .expect("imported note type");
    assert_eq!(imported_model.raw_config, Some(raw));
}

impl Fixture {
    fn empty_schema18(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        let _ = fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        ranki_storage::register_collations(&connection).unwrap();
        connection.execute_batch(SCHEMA11).unwrap();
        connection.execute_batch(SCHEMA18_TABLES).unwrap();
        connection
            .execute(
                "INSERT INTO col (id, crt, mod, scm, ver, dty, usn, ls, conf, models, decks, dconf, tags)
                 VALUES (1, 0, 10, 999, 18, 0, 3, 0, '{}', '', '', '', '')",
                [],
            )
            .unwrap();
        path
    }
}

#[test]
fn update_never_keeps_target_fields_when_merging() {
    let fixture = Fixture::new();
    let mut target = model(1, "Basic", &["Front", "Back"]);
    if let Some(object) = target.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(100));
    }
    let mut source = model(1, "Basic", &["Front", "Back", "Extra"]);
    if let Some(object) = source.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(200));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "never-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[note(1, "kept", 1, 100, "front\u{001f}back")],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "never-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[note(1, "kept", 1, 200, "front\u{001f}back\u{001f}extra")],
        &[Card {
            id: 20,
            nid: 1,
            did: 1,
            ord: 1,
        }],
        &[],
    );
    let package = fixture.package(&source_path, "never");
    let (collection, report) = fixture.import_with(
        &package,
        "never-dest",
        ImportOptions {
            merge_notetypes: true,
            update_notetypes: ranki_format::UpdateCondition::Never,
            ..ImportOptions::default()
        },
    );
    assert_eq!(report.notetype_outcomes.duplicate, 1);
    let connection = Connection::open(collection).unwrap();
    let models = col_value(&connection, "models");
    let fields = models
        .get("1")
        .unwrap()
        .get("flds")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(fields.len(), 2);
    assert_eq!(
        field(&connection, "SELECT flds FROM notes WHERE guid = 'kept'"),
        "front\u{001f}back"
    );
    assert_eq!(ids(&connection, "SELECT id FROM cards"), Vec::<i64>::new());
}

#[test]
fn normalized_model_names_skip_existing_suffixes() {
    let fixture = Fixture::new();
    let destination = fixture.empty_schema18("names-normalized");
    {
        let connection = Connection::open(&destination).unwrap();
        ranki_storage::register_collations(&connection).unwrap();
        for (id, name) in [(1, "Basic"), (2, "Basic 2")] {
            let mut model = ranki_storage::models_from_json(
                &entries(&[model(id, name, &["Front", "Back"])][..])
                    .to_string(),
            )
            .unwrap()
            .remove(0);
            ranki_storage::insert_model_tables_for_import(
                &connection,
                &model,
                0,
                0,
            )
            .unwrap();
            model.raw_json = None;
        }
    }
    let models = entries(&[model(900, "Basic", &["Extra"])]);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    let source = fixture.collection11(
        "names-source",
        models,
        decks,
        presets,
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source, "names");
    let (collection, _) = fixture.import(&package, "names-normalized");
    let connection = Connection::open(collection).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    let names =
        strings(&connection, "SELECT name FROM notetypes ORDER BY name");
    assert_eq!(
        names,
        [
            "Basic".to_owned(),
            "Basic 2".to_owned(),
            "Basic 3".to_owned()
        ]
    );
}

fn strings(connection: &Connection, sql: &str) -> Vec<String> {
    let mut statement = connection.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|value| value.unwrap())
        .collect()
}

#[test]
fn sort_field_comes_from_the_committed_model() {
    let fixture = Fixture::new();
    let mut target = model(1, "Basic", &["Front", "Back"]);
    if let Some(object) = target.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(100));
        let _ = object.insert("sortf".to_owned(), json!(1));
    }
    let mut source = model(1, "Basic", &["Front", "Back"]);
    if let Some(object) = source.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(200));
        let _ = object.insert("sortf".to_owned(), json!(0));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "sortf-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[note(1, "sorted", 1, 100, "first\u{001f}second")],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "sortf-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[note(1, "sorted", 1, 200, "first\u{001f}second")],
        &[],
        &[],
    );
    let package = fixture.package(&source_path, "sortf");
    let (collection, _) = fixture.import(&package, "sortf-dest");
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        field(&connection, "SELECT sfld FROM notes WHERE guid = 'sorted'"),
        "first"
    );
}

#[test]
fn recomputed_sort_field_strips_html() {
    let fixture = Fixture::new();
    let mut target = model(1, "Basic", &["Front", "Back", "Extra"]);
    if let Some(object) = target.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(100));
    }
    let mut source = model(1, "Basic", &["Extra", "Front", "Back"]);
    if let Some(object) = source.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(200));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "html-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "html-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[note(
            1,
            "html",
            1,
            200,
            "extra\u{001f}<b>one</b>\u{001f}two",
        )],
        &[],
        &[],
    );
    let package = fixture.package(&source_path, "html");
    let (collection, _) = fixture.import_with(
        &package,
        "html-dest",
        ImportOptions {
            merge_notetypes: true,
            ..ImportOptions::default()
        },
    );
    let connection = Connection::open(collection).unwrap();
    let sort_field =
        field(&connection, "SELECT sfld FROM notes WHERE guid = 'html'");
    assert!(!sort_field.contains("<b>"));
    assert_eq!(sort_field, "one");
}

#[test]
fn committed_template_ord_is_preserved_for_new_note_types() {
    let fixture = Fixture::new();
    let mut source_model = model(900, "Single", &["Front", "Back"]);
    if let Some(template) = source_model
        .get_mut("tmpls")
        .and_then(Value::as_array_mut)
        .and_then(|templates| templates.first_mut())
        .and_then(Value::as_object_mut)
    {
        let _ = template.insert("ord".to_owned(), json!(1));
    }
    let decks = entries(&[deck(1, "Default", 1), deck(701, "Other", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    let source = fixture.collection11(
        "ord-source",
        entries(&[source_model]),
        decks,
        presets,
        &[note(1, "ord", 900, 100, "front\u{001f}back")],
        &[Card {
            id: 20,
            nid: 1,
            did: 701,
            ord: 1,
        }],
        &[],
    );
    let package = fixture.package(&source, "ord");
    let (collection, report) = fixture.import(&package, "ord-fresh");
    assert_eq!(report.cards, 1);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        number(&connection, "SELECT ord FROM cards WHERE id = 20"),
        1
    );
}

#[test]
fn skipped_update_drops_cards_for_absent_templates() {
    let fixture = Fixture::new();
    let target = model(1, "Basic", &["Front", "Back"]);
    let mut source = model(1, "Basic", &["Front", "Back"]);
    add_template(&mut source, "Card 2", 1);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "absent-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[note(1, "absent", 1, 100, "front\u{001f}back")],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "absent-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[note(1, "absent", 1, 100, "front\u{001f}back")],
        &[Card {
            id: 20,
            nid: 1,
            did: 1,
            ord: 1,
        }],
        &[],
    );
    let package = fixture.package(&source_path, "absent");
    let (collection, report) = fixture.import(&package, "absent-dest");
    assert_eq!(report.cards, 0);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(ids(&connection, "SELECT id FROM cards"), Vec::<i64>::new());
}

fn append_varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        bytes.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
}

fn proto_varint(field: u32, value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_varint(&mut bytes, (u64::from(field) << 3) | 0);
    append_varint(&mut bytes, value);
    bytes
}

#[test]
fn unknown_preset_fields_survive_changed_and_unchanged_imports() {
    let fixture = Fixture::new();
    let mut preset = ranki_storage::preset_from_json(
        700,
        &preset(700, "Fast", &[5.0]),
        true,
    );
    let source = fixture.collection18(
        "preset-raw-source",
        &preset,
        &ranki_storage::deck_from_json(
            1,
            &json!({"id": 1, "name": "Default", "conf": 700, "dyn": 0}),
        ),
        &ranki_storage::models_from_json(
            &entries(&[model(500, "Extra", &["Front"])][..]).to_string(),
        )
        .unwrap()
        .remove(0),
        &[],
        &[],
    );
    {
        let connection = Connection::open(&source).unwrap();
        let config: Vec<u8> = connection
            .query_row(
                "SELECT config FROM deck_config WHERE id = 700",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "UPDATE deck_config SET config = ?1 WHERE id = 700",
                params![config],
            )
            .unwrap();
    }
    let destination = fixture.empty_schema18("preset-raw-dest");
    {
        let connection = Connection::open(&destination).unwrap();
        preset.name = "Other".to_owned();
        let mut config = ranki_storage::preset_to_proto(&preset);
        config.extend(proto_varint(99, 7));
        connection
            .execute(
                "INSERT INTO deck_config (id, name, mtime_secs, usn, config) VALUES (700, 'Fast', 0, 0, ?1)",
                params![config],
            )
            .unwrap();
    }
    let package = fixture.package(&source, "preset-raw");
    let (collection, _) = fixture.import(&package, "preset-raw-dest");
    let connection = Connection::open(collection).unwrap();
    let config: Vec<u8> = connection
        .query_row("SELECT config FROM deck_config WHERE id = 700", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(protobuf_varint(&config, 99), Some(7));
}

#[test]
fn colliding_preset_id_does_not_replace_unrelated_preset() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(1, "Default", 7)]);
    let presets = entries(&[preset(7, "Slow", &[1.0, 10.0])]);
    fixture.collection11(
        "collide-dest",
        models.clone(),
        decks.clone(),
        presets,
        &[],
        &[],
        &[],
    );
    let source = fixture.collection11(
        "collide-source",
        models,
        decks,
        entries(&[preset(7, "Fast", &[5.0, 20.0])]),
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source, "collide");
    let (collection, _) = fixture.import(&package, "collide-dest");
    let connection = Connection::open(collection).unwrap();
    let presets = col_value(&connection, "dconf");
    assert_eq!(presets.as_object().unwrap().len(), 2);
    assert_eq!(presets.get("7").unwrap().get("name").unwrap(), "Slow");
}

#[test]
fn no_deck_configs_keeps_matched_deck_preset() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(1, "Default", 2)]);
    let presets = entries(&[preset(2, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "keep-preset-dest",
        models.clone(),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    let source = fixture.collection11(
        "keep-preset-source",
        models,
        decks,
        presets,
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source, "keep-preset");
    let (collection, _) = fixture.import_with(
        &package,
        "keep-preset-dest",
        ImportOptions {
            with_deck_configs: false,
            ..ImportOptions::default()
        },
    );
    let connection = Connection::open(collection).unwrap();
    let decks = col_value(&connection, "decks");
    assert_eq!(decks.get("1").unwrap().get("conf").unwrap(), 2);
}

#[test]
fn active_daily_limit_survives_zero_study_counter() {
    let fixture = Fixture::new();
    let value = json!({
        "id": 1, "name": "Default", "conf": 1, "dyn": 0,
        "newLimitToday": [42, 200], "newToday": [0, 0]
    });
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[value]);
    let presets = entries(&[preset(1, "Default", &[1.0, 10.0])]);
    fixture.collection11(
        "day-limit-dest",
        models.clone(),
        entries(&[deck(1, "Default", 1)]),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    let source = fixture.collection11(
        "day-limit-source",
        models,
        decks,
        presets,
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source, "day-limit");
    let (collection, _) = fixture.import(&package, "day-limit-dest");
    let connection = Connection::open(collection).unwrap();
    let decks = col_value(&connection, "decks");
    let limit = decks.get("1").unwrap().get("newLimitToday").unwrap();
    assert_eq!(limit.get(0).unwrap(), 42);
}

#[test]
fn filtered_parent_replacement_is_shared_between_children() {
    let fixture = Fixture::new();
    let destination = fixture.empty_schema18("filtered-parent-dest");
    let filtered = ranki_storage::deck_from_json(
        9,
        &json!({"id": 9, "name": "Trips", "conf": 1, "dyn": 1}),
    );
    let (common, kind) = ranki_storage::deck_to_proto(&filtered);
    let connection = Connection::open(&destination).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO decks (id, name, mtime_secs, usn, common, kind) VALUES (9, 'Trips', 0, 0, ?1, ?2)",
            params![common, kind],
        )
        .unwrap();
    drop(connection);
    let models = entries(&[model(1, "Basic", &["Front"])]);
    let decks = entries(&[
        deck(1, "Default", 1),
        deck(20, "Trips::One", 1),
        deck(21, "Trips::Two", 1),
    ]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    let source = fixture.collection11(
        "filtered-parent-source",
        models,
        decks,
        presets,
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source, "filtered-parent");
    let (collection, _) = fixture.import(&package, "filtered-parent-dest");
    let connection = Connection::open(collection).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    let names = strings(&connection, "SELECT name FROM decks ORDER BY name");
    let replacement = names
        .iter()
        .find(|name| name.starts_with("Trips "))
        .expect("replacement parent");
    assert_eq!(
        names
            .iter()
            .filter(|name| name.starts_with(&format!("{replacement}\u{1f}")))
            .count(),
        2
    );
}

#[test]
fn merge_notetypes_keeps_cloze_and_normal_separate() {
    let fixture = Fixture::new();
    let mut target = model(50, "Basic", &["Front"]);
    let mut source = model(50, "Basic", &["Front"]);
    if let Some(object) = source.as_object_mut() {
        let _ = object.insert("type".to_owned(), json!(1));
        let _ = object.insert("mod".to_owned(), json!(200));
    }
    if let Some(object) = target.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(100));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    fixture.collection11(
        "cloze-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "cloze-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[note(1, "cloze", 50, 200, "{{c1::Paris}}\u{001f}back")],
        &[],
        &[],
    );
    let package = fixture.package(&source_path, "cloze");
    let (collection, _) = fixture.import_with(
        &package,
        "cloze-dest",
        ImportOptions {
            merge_notetypes: true,
            ..ImportOptions::default()
        },
    );
    let connection = Connection::open(collection).unwrap();
    let models = col_value(&connection, "models");
    assert_eq!(models.as_object().unwrap().len(), 2);
    let mid = number(&connection, "SELECT mid FROM notes WHERE guid = 'cloze'");
    assert_eq!(
        models.get(&mid.to_string()).unwrap().get("type").unwrap(),
        1
    );
}

#[test]
fn reimport_reuses_original_id_remapped_note_type() {
    let fixture = Fixture::new();
    let target = model(50, "Basic", &["Front"]);
    let mut source = model(50, "Basic", &["Front", "Extra"]);
    if let Some(object) = source.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(200));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    fixture.collection11(
        "remap-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "remap-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[note(1, "remap", 50, 200, "front\u{001f}extra")],
        &[],
        &[],
    );
    let package = fixture.package(&source_path, "remap");
    let options = ImportOptions::default();
    let (collection, _) = fixture.import_with(&package, "remap-dest", options);
    let first_mid = number(
        &Connection::open(&collection).unwrap(),
        "SELECT mid FROM notes",
    );
    let (_, _) = fixture.import_with(&package, "remap-dest", options);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(number(&connection, "SELECT mid FROM notes"), first_mid);
    assert_eq!(
        col_value(&connection, "models").as_object().unwrap().len(),
        2
    );
}

#[test]
fn effective_note_type_mtime_controls_later_updates() {
    let fixture = Fixture::new();
    let mut target = model(1, "Basic", &["Front"]);
    if let Some(object) = target.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(100));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    fixture.collection11(
        "mtime-chain-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    for (name, modified) in [("newer", 200), ("older", 150)] {
        let mut source = model(1, "Basic", &["Front"]);
        if let Some(object) = source.as_object_mut() {
            let _ = object.insert("mod".to_owned(), json!(modified));
            let _ = object
                .insert("css".to_owned(), json!(format!("body {{ {name} }}")));
        }
        let source_path = fixture.collection11(
            &format!("mtime-chain-{name}"),
            entries(slice::from_ref(&source)),
            decks.clone(),
            presets.clone(),
            &[],
            &[],
            &[],
        );
        let package = fixture.package(&source_path, name);
        let (_, _) = fixture.import(&package, "mtime-chain-dest");
    }
    let connection =
        Connection::open(fixture.path("mtime-chain-dest")).unwrap();
    let models = col_value(&connection, "models");
    assert_eq!(
        models.get("1").unwrap().get("css").unwrap(),
        "body { newer }"
    );
}

#[test]
fn appended_template_keeps_existing_ordinal() {
    let fixture = Fixture::new();
    let mut target = model(1, "Basic", &["Front"]);
    let template = target
        .get_mut("tmpls")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .first_mut()
        .unwrap()
        .as_object_mut()
        .unwrap();
    let _ = template.insert("ord".to_owned(), json!(1));
    let mut source = target.clone();
    add_template(&mut source, "Card 2", 2);
    if let Some(object) = source.as_object_mut() {
        let _ = object.insert("mod".to_owned(), json!(200));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    fixture.collection11(
        "template-ord-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "template-ord-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source_path, "template-ord");
    let (collection, _) = fixture.import(&package, "template-ord-dest");
    let connection = Connection::open(collection).unwrap();
    let models = col_value(&connection, "models");
    let ords: Vec<i64> = models
        .get("1")
        .unwrap()
        .get("tmpls")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.get("ord").unwrap().as_i64().unwrap())
        .collect();
    assert_eq!(ords, vec![1, 2]);
}

#[test]
fn cloze_deletion_ordinals_survive_equal_mtime() {
    let fixture = Fixture::new();
    let mut target = model(50, "Cloze", &["Text"]);
    let mut source = target.clone();
    if let Some(object) = target.as_object_mut() {
        let _ = object.insert("type".to_owned(), json!(1));
    }
    if let Some(object) = source.as_object_mut() {
        let _ = object.insert("type".to_owned(), json!(1));
    }
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    fixture.collection11(
        "cloze-ord-dest",
        entries(slice::from_ref(&target)),
        decks.clone(),
        presets.clone(),
        &[note(1, "cloze-ord", 50, 100, "{{c1::one}} {{c2::two}}")],
        &[],
        &[],
    );
    let source_path = fixture.collection11(
        "cloze-ord-source",
        entries(slice::from_ref(&source)),
        decks,
        presets,
        &[note(1, "cloze-ord", 50, 100, "{{c1::one}} {{c2::two}}")],
        &[Card {
            id: 20,
            nid: 1,
            did: 1,
            ord: 2,
        }],
        &[],
    );
    let package = fixture.package(&source_path, "cloze-ord");
    let (collection, _) = fixture.import(&package, "cloze-ord-dest");
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        number(&connection, "SELECT ord FROM cards WHERE id = 20"),
        2
    );
}

#[test]
fn no_scheduling_returns_filtered_cards_to_home_deck() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(1, "Default", 1), deck(9, "Filtered", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    let source = fixture.collection11(
        "home-deck-source",
        models,
        decks,
        presets,
        &[note(1, "home", 1, 100, "front\u{001f}back")],
        &[Card {
            id: 20,
            nid: 1,
            did: 9,
            ord: 0,
        }],
        &[],
    );
    {
        let connection = Connection::open(&source).unwrap();
        connection
            .execute("UPDATE cards SET odid = 3 WHERE id = 20", [])
            .unwrap();
    }
    let package = fixture.package(&source, "home-deck");
    let (collection, _) = fixture.import_with(
        &package,
        "home-deck-dest",
        ImportOptions {
            with_scheduling: false,
            ..ImportOptions::default()
        },
    );
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        number(&connection, "SELECT did FROM cards WHERE id = 20"),
        3
    );
    assert_eq!(
        number(&connection, "SELECT odid FROM cards WHERE id = 20"),
        0
    );
}

#[test]
fn col_mod_advances_past_existing_stamp() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(1, "Default", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    let destination = fixture.collection11(
        "stamp-monotonic-dest",
        models.clone(),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    {
        let connection = Connection::open(&destination).unwrap();
        connection
            .execute(
                "UPDATE col SET mod = 9999999999999, scm = 4242 WHERE id = 1",
                [],
            )
            .unwrap();
    }
    let source = fixture.collection11(
        "stamp-monotonic-source",
        models,
        decks,
        presets,
        &[note(1, "stamp", 1, 100, "front\u{001f}back")],
        &[],
        &[],
    );
    let package = fixture.package(&source, "stamp-monotonic");
    let (collection, _) = fixture.import(&package, "stamp-monotonic-dest");
    let connection = Connection::open(collection).unwrap();
    assert!(
        number(&connection, "SELECT mod FROM col WHERE id = 1") > 9999999999999
    );
    assert_eq!(
        number(&connection, "SELECT scm FROM col WHERE id = 1"),
        4242
    );
}

#[test]
fn target_collection_selections_stay_local() {
    let fixture = Fixture::new();
    let models = entries(&[model(1, "Basic", &["Front", "Back"])]);
    let decks = entries(&[deck(7, "Study", 1)]);
    let presets = entries(&[preset(1, "Default", &[1.0])]);
    let destination = fixture.collection11(
        "selections-dest",
        models.clone(),
        decks.clone(),
        presets.clone(),
        &[],
        &[],
        &[],
    );
    {
        let connection = Connection::open(&destination).unwrap();
        connection
            .execute(
                "UPDATE col SET conf = ?1 WHERE id = 1",
                params![
                    json!({"curDeck": 7, "curModel": 1, "activeDecks": [7]})
                        .to_string()
                ],
            )
            .unwrap();
    }
    let source = fixture.collection11(
        "selections-source",
        models,
        entries(&[deck(7, "Travel", 1)]),
        presets,
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source, "selections");
    let (collection, _) = fixture.import(&package, "selections-dest");
    let connection = Connection::open(collection).unwrap();
    let conf = col_value(&connection, "conf");
    assert_eq!(conf.get("curDeck").unwrap(), 7);
    assert_eq!(conf.get("activeDecks").unwrap(), &json!([7]));
}

#[test]
fn normalized_deck_description_reaches_kind_blob() {
    let fixture = Fixture::new();
    let destination = fixture.empty_schema18("description-dest");
    let deck = ranki_storage::deck_from_json(
        1,
        &json!({"id": 1, "name": "Default", "conf": 1, "desc": "old", "dyn": 0}),
    );
    let (common, kind) = ranki_storage::deck_to_proto(&deck);
    let connection = Connection::open(&destination).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO decks (id, name, mtime_secs, usn, common, kind) VALUES (1, 'Default', 0, 0, ?1, ?2)",
            params![common, kind],
        )
        .unwrap();
    drop(connection);
    let models = entries(&[model(1, "Basic", &["Front"])]);
    let source = fixture.collection11(
        "description-source",
        models,
        entries(&[json!({"id": 1, "name": "Default", "conf": 1, "desc": "new", "dyn": 0})]),
        entries(&[preset(1, "Default", &[1.0])]),
        &[],
        &[],
        &[],
    );
    let package = fixture.package(&source, "description");
    let (collection, _) = fixture.import(&package, "description-dest");
    let connection = Connection::open(collection).unwrap();
    let kind: Vec<u8> = connection
        .query_row("SELECT kind FROM decks WHERE id = 1", [], |row| row.get(0))
        .unwrap();
    let normal = read_proto_bytes_for_test(&kind, 1);
    assert_eq!(
        String::from_utf8_lossy(&read_proto_bytes_for_test(&normal, 4)),
        "new"
    );
}

#[test]
fn unchanged_normalized_note_type_keeps_mtime() {
    let fixture = Fixture::new();
    let destination = fixture.empty_schema18("mtime-keep-dest");
    let model_entry = ranki_storage::models_from_json(
        &entries(&[model(1, "Basic", &["Front"])][..]).to_string(),
    )
    .unwrap()
    .remove(0);
    let connection = Connection::open(&destination).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    ranki_storage::insert_model_tables_for_import(
        &connection,
        &model_entry,
        50,
        0,
    )
    .unwrap();
    drop(connection);
    let source = fixture.collection11(
        "mtime-keep-source",
        entries(&[model(1, "Basic", &["Front"])]),
        entries(&[deck(1, "Default", 1)]),
        entries(&[preset(1, "Default", &[1.0])]),
        &[note(1, "keep", 1, 100, "front")],
        &[],
        &[],
    );
    let package = fixture.package(&source, "mtime-keep");
    let (collection, _) = fixture.import(&package, "mtime-keep-dest");
    let connection = Connection::open(collection).unwrap();
    assert_eq!(
        number(&connection, "SELECT mtime_secs FROM notetypes WHERE id = 1"),
        50
    );
}

fn read_proto_bytes_for_test(bytes: &[u8], wanted: u64) -> Vec<u8> {
    let mut position = 0;
    while position < bytes.len() {
        let tag = read_varint(bytes, &mut position).unwrap();
        if (tag >> 3, tag & 7) == (wanted, 2) {
            let length = read_varint(bytes, &mut position).unwrap() as usize;
            return bytes[position..position + length].to_vec();
        }
        match tag & 7 {
            2 => {
                let length =
                    read_varint(bytes, &mut position).unwrap() as usize;
                position += length;
            }
            0 => {
                read_varint(bytes, &mut position).unwrap();
            }
            1 => position += 8,
            5 => position += 4,
            _ => break,
        }
    }
    Vec::new()
}

#[test]
fn remapped_preset_keeps_unknown_fields() {
    let fixture = Fixture::new();
    let mut fast =
        ranki_storage::preset_from_json(7, &preset(7, "Fast", &[5.0]), true);
    fast.name = "Fast".to_owned();
    let slow =
        ranki_storage::preset_from_json(7, &preset(7, "Slow", &[1.0]), true);
    let model = ranki_storage::models_from_json(
        &entries(&[model(500, "Extra", &["Front"])][..]).to_string(),
    )
    .unwrap()
    .remove(0);
    let deck = ranki_storage::deck_from_json(
        1,
        &json!({"id": 1, "name": "Default", "conf": 7, "dyn": 0}),
    );
    fixture.collection18(
        "remapped-preset-dest",
        &slow,
        &deck,
        &model,
        &[],
        &[],
    );
    let source = fixture.collection18(
        "remapped-preset-source",
        &fast,
        &deck,
        &model,
        &[],
        &[],
    );
    {
        let connection = Connection::open(&source).unwrap();
        let mut config: Vec<u8> = connection
            .query_row(
                "SELECT config FROM deck_config WHERE id = 7",
                [],
                |row| row.get(0),
            )
            .unwrap();
        config.extend(proto_varint(99, 7));
        connection
            .execute(
                "UPDATE deck_config SET config = ?1 WHERE id = 7",
                params![config],
            )
            .unwrap();
    }
    let package = fixture.package(&source, "remapped-preset");
    let (collection, _) = fixture.import(&package, "remapped-preset-dest");
    let connection = Connection::open(collection).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    let id: i64 = connection
        .query_row(
            "SELECT id FROM deck_config WHERE name = 'Fast'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(id, 7);
    let config: Vec<u8> = connection
        .query_row(
            "SELECT config FROM deck_config WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(protobuf_varint(&config, 99), Some(7));
}
