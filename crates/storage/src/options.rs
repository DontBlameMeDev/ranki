#![expect(
    clippy::absolute_paths,
    clippy::missing_const_for_fn,
    clippy::or_fun_call,
    clippy::redundant_closure,
    clippy::shadow_reuse,
    clippy::similar_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::wildcard_enum_match_arm,
    reason = "Anki codecs mirror the stored JSON and protobuf shapes"
)]
//! Codec between Anki deck options and their stored JSON/protobuf forms.

use ranki_model::DayLimit;
use ranki_model::DeckCommon;
use ranki_model::DeckKind;
use ranki_model::DeckOverrides;
use ranki_model::DeckPreset;
use ranki_model::EffectiveDeck;
use ranki_model::Id;
use ranki_model::LeechAction;
use ranki_model::StoreError;
use rusqlite::Connection;
use rusqlite::params;
use rusqlite::types::Value as SqlValue;
use serde_json::Value as Json;
use serde_json::json;

use crate::anki;
use crate::records;
use crate::schema;

/// Schema 18 moved decks, presets, and note types into normalized tables.
pub fn normalized(conn: &Connection) -> Result<bool, StoreError> {
    let version: i64 = conn
        .query_row("SELECT ver FROM col WHERE id = 1", [], |row| row.get(0))
        .map_err(crate::backend)?;
    Ok(version >= 18
        && records::table_exists(conn, "decks")?
        && records::table_exists(conn, "deck_config")?)
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidData(message.into())
}

fn parse_json(text: &str, label: &str) -> Result<Json, StoreError> {
    serde_json::from_str(text)
        .map_err(|error| invalid(format!("{label}: {error}")))
}

fn number(value: Option<&Json>) -> Option<f32> {
    value
        .and_then(Json::as_f64)
        .and_then(|number| to_f32(number))
}

fn unsigned(value: Option<&Json>) -> Option<u32> {
    value
        .and_then(Json::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

fn signed(value: Option<&Json>) -> Option<i32> {
    value
        .and_then(Json::as_i64)
        .and_then(|number| i32::try_from(number).ok())
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Anki stores JSON numbers that fit f32"
)]
fn to_f32(value: f64) -> Option<f32> {
    if value.is_finite() {
        Some(value as f32)
    } else {
        None
    }
}

fn text(value: Option<&Json>) -> Option<String> {
    value.and_then(Json::as_str).map(ToOwned::to_owned)
}

fn flag(value: Option<&Json>, default: bool) -> bool {
    value.and_then(Json::as_bool).unwrap_or(default)
}

fn floats(value: Option<&Json>) -> Vec<f32> {
    value
        .and_then(Json::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Json::as_f64)
                .filter_map(|item| to_f32(item))
                .collect()
        })
        .unwrap_or_default()
}

fn pair(value: Option<&Json>) -> (u32, u32) {
    let items = value.and_then(Json::as_array);
    (
        items
            .and_then(|values| values.first())
            .and_then(|item| unsigned(Some(item)))
            .unwrap_or_default(),
        items
            .and_then(|values| values.get(1))
            .and_then(|item| unsigned(Some(item)))
            .unwrap_or_default(),
    )
}

fn valid_u32(value: u32, default: u32, min: u32, max: u32) -> u32 {
    if value < min || value > max {
        default
    } else {
        value
    }
}

fn valid_f32(value: f32, default: f32, min: f32, max: f32) -> f32 {
    if value.is_nan() || value < min || value > max {
        default
    } else {
        value
    }
}

fn i32_from_u32(value: u32) -> i32 {
    i32::from_le_bytes(value.to_le_bytes())
}

fn u32_from_i32(value: i32) -> u32 {
    u32::from_le_bytes(value.to_le_bytes())
}

fn proto_u32(bytes: &[u8], field: u32) -> Option<u32> {
    anki::protobuf_varint(bytes, field)
        .and_then(|value| u32::try_from(value).ok())
}

fn proto_i32(bytes: &[u8], field: u32) -> Option<i32> {
    anki::protobuf_varint(bytes, field)
        .and_then(|value| u32::try_from(value).ok())
        .map(i32_from_u32)
}

fn proto_i64(bytes: &[u8], field: u32) -> Option<i64> {
    anki::protobuf_varint(bytes, field)
        .and_then(|value| i64::try_from(value).ok())
}

fn load_balancer(conn: &Connection) -> bool {
    let Ok(conf) =
        conn.query_row("SELECT conf FROM col WHERE id = 1", [], |row| {
            row.get::<_, SqlValue>(0)
        })
    else {
        return true;
    };
    match conf {
        SqlValue::Text(ref text) => parse_json(text, "conf")
            .ok()
            .and_then(|value| {
                value.get("loadBalancerEnabled").and_then(Json::as_bool)
            })
            .unwrap_or(true),
        SqlValue::Blob(ref bytes) => serde_json::from_slice::<Json>(bytes)
            .ok()
            .and_then(|value| {
                value.get("loadBalancerEnabled").and_then(Json::as_bool)
            })
            .unwrap_or(true),
        _ => true,
    }
}

