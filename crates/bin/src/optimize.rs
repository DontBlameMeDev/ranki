#![expect(
    clippy::redundant_pub_crate,
    reason = "helpers are shared only with the crate root"
)]
//! Auto-optimization at session and sync boundaries.

use ranki_core::AUTO_OPTIMIZE_MIN_REVIEWS;
use ranki_core::FsrsParams;
use ranki_core::MIN_TRAINING_ITEMS;
use ranki_core::OptimizationInput;
use ranki_core::ReviewRecord;
use ranki_core::day_gate;
use ranki_core::should_auto_optimize;
use ranki_model::Config;
use ranki_model::RevLog;
use ranki_model::RevLogKind;
use ranki_model::Store;

const SECONDS_PER_DAY: i64 = 86_400;
const MILLIS_PER_DAY: i64 = 86_400_000;

/// Runs one optimizer pass when the documented trigger policy allows it.
///
/// The config is updated with the fitted weights and reset counters when a
/// pass ran. The config-only part of the trigger policy is checked before the
/// review log is read, so the day gate rejects most sessions without a query.
/// Review counters refresh once that gate passes. A count query rejects sessions
/// below the review threshold before the full history is loaded.
pub(super) fn maybe_auto_optimize(
    store: &dyn Store,
    config: &mut Config,
    timing: &ranki_model::Timing,
) {
    if !config.scheduler.auto_optimize {
        return;
    }
    let today = timing.days_elapsed;
    if !day_gate(u64::from(today), u64::from(config.scheduler.last_optimized)) {
        return;
    }
    let cutoff_ms = review_cutoff_ms(timing, config.scheduler.last_optimized);
    let reviews_since = match store.review_count_since(cutoff_ms) {
        Ok(count) => count,
        Err(error) => {
            eprintln!("ranki: auto-optimize: {error}");
            return;
        }
    };
    config.scheduler.reviews_since = reviews_since;
    if reviews_since < AUTO_OPTIMIZE_MIN_REVIEWS {
        return;
    }
    let history = match store.review_history() {
        Ok(history) => history,
        Err(error) => {
            eprintln!("ranki: auto-optimize: {error}");
            return;
        }
    };
    let records = review_records(&history);
    let usable_items = records
        .iter()
        .filter(|record| {
            record.elapsed_days > 0
                && matches!(
                    record.kind,
                    RevLogKind::Learn
                        | RevLogKind::Review
                        | RevLogKind::Relearn
                )
        })
        .count();
    let trigger = ranki_core::OptimizationTrigger {
        days_since_last_optimization: today
            .saturating_sub(config.scheduler.last_optimized),
        reviews_since_last_optimization: reviews_since,
        usable_items,
        optimization_running: false,
    };
    if !should_auto_optimize(trigger) {
        return;
    }
    let current = configured_params(config);
    let report = ranki_core::optimize(&OptimizationInput {
        records: &records,
        current_params: &current,
    });
    if report.items < MIN_TRAINING_ITEMS {
        return;
    }
    config.scheduler.params = report.params.weights().to_vec();
    config.scheduler.decay = report.params.decay();
    config.scheduler.last_optimized = today;
    config.scheduler.reviews_since = 0;
}

#[cfg(test)]
fn day_number(now_secs: i64) -> u32 {
    u32::try_from(now_secs.div_euclid(SECONDS_PER_DAY)).unwrap_or(u32::MAX)
}

/// Milliseconds since the last optimization day, on the collection clock.
fn review_cutoff_ms(timing: &ranki_model::Timing, last_optimized: u32) -> i64 {
    let days = timing.days_elapsed.saturating_sub(last_optimized);
    timing
        .now
        .saturating_sub(i64::from(days).saturating_mul(SECONDS_PER_DAY))
        .saturating_mul(1_000)
}
fn review_records(history: &[RevLog]) -> Vec<ReviewRecord> {
    let mut records = Vec::with_capacity(history.len());
    let mut previous: Option<(i64, i64)> = None;
    for log in history {
        let elapsed_days = previous
            .filter(|(card_id, _)| *card_id == log.card_id)
            .map_or(0, |(_, previous_id)| {
                let millis = log.id.saturating_sub(previous_id);
                u32::try_from(millis.div_euclid(MILLIS_PER_DAY))
                    .unwrap_or(u32::MAX)
            });
        previous = Some((log.card_id, log.id));
        records.push(ReviewRecord {
            card_id: log.card_id,
            rating: ranki_model::Rating::from_code(log.ease)
                .unwrap_or(ranki_model::Rating::Good),
            elapsed_days,
            kind: log.kind,
        });
    }
    records
}

