//! Statistics screen: a review heatmap, a seven-day forecast, and card counts.

use chrono::{Datelike, NaiveDate};
use gpui::{
    Context, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    Rgba, StatefulInteractiveElement, Styled, Window, div, px,
};
use ranki_model::{RevLog, RevLogKind};
use std::time::Duration;

use crate::{
    app::{Root, UiAction, duration_label},
    components::{ButtonTone, action_button, panel},
    theme::{
        PAGE_X, PAGE_Y, Palette, SPACE_BLOCK, SPACE_GROUP, SPACE_INLINE,
        SPACE_SECTION, Style, alpha, blend, page_width, queue_text,
    },
};

const SECONDS_PER_DAY: i64 = 86_400;
const DAYS: usize = 7;
/// Six months of review days plus one partial week for Sunday alignment.
const HEATMAP_WEEKS: usize = 24;
pub const HEATMAP_DAYS: usize = HEATMAP_WEEKS * 7;
const HEATMAP_COLUMNS: usize = HEATMAP_WEEKS + 1;
const HEATMAP_CELL: f32 = 26.0;
const HEATMAP_GAP: f32 = 4.0;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const HEATMAP_LABEL_WIDTH: f32 = 32.0;
const PANEL_PADDING: f32 = 48.0;
const FORECAST_BAR_WIDTH: f32 = 48.0;

/// Cell size and gap that keep every heatmap week inside the panel.
fn heatmap_geometry(width: f32) -> (f32, f32) {
    let gap = if width < 700.0 { 2.0 } else { HEATMAP_GAP };
    let inner = width - PANEL_PADDING - HEATMAP_LABEL_WIDTH - SPACE_GROUP;
    let columns = HEATMAP_COLUMNS as f32;
    let cell = (inner - gap * (columns - 1.0)) / columns;
    (cell.clamp(6.0, HEATMAP_CELL), gap)
}

/// Bar width and gap that keep all fourteen forecast bars inside the panel.
fn forecast_geometry(width: f32) -> (f32, f32) {
    let gap = if width < 760.0 {
        SPACE_GROUP
    } else {
        SPACE_BLOCK
    };
    let bars = (DAYS * 2) as f32;
    let inner = width - PANEL_PADDING;
    let bar = (inner - gap * (bars - 1.0)) / bars;
    (bar.clamp(10.0, FORECAST_BAR_WIDTH), gap)
}

fn forecast_text_size(bar_width: f32) -> f32 {
    if bar_width < 20.0 { 9.0 } else { 12.0 }
}

/// Compact count for a narrow forecast bar. Rounding keeps small labels short
/// without dropping a count close to a million.
fn compact_forecast_value(value: usize) -> String {
    if value >= 1_000_000 {
        let millions = (value.saturating_add(500_000) / 1_000_000).min(99);
        return format!("{millions}m");
    }
    let thousands = value.saturating_add(500) / 1_000;
    if thousands >= 1_000 {
        return String::from("1m");
    }
    format!("{thousands}k")
}

/// Counts stay visible on narrow charts; zeros stay quiet to reduce noise.
fn forecast_value_label(count: Option<usize>, bar_width: f32) -> String {
    match count {
        None => String::from("-"),
        Some(value) if bar_width < 32.0 && value > 0 => {
            if value < 1_000 {
                value.to_string()
            } else {
                compact_forecast_value(value)
            }
        }
        Some(_) if bar_width < 32.0 => String::new(),
        Some(value) => value.to_string(),
    }
}

const EMPTY_CHART_COPY: &str = "Review a card to see your activity";

fn empty_chart(colors: Palette) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .min_h(px(150.))
        .text_size(px(14.))
        .text_color(colors.subtext)
        .child(EMPTY_CHART_COPY)
}

fn charts_are_empty(stats: &Statistics, due: &[usize]) -> bool {
    stats.total_reviews == 0 && due.iter().all(|&count| count == 0)
}

fn activity_summary(total_days: usize, total: usize, peak: usize) -> String {
    format!("{total_days} days, {total} reviews, max {peak}/day")
}

fn hover_caption(day: i64, collection_start: i64) -> String {
    format!("{}, day {day}", weekday(day, collection_start))
}

/// One day of the review heatmap, oldest first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DayReviews {
    pub day: i64,
    pub reviews: usize,
    pub millis: u64,
    /// `Again`, `Hard`, `Good`, and `Easy` counts.
    pub ratings: [usize; 4],
}