/// Reads one preset from schema-11 JSON.
#[must_use]
pub fn preset_from_json(
    id: Id,
    value: &Json,
    load_balancer: bool,
) -> DeckPreset {
    let mut preset = DeckPreset {
        id,
        name: text(value.get("name")).unwrap_or_else(|| "Default".into()),
        mtime_secs: value.get("mod").and_then(Json::as_i64).unwrap_or_default(),
        usn: signed(value.get("usn")).unwrap_or_default(),
        load_balancer_enabled: load_balancer,
        raw_json: serde_json::to_string(value).ok(),
        ..DeckPreset::default()
    };
    let new = value.get("new");
    let rev = value.get("rev");
    let lapse = value.get("lapse");
    preset.learning_steps = floats(new.and_then(|value| value.get("delays")));
    if preset.learning_steps.is_empty() {
        preset.learning_steps = DeckPreset::default().learning_steps;
    }
    preset.relearning_steps =
        floats(lapse.and_then(|value| value.get("delays")));
    if preset.relearning_steps.is_empty() {
        preset.relearning_steps = DeckPreset::default().relearning_steps;
    }
    let intervals = new
        .and_then(|value| value.get("ints"))
        .and_then(Json::as_array);
    preset.graduating_interval_good = intervals
        .and_then(|items| items.first())
        .and_then(|item| unsigned(Some(item)))
        .unwrap_or(1);
    preset.graduating_interval_easy = intervals
        .and_then(|items| items.get(1))
        .and_then(|item| unsigned(Some(item)))
        .unwrap_or(4);
    if let Some(ease) =
        unsigned(new.and_then(|value| value.get("initialFactor")))
    {
        preset.initial_ease =
            f32::from(u16::try_from(ease).unwrap_or(2500)) / 1000.0;
    }
    preset.hard_multiplier =
        number(rev.and_then(|value| value.get("hardFactor"))).unwrap_or(1.2);
    preset.easy_multiplier =
        number(rev.and_then(|value| value.get("ease4"))).unwrap_or(1.3);
    preset.interval_multiplier =
        number(rev.and_then(|value| value.get("ivlFct"))).unwrap_or(1.0);
    preset.lapse_multiplier =
        number(lapse.and_then(|value| value.get("mult"))).unwrap_or_default();
    preset.maximum_interval =
        unsigned(rev.and_then(|value| value.get("maxIvl"))).unwrap_or(36_500);
    preset.minimum_lapse_interval =
        unsigned(lapse.and_then(|value| value.get("minInt"))).unwrap_or(1);
    preset.new_per_day =
        unsigned(new.and_then(|value| value.get("perDay"))).unwrap_or(20);
    preset.reviews_per_day =
        unsigned(rev.and_then(|value| value.get("perDay"))).unwrap_or(200);
    preset.new_per_day_minimum =
        unsigned(value.get("newPerDayMinimum")).unwrap_or_default();
    preset.bury_new = flag(new.and_then(|value| value.get("bury")), false);
    preset.bury_reviews = flag(rev.and_then(|value| value.get("bury")), false);
    preset.bury_interday_learning =
        flag(value.get("buryInterdayLearning"), false);
    let leech_action =
        unsigned(lapse.and_then(|value| value.get("leechAction"))).unwrap_or(1);
    preset.leech_action = LeechAction::from_code(leech_action);
    preset.leech_threshold =
        unsigned(lapse.and_then(|value| value.get("leechFails"))).unwrap_or(8);
    preset.new_card_insert_order =
        match unsigned(new.and_then(|value| value.get("order"))).unwrap_or(1) {
            0 => 1,
            _ => 0,
        };
    preset.new_card_gather_priority =
        unsigned(value.get("newGatherPriority")).unwrap_or_default();
    preset.new_card_sort_order =
        unsigned(value.get("newSortOrder")).unwrap_or_default();
    preset.review_order =
        unsigned(value.get("reviewOrder")).unwrap_or_default();
    preset.new_mix = unsigned(value.get("newMix")).unwrap_or_default();
    preset.interday_learning_mix =
        unsigned(value.get("interdayLearningMix")).unwrap_or_default();
    preset.fsrs_params_4 = floats(value.get("fsrsWeights"));
    preset.fsrs_params_5 = floats(value.get("fsrsParams5"));
    preset.fsrs_params_6 = floats(value.get("fsrsParams6"));
    preset.desired_retention =
        number(value.get("desiredRetention")).unwrap_or(0.9);
    preset.historical_retention =
        number(value.get("sm2Retention")).unwrap_or(0.9);
    let easy_days = floats(value.get("easyDaysPercentages"));
    preset.easy_days_percentages = if easy_days.is_empty() {
        vec![1.0; 7]
    } else {
        easy_days
    };
    preset.param_search = text(value.get("weightSearch")).unwrap_or_default();
    preset.ignore_revlogs_before_date =
        text(value.get("ignoreRevlogsBeforeDate")).unwrap_or_default();
    preset.max_taken_secs = unsigned(value.get("maxTaken")).unwrap_or(60);
    preset.disable_autoplay = !flag(value.get("autoplay"), true);
    preset.show_timer = unsigned(value.get("timer")).unwrap_or_default() != 0;
    preset.stop_timer_on_answer = flag(value.get("stopTimerOnAnswer"), false);
    preset.seconds_to_show_question =
        number(value.get("secondsToShowQuestion")).unwrap_or_default();
    preset.seconds_to_show_answer =
        number(value.get("secondsToShowAnswer")).unwrap_or_default();
    preset.question_action =
        unsigned(value.get("questionAction")).unwrap_or_default();
    preset.answer_action =
        unsigned(value.get("answerAction")).unwrap_or_default();
    preset.wait_for_audio = flag(value.get("waitForAudio"), true);
    preset.skip_question_when_replaying_answer =
        !flag(value.get("replayq"), true);
    preset
}

