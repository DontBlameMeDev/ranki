//! Shared navigation rail, toasts, and modal overlays.

use gpui::{
    AnyElement, AnyView, App, AppContext, ClickEvent, Context, Corner,
    CursorStyle, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, anchored, div, point, px,
};

use crate::{
    app::{Confirmation, Root, UiAction, View},
    components::{
        ButtonTone, ClickHandler, Interaction, action_button, brand, icon,
        keycap, panel, secondary_button,
    },
    theme::{
        BUTTON_HEIGHT, BUTTON_RADIUS, BUTTON_TEXT_SIZE, MOBILE_NAV_HEIGHT,
        MOBILE_TOUCH_TARGET, NAV_DIVIDER_WIDTH, Palette, SPACE_BLOCK,
        SPACE_GROUP, SPACE_INLINE, SPACE_RELATED, SPACE_SECTION, Style, alpha,
        is_mobile_width, visible_nav_width,
    },
};

struct LabelTooltip {
    text: SharedString,
    colors: Palette,
}

impl Render for LabelTooltip {
    fn render(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .px(px(SPACE_GROUP))
            .py(px(SPACE_INLINE))
            .rounded(px(8.))
            .bg(self.colors.mantle)
            .border_1()
            .border_color(self.colors.surface0)
            .text_size(px(12.))
            .text_color(self.colors.text)
            .child(self.text.clone())
    }
}

fn label_tooltip(
    label: impl Into<SharedString>,
    colors: Palette,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = label.into();
    move |_, cx| {
        cx.new(|_| LabelTooltip {
            text: text.clone(),
            colors,
        })
        .into()
    }
}

