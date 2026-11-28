# Mockup design system

Research date: 2026-11-22. Scope: the four supplied 1280x800 desktop mockups
(Decks, Add card, Learning question, Learning answer). No product code changed.
All four were extracted to temporary directories, read as HTML/CSS, and rendered
with `html_preview` at 1280x816. The React runtime scripts were blocked in the
preview sandbox, but the artboard markup rendered fully in every case.

The mockups are already Catppuccin Mocha with a mauve accent. Treat the values
below as the shared desktop spec, not as four separate themes.

View keys used below: `D` Decks, `A` Add card, `Q` Learning question,
`R` Learning answer.

## Color tokens

| Token | Mockup value | Role | Views |
| --- | --- | --- | --- |
| `base` | `#1e1e2e` | App canvas and default text backdrop | all |
| `crust` | `#11111b` | Sidebar background, ink on mauve fills | D, A, Q |
| `mantle` | `#181825` | Sidebar footer, graph panel, example sentence box, add footer, session list, sync card | D, A, R |
| `surface0` | `#313244` | Search field, segmented track, deck tile, card stage, active field, selector, added-card panel | all |
| `surface1` | `#45475a` | Separators, keycaps, inactive badges, inactive bar fill, idle field border | all |
| `text` | `#cdd6f4` | Primary text and headings | all |
| `subtext` | `#a6adc8` | Secondary labels, metadata, muted buttons | all |
| `overlay1` | `#7f849c` | Tertiary icons, timestamps, eyebrows, zero counts | D, A, Q, R |
| `mauve` | `#cba6f7` | Accent: primary buttons, active nav, progress, answer text, completed steps | all |
| `red` | `#f38ba8` | Learning queue, Again rating | D, Q, R |
| `peach` | `#fab387` | Hard rating, Spanish deck tile | D, R |
| `green` | `#a6e3a1` | Review queue, success ticks, sync dot | D, A, Q, R |
| `blue` | `#89b4fa` | New queue, Easy rating | D, Q, R |
| `teal` | `#94e2d5` | Computer Science deck tile | D |
| `yellow` | `#f9e2af` | Present in the repo palette, absent from all four mockups | none |

Translucent values are the same palette colors, not new hues:

| Overlay | Alpha | Use |
| --- | --- | --- |
| `rgba(203,166,247,.16)` | 41/255 | Selected nav item, streak badge, audio button, deck tiles (D, Q, R) |
| `rgba(203,166,247,.22)` | 56/255 | Selected segment (A) |
| `rgba(17,17,27,.18)` | 46/255 | Keycap on a mauve fill (D, A, Q) |
| `rgba(hue,.14)` / `rgba(hue,.4)` / `rgba(hue,.2)` | 36 / 102 / 51 | Rating fill / border / key badge (R) |
| `rgba(148,226,213,.16)`, `rgba(250,179,135,.16)` | 41/255 | Teal and peach deck tiles (D) |

No gradients, shadows, or colored glows appear. Borders are flat 1px, 1.5px,
or 2px, never softened by a shadow.

## Typography

All artboards declare `Inter` at weights 400, 500, 600, 700, then `Noto Sans JP`
at weight 500 for Japanese glyphs, then `system-ui, sans-serif`. No Nerd Font is
used in the mockups. Icons are inline Lucide-style SVG: 14, 16, 18, or 20 px,
`stroke-width="2"`, round caps and joins, currentColor. The repo's `CCL Nerd Font` fallback is for text glyph icons in egui, not for matching these SVGs.

| Size | Weight | Line height / tracking | Use |
| --- | --- | --- | --- |
| 84px | 500 | 1.1 | Study question and answer word (Q, R) |
| 64px | 700 | 1.05, -0.03em | Due-count number (D) |
| 52px | 500 | 1.15 | Live preview front text (A) |
| 36px | 700 | default | Answer meaning on study card (R) |
| 32px | 700 | -0.02em | Decks page title (D) |
| 28px | 700 | -0.02em | New card title (A) |
| 26px | 500 | default | Front editor text (A) |
| 20px | 600 / 500 | default | Queue counts (D), back editor text (A) |
| 18px | 700 | default | Sidebar brand (D, A) |
| 17px | 500 | default | Example sentence (R) |
| 16px | 600 | default | Rating label (R), Study all button (D) |
| 15px | 600 / 500 | default | Deck row names, summary numbers, action buttons |
| 14.5px | 500 | default | Subdeck row label (D) |
| 14px | 600 / 500 | default | Nav items, buttons, labels, body copy |
| 13px | 600 / 500 | default | Field labels, chips, footer actions, hints |
| 12.5px | 400 | default | Deck metadata (D) |
| 12px | 600 / 500 | default | Captions, table headers, legend labels, timestamps |
| 11px | 600 / 700 | 0.08em on overlines | Keycaps, eyebrows, small badges (all) |
| 11px | 600 | 0.1em | Centered stage eyebrow (Q, R) |

Only three line heights are explicit (1.05, 1.1, 1.15); other sizes use the
browser default. Use 1.2 as the egui default for compact labels. Tabular numerals
are used on all deck count columns.

