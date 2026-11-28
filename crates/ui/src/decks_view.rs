//! Decks screen: header, summary cards, five-column table, and empty states.

use gpui::{
    App, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Rgba, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::{
    app::{self, Root, UiAction, due_total},
    components::{
        ButtonTone, Interaction, action_button, icon, icon_tile, keycap, panel,
        same_press, search_field_icon,
    },
    theme::{
        PAGE_X, PAGE_Y, SPACE_BLOCK, SPACE_GROUP, SPACE_INLINE, SPACE_RELATED,
        SPACE_SECTION, Style, TABLE_COLUMNS, alpha, page_width, queue_text,
    },
};

/// Rendered heights of a deck row, used to keep only nearby rows in the tree.
const ROW_ROOT_HEIGHT: f32 = 60.0;
const ROW_CHILD_HEIGHT: f32 = 45.0;
const ROW_OVERSCAN: f32 = 240.0;
/// Count columns that still fit beside the deck name and row actions.
#[derive(Clone, Copy)]
struct DeckColumns {
    new: bool,
    learning: bool,
    review: bool,
}

impl DeckColumns {
    fn for_width(width: f32) -> Self {
        Self {
            new: width >= 420.0,
            learning: width >= 580.0,
            review: width >= 580.0,
        }
    }
}

impl Root {
    pub(crate) fn visible_deck_rows(&self, _cx: &App) -> &[app::DeckRow] {
        &self.visible_rows
    }

    pub(crate) fn render_decks(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let width =
            page_width(f32::from(window.viewport_size().width), self.nav_width);
        let columns = DeckColumns::for_width(width);
        let colors = self.colors();
        let totals = self.leaf_totals;
        let smooth = self.motion_ms(1) > 0;
        self.decks_scroll.advance(window, smooth);
        let rows = self.visible_deck_rows(cx);

        let create_click = self.action_click(UiAction::CreateDeck);
        let mut content = div()
            .size_full()
            .flex()
            .flex_col()
            .id("decks-scroll")
            .gap(px(SPACE_SECTION))
            .overflow_scroll()
            .track_scroll(self.decks_scroll.handle())
            .px(px(PAGE_X))
            .py(px(PAGE_Y));

        content = content.on_scroll_wheel(cx.listener(
            move |this, event: &ScrollWheelEvent, window, cx| {
                this.deck_menu = None;
                if smooth {
                    let delta = event.delta.pixel_delta(window.line_height());
                    this.decks_scroll.wheel(-f32::from(delta.y));
                    window.refresh();
                    cx.stop_propagation();
                }
                cx.notify();
            },
        ));

        content = content.child(
            div()
                .flex()
                .flex_wrap()
                .items_end()
                .justify_between()
                .gap(px(SPACE_BLOCK))
                .child(
                    div()
                        .text_size(px(32.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Decks"),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(SPACE_RELATED))
                        .min_w(px(0.))
                        .child(
                            Interaction::raised(colors)
                                .apply(
                                    div()
                                        .id("decks-search")
                                        .flex()
                                        .items_center()
                                        .gap(px(SPACE_GROUP))
                                        .flex_initial()
                                        .w(px(460.))
                                        .min_w(px(160.))
                                        .h(px(44.))
                                        .px(px(SPACE_RELATED))
                                        .rounded(px(12.))
                                        .bg(colors.surface0)
                                        .text_size(px(14.))
                                        .text_color(colors.text)
                                        .cursor_pointer(),
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        let field = this.deck_search.clone();
                                        window.focus(
                                            field.read(cx).focus_handle(),
                                        );
                                    },
                                ))
                                .child(search_field_icon(
                                    self.deck_search.read(cx).focus_handle(),
                                    18.,
                                    colors,
                                    window,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(self.deck_search.clone()),
                                )
                                .child(keycap("/", colors, false)),
                        )
                        .child(
                            Interaction::raised(colors)
                                .apply(
                                    div()
                                        .id("decks-import")
                                        .flex()
                                        .items_center()
                                        .gap(px(SPACE_GROUP))
                                        .h(px(44.))
                                        .px(px(SPACE_BLOCK))
                                        .rounded(px(12.))
                                        .bg(colors.surface0)
                                        .text_color(colors.text)
                                        .text_size(px(14.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .cursor_pointer(),
                                )
                                .on_click(cx.listener(|_, _, _, cx| {
                                    Self::import_deck(cx);
                                }))
                                .child(icon("icon-upload", 18., colors.text))
                                .child("Import deck"),
                        )
                        .child(
                            Interaction::raised(colors)
                                .apply(
                                    div()
                                        .id("decks-create")
                                        .flex()
                                        .items_center()
                                        .gap(px(SPACE_GROUP))
                                        .h(px(44.))
                                        .px(px(SPACE_BLOCK))
                                        .rounded(px(12.))
                                        .bg(colors.surface0)
                                        .text_color(colors.text)
                                        .text_size(px(14.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .cursor_pointer(),
                                )
                                .on_click(create_click)
                                .child(icon("icon-plus", 18., colors.text))
                                .child("Create deck"),
                        ),
                ),
        );

        content = content.child(self.render_summary(totals, width, cx));

        let kind = app::empty_kind(self.decks.len(), rows.len());
        if !rows.is_empty() {
            let mut row_tops = Vec::with_capacity(rows.len().saturating_add(1));
            let mut rows_height = 0.0;
            for row in rows {
                row_tops.push(rows_height);
                rows_height += if row.depth == 0 {
                    ROW_ROOT_HEIGHT
                } else {
                    ROW_CHILD_HEIGHT
                };
            }
            row_tops.push(rows_height);
            let (mut first_row, mut last_row) =
                if self.decks_scroll.handle().children_count() > 2 {
                    let handle = self.decks_scroll.handle();
                    let table_top = handle
                        .bounds_for_item(2)
                        .map_or(0.0, |bounds| f32::from(bounds.top()));
                    let viewport = handle.bounds();
                    let table_scroll = self.decks_scroll.offset()
                        - (table_top - f32::from(viewport.top()));
                    let header_height =
                        2.0 * SPACE_RELATED + f32::from(window.line_height());
                    let visible_top =
                        table_scroll - header_height - ROW_OVERSCAN;
                    let visible_bottom = table_scroll - header_height
                        + f32::from(viewport.size.height)
                        + ROW_OVERSCAN;
                    let mut first = 0;
                    while first < rows.len()
                        && row_tops[first + 1] <= visible_top
                    {
                        first += 1;
                    }
                    let mut last = first;
                    while last < rows.len() && row_tops[last] < visible_bottom {
                        last += 1;
                    }
                    (first, last)
                } else {
                    (0, rows.len())
                };
            if let Some(edit) = self.deck_edit.as_ref()
                && let Some(index) =
                    rows.iter().position(|row| row.id == Some(edit.id))
            {
                first_row = first_row.min(index);
                last_row = last_row.max(index.saturating_add(1));
            }
            let mut table = div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .rounded(px(20.))
                .bg(colors.mantle)
                .border_1()
                .border_color(colors.surface0)
                .overflow_hidden();
            table = table.child(
                div()
                    .flex()
                    .items_center()
                    .px(px(SPACE_SECTION))
                    .py(px(SPACE_RELATED))
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .children(
                        [
                            Some(header_cell("DECK", colors.subtext, 0)),
                            columns.new.then(|| {
                                header_cell(
                                    "NEW",
                                    queue_text(0, self.theme()),
                                    1,
                                )
                            }),
                            columns.learning.then(|| {
                                header_cell(
                                    "LEARNING",
                                    queue_text(1, self.theme()),
                                    2,
                                )
                            }),
                            columns.review.then(|| {
                                header_cell(
                                    "REVIEW",
                                    queue_text(2, self.theme()),
                                    3,
                                )
                            }),
                            Some(header_cell("", colors.subtext, 4)),
                        ]
                        .into_iter()
                        .flatten(),
                    ),
            );
            if first_row > 0 {
                table =
                    table.child(div().h(px(row_tops[first_row])).flex_none());
            }
            for row in &rows[first_row..last_row] {
                table = table.child(self.render_deck_row(row, columns, cx));
            }
            if last_row < rows.len() {
                table = table.child(
                    div().h(px(rows_height - row_tops[last_row])).flex_none(),
                );
            }
            content = content.child(table);
        }
        if kind != app::EmptyKind::None {
            content = content.child(self.render_empty(kind));
        }
        content
    }

    fn render_summary(
        &self,
        totals: ranki_model::DeckStats,
        width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = self.colors();
        let style = Style::new(colors, self.theme());
        let due = due_total(totals);
        let queue_stat = |label: &str, value: usize, color: Rgba| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(SPACE_INLINE))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_GROUP))
                        .child(crate::components::dot(color, 8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(colors.subtext)
                                .child(label.to_owned()),
                        ),
                )
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(format!("{value}")),
                )
        };
        div()
            .flex()
            .flex_wrap()
            .gap(px(SPACE_SECTION))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(280.))
                    .flex_col()
                    .p(px(SPACE_SECTION))
                    .rounded(px(24.))
                    .bg(colors.surface0)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_end()
                            .justify_between()
                            .gap(px(SPACE_BLOCK))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .line_height(px(16.))
                                            .text_color(colors.subtext)
                                            .child(app::DECK_SUMMARY_LABEL),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(64.))
                                            .line_height(px(68.))
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .text_color(colors.mauve)
                                            .child(format!("{due}")),
                                    ),
                            )
                            .child(
                                Interaction::accent(style)
                                    .apply(
                                        div()
                                            .id("decks-study-all")
                                            .flex()
                                            .flex_none()
                                            .items_center()
                                            .gap(px(SPACE_GROUP))
                                            .h(px(52.))
                                            .px(px(SPACE_SECTION))
                                            .rounded(px(26.))
                                            .bg(colors.mauve)
                                            .text_color(style.ink())
                                            .text_size(px(16.))
                                            .font_weight(
                                                gpui::FontWeight::SEMIBOLD,
                                            )
                                            .cursor_pointer(),
                                    )
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.study_all(window, cx);
                                        },
                                    ))
                                    .child(icon("icon-play", 18., style.ink()))
                                    .child("Study all"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(SPACE_BLOCK))
                            .mt(px(SPACE_BLOCK))
                            .pt(px(SPACE_BLOCK))
                            .border_t_1()
                            .border_color(colors.surface1)
                            .child(queue_stat(
                                "New",
                                totals.new_cards,
                                colors.blue,
                            ))
                            .child(queue_stat(
                                "Learning",
                                totals.learning_cards,
                                colors.red,
                            ))
                            .child(queue_stat(
                                "Review",
                                totals.review_cards,
                                colors.green,
                            )),
                    ),
            )
            .child(
                panel(colors)
                    .w(px(width.min(320.0)))
                    .flex_none()
                    // The forecast supplies its own top margin.
                    .gap(px(0.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Next 7 days"),
                            )
                            .child(
                                div()
                                    .px(px(SPACE_RELATED))
                                    .py(px(SPACE_INLINE))
                                    .rounded(px(10.))
                                    .bg(colors.surface0)
                                    .text_size(px(12.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(colors.mauve)
                                    .child(format!(
                                        "{} due",
                                        app::forecast_total(&self.forecast)
                                    )),
                            ),
                    )
                    .child(if self.forecast.is_empty() {
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(colors.overlay)
                                    .child("7-day forecast unavailable"),
                            )
                    } else {
                        let today = i64::from(self.timing(Self::now()).days_elapsed);
                        let peak = self
                            .forecast
                            .iter()
                            .copied()
                            .max()
                            .unwrap_or_default();
                        let mut bars = div()
                            .mt(px(SPACE_BLOCK))
                            .flex()
                            .flex_1()
                            .gap(px(SPACE_GROUP));
                        for (index, count) in self.forecast.iter().enumerate() {
                            bars = bars.child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_end()
                                    .gap(px(SPACE_GROUP))
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .font_weight(
                                                gpui::FontWeight::SEMIBOLD,
                                            )
                                            .text_color(if *count == 0 {
                                                colors.overlay
                                            } else {
                                                colors.text
                                            })
                                            .child(format!("{count}")),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .h(px(app::forecast_bar_height(
                                                *count, peak,
                                            )))
                                            .rounded(px(8.))
                                            .bg(if *count == 0 {
                                                colors.surface1
                                            } else {
                                                colors.mauve
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(if index == 0 {
                                                colors.mauve
                                            } else {
                                                colors.subtext
                                            })
                                            .child(
                                                crate::statistics_view::weekday(
                                                    today.saturating_add(
                                                        i64::try_from(index)
                                                            .unwrap_or(
                                                                i64::MAX,
                                                            ),
                                                    ),
                                                    self.collection_start,
                                                ),
                                            ),
                                    ),
                            );
                        }
                        bars
                    }),
            )
    }

    fn render_deck_row(
        &self,
        row: &app::DeckRow,
        columns: DeckColumns,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = self.colors();
        let style = Style::new(colors, self.theme());
        let row_path = row.path.clone();
        let virtual_parent = row.id.is_none() && row.has_children;
        let deck_id = row.id;
        let index = row.deck_index;
        let is_selected = index.is_some() && index == self.selected;
        let has_due = due_total(row.stats) > 0;
        let meta = row.meta.clone();
        let mut name_cell = div()
            .flex()
            .items_center()
            .gap(px(SPACE_RELATED))
            .pl(px(SPACE_SECTION * row.depth as f32))
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden();
        if row.depth > 0 {
            name_cell = name_cell.child(
                div().h(px(28.)).w(px(2.)).bg(colors.surface1).flex_none(),
            );
        }
        if row.has_children {
            let path = row.path.clone();
            name_cell = name_cell.child(
                Interaction::surface(colors)
                    .apply(
                        div()
                            .id(row.chevron_key.clone())
                            .debug_selector(|| row.chevron_key.to_string())
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(24.))
                            .h(px(24.))
                            .flex_none()
                            .cursor_pointer(),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_deck(path.clone(), cx);
                        },
                    ))
                    .child(icon(
                        if row.expanded {
                            "icon-chevron-down"
                        } else {
                            "icon-chevron-right"
                        },
                        18.,
                        colors.overlay,
                    )),
            );
        } else {
            name_cell = name_cell.child(div().w(px(24.)).flex_none());
        }
        let mut label = div()
            .flex()
            .items_center()
            .gap(px(SPACE_GROUP))
            .min_w(px(0.))
            .overflow_hidden();
        label = label.child(
            div()
                .text_size(px(if row.depth == 0 { 15. } else { 14.5 }))
                .font_weight(if row.depth == 0 {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::MEDIUM
                })
                .font_family(crate::theme::font_for_text(&row.name))
                .child(row.name.clone()),
        );
        if row.depth > 0 && row.has_children {
            label = label.child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(colors.subtext)
                    .child(meta.clone()),
            );
        }
        if row.depth == 0 {
            name_cell = name_cell.child(icon_tile(
                if row.has_children {
                    "icon-layers"
                } else {
                    "icon-book"
                },
                deck_hue(&row.path, colors),
            ));
            name_cell = name_cell.child(
                div().flex().flex_col().child(label).child(
                    div()
                        .text_size(px(12.5))
                        .text_color(colors.subtext)
                        .child(meta),
                ),
            );
        } else {
            name_cell = name_cell.child(label);
        }
        if let Some(deck_id) = row.id
            && let Some(edit) = self.deck_edit.clone()
            && edit.id == deck_id
        {
            name_cell = name_cell
                .child(
                    div()
                        .debug_selector(|| String::from("deck-rename-field"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_, _, _, cx| cx.stop_propagation()),
                        )
                        .child(self.rename_field.clone()),
                )
                .child(action_button(
                    "decks-save-rename",
                    "Save",
                    ButtonTone::Primary,
                    style,
                    Box::new(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.apply_rename(window, cx);
                        },
                    )),
                ));
        }
        let count = |value: usize, color: Rgba| {
            div()
                .w(px(TABLE_COLUMNS[1]))
                .flex_none()
                .flex()
                .justify_center()
                .text_size(px(15.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if value == 0 { colors.overlay } else { color })
                .child(format!("{value}"))
        };
        let mut action = div()
            .w(px(TABLE_COLUMNS[4]))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(SPACE_GROUP));
        if let Some(target) = index {
            if has_due {
                action = action.child(
                    Interaction::accent(style)
                        .apply(
                            div()
                                .id(row.study_key.clone())
                                .h(px(30.))
                                .px(px(SPACE_RELATED))
                                .flex()
                                .items_center()
                                .rounded(px(9.))
                                .bg(colors.mauve)
                                .text_color(style.ink())
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .cursor_pointer(),
                        )
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                this.study_deck(target, window, cx);
                            },
                        ))
                        .child("Study"),
                );
            }
            let id = row.id.unwrap_or(0);
            let menu_open = self.deck_menu.is_some_and(|menu| menu.id == id);
            action = action.child(
                Interaction::new(
                    alpha(colors.text, 0.0),
                    if menu_open {
                        alpha(colors.mauve, 0.24)
                    } else {
                        alpha(colors.mauve, 0.12)
                    },
                    if menu_open {
                        alpha(colors.mauve, 0.32)
                    } else {
                        alpha(colors.mauve, 0.22)
                    },
                    colors.mauve,
                )
                .apply(
                    div()
                        .id(row.menu_key.clone())
                        .debug_selector(|| row.menu_key.to_string())
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(30.))
                        .h(px(30.))
                        .rounded(px(8.))
                        .bg(if menu_open {
                            alpha(colors.mauve, 0.16)
                        } else {
                            alpha(colors.text, 0.0)
                        })
                        .cursor_pointer(),
                )
                .on_click(cx.listener(
                    move |this, event: &ClickEvent, _, cx| {
                        let same_press = same_press(
                            event,
                            this.deck_menu_dismissed_press.as_ref(),
                        );
                        cx.stop_propagation();
                        this.deck_menu_dismissed_press = None;
                        if !same_press {
                            this.deck_menu = if menu_open {
                                None
                            } else {
                                Some(app::DeckMenu {
                                    id,
                                    anchor: event.position(),
                                })
                            };
                        }
                        cx.notify();
                    },
                ))
                .child(icon("icon-more", 18., colors.overlay)),
            );
        }
        let hover_bg = if is_selected {
            crate::theme::blend(colors.surface0, colors.surface1, 0.5)
        } else {
            colors.surface0
        };
        let press_bg = if is_selected {
            crate::theme::blend(colors.surface1, colors.crust, 0.14)
        } else {
            crate::theme::blend(colors.surface0, colors.surface1, 0.7)
        };
        div()
            .id(row.key.clone())
            .debug_selector(|| row.key.to_string())
            .flex()
            .items_center()
            .h(px(if row.depth == 0 {
                ROW_ROOT_HEIGHT
            } else {
                ROW_CHILD_HEIGHT
            }))
            .px(px(SPACE_SECTION))
            .border_t_1()
            .border_color(colors.surface0)
            .cursor_pointer()
            .focusable()
            .hover(move |style| style.bg(hover_bg))
            .active(move |style| style.bg(press_bg))
            .focus(move |style| {
                style.bg(crate::theme::blend(hover_bg, colors.text, 0.08))
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    if let Some(id) = deck_id {
                        this.deck_menu = Some(app::DeckMenu {
                            id,
                            anchor: event.position,
                        });
                        cx.notify();
                    }
                }),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if let Some(index) = index {
                    this.study_deck(index, window, cx);
                } else if virtual_parent {
                    this.toggle_deck(row_path.clone(), cx);
                }
            }))
            .bg(if is_selected {
                colors.surface0
            } else {
                colors.mantle
            })
            .child(name_cell)
            .children(
                [
                    columns.new.then(|| {
                        count(row.stats.new_cards, queue_text(0, self.theme()))
                    }),
                    columns.learning.then(|| {
                        count(
                            row.stats.learning_cards,
                            queue_text(1, self.theme()),
                        )
                    }),
                    columns.review.then(|| {
                        count(
                            row.stats.review_cards,
                            queue_text(2, self.theme()),
                        )
                    }),
                    Some(action),
                ]
                .into_iter()
                .flatten(),
            )
    }

    fn render_empty(&self, kind: app::EmptyKind) -> impl IntoElement {
        let colors = self.colors();
        let theme = self.theme();
        let (title, detail) = app::empty_copy(kind);
        let (label, action) = match kind {
            app::EmptyKind::NoDecks => ("Create deck", UiAction::CreateDeck),
            app::EmptyKind::NoMatch => ("Clear search", UiAction::ClearSearch),
            app::EmptyKind::None => ("", UiAction::OpenDecks),
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SPACE_GROUP))
            .min_h(px(260.))
            .rounded(px(20.))
            .bg(colors.mantle)
            .border_1()
            .border_color(colors.surface0)
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(colors.subtext)
                    .child(detail),
            )
            .child(
                Interaction::accent(Style::new(colors, theme))
                    .apply(
                        div()
                            .id("decks-empty-action")
                            .mt(px(SPACE_RELATED))
                            .flex()
                            .items_center()
                            .h(px(44.))
                            .px(px(SPACE_SECTION))
                            .rounded(px(12.))
                            .bg(colors.mauve)
                            .text_color(crate::theme::accent_ink(theme))
                            .text_size(px(14.))
                            .cursor_pointer(),
                    )
                    .on_click(self.action_click(action))
                    .child(label),
            )
    }
}

