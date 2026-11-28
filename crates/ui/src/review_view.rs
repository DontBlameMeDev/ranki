//! Learning screen: header, fixed card stage, reveal and rating controls.

use gpui::{
    ClickEvent, Context, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Rgba, SharedString, StatefulInteractiveElement, Styled,
    Window, div, px,
};
use ranki_model::{CardType, Note, NoteModel};

use crate::{
    app::{Phase, Root, UiAction, duration_label, queue_counts, rating_name},
    card_view,
    components::{
        ClickHandler, Interaction, chip, fade_in, icon, keycap, progress_bar,
    },
    theme::{
        BUTTON_HEIGHT, BUTTON_PADDING_X, BUTTON_RADIUS, BUTTON_TEXT_SIZE,
        CONTROL_HEIGHT, FOOTER_HEIGHT, MOTION_PROGRESS_MS, MOTION_QUICK_MS,
        NAV_AUTO_COMPACT_WIDTH, PROGRESS_WIDTH, RATING_GAP, SPACE_BLOCK,
        SPACE_GROUP, SPACE_GUTTER, SPACE_INLINE, SPACE_RELATED, SPACE_SECTION,
        STAGE_HEIGHT, STAGE_PADDING, STAGE_RADIUS, TOPBAR_HEIGHT, accent_hover,
        accent_press, alpha, interval_label, rating_fill, rating_label,
        stage_width,
    },
};

fn review_button_handle(
    root: &Root,
    cx: &Context<Root>,
    index: usize,
) -> FocusHandle {
    let mut handles = root.review_button_focus.borrow_mut();
    while handles.len() <= index {
        handles.push(cx.focus_handle());
    }
    handles
        .get(index)
        .cloned()
        .unwrap_or_else(|| cx.focus_handle())
}

const NARROW_REVIEW_WIDTH: f32 = 400.0;
const COMPACT_RATING_CELL_HEIGHT: f32 = BUTTON_HEIGHT;

fn compact_review(viewport_width: f32) -> bool {
    viewport_width < NARROW_REVIEW_WIDTH
}

fn rating_control_height(viewport_width: f32) -> f32 {
    if compact_review(viewport_width) {
        2.0 * COMPACT_RATING_CELL_HEIGHT + RATING_GAP
    } else {
        CONTROL_HEIGHT
    }
}

