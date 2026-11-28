//! Anki package import and export.
//!
//! The crate reads and writes `.apkg` and `.colpkg` files directly. Collection
//! changes are applied through SQLite so existing notes, cards and metadata
//! survive an import.

mod error;
mod media;
mod media_db;
mod merge;
mod package;
mod schema;
mod text;

pub use error::FormatError;
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use ranki_model::Id;
use ranki_storage::Collection;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;
use serde_json::Value;
use tempfile::Builder;
use tempfile::NamedTempFile;

use crate::media::MediaEntry;
use crate::media::MediaFile;
use crate::media::MediaRename;
use crate::media::ReferenceMatcher;
use crate::media::copy_media_with_db;
use crate::media::percent_encode;
use crate::media::sanitize_filename;
use crate::media::sha1_bytes;
use crate::media_db::media_db_path;
use crate::package::choose_version;
use crate::package::validate_collection_ids;
use crate::text::media_reference_names;

/// Counts inserted records from an `.apkg` import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub notes: usize,
    pub cards: usize,
    pub media: usize,
}

/// Choices shared by package exports.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four export choices map directly onto CLI flags"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    pub with_scheduling: bool,
    pub with_deck_configs: bool,
    pub with_media: bool,
    pub legacy: bool,
    pub overwrite: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            with_scheduling: true,
            with_deck_configs: true,
            with_media: true,
            legacy: false,
            overwrite: true,
        }
    }
}

/// Result of replacing a collection from a `.colpkg` backup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreReport {
    pub media: usize,
    pub backup: Option<PathBuf>,
}

/// Merge an `.apkg` into an existing collection, creating one when needed.
///
/// # Errors
///
/// Returns an error when the package is malformed, media cannot be copied, or
/// the destination collection cannot be opened and updated.
pub fn import_apkg(
    imported_package: &Path,
    collection: &Path,
    media_dir: &Path,
) -> Result<ImportReport, FormatError> {
    let scratch_dir = collection
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    schema::ensure_parent(collection)?;
    let mut contents = package::read_apkg(imported_package, scratch_dir)?;
    let source_crt = package_creation_time(contents.collection.path())?;
    let referenced = referenced_collection_media(contents.collection.path())?;
    contents
        .media
        .retain(|entry| media_is_referenced(entry, &referenced));
    let copied = copy_media_with_db(
        media_dir,
        &media_db_path(media_dir),
        &contents.media,
    )?;
    let media_count = contents.media.len();
    {
        let connection = Connection::open(contents.collection.path())
            .map_err(error::db_error("cannot open package collection"))?;
        rewrite_note_media(&connection, copied.renames())?;
    }

    let mut connection = schema::open_or_create(collection, Some(source_crt))?;
    let incoming = contents.collection.path().to_string_lossy().into_owned();
    connection
        .execute("ATTACH DATABASE ? AS incoming", params![incoming])
        .map_err(error::db_error("cannot open package collection"))?;

    let result = merge::merge_collection(&mut connection, &[], media_count);
    let detach = connection
        .execute_batch("DETACH DATABASE incoming")
        .map_err(error::db_error("cannot close package collection"));
    let report = result?;
    copied.keep();
    detach?;
    Ok(report)
}

fn package_creation_time(path: &Path) -> Result<i64, FormatError> {
    let connection = rusqlite::Connection::open(path)
        .map_err(error::db_error("cannot open package collection"))?;
    connection
        .query_row("SELECT crt FROM col WHERE id = 1", [], |row| row.get(0))
        .map_err(error::db_error("cannot read package creation time"))
}

type TextRef = (String, bool);