impl DayReviews {
    /// Percentage of a day's answers that were not Again.
    #[must_use]
    pub fn accuracy_percent(&self) -> Option<u8> {
        if self.reviews == 0 {
            return None;
        }
        let correct = self.reviews.saturating_sub(self.ratings[0]);
        let percent = correct.saturating_mul(100) / self.reviews;
        u8::try_from(percent).ok()
    }
}

/// Review history aggregated for the statistics screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statistics {
    pub reviews_today: usize,
    pub millis_today: u64,
    /// `Again`, `Hard`, `Good`, and `Easy` counts for today.
    pub ratings_today: [usize; 4],
    /// Days ending today, oldest first.
    pub days: [DayReviews; HEATMAP_DAYS],
    pub streak: usize,
    pub total_reviews: usize,
    pub total_millis: u64,
}

impl Default for Statistics {
    fn default() -> Self {
        Self {
            reviews_today: 0,
            millis_today: 0,
            ratings_today: [0; 4],
            days: [DayReviews::default(); HEATMAP_DAYS],
            streak: 0,
            total_reviews: 0,
            total_millis: 0,
        }
    }
}

impl Statistics {
    /// Percentage of today's answers that were not Again.
    #[must_use]
    pub fn accuracy_percent(&self) -> Option<u8> {
        if self.reviews_today == 0 {
            return None;
        }
        let again = self.ratings_today[0];
        let correct = self.reviews_today.saturating_sub(again);
        let percent = correct.saturating_mul(100) / self.reviews_today;
        u8::try_from(percent).ok()
    }
}

/// Anki numbers days from the collection's creation time, not from midnight.
#[cfg(test)]
#[must_use]
pub fn day_number(timestamp_secs: i64, collection_start_secs: i64) -> i64 {
    timestamp_secs
        .saturating_sub(collection_start_secs)
        .div_euclid(SECONDS_PER_DAY)
}

/// Index of a day in the Sunday-first week grid.
#[must_use]
pub fn weekday_index(day: i64, collection_start_secs: i64) -> usize {
    let absolute = collection_start_secs
        .div_euclid(SECONDS_PER_DAY)
        .saturating_add(day);
    let index = absolute.saturating_add(4).rem_euclid(7);
    usize::try_from(index).unwrap_or(0)
}

/// Sunday of the week containing the first heatmap day.
#[must_use]
pub fn heatmap_start(first_day: i64, collection_start_secs: i64) -> i64 {
    first_day.saturating_sub(
        i64::try_from(weekday_index(first_day, collection_start_secs))
            .unwrap_or(0),
    )
}

/// Weekday name for a heatmap day, using the Unix epoch as the anchor.
#[must_use]
pub fn weekday(day: i64, collection_start_secs: i64) -> &'static str {
    WEEKDAYS
        .get(weekday_index(day, collection_start_secs))
        .copied()
        .unwrap_or("Sun")
}

/// ISO calendar week for an Anki day.
#[must_use]
pub fn calendar_week(day: i64, collection_start_secs: i64) -> u32 {
    let epoch_day = collection_start_secs
        .div_euclid(SECONDS_PER_DAY)
        .saturating_add(day);
    NaiveDate::from_ymd_opt(1970, 1, 1)
        .and_then(|epoch| {
            epoch.checked_add_signed(chrono::Duration::days(epoch_day))
        })
        .map_or(1, |date| date.iso_week().week())
}
/// One bar in the forecast chart: past reviews or upcoming dues.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ForecastDay {
    pub day: i64,
    pub reviews: usize,
    pub due: Option<usize>,
}

/// Joins seven days of reviews with seven days of scheduled dues. Both halves
/// include today, so the last history bar and the first forecast bar share a
/// day but show different counts.
#[must_use]
pub fn forecast_days(
    stats: &Statistics,
    due: &[usize],
) -> [ForecastDay; DAYS * 2] {
    let mut days = [ForecastDay::default(); DAYS * 2];
    for (index, history) in
        stats.days.iter().skip(HEATMAP_DAYS - DAYS).enumerate()
    {
        days[index] = ForecastDay {
            day: history.day,
            reviews: history.reviews,
            due: None,
        };
    }
    let today = stats.days.last().map_or(0, |day| day.day);
    for index in 0..DAYS {
        days[DAYS + index] = ForecastDay {
            day: today.saturating_add(i64::try_from(index).unwrap_or(0)),
            reviews: 0,
            due: due.get(index).copied(),
        };
    }
    days
}