/// Reads one preset from a schema-18 `deck_config.config` protobuf.
#[must_use]
pub fn preset_from_proto(
    id: Id,
    name: String,
    mtime_secs: i64,
    usn: i32,
    bytes: &[u8],
    load_balancer: bool,
) -> DeckPreset {
    let defaults = DeckPreset::default();
    let mut preset = DeckPreset {
        id,
        name,
        mtime_secs,
        usn,
        load_balancer_enabled: load_balancer,
        raw: anki::protobuf_bytes(bytes, 255).unwrap_or_default(),
        ..defaults.clone()
    };
    preset.learning_steps = protobuf_steps(bytes, 1, &defaults.learning_steps);
    preset.relearning_steps =
        protobuf_steps(bytes, 2, &defaults.relearning_steps);
    preset.fsrs_params_4 = anki::protobuf_f32s(bytes, 3);
    preset.easy_days_percentages =
        protobuf_steps(bytes, 4, &defaults.easy_days_percentages);
    preset.fsrs_params_5 = anki::protobuf_f32s(bytes, 5);
    preset.fsrs_params_6 = anki::protobuf_f32s(bytes, 6);
    preset.new_per_day = valid_u32(
        proto_u32(bytes, 9).unwrap_or(defaults.new_per_day),
        defaults.new_per_day,
        0,
        9999,
    );
    preset.reviews_per_day = valid_u32(
        proto_u32(bytes, 10).unwrap_or(defaults.reviews_per_day),
        defaults.reviews_per_day,
        0,
        9999,
    );
    preset.new_per_day_minimum = proto_u32(bytes, 35).unwrap_or_default();
    preset.initial_ease = valid_f32(
        anki::protobuf_f32(bytes, 11).unwrap_or(defaults.initial_ease),
        defaults.initial_ease,
        1.31,
        5.0,
    );
    preset.easy_multiplier = valid_f32(
        anki::protobuf_f32(bytes, 12).unwrap_or(defaults.easy_multiplier),
        defaults.easy_multiplier,
        1.0,
        5.0,
    );
    preset.hard_multiplier = valid_f32(
        anki::protobuf_f32(bytes, 13).unwrap_or(defaults.hard_multiplier),
        defaults.hard_multiplier,
        0.5,
        1.3,
    );
    preset.lapse_multiplier = valid_f32(
        anki::protobuf_f32(bytes, 14).unwrap_or(defaults.lapse_multiplier),
        defaults.lapse_multiplier,
        0.0,
        1.0,
    );
    preset.interval_multiplier = valid_f32(
        anki::protobuf_f32(bytes, 15).unwrap_or(defaults.interval_multiplier),
        defaults.interval_multiplier,
        0.5,
        2.0,
    );
    preset.maximum_interval = valid_u32(
        proto_u32(bytes, 16).unwrap_or(defaults.maximum_interval),
        defaults.maximum_interval,
        1,
        36_500,
    );
    preset.minimum_lapse_interval = valid_u32(
        proto_u32(bytes, 17).unwrap_or(defaults.minimum_lapse_interval),
        defaults.minimum_lapse_interval,
        1,
        36_500,
    );
    preset.graduating_interval_good = valid_u32(
        proto_u32(bytes, 18).unwrap_or(defaults.graduating_interval_good),
        defaults.graduating_interval_good,
        1,
        36_500,
    );
    preset.graduating_interval_easy = valid_u32(
        proto_u32(bytes, 19).unwrap_or(defaults.graduating_interval_easy),
        defaults.graduating_interval_easy,
        1,
        36_500,
    );
    preset.new_card_insert_order =
        proto_u32(bytes, 20).unwrap_or(defaults.new_card_insert_order);
    preset.new_card_gather_priority =
        proto_u32(bytes, 34).unwrap_or(defaults.new_card_gather_priority);
    preset.new_card_sort_order =
        proto_u32(bytes, 32).unwrap_or(defaults.new_card_sort_order);
    preset.new_mix = proto_u32(bytes, 30).unwrap_or(defaults.new_mix);
    preset.review_order = proto_u32(bytes, 33).unwrap_or(defaults.review_order);
    preset.interday_learning_mix =
        proto_u32(bytes, 31).unwrap_or(defaults.interday_learning_mix);
    let leech_action =
        proto_u32(bytes, 21).unwrap_or(defaults.leech_action.code());
    preset.leech_action = LeechAction::from_code(leech_action);
    preset.leech_threshold = valid_u32(
        proto_u32(bytes, 22).unwrap_or(defaults.leech_threshold),
        defaults.leech_threshold,
        1,
        9999,
    );
    preset.disable_autoplay =
        anki::protobuf_varint(bytes, 23).unwrap_or_default() != 0;
    preset.max_taken_secs = valid_u32(
        proto_u32(bytes, 24).unwrap_or(defaults.max_taken_secs),
        defaults.max_taken_secs,
        1,
        9999,
    );
    preset.show_timer =
        anki::protobuf_varint(bytes, 25).unwrap_or_default() != 0;
    preset.stop_timer_on_answer =
        anki::protobuf_varint(bytes, 38).unwrap_or_default() != 0;
    preset.seconds_to_show_question =
        anki::protobuf_f32(bytes, 41).unwrap_or_default();
    preset.seconds_to_show_answer =
        anki::protobuf_f32(bytes, 42).unwrap_or_default();
    preset.question_action =
        proto_u32(bytes, 36).unwrap_or(defaults.question_action);
    preset.answer_action =
        proto_u32(bytes, 43).unwrap_or(defaults.answer_action);
    preset.wait_for_audio = anki::protobuf_varint(bytes, 44)
        .map_or(defaults.wait_for_audio, |value| value != 0);
    preset.skip_question_when_replaying_answer =
        anki::protobuf_varint(bytes, 26).unwrap_or_default() != 0;
    preset.bury_new = anki::protobuf_varint(bytes, 27).unwrap_or_default() != 0;
    preset.bury_reviews =
        anki::protobuf_varint(bytes, 28).unwrap_or_default() != 0;
    preset.bury_interday_learning =
        anki::protobuf_varint(bytes, 29).unwrap_or_default() != 0;
    preset.desired_retention = valid_f32(
        anki::protobuf_f32(bytes, 37).unwrap_or(defaults.desired_retention),
        defaults.desired_retention,
        0.7,
        0.99,
    );
    preset.historical_retention = valid_f32(
        anki::protobuf_f32(bytes, 40).unwrap_or(defaults.historical_retention),
        defaults.historical_retention,
        0.7,
        0.97,
    );
    preset.param_search = anki::protobuf_string(bytes, 45).unwrap_or_default();
    preset.ignore_revlogs_before_date =
        anki::protobuf_string(bytes, 46).unwrap_or_default();
    preset
}

