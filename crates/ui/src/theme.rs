//! Catppuccin palette, layout tokens, and semantic colors for the GPUI UI.

use gpui::Rgba;
use ranki_model::ThemeChoice;

/// Dark Catppuccin Mocha, the palette used by the mockups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// Resolves the configured choice against the current window appearance.
#[must_use]
pub const fn resolve_theme(system_dark: bool, choice: ThemeChoice) -> Theme {
    match choice {
        ThemeChoice::Dark => Theme::Dark,
        ThemeChoice::Light => Theme::Light,
        ThemeChoice::System => {
            if system_dark {
                Theme::Dark
            } else {
                Theme::Light
            }
        }
    }
}

/// A flat Catppuccin role set. Every view reads colors from here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub base: Rgba,
    pub mantle: Rgba,
    pub crust: Rgba,
    pub surface0: Rgba,
    pub surface1: Rgba,
    pub text: Rgba,
    pub subtext: Rgba,
    pub overlay: Rgba,
    pub mauve: Rgba,
    pub red: Rgba,
    pub peach: Rgba,
    pub green: Rgba,
    pub blue: Rgba,
    pub teal: Rgba,
}

const fn c(hex: u32) -> Rgba {
    let [_, r, g, b] = hex.to_be_bytes();
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

pub const MOCHA: Palette = Palette {
    base: c(0x1e1e2e),
    mantle: c(0x181825),
    crust: c(0x11111b),
    surface0: c(0x313244),
    surface1: c(0x45475a),
    text: c(0xcdd6f4),
    subtext: c(0xa6adc8),
    overlay: c(0x7f849c),
    mauve: c(0xcba6f7),
    red: c(0xf38ba8),
    peach: c(0xfab387),
    green: c(0xa6e3a1),
    blue: c(0x89b4fa),
    teal: c(0x94e2d5),
};

pub const LATTE: Palette = Palette {
    base: c(0xeff1f5),
    mantle: c(0xe6e9ef),
    crust: c(0xdce0e8),
    surface0: c(0xccd0da),
    surface1: c(0xbcc0cc),
    text: c(0x4c4f69),
    subtext: c(0x6c6f85),
    overlay: c(0x8c8fa1),
    mauve: c(0x8839ef),
    red: c(0xd20f39),
    peach: c(0xfe640b),
    green: c(0x40a02b),
    blue: c(0x1e66f5),
    teal: c(0x179299),
};

#[must_use]
pub const fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark => MOCHA,
        Theme::Light => LATTE,
    }
}

/// A preset offered by the card color picker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorSwatch {
    pub name: &'static str,
    /// CSS value written into the card markup. `inherit` clears an override.
    pub hex: &'static str,
    pub color: Option<Rgba>,
}

const MOCHA_SWATCHES: [ColorSwatch; 9] = [
    ColorSwatch {
        name: "Default",
        hex: "inherit",
        color: None,
    },
    ColorSwatch {
        name: "Text",
        hex: "#cdd6f4",
        color: Some(MOCHA.text),
    },
    ColorSwatch {
        name: "Mauve",
        hex: "#cba6f7",
        color: Some(MOCHA.mauve),
    },
    ColorSwatch {
        name: "Red",
        hex: "#f38ba8",
        color: Some(MOCHA.red),
    },
    ColorSwatch {
        name: "Peach",
        hex: "#fab387",
        color: Some(MOCHA.peach),
    },
    ColorSwatch {
        name: "Yellow",
        hex: "#f9e2af",
        color: Some(c(0xf9e2af)),
    },
    ColorSwatch {
        name: "Green",
        hex: "#a6e3a1",
        color: Some(MOCHA.green),
    },
    ColorSwatch {
        name: "Teal",
        hex: "#94e2d5",
        color: Some(MOCHA.teal),
    },
    ColorSwatch {
        name: "Blue",
        hex: "#89b4fa",
        color: Some(MOCHA.blue),
    },
];

const LATTE_SWATCHES: [ColorSwatch; 9] = [
    ColorSwatch {
        name: "Default",
        hex: "inherit",
        color: None,
    },
    ColorSwatch {
        name: "Text",
        hex: "#4c4f69",
        color: Some(LATTE.text),
    },
    ColorSwatch {
        name: "Mauve",
        hex: "#8839ef",
        color: Some(LATTE.mauve),
    },
    ColorSwatch {
        name: "Red",
        hex: "#d20f39",
        color: Some(LATTE.red),
    },
    ColorSwatch {
        name: "Peach",
        hex: "#fe640b",
        color: Some(LATTE.peach),
    },
    ColorSwatch {
        name: "Yellow",
        hex: "#e5c890",
        color: Some(c(0xe5c890)),
    },
    ColorSwatch {
        name: "Green",
        hex: "#40a02b",
        color: Some(LATTE.green),
    },
    ColorSwatch {
        name: "Teal",
        hex: "#179299",
        color: Some(LATTE.teal),
    },
    ColorSwatch {
        name: "Blue",
        hex: "#1e66f5",
        color: Some(LATTE.blue),
    },
];