fn collection_text(
    connection: &Connection,
) -> Result<Vec<TextRef>, FormatError> {
    let mut statement = connection
        .prepare("SELECT flds FROM notes")
        .map_err(error::db_error("cannot read package notes"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(error::db_error("cannot read package notes"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error::db_error("cannot read package notes"))?;
    drop(statement);
    let mut result = rows
        .into_iter()
        .map(|text| (text, false))
        .collect::<Vec<_>>();
    result.extend(model_text(connection)?.into_iter().map(|text| (text, true)));
    Ok(result)
}

fn referenced_collection_media(
    collection: &Path,
) -> Result<Vec<String>, FormatError> {
    let connection = Connection::open(collection)
        .map_err(error::db_error("cannot open package collection"))?;
    let mut names = Vec::new();
    for (text, css) in collection_text(&connection)? {
        names.extend(media_reference_names(&text, css));
    }
    Ok(names)
}
fn media_is_referenced(entry: &MediaEntry, references: &[String]) -> bool {
    names_match(&entry.name, references)
}

fn names_match(name: &str, references: &[String]) -> bool {
    let normalized = sanitize_filename(name);
    let lowercase = normalized.to_lowercase();
    let variants = [
        name,
        normalized.as_str(),
        lowercase.as_str(),
        &percent_encode(name),
        &percent_encode(&normalized),
    ];
    references.iter().any(|reference| {
        variants.iter().any(|variant| {
            reference == *variant || reference.eq_ignore_ascii_case(variant)
        }) || (!is_remote_reference(reference)
            && sanitize_filename(reference) == normalized)
    })
}

fn is_remote_reference(reference: &str) -> bool {
    let lowercase = reference.to_ascii_lowercase();
    reference.contains("://")
        || reference.starts_with("//")
        || lowercase.starts_with("data:")
        || lowercase.starts_with("blob:")
}

fn rewrite_note_media(
    connection: &Connection,
    renames: &[MediaRename],
) -> Result<(), FormatError> {
    if renames.is_empty() {
        return Ok(());
    }
    let matcher = ReferenceMatcher::new(renames);
    let sort_fields = model_sort_indexes(connection)?;
    let mut statement =
        connection
            .prepare("SELECT id, mid, flds FROM notes")
            .map_err(error::db_error("cannot read package notes"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(error::db_error("cannot read package notes"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error::db_error("cannot read package notes"))?;
    drop(statement);
    let mut update = connection
        .prepare(
            "UPDATE notes SET flds = ?1, sfld = ?2, csum = ?3 WHERE id = ?4",
        )
        .map_err(error::db_error("cannot rewrite package notes"))?;
    for (id, model_id, fields) in rows {
        let rewritten = fields
            .split('\u{001f}')
            .map(|field| matcher.rewrite(field).into_owned())
            .collect::<Vec<_>>();
        if rewritten.join("\u{001f}") == fields {
            continue;
        }
        let first = rewritten.first().map_or("", String::as_str);
        let sort_index =
            sort_fields.get(&model_id).copied().unwrap_or_default();
        let sort_field =
            rewritten.get(sort_index).map_or_else(String::new, |field| {
                ranki_storage::strip_html_preserving_media_filenames(field)
            });
        let checksum = checksum_value(
            &ranki_storage::strip_html_preserving_media_filenames(first),
        );
        update
            .execute(params![
                rewritten.join("\u{001f}"),
                sort_field,
                checksum,
                id
            ])
            .map_err(error::db_error("cannot rewrite package notes"))?;
    }
    rewrite_models(connection, &matcher)?;
    Ok(())
}

#[expect(
    clippy::excessive_nesting,
    reason = "model metadata rewrite follows fixed nested JSON fields"
)]
fn rewrite_models(
    connection: &Connection,
    matcher: &ReferenceMatcher,
) -> Result<(), FormatError> {
    let raw: Option<String> = connection
        .query_row("SELECT models FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(error::db_error("cannot read package note types"))?;
    let Some(stored) = raw else {
        return rewrite_table_blobs(connection, matcher);
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&stored) else {
        return rewrite_table_blobs(connection, matcher);
    };
    let Some(models) = value.as_object_mut() else {
        return rewrite_table_blobs(connection, matcher);
    };
    for model in models.values_mut() {
        let css_text = model
            .get("css")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if let Some(current_css) = css_text {
            let rewritten = matcher.rewrite_css(&current_css);
            if rewritten.as_ref() != current_css
                && let Some(object) = model.as_object_mut()
            {
                let _ = object.insert(
                    "css".to_owned(),
                    Value::from(rewritten.into_owned()),
                );
            }
        }
        let template_values =
            model.get("tmpls").and_then(Value::as_array).cloned();
        if let Some(current_templates) = template_values {
            for (index, template) in current_templates.iter().enumerate() {
                for key in ["qfmt", "afmt", "bqfmt", "bafmt"] {
                    let template_text =
                        template.get(key).and_then(Value::as_str);
                    if let Some(current_text) = template_text {
                        let rewritten = matcher.rewrite_css(current_text);
                        if rewritten.as_ref() != current_text
                            && let Some(target) = model
                                .get_mut("tmpls")
                                .and_then(Value::as_array_mut)
                                .and_then(|items| items.get_mut(index))
                                .and_then(Value::as_object_mut)
                        {
                            let _ = target.insert(
                                key.to_owned(),
                                Value::from(rewritten.into_owned()),
                            );
                        }
                    }
                }
            }
        }
    }
    let encoded = serde_json::to_string(&value)
        .map_err(error::json_error("cannot encode note types"))?;
    if encoded != stored {
        connection
            .execute(
                "UPDATE col SET models = ?1 WHERE id = 1",
                params![encoded],
            )
            .map_err(error::db_error(
                "cannot rewrite note type media references",
            ))?;
    }
    rewrite_table_blobs(connection, matcher)
}

#[expect(
    clippy::excessive_nesting,
    reason = "config table rewrites follow a fixed shape"
)]
fn rewrite_table_blobs(
    connection: &Connection,
    matcher: &ReferenceMatcher,
) -> Result<(), FormatError> {
    let has_notetypes: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'notetypes')",
            [],
            |row| row.get(0),
        )
        .map_err(error::db_error("cannot inspect note type tables"))?;
    if has_notetypes {
        let rows = query_blobs(connection, "SELECT id, config FROM notetypes")?;
        for (keys, blob) in rows {
            let rewritten = rewrite_protobuf_strings(&blob, &[3], matcher);
            if rewritten != blob {
                connection
                    .execute(
                        "UPDATE notetypes SET config = ?1 WHERE id = ?2",
                        params![
                            rewritten,
                            keys.first().copied().unwrap_or_default()
                        ],
                    )
                    .map_err(error::db_error(
                        "cannot rewrite note type config",
                    ))?;
            }
        }
    }
    let has_templates: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'templates')",
            [],
            |row| row.get(0),
        )
        .map_err(error::db_error("cannot inspect template tables"))?;
    if has_templates {
        let rows =
            query_blobs(connection, "SELECT ntid, ord, config FROM templates")?;
        for (keys, blob) in rows {
            let rewritten = rewrite_protobuf_strings(&blob, &[1, 2], matcher);
            if rewritten != blob {
                connection
                    .execute(
                        "UPDATE templates SET config = ?1 WHERE ntid = ?2 AND ord = ?3",
                        params![
                            rewritten,
                            keys.first().copied().unwrap_or_default(),
                            keys.get(1).copied().unwrap_or_default()
                        ],
                    )
                    .map_err(error::db_error("cannot rewrite template config"))?;
            }
        }
    }
    Ok(())
}