fn protobuf_steps(bytes: &[u8], field: u32, default: &[f32]) -> Vec<f32> {
    let values = anki::protobuf_f32s(bytes, field);
    if values.is_empty() {
        default.to_vec()
    } else {
        values
    }
}

/// Encodes a preset into the schema-11 `dconf` JSON shape.
#[must_use]
pub fn preset_to_json(preset: &DeckPreset) -> Json {
    let mut entry = preset
        .raw_json
        .as_deref()
        .and_then(|json| parse_json(json, "deck config").ok())
        .unwrap_or_else(|| {
            schema::schema11_default_deck_config(preset.mtime_secs)
        });
    let Some(object) = entry.as_object_mut() else {
        return entry;
    };
    object.insert("id".into(), json!(preset.id));
    object.insert("name".into(), json!(preset.name));
    object.insert("mod".into(), json!(preset.mtime_secs));
    object.insert("usn".into(), json!(preset.usn));
    object.insert("maxTaken".into(), json!(preset.max_taken_secs));
    object.insert("autoplay".into(), json!(!preset.disable_autoplay));
    object.insert("timer".into(), json!(u32::from(preset.show_timer)));
    object.insert(
        "replayq".into(),
        json!(!preset.skip_question_when_replaying_answer),
    );
    let new = object.entry("new").or_insert_with(|| json!({}));
    if let Some(new) = new.as_object_mut() {
        new.insert("bury".into(), json!(preset.bury_new));
        new.insert("delays".into(), json!(preset.learning_steps));
        new.insert(
            "initialFactor".into(),
            json!(u32::from(f32_to_ease(preset.initial_ease))),
        );
        new.insert(
            "ints".into(),
            json!([
                preset.graduating_interval_good,
                preset.graduating_interval_easy,
                0
            ]),
        );
        new.insert(
            "order".into(),
            json!(u32::from(preset.new_card_insert_order == 0)),
        );
        new.insert("perDay".into(), json!(preset.new_per_day));
    }
    let rev = object.entry("rev").or_insert_with(|| json!({}));
    if let Some(rev) = rev.as_object_mut() {
        rev.insert("bury".into(), json!(preset.bury_reviews));
        rev.insert("ease4".into(), json!(preset.easy_multiplier));
        rev.insert("hardFactor".into(), json!(preset.hard_multiplier));
        rev.insert("ivlFct".into(), json!(preset.interval_multiplier));
        rev.insert("maxIvl".into(), json!(preset.maximum_interval));
        rev.insert("perDay".into(), json!(preset.reviews_per_day));
    }
    let lapse = object.entry("lapse").or_insert_with(|| json!({}));
    if let Some(lapse) = lapse.as_object_mut() {
        lapse.insert("delays".into(), json!(preset.relearning_steps));
        lapse.insert("leechAction".into(), json!(preset.leech_action.code()));
        lapse.insert("leechFails".into(), json!(preset.leech_threshold));
        lapse.insert("minInt".into(), json!(preset.minimum_lapse_interval));
        lapse.insert("mult".into(), json!(preset.lapse_multiplier));
    }
    object.insert("newMix".into(), json!(preset.new_mix));
    object.insert("newPerDayMinimum".into(), json!(preset.new_per_day_minimum));
    object.insert(
        "interdayLearningMix".into(),
        json!(preset.interday_learning_mix),
    );
    object.insert("reviewOrder".into(), json!(preset.review_order));
    object.insert("newSortOrder".into(), json!(preset.new_card_sort_order));
    object.insert(
        "newGatherPriority".into(),
        json!(preset.new_card_gather_priority),
    );
    object.insert(
        "buryInterdayLearning".into(),
        json!(preset.bury_interday_learning),
    );
    object.insert(
        "stopTimerOnAnswer".into(),
        json!(preset.stop_timer_on_answer),
    );
    object.insert(
        "secondsToShowQuestion".into(),
        json!(preset.seconds_to_show_question),
    );
    object.insert(
        "secondsToShowAnswer".into(),
        json!(preset.seconds_to_show_answer),
    );
    object.insert("questionAction".into(), json!(preset.question_action));
    object.insert("answerAction".into(), json!(preset.answer_action));
    object.insert("waitForAudio".into(), json!(preset.wait_for_audio));
    object.insert(
        "easyDaysPercentages".into(),
        json!(preset.easy_days_percentages),
    );
    object.insert("fsrsWeights".into(), json!(preset.fsrs_params_4));
    object.insert("fsrsParams5".into(), json!(preset.fsrs_params_5));
    object.insert("fsrsParams6".into(), json!(preset.fsrs_params_6));
    object.insert("desiredRetention".into(), json!(preset.desired_retention));
    object.insert("sm2Retention".into(), json!(preset.historical_retention));
    object.insert("weightSearch".into(), json!(preset.param_search));
    object.insert(
        "ignoreRevlogsBeforeDate".into(),
        json!(preset.ignore_revlogs_before_date),
    );
    entry
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "retention is stored as an integer percentage"
)]
fn retention_percent(retention: f32) -> i64 {
    (f64::from(retention) * 100.0).round() as i64
}

