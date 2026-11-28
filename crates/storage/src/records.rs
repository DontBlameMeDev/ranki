#![expect(
    clippy::collapsible_if,
    clippy::excessive_nesting,
    clippy::explicit_iter_loop,
    clippy::redundant_pub_crate,
    clippy::shadow_reuse,
    clippy::too_many_arguments,
    clippy::useless_conversion,
    clippy::wildcard_enum_match_arm,
    reason = "model conversion follows Anki JSON and protobuf layouts"
)]
//! Conversion between Anki rows and the shared model types.

use std::collections::BTreeMap;

use crate::anki;
use ranki_model::Card;
use ranki_model::CardQueue;
use ranki_model::CardTemplate;
use ranki_model::CardType;
use ranki_model::Deck;
use ranki_model::Id;
use ranki_model::Note;
use ranki_model::NoteKind;
use ranki_model::NoteModel;
use ranki_model::RevLog;
use ranki_model::RevLogKind;
use ranki_model::StoreError;
use ranki_model::decode_hex;
use ranki_model::encode_hex;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Result as SqlResult;
use rusqlite::Row;
use rusqlite::types::Type;
use rusqlite::types::Value;
use serde_json::Map;
use serde_json::Value as Json;
use serde_json::json;

pub struct ColData {
    pub modified: i64,
    pub scm: i64,
    pub ver: i64,
    pub conf: String,
    pub dconf: String,
    pub models: String,
    pub decks: String,
}

pub fn read_col(conn: &Connection) -> Result<ColData, StoreError> {
    conn.query_row(
        "SELECT mod, scm, ver, conf, dconf, models, decks FROM col WHERE id = 1",
        [],
        |row| {
            Ok(ColData {
                modified: row.get(0)?,
                scm: row.get(1)?,
                ver: row.get(2)?,
                conf: text(row.get(3)?),
                dconf: text(row.get(4)?),
                models: text(row.get(5)?),
                decks: text(row.get(6)?),
            })
        },
    )
    .map_err(crate::backend)
}

/// Reads one `col` text column without loading the other JSON columns.
fn read_col_column(
    conn: &Connection,
    column: &str,
) -> Result<String, StoreError> {
    let sql = match column {
        "decks" => "SELECT decks FROM col WHERE id = 1",
        "models" => "SELECT models FROM col WHERE id = 1",
        _ => return Err(crate::invalid("invalid col column")),
    };
    conn.query_row(sql, [], |row| Ok(text(row.get::<_, Value>(0)?)))
        .map_err(crate::backend)
}

pub fn read_col_decks(conn: &Connection) -> Result<String, StoreError> {
    read_col_column(conn, "decks")
}

pub fn card_from_row(row: &Row<'_>) -> SqlResult<Card> {
    let ordinal = anki::as_u16(row.get(3)?).map_err(conversion)?;
    let factor = anki::as_u32(row.get(10)?).map_err(conversion)?;
    let reps = anki::as_u32(row.get(11)?).map_err(conversion)?;
    let lapses = anki::as_u32(row.get(12)?).map_err(conversion)?;
    let left = anki::as_u32(row.get(13)?).map_err(conversion)?;
    Ok(Card {
        id: row.get(0)?,
        note_id: row.get(1)?,
        deck_id: row.get(2)?,
        ordinal,
        modified: row.get(4)?,
        usn: i32::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
        card_type: CardType::from_code(row.get(6)?),
        queue: CardQueue::from_code(row.get(7)?),
        due: row.get(8)?,
        interval: i32::try_from(row.get::<_, i64>(9)?).unwrap_or(i32::MAX),
        factor,
        reps,
        lapses,
        left,
        original_due: row.get(14)?,
        original_deck_id: row.get(15)?,
        flags: i32::try_from(row.get::<_, i64>(16)?).unwrap_or(0),
        data: text(row.get(17)?),
    })
}

pub fn note_from_row(row: &Row<'_>) -> SqlResult<Note> {
    let fields = text(row.get::<_, Value>(6)?);
    let raw = row.get::<_, i64>(8)?;
    let checksum = u32::try_from(raw & i64::from(u32::MAX)).unwrap_or(0);
    Ok(Note {
        id: row.get(0)?,
        guid: text(row.get(1)?),
        model_id: row.get(2)?,
        modified: row.get(3)?,
        usn: i32::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
        tags: anki::decode_tags(&text(row.get::<_, Value>(5)?)),
        fields: anki::decode_fields(&fields),
        sort_field: text(row.get::<_, Value>(7)?),
        checksum: anki::signed_i32(checksum),
        flags: i32::try_from(row.get::<_, i64>(9)?).unwrap_or(0),
        data: text(row.get(10)?),
    })
}