/// Leaves equal room for the end button and queue counts, so the centered
/// progress block stays centered when the counts disappear.
fn balanced_progress_width(viewport_width: f32) -> f32 {
    PROGRESS_WIDTH.min(viewport_width - 480.0).max(80.0)
}
impl Root {
    pub(crate) fn render_review(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = self.colors();
        if self.review_answer_scroll_pending
            && self.review.phase == Phase::Answer
            && self.review.current.is_some()
        {
            if let Some(anchor) = self.review_answer_anchor.as_ref() {
                anchor.scroll_to(window, cx);
            }
            // Test draws do not run GPUI frame callbacks, so mirror the
            // anchor's origin and apply the same scroll during the draw.
            if let Some(origin) = self.review_answer_origin.get() {
                let viewport = self.review_stage_scroll.bounds();
                self.review_stage_scroll
                    .set_offset(viewport.origin - origin);
                self.review_answer_scroll_pending = false;
            } else {
                cx.notify();
            }
        }
        let style = self.style();
        let theme = self.theme();
        let session = queue_counts(
            self.review.current.as_ref(),
            &self.review.queue,
            &self.review.pending,
        );
        let more_due = self.review.more_due;
        let session_complete = self.session_complete();
        let viewport_width = f32::from(window.viewport_size().width);
        let viewport_height = f32::from(window.viewport_size().height);
        let card_width = stage_width(viewport_width);
        let text_width = (card_width - 2.0 * STAGE_PADDING).max(0.0);
        let topbar_compact = viewport_width < NAV_AUTO_COMPACT_WIDTH;
        let compact = compact_review(viewport_width);
        let progress_width = balanced_progress_width(viewport_width);
        let control_height = rating_control_height(viewport_width);
        let stage_height = (viewport_height
            - TOPBAR_HEIGHT
            - FOOTER_HEIGHT
            - control_height
            - 48.0
            - 18.0)
            .clamp(220.0, STAGE_HEIGHT);
        let image_height = (stage_height * 0.38).clamp(96.0, 180.0);
        let fraction = self.progress_fraction();
        let from = self.review.progress_from;
        let current = self.review.current.as_ref();
        let phase = self.review.phase;
        let question = self.review.question.as_ref();
        let answer = self.review.answer.as_ref();
        let answer_media = if phase == Phase::Answer {
            (!self.review.answer_media.is_empty()).then(|| {
                card_view::media_column(
                    &self.review.answer_media,
                    &self.options.paths.media_dir,
                    colors,
                    image_height,
                )
            })
        } else {
            None
        };
        let note = self.review.note.as_ref();
        let answered = self.review.answered;
        let animation_seq = self.review.animation_seq;
        let load_errors = self.review.load_errors;
        let load_error = self.review.load_error.clone();
        let again = self.review.again_count;
        let studied = duration_label(self.session_elapsed());
        let can_undo = self.review.last_grade.is_some();
        let quick = self.motion_ms(MOTION_QUICK_MS);
        let progress_ms = self.motion_ms(MOTION_PROGRESS_MS);
        let active_queue = current.map(|card| card.card_type);
        let previews = self.review.previews;
        let deck_name = if self.review.scope_all {
            String::from("All decks")
        } else {
            self.selected
                .and_then(|index| self.decks.get(index))
                .map_or_else(
                    || String::from("All decks"),
                    |deck| deck.name.replace("::", " \u{203a} "),
                )
        };
        let tags = note.map_or(&[][..], |note| note.tags.as_slice());
        let reading = (phase == Phase::Answer)
            .then(|| reading_label(&self.models, note))
            .flatten();
        let reveal = self.action_click(UiAction::Reveal);
        let reveal_focus = review_button_handle(self, cx, 1);
        let end = self.action_click(UiAction::EndSession);
        let end_focus = review_button_handle(self, cx, 0);
        let queue_count = |name: &str,
                           value: usize,
                           color: Rgba,
                           ink: Rgba,
                           active: bool| {
            let mut wrapper = div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(SPACE_INLINE))
                .pb(px(SPACE_INLINE))
                .border_b_2()
                .border_color(if active { color } else { alpha(color, 0.0) });
            if active {
                wrapper = wrapper.child(
                    div()
                        .text_size(px(27.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink)
                        .child(format!("{value}")),
                );
            } else {
                wrapper = wrapper.child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(ink)
                        .child(format!("{value}")),
                );
            }
            wrapper.child(
                div()
                    .text_size(px(13.))
                    .text_color(colors.subtext)
                    .child(name.to_owned()),
            )
        };
        let queue_counts = (!topbar_compact).then(|| {
            div()
                .flex()
                .items_end()
                .gap(px(SPACE_SECTION))
                .child(queue_count(
                    "New",
                    session.new_cards,
                    colors.blue,
                    crate::theme::queue_text(0, theme),
                    active_queue == Some(CardType::New),
                ))
                .child(queue_count(
                    "Learning",
                    session.learning_cards,
                    colors.red,
                    crate::theme::queue_text(1, theme),
                    matches!(
                        active_queue,
                        Some(CardType::Learning | CardType::Relearning)
                    ),
                ))
                .child(queue_count(
                    "Review",
                    session.review_cards,
                    colors.green,
                    crate::theme::queue_text(2, theme),
                    active_queue == Some(CardType::Review),
                ))
        });
        let mut screen = div().size_full().flex().flex_col();