fn f32_to_ease(ease: f32) -> u16 {
    let scaled = f64::from(ease) * 1000.0;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "initial ease is stored in thousands"
    )]
    let rounded = scaled.round() as i64;
    u16::try_from(rounded).unwrap_or(2500)
}

/// Encodes a preset into the schema-18 `deck_config.config` protobuf.
#[must_use]
pub fn preset_to_proto(preset: &DeckPreset) -> Vec<u8> {
    let mut bytes = Vec::new();
    anki::put_f32s(&mut bytes, 1, &preset.learning_steps);
    anki::put_f32s(&mut bytes, 2, &preset.relearning_steps);
    anki::put_f32s(&mut bytes, 3, &preset.fsrs_params_4);
    anki::put_f32s(&mut bytes, 4, &preset.easy_days_percentages);
    anki::put_f32s(&mut bytes, 5, &preset.fsrs_params_5);
    anki::put_f32s(&mut bytes, 6, &preset.fsrs_params_6);
    put_u32(&mut bytes, 9, preset.new_per_day);
    put_u32(&mut bytes, 10, preset.reviews_per_day);
    anki::put_f32(&mut bytes, 11, preset.initial_ease);
    anki::put_f32(&mut bytes, 12, preset.easy_multiplier);
    anki::put_f32(&mut bytes, 13, preset.hard_multiplier);
    anki::put_f32(&mut bytes, 14, preset.lapse_multiplier);
    anki::put_f32(&mut bytes, 15, preset.interval_multiplier);
    put_u32(&mut bytes, 16, preset.maximum_interval);
    put_u32(&mut bytes, 17, preset.minimum_lapse_interval);
    put_u32(&mut bytes, 18, preset.graduating_interval_good);
    put_u32(&mut bytes, 19, preset.graduating_interval_easy);
    put_u32(&mut bytes, 20, preset.new_card_insert_order);
    put_u32(&mut bytes, 21, preset.leech_action.code());
    put_u32(&mut bytes, 22, preset.leech_threshold);
    put_bool(&mut bytes, 23, preset.disable_autoplay);
    put_u32(&mut bytes, 24, preset.max_taken_secs);
    put_bool(&mut bytes, 25, preset.show_timer);
    put_bool(&mut bytes, 26, preset.skip_question_when_replaying_answer);
    put_bool(&mut bytes, 27, preset.bury_new);
    put_bool(&mut bytes, 28, preset.bury_reviews);
    put_bool(&mut bytes, 29, preset.bury_interday_learning);
    put_u32(&mut bytes, 30, preset.new_mix);
    put_u32(&mut bytes, 31, preset.interday_learning_mix);
    put_u32(&mut bytes, 32, preset.new_card_sort_order);
    put_u32(&mut bytes, 33, preset.review_order);
    put_u32(&mut bytes, 34, preset.new_card_gather_priority);
    put_u32(&mut bytes, 35, preset.new_per_day_minimum);
    put_u32(&mut bytes, 36, preset.question_action);
    anki::put_f32(&mut bytes, 37, preset.desired_retention);
    put_bool(&mut bytes, 38, preset.stop_timer_on_answer);
    anki::put_f32(&mut bytes, 40, preset.historical_retention);
    anki::put_f32(&mut bytes, 41, preset.seconds_to_show_question);
    anki::put_f32(&mut bytes, 42, preset.seconds_to_show_answer);
    put_u32(&mut bytes, 43, preset.answer_action);
    put_bool(&mut bytes, 44, preset.wait_for_audio);
    anki::put_bytes(&mut bytes, 45, preset.param_search.as_bytes());
    anki::put_bytes(
        &mut bytes,
        46,
        preset.ignore_revlogs_before_date.as_bytes(),
    );
    if !preset.raw.is_empty() {
        anki::put_bytes(&mut bytes, 255, &preset.raw);
    }
    bytes
}

fn put_u32(bytes: &mut Vec<u8>, field: u32, value: u32) {
    anki::put_varint(bytes, field, u64::from(value));
}

fn put_bool(bytes: &mut Vec<u8>, field: u32, value: bool) {
    anki::put_varint(bytes, field, u64::from(value));
}

fn day_limit(bytes: &[u8], studied: u32) -> DayLimit {
    DayLimit {
        limit: Some(proto_u32(bytes, 1).unwrap_or_default()),
        day: proto_u32(bytes, 2).unwrap_or_default(),
        today: studied,
    }
}

/// `newLimitToday` and `reviewLimitToday` are `[limit, day]`, not counts.
fn json_day_limit(
    entry: Option<&Json>,
    studied_day: u32,
    studied: u32,
) -> DayLimit {
    let valid = entry.and_then(Json::as_array).is_some();
    let (limit, day) = pair(entry);
    DayLimit {
        limit: valid.then_some(limit),
        day: if valid { day } else { studied_day },
        today: studied,
    }
}

