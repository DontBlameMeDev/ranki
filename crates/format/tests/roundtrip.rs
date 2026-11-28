#![expect(clippy::unwrap_used, reason = "test fixtures use unwrap for setup")]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use ranki_format::ImportReport;
use ranki_format::export_apkg;
use ranki_format::export_colpkg;
use ranki_format::import_apkg;
use ranki_format::import_colpkg;
use ranki_model::DeckSteps;
use ranki_model::Store;
use ranki_storage::Collection as AnkiCollection;
use rusqlite::Connection;
use rusqlite::params;
use serde_json::Value;
use serde_json::json;
use sha1::Digest as _;
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const TEST_SCHEMA: &str = "
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

fn real_fixture() -> Option<PathBuf> {
    env::var_os("RANKI_TEST_APKG")
        .map(PathBuf::from)
        .filter(|path| path.exists())
}
const DAY_SECS: i64 = 86_400;

#[derive(Clone)]
struct TestNote {
    id: i64,
    guid: String,
    mid: i64,
    fields: String,
}

#[derive(Clone)]
struct TestCard {
    id: i64,
    note_id: i64,
    deck_id: i64,
    ordinal: i32,
}

#[derive(Clone)]
struct Seed {
    notes: Vec<TestNote>,
    cards: Vec<TestCard>,
    revlog: bool,
}

struct Fixture {
    dir: TempDir,
    source: PathBuf,
    media: PathBuf,
    seed: Seed,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.anki2");
        let media = dir.path().join("source-media");
        fs::create_dir_all(&media).unwrap();
        let seed = Seed {
            notes: vec![
                TestNote {
                    id: 1,
                    guid: "guid-a".to_owned(),
                    mid: 1,
                    fields: "front <img src=\"photo.png\">\u{001f}back"
                        .to_owned(),
                },
                TestNote {
                    id: 2,
                    guid: "guid-b".to_owned(),
                    mid: 1,
                    fields: "second\u{001f}answer".to_owned(),
                },
            ],
            cards: vec![
                TestCard {
                    id: 100,
                    note_id: 1,
                    deck_id: 1,
                    ordinal: 0,
                },
                TestCard {
                    id: 101,
                    note_id: 2,
                    deck_id: 1,
                    ordinal: 0,
                },
            ],
            revlog: true,
        };
        write_collection(&source, &seed);
        Self {
            dir,
            source,
            media,
            seed,
        }
    }

    fn package(&self) -> PathBuf {
        let package = self.dir.path().join("deck.apkg");
        export_apkg(&self.source, &self.media, &package).unwrap();
        package
    }

    fn import(
        &self,
        package: &Path,
        name: &str,
    ) -> (PathBuf, PathBuf, ImportReport) {
        let collection = self.dir.path().join(format!("{name}.anki2"));
        let media = self.dir.path().join(format!("{name}-media"));
        let report = import_apkg(package, &collection, &media).unwrap();
        (collection, media, report)
    }
}

struct SourceMeta {
    models: Value,
    decks: Value,
}

impl SourceMeta {
    fn fixture() -> Self {
        Self {
            models: json!({"1": {"id": 1, "name": "Basic", "did": 1}}),
            decks: json!({"1": {"id": 1, "name": "Default", "conf": 1}}),
        }
    }
}

fn write_collection(path: &Path, seed: &Seed) {
    write_collection_with(path, seed, &SourceMeta::fixture());
}

fn write_collection_with(path: &Path, seed: &Seed, meta: &SourceMeta) {
    let _ = fs::remove_file(path);
    let mut connection = Connection::open(path).unwrap();
    connection.execute_batch(TEST_SCHEMA).unwrap();
    let conf =
        json!({"activeDecks": [1], "curDeck": 1, "curModel": 1}).to_string();
    let models = meta.models.to_string();
    let decks = meta.decks.to_string();
    let dconf = json!({"1": {"id": 1, "name": "Default"}}).to_string();
    let tags = json!({}).to_string();
    connection
        .execute(
            "INSERT INTO col (id, crt, mod, scm, ver, dty, usn, ls, conf, models, decks, dconf, tags)
             VALUES (1, 0, 0, 0, 11, 0, 0, 0, ?, ?, ?, ?, ?)",
            params![conf, models, decks, dconf, tags],
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();

    for note in &seed.notes {
        let sort_field = note.fields.split('\u{001f}').next().unwrap_or("");
        transaction
            .execute(
                "INSERT INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data)
                 VALUES (?, ?, ?, 0, 0, '', ?, ?, 0, 0, '')",
                params![note.id, note.guid, note.mid, note.fields, sort_field],
            )
            .unwrap();
    }
    for card in &seed.cards {
        transaction
            .execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor,
                     reps, lapses, left, odue, odid, flags, data)
                 VALUES (?, ?, ?, ?, 0, 0, 0, 0, 1, 0, 2500, 0, 0, 0, 0, 0, 0, '')",
                params![card.id, card.note_id, card.deck_id, card.ordinal],
            )
            .unwrap();
    }
    if seed.revlog {
        transaction
            .execute(
                "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type)
                 VALUES (1000, 100, 0, 3, 1, 0, 2500, 1200, 0)",
                [],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn strings(connection: &Connection, sql: &str) -> Vec<String> {
    let mut statement = connection.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|value| value.unwrap())
        .collect()
}

fn ids(connection: &Connection, sql: &str) -> Vec<i64> {
    let mut statement = connection.prepare(sql).unwrap();
    statement
        .query_map([], |row| row.get::<_, i64>(0))
        .unwrap()
        .map(|value| value.unwrap())
        .collect()
}

fn query_string(connection: &Connection, sql: &str, value: &str) -> String {
    connection
        .query_row(sql, params![value], |row| row.get(0))
        .unwrap()
}

fn query_id(connection: &Connection, sql: &str, value: &str) -> i64 {
    connection
        .query_row(sql, params![value], |row| row.get(0))
        .unwrap()
}

fn build_zip(path: &Path, entries: &[(&str, Vec<u8>)]) {
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    for (name, bytes) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn entry_bytes(archive: &mut zip::ZipArchive<File>, name: &str) -> Vec<u8> {
    let mut entry = archive.by_name(name).unwrap();
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).unwrap();
    bytes
}

fn unix_seconds() -> i64 {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(elapsed.as_secs()).unwrap()
}

fn write_review_source(path: &Path, crt: i64, due: i64) {
    let seed = Seed {
        notes: vec![TestNote {
            id: 1,
            guid: "review-guid".to_owned(),
            mid: 1,
            fields: "question\u{001f}answer".to_owned(),
        }],
        cards: vec![TestCard {
            id: 100,
            note_id: 1,
            deck_id: 1,
            ordinal: 0,
        }],
        revlog: false,
    };
    write_collection(path, &seed);
    let connection = Connection::open(path).unwrap();
    connection
        .execute("UPDATE col SET crt = ?1 WHERE id = 1", [crt])
        .unwrap();
    connection
        .execute(
            "UPDATE cards SET type = 2, queue = 2, ivl = 10, due = ?1 WHERE id = 100",
            [due],
        )
        .unwrap();
}

fn set_collection_crt(path: &Path, crt: i64) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute("UPDATE col SET crt = ?1 WHERE id = 1", [crt])
        .unwrap();
}

#[test]
fn roundtrip_notes_cards_and_review_log() {
    let fixture = Fixture::new();
    let package = fixture.package();
    let (collection, _media, report) = fixture.import(&package, "fresh");

    assert_eq!(report.notes, 2);
    assert_eq!(report.cards, 2);
    assert_eq!(report.media, 0);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(count(&connection, "notes"), 2);
    assert_eq!(count(&connection, "cards"), 2);
    assert_eq!(count(&connection, "revlog"), 1);
    let guids = strings(&connection, "SELECT guid FROM notes ORDER BY guid");
    assert_eq!(guids, vec!["guid-a", "guid-b"]);
}