/// Aggregates a full review log. Manual log entries are scheduling changes,
/// not answers, so they stay out of every count.
#[cfg(test)]
#[must_use]
pub fn build_statistics(
    history: &[RevLog],
    now_secs: i64,
    collection_start_secs: i64,
) -> Statistics {
    build_statistics_impl(
        history,
        day_number(now_secs, collection_start_secs),
        |timestamp| day_number(timestamp, collection_start_secs),
    )
}

/// Aggregates a full review log using Anki's day boundaries.
#[must_use]
pub fn build_statistics_timed(
    history: &[RevLog],
    timing: &ranki_model::Timing,
) -> Statistics {
    let today = i64::from(timing.days_elapsed);
    build_statistics_impl(history, today, |timestamp| {
        today.saturating_sub(i64::from(timing.elapsed_days_since(timestamp)))
    })
}

fn build_statistics_impl(
    history: &[RevLog],
    today: i64,
    day_of: impl Fn(i64) -> i64,
) -> Statistics {
    let mut days = [DayReviews::default(); HEATMAP_DAYS];
    for (index, day) in days.iter_mut().enumerate() {
        day.day = today.saturating_sub(
            i64::try_from(HEATMAP_DAYS - 1 - index).unwrap_or(0),
        );
    }

    let mut stats = Statistics::default();
    for log in history {
        if matches!(log.kind, RevLogKind::Manual | RevLogKind::Rescheduled) {
            continue;
        }
        let day = day_of(log.id / 1_000);
        let millis = u64::from(log.time_ms);
        stats.total_reviews += 1;
        stats.total_millis = stats.total_millis.saturating_add(millis);

        let rating = usize::from(log.ease.saturating_sub(1).min(3));
        let offset = today.saturating_sub(day);
        if day <= today && offset < i64::try_from(HEATMAP_DAYS).unwrap_or(168) {
            let index = HEATMAP_DAYS
                - 1
                - usize::try_from(offset).unwrap_or(HEATMAP_DAYS);
            let bucket = &mut days[index];
            bucket.reviews += 1;
            bucket.millis = bucket.millis.saturating_add(millis);
            bucket.ratings[rating] += 1;
        }
        if day == today {
            stats.ratings_today[rating] += 1;
            stats.reviews_today += 1;
            stats.millis_today = stats.millis_today.saturating_add(millis);
        }
    }
    stats.days = days;

    stats.streak = ranki_model::review_streak(history, &day_of, today);
    stats
}

impl Root {
    pub(crate) fn render_statistics_with_error(
        &self,
        window: &Window,
        error: Option<&str>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let width =
            page_width(f32::from(window.viewport_size().width), self.nav_width);
        let colors = self.colors();
        let style = Style::new(colors, self.theme());
        let stats = &self.statistics;
        let totals = self.leaf_totals;
        let empty = error.is_none() && charts_are_empty(stats, &self.forecast);
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(SPACE_SECTION))
            .id("statistics-scroll")
            .overflow_scroll()
            .track_scroll(&self.statistics_scroll)
            .px(px(PAGE_X))
            .py(px(PAGE_Y))
            .child(
                div()
                    .text_size(px(32.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Statistics"),
            )
            .child(self.render_activity(stats, style, width, error, empty, cx))
            .child(self.render_history(stats, style, width, empty))
            .child(Self::render_card_counts(totals, style))
    }

    fn render_activity(
        &self,
        stats: &Statistics,
        style: Style,
        width: f32,
        error: Option<&str>,
        empty: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let (cell_size, gap) = heatmap_geometry(width);
        let colors = style.colors;
        let collection_start = self.collection_start;
        let today = stats.days.last().map_or(0, |day| day.day);
        let oldest = stats.days.first().map_or(0, |day| day.day);
        let start = heatmap_start(oldest, collection_start);
        let peak = stats
            .days
            .iter()
            .map(|day| day.reviews)
            .max()
            .unwrap_or_default();
        let total = stats
            .days
            .iter()
            .fold(0_usize, |sum, day| sum.saturating_add(day.reviews));
        let level_fill = |level: u8| {
            if level == 0 {
                alpha(colors.surface1, 0.55)
            } else {
                blend(colors.surface1, colors.mauve, f32::from(level) / 4.0)
            }
        };

        let mut legend =
            div().flex().items_center().gap(px(SPACE_INLINE)).child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.overlay)
                    .child("Less"),
            );
        for level in 0..=4 {
            legend = legend.child(
                div()
                    .w(px(12.))
                    .h(px(12.))
                    .rounded(px(3.))
                    .bg(level_fill(level)),
            );
        }
        legend = legend.child(
            div()
                .text_size(px(11.))
                .text_color(colors.overlay)
                .child("More"),
        );

