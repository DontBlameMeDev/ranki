//! Shared GPUI elements for the three views.

use gpui::{
    App, ClickEvent, Div, FocusHandle, ImageSource, Img, InteractiveElement,
    IntoElement, MouseDownEvent, ParentElement, Resource, Rgba, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Svg, Window, div, img, px,
    svg,
};

use crate::theme::{
    BUTTON_HEIGHT, BUTTON_PADDING_X, BUTTON_RADIUS, BUTTON_TEXT_SIZE, Palette,
    SPACE_BLOCK, SPACE_GROUP, SPACE_INLINE, SPACE_RELATED, SPACE_SECTION,
    Style, accent_hover, accent_press, alpha, blend, danger_hover,
    danger_press, keycap_ink,
};

pub type ClickHandler =
    Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Shared rest, hover, pressed, and focus colors for one clickable element.
/// The border is always reserved, so a focus ring never moves nearby content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interaction {
    border: Rgba,
    hover: Rgba,
    press: Rgba,
    focus: Rgba,
}

impl Interaction {
    #[must_use]
    pub const fn new(
        border: Rgba,
        hover: Rgba,
        press: Rgba,
        focus: Rgba,
    ) -> Self {
        Self {
            border,
            hover,
            press,
            focus,
        }
    }

    /// Controls sitting on a dark page: menu entries, text actions, icons.
    #[must_use]
    pub const fn surface(colors: Palette) -> Self {
        Self::new(
            alpha(colors.text, 0.0),
            colors.surface0,
            colors.surface1,
            colors.mauve,
        )
    }

    /// Controls whose own fill already uses `surface0`.
    #[must_use]
    pub fn raised(colors: Palette) -> Self {
        Self::new(
            alpha(colors.text, 0.0),
            blend(colors.surface0, colors.surface1, 0.5),
            colors.surface1,
            colors.mauve,
        )
    }

    /// Filled accent buttons. The focus ring uses the button's ink so it
    /// stays visible against the mauve fill in both themes.
    #[must_use]
    pub fn accent(style: Style) -> Self {
        Self::new(
            alpha(style.colors.text, 0.0),
            accent_hover(style.theme),
            accent_press(style.theme),
            style.ink(),
        )
    }

    /// Destructive menu entries.
    #[must_use]
    pub const fn danger(colors: Palette) -> Self {
        Self::new(
            alpha(colors.text, 0.0),
            alpha(colors.red, 0.14),
            alpha(colors.red, 0.26),
            colors.red,
        )
    }

    /// Controls tinted by a semantic hue, such as queue or rating colors.
    #[must_use]
    pub fn tinted(colors: Palette, hue: Rgba) -> Self {
        Self::new(
            alpha(colors.text, 0.0),
            alpha(hue, 0.2),
            alpha(hue, 0.32),
            hue,
        )
    }

    #[must_use]
    pub fn apply(self, element: Stateful<Div>) -> Stateful<Div> {
        element
            .focusable()
            .border_1()
            .border_color(self.border)
            .hover(move |style| style.bg(self.hover))
            .active(move |style| style.bg(self.press))
            .focus(move |style| style.border_color(self.focus))
    }
}

/// Lucide-style icon served by the embedded asset source.
///
/// GPUI paints an SVG mask in the element's own text color, so every call
/// must name its tint.
#[must_use]
pub fn icon(name: &'static str, size: f32, color: Rgba) -> Svg {
    svg()
        .path(name)
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(color)
}

/// Search glyph that turns mauve while its field is focused.
#[must_use]
pub fn search_field_icon(
    focus: &FocusHandle,
    size: f32,
    colors: Palette,
    window: &Window,
) -> Svg {
    let color = if focus.is_focused(window) {
        colors.mauve
    } else {
        colors.subtext
    };
    icon("icon-search", size, color)
}

/// 36x36 tinted tile holding a hue-colored icon.
#[must_use]
pub fn icon_tile(name: &'static str, hue: Rgba) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(36.))
        .h(px(36.))
        .flex_none()
        .rounded(px(10.))
        .bg(alpha(hue, 0.16))
        .child(icon(name, 18., hue))
}

/// Visual weight of an action button. Primary marks the intended next step,
/// Neutral covers cancellation and alternatives, Danger marks deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonTone {
    Primary,
    Neutral,
    Danger,
}

/// Outline, rest fill, ink, hover fill, pressed fill, and focus ring.
struct Shades {
    border: Rgba,
    fill: Rgba,
    ink: Rgba,
    hover: Rgba,
    press: Rgba,
    focus: Rgba,
}

fn tone_shades(tone: ButtonTone, style: Style) -> Shades {
    let colors = style.colors;
    match tone {
        ButtonTone::Primary => Shades {
            border: alpha(colors.text, 0.0),
            fill: colors.mauve,
            ink: style.ink(),
            hover: accent_hover(style.theme),
            press: accent_press(style.theme),
            focus: style.ink(),
        },
        ButtonTone::Neutral => neutral_shades(colors),
        ButtonTone::Danger => Shades {
            border: alpha(colors.text, 0.0),
            fill: colors.red,
            ink: style.ink(),
            hover: danger_hover(style.theme),
            press: danger_press(style.theme),
            focus: style.ink(),
        },
    }
}