#[test]
fn fresh_import_preserves_source_crt_and_due_scale() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.anki2");
    let now = unix_seconds();
    let source_days = 20_i64;
    let source_crt = now.saturating_sub(DAY_SECS.saturating_mul(source_days));
    let source_due = source_days.saturating_add(5);
    write_review_source(&source, source_crt, source_due);

    let package = dir.path().join("review.apkg");
    export_apkg(&source, &dir.path().join("media"), &package).unwrap();
    let collection = dir.path().join("fresh.anki2");
    import_apkg(&package, &collection, &dir.path().join("fresh-media"))
        .unwrap();

    let connection = Connection::open(&collection).unwrap();
    let stored_crt: i64 = connection
        .query_row("SELECT crt FROM col WHERE id = 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored_crt, source_crt);
    drop(connection);

    let store = AnkiCollection::open(&collection).unwrap();
    let due = store.due_cards(None, 10, now).unwrap();
    assert_eq!(due.as_slice(), &[]);
    let card = store.card(100).unwrap().unwrap();
    assert_eq!(card.due, source_due);
}

#[test]
fn import_keeps_deck_learning_steps() {
    let fixture = Fixture::new();
    let connection = Connection::open(&fixture.source).unwrap();
    let dconf = json!({
        "1": {
            "id": 1,
            "name": "Default",
            "new": {"delays": [5.0, 20.0]},
            "lapse": {"delays": [3.0, 0.5]},
        }
    });
    connection
        .execute(
            "UPDATE col SET dconf = ?1 WHERE id = 1",
            [dconf.to_string()],
        )
        .unwrap();
    drop(connection);

    let package = fixture.package();
    let (collection, _, _) = fixture.import(&package, "steps");
    let store = AnkiCollection::open(&collection).unwrap();
    let deck_id = store
        .decks()
        .unwrap()
        .into_iter()
        .find(|deck| deck.name == "Default")
        .unwrap()
        .id;
    assert_eq!(
        store.deck_steps(deck_id).unwrap(),
        DeckSteps {
            learning: vec![300, 1_200],
            relearning: vec![180, 30],
        }
    );
}

#[test]
fn existing_import_shifts_review_due_to_destination_scale() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("existing.anki2");
    let created = AnkiCollection::create(&destination).unwrap();
    let destination_crt = created.collection_created().unwrap();
    drop(created);

    let source_days = 20_i64;
    let source_crt =
        destination_crt.saturating_sub(DAY_SECS.saturating_mul(source_days));
    let source_due = source_days.saturating_add(5);
    let source = dir.path().join("source.anki2");
    write_review_source(&source, source_crt, source_due);

    let package = dir.path().join("review.apkg");
    export_apkg(&source, &dir.path().join("media"), &package).unwrap();
    import_apkg(&package, &destination, &dir.path().join("shifted-media"))
        .unwrap();

    let store = AnkiCollection::open(&destination).unwrap();
    let card = store.card(100).unwrap().unwrap();
    assert_eq!(card.due, 5);
    let due = store.due_cards(None, 10, destination_crt).unwrap();
    assert_eq!(due.as_slice(), &[]);
}

#[test]
fn existing_import_uses_day_indices_when_crt_times_differ() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("existing.anki2");
    let created = AnkiCollection::create(&destination).unwrap();
    drop(created);

    let now = unix_seconds();
    let half_day = DAY_SECS.saturating_div(2);
    let destination_crt = now.saturating_sub(half_day);
    let source_crt = now
        .saturating_sub(DAY_SECS.saturating_mul(20))
        .saturating_sub(half_day.saturating_add(DAY_SECS.saturating_div(4)));
    set_collection_crt(&destination, destination_crt);

    let source = dir.path().join("source.anki2");
    write_review_source(&source, source_crt, 20);

    let package = dir.path().join("review.apkg");
    export_apkg(&source, &dir.path().join("media"), &package).unwrap();
    import_apkg(&package, &destination, &dir.path().join("shifted-media"))
        .unwrap();

    let connection = Connection::open(&destination).unwrap();
    let due: i64 = connection
        .query_row("SELECT due FROM cards WHERE id = 100", [], |row| row.get(0))
        .unwrap();
    assert_eq!(due, 0);
    drop(connection);

    let store = AnkiCollection::open(&destination).unwrap();
    let due_cards = store.due_cards(None, 10, now).unwrap();
    assert!(due_cards.iter().any(|card| card.id == 100));
}

fn write_review_shift_source(path: &Path, crt: i64, due: i64) {
    let notes = [
        ("suspended-review", "suspended\u{001f}answer"),
        ("odue-review", "odue\u{001f}answer"),
        ("new-card", "new\u{001f}answer"),
        ("learning-card", "learning\u{001f}answer"),
        ("no-original-due", "no-original\u{001f}answer"),
    ];
    let seed = Seed {
        notes: notes
            .iter()
            .enumerate()
            .map(|(index, (guid, fields))| TestNote {
                id: i64::try_from(index).unwrap().saturating_add(1),
                guid: (*guid).to_owned(),
                mid: 1,
                fields: (*fields).to_owned(),
            })
            .collect(),
        cards: (1_i64..=5)
            .map(|id| TestCard {
                id: id.saturating_add(99),
                note_id: id,
                deck_id: 1,
                ordinal: 0,
            })
            .collect(),
        revlog: false,
    };
    write_collection(path, &seed);
    let connection = Connection::open(path).unwrap();
    connection
        .execute("UPDATE col SET crt = ?1 WHERE id = 1", [crt])
        .unwrap();
    connection
        .execute(
            "UPDATE cards SET type = 2, queue = -1, due = ?1, odue = ?2 WHERE id = 100",
            params![due, due.saturating_add(2)],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE cards SET type = 2, queue = 2, due = ?1, odue = ?2 WHERE id = 101",
            params![due, due.saturating_add(5)],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE cards SET type = 0, queue = 0, due = 42, odue = 99 WHERE id = 102",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE cards SET type = 1, queue = 1, due = 999999, odue = 888888 WHERE id = 103",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE cards SET type = 2, queue = 2, due = ?1, odue = 0 WHERE id = 104",
            [due],
        )
        .unwrap();
}

type CardRowData = (i64, i64, i64, i64, i64);

fn card_rows(path: &Path) -> Vec<CardRowData> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("SELECT id, type, queue, due, odue FROM cards ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[test]
fn existing_import_shifts_suspended_and_original_due_for_reviews() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("existing.anki2");
    let created = AnkiCollection::create(&destination).unwrap();
    drop(created);

    let now = unix_seconds();
    let destination_crt = now.saturating_sub(DAY_SECS.saturating_div(2));
    set_collection_crt(&destination, destination_crt);
    let source_crt =
        destination_crt.saturating_sub(DAY_SECS.saturating_mul(20));
    let source_due = 20_i64.saturating_add(5);

    let source = dir.path().join("source.anki2");
    write_review_shift_source(&source, source_crt, source_due);

    let package = dir.path().join("reviews.apkg");
    export_apkg(&source, &dir.path().join("media"), &package).unwrap();
    import_apkg(&package, &destination, &dir.path().join("shifted-media"))
        .unwrap();

    assert_eq!(
        card_rows(&destination),
        vec![
            (100, 2, -1, 5, 7),
            (101, 2, 2, 5, 10),
            (102, 0, 0, 42, 99),
            (103, 1, 1, 999_999, 888_888),
            (104, 2, 2, 5, 0),
        ]
    );
}

#[test]
fn created_collection_uses_anki_dty_column() {
    let fixture = Fixture::new();
    let package = fixture.package();
    let (collection, _, _) = fixture.import(&package, "created");

    let connection = Connection::open(collection).unwrap();
    let columns =
        strings(&connection, "SELECT name FROM pragma_table_info('col')");
    assert!(columns.iter().any(|name| name == "dty"));
    assert!(!columns.iter().any(|name| name == "dt"));
}