## Space, shape, and borders

- Spacing steps actually used: 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28,
  32, 40 px. Page padding is 32px vertical by 40px horizontal. Sidebar padding is
  24px top, 14px sides, 20px bottom. Content gaps: 24px (D), 20px (A form),
  14px (A grid and R ratings), 6px (sidebar nav), 4px (footer actions).
- Card padding is 28px (study stage), 24px (summary panels), 20px (preview and
  stepper panels), 16px (editor fields), 11-14px (list rows).
- Radii: 3px progress, 4px status dot, 6px keycap, 8px bar and toast-scale,
  9px segment, 10px nav/tiles, 12px controls, 13px toggle knob, 14px buttons and
  rows, 16px fields and example box, 18px rating buttons, 20px table shell,
  24px panels, 26px pill button, 32px study stage. Group related controls to a
  common radius; do not average them.
- Borders: 1px `surface1` for secondary buttons and footer; 1px `surface0` for
  panel and list separators; 1.5px `mauve` for the focused editor, 1.5px
  `surface1` for the idle editor, 1.5px dashed `mauve` for Add tag; 2px
  `surface1` for the subdeck tree line; 2px queue-colored bottom border for the
  active study queue.
- Focus and interaction states are not drawn in any mockup. The only CSS is a
  pointer cursor on buttons. Use egui's widget states as the extension: hover
  raises `surface0` to `surface1`, pressed uses `surface1` with text, focus adds
  a 2px mauve ring drawn outside the widget rect. Keep it instant.
- No media queries or fluid widths exist. The artboard is fixed at 1280x800.
  Treat this as a desktop reference; the app's 800x600 minimum still needs a
  narrower layout with the sidebar collapsed.

## Component inventory

| Component | States shown in the mockups |
| --- | --- |
| Primary button | Mauve fill, crust ink, 14-17px semibold, radius 12-18; Add card nav, Study all pill, Show answer wide, Add card footer |
| Secondary button | Transparent fill, 1px `surface1` border, text ink; Create deck, Cancel |
| Tertiary button | `surface0` fill or transparent, subtext ink; End session, Undo, Flag, Edit, Bury |
| Icon button | Mauve tint with mauve icon (audio), subtext icon on transparent (overflow, editor tools, footer) |
| Segmented control | Track `surface0`, selected segment mauve at 22% with mauve text, others subtext; note types (A) |
| Inputs | Active front editor (mauve 1.5px border and caret), idle back editor (surface1 border), search field (surface0, placeholder, keycap), deck selector rendered as a button with chevron, tags as removable chips, Add tag dashed, toggle on (A) |
| Deck rows | Parent expanded and collapsed chevrons, child rows with selected fill, subtree guide line, count columns in queue colors, zero count in overlay1, per-row Study button on selected child, overflow menu (D) |
| Cards and panels | Study stage 760x440 radius 32 with tags, audio, question, answer divider, and example box (Q, R); due panel, 7-day bar chart, table (D); add-card preview (A) |
| Modal | None shown. If added, use `mantle` fill, 1px `surface0` border, radius 24, centered |
| Toast | None shown. Existing app toast uses error fg on noninteractive fill; use `red` tint and 12px caption if aligned to this system |
| Progress | 420x6 track, radius 3, `surface0` track, mauve fill at 43% (Q, R); 7-day bars use mauve for today and `surface1` otherwise (D); add-card stepper uses filled check, mauve outline for current, `surface0` for future (A) |
| Badges | Keycaps, tags, streak badge, queue legends, success check icons, queue counters with colored underline |
| Keyboard hints | 11px 600/700, 2px 6px padding, radius 6, `surface1` fill with subtext ink; on mauve fills use crust at 18%. Keys shown: A, D, B, T, comma, slash, Space, 1-4, Esc, R, Z, F, E, B, command+Return |

## Motion

The mockups ship no transitions. The app adds one motion layer on top of this
system: page swaps cross-fade, menus, pickers, dialogs, banners, and toasts fade
and settle a few pixels into place, the answer and rating row rise in, and
forecast bars grow from the baseline. Entrances run 120 ms, page swaps 160 ms,
bar growth 180 ms, all on a cubic ease-out. They read the shared tokens in
`crates/ui/src/theme.rs` and honor the reduced-motion setting and desktop
preference, which paint every state instantly. Hover and press colors stay
instant, and card flips, confetti, scale pulses, and auto-advance stay out.

## Catppuccin mapping