#[must_use]
pub const fn card_colors(theme: Theme) -> &'static [ColorSwatch] {
    match theme {
        Theme::Dark => &MOCHA_SWATCHES,
        Theme::Light => &LATTE_SWATCHES,
    }
}

/// Parses `#rgb`, `rgb`, `#rrggbb`, and `rrggbb` into an opaque GPUI color.
#[must_use]
pub fn parse_hex_color(input: &str) -> Option<Rgba> {
    let input = input.trim();
    let raw = input.strip_prefix('#').unwrap_or(input);
    let value = match raw.len() {
        3 => {
            let r = raw.get(0..1)?;
            let g = raw.get(1..2)?;
            let b = raw.get(2..3)?;
            u32::from_str_radix(&format!("{r}{r}{g}{g}{b}{b}"), 16).ok()?
        }
        6 => u32::from_str_radix(raw, 16).ok()?,
        _ => return None,
    };
    let [_, r, g, b] = value.to_be_bytes();
    Some(Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    })
}

/// Applies alpha to a role color without introducing a new hue.
#[must_use]
pub const fn alpha(color: Rgba, amount: f32) -> Rgba {
    let mut result = color;
    result.a = amount;
    result
}

#[must_use]
pub const fn accent_ink(theme: Theme) -> Rgba {
    match theme {
        Theme::Dark => MOCHA.crust,
        Theme::Light => c(0xffffff),
    }
}

/// Motion durations. Every animated surface reads one of these through
/// `Root::motion_ms`, so the reduce-motion setting and desktop preference
/// switch the whole app at once. None blocks input: gpui advances animations
/// on the frame clock, not on the action.
pub const MOTION_QUICK_MS: u64 = 120;
pub const MOTION_PROGRESS_MS: u64 = 180;
/// Full-page swap, such as Decks to Browse.
pub const MOTION_PAGE_MS: u64 = 160;
/// Menus, popovers, dialogs, banners, and toasts.
pub const MOTION_POPUP_MS: u64 = 120;
/// How far a surface travels vertically while it fades in, in pixels.
pub const ENTER_RISE_PX: f32 = 8.0;

/// Cubic ease-out: quick start, soft landing. Entrances all share this curve
/// so surfaces settle with the same weight.
#[must_use]
pub fn ease_out_cubic(delta: f32) -> f32 {
    let inverse = 1.0 - delta.clamp(0.0, 1.0);
    1.0 - inverse * inverse * inverse
}

/// Mixes two roles. Used only to derive hover and pressed shades of an
/// existing role, never to introduce a new hue.
#[must_use]
pub fn blend(from: Rgba, to: Rgba, amount: f32) -> Rgba {
    let amount = amount.clamp(0.0, 1.0);
    let mix = |start: f32, end: f32| start + (end - start) * amount;
    Rgba {
        r: mix(from.r, to.r),
        g: mix(from.g, to.g),
        b: mix(from.b, to.b),
        a: from.a + (to.a - from.a) * amount,
    }
}

/// Hover shade for an accent fill: mauve lifted toward white in Mocha and
/// pulled toward the text in Latte, so the label keeps its contrast either way.
#[must_use]
pub fn accent_hover(theme: Theme) -> Rgba {
    let colors = palette(theme);
    match theme {
        Theme::Dark => blend(colors.mauve, c(0xffffff), 0.12),
        Theme::Light => blend(colors.mauve, colors.text, 0.12),
    }
}

/// Pressed shade for an accent fill: mauve pulled toward crust.
#[must_use]
pub fn accent_press(theme: Theme) -> Rgba {
    let colors = palette(theme);
    blend(colors.mauve, colors.crust, 0.14)
}

/// Hover shade for a red fill: red lifted toward white.
#[must_use]
pub fn danger_hover(theme: Theme) -> Rgba {
    blend(palette(theme).red, c(0xffffff), 0.12)
}

/// Pressed shade for a red fill: red pulled toward crust.
#[must_use]
pub fn danger_press(theme: Theme) -> Rgba {
    blend(palette(theme).red, palette(theme).crust, 0.14)
}

/// Keycap ink on a `surface1` notch. Mockup `subtext` reaches 4.2:1 at 11px,
/// below WCAG AA. `text` clears it in Mocha, and Latte needs a 15% darker
/// shade of the same role to clear 4.5:1 on the light notch.
#[must_use]
pub fn keycap_ink(colors: Palette) -> Rgba {
    if colors.base.r > 0.5 {
        blend(
            colors.text,
            Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            0.15,
        )
    } else {
        colors.text
    }
}

/// True when the user asked for reduced motion. The explicit setting wins over
/// the desktop preference, which wins over the default of full motion.
#[must_use]
pub fn reduced_motion(setting: Option<bool>) -> bool {
    setting.unwrap_or_else(system_reduced_motion)
}

