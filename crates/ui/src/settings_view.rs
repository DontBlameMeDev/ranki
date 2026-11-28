//! Settings screen for theme and optional `AnkiWeb` sync.

use gpui::{
    FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::{
    ThemeChoice,
    app::{Root, UiAction},
    components::{ClickHandler, Interaction, icon, panel},
    theme::{
        PAGE_X, PAGE_Y, Palette, SPACE_INLINE, SPACE_RELATED, SPACE_SECTION,
        alpha,
    },
};

fn choice_segment(
    id: String,
    label: &'static str,
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
    .child(label)
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
            .child(self.render_sync())
    }
}
