//! Touch-first egui layout for the mobile app.

#![expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::excessive_nesting,
    clippy::fn_params_excessive_bools,
    clippy::shadow_reuse,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::wildcard_enum_match_arm,
    reason = "egui widget nesting and bounded layout math keep screen order obvious"
)]

use crate::mobile::card::{render_side, sides, sides_answer};
use crate::mobile::state::{AppState, DeckRow, ReviewPhase, Screen, ToastKind};
use egui::{
    Align, Button, Color32, ComboBox, CornerRadius, Event, FontId,
    Frame as EguiFrame, Key, Layout, Margin, ProgressBar, RichText, ScrollArea,
    Sense, Stroke, TextEdit, TextStyle, Ui, vec2,
};
use ranki_model::{Card, CardType, Rating, ThemeChoice};
use std::time::Duration;
use std::time::Instant;

const PAGE_MARGIN: i8 = 16;
const TAB_HEIGHT: f32 = 56.0;
const TAP_HEIGHT: f32 = 48.0;
const RATING_HEIGHT: f32 = 60.0;
const REVIEW_GAP: f32 = 8.0;
const COMPACT_HEIGHT: f32 = 360.0;
const NARROW_WIDTH: f32 = 320.0;

type Tab = (Screen, &'static str, &'static str);

/// egui app that owns the mobile state.
pub struct MobileApp {
    pub state: AppState,
    last_ime_focus: Option<egui::Id>,
}

impl MobileApp {
    #[must_use]
    pub const fn new(state: AppState) -> Self {
        Self {
            state,
            last_ime_focus: None,
        }
    }
}

impl eframe::App for MobileApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_style(ctx, self.state.settings.theme);
        let reviewing = self.state.screen == Screen::Review;
        let focused = reviewing && ctx.input(|input| input.focused);
        self.state.tick_focus(focused);
        if self.state.review.phase == ReviewPhase::Waiting {
            match self.state.pending_delay_secs() {
                Some(0) => self.state.poll_pending(),
                Some(delay) => ctx.request_repaint_after(Duration::from_secs(
                    u64::try_from(delay).unwrap_or(3_600).min(3_600),
                )),
                None => {}
            }
        }
        let now = Instant::now();
        if let Some(wait) = self.state.poll_toast(now) {
            ctx.request_repaint_after(wait);
        }
        let _ = self.state.poll_sync();
        if self.state.sync.busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if reviewing && !text_field_focused(ctx) {
            let phase = self.state.review.phase;
            let can_undo = self.state.review.last.is_some();
            let actions = ctx.input(|input| {
                input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        Event::Key {
                            key, pressed: true, ..
                        } => review_shortcut(*key, phase, can_undo, false),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });
            for action in actions {
                match action {
                    ReviewShortcut::Reveal => self.state.reveal(),
                    ReviewShortcut::Grade(rating) => {
                        let _ = self.state.rate(rating);
                    }
                    ReviewShortcut::Undo => {
                        let _ = self.state.undo();
                    }
                    ReviewShortcut::End => self.state.end_review(),
                }
            }
        }
        render(&mut self.state, ctx);
        let ime_focus = ctx.memory(egui::Memory::focused);
        if ime_focus != self.last_ime_focus {
            match (self.last_ime_focus, ime_focus) {
                (Some(_), Some(_)) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(
                        false,
                    ));
                    ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(
                        true,
                    ));
                }
                (Some(_), None) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(
                        false,
                    ));
                }
                (None, Some(_)) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(
                        true,
                    ));
                }
                (None, None) => {}
            }
            self.last_ime_focus = ime_focus;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReviewShortcut {
    Reveal,
    Grade(Rating),
    Undo,
    End,
}

fn text_field_focused(ctx: &egui::Context) -> bool {
    ctx.memory(egui::Memory::focused)
        .is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}

fn review_shortcut(
    key: Key,
    phase: ReviewPhase,
    can_undo: bool,
    text_focused: bool,
) -> Option<ReviewShortcut> {
    if text_focused {
        return None;
    }
    match key {
        Key::Space | Key::Enter if phase == ReviewPhase::Question => {
            Some(ReviewShortcut::Reveal)
        }
        Key::Num1 if phase == ReviewPhase::Answer => {
            Some(ReviewShortcut::Grade(Rating::Again))
        }
        Key::Num2 if phase == ReviewPhase::Answer => {
            Some(ReviewShortcut::Grade(Rating::Hard))
        }
        Key::Num3 if phase == ReviewPhase::Answer => {
            Some(ReviewShortcut::Grade(Rating::Good))
        }
        Key::Num4 if phase == ReviewPhase::Answer => {
            Some(ReviewShortcut::Grade(Rating::Easy))
        }
        Key::Backspace if can_undo => Some(ReviewShortcut::Undo),
        Key::Escape => Some(ReviewShortcut::End),
        _ => None,
    }
}

struct Palette {
    bg: Color32,
    panel: Color32,
    panel_alt: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    accent_ink: Color32,
    border: Color32,
    danger: Color32,
    peach: Color32,
    success: Color32,
}