        let mut labels = div()
            .flex()
            .flex_col()
            .gap(px(gap))
            .w(px(HEATMAP_LABEL_WIDTH))
            .flex_none()
            .child(div().h(px(cell_size.min(14.))));
        for label in WEEKDAYS {
            labels = labels.child(
                div()
                    .h(px(cell_size))
                    .flex()
                    .items_center()
                    .text_size(px(cell_size.min(11.0)))
                    .line_height(px(cell_size))
                    .text_color(colors.subtext)
                    .child(label),
            );
        }

        let mut columns = div()
            .flex()
            .gap(px(gap))
            .min_w(px(0.))
            .flex_1()
            .id("statistics-heatmap-grid")
            .overflow_x_scroll();
        for week in 0..HEATMAP_COLUMNS {
            let mut column = div().flex().flex_col().gap(px(gap)).flex_none();
            let week_start =
                start.saturating_add(i64::try_from(week * 7).unwrap_or(0));
            let week_label = if cell_size >= 20.0 {
                format!(
                    "W{:02}",
                    calendar_week(
                        week_start.saturating_add(1),
                        collection_start
                    )
                )
            } else {
                String::new()
            };
            column = column.child(
                div()
                    .w(px(cell_size))
                    .h(px(cell_size.min(14.)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(cell_size.min(11.0)))
                    .text_color(colors.subtext)
                    .child(week_label),
            );
            for row in 0..7 {
                let index = week * 7 + row;
                let day =
                    start.saturating_add(i64::try_from(index).unwrap_or(0));
                let entry = if day >= oldest && day <= today {
                    usize::try_from(today.saturating_sub(day))
                        .ok()
                        .filter(|&offset| offset < HEATMAP_DAYS)
                        .and_then(|offset| {
                            stats.days.get(HEATMAP_DAYS - 1 - offset)
                        })
                } else {
                    None
                };
                let reviews = entry.map_or(0, |day| day.reviews);
                let level = if reviews == 0 || peak == 0 {
                    0
                } else {
                    u8::try_from(reviews.saturating_mul(4) / peak)
                        .unwrap_or(4)
                        .clamp(1, 4)
                };
                let fill = if entry.is_none() {
                    alpha(colors.surface1, 0.2)
                } else {
                    level_fill(level)
                };
                let border = if self.heatmap_hover == Some(day) {
                    colors.text
                } else if day == today {
                    colors.peach
                } else {
                    alpha(colors.text, 0.0)
                };
                let mut cell = div()
                    .id(ElementId::named_usize("statistics-heatmap", index))
                    .w(px(cell_size))
                    .h(px(cell_size))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(border)
                    .bg(fill);
                if entry.is_some() {
                    cell = cell.on_hover(cx.listener(
                        move |root, hovered: &bool, _, cx| {
                            if *hovered {
                                root.heatmap_hover = Some(day);
                                cx.notify();
                            } else if root.heatmap_hover == Some(day) {
                                root.heatmap_hover = None;
                                cx.notify();
                            }
                        },
                    ));
                }
                column = column.child(cell);
            }
            columns = columns.child(column);
        }

        let hovered = self
            .heatmap_hover
            .and_then(|day| stats.days.iter().find(|entry| entry.day == day));
        let caption =
            hovered.map(|day| hover_caption(day.day, collection_start));
        let accuracy = hovered
            .map_or_else(
                || stats.accuracy_percent(),
                DayReviews::accuracy_percent,
            )
            .map_or_else(|| String::from("-"), |value| format!("{value}%"));
        let millis = hovered.map_or(stats.millis_today, |day| day.millis);
        let metric = |label: &str, value: Div, hue: Rgba| {
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_INLINE))
                .min_w(px(120.))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.subtext)
                        .child(label.to_owned()),
                )
                .child(value.text_size(px(22.)).text_color(hue))
        };

        panel(colors)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(SPACE_BLOCK))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Review activity"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(SPACE_BLOCK))
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(colors.subtext)
                                    .child(activity_summary(
                                        HEATMAP_DAYS,
                                        total,
                                        peak,
                                    )),
                            )
                            .child(legend),
                    ),
            )
            .child({
                if let Some(error) = error {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .min_h(px(150.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap(px(SPACE_GROUP))
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(colors.red)
                                        .child("Could not load review history"),
                                )
                                .child(
                                    div()
                                        .max_w(px(360.))
                                        .text_size(px(12.))
                                        .text_color(colors.subtext)
                                        .child(error.to_owned()),
                                )
                                .child(action_button(
                                    "statistics-retry",
                                    "Retry",
                                    ButtonTone::Primary,
                                    style,
                                    self.action_click(
                                        UiAction::RetryStatistics,
                                    ),
                                )),
                        )
                        .into_any_element()
                } else if empty {
                    empty_chart(colors).into_any_element()
                } else {
                    div()
                        .flex()
                        .min_w(px(0.))
                        .justify_center()
                        .gap(px(SPACE_GROUP))
                        .child(labels)
                        .child(columns)
                        .into_any_element()
                }
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(SPACE_BLOCK))
                    .border_t_1()
                    .border_color(colors.surface1)
                    .pt(px(SPACE_BLOCK))
                    .children(caption.map(|label| {
                        div()
                            .w_full()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(label)
                    }))
                    .child(metric(
                        "Accuracy",
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(accuracy),
                        queue_text(2, style.theme),
                    ))
                    .child(metric(
                        "Time spent learning",
                        div().font_weight(gpui::FontWeight::SEMIBOLD).child(
                            duration_label(Duration::from_millis(millis)),
                        ),
                        colors.text,
                    ))
                    .child(metric(
                        "Streak",
                        div().font_weight(gpui::FontWeight::SEMIBOLD).child(
                            format!(
                                "{} day{}",
                                stats.streak,
                                if stats.streak == 1 { "" } else { "s" }
                            ),
                        ),
                        colors.peach,
                    )),
            )
    }

    fn render_history(
        &self,
        stats: &Statistics,
        style: Style,
        width: f32,
        empty: bool,
    ) -> impl IntoElement {
        let (bar_width, gap) = forecast_geometry(width);
        let colors = style.colors;
        let due_text = queue_text(2, style.theme);
        let collection_start = self.collection_start;
        let days = forecast_days(stats, &self.forecast);
        let peak = days
            .iter()
            .map(|day| day.due.unwrap_or(day.reviews))
            .max()
            .unwrap_or_default();
        let mut bars =
            div().flex().flex_1().min_w(px(0.)).items_end().gap(px(gap));
        for (index, day) in days.iter().enumerate() {
            let future = index >= DAYS;
            let count = if future { day.due } else { Some(day.reviews) };
            let value = count.unwrap_or_default();
            let height = if peak == 0 || value == 0 {
                6.0
            } else {
                20.0 + 100.0 * value as f32 / peak as f32
            };
            let hue = if value == 0 {
                colors.surface1
            } else if future {
                colors.green
            } else {
                colors.mauve
            };
            let value_label = forecast_value_label(count, bar_width);
            let day_name = weekday(day.day, collection_start);
            let short_day = bar_width < 20.0;
            let day_label = if short_day {
                day_name.chars().take(1).collect()
            } else {
                day_name.to_owned()
            };
            let text_size = px(forecast_text_size(bar_width));
            bars = bars.child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .flex_col()
                    .items_center()
                    .justify_end()
                    .gap(px(SPACE_GROUP))
                    .child(
                        div()
                            .max_w(px(bar_width))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(text_size)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if count.is_none() {
                                colors.overlay
                            } else {
                                colors.text
                            })
                            .child(value_label),
                    )
                    .child(crate::components::grow_height(
                        div().w(px(bar_width)).rounded(px(8.)).bg(hue),
                        gpui::ElementId::named_usize("forecast-bar", index),
                        self.motion_ms(crate::theme::MOTION_PROGRESS_MS),
                        height,
                    ))
                    .child(
                        div()
                            .max_w(px(bar_width))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(text_size)
                            .text_color(if index == DAYS - 1 {
                                colors.mauve
                            } else if index == DAYS {
                                due_text
                            } else {
                                colors.subtext
                            })
                            .child(day_label),
                    ),
            );
        }
        panel(colors)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(SPACE_GROUP))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Forecast"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(colors.subtext)
                            .child(format!(
                                "{} reviews  {} studied",
                                stats.total_reviews,
                                duration_label(Duration::from_millis(
                                    stats.total_millis,
                                ))
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(SPACE_BLOCK))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_GROUP))
                            .text_size(px(12.))
                            .text_color(colors.subtext)
                            .child(crate::components::dot(colors.mauve, 8.))
                            .child("Reviews, last 7 days"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_GROUP))
                            .text_size(px(12.))
                            .text_color(due_text)
                            .child(crate::components::dot(colors.green, 8.))
                            .child("Due, next 7 days"),
                    ),
            )
            .child(if empty {
                empty_chart(colors).into_any_element()
            } else {
                bars.h(px(150.)).into_any_element()
            })
    }

    fn render_card_counts(
        totals: ranki_model::DeckStats,
        style: Style,
    ) -> impl IntoElement {
        let colors = style.colors;
        let count = |label: &str, value: usize, hue: Rgba| {
            div()
                .flex()
                .flex_1()
                .flex_col()
                .gap(px(SPACE_INLINE))
                .min_w(px(110.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_GROUP))
                        .text_size(px(12.))
                        .text_color(colors.subtext)
                        .child(crate::components::dot(hue, 8.))
                        .child(label.to_owned()),
                )
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(format!("{value}")),
                )
        };
        panel(colors)
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Cards"),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(SPACE_BLOCK))
                    .child(count(
                        "New",
                        totals.new_cards,
                        queue_text(0, style.theme),
                    ))
                    .child(count(
                        "Learning",
                        totals.learning_cards,
                        queue_text(1, style.theme),
                    ))
                    .child(count(
                        "Review",
                        totals.review_cards,
                        queue_text(2, style.theme),
                    ))
                    .child(count("Total", totals.total_cards, colors.mauve)),
            )
    }
}

