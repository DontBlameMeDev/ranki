//! Shared configuration schema, validation, and resolved settings.

#![expect(
    clippy::unnecessary_wraps,
    reason = "serde field validators must return Result"
)]

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::Id;
/// Which visual theme the review window should use.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Dark,
    Light,
    #[default]
    System,
}

/// Screen the window restores on the next launch.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum UiView {
    #[default]
    Decks,
    Browse,
    Review,
    AddCard,
    Settings,
    Statistics,
}

/// Window layout stored by the GUI in its own state file.
///
/// This is deliberately separate from `config.toml`, which may be managed by
/// Home Manager. Every field is optional in serialized form so older files and
/// hand-written files keep working.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub view: UiView,
    pub previous_view: UiView,
    /// `None` lets the configured theme win.
    pub theme: Option<ThemeChoice>,
    /// Clamped to the rail's range when the window opens.
    pub nav_width: Option<f32>,
    /// Deck id, rather than a row index, so reordering cannot change the pick.
    pub selected_deck: Option<Id>,
    pub expanded_decks: Vec<String>,
    pub deck_query: String,
    pub browse_query: String,
    pub review_all_decks: bool,
}

/// Files owned by Ranki inside one application data directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPaths {
    pub collection: PathBuf,
    pub media_dir: PathBuf,
    pub sync_state: PathBuf,
}

impl DataPaths {
    /// Builds Ranki's storage layout under an app-supplied data directory.
    #[must_use]
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let root = data_dir.into();
        Self {
            collection: root.join("collection.anki2"),
            media_dir: root.join("collection.media"),
            sync_state: root.join("ranki-sync.json"),
        }
    }
}

impl Default for DataPaths {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}
/// Settings after CLI flags, config, and defaults are merged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "settings mirror independently toggled CLI and sync options"
)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub paths: DataPaths,
    /// Legacy benchmark fields. The benchmark harness is shared with older
    /// commits, so these stay for the performance workflow. Production code
    /// uses `paths`.
    pub collection: PathBuf,
    pub media_dir: PathBuf,
    pub theme: ThemeChoice,
    pub font_size: f32,
    pub review_batch_size: usize,
    pub show_keyboard_hints: bool,
    /// `None` follows the desktop animation preference.
    pub reduce_motion: Option<bool>,
    pub desired_retention: f32,
    pub auto_optimize: bool,
    pub fsrs_params: Vec<f32>,
    pub fsrs_decay: f32,
    pub last_optimized: u32,
    pub reviews_since: u32,
    pub sync: SyncSettings,
    pub study: StudyConfig,
    pub ui_state: UiState,
}

/// Resolved sync settings. Passwords are never part of this struct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "sync has independent enable and automatic flags"
)]
pub struct SyncSettings {
    pub enabled: bool,
    pub auto: bool,
    pub username: Option<String>,
    pub hkey_file: Option<PathBuf>,
    pub retries: u8,
    pub backoff_ms: u64,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto: true,
            username: None,
            hkey_file: None,
            retries: 2,
            backoff_ms: 1_000,
        }
    }
}

/// Local study preferences stored in `[study]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StudyConfig {
    #[serde(deserialize_with = "leech_enabled_field")]
    pub leech_enabled: bool,
    #[serde(deserialize_with = "again_field")]
    pub again: String,
    #[serde(deserialize_with = "hard_field")]
    pub hard: String,
    #[serde(deserialize_with = "good_field")]
    pub good: String,
    #[serde(deserialize_with = "easy_field")]
    pub easy: String,
}

impl Default for StudyConfig {
    fn default() -> Self {
        Self {
            leech_enabled: true,
            again: String::from("1"),
            hard: String::from("2"),
            good: String::from("3"),
            easy: String::from("4"),
        }
    }
}