const fn palette(theme: ThemeChoice, system_dark: bool) -> Palette {
    let dark = match theme {
        ThemeChoice::Dark => true,
        ThemeChoice::Light => false,
        ThemeChoice::System => system_dark,
    };
    if dark {
        Palette {
            bg: Color32::from_rgb(24, 24, 37),
            panel: Color32::from_rgb(35, 36, 52),
            panel_alt: Color32::from_rgb(49, 50, 68),
            text: Color32::from_rgb(220, 224, 247),
            muted: Color32::from_rgb(166, 173, 200),
            accent: Color32::from_rgb(137, 180, 250),
            accent_ink: Color32::from_rgb(17, 17, 27),
            border: Color32::from_rgb(69, 71, 90),
            danger: Color32::from_rgb(243, 139, 168),
            peach: Color32::from_rgb(250, 179, 135),
            success: Color32::from_rgb(166, 227, 161),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(239, 241, 245),
            panel: Color32::from_rgb(255, 255, 255),
            panel_alt: Color32::from_rgb(230, 233, 239),
            text: Color32::from_rgb(76, 79, 105),
            muted: Color32::from_rgb(108, 111, 133),
            accent: Color32::from_rgb(30, 102, 245),
            accent_ink: Color32::from_rgb(255, 255, 255),
            border: Color32::from_rgb(188, 192, 204),
            danger: Color32::from_rgb(210, 15, 57),
            peach: Color32::from_rgb(254, 100, 11),
            success: Color32::from_rgb(64, 160, 43),
        }
    }
}

fn apply_style(ctx: &egui::Context, theme: ThemeChoice) {
    let system_dark = matches!(ctx.theme(), egui::Theme::Dark);
    let p = palette(theme, system_dark);
    let mut style = (*ctx.style()).clone();
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(18.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(17.0));
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(26.0));
    style.spacing.item_spacing = vec2(12.0, 12.0);
    style.spacing.button_padding = vec2(14.0, 10.0);
    style.spacing.interact_size = vec2(TAP_HEIGHT, TAP_HEIGHT);
    style.visuals.dark_mode = matches!(theme, ThemeChoice::Dark)
        || (theme == ThemeChoice::System && system_dark);
    style.visuals.override_text_color = Some(p.text);
    style.visuals.weak_text_color = Some(p.muted);
    style.visuals.panel_fill = p.bg;
    style.visuals.window_fill = p.bg;
    style.visuals.extreme_bg_color = p.panel;
    style.visuals.widgets.noninteractive.bg_fill = p.panel;
    style.visuals.widgets.inactive.bg_fill = p.panel_alt;
    style.visuals.widgets.hovered.bg_fill = p.panel_alt;
    style.visuals.widgets.active.bg_fill = p.panel_alt;
    style.visuals.widgets.inactive.fg_stroke.color = p.text;
    style.visuals.widgets.hovered.fg_stroke.color = p.text;
    style.visuals.widgets.active.fg_stroke.color = p.text;
    style.visuals.text_edit_bg_color = Some(p.panel_alt);
    style.visuals.widgets.inactive.weak_bg_fill = p.panel_alt;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.border);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.border);
    style.visuals.widgets.noninteractive.bg_stroke =
        Stroke::new(1.0_f32, p.border);
    style.visuals.widgets.noninteractive.corner_radius = CornerRadius::same(12);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(12);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(12);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(12);
    ctx.set_style(style);
}

/// Draws the current screen, with review actions or tabs pinned to the bottom.
pub fn render(app: &mut AppState, ctx: &egui::Context) {
    let system_dark = matches!(ctx.theme(), egui::Theme::Dark);
    let p = palette(app.settings.theme, system_dark);
    egui::CentralPanel::default()
        .frame(
            EguiFrame::new()
                .fill(p.bg)
                .inner_margin(Margin::same(PAGE_MARGIN)),
        )
        .show(ctx, |ui| {
            show_toast(app, ui, &p);
            let size = ui.available_size();
            let short = size.y < 420.0;
            let reviewing = app.screen == Screen::Review;
            let show_tabs = !reviewing;
            let compact = size.y < COMPACT_HEIGHT || size.x < NARROW_WIDTH;
            let spacing = ui.spacing().item_spacing.y;
            let chrome = if show_tabs {
                tab_height(size.x) + spacing
            } else if reviewing {
                review_actions_height(app, compact, short) + spacing
            } else {
                0.0
            };
            let content_height = (size.y - chrome).max(1.0);
            ui.allocate_ui_with_layout(
                vec2(size.x, content_height),
                Layout::top_down(Align::Min),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt((
                            app.screen,
                            app.review.current.as_ref().map(|card| card.id),
                        ))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            match app.screen {
                                Screen::Decks => render_decks(app, ui, &p),
                                Screen::Review => {
                                    render_review_content(app, ui, &p, short);
                                }
                                Screen::Add => render_add(app, ui, &p),
                                Screen::Browse => render_browse(app, ui, &p),
                                Screen::Statistics => {
                                    render_statistics(app, ui, &p);
                                }
                                Screen::Settings => {
                                    render_settings(app, ui, &p);
                                }
                            }
                        });
                },
            );
            if reviewing {
                let height = review_actions_height(app, compact, short);
                ui.allocate_ui_with_layout(
                    vec2(size.x, height),
                    Layout::top_down(Align::Min),
                    |ui| {
                        render_review_actions(app, ui, &p, compact, short);
                    },
                );
            } else if show_tabs {
                let height = tab_height(size.x);
                ui.allocate_ui_with_layout(
                    vec2(size.x, height),
                    Layout::top_down(Align::Min),
                    |ui| render_tabs(app, ui, &p),
                );
            }
        });
}

fn show_toast(app: &mut AppState, ui: &mut Ui, p: &Palette) {
    let Some(toast) = app.toast.clone() else {
        return;
    };
    let color = match toast.kind {
        ToastKind::Success => p.success,
        ToastKind::Error => p.danger,
    };
    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, color))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(toast.text).color(color));
                if ui
                    .add_sized(
                        [72.0, TAP_HEIGHT],
                        touch_button(
                            RichText::new("Dismiss").color(p.muted),
                            TAP_HEIGHT,
                            Color32::TRANSPARENT,
                            p.muted,
                        ),
                    )
                    .clicked()
                {
                    app.toast = None;
                }
            });
        });
    ui.add_space(10.0);
}

