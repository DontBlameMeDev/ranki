#![expect(
    clippy::excessive_nesting,
    clippy::indexing_slicing,
    clippy::missing_const_for_fn,
    clippy::shadow_reuse,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::wildcard_enum_match_arm,
    reason = "Anki's two collection layouts share one import pipeline"
)]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

use ranki_model::DeckKind;
use ranki_model::DeckPreset;
use ranki_model::EffectiveDeck;
use ranki_model::NoteKind;
use ranki_model::NoteModel;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::params;
use rusqlite::types::Value as SqlValue;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

use crate::FormatError;
use crate::ImportCounts;
use crate::ImportOptions;
use crate::ImportReport;
use crate::UpdateCondition;
use crate::error::db_error;
use crate::error::json_error;
use crate::media::MediaRename;
use crate::media::ReferenceMatcher;
use crate::schema::current_time;
use crate::schema::encode_json;

const DAY_SECS: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Legacy,
    Normalized,
}

#[derive(Clone, Copy, Debug)]
struct ColMeta {
    crt: i64,
    usn: i32,
    layout: Layout,
    load_balancer: bool,
}

#[derive(Debug, Default)]
struct Metadata {
    presets: Vec<DeckPreset>,
    decks: Vec<EffectiveDeck>,
    models: Vec<NoteModel>,
    model_mtimes: BTreeMap<i64, i64>,
    kind_blobs: BTreeMap<i64, Vec<u8>>,
    template_blobs: BTreeMap<(i64, i64), Vec<u8>>,
}

#[derive(Debug, Default)]
struct SourceBlobs {
    template_blobs: BTreeMap<(i64, i64), Vec<u8>>,
}

struct MergeState {
    next_id: i64,
    used_ids: HashSet<i64>,
    usn: i32,
    now_secs: i64,
    source_day_delta: i64,
    source_today: i64,
    target_today: i64,
    options: ImportOptions,
    unique_names: bool,
    model_map: BTreeMap<i64, i64>,
    field_maps: BTreeMap<i64, FieldMap>,
    sort_fields: BTreeMap<i64, usize>,
    template_maps: BTreeMap<i64, BTreeMap<i64, i64>>,
    restricted_templates: HashSet<i64>,
    deck_map: BTreeMap<i64, i64>,
    preset_map: BTreeMap<i64, i64>,
    note_map: BTreeMap<i64, Option<i64>>,
    card_map: BTreeMap<i64, Option<i64>>,
    note_model: BTreeMap<i64, i64>,
    kind_blobs_new: BTreeMap<i64, Vec<u8>>,
    template_blob_overrides: BTreeMap<(i64, i64), Vec<u8>>,
    used_revlog_ids: HashSet<i64>,
    report: ImportReport,
}

struct NoteRow {
    id: i64,
    guid: String,
    mid: i64,
    modified: i64,
    tags: String,
    fields: String,
    sort_field: String,
    checksum: i64,
    flags: i32,
    data: String,
}

struct CardRow {
    id: i64,
    note_id: i64,
    deck_id: i64,
    ordinal: i64,
    modified: i64,
    kind: i32,
    queue: i32,
    due: i64,
    interval: i32,
    factor: i32,
    reps: i32,
    lapses: i32,
    left: i32,
    original_due: i64,
    original_deck_id: i64,
    flags: i32,
    data: String,
}

struct RevlogRow {
    id: i64,
    card_id: i64,
    ease: i32,
    interval: i32,
    last_interval: i32,
    factor: i32,
    time_ms: i32,
    kind: i32,
}

/// Merge an attached package collection into the main collection.
pub fn merge_collection(
    connection: &mut Connection,
    renames: &[MediaRename],
    media_count: usize,
    options: ImportOptions,
) -> Result<ImportReport, FormatError> {
    let transaction = connection
        .transaction()
        .map_err(db_error("cannot start import transaction"))?;
    rewrite_incoming_notes(&transaction, renames)?;

    let destination = read_col_meta(&transaction, "main")?;
    let incoming = read_col_meta(&transaction, "incoming")?;
    let now_secs = current_time()?;
    let source_today = day_number(now_secs, incoming.crt)?;
    let target_today = day_number(now_secs, destination.crt)?;
    let source_day_delta = source_today
        .checked_sub(target_today)
        .ok_or_else(|| FormatError::new("invalid collection creation time"))?;
    let target_usn = destination
        .usn
        .checked_add(1)
        .ok_or_else(|| FormatError::new("collection update number overflow"))?;
    let incoming_layout = incoming.layout;
    let destination_layout = destination.layout;
    let mut target_metadata = read_metadata(&transaction, "main", destination)?;
    let source_metadata = read_metadata(&transaction, "incoming", incoming)?;
    let source_blobs = read_source_blobs(&transaction, incoming_layout)?;
    let mut state = MergeState::new(
        &transaction,
        &target_metadata,
        target_usn,
        now_secs,
        source_day_delta,
        source_today,
        target_today,
        options,
        destination_layout,
    )?;
    state.report.media = media_count;
    state.preset_map = merge_presets(
        &mut target_metadata.presets,
        &source_metadata.presets,
        &mut state,
    )?;
    state.deck_map =
        merge_decks(&mut target_metadata, &source_metadata, &mut state)?;
    state.model_map = merge_models(
        &mut target_metadata.models,
        &source_metadata.models,
        &target_metadata.model_mtimes,
        &state.deck_map.clone(),
        &source_blobs,
        &mut state,
    )?;
    for ((ntid, ord), blob) in &state.template_blob_overrides {
        let _ = target_metadata
            .template_blobs
            .insert((*ntid, *ord), blob.clone());
    }

    merge_notes(&transaction, &mut state)?;
    merge_cards(&transaction, &mut state)?;
    merge_revlog(&transaction, &mut state)?;
    write_metadata(&transaction, &target_metadata, &state)?;
    write_col_stamp(&transaction, &state)?;

    transaction
        .commit()
        .map_err(db_error("cannot commit imported collection"))?;
    Ok(state.report)
}

fn day_number(now_secs: i64, crt: i64) -> Result<i64, FormatError> {
    now_secs
        .checked_sub(crt)
        .map(|elapsed| elapsed.div_euclid(DAY_SECS))
        .ok_or_else(|| FormatError::new("invalid collection creation time"))
}

impl MergeState {
    #[expect(
        clippy::too_many_arguments,
        reason = "the import state is built once from the two collections"
    )]
    fn new(
        connection: &Connection,
        target: &Metadata,
        usn: i32,
        now_secs: i64,
        source_day_delta: i64,
        source_today: i64,
        target_today: i64,
        options: ImportOptions,
        layout: Layout,
    ) -> Result<Self, FormatError> {
        let mut used_ids = HashSet::new();
        for table in [
            "notes",
            "cards",
            "revlog",
            "decks",
            "deck_config",
            "notetypes",
        ] {
            for id in all_ids(connection, "main", table)? {
                let _ = used_ids.insert(id);
            }
        }
        for model in &target.models {
            let _ = used_ids.insert(model.id);
        }
        for preset in &target.presets {
            let _ = used_ids.insert(preset.id);
        }
        for deck in &target.decks {
            let _ = used_ids.insert(deck.id);
        }
        Ok(Self {
            next_id: used_ids
                .iter()
                .copied()
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| {
                    FormatError::new(
                        "identifier overflow while importing package",
                    )
                })?,
            used_ids,
            usn,
            now_secs,
            source_day_delta,
            source_today,
            target_today,
            options,
            unique_names: layout == Layout::Normalized,
            model_map: BTreeMap::new(),
            field_maps: BTreeMap::new(),
            sort_fields: BTreeMap::new(),
            template_maps: BTreeMap::new(),
            restricted_templates: HashSet::new(),
            deck_map: BTreeMap::new(),
            preset_map: BTreeMap::new(),
            note_map: BTreeMap::new(),
            card_map: BTreeMap::new(),
            note_model: BTreeMap::new(),
            kind_blobs_new: BTreeMap::new(),
            template_blob_overrides: BTreeMap::new(),
            used_revlog_ids: all_ids(connection, "main", "revlog")?
                .into_iter()
                .collect(),
            report: ImportReport::default(),
        })
    }

    fn take_id(&mut self) -> Result<i64, FormatError> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or_else(|| {
            FormatError::new("identifier overflow while importing package")
        })?;
        let _ = self.used_ids.insert(id);
        Ok(id)
    }

    fn bump(count: &mut usize) -> Result<(), FormatError> {
        ImportCounts::increment(count)
    }
    #[cfg(test)]
    fn new_for_test(options: ImportOptions) -> Self {
        Self {
            next_id: 1_000,
            used_ids: HashSet::new(),
            usn: 1,
            now_secs: 1_000_000,
            source_day_delta: 0,
            source_today: 0,
            target_today: 0,
            options,
            unique_names: false,
            model_map: BTreeMap::new(),
            field_maps: BTreeMap::new(),
            sort_fields: BTreeMap::new(),
            template_maps: BTreeMap::new(),
            restricted_templates: HashSet::new(),
            deck_map: BTreeMap::new(),
            preset_map: BTreeMap::new(),
            note_map: BTreeMap::new(),
            card_map: BTreeMap::new(),
            note_model: BTreeMap::new(),
            kind_blobs_new: BTreeMap::new(),
            template_blob_overrides: BTreeMap::new(),
            used_revlog_ids: HashSet::new(),
            report: ImportReport::default(),
        }
    }
}