#[cfg(test)]
mod tests {

    use gpui::Rgba;
    use ranki_model::{Rating, RevLog, RevLogKind};

    use super::{
        DAYS, EMPTY_CHART_COPY, ForecastDay, HEATMAP_COLUMNS, HEATMAP_DAYS,
        HEATMAP_LABEL_WIDTH, PANEL_PADDING, SPACE_GROUP, Statistics,
        activity_summary, build_statistics, calendar_week, charts_are_empty,
        compact_forecast_value, day_number, forecast_days, forecast_geometry,
        forecast_text_size, forecast_value_label, heatmap_geometry,
        heatmap_start, hover_caption, weekday, weekday_index,
    };
    use crate::theme::{Theme, palette, queue_text};

    #[test]
    fn charts_fit_narrow_pages() {
        for width in [320.0, 408.0, 504.0, 668.0, 1048.0] {
            let (cell, gap) = heatmap_geometry(width);
            let columns = HEATMAP_COLUMNS as f32;
            let heatmap = cell * columns
                + gap * (columns - 1.0)
                + HEATMAP_LABEL_WIDTH
                + SPACE_GROUP
                + PANEL_PADDING;
            assert!(heatmap <= width, "heatmap overflows {width}px");

            let (bar, gap) = forecast_geometry(width);
            let bars = (DAYS * 2) as f32;
            let forecast = bar * bars + gap * (bars - 1.0) + PANEL_PADDING;
            assert!(forecast <= width, "forecast overflows {width}px");

            let label = forecast_value_label(Some(5555), bar);
            let size = forecast_text_size(bar);
            let pitch = bar + gap;
            assert!(
                label.chars().count() as f32 * size * 0.6 <= pitch,
                "forecast label overlaps at {width}px"
            );
        }
    }
    const START: i64 = 1_700_000_000;