pub fn revlog_from_row(row: &Row<'_>) -> SqlResult<RevLog> {
    let ease = u8::try_from(row.get::<_, i64>(3)?).unwrap_or(0);
    let kind = u8::try_from(row.get::<_, i64>(8)?).unwrap_or(0);
    Ok(RevLog {
        id: row.get(0)?,
        card_id: row.get(1)?,
        usn: i32::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
        ease,
        interval: i32::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
        last_interval: i32::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
        factor: anki::as_u32(row.get(6)?).map_err(conversion)?,
        time_ms: anki::as_u32(row.get(7)?).map_err(conversion)?,
        kind: RevLogKind::from_code(kind),
    })
}

pub fn decks_from_json(json: &str) -> Result<Vec<Deck>, StoreError> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Json = serde_json::from_str(json)
        .map_err(|error| crate::invalid(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| crate::invalid("decks is not an object"))?;
    let mut decks = Vec::with_capacity(object.len());
    for (key, value) in object {
        let id = key
            .parse::<Id>()
            .map_err(|_| crate::invalid("invalid deck id"))?;
        decks.push(Deck {
            id,
            name: value
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .into(),
            description: value
                .get("desc")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .into(),
        });
    }
    decks.sort_unstable_by_key(|deck| deck.id);
    Ok(decks)
}

fn ordinal_at(entry: (usize, &Json)) -> u16 {
    template_ordinal(entry.1, entry.0)
}

fn template_ordinal(template: &Json, position: usize) -> u16 {
    template
        .get("ord")
        .and_then(Json::as_u64)
        .and_then(|ord| u16::try_from(ord).ok())
        .unwrap_or_else(|| u16::try_from(position).unwrap_or_default())
}