fn touch_button(
    text: RichText,
    height: f32,
    fill: Color32,
    ink: Color32,
) -> Button<'static> {
    Button::new(text.color(ink).strong())
        .min_size(vec2(0.0, height))
        .corner_radius(CornerRadius::same(12))
        .fill(fill)
}

fn secondary_button(
    text: RichText,
    height: f32,
    p: &Palette,
) -> Button<'static> {
    Button::new(text.color(p.text).size(17.0).strong())
        .min_size(vec2(0.0, height))
        .corner_radius(CornerRadius::same(12))
        .fill(p.panel_alt)
        .stroke(Stroke::new(1.0_f32, p.border))
}

fn title(ui: &mut Ui, text: &str, p: &Palette) {
    ui.label(RichText::new(text).size(26.0).strong().color(p.text));
    ui.add_space(10.0);
}

fn tab_height(width: f32) -> f32 {
    if width < NARROW_WIDTH {
        TAP_HEIGHT + REVIEW_GAP + TAP_HEIGHT
    } else {
        TAB_HEIGHT
    }
}

fn render_tabs(app: &mut AppState, ui: &mut Ui, p: &Palette) {
    let tabs = [
        (Screen::Decks, "Decks", "Decks"),
        (Screen::Add, "Add", "Add"),
        (Screen::Browse, "Browse", "Browse"),
        (Screen::Statistics, "Stats", "Stats"),
        (Screen::Settings, "Settings", "Prefs"),
    ];
    let narrow = ui.available_width() < NARROW_WIDTH;
    let rows: &[&[Tab]] = if narrow {
        &[&tabs[..3], &tabs[3..]]
    } else {
        &[&tabs[..]]
    };
    let height = if narrow { TAP_HEIGHT } else { TAB_HEIGHT };
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing =
            vec2(8.0, if narrow { REVIEW_GAP } else { 0.0 });
        ui.spacing_mut().button_padding = vec2(2.0, 8.0);
        ui.style_mut()
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        ui.vertical(|ui| {
            for row in rows {
                let width = (ui.available_width()
                    - ((row.len() as f32) - 1.0) * 8.0)
                    / (row.len() as f32);
                ui.horizontal(|ui| {
                    for (screen, wide, short) in *row {
                        let label = if narrow { *short } else { *wide };
                        let selected = app.screen == *screen;
                        let button = touch_button(
                            RichText::new(label),
                            height,
                            if selected { p.accent } else { p.panel },
                            if selected { p.accent_ink } else { p.text },
                        )
                        .stroke(Stroke::new(1.0_f32, p.border));
                        if ui.add_sized([width, height], button).clicked() {
                            let _ = app.set_screen(*screen);
                        }
                    }
                });
            }
        });
    });
}

fn render_decks(app: &mut AppState, ui: &mut Ui, p: &Palette) {
    title(ui, "Decks", p);
    if let Some(row) = app
        .deck_draft
        .confirm_remove
        .and_then(|id| app.decks.iter().find(|row| row.id == Some(id)))
        .cloned()
    {
        deck_removal(app, ui, p, &row);
    }

    let mut query = app.deck_query.clone();
    let search = TextEdit::singleline(&mut query)
        .hint_text("Search decks")
        .desired_width(f32::MAX)
        .min_size(vec2(0.0, TAP_HEIGHT));
    if ui
        .add_sized([ui.available_width(), TAP_HEIGHT], search)
        .changed()
    {
        app.set_deck_query(query);
    }
    ui.add_space(12.0);

    let study_all = touch_button(
        RichText::new("Study all"),
        TAP_HEIGHT,
        p.accent,
        p.accent_ink,
    )
    .min_size(vec2(ui.available_width(), TAP_HEIGHT));
    if ui.add(study_all).clicked() {
        let _ = app.start_review(None);
    }
    ui.add_space(12.0);

    deck_editor(app, ui, p);

    let filter = app.deck_query.trim().to_lowercase();
    let rows: Vec<DeckRow> = app
        .decks
        .iter()
        .filter(|row| {
            filter.is_empty() || row.name.to_lowercase().contains(&filter)
        })
        .cloned()
        .collect();
    if rows.is_empty() {
        if app.deck_query.trim().is_empty() {
            ui.label(RichText::new("No decks yet.").color(p.muted));
        } else {
            ui.label(
                RichText::new("No decks match this search.").color(p.muted),
            );
            ui.add_space(8.0);
            let clear =
                secondary_button(RichText::new("Clear search"), TAP_HEIGHT, p);
            if ui
                .add_sized([ui.available_width(), TAP_HEIGHT], clear)
                .clicked()
            {
                app.set_deck_query(String::new());
            }
        }
    }
    for row in rows {
        EguiFrame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .inner_margin(Margin::same(12))
            .corner_radius(CornerRadius::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new(row.name).size(20.0).strong());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    stat(ui, "New", row.stats.new_cards, p);
                    stat(ui, "Learning", row.stats.learning_cards, p);
                    stat(ui, "Review", row.stats.review_cards, p);
                });
                ui.add_space(10.0);
                let label = if row.due == 0 {
                    String::from("No cards due")
                } else {
                    format!("Study ({} due)", row.due)
                };
                let button = touch_button(
                    RichText::new(label),
                    TAP_HEIGHT,
                    p.panel_alt,
                    p.text,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add(button).clicked() {
                    let _ = app.start_review(row.id);
                }
                if let Some(id) = row.id {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let width = (ui.available_width() - 8.0).max(0.0) / 2.0;
                        let rename = secondary_button(
                            RichText::new("Rename"),
                            TAP_HEIGHT,
                            p,
                        );
                        if ui.add_sized([width, TAP_HEIGHT], rename).clicked() {
                            app.start_deck_rename(id);
                        }
                        let remove = secondary_button(
                            RichText::new("Remove"),
                            TAP_HEIGHT,
                            p,
                        );
                        if ui.add_sized([width, TAP_HEIGHT], remove).clicked() {
                            app.request_remove_deck(id);
                        }
                    });
                }
            });
        ui.add_space(10.0);
    }
}