        screen = screen.child(
            div()
                .h(px(TOPBAR_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .px(px(SPACE_GUTTER))
                .child(
                    div().flex_1().min_w(px(0.)).justify_start().child(
                        div()
                            .id("review-end")
                            .flex()
                            .items_center()
                            .gap(px(SPACE_GROUP))
                            .h(px(40.))
                            .px(px(SPACE_RELATED))
                            .rounded(px(12.))
                            .bg(colors.surface0)
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .focusable()
                            .track_focus(&end_focus)
                            .key_context("ranki_button")
                            .border_1()
                            .border_color(alpha(colors.text, 0.0))
                            .hover(move |row| row.bg(colors.surface1))
                            .active(move |row| row.bg(alpha(colors.text, 0.08)))
                            .focus(move |row| row.border_color(colors.mauve))
                            .on_click(end)
                            .child(icon("icon-arrow-left", 18., colors.text))
                            .children(
                                (!compact).then(|| div().child("End session")),
                            )
                            .children(
                                (!compact)
                                    .then(|| keycap("Esc", colors, false)),
                            ),
                    ),
                )
                .child(
                    div().flex_none().child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(SPACE_INLINE))
                            .child(
                                div()
                                    .max_w(px(progress_width))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(14.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(deck_name),
                            )
                            .child(progress_bar(
                                from,
                                fraction,
                                colors,
                                progress_width,
                                progress_ms,
                                SharedString::from(format!(
                                    "progress-{answered}-{animation_seq}"
                                )),
                            )),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .justify_end()
                        .children(queue_counts),
                ),
        );

        let skipped = (load_errors > 0).then(|| skipped_copy(load_errors));
        let load_failure = load_error.or_else(|| {
            (load_errors > 0).then(|| {
                format!(
                    "{load_errors} cards could not be loaded and were skipped."
                )
            })
        });
        let body = if current.is_none()
            && answered == 0
            && let Some(message) = load_failure
        {
            let retry = self.action_click(UiAction::StudyAgain);
            let back = self.action_click(UiAction::EndSession);
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(card_width))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(SPACE_BLOCK))
                        .p(px(SPACE_GUTTER))
                        .rounded(px(STAGE_RADIUS))
                        .bg(colors.surface0)
                        .child(
                            div()
                                .text_size(px(28.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("Could not load cards"),
                        )
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(colors.subtext)
                                .child(message),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(SPACE_GROUP))
                                .child(footer_button(
                                    "Back to decks",
                                    colors,
                                    false,
                                    None,
                                    back,
                                ))
                                .child(footer_button(
                                    "Try again",
                                    colors,
                                    true,
                                    None,
                                    retry,
                                )),
                        )
                        .into_any_element(),
                )
                .into_any_element()
        } else if session_complete {
            let practice = self.action_click(UiAction::StudyAgain);
            let back = self.action_click(UiAction::EndSession);
            let summary =
                div().flex_1().flex().items_center().justify_center().child(
                    div()
                        .w(px(card_width))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(SPACE_BLOCK))
                        .p(px(SPACE_GUTTER))
                        .rounded(px(STAGE_RADIUS))
                        .bg(colors.surface0)
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(colors.subtext)
                                .child("SESSION COMPLETE"),
                        )
                        .child(
                            div()
                                .text_size(px(32.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(summary_title(answered)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_center()
                                .gap(px(SPACE_BLOCK))
                                .child(summary_tile(
                                    "REVIEWED",
                                    &format!("{answered}"),
                                    colors.mauve,
                                    colors,
                                ))
                                .child(summary_tile(
                                    "AGAIN",
                                    &format!("{again}"),
                                    colors.red,
                                    colors,
                                ))
                                .child(summary_tile(
                                    "TIME",
                                    &studied,
                                    colors.green,
                                    colors,
                                )),
                        )
                        .children(skipped.map(|text| {
                            div()
                                .text_size(px(14.))
                                .text_color(colors.subtext)
                                .child(text)
                        }))
                        .children(more_due.then(|| {
                            div()
                                .text_size(px(14.))
                                .text_color(colors.subtext)
                                .child("More cards are due now.")
                        }))
                        .child(
                            div()
                                .flex()
                                .gap(px(SPACE_GROUP))
                                .child(footer_button(
                                    "Back to decks",
                                    colors,
                                    !more_due,
                                    Some(BACK_HINT),
                                    back,
                                ))
                                .children(more_due.then(|| {
                                    footer_button(
                                        "Study again",
                                        colors,
                                        true,
                                        Some(STUDY_HINT),
                                        practice,
                                    )
                                })),
                        ),
                );
            summary.into_any_element()
        } else if current.is_none() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(SPACE_GROUP))
                        .w(px(card_width))
                        .p(px(SPACE_GUTTER))
                        .rounded(px(STAGE_RADIUS))
                        .bg(colors.surface0)
                        .child(
                            div()
                                .text_size(px(32.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("All caught up"),
                        )
                        .child(footer_button(
                            "Back to decks",
                            colors,
                            true,
                            None,
                            self.action_click(UiAction::EndSession),
                        ))
                        .into_any_element(),
                )
                .into_any_element()
        } else {
            let mut stage = div()
                .w(px(card_width))
                .h(px(stage_height))
                .flex_none()
                .flex()
                .flex_col()
                .p(px(STAGE_PADDING))
                .rounded(px(STAGE_RADIUS))
                .bg(colors.surface0)
                .child(div().flex().items_center().justify_between().child(
                    div().flex().gap(px(SPACE_GROUP)).children(
                        tags.iter().map(|tag| chip(tag.clone(), colors)),
                    ),
                ));
            let (question_size, answer_size) = stage_text_sizes(
                question,
                answer,
                stage_height,
                card_width,
                phase,
            );
            let content_height = stage_content_height(stage_height);
            let center = div()
                .id("review-stage-scroll")
                .flex_1()
                .min_h(px(0.))
                .track_scroll(&self.review_stage_scroll)
                .overflow_scroll();
            let occlusion_image = self.review.occlusion_image.as_ref();
            let masked = question
                .filter(|card| !card.masks.is_empty())
                .and(occlusion_image.as_ref());
            let stage_media = if let Some(card) = question {
                if let Some(image) = masked {
                    let mut media = div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(SPACE_GROUP))
                        .child(card_view::occlusion_media(
                            &self.options.paths.media_dir,
                            &image.name,
                            &card.masks,
                            colors,
                            phase == Phase::Answer,
                            image_height,
                            self.review.occlusion_image_size,
                        ));
                    let rest = card
                        .media
                        .iter()
                        .filter(|media| Some(*media) != occlusion_image)
                        .cloned()
                        .collect::<Vec<_>>();
                    if let Some(rest) = card_view::media_column(
                        &rest,
                        &self.options.paths.media_dir,
                        colors,
                        image_height,
                    ) {
                        media = media.child(rest);
                    }
                    Some(media)
                } else {
                    card_view::media_column(
                        &card.media,
                        &self.options.paths.media_dir,
                        colors,
                        image_height,
                    )
                }
            } else {
                None
            };
            let mut content = div()
                .min_h(px(content_height))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_GROUP))
                .children(stage_media)
                .children((phase == Phase::Question).then(|| {
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(colors.subtext)
                        .child("RECALL THE MEANING")
                }))
                .child(if let Some(card) = question {
                    card_view::rendered_text(
                        card,
                        colors,
                        colors.text,
                        question_size,
                    )
                    .max_w(px(text_width))
                    .into_any_element()
                } else {
                    div()
                        .text_size(px(84.))
                        .child("No cards due")
                        .into_any_element()
                })
                .children(reading.as_ref().map(|text| {
                    div()
                        .text_size(px(18.))
                        .text_color(colors.subtext)
                        .child(text.clone())
                }));
            if phase == Phase::Answer {
                let answer_size = answer_size.unwrap_or(32.0);
                let mut answer_group = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(SPACE_GROUP))
                    .child(separator(colors))
                    .child(if let Some(card) = answer {
                        card_view::rendered_text(
                            card,
                            colors,
                            colors.mauve,
                            answer_size,
                        )
                        .max_w(px(text_width))
                        .into_any_element()
                    } else {
                        div().into_any_element()
                    });
                if let Some(media) = answer_media {
                    answer_group = answer_group.children(media);
                }
                let key = current
                    .map_or(0, |card| usize::try_from(card.id).unwrap_or(0));
                let origin = self.review_answer_origin.clone();
                let answer_track = div()
                    .on_children_prepainted(move |bounds, window, _| {
                        if let Some(child) = bounds.first() {
                            origin.set(Some(
                                child.origin - window.element_offset(),
                            ));
                        }
                    })
                    .id("review-answer-anchor")
                    .anchor_scroll(self.review_answer_anchor.clone())
                    .child(answer_group);
                content = content.child(fade_in(
                    answer_track,
                    SharedString::from(format!("answer-{key}-{animation_seq}")),
                    quick,
                ));
            }
            stage = stage.child(center.child(content));
            let stage = gpui::image_cache(self.images.clone()).child(stage);
            let key = current
                .as_ref()
                .map_or(0, |card| usize::try_from(card.id).unwrap_or(0));

            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_SECTION))
                .child(fade_in(
                    stage,
                    SharedString::from(format!("stage-{key}-{animation_seq}")),
                    quick,
                ))
                .child(if phase == Phase::Question {
                    div()
                        .id("review-reveal")
                        .w(px(card_width))
                        .h(px(CONTROL_HEIGHT))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(SPACE_RELATED))
                        .rounded(px(18.))
                        .bg(colors.mauve)
                        .text_color(style.ink())
                        .text_size(px(17.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .focusable()
                        .track_focus(&reveal_focus)
                        .key_context("ranki_button")
                        .border_1()
                        .border_color(alpha(colors.text, 0.0))
                        .hover(move |button| button.bg(accent_hover(theme)))
                        .active(move |button| button.bg(accent_press(theme)))
                        .focus(move |button| button.border_color(style.ink()))
                        .on_click(reveal)
                        .child("Show answer")
                        .child(keycap("Space", colors, true))
                        .into_any_element()
                } else {
                    rating_row(card_width, colors, &previews, self, compact, cx)
                        .into_any_element()
                })
                .into_any_element()
        };
        screen = screen.child(body);

        let undo = self.action_click(UiAction::Undo);
        let help = self.action_click(UiAction::ToggleHelp);
        let add = self.action_click(UiAction::OpenAdd);
        screen.child(
            div()
                .h(px(FOOTER_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .px(px(SPACE_GUTTER))
                .child(
                    div()
                        .flex()
                        .gap(px(SPACE_INLINE))
                        .child(footer_action(
                            "Undo",
                            (!compact).then_some("Ctrl Z"),
                            Some("icon-undo"),
                            colors,
                            can_undo,
                            undo,
                        ))
                        .child(footer_action(
                            "Shortcuts",
                            (!compact).then_some("?"),
                            None,
                            colors,
                            true,
                            help,
                        )),
                )
                .children(self.render_review_sync())
                .child(div().flex().gap(px(SPACE_INLINE)).child(
                    footer_action(
                        "Add card",
                        (!compact).then_some("A"),
                        Some("icon-plus"),
                        colors,
                        true,
                        add,
                    ),
                )),
        )
    }
}