/// Reads note types from schema-11 JSON.
///
/// # Errors
///
/// Returns an error when the JSON is malformed or a note type id is invalid.
pub fn models_from_json(json: &str) -> Result<Vec<NoteModel>, StoreError> {
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Json = serde_json::from_str(json)
        .map_err(|error| crate::invalid(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| crate::invalid("models is not an object"))?;
    let mut models = Vec::with_capacity(object.len());
    for (key, value) in object {
        models.push(model_from_json(key, value)?);
    }
    models.sort_unstable_by_key(|model| model.id);
    Ok(models)
}

fn model_from_json(key: &str, value: &Json) -> Result<NoteModel, StoreError> {
    let id = key
        .parse::<Id>()
        .map_err(|_| crate::invalid("invalid model id"))?;
    let templates = value.get("tmpls").and_then(Json::as_array);
    let kind = NoteKind::from_code(
        value
            .get("type")
            .and_then(Json::as_i64)
            .and_then(|code| i32::try_from(code).ok())
            .unwrap_or_default(),
    );
    let fields = string_array(value.get("flds"), "name");
    let field_tags = optional_numbers(value.get("flds"), "tag")
        .into_iter()
        .map(|tag| u32::try_from(tag).ok())
        .collect();
    let template_ordinals = templates.map_or_else(Vec::new, |items| {
        items.iter().enumerate().map(ordinal_at).collect()
    });
    let raw_config = value
        .get("_rankiRawConfig")
        .and_then(Json::as_str)
        .and_then(decode_hex);
    let mut model = NoteModel {
        id,
        name: value
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .into(),
        fields,
        templates: templates.map_or_else(Vec::new, |items| {
            items
                .iter()
                .enumerate()
                .map(|(position, template)| CardTemplate {
                    name: template
                        .get("name")
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                        .into(),
                    question_format: template
                        .get("qfmt")
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                        .into(),
                    answer_format: template
                        .get("afmt")
                        .and_then(Json::as_str)
                        .unwrap_or_default()
                        .into(),
                    ord: template_ordinal(template, position),
                })
                .collect()
        }),
        css: value
            .get("css")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .into(),
        sort_field_index: value
            .get("sortf")
            .and_then(Json::as_u64)
            .and_then(|index| u16::try_from(index).ok())
            .unwrap_or_default(),
        kind,
        original_stock_kind: value
            .get("originalStockKind")
            .and_then(Json::as_i64)
            .and_then(|kind| i32::try_from(kind).ok()),
        field_tags,
        template_ordinals,
        raw_config,
        raw_json: serde_json::to_string(value).ok(),
    };
    infer_cloze_tags(&mut model);
    Ok(model)
}

pub fn decks_from_tables(conn: &Connection) -> Result<Vec<Deck>, StoreError> {
    let mut statement = conn
        .prepare("SELECT id, name, common FROM decks")
        .map_err(crate::backend)?;
    let rows = statement
        .query_map([], |row| {
            let blob: Value = row.get(2)?;
            let description = match blob {
                Value::Blob(bytes) => {
                    anki::protobuf_string(&bytes, 4).unwrap_or_default()
                }
                Value::Null
                | Value::Integer(_)
                | Value::Real(_)
                | Value::Text(_) => String::new(),
            };
            Ok(Deck {
                id: row.get(0)?,
                name: anki::decode_deck_name(&text(row.get::<_, Value>(1)?)),
                description,
            })
        })
        .map_err(crate::backend)?;
    let mut decks = Vec::new();
    for row in rows {
        decks.push(row.map_err(crate::backend)?);
    }
    decks.sort_unstable_by_key(|deck| deck.id);
    Ok(decks)
}

fn config_blob(value: &Value) -> Vec<u8> {
    match value {
        Value::Blob(bytes) => bytes.clone(),
        _ => Vec::new(),
    }
}

fn proto_bool(bytes: &[u8], field: u32) -> bool {
    anki::protobuf_varint(bytes, field).unwrap_or_default() != 0
}

fn proto_u32(bytes: &[u8], field: u32) -> Option<u32> {
    anki::protobuf_varint(bytes, field)
        .and_then(|value| u32::try_from(value).ok())
}

fn proto_i64(bytes: &[u8], field: u32) -> Option<i64> {
    anki::protobuf_varint(bytes, field)
        .and_then(|value| i64::try_from(value).ok())
}

fn other_hex(bytes: &[u8]) -> String {
    encode_hex(&anki::protobuf_bytes(bytes, 255).unwrap_or_default())
}

fn field_json(ord: usize, name: &str, config: &[u8]) -> Json {
    json!({
        "name": name,
        "ord": ord,
        "sticky": proto_bool(config, 1),
        "rtl": proto_bool(config, 2),
        "font": anki::protobuf_string(config, 3).unwrap_or_else(|| "Arial".into()),
        "size": proto_u32(config, 4).unwrap_or(20),
        "description": anki::protobuf_string(config, 5).unwrap_or_default(),
        "plainText": proto_bool(config, 6),
        "collapsed": proto_bool(config, 7),
        "excludeFromSearch": proto_bool(config, 8),
        "id": proto_i64(config, 9),
        "tag": proto_u32(config, 10),
        "preventDeletion": proto_bool(config, 11),
        "_rankiOther": other_hex(config),
    })
}

fn template_json(
    ord: u16,
    name: &str,
    mtime_secs: i64,
    usn: i32,
    config: &[u8],
) -> Json {
    json!({
        "name": name,
        "ord": ord,
        "qfmt": anki::protobuf_string(config, 1).unwrap_or_default(),
        "afmt": anki::protobuf_string(config, 2).unwrap_or_default(),
        "bqfmt": anki::protobuf_string(config, 3).unwrap_or_default(),
        "bafmt": anki::protobuf_string(config, 4).unwrap_or_default(),
        "did": proto_i64(config, 5),
        "bfont": anki::protobuf_string(config, 6).unwrap_or_default(),
        "bsize": proto_u32(config, 7).unwrap_or_default(),
        "id": proto_i64(config, 8),
        "mod": mtime_secs,
        "usn": usn,
        "_rankiOther": other_hex(config),
    })
}

fn reqs_json(config: &[u8]) -> Json {
    let kinds = ["none", "any", "all"];
    let reqs = anki::protobuf_messages(config, 8)
        .iter()
        .map(|message| {
            let kind =
                usize::try_from(proto_u32(message, 2).unwrap_or_default())
                    .unwrap_or_default();
            let fields = anki::protobuf_varints(message, 3)
                .into_iter()
                .filter_map(|value| u32::try_from(value).ok())
                .collect::<Vec<_>>();
            json!([
                proto_u32(message, 1).unwrap_or_default(),
                kinds.get(kind).copied().unwrap_or("none"),
                fields
            ])
        })
        .collect::<Vec<_>>();
    Json::Array(reqs)
}

fn model_json(
    id: Id,
    name: &str,
    mtime_secs: i64,
    usn: i32,
    config: &[u8],
) -> Json {
    json!({
        "id": id,
        "name": name,
        "type": proto_u32(config, 1).unwrap_or_default(),
        "mod": mtime_secs,
        "usn": usn,
        "sortf": proto_u32(config, 2).unwrap_or_default(),
        "did": proto_i64(config, 4).unwrap_or(1),
        "css": anki::protobuf_string(config, 3).unwrap_or_default(),
        "latexPre": anki::protobuf_string(config, 5).unwrap_or_default(),
        "latexPost": anki::protobuf_string(config, 6).unwrap_or_default(),
        "latexSvg": proto_bool(config, 7),
        "req": reqs_json(config),
        "flds": [],
        "tmpls": [],
        "originalStockKind": proto_u32(config, 9),
        "originalId": proto_i64(config, 10),
        "_rankiRawConfig": other_hex(config),
    })
}

/// Reads schema-18 note types from normalized tables.
pub fn models_from_tables(
    conn: &Connection,
) -> Result<Vec<NoteModel>, StoreError> {
    models_from_tables_with_prefix(conn, "main")
}

/// Reads schema-18 note types from a named database prefix.
/// Import passes `incoming` for the attached package collection.
///
/// # Errors
///
/// Returns an error when the prefix is invalid or the tables cannot be read.
pub fn models_from_tables_with_prefix(
    conn: &Connection,
    prefix: &str,
) -> Result<Vec<NoteModel>, StoreError> {
    if !matches!(prefix, "main" | "incoming") {
        return Err(crate::invalid("invalid table prefix"));
    }
    let mut models = BTreeMap::new();
    let mut raw_json = BTreeMap::new();
    {
        let sql = format!(
            "SELECT id, name, mtime_secs, usn, config FROM {prefix}.notetypes"
        );
        let mut statement = conn.prepare(&sql).map_err(crate::backend)?;
        let rows = statement
            .query_map([], |row| {
                let id = row.get::<_, i64>(0)?;
                let name = text(row.get::<_, Value>(1)?);
                let mtime_secs = row.get::<_, i64>(2)?;
                let usn =
                    i32::try_from(row.get::<_, i64>(3)?).unwrap_or_default();
                let config = config_blob(&row.get::<_, Value>(4)?);
                let kind = proto_u32(&config, 1).unwrap_or_default();
                let sort = proto_u32(&config, 2).unwrap_or_default();
                let stock = proto_u32(&config, 9);
                let css = anki::protobuf_string(&config, 3).unwrap_or_default();
                let other =
                    anki::protobuf_bytes(&config, 255).unwrap_or_default();
                let entry = model_json(id, &name, mtime_secs, usn, &config);
                Ok((
                    id,
                    NoteModel {
                        id,
                        name,
                        fields: Vec::new(),
                        templates: Vec::new(),
                        css,
                        sort_field_index: u16::try_from(sort)
                            .unwrap_or_default(),
                        kind: NoteKind::from_code(
                            i32::try_from(kind).unwrap_or_default(),
                        ),
                        original_stock_kind: stock
                            .and_then(|kind| i32::try_from(kind).ok()),
                        field_tags: Vec::new(),
                        template_ordinals: Vec::new(),
                        raw_config: Some(other),
                        raw_json: None,
                    },
                    entry,
                ))
            })
            .map_err(crate::backend)?;
        for row in rows {
            let (id, model, entry) = row.map_err(crate::backend)?;
            models.insert(id, model);
            raw_json.insert(id, entry);
        }
    }
    add_fields(conn, prefix, &mut models, &mut raw_json)?;
    add_templates(conn, prefix, &mut models, &mut raw_json)?;
    for (id, model) in models.iter_mut() {
        if let Some(entry) = raw_json.get(id) {
            model.raw_json = serde_json::to_string(entry).ok();
        }
        infer_cloze_tags(model);
    }
    Ok(models.into_values().collect())
}

fn add_fields(
    conn: &Connection,
    prefix: &str,
    models: &mut BTreeMap<i64, NoteModel>,
    raw_json: &mut BTreeMap<i64, Json>,
) -> Result<(), StoreError> {
    let sql = format!(
        "SELECT ntid, ord, name, config FROM {prefix}.fields ORDER BY ntid, ord"
    );
    let mut statement = conn.prepare(&sql).map_err(crate::backend)?;
    let rows = statement
        .query_map([], |row| {
            let config = config_blob(&row.get::<_, Value>(3)?);
            Ok((
                row.get::<_, i64>(0)?,
                usize::try_from(row.get::<_, i64>(1)?).unwrap_or_default(),
                text(row.get::<_, Value>(2)?),
                config,
            ))
        })
        .map_err(crate::backend)?;
    for row in rows {
        let (id, ord, name, config) = row.map_err(crate::backend)?;
        let tag = proto_u32(&config, 10);
        if let Some(model) = models.get_mut(&id) {
            model.fields.push(name.clone());
            model
                .field_tags
                .push(tag.and_then(|tag| u32::try_from(tag).ok()));
        }
        if let Some(entry) = raw_json.get_mut(&id).and_then(Json::as_object_mut)
        {
            if let Some(fields) =
                entry.get_mut("flds").and_then(Json::as_array_mut)
            {
                fields.push(field_json(ord, &name, &config));
            }
        }
    }
    Ok(())
}

fn add_templates(
    conn: &Connection,
    prefix: &str,
    models: &mut BTreeMap<i64, NoteModel>,
    raw_json: &mut BTreeMap<i64, Json>,
) -> Result<(), StoreError> {
    let sql = format!(
        "SELECT ntid, ord, name, mtime_secs, usn, config FROM {prefix}.templates ORDER BY ntid, ord"
    );
    let mut statement = conn.prepare(&sql).map_err(crate::backend)?;
    let rows = statement
        .query_map([], |row| {
            let config = config_blob(&row.get::<_, Value>(5)?);
            Ok((
                row.get::<_, i64>(0)?,
                u16::try_from(row.get::<_, i64>(1)?).unwrap_or_default(),
                text(row.get::<_, Value>(2)?),
                row.get::<_, i64>(3)?,
                i32::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
                config,
            ))
        })
        .map_err(crate::backend)?;
    for row in rows {
        let (id, ord, name, mtime, usn, config) =
            row.map_err(crate::backend)?;
        let question = anki::protobuf_string(&config, 1).unwrap_or_default();
        let answer = anki::protobuf_string(&config, 2).unwrap_or_default();
        if let Some(model) = models.get_mut(&id) {
            model.templates.push(CardTemplate {
                name: name.clone(),
                question_format: question,
                answer_format: answer,
                ord,
            });
            model.template_ordinals.push(ord);
        }
        if let Some(entry) = raw_json.get_mut(&id).and_then(Json::as_object_mut)
        {
            if let Some(templates) =
                entry.get_mut("tmpls").and_then(Json::as_array_mut)
            {
                templates.push(template_json(ord, &name, mtime, usn, &config));
            }
        }
    }
    Ok(())
}

fn raw_entry(model: &NoteModel) -> Option<Json> {
    model
        .raw_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
}

fn json_bool(entry: &Json, key: &str) -> bool {
    entry.get(key).and_then(Json::as_bool).unwrap_or_default()
}

fn json_u64(entry: &Json, key: &str) -> Option<u64> {
    entry.get(key).and_then(Json::as_u64)
}

fn json_string(entry: &Json, key: &str) -> Option<String> {
    entry.get(key).and_then(Json::as_str).map(ToOwned::to_owned)
}

fn put_varint_u64(bytes: &mut Vec<u8>, field: u32, value: u64) {
    anki::put_varint(bytes, field, value);
}

fn put_string(bytes: &mut Vec<u8>, field: u32, value: Option<String>) {
    if let Some(value) = value {
        anki::put_bytes(bytes, field, value.as_bytes());
    }
}

fn put_other(bytes: &mut Vec<u8>, entry: &Json, key: &str) {
    let raw = entry
        .get(key)
        .and_then(Json::as_str)
        .and_then(decode_hex)
        .unwrap_or_default();
    if !raw.is_empty() {
        anki::put_bytes(bytes, 255, &raw);
    }
}

fn put_reqs(bytes: &mut Vec<u8>, model: &NoteModel) {
    let Some(reqs) = raw_entry(model)
        .and_then(|entry| entry.get("req").and_then(Json::as_array).cloned())
    else {
        return;
    };
    for req in reqs {
        let Some(parts) = req.as_array() else {
            continue;
        };
        let card_ord = parts.first().and_then(Json::as_u64).unwrap_or_default();
        let kind = match parts.get(1).and_then(Json::as_str) {
            Some("any") => 1,
            Some("all") => 2,
            _ => 0,
        };
        let mut message = Vec::new();
        put_varint_u64(&mut message, 1, card_ord);
        put_varint_u64(&mut message, 2, kind);
        let mut fields = Vec::new();
        if let Some(ords) = parts.get(2).and_then(Json::as_array) {
            for ord in ords.iter().filter_map(Json::as_u64) {
                anki::append_varint(&mut fields, ord);
            }
        }
        if !fields.is_empty() {
            anki::put_bytes(&mut message, 3, &fields);
        }
        anki::put_bytes(bytes, 8, &message);
    }
}

pub(crate) fn notetype_config_for(model: &NoteModel) -> Vec<u8> {
    let mut bytes = Vec::new();
    put_varint_u64(
        &mut bytes,
        1,
        u64::try_from(model.kind.code()).unwrap_or(0),
    );
    put_varint_u64(&mut bytes, 2, u64::from(model.sort_field_index));
    anki::put_bytes(&mut bytes, 3, model.css.as_bytes());
    if let Some(entry) = raw_entry(model) {
        put_string(&mut bytes, 5, json_string(&entry, "latexPre"));
        put_string(&mut bytes, 6, json_string(&entry, "latexPost"));
        if json_bool(&entry, "latexSvg") || json_bool(&entry, "latexsvg") {
            put_varint_u64(&mut bytes, 7, 1);
        }
        if let Some(stock) = model.original_stock_kind {
            put_varint_u64(&mut bytes, 9, u64::try_from(stock).unwrap_or(0));
        }
        if let Some(original) = json_u64(&entry, "originalId") {
            put_varint_u64(&mut bytes, 10, original);
        }
    } else if let Some(stock) = model.original_stock_kind {
        put_varint_u64(&mut bytes, 9, u64::try_from(stock).unwrap_or(0));
    }
    if let Some(raw) = &model.raw_config {
        if !raw.is_empty() {
            anki::put_bytes(&mut bytes, 255, raw);
        }
    }
    put_reqs(&mut bytes, model);
    bytes
}

pub(crate) fn field_config_for(
    model: &NoteModel,
    ord: usize,
    tag: Option<u32>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(entry) = raw_entry(model)
        .as_ref()
        .and_then(|entry| entry.get("flds"))
        .and_then(Json::as_array)
        .and_then(|fields| fields.get(ord))
    {
        if json_bool(entry, "sticky") {
            put_varint_u64(&mut bytes, 1, 1);
        }
        if json_bool(entry, "rtl") {
            put_varint_u64(&mut bytes, 2, 1);
        }
        put_string(&mut bytes, 3, json_string(entry, "font"));
        if let Some(size) = json_u64(entry, "size") {
            put_varint_u64(&mut bytes, 4, size);
        }
        put_string(&mut bytes, 5, json_string(entry, "description"));
        for (field, key) in [
            (6, "plainText"),
            (7, "collapsed"),
            (8, "excludeFromSearch"),
            (11, "preventDeletion"),
        ] {
            if json_bool(entry, key) {
                put_varint_u64(&mut bytes, field, 1);
            }
        }
        if let Some(id) = json_u64(entry, "id") {
            put_varint_u64(&mut bytes, 9, id);
        }
        put_other(&mut bytes, entry, "_rankiOther");
    }
    if let Some(tag) = tag.or_else(|| {
        raw_entry(model)
            .as_ref()
            .and_then(|entry| entry.get("flds"))
            .and_then(Json::as_array)
            .and_then(|fields| fields.get(ord))
            .and_then(|entry| entry.get("tag"))
            .and_then(Json::as_u64)
            .and_then(|tag| u32::try_from(tag).ok())
    }) {
        put_varint_u64(&mut bytes, 10, u64::from(tag));
    }
    bytes
}

pub(crate) fn template_config_for(
    model: &NoteModel,
    position: usize,
    question: &str,
    answer: &str,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    anki::put_bytes(&mut bytes, 1, question.as_bytes());
    anki::put_bytes(&mut bytes, 2, answer.as_bytes());
    if let Some(entry) = raw_entry(model)
        .as_ref()
        .and_then(|entry| entry.get("tmpls"))
        .and_then(Json::as_array)
        .and_then(|templates| templates.get(position))
    {
        put_string(&mut bytes, 3, json_string(entry, "bqfmt"));
        put_string(&mut bytes, 4, json_string(entry, "bafmt"));
        if let Some(did) = json_u64(entry, "did") {
            put_varint_u64(&mut bytes, 5, did);
        }
        put_string(&mut bytes, 6, json_string(entry, "bfont"));
        if let Some(size) = json_u64(entry, "bsize") {
            put_varint_u64(&mut bytes, 7, size);
        }
        if let Some(id) = json_u64(entry, "id") {
            put_varint_u64(&mut bytes, 8, id);
        }
        put_other(&mut bytes, entry, "_rankiOther");
    }
    bytes
}

/// Builds schema-11 model JSON from decoded note types.
///
/// Unknown protobuf config bytes are kept in `_rankiRawConfig` so an export
/// can be imported by Anki without losing settings it does not expose.
#[must_use]
pub fn models_to_json(models: &[NoteModel]) -> Json {
    let mut map = Map::new();
    for model in models {
        let mut entry = model
            .raw_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_else(|| model_entry(model));
        overlay_model(&mut entry, model);
        let _ = map.insert(model.id.to_string(), entry);
    }
    Json::Object(map)
}

fn model_entry(model: &NoteModel) -> Json {
    let fields = model
        .fields
        .iter()
        .enumerate()
        .map(|(ord, name)| {
            json!({
                "name": name,
                "ord": ord,
                "sticky": false,
                "rtl": false,
                "font": "Arial",
                "size": 20,
                "tag": model.field_tags.get(ord).copied().flatten(),
            })
        })
        .collect::<Vec<_>>();
    let templates = model
        .templates
        .iter()
        .map(|template| {
            json!({
                "name": template.name,
                "ord": template.ord,
                "qfmt": template.question_format,
                "afmt": template.answer_format,
                "bqfmt": "",
                "bafmt": "",
                "did": null,
                "bfont": "",
                "bsize": 0,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "id": model.id,
        "name": model.name,
        "type": model.kind.code(),
        "mod": 0,
        "usn": 0,
        "sortf": model.sort_field_index,
        "did": 1,
        "flds": fields,
        "tmpls": templates,
        "css": model.css,
        "latexPre": "",
        "latexPost": "\\end{document}",
        "latexsvg": false,
        "req": [],
    })
}

/// Updates the known keys in an existing schema-11 model entry. Every other
/// key, including field and template settings, is left exactly as stored.
fn overlay_model(entry: &mut Json, model: &NoteModel) {
    if let Some(object) = entry.as_object_mut() {
        let _ = object.insert("id".into(), json!(model.id));
        let _ = object.insert("name".into(), json!(model.name));
        let _ = object.insert("type".into(), json!(model.kind.code()));
        let _ = object.insert("sortf".into(), json!(model.sort_field_index));
        let _ = object.insert("css".into(), json!(model.css));
        let _ = object.entry("mod").or_insert(json!(0));
        let _ = object.entry("usn").or_insert(json!(0));
        let _ = object.entry("latexPre").or_insert(json!(""));
        let _ = object.entry("latexPost").or_insert(json!(""));
        let _ = object.entry("req").or_insert(json!([]));
        if let Some(kind) = model.original_stock_kind {
            let _ = object.insert("originalStockKind".into(), json!(kind));
        }
        if let Some(raw) = &model.raw_config {
            let _ =
                object.insert("_rankiRawConfig".into(), json!(encode_hex(raw)));
        }
    }
    if let Some(object) = entry.as_object_mut() {
        let fields = object.entry("flds").or_insert(json!([]));
        if let Some(fields) = fields.as_array_mut() {
            for (ord, name) in model.fields.iter().enumerate() {
                let tag = model.field_tags.get(ord).copied().flatten();
                if fields.len() <= ord {
                    fields.push(json!({
                        "name": name,
                        "ord": ord,
                        "sticky": false,
                        "rtl": false,
                        "font": "Arial",
                        "size": 20,
                        "tag": tag,
                    }));
                } else if let Some(field) =
                    fields.get_mut(ord).and_then(Json::as_object_mut)
                {
                    let _ = field.insert("name".into(), json!(name));
                    let _ = field.insert("ord".into(), json!(ord));
                    let _ = field.insert("tag".into(), json!(tag));
                }
            }
        }
    }
    if let Some(object) = entry.as_object_mut() {
        let templates = object.entry("tmpls").or_insert(json!([]));
        if let Some(templates) = templates.as_array_mut() {
            for (position, template) in model.templates.iter().enumerate() {
                let value = json!({
                    "name": template.name,
                    "ord": template.ord,
                    "qfmt": template.question_format,
                    "afmt": template.answer_format,
                });
                if templates.len() <= position {
                    let mut value = value;
                    if let Some(object) = value.as_object_mut() {
                        let _ = object.insert("bqfmt".into(), json!(""));
                        let _ = object.insert("bafmt".into(), json!(""));
                        let _ = object.insert("did".into(), Json::Null);
                    }
                    templates.push(value);
                } else if let Some(object) =
                    templates.get_mut(position).and_then(Json::as_object_mut)
                {
                    if let Some(source) = value.as_object() {
                        for (key, field) in source {
                            let _ = object.insert(key.clone(), field.clone());
                        }
                    }
                }
            }
        }
    }
}

pub fn table_exists(conn: &Connection, name: &str) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .map_err(crate::backend)
}

fn conversion(error: StoreError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(error))
}

fn string_array(value: Option<&Json>, key: &str) -> Vec<String> {
    let Some(items) = value.and_then(Json::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.get(key).and_then(Json::as_str))
        .map(ToOwned::to_owned)
        .collect()
}
fn optional_numbers(value: Option<&Json>, key: &str) -> Vec<u64> {
    let Some(items) = value.and_then(Json::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.get(key).and_then(Json::as_u64))
        .collect()
}

/// Anki stores field tags in protobuf configs. Older cloze models omit them,
/// so recover them from the cloze filter in the question template.
fn infer_cloze_tags(model: &mut NoteModel) {
    if !model.is_cloze()
        || model.field_tags.iter().any(Option::is_some)
        || model.templates.is_empty()
    {
        return;
    }
    let Some(question) =
        model.templates.first().map(|item| &item.question_format)
    else {
        return;
    };
    let Some(start) = question.find("{{cloze:") else {
        return;
    };
    let name_start = start.saturating_add(8);
    let Some(name) = question
        .get(name_start..)
        .and_then(|tail| tail.find("}}").and_then(|end| tail.get(..end)))
    else {
        return;
    };
    let Some(index) = model.fields.iter().position(|field| field == name)
    else {
        return;
    };
    if let Some(tag) = model.field_tags.get_mut(index) {
        *tag = Some(0);
    }
    if let Some(tag) =
        model
            .field_tags
            .iter_mut()
            .enumerate()
            .find_map(|(position, tag)| {
                (position != index && tag.is_none()).then_some(tag)
            })
    {
        *tag = Some(1);
    }
}

pub(crate) fn text(raw: Value) -> String {
    match raw {
        Value::Text(value) => value,
        Value::Blob(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Value::Integer(number) => number.to_string(),
        Value::Real(number) => number.to_string(),
        Value::Null => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use ranki_model::CardTemplate;
    use ranki_model::NoteKind;
    use ranki_model::NoteModel;

    use super::template_config_for;
    use crate::anki::protobuf_string;

    #[test]
    fn sparse_template_ordinals_use_the_json_position_for_settings() {
        let model = NoteModel {
            id: 7,
            name: "Sparse".into(),
            fields: vec!["Front".into()],
            templates: vec![
                CardTemplate {
                    name: "First".into(),
                    ord: 0,
                    ..CardTemplate::default()
                },
                CardTemplate {
                    name: "Second".into(),
                    ord: 2,
                    ..CardTemplate::default()
                },
            ],
            css: String::new(),
            sort_field_index: 0,
            kind: NoteKind::Normal,
            original_stock_kind: None,
            field_tags: Vec::new(),
            template_ordinals: vec![0, 2],
            raw_config: None,
            raw_json: Some(
                serde_json::json!({
                    "tmpls": [
                        {"bqfmt": "first browser"},
                        {"bqfmt": "second browser"}
                    ]
                })
                .to_string(),
            ),
        };
        let config = template_config_for(&model, 1, "{{Front}}", "{{Back}}");
        assert_eq!(
            protobuf_string(&config, 3).as_deref(),
            Some("second browser")
        );
    }
}