/// Reads a deck from its schema-11 JSON entry.
#[must_use]
pub fn deck_from_json(id: Id, value: &Json) -> EffectiveDeck {
    let preset_id = value.get("conf").and_then(Json::as_i64).unwrap_or(1);
    let dynamic = value.get("dyn").and_then(Json::as_i64).unwrap_or_default();
    let (new_day, new_amount) = pair(value.get("newToday"));
    let (review_day, review_amount) = pair(value.get("revToday"));
    let (learning_day, learning_amount) = pair(value.get("lrnToday"));
    let (time_day, time_amount) = pair(value.get("timeToday"));
    let combined_day = new_day.max(review_day).max(learning_day).max(time_day);
    let new_amount = if new_day == combined_day {
        new_amount
    } else {
        0
    };
    let review_amount = if review_day == combined_day {
        review_amount
    } else {
        0
    };
    let learning_amount = if learning_day == combined_day {
        learning_amount
    } else {
        0
    };
    let time_amount = if time_day == combined_day {
        time_amount
    } else {
        0
    };
    let desired_retention = unsigned(value.get("desiredRetention"))
        .map(|percent| f32::from(u16::try_from(percent).unwrap_or(90)) / 100.0);
    EffectiveDeck {
        id,
        name: text(value.get("name")).unwrap_or_default(),
        description: text(value.get("desc")).unwrap_or_default(),
        kind: DeckKind::from_code(i32::try_from(dynamic).unwrap_or_default()),
        preset_id,
        common: DeckCommon {
            last_day_studied: combined_day,
            new_studied: i32_from_u32(new_amount),
            review_studied: i32_from_u32(review_amount),
            learning_studied: i32_from_u32(learning_amount),
            milliseconds_studied: i32_from_u32(time_amount),
            study_collapsed: value
                .get("collapsed")
                .and_then(Json::as_bool)
                .unwrap_or_default(),
            browser_collapsed: value
                .get("browserCollapsed")
                .and_then(Json::as_bool)
                .unwrap_or_default(),
        },
        overrides: DeckOverrides {
            new_limit: unsigned(value.get("newLimit")),
            review_limit: unsigned(value.get("reviewLimit")),
            desired_retention,
            extend_new: unsigned(value.get("extendNew")).unwrap_or_default(),
            extend_review: unsigned(value.get("extendRev")).unwrap_or_default(),
            new_today: json_day_limit(
                value.get("newLimitToday"),
                combined_day,
                new_amount,
            ),
            review_today: json_day_limit(
                value.get("reviewLimitToday"),
                combined_day,
                review_amount,
            ),
        },
        markdown_description: value
            .get("md")
            .and_then(Json::as_bool)
            .unwrap_or_default(),
        raw_json: serde_json::to_string(value).ok(),
        raw_common: Vec::new(),
        raw_kind: None,
    }
}

/// Reads a deck from schema-18 `decks.common` and `decks.kind` blobs.
#[must_use]
pub fn deck_from_proto(
    id: Id,
    name: &str,
    common: &[u8],
    kind: &[u8],
) -> EffectiveDeck {
    let default = EffectiveDeck::default();
    let normal = anki::protobuf_bytes(kind, 1);
    let filtered = anki::protobuf_bytes(kind, 2);
    let normal = normal.as_deref().unwrap_or_default();
    let mut deck = EffectiveDeck {
        id,
        name: crate::anki::decode_deck_name(name),
        kind: if filtered.is_some() {
            DeckKind::Filtered
        } else {
            DeckKind::Normal
        },
        raw_common: anki::protobuf_bytes(common, 255).unwrap_or_default(),
        raw_kind: filtered,
        ..default
    };
    deck.common = DeckCommon {
        study_collapsed: anki::protobuf_varint(common, 1).unwrap_or_default()
            != 0,
        browser_collapsed: anki::protobuf_varint(common, 2).unwrap_or_default()
            != 0,
        last_day_studied: proto_u32(common, 3).unwrap_or_default(),
        new_studied: proto_i32(common, 4).unwrap_or_default(),
        review_studied: proto_i32(common, 5).unwrap_or_default(),
        learning_studied: proto_i32(common, 6).unwrap_or_default(),
        milliseconds_studied: proto_i32(common, 7).unwrap_or_default(),
    };
    if anki::protobuf_bytes(kind, 1).is_some() {
        deck.preset_id = proto_i64(normal, 1).unwrap_or(1);
        deck.description = anki::protobuf_string(normal, 4).unwrap_or_default();
        deck.markdown_description =
            anki::protobuf_varint(normal, 5).unwrap_or_default() != 0;
        deck.overrides = DeckOverrides {
            extend_new: proto_u32(normal, 2).unwrap_or_default(),
            extend_review: proto_u32(normal, 3).unwrap_or_default(),
            review_limit: proto_u32(normal, 6),
            new_limit: proto_u32(normal, 7),
            desired_retention: anki::protobuf_f32(normal, 10),
            new_today: if let Some(bytes) = anki::protobuf_bytes(normal, 9) {
                day_limit(&bytes, u32_from_i32(deck.common.new_studied))
            } else {
                DayLimit {
                    limit: None,
                    today: u32_from_i32(deck.common.new_studied),
                    day: deck.common.last_day_studied,
                }
            },
            review_today: if let Some(bytes) = anki::protobuf_bytes(normal, 8) {
                day_limit(&bytes, u32_from_i32(deck.common.review_studied))
            } else {
                DayLimit {
                    limit: None,
                    today: u32_from_i32(deck.common.review_studied),
                    day: deck.common.last_day_studied,
                }
            },
        };
    }
    deck
}