type BlobRow = (Vec<i64>, Vec<u8>);

fn query_blobs(
    connection: &Connection,
    sql: &str,
) -> Result<Vec<BlobRow>, FormatError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(error::db_error("cannot read note type config"))?;
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            let keys = (0..columns.saturating_sub(1))
                .filter_map(|index| {
                    row.get::<_, Option<i64>>(index).ok().flatten()
                })
                .collect();
            let blob =
                match row.get::<_, SqlValue>(columns.saturating_sub(1))? {
                    SqlValue::Blob(bytes) => bytes,
                    SqlValue::Null
                    | SqlValue::Integer(_)
                    | SqlValue::Real(_)
                    | SqlValue::Text(_) => Vec::new(),
                };
            Ok((keys, blob))
        })
        .map_err(error::db_error("cannot read note type config"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error::db_error("cannot read note type config"))
}

#[expect(
    clippy::excessive_nesting,
    reason = "protobuf fields have a fixed set of wire types"
)]
fn rewrite_protobuf_strings(
    bytes: &[u8],
    fields: &[u32],
    matcher: &ReferenceMatcher,
) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some((tag, after_tag)) = read_protobuf_varint(bytes, cursor) else {
            return bytes.to_vec();
        };
        output.extend_from_slice(
            bytes.get(cursor..after_tag).unwrap_or_default(),
        );
        cursor = after_tag;
        let field = tag >> 3;
        match tag & 0x07 {
            0 => {
                let Some((_, next)) = read_protobuf_varint(bytes, cursor)
                else {
                    return bytes.to_vec();
                };
                output.extend_from_slice(
                    bytes.get(cursor..next).unwrap_or_default(),
                );
                cursor = next;
            }
            1 | 5 => {
                let width =
                    usize::from(tag & 0x07 == 1).checked_mul(8).unwrap_or(4);
                let Some(end) = cursor.checked_add(width) else {
                    return bytes.to_vec();
                };
                let Some(slice) = bytes.get(cursor..end) else {
                    return bytes.to_vec();
                };
                output.extend_from_slice(slice);
                cursor = end;
            }
            2 => {
                let Some((raw_length, after_length)) =
                    read_protobuf_varint(bytes, cursor)
                else {
                    return bytes.to_vec();
                };
                let Ok(length) = usize::try_from(raw_length) else {
                    return bytes.to_vec();
                };
                let Some(end) = after_length.checked_add(length) else {
                    return bytes.to_vec();
                };
                let Some(value) = bytes.get(after_length..end) else {
                    return bytes.to_vec();
                };
                if fields.contains(&u32::try_from(field).unwrap_or_default())
                    && let Ok(text) = str::from_utf8(value)
                {
                    let rewritten = matcher.rewrite_css(text);
                    append_protobuf_varint(&mut output, rewritten.len() as u64);
                    output.extend_from_slice(rewritten.as_bytes());
                } else {
                    output.extend_from_slice(
                        bytes.get(cursor..after_length).unwrap_or_default(),
                    );
                    output.extend_from_slice(value);
                }
                cursor = end;
            }
            _ => return bytes.to_vec(),
        }
    }
    output
}