fn claim_id(
    state: &mut MergeState,
    used: &mut HashSet<i64>,
    preferred: i64,
) -> Result<i64, FormatError> {
    if preferred > 0 && used.insert(preferred) {
        return Ok(preferred);
    }
    loop {
        let id = state.take_id()?;
        if used.insert(id) {
            return Ok(id);
        }
    }
}

fn all_ids(
    connection: &Connection,
    prefix: &str,
    table: &str,
) -> Result<Vec<i64>, FormatError> {
    if !has_table(connection, prefix, table)? {
        return Ok(Vec::new());
    }
    let sql = format!("SELECT id FROM {prefix}.{table}");
    let mut statement = connection.prepare(&sql).map_err(db_error(format!(
        "cannot read identifiers from {prefix}.{table}"
    )))?;
    let rows = statement
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(db_error(format!(
            "cannot read identifiers from {prefix}.{table}"
        )))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error(format!(
            "cannot read identifiers from {prefix}.{table}"
        )))?;
    Ok(rows)
}

fn has_table(
    connection: &Connection,
    prefix: &str,
    table: &str,
) -> Result<bool, FormatError> {
    let sql = format!(
        "SELECT 1 FROM {prefix}.sqlite_master WHERE type = 'table' AND name = ?1"
    );
    connection
        .query_row(&sql, [table], |_| Ok(()))
        .optional()
        .map(|row| row.is_some())
        .map_err(db_error(format!("cannot inspect {prefix} schema")))
}

fn read_col_meta(
    connection: &Connection,
    prefix: &str,
) -> Result<ColMeta, FormatError> {
    let sql =
        format!("SELECT crt, usn, ver, conf FROM {prefix}.col WHERE id = 1");
    let row = connection
        .query_row(&sql, [], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, SqlValue>(3)?,
            ))
        })
        .optional()
        .map_err(db_error(format!(
            "cannot read {prefix} collection metadata"
        )))?
        .ok_or_else(|| {
            FormatError::new(format!("{prefix} collection has no metadata row"))
        })?;
    let conf = match row.3 {
        SqlValue::Text(text) => decode_json(Some(text), "conf")?,
        SqlValue::Blob(bytes) => serde_json::from_slice(&bytes)
            .map_err(json_error("invalid collection conf JSON"))?,
        _ => Value::Object(Map::new()),
    };
    let load_balancer = conf
        .get("loadBalancerEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let layout = if row.2 >= 18
        && has_table(connection, prefix, "decks")?
        && has_table(connection, prefix, "deck_config")?
        && has_table(connection, prefix, "notetypes")?
    {
        Layout::Normalized
    } else {
        Layout::Legacy
    };
    Ok(ColMeta {
        crt: row.0,
        usn: i32::try_from(row.1).unwrap_or_default(),
        layout,
        load_balancer,
    })
}

fn decode_json(
    text: Option<String>,
    label: &str,
) -> Result<Value, FormatError> {
    let Some(contents) = text else {
        return Ok(Value::Object(Map::new()));
    };
    if contents.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&contents)
        .map_err(json_error(format!("invalid collection {label} JSON")))
}

fn read_json_column(
    connection: &Connection,
    prefix: &str,
    column: &str,
) -> Result<Value, FormatError> {
    let sql = format!("SELECT {column} FROM {prefix}.col WHERE id = 1");
    let value: Option<SqlValue> = connection
        .query_row(&sql, [], |row| row.get(0))
        .optional()
        .map_err(db_error(format!("cannot read {prefix} {column}")))?
        .flatten();
    match value {
        Some(SqlValue::Text(text)) => decode_json(Some(text), column),
        Some(SqlValue::Blob(bytes)) => serde_json::from_slice(&bytes)
            .map_err(json_error(format!("invalid {column} JSON"))),
        _ => Ok(Value::Object(Map::new())),
    }
}

fn read_metadata(
    connection: &Connection,
    prefix: &str,
    col: ColMeta,
) -> Result<Metadata, FormatError> {
    let mut metadata = Metadata::default();
    metadata.presets = read_presets(connection, prefix, col)?;
    metadata.decks = read_decks(connection, prefix, col, &mut metadata)?;
    metadata.models = read_models(connection, prefix, col)?;
    metadata.model_mtimes = metadata
        .models
        .iter()
        .map(|model| (model.id, model_mtime(model)))
        .collect();
    Ok(metadata)
}
fn preset_entries(json: &Value) -> Result<Vec<(String, Value)>, FormatError> {
    let object = as_object(json, "deck config")?;
    if object.keys().any(|key| key.parse::<i64>().is_ok()) {
        return Ok(object.into_iter().collect());
    }
    if object.contains_key("id") || object.contains_key("name") {
        return Ok(vec![(
            object
                .get("id")
                .and_then(Value::as_i64)
                .unwrap_or(1)
                .to_string(),
            Value::Object(object),
        )]);
    }
    Ok(Vec::new())
}