/// Reads the desktop animation preference once. gpui 0.2.2 has no
/// reduced-motion API and the portal exposes no motion key, so the GTK and KDE
/// conventions are the only sources available.
fn system_reduced_motion() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        if let Some(value) = std::env::var("RANKI_REDUCE_MOTION")
            .ok()
            .and_then(|raw| parse_flag_bool(&raw))
        {
            return value;
        }
        dconf_animations_disabled()
            || gtk_settings_animations_disabled()
            || std::fs::read_to_string(kdeglobals_path())
                .is_ok_and(|text| kde_animations_disabled(&text))
    })
}

/// Parses the boolean spellings used by GTK settings, environment variables,
/// and `dconf`. Unknown text is `None`, so callers keep their fallback.
#[must_use]
pub fn parse_flag_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// KDE exposes reduced motion as a zero animation duration factor.
#[must_use]
pub fn kde_animations_disabled(contents: &str) -> bool {
    contents.lines().any(|line| {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        key.trim() == "AnimationDurationFactor"
            && value
                .trim()
                .parse::<f32>()
                .is_ok_and(|factor| factor <= 0.0)
    })
}

fn dconf_animations_disabled() -> bool {
    let Ok(output) = std::process::Command::new("dconf")
        .args(["read", "/org/gnome/desktop/interface/enable-animations"])
        .output()
    else {
        return false;
    };
    output.status.success()
        && parse_flag_bool(&String::from_utf8_lossy(&output.stdout))
            == Some(false)
}

/// GTK falls back to `settings.ini` when no settings portal or dconf value is
/// available, so read both toolkit generations.
fn gtk_settings_animations_disabled() -> bool {
    ["gtk-4.0", "gtk-3.0"].iter().any(|version| {
        std::fs::read_to_string(config_home("gtk", version))
            .is_ok_and(|text| gtk_animations_disabled(&text))
    })
}

fn config_home(toolkit: &str, version: &str) -> std::path::PathBuf {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    xdg_config_dir(xdg.as_deref(), home.as_deref())
        .join(toolkit)
        .join(version)
        .join("settings.ini")
}

/// `XDG_CONFIG_HOME` wins when it is a non-empty path; otherwise the
/// specification puts user configuration under `$HOME/.config`.
#[must_use]
pub fn xdg_config_dir(
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> std::path::PathBuf {
    let explicit = xdg_config_home
        .map(str::trim)
        .filter(|value| !value.is_empty());
    explicit.map_or_else(
        || {
            home.map_or_else(std::path::PathBuf::new, |home| {
                std::path::Path::new(home).join(".config")
            })
        },
        std::path::PathBuf::from,
    )
}

/// Reads the `gtk-enable-animations` key from a GTK `settings.ini` body.
#[must_use]
pub fn gtk_animations_disabled(contents: &str) -> bool {
    let mut in_settings = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_settings = line.eq_ignore_ascii_case("[Settings]");
            continue;
        }
        if !in_settings {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("gtk-enable-animations") {
            return parse_flag_bool(value) == Some(false);
        }
    }
    false
}

fn kdeglobals_path() -> std::path::PathBuf {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    xdg_config_dir(xdg.as_deref(), home.as_deref()).join("kdeglobals")
}
/// Palette plus resolved theme, passed by value to shared element helpers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub colors: Palette,
    pub theme: Theme,
}

impl Style {
    #[must_use]
    pub const fn new(colors: Palette, theme: Theme) -> Self {
        Self { colors, theme }
    }

    /// Ink used on mauve fills.
    #[must_use]
    pub const fn ink(self) -> Rgba {
        accent_ink(self.theme)
    }
}

/// Field placeholder ink. Latte `subtext` reaches only 3.2:1 on a `surface0`
/// input, so light placeholders use a darker ink that clears 4.5:1 on both
/// input roles.
#[must_use]
pub const fn placeholder_color(theme: Theme) -> Rgba {
    match theme {
        Theme::Dark => MOCHA.subtext,
        Theme::Light => c(0x545869),
    }
}

/// Rating hue: Again red, Hard peach, Good green, Easy blue.
#[must_use]
pub const fn rating_fill(code: u8, theme: Theme) -> Rgba {
    let colors = palette(theme);
    match code {
        1 => colors.red,
        2 => colors.peach,
        3 => colors.green,
        4 => colors.blue,
        _ => colors.mauve,
    }
}

/// Solid label color. Latte keeps labels readable instead of tinting them.
#[must_use]
pub const fn rating_label(code: u8, theme: Theme) -> Rgba {
    match theme {
        Theme::Dark => rating_fill(code, theme),
        Theme::Light => LATTE.text,
    }
}

#[must_use]
pub const fn queue_color(code: u8, theme: Theme) -> Rgba {
    let colors = palette(theme);
    match code {
        0 => colors.blue,
        1 => colors.red,
        2 => colors.green,
        _ => colors.mauve,
    }
}