fn read_protobuf_varint(
    bytes: &[u8],
    mut cursor: usize,
) -> Option<(u64, usize)> {
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

fn append_protobuf_varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        bytes.push(u8::try_from(value & 0x7F).unwrap_or_default() | 0x80);
        value >>= 7;
    }
    bytes.push(u8::try_from(value & 0x7F).unwrap_or_default());
}

#[expect(
    clippy::excessive_nesting,
    reason = "model metadata has a fixed set of representations"
)]
fn model_sort_indexes(
    connection: &Connection,
) -> Result<HashMap<i64, usize>, FormatError> {
    let mut indexes = HashMap::new();
    let raw: Option<String> = connection
        .query_row("SELECT models FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(error::db_error("cannot read package note types"))?;
    if let Some(stored) = raw
        && let Ok(Value::Object(object)) =
            serde_json::from_str::<Value>(&stored)
    {
        for (key, model) in object {
            if let Ok(id) = key.parse::<i64>()
                && let Some(index) = model.get("sortf").and_then(Value::as_i64)
                && index >= 0
            {
                let _ = indexes
                    .insert(id, usize::try_from(index).unwrap_or_default());
            }
        }
    }
    if !indexes.is_empty() {
        return Ok(indexes);
    }
    let exists: i64 = connection
        .query_row(
            "SELECT (EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'notetypes'))
             + (EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'fields'))
             + (EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'templates'))",
            [],
            |row| row.get(0),
        )
        .map_err(error::db_error("cannot inspect package note types"))?;
    if exists == 3
        && let Ok(models) =
            ranki_storage::models_from_tables_with_prefix(connection, "main")
    {
        for model in models {
            let _ =
                indexes.insert(model.id, usize::from(model.sort_field_index));
        }
    }
    Ok(indexes)
}

