//! Storage-facing Anki deck options.
//!
//! These types describe the values Anki stores in schema-11 `dconf` JSON and
//! schema-18 `deck_config`/`decks` tables. They are deliberately close to the
//! stored shape: fields that Ranki does not use keep their raw representation
//! instead of gaining a UI model.

use serde::{Deserialize, Serialize};

use crate::Id;

/// Collection-wide Anki configuration.
///
/// `rollover` is the local hour at which a new Anki day starts, `0..=23`.
/// Anki defaults to 04:00.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnkiConfig {
    pub rollover: u8,
}

impl Default for AnkiConfig {
    fn default() -> Self {
        Self { rollover: 4 }
    }
}

/// Options for persisting a review.
///
/// A `false` `leech_enabled` skips leech marking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyReviewOptions {
    pub leech_enabled: bool,
}

impl Default for ApplyReviewOptions {
    fn default() -> Self {
        Self {
            leech_enabled: true,
        }
    }
}

/// Anki's deck kind, stored as `dyn` in schema 11 and as a oneof in schema 18.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum DeckKind {
    #[default]
    Normal,
    Filtered,
}

impl DeckKind {
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Filtered => 1,
        }
    }

    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        if code == 0 {
            Self::Normal
        } else {
            Self::Filtered
        }
    }
}

/// What Anki does when a card reaches the leech threshold.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum LeechAction {
    Suspend,
    #[default]
    TagOnly,
}

impl LeechAction {
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Suspend => 0,
            Self::TagOnly => 1,
        }
    }

    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        if code == 0 {
            Self::Suspend
        } else {
            Self::TagOnly
        }
    }
}

/// One of Anki's per-day limit pairs: the configured limit and today's count.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct DayLimit {
    pub limit: Option<u32>,
    pub today: u32,
    /// Day number from a schema-11 `[day, amount]` pair.
    pub day: u32,
}

/// Deck-specific values that override a preset when reviewing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeckOverrides {
    pub new_limit: Option<u32>,
    pub review_limit: Option<u32>,
    pub desired_retention: Option<f32>,
    pub extend_new: u32,
    pub extend_review: u32,
    pub new_today: DayLimit,
    pub review_today: DayLimit,
}

/// Counters Anki keeps in a deck's `common` protobuf.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Anki stores these independently toggled flags"
)]
#[serde(default)]
pub struct DeckCommon {
    pub study_collapsed: bool,
    pub browser_collapsed: bool,
    pub last_day_studied: u32,
    pub new_studied: i32,
    pub review_studied: i32,
    pub learning_studied: i32,
    pub milliseconds_studied: i32,
}

/// A deck with its effective identity and overrides resolved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EffectiveDeck {
    pub id: Id,
    pub name: String,
    pub description: String,
    pub kind: DeckKind,
    pub preset_id: Id,
    pub common: DeckCommon,
    pub overrides: DeckOverrides,
    pub markdown_description: bool,
    /// The exact schema-11 deck entry, kept so unknown keys survive writes.
    pub raw_json: Option<String>,
    /// Unknown protobuf `common.other` bytes.
    pub raw_common: Vec<u8>,
    /// Unknown fields in a filtered deck's `kind` message.
    pub raw_kind: Option<Vec<u8>>,
}

impl EffectiveDeck {
    /// Resolves the deck's desired retention against its preset.
    #[must_use]
    pub fn desired_retention(&self, preset: &DeckPreset) -> f32 {
        self.overrides
            .desired_retention
            .unwrap_or(preset.desired_retention)
    }
}