/// Text-safe queue color. Latte green fails 4.5:1 on raised surfaces.
#[must_use]
pub const fn queue_text(code: u8, theme: Theme) -> Rgba {
    match (theme, code) {
        (Theme::Light, 2) => LATTE.text,
        _ => queue_color(code, theme),
    }
}

pub const FONT_FAMILY: &str = "Liberation Sans";
pub const CJK_FAMILY: &str = "Noto Sans CJK JP";
/// Last-resort family that still covers Latin, kana, and Nerd icons.
pub const FONT_FALLBACK: &str = "CCL Nerd Font";

/// Picks the first installed family from the mockup and Ranki fallbacks.
#[must_use]
pub fn detected_font_family() -> &'static str {
    font_candidates(None)
        .into_iter()
        .find(|family| family_installed(family))
        .unwrap_or(FONT_FALLBACK)
}

/// Resolved UI family, cached after the first fontconfig probe.
pub fn ui_font_family() -> &'static str {
    static UI_FONT: std::sync::OnceLock<&'static str> =
        std::sync::OnceLock::new();
    UI_FONT.get_or_init(detected_font_family)
}

/// Japanese family for glyph runs that need a real CJK face.
pub fn cjk_font_family() -> &'static str {
    static CJK_FONT: std::sync::OnceLock<&'static str> =
        std::sync::OnceLock::new();
    CJK_FONT.get_or_init(|| {
        [CJK_FAMILY, "Noto Sans JP"]
            .into_iter()
            .find(|family| family_installed(family))
            .unwrap_or_else(ui_font_family)
    })
}

/// Monospace family for code blocks and inline code.
#[must_use]
pub fn mono_font_family() -> &'static str {
    static MONO_FONT: std::sync::OnceLock<&'static str> =
        std::sync::OnceLock::new();
    MONO_FONT.get_or_init(|| {
        ["JetBrains Mono", "DejaVu Sans Mono", "Liberation Mono"]
            .into_iter()
            .find(|family| family_installed(family))
            .unwrap_or_else(ui_font_family)
    })
}

/// True when the text needs the CJK face. Unifont is the wrong fallback.
#[must_use]
pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(
            u32::from(character),
            0x3040..=0x30ff
                | 0x3400..=0x4dbf
                | 0x4e00..=0x9fff
                | 0xf900..=0xfaff
                | 0xff66..=0xff9f
        )
    })
}

/// Family for one text block. Mixed strings favor the CJK face.
#[must_use]
pub fn font_for_text(text: &str) -> &'static str {
    if contains_cjk(text) {
        cjk_font_family()
    } else {
        ui_font_family()
    }
}

fn family_installed(family: &str) -> bool {
    let Ok(output) = std::process::Command::new("fc-match")
        .args(["-f", "%{family}", family])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .split(',')
        .any(|matched| matched.trim().eq_ignore_ascii_case(family))
}

/// Font candidates: clean sans and real CJK first, CCL Nerd Font last.
#[must_use]
pub fn font_candidates(matched: Option<&str>) -> Vec<&str> {
    let mut candidates =
        vec![FONT_FAMILY, "Inter", "Noto Sans JP", "DejaVu Sans"];
    if let Some(name) = matched {
        let first = name.split(',').next().unwrap_or(name).trim();
        if !first.is_empty()
            && !candidates
                .iter()
                .any(|family| family.eq_ignore_ascii_case(first))
        {
            candidates.push(first);
        }
    }
    candidates.push(FONT_FALLBACK);
    candidates
}

/// Spacing scale. The distance maps to the relation between the elements:
/// inline parts of one control, siblings in a group, groups in a block,
/// blocks in a panel, panels in a page, and page edges. Every step is a
/// 4 px multiple.
pub const SPACE_INLINE: f32 = 4.0;
pub const SPACE_GROUP: f32 = 8.0;
pub const SPACE_RELATED: f32 = 12.0;
pub const SPACE_BLOCK: f32 = 16.0;
pub const SPACE_SECTION: f32 = 24.0;
pub const SPACE_GUTTER: f32 = 32.0;

/// Shared geometry for action buttons. Height, radius, and padding stay on the
/// four-pixel grid; only color and label distinguish primary from secondary.
pub const BUTTON_HEIGHT: f32 = 44.0;
pub const BUTTON_RADIUS: f32 = 12.0;
pub const BUTTON_PADDING_X: f32 = SPACE_BLOCK;
pub const BUTTON_TEXT_SIZE: f32 = 14.0;