fn checksum_value(field: &str) -> i64 {
    let digest = sha1_bytes(field.as_bytes());
    let unsigned = u32::from_be_bytes([
        digest.first().copied().unwrap_or_default(),
        digest.get(1).copied().unwrap_or_default(),
        digest.get(2).copied().unwrap_or_default(),
        digest.get(3).copied().unwrap_or_default(),
    ]);
    i64::from(unsigned)
}

/// Export a collection into an `.apkg` file Anki can import.
///
/// # Errors
///
/// Returns an error when the collection cannot be read, media cannot be read,
/// or the destination package cannot be written.
pub fn export_apkg(
    collection: &Path,
    media_dir: &Path,
    destination: &Path,
) -> Result<(), FormatError> {
    export_apkg_with_options(
        collection,
        media_dir,
        destination,
        ExportOptions::default(),
    )
}

/// Export a collection with Anki's common package choices.
///
/// # Errors
///
/// Returns an error when the collection or media cannot be read, or the
/// package cannot be written.
#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, destination, and options are separate inputs"
)]
pub fn export_apkg_with_options(
    collection: &Path,
    media_dir: &Path,
    destination: &Path,
    options: ExportOptions,
) -> Result<(), FormatError> {
    export_package(collection, media_dir, destination, options, None)
}

/// Export one deck and its subdecks into an `.apkg` file Anki can import.
///
/// # Errors
///
/// Returns an error when the deck does not exist, the collection cannot be
/// read, or the destination package cannot be written.
#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, destination, and deck id are separate inputs"
)]
pub fn export_deck_apkg(
    collection: &Path,
    media_dir: &Path,
    destination: &Path,
    deck_id: Id,
) -> Result<(), FormatError> {
    export_deck_apkg_with_options(
        collection,
        media_dir,
        destination,
        deck_id,
        ExportOptions::default(),
    )
}

/// Export one deck and its subdecks with Anki's common package choices.
///
/// # Errors
///
/// Returns an error when the deck is missing or the package cannot be written.
#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, destination, deck id, and options are separate inputs"
)]
pub fn export_deck_apkg_with_options(
    collection: &Path,
    media_dir: &Path,
    destination: &Path,
    deck_id: Id,
    options: ExportOptions,
) -> Result<(), FormatError> {
    export_package(collection, media_dir, destination, options, Some(deck_id))
}

#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, destination, options, and deck id are separate inputs"
)]
fn export_package(
    collection: &Path,
    media_dir: &Path,
    destination: &Path,
    options: ExportOptions,
    deck_id: Option<Id>,
) -> Result<(), FormatError> {
    let snapshot = deck_snapshot(collection)?;
    if let Some(id) = deck_id {
        let subtree = deck_subtree(snapshot.path(), id)?;
        if subtree.is_empty() {
            return Err(FormatError::new(format!("deck does not exist: {id}")));
        }
        let connection = Connection::open(snapshot.path())
            .map_err(error::db_error("cannot open export snapshot"))?;
        ranki_storage::register_collations(&connection).map_err(|error| {
            FormatError::with_source("cannot register export collations", error)
        })?;
        filter_records(&connection, &subtree)?;
        filter_deck_metadata(&connection, &subtree)?;
        drop(connection);
    }
    let connection = Connection::open(snapshot.path())
        .map_err(error::db_error("cannot open export snapshot"))?;
    ranki_storage::register_collations(&connection).map_err(|error| {
        FormatError::with_source("cannot register export collations", error)
    })?;
    validate_collection_ids(&connection)?;
    package::normalize_export_collection(
        &connection,
        options.with_scheduling,
        options.with_deck_configs,
        true,
        false,
    )?;
    let media = if options.with_media {
        let mut names = Vec::new();
        for (text, css) in collection_text(&connection)? {
            names.extend(media_reference_names(&text, css));
        }
        media::collect_media_files(media_dir)?
            .into_iter()
            .filter(|file| media_is_referenced_file(file, &names))
            .collect()
    } else {
        Vec::new()
    };
    let schema = package::collection_schema_version(snapshot.path())?;
    let version = choose_version(schema, options.legacy)?;
    drop(connection);
    package::write_apkg_bytes_with_version(
        &fs::read(snapshot.path())
            .map_err(error::io_error("cannot read export snapshot"))?,
        &media,
        destination,
        version,
        options.overwrite,
    )
}