/// An Anki deck preset, decoded from schema-11 JSON or schema-18 protobuf.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "these flags mirror independently stored Anki options"
)]
pub struct DeckPreset {
    pub id: Id,
    pub name: String,
    pub mtime_secs: i64,
    pub usn: i32,
    pub learning_steps: Vec<f32>,
    pub relearning_steps: Vec<f32>,
    pub graduating_interval_good: u32,
    pub graduating_interval_easy: u32,
    pub initial_ease: f32,
    pub hard_multiplier: f32,
    pub easy_multiplier: f32,
    pub interval_multiplier: f32,
    pub lapse_multiplier: f32,
    pub maximum_interval: u32,
    pub minimum_lapse_interval: u32,
    pub new_per_day: u32,
    pub reviews_per_day: u32,
    pub new_per_day_minimum: u32,
    pub bury_new: bool,
    pub bury_reviews: bool,
    pub bury_interday_learning: bool,
    pub leech_action: LeechAction,
    pub leech_threshold: u32,
    pub new_card_insert_order: u32,
    pub new_card_gather_priority: u32,
    pub new_card_sort_order: u32,
    pub new_mix: u32,
    pub interday_learning_mix: u32,
    pub review_order: u32,
    pub fsrs_params_4: Vec<f32>,
    pub fsrs_params_5: Vec<f32>,
    pub fsrs_params_6: Vec<f32>,
    pub desired_retention: f32,
    pub historical_retention: f32,
    pub easy_days_percentages: Vec<f32>,
    pub load_balancer_enabled: bool,
    pub param_search: String,
    pub ignore_revlogs_before_date: String,
    pub max_taken_secs: u32,
    pub disable_autoplay: bool,
    pub show_timer: bool,
    pub stop_timer_on_answer: bool,
    pub seconds_to_show_question: f32,
    pub seconds_to_show_answer: f32,
    pub question_action: u32,
    pub answer_action: u32,
    pub wait_for_audio: bool,
    pub skip_question_when_replaying_answer: bool,
    /// The exact schema-11 config entry, kept so unknown keys survive writes.
    pub raw_json: Option<String>,
    /// Unknown protobuf `config.other` bytes.
    pub raw: Vec<u8>,
}

impl DeckPreset {
    /// FSRS weights use the newest version present, falling back to older ones.
    #[must_use]
    pub fn fsrs_params(&self) -> &[f32] {
        if !self.fsrs_params_6.is_empty() {
            &self.fsrs_params_6
        } else if !self.fsrs_params_5.is_empty() {
            &self.fsrs_params_5
        } else {
            &self.fsrs_params_4
        }
    }
}

impl Default for DeckPreset {
    fn default() -> Self {
        Self {
            id: 1,
            name: "Default".into(),
            mtime_secs: 0,
            usn: 0,
            learning_steps: vec![1.0, 10.0],
            relearning_steps: vec![10.0],
            graduating_interval_good: 1,
            graduating_interval_easy: 4,
            initial_ease: 2.5,
            hard_multiplier: 1.2,
            easy_multiplier: 1.3,
            interval_multiplier: 1.0,
            lapse_multiplier: 0.0,
            maximum_interval: 36_500,
            minimum_lapse_interval: 1,
            new_per_day: 20,
            reviews_per_day: 200,
            new_per_day_minimum: 0,
            bury_new: false,
            bury_reviews: false,
            bury_interday_learning: false,
            leech_action: LeechAction::TagOnly,
            leech_threshold: 8,
            new_card_insert_order: 0,
            new_card_gather_priority: 0,
            new_card_sort_order: 0,
            new_mix: 0,
            interday_learning_mix: 0,
            review_order: 0,
            fsrs_params_4: Vec::new(),
            fsrs_params_5: Vec::new(),
            fsrs_params_6: Vec::new(),
            desired_retention: 0.9,
            historical_retention: 0.9,
            easy_days_percentages: vec![1.0; 7],
            load_balancer_enabled: true,
            param_search: String::new(),
            ignore_revlogs_before_date: String::new(),
            max_taken_secs: 60,
            disable_autoplay: false,
            show_timer: false,
            stop_timer_on_answer: false,
            seconds_to_show_question: 0.0,
            seconds_to_show_answer: 0.0,
            question_action: 0,
            answer_action: 0,
            wait_for_audio: true,
            skip_question_when_replaying_answer: false,
            raw_json: None,
            raw: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DeckPreset;

    #[test]
    fn fsrs_params_prefer_newer_versions() {
        let mut preset = DeckPreset {
            fsrs_params_4: vec![4.0],
            fsrs_params_5: vec![5.0],
            ..DeckPreset::default()
        };
        assert_eq!(preset.fsrs_params(), [5.0]);
        preset.fsrs_params_6 = vec![6.0];
        assert_eq!(preset.fsrs_params(), [6.0]);
        preset.fsrs_params_5.clear();
        preset.fsrs_params_6.clear();
        assert_eq!(preset.fsrs_params(), [4.0]);
    }
}