#[test]
fn export_uses_expected_zip_entries() {
    let fixture = Fixture::new();
    fs::write(fixture.media.join("photo.png"), b"image").unwrap();
    let package = fixture.package();

    let mut archive =
        zip::ZipArchive::new(File::open(package).unwrap()).unwrap();
    let names: BTreeSet<String> = archive
        .file_names()
        .map(|name| name.unwrap().into_owned())
        .collect();
    for expected in [
        "collection.anki21",
        "collection.anki2",
        "media",
        "0",
        "meta",
    ] {
        assert!(names.contains(expected), "missing entry {expected}");
    }

    assert_eq!(entry_bytes(&mut archive, "meta"), vec![0x08, 0x02]);
    assert!(archive.by_name("collection.anki21").is_ok());
    assert!(archive.by_name("collection.anki2").is_ok());
    assert!(!names.contains("collection.anki21b"));
    for name in ["meta", "collection.anki2", "media", "0"] {
        let entry = archive.by_name(name).unwrap();
        assert_eq!(
            entry.compression(),
            zip::CompressionMethod::Stored,
            "{name} must be stored"
        );
    }
    assert_eq!(
        archive.by_name("collection.anki21").unwrap().compression(),
        zip::CompressionMethod::Deflated
    );
    let mapping: BTreeMap<String, String> =
        serde_json::from_slice(&entry_bytes(&mut archive, "media")).unwrap();
    assert_eq!(mapping.get("0").map(String::as_str), Some("photo.png"));
    assert_eq!(entry_bytes(&mut archive, "0"), b"image".to_vec());
}

#[test]
fn malformed_zip_has_clear_error() {
    let fixture = Fixture::new();
    let package = fixture.dir.path().join("broken.apkg");
    fs::write(&package, b"not a zip").unwrap();

    let error = import_apkg(
        &package,
        &fixture.dir.path().join("broken.anki2"),
        &fixture.dir.path().join("broken-media"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("malformed apkg package"));
}

#[test]
fn missing_media_mapping_is_tolerated() {
    let fixture = Fixture::new();
    let package = fixture.dir.path().join("no-mapping.apkg");
    build_zip(
        &package,
        &[
            ("collection.anki21", fs::read(&fixture.source).unwrap()),
            ("0", b"image".to_vec()),
            ("meta", vec![0x08, 0x02]),
        ],
    );

    let media = fixture.dir.path().join("no-mapping-media");
    let report = import_apkg(
        &package,
        &fixture.dir.path().join("no-mapping.anki2"),
        &media,
    )
    .unwrap();
    assert_eq!(report.media, 0);
    assert_eq!(fs::read_dir(&media).unwrap().count(), 0);
}

#[test]
fn missing_media_entry_has_clear_error() {
    let fixture = Fixture::new();
    let package = fixture.dir.path().join("missing-media.apkg");
    let mapping = json!({"0": "photo.png"}).to_string().into_bytes();
    build_zip(
        &package,
        &[
            ("collection.anki21", fs::read(&fixture.source).unwrap()),
            ("media", mapping),
        ],
    );

    let error = import_apkg(
        &package,
        &fixture.dir.path().join("missing.anki2"),
        &fixture.dir.path().join("missing-media"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("missing from package"));
}

#[test]
fn duplicate_guid_in_package_skips_the_second_note() {
    let fixture = Fixture::new();
    let mut seed = fixture.seed.clone();
    seed.notes[1].guid = "guid-a".to_owned();
    write_collection(&fixture.source, &seed);
    let package = fixture.package();

    import_apkg(
        &package,
        &fixture.dir.path().join("duplicate.anki2"),
        &fixture.dir.path().join("duplicate-media"),
    )
    .expect("duplicate guids are skipped like Anki does");
}

#[test]
fn importing_twice_skips_existing_notes() {
    let fixture = Fixture::new();
    let package = fixture.package();
    let (collection, media, first) = fixture.import(&package, "twice");
    assert_eq!(first.notes, 2);

    let second = import_apkg(&package, &collection, &media).unwrap();
    assert_eq!(second.notes, 0);
    assert_eq!(second.cards, 0);
    let connection = Connection::open(collection).unwrap();
    assert_eq!(count(&connection, "notes"), 2);
    assert_eq!(count(&connection, "cards"), 2);
}

#[test]
fn media_names_are_sanitized_and_rewritten() {
    let fixture = Fixture::new();
    fs::write(fixture.media.join("bad:name?.png"), b"pixel").unwrap();
    let mut seed = fixture.seed.clone();
    seed.notes[0].fields =
        "<img src=\"bad:name?.png\"> front\u{001f}back".to_owned();
    write_collection(&fixture.source, &seed);
    let package = fixture.package();

    let (collection, media, report) = fixture.import(&package, "sanitized");
    assert_eq!(report.media, 1);
    assert!(media.join("badname.png").exists());
    let connection = Connection::open(collection).unwrap();
    let fields = query_string(
        &connection,
        "SELECT flds FROM notes WHERE guid = ?",
        "guid-a",
    );
    assert!(fields.contains("badname.png"));
    assert!(!fields.contains("bad:name?.png"));
}

#[test]
fn media_collision_uses_new_name_and_rewrites_notes() {
    let fixture = Fixture::new();
    fs::write(fixture.media.join("photo.png"), b"new").unwrap();
    let package = fixture.package();

    let collection = fixture.dir.path().join("collision.anki2");
    let media = fixture.dir.path().join("collision-media");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("photo.png"), b"old").unwrap();
    import_apkg(&package, &collection, &media).unwrap();

    assert_eq!(fs::read(media.join("photo.png")).unwrap(), b"old".to_vec());
    let hashed = media
        .read_dir()
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|name| {
            name.starts_with("photo-")
                && Path::new(name).extension().and_then(|ext| ext.to_str())
                    == Some("png")
        })
        .expect("conflicting media gets a hash suffix");
    assert_eq!(fs::read(media.join(&hashed)).unwrap(), b"new".to_vec());
    let connection = Connection::open(collection).unwrap();
    let fields = query_string(
        &connection,
        "SELECT flds FROM notes WHERE guid = ?",
        "guid-a",
    );
    assert!(fields.contains(&hashed));
}

#[test]
fn reimport_with_conflicting_media_is_idempotent() {
    let fixture = Fixture::new();
    fs::write(fixture.media.join("photo.png"), b"new").unwrap();
    let package = fixture.package();
    let collection = fixture.dir.path().join("idempotent.anki2");
    let media = fixture.dir.path().join("idempotent-media");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("photo.png"), b"old").unwrap();

    let first = import_apkg(&package, &collection, &media).unwrap();
    assert_eq!(first.notes, 2);
    let before = {
        let connection = Connection::open(&collection).unwrap();
        query_string(
            &connection,
            "SELECT flds FROM notes WHERE guid = ?",
            "guid-a",
        )
    };
    let hashed = media
        .read_dir()
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|name| {
            name.starts_with("photo-")
                && Path::new(name).extension().and_then(|ext| ext.to_str())
                    == Some("png")
        })
        .expect("conflicting media gets a hash suffix");
    assert!(before.contains(&hashed));

    let second = import_apkg(&package, &collection, &media).unwrap();
    assert_eq!(second.notes, 0);
    let connection = Connection::open(&collection).unwrap();
    assert_eq!(count(&connection, "notes"), 2);
    assert_eq!(
        query_string(
            &connection,
            "SELECT flds FROM notes WHERE guid = ?",
            "guid-a",
        ),
        before
    );
    assert_eq!(fs::read_dir(&media).unwrap().count(), 2);
}