impl Root {
    pub(crate) fn render_nav(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = self.colors();
        let style = Style::new(colors, self.theme());
        let active = self.view;
        let viewport_width = f32::from(window.viewport_size().width);
        if is_mobile_width(viewport_width) {
            return self.render_mobile_nav().into_any_element();
        }

        let nav_width = visible_nav_width(viewport_width, self.nav_width);
        let auto_compact =
            viewport_width < crate::theme::NAV_AUTO_COMPACT_WIDTH;
        let compact = self.nav_compact() || auto_compact;
        let add_selected = active == View::AddCard;
        let add_ink = if add_selected {
            style.ink()
        } else {
            colors.subtext
        };
        let mut add = if add_selected {
            Interaction::accent(style)
        } else {
            Interaction::tinted(colors, colors.mauve)
        }
        .apply(
            div()
                .id("nav-add")
                .flex()
                .items_center()
                .gap(px(SPACE_GROUP))
                .h(px(44.))
                .px(px(SPACE_RELATED))
                .rounded(px(12.))
                .text_size(px(14.))
                .text_color(add_ink)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .cursor_pointer(),
        )
        .on_click(self.action_click(UiAction::OpenAdd))
        .child(icon("icon-plus", 18., add_ink));
        if compact {
            add = add
                .justify_center()
                .tooltip(label_tooltip("Add card", colors));
        } else {
            add = add.child("Add card").child(div().flex_1()).child(keycap(
                "A",
                colors,
                add_selected,
            ));
        }
        if add_selected {
            add = add.bg(colors.mauve).text_color(style.ink());
        }
        let add = add.w_full();
        let nav = |label: &'static str,
                   icon_name: &'static str,
                   key: &'static str,
                   item: View,
                   action: UiAction,
                   enabled: bool| {
            let selected = enabled && active == item;
            let ink = if selected {
                colors.mauve
            } else if enabled {
                colors.subtext
            } else {
                crate::theme::alpha(colors.subtext, 0.45)
            };
            let mut row = div()
                .id(SharedString::from(format!("nav-{label}")))
                .flex()
                .items_center()
                .gap(px(SPACE_RELATED))
                .h(px(40.))
                .px(px(SPACE_RELATED))
                .rounded(px(10.))
                .text_size(px(14.))
                .text_color(ink)
                .font_weight(gpui::FontWeight::SEMIBOLD);
            if selected {
                row = row.bg(alpha(colors.mauve, 0.16));
            }
            if enabled {
                row = Interaction::tinted(colors, colors.mauve)
                    .apply(row.cursor_pointer())
                    .on_click(self.action_click(action));
            }
            row = row.child(icon(icon_name, 18., ink));
            if compact {
                row =
                    row.justify_center().tooltip(label_tooltip(label, colors));
            } else {
                row = row.child(label.to_owned()).child(div().flex_1()).child(
                    if enabled {
                        keycap(key, colors, false).into_any_element()
                    } else {
                        div()
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(crate::theme::alpha(
                                colors.subtext,
                                0.6,
                            ))
                            .child("SOON")
                            .into_any_element()
                    },
                );
            }
            row
        };
        let mut header = div()
            .flex()
            .items_center()
            .gap(px(SPACE_GROUP))
            .px(px(SPACE_GROUP))
            .pb(px(SPACE_BLOCK))
            .child(brand());
        if compact {
            header = header.justify_center();
        } else {
            header = header.child(
                div()
                    .text_size(px(18.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Ranki"),
            );
        }
        div()
            .w(px(nav_width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .relative()
            .gap(px(SPACE_GROUP))
            .bg(colors.crust)
            .pt(px(SPACE_SECTION))
            .px(px(SPACE_RELATED))
            .pb(px(SPACE_SECTION))
            .child(header)
            .child(add)
            .child(div().h(px(SPACE_RELATED)))
            .child(nav(
                "Decks",
                "icon-grid",
                "D",
                View::Decks,
                UiAction::OpenDecks,
                true,
            ))
            .child(nav(
                "Browse",
                "icon-book",
                "B",
                View::Browse,
                UiAction::OpenBrowse,
                true,
            ))
            .child(nav(
                "Statistics",
                "icon-chart",
                "T",
                View::Statistics,
                UiAction::OpenStatistics,
                true,
            ))
            .child(div().flex_1())
            .children(self.render_nav_sync(compact))
            .child(nav(
                "Settings",
                "icon-settings",
                ",",
                View::Settings,
                UiAction::OpenSettings,
                true,
            ))
            .children((!auto_compact).then(|| self.render_nav_divider(cx)))
            .into_any_element()
    }

    /// Bottom bar used at phone widths. Each tab keeps a 44px touch target.
    fn render_mobile_nav(&mut self) -> impl IntoElement {
        let colors = self.colors();
        let active = self.view;
        let tab = |label: &'static str,
                   icon_name: &'static str,
                   item: View,
                   action: UiAction| {
            let selected = active == item;
            let ink = if selected {
                colors.mauve
            } else {
                colors.subtext
            };
            let mut button = div()
                .id(SharedString::from(format!("nav-{label}")))
                .debug_selector(move || format!("nav-{label}"))
                .flex()
                .flex_1()
                .min_w(px(MOBILE_TOUCH_TARGET))
                .h(px(MOBILE_TOUCH_TARGET))
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_INLINE))
                .rounded(px(10.))
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(ink)
                .cursor_pointer();
            if selected {
                button = button.bg(alpha(colors.mauve, 0.16));
            }
            Interaction::tinted(colors, colors.mauve)
                .apply(button)
                .on_click(self.action_click(action))
                .child(icon(icon_name, 20., ink))
                .child(label)
        };
        div()
            .id("mobile-nav")
            .debug_selector(|| String::from("mobile-nav"))
            .absolute()
            .bottom(px(0.))
            .left(px(0.))
            .right(px(0.))
            .h(px(MOBILE_NAV_HEIGHT))
            .flex()
            .items_center()
            .gap(px(SPACE_INLINE))
            .px(px(SPACE_INLINE))
            .bg(colors.crust)
            .border_t_1()
            .border_color(colors.surface0)
            .children([
                tab("Decks", "icon-grid", View::Decks, UiAction::OpenDecks),
                tab("Browse", "icon-book", View::Browse, UiAction::OpenBrowse),
                tab("Add", "icon-plus", View::AddCard, UiAction::OpenAdd),
                tab(
                    "Statistics",
                    "icon-chart",
                    View::Statistics,
                    UiAction::OpenStatistics,
                ),
                tab(
                    "Settings",
                    "icon-settings",
                    View::Settings,
                    UiAction::OpenSettings,
                ),
            ])
    }

    /// Grab strip on the rail's edge, drawn over the last few pixels.
    fn render_nav_divider(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.colors();
        let mut bar = div()
            .id("nav-divider")
            .debug_selector(|| String::from("nav-divider"))
            .absolute()
            .top(px(0.))
            .right(px(0.))
            .w(px(NAV_DIVIDER_WIDTH))
            .h_full()
            .cursor(CursorStyle::ResizeColumn);
        if self.nav_drag.is_some() {
            bar = bar.bg(colors.mauve);
        }
        bar.hover(move |style| style.bg(alpha(colors.mauve, 0.6)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.start_nav_drag(event, cx);
                }),
            )
    }

    pub(crate) fn render_overlay(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let viewport_width = f32::from(window.viewport_size().width);
        let viewport_height = f32::from(window.viewport_size().height);
        let modal_width = (viewport_width - 32.0).clamp(0.0, 420.0);
        let nav_width = visible_nav_width(viewport_width, self.nav_width);
        if self.help_open && !self.dialog_focus.contains_focused(window, cx) {
            let focus = self.dialog_focus.clone();
            cx.defer_in(window, move |root, window, _| {
                if root.help_open {
                    window.focus(&focus);
                }
            });
        }
        let colors = self.colors();
        let theme = self.theme();
        let style = Style::new(colors, theme);
        let mut layers: Vec<AnyElement> = Vec::new();
        let mut bars = self.active_scrollbars(window);
        let deck_picker_bar = bars
            .iter()
            .position(|(id, _)| *id == "deck-picker")
            .map(|index| bars.remove(index));
        for (id, handle) in bars {
            if let Some(scrollbar) = self.render_scrollbar(id, handle, cx) {
                layers.push(scrollbar);
            }
        }
        if self.external_change {
            let refresh = self.action_click(UiAction::Reload);
            layers.push(
                div()
                    .absolute()
                    .top(px(16.))
                    .left(px(nav_width + 32.))
                    .right(px(32.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(SPACE_BLOCK))
                    .px(px(SPACE_BLOCK))
                    .py(px(SPACE_RELATED))
                    .rounded(px(14.))
                    .bg(crate::theme::alpha(colors.red, 0.16))
                    .border_1()
                    .border_color(crate::theme::alpha(colors.red, 0.5))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_GROUP))
                            .child(icon("icon-bell", 18., colors.red))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(colors.text)
                                    .child(
                                        "Collection changed outside Ranki. Refresh before reviewing.",
                                    ),
                            ),
                    )
                    .child(
                        Interaction::accent(style)
                            .apply(
                                div()
                                    .id("external-refresh")
                                    .flex()
                                    .items_center()
                                    .gap(px(SPACE_GROUP))
                                    .h(px(36.))
                                    .px(px(SPACE_RELATED))
                                    .rounded(px(10.))
                                    .bg(colors.mauve)
                                    .text_color(style.ink())
                                    .cursor_pointer(),
                            )
                            .on_click(refresh)
                            .child("Refresh")
                            .child(keycap("F5", colors, true)),
                    )
                    .into_any_element(),
            );
        }
        if self.view == View::Decks
            && let Some(menu) = self.deck_menu
            && self
                .visible_deck_rows(cx)
                .iter()
                .any(|row| row.id == Some(menu.id))
            && let Some(deck) =
                self.decks.iter().find(|deck| deck.id == menu.id).cloned()
        {
            let rename_deck = deck.clone();
            let export_deck = deck.clone();
            let remove_deck = deck;
            layers.push(
                anchored()
                    .anchor(Corner::TopRight)
                    .snap_to_window_with_margin(px(16.))
                    .position(menu.anchor)
                    .offset(point(px(15.), px(19.)))
                    .child(
                        div()
                            .id("deck-actions-menu")
                            .debug_selector(|| "deck-actions-menu".to_owned())
                            .occlude()
                            .on_mouse_down_out(cx.listener(
                                move |this,
                                      event: &gpui::MouseDownEvent,
                                      _,
                                      cx| {
                                    this.deck_menu = None;
                                    this.deck_menu_dismissed_press =
                                        Some(event.clone());
                                    cx.notify();
                                },
                            ))
                            .flex_col()
                            .w(px(160.))
                            .gap(px(SPACE_INLINE))
                            .p(px(SPACE_GROUP))
                            .rounded(px(12.))
                            .bg(colors.mantle)
                            .border_1()
                            .border_color(colors.surface1)
                            .child(
                                Interaction::surface(colors)
                                    .apply(
                                        div()
                                            .id("decks-rename")
                                            .debug_selector(|| {
                                                "decks-rename".to_owned()
                                            })
                                            .w_full()
                                            .px(px(SPACE_RELATED))
                                            .py(px(SPACE_GROUP))
                                            .rounded(px(8.))
                                            .cursor_pointer(),
                                    )
                                    .on_click(cx.listener(
                                        move |this,
                                              _: &ClickEvent,
                                              window,
                                              cx| {
                                            this.deck_menu = None;
                                            this.start_rename(
                                                &rename_deck,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .text_size(px(13.))
                                    .child("Rename"),
                            )
                            .child(
                                Interaction::surface(colors)
                                    .apply(
                                        div()
                                            .id("decks-export")
                                            .debug_selector(|| {
                                                "decks-export".to_owned()
                                            })
                                            .w_full()
                                            .px(px(SPACE_RELATED))
                                            .py(px(SPACE_GROUP))
                                            .rounded(px(8.))
                                            .cursor_pointer(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            this.deck_menu = None;
                                            this.export_deck(&export_deck, cx);
                                        },
                                    ))
                                    .text_size(px(13.))
                                    .child("Export .apkg"),
                            )
                            .child(
                                Interaction::danger(colors)
                                    .apply(
                                        div()
                                            .id("decks-remove")
                                            .debug_selector(|| {
                                                "decks-remove".to_owned()
                                            })
                                            .w_full()
                                            .px(px(SPACE_RELATED))
                                            .py(px(SPACE_GROUP))
                                            .rounded(px(8.))
                                            .cursor_pointer()
                                            .text_color(colors.red),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.deck_menu = None;
                                            this.confirmation = Some(
                                                Confirmation::RemoveDeck {
                                                    id: remove_deck.id,
                                                    name: remove_deck
                                                        .name
                                                        .clone(),
                                                },
                                            );
                                            cx.notify();
                                            window.focus(&this.dialog_focus);
                                        },
                                    ))
                                    .text_size(px(13.))
                                    .child("Remove"),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if self.view == View::Browse
            && let Some(menu) = self.card_menu
            && let Some(row) = self.browse.rows.get(&menu.id)
        {
            let card_id = menu.id;
            let front = crate::browser_view::preview_line(&row.front);
            layers.push(
                anchored()
                    .anchor(Corner::TopRight)
                    .snap_to_window_with_margin(px(16.))
                    .position(menu.anchor)
                    .offset(point(px(15.), px(19.)))
                    .child(
                        div()
                            .id("card-actions-menu")
                            .debug_selector(|| "card-actions-menu".to_owned())
                            .occlude()
                            .on_mouse_down_out(cx.listener(
                                move |this,
                                      event: &gpui::MouseDownEvent,
                                      _,
                                      cx| {
                                    this.card_menu = None;
                                    this.card_menu_dismissed_press =
                                        Some(event.clone());
                                    cx.notify();
                                },
                            ))
                            .flex_col()
                            .w(px(160.))
                            .gap(px(SPACE_INLINE))
                            .p(px(SPACE_GROUP))
                            .rounded(px(12.))
                            .bg(colors.mantle)
                            .border_1()
                            .border_color(colors.surface1)
                            .child(
                                Interaction::surface(colors)
                                    .apply(
                                        div()
                                            .id("browse-edit")
                                            .w_full()
                                            .px(px(SPACE_RELATED))
                                            .py(px(SPACE_GROUP))
                                            .rounded(px(8.))
                                            .cursor_pointer(),
                                    )
                                    .on_click(cx.listener(
                                        move |this,
                                              _: &ClickEvent,
                                              window,
                                              cx| {
                                            this.card_menu = None;
                                            this.start_card_edit(
                                                card_id, window, cx,
                                            );
                                        },
                                    ))
                                    .text_size(px(13.))
                                    .child("Edit"),
                            )
                            .child(
                                Interaction::danger(colors)
                                    .apply(
                                        div()
                                            .id("browse-remove")
                                            .w_full()
                                            .px(px(SPACE_RELATED))
                                            .py(px(SPACE_GROUP))
                                            .rounded(px(8.))
                                            .cursor_pointer()
                                            .text_color(colors.red),
                                    )
                                    .on_click(cx.listener(
                                        move |this,
                                              _: &ClickEvent,
                                              window,
                                              cx| {
                                            this.card_menu = None;
                                            this.confirmation = Some(
                                                Confirmation::RemoveCard {
                                                    id: card_id,
                                                    front: front.clone(),
                                                },
                                            );
                                            cx.notify();
                                            window.focus(&this.dialog_focus);
                                        },
                                    ))
                                    .text_size(px(13.))
                                    .child("Remove"),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if self.view == View::AddCard && self.add.deck_picker {
            layers.push(
                anchored()
                    .anchor(Corner::TopLeft)
                    .snap_to_window_with_margin(px(16.))
                    .position(self.add.deck_picker_anchor)
                    .offset(point(px(0.), px(SPACE_BLOCK + SPACE_INLINE)))
                    .child(self.render_deck_picker(colors, window, cx))
                    .into_any_element(),
            );
        }
        if self.view == View::AddCard && self.add.color_picker.is_some() {
            let anchor = self
                .add
                .color_picker
                .as_ref()
                .map_or(point(px(0.), px(0.)), |picker| picker.anchor);
            layers.push(
                anchored()
                    .anchor(Corner::TopLeft)
                    .snap_to_window_with_margin(px(16.))
                    .position(anchor)
                    .offset(point(px(0.), px(SPACE_BLOCK + SPACE_INLINE)))
                    .child(self.render_color_picker(cx))
                    .into_any_element(),
            );
        }
        if let Some((id, handle)) = deck_picker_bar
            && let Some(scrollbar) = self.render_scrollbar(id, handle, cx)
        {
            layers.push(scrollbar);
        }
        if self.create_modal {
            let cancel = cx.listener(|this, _: &ClickEvent, window, cx| {
                this.cancel_create(window, cx);
            });
            let create_error = self.create_error.clone();
            layers.push(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(crate::theme::alpha(colors.crust, 0.7))
                    .occlude()
                    .id("create-deck-backdrop")
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            this.cancel_create(window, cx);
                        },
                    ))
                    .key_context("ranki_dialog")
                    .track_focus(&self.dialog_focus)
                    .on_action(cx.listener(
                        |this, _: &crate::app::ConfirmDialog, window, cx| {
                            this.dispatch(UiAction::Confirm, window, cx);
                        },
                    ))
                    .on_action(cx.listener(
                        |this, _: &crate::app::CancelDialog, window, cx| {
                            this.dispatch(UiAction::CancelDialog, window, cx);
                        },
                    ))
                    .child(
                        div()
                            .w(px(modal_width))
                            .min_w(px(0.))
                            .id("create-deck-modal")
                            .flex()
                            .flex_col()
                            .gap(px(16.))
                            .p(px(24.))
                            .rounded(px(24.))
                            .bg(colors.mantle)
                            .border_1()
                            .border_color(colors.surface0)
                            .occlude()
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Create deck"),
                            )
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .text_color(colors.subtext)
                                    .child("Name the new deck."),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .h(px(44.))
                                    .px(px(14.))
                                    .rounded(px(12.))
                                    .bg(colors.surface0)
                                    .border_1()
                                    .border_color(if create_error.is_some() {
                                        colors.red
                                    } else {
                                        colors.mauve
                                    })
                                    .text_size(px(14.))
                                    .text_color(colors.text)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .child(self.create_field.clone()),
                                    ),
                            )
                            .children(create_error.map(|message| {
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(SPACE_INLINE))
                                    .text_size(px(13.))
                                    .text_color(colors.red)
                                    .child(icon("icon-bell", 14., colors.red))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .child(message),
                                    )
                            }))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .justify_end()
                                    .gap(px(SPACE_GROUP))
                                    .child(secondary_button(
                                        "Cancel",
                                        colors,
                                        Box::new(cancel),
                                    ))
                                    .child(action(
                                        "create-deck-primary",
                                        "Create",
                                        ButtonTone::Primary,
                                        style,
                                        UiAction::SubmitDeck,
                                        cx,
                                    )),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(confirmation) = self.confirmation.clone() {
            let (title, detail, accept, tone) = match confirmation {
                Confirmation::RemoveCard { front, .. } => (
                    "Remove card?",
                    format!("{front} will be deleted with its review history."),
                    "Remove card",
                    ButtonTone::Danger,
                ),
                Confirmation::RemoveDeck { name, .. } => (
                    "Remove deck?",
                    format!("{name} and every card below it will be deleted."),
                    "Remove deck",
                    ButtonTone::Danger,
                ),
                Confirmation::Reload => (
                    "Reload collection?",
                    String::from(
                        "This ends the current review session and loads the latest collection.",
                    ),
                    "Reload",
                    ButtonTone::Primary,
                ),
            };
            let cancel = self.action_click(UiAction::CancelDialog);
            layers.push(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(crate::theme::alpha(colors.crust, 0.7))
                    .occlude()
                    .key_context("ranki_dialog")
                    .track_focus(&self.dialog_focus)
                    .on_action(cx.listener(
                        |this, _: &crate::app::ConfirmDialog, window, cx| {
                            this.dispatch(UiAction::Confirm, window, cx);
                        },
                    ))
                    .on_action(cx.listener(
                        |this, _: &crate::app::CancelDialog, window, cx| {
                            this.dispatch(UiAction::CancelDialog, window, cx);
                        },
                    ))
                    .child(
                        panel(colors)
                            .w(px(modal_width))
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .text_size(px(14.))
                                    .text_color(colors.subtext)
                                    .child(detail),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .justify_end()
                                    .gap(px(SPACE_GROUP))
                                    .child(secondary_button(
                                        "Cancel", colors, cancel,
                                    ))
                                    .child(action(
                                        "confirm-primary",
                                        accept,
                                        tone,
                                        style,
                                        UiAction::Confirm,
                                        cx,
                                    )),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if self.help_open {
            let close = self.action_click(UiAction::ToggleHelp);
            let help_width = (viewport_width - 32.0).clamp(0.0, 300.0);
            let help_height = (viewport_height - 32.0).max(0.0);
            let grade_keys = [
                ranki_model::Rating::Again,
                ranki_model::Rating::Hard,
                ranki_model::Rating::Good,
                ranki_model::Rating::Easy,
            ]
            .into_iter()
            .map(|rating| {
                crate::app::display_answer_key(
                    self.options.study.answer_key(rating),
                )
            })
            .filter(|key| !key.is_empty())
            .collect::<Vec<_>>()
            .join(" / ");
            layers.push(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(crate::theme::alpha(colors.crust, 0.7))
                    .occlude()
                    .id("help-backdrop")
                    .debug_selector(|| "help-backdrop".to_owned())
                    .key_context("ranki_dialog")
                    .on_action(cx.listener(
                        |this, _: &crate::app::CancelDialog, window, cx| {
                            if this.help_open {
                                this.help_open = false;
                                let focus = this.focus.clone();
                                window.focus(&focus);
                                cx.defer_in(window, move |_, window, _| {
                                    window.focus(&focus);
                                });
                                cx.notify();
                            }
                        },
                    ))
                    .on_action(cx.listener(
                        |this, _: &crate::app::ConfirmDialog, window, cx| {
                            if this.help_open {
                                this.help_open = false;
                                let focus = this.focus.clone();
                                window.focus(&focus);
                                cx.defer_in(window, move |_, window, _| {
                                    window.focus(&focus);
                                });
                                cx.notify();
                            }
                        },
                    ))
                    .child(
                        div()
                            .id("help-modal")
                            .w(px(help_width))
                            .max_h(px(help_height))
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_INLINE))
                            .p(px(SPACE_RELATED))
                            .rounded(px(18.))
                            .bg(colors.mantle)
                            .border_1()
                            .border_color(colors.surface0)
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(14.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Keyboard"),
                            )
                            .child(
                                div()
                                    .id("help-rows")
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap(px(SPACE_INLINE))
                                    .child(help_row(
                                        "Space",
                                        "Reveal / Good",
                                        colors,
                                    ))
                                    .child(help_row(
                                        grade_keys,
                                        "Grade card",
                                        colors,
                                    ))
                                    .child(help_row(
                                        "Ctrl Z",
                                        "Undo last grade",
                                        colors,
                                    ))
                                    .child(help_row("R", "Flip card", colors))
                                    .child(help_row("S", "Study again", colors))
                                    .child(help_row(
                                        "Esc",
                                        "End session",
                                        colors,
                                    ))
                                    .child(help_row("B", "Browse", colors))
                                    .child(help_row(",", "Settings", colors))
                                    .child(help_row("N", "Next deck", colors))
                                    .child(help_row(
                                        "F5",
                                        "Reload collection",
                                        colors,
                                    ))
                                    .child(help_row(
                                        "A / D",
                                        "Add card / Decks",
                                        colors,
                                    ))
                                    .child(help_row("T", "Statistics", colors))
                                    .child(help_row(
                                        "Ctrl/Cmd Enter",
                                        "Save note",
                                        colors,
                                    ))
                                    .child(help_row(
                                        "Tab",
                                        "Next control",
                                        colors,
                                    ))
                                    .child(help_row(
                                        "Shift Tab",
                                        "Previous control",
                                        colors,
                                    ))
                                    .child(help_row("?", "This sheet", colors)),
                            )
                            .child(
                                div().flex().flex_none().justify_end().child(
                                    Interaction::surface(colors)
                                        .apply(
                                            div()
                                                .id("help-close")
                                                .debug_selector(|| {
                                                    "help-close".to_owned()
                                                })
                                                .track_focus(&self.dialog_focus)
                                                .focusable()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .h(px(BUTTON_HEIGHT))
                                                .px(px(SPACE_RELATED))
                                                .rounded(px(BUTTON_RADIUS))
                                                .bg(colors.surface0)
                                                .border_1()
                                                .border_color(colors.surface1)
                                                .text_size(px(BUTTON_TEXT_SIZE))
                                                .font_weight(
                                                    gpui::FontWeight::SEMIBOLD,
                                                )
                                                .cursor_pointer(),
                                        )
                                        .on_click(close)
                                        .child("Close"),
                                ),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if self.scrollbar_drag.is_some() {
            layers.push(
                div()
                    .id("scrollbar-drag-catcher")
                    .debug_selector(|| String::from("scrollbar-drag-catcher"))
                    .absolute()
                    .inset_0()
                    .on_mouse_move(cx.listener(
                        |this: &mut Self, event: &MouseMoveEvent, window, _| {
                            this.drag_scrollbar(
                                f32::from(event.position.y),
                                window,
                            );
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this: &mut Self, _, _, cx| {
                            this.end_scrollbar_drag(cx);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this: &mut Self, _, _, cx| {
                            this.end_scrollbar_drag(cx);
                        }),
                    )
                    .into_any_element(),
            );
        }
        let toast_width = (viewport_width - 2.0 * (nav_width + 32.0)).max(0.0);
        if let Some(toast) = self.live_toast() {
            let (hue, icon_name, prefix) = match toast.kind {
                crate::app::ToastKind::Success => {
                    (colors.green, "icon-check", "Done")
                }
                crate::app::ToastKind::Error => {
                    (colors.red, "icon-bell", "Error")
                }
            };
            let dismiss = cx.listener(|root, _: &ClickEvent, _, cx| {
                root.clear_toast();
                cx.notify();
            });
            let body = div()
                .flex()
                .w_full()
                .min_w(px(0.))
                .items_center()
                .gap(px(SPACE_GROUP))
                .px(px(SPACE_RELATED))
                .py(px(SPACE_RELATED))
                .rounded(px(12.))
                .bg(crate::theme::alpha(hue, 0.16))
                .border_1()
                .border_color(crate::theme::alpha(hue, 0.4))
                .text_size(px(13.))
                .text_color(colors.text)
                .child(icon(icon_name, 16., hue))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(hue)
                        .child(prefix),
                )
                .child(div().flex_1().min_w(px(0.)).child(toast.text.clone()))
                .child(
                    Interaction::surface(colors)
                        .apply(
                            div()
                                .id("toast-dismiss")
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(24.))
                                .h(px(24.))
                                .flex_none()
                                .rounded(px(6.))
                                .cursor_pointer(),
                        )
                        .on_click(dismiss)
                        .tooltip(label_tooltip("Dismiss", colors))
                        .child(icon("icon-x", 14., colors.subtext)),
                );
            layers.push(
                div()
                    .absolute()
                    .bottom(px(72.))
                    .left(px(nav_width + 32.))
                    .right(px(nav_width + 32.))
                    .w(px(toast_width))
                    .child(crate::components::fade_in(
                        body,
                        ("toast", usize::try_from(toast.seq).unwrap_or(0)),
                        self.motion_ms(crate::theme::MOTION_QUICK_MS),
                    ))
                    .into_any_element(),
            );
        }

        layers
    }

    pub(crate) fn action_click(&self, action: UiAction) -> ClickHandler {
        let focus = self.focus.clone();
        Box::new(move |_event: &ClickEvent, window: &mut Window, cx| {
            window.focus(&focus);
            crate::app::dispatch_app_action(window, cx, action.clone());
        })
    }
}

fn action(
    id: &'static str,
    label: &'static str,
    tone: ButtonTone,
    style: Style,
    action: UiAction,
    cx: &Context<Root>,
) -> impl IntoElement {
    action_button(
        id,
        label,
        tone,
        style,
        Box::new(cx.listener(move |_, _: &ClickEvent, window, cx| {
            crate::app::dispatch_app_action(window, cx, action.clone());
        })),
    )
}

fn help_row(
    key: impl Into<SharedString>,
    label: &'static str,
    colors: crate::theme::Palette,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_GROUP))
        .text_size(px(12.))
        .child(keycap(key, colors, false))
        .child(div().text_color(colors.subtext).child(label))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AppContext as _, point, size};

    use crate::{Options, app::View};

    #[gpui::test]
    fn help_modal_focuses_close_and_escape_dismisses(
        cx: &mut gpui::TestAppContext,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("collection.anki2");
        let store = Box::new(
            ranki_storage::Collection::create(&path).expect("collection"),
        );
        let options = Options {
            paths: ranki_model::DataPaths::new(dir.path()),
            ..Options::default()
        };
        let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
        cx.update(|cx| cx.bind_keys(crate::app::key_bindings()));
        let (root, cx) = cx.add_window_view(|window, cx| {
            super::Root::new(
                crate::app::Launch {
                    store,
                    options,
                    save_settings,
                },
                window,
                cx,
            )
        });
        let viewport = size(gpui::px(720.), gpui::px(540.));
        cx.simulate_resize(viewport);
        cx.update_entity(&root, |root, cx| {
            root.view = View::Review;
            root.help_open = true;
            cx.notify();
        });
        cx.draw(point(gpui::px(0.), gpui::px(0.)), viewport, |_, _| {
            root.clone()
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();

        let backdrop = cx
            .debug_bounds("help-backdrop")
            .expect("help backdrop bounds");
        assert_eq!(backdrop.size.width, gpui::px(720.));
        assert_eq!(backdrop.size.height, gpui::px(540.));
        assert!(cx.debug_bounds("help-close").is_some());

        let focus = cx.read_entity(&root, |root, _| root.dialog_focus.clone());
        cx.update(|window, _| assert!(focus.is_focused(window)));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!cx.read_entity(&root, |root, _| root.help_open));
    }
}