fn media_is_referenced_file(file: &MediaFile, references: &[String]) -> bool {
    names_match(&file.name, references)
}

fn deck_snapshot(collection: &Path) -> Result<NamedTempFile, FormatError> {
    let bytes = package::snapshot_collection(collection)?;
    let parent = collection
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut snapshot = Builder::new()
        .prefix(".ranki-deck-")
        .suffix(".anki2")
        .tempfile_in(parent)
        .map_err(error::io_error("cannot create deck export snapshot"))?;
    snapshot
        .write_all(&bytes)
        .map_err(error::io_error("cannot write deck export snapshot"))?;
    snapshot
        .flush()
        .map_err(error::io_error("cannot flush deck export snapshot"))?;
    Ok(snapshot)
}

fn deck_subtree(path: &Path, deck_id: Id) -> Result<Vec<Id>, FormatError> {
    let store = Collection::open(path).map_err(|error| {
        FormatError::with_source("cannot read deck export snapshot", error)
    })?;
    store.deck_subtree(deck_id).map_err(|error| {
        FormatError::with_source("cannot read exported deck", error)
    })
}

fn filter_records(
    connection: &Connection,
    subtree: &[Id],
) -> Result<(), FormatError> {
    let list = placeholders(subtree.len());
    connection
        .execute(
            &format!("DELETE FROM cards WHERE did NOT IN ({list})"),
            params_from_iter(subtree.iter().copied()),
        )
        .map_err(error::db_error("cannot filter exported cards"))?;
    connection
        .execute_batch(
            "DELETE FROM revlog WHERE cid NOT IN (SELECT id FROM cards);
             DELETE FROM notes WHERE id NOT IN (SELECT nid FROM cards);
             DELETE FROM graves;",
        )
        .map_err(error::db_error("cannot filter exported records"))?;
    connection
        .execute(
            &format!("UPDATE cards SET odid = 0 WHERE odid NOT IN ({list})"),
            params_from_iter(subtree.iter().copied()),
        )
        .map_err(error::db_error("cannot filter exported card decks"))?;

    let has_deck_table: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'decks')",
            [],
            |row| row.get(0),
        )
        .map_err(error::db_error("cannot inspect exported decks"))?;
    if has_deck_table {
        connection
            .execute(
                &format!("DELETE FROM decks WHERE id NOT IN ({list})"),
                params_from_iter(subtree.iter().copied()),
            )
            .map_err(error::db_error("cannot filter exported decks"))?;
    }
    Ok(())
}