/// Sidebar width. Dragging the rail's divider picks any width between the
/// icon-only minimum and the maximum, which is about a fifth wider.
pub const NAV_WIDTH: f32 = 232.0;
pub const NAV_WIDTH_MIN: f32 = 72.0;
pub const NAV_WIDTH_MAX: f32 = 280.0;
/// Rails narrower than this drop labels, keycaps, and the wordmark.
pub const NAV_COMPACT_WIDTH: f32 = 180.0;
/// Windows narrower than this always show the icon-only rail.
pub const NAV_AUTO_COMPACT_WIDTH: f32 = 840.0;
/// Below this width the rail becomes a bottom bar with no side gutter.
pub const MOBILE_WIDTH: f32 = 640.0;
/// Height of the bottom navigation bar.
pub const MOBILE_NAV_HEIGHT: f32 = 56.0;
/// Smallest tappable side of a mobile navigation item.
pub const MOBILE_TOUCH_TARGET: f32 = 44.0;
/// Grab strip on the rail's right edge.
pub const NAV_DIVIDER_WIDTH: f32 = 4.0;
pub const TOPBAR_HEIGHT: f32 = 72.0;
pub const FOOTER_HEIGHT: f32 = 64.0;
pub const STAGE_WIDTH: f32 = 1064.0;
pub const STAGE_HEIGHT: f32 = 528.0;
pub const STAGE_RADIUS: f32 = 32.0;
pub const STAGE_PADDING: f32 = SPACE_GUTTER;
pub const CONTROL_HEIGHT: f32 = 76.0;
pub const RATING_GAP: f32 = SPACE_RELATED;
pub const PROGRESS_WIDTH: f32 = 420.0;
pub const PROGRESS_HEIGHT: f32 = 6.0;
pub const PAGE_X: f32 = SPACE_GUTTER;
pub const PAGE_Y: f32 = SPACE_GUTTER;
/// Fixed width of one queue-count column in the deck table.
pub const COUNT_COLUMN_WIDTH: f32 = 72.0;
/// Reserved width of the deck table action column, so rows never shift.
pub const ACTION_COLUMN_WIDTH: f32 = 120.0;
/// Deck table widths: deck name flexes, three counts, then the action cell.
pub const TABLE_COLUMNS: [f32; 5] = [
    1.0,
    COUNT_COLUMN_WIDTH,
    COUNT_COLUMN_WIDTH,
    COUNT_COLUMN_WIDTH,
    ACTION_COLUMN_WIDTH,
];
/// Form column cap for the add-card screen.
pub const ADD_FORM_WIDTH: f32 = 720.0;
/// Preview rail width on the add-card screen.
pub const PREVIEW_WIDTH: f32 = 380.0;
/// Gap between the add-card form and its preview rail.
pub const ADD_GAP: f32 = SPACE_GUTTER;
/// Full add-card content width: form plus gap plus preview rail.
pub const ADD_CONTENT_WIDTH: f32 = ADD_FORM_WIDTH + ADD_GAP + PREVIEW_WIDTH;

/// Clamps a dragged sidebar width to the rail's allowed range.
#[must_use]
pub fn nav_width(width: f32) -> f32 {
    width.clamp(NAV_WIDTH_MIN, NAV_WIDTH_MAX)
}

/// True when a window is narrow enough to use the bottom navigation bar.
#[must_use]
pub const fn is_mobile_width(viewport_width: f32) -> bool {
    viewport_width < MOBILE_WIDTH
}

/// Width the side rail gets in this window. Phone widths use a bottom bar
/// instead, so the page keeps its full width; the saved width stays intact.
#[must_use]
pub fn visible_nav_width(viewport_width: f32, nav_width: f32) -> f32 {
    if is_mobile_width(viewport_width) {
        0.0
    } else if viewport_width < NAV_AUTO_COMPACT_WIDTH {
        NAV_WIDTH_MIN
    } else {
        nav_width
    }
}

/// Width left for a page after the rail and its page padding.
#[must_use]
pub fn page_width(viewport_width: f32, nav_width: f32) -> f32 {
    (viewport_width
        - visible_nav_width(viewport_width, nav_width)
        - 2.0 * PAGE_X)
        .max(0.0)
}

/// Fits the card into the window while keeping the desktop width as a cap.
#[must_use]
pub fn stage_width(viewport_width: f32) -> f32 {
    (viewport_width - 2.0 * PAGE_X).clamp(0.0, STAGE_WIDTH)
}

/// Converts an interval in days or negative seconds into an Anki-style label.
#[must_use]
pub fn interval_label(interval: i32) -> String {
    if interval < 0 {
        let seconds = interval.unsigned_abs();
        if seconds < 60 {
            format!("{seconds}s")
        } else {
            format!("{}m", seconds / 60)
        }
    } else if interval < 30 {
        format!("{interval}d")
    } else if interval < 365 {
        format!("{}mo", interval / 30)
    } else {
        format!("{}y", interval / 365)
    }
}

