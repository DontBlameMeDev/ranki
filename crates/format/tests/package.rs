#![expect(clippy::unwrap_used, reason = "test fixtures use unwrap for setup")]

use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use ranki_format::ExportOptions;
use ranki_format::export_apkg_with_options;
use ranki_format::export_colpkg;
use ranki_format::export_colpkg_with_options;
use ranki_format::export_deck_apkg_with_options;
use ranki_format::import_apkg;
use ranki_format::restore_colpkg;
use ranki_model::NewNote;
use ranki_model::Store as _;
use ranki_storage::Collection;
use ranki_storage::checksum as note_checksum;
use ranki_storage::csum_value;
use ranki_storage::register_collations;
use ranki_storage::strip_html_preserving_media_filenames;
use rusqlite::Connection;
use sha1::Digest as _;
use sha1::Sha1;
use tempfile::TempDir;
use zip::CompressionMethod;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;
use zstd::stream::decode_all;
use zstd::stream::encode_all;

struct Fixture {
    dir: TempDir,
    collection: PathBuf,
    media: PathBuf,
}

impl Fixture {
    fn new(field: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let collection = dir.path().join("source.anki2");
        let media = dir.path().join("source.media");
        fs::create_dir_all(&media).unwrap();
        let mut store = Collection::create(&collection).unwrap();
        store
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![field.to_owned(), "back".to_owned()],
                tags: Vec::new(),
            })
            .unwrap();
        drop(store);
        Self {
            dir,
            collection,
            media,
        }
    }

    fn import(&self, package: &Path, name: &str) -> (PathBuf, PathBuf) {
        let collection = self.dir.path().join(format!("{name}.anki2"));
        let media = self.dir.path().join(format!("{name}.media"));
        import_apkg(package, &collection, &media).unwrap();
        (collection, media)
    }
}

