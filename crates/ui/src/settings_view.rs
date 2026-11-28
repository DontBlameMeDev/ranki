//! Settings screen for theme and optional `AnkiWeb` sync.

use gpui::{
    Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};

use ranki_model::{LeechAction, Rating};

use crate::{
    ThemeChoice,
    app::{Root, UiAction},
    components::{
        ButtonTone, ClickHandler, Interaction, action_button, icon, panel,
    },
    theme::{
        PAGE_X, PAGE_Y, Palette, SPACE_GROUP, SPACE_INLINE, SPACE_RELATED,
        SPACE_SECTION, alpha,
    },
};

fn choice_segment(
    id: String,
    label: impl Into<SharedString>,
    selected: bool,
    colors: Palette,
    on_click: ClickHandler,
) -> impl IntoElement {
    let ink = if selected {
        colors.text
    } else {
        colors.subtext
    };
    let debug = id.clone();
    let (hover, press) = if selected {
        (alpha(colors.mauve, 0.32), alpha(colors.mauve, 0.42))
    } else {
        (alpha(colors.mauve, 0.12), alpha(colors.mauve, 0.28))
    };
    let fill = if selected {
        alpha(colors.mauve, 0.22)
    } else {
        colors.surface0
    };
    Interaction::new(
        if selected {
            colors.mauve
        } else {
            alpha(colors.text, 0.0)
        },
        hover,
        press,
        if selected { colors.text } else { colors.mauve },
    )
    .apply(
        div()
            .id(SharedString::from(id))
            .debug_selector(move || debug)
            .flex()
            .flex_col()
            .gap(px(SPACE_INLINE))
            .flex_1()
            .min_w(px(80.))
            .h(px(44.))
            .items_center()
            .justify_center()
            .rounded(px(9.))
            .bg(fill)
            .text_color(ink)
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer(),
    )
    .on_click(on_click)
    .child(label.into())
}

fn root_click(
    action: impl Fn(&mut Root, &mut Context<Root>) + 'static,
) -> ClickHandler {
    Box::new(move |_event, window, cx| {
        let Some(Some(root)) = window.root::<Root>() else {
            return;
        };
        root.update(cx, |root, cx| action(root, cx));
    })
}

fn segment_row(colors: Palette) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .min_w(px(0.))
        .gap(px(SPACE_INLINE))
        .p(px(SPACE_INLINE))
        .rounded(px(12.))
        .bg(colors.surface0)
}

fn setting_row(
    label: impl Into<SharedString>,
    control: impl IntoElement,
    colors: Palette,
) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap(px(SPACE_GROUP))
        .text_color(colors.text)
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.into()),
        )
        .child(control)
}

fn caption(text: &'static str, colors: Palette) -> Div {
    div()
        .pl(px(SPACE_GROUP))
        .text_size(px(12.))
        .text_color(colors.subtext)
        .child(text)
}

fn stepper(
    minus_id: &'static str,
    plus_id: &'static str,
    value: impl Into<SharedString>,
    style: crate::theme::Style,
    minus: ClickHandler,
    plus: ClickHandler,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_INLINE))
        .child(action_button(
            minus_id,
            "−",
            ButtonTone::Neutral,
            style,
            minus,
        ))
        .child(
            div()
                .min_w(px(32.))
                .text_center()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(value.into()),
        )
        .child(action_button(
            plus_id,
            "+",
            ButtonTone::Neutral,
            style,
            plus,
        ))
}