impl StudyConfig {
    /// Key shown for one answer rating.
    #[must_use]
    pub fn answer_key(&self, rating: crate::Rating) -> &str {
        match rating {
            crate::Rating::Again => &self.again,
            crate::Rating::Hard => &self.hard,
            crate::Rating::Good => &self.good,
            crate::Rating::Easy => &self.easy,
        }
    }

    /// Sets the key shown for one answer rating.
    pub fn set_answer_key(
        &mut self,
        rating: crate::Rating,
        key: impl Into<String>,
    ) {
        match rating {
            crate::Rating::Again => self.again = key.into(),
            crate::Rating::Hard => self.hard = key.into(),
            crate::Rating::Good => self.good = key.into(),
            crate::Rating::Easy => self.easy = key.into(),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            paths: DataPaths::default(),
            collection: PathBuf::new(),
            media_dir: PathBuf::new(),
            theme: ThemeChoice::System,
            font_size: 16.0,
            review_batch_size: 20,
            show_keyboard_hints: true,
            reduce_motion: None,
            desired_retention: 0.9,
            auto_optimize: true,
            fsrs_params: Vec::new(),
            fsrs_decay: 0.1542,
            last_optimized: 0,
            reviews_since: 0,
            sync: SyncSettings::default(),
            study: StudyConfig::default(),
            ui_state: UiState::default(),
        }
    }
}
impl Settings {
    /// Resolves preferences while keeping storage paths app-owned.
    #[must_use]
    pub fn resolve(config: &Config, paths: DataPaths) -> Self {
        Self {
            paths,
            collection: PathBuf::new(),
            media_dir: PathBuf::new(),
            theme: config.ui.theme,
            font_size: config.ui.font_size,
            review_batch_size: config.ui.review_batch_size,
            show_keyboard_hints: config.ui.show_keyboard_hints,
            reduce_motion: config.ui.reduce_motion,
            desired_retention: config.scheduler.desired_retention,
            auto_optimize: config.scheduler.auto_optimize,
            fsrs_params: config.scheduler.params.clone(),
            fsrs_decay: config.scheduler.decay,
            last_optimized: config.scheduler.last_optimized,
            reviews_since: config.scheduler.reviews_since,
            sync: SyncSettings {
                enabled: config.sync.enabled,
                auto: config.sync.auto,
                username: config.sync.username.clone(),
                hkey_file: config.sync.hkey_file.as_ref().map(PathBuf::from),
                retries: config.sync.retries,
                backoff_ms: config.sync.backoff_ms,
            },
            study: config.study.clone(),
            ui_state: UiState::default(),
        }
    }
}
/// Raw TOML configuration. Unknown keys and out-of-range values fall back to
/// defaults with a warning naming the full key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub scheduler: SchedulerConfig,
    pub sync: SyncConfig,
    pub study: StudyConfig,
}

/// Raw sync settings. The host key is a path or environment value only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "sync has independent enable and automatic flags"
)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    #[serde(deserialize_with = "sync_enabled_field")]
    pub enabled: bool,
    #[serde(deserialize_with = "sync_auto_field")]
    pub auto: bool,
    #[serde(deserialize_with = "sync_username_field")]
    pub username: Option<String>,
    #[serde(deserialize_with = "hkey_file_field")]
    pub hkey_file: Option<String>,
    #[serde(deserialize_with = "retries_field")]
    pub retries: u8,
    #[serde(deserialize_with = "backoff_field")]
    pub backoff_ms: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto: true,
            username: None,
            hkey_file: None,
            retries: 2,
            backoff_ms: 1_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    #[serde(deserialize_with = "theme_field")]
    pub theme: ThemeChoice,
    #[serde(deserialize_with = "font_size_field")]
    pub font_size: f32,
    #[serde(deserialize_with = "batch_size_field")]
    pub review_batch_size: usize,
    #[serde(deserialize_with = "hints_field")]
    pub show_keyboard_hints: bool,
    #[serde(default, deserialize_with = "reduced_motion_field")]
    pub reduce_motion: Option<bool>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            font_size: 16.0,
            review_batch_size: 20,
            show_keyboard_hints: true,
            reduce_motion: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    #[serde(deserialize_with = "retention_field")]
    pub desired_retention: f32,
    #[serde(deserialize_with = "auto_optimize_field")]
    pub auto_optimize: bool,
    #[serde(deserialize_with = "params_field")]
    pub params: Vec<f32>,
    #[serde(deserialize_with = "decay_field")]
    pub decay: f32,
    #[serde(deserialize_with = "day_field")]
    pub last_optimized: u32,
    #[serde(deserialize_with = "reviews_field")]
    pub reviews_since: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            desired_retention: 0.9,
            auto_optimize: true,
            params: Vec::new(),
            decay: 0.1542,
            last_optimized: 0,
            reviews_since: 0,
        }
    }
}