fn filter_deck_metadata(
    connection: &Connection,
    subtree: &[Id],
) -> Result<(), FormatError> {
    let raw_metadata: Option<String> = connection
        .query_row("SELECT decks FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(error::db_error("cannot read exported deck metadata"))?;
    let Some(stored) = raw_metadata.filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let mut value: Value = serde_json::from_str(&stored)
        .map_err(error::json_error("cannot read exported deck metadata"))?;
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    object.retain(|key, _| {
        key.parse::<Id>().is_ok_and(|id| subtree.contains(&id))
    });
    let filtered = serde_json::to_string(&value)
        .map_err(error::json_error("cannot write exported deck metadata"))?;
    if filtered == stored {
        return Ok(());
    }
    connection
        .execute("UPDATE col SET decks = ?1 WHERE id = 1", params![filtered])
        .map_err(error::db_error("cannot write exported deck metadata"))?;
    Ok(())
}

fn placeholders(count: usize) -> String {
    (0..count).map(|_| "?").collect::<Vec<_>>().join(", ")
}

#[expect(
    clippy::excessive_nesting,
    reason = "legacy models have a fixed nested shape"
)]
fn model_text(connection: &Connection) -> Result<Vec<String>, FormatError> {
    let raw_models: Option<String> = connection
        .query_row("SELECT models FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(error::db_error("cannot read note types"))?;
    let Some(stored) = raw_models.filter(|value| !value.trim().is_empty())
    else {
        return Ok(table_model_text(connection).unwrap_or_default());
    };
    if stored.trim() == "{}" {
        return Ok(table_model_text(connection).unwrap_or_default());
    }
    let mut texts = Vec::new();
    if let Ok(Value::Object(models)) = serde_json::from_str::<Value>(&stored) {
        for model in models.values() {
            if let Some(css) = model.get("css").and_then(Value::as_str) {
                texts.push(css.to_owned());
            }
            if let Some(templates) =
                model.get("tmpls").and_then(Value::as_array)
            {
                for template in templates {
                    for key in ["qfmt", "afmt", "bqfmt", "bafmt"] {
                        if let Some(text) =
                            template.get(key).and_then(Value::as_str)
                        {
                            texts.push(text.to_owned());
                        }
                    }
                }
            }
        }
    }
    Ok(texts)
}

fn table_model_text(
    connection: &Connection,
) -> Result<Vec<String>, FormatError> {
    let mut has_tables = true;
    for table in ["notetypes", "fields", "templates"] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![table],
                |row| row.get(0),
            )
            .map_err(error::db_error("cannot inspect note type tables"))?;
        has_tables &= exists;
    }
    if !has_tables {
        return Ok(Vec::new());
    }
    let models =
        ranki_storage::models_from_tables_with_prefix(connection, "main")
            .map_err(|error| {
                FormatError::with_source(
                    "cannot read exported note types",
                    error,
                )
            })?;
    let mut texts = Vec::new();
    for model in models {
        texts.push(model.css);
        for template in model.templates {
            texts.push(template.question_format);
            texts.push(template.answer_format);
        }
    }
    Ok(texts)
}

/// Export a collection into a `.colpkg` backup Anki can import.
///
/// # Errors
///
/// Returns an error when the collection or media cannot be read, or the
/// destination package cannot be written.
pub fn export_colpkg(
    collection: &Path,
    media_dir: &Path,
    destination: &Path,
) -> Result<(), FormatError> {
    export_colpkg_with_options(
        collection,
        media_dir,
        destination,
        ExportOptions::default(),
    )
}

/// Export a backup with Anki's common package choices.
///
/// # Errors
///
/// Returns an error when the collection or media cannot be read, or the
/// package cannot be written.
#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, destination, and options are separate inputs"
)]
pub fn export_colpkg_with_options(
    collection: &Path,
    media_dir: &Path,
    destination: &Path,
    options: ExportOptions,
) -> Result<(), FormatError> {
    let snapshot = deck_snapshot(collection)?;
    let version = choose_version(
        package::collection_schema_version(snapshot.path())?,
        options.legacy,
    )?;
    let connection = Connection::open(snapshot.path())
        .map_err(error::db_error("cannot open backup snapshot"))?;
    validate_collection_ids(&connection)?;
    if !options.with_scheduling || !options.with_deck_configs {
        ranki_storage::register_collations(&connection).map_err(|error| {
            FormatError::with_source("cannot register backup collations", error)
        })?;
        package::normalize_export_collection(
            &connection,
            options.with_scheduling,
            options.with_deck_configs,
            false,
            options.with_scheduling && !options.with_deck_configs,
        )?;
    }
    let media = if options.with_media {
        media::collect_media_files(media_dir)?
    } else {
        Vec::new()
    };
    drop(connection);
    package::write_apkg_bytes_with_version(
        &fs::read(snapshot.path())
            .map_err(error::io_error("cannot read export snapshot"))?,
        &media,
        destination,
        version,
        options.overwrite,
    )
}

/// Extract a `.colpkg` backup without overwriting existing files.
///
/// # Errors
///
/// Returns an error when the package is malformed or the destination already
/// contains a collection file.
pub fn import_colpkg(
    imported_package: &Path,
    destination_dir: &Path,
) -> Result<PathBuf, FormatError> {
    package::extract_colpkg(imported_package, destination_dir)
}