fn write_stress_package(path: &Path, collection_bytes: &[u8], count: usize) {
    let mapping: BTreeMap<String, String> = (0..count)
        .map(|index| (index.to_string(), format!("m{index:06}.png")))
        .collect();
    let file = File::create(path).unwrap();
    let mut writer = ZipWriter::new(file);
    writer
        .start_file("collection.anki21", SimpleFileOptions::default())
        .unwrap();
    writer.write_all(collection_bytes).unwrap();
    writer
        .start_file("media", SimpleFileOptions::default())
        .unwrap();
    writer
        .write_all(&serde_json::to_vec(&mapping).unwrap())
        .unwrap();
    for index in 0..count {
        writer
            .start_file(index.to_string(), SimpleFileOptions::default())
            .unwrap();
        writer.write_all(format!("new-{index}").as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}
#[expect(clippy::format_collect, reason = "test helper builds a hex string")]
fn hashed_media_name(name: &str, data: &[u8]) -> String {
    let digest = sha1::Sha1::digest(data);
    let hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    if extension.is_empty() {
        format!("{stem}-{hash}")
    } else {
        format!("{stem}-{hash}.{extension}")
    }
}

fn seed_conflicting_media(directory: &Path, count: usize) -> PathBuf {
    let media = directory.join("stress-media");
    fs::create_dir_all(&media).unwrap();
    for index in 0..count {
        fs::write(
            media.join(format!("m{index:06}.png")),
            format!("old-{index}"),
        )
        .unwrap();
    }
    media
}

#[test]
#[expect(clippy::integer_division, reason = "sample the midpoint index")]
fn stress_import_with_many_conflicting_media() {
    const COUNT: usize = 256;
    let fixture = Fixture::new();
    let notes: Vec<TestNote> = (0..COUNT)
        .map(|index| TestNote {
            id: i64::try_from(index + 1).unwrap(),
            guid: format!("stress-{index}"),
            mid: 1,
            fields: format!(
                "<img src=\"m{index:06}.png\"> front\u{001f}back m{index:06}.png"
            ),
        })
        .collect();
    write_collection(
        &fixture.source,
        &Seed {
            notes,
            cards: Vec::new(),
            revlog: false,
        },
    );
    let collection_bytes = fs::read(&fixture.source).unwrap();
    let package = fixture.dir.path().join("stress.apkg");
    write_stress_package(&package, &collection_bytes, COUNT);

    let media = seed_conflicting_media(fixture.dir.path(), COUNT);

    let collection = fixture.dir.path().join("stress.anki2");
    let started = Instant::now();
    let report = import_apkg(&package, &collection, &media).unwrap();
    eprintln!("stress import took {:?}", started.elapsed());
    assert_eq!(report.notes, COUNT);
    assert_eq!(report.media, COUNT);
    assert_eq!(fs::read_dir(&media).unwrap().count(), COUNT * 2);
    for index in [0, COUNT / 2, COUNT - 1] {
        let name = format!("m{index:06}.png");
        assert_eq!(
            fs::read(media.join(&name)).unwrap(),
            format!("old-{index}").into_bytes()
        );
        let payload = format!("new-{index}").into_bytes();
        assert_eq!(
            fs::read(media.join(hashed_media_name(&name, &payload))).unwrap(),
            payload
        );
    }
    let connection = Connection::open(&collection).unwrap();
    let fields = query_string(
        &connection,
        "SELECT flds FROM notes WHERE guid = ?",
        "stress-0",
    );
    assert!(fields.contains(&hashed_media_name("m000000.png", b"new-0")));
    assert!(!fields.contains("src=\"m000000.png\""));

    let second = import_apkg(&package, &collection, &media).unwrap();
    assert_eq!(second.notes, 0);
    assert_eq!(fs::read_dir(&media).unwrap().count(), COUNT * 2);
    let connection = Connection::open(&collection).unwrap();
    assert_eq!(
        query_string(
            &connection,
            "SELECT flds FROM notes WHERE guid = ?",
            "stress-0",
        ),
        fields
    );
}

#[test]
fn id_collisions_do_not_lose_existing_notes() {
    let fixture = Fixture::new();
    let destination = fixture.dir.path().join("existing.anki2");
    write_collection(
        &destination,
        &Seed {
            notes: vec![TestNote {
                id: 1,
                guid: "existing".to_owned(),
                mid: 1,
                fields: "keep me\u{001f}here".to_owned(),
            }],
            cards: vec![TestCard {
                id: 100,
                note_id: 1,
                deck_id: 1,
                ordinal: 0,
            }],
            revlog: false,
        },
    );
    let package = fixture.package();

    let report = import_apkg(
        &package,
        &destination,
        &fixture.dir.path().join("merged-media"),
    )
    .unwrap();
    assert_eq!(report.notes, 2);
    let connection = Connection::open(destination).unwrap();
    assert_eq!(count(&connection, "notes"), 3);
    assert_eq!(count(&connection, "cards"), 3);
    assert_eq!(
        query_id(
            &connection,
            "SELECT id FROM notes WHERE guid = ?",
            "existing"
        ),
        1
    );
    let imported_id =
        query_id(&connection, "SELECT id FROM notes WHERE guid = ?", "guid-a");
    assert_ne!(imported_id, 1);
    assert_eq!(
        query_id(
            &connection,
            "SELECT COUNT(*) FROM cards WHERE nid = ?",
            &imported_id.to_string()
        ),
        1
    );
    assert_eq!(
        strings(
            &connection,
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
        )
        .len(),
        5
    );
    assert!(fixture.source.exists());
}

#[test]
fn colpkg_extracts_without_overwriting() {
    let fixture = Fixture::new();
    let package = fixture.dir.path().join("backup.colpkg");
    let meta = vec![0x08, 0x01];
    build_zip(
        &package,
        &[
            ("collection.anki2", fs::read(&fixture.source).unwrap()),
            ("media/photo.png", b"image".to_vec()),
            ("meta", meta),
        ],
    );

    let destination = fixture.dir.path().join("colpkg");
    let collection = import_colpkg(&package, &destination).unwrap();
    assert_eq!(collection, destination.join("collection.anki2"));
    assert!(collection.exists());
    assert_eq!(
        fs::read(destination.join("media/photo.png")).unwrap(),
        b"image".to_vec()
    );

    fs::write(destination.join("media/photo.png"), b"changed").unwrap();
    let error = import_colpkg(&package, &destination).unwrap_err();
    assert!(error.to_string().contains("already exists"));
    assert_eq!(
        fs::read(destination.join("media/photo.png")).unwrap(),
        b"changed".to_vec()
    );
}

#[test]
fn colpkg_extracts_numbered_media_mapping() {
    let fixture = Fixture::new();
    let package = fixture.dir.path().join("mapped.colpkg");
    let mapping = json!({"0": "photo.png", "1": "../bad:name?.png"})
        .to_string()
        .into_bytes();
    build_zip(
        &package,
        &[
            ("collection.anki21", fs::read(&fixture.source).unwrap()),
            ("0", b"image".to_vec()),
            ("1", b"pixel".to_vec()),
            ("media", mapping),
            ("meta", vec![0x08, 0x02]),
        ],
    );

    let destination = fixture.dir.path().join("mapped");
    let collection = import_colpkg(&package, &destination).unwrap();
    assert_eq!(collection, destination.join("collection.anki21"));
    assert_eq!(
        fs::read(destination.join("media/photo.png")).unwrap(),
        b"image".to_vec()
    );
    assert_eq!(
        fs::read(destination.join("media/badname.png")).unwrap(),
        b"pixel".to_vec()
    );
}

#[test]
fn colpkg_roundtrip_preserves_notes_cards_and_media() {
    let fixture = Fixture::new();
    fs::write(fixture.media.join("photo.png"), b"image").unwrap();
    let package = fixture.dir.path().join("backup.colpkg");
    export_colpkg(&fixture.source, &fixture.media, &package).unwrap();

    let destination = fixture.dir.path().join("restored");
    let collection = import_colpkg(&package, &destination).unwrap();
    let connection = Connection::open(collection).unwrap();
    assert_eq!(count(&connection, "notes"), 2);
    assert_eq!(count(&connection, "cards"), 2);
    assert_eq!(count(&connection, "revlog"), 1);
    let guids = strings(&connection, "SELECT guid FROM notes ORDER BY guid");
    assert_eq!(guids, vec!["guid-a", "guid-b"]);
    assert_eq!(
        fs::read(destination.join("media/photo.png")).unwrap(),
        b"image".to_vec()
    );
}

#[test]
fn invalid_media_mapping_has_clear_error() {
    let fixture = Fixture::new();
    let package = fixture.dir.path().join("bad-mapping.apkg");
    build_zip(
        &package,
        &[
            ("collection.anki21", fs::read(&fixture.source).unwrap()),
            ("media", b"not json".to_vec()),
        ],
    );

    let error = import_apkg(
        &package,
        &fixture.dir.path().join("bad-mapping.anki2"),
        &fixture.dir.path().join("bad-mapping-media"),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid media mapping in package")
    );
}

#[test]
fn real_world_apkg_roundtrip_when_available() {
    let Some(fixture) = real_fixture() else {
        eprintln!("skipping real-world apkg test: RANKI_TEST_APKG is absent");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("WorldFlags.apkg");
    fs::copy(&fixture, &package).unwrap();
    let collection = dir.path().join("flags.anki2");
    let media = dir.path().join("flags-media");
    let report = import_apkg(&package, &collection, &media).unwrap();

    assert!(report.notes > 200);
    assert!(report.cards > 200);
    assert!(fs::read_dir(&media).unwrap().count() > 0);

    let exported = dir.path().join("roundtrip.apkg");
    export_apkg(&collection, &media, &exported).unwrap();
    let mut archive =
        zip::ZipArchive::new(File::open(&exported).unwrap()).unwrap();
    assert!(archive.by_name("collection.anki21").is_ok());

    let fresh = dir.path().join("fresh.anki2");
    let fresh_media = dir.path().join("fresh-media");
    let second = import_apkg(&exported, &fresh, &fresh_media).unwrap();
    assert_eq!(second.notes, report.notes);
    assert_eq!(second.cards, report.cards);
}

const PACKAGE_MODEL_ID: i64 = 500;
const PACKAGE_DECK_ID: i64 = 700;

fn merge_package(directory: &Path, meta: &SourceMeta, seed: &Seed) -> PathBuf {
    let collection = directory.join("merge-source.anki2");
    write_collection_with(&collection, seed, meta);
    let package = directory.join("merge.apkg");
    build_zip(
        &package,
        &[("collection.anki2", fs::read(&collection).unwrap())],
    );
    package
}

fn model_json(id: i64, name: &str, fields: &[&str]) -> Value {
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
        "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}", "afmt": "{{FrontSide}}"}],
        "css": "",
        "latexPre": "",
        "latexPost": "",
        "req": []
    })
}