/// Height available to the scrolling stage body inside the stage padding.
const fn stage_content_height(stage_height: f32) -> f32 {
    (stage_height - 2.0 * STAGE_PADDING - 56.0).max(0.0)
}
fn card_text_width_limit(
    card: &card_view::RenderedCard,
    stage_width: f32,
) -> f32 {
    let widest_line = card
        .text
        .lines()
        .map(str::chars)
        .map(Iterator::count)
        .max()
        .unwrap_or(1)
        .max(1);
    (stage_width - 2.0 * STAGE_PADDING).max(0.0) / (widest_line as f32 * 0.62)
}

fn card_text_size(
    card: &card_view::RenderedCard,
    stage_height: f32,
    stage_width: f32,
    media: bool,
) -> f32 {
    let characters = card.text.chars().count();
    let lines = card.text.lines().count().max(1);
    let estimated_lines = characters.div_ceil(40).max(lines);
    let base = if media { 48.0 } else { 84.0 };
    let available = stage_height * if media { 0.42 } else { 0.68 };
    (available / (estimated_lines as f32 * 1.25))
        .min(base)
        .min(card_text_width_limit(card, stage_width))
}

/// Sizes the question and answer text for one stage frame. The answer size is
/// only computed once the answer is visible.
fn stage_text_sizes(
    question: Option<&card_view::RenderedCard>,
    answer: Option<&card_view::RenderedCard>,
    stage_height: f32,
    stage_width: f32,
    phase: Phase,
) -> (f32, Option<f32>) {
    let question_size = question.map_or(84.0, |card| {
        card_text_size(
            card,
            stage_height,
            stage_width,
            !card.media.is_empty() || !card.masks.is_empty(),
        )
    });
    let question_size = if phase == Phase::Answer {
        question_size.min(40.0)
    } else {
        question_size
    };
    let answer_size = (phase == Phase::Answer).then(|| {
        answer.map_or(32.0, |card| {
            card_text_size(
                card,
                stage_height,
                stage_width,
                !card.media.is_empty() || !card.masks.is_empty(),
            )
            .clamp(22.0, 32.0)
            .min(card_text_width_limit(card, stage_width))
        })
    });
    (question_size, answer_size)
}