fn neutral_shades(colors: Palette) -> Shades {
    // The hover stops short of surface1: Latte body text only clears 4.5:1
    // on a fill that stays between surface0 and surface1.
    Shades {
        border: colors.surface1,
        fill: colors.surface0,
        ink: colors.text,
        hover: blend(colors.surface0, colors.surface1, 0.5),
        press: blend(colors.surface0, colors.surface1, 0.75),
        focus: colors.mauve,
    }
}

fn tone_button(
    id: SharedString,
    label: SharedString,
    shades: Shades,
    on_click: ClickHandler,
) -> impl IntoElement {
    let debug = id.clone();
    Interaction::new(shades.border, shades.hover, shades.press, shades.focus)
        .apply(
            div()
                .id(id)
                .debug_selector(move || debug.to_string())
                .flex()
                .items_center()
                .justify_center()
                .gap(px(SPACE_GROUP))
                .h(px(BUTTON_HEIGHT))
                .px(px(BUTTON_PADDING_X))
                .rounded(px(BUTTON_RADIUS))
                .bg(shades.fill)
                .text_color(shades.ink)
                .text_size(px(BUTTON_TEXT_SIZE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .cursor_pointer(),
        )
        .on_click(on_click)
        .child(label)
}

/// Filled action button for forms, dialogs, and empty states.
#[must_use]
pub fn action_button(
    id: &'static str,
    label: impl Into<SharedString>,
    tone: ButtonTone,
    style: Style,
    on_click: ClickHandler,
) -> impl IntoElement {
    tone_button(
        SharedString::from(id),
        label.into(),
        tone_shades(tone, style),
        on_click,
    )
}

/// Standard panel surface used by screens, sync, and dialogs.
#[must_use]
pub fn panel(colors: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_BLOCK))
        .p(px(SPACE_SECTION))
        .rounded(px(24.))
        .bg(colors.mantle)
        .border_1()
        .border_color(colors.surface0)
}

/// Whether a click repeats the mouse press that dismissed a popup.
#[must_use]
pub fn same_press(event: &ClickEvent, press: Option<&MouseDownEvent>) -> bool {
    match event {
        ClickEvent::Mouse(click) => press.is_some_and(|down| {
            down.button == click.down.button
                && down.position == click.down.position
                && down.click_count == click.down.click_count
                && down.first_mouse == click.down.first_mouse
        }),
        ClickEvent::Keyboard(_) => false,
    }
}

/// Outline button for the alternative to a primary action.
#[must_use]
pub fn secondary_button(
    label: impl Into<SharedString>,
    colors: Palette,
    on_click: ClickHandler,
) -> impl IntoElement {
    let label = label.into();
    let id = SharedString::from(format!("secondary-{label}"));
    tone_button(id, label, neutral_shades(colors), on_click)
}

/// Fades an element in where it first paints. The clock starts on the frame
/// that mounts `id`; changing the id replays the fade for new content. A zero
/// duration returns the element unwrapped, which is what reduced motion uses.
pub fn fade_in(
    element: impl IntoElement + gpui::Styled + 'static,
    id: impl Into<gpui::ElementId>,
    milliseconds: u64,
) -> gpui::AnyElement {
    use gpui::AnimationExt as _;
    if milliseconds == 0 {
        return element.into_any_element();
    }
    element
        .with_animation(
            id,
            gpui::Animation::new(std::time::Duration::from_millis(
                milliseconds,
            ))
            .with_easing(|delta| 1.0 - (1.0 - delta) * (1.0 - delta)),
            gpui::Styled::opacity,
        )
        .into_any_element()
}

/// Small key badge. Mauve fills use crust ink on an 18% crust notch.
#[must_use]
pub fn keycap(
    label: impl Into<SharedString>,
    colors: Palette,
    on_accent: bool,
) -> Div {
    let (ink, fill) = if on_accent {
        (colors.crust, alpha(colors.crust, 0.18))
    } else {
        (keycap_ink(colors), colors.surface1)
    };
    div()
        .flex_none()
        .px(px(SPACE_GROUP))
        .py(px(SPACE_INLINE))
        .rounded(px(6.))
        .bg(fill)
        .text_color(ink)
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .child(label.into())
}

/// Rounded tag chip from a note tag.
#[must_use]
pub fn chip(label: impl Into<SharedString>, colors: Palette) -> Div {
    div()
        .flex_none()
        .px(px(SPACE_RELATED))
        .py(px(SPACE_INLINE))
        .rounded(px(10.))
        .bg(colors.surface1)
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.into())
}