fn deck_json(id: i64, name: &str) -> Value {
    json!({"id": id, "name": name, "conf": 1, "desc": "", "dyn": 0, "usn": 0})
}

fn entry_map(entries: &[Value]) -> Value {
    let mut map = serde_json::Map::new();
    for entry in entries {
        let id = entry.get("id").and_then(Value::as_i64).unwrap();
        let _ = map.insert(id.to_string(), entry.clone());
    }
    Value::Object(map)
}

fn col_column(connection: &Connection, column: &str) -> Value {
    let sql = format!("SELECT {column} FROM col WHERE id = 1");
    let text: String =
        connection.query_row(&sql, [], |row| row.get(0)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn entry_names(value: &Value) -> Vec<String> {
    let mut names = value
        .as_object()
        .unwrap()
        .values()
        .map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn entry_ids(value: &Value, name: &str) -> Vec<i64> {
    let mut ids = value
        .as_object()
        .unwrap()
        .values()
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        .map(|entry| entry.get("id").and_then(Value::as_i64).unwrap())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn model_fields(value: &Value, id: i64) -> Vec<String> {
    value
        .as_object()
        .unwrap()
        .get(&id.to_string())
        .unwrap()
        .get("flds")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|field| {
            field
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

fn one_note(guid: &str, mid: i64, deck_id: i64) -> Seed {
    Seed {
        notes: vec![TestNote {
            id: 10,
            guid: guid.to_owned(),
            mid,
            fields: "front\u{001f}back".to_owned(),
        }],
        cards: vec![TestCard {
            id: 20,
            note_id: 10,
            deck_id,
            ordinal: 0,
        }],
        revlog: false,
    }
}

fn import_into(
    directory: &Path,
    package: &Path,
    collection: &Path,
) -> Connection {
    import_apkg(package, collection, &directory.join("import-media")).unwrap();
    let connection = Connection::open(collection).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    connection
}

fn package_meta(fields: &[&str], extra_decks: &[Value]) -> SourceMeta {
    let mut decks = vec![deck_json(PACKAGE_DECK_ID, "Default")];
    decks.extend_from_slice(extra_decks);
    SourceMeta {
        models: entry_map(&[model_json(PACKAGE_MODEL_ID, "Basic", fields)]),
        decks: entry_map(&decks),
    }
}

#[test]
fn fresh_import_merges_named_deck_and_model() {
    let dir = tempfile::tempdir().unwrap();
    let meta = package_meta(&["Front", "Back"], &[]);
    let package = merge_package(
        dir.path(),
        &meta,
        &one_note("flags", PACKAGE_MODEL_ID, PACKAGE_DECK_ID),
    );

    let collection = dir.path().join("fresh.anki2");
    let connection = import_into(dir.path(), &package, &collection);

    let decks = col_column(&connection, "decks");
    let models = col_column(&connection, "models");
    assert_eq!(entry_names(&decks), ["Default".to_owned()]);
    assert_eq!(entry_ids(&decks, "Default"), [1]);
    assert_eq!(entry_names(&models), ["Basic".to_owned()]);
    assert_eq!(entry_ids(&models, "Basic"), [1]);
    assert_eq!(
        query_id(&connection, "SELECT mid FROM notes WHERE guid = ?", "flags"),
        1
    );
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "10"),
        1
    );
}

#[test]
fn existing_import_reuses_same_named_deck_and_model() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("existing.anki2");
    write_collection_with(
        &destination,
        &Seed {
            notes: vec![TestNote {
                id: 1,
                guid: "existing".to_owned(),
                mid: 11,
                fields: "keep\u{001f}me".to_owned(),
            }],
            cards: vec![TestCard {
                id: 9,
                note_id: 1,
                deck_id: 12,
                ordinal: 0,
            }],
            revlog: false,
        },
        &SourceMeta {
            models: entry_map(&[model_json(11, "basic", &["Front", "Back"])]),
            decks: entry_map(&[deck_json(12, "default")]),
        },
    );
    let meta = SourceMeta {
        models: entry_map(&[model_json(
            PACKAGE_MODEL_ID,
            "Basic",
            &["Front", "Back"],
        )]),
        decks: entry_map(&[deck_json(PACKAGE_DECK_ID, "Default")]),
    };
    let package = merge_package(
        dir.path(),
        &meta,
        &one_note("flags", PACKAGE_MODEL_ID, PACKAGE_DECK_ID),
    );

    let connection = import_into(dir.path(), &package, &destination);

    let decks = col_column(&connection, "decks");
    let models = col_column(&connection, "models");
    assert_eq!(entry_names(&decks), ["default".to_owned()]);
    assert_eq!(entry_ids(&decks, "default"), [12]);
    assert_eq!(entry_names(&models), ["basic".to_owned()]);
    assert_eq!(entry_ids(&models, "basic"), [11]);
    assert_eq!(
        query_id(&connection, "SELECT mid FROM notes WHERE guid = ?", "flags"),
        11
    );
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "10"),
        12
    );
    assert_eq!(count(&connection, "notes"), 2);
}

#[test]
fn subdeck_merge_keeps_hierarchy() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("existing.anki2");
    write_collection_with(
        &destination,
        &Seed {
            notes: vec![],
            cards: vec![],
            revlog: false,
        },
        &SourceMeta {
            models: entry_map(&[model_json(11, "Basic", &["Front", "Back"])]),
            decks: entry_map(&[deck_json(12, "Parent")]),
        },
    );
    let meta = SourceMeta {
        models: entry_map(&[model_json(
            PACKAGE_MODEL_ID,
            "Basic",
            &["Front", "Back"],
        )]),
        decks: entry_map(&[
            deck_json(PACKAGE_DECK_ID, "Default"),
            deck_json(701, "Parent"),
            deck_json(702, "Parent::Child"),
        ]),
    };
    let seed = Seed {
        notes: vec![
            TestNote {
                id: 10,
                guid: "parent".to_owned(),
                mid: PACKAGE_MODEL_ID,
                fields: "a\u{001f}b".to_owned(),
            },
            TestNote {
                id: 11,
                guid: "child".to_owned(),
                mid: PACKAGE_MODEL_ID,
                fields: "c\u{001f}d".to_owned(),
            },
        ],
        cards: vec![
            TestCard {
                id: 20,
                note_id: 10,
                deck_id: 701,
                ordinal: 0,
            },
            TestCard {
                id: 21,
                note_id: 11,
                deck_id: 702,
                ordinal: 0,
            },
        ],
        revlog: false,
    };
    let package = merge_package(dir.path(), &meta, &seed);

    let connection = import_into(dir.path(), &package, &destination);

    let decks = col_column(&connection, "decks");
    assert_eq!(
        entry_names(&decks),
        [
            "Default".to_owned(),
            "Parent".to_owned(),
            "Parent::Child".to_owned()
        ]
    );
    assert_eq!(entry_ids(&decks, "Parent"), [12]);
    let child_id = entry_ids(&decks, "Parent::Child").first().copied().unwrap();
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "10"),
        12
    );
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "11"),
        child_id
    );
}