    fn log(day: i64, rating: Rating, millis: u32) -> RevLog {
        RevLog {
            id: (START + day * 86_400) * 1000,
            card_id: 1,
            usn: 0,
            ease: rating.code(),
            interval: 1,
            last_interval: 0,
            factor: 2500,
            time_ms: millis,
            kind: RevLogKind::Review,
        }
    }

    fn with_interval(entry: RevLog, interval: i32) -> RevLog {
        RevLog { interval, ..entry }
    }

    #[test]
    fn days_follow_the_collection_creation_clock() {
        assert_eq!(day_number(START + 86_401, START), 1);
        assert_eq!(day_number(START - 86_399, START), -1);
        assert_eq!(weekday(0, START), "Tue");
    }

    #[test]
    fn heatmap_week_labels_follow_iso_weeks() {
        assert_eq!(calendar_week(0, START), 46);
        assert_eq!(calendar_week(6, START), 47);
        assert_eq!(calendar_week(-162, START), 23);
    }

    #[test]
    fn today_counts_ratings_time_and_accuracy() {
        let logs = [
            log(3, Rating::Good, 1000),
            log(3, Rating::Again, 500),
            log(3, Rating::Easy, 1500),
            log(2, Rating::Good, 2000),
        ];
        let stats = build_statistics(&logs, START + 3 * 86_400 + 100, START);
        assert_eq!(stats.reviews_today, 3);
        assert_eq!(stats.ratings_today, [1, 0, 1, 1]);
        assert_eq!(stats.millis_today, 3000);
        assert_eq!(stats.accuracy_percent(), Some(66));
        assert_eq!(stats.total_reviews, 4);
        assert_eq!(stats.total_millis, 5000);
        assert_eq!(stats.days.last().map(|day| day.reviews), Some(3));
        assert_eq!(stats.days[HEATMAP_DAYS - 2].reviews, 1);
    }