/// Editable tag chip: the label plus a close button that owns the removal.
#[must_use]
pub fn removable_chip(
    label: impl Into<SharedString>,
    colors: Palette,
    on_remove: ClickHandler,
) -> impl IntoElement {
    let label = label.into();
    let chip_id = format!("tag-chip-{label}");
    let remove_id = format!("{chip_id}-remove");
    div()
        .id(SharedString::from(chip_id))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SPACE_GROUP))
        .py(px(SPACE_GROUP))
        .pr(px(SPACE_GROUP))
        .pl(px(SPACE_RELATED))
        .rounded(px(12.))
        .bg(colors.surface0)
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label)
        .child(
            Interaction::surface(colors)
                .apply(
                    div()
                        .id(SharedString::from(remove_id))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer(),
                )
                .on_click(on_remove)
                .child(icon("icon-x", 14., colors.overlay)),
        )
}

/// Brand wordmark served by the embedded asset source.
#[must_use]
pub fn brand() -> Img {
    img(ImageSource::Resource(Resource::Embedded("rai.svg".into())))
        .w(px(48.))
        .h(px(48.))
        .flex_none()
        .mb(px(4.))
}

#[must_use]
pub fn dot(color: Rgba, size: f32) -> Div {
    div()
        .w(px(size))
        .h(px(size))
        .rounded_full()
        .bg(color)
        .flex_none()
}

/// Session progress track. The fill moves from `from` to `to` over
/// `milliseconds`; a zero duration draws the target immediately.
#[must_use]
pub fn progress_bar(
    from: f32,
    to: f32,
    colors: Palette,
    width: f32,
    milliseconds: u64,
    id: impl Into<gpui::ElementId> + 'static,
) -> impl IntoElement {
    use gpui::AnimationExt as _;
    let from = from.clamp(0.0, 1.0);
    let to = to.clamp(0.0, 1.0);
    let height = crate::theme::PROGRESS_HEIGHT;
    let fill = div().h(px(height)).rounded(px(3.)).bg(colors.mauve);
    let animated = if milliseconds == 0 {
        fill.w(px(to * width)).into_any_element()
    } else {
        fill.with_animation(
            id,
            gpui::Animation::new(std::time::Duration::from_millis(
                milliseconds,
            ))
            .with_easing(|delta| 1.0 - (1.0 - delta) * (1.0 - delta)),
            move |fill, delta| {
                let value = from + (to - from) * delta;
                fill.w(px(value * width))
            },
        )
        .into_any_element()
    };
    div()
        .w(px(width))
        .h(px(height))
        .rounded(px(3.))
        .bg(colors.surface0)
        .child(animated)
}

#[cfg(test)]
mod tests {
    use super::{ButtonTone, Interaction, tone_shades};
    use crate::theme::{
        LATTE, MOCHA, Style, Theme, accent_hover, accent_press, contrast,
    };

    #[test]
    fn action_tones_keep_filled_ink_readable_in_both_themes() {
        for style in [
            Style::new(MOCHA, Theme::Dark),
            Style::new(LATTE, Theme::Light),
        ] {
            let primary = tone_shades(ButtonTone::Primary, style);
            let neutral = tone_shades(ButtonTone::Neutral, style);
            let danger = tone_shades(ButtonTone::Danger, style);
            assert_eq!(primary.fill, style.colors.mauve);
            assert_eq!(neutral.fill, style.colors.surface0);
            assert_eq!(neutral.border, style.colors.surface1);
            assert_eq!(danger.fill, style.colors.red);
            assert_eq!(primary.ink, style.ink());
            assert_eq!(danger.ink, style.ink());
            assert!(contrast(primary.ink, primary.fill) >= 4.5);
            assert!(contrast(primary.ink, primary.hover) >= 4.5);
            assert!(contrast(primary.ink, primary.press) >= 4.5);
            assert!(contrast(danger.ink, danger.fill) >= 4.5);
            assert!(contrast(danger.ink, danger.hover) >= 4.5);
            assert!(contrast(danger.ink, danger.press) >= 4.5);
            assert!(contrast(neutral.ink, neutral.fill) >= 4.5);
            assert!(contrast(neutral.ink, neutral.hover) >= 4.5);
            assert!(contrast(neutral.ink, neutral.press) >= 4.5);
        }
        assert_ne!(
            Style::new(MOCHA, Theme::Dark).ink(),
            Style::new(LATTE, Theme::Light).ink()
        );
    }

    #[test]
    fn interaction_states_stay_distinct_without_reserving_extra_space() {
        let surface = Interaction::surface(MOCHA);
        assert_eq!(surface.border.a, 0.0);
        assert_eq!(surface.focus, MOCHA.mauve);
        assert_ne!(surface.hover, surface.press);

        let raised = Interaction::raised(MOCHA);
        assert_eq!(raised.border.a, 0.0);
        assert_ne!(raised.hover, raised.press);

        let accent = Interaction::accent(Style::new(MOCHA, Theme::Dark));
        assert_eq!(accent.hover, accent_hover(Theme::Dark));
        assert_eq!(accent.press, accent_press(Theme::Dark));
        assert_ne!(accent.hover, accent.press);

        let danger = Interaction::danger(MOCHA);
        assert_eq!(danger.focus, MOCHA.red);
        assert!(danger.press.a > danger.hover.a);

        let tinted = Interaction::tinted(MOCHA, MOCHA.teal);
        assert_eq!(tinted.focus, MOCHA.teal);
        assert!(tinted.press.a > tinted.hover.a);
    }
}