#[test]
fn reimport_keeps_deck_and_model_counts() {
    let dir = tempfile::tempdir().unwrap();
    let meta =
        package_meta(&["Front", "Back"], &[deck_json(701, "Parent::Child")]);
    let package = merge_package(
        dir.path(),
        &meta,
        &one_note("flags", PACKAGE_MODEL_ID, PACKAGE_DECK_ID),
    );
    let collection = dir.path().join("fresh.anki2");

    let first = import_into(dir.path(), &package, &collection);
    let first_decks = entry_ids(&col_column(&first, "decks"), "Default");
    let first_models = entry_ids(&col_column(&first, "models"), "Basic");
    drop(first);

    let second = import_into(dir.path(), &package, &collection);
    assert_eq!(
        entry_ids(&col_column(&second, "decks"), "Default"),
        first_decks
    );
    assert_eq!(
        entry_ids(&col_column(&second, "models"), "Basic"),
        first_models
    );
    assert_eq!(count(&second, "notes"), 1);
}

#[test]
fn incompatible_same_named_model_is_kept_separate() {
    let dir = tempfile::tempdir().unwrap();
    let meta = package_meta(&["Front", "Back", "Extra"], &[]);
    let package = merge_package(
        dir.path(),
        &meta,
        &one_note("flags", PACKAGE_MODEL_ID, PACKAGE_DECK_ID),
    );

    let collection = dir.path().join("fresh.anki2");
    let connection = import_into(dir.path(), &package, &collection);

    let models = col_column(&connection, "models");
    assert_eq!(entry_ids(&models, "Basic"), [1, PACKAGE_MODEL_ID]);
    assert_eq!(
        model_fields(&models, 1),
        ["Front".to_owned(), "Back".to_owned()]
    );
    assert_eq!(
        model_fields(&models, PACKAGE_MODEL_ID),
        ["Front".to_owned(), "Back".to_owned(), "Extra".to_owned()]
    );
    assert_eq!(
        query_id(&connection, "SELECT mid FROM notes WHERE guid = ?", "flags"),
        PACKAGE_MODEL_ID
    );
}

#[test]
fn imported_original_deck_points_at_merged_deck() {
    let dir = tempfile::tempdir().unwrap();
    let meta = package_meta(&["Front", "Back"], &[deck_json(701, "Later")]);
    let source = dir.path().join("merge-source.anki2");
    write_collection_with(
        &source,
        &one_note("flags", PACKAGE_MODEL_ID, 701),
        &meta,
    );
    {
        let connection = Connection::open(&source).unwrap();
        connection
            .execute(
                "UPDATE cards SET odid = ?1, odue = 7 WHERE id = 20",
                [PACKAGE_DECK_ID],
            )
            .unwrap();
    }
    let package = dir.path().join("merge.apkg");
    build_zip(
        &package,
        &[("collection.anki2", fs::read(&source).unwrap())],
    );

    let collection = dir.path().join("fresh.anki2");
    let connection = import_into(dir.path(), &package, &collection);

    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "10"),
        701
    );
    assert_eq!(
        query_id(&connection, "SELECT odid FROM cards WHERE nid = ?", "10"),
        1
    );
}