const UI_KEYS: &[&str] = &[
    "theme",
    "font_size",
    "review_batch_size",
    "show_keyboard_hints",
    "reduce_motion",
];
const SCHEDULER_KEYS: &[&str] = &[
    "desired_retention",
    "auto_optimize",
    "params",
    "decay",
    "last_optimized",
    "reviews_since",
];
const SYNC_KEYS: &[&str] = &[
    "enabled",
    "auto",
    "username",
    "hkey_file",
    "retries",
    "backoff_ms",
];

const STUDY_KEYS: &[&str] = &["leech_enabled", "again", "hard", "good", "easy"];

/// Parses a config file, warning and falling back field by field.
#[must_use]
pub fn parse(input: &str) -> Config {
    let Ok(value) = toml::from_str::<toml::Value>(input) else {
        warn("config is not valid TOML; using defaults");
        return Config::default();
    };
    let Some(root) = value.as_table() else {
        warn("config root must be a table; using defaults");
        return Config::default();
    };
    if root.contains_key("general") {
        warn("general is ignored; Ranki manages its own data directory");
    }
    Config {
        ui: section(root, "ui", UI_KEYS),
        scheduler: section(root, "scheduler", SCHEDULER_KEYS),
        sync: section(root, "sync", SYNC_KEYS),
        study: section(root, "study", STUDY_KEYS),
    }
}

fn section<T: DeserializeOwned + Default>(
    root: &toml::Table,
    name: &str,
    allowed: &[&str],
) -> T {
    let Some(value) = root.get(name) else {
        return T::default();
    };
    let Some(table) = value.as_table() else {
        warn(&format!("[{name}] must be a table; using defaults"));
        return T::default();
    };
    let mut cleaned = table.clone();
    cleaned.retain(|key, _| {
        if allowed.contains(&key) {
            true
        } else {
            warn(&format!("{name}.{key} is unknown; ignoring"));
            false
        }
    });
    match toml::Value::Table(cleaned).try_into::<T>() {
        Ok(section) => section,
        Err(error) => {
            warn(&format!(
                "[{name}] could not be read: {error}; using defaults"
            ));
            T::default()
        }
    }
}

fn warn(message: &str) {
    eprintln!("ranki: config: {message}");
}

fn invalid(key: &str) {
    warn(&format!("{key} has an invalid value; using its default"));
}

fn choice_text<'de, D>(deserializer: D, key: &str) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match String::deserialize(deserializer) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => {
            invalid(key);
            Ok(String::new())
        }
    }
}

fn optional_text<'de, D>(
    deserializer: D,
    key: &str,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer) {
        Ok(Some(value)) if !value.trim().is_empty() => Ok(Some(value)),
        _ => {
            invalid(key);
            Ok(None)
        }
    }
}

fn theme_field<'de, D>(deserializer: D) -> Result<ThemeChoice, D::Error>
where
    D: Deserializer<'de>,
{
    let value = choice_text(deserializer, "ui.theme")?;
    match value.as_str() {
        "dark" => Ok(ThemeChoice::Dark),
        "light" => Ok(ThemeChoice::Light),
        "system" => Ok(ThemeChoice::System),
        _ => {
            invalid("ui.theme");
            Ok(ThemeChoice::System)
        }
    }
}