    #[test]
    fn rescheduled_logs_stay_out_of_review_counts() {
        let mut rescheduled = log(3, Rating::Good, 1_000);
        rescheduled.kind = RevLogKind::Rescheduled;
        let mut manual = log(3, Rating::Good, 1_000);
        manual.kind = RevLogKind::Manual;
        let stats = build_statistics(
            &[rescheduled, manual],
            START + 3 * 86_400 + 100,
            START,
        );
        assert_eq!(stats.total_reviews, 0);
        assert_eq!(stats.total_millis, 0);
    }
    #[test]
    fn heatmap_covers_the_full_window() {
        let logs: Vec<_> =
            (0..7).map(|day| log(day, Rating::Good, 100)).collect();
        let stats = build_statistics(&logs, START + 6 * 86_400, START);
        let recent: Vec<_> = stats
            .days
            .iter()
            .skip(HEATMAP_DAYS - 7)
            .map(|day| day.reviews)
            .collect();
        assert_eq!(recent, [1, 1, 1, 1, 1, 1, 1]);
        assert!(
            stats.days[..HEATMAP_DAYS - 7]
                .iter()
                .all(|day| day.reviews == 0)
        );
        assert_eq!(stats.days[0].day, -161);
        assert_eq!(stats.days[HEATMAP_DAYS - 1].day, 6);
        assert_eq!(stats.streak, 7);
    }

    #[test]
    fn heatmap_grid_holds_every_stat_day() {
        let today = 6;
        let oldest = today - i64::try_from(HEATMAP_DAYS - 1).unwrap_or(0);
        let start = heatmap_start(oldest, START);
        let last_cell = start
            .saturating_add(i64::try_from(HEATMAP_COLUMNS * 7).unwrap_or(0))
            .saturating_sub(1);
        assert_eq!(weekday_index(start, START), 0);
        assert!(start <= oldest);
        assert!(last_cell >= today);
    }

    #[test]
    fn heatmap_days_keep_time_and_ratings() {
        let logs = [
            log(3, Rating::Again, 500),
            log(3, Rating::Good, 1000),
            log(1, Rating::Easy, 2000),
        ];
        let stats = build_statistics(&logs, START + 3 * 86_400 + 100, START);
        let today = stats.days.last().unwrap();
        assert_eq!(today.reviews, 2);
        assert_eq!(today.millis, 1500);
        assert_eq!(today.ratings, [1, 0, 1, 0]);
        assert_eq!(today.accuracy_percent(), Some(50));

        let older = stats.days.get(HEATMAP_DAYS - 3).unwrap();
        assert_eq!(older.day, 1);
        assert_eq!(older.reviews, 1);
        assert_eq!(older.millis, 2000);
        assert_eq!(older.ratings, [0, 0, 0, 1]);
    }

    #[test]
    fn streak_starts_yesterday_when_today_has_no_reviews() {
        let logs = [log(1, Rating::Good, 100), log(0, Rating::Good, 100)];
        let stats = build_statistics(&logs, START + 2 * 86_400, START);
        assert_eq!(stats.reviews_today, 0);
        assert_eq!(stats.accuracy_percent(), None);
        assert_eq!(stats.streak, 2);
    }

    #[test]
    fn idle_days_do_not_break_the_streak() {
        let logs = [
            with_interval(log(0, Rating::Good, 100), 3),
            log(2, Rating::Good, 100),
            log(3, Rating::Good, 100),
        ];
        let stats = build_statistics(&logs, START + 3 * 86_400, START);
        assert_eq!(stats.streak, 3);
    }

    #[test]
    fn skipped_due_days_break_the_streak() {
        let logs = [log(0, Rating::Good, 100), log(3, Rating::Good, 100)];
        let stats = build_statistics(&logs, START + 3 * 86_400, START);
        assert_eq!(stats.streak, 1);
    }

    #[test]
    fn unanswered_days_after_the_last_review_break_the_streak() {
        let logs = [with_interval(log(0, Rating::Good, 100), 2)];
        let stats = build_statistics(&logs, START + 5 * 86_400, START);
        assert_eq!(stats.streak, 0);
    }

    #[test]
    fn manual_logs_are_not_reviews() {
        let mut entry = log(0, Rating::Good, 100);
        entry.kind = RevLogKind::Manual;
        let stats = build_statistics(&[entry], START, START);
        assert_eq!(stats.total_reviews, 0);
        assert_eq!(stats.reviews_today, 0);
    }