#[test]
fn real_world_apkg_import_keeps_single_default_deck() {
    let Some(fixture) = real_fixture() else {
        eprintln!("skipping real-world apkg test: RANKI_TEST_APKG is absent");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("WorldFlags.apkg");
    fs::copy(&fixture, &package).unwrap();
    let collection = dir.path().join("flags.anki2");
    let connection = import_into(dir.path(), &package, &collection);

    let decks = col_column(&connection, "decks");
    assert_eq!(entry_ids(&decks, "Default"), [1]);
    assert_eq!(
        entry_names(&decks),
        ["Default".to_owned(), "WorldFlags".to_owned()]
    );
}

const SCHEMA18_TABLES: &str = "
CREATE TABLE IF NOT EXISTS col (
    id INTEGER PRIMARY KEY, crt INTEGER NOT NULL, mod INTEGER NOT NULL,
    scm INTEGER NOT NULL, ver INTEGER NOT NULL, dty INTEGER NOT NULL,
    usn INTEGER NOT NULL, ls INTEGER NOT NULL, conf TEXT NOT NULL,
    models TEXT NOT NULL, decks TEXT NOT NULL, dconf TEXT NOT NULL,
    tags TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS notes (
    id INTEGER PRIMARY KEY, guid TEXT NOT NULL, mid INTEGER NOT NULL,
    mod INTEGER NOT NULL, usn INTEGER NOT NULL, tags TEXT NOT NULL,
    flds TEXT NOT NULL, sfld TEXT NOT NULL, csum INTEGER NOT NULL,
    flags INTEGER NOT NULL, data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS cards (
    id INTEGER PRIMARY KEY, nid INTEGER NOT NULL, did INTEGER NOT NULL,
    ord INTEGER NOT NULL, mod INTEGER NOT NULL, usn INTEGER NOT NULL,
    type INTEGER NOT NULL, queue INTEGER NOT NULL, due INTEGER NOT NULL,
    ivl INTEGER NOT NULL, factor INTEGER NOT NULL, reps INTEGER NOT NULL,
    lapses INTEGER NOT NULL, left INTEGER NOT NULL, odue INTEGER NOT NULL,
    odid INTEGER NOT NULL, flags INTEGER NOT NULL, data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS revlog (
    id INTEGER PRIMARY KEY, cid INTEGER NOT NULL, usn INTEGER NOT NULL,
    ease INTEGER NOT NULL, ivl INTEGER NOT NULL, lastIvl INTEGER NOT NULL,
    factor INTEGER NOT NULL, time INTEGER NOT NULL, type INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS graves (
    oid INTEGER NOT NULL, type INTEGER NOT NULL, usn INTEGER NOT NULL,
    PRIMARY KEY (oid, type)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS decks (
    id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL COLLATE unicase,
    mtime_secs INTEGER NOT NULL, usn INTEGER NOT NULL,
    common BLOB NOT NULL, kind BLOB NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_decks_name ON decks (name);
CREATE TABLE IF NOT EXISTS deck_config (
    id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL COLLATE unicase,
    mtime_secs INTEGER NOT NULL, usn INTEGER NOT NULL, config BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS tags (
    tag TEXT PRIMARY KEY COLLATE unicase, usn INTEGER NOT NULL, collapsed INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS notetypes (
    id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL COLLATE unicase,
    mtime_secs INTEGER NOT NULL, usn INTEGER NOT NULL, config BLOB NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_notetypes_name ON notetypes (name);
CREATE TABLE IF NOT EXISTS fields (
    ntid INTEGER NOT NULL, ord INTEGER NOT NULL,
    name TEXT NOT NULL COLLATE unicase, config BLOB NOT NULL,
    PRIMARY KEY (ntid, ord)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS templates (
    ntid INTEGER NOT NULL, ord INTEGER NOT NULL,
    name TEXT NOT NULL COLLATE unicase, mtime_secs INTEGER NOT NULL,
    usn INTEGER NOT NULL, config BLOB NOT NULL,
    PRIMARY KEY (ntid, ord)
) WITHOUT ROWID;
";

struct Schema18Notetype<'a> {
    id: i64,
    name: &'a str,
    fields: &'a [&'a str],
}

#[expect(
    clippy::too_many_arguments,
    reason = "fixture rows mirror the normalized Anki tables"
)]
fn write_schema18_collection(
    path: &Path,
    decks: &[(i64, &str)],
    notetypes: &[Schema18Notetype<'_>],
    seed: &Seed,
) {
    let _ = fs::remove_file(path);
    let connection = Connection::open(path).unwrap();
    ranki_storage::register_collations(&connection).unwrap();
    connection.execute_batch(SCHEMA18_TABLES).unwrap();
    connection
        .execute(
            "INSERT INTO col (id, crt, mod, scm, ver, dty, usn, ls, conf, models, decks, dconf, tags)
             VALUES (1, 0, 0, 0, 18, 0, 0, 0, '{}', '', '', '', '{}')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO deck_config (id, name, mtime_secs, usn, config)
             VALUES (1, 'Default', 0, 0, x''), (2, 'Hard', 0, 0, x'')",
            [],
        )
        .unwrap();
    for (id, name) in decks {
        connection
            .execute(
                "INSERT INTO decks (id, name, mtime_secs, usn, common, kind)
                 VALUES (?1, ?2, 0, 0, x'', x'')",
                params![id, name],
            )
            .unwrap();
    }
    for notetype in notetypes {
        connection
            .execute(
                "INSERT INTO notetypes (id, name, mtime_secs, usn, config)
                 VALUES (?1, ?2, 0, 0, x'')",
                params![notetype.id, notetype.name],
            )
            .unwrap();
        for (ord, field) in notetype.fields.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO fields (ntid, ord, name, config)
                     VALUES (?1, ?2, ?3, x'')",
                    params![notetype.id, i64::try_from(ord).unwrap(), field],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO templates (ntid, ord, name, mtime_secs, usn, config)
                 VALUES (?1, 0, 'Card 1', 0, 0, x'')",
                params![notetype.id],
            )
            .unwrap();
    }
    for note in &seed.notes {
        let sort_field = note.fields.split('\u{001f}').next().unwrap_or("");
        connection
            .execute(
                "INSERT INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data)
                 VALUES (?, ?, ?, 0, 0, '', ?, ?, 0, 0, '')",
                params![note.id, note.guid, note.mid, note.fields, sort_field],
            )
            .unwrap();
    }
    for card in &seed.cards {
        connection
            .execute(
                "INSERT INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor,
                     reps, lapses, left, odue, odid, flags, data)
                 VALUES (?, ?, ?, ?, 0, 0, 0, 0, 1, 0, 2500, 0, 0, 0, 0, 0, 0, '')",
                params![card.id, card.note_id, card.deck_id, card.ordinal],
            )
            .unwrap();
    }
}

fn schema18_package(directory: &Path, source: &Path) -> PathBuf {
    let package = directory.join("schema18.apkg");
    build_zip(
        &package,
        &[("collection.anki21", fs::read(source).unwrap())],
    );
    package
}

fn schema18_seed(mid: i64, deck_id: i64) -> Seed {
    Seed {
        notes: vec![TestNote {
            id: 10,
            guid: "schema18".to_owned(),
            mid,
            fields: "front\u{001f}back".to_owned(),
        }],
        cards: vec![TestCard {
            id: 20,
            note_id: 10,
            deck_id,
            ordinal: 0,
        }],
        revlog: false,
    }
}

#[test]
fn schema18_source_merges_deck_and_note_type_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("schema18-source.anki2");
    write_schema18_collection(
        &source,
        &[(1, "Default"), (700, "Flags")],
        &[Schema18Notetype {
            id: 77,
            name: "Basic",
            fields: &["Front", "Back"],
        }],
        &schema18_seed(77, 700),
    );
    let package = schema18_package(dir.path(), &source);

    let collection = dir.path().join("fresh.anki2");
    let connection = import_into(dir.path(), &package, &collection);

    let decks = col_column(&connection, "decks");
    assert_eq!(
        entry_names(&decks),
        ["Default".to_owned(), "Flags".to_owned()]
    );
    assert_eq!(entry_ids(&decks, "Default"), [1]);
    let models = col_column(&connection, "models");
    assert_eq!(entry_ids(&models, "Basic"), [1]);
    assert_eq!(
        query_id(
            &connection,
            "SELECT mid FROM notes WHERE guid = ?",
            "schema18"
        ),
        1
    );
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "10"),
        700
    );
}

#[test]
fn schema18_source_reimport_keeps_deck_and_model_counts() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("schema18-source.anki2");
    write_schema18_collection(
        &source,
        &[(1, "Default"), (700, "Flags")],
        &[Schema18Notetype {
            id: 77,
            name: "Basic",
            fields: &["Front", "Back"],
        }],
        &schema18_seed(77, 700),
    );
    let package = schema18_package(dir.path(), &source);
    let collection = dir.path().join("fresh.anki2");

    let first = import_into(dir.path(), &package, &collection);
    let first_decks = col_column(&first, "decks");
    let first_models = col_column(&first, "models");
    drop(first);

    let second = import_into(dir.path(), &package, &collection);
    let second_decks = col_column(&second, "decks");
    let second_models = col_column(&second, "models");
    assert_eq!(
        second_decks.as_object().unwrap().len(),
        first_decks.as_object().unwrap().len()
    );
    assert_eq!(
        second_models.as_object().unwrap().len(),
        first_models.as_object().unwrap().len()
    );
    assert_eq!(entry_ids(&second_decks, "Default"), [1]);
    assert_eq!(entry_ids(&second_models, "Basic"), [1]);
    assert_eq!(count(&second, "notes"), 1);
    assert_eq!(count(&second, "cards"), 1);
}

#[test]
fn schema18_destination_copies_new_deck_and_note_type_rows() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("modern.anki2");
    write_schema18_collection(
        &destination,
        &[(1, "Default")],
        &[],
        &Seed {
            notes: vec![],
            cards: vec![],
            revlog: false,
        },
    );
    let source = dir.path().join("schema18-source.anki2");
    write_schema18_collection(
        &source,
        &[(1, "Default"), (700, "Flags")],
        &[Schema18Notetype {
            id: 77,
            name: "Extra",
            fields: &["Front", "Back"],
        }],
        &schema18_seed(77, 700),
    );
    let package = schema18_package(dir.path(), &source);

    let connection = import_into(dir.path(), &package, &destination);

    assert_eq!(
        strings(&connection, "SELECT name FROM decks ORDER BY id"),
        ["Default".to_owned(), "Flags".to_owned()]
    );
    assert_eq!(
        strings(&connection, "SELECT name FROM notetypes ORDER BY id"),
        ["Extra".to_owned()]
    );
    assert_eq!(
        strings(
            &connection,
            "SELECT name FROM fields WHERE ntid = 77 ORDER BY ord"
        ),
        ["Front".to_owned(), "Back".to_owned()]
    );
    assert_eq!(
        strings(&connection, "SELECT name FROM deck_config ORDER BY id"),
        ["Default".to_owned(), "Hard".to_owned()]
    );
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "10"),
        700
    );
    assert_eq!(
        query_id(
            &connection,
            "SELECT mid FROM notes WHERE guid = ?",
            "schema18"
        ),
        77
    );
}

#[test]
fn schema18_import_keeps_deck_steps_with_remapped_preset() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("modern.anki2");
    write_schema18_collection(
        &destination,
        &[(1, "Default")],
        &[],
        &Seed {
            notes: vec![],
            cards: vec![],
            revlog: false,
        },
    );
    let mut old_config = schema18_floats(1, &[1.0, 10.0]);
    old_config.extend(schema18_floats(2, &[10.0]));
    let connection = Connection::open(&destination).unwrap();
    connection
        .execute(
            "UPDATE deck_config SET config = ?1 WHERE id = 2",
            [old_config],
        )
        .unwrap();
    drop(connection);
    let source = dir.path().join("schema18-source.anki2");
    write_schema18_collection(
        &source,
        &[(1, "Default"), (700, "Flags")],
        &[],
        &schema18_seed(1, 700),
    );
    let mut config = schema18_floats(1, &[5.0, 20.0]);
    config.extend(schema18_floats(2, &[3.0]));
    let connection = Connection::open(&source).unwrap();
    connection
        .execute(
            "UPDATE decks SET kind = ?1 WHERE id = 700",
            [ranki_storage::normal_deck_kind(2)],
        )
        .unwrap();
    connection
        .execute("UPDATE deck_config SET config = ?1 WHERE id = 2", [config])
        .unwrap();
    drop(connection);
    let package = schema18_package(dir.path(), &source);
    import_into(dir.path(), &package, &destination);

    let store = AnkiCollection::open(&destination).unwrap();
    let deck_id = store
        .decks()
        .unwrap()
        .into_iter()
        .find(|deck| deck.name == "Flags")
        .unwrap()
        .id;
    assert_eq!(
        store.deck_steps(deck_id).unwrap(),
        DeckSteps {
            learning: vec![300, 1_200],
            relearning: vec![180],
        }
    );
}