/// Encodes a deck into the schema-18 `common` and `kind` blobs.
#[must_use]
pub fn deck_to_proto(deck: &EffectiveDeck) -> (Vec<u8>, Vec<u8>) {
    let mut common = Vec::new();
    put_bool(&mut common, 1, deck.common.study_collapsed);
    put_bool(&mut common, 2, deck.common.browser_collapsed);
    put_u32(&mut common, 3, deck.common.last_day_studied);
    put_i32(&mut common, 4, deck.common.new_studied);
    put_i32(&mut common, 5, deck.common.review_studied);
    put_i32(&mut common, 6, deck.common.learning_studied);
    put_i32(&mut common, 7, deck.common.milliseconds_studied);
    if !deck.raw_common.is_empty() {
        anki::put_bytes(&mut common, 255, &deck.raw_common);
    }
    if deck.kind == DeckKind::Filtered {
        let filtered = deck.raw_kind.clone().unwrap_or_else(|| {
            anki::protobuf_bytes(&[], 2).unwrap_or_default()
        });
        let mut kind = Vec::new();
        anki::put_bytes(&mut kind, 2, &filtered);
        return (common, kind);
    }
    let mut normal = Vec::new();
    anki::put_varint(
        &mut normal,
        1,
        u64::try_from(deck.preset_id).unwrap_or(1),
    );
    put_u32(&mut normal, 2, deck.overrides.extend_new);
    put_u32(&mut normal, 3, deck.overrides.extend_review);
    anki::put_bytes(&mut normal, 4, deck.description.as_bytes());
    put_bool(&mut normal, 5, deck.markdown_description);
    if let Some(limit) = deck.overrides.review_limit {
        put_u32(&mut normal, 6, limit);
    }
    if let Some(limit) = deck.overrides.new_limit {
        put_u32(&mut normal, 7, limit);
    }
    put_day_limit(&mut normal, 8, &deck.overrides.review_today);
    put_day_limit(&mut normal, 9, &deck.overrides.new_today);
    if let Some(retention) = deck.overrides.desired_retention {
        anki::put_f32(&mut normal, 10, retention);
    }
    let mut kind = Vec::new();
    anki::put_bytes(&mut kind, 1, &normal);
    (common, kind)
}

fn put_i32(bytes: &mut Vec<u8>, field: u32, value: i32) {
    anki::put_varint(bytes, field, u64::from(u32_from_i32(value)));
}

fn put_day_limit(bytes: &mut Vec<u8>, field: u32, limit: &DayLimit) {
    if limit.limit.is_none() {
        return;
    }
    let mut blob = Vec::new();
    if let Some(value) = limit.limit {
        put_u32(&mut blob, 1, value);
    }
    put_u32(&mut blob, 2, limit.day);
    anki::put_bytes(bytes, field, &blob);
}

/// Preserves unknown keys from a schema-11 deck entry.
#[must_use]
pub fn deck_to_json(deck: &EffectiveDeck) -> Json {
    let mut entry = deck
        .raw_json
        .as_deref()
        .and_then(|json| parse_json(json, "deck").ok())
        .unwrap_or_else(|| json!({}));
    let Some(object) = entry.as_object_mut() else {
        return entry;
    };
    object.insert("name".into(), json!(deck.name));
    object.insert("conf".into(), json!(deck.preset_id));
    object.insert("desc".into(), json!(deck.description));
    object.insert("dyn".into(), json!(deck.kind.code()));
    object.insert("extendNew".into(), json!(deck.overrides.extend_new));
    object.insert("extendRev".into(), json!(deck.overrides.extend_review));
    object.insert(
        "newToday".into(),
        json!([
            deck.common.last_day_studied,
            u32_from_i32(deck.common.new_studied)
        ]),
    );
    object.insert(
        "revToday".into(),
        json!([
            deck.common.last_day_studied,
            u32_from_i32(deck.common.review_studied)
        ]),
    );
    if let Some(limit) = deck.overrides.new_limit {
        object.insert("newLimit".into(), json!(limit));
    }
    if let Some(limit) = deck.overrides.review_limit {
        object.insert("reviewLimit".into(), json!(limit));
    }
    if let Some(limit) = deck.overrides.new_today.limit {
        object.insert(
            "newLimitToday".into(),
            json!([limit, deck.overrides.new_today.day]),
        );
    }
    if let Some(limit) = deck.overrides.review_today.limit {
        object.insert(
            "reviewLimitToday".into(),
            json!([limit, deck.overrides.review_today.day]),
        );
    }
    if let Some(retention) = deck.overrides.desired_retention {
        object.insert(
            "desiredRetention".into(),
            json!(u32::from(
                u16::try_from(retention_percent(retention)).unwrap_or(90)
            )),
        );
    }
    if deck.markdown_description {
        object.insert("md".into(), json!(true));
    }
    entry
}

/// Reads all presets from the collection's active storage layout.
pub fn presets(conn: &Connection) -> Result<Vec<DeckPreset>, StoreError> {
    let balancer = load_balancer(conn);
    if !normalized(conn)? {
        let json = conn
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get::<_, SqlValue>(0)
            })
            .map_err(crate::backend)?;
        let text = match json {
            SqlValue::Text(text) => text,
            SqlValue::Blob(bytes) => {
                String::from_utf8_lossy(&bytes).into_owned()
            }
            _ => String::new(),
        };
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let value = parse_json(&text, "dconf")?;
        let Some(object) = value.as_object() else {
            return Err(invalid("dconf is not an object"));
        };
        let mut presets = Vec::with_capacity(object.len());
        for (key, entry) in object {
            let id = key
                .parse::<Id>()
                .map_err(|_| invalid("invalid deck config id"))?;
            presets.push(preset_from_json(id, entry, balancer));
        }
        presets.sort_unstable_by_key(|preset| preset.id);
        return Ok(presets);
    }
    let mut statement = conn
        .prepare("SELECT id, name, mtime_secs, usn, config FROM deck_config")
        .map_err(crate::backend)?;
    let rows = statement
        .query_map([], |row| {
            let config: SqlValue = row.get(4)?;
            Ok((
                row.get::<_, Id>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                i32::try_from(row.get::<_, i64>(3)?).unwrap_or_default(),
                config,
            ))
        })
        .map_err(crate::backend)?;
    let mut presets = Vec::new();
    for row in rows {
        let (id, name, mtime, usn, value) = row.map_err(crate::backend)?;
        let bytes = match value {
            SqlValue::Blob(bytes) => bytes,
            _ => Vec::new(),
        };
        presets.push(preset_from_proto(id, name, mtime, usn, &bytes, balancer));
    }
    presets.sort_unstable_by_key(|preset| preset.id);
    Ok(presets)
}