/// One table header cell. Every column keeps the exact width its row cell
/// uses, so counts stay under their labels and the action cell stays reserved.
fn header_cell(label: &str, color: Rgba, column: usize) -> gpui::Div {
    let mut cell = div()
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(color);
    if column == 0 {
        cell = cell.flex_1();
    } else {
        cell = cell
            .flex()
            .flex_none()
            .justify_center()
            .w(px(TABLE_COLUMNS[column]));
    }
    cell.child(label.to_owned())
}

/// Deterministic deck tint from the path, matching the mockup palette.
fn deck_hue(path: &str, colors: crate::theme::Palette) -> Rgba {
    let fingerprint = path
        .bytes()
        .fold(0usize, |sum, byte| sum.wrapping_add(byte as usize));
    match fingerprint % 3 {
        0 => colors.mauve,
        1 => colors.teal,
        _ => colors.peach,
    }
}

#[cfg(test)]
mod tests {
    use super::{DeckColumns, deck_hue, header_cell};

    #[test]
    fn deck_columns_drop_as_the_page_narrows() {
        let narrow = DeckColumns::for_width(340.0);
        assert!(!narrow.new && !narrow.learning && !narrow.review);
        let medium = DeckColumns::for_width(600.0);
        assert!(medium.new && medium.learning && medium.review);
        let wide = DeckColumns::for_width(1048.0);
        assert!(wide.new && wide.learning && wide.review);
    }
    #[test]
    fn deck_header_cells_share_the_row_column_widths() {
        let head = header_cell("NEW", crate::theme::MOCHA.blue, 1);
        let _ = head;
        let columns = crate::theme::TABLE_COLUMNS;
        assert_eq!(columns[0], 1.0);
        assert_eq!(columns[1], columns[2]);
        assert_eq!(columns[2], columns[3]);
        assert!(columns[4] >= 120.0);
    }

    #[test]
    fn deck_tints_are_stable_and_varied() {
        let colors = crate::theme::MOCHA;
        assert_eq!(deck_hue("Japanese", colors), deck_hue("Japanese", colors));
        let hues = ["Default", "Japanese", "Spanish"]
            .map(|path| deck_hue(path, colors));
        assert!(hues.iter().any(|hue| *hue != hues[0]));
        let _ = &colors;
    }
}