/// WCAG 2.1 relative luminance ratio for two opaque roles.
#[cfg(test)]
pub fn contrast(a: gpui::Rgba, b: gpui::Rgba) -> f32 {
    let lum = |color: gpui::Rgba| {
        let channel = |value: f32| {
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.212_6 * channel(color.r)
            + 0.715_2 * channel(color.g)
            + 0.072_2 * channel(color.b)
    };
    let (high, low) = {
        let (a, b) = (lum(a), lum(b));
        if a >= b { (a, b) } else { (b, a) }
    };
    (high + 0.05) / (low + 0.05)
}

#[cfg(test)]
mod tests {
    use gpui::rgb;

    use super::{
        ADD_GAP, BUTTON_HEIGHT, BUTTON_PADDING_X, BUTTON_RADIUS,
        BUTTON_TEXT_SIZE, LATTE, MOBILE_NAV_HEIGHT, MOBILE_TOUCH_TARGET,
        MOBILE_WIDTH, MOCHA, MOTION_PAGE_MS, MOTION_POPUP_MS,
        MOTION_PROGRESS_MS, MOTION_QUICK_MS, NAV_AUTO_COMPACT_WIDTH,
        NAV_COMPACT_WIDTH, NAV_WIDTH, NAV_WIDTH_MAX, NAV_WIDTH_MIN, PAGE_X,
        PAGE_Y, PROGRESS_HEIGHT, PROGRESS_WIDTH, RATING_GAP, SPACE_BLOCK,
        SPACE_GROUP, SPACE_GUTTER, SPACE_INLINE, SPACE_RELATED, SPACE_SECTION,
        STAGE_HEIGHT, STAGE_PADDING, STAGE_RADIUS, STAGE_WIDTH, Theme,
        accent_hover, accent_press, alpha, blend, card_colors, contrast,
        ease_out_cubic, font_candidates, gtk_animations_disabled,
        interval_label, is_mobile_width, kde_animations_disabled, keycap_ink,
        nav_width, page_width, palette, parse_flag_bool, parse_hex_color,
        placeholder_color, rating_fill, rating_label, reduced_motion,
        resolve_theme, stage_width, visible_nav_width, xdg_config_dir,
    };
    use crate::ThemeChoice;

    #[test]
    fn mocha_matches_the_mockup_exactly() {
        assert_eq!(MOCHA.base, rgb(0x1e1e2e));
        assert_eq!(MOCHA.surface0, rgb(0x313244));
        assert_eq!(MOCHA.mauve, rgb(0xcba6f7));
        assert_eq!(palette(Theme::Dark), MOCHA);
    }

    #[test]
    fn latte_is_the_light_mapping() {
        assert_eq!(LATTE.base, rgb(0xeff1f5));
        assert_eq!(LATTE.mauve, rgb(0x8839ef));
        assert_eq!(palette(Theme::Light), LATTE);
    }

    #[test]
    fn explicit_theme_beats_system_appearance() {
        assert_eq!(resolve_theme(false, ThemeChoice::Dark), Theme::Dark);
        assert_eq!(resolve_theme(true, ThemeChoice::Light), Theme::Light);
        assert_eq!(resolve_theme(true, ThemeChoice::System), Theme::Dark);
        assert_eq!(resolve_theme(false, ThemeChoice::System), Theme::Light);
    }

    #[test]
    fn rating_colors_match_the_learning_mockup() {
        assert_eq!(rating_fill(1, Theme::Dark), MOCHA.red);
        assert_eq!(rating_fill(2, Theme::Dark), MOCHA.peach);
        assert_eq!(rating_fill(3, Theme::Light), LATTE.green);
        assert_eq!(rating_fill(4, Theme::Light), LATTE.blue);
    }

    #[test]
    fn latte_rating_labels_use_text_contrast() {
        for code in 1..=4 {
            assert_eq!(rating_label(code, Theme::Light), LATTE.text);
            assert_eq!(
                rating_label(code, Theme::Dark),
                rating_fill(code, Theme::Dark)
            );
        }
    }

    #[test]
    fn alpha_only_changes_opacity() {
        let faded = alpha(MOCHA.mauve, 0.16);
        assert_eq!(faded.r, MOCHA.mauve.r);
        assert_eq!(faded.a, 0.16);
    }

    #[test]
    fn fonts_put_clean_sans_first_and_keep_ccl_fallback() {
        let candidates = font_candidates(Some("Inter, sans-serif"));
        assert_eq!(candidates[0], "Liberation Sans");
        assert_eq!(candidates.last(), Some(&"CCL Nerd Font"));
        assert!(candidates.contains(&"Noto Sans JP"));
        assert_eq!(font_candidates(None).last(), Some(&"CCL Nerd Font"));
    }

    #[test]
    fn layout_constants_match_the_artboard() {
        assert_eq!(STAGE_WIDTH, 1064.0);
        assert_eq!(STAGE_HEIGHT, 528.0);
        assert_eq!(STAGE_RADIUS, 32.0);
        assert_eq!(PROGRESS_WIDTH, 420.0);
        assert_eq!(PROGRESS_HEIGHT, 6.0);
    }

    #[test]
    fn stage_width_fits_the_window_without_growing_past_desktop() {
        assert_eq!(stage_width(1280.0), STAGE_WIDTH);
        assert_eq!(stage_width(900.0), 900.0 - 2.0 * PAGE_X);
        assert_eq!(stage_width(500.0), 500.0 - 2.0 * PAGE_X);
        assert_eq!(stage_width(64.0), 0.0);
    }

    #[test]
    fn small_windows_use_the_icon_rail() {
        assert_eq!(visible_nav_width(640.0, NAV_WIDTH), NAV_WIDTH_MIN);
        assert_eq!(
            visible_nav_width(NAV_AUTO_COMPACT_WIDTH, NAV_WIDTH),
            NAV_WIDTH
        );
        assert_eq!(page_width(640.0, NAV_WIDTH), 640.0 - NAV_WIDTH_MIN - 64.0);
        assert_eq!(page_width(64.0, NAV_WIDTH), 0.0);
    }

    #[test]
    fn phone_widths_drop_the_rail_and_keep_the_page_wide() {
        assert!(is_mobile_width(390.0));
        assert!(is_mobile_width(MOBILE_WIDTH - 1.0));
        assert!(!is_mobile_width(MOBILE_WIDTH));

        assert_eq!(visible_nav_width(390.0, NAV_WIDTH), 0.0);
        assert_eq!(page_width(390.0, NAV_WIDTH), 390.0 - 2.0 * PAGE_X);
        assert_eq!(
            page_width(640.0, NAV_WIDTH),
            640.0 - NAV_WIDTH_MIN - 2.0 * PAGE_X
        );
    }

    #[test]
    fn bottom_bar_fits_five_finger_sized_tabs() {
        const {
            assert!(MOBILE_NAV_HEIGHT >= MOBILE_TOUCH_TARGET);
            assert!(MOBILE_TOUCH_TARGET >= 44.0);
            assert!(320.0 >= 5.0 * MOBILE_TOUCH_TARGET);
        }
    }

    #[test]
    fn nav_width_stays_between_icons_only_and_a_fifth_wider() {
        assert_eq!(nav_width(0.0), NAV_WIDTH_MIN);
        assert_eq!(nav_width(f32::INFINITY), NAV_WIDTH_MAX);
        assert_eq!(nav_width(NAV_WIDTH), NAV_WIDTH);
        const {
            assert!(NAV_WIDTH <= NAV_WIDTH_MAX / 1.2);
            assert!(NAV_WIDTH_MIN < NAV_COMPACT_WIDTH);
            assert!(NAV_COMPACT_WIDTH < NAV_WIDTH);
        }
        assert_eq!(NAV_WIDTH_MIN % 4.0, 0.0);
        assert_eq!(NAV_WIDTH_MAX % 4.0, 0.0);
    }

    #[test]
    fn spacing_steps_express_relations_on_a_four_pixel_grid() {
        let steps = [
            SPACE_INLINE,
            SPACE_GROUP,
            SPACE_RELATED,
            SPACE_BLOCK,
            SPACE_SECTION,
            SPACE_GUTTER,
        ];
        for step in steps {
            assert_eq!(step % 4.0, 0.0);
        }
        assert!(
            steps
                .iter()
                .zip(steps.iter().skip(1))
                .all(|(near, far)| near < far)
        );
        for spacing in [PAGE_X, PAGE_Y, STAGE_PADDING, RATING_GAP, ADD_GAP] {
            assert!(steps.contains(&spacing), "{spacing} is off the scale");
        }
    }

    #[test]
    fn button_geometry_is_shared_and_on_the_four_pixel_grid() {
        assert_eq!(BUTTON_HEIGHT, 44.0);
        assert_eq!(BUTTON_RADIUS, 12.0);
        assert_eq!(BUTTON_PADDING_X, SPACE_BLOCK);
        assert_eq!(BUTTON_TEXT_SIZE, 14.0);
        for value in [BUTTON_HEIGHT, BUTTON_RADIUS, BUTTON_PADDING_X] {
            assert_eq!(value % 4.0, 0.0);
        }
    }

    #[test]
    fn interval_labels_cover_seconds_days_and_years() {
        assert_eq!(interval_label(-10), "10s");
        assert_eq!(interval_label(-600), "10m");
        assert_eq!(interval_label(6), "6d");
        assert_eq!(interval_label(60), "2mo");
        assert_eq!(interval_label(365), "1y");
    }
    #[test]
    fn placeholders_clear_4_5_to_1_on_both_input_roles() {
        for theme in [Theme::Dark, Theme::Light] {
            let ink = placeholder_color(theme);
            let input = palette(theme).base;
            let raised = palette(theme).surface0;
            assert!(contrast(ink, input) >= 4.5, "{theme:?} on base");
            assert!(contrast(ink, raised) >= 4.5, "{theme:?} on surface0");
        }
    }
    #[test]
    fn keycaps_clear_4_5_to_1_on_the_notch_and_the_sidebar() {
        for theme in [Theme::Dark, Theme::Light] {
            let colors = palette(theme);
            let ink = keycap_ink(colors);
            assert!(contrast(ink, colors.surface1) >= 4.5, "{theme:?} notch");
            assert!(contrast(ink, colors.crust) >= 4.5, "{theme:?} sidebar");
        }
    }

    #[test]
    fn blend_keeps_the_hue_between_its_inputs() {
        let hover = accent_hover(Theme::Dark);
        let press = accent_press(Theme::Dark);
        assert!(hover.r > MOCHA.mauve.r && hover.b >= MOCHA.mauve.b);
        assert!(press.r < MOCHA.mauve.r && press.b < MOCHA.mauve.b);
        assert_eq!(hover.a, 1.0);
        let close = |left: gpui::Rgba, right: gpui::Rgba| {
            (left.r - right.r).abs() < 0.01
                && (left.g - right.g).abs() < 0.01
                && (left.b - right.b).abs() < 0.01
        };
        assert!(close(blend(MOCHA.mauve, MOCHA.crust, 0.0), MOCHA.mauve));
        assert!(close(blend(MOCHA.mauve, MOCHA.crust, 1.0), MOCHA.crust));
    }

    #[test]
    fn motion_durations_stay_short() {
        const { assert!(MOTION_QUICK_MS <= 150) };
        const { assert!(MOTION_PROGRESS_MS <= 200) };
        const { assert!(MOTION_PAGE_MS <= 200) };
        const { assert!(MOTION_POPUP_MS <= 150) };
        assert!(reduced_motion(Some(true)));
        assert!(!reduced_motion(Some(false)));
    }

    #[test]
    fn ease_out_cubic_settles_at_the_ends() {
        assert!(ease_out_cubic(-1.0).abs() < 0.0001);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < 0.0001);
        assert!((ease_out_cubic(2.0) - 1.0).abs() < 0.0001);
        let mut last = -1.0;
        for step in 0..=10u8 {
            let value = ease_out_cubic(f32::from(step) / 10.0);
            assert!(value >= last, "easing must not go backwards");
            last = value;
        }
    }

    #[test]
    fn flag_booleans_cover_the_desktop_spellings() {
        assert_eq!(parse_flag_bool("true"), Some(true));
        assert_eq!(parse_flag_bool(" FALSE\n"), Some(false));
        assert_eq!(parse_flag_bool("1"), Some(true));
        assert_eq!(parse_flag_bool("off"), Some(false));
        assert_eq!(parse_flag_bool("maybe"), None);
    }

    #[test]
    fn config_home_follows_the_xdg_specification() {
        assert_eq!(
            xdg_config_dir(Some("/tmp/xdg"), Some("/home/u")),
            std::path::PathBuf::from("/tmp/xdg")
        );
        assert_eq!(
            xdg_config_dir(Some("  "), Some("/home/u")),
            std::path::PathBuf::from("/home/u/.config")
        );
        assert_eq!(
            xdg_config_dir(None, Some("/home/u")),
            std::path::PathBuf::from("/home/u/.config")
        );
        assert_eq!(xdg_config_dir(None, None), std::path::PathBuf::new());
    }

    #[test]
    fn gtk_settings_ini_can_disable_animation() {
        assert!(gtk_animations_disabled(
            "[Settings]\ngtk-enable-animations=false\n"
        ));
        assert!(gtk_animations_disabled(
            "[settings]\ngtk-enable-animations = 0\n"
        ));
        assert!(!gtk_animations_disabled(
            "[Settings]\ngtk-enable-animations=true\n"
        ));
        assert!(!gtk_animations_disabled(
            "[Other]\ngtk-enable-animations=false\n"
        ));
        assert!(!gtk_animations_disabled("gtk-enable-animations=false"));
    }

    #[test]
    fn kde_zero_animation_factor_disables_motion() {
        assert!(kde_animations_disabled(
            "[KDE]\nAnimationDurationFactor=0\n"
        ));
        assert!(kde_animations_disabled("AnimationDurationFactor=0.0\n"));
        assert!(!kde_animations_disabled("AnimationDurationFactor=1\n"));
        assert!(!kde_animations_disabled("[KDE]\nSingleClick=false\n"));
        assert!(!kde_animations_disabled("not a pair"));
    }

    #[test]
    fn hex_colors_accept_short_and_long_forms() {
        assert_eq!(parse_hex_color("#f38ba8"), Some(MOCHA.red));
        assert_eq!(parse_hex_color("#fba"), parse_hex_color("#ffbbaa"));
        assert_eq!(parse_hex_color("fba"), parse_hex_color("#ffbbaa"));
        assert_eq!(parse_hex_color("cba6f7"), parse_hex_color("#cba6f7"));
        assert_eq!(parse_hex_color(" #4c4f69 "), Some(LATTE.text));
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("#gggggg"), None);
    }

    #[test]
    fn every_card_swatch_has_a_parseable_hex_value() {
        for theme in [Theme::Dark, Theme::Light] {
            for swatch in card_colors(theme) {
                if let Some(color) = swatch.color {
                    assert_eq!(parse_hex_color(swatch.hex), Some(color));
                } else {
                    assert_eq!(swatch.hex, "inherit");
                }
            }
        }
    }
}