fn deck_editor(app: &mut AppState, ui: &mut Ui, p: &Palette) {
    let renaming = app.deck_draft.renaming.is_some();
    let heading = if renaming { "Rename deck" } else { "New deck" };
    let action = if renaming { "Save name" } else { "Create deck" };
    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.border))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(heading).size(20.0).strong());
            ui.add_space(8.0);
            let mut name = app.deck_draft.name.clone();
            let field = TextEdit::singleline(&mut name)
                .hint_text("Deck name")
                .desired_width(f32::MAX)
                .min_size(vec2(0.0, TAP_HEIGHT));
            if ui
                .add_sized([ui.available_width(), TAP_HEIGHT], field)
                .changed()
            {
                app.deck_draft.name = name;
            }
            ui.add_space(8.0);
            let save = touch_button(
                RichText::new(action),
                TAP_HEIGHT,
                p.accent,
                p.accent_ink,
            )
            .min_size(vec2(ui.available_width(), TAP_HEIGHT));
            if ui.add(save).clicked() {
                let _ = app.save_deck();
            }
            if renaming {
                ui.add_space(8.0);
                let cancel = secondary_button(
                    RichText::new("Cancel rename"),
                    TAP_HEIGHT,
                    p,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add(cancel).clicked() {
                    app.cancel_deck_draft();
                }
            }
        });
    ui.add_space(10.0);
}

fn deck_removal(app: &mut AppState, ui: &mut Ui, p: &Palette, row: &DeckRow) {
    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.danger))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("Remove deck?").size(20.0).strong());
            ui.label(
                RichText::new(format!(
                    "{} and every card in it will be deleted.",
                    row.name
                ))
                .color(p.muted),
            );
            ui.add_space(10.0);
            let remove = touch_button(
                RichText::new("Remove deck"),
                TAP_HEIGHT,
                p.danger,
                p.accent_ink,
            )
            .min_size(vec2(ui.available_width(), TAP_HEIGHT));
            if ui.add(remove).clicked() {
                let _ = app.confirm_remove_deck();
            }
            ui.add_space(8.0);
            let keep =
                secondary_button(RichText::new("Keep deck"), TAP_HEIGHT, p)
                    .min_size(vec2(ui.available_width(), TAP_HEIGHT));
            if ui.add(keep).clicked() {
                app.cancel_remove_deck();
            }
        });
    ui.add_space(12.0);
}

fn metric(ui: &mut Ui, label: &str, value: String, p: &Palette) {
    EguiFrame::new()
        .fill(p.panel_alt)
        .inner_margin(Margin::symmetric(10, 8))
        .corner_radius(CornerRadius::same(10))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(value).size(18.0).strong());
                ui.label(RichText::new(label).size(13.0).color(p.muted));
            });
        });
}

fn stat(ui: &mut Ui, label: &str, value: usize, p: &Palette) {
    metric(ui, label, value.to_string(), p);
}

fn render_review_content(
    app: &mut AppState,
    ui: &mut Ui,
    p: &Palette,
    short: bool,
) {
    if short {
        ui.add_space(4.0);
    } else {
        title(ui, "Review", p);
    }
    let review = app.review.clone();
    let remaining = review.queue.len()
        + review.pending.len()
        + usize::from(review.current.is_some());
    let total = review.answered + remaining;
    let fraction = if total == 0 {
        1.0
    } else {
        (review.answered as f32) / (total as f32)
    };
    ui.add(
        ProgressBar::new(fraction)
            .desired_width(ui.available_width())
            .desired_height(8.0)
            .fill(p.accent),
    );
    ui.add_space(if short { 4.0 } else { 8.0 });
    ui.label(
        RichText::new(format!("{} / {} reviewed", review.answered, total))
            .color(p.muted),
    );
    if !short {
        let counts = review_counts(
            &review.queue,
            &review.pending,
            review.current.as_ref(),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            stat(ui, "New", counts[0], p);
            stat(ui, "Learning", counts[1], p);
            stat(ui, "Review", counts[2], p);
        });
    }
    ui.add_space(if short { 6.0 } else { 12.0 });

    if matches!(review.phase, ReviewPhase::Summary | ReviewPhase::Waiting) {
        let waiting = review.phase == ReviewPhase::Waiting;
        EguiFrame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .inner_margin(Margin::same(16))
            .corner_radius(CornerRadius::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let heading = if waiting {
                    "Next card pending"
                } else {
                    "Session complete"
                };
                ui.label(RichText::new(heading).size(22.0).strong());
                ui.add_space(8.0);
                ui.label(&review.summary);
            });
        if short && app.review.last.is_some() {
            ui.add_space(8.0);
            let undo = secondary_button(RichText::new("Undo"), TAP_HEIGHT, p)
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
            if ui.add(undo).clicked() {
                let _ = app.undo();
            }
        }
        return;
    }

    let media_dir = app.settings.paths.media_dir.clone();
    if let (Some(card), Some(note), Some(model)) = (
        review.current.as_ref(),
        review.note.as_ref(),
        review.model.as_ref(),
    ) {
        let (question, _) = sides(model, note, card.ordinal);
        let (_, answer) = sides_answer(model, note, card.ordinal);
        EguiFrame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .inner_margin(Margin::same(16))
            .corner_radius(CornerRadius::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new("Question").size(14.0).color(p.muted));
                ui.add_space(6.0);
                render_side(ui, &question, &media_dir);
                if review.phase == ReviewPhase::Answer {
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.label(RichText::new("Answer").size(14.0).color(p.muted));
                    ui.add_space(6.0);
                    render_side(ui, &answer, &media_dir);
                }
            });
    } else {
        ui.label(RichText::new("No card loaded.").color(p.muted));
    }
    if short {
        ui.add_space(8.0);
        if app.review.last.is_some() {
            let undo = secondary_button(RichText::new("Undo"), TAP_HEIGHT, p)
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
            if ui.add(undo).clicked() {
                let _ = app.undo();
            }
            ui.add_space(6.0);
        }
        let end = secondary_button(RichText::new("End session"), TAP_HEIGHT, p)
            .min_size(vec2(ui.available_width(), TAP_HEIGHT));
        if ui.add(end).clicked() {
            app.end_review();
        }
    }
}