const BACK_HINT: &str = "Space / Esc";
const STUDY_HINT: &str = "S";

/// Note under a completed summary when some cards could not be loaded.
fn skipped_copy(load_errors: usize) -> String {
    if load_errors == 1 {
        String::from("1 card was skipped.")
    } else {
        format!("{load_errors} cards were skipped.")
    }
}

/// Headline for a finished session, sized by what actually happened.
fn summary_title(answered: usize) -> String {
    match answered {
        0 => String::from("Nothing due"),
        1 => String::from("One card reviewed"),
        _ => format!("{answered} cards reviewed"),
    }
}

fn summary_tile(
    label: &str,
    value: &str,
    hue: Rgba,
    colors: crate::theme::Palette,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_INLINE))
        .w(px(160.))
        .p(px(SPACE_RELATED))
        .rounded(px(16.))
        .border_1()
        .border_color(colors.surface1)
        .bg(colors.mantle)
        .child(
            div()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.subtext)
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_size(px(22.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(hue)
                .child(value.to_owned()),
        )
}

fn separator(colors: crate::theme::Palette) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_RELATED))
        .w_full()
        .my(px(SPACE_RELATED))
        .child(div().h(px(1.)).flex_1().bg(colors.surface1))
        .child(
            div()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.subtext)
                .child("ANSWER"),
        )
        .child(div().h(px(1.)).flex_1().bg(colors.surface1))
}