fn write_zip(path: &Path, entries: &[(&str, Vec<u8>)]) {
    let mut writer = ZipWriter::new(fs::File::create(path).unwrap());
    for (name, bytes) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn zip_bytes(package: &Path, name: &str) -> Vec<u8> {
    let mut archive =
        zip::ZipArchive::new(fs::File::open(package).unwrap()).unwrap();
    let mut file = archive.by_name(name).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

fn compression(package: &Path, name: &str) -> CompressionMethod {
    let mut archive =
        zip::ZipArchive::new(fs::File::open(package).unwrap()).unwrap();
    archive.by_name(name).unwrap().compression()
}

fn zstd(bytes: &[u8]) -> Vec<u8> {
    encode_all(bytes, 0).unwrap()
}

#[expect(clippy::format_collect, reason = "test helper builds a hex string")]
fn sha1_hex(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hashed_name(name: &str, bytes: &[u8]) -> String {
    let hash = sha1_hex(bytes);
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    if extension.is_empty() {
        format!("{stem}-{hash}")
    } else {
        format!("{stem}-{hash}.{extension}")
    }
}

fn checksum(field: &str) -> i64 {
    csum_value(note_checksum(field))
}

fn note_row(collection: &Path) -> (String, String, i64) {
    let connection = Connection::open(collection).unwrap();
    connection
        .query_row("SELECT flds, sfld, csum FROM notes LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
}

fn media_db_rows(media: &Path) -> Vec<(String, String, i64, i64)> {
    let db = media.with_extension("media.db2");
    let connection = Connection::open(db).unwrap();
    let mut statement = connection
        .prepare("SELECT fname, csum, mtime, dirty FROM media ORDER BY fname")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn append_varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        bytes.push(u8::try_from(value & 0x7F).unwrap_or_default() | 0x80);
        value >>= 7;
    }
    bytes.push(u8::try_from(value & 0x7F).unwrap_or_default());
}

fn protobuf_string_field(field: u32, value: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    append_varint(&mut bytes, u64::from(field) << 3 | 2);
    append_varint(&mut bytes, u64::try_from(value.len()).unwrap_or_default());
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

#[expect(
    clippy::excessive_nesting,
    reason = "test protobuf decoding is clearer inline"
)]
fn protobuf_string(bytes: &[u8], wanted: u32) -> Option<String> {
    let mut cursor = 0;
    while cursor < bytes.len() {
        let (tag, after_tag) = test_varint(bytes, cursor)?;
        cursor = after_tag;
        let field = tag >> 3;
        match tag & 7 {
            0 => cursor = test_varint(bytes, cursor)?.1,
            2 => {
                let (raw_length, after_length) = test_varint(bytes, cursor)?;
                let field_length = usize::try_from(raw_length).ok()?;
                let end = after_length.checked_add(field_length)?;
                if field == u64::from(wanted) {
                    return String::from_utf8(
                        bytes.get(after_length..end)?.to_vec(),
                    )
                    .ok();
                }
                cursor = end;
            }
            1 => cursor = cursor.checked_add(8)?,
            5 => cursor = cursor.checked_add(4)?,
            _ => return None,
        }
    }
    None
}

fn test_varint(bytes: &[u8], mut cursor: usize) -> Option<(u64, usize)> {
    let mut value = 0_u64;
    let mut shift = 0_u32;
    loop {
        let byte = *bytes.get(cursor)?;
        cursor = cursor.checked_add(1)?;
        value =
            value.checked_add(u64::from(byte & 0x7F).checked_shl(shift)?)?;
        if byte & 0x80 == 0 {
            return Some((value, cursor));
        }
        shift = shift.checked_add(7)?;
    }
}

fn media_map_without_size(name: &str) -> Vec<u8> {
    let mut entry = Vec::new();
    append_varint(&mut entry, 0x0A);
    append_varint(&mut entry, u64::try_from(name.len()).unwrap_or_default());
    entry.extend_from_slice(name.as_bytes());
    append_varint(&mut entry, 0x1A);
    append_varint(&mut entry, 20);
    entry.extend_from_slice(Sha1::digest(b"").as_slice());
    let mut bytes = Vec::new();
    append_varint(&mut bytes, 0x0A);
    append_varint(&mut bytes, u64::try_from(entry.len()).unwrap_or_default());
    bytes.extend_from_slice(&entry);
    bytes
}

fn media_map(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (index, (name, data)) in entries.iter().enumerate() {
        let mut entry = Vec::new();
        append_varint(&mut entry, 0x0A);
        append_varint(&mut entry, name.len() as u64);
        entry.extend_from_slice(name.as_bytes());
        append_varint(&mut entry, 0x10);
        append_varint(&mut entry, data.len() as u64);
        append_varint(&mut entry, 0x1A);
        append_varint(&mut entry, 20);
        entry.extend_from_slice(Sha1::digest(data).as_slice());
        append_varint(&mut bytes, 0x0A);
        append_varint(&mut bytes, entry.len() as u64);
        bytes.extend_from_slice(&entry);
        let _ = index;
    }
    bytes
}

#[test]
fn legacy_one_and_two_packages_read_media_and_ignore_unmapped_entries() {
    for (version, collection_name) in
        [(1_u8, "collection.anki2"), (2_u8, "collection.anki21")]
    {
        let fixture = Fixture::new("<img src=pic.png> front");
        fs::write(fixture.media.join("pic.png"), b"image").unwrap();
        let mapping = serde_json::json!({"3": "pic.png"}).to_string();
        let package = fixture.dir.path().join(format!("legacy{version}.apkg"));
        write_zip(
            &package,
            &[
                (collection_name, fs::read(&fixture.collection).unwrap()),
                ("3", b"image".to_vec()),
                ("4", b"unmapped".to_vec()),
                ("media", mapping.into_bytes()),
                ("meta", vec![0x08, version]),
            ],
        );

        let (collection, media) =
            fixture.import(&package, &format!("legacy{version}"));
        assert_eq!(fs::read(media.join("pic.png")).unwrap(), b"image");
        assert!(!media.join("4").exists());
        assert!(
            note_row(&collection)
                .0
                .starts_with("<img src=pic.png> front")
        );
        let rows = media_db_rows(&media);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "pic.png");
        assert_eq!(rows[0].1, sha1_hex(b"image"));
        assert!(rows[0].2 > 0);
        assert_eq!(rows[0].3, 1);
    }
}

#[test]
fn latest_package_uses_zstd_and_validates_media_metadata() {
    let fixture = Fixture::new("<img src=pic.png> front");
    let collection_bytes = zstd(&fs::read(&fixture.collection).unwrap());
    let media_bytes = zstd(b"image");
    let package = fixture.dir.path().join("latest.apkg");
    write_zip(
        &package,
        &[
            ("collection.anki21b", collection_bytes),
            ("0", media_bytes),
            ("media", zstd(&media_map(&[("pic.png", b"image")]))),
            ("meta", vec![0x08, 3]),
        ],
    );

    let (_, media) = fixture.import(&package, "latest");
    assert_eq!(fs::read(media.join("pic.png")).unwrap(), b"image");

    let bad = fixture.dir.path().join("bad-hash.apkg");
    let map = media_map(&[("pic.png", b"other")]);
    write_zip(
        &bad,
        &[
            (
                "collection.anki21b",
                zstd(&fs::read(&fixture.collection).unwrap()),
            ),
            ("0", zstd(b"image")),
            ("media", zstd(&map)),
            ("meta", vec![0x08, 3]),
        ],
    );
    let error = import_apkg(
        &bad,
        &fixture.dir.path().join("bad.anki2"),
        &fixture.dir.path().join("bad.media"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("wrong checksum"), "{error}");
}

#[test]
fn unknown_package_version_is_rejected() {
    let fixture = Fixture::new("front");
    let package = fixture.dir.path().join("future.apkg");
    write_zip(
        &package,
        &[
            (
                "collection.anki21b",
                zstd(&fs::read(&fixture.collection).unwrap()),
            ),
            ("media", zstd(&media_map(&[]))),
            ("meta", vec![0x08, 9]),
        ],
    );
    let error = import_apkg(
        &package,
        &fixture.dir.path().join("future.anki2"),
        &fixture.dir.path().join("future.media"),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("unsupported package version"),
        "{error}"
    );
}

#[test]
fn legacy_media_with_zstd_magic_is_not_decompressed() {
    let raw = [0x28, 0xB5, 0x2F, 0xFD, 0x01, 0x02];
    let fixture = Fixture::new("<img src=raw.bin> front");
    let package = fixture.dir.path().join("raw.apkg");
    write_zip(
        &package,
        &[
            ("collection.anki21", fs::read(&fixture.collection).unwrap()),
            ("0", raw.to_vec()),
            ("media", br#"{"0":"raw.bin"}"#.to_vec()),
            ("meta", vec![0x08, 0x02]),
        ],
    );

    let (_, media) = fixture.import(&package, "raw");
    assert_eq!(fs::read(media.join("raw.bin")).unwrap(), raw);
}

#[test]
fn import_references_only_media_and_recomputes_note_fields() {
    let fixture = Fixture::new("<img src=PIC.PNG> front");
    fs::write(fixture.media.join("PIC.PNG"), b"new").unwrap();
    fs::write(fixture.media.join("unused.png"), b"unused").unwrap();
    let package = fixture.dir.path().join("media.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();

    let destination_media = fixture.dir.path().join("destination.media");
    fs::create_dir_all(&destination_media).unwrap();
    fs::write(destination_media.join("pic.png"), b"old").unwrap();
    let collection = fixture.dir.path().join("destination.anki2");
    import_apkg(&package, &collection, &destination_media).unwrap();

    let hashed = hashed_name("pic.png", b"new");
    assert!(destination_media.join(&hashed).exists());
    assert!(!destination_media.join("unused.png").exists());
    assert_eq!(fs::read(destination_media.join("pic.png")).unwrap(), b"old");

    let (fields, sort_field, sum) = note_row(&collection);
    assert!(fields.contains(&hashed), "{fields}");
    assert!(!fields.contains("PIC.PNG"));
    assert!(!fields.contains("unused.png"));
    let first_field = fields.split('\u{001f}').next().unwrap_or_default();
    assert_eq!(
        sort_field,
        strip_html_preserving_media_filenames(first_field)
    );
    assert_eq!(sum, checksum(first_field));

    let rows = media_db_rows(&destination_media);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, hashed);
    assert_eq!(rows[0].1, sha1_hex(b"new"));
    assert_eq!(rows[0].3, 1);
}

#[test]
fn schema11_export_has_legacy_envelope_and_referenced_media_only() {
    let fixture = Fixture::new("<img src=pic.png> front");
    fs::write(fixture.media.join("pic.png"), b"image").unwrap();
    fs::write(fixture.media.join("unused.png"), b"unused").unwrap();
    let package = fixture.dir.path().join("legacy.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();

    assert_eq!(zip_bytes(&package, "meta"), vec![0x08, 0x02]);
    assert!(zip_bytes(&package, "collection.anki21").starts_with(b"SQLite"));
    assert!(zip_bytes(&package, "collection.anki2").starts_with(b"SQLite"));
    assert_eq!(compression(&package, "0"), CompressionMethod::Stored);
    assert_eq!(
        compression(&package, "collection.anki21"),
        CompressionMethod::Deflated
    );
    assert_eq!(zip_bytes(&package, "0"), b"image");
    let names: Vec<String> = {
        let archive =
            zip::ZipArchive::new(fs::File::open(&package).unwrap()).unwrap();
        archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect()
    };
    assert!(!names.iter().any(|name| name == "1"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&zip_bytes(
            &package, "media"
        ))
        .unwrap(),
        serde_json::json!({"0": "pic.png"})
    );
}

#[test]
fn schema18_export_has_latest_envelope() {
    let fixture = Fixture::new("<img src=pic.png> front");
    fs::write(fixture.media.join("pic.png"), b"image").unwrap();
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        connection
            .execute("UPDATE col SET ver = 18 WHERE id = 1", [])
            .unwrap();
    }
    let package = fixture.dir.path().join("latest.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();

    assert_eq!(zip_bytes(&package, "meta"), vec![0x08, 0x03]);
    assert!(
        zip_bytes(&package, "collection.anki21b")
            .starts_with(&[0x28, 0xB5, 0x2F, 0xFD])
    );
    assert!(zip_bytes(&package, "collection.anki2").starts_with(b"SQLite"));
    for name in ["meta", "collection.anki21b", "collection.anki2", "media"] {
        assert_eq!(
            compression(&package, name),
            CompressionMethod::Stored,
            "{name}"
        );
    }
    let mapping = decode_all(zip_bytes(&package, "media").as_slice()).unwrap();
    assert!(String::from_utf8_lossy(&mapping).contains("pic"));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the export option assertions are clearer together"
)]
fn export_options_strip_graves_scheduling_and_filtered_state() {
    let fixture = Fixture::new("<img src=pic.png> front");
    fs::write(fixture.media.join("pic.png"), b"image").unwrap();
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        connection
            .execute_batch(
                "INSERT INTO graves (usn, oid, type) VALUES (0, 99, 1);
                 UPDATE notes SET tags = ' marked leech kept ';
                 UPDATE cards SET queue = 2, ivl = 10, factor = 2500;",
            )
            .unwrap();
        let raw_dconf: String = connection
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut configs: serde_json::Value =
            serde_json::from_str(&raw_dconf).unwrap();
        configs["9"] = serde_json::json!({"id": 9, "name": "unused"});
        connection
            .execute(
                "UPDATE col SET dconf = ?1 WHERE id = 1",
                [configs.to_string()],
            )
            .unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["1"]["dyn"] = serde_json::json!(1);
        decks["1"]["lrnToday"] = serde_json::json!([1, 2]);
        decks["1"]["conf"] = serde_json::json!(9);
        let updated = decks.to_string();
        connection
            .execute("UPDATE col SET decks = ?1 WHERE id = 1", [updated])
            .unwrap();
    }
    let package = fixture.dir.path().join("minimal.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions {
            with_scheduling: false,
            with_deck_configs: false,
            with_media: false,
            legacy: false,
        },
    )
    .unwrap();

    let collection_bytes =
        decode_all(zip_bytes(&package, "collection.anki21").as_slice())
            .unwrap_or_else(|_| zip_bytes(&package, "collection.anki21"));
    let path = fixture.dir.path().join("exported.anki2");
    fs::write(&path, &collection_bytes).unwrap();
    let connection = Connection::open(&path).unwrap();
    let graves: i64 = connection
        .query_row("SELECT COUNT(*) FROM graves", [], |row| row.get(0))
        .unwrap();
    let reviews: i64 = connection
        .query_row("SELECT COUNT(*) FROM revlog", [], |row| row.get(0))
        .unwrap();
    let queue: i64 = connection
        .query_row("SELECT queue FROM cards LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let decks: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    let dconf: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        dconf.as_object().map(serde_json::Map::len),
        Some(1),
        "unused deck configs must be stripped"
    );
    assert_eq!(graves, 0);
    assert_eq!(reviews, 0);
    assert_eq!(queue, 0);
    assert_eq!(decks["1"]["dyn"], 0);
    assert_eq!(decks["1"]["conf"], 1);
    assert_eq!(decks["1"]["lrnToday"], serde_json::json!([0, 0]));
    let tags: String = connection
        .query_row("SELECT tags FROM notes LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(!tags.contains("marked"));
    assert!(tags.contains("kept"));

    let names: Vec<String> = {
        let archive =
            zip::ZipArchive::new(fs::File::open(&package).unwrap()).unwrap();
        archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect()
    };
    assert!(!names.iter().any(|name| name == "0"));
    assert_eq!(zip_bytes(&package, "media"), b"{}");
}

#[test]
fn filtered_deck_cards_lose_transient_original_deck() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["2"] = serde_json::json!({
            "id": 2,
            "name": "Filtered",
            "dyn": 1,
            "lrnToday": [1, 2]
        });
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE cards SET did = 2, odid = 1, odue = 123 WHERE id = (SELECT MIN(id) FROM cards)",
                [],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("filtered.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();

    let path = fixture.dir.path().join("filtered-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(&path).unwrap();
    let (due, original, original_due): (i64, i64, i64) = connection
        .query_row(
            "SELECT due, odid, odue FROM cards WHERE did = 2 LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(due, 123);
    assert_eq!(original, 0);
    assert_eq!(original_due, 0);
    let decks: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(decks["2"]["dyn"], 0);
    assert_eq!(decks["2"]["lrnToday"], serde_json::json!([0, 0]));
}

#[test]
fn id_validation_accepts_near_future_ids_but_rejects_absurd_ones() {
    let fixture = Fixture::new("front");
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        connection
            .execute(
                "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type)
                 VALUES (?1, (SELECT MIN(id) FROM cards), 0, 3, 1, 1, 2500, 1, 0)",
                [now + 2 * 86_400_000],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("future-ok.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();

    {
        let connection = Connection::open(&fixture.collection).unwrap();
        connection
            .execute("UPDATE revlog SET id = ?1", [now + 30 * 86_400_000])
            .unwrap();
    }
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &fixture.dir.path().join("future-boundary.apkg"),
        ExportOptions::default(),
    )
    .unwrap();

    for (name, future_id) in [
        ("future-bad.apkg", now + 30 * 86_400_000 + 60_000),
        ("absurd-future.apkg", now + 365 * 86_400_000),
    ] {
        {
            let connection = Connection::open(&fixture.collection).unwrap();
            connection
                .execute("UPDATE revlog SET id = ?1", [future_id])
                .unwrap();
        }
        let error = export_apkg_with_options(
            &fixture.collection,
            &fixture.media,
            &fixture.dir.path().join(name),
            ExportOptions::default(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("invalid revlog identifier"),
            "{error}"
        );
    }
}

#[test]
fn latest_media_map_allows_omitted_zero_size() {
    let fixture = Fixture::new("<img src=empty.png> front");
    let package = fixture.dir.path().join("empty.apkg");
    write_zip(
        &package,
        &[
            (
                "collection.anki21b",
                zstd(&fs::read(&fixture.collection).unwrap()),
            ),
            ("0", zstd(b"")),
            ("media", zstd(&media_map_without_size("empty.png"))),
            ("meta", vec![0x08, 0x03]),
        ],
    );

    let (_, media) = fixture.import(&package, "empty");
    assert_eq!(fs::read(media.join("empty.png")).unwrap(), b"");
}

#[test]
fn exact_media_names_are_not_shadowed_by_normalized_aliases() {
    let fixture = Fixture::new("<img src=Pic.png> <img src=pic.png> front");
    let package = fixture.dir.path().join("aliases.apkg");
    let mapping =
        serde_json::json!({"0": "Pic.png", "1": "pic.png"}).to_string();
    write_zip(
        &package,
        &[
            ("collection.anki21", fs::read(&fixture.collection).unwrap()),
            ("0", b"A".to_vec()),
            ("1", b"B".to_vec()),
            ("media", mapping.into_bytes()),
            ("meta", vec![0x08, 0x02]),
        ],
    );

    let (collection, media) = fixture.import(&package, "aliases");
    let hashed = hashed_name("pic.png", b"B");
    assert_eq!(fs::read(media.join("pic.png")).unwrap(), b"A");
    assert_eq!(fs::read(media.join(&hashed)).unwrap(), b"B");
    let (fields, _, _) = note_row(&collection);
    assert_eq!(fields.matches("src=pic.png").count(), 1, "{fields}");
    assert_eq!(fields.matches(&hashed).count(), 1, "{fields}");
}

#[test]
fn template_media_references_are_selected_and_rewritten() {
    let fixture = Fixture::new("front");
    fs::write(fixture.media.join("template.png"), b"image").unwrap();
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let raw: String = connection
            .query_row("SELECT models FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut models: serde_json::Value = serde_json::from_str(&raw).unwrap();
        models["1"]["css"] =
            serde_json::json!("background: url(template.png);");
        models["1"]["tmpls"][0]["qfmt"] =
            serde_json::json!(r#"<img src = "template.png">{{Front}}"#);
        models["1"]["tmpls"][0]["bqfmt"] =
            serde_json::json!("<img src=template.png>");
        connection
            .execute(
                "UPDATE col SET models = ?1 WHERE id = 1",
                [models.to_string()],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("template.colpkg");
    export_colpkg(&fixture.collection, &fixture.media, &package).unwrap();
    assert_eq!(zip_bytes(&package, "0"), b"image");

    let destination_media = fixture.dir.path().join("template-import.media");
    fs::create_dir_all(&destination_media).unwrap();
    fs::write(destination_media.join("template.png"), b"old").unwrap();
    let collection = fixture.dir.path().join("template-import.anki2");
    restore_colpkg(&package, &collection, &destination_media).unwrap();
    let hashed = hashed_name("template.png", b"image");
    let connection = Connection::open(&collection).unwrap();
    let models: String = connection
        .query_row("SELECT models FROM col WHERE id = 1", [], |row| row.get(0))
        .unwrap();
    assert!(models.contains(&hashed), "{models}");
    assert_eq!(models.matches(&hashed).count(), 3, "{models}");
}

#[test]
fn colpkg_backup_keeps_empty_decks_and_unused_models() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let decks: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut deck_values: serde_json::Value =
            serde_json::from_str(&decks).unwrap();
        deck_values["2"] =
            serde_json::json!({"id": 2, "name": "Empty", "dyn": 0});
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [deck_values.to_string()],
            )
            .unwrap();
        let models: String = connection
            .query_row("SELECT models FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut model_values: serde_json::Value =
            serde_json::from_str(&models).unwrap();
        model_values["2"] = model_values["1"].clone();
        connection
            .execute(
                "UPDATE col SET models = ?1 WHERE id = 1",
                [model_values.to_string()],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("backup.colpkg");
    export_colpkg(&fixture.collection, &fixture.media, &package).unwrap();
    let path = fixture.dir.path().join("backup-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let decks: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    let models: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT models FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert!(decks.get("2").is_some());
    assert!(models.get("2").is_some());
}

#[test]
fn restore_rejects_incomplete_collection_schemas() {
    let fixture = Fixture::new("front");
    let incomplete = fixture.dir.path().join("incomplete.anki2");
    {
        let connection = Connection::open(&incomplete).unwrap();
        connection
            .execute_batch("CREATE TABLE col (id INTEGER PRIMARY KEY, ver INTEGER); INSERT INTO col VALUES (1, 11);")
            .unwrap();
    }
    let package = fixture.dir.path().join("incomplete.colpkg");
    write_zip(
        &package,
        &[
            ("collection.anki21", fs::read(&incomplete).unwrap()),
            ("media", b"{}".to_vec()),
            ("meta", vec![0x08, 0x02]),
        ],
    );
    let error = restore_colpkg(
        &package,
        &fixture.dir.path().join("restored.anki2"),
        &fixture.dir.path().join("restore.media"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("missing col.crt"), "{error}");
}

#[test]
fn restore_accepts_schema18_collections_with_unicase_indexes() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        register_collations(&connection).unwrap();
        connection
            .execute("UPDATE col SET ver = 18 WHERE id = 1", [])
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE fields (ntid INTEGER, ord INTEGER, name TEXT COLLATE unicase, config BLOB);
                 CREATE TABLE templates (ntid INTEGER, ord INTEGER, name TEXT COLLATE unicase, config BLOB);
                 CREATE UNIQUE INDEX idx_fields_name ON fields (name);
                 CREATE UNIQUE INDEX idx_templates_name ON templates (name);
                 INSERT INTO fields VALUES (1, 0, 'Front', x'');
                 INSERT INTO templates VALUES (1, 0, 'Card 1', x'');",
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("schema18.colpkg");
    write_zip(
        &package,
        &[
            (
                "collection.anki21b",
                zstd(&fs::read(&fixture.collection).unwrap()),
            ),
            ("media", zstd(&media_map(&[]))),
            ("meta", vec![0x08, 0x03]),
        ],
    );
    let target = fixture.dir.path().join("schema18-restored.anki2");
    restore_colpkg(
        &package,
        &target,
        &fixture.dir.path().join("schema18.media"),
    )
    .unwrap();
    assert!(target.is_file());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the export flag assertions are clearer together"
)]
fn colpkg_flags_reset_scheduling_and_presets_without_pruning() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        connection
            .execute(
                "INSERT INTO revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type)
                 VALUES (99, (SELECT MIN(id) FROM cards), 0, 3, 10, 5, 2500, 1, 0)",
                [],
            )
            .unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["1"]["conf"] = serde_json::json!(9);
        decks["2"] =
            serde_json::json!({"id": 2, "name": "Empty", "dyn": 0, "conf": 9});
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        let raw: String = connection
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut configs: serde_json::Value =
            serde_json::from_str(&raw).unwrap();
        configs["9"] = configs["1"].clone();
        connection
            .execute(
                "UPDATE col SET dconf = ?1 WHERE id = 1",
                [configs.to_string()],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("flags.colpkg");
    export_colpkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions {
            with_scheduling: false,
            with_deck_configs: false,
            with_media: true,
            legacy: false,
        },
    )
    .unwrap();

    let path = fixture.dir.path().join("flags-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let reviews: i64 = connection
        .query_row("SELECT COUNT(*) FROM revlog", [], |row| row.get(0))
        .unwrap();
    let queue: i64 = connection
        .query_row("SELECT queue FROM cards LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let decks: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    let configs: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(reviews, 0);
    assert_eq!(queue, 0);
    assert_eq!(decks["1"]["conf"], 1);
    assert_eq!(decks["2"]["conf"], 1);
    assert_eq!(decks["1"]["newToday"], serde_json::json!([0, 0]));
    assert_eq!(decks["2"]["revToday"], serde_json::json!([0, 0]));
    assert!(decks.get("2").is_some());
    assert_eq!(configs.as_object().map(serde_json::Map::len), Some(1));
    assert!(configs.get("1").is_some_and(serde_json::Value::is_object));
}

#[test]
fn schema18_template_config_media_is_rewritten_on_restore() {
    let fixture = Fixture::new("front");
    fs::write(fixture.media.join("logo.png"), b"image").unwrap();
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        register_collations(&connection).unwrap();
        connection
            .execute("UPDATE col SET ver = 18, models = '{}' WHERE id = 1", [])
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE notetypes (id INTEGER PRIMARY KEY, name TEXT, mtime_secs INTEGER, usn INTEGER NOT NULL DEFAULT 0, config BLOB);
                 CREATE TABLE fields (ntid INTEGER, ord INTEGER, name TEXT COLLATE unicase, config BLOB, usn INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE templates (ntid INTEGER, ord INTEGER, name TEXT COLLATE unicase, config BLOB, usn INTEGER NOT NULL DEFAULT 0);
                 CREATE UNIQUE INDEX idx_fields_name ON fields (name);
                 CREATE UNIQUE INDEX idx_templates_name ON templates (name);",
            )
            .unwrap();
        let css = protobuf_string_field(3, "background: url(logo.png);");
        let qfmt = protobuf_string_field(1, "<img src=logo.png>{{Front}}");
        connection
            .execute(
                "INSERT INTO notetypes (id, name, mtime_secs, usn, config) VALUES (1, 'Basic', 0, 0, ?1)",
                [css],
            )
            .unwrap();
        connection
            .execute("INSERT INTO fields (ntid, ord, name, config) VALUES (1, 0, 'Front', x'')", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO templates (ntid, ord, name, config) VALUES (1, 0, 'Card 1', ?1)",
                [qfmt],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("schema18-template.colpkg");
    export_colpkg(&fixture.collection, &fixture.media, &package).unwrap();
    assert_eq!(
        decode_all(zip_bytes(&package, "0").as_slice()).unwrap(),
        b"image"
    );

    let destination_media = fixture.dir.path().join("schema18-template.media");
    fs::create_dir_all(&destination_media).unwrap();
    fs::write(destination_media.join("logo.png"), b"old").unwrap();
    let collection = fixture.dir.path().join("schema18-template.anki2");
    restore_colpkg(&package, &collection, &destination_media).unwrap();

    let connection = Connection::open(&collection).unwrap();
    let hashed = hashed_name("logo.png", b"image");
    let css: Vec<u8> = connection
        .query_row("SELECT config FROM notetypes WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let question: Vec<u8> = connection
        .query_row("SELECT config FROM templates WHERE ntid = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let expected_css = format!("background: url({hashed});");
    let expected_question = format!("<img src={hashed}>{{{{Front}}}}");
    assert_eq!(
        protobuf_string(&css, 3).as_deref(),
        Some(expected_css.as_str())
    );
    assert_eq!(
        protobuf_string(&question, 1).as_deref(),
        Some(expected_question.as_str())
    );
}

#[test]
fn restore_creates_parent_and_preserves_earlier_backups() {
    let fixture = Fixture::new("front");
    let package = fixture.dir.path().join("nested.colpkg");
    export_colpkg(&fixture.collection, &fixture.media, &package).unwrap();
    let target = fixture.dir.path().join("missing/collection.anki2");
    let media = fixture.dir.path().join("nested.media");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(&fixture.collection, &target).unwrap();

    restore_colpkg(&package, &target, &media).unwrap();
    let first = target.with_extension("pre-restore.anki2");
    assert!(first.is_file());
    {
        let connection = Connection::open(&target).unwrap();
        connection
            .execute("UPDATE notes SET flds = 'changed'", [])
            .unwrap();
    }
    restore_colpkg(&package, &target, &media).unwrap();
    let second = target.with_extension("pre-restore-1.anki2");
    assert!(second.is_file());
    let first_bytes = fs::read(&first).unwrap();
    let first_text = String::from_utf8_lossy(&first_bytes);
    assert!(first_text.contains("front"));
}

#[test]
fn remote_url_media_is_not_selected() {
    let fixture = Fixture::new("<img src=https://example.com/logo.png> front");
    fs::write(fixture.media.join("logo.png"), b"remote").unwrap();
    let package = fixture.dir.path().join("remote.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();
    let names = {
        let archive =
            zip::ZipArchive::new(fs::File::open(&package).unwrap()).unwrap();
        archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect::<Vec<_>>()
    };
    assert!(!names.iter().any(|name| name == "0"));
}

#[test]
fn no_scheduling_exports_bounded_new_card_positions() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        connection
            .execute(
                "UPDATE cards SET id = 1700000000000, due = 1700000000000, odid = 1, odue = 99",
                [],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("bounded.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions {
            with_scheduling: false,
            with_deck_configs: true,
            with_media: true,
            legacy: false,
        },
    )
    .unwrap();
    let path = fixture.dir.path().join("bounded-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let (due, original): (i64, i64) = connection
        .query_row("SELECT due, odid FROM cards LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert!((0..1_000_000_000).contains(&due), "{due}");
    assert_eq!(original, 0);
}

#[test]
fn restore_creates_missing_parent_directory() {
    let fixture = Fixture::new("front");
    let package = fixture.dir.path().join("fresh.colpkg");
    export_colpkg(&fixture.collection, &fixture.media, &package).unwrap();
    let target = fixture.dir.path().join("new-root/collection.anki2");
    restore_colpkg(&package, &target, &fixture.dir.path().join("fresh.media"))
        .unwrap();
    assert!(target.is_file());
}

#[test]
fn latest_backup_with_decomposed_media_name_is_restorable() {
    let fixture = Fixture::new("<img src=cafe\u{301}.png> front");
    let name = "cafe\u{301}.png";
    let package = fixture.dir.path().join("unicode.colpkg");
    write_zip(
        &package,
        &[
            (
                "collection.anki21b",
                zstd(&fs::read(&fixture.collection).unwrap()),
            ),
            ("0", zstd(b"image")),
            ("media", zstd(&media_map(&[(name, b"image")]))),
            ("meta", vec![0x08, 0x03]),
        ],
    );

    let target = fixture.dir.path().join("unicode-restored.anki2");
    let media = fixture.dir.path().join("unicode.media");
    restore_colpkg(&package, &target, &media).unwrap();
    assert_eq!(fs::read(media.join("café.png")).unwrap(), b"image");
    let (fields, _, _) = note_row(&target);
    assert!(fields.contains("café.png"), "{fields}");
}

#[test]
fn no_scheduling_returns_filtered_cards_to_home_decks() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["2"] = serde_json::json!({"id": 2, "name": "Filtered", "dyn": 1});
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        connection
            .execute("UPDATE cards SET did = 2, odid = 1, odue = 99", [])
            .unwrap();
    }
    let package = fixture.dir.path().join("home.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions {
            with_scheduling: false,
            with_deck_configs: true,
            with_media: true,
            legacy: false,
        },
    )
    .unwrap();
    let path = fixture.dir.path().join("home-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let deck_id: i64 = connection
        .query_row("SELECT did FROM cards LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(deck_id, 1);
}

#[test]
fn preset_only_colpkg_preserves_filtered_card_state() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["1"]["conf"] = serde_json::json!(9);
        decks["2"] = serde_json::json!({
            "id": 2, "name": "Filtered", "dyn": 1, "conf": 9
        });
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        connection
            .execute("UPDATE cards SET did = 2, odid = 1, odue = 123", [])
            .unwrap();
    }
    let package = fixture.dir.path().join("preset-only.colpkg");
    export_colpkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions {
            with_scheduling: true,
            with_deck_configs: false,
            with_media: true,
            legacy: false,
        },
    )
    .unwrap();
    let path = fixture.dir.path().join("preset-only-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let (did, original, original_due): (i64, i64, i64) = connection
        .query_row("SELECT did, odid, odue FROM cards LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    let decks: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(did, 2);
    assert_eq!(original, 1);
    assert_eq!(original_due, 123);
    assert_eq!(decks["1"]["conf"], 1);
    assert_eq!(decks["2"]["conf"], 1);
}

#[test]
fn repeated_exports_write_the_same_valid_dummy_collection() {
    let fixture = Fixture::new("front");
    let first = fixture.dir.path().join("first.apkg");
    let second = fixture.dir.path().join("second.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &first,
        ExportOptions::default(),
    )
    .unwrap();
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &second,
        ExportOptions::default(),
    )
    .unwrap();
    let first_dummy = zip_bytes(&first, "collection.anki2");
    let second_dummy = zip_bytes(&second, "collection.anki2");
    assert_eq!(first_dummy, second_dummy);
    assert!(first_dummy.starts_with(b"SQLite"));
    let path = fixture.dir.path().join("dummy.anki2");
    fs::write(&path, &first_dummy).unwrap();
    Collection::open(&path).unwrap();
}

#[test]
fn selected_deck_export_keeps_deck_ids_inside_the_package() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["2"] = serde_json::json!({"id": 2, "name": "Selected", "dyn": 1});
        decks["3"] = serde_json::json!({"id": 3, "name": "Home", "dyn": 0});
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        connection
            .execute("UPDATE cards SET did = 2, odid = 3", [])
            .unwrap();
    }
    let package = fixture.dir.path().join("selected.apkg");
    export_deck_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        2,
        ExportOptions {
            with_scheduling: false,
            with_deck_configs: true,
            with_media: true,
            legacy: false,
        },
    )
    .unwrap();
    let path = fixture.dir.path().join("selected-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let did: i64 = connection
        .query_row("SELECT did FROM cards LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let decks: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(did, 2);
    assert!(decks.get("3").is_none());
}

#[test]
fn converted_filtered_deck_gets_default_preset() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["2"] = serde_json::json!({"id": 2, "name": "Filtered", "dyn": 1});
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        connection.execute("UPDATE cards SET did = 2", []).unwrap();
    }
    let package = fixture.dir.path().join("preset.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();
    let path = fixture.dir.path().join("preset-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let decks: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(decks["2"]["dyn"], 0);
    assert_eq!(decks["2"]["conf"], 1);
}

#[test]
fn numeric_string_preset_ids_are_retained() {
    let fixture = Fixture::new("front");
    {
        let connection = Connection::open(&fixture.collection).unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["1"]["conf"] = serde_json::json!("9");
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        let raw: String = connection
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut configs: serde_json::Value =
            serde_json::from_str(&raw).unwrap();
        configs["9"] = configs["1"].clone();
        connection
            .execute(
                "UPDATE col SET dconf = ?1 WHERE id = 1",
                [configs.to_string()],
            )
            .unwrap();
    }
    let package = fixture.dir.path().join("string-preset.apkg");
    export_apkg_with_options(
        &fixture.collection,
        &fixture.media,
        &package,
        ExportOptions::default(),
    )
    .unwrap();
    let path = fixture.dir.path().join("string-preset-export.anki2");
    fs::write(&path, zip_bytes(&package, "collection.anki21")).unwrap();
    let connection = Connection::open(path).unwrap();
    let configs: serde_json::Value = serde_json::from_str(
        &connection
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert!(configs.get("9").is_some());
}