fn review_actions_height(app: &AppState, compact: bool, short: bool) -> f32 {
    if short {
        let primary = match app.review.phase {
            ReviewPhase::Answer if compact => TAP_HEIGHT,
            ReviewPhase::Answer => RATING_HEIGHT,
            ReviewPhase::Question
            | ReviewPhase::Summary
            | ReviewPhase::Waiting => TAP_HEIGHT,
        };
        return REVIEW_GAP + primary + REVIEW_GAP;
    }
    let primary = match app.review.phase {
        ReviewPhase::Question | ReviewPhase::Summary => TAP_HEIGHT,
        ReviewPhase::Answer if compact => TAP_HEIGHT,
        ReviewPhase::Answer => RATING_HEIGHT,
        ReviewPhase::Waiting => 0.0,
    };
    let gap = if primary > 0.0 { REVIEW_GAP } else { 0.0 };
    let height = REVIEW_GAP + primary + gap + TAP_HEIGHT + REVIEW_GAP;
    if compact || app.review.last.is_none() {
        height
    } else {
        height + TAP_HEIGHT + REVIEW_GAP
    }
}

fn render_review_actions(
    app: &mut AppState,
    ui: &mut Ui,
    p: &Palette,
    compact: bool,
    short: bool,
) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        ui.add_space(REVIEW_GAP);
        let phase = app.review.phase;
        if short && matches!(phase, ReviewPhase::Summary | ReviewPhase::Waiting)
        {
            let back =
                secondary_button(RichText::new("Back to decks"), TAP_HEIGHT, p)
                    .min_size(vec2(ui.available_width(), TAP_HEIGHT));
            if ui.add(back).clicked() {
                app.end_review();
            }
            return;
        }
        match phase {
            ReviewPhase::Question => {
                let reveal = touch_button(
                    RichText::new("Show answer"),
                    TAP_HEIGHT,
                    p.accent,
                    p.accent_ink,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add(reveal).clicked() {
                    app.reveal();
                }
            }
            ReviewPhase::Answer => {
                let ratings = [
                    (Rating::Again, "Again", p.danger),
                    (Rating::Hard, "Hard", p.peach),
                    (Rating::Good, "Good", p.accent),
                    (Rating::Easy, "Easy", p.success),
                ];
                let height = if compact { TAP_HEIGHT } else { RATING_HEIGHT };
                let gap = if compact { 4.0 } else { 8.0 };
                let width = (ui.available_width() - 3.0 * gap).max(0.0) / 4.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    if compact {
                        ui.spacing_mut().button_padding = vec2(4.0, 2.0);
                    }
                    for (index, (rating, label, fill)) in
                        ratings.iter().enumerate()
                    {
                        let ink =
                            if matches!(rating, Rating::Hard | Rating::Easy) {
                                Color32::from_rgb(24, 24, 37)
                            } else {
                                p.accent_ink
                            };
                        let interval = interval_label(
                            *app.review.intervals.get(index).unwrap_or(&0),
                        );
                        let text = format!("{label}\n{interval}");
                        let size = if compact { 14.0 } else { 17.0 };
                        let button = touch_button(
                            RichText::new(text).size(size),
                            height,
                            *fill,
                            ink,
                        );
                        if ui.add_sized([width, height], button).clicked() {
                            let _ = app.rate(*rating);
                        }
                    }
                });
            }
            ReviewPhase::Summary => {
                let again = touch_button(
                    RichText::new("Study again"),
                    TAP_HEIGHT,
                    p.accent,
                    p.accent_ink,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add(again).clicked() {
                    let _ = app.start_review(app.review_scope);
                }
            }
            ReviewPhase::Waiting => {}
        }
        if short {
            return;
        }
        if phase != ReviewPhase::Waiting {
            ui.add_space(REVIEW_GAP);
        }
        let undo = app.review.last.is_some();
        let end_label =
            if matches!(phase, ReviewPhase::Summary | ReviewPhase::Waiting) {
                "Back to decks"
            } else {
                "End session"
            };
        if compact {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = REVIEW_GAP;
                let width = if undo {
                    (ui.available_width() - REVIEW_GAP) / 2.0
                } else {
                    ui.available_width()
                };
                if undo {
                    let button =
                        secondary_button(RichText::new("Undo"), TAP_HEIGHT, p);
                    if ui.add_sized([width, TAP_HEIGHT], button).clicked() {
                        let _ = app.undo();
                    }
                }
                let button =
                    secondary_button(RichText::new(end_label), TAP_HEIGHT, p);
                if ui.add_sized([width, TAP_HEIGHT], button).clicked() {
                    app.end_review();
                }
            });
        } else {
            if undo {
                let button =
                    secondary_button(RichText::new("Undo"), TAP_HEIGHT, p)
                        .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add(button).clicked() {
                    let _ = app.undo();
                }
                ui.add_space(REVIEW_GAP);
            }
            let button =
                secondary_button(RichText::new(end_label), TAP_HEIGHT, p)
                    .min_size(vec2(ui.available_width(), TAP_HEIGHT));
            if ui.add(button).clicked() {
                app.end_review();
            }
        }
        ui.add_space(REVIEW_GAP);
    });
}