fn font_size_field<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    match f32::deserialize(deserializer) {
        Ok(value) if value.is_finite() && (8.0..=40.0).contains(&value) => {
            Ok(value)
        }
        _ => {
            invalid("ui.font_size");
            Ok(16.0)
        }
    }
}

fn batch_size_field<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    match usize::deserialize(deserializer) {
        Ok(value) if (1..=500).contains(&value) => Ok(value),
        _ => {
            invalid("ui.review_batch_size");
            Ok(20)
        }
    }
}

fn hints_field<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    bool::deserialize(deserializer).map_or_else(
        |_| {
            invalid("ui.show_keyboard_hints");
            Ok(true)
        },
        Ok,
    )
}

fn reduced_motion_field<'de, D>(
    deserializer: D,
) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<bool>::deserialize(deserializer).map_or_else(
        |_| {
            invalid("ui.reduce_motion");
            Ok(None)
        },
        Ok,
    )
}

fn retention_field<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    match f32::deserialize(deserializer) {
        Ok(value) if value.is_finite() && (0.7..=0.99).contains(&value) => {
            Ok(value)
        }
        _ => {
            invalid("scheduler.desired_retention");
            Ok(0.9)
        }
    }
}

fn auto_optimize_field<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    bool::deserialize(deserializer).map_or_else(
        |_| {
            invalid("scheduler.auto_optimize");
            Ok(true)
        },
        Ok,
    )
}

fn params_field<'de, D>(deserializer: D) -> Result<Vec<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    match Vec::<f32>::deserialize(deserializer) {
        Ok(value)
            if matches!(value.len(), 0 | 17 | 19 | 21)
                && value.iter().all(|weight| weight.is_finite()) =>
        {
            Ok(value)
        }
        _ => {
            invalid("scheduler.params");
            Ok(Vec::new())
        }
    }
}

fn decay_field<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    match f32::deserialize(deserializer) {
        Ok(value) if value.is_finite() && (0.1..=0.8).contains(&value) => {
            Ok(value)
        }
        _ => {
            invalid("scheduler.decay");
            Ok(0.1542)
        }
    }
}

fn day_field<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    u32::deserialize(deserializer).map_or_else(
        |_| {
            invalid("scheduler.last_optimized");
            Ok(0)
        },
        Ok,
    )
}

fn reviews_field<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    u32::deserialize(deserializer).map_or_else(
        |_| {
            invalid("scheduler.reviews_since");
            Ok(0)
        },
        Ok,
    )
}

fn sync_enabled_field<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    bool::deserialize(deserializer).map_or_else(
        |_| {
            invalid("sync.enabled");
            Ok(false)
        },
        Ok,
    )
}

fn sync_auto_field<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    bool::deserialize(deserializer).map_or_else(
        |_| {
            invalid("sync.auto");
            Ok(true)
        },
        Ok,
    )
}

fn sync_username_field<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    optional_text(deserializer, "sync.username")
}

fn hkey_file_field<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    optional_text(deserializer, "sync.hkey_file")
}

fn retries_field<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    match u8::deserialize(deserializer) {
        Ok(value) if value <= 5 => Ok(value),
        _ => {
            invalid("sync.retries");
            Ok(2)
        }
    }
}

fn backoff_field<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    match u64::deserialize(deserializer) {
        Ok(value) if value <= 10_000 => Ok(value),
        _ => {
            invalid("sync.backoff_ms");
            Ok(1_000)
        }
    }
}
fn leech_enabled_field<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    bool::deserialize(deserializer).map_or_else(
        |_| {
            invalid("study.leech_enabled");
            Ok(true)
        },
        Ok,
    )
}

fn answer_key_field<'de, D>(
    deserializer: D,
    field: &str,
    default: &str,
) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map_or_else(
        |_| {
            invalid(field);
            Ok(default.to_owned())
        },
        |value| {
            let key = value.trim();
            if key.chars().count() <= 24 {
                Ok(key.to_owned())
            } else {
                invalid(field);
                Ok(default.to_owned())
            }
        },
    )
}