/// A field named Reading, Kana, or Pronunciation, when the note has one.
fn reading_label(models: &[NoteModel], note: Option<&Note>) -> Option<String> {
    let note = note?;
    let model = models.iter().find(|model| model.id == note.model_id)?;
    let index = model.fields.iter().position(|field| {
        field.eq_ignore_ascii_case("reading")
            || field.eq_ignore_ascii_case("kana")
            || field.eq_ignore_ascii_case("pronunciation")
    })?;
    let text = note.fields.get(index)?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn rating_row(
    card_width: f32,
    colors: crate::theme::Palette,
    previews: &[i32; 4],
    root: &Root,
    compact: bool,
    cx: &Context<Root>,
) -> gpui::Div {
    let cell_height = if compact {
        COMPACT_RATING_CELL_HEIGHT
    } else {
        CONTROL_HEIGHT
    };
    let grid_height = if compact {
        2.0 * cell_height + RATING_GAP
    } else {
        CONTROL_HEIGHT
    };
    let cell_width = (card_width - RATING_GAP).max(0.0) / 2.0;
    let mut row = div()
        .w(px(card_width))
        .h(px(grid_height))
        .flex()
        .gap(px(RATING_GAP));
    if compact {
        row = row.flex_wrap();
    }
    for (index, (rating, interval)) in [
        (ranki_model::Rating::Again, previews[0]),
        (ranki_model::Rating::Hard, previews[1]),
        (ranki_model::Rating::Good, previews[2]),
        (ranki_model::Rating::Easy, previews[3]),
    ]
    .into_iter()
    .enumerate()
    {
        let code = rating.code();
        let action = UiAction::Grade(rating);
        let theme = theme_from(colors);
        let hue = rating_fill(code, theme);
        let label_color = rating_label(code, theme);
        let hover_fill = alpha(hue, 0.22);
        let hover_border = alpha(hue, 0.6);
        let press_fill = alpha(hue, 0.3);
        let press_border = alpha(hue, 0.8);
        let focus = review_button_handle(root, cx, index + 2).tab_stop(true);
        let mut grade = div()
            .id(SharedString::from(format!("review-rating-{code}")))
            .debug_selector(move || format!("review-rating-{code}"))
            .h(px(cell_height))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SPACE_INLINE))
            .rounded(px(18.))
            .bg(alpha(hue, 0.14))
            .border_1()
            .border_color(alpha(hue, 0.4))
            .cursor_pointer()
            .focusable()
            .track_focus(&focus)
            .key_context("ranki_button")
            .hover(move |button| {
                button.bg(hover_fill).border_color(hover_border)
            })
            .active(move |button| {
                button.bg(press_fill).border_color(press_border)
            })
            .focus(move |button| button.border_color(hue))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.dispatch(action.clone(), window, cx);
            }))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(colors.text)
                    .child(interval_label(interval)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_GROUP))
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(label_color)
                    .child(rating_name(rating))
                    .child(
                        div()
                            .px(px(SPACE_GROUP))
                            .py(px(SPACE_INLINE))
                            .rounded(px(6.))
                            .bg(alpha(hue, 0.2))
                            .text_size(px(11.))
                            .child(format!("{}", index + 1)),
                    ),
            );
        if compact {
            grade = grade.w(px(cell_width)).flex_none();
        } else {
            grade = grade.flex_1();
        }
        row = row.child(grade);
    }
    row
}

fn theme_from(colors: crate::theme::Palette) -> crate::theme::Theme {
    if colors.base == crate::theme::MOCHA.base {
        crate::theme::Theme::Dark
    } else {
        crate::theme::Theme::Light
    }
}