fn review_counts(
    queue: &[Card],
    pending: &[Card],
    current: Option<&Card>,
) -> [usize; 3] {
    let mut counts = [0_usize; 3];
    for card in current
        .into_iter()
        .chain(queue.iter())
        .chain(pending.iter())
    {
        if let Some(slot) = counts.get_mut(card_bucket(card)) {
            *slot = (*slot).saturating_add(1);
        }
    }
    counts
}

const fn card_bucket(card: &Card) -> usize {
    match card.card_type {
        CardType::New => 0,
        CardType::Learning | CardType::Relearning => 1,
        CardType::Review => 2,
    }
}

fn interval_label(interval: i32) -> String {
    if interval >= 0 {
        return format!("{interval}d");
    }
    let seconds = interval.saturating_abs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", ceil_div(seconds, 60))
    } else if seconds < 86_400 {
        format!("{}h", ceil_div(seconds, 3_600))
    } else {
        format!("{}d", ceil_div(seconds, 86_400))
    }
}

const fn ceil_div(value: i32, divisor: i32) -> i32 {
    value
        .saturating_add(divisor.saturating_sub(1))
        .saturating_div(divisor)
}

fn render_add(app: &mut AppState, ui: &mut Ui, p: &Palette) {
    let editing = app.add.editing.is_some();
    title(ui, if editing { "Edit note" } else { "Add note" }, p);
    if editing {
        EguiFrame::new()
            .fill(p.panel_alt)
            .stroke(Stroke::new(1.0_f32, p.border))
            .inner_margin(Margin::same(12))
            .corner_radius(CornerRadius::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    RichText::new("Editing an existing note").color(p.muted),
                );
                ui.add_space(8.0);
                let cancel = secondary_button(
                    RichText::new("Cancel edit"),
                    TAP_HEIGHT,
                    p,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add(cancel).clicked() {
                    app.cancel_edit();
                }
            });
        ui.add_space(12.0);
    }

    let deck_names: Vec<String> =
        app.add.decks.iter().map(|deck| deck.name.clone()).collect();
    let mut deck_index = app.add.deck_index;
    ui.label(RichText::new("Deck").color(p.muted));
    ComboBox::from_id_salt("mobile-add-deck")
        .selected_text(
            deck_names
                .get(deck_index)
                .cloned()
                .unwrap_or_else(|| String::from("Choose a deck")),
        )
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for (index, name) in deck_names.iter().enumerate() {
                ui.selectable_value(&mut deck_index, index, name.clone());
            }
        });
    if deck_index != app.add.deck_index {
        app.set_add_deck(deck_index);
    }
    ui.add_space(8.0);

    let model_names: Vec<String> = app
        .add
        .models
        .iter()
        .map(|model| model.name.clone())
        .collect();
    let mut model_index = app.add.model_index;
    ui.label(RichText::new("Note type").color(p.muted));
    ui.add_enabled_ui(!editing, |ui| {
        ComboBox::from_id_salt("mobile-add-note-type")
            .selected_text(
                model_names
                    .get(model_index)
                    .cloned()
                    .unwrap_or_else(|| String::from("Choose a note type")),
            )
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for (index, name) in model_names.iter().enumerate() {
                    ui.selectable_value(&mut model_index, index, name.clone());
                }
            });
    });
    if model_index != app.add.model_index {
        app.set_add_model(model_index);
    }
    ui.add_space(12.0);

    let field_names: Vec<String> = app
        .add
        .models
        .get(app.add.model_index)
        .map_or_else(Vec::new, |model| model.fields.clone());
    for index in 0..app.add.fields.len() {
        let label = field_names
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("Field {}", index + 1));
        ui.label(RichText::new(label).color(p.muted));
        let mut value = app.add.fields.get(index).cloned().unwrap_or_default();
        let field = TextEdit::multiline(&mut value)
            .desired_rows(2)
            .desired_width(ui.available_width())
            .min_size(vec2(0.0, TAP_HEIGHT));
        if ui.add(field).changed() {
            app.set_add_field(index, value);
        }
        ui.add_space(8.0);
    }

    ui.label(RichText::new("Tags").color(p.muted));
    let tags = TextEdit::singleline(&mut app.add.tags)
        .hint_text("Separate tags with spaces")
        .desired_width(ui.available_width())
        .min_size(vec2(0.0, TAP_HEIGHT));
    ui.add(tags);
    ui.add_space(12.0);

    title(ui, "Preview", p);
    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.border))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if app.add.preview.is_empty() {
                ui.label(
                    RichText::new("The card preview appears here.")
                        .color(p.muted),
                );
            } else {
                ui.label(&app.add.preview);
            }
        });
    ui.add_space(12.0);

    let save = touch_button(
        RichText::new(if editing { "Save changes" } else { "Save note" }),
        TAP_HEIGHT,
        p.accent,
        p.accent_ink,
    );
    if ui
        .add_sized([ui.available_width(), TAP_HEIGHT], save)
        .clicked()
    {
        let _ = app.save_note();
    }
}