fn schema18_floats(field: u32, values: &[f32]) -> Vec<u8> {
    let mut bytes = vec![u8::try_from((field << 3) | 2).unwrap()];
    bytes.push(u8::try_from(values.len().checked_mul(4).unwrap()).unwrap());
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "fixture and assertions read better in one test"
)]
fn schema18_destination_keeps_tables_in_sync_with_merged_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("modern.anki2");
    write_schema18_collection(
        &destination,
        &[(1, "Default")],
        &[Schema18Notetype {
            id: 1,
            name: "Basic",
            fields: &["Front", "Back"],
        }],
        &Seed {
            notes: vec![],
            cards: vec![],
            revlog: false,
        },
    );

    let meta = SourceMeta {
        models: entry_map(&[model_json(
            PACKAGE_MODEL_ID,
            "Basic",
            &["Front", "Back"],
        )]),
        decks: entry_map(&[
            deck_json(PACKAGE_DECK_ID, "Default"),
            deck_json(701, "Flags"),
        ]),
    };
    let seed = Seed {
        notes: vec![
            TestNote {
                id: 10,
                guid: "merged".to_owned(),
                mid: PACKAGE_MODEL_ID,
                fields: "front\u{001f}back".to_owned(),
            },
            TestNote {
                id: 11,
                guid: "fresh".to_owned(),
                mid: PACKAGE_MODEL_ID,
                fields: "front\u{001f}back".to_owned(),
            },
        ],
        cards: vec![
            TestCard {
                id: 20,
                note_id: 10,
                deck_id: PACKAGE_DECK_ID,
                ordinal: 0,
            },
            TestCard {
                id: 21,
                note_id: 11,
                deck_id: 701,
                ordinal: 0,
            },
        ],
        revlog: false,
    };
    let package = merge_package(dir.path(), &meta, &seed);

    let connection = import_into(dir.path(), &package, &destination);

    assert_eq!(
        strings(&connection, "SELECT name FROM decks ORDER BY id"),
        ["Default".to_owned(), "Flags".to_owned()]
    );
    assert_eq!(
        ids(&connection, "SELECT id FROM decks ORDER BY id"),
        [1, 701]
    );
    assert_eq!(
        strings(&connection, "SELECT name FROM notetypes ORDER BY id"),
        ["Basic".to_owned()]
    );
    for column in ["models", "decks", "dconf", "tags"] {
        let stored: String = connection
            .query_row(
                &format!("SELECT {column} FROM col WHERE id = 1"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "", "{column} must stay empty in schema 18");
    }
    assert_eq!(
        strings(&connection, "SELECT name FROM decks ORDER BY id"),
        ["Default".to_owned(), "Flags".to_owned()]
    );
    assert_eq!(count(&connection, "decks"), 2);
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "10"),
        1
    );
    assert_eq!(
        query_id(&connection, "SELECT did FROM cards WHERE nid = ?", "11"),
        701
    );
    assert_eq!(
        query_id(
            &connection,
            "SELECT mid FROM notes WHERE guid = ?",
            "merged"
        ),
        1
    );
}

#[test]
fn fresh_legacy_import_keeps_schema18_deck_steps() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("schema18-source.anki2");
    write_schema18_collection(
        &source,
        &[(1, "Default"), (700, "Flags")],
        &[],
        &schema18_seed(1, 700),
    );
    let mut config = schema18_floats(1, &[5.0, 20.0]);
    config.extend(schema18_floats(2, &[3.0]));
    let connection = Connection::open(&source).unwrap();
    connection
        .execute(
            "UPDATE decks SET kind = ?1 WHERE id = 700",
            [ranki_storage::normal_deck_kind(2)],
        )
        .unwrap();
    connection
        .execute("UPDATE deck_config SET config = ?1 WHERE id = 2", [config])
        .unwrap();
    drop(connection);
    let package = schema18_package(dir.path(), &source);
    let collection = dir.path().join("legacy.anki2");
    let connection = import_into(dir.path(), &package, &collection);
    drop(connection);

    let store = AnkiCollection::open(&collection).unwrap();
    let deck_id = store
        .decks()
        .unwrap()
        .into_iter()
        .find(|deck| deck.name == "Flags")
        .unwrap()
        .id;
    assert_eq!(
        store.deck_steps(deck_id).unwrap(),
        DeckSteps {
            learning: vec![300, 1_200],
            relearning: vec![180],
        }
    );
}

#[test]
fn schema18_import_updates_an_existing_deck_preset() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("modern.anki2");
    write_schema18_collection(
        &destination,
        &[(1, "Default"), (700, "Flags")],
        &[],
        &Seed {
            notes: vec![],
            cards: vec![],
            revlog: false,
        },
    );
    let connection = Connection::open(&destination).unwrap();
    connection
        .execute(
            "UPDATE decks SET kind = ?1 WHERE id = 700",
            [ranki_storage::normal_deck_kind(1)],
        )
        .unwrap();
    drop(connection);

    let source = dir.path().join("schema18-source.anki2");
    write_schema18_collection(
        &source,
        &[(1, "Default"), (700, "Flags")],
        &[],
        &schema18_seed(1, 700),
    );
    let mut config = schema18_floats(1, &[5.0, 20.0]);
    config.extend(schema18_floats(2, &[3.0]));
    let connection = Connection::open(&source).unwrap();
    connection
        .execute(
            "UPDATE decks SET kind = ?1 WHERE id = 700",
            [ranki_storage::normal_deck_kind(2)],
        )
        .unwrap();
    connection
        .execute("UPDATE deck_config SET config = ?1 WHERE id = 2", [config])
        .unwrap();
    drop(connection);
    let package = schema18_package(dir.path(), &source);
    import_into(dir.path(), &package, &destination);

    let store = AnkiCollection::open(&destination).unwrap();
    let deck_id = store
        .decks()
        .unwrap()
        .into_iter()
        .find(|deck| deck.name == "Flags")
        .unwrap()
        .id;
    assert_eq!(
        store.deck_steps(deck_id).unwrap(),
        DeckSteps {
            learning: vec![300, 1_200],
            relearning: vec![180],
        }
    );
}

#[test]
fn real_world_import_puts_all_cards_in_the_package_deck() {
    let Some(fixture) = real_fixture() else {
        eprintln!("skipping real-world apkg test: RANKI_TEST_APKG is absent");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("WorldFlags.apkg");
    fs::copy(&fixture, &package).unwrap();
    let collection = dir.path().join("flags.anki2");
    let connection = import_into(dir.path(), &package, &collection);

    let decks = col_column(&connection, "decks");
    assert_eq!(decks.as_object().unwrap().len(), 2);
    assert_eq!(entry_ids(&decks, "Default"), [1]);
    let world_flags = entry_ids(&decks, "WorldFlags");
    assert_eq!(world_flags.len(), 1);
    let in_deck: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM cards WHERE did = ?1",
            [world_flags[0]],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(in_deck, 204);
}

#[test]
fn real_world_import_into_existing_collection_keeps_one_default_deck() {
    let Some(fixture) = real_fixture() else {
        eprintln!("skipping real-world apkg test: RANKI_TEST_APKG is absent");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("WorldFlags.apkg");
    fs::copy(&fixture, &package).unwrap();
    let collection = dir.path().join("existing.anki2");
    AnkiCollection::create(&collection)
        .unwrap()
        .close()
        .unwrap();

    let connection = import_into(dir.path(), &package, &collection);

    let decks = col_column(&connection, "decks");
    assert_eq!(
        entry_names(&decks),
        ["Default".to_owned(), "WorldFlags".to_owned()]
    );
    assert_eq!(entry_ids(&decks, "Default"), [1]);
    let models = col_column(&connection, "models");
    assert_eq!(entry_ids(&models, "Basic"), [1]);
}