/// Reads all effective decks from the collection's active storage layout.
pub fn effective_decks(
    conn: &Connection,
) -> Result<Vec<EffectiveDeck>, StoreError> {
    if normalized(conn)? {
        return decks_from_tables(conn);
    }
    let json = records::read_col_decks(conn)?;
    let value = parse_json(&json, "decks")?;
    let Some(object) = value.as_object() else {
        return Err(invalid("decks is not an object"));
    };
    let mut decks = Vec::with_capacity(object.len());
    for (key, entry) in object {
        let id = key.parse::<Id>().map_err(|_| invalid("invalid deck id"))?;
        decks.push(deck_from_json(id, entry));
    }
    decks.sort_unstable_by_key(|deck| deck.id);
    Ok(decks)
}

fn decks_from_tables(
    conn: &Connection,
) -> Result<Vec<EffectiveDeck>, StoreError> {
    let mut statement = conn
        .prepare("SELECT id, name, common, kind FROM decks")
        .map_err(crate::backend)?;
    let rows = statement
        .query_map([], |row| {
            let common: SqlValue = row.get(2)?;
            let kind: SqlValue = row.get(3)?;
            Ok((row.get::<_, Id>(0)?, row.get::<_, String>(1)?, common, kind))
        })
        .map_err(crate::backend)?;
    let mut decks = Vec::new();
    for row in rows {
        let (id, name, common, kind) = row.map_err(crate::backend)?;
        let blob = |value: SqlValue| match value {
            SqlValue::Blob(bytes) => bytes,
            _ => Vec::new(),
        };
        decks.push(deck_from_proto(id, &name, &blob(common), &blob(kind)));
    }
    decks.sort_unstable_by_key(|deck| deck.id);
    Ok(decks)
}
fn atomic_write(
    conn: &Connection,
    write: impl FnOnce(&Connection) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if conn.is_autocommit() {
        let transaction =
            conn.unchecked_transaction().map_err(crate::backend)?;
        write(&transaction)?;
        transaction.commit().map_err(crate::backend)
    } else {
        write(conn)
    }
}

/// Writes one preset back to whatever layout the collection uses.
pub fn write_preset(
    conn: &Connection,
    preset: &DeckPreset,
) -> Result<(), StoreError> {
    if normalized(conn)? {
        atomic_write(conn, |conn| {
            let col = records::read_col(conn)?;
            let stamp = crate::col_stamp(&col)?;
            conn.execute(
                "UPDATE deck_config SET name = ?1, mtime_secs = ?2, usn = ?3, config = ?4 WHERE id = ?5",
                params![
                    preset.name,
                    preset.mtime_secs,
                    preset.usn,
                    preset_to_proto(preset),
                    preset.id,
                ],
            )
            .map_err(crate::backend)?;
            conn.execute(
                "UPDATE col SET mod = ?1 WHERE id = 1",
                [stamp.modified],
            )
            .map_err(crate::backend)?;
            Ok(())
        })?;
    } else {
        atomic_write(conn, |conn| {
            let col = records::read_col(conn)?;
            let stamp = crate::col_stamp(&col)?;
            let mut decks = parse_json(&col.dconf, "dconf")?;
            let Some(object) = decks.as_object_mut() else {
                return Err(invalid("dconf is not an object"));
            };
            object.insert(preset.id.to_string(), preset_to_json(preset));
            let json = serde_json::to_string(&decks)
                .map_err(|error| invalid(error.to_string()))?;
            conn.execute(
                "UPDATE col SET mod = ?1, scm = ?2, dconf = ?3 WHERE id = 1",
                params![stamp.modified, stamp.scm, json],
            )
            .map_err(crate::backend)?;
            Ok(())
        })?;
    }
    Ok(())
}

/// Writes one deck back to whatever layout the collection uses.
pub fn write_deck(
    conn: &Connection,
    deck: &EffectiveDeck,
) -> Result<(), StoreError> {
    if normalized(conn)? {
        let (common, kind) = deck_to_proto(deck);
        atomic_write(conn, |conn| {
            let col = records::read_col(conn)?;
            let stamp = crate::col_stamp(&col)?;
            conn.execute(
                "UPDATE decks SET name = ?1, mtime_secs = ?2, usn = ?3, common = ?4, kind = ?5 WHERE id = ?6",
                params![
                    crate::anki::encode_deck_name(&deck.name),
                    stamp.now_secs,
                    stamp.usn,
                    common,
                    kind,
                    deck.id,
                ],
            )
            .map_err(crate::backend)?;
            conn.execute(
                "UPDATE col SET mod = ?1 WHERE id = 1",
                [stamp.modified],
            )
            .map_err(crate::backend)?;
            Ok(())
        })?;
        return Ok(());
    }
    atomic_write(conn, |conn| {
        let col = records::read_col(conn)?;
        let stamp = crate::col_stamp(&col)?;
        let mut decks = parse_json(&col.decks, "decks")?;
        let Some(object) = decks.as_object_mut() else {
            return Err(invalid("decks is not an object"));
        };
        object.insert(deck.id.to_string(), deck_to_json(deck));
        let json = serde_json::to_string(&decks)
            .map_err(|error| invalid(error.to_string()))?;
        conn.execute(
            "UPDATE col SET mod = ?1, scm = ?2, decks = ?3 WHERE id = 1",
            params![stamp.modified, stamp.scm, json],
        )
        .map_err(crate::backend)?;
        Ok(())
    })?;
    Ok(())
}