fn again_field<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    answer_key_field(deserializer, "study.again", "1")
}

fn hard_field<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    answer_key_field(deserializer, "study.hard", "2")
}

fn good_field<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    answer_key_field(deserializer, "study.good", "3")
}

fn easy_field<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    answer_key_field(deserializer, "study.easy", "4")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        Config, DataPaths, Settings, StudyConfig, ThemeChoice, UiState, UiView,
        parse,
    };
    use crate::Rating;

    #[test]
    fn defaults_are_stable() {
        assert_eq!(parse(""), Config::default());
        assert_eq!(Settings::default().theme, ThemeChoice::System);
    }

    #[test]
    fn full_config_is_parsed() {
        let config = parse(
            "[ui]\ntheme = 'light'\nfont_size = 18.0\nreview_batch_size = 40\n\
             show_keyboard_hints = false\n\
             [scheduler]\ndesired_retention = 0.85\n",
        );
        assert_eq!(config.ui.theme, ThemeChoice::Light);
        assert_eq!(config.ui.font_size, 18.0);
        assert_eq!(config.ui.review_batch_size, 40);
        assert!(!config.ui.show_keyboard_hints);
        assert_eq!(config.scheduler.desired_retention, 0.85);
    }

    #[test]
    fn ui_state_round_trips() {
        let state: UiState = toml::from_str(
            "view = 'browse'\nprevious_view = 'settings'\n\
             theme = 'light'\nnav_width = 140.0\nselected_deck = 42\n\
             expanded_decks = ['Topic', 'Other']\ndeck_query = 'foo'\n\
             browse_query = 'bar'\nreview_all_decks = true\n",
        )
        .expect("ui state");
        assert_eq!(state.view, UiView::Browse);
        assert_eq!(state.previous_view, UiView::Settings);
        assert_eq!(state.theme, Some(ThemeChoice::Light));
        assert_eq!(state.nav_width, Some(140.0));
        assert_eq!(state.selected_deck, Some(42));
        assert_eq!(state.expanded_decks, ["Topic", "Other"]);
        assert_eq!(state.deck_query, "foo");
        assert_eq!(state.browse_query, "bar");
        assert!(state.review_all_decks);
        let text = toml::to_string(&state).expect("serialize ui state");
        assert_eq!(
            toml::from_str::<UiState>(&text).expect("parse ui state"),
            state
        );
    }

    #[test]
    fn ui_state_defaults_when_older_or_invalid() {
        let state: UiState =
            toml::from_str("view = 'nope'\nnav_width = 'wide'\n")
                .unwrap_or_default();
        assert_eq!(state, UiState::default());
    }

    #[test]
    fn legacy_scheduler_algorithm_is_ignored() {
        let config =
            parse("[scheduler]\nalgorithm = 'sm2'\ndesired_retention = 0.85\n");
        assert_eq!(config.scheduler.desired_retention, 0.85);
        assert_eq!(config.scheduler.params, Vec::<f32>::new());
        assert!(!toml::to_string(&config).unwrap().contains("algorithm"));
    }

    #[test]
    fn invalid_values_and_unknown_keys_fall_back_by_field() {
        let config = parse(
            "[ui]\nthem = 'dark'\ntheme = 'solarized'\nfont_size = -4\n\
             review_batch_size = 0\nshow_keyboard_hints = 'yes'\n\
             [scheduler]\nparams = [1.0]\ndesired_retention = 2.0\n",
        );
        assert_eq!(config, Config::default());
    }

    #[test]
    fn reduced_motion_setting_is_optional_and_validated() {
        assert_eq!(
            parse("[ui]\nreduce_motion = true\n").ui.reduce_motion,
            Some(true)
        );
        assert_eq!(
            parse("[ui]\nreduce_motion = false\n").ui.reduce_motion,
            Some(false)
        );
        assert_eq!(parse("").ui.reduce_motion, None);
        assert_eq!(
            parse("[ui]\nreduce_motion = 'yes'\n").ui.reduce_motion,
            None
        );
        let config = parse("[ui]\nreduce_motion = true\n");
        let settings = Settings::resolve(&config, DataPaths::default());
        assert_eq!(settings.reduce_motion, Some(true));
    }

    #[test]
    fn retention_outside_the_anki_range_falls_back() {
        let config = parse("[scheduler]\ndesired_retention = 0.6\n");
        assert_eq!(config.scheduler.desired_retention, 0.9);
        let config = parse("[scheduler]\ndesired_retention = 0.8\n");
        assert_eq!(config.scheduler.desired_retention, 0.8);
    }

    #[test]
    fn malformed_toml_uses_defaults() {
        assert_eq!(parse("[general\ncollection = "), Config::default());
    }

    #[test]
    fn resolution_uses_only_app_owned_paths() {
        let paths = DataPaths::new("/data/ranki");
        let settings = Settings::resolve(&Config::default(), paths.clone());
        assert_eq!(settings.paths, paths);
        assert_eq!(
            settings.paths.collection,
            Path::new("/data/ranki/collection.anki2")
        );
        assert_eq!(
            settings.paths.media_dir,
            Path::new("/data/ranki/collection.media")
        );
        assert_eq!(
            settings.paths.sync_state,
            Path::new("/data/ranki/ranki-sync.json")
        );
    }

    #[test]
    fn legacy_storage_paths_are_ignored() {
        let config = parse(
            "[general]\ncollection = '/home/user/.local/share/Anki2/main/collection.anki2'\n\
             media_dir = '/home/user/.local/share/Anki2/main/collection.media'\n",
        );
        assert_eq!(config, Config::default());
    }
    #[test]
    fn sync_section_is_parsed() {
        let config = parse(
            "[sync]\nenabled = true\nauto = false\nusername = 'alice'\n\
             hkey_file = '/keys/ranki'\nretries = 4\nbackoff_ms = 250\n",
        );
        assert!(config.sync.enabled);
        assert!(!config.sync.auto);
        assert_eq!(config.sync.username.as_deref(), Some("alice"));
        assert_eq!(config.sync.hkey_file.as_deref(), Some("/keys/ranki"));
        assert_eq!(config.sync.retries, 4);
        assert_eq!(config.sync.backoff_ms, 250);
    }

    #[test]
    fn sync_defaults_and_invalid_values_are_field_local() {
        assert_eq!(parse("").sync, super::SyncConfig::default());
        let config = parse(
            "[sync]\nenabled = 'yes'\nauto = 1\nusername = ''\n\
             hkey_file = ''\nretries = 9\nbackoff_ms = 99_999\n",
        );
        assert_eq!(config.sync, super::SyncConfig::default());
    }

    #[test]
    fn scheduler_optimization_config_round_trips() {
        let config = parse(
            "[scheduler]\nauto_optimize = false\n\
             params = [0.2, 1.3, 2.3, 8.2, 6.4, 0.8, 3.0, 0.0, 1.8, 0.1, \
             0.7, 1.4, 0.0, 0.2, 1.6, 0.6, 1.8, 0.5, 0.0, 0.0, 0.15]\n\
             decay = 0.2\nlast_optimized = 19_800\nreviews_since = 42\n",
        );
        assert!(!config.scheduler.auto_optimize);
        assert_eq!(config.scheduler.params.len(), 21);
        assert_eq!(config.scheduler.decay, 0.2);
        assert_eq!(config.scheduler.last_optimized, 19_800);
        assert_eq!(config.scheduler.reviews_since, 42);
        let text = toml::to_string(&config).unwrap();
        assert_eq!(parse(&text), config);
    }

    #[test]
    fn invalid_optimization_fields_fall_back() {
        let config = parse(
            "[scheduler]\nparams = [1.0, 2.0]\ndecay = 3.0\n\
             last_optimized = -1\nreviews_since = -2\nauto_optimize = 'no'\n",
        );
        assert_eq!(config.scheduler, super::SchedulerConfig::default());
    }

    #[test]
    fn resolution_carries_sync_and_optimizer_fields() {
        let mut config = Config::default();
        config.sync.enabled = true;
        config.sync.auto = false;
        config.sync.username = Some(String::from("alice"));
        config.sync.retries = 1;
        config.scheduler.auto_optimize = false;
        config.scheduler.params = vec![0.2; 21];
        config.scheduler.decay = 0.3;
        config.scheduler.last_optimized = 12;
        config.scheduler.reviews_since = 9;
        let settings = Settings::resolve(&config, DataPaths::default());
        assert!(settings.sync.enabled);
        assert!(!settings.sync.auto);
        assert_eq!(settings.sync.username.as_deref(), Some("alice"));
        assert_eq!(settings.sync.retries, 1);
        assert!(!settings.auto_optimize);
        assert_eq!(settings.fsrs_params, vec![0.2; 21]);
        assert_eq!(settings.fsrs_decay, 0.3);
        assert_eq!(settings.last_optimized, 12);
        assert_eq!(settings.reviews_since, 9);
    }

    #[test]
    fn study_defaults_are_stable() {
        let study = StudyConfig::default();
        assert!(study.leech_enabled);
        assert_eq!(study.again, "1");
        assert_eq!(study.hard, "2");
        assert_eq!(study.good, "3");
        assert_eq!(study.easy, "4");
        assert_eq!(parse("").study, study);
        assert_eq!(Settings::default().study, study);
    }

    #[test]
    fn study_section_is_parsed() {
        let config = parse(
            "[study]\nleech_enabled = false\nagain = ' a '\nhard = '2'\n\
             good = '3'\neasy = '4'\n",
        );
        assert!(!config.study.leech_enabled);
        assert_eq!(config.study.again, "a");
        assert_eq!(config.study.hard, "2");
        assert_eq!(config.study.good, "3");
        assert_eq!(config.study.easy, "4");
        let text = toml::to_string(&config).unwrap();
        assert_eq!(parse(&text), config);
    }

    #[test]
    fn study_invalid_values_and_unknown_keys_fall_back_by_field() {
        let config = parse(
            "[study]\nleech_enabled = 'yes'\nagain = 1\nhard = '   '\n\
             good = 'abcdefghijklmnopqrstuvwxy'\neasy = '  4  '\n\
             leech = true\n",
        );
        assert!(config.study.leech_enabled);
        assert_eq!(config.study.again, "1");
        assert_eq!(config.study.hard, "");
        assert_eq!(config.study.good, "3");
        assert_eq!(config.study.easy, "4");
    }

    #[test]
    fn resolution_copies_study_settings() {
        let mut config = Config::default();
        config.study.leech_enabled = false;
        config.study.again = String::from("a");
        config.study.hard = String::from("b");
        config.study.good = String::from("c");
        config.study.easy = String::from("d");
        let settings = Settings::resolve(&config, DataPaths::default());
        assert_eq!(settings.study, config.study);
    }

    #[test]
    fn answer_keys_are_mapped_and_set_for_every_rating() {
        let mut study = StudyConfig::default();
        assert_eq!(study.answer_key(Rating::Again), "1");
        assert_eq!(study.answer_key(Rating::Hard), "2");
        assert_eq!(study.answer_key(Rating::Good), "3");
        assert_eq!(study.answer_key(Rating::Easy), "4");
        for (rating, key) in [
            (Rating::Again, "a"),
            (Rating::Hard, "b"),
            (Rating::Good, "c"),
            (Rating::Easy, "d"),
        ] {
            study.set_answer_key(rating, key);
            assert_eq!(study.answer_key(rating), key);
        }
    }
}