fn render_browse(app: &mut AppState, ui: &mut Ui, p: &Palette) {
    title(ui, "Browse", p);
    let mut draft = app.browse.draft.clone();
    let search = TextEdit::singleline(&mut draft)
        .hint_text("Search cards, tags, or decks")
        .desired_width(ui.available_width())
        .min_size(vec2(0.0, TAP_HEIGHT));
    let response = ui.add(search);
    if response.changed() {
        app.browse.draft.clone_from(&draft);
    }
    let commit = response.lost_focus()
        || (response.has_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter)));
    if commit && draft != app.browse.query {
        let _ = app.set_browse_query(draft);
    }
    ui.add_space(10.0);
    let count = if app.browse.total == 1 {
        String::from("1 card")
    } else {
        format!("{} cards", app.browse.total)
    };
    ui.label(RichText::new(count).color(p.muted));
    if app.browse.rows.len() < app.browse.total {
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "Showing first {} of {}",
                app.browse.rows.len(),
                app.browse.total
            ))
            .color(p.muted),
        );
    }
    ui.add_space(6.0);

    if app.browse.rows.is_empty() {
        let empty = if app.browse.query.trim().is_empty() {
            "No cards yet."
        } else {
            "No cards match this search."
        };
        ui.label(RichText::new(empty).color(p.muted));
    }
    let mut open = None;
    for row in &app.browse.rows {
        let card = EguiFrame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .inner_margin(Margin::same(12))
            .corner_radius(CornerRadius::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new(row.front.clone()).size(18.0).strong());
                ui.add_space(4.0);
                ui.label(RichText::new(row.back.clone()).color(p.muted));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("{} / {}", row.deck, row.meta))
                        .size(13.0)
                        .color(p.muted),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Tap to edit").size(13.0).color(p.accent),
                );
            })
            .response;
        if ui
            .interact(
                card.rect,
                ui.id().with(("browse-row", row.card_id)),
                Sense::click(),
            )
            .clicked()
        {
            open = Some(row.card_id);
        }
        ui.add_space(8.0);
    }
    if app.browse.rows.len() < app.browse.total {
        let more = secondary_button(RichText::new("Load more"), TAP_HEIGHT, p)
            .min_size(vec2(ui.available_width(), TAP_HEIGHT));
        if ui.add(more).clicked() {
            app.load_more();
        }
    }
    if let Some(card_id) = open {
        let _ = app.start_edit(card_id);
    }
}

fn render_statistics(app: &AppState, ui: &mut Ui, p: &Palette) {
    title(ui, "Statistics", p);
    let stats = app.statistics.clone();

    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.border))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("Today").size(20.0).strong());
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                stat(ui, "Reviews", stats.reviews_today, p);
                metric(ui, "Time", duration_label(stats.millis_today), p);
                metric(
                    ui,
                    "Accuracy",
                    stats.accuracy_percent().map_or_else(
                        || String::from("-"),
                        |value| format!("{value}%"),
                    ),
                    p,
                );
            });
            ui.add_space(8.0);
            ui.label(RichText::new(streak_label(stats.streak)).color(p.text));
            if stats.reviews_today == 0 {
                ui.label(RichText::new("No reviews yet today.").color(p.muted));
            }
        });
    ui.add_space(14.0);

    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.border))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("Forecast").size(20.0).strong());
            ui.label(
                RichText::new("Cards due over the next seven days")
                    .size(13.0)
                    .color(p.muted),
            );
            ui.add_space(8.0);
            if stats.forecast.is_empty() {
                ui.label(
                    RichText::new("The due forecast is unavailable.")
                        .color(p.muted),
                );
                return;
            }
            for (offset, due) in stats.forecast.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(forecast_label(offset)).color(p.muted),
                    );
                    ui.with_layout(
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            ui.label(RichText::new(due.to_string()).strong());
                        },
                    );
                });
            }
        });
}

fn streak_label(streak: usize) -> String {
    format!("Streak: {streak} day{}", if streak == 1 { "" } else { "s" })
}

fn forecast_label(offset: usize) -> String {
    match offset {
        0 => String::from("Today"),
        1 => String::from("Tomorrow"),
        _ => format!("+{offset} days"),
    }
}

fn duration_label(millis: u64) -> String {
    let seconds = millis.checked_div(1_000).unwrap_or_default();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3_600 {
        return format!(
            "{}m {}s",
            seconds.checked_div(60).unwrap_or_default(),
            seconds.checked_rem(60).unwrap_or_default()
        );
    }
    format!(
        "{}h {}m",
        seconds.checked_div(3_600).unwrap_or_default(),
        seconds
            .checked_rem(3_600)
            .unwrap_or_default()
            .checked_div(60)
            .unwrap_or_default()
    )
}