/// Footer hint row. Disabled actions keep their key badge off and explain the
/// reason through their label, so a dead control never looks live.
fn footer_action(
    label: &'static str,
    key: Option<&'static str>,
    icon_name: Option<&'static str>,
    colors: crate::theme::Palette,
    enabled: bool,
    on_click: ClickHandler,
) -> gpui::Stateful<gpui::Div> {
    let ink = if enabled {
        colors.subtext
    } else {
        alpha(colors.subtext, 0.4)
    };
    let tight = key.is_none();
    let gap = if tight { SPACE_INLINE } else { SPACE_GROUP };
    let padding = if tight { SPACE_INLINE } else { SPACE_RELATED };
    let mut row = div()
        .id(SharedString::from(format!("footer-{label}")))
        .key_context("ranki_button")
        .flex()
        .items_center()
        .gap(px(gap))
        .h(px(36.))
        .px(px(padding))
        .rounded(px(10.))
        .text_size(px(13.))
        .text_color(ink)
        .children(icon_name.map(|name| icon(name, 16., ink)))
        .child(label.to_owned())
        .children(
            key.filter(|_| enabled)
                .map(|key| keycap(key.to_owned(), colors, false)),
        );
    if enabled {
        row = Interaction::surface(colors)
            .apply(row.cursor_pointer())
            .on_click(on_click);
    }
    row
}

/// One filled or outlined button for the session summary.
fn footer_button(
    label: &'static str,
    colors: crate::theme::Palette,
    primary: bool,
    key: Option<&'static str>,
    on_click: ClickHandler,
) -> impl IntoElement {
    let theme = theme_from(colors);
    let (bg, ink) = if primary {
        (colors.mauve, crate::theme::accent_ink(theme))
    } else {
        (colors.surface0, colors.text)
    };
    let hover = if primary {
        accent_hover(theme)
    } else {
        crate::theme::blend(colors.surface0, colors.surface1, 0.5)
    };
    let border = if primary {
        colors.mauve
    } else {
        colors.surface1
    };
    let press = if primary {
        accent_press(theme)
    } else {
        crate::theme::blend(colors.surface0, colors.surface1, 0.75)
    };
    Interaction::new(
        border,
        hover,
        press,
        if primary {
            crate::theme::accent_ink(theme)
        } else {
            colors.mauve
        },
    )
    .apply(
        div()
            .id(SharedString::from(format!("summary-{label}")))
            .key_context("ranki_button")
            .flex()
            .items_center()
            .justify_center()
            .h(px(BUTTON_HEIGHT))
            .px(px(BUTTON_PADDING_X))
            .rounded(px(BUTTON_RADIUS))
            .bg(bg)
            .text_color(ink)
            .text_size(px(BUTTON_TEXT_SIZE))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .cursor_pointer(),
    )
    .on_click(on_click)
    .child(label)
    .children(key.map(|key| keycap(key, colors, primary)))
}

#[cfg(test)]
mod tests {
    use crate::{
        app::Phase,
        theme::{Theme, rating_fill},
    };

    const STAGE_WIDTH: f32 = 576.0;
    #[test]
    fn question_phase_hides_answer_controls() {
        assert_ne!(Phase::Question, Phase::Answer);
    }

    #[test]
    fn rating_palette_uses_peach_for_hard() {
        assert_eq!(rating_fill(2, Theme::Dark), crate::theme::MOCHA.peach);
    }

    #[test]
    fn long_card_text_uses_a_smaller_reading_size() {
        let short = crate::card_view::RenderedCard {
            text: "Short question".into(),
            ..Default::default()
        };
        let long = crate::card_view::RenderedCard {
            text: "A long question ".repeat(40),
            ..Default::default()
        };
        assert!(
            super::card_text_size(&long, 322.0, STAGE_WIDTH, false)
                < super::card_text_size(&short, 322.0, STAGE_WIDTH, false)
        );
    }

    #[test]
    fn card_text_fits_a_narrow_stage() {
        let card = crate::card_view::RenderedCard {
            text: "Question?".into(),
            ..Default::default()
        };
        let wide = super::card_text_size(&card, 322.0, 576.0, false);
        let narrow = super::card_text_size(&card, 322.0, 288.0, false);
        assert!(narrow < wide, "{narrow} should be smaller than {wide}");
    }

    #[test]
    fn long_unbroken_answers_stay_inside_a_narrow_stage() {
        let question = crate::card_view::RenderedCard {
            text: "Question?".into(),
            ..Default::default()
        };
        let answer = crate::card_view::RenderedCard {
            text: "Supercalifragilisticexpialidocious".into(),
            ..Default::default()
        };
        let (_, size) = super::stage_text_sizes(
            Some(&question),
            Some(&answer),
            322.0,
            288.0,
            crate::app::Phase::Answer,
        );
        let limit = super::card_text_width_limit(&answer, 288.0);
        assert!(size.is_some_and(|size| size <= limit));
    }