/// Replace the target collection from a `.colpkg` backup.
///
/// A copy of the existing collection is written beside it before the restore.
/// Media is restored with Anki's filename rules and recorded in the media DB.
///
/// # Errors
///
/// Returns an error when the package is invalid or the collection cannot be
/// replaced.
pub fn restore_colpkg(
    package: &Path,
    collection: &Path,
    media_dir: &Path,
) -> Result<RestoreReport, FormatError> {
    schema::ensure_parent(collection)?;
    let scratch_dir = collection
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let contents = package::read_apkg(package, scratch_dir)?;
    validate_restored_collection(contents.collection.path())?;
    let copied = copy_media_with_db(
        media_dir,
        &media_db_path(media_dir),
        &contents.media,
    )?;
    {
        let connection = Connection::open(contents.collection.path())
            .map_err(error::db_error("cannot open restored collection"))?;
        rewrite_note_media(&connection, copied.renames())?;
    }

    let backup = if collection.is_file() {
        let backup = backup_path(collection);
        package::backup_collection(collection, &backup)?;
        Some(backup)
    } else {
        None
    };
    let parent = collection
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = Builder::new()
        .prefix(".ranki-restore-")
        .suffix(".anki2")
        .tempfile_in(parent)
        .map_err(error::io_error("cannot create restored collection"))?;
    temporary
        .write_all(
            &fs::read(contents.collection.path())
                .map_err(error::io_error("cannot read restored collection"))?,
        )
        .map_err(error::io_error("cannot write restored collection"))?;
    temporary
        .flush()
        .map_err(error::io_error("cannot flush restored collection"))?;
    temporary.persist(collection).map_err(|error| {
        FormatError::with_source(
            format!("cannot replace collection {}", collection.display()),
            error.error,
        )
    })?;
    copied.keep();
    Ok(RestoreReport {
        media: contents.media.len(),
        backup,
    })
}

fn backup_path(collection: &Path) -> PathBuf {
    let first = collection.with_extension("pre-restore.anki2");
    if !first.exists() {
        return first;
    }
    let mut counter = 1_u32;
    loop {
        let candidate =
            collection.with_extension(format!("pre-restore-{counter}.anki2"));
        if !candidate.exists() {
            return candidate;
        }
        counter = counter.saturating_add(1);
    }
}

fn validate_restored_collection(path: &Path) -> Result<(), FormatError> {
    let version = package::collection_schema_version(path)?;
    choose_version(version, false)?;
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(error::db_error("cannot open restored collection"))?;
    ranki_storage::register_collations(&connection).map_err(|error| {
        FormatError::with_source("cannot register restore collations", error)
    })?;
    require_columns(
        &connection,
        "col",
        &[
            "id", "crt", "mod", "scm", "ver", "dty", "usn", "ls", "conf",
            "models", "decks", "dconf", "tags",
        ],
    )?;
    require_columns(
        &connection,
        "notes",
        &[
            "id", "guid", "mid", "mod", "usn", "tags", "flds", "sfld", "csum",
            "flags", "data",
        ],
    )?;
    require_columns(
        &connection,
        "cards",
        &[
            "id", "nid", "did", "ord", "mod", "usn", "type", "queue", "due",
            "ivl", "factor", "reps", "lapses", "left", "odue", "odid", "flags",
            "data",
        ],
    )?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(error::db_error("cannot validate restored collection"))?;
    if integrity != "ok" {
        return Err(FormatError::new(
            "restored collection failed integrity check",
        ));
    }
    validate_collection_ids(&connection)?;
    Ok(())
}

fn require_columns(
    connection: &Connection,
    table: &str,
    columns: &[&str],
) -> Result<(), FormatError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(error::db_error("cannot inspect restored collection"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(error::db_error("cannot inspect restored collection"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error::db_error("cannot inspect restored collection"))?;
    for column in columns {
        if !rows.iter().any(|name| name == column) {
            return Err(FormatError::new(format!(
                "restored collection is missing {table}.{column}"
            )));
        }
    }
    Ok(())
}