fn read_presets(
    connection: &Connection,
    prefix: &str,
    col: ColMeta,
) -> Result<Vec<DeckPreset>, FormatError> {
    let mut presets = Vec::new();
    if col.layout == Layout::Legacy {
        let json = read_json_column(connection, prefix, "dconf")?;
        for (key, value) in preset_entries(&json)? {
            let id = entry_id(&key, &value, "deck config")?;
            let mut preset =
                ranki_storage::preset_from_json(id, &value, col.load_balancer);
            if let Some(hex) =
                value.get("_rankiRawConfig").and_then(Value::as_str)
                && let Some(bytes) = ranki_model::decode_hex(hex)
            {
                preset.raw = bytes;
            }
            presets.push(preset);
        }
    } else if has_table(connection, prefix, "deck_config")? {
        let sql = format!(
            "SELECT id, name, mtime_secs, usn, config FROM {prefix}.deck_config ORDER BY id"
        );
        let mut statement = connection
            .prepare(&sql)
            .map_err(db_error("cannot read package deck presets"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            })
            .map_err(db_error("cannot read package deck presets"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error("cannot read package deck presets"))?;
        for (id, name, mtime, usn, config) in rows {
            let mut preset = ranki_storage::preset_from_proto(
                id,
                name,
                mtime,
                i32::try_from(usn).unwrap_or_default(),
                &config,
                col.load_balancer,
            );
            if !preset.raw.is_empty() {
                let mut entry = ranki_storage::preset_to_json(&preset);
                if let Some(object) = entry.as_object_mut() {
                    let _ = object.insert(
                        "_rankiRawConfig".to_owned(),
                        json!(ranki_model::encode_hex(&preset.raw)),
                    );
                    preset.raw_json = serde_json::to_string(&entry).ok();
                }
            }
            presets.push(preset);
        }
    }
    presets.sort_unstable_by_key(|preset| preset.id);
    Ok(presets)
}

fn read_decks(
    connection: &Connection,
    prefix: &str,
    col: ColMeta,
    metadata: &mut Metadata,
) -> Result<Vec<EffectiveDeck>, FormatError> {
    let mut decks = Vec::new();
    if col.layout == Layout::Legacy {
        let json = read_json_column(connection, prefix, "decks")?;
        for (key, value) in as_object(&json, "decks")? {
            let id = entry_id(&key, &value, "deck")?;
            let deck = ranki_storage::deck_from_json(id, &value);
            let (_, kind) = ranki_storage::deck_to_proto(&deck);
            let _ = metadata.kind_blobs.insert(id, kind);
            decks.push(deck);
        }
    } else if has_table(connection, prefix, "decks")? {
        let sql = format!(
            "SELECT id, name, common, kind FROM {prefix}.decks ORDER BY id"
        );
        let mut statement = connection
            .prepare(&sql)
            .map_err(db_error("cannot read package decks"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(db_error("cannot read package decks"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error("cannot read package decks"))?;
        for (id, name, common, kind) in rows {
            let _ = metadata.kind_blobs.insert(id, kind.clone());
            decks.push(ranki_storage::deck_from_proto(
                id, &name, &common, &kind,
            ));
        }
    }
    decks.sort_unstable_by_key(|deck| deck.id);
    Ok(decks)
}

fn read_models(
    connection: &Connection,
    prefix: &str,
    col: ColMeta,
) -> Result<Vec<NoteModel>, FormatError> {
    if col.layout == Layout::Legacy {
        let sql = format!("SELECT models FROM {prefix}.col WHERE id = 1");
        let stored: Option<SqlValue> = connection
            .query_row(&sql, [], |row| row.get(0))
            .optional()
            .map_err(db_error("cannot read package note types"))?
            .flatten();
        let json = match stored {
            Some(SqlValue::Text(text)) => text,
            Some(SqlValue::Blob(bytes)) => {
                String::from_utf8_lossy(&bytes).into_owned()
            }
            _ => String::new(),
        };
        ranki_storage::models_from_json(&json).map_err(|error| {
            FormatError::with_source("cannot decode note types", error)
        })
    } else if has_table(connection, prefix, "notetypes")? {
        ranki_storage::models_from_tables_with_prefix(connection, prefix)
            .map_err(|error| {
                FormatError::with_source("cannot decode note types", error)
            })
    } else {
        Ok(Vec::new())
    }
}

fn read_source_blobs(
    connection: &Connection,
    layout: Layout,
) -> Result<SourceBlobs, FormatError> {
    let mut blobs = SourceBlobs::default();
    if layout != Layout::Normalized
        || !has_table(connection, "incoming", "templates")?
    {
        return Ok(blobs);
    }
    let mut statement = connection
        .prepare(
            "SELECT ntid, ord, config FROM incoming.templates ORDER BY ntid, ord",
        )
        .map_err(db_error("cannot read package templates"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(db_error("cannot read package templates"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read package templates"))?;
    for (ntid, ord, config) in rows {
        let _ = blobs.template_blobs.insert((ntid, ord), config);
    }
    Ok(blobs)
}

fn as_object(
    value: &Value,
    label: &str,
) -> Result<Map<String, Value>, FormatError> {
    value.as_object().cloned().ok_or_else(|| {
        FormatError::new(format!("{label} must be a JSON object"))
    })
}

fn entry_id(key: &str, value: &Value, label: &str) -> Result<i64, FormatError> {
    if let Some(id) = value.get("id").and_then(Value::as_i64) {
        return Ok(id);
    }
    key.parse::<i64>().map_err(|_| {
        FormatError::new(format!("invalid {label} identifier '{key}'"))
    })
}

fn should_update(
    condition: UpdateCondition,
    existing: i64,
    incoming: i64,
) -> bool {
    match condition {
        UpdateCondition::IfNewer => existing < incoming,
        UpdateCondition::Always => true,
        UpdateCondition::Never => false,
    }
}

fn settings_equal(preset: &DeckPreset, other: &DeckPreset) -> bool {
    let mut left = preset.clone();
    let mut right = other.clone();
    for entry in [&mut left, &mut right] {
        entry.id = 0;
        entry.name.clear();
        entry.mtime_secs = 0;
        entry.usn = 0;
        entry.raw_json = None;
        entry.raw.clear();
    }
    left == right
}

fn merge_presets(
    destination: &mut Vec<DeckPreset>,
    incoming: &[DeckPreset],
    state: &mut MergeState,
) -> Result<BTreeMap<i64, i64>, FormatError> {
    let mut mapping = BTreeMap::new();
    let mut used: HashSet<i64> =
        destination.iter().map(|preset| preset.id).collect();
    if !state.options.with_deck_configs {
        let target_default = destination
            .iter()
            .find(|preset| preset.name.eq_ignore_ascii_case("Default"))
            .or_else(|| destination.iter().find(|preset| preset.id == 1))
            .or_else(|| destination.first())
            .map_or(1, |preset| preset.id);
        for preset in incoming {
            let _ = mapping.insert(preset.id, target_default);
            let _ = ImportCounts::increment(
                &mut state.report.preset_outcomes.duplicate,
            );
        }
        return Ok(mapping);
    }

    for source in incoming {
        let position = destination
            .iter()
            .position(|preset| preset.id == source.id)
            .or_else(|| {
                destination.iter().position(|preset| {
                    preset.name.eq_ignore_ascii_case(&source.name)
                })
            });
        if let Some(position) = position {
            let target_id = destination[position].id;
            let _ = mapping.insert(source.id, target_id);
            if settings_equal(&destination[position], source) {
                let _ = ImportCounts::increment(
                    &mut state.report.preset_outcomes.duplicate,
                );
                continue;
            }
            let mut updated = source.clone();
            updated.id = target_id;
            destination[position] = updated;
            let _ = ImportCounts::increment(
                &mut state.report.preset_outcomes.updated,
            );
            continue;
        }
        let id = claim_id(state, &mut used, source.id)?;
        let mut added = source.clone();
        added.id = id;
        let _ = mapping.insert(source.id, id);
        let _ = ImportCounts::increment(&mut state.report.preset_outcomes.new);
        destination.push(added);
    }
    Ok(mapping)
}

fn merge_decks(
    destination: &mut Metadata,
    source: &Metadata,
    state: &mut MergeState,
) -> Result<BTreeMap<i64, i64>, FormatError> {
    let mut incoming = source.decks.clone();
    incoming.sort_unstable_by_key(|deck| deck.name.split("::").count());
    for original in &incoming {
        let _ = state.kind_blobs_new.insert(
            original.id,
            source
                .kind_blobs
                .get(&original.id)
                .cloned()
                .unwrap_or_default(),
        );
    }
    let mut mapping = BTreeMap::new();
    let mut used: HashSet<i64> =
        destination.decks.iter().map(|deck| deck.id).collect();
    for original in incoming {
        let mut deck = original.clone();
        deck.preset_id = state
            .preset_map
            .get(&deck.preset_id)
            .copied()
            .unwrap_or(deck.preset_id);
        let mut by_name = BTreeMap::new();
        for (position, existing) in destination.decks.iter().enumerate() {
            let _ = by_name
                .entry(existing.name.to_lowercase())
                .or_insert(position);
        }
        let match_position = by_name.get(&deck.name.to_lowercase()).copied();
        let same_kind = match_position
            .and_then(|position| destination.decks.get(position))
            .is_some_and(|existing| existing.kind == deck.kind);
        if let Some(position) = match_position.filter(|_| same_kind) {
            let target_id = destination.decks[position].id;
            merge_deck_fields(&mut destination.decks[position], &deck, state);
            let _ = mapping.insert(original.id, target_id);
            let _ = ImportCounts::increment(
                &mut state.report.deck_outcomes.updated,
            );
            continue;
        }
        if match_position.is_some_and(|_| !same_kind) {
            deck.name = format!("{} {}", deck.name, state.now_secs);
        }
        ensure_parents(&mut destination.decks, &mut deck, state, &mut used)?;
        let id = claim_id(state, &mut used, deck.id)?;
        let old_id = deck.id;
        deck.id = id;
        if let Some(blob) = state.kind_blobs_new.get(&old_id).cloned() {
            let _ = destination.kind_blobs.insert(id, blob);
        }
        let _ = mapping.insert(original.id, id);
        let _ = ImportCounts::increment(&mut state.report.deck_outcomes.new);
        destination.decks.push(deck);
    }
    Ok(mapping)
}

fn merge_deck_fields(
    target: &mut EffectiveDeck,
    incoming: &EffectiveDeck,
    state: &MergeState,
) {
    if !incoming.description.is_empty() {
        target.description.clone_from(&incoming.description);
        target.markdown_description = incoming.markdown_description;
    }
    if incoming.preset_id != 1 {
        target.preset_id = incoming.preset_id;
    }
    target.overrides.new_limit =
        incoming.overrides.new_limit.or(target.overrides.new_limit);
    target.overrides.review_limit = incoming
        .overrides
        .review_limit
        .or(target.overrides.review_limit);
    if let Some(limit) = shift_limit(
        incoming.overrides.new_today,
        state.source_day_delta,
        state.source_today,
        state.target_today,
    ) {
        target.overrides.new_today = limit;
    }
    if let Some(limit) = shift_limit(
        incoming.overrides.review_today,
        state.source_day_delta,
        state.source_today,
        state.target_today,
    ) {
        target.overrides.review_today = limit;
    }
}

fn shift_limit(
    limit: ranki_model::DayLimit,
    delta: i64,
    source_today: i64,
    target_today: i64,
) -> Option<ranki_model::DayLimit> {
    if limit.limit.is_none() && limit.today == 0 {
        return None;
    }
    let day = i64::from(limit.day).checked_sub(delta)?;
    let today = if i64::from(limit.today) == source_today {
        u32::try_from(target_today.max(0)).ok()?
    } else if target_today > 0 {
        u32::try_from(target_today.checked_sub(1)?)
            .ok()?
            .min(limit.today)
    } else {
        return None;
    };
    Some(ranki_model::DayLimit {
        limit: limit.limit,
        today,
        day: u32::try_from(day).ok()?,
    })
}

fn ensure_parents(
    decks: &mut Vec<EffectiveDeck>,
    deck: &mut EffectiveDeck,
    state: &mut MergeState,
    used: &mut HashSet<i64>,
) -> Result<(), FormatError> {
    let parts: Vec<String> = deck.name.split("::").map(str::to_owned).collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let mut prefix = String::new();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push_str("::");
        }
        prefix.push_str(part);
        let position = decks.iter().position(|existing| {
            existing.name.to_lowercase() == prefix.to_lowercase()
        });
        match position {
            Some(position) if decks[position].kind == DeckKind::Filtered => {
                let renamed = format!("{} {}", prefix, state.now_secs);
                let id = claim_id(state, used, 0)?;
                decks.push(EffectiveDeck {
                    id,
                    name: renamed.clone(),
                    kind: DeckKind::Normal,
                    preset_id: 1,
                    ..EffectiveDeck::default()
                });
                deck.name = deck.name.replacen(&prefix, &renamed, 1);
                prefix = renamed;
            }
            Some(_) => {}
            None => {
                let id = claim_id(state, used, 0)?;
                decks.push(EffectiveDeck {
                    id,
                    name: prefix.clone(),
                    kind: DeckKind::Normal,
                    preset_id: 1,
                    ..EffectiveDeck::default()
                });
            }
        }
    }
    Ok(())
}

fn schema_key(
    model: &NoteModel,
) -> (i32, Vec<String>, Vec<(String, String)>, String, u16) {
    (
        model.kind.code(),
        model
            .fields
            .iter()
            .map(|field| field.to_lowercase())
            .collect(),
        model
            .templates
            .iter()
            .map(|template| {
                (
                    template.name.to_lowercase(),
                    template.question_format.clone(),
                )
            })
            .collect(),
        model.css.clone(),
        model.sort_field_index,
    )
}

fn model_mtime(model: &NoteModel) -> i64 {
    model
        .raw_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| value.get("mod").and_then(Value::as_i64))
        .unwrap_or_default()
}

fn original_id(model: &NoteModel) -> Option<i64> {
    model
        .raw_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|value| {
            value
                .get("originalId")
                .and_then(Value::as_i64)
                .filter(|id| *id > 0)
        })
}

fn same_names(model: &NoteModel, other: &NoteModel) -> bool {
    model.name.eq_ignore_ascii_case(&other.name)
        && model.kind == other.kind
        && model.fields.len() == other.fields.len()
        && model
            .fields
            .iter()
            .zip(other.fields.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn merge_models(
    destination: &mut Vec<NoteModel>,
    incoming: &[NoteModel],
    target_mtimes: &BTreeMap<i64, i64>,
    deck_map: &BTreeMap<i64, i64>,
    source_blobs: &SourceBlobs,
    state: &mut MergeState,
) -> Result<BTreeMap<i64, i64>, FormatError> {
    let mut mapping = BTreeMap::new();
    let mut used: HashSet<i64> =
        destination.iter().map(|preset| preset.id).collect();
    for incoming_model in incoming {
        let mut source = incoming_model.clone();
        remap_model_decks(&mut source, deck_map);
        let original = original_id(&source).unwrap_or(source.id);
        let position = destination
            .iter()
            .position(|model| model.id == source.id)
            .or_else(|| {
                destination.iter().position(|model| {
                    original_id(model) == Some(original)
                        && schema_key(model) == schema_key(&source)
                })
            })
            .or_else(|| {
                destination.iter().position(|model| {
                    model.id == original
                        && schema_key(model) == schema_key(&source)
                })
            })
            .or_else(|| {
                destination
                    .iter()
                    .position(|model| same_names(model, &source))
            })
            .or_else(|| {
                if state.options.merge_notetypes {
                    destination.iter().position(|model| {
                        model.id == original
                            || original_id(model) == Some(original)
                    })
                } else {
                    None
                }
            });
        if let Some(position) = position {
            let target_id = destination[position].id;
            let _ = mapping.insert(source.id, target_id);
            let compatible = schema_key(&destination[position])
                == schema_key(&source)
                || same_names(&destination[position], &source);
            if !compatible && !state.options.merge_notetypes {
                let id = claim_id(state, &mut used, source.id)?;
                let mut added = source.clone();
                added.id = id;
                set_original_id(&mut added, original);
                if state.unique_names {
                    added.name = unique_model_name(destination, &added.name);
                }
                let _ = ImportCounts::increment(
                    &mut state.report.notetype_outcomes.new,
                );
                let _ = state
                    .sort_fields
                    .insert(source.id, usize::from(added.sort_field_index));
                destination.push(added);
                let _ = mapping.insert(source.id, id);
                record_template_blobs(
                    state,
                    source.id,
                    id,
                    &identity_templates(&source),
                    source_blobs,
                );
                continue;
            }
            let will_update = should_update(
                state.options.update_notetypes,
                target_mtimes.get(&target_id).copied().unwrap_or_default(),
                model_mtime(&source),
            );
            let (field_map, template_map, sort_index, committed) = if compatible
            {
                if will_update {
                    let mut updated = source.clone();
                    updated.id = target_id;
                    updated.name.clone_from(&destination[position].name);
                    set_original_id(&mut updated, original);
                    (
                        identity_fields(&updated),
                        identity_templates(&updated),
                        usize::from(updated.sort_field_index),
                        Some(updated),
                    )
                } else {
                    if !destination[position].templates.is_empty() {
                        let _ = state.restricted_templates.insert(source.id);
                    }
                    (
                        identity_fields(&destination[position]),
                        existing_templates(&destination[position], &source),
                        usize::from(destination[position].sort_field_index),
                        None,
                    )
                }
            } else {
                let mut merged = destination[position].clone();
                let (fields, templates) =
                    merge_model_ords(&mut merged, &source);
                if will_update {
                    let mut updated = merged;
                    updated.id = target_id;
                    updated.name.clone_from(&destination[position].name);
                    set_original_id(&mut updated, original);
                    (
                        fields,
                        templates,
                        usize::from(updated.sort_field_index),
                        Some(updated),
                    )
                } else {
                    if !destination[position].templates.is_empty() {
                        let _ = state.restricted_templates.insert(source.id);
                    }
                    (
                        existing_fields(&destination[position], &source),
                        existing_templates(&destination[position], &source),
                        usize::from(destination[position].sort_field_index),
                        None,
                    )
                }
            };
            let _ = state.field_maps.insert(source.id, field_map);
            let _ = state.sort_fields.insert(source.id, sort_index);
            let _ = state.template_maps.insert(source.id, template_map.clone());
            if let Some(updated) = committed {
                destination[position] = updated;
                record_template_blobs(
                    state,
                    source.id,
                    target_id,
                    &template_map,
                    source_blobs,
                );
                let _ = ImportCounts::increment(
                    &mut state.report.notetype_outcomes.updated,
                );
            } else {
                let _ = ImportCounts::increment(
                    &mut state.report.notetype_outcomes.duplicate,
                );
            }
            continue;
        }
        if source.kind == NoteKind::Cloze
            && destination.iter().any(|model| {
                model.kind == NoteKind::Normal
                    && model.name.eq_ignore_ascii_case(&source.name)
            })
            || source.kind == NoteKind::Normal
                && destination.iter().any(|model| {
                    model.kind == NoteKind::Cloze
                        && model.name.eq_ignore_ascii_case(&source.name)
                })
        {
            let _ = ImportCounts::increment(
                &mut state.report.notetype_outcomes.conflicting,
            );
        } else {
            let _ = ImportCounts::increment(
                &mut state.report.notetype_outcomes.new,
            );
        }
        let id = claim_id(state, &mut used, source.id)?;
        let mut added = source.clone();
        added.id = id;
        set_original_id(&mut added, original);
        if state.unique_names {
            added.name = unique_model_name(destination, &added.name);
        }
        let _ = state
            .sort_fields
            .insert(source.id, usize::from(added.sort_field_index));
        let _ = state.field_maps.insert(source.id, identity_fields(&added));
        let _ = state
            .template_maps
            .insert(source.id, identity_templates(&added));
        record_template_blobs(
            state,
            source.id,
            id,
            &identity_templates(&source),
            source_blobs,
        );
        destination.push(added);
        let _ = mapping.insert(source.id, id);
    }
    Ok(mapping)
}

fn record_template_blobs(
    state: &mut MergeState,
    old_id: i64,
    new_id: i64,
    ord_map: &BTreeMap<i64, i64>,
    blobs: &SourceBlobs,
) {
    for ((ntid, ord), blob) in &blobs.template_blobs {
        if *ntid == old_id {
            let target_ord = ord_map.get(ord).copied().unwrap_or(*ord);
            let _ = state
                .template_blob_overrides
                .insert((new_id, target_ord), blob.clone());
        }
    }
}

fn remap_model_decks(model: &mut NoteModel, deck_map: &BTreeMap<i64, i64>) {
    let Some(raw) = model.raw_json.as_deref() else {
        return;
    };
    let Ok(mut entry) = serde_json::from_str::<Value>(raw) else {
        return;
    };
    let Some(object) = entry.as_object_mut() else {
        return;
    };
    remap_number(object, "did", deck_map);
    if let Some(templates) =
        object.get_mut("tmpls").and_then(Value::as_array_mut)
    {
        for template in templates.iter_mut() {
            if let Some(template) = template.as_object_mut() {
                remap_number(template, "did", deck_map);
            }
        }
    }
    model.raw_json = serde_json::to_string(&entry).ok();
}

fn unique_model_name(models: &[NoteModel], name: &str) -> String {
    if !models
        .iter()
        .any(|model| model.name.eq_ignore_ascii_case(name))
    {
        return name.to_owned();
    }
    for suffix in 2_u32..u32::MAX {
        let candidate = format!("{name} {suffix}");
        if !models
            .iter()
            .any(|model| model.name.eq_ignore_ascii_case(&candidate))
        {
            return candidate;
        }
    }
    name.to_owned()
}

fn set_original_id(model: &mut NoteModel, id: i64) {
    let mut entry = model
        .raw_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or_else(|| Value::Object(Map::new()));
    if let Some(object) = entry.as_object_mut() {
        let _ = object.insert("originalId".to_owned(), json!(id));
        model.raw_json = serde_json::to_string(&entry).ok();
    }
}

fn identity_fields(model: &NoteModel) -> FieldMap {
    FieldMap {
        positions: (0..model.fields.len())
            .map(|index| u16::try_from(index).ok())
            .collect(),
        len: model.fields.len(),
    }
}

fn existing_fields(target: &NoteModel, source: &NoteModel) -> FieldMap {
    FieldMap {
        positions: source
            .fields
            .iter()
            .map(|field| {
                target
                    .fields
                    .iter()
                    .position(|existing| existing.eq_ignore_ascii_case(field))
                    .and_then(|position| u16::try_from(position).ok())
            })
            .collect(),
        len: target.fields.len(),
    }
}

fn existing_templates(
    target: &NoteModel,
    source: &NoteModel,
) -> BTreeMap<i64, i64> {
    let mut map = BTreeMap::new();
    for (position, template) in source.templates.iter().enumerate() {
        if let Some(new_ord) = target.templates.iter().position(|existing| {
            existing.name.eq_ignore_ascii_case(&template.name)
        }) {
            let old_ord =
                source.template_ordinals.get(position).copied().unwrap_or(0);
            let committed_ord = target.templates.get(new_ord).map_or_else(
                || u16::try_from(new_ord).unwrap_or(u16::MAX),
                |entry| entry.ord,
            );
            let _ = map.insert(i64::from(old_ord), i64::from(committed_ord));
        }
    }
    map
}

fn identity_templates(model: &NoteModel) -> BTreeMap<i64, i64> {
    let ords = if model.template_ordinals.len() == model.templates.len() {
        model.template_ordinals.clone()
    } else {
        model
            .templates
            .iter()
            .map(|template| template.ord)
            .collect()
    };
    ords.into_iter()
        .map(|ord| (i64::from(ord), i64::from(ord)))
        .collect()
}

fn merge_model_ords(
    target: &mut NoteModel,
    source: &NoteModel,
) -> (FieldMap, BTreeMap<i64, i64>) {
    let positions = source
        .fields
        .iter()
        .map(|field| {
            target
                .fields
                .iter()
                .position(|existing| existing.eq_ignore_ascii_case(field))
                .or_else(|| {
                    let ord = target.fields.len();
                    target.fields.push(field.clone());
                    Some(ord)
                })
                .and_then(|position| u16::try_from(position).ok())
        })
        .collect();
    let field_map = FieldMap {
        positions,
        len: target.fields.len(),
    };
    let mut template_map = BTreeMap::new();
    for (position, template) in source.templates.iter().enumerate() {
        let new_ord = target
            .templates
            .iter()
            .position(|existing| {
                existing.name.eq_ignore_ascii_case(&template.name)
            })
            .unwrap_or_else(|| {
                target.templates.push(template.clone());
                target.templates.len().saturating_sub(1)
            });
        let _ = template_map.insert(
            i64::from(
                source.template_ordinals.get(position).copied().unwrap_or(0),
            ),
            i64::try_from(new_ord).unwrap_or(0),
        );
    }
    for (ord, template) in target.templates.iter_mut().enumerate() {
        template.ord = u16::try_from(ord).unwrap_or(u16::MAX);
    }
    target.template_ordinals = (0..target.templates.len())
        .map(|index| u16::try_from(index).unwrap_or(u16::MAX))
        .collect();
    (field_map, template_map)
}

fn text_value(value: SqlValue) -> String {
    match value {
        SqlValue::Text(text) => text,
        SqlValue::Blob(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        SqlValue::Integer(number) => number.to_string(),
        SqlValue::Real(number) => number.to_string(),
        SqlValue::Null => String::new(),
    }
}

fn rewrite_incoming_notes(
    connection: &Connection,
    renames: &[MediaRename],
) -> Result<(), FormatError> {
    if renames.is_empty() {
        return Ok(());
    }
    let matcher = ReferenceMatcher::new(renames);
    let mut statement = connection
        .prepare_cached("SELECT id, flds, sfld FROM incoming.notes")
        .map_err(db_error("cannot read package notes"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                text_value(row.get::<_, SqlValue>(2)?),
            ))
        })
        .map_err(db_error("cannot read package notes"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read package notes"))?;
    drop(statement);

    let mut update = connection
        .prepare_cached(
            "UPDATE incoming.notes SET flds = ?, sfld = ? WHERE id = ?",
        )
        .map_err(db_error("cannot rewrite package media references"))?;
    for (id, fields, sort_field) in rows {
        let new_fields = matcher.rewrite(&fields);
        let new_sort = matcher.rewrite(&sort_field);
        if new_fields.as_ref() != fields.as_str()
            || new_sort.as_ref() != sort_field.as_str()
        {
            update
                .execute(params![new_fields.as_ref(), new_sort.as_ref(), id])
                .map_err(db_error("cannot rewrite package media references"))?;
        }
    }
    Ok(())
}

fn read_notes(
    connection: &Connection,
    prefix: &str,
) -> Result<Vec<NoteRow>, FormatError> {
    let sql = format!(
        "SELECT id, guid, mid, mod, tags, flds, sfld, csum, flags, data
         FROM {prefix}.notes ORDER BY id"
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(db_error("cannot read package notes"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(NoteRow {
                id: row.get(0)?,
                guid: row.get(1)?,
                mid: row.get(2)?,
                modified: row.get(3)?,
                tags: row.get(4)?,
                fields: text_value(row.get::<_, SqlValue>(5)?),
                sort_field: text_value(row.get::<_, SqlValue>(6)?),
                checksum: row.get(7)?,
                flags: row.get(8)?,
                data: text_value(row.get::<_, SqlValue>(9)?),
            })
        })
        .map_err(db_error("cannot read package notes"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read package notes"))?;
    Ok(rows)
}

fn canonify_tags(
    tags: &str,
    connection: &Connection,
    state: &MergeState,
) -> Result<String, FormatError> {
    let entries: Vec<String> = tags
        .split([' ', '\u{3000}'])
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect();
    let canonified = ranki_storage::canonify_tags(
        &entries, connection, state.usn,
    )
    .map_err(|error| {
        FormatError::with_source("cannot register imported tags", error)
    })?;
    Ok(canonified.join(" "))
}

fn merge_notes(
    transaction: &Transaction<'_>,
    state: &mut MergeState,
) -> Result<(), FormatError> {
    let mut existing: HashMap<String, (i64, i64, i64)> = HashMap::new();
    let mut statement = transaction
        .prepare("SELECT id, guid, mid, mod FROM main.notes")
        .map_err(db_error("cannot read existing notes"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(db_error("cannot read existing notes"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read existing notes"))?;
    drop(statement);
    for (id, guid, mid, modified) in rows {
        let _ = existing.insert(guid, (id, mid, modified));
    }
    let mut used: HashSet<i64> =
        existing.values().map(|(id, _, _)| *id).collect();
    let incoming = read_notes(transaction, "incoming")?;
    let mut card_counts: BTreeMap<i64, usize> = BTreeMap::new();
    let mut cards = transaction
        .prepare("SELECT nid FROM incoming.cards")
        .map_err(db_error("cannot read package cards"))?;
    let note_ids = cards
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(db_error("cannot read package cards"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read package cards"))?;
    drop(cards);
    for id in note_ids {
        let count = card_counts.entry(id).or_default();
        *count = count.saturating_add(1);
    }

    let mut insert = transaction
        .prepare_cached(
            "INSERT INTO main.notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .map_err(db_error("cannot insert imported note"))?;
    let mut update = transaction
        .prepare_cached(
            "UPDATE main.notes SET mid = ?, mod = ?, usn = ?, tags = ?, flds = ?, sfld = ?, csum = ?, flags = ?, data = ?
             WHERE id = ?",
        )
        .map_err(db_error("cannot update imported note"))?;
    for mut note in incoming {
        if note.id <= 0
            || note.mid <= 0
            || note.modified < 0
            || note.guid.is_empty()
        {
            return Err(FormatError::new("invalid note record in package"));
        }
        if card_counts.get(&note.id).copied().unwrap_or_default() == 0 {
            let _ =
                ImportCounts::increment(&mut state.report.note_outcomes.empty);
        }
        let model_id =
            state.model_map.get(&note.mid).copied().unwrap_or(note.mid);
        if state.field_maps.contains_key(&note.mid) {
            let fields =
                state.field_maps.get(&note.mid).cloned().unwrap_or_default();
            let sort_index = state
                .sort_fields
                .get(&note.mid)
                .copied()
                .unwrap_or_default();
            note.fields = reorder_fields(&note.fields, &fields);
            let reordered = note
                .fields
                .split('\u{001f}')
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let first = reordered.first().map_or("", String::as_str);
            note.checksum =
                ranki_storage::csum_value(ranki_storage::checksum(first));
            note.sort_field =
                ranki_storage::sort_field_with(&reordered, sort_index, true);
        }
        note.tags = canonify_tags(&note.tags, transaction, state)?;
        let _ = state.note_model.insert(note.id, note.mid);
        if let Some((target_id, target_mid, target_modified)) =
            existing.get(&note.guid).copied()
        {
            if model_id != target_mid {
                let _ = state.note_map.insert(note.id, None);
                let _ = ImportCounts::increment(
                    &mut state.report.note_outcomes.conflicting,
                );
                continue;
            }
            if !should_update(
                state.options.update_notes,
                target_modified,
                note.modified,
            ) {
                let _ = state.note_map.insert(note.id, Some(target_id));
                let _ = ImportCounts::increment(
                    &mut state.report.note_outcomes.duplicate,
                );
                continue;
            }
            update
                .execute(params![
                    model_id,
                    note.modified,
                    state.usn,
                    note.tags.clone(),
                    note.fields.clone(),
                    note.sort_field,
                    note.checksum,
                    note.flags,
                    note.data,
                    target_id,
                ])
                .map_err(db_error("cannot update imported note"))?;
            let _ = state.note_map.insert(note.id, Some(target_id));
            let _ = ImportCounts::increment(
                &mut state.report.note_outcomes.updated,
            );
            state.report.notes =
                state.report.notes.checked_add(1).ok_or_else(|| {
                    FormatError::new("too many imported notes")
                })?;
            let _ = existing
                .insert(note.guid, (target_id, model_id, note.modified));
            continue;
        }
        let id = claim_id(state, &mut used, note.id)?;
        insert
            .execute(params![
                id,
                note.guid.clone(),
                model_id,
                note.modified,
                state.usn,
                note.tags,
                note.fields,
                note.sort_field,
                note.checksum,
                note.flags,
                note.data,
            ])
            .map_err(db_error("cannot insert imported note"))?;
        let _ = state.note_map.insert(note.id, Some(id));
        let _ = existing.insert(note.guid, (id, model_id, note.modified));
        let _ = ImportCounts::increment(&mut state.report.note_outcomes.new);
        state.report.notes = state
            .report
            .notes
            .checked_add(1)
            .ok_or_else(|| FormatError::new("too many imported notes"))?;
    }
    Ok(())
}

fn reorder_fields(fields: &str, map: &FieldMap) -> String {
    let mut result: Vec<Option<&str>> = vec![None; map.len];
    for (position, value) in fields.split('\u{001f}').enumerate() {
        if let Some(Some(target)) = map.positions.get(position)
            && let Some(entry) = result.get_mut(usize::from(*target))
        {
            *entry = Some(value);
        }
    }
    result
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect::<Vec<_>>()
        .join("\u{001f}")
}

fn read_cards(
    connection: &Connection,
    prefix: &str,
) -> Result<Vec<CardRow>, FormatError> {
    let sql = format!(
        "SELECT id, nid, did, ord, mod, type, queue, due, ivl, factor, reps,
                lapses, left, odue, odid, flags, data
         FROM {prefix}.cards ORDER BY id"
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(db_error("cannot read package cards"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(CardRow {
                id: row.get(0)?,
                note_id: row.get(1)?,
                deck_id: row.get(2)?,
                ordinal: row.get(3)?,
                modified: row.get(4)?,
                kind: row.get(5)?,
                queue: row.get(6)?,
                due: row.get(7)?,
                interval: row.get(8)?,
                factor: row.get(9)?,
                reps: row.get(10)?,
                lapses: row.get(11)?,
                left: row.get(12)?,
                original_due: row.get(13)?,
                original_deck_id: row.get(14)?,
                flags: row.get(15)?,
                data: text_value(row.get::<_, SqlValue>(16)?),
            })
        })
        .map_err(db_error("cannot read package cards"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read package cards"))?;
    Ok(rows)
}

fn shifted_due(
    value: i64,
    row: &CardRow,
    source_day_delta: i64,
) -> Result<i64, FormatError> {
    if !matches!(row.queue, 2 | 3) && row.kind != 2 {
        return Ok(value);
    }
    value.checked_sub(source_day_delta).ok_or_else(|| {
        FormatError::new("card due overflow while importing package")
    })
}

fn merge_cards(
    transaction: &Transaction<'_>,
    state: &mut MergeState,
) -> Result<(), FormatError> {
    let mut existing_ords = BTreeSet::new();
    let mut statement = transaction
        .prepare("SELECT nid, ord FROM main.cards")
        .map_err(db_error("cannot read existing cards"))?;
    let pairs = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .map_err(db_error("cannot read existing cards"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read existing cards"))?;
    drop(statement);
    for pair in pairs {
        let _ = existing_ords.insert(pair);
    }
    let mut used: HashSet<i64> =
        all_ids(transaction, "main", "cards")?.into_iter().collect();
    let rows = read_cards(transaction, "incoming")?;
    let mut insert = transaction
        .prepare_cached(
            "INSERT INTO main.cards (id, nid, did, ord, mod, usn, type, queue, due, ivl,
                 factor, reps, lapses, left, odue, odid, flags, data)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .map_err(db_error("cannot insert imported card"))?;
    for row in rows {
        if row.id <= 0 || row.note_id <= 0 || row.ordinal < 0 {
            return Err(FormatError::new("invalid card record in package"));
        }
        let Some(note_id) = state.note_map.get(&row.note_id).copied().flatten()
        else {
            let _ = state.card_map.insert(row.id, None);
            let _ = ImportCounts::increment(&mut state.report.cards_missing);
            continue;
        };
        let source_mid = state.note_model.get(&row.note_id).copied();
        let restricted = source_mid
            .is_some_and(|mid| state.restricted_templates.contains(&mid));
        let template_map =
            source_mid.and_then(|mid| state.template_maps.get(&mid));
        let ordinal = match template_map {
            Some(map) => match map.get(&row.ordinal).copied() {
                Some(ordinal) => ordinal,
                None if restricted => {
                    let _ = state.card_map.insert(row.id, None);
                    continue;
                }
                None => row.ordinal,
            },
            None => row.ordinal,
        };
        if existing_ords.contains(&(note_id, ordinal)) {
            let _ = state.card_map.insert(row.id, None);
            continue;
        }
        let deck_id = state
            .deck_map
            .get(&row.deck_id)
            .copied()
            .unwrap_or(row.deck_id);
        let original_deck_id = if row.original_deck_id == 0 {
            0
        } else {
            state
                .deck_map
                .get(&row.original_deck_id)
                .copied()
                .unwrap_or(row.original_deck_id)
        };
        let id = claim_id(state, &mut used, row.id)?;
        let (
            kind,
            queue,
            due,
            interval,
            factor,
            reps,
            lapses,
            left,
            original_due,
        ) = if state.options.with_scheduling {
            (
                row.kind,
                row.queue,
                shifted_due(row.due, &row, state.source_day_delta)?,
                row.interval,
                row.factor,
                row.reps,
                row.lapses,
                row.left,
                if row.original_due == 0 {
                    0
                } else {
                    shifted_due(row.original_due, &row, state.source_day_delta)?
                },
            )
        } else {
            (
                0,
                0,
                i64::try_from(state.report.cards).unwrap_or(i64::MAX),
                0,
                0,
                0,
                0,
                0,
                0,
            )
        };
        insert
            .execute(params![
                id,
                note_id,
                deck_id,
                ordinal,
                row.modified,
                state.usn,
                kind,
                queue,
                due,
                interval,
                factor,
                reps,
                lapses,
                left,
                original_due,
                original_deck_id,
                row.flags,
                row.data,
            ])
            .map_err(db_error("cannot insert imported card"))?;
        let _ = state.card_map.insert(row.id, Some(id));
        let _ = existing_ords.insert((note_id, ordinal));
        MergeState::bump(&mut state.report.cards)?;
    }
    Ok(())
}

fn merge_revlog(
    transaction: &Transaction<'_>,
    state: &mut MergeState,
) -> Result<(), FormatError> {
    if !state.options.with_scheduling {
        return Ok(());
    }
    let mut message = transaction
        .prepare(
            "SELECT id, cid, usn, ease, ivl, lastIvl, factor, time, type
                  FROM incoming.revlog ORDER BY id",
        )
        .map_err(db_error("cannot read package review log"))?;
    let rows = message
        .query_map([], |row| {
            Ok(RevlogRow {
                id: row.get(0)?,
                card_id: row.get(1)?,
                ease: row.get(3)?,
                interval: row.get(4)?,
                last_interval: row.get(5)?,
                factor: row.get(6)?,
                time_ms: row.get(7)?,
                kind: row.get(8)?,
            })
        })
        .map_err(db_error("cannot read package review log"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read package review log"))?;
    drop(message);

    let mut insert = transaction
        .prepare_cached(
            "INSERT INTO main.revlog (id, cid, usn, ease, ivl, lastIvl, factor, time, type)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .map_err(db_error("cannot insert imported review log entry"))?;
    for row in rows {
        if row.id <= 0 {
            return Err(FormatError::new(
                "invalid review log entry in package",
            ));
        }
        if state.used_revlog_ids.contains(&row.id) {
            continue;
        }
        let Some(card_id) = state.card_map.get(&row.card_id).copied().flatten()
        else {
            continue;
        };
        let _ = state.used_revlog_ids.insert(row.id);
        insert
            .execute(params![
                row.id,
                card_id,
                state.usn,
                row.ease,
                row.interval,
                row.last_interval,
                row.factor,
                row.time_ms,
                row.kind,
            ])
            .map_err(db_error("cannot insert imported review log entry"))?;
        MergeState::bump(&mut state.report.revlogs)?;
    }
    Ok(())
}

fn write_col_stamp(
    transaction: &Transaction<'_>,
    state: &MergeState,
) -> Result<(), FormatError> {
    let modified = state
        .now_secs
        .checked_mul(1000)
        .ok_or_else(|| FormatError::new("collection timestamp overflow"))?;
    transaction
        .execute(
            "UPDATE main.col SET mod = ?, usn = ? WHERE id = 1",
            params![modified, state.usn],
        )
        .map_err(db_error("cannot update collection timestamps"))
        .map(|_| ())
}

fn write_metadata(
    transaction: &Transaction<'_>,
    metadata: &Metadata,
    state: &MergeState,
) -> Result<(), FormatError> {
    let layout = read_col_meta(transaction, "main")?.layout;
    if layout == Layout::Normalized {
        write_normalized_metadata(transaction, metadata, state)
    } else {
        write_legacy_metadata(transaction, metadata, state)
    }
}

fn write_legacy_metadata(
    transaction: &Transaction<'_>,
    metadata: &Metadata,
    state: &MergeState,
) -> Result<(), FormatError> {
    let mut presets = Map::new();
    for preset in &metadata.presets {
        let _ = presets.insert(
            preset.id.to_string(),
            ranki_storage::preset_to_json(preset),
        );
    }
    let mut decks = Map::new();
    for deck in &metadata.decks {
        let mut entry = ranki_storage::deck_to_json(deck);
        if let Some(object) = entry.as_object_mut() {
            let _ = object.insert("id".to_owned(), json!(deck.id));
        }
        let _ = decks.insert(deck.id.to_string(), entry);
    }
    let models = ranki_storage::models_to_json(&metadata.models);
    let mut conf = read_json_column(transaction, "main", "conf")?;
    remap_conf(&mut conf, &state.model_map, &state.deck_map);
    let mut tags = read_json_column(transaction, "main", "tags")?;
    let incoming_tags = read_json_column(transaction, "incoming", "tags")?;
    if let (Some(object), Some(incoming)) =
        (tags.as_object_mut(), incoming_tags.as_object())
    {
        for name in incoming.keys() {
            let _ = object
                .entry(name.clone())
                .or_insert_with(|| json!(state.usn));
        }
    }
    transaction
        .execute(
            "UPDATE main.col SET conf = ?, models = ?, decks = ?, dconf = ?, tags = ? WHERE id = 1",
            params![
                encode_json(&conf)?,
                encode_json(&models)?,
                encode_json(&Value::Object(decks))?,
                encode_json(&Value::Object(presets))?,
                encode_json(&tags)?,
            ],
        )
        .map_err(db_error("cannot write collection metadata"))
        .map(|_| ())
}

fn remap_conf(
    conf: &mut Value,
    model_map: &BTreeMap<i64, i64>,
    deck_map: &BTreeMap<i64, i64>,
) {
    let Some(object) = conf.as_object_mut() else {
        return;
    };
    remap_number(object, "curModel", model_map);
    remap_number(object, "activeModel", model_map);
    remap_number(object, "curDeck", deck_map);
    if let Some(decks) =
        object.get_mut("activeDecks").and_then(Value::as_array_mut)
    {
        for deck in decks.iter_mut() {
            if let Some(old_id) = deck.as_i64()
                && let Some(new_id) = deck_map.get(&old_id)
            {
                *deck = Value::from(*new_id);
            }
        }
    }
}

fn remap_number(
    object: &mut Map<String, Value>,
    field: &str,
    mapping: &BTreeMap<i64, i64>,
) {
    if let Some(old_id) = object.get(field).and_then(Value::as_i64)
        && let Some(new_id) = mapping.get(&old_id)
    {
        let _ = object.insert(field.to_owned(), Value::from(*new_id));
    }
}

#[derive(Clone, Debug, Default)]
struct FieldMap {
    positions: Vec<Option<u16>>,
    len: usize,
}
fn write_normalized_metadata(
    transaction: &Transaction<'_>,
    metadata: &Metadata,
    state: &MergeState,
) -> Result<(), FormatError> {
    transaction
        .execute_batch(
            "DELETE FROM deck_config;
             DELETE FROM decks;
             DELETE FROM templates;
             DELETE FROM fields;
             DELETE FROM notetypes;",
        )
        .map_err(db_error("cannot clear normalized metadata"))?;
    transaction
        .execute("UPDATE main.col SET tags = '' WHERE id = 1", [])
        .map_err(db_error("cannot clear legacy tag JSON"))?;
    for preset in &metadata.presets {
        transaction
            .execute(
                "INSERT INTO deck_config (id, name, mtime_secs, usn, config)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    preset.id,
                    preset.name,
                    state.now_secs,
                    state.usn,
                    ranki_storage::preset_to_proto(preset),
                ],
            )
            .map_err(db_error("cannot write imported deck preset"))?;
    }
    for deck in &metadata.decks {
        let (common, generated_kind) = ranki_storage::deck_to_proto(deck);
        let kind = metadata
            .kind_blobs
            .get(&deck.id)
            .and_then(|blob| {
                ranki_storage::with_deck_kind_config_id(blob, deck.preset_id)
            })
            .unwrap_or(generated_kind);
        transaction
            .execute(
                "INSERT INTO decks (id, name, mtime_secs, usn, common, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    deck.id,
                    ranki_storage::encode_deck_name(&deck.name),
                    state.now_secs,
                    state.usn,
                    common,
                    kind,
                ],
            )
            .map_err(db_error("cannot write imported deck"))?;
    }
    for model in &metadata.models {
        ranki_storage::insert_model_tables_for_import(
            transaction,
            model,
            state.now_secs,
            state.usn,
        )
        .map_err(|error| {
            FormatError::with_source("cannot write imported note type", error)
        })?;
        for ((ntid, ord), blob) in &metadata.template_blobs {
            if *ntid != model.id {
                continue;
            }
            let source_deck = read_proto_varint(blob, 5).unwrap_or(1);
            if let Some(mapped) = state.deck_map.get(&source_deck).copied()
                && let Some(rewritten) = replace_proto_varint(blob, 5, mapped)
            {
                transaction
                    .execute(
                        "UPDATE templates SET config = ?1 WHERE ntid = ?2 AND ord = ?3",
                        params![rewritten, model.id, ord],
                    )
                    .map_err(db_error("cannot remap template target deck"))?;
            }
        }
    }
    Ok(())
}
fn read_proto_varint(bytes: &[u8], wanted: u32) -> Option<i64> {
    let mut position = 0;
    while position < bytes.len() {
        let tag = read_varint(bytes, &mut position)?;
        let field = tag >> 3;
        let wire = tag & 7;
        match wire {
            0 => {
                let value = read_varint(bytes, &mut position)?;
                if field == u64::from(wanted) {
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
        if position > bytes.len() {
            return None;
        }
    }
    None
}

fn replace_proto_varint(
    bytes: &[u8],
    wanted: u32,
    value: i64,
) -> Option<Vec<u8>> {
    let mut position = 0;
    while position < bytes.len() {
        let tag = read_varint(bytes, &mut position)?;
        let field = tag >> 3;
        let wire = tag & 7;
        let start = position;
        match wire {
            0 => {
                let _ = read_varint(bytes, &mut position)?;
                if field == u64::from(wanted) {
                    let mut replacement = Vec::new();
                    append_varint(&mut replacement, u64::try_from(value).ok()?);
                    let mut result = bytes.get(..start)?.to_vec();
                    result.extend(replacement);
                    result.extend_from_slice(bytes.get(position..)?);
                    return Some(result);
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
        if position > bytes.len() {
            return None;
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

fn append_varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        let byte = u8::try_from(value & 0x7f).unwrap_or(0) | 0x80;
        bytes.push(byte);
        value >>= 7;
    }
    bytes.push(u8::try_from(value).unwrap_or(0));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::append_varint;
    use super::should_update;
    use super::{
        MergeState, SourceBlobs, merge_models, replace_proto_varint,
        settings_equal,
    };
    use crate::ImportOptions;
    use crate::UpdateCondition;
    #[test]
    fn update_conditions_compare_mtimes() {
        assert!(should_update(UpdateCondition::IfNewer, 1, 2));
        assert!(!should_update(UpdateCondition::IfNewer, 2, 2));
        assert!(should_update(UpdateCondition::Always, 2, 2));
        assert!(!should_update(UpdateCondition::Never, 1, 9));
    }

    #[test]
    fn protobuf_rewrite_keeps_unknown_fields() {
        let mut bytes = vec![0x08, 0x01];
        append_varint(&mut bytes, 5 << 3);
        append_varint(&mut bytes, 700);
        bytes.extend([0xf8, 0x03, 0x07]);
        let rewritten = replace_proto_varint(&bytes, 5, 701).unwrap();
        let mut expected = vec![0x08, 0x01];
        append_varint(&mut expected, 5 << 3);
        append_varint(&mut expected, 701);
        expected.extend([0xf8, 0x03, 0x07]);
        assert_eq!(rewritten, expected);
    }

    #[test]
    fn presets_compare_settings_without_stamps() {
        let left = ranki_storage::preset_from_json(
            1,
            &json!({"name": "Default", "new": {"perDay": 20}}),
            true,
        );
        let mut right = left.clone();
        right.id = 9;
        right.name = "Other".to_owned();
        right.mtime_secs = 100;
        assert!(settings_equal(&left, &right));
    }

    #[test]
    fn model_merge_uses_id_before_schema_and_original_id() {
        let target = ranki_storage::models_from_json(
            &json!({
                "50": {"id": 50, "name": "Basic", "type": 0, "mod": 0, "sortf": 0,
                       "flds": [{"name": "Front"}, {"name": "Back"}],
                       "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}"}],
                       "css": ""}
            })
            .to_string(),
        )
        .unwrap();
        let incoming = ranki_storage::models_from_json(
            &json!({
                "1": {"id": 1, "name": "Copy", "type": 0, "mod": 10, "sortf": 0,
                      "flds": [{"name": "Front"}, {"name": "Back"}],
                      "tmpls": [{"name": "Card 1", "ord": 0, "qfmt": "{{Front}}"}],
                      "css": "", "originalId": 50}
            })
            .to_string(),
        )
        .unwrap();
        let mut destination = target;
        let mut state = MergeState::new_for_test(ImportOptions::default());
        let blobs = SourceBlobs::default();
        let mapping = merge_models(
            &mut destination,
            &incoming,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &blobs,
            &mut state,
        )
        .unwrap();
        assert_eq!(mapping.get(&1), Some(&50));
        assert_eq!(destination.len(), 1);
    }
}