fn configured_params(config: &Config) -> FsrsParams {
    let params =
        FsrsParams::from_slice(&config.scheduler.params).unwrap_or_default();
    params.with_decay(config.scheduler.decay)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ranki_model::NewNote;
    use ranki_model::RevLog;
    use ranki_model::RevLogKind;
    use ranki_model::ReviewDecision;
    use ranki_model::Store;
    use ranki_storage::Collection;

    use super::maybe_auto_optimize;

    const NOW: i64 = 2_000_000_000;

    fn timing(now: i64) -> ranki_model::Timing {
        ranki_model::Timing::v1(now, 0)
    }

    fn collection_with_reviews(path: &Path, count: usize) -> Collection {
        let mut collection =
            Collection::create(&path.join("collection.anki2")).unwrap();
        let ids = collection
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![String::from("front"), String::from("back")],
                tags: Vec::new(),
            })
            .unwrap();
        let card = collection.card(ids[0]).unwrap().unwrap();
        let base = NOW.saturating_mul(1_000);
        let decisions = (0..count)
            .map(|index| ReviewDecision {
                card: card.clone(),
                expected_modified: card.modified,
                expected_usn: card.usn,
                revlog: RevLog {
                    id: i64::try_from(index)
                        .unwrap_or(0)
                        .saturating_mul(super::MILLIS_PER_DAY)
                        .saturating_add(base),
                    card_id: card.id,
                    usn: 0,
                    ease: 3,
                    interval: 1,
                    last_interval: 0,
                    factor: 2_500,
                    time_ms: 100,
                    kind: RevLogKind::Review,
                },
            })
            .collect::<Vec<_>>();
        collection.apply_reviews(&decisions).unwrap();
        collection
    }

    #[test]
    fn review_cutoff_uses_the_collection_clock() {
        let created =
            NOW.saturating_sub(10_i64.saturating_mul(super::SECONDS_PER_DAY));
        let timing = ranki_model::Timing::v1(NOW, created);

        assert_eq!(timing.days_elapsed, 10);
        assert_eq!(
            super::review_cutoff_ms(&timing, 5),
            NOW.saturating_sub(5_i64.saturating_mul(super::SECONDS_PER_DAY))
                .saturating_mul(1_000)
        );
    }

    #[test]
    fn trigger_requires_thirty_days() {
        let directory = tempfile::tempdir().unwrap();
        let collection = collection_with_reviews(directory.path(), 1_000);
        let mut config = ranki_model::Config::default();
        config.scheduler.last_optimized =
            super::day_number(NOW).saturating_sub(29);
        maybe_auto_optimize(&collection, &mut config, &timing(NOW));
        assert_eq!(config.scheduler.params.len(), 0);
        // The day gate returns before the review log is read.
        assert_eq!(config.scheduler.reviews_since, 0);

        config.scheduler.last_optimized =
            super::day_number(NOW).saturating_sub(30);
        maybe_auto_optimize(&collection, &mut config, &timing(NOW));
        assert_eq!(config.scheduler.params.len(), 21);
        assert_eq!(config.scheduler.last_optimized, super::day_number(NOW));
        assert_eq!(config.scheduler.reviews_since, 0);
    }

    #[test]
    fn trigger_requires_one_thousand_reviews() {
        let first = tempfile::tempdir().unwrap();
        let collection = collection_with_reviews(first.path(), 999);
        let mut config = ranki_model::Config::default();
        config.scheduler.last_optimized = 0;
        maybe_auto_optimize(&collection, &mut config, &timing(NOW));
        assert_eq!(config.scheduler.params.len(), 0);
        assert_eq!(config.scheduler.reviews_since, 999);

        let second = tempfile::tempdir().unwrap();
        let collection = collection_with_reviews(second.path(), 1_000);
        let mut config = ranki_model::Config::default();
        config.scheduler.last_optimized = 0;
        maybe_auto_optimize(&collection, &mut config, &timing(NOW));
        assert_eq!(config.scheduler.params.len(), 21);
    }

    #[test]
    fn disabled_optimizer_leaves_config_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let collection = collection_with_reviews(directory.path(), 1_000);
        let mut config = ranki_model::Config::default();
        config.scheduler.auto_optimize = false;
        let before = config.clone();
        maybe_auto_optimize(&collection, &mut config, &timing(NOW));
        assert_eq!(config, before);
    }

    #[test]
    fn optimizer_persists_configured_decay_and_params() {
        let directory = tempfile::tempdir().unwrap();
        let collection = collection_with_reviews(directory.path(), 1_000);
        let mut config = ranki_model::Config::default();
        config.scheduler.params = vec![2.0; 21];
        config.scheduler.decay = 0.4;
        config.scheduler.last_optimized = 0;
        maybe_auto_optimize(&collection, &mut config, &timing(NOW));
        assert_eq!(config.scheduler.params.len(), 21);
        assert!((0.1..=0.8).contains(&config.scheduler.decay));
        assert_eq!(config.scheduler.last_optimized, super::day_number(NOW));
        assert_eq!(config.scheduler.reviews_since, 0);
    }
}