    #[test]
    fn question_layout_does_not_depend_on_the_hidden_answer() {
        let question = crate::card_view::RenderedCard {
            text: "Question?".into(),
            ..Default::default()
        };
        let short_answer = crate::card_view::RenderedCard {
            text: "Answer".into(),
            ..Default::default()
        };
        let long_answer = crate::card_view::RenderedCard {
            text: "A long answer ".repeat(400),
            ..Default::default()
        };
        let (question_only, hidden) = super::stage_text_sizes(
            Some(&question),
            None,
            322.0,
            STAGE_WIDTH,
            Phase::Question,
        );
        let (with_short, short_size) = super::stage_text_sizes(
            Some(&question),
            Some(&short_answer),
            322.0,
            STAGE_WIDTH,
            Phase::Question,
        );
        let (with_long, long_size) = super::stage_text_sizes(
            Some(&question),
            Some(&long_answer),
            322.0,
            STAGE_WIDTH,
            Phase::Question,
        );
        assert_eq!(question_only, with_short);
        assert_eq!(question_only, with_long);
        assert_eq!(hidden, None);
        assert_eq!(short_size, None);
        assert_eq!(long_size, None);

        let (revealed, answer_size) = super::stage_text_sizes(
            Some(&question),
            Some(&long_answer),
            322.0,
            STAGE_WIDTH,
            Phase::Answer,
        );
        assert_eq!(revealed, question_only.min(40.0));
        assert!(answer_size.is_some());
        let (short_reveal, short_answer_size) = super::stage_text_sizes(
            Some(&question),
            Some(&short_answer),
            322.0,
            STAGE_WIDTH,
            Phase::Answer,
        );
        assert_eq!(revealed, short_reveal);
        assert_eq!(
            answer_size,
            Some(
                super::card_text_size(&long_answer, 322.0, STAGE_WIDTH, false)
                    .clamp(22.0, 32.0)
                    .min(super::card_text_width_limit(
                        &long_answer,
                        STAGE_WIDTH,
                    ))
            )
        );
        assert_eq!(
            short_answer_size,
            Some(
                super::card_text_size(&short_answer, 322.0, STAGE_WIDTH, false)
                    .clamp(22.0, 32.0)
                    .min(super::card_text_width_limit(
                        &short_answer,
                        STAGE_WIDTH,
                    ))
            )
        );
    }

    #[test]
    fn short_viewports_leave_room_for_grading_controls() {
        for width in [320.0, 400.0, 720.0] {
            let stage = (600.0_f32
                - 72.0
                - 64.0
                - super::rating_control_height(width)
                - 48.0
                - 18.0)
                .clamp(220.0, crate::theme::STAGE_HEIGHT);
            assert!(
                stage
                    + 72.0
                    + 64.0
                    + super::rating_control_height(width)
                    + 48.0
                    + 18.0
                    <= 600.0
            );
        }
    }

    #[test]
    fn narrow_review_uses_a_two_row_rating_grid() {
        assert!(super::compact_review(320.0));
        assert!(!super::compact_review(400.0));
        assert_eq!(super::rating_control_height(320.0), 100.0);
        assert_eq!(super::rating_control_height(400.0), 76.0);
    }

    #[test]
    fn progress_keeps_equal_slots_at_both_breakpoints() {
        let wide = super::balanced_progress_width(900.0);
        let narrow = super::balanced_progress_width(720.0);
        assert_eq!(wide, 420.0);
        assert_eq!(narrow, 240.0);
        assert!(900.0 - 2.0 * 32.0 - wide >= 180.0 * 2.0);
        assert!(720.0 - 2.0 * 32.0 - narrow >= 180.0 * 2.0);
    }

    #[test]
    fn stage_body_keeps_the_stage_padding() {
        assert_eq!(
            super::stage_content_height(440.0),
            440.0 - 2.0 * super::STAGE_PADDING - 56.0
        );
        assert_eq!(super::stage_content_height(80.0), 0.0);
    }

    #[test]
    fn summary_titles_count_cards() {
        assert_eq!(super::summary_title(0), "Nothing due");
        assert_eq!(super::summary_title(1), "One card reviewed");
        assert_eq!(super::summary_title(12), "12 cards reviewed");
    }
    #[test]
    fn skipped_copy_counts_the_cards_that_failed_to_load() {
        assert_eq!(super::skipped_copy(1), "1 card was skipped.");
        assert_eq!(super::skipped_copy(3), "3 cards were skipped.");
    }

    #[test]
    fn summary_badges_name_the_available_keys() {
        assert_eq!(super::BACK_HINT, "Space / Esc");
        assert_eq!(super::STUDY_HINT, "S");
    }
}