| Role | Mocha | Latte | Note |
| --- | --- | --- | --- |
| background / base | `#1e1e2e` | `#eff1f5` | Exact mockup match in dark |
| surface raised / mantle | `#181825` | `#e6e9ef` | Exact mockup match in dark |
| surface sunken / crust | `#11111b` | `#dce0e8` | Also the dark ink on mauve |
| surface0 | `#313244` | `#ccd0da` | Exact mockup match in dark |
| surface1 | `#45475a` | `#bcc0cc` | Exact mockup match in dark |
| text | `#cdd6f4` | `#4c4f69` | Exact mockup match in dark |
| subtext | `#a6adc8` | `#6c6f85` | Exact mockup match in dark |
| overlay1 | `#7f849c` | `#8c8fa1` | Missing from `Palette`; add an `overlay` field |
| accent / mauve | `#cba6f7` | `#8839ef` | Exact mockup match in dark |
| danger / red | `#f38ba8` | `#d20f39` | Learning and Again |
| warning / peach | `#fab387` | `#fe640b` | Mockup Hard uses peach, not yellow |
| success / green | `#a6e3a1` | `#40a02b` | Review and success ticks |
| info / blue | `#89b4fa` | `#1e66f5` | New and Easy |
| teal | `#94e2d5` | `#179299` | Deck icon only |
| yellow | `#f9e2af` | `#df8e1d` | Palette only, no mockup role |

No mockup color lacks a Catppuccin equivalent. Mocha is a literal match. For
Latte, three adaptations are required because saturated hues fail 4.5:1 as text
on `#eff1f5` (green 2.96, peach 2.64, yellow 2.5). Keep hue as a 14% tint plus a
40% border, and set rating labels to `text`. Keep hue-colored text only where
contrast holds: red, blue, and mauve. The dark-on-mauve keycap `#11111b` at 18%
has no Latte equivalent; use white at 18% over `#8839ef` so the keycap stays a
lighter notch.

## egui 0.37 implementation notes

- `crates/ui/src/theme.rs` already defines `MOCHA`, `LATTE`, `palette`,
  `accent_fill`, `accent_ink`, `rating_fill`, `rating_ink`, and `visuals`.
  Extend it rather than adding a second palette.
- Add `overlay` to `Palette` and both constants. Add an alpha helper and the
  geometry constants below. `visuals.widgets` maps focus to `active` in 0.37, so
  a focus ring must be drawn explicitly with `Response::has_focus`.
- Use `Frame::NONE.fill(...).inner_margin(...).corner_radius(...)` for all
  surfaces. `CornerRadius::same` takes u8. `Button::new(...).fill(...)`,
  `.stroke(...)`, `.corner_radius(...)`, `.min_size(...)` cover the button
  states. `ProgressBar::new(...).fill(...).desired_height(...)` covers progress.
- Add custom text styles with `TextStyle::Name("question".into())` and
  `"answer"`, and keep the existing base-scaled helpers for accessibility.
  Current `question_size` caps at 32px and `answer_size` at 24px, far below the
  mockup's 84px and 36px; raise the caps or bypass them on the study stage.
- Add shared helpers in a new module under `crates/ui/src`: `surface_frame`,
  `primary_button`, `secondary_button`, `keycap`, `rating_button`, `segmented`,
  `deck_row`, `progress_bar`, and `focus_ring`. Route them through `palette(theme)`
  so no view hard-codes a hex value.
- Resolve CJK glyphs with Noto Sans JP only when Japanese text is present; the
  existing `font_candidates` logic already prefers the matched family and falls
  back to CCL Nerd Font.

<!-- end list -->

```rust
// crates/ui/src/theme.rs
use egui::Color32;

pub const RADIUS_KEYCAP: u8 = 6;
pub const RADIUS_CONTROL: u8 = 12;
pub const RADIUS_BUTTON: u8 = 14;
pub const RADIUS_FIELD: u8 = 16;
pub const RADIUS_RATING: u8 = 18;
pub const RADIUS_PANEL: u8 = 24;
pub const RADIUS_STAGE: u8 = 32;

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;
pub const SPACE_XL: f32 = 24.0;
pub const SPACE_XXL: f32 = 32.0;

pub const NAV_WIDTH: f32 = 232.0;
pub const TOPBAR_HEIGHT: f32 = 72.0;
pub const FOOTER_HEIGHT: f32 = 64.0;
pub const STAGE_SIZE: egui::Vec2 = egui::vec2(760.0, 440.0);
pub const REVEAL_HEIGHT: f32 = 76.0;
pub const RATING_HEIGHT: f32 = 76.0;
pub const PROGRESS_SIZE: egui::Vec2 = egui::vec2(420.0, 6.0);

pub const ACCENT_TINT_ALPHA: u8 = 41;       // .16
pub const ACCENT_SELECT_ALPHA: u8 = 56;     // .22
pub const ACCENT_KEY_ALPHA: u8 = 46;        // .18
pub const RATING_FILL_ALPHA: u8 = 36;       // .14
pub const RATING_BORDER_ALPHA: u8 = 102;    // .40
pub const RATING_KEY_ALPHA: u8 = 51;        // .20

pub const fn alpha(color: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied_const(color.r(), color.g(), color.b(), a)
}
```

`accent_ink` already returns crust in dark and white in light. For rating
buttons, add `rating_tint(code, theme) = alpha(rating_fill(code, theme), RATING_FILL_ALPHA)` and use `rating_fill` only for the 40% border. In Latte use
`palette(theme).text` for the label; in Mocha use `rating_fill` directly.