    #[test]
    fn forecast_days_join_last_week_with_next_week() {
        let stats = build_statistics(
            &[log(0, Rating::Good, 100), log(6, Rating::Good, 100)],
            START + 6 * 86_400,
            START,
        );
        let days = forecast_days(&stats, &[2, 3, 4, 5, 6, 7, 8]);

        assert_eq!(days.len(), 14);
        assert_eq!(
            days[0],
            ForecastDay {
                day: 0,
                reviews: 1,
                due: None
            }
        );
        assert_eq!(
            days[6],
            ForecastDay {
                day: 6,
                reviews: 1,
                due: None
            }
        );
        assert_eq!(
            days[7],
            ForecastDay {
                day: 6,
                reviews: 0,
                due: Some(2)
            }
        );
        assert_eq!(
            days[13],
            ForecastDay {
                day: 12,
                reviews: 0,
                due: Some(8)
            }
        );
    }

    #[test]
    fn forecast_days_without_due_data_are_unavailable() {
        let stats = build_statistics(&[], START + 6 * 86_400, START);
        let days = forecast_days(&stats, &[]);
        let upcoming: Vec<_> = days[7..].iter().map(|day| day.day).collect();
        assert_eq!(upcoming, [6, 7, 8, 9, 10, 11, 12]);
        assert!(days[7..].iter().all(|day| day.due.is_none()));
    }

    #[test]
    fn narrow_forecast_bars_keep_nonzero_values_legible() {
        for width in [292.0, 320.0, 408.0, 504.0] {
            let (bar, gap) = forecast_geometry(width);
            assert!(
                (10.0..32.0).contains(&bar),
                "expected a compact bar at {width}px, got {bar}"
            );
            let pitch = bar + gap;
            let size = forecast_text_size(bar);
            for value in [1, 42, 999, 5_555, 999_999] {
                let label = forecast_value_label(Some(value), bar);
                assert!(!label.is_empty(), "value hidden at {width}px");
                assert!(label.chars().count() <= 3, "label too long: {label}");
                assert!(
                    label.chars().count() as f32 * size * 0.6 <= pitch,
                    "labels overlap at {width}px"
                );
            }
        }
        assert_eq!(compact_forecast_value(5_555), "6k");
        assert_eq!(compact_forecast_value(999_499), "999k");
        assert_eq!(compact_forecast_value(999_999), "1m");
        assert_eq!(forecast_value_label(Some(0), 12.0), "");
        assert_eq!(forecast_value_label(None, 12.0), "-");
    }

    #[test]
    fn empty_charts_wait_for_a_review_or_due_card() {
        let stats = Statistics::default();
        assert!(charts_are_empty(&stats, &[0; DAYS]));
        assert!(charts_are_empty(&stats, &[]));
        let mut reviewed = stats.clone();
        reviewed.total_reviews = 1;
        assert!(!charts_are_empty(&reviewed, &[0; DAYS]));
        assert!(!charts_are_empty(&stats, &[0, 1, 0, 0, 0, 0, 0]));
        assert_eq!(EMPTY_CHART_COPY, "Review a card to see your activity");
    }

    #[test]
    fn hover_caption_names_the_weekday_and_day() {
        assert_eq!(hover_caption(162, START + 86_400), "Thu, day 162");
        assert_eq!(hover_caption(0, START), "Tue, day 0");
    }

    #[test]
    fn activity_header_explains_the_heatmap_scale() {
        assert_eq!(
            activity_summary(HEATMAP_DAYS, 42, 7),
            format!("{HEATMAP_DAYS} days, 42 reviews, max 7/day")
        );
    }

    #[test]
    fn light_chart_text_does_not_use_raw_green() {
        let light = palette(Theme::Light);
        let ink = queue_text(2, Theme::Light);
        assert_eq!(ink, light.text);
        assert_ne!(ink, light.green);
        assert_eq!(queue_text(2, Theme::Dark), palette(Theme::Dark).green);
        assert!(contrast(ink, light.mantle) >= 4.5);
        assert!(contrast(light.green, light.mantle) < 4.5);
    }

    fn luminance(color: Rgba) -> f32 {
        let linear = |channel: f32| {
            if channel <= 0.040_45 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r)
            + 0.7152 * linear(color.g)
            + 0.0722 * linear(color.b)
    }

    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (first, second) = if luminance(a) >= luminance(b) {
            (luminance(a), luminance(b))
        } else {
            (luminance(b), luminance(a))
        };
        (first + 0.05) / (second + 0.05)
    }
}