const fn easy_day_choice(value: f32) -> usize {
    if value < 0.25 {
        0
    } else if value < 0.75 {
        1
    } else {
        2
    }
}
impl Root {
    pub(crate) fn render_settings(&self) -> impl IntoElement {
        let colors = self.colors();
        let chosen = self.options.theme;

        let mut theme_segments = div()
            .flex()
            .flex_wrap()
            .items_center()
            .min_w(px(0.))
            .gap(px(SPACE_INLINE))
            .p(px(SPACE_INLINE))
            .rounded(px(12.))
            .bg(colors.surface0);
        for (choice, label) in [
            (ThemeChoice::Light, "Light"),
            (ThemeChoice::Dark, "Dark"),
            (ThemeChoice::System, "System"),
        ] {
            theme_segments = theme_segments.child(choice_segment(
                format!("theme-{choice:?}"),
                label,
                chosen == choice,
                colors,
                self.action_click(UiAction::SetTheme(choice)),
            ));
        }

        let motion = self.options.reduce_motion;
        let mut motion_segments = div()
            .flex()
            .flex_wrap()
            .items_center()
            .min_w(px(0.))
            .gap(px(SPACE_INLINE))
            .p(px(SPACE_INLINE))
            .rounded(px(12.))
            .bg(colors.surface0);
        for (choice, label) in [
            (Some(true), "Reduce"),
            (Some(false), "Full"),
            (None, "System"),
        ] {
            motion_segments = motion_segments.child(choice_segment(
                format!("motion-{choice:?}"),
                label,
                motion == choice,
                colors,
                self.action_click(UiAction::SetReduceMotion(choice)),
            ));
        }
        div()
            .id("settings-scroll")
            .track_focus(&self.settings_focus)
            .key_context("ranki_settings")
            .size_full()
            .flex()
            .flex_col()
            .gap(px(SPACE_SECTION))
            .overflow_scroll()
            .track_scroll(&self.settings_scroll)
            .px(px(PAGE_X))
            .py(px(PAGE_Y))
            .child(
                div()
                    .text_size(px(32.))
                    .font_weight(FontWeight::BOLD)
                    .child("Settings"),
            )
            .child(
                panel(colors)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_RELATED))
                            .child(icon("icon-settings", 18., colors.mauve))
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::BOLD)
                                    .child("Appearance"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap(px(SPACE_SECTION))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Theme"),
                            )
                            .child(theme_segments),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap(px(SPACE_SECTION))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Reduce motion"),
                            )
                            .child(motion_segments),
                    ),
            )
            .child(self.render_study())
            .child(self.render_sync())
    }

    pub(crate) fn render_study(&self) -> Div {
        let colors = self.colors();
        let style = self.style();
        let presets = &self.study_presets;
        let preset = self.study_preset();
        let preset_id = preset.map(|preset| preset.id);
        let preset_name = preset
            .map(|preset| preset.name.as_str())
            .or_else(|| presets.first().map(|preset| preset.name.as_str()))
            .unwrap_or("Default");
        let default_easy_days = [1.0_f32; 7];
        let easy_days = preset.map_or(&default_easy_days[..], |preset| {
            preset.easy_days_percentages.as_slice()
        });
        let leech_enabled = self.options.study.leech_enabled;
        let leech_threshold = preset.map_or(8, |preset| preset.leech_threshold);
        let leech_action =
            preset.map_or(LeechAction::TagOnly, |preset| preset.leech_action);

        let preset_control = if presets.len() > 1 {
            let mut segments = segment_row(colors);
            for candidate in presets {
                let id = candidate.id;
                segments = segments.child(choice_segment(
                    format!("study-preset-{id}"),
                    candidate.name.clone(),
                    preset_id == Some(id),
                    colors,
                    root_click(move |root, cx| {
                        root.select_study_preset(id, cx);
                    }),
                ));
            }
            segments
        } else {
            div()
                .text_size(px(13.))
                .text_color(colors.text)
                .child(preset_name.to_owned())
        };

        let mut leech_modes = segment_row(colors);
        for (enabled, label) in [(true, "On"), (false, "Off")] {
            leech_modes = leech_modes.child(choice_segment(
                format!("study-leech-{enabled}"),
                label,
                leech_enabled == enabled,
                colors,
                root_click(move |root, cx| {
                    root.set_leech_enabled(enabled, cx);
                }),
            ));
        }

        let threshold_control = stepper(
            "study-threshold-minus",
            "study-threshold-plus",
            leech_threshold.to_string(),
            style,
            root_click(move |root, cx| {
                root.set_leech_threshold(
                    leech_threshold.saturating_sub(1).max(1),
                    cx,
                );
            }),
            root_click(move |root, cx| {
                root.set_leech_threshold(leech_threshold.saturating_add(1), cx);
            }),
        );

        let mut leech_actions = segment_row(colors);
        for (action, label) in [
            (LeechAction::TagOnly, "Tag only"),
            (LeechAction::Suspend, "Suspend"),
        ] {
            leech_actions = leech_actions.child(choice_segment(
                format!("study-action-{action:?}"),
                label,
                leech_action == action,
                colors,
                root_click(move |root, cx| {
                    root.set_leech_action(action, cx);
                }),
            ));
        }

        let rollover = self.rollover;
        let rollover_control = stepper(
            "study-rollover-minus",
            "study-rollover-plus",
            rollover.to_string(),
            style,
            root_click(move |root, cx| {
                root.set_rollover(rollover.saturating_sub(1), cx);
            }),
            root_click(move |root, cx| {
                root.set_rollover(rollover.saturating_add(1).min(23), cx);
            }),
        );

        let mut answer_rows = div().flex().flex_col().gap(px(SPACE_GROUP));
        for (rating, label) in [
            (Rating::Again, "Again"),
            (Rating::Hard, "Hard"),
            (Rating::Good, "Good"),
            (Rating::Easy, "Easy"),
        ] {
            answer_rows = answer_rows.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(SPACE_GROUP))
                    .child(
                        div()
                            .w(px(52.))
                            .flex_none()
                            .text_size(px(12.))
                            .text_color(colors.subtext)
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(140.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(colors.surface1)
                            .bg(colors.base)
                            .px(px(SPACE_GROUP))
                            .py(px(SPACE_INLINE))
                            .child(self.answer_field(rating).clone()),
                    ),
            );
        }

        let mut easy_rows = div().flex().flex_col().gap(px(SPACE_GROUP));
        for (index, day) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            .into_iter()
            .enumerate()
        {
            let selected =
                easy_day_choice(easy_days.get(index).copied().unwrap_or(1.0));
            let mut segments = segment_row(colors);
            for (choice, (label, amount)) in [
                (0, ("Minimum", 0.0_f32)),
                (1, ("Reduced", 0.5)),
                (2, ("Normal", 1.0)),
            ] {
                segments = segments.child(choice_segment(
                    format!("study-easy-{index}-{choice}"),
                    label,
                    selected == choice,
                    colors,
                    root_click(move |root, cx| {
                        root.set_easy_day(index, amount, cx);
                    }),
                ));
            }
            easy_rows = easy_rows.child(setting_row(day, segments, colors));
        }

        panel(colors)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_RELATED))
                    .child(icon("icon-book", 18., colors.mauve))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .child("Study"),
                    ),
            )
            .child(setting_row("Options preset", preset_control, colors))
            .child(setting_row("Mark leeches", leech_modes, colors))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_INLINE))
                    .child(setting_row(
                        "Leech threshold",
                        threshold_control,
                        colors,
                    ))
                    .child(caption("Again presses on a review card", colors)),
            )
            .child(setting_row("Leech action", leech_actions, colors))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_INLINE))
                    .child(setting_row(
                        "Next day starts at",
                        rollover_control,
                        colors,
                    ))
                    .child(caption("hours past midnight", colors)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_GROUP))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Answer keys"),
                    )
                    .child(answer_rows)
                    .child(caption(
                        "Anki QKeySequence. Empty disables the key.",
                        colors,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_GROUP))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Easy days"),
                    )
                    .child(easy_rows)
                    .child(caption(
                        "Anki uses these with load balancing.",
                        colors,
                    )),
            )
    }
}