fn render_settings(app: &mut AppState, ui: &mut Ui, p: &Palette) {
    title(ui, "Settings", p);

    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.border))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("Appearance").size(20.0).strong());
            ui.add_space(8.0);
            let mut theme = app.settings.theme;
            ui.label(RichText::new("Theme").color(p.muted));
            ComboBox::from_id_salt("mobile-theme")
                .selected_text(theme_name(theme))
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut theme, ThemeChoice::Dark, "Dark");
                    ui.selectable_value(
                        &mut theme,
                        ThemeChoice::Light,
                        "Light",
                    );
                    ui.selectable_value(
                        &mut theme,
                        ThemeChoice::System,
                        "System",
                    );
                });
            if theme != app.settings.theme {
                app.set_theme(theme);
                app.say(format!("Theme set to {}", theme_name(theme)));
            }
        });
    ui.add_space(14.0);

    EguiFrame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.border))
        .inner_margin(Margin::same(12))
        .corner_radius(CornerRadius::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("AnkiWeb sync").size(20.0).strong());
            ui.add_space(8.0);
            if !app.sync.status.is_empty() {
                ui.label(RichText::new(app.sync.status.clone()).color(p.muted));
                ui.add_space(8.0);
            }

            if app.sync.signed_in {
                ui.label(RichText::new("Signed in").color(p.success));
                ui.add_space(8.0);
                if !app.sync.username.is_empty() {
                    ui.label(
                        RichText::new(app.sync.username.clone()).color(p.text),
                    );
                    ui.add_space(8.0);
                }
                let sync_now =
                    secondary_button(RichText::new("Sync now"), TAP_HEIGHT, p)
                        .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add_enabled(!app.sync.busy, sync_now).clicked() {
                    let _ = app.sync_now();
                }
                ui.add_space(8.0);
                let sign_out =
                    secondary_button(RichText::new("Sign out"), TAP_HEIGHT, p)
                        .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add_enabled(!app.sync.busy, sign_out).clicked() {
                    let _ = app.sign_out();
                }
            } else {
                ui.label(RichText::new("Not signed in").color(p.muted));
                ui.add_space(8.0);
                let username = TextEdit::singleline(&mut app.sync.username)
                    .hint_text("Username")
                    .desired_width(ui.available_width())
                    .min_size(vec2(0.0, TAP_HEIGHT));
                ui.add(username);
                ui.add_space(8.0);
                let password = TextEdit::singleline(&mut app.sync.password)
                    .hint_text("Password")
                    .password(true)
                    .desired_width(ui.available_width())
                    .min_size(vec2(0.0, TAP_HEIGHT));
                ui.add(password);
                ui.add_space(8.0);
                let sign_in = touch_button(
                    RichText::new("Sign in and sync"),
                    TAP_HEIGHT,
                    p.accent,
                    p.accent_ink,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add_enabled(!app.sync.busy, sign_in).clicked() {
                    let _ = app.sign_in();
                }
            }

            if app.sync.conflict {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Sync stopped on conflicting changes.")
                        .color(p.danger),
                );
                ui.add_space(8.0);
                let keep_local = secondary_button(
                    RichText::new("Keep local"),
                    TAP_HEIGHT,
                    p,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add_enabled(!app.sync.busy, keep_local).clicked() {
                    let _ = app.resolve_conflict(false);
                }
                ui.add_space(8.0);
                let keep_remote = secondary_button(
                    RichText::new("Keep remote"),
                    TAP_HEIGHT,
                    p,
                )
                .min_size(vec2(ui.available_width(), TAP_HEIGHT));
                if ui.add_enabled(!app.sync.busy, keep_remote).clicked() {
                    let _ = app.resolve_conflict(true);
                }
            }
        });
}

const fn theme_name(theme: ThemeChoice) -> &'static str {
    match theme {
        ThemeChoice::Dark => "Dark",
        ThemeChoice::Light => "Light",
        ThemeChoice::System => "System",
    }
}

#[cfg(test)]
mod tests {
    use super::{ReviewShortcut, interval_label, review_shortcut};
    use crate::mobile::state::ReviewPhase;
    use egui::Key;
    use ranki_model::Rating;

    #[test]
    fn interval_labels_use_readable_time_units() {
        assert_eq!(interval_label(-30), "30s");
        assert_eq!(interval_label(-119), "2m");
        assert_eq!(interval_label(-7_200), "2h");
        assert_eq!(interval_label(-86_400), "1d");
        assert_eq!(interval_label(3), "3d");
    }

    #[test]
    fn review_keys_reveal_grade_undo_and_end() {
        assert_eq!(
            review_shortcut(Key::Space, ReviewPhase::Question, false, false),
            Some(ReviewShortcut::Reveal)
        );
        assert_eq!(
            review_shortcut(Key::Enter, ReviewPhase::Question, false, false),
            Some(ReviewShortcut::Reveal)
        );
        let grades = [
            (Key::Num1, Rating::Again),
            (Key::Num2, Rating::Hard),
            (Key::Num3, Rating::Good),
            (Key::Num4, Rating::Easy),
        ];
        for (key, rating) in grades {
            assert_eq!(
                review_shortcut(key, ReviewPhase::Answer, false, false),
                Some(ReviewShortcut::Grade(rating))
            );
        }
        assert_eq!(
            review_shortcut(Key::Backspace, ReviewPhase::Answer, true, false),
            Some(ReviewShortcut::Undo)
        );
        assert_eq!(
            review_shortcut(Key::Escape, ReviewPhase::Question, false, false),
            Some(ReviewShortcut::End)
        );
    }

    #[test]
    fn review_keys_skip_text_fields_and_inactive_phases() {
        for key in [Key::Space, Key::Num3, Key::Backspace, Key::Escape] {
            assert_eq!(
                review_shortcut(key, ReviewPhase::Answer, true, true),
                None
            );
        }
        assert_eq!(
            review_shortcut(Key::Num3, ReviewPhase::Question, true, false),
            None
        );
        assert_eq!(
            review_shortcut(Key::Space, ReviewPhase::Answer, true, false),
            None
        );
        assert_eq!(
            review_shortcut(Key::Backspace, ReviewPhase::Answer, false, false),
            None
        );
    }
}
