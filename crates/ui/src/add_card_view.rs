//! Add-card screen: setup, dynamic fields, tags, live preview, and footer.

use std::collections::BTreeMap;

use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, TextOverflow, Window, div,
    img, px, relative,
};

use crate::{
    app::{
        AcceptTagSuggestion, ApplyCustomColor, DismissTagSuggestions, Root,
        UiAction, dispatch_app_action, note_type_name, resolve_model,
    },
    card_view::{self, Side},
    components::{
        ClickHandler, Interaction, icon, keycap, removable_chip, same_press,
        search_field_icon,
    },
    occlusion::{self, OcclusionMask},
    text_field::TextField,
    theme::{
        ADD_CONTENT_WIDTH, ADD_FORM_WIDTH, ADD_GAP, PAGE_X, PREVIEW_WIDTH,
        SPACE_BLOCK, SPACE_GROUP, SPACE_GUTTER, SPACE_INLINE, SPACE_RELATED,
        SPACE_SECTION, Style, alpha, card_colors, page_width, parse_hex_color,
    },
};

/// Height of the occlusion drawing area.
const OCCLUSION_CANVAS_HEIGHT: f32 = 320.0;

/// Which CSS property a color choice edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTarget {
    Text,
    Background,
}

impl ColorTarget {
    const fn property(self) -> &'static str {
        match self {
            Self::Text => "color",
            Self::Background => "background-color",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Text => "Text color",
            Self::Background => "Background color",
        }
    }
}

/// Open state of the card color picker.
#[derive(Clone, Debug)]
pub struct ColorPicker {
    pub target: ColorTarget,
    pub field: Entity<TextField>,
    pub anchor: Point<Pixels>,
}

/// Markup that colors a selection in a note field.
#[must_use]
pub fn color_tags(target: ColorTarget, value: &str) -> (String, String) {
    (
        format!("<span style=\"{}: {value}\">", target.property()),
        String::from("</span>"),
    )
}

impl Root {
    pub(crate) fn render_add_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = self.colors();
        let style = Style::new(colors, self.theme());
        let decks = &self.decks;
        let models = &self.models;
        let fields = &self.add.fields;
        let tags_field = self.add.tags.clone();
        let tag_chips = self.add.tag_chips.clone();
        let errors = &self.add.errors;
        let model = resolve_model(models, self.add.model_index);
        let occlusion_mode =
            model.is_some_and(ranki_model::NoteModel::is_image_occlusion);
        let tabs = if self.card_edit.is_some() {
            model
                .map(|model| {
                    vec![(note_type_name(model), self.add.model_index)]
                })
                .unwrap_or_default()
        } else {
            note_type_tabs(models)
        };
        let save: ClickHandler =
            Box::new(|_: &ClickEvent, window: &mut Window, cx: &mut App| {
                dispatch_app_action(window, cx, UiAction::SaveNote);
            });
        let content_width =
            page_width(f32::from(window.viewport_size().width), self.nav_width);
        let side_by_side = content_width >= 900.;

        let screen = div()
            .size_full()
            .id("add-scroll")
            .flex()
            .flex_col()
            .overflow_scroll()
            .track_scroll(&self.add_scroll)
            .on_scroll_wheel(cx.listener(
                |this, _: &gpui::ScrollWheelEvent, window, cx| {
                    if this.add.deck_picker {
                        this.add.deck_picker = false;
                        cx.notify();
                    }
                    this.close_color_picker(window, cx);
                },
            ))
            .px(px(PAGE_X))
            .pt(px(SPACE_GUTTER));
        let mut column = div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(ADD_CONTENT_WIDTH));

        column = column
            .child(
                div().flex().items_center().child(
                    div()
                        .text_size(px(28.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(if self.card_edit.is_some() { "Edit card" } else { "New card" }),
                ),
            )
            .child(div().h(px(20.)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(SPACE_BLOCK))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_INLINE))
                            .flex_1()
                            .min_w(px(180.))
                            .max_w(px(340.))
                            .p(px(SPACE_RELATED))
                            .px(px(SPACE_RELATED))
                            .rounded(px(14.))
                            .bg(colors.surface0)
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.subtext)
                                    .child("DECK"),
                            )
                            .child(
                                Interaction::raised(colors)
                                    .apply(
                                        div()
                                            .id("add-deck-picker")
                                            .debug_selector(|| {
                                                "add-deck-picker".to_owned()
                                            })
                                            .flex()
                                            .items_center()
                                            .gap(px(SPACE_GROUP))
                                            .text_size(px(14.))
                                            .font_weight(
                                                gpui::FontWeight::SEMIBOLD,
                                            )
                                            .cursor_pointer(),
                                    )
                                    .on_click(cx.listener(
                                        |this, event: &ClickEvent, window, cx| {
                                            let same_press = same_press(
                                                event,
                                                this.add
                                                    .deck_picker_dismissed_press
                                                    .as_ref(),
                                            );
                                            this.add
                                                .deck_picker_dismissed_press =
                                                None;
                                            if !same_press {
                                                this.toggle_add_deck_picker(
                                                    event.position(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                        },
                                    ))
                                    .child(
                                        self.add_deck_index()
                                            .and_then(|index| decks.get(index))
                                            .map_or_else(
                                                || {
                                                    String::from(
                                                        "Choose a deck",
                                                    )
                                                },
                                                |deck| {
                                                    deck.name
                                                        .replace("::", " › ")
                                                },
                                            ),
                                    )
                                    .child(icon(
                                        "icon-chevron-down",
                                        16.,
                                        colors.subtext,
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .min_w(px(0.))
                            .gap(px(SPACE_INLINE))
                            .p(px(SPACE_INLINE))
                            .rounded(px(12.))
                            .bg(colors.surface0)
                            .children(tabs.into_iter().map(
                                |(label, index)| {
                                    let selected =
                                        index == self.add.model_index;
                                    Interaction::tinted(colors, colors.mauve)
                                        .apply(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "add-model-tab-{index}"
                                                )))
                                                .h(px(32.))
                                                .px(px(SPACE_RELATED))
                                                .flex()
                                                .items_center()
                                                .rounded(px(9.))
                                                .bg(if selected {
                                                    alpha(colors.mauve, 0.22)
                                                } else {
                                                    alpha(colors.surface0, 0.0)
                                                })
                                                .text_size(px(13.))
                                                .font_weight(
                                                    gpui::FontWeight::SEMIBOLD,
                                                )
                                                .text_color(if selected {
                                                    colors.mauve
                                                } else {
                                                    colors.subtext
                                                })
                                                .cursor_pointer(),
                                        )
                                        .on_click(cx.listener(
                                            move |this,
                                                  _: &ClickEvent,
                                                  _,
                                                  cx| {
                                                this.select_model(index, cx);
                                            },
                                        ))
                                        .child(label)
                                },
                            )),
                    ),
            );

        if let Some(error) = errors.get("deck") {
            column = column.child(validation(error, colors));
        }
        if let Some(error) = errors.get("model") {
            column = column.child(validation(error, colors));
        }

        let is_cloze = model
            .is_some_and(|model| model.kind == ranki_model::NoteKind::Cloze);
        let mut left = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_BLOCK))
            .min_w(px(0.));
        if occlusion_mode {
            let editor = self.render_occlusion_editor(colors, errors, cx);
            left = left.child(editor);
        }
        for (index, field) in fields
            .iter()
            .enumerate()
            .filter(|(index, _)| !occlusion_mode || *index >= 2)
        {
            let name = model
                .and_then(|model| model.fields.get(index).cloned())
                .unwrap_or_else(|| format!("Field {}", index + 1));
            let focused = field.read(cx).focus_handle().is_focused(window);
            let field_error = errors.get(&format!("field:{index}"));
            let invalid =
                errors.contains_key("fields") || field_error.is_some();
            let border = if invalid {
                colors.red
            } else if focused {
                colors.mauve
            } else {
                colors.surface1
            };
            let mut field_box = div()
                .flex()
                .flex_col()
                .px(px(SPACE_BLOCK))
                .py(px(SPACE_RELATED))
                .rounded(px(16.))
                .border_1()
                .border_color(border)
                .bg(if focused {
                    colors.surface0
                } else {
                    colors.base
                })
                .child(
                    div()
                        .min_h(px(56.))
                        .text_size(px(26.))
                        .child(field.clone()),
                );
            if index == 0 {
                field_box = field_box.child(formatting_toolbar(
                    field.clone(),
                    is_cloze,
                    colors,
                    cx,
                ));
            }
            if let Some(error) = field_error {
                field_box =
                    field_box.child(validation(error, colors).debug_selector(
                        move || format!("add-field-error-{index}"),
                    ));
            }
            left = left.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_GROUP))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.subtext)
                            .child(name),
                    )
                    .child(field_box),
            );
        }
        if let Some(error) = errors.get("fields") {
            left = left.child(validation(error, colors));
        }
        let tags_handle = tags_field.read(cx).focus_handle().clone();
        let suggestions = if self.add.tag_suggestions_dismissed {
            Vec::new()
        } else {
            self.tag_suggestions(cx)
        };
        let selected_suggestion = self
            .add
            .tag_suggestion
            .min(suggestions.len().saturating_sub(1));
        let key_suggestions = suggestions.len();
        let mut tags_row = div()
            .id("add-tags")
            .debug_selector(|| "add-tags".to_owned())
            .key_context("ranki_tag_input")
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(SPACE_GROUP))
            .min_h(px(46.))
            .px(px(SPACE_RELATED))
            .py(px(SPACE_GROUP))
            .rounded(px(12.))
            .border_1()
            .border_color(colors.surface1)
            .on_key_down(cx.listener(
                move |this, event: &gpui::KeyDownEvent, _window, cx| {
                    if key_suggestions == 0 {
                        return;
                    }
                    let last = key_suggestions.saturating_sub(1);
                    let key = event.keystroke.key.as_str();
                    if key.eq_ignore_ascii_case("down") {
                        this.add.tag_suggestions_dismissed = false;
                        this.add.tag_suggestion =
                            this.add.tag_suggestion.saturating_add(1).min(last);
                        cx.notify();
                        cx.stop_propagation();
                    } else if key.eq_ignore_ascii_case("up") {
                        this.add.tag_suggestions_dismissed = false;
                        this.add.tag_suggestion =
                            this.add.tag_suggestion.saturating_sub(1);
                        cx.notify();
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(
                move |this, _: &AcceptTagSuggestion, window, cx| {
                    if key_suggestions == 0
                        || this.add.tag_suggestions_dismissed
                    {
                        return;
                    }
                    let index =
                        this.add.tag_suggestion.min(key_suggestions - 1);
                    this.choose_tag_suggestion(index, window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, _: &DismissTagSuggestions, _, cx| {
                    this.add.tag_suggestions_dismissed = true;
                    cx.notify();
                },
            ));
        let weak = cx.entity().downgrade();
        for tag in &tag_chips {
            let tag = tag.clone();
            let handle = tags_handle.clone();
            let weak = weak.clone();
            tags_row = tags_row.child(removable_chip(
                tag.clone(),
                colors,
                Box::new(move |_, window, cx: &mut App| {
                    let tag = tag.clone();
                    let _ = weak.update(cx, |root, cx| {
                        root.add.tag_chips.retain(|known| *known != tag);
                        cx.notify();
                    });
                    window.focus(&handle);
                    cx.stop_propagation();
                }),
            ));
        }
        let suggestion_popup = if suggestions.is_empty() {
            None
        } else {
            Some(Self::render_tag_suggestions(
                suggestions,
                selected_suggestion,
                colors,
                cx,
            ))
        };
        let tag_input = tags_field;
        let click_handle = tags_handle;
        let tags_group = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_GROUP))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.subtext)
                    .child("Tags"),
            )
            .child(
                div()
                    .relative()
                    .child(
                        tags_row
                            .hover(move |style| style.bg(colors.surface0))
                            .on_click(cx.listener(
                                move |_, _: &ClickEvent, window, _| {
                                    window.focus(&click_handle);
                                },
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(SPACE_GROUP))
                                    .min_w(px(140.))
                                    .child(icon("icon-tag", 14., colors.mauve))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(80.))
                                            .child(tag_input),
                                    ),
                            ),
                    )
                    .children(suggestion_popup),
            );

        let preview = if occlusion_mode {
            None
        } else {
            let field_values = fields
                .iter()
                .map(|field| field.read(cx).text())
                .collect::<Vec<_>>();
            let has_content = has_preview_content(&field_values);
            let rendered_question = model.map(|model| {
                card_view::preview_rendered(
                    model,
                    &field_values,
                    Side::Question,
                )
            });
            let rendered_answer = model.map(|model| {
                card_view::preview_rendered(model, &field_values, Side::Answer)
            });
            let (question_media, answer_media) = preview_media(
                rendered_question.as_ref(),
                rendered_answer.as_ref(),
            );
            let preview_question = rendered_question.as_ref().map_or_else(
                || {
                    div()
                        .text_size(px(52.))
                        .text_color(colors.text)
                        .child("Choose a note type")
                },
                |card| card_view::preview_line(card, colors.text, 52.),
            );
            let preview_answer =
                rendered_answer.as_ref().map_or_else(div, |card| {
                    card_view::preview_line(card, colors.mauve, 28.)
                        .font_weight(gpui::FontWeight::BOLD)
                });
            let preview_reading = model.and_then(|model| {
                field_value(model, &field_values, &["Reading", "Kana"])
            });
            let preview_example = model.and_then(|model| {
                model
                    .fields
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index >= 2)
                    .find_map(|(index, _)| {
                        field_values
                            .get(index)
                            .map(|value| value.trim())
                            .filter(|value| !value.is_empty())
                            .map(ToOwned::to_owned)
                    })
            });
            let mut preview = div()
                .flex()
                .flex_col()
                .gap(px(SPACE_BLOCK))
                .p(px(SPACE_SECTION))
                .min_h(px(300.))
                .rounded(px(24.))
                .bg(colors.surface0);
            if side_by_side {
                preview = preview.w(px(PREVIEW_WIDTH)).flex_none();
            } else {
                preview = preview.w_full();
            }
            Some(preview.child(if has_content {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(SPACE_GROUP))
                    .child(preview_question)
                    .children(preview_reading.map(|reading| {
                        div()
                            .text_size(px(14.))
                            .text_color(colors.subtext)
                            .font_family(crate::theme::font_for_text(&reading))
                            .child(reading)
                    }))
                    .children(card_view::media_column(
                        &question_media,
                        &self.options.paths.media_dir,
                        colors,
                        120.,
                    ))
                    .child(div().w_full().h(px(1.)).bg(colors.surface1))
                    .child(preview_answer)
                    .children(card_view::media_column(
                        &answer_media,
                        &self.options.paths.media_dir,
                        colors,
                        120.,
                    ))
                    .children(preview_example.map(|example| {
                        div()
                            .w_full()
                            .px(px(SPACE_RELATED))
                            .py(px(SPACE_RELATED))
                            .rounded(px(14.))
                            .bg(colors.base)
                            .text_size(px(14.))
                            .font_family(crate::theme::font_for_text(&example))
                            .child(example)
                    }))
                    .into_any_element()
            } else {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .min_h(px(160.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(colors.overlay)
                            .child("Preview appears as you type"),
                    )
                    .into_any_element()
            }))
        };

        let mut form =
            div().mt(px(SPACE_SECTION)).flex().w_full().gap(px(ADD_GAP));
        if side_by_side && !occlusion_mode {
            let editor = div()
                .flex_1()
                .min_w(px(ADD_FORM_WIDTH * 0.5))
                .flex()
                .flex_col()
                .gap(px(SPACE_BLOCK))
                .child(left)
                .child(tags_group);
            form = form.items_start().child(editor);
            if let Some(preview) = preview {
                form = form.child(
                    div().w(px(PREVIEW_WIDTH)).flex_none().child(preview),
                );
            }
        } else {
            form = form
                .flex_col()
                .gap(px(SPACE_BLOCK))
                .child(left)
                .child(tags_group);
            if let Some(preview) = preview {
                form = form.child(preview);
            }
        }
        column = column.child(form);

        column = column.child(
            div()
                .mt(px(SPACE_SECTION))
                .mb(px(SPACE_SECTION))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SPACE_BLOCK))
                .pt(px(SPACE_BLOCK))
                .border_t_1()
                .border_color(colors.surface1)
                .child(div().flex_1())
                .child(
                    Interaction::accent(style)
                        .apply(
                            div()
                                .id("add-save")
                                .debug_selector(|| "add-save".to_owned())
                                .h(px(48.))
                                .px(px(SPACE_SECTION))
                                .flex()
                                .items_center()
                                .gap(px(SPACE_GROUP))
                                .rounded(px(14.))
                                .bg(colors.mauve)
                                .text_color(style.ink())
                                .text_size(px(15.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .cursor_pointer(),
                        )
                        .on_click(save)
                        .child(icon("icon-plus", 18., style.ink()))
                        .child(if self.card_edit.is_some() {
                            "Save changes"
                        } else {
                            "Add card"
                        })
                        .child(keycap("Ctrl+Enter", colors, true)),
                ),
        );

        screen.child(div().flex().w_full().justify_center().child(column))
    }

    /// Popup list of tags already used in the collection.
    fn render_tag_suggestions(
        suggestions: Vec<String>,
        selected: usize,
        colors: crate::theme::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut list = div().flex().flex_col().gap(px(SPACE_INLINE));
        for (index, tag) in suggestions.iter().enumerate() {
            let active = index == selected;
            list = list.child(
                div()
                    .id(SharedString::from(format!(
                        "add-tag-suggestion-{index}"
                    )))
                    .debug_selector(move || {
                        format!("add-tag-suggestion-{index}")
                    })
                    .h(px(32.))
                    .flex()
                    .items_center()
                    .px(px(SPACE_RELATED))
                    .rounded(px(8.))
                    .bg(if active {
                        alpha(colors.mauve, 0.2)
                    } else {
                        colors.base
                    })
                    .text_color(if active { colors.mauve } else { colors.text })
                    .text_size(px(13.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_tag_suggestion(index, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(tag.clone()),
            );
        }
        div()
            .id("add-tag-suggestions-popup")
            .debug_selector(|| "add-tag-suggestions-popup".to_owned())
            .absolute()
            .left(px(0.))
            .top(relative(1.0))
            .mt(px(SPACE_INLINE))
            .w(px(280.))
            .occlude()
            .on_mouse_down_out(cx.listener(
                |this, _: &MouseDownEvent, _, cx| {
                    this.add.tag_suggestions_dismissed = true;
                    cx.notify();
                },
            ))
            .flex()
            .flex_col()
            .gap(px(SPACE_INLINE))
            .p(px(SPACE_INLINE))
            .rounded(px(12.))
            .border_1()
            .border_color(colors.surface1)
            .bg(colors.mantle)
            .child(list)
    }

    /// Image-occlusion editor: upload control and drawing area.
    fn render_occlusion_editor(
        &self,
        colors: crate::theme::Palette,
        errors: &BTreeMap<String, String>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let image = self.add.occlusion.image.clone();
        let masks = self.add.occlusion.masks.clone();
        let dragging = self.add.occlusion.drag_rect();

        let image_button = {
            let has_image = image.is_some();
            let hue = if has_image { colors.red } else { colors.mauve };
            let handler: ClickHandler = if has_image {
                Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.remove_occlusion_image(cx);
                }))
            } else {
                Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.upload_occlusion_image(cx);
                }))
            };
            Interaction::new(
                alpha(hue, 0.3),
                alpha(hue, 0.22),
                alpha(hue, 0.34),
                hue,
            )
            .apply(
                div()
                    .id("io-image")
                    .debug_selector(|| "io-image".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_GROUP))
                    .h(px(32.))
                    .px(px(SPACE_RELATED))
                    .rounded(px(10.))
                    .bg(alpha(hue, 0.14))
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hue)
                    .cursor_pointer()
                    .child(icon(
                        if has_image { "icon-x" } else { "icon-upload" },
                        14.,
                        hue,
                    ))
                    .child(if has_image {
                        "Remove image"
                    } else {
                        "Upload image"
                    }),
            )
            .on_click(handler)
        };

        let mut canvas = div()
            .id("io-canvas")
            .relative()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .h(px(OCCLUSION_CANVAS_HEIGHT))
            .rounded(px(16.))
            .overflow_hidden()
            .border_1()
            .border_color(if errors.contains_key("masks") {
                colors.red
            } else {
                colors.surface1
            })
            .bg(colors.mantle);
        if let Some(path) = image.as_deref().and_then(|name| {
            card_view::media_path(&self.options.paths.media_dir, name)
        }) {
            let mut fitted_image = img(path)
                .h(px(OCCLUSION_CANVAS_HEIGHT))
                .object_fit(gpui::ObjectFit::Contain);
            if let Some((width, height)) = self.add.occlusion.image_size {
                let width = OCCLUSION_CANVAS_HEIGHT * width / height;
                let width = self.add.occlusion.bounds.map_or(width, |bounds| {
                    width.min(f32::from(bounds.size.width))
                });
                fitted_image = fitted_image.w(px(width));
            } else {
                fitted_image = fitted_image.w_full();
            }
            canvas = canvas.child(fitted_image);
        } else {
            canvas = canvas.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.))
                    .text_color(colors.overlay)
                    .child("Upload an image, then drag to draw masks"),
            );
        }
        let fitted = self.add.occlusion.image_rect().unwrap_or(
            occlusion::NormalizedRect {
                left: 0.0,
                top: 0.0,
                width: 1.0,
                height: 1.0,
            },
        );
        let mut overlay = div()
            .absolute()
            .left(relative(fitted.left))
            .top(relative(fitted.top))
            .w(relative(fitted.width))
            .h(relative(fitted.height))
            .id("io-overlay");
        for (index, mask) in masks.iter().enumerate() {
            let remove: ClickHandler =
                Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.remove_occlusion_mask(index, cx);
                    cx.stop_propagation();
                }));
            overlay = overlay.child(mask_box(
                mask,
                index.saturating_add(1),
                colors,
                remove,
                cx,
            ));
        }
        if let Some(rect) = dragging {
            overlay = overlay.child(
                div()
                    .absolute()
                    .left(relative(rect.left))
                    .top(relative(rect.top))
                    .w(relative(rect.width))
                    .h(relative(rect.height))
                    .border_1()
                    .border_dashed()
                    .border_color(colors.mauve),
            );
        }
        canvas = canvas
            .child(overlay)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.occlusion_press(
                        pointer(event.position),
                        MouseButton::Left,
                        cx,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.occlusion_press(
                        pointer(event.position),
                        MouseButton::Right,
                        cx,
                    );
                }),
            )
            .on_mouse_move(cx.listener(
                move |this, event: &MouseMoveEvent, _, cx| {
                    this.occlusion_drag_to(pointer(event.position), cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.occlusion_release(cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.occlusion_release(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.cancel_occlusion_drag(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Right,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.cancel_occlusion_drag(cx);
                }),
            );

        let weak = cx.entity().downgrade();
        let canvas = div()
            .w_full()
            .on_children_prepainted(move |children, _, cx| {
                if let Some(first) = children.first() {
                    let _ = weak.update(cx, |root, cx| {
                        root.set_occlusion_bounds(*first, cx);
                    });
                }
            })
            .child(canvas);

        let mut editor = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_RELATED))
            .p(px(SPACE_BLOCK))
            .rounded(px(18.))
            .border_1()
            .border_color(colors.surface1)
            .bg(colors.surface0)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_GROUP))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.subtext)
                            .child("OCCLUSION EDITOR"),
                    )
                    .child(div().flex_1())
                    .child(
                        Interaction::new(
                            alpha(colors.mauve, 0.3),
                            alpha(colors.mauve, 0.22),
                            alpha(colors.mauve, 0.34),
                            colors.mauve,
                        )
                        .apply(
                            div()
                                .id("io-add-mask")
                                .debug_selector(|| "io-add-mask".to_owned())
                                .flex()
                                .items_center()
                                .gap(px(SPACE_GROUP))
                                .h(px(32.))
                                .px(px(SPACE_RELATED))
                                .rounded(px(10.))
                                .bg(alpha(colors.mauve, 0.14))
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(colors.mauve)
                                .cursor_pointer()
                                .child(icon("icon-plus", 14., colors.mauve))
                                .child("Add mask"),
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.add_default_occlusion_mask(cx);
                            },
                        )),
                    )
                    .child(image_button),
            )
            .child(canvas);
        editor = editor.child(
            div()
                .debug_selector(|| "io-occlusion-hint".to_owned())
                .text_size(px(11.))
                .text_color(colors.overlay)
                .child(
                    "Drag to draw or move. Right-drag a mask to resize. \
                     Ctrl+M adds a mask, Ctrl+Shift+M removes the last one.",
                ),
        );
        if let Some(error) = errors.get("occlusion") {
            editor = editor.child(validation(error, colors));
        }
        if let Some(error) = errors.get("masks") {
            editor = editor.child(validation(error, colors));
        }
        editor
    }
}
impl Root {
    pub(crate) fn render_deck_picker(
        &self,
        colors: crate::theme::Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let matching = self.add_deck_matches(cx);
        let filter = self.add.deck_filter.clone();
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_INLINE))
            .max_h(px(220.))
            .id("add-deck-list")
            .overflow_scroll()
            .track_scroll(&self.deck_picker_scroll)
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        let mut visible = 0;
        for index in matching {
            let deck = &self.decks[index];
            visible += 1;
            let selected = self.add.deck_id == Some(deck.id);
            let due = self.stats.get(index).map_or(0, |stats| {
                stats
                    .new_cards
                    .saturating_add(stats.learning_cards)
                    .saturating_add(stats.review_cards)
            });
            let (hover, press) = if selected {
                (alpha(colors.mauve, 0.28), alpha(colors.mauve, 0.4))
            } else {
                (colors.surface0, colors.surface1)
            };
            list = list.child(
                Interaction::new(
                    alpha(colors.text, 0.0),
                    hover,
                    press,
                    colors.mauve,
                )
                .apply(
                    div()
                        .id(SharedString::from(format!("add-deck-{index}")))
                        .debug_selector(move || format!("add-deck-{index}"))
                        .flex()
                        .items_center()
                        .justify_between()
                        .h(px(38.))
                        .flex_none()
                        .px(px(SPACE_RELATED))
                        .rounded(px(10.))
                        .bg(if selected {
                            alpha(colors.mauve, 0.18)
                        } else {
                            colors.base
                        })
                        .text_color(if selected {
                            colors.mauve
                        } else {
                            colors.text
                        })
                        .cursor_pointer(),
                )
                .on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.select_add_deck(index, cx);
                        window.focus(&this.focus);
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_GROUP))
                        .child(icon("icon-book", 16., colors.subtext))
                        .child(
                            div()
                                .debug_selector(|| {
                                    "add-deck-picker-name".to_owned()
                                })
                                .w_full()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .text_overflow(TextOverflow::Truncate(
                                    "...".into(),
                                ))
                                .child(deck.name.replace("::", " › ")),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "add-deck-picker-due".to_owned())
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(colors.subtext)
                        .child(format!("{due} due")),
                ),
            );
        }
        if visible == 0 {
            list = list.child(
                div()
                    .p(px(SPACE_RELATED))
                    .text_size(px(13.))
                    .text_color(colors.overlay)
                    .child("No decks match"),
            );
        }
        div()
            .id("add-deck-picker-popup")
            .debug_selector(|| "add-deck-picker-popup".to_owned())
            .w(px(320.))
            .occlude()
            .on_mouse_down_out(cx.listener(
                |this, event: &MouseDownEvent, window, cx| {
                    this.add.deck_picker = false;
                    this.add.deck_picker_dismissed_press = Some(event.clone());
                    window.focus(&this.focus);
                    cx.notify();
                },
            ))
            .flex()
            .flex_col()
            .gap(px(SPACE_GROUP))
            .p(px(SPACE_RELATED))
            .rounded(px(16.))
            .border_1()
            .border_color(colors.surface1)
            .bg(colors.mantle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_GROUP))
                    .h(px(38.))
                    .px(px(SPACE_RELATED))
                    .rounded(px(10.))
                    .bg(colors.base)
                    .id("add-deck-search")
                    .on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| {
                            let field = this.add.deck_filter.clone();
                            window.focus(field.read(cx).focus_handle());
                        },
                    ))
                    .child(search_field_icon(
                        self.add.deck_filter.read(cx).focus_handle(),
                        15.,
                        colors,
                        window,
                    ))
                    .child(div().flex_1().min_w(px(120.)).child(filter)),
            )
            .child(list)
    }

    pub(crate) fn render_color_picker(
        &self,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let colors = self.colors();
        let theme = self.theme();
        let style = Style::new(colors, theme);
        let Some(picker) = self.add.color_picker.as_ref() else {
            return div().into_any_element();
        };
        let mut grid = div().flex().flex_wrap().gap(px(SPACE_INLINE));
        for (index, swatch) in card_colors(theme).iter().enumerate() {
            let preview = match swatch.color {
                Some(color) => div()
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(8.))
                    .bg(color)
                    .border_1()
                    .border_color(alpha(colors.text, 0.16)),
                None => div()
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(8.))
                    .bg(colors.base)
                    .border_1()
                    .border_color(colors.surface1)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("icon-x", 14., colors.overlay)),
            };
            let target = picker.target;
            let field = picker.field.clone();
            let value = swatch.hex;
            let name = swatch.name;
            grid = grid.child(
                Interaction::surface(colors)
                    .apply(
                        div()
                            .id(SharedString::from(format!(
                                "add-color-{index}"
                            )))
                            .debug_selector(move || {
                                format!("add-color-{index}")
                            })
                            .w(px(60.))
                            .py(px(SPACE_INLINE))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(SPACE_INLINE))
                            .rounded(px(8.))
                            .cursor_pointer(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.apply_color(
                            target,
                            field.clone(),
                            value,
                            window,
                            cx,
                        );
                    }))
                    .child(preview)
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(colors.subtext)
                            .child(name),
                    ),
            );
        }

        let custom = self.add.color_hex.read(cx).text();
        let custom_color = parse_hex_color(&custom);
        let custom_preview = custom_color.map_or_else(
            || {
                div()
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(8.))
                    .bg(colors.surface0)
                    .border_1()
                    .border_color(colors.surface1)
            },
            |color| div().w(px(30.)).h(px(30.)).rounded(px(8.)).bg(color),
        );
        let hint = if custom.trim().is_empty() {
            "Enter rrggbb (# optional)"
        } else if custom_color.is_some() {
            "Custom"
        } else {
            "Use 3 or 6 hex digits"
        };
        let target = picker.target;
        let field = self.add.color_hex.clone();
        let custom_field = picker.field.clone();
        let apply = Interaction::accent(style)
            .apply(
                div()
                    .id("add-color-apply")
                    .h(px(30.))
                    .px(px(SPACE_RELATED))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(colors.mauve)
                    .text_color(style.ink())
                    .text_size(px(12.))
                    .cursor_pointer(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                let value = this.add.color_hex.read(cx).text();
                if parse_hex_color(&value).is_some() {
                    this.apply_color(
                        target,
                        custom_field.clone(),
                        &value,
                        window,
                        cx,
                    );
                }
            }))
            .child("Apply");

        div()
            .id("add-color-picker-popup")
            .debug_selector(|| "add-color-picker-popup".to_owned())
            .key_context("ranki_color_input")
            .on_action(cx.listener(|this, _: &ApplyCustomColor, window, cx| {
                let value = this.add.color_hex.read(cx).text();
                if parse_hex_color(&value).is_none() {
                    return;
                }
                let Some(picker) = this.add.color_picker.as_ref() else {
                    return;
                };
                let (target, field) = (picker.target, picker.field.clone());
                this.apply_color(target, field, &value, window, cx);
            }))
            .w(px(276.))
            .occlude()
            .on_mouse_down_out(cx.listener(
                |this, event: &MouseDownEvent, window, cx| {
                    this.add.color_picker_dismissed_target =
                        this.add.color_picker.as_ref().map(|p| p.target);
                    this.add.color_picker = None;
                    this.add.color_picker_dismissed_press = Some(event.clone());
                    window.focus(&this.focus);
                    cx.notify();
                },
            ))
            .flex()
            .flex_col()
            .gap(px(SPACE_GROUP))
            .p(px(SPACE_RELATED))
            .rounded(px(16.))
            .border_1()
            .border_color(colors.surface1)
            .bg(colors.mantle)
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.text)
                    .child(picker.target.label()),
            )
            .child(grid)
            .child(
                div()
                    .pt(px(SPACE_GROUP))
                    .border_t_1()
                    .border_color(colors.surface1)
                    .flex()
                    .items_center()
                    .gap(px(SPACE_INLINE))
                    .child(custom_preview)
                    .child(div().flex_1().min_w(px(84.)).child(field))
                    .child(apply),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.overlay)
                    .child(hint),
            )
            .into_any_element()
    }

    pub(crate) fn apply_color(
        &mut self,
        target: ColorTarget,
        field: Entity<TextField>,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = field.read(cx).focus_handle().clone();
        let value = value.trim();
        let value =
            if value.starts_with('#') || parse_hex_color(value).is_none() {
                value.to_owned()
            } else {
                format!("#{value}")
            };
        field.update(cx, |field, cx| {
            let (open, close) = color_tags(target, &value);
            field.wrap_markup((&open, &close), cx);
        });
        self.close_color_picker(window, cx);
        window.focus(&focus);
    }
}

/// Label shown when hovering an icon-only formatting button.
struct TooltipLabel {
    text: SharedString,
    colors: crate::theme::Palette,
}

impl Render for TooltipLabel {
    fn render(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .px(px(SPACE_GROUP))
            .py(px(SPACE_INLINE))
            .rounded(px(6.))
            .border_1()
            .border_color(self.colors.surface1)
            .bg(self.colors.mantle)
            .text_size(px(12.))
            .text_color(self.colors.text)
            .child(self.text.clone())
    }
}

/// One drawn mask, translucent red with its cloze ordinal.
fn mask_box(
    mask: &OcclusionMask,
    ordinal: usize,
    colors: crate::theme::Palette,
    remove: ClickHandler,
    cx: &Context<Root>,
) -> gpui::Div {
    let mut shape = div()
        .absolute()
        .left(relative(mask.rect.left))
        .top(relative(mask.rect.top))
        .w(relative(mask.rect.width))
        .h(relative(mask.rect.height))
        .flex()
        .items_center()
        .justify_center()
        .bg(alpha(colors.red, 0.28))
        .border_1()
        .border_color(colors.red)
        .text_size(px(11.))
        .text_color(colors.crust)
        .child(format!("c{ordinal}"));
    shape = if mask.shape == crate::occlusion::MaskShape::Ellipse {
        shape.rounded_full()
    } else {
        shape.rounded(px(4.))
    };
    shape.child(
        div()
            .id(SharedString::from(format!("io-remove-mask-{ordinal}")))
            .debug_selector(move || format!("io-remove-mask-{ordinal}"))
            .absolute()
            .right(px(0.))
            .top(px(0.))
            .flex()
            .items_center()
            .justify_center()
            .h(px(18.))
            .w(px(18.))
            .rounded_full()
            .bg(colors.crust)
            .text_color(colors.red)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                }),
            )
            .on_click(remove)
            .child("×"),
    )
}

fn pointer(position: gpui::Point<Pixels>) -> (f32, f32) {
    (f32::from(position.x), f32::from(position.y))
}

/// True when any field carries text, which gates the preview side tabs.
#[must_use]
pub fn has_preview_content(values: &[String]) -> bool {
    values.iter().any(|value| !value.trim().is_empty())
}

/// Media shown beside the question and answer halves of the add-card preview.
fn preview_media(
    question: Option<&card_view::RenderedCard>,
    answer: Option<&card_view::RenderedCard>,
) -> (Vec<card_view::MediaRef>, Vec<card_view::MediaRef>) {
    let Some(question) = question else {
        return (Vec::new(), Vec::new());
    };
    let answer = answer.map_or_else(Vec::new, |answer| {
        card_view::answer_only_media(&question.media, &answer.media)
    });
    (question.media.clone(), answer)
}

/// Cloze buttons shown for cloze notes, in the order they appear.
const CLOZE_BUTTONS: [(&str, u32); 3] =
    [("[…] New c1", 1), ("[…] New c2", 2), ("[…] New c3", 3)];

fn cloze_tags(number: u32) -> (String, String) {
    (format!("{{{{c{number}::"), String::from("}}"))
}

/// Buttons that wrap the selected field text in Anki markup.
const MARKUP_BUTTONS: [(&str, &str, &str, bool, Option<&'static str>); 5] = [
    ("B", "<b>", "</b>", true, None),
    ("I", "<i>", "</i>", false, None),
    ("U", "<u>", "</u>", false, None),
    (
        "Code",
        "<pre><code>",
        "</code></pre>",
        false,
        Some("icon-code"),
    ),
    ("Math", "\\(", "\\)", false, Some("icon-math")),
];

/// Field formatting bar under the first field.
fn formatting_toolbar(
    field: gpui::Entity<crate::text_field::TextField>,
    is_cloze: bool,
    colors: crate::theme::Palette,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    let handle = field.read(cx).focus_handle().clone();
    let wrap = |tags: (&'static str, &'static str),
                field: gpui::Entity<crate::text_field::TextField>,
                handle: gpui::FocusHandle| {
        let handler: ClickHandler = Box::new(
            move |_event: &ClickEvent, window: &mut Window, cx: &mut App| {
                field.update(cx, |field, cx| {
                    field.wrap_markup(tags, cx);
                });
                window.focus(&handle);
            },
        );
        handler
    };
    let wrap_cloze = |number: u32,
                      field: gpui::Entity<crate::text_field::TextField>,
                      handle: gpui::FocusHandle| {
        let (open, close) = cloze_tags(number);
        let handler: ClickHandler = Box::new(
            move |_event: &ClickEvent, window: &mut Window, cx: &mut App| {
                field.update(cx, |field, cx| {
                    field.wrap_markup((&open, &close), cx);
                });
                window.focus(&handle);
            },
        );
        handler
    };
    let button = |label: &'static str,
                  icon_name: Option<&'static str>,
                  bold: bool,
                  on_click: ClickHandler| {
        let tooltip_label = label;
        let tooltip_colors = colors;
        let button = Interaction::surface(colors)
            .apply(
                div()
                    .id(SharedString::from(format!("add-format-{label}")))
                    .debug_selector(move || format!("add-format-{label}"))
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(SPACE_RELATED))
                    .h(px(30.))
                    .rounded(px(8.))
                    .text_size(px(14.))
                    .text_color(colors.subtext)
                    .font_weight(if bold {
                        gpui::FontWeight::BOLD
                    } else {
                        gpui::FontWeight::MEDIUM
                    })
                    .tooltip(move |_, cx| {
                        cx.new(|_| TooltipLabel {
                            text: tooltip_label.into(),
                            colors: tooltip_colors,
                        })
                        .into()
                    })
                    .cursor_pointer(),
            )
            .on_click(on_click);
        let child = match icon_name {
            Some(name) => icon(name, 16., colors.subtext).into_any_element(),
            None => label.into_any_element(),
        };
        button.child(child)
    };
    let mut toolbar = div()
        .mt(px(SPACE_RELATED))
        .pt(px(SPACE_GROUP))
        .border_t_1()
        .border_color(colors.surface1)
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SPACE_INLINE));
    for (label, open, close, bold, icon_name) in MARKUP_BUTTONS {
        toolbar = toolbar.child(button(
            label,
            icon_name,
            bold,
            wrap((open, close), field.clone(), handle.clone()),
        ));
    }
    let weak = cx.entity().downgrade();
    let image_field = field.clone();
    let image_handle = handle.clone();
    let image_weak = weak.clone();
    toolbar = toolbar.child(button(
        "Image",
        Some("icon-image"),
        false,
        Box::new(move |_, window, cx| {
            let field = image_field.clone();
            let _ = image_weak.update(cx, |root, cx| {
                root.upload_field_image(field, cx);
            });
            window.focus(&image_handle);
        }),
    ));
    for (label, icon_name, target) in [
        ("Text color", "icon-text-color", ColorTarget::Text),
        ("Highlight", "icon-highlight", ColorTarget::Background),
    ] {
        let color_field = field.clone();
        let color_handle = handle.clone();
        let color_weak = weak.clone();
        toolbar = toolbar.child(button(
            label,
            Some(icon_name),
            false,
            Box::new(move |event: &ClickEvent, window, cx| {
                let anchor = event.position();
                let _ = color_weak.update(cx, |root, cx| {
                    let same = same_press(
                        event,
                        root.add.color_picker_dismissed_press.as_ref(),
                    ) && root.add.color_picker_dismissed_target
                        == Some(target);
                    root.add.color_picker_dismissed_press = None;
                    root.add.color_picker_dismissed_target = None;
                    if same {
                        window.focus(&color_handle);
                    } else {
                        root.toggle_color_picker(
                            target,
                            color_field.clone(),
                            anchor,
                            window,
                            cx,
                        );
                    }
                });
            }),
        ));
    }
    if is_cloze {
        for (label, number) in CLOZE_BUTTONS {
            toolbar = toolbar.child(button(
                label,
                None,
                false,
                wrap_cloze(number, field.clone(), handle.clone()),
            ));
        }
    }
    toolbar
}

fn field_value(
    model: &ranki_model::NoteModel,
    values: &[String],
    names: &[&str],
) -> Option<String> {
    let index = model.fields.iter().position(|field| {
        names.iter().any(|name| field.eq_ignore_ascii_case(name))
    })?;
    let text = values.get(index)?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// One tab per note kind, preferring installed genanki models.
#[must_use]
pub fn note_type_tabs(
    models: &[ranki_model::NoteModel],
) -> Vec<(String, usize)> {
    let order = ["Basic", "Reversed", "Cloze", "Image Occlusion"];
    let mut tabs: Vec<(String, usize)> = Vec::new();
    for (index, model) in models.iter().enumerate() {
        let label = note_type_name(model);
        match tabs.iter_mut().find(|(name, _)| *name == label) {
            Some((_, selected))
                if tab_rank(model) < tab_rank(&models[*selected]) =>
            {
                *selected = index;
            }
            Some(_) => {}
            None => tabs.push((label, index)),
        }
    }
    tabs.sort_by_key(|(label, _)| {
        order
            .iter()
            .position(|name| name == label)
            .unwrap_or(order.len())
    });
    tabs
}

#[must_use]
pub fn preferred_model_index(
    models: &[ranki_model::NoteModel],
    index: usize,
) -> Option<usize> {
    let label = note_type_name(models.get(index)?);
    note_type_tabs(models)
        .into_iter()
        .find(|(tab_label, _)| *tab_label == label)
        .map(|(_, tab_index)| tab_index)
}

#[must_use]
fn tab_rank(model: &ranki_model::NoteModel) -> (bool, bool) {
    (
        model.id == 0,
        !model.name.to_ascii_lowercase().contains("genanki"),
    )
}

fn validation(message: &str, colors: crate::theme::Palette) -> gpui::Div {
    div()
        .text_size(px(13.))
        .text_color(colors.red)
        .child(message.to_owned())
}

#[cfg(test)]
mod tests {
    use crate::{
        app::{UiAction, models_with_stock},
        theme::MOCHA,
    };

    #[test]
    fn validation_uses_the_shared_helper() {
        let deck = ranki_model::Deck {
            id: 1,
            name: "Default".into(),
            description: String::new(),
        };
        let model = ranki_model::NoteModel {
            id: 1,
            name: "Basic".into(),
            fields: vec!["Front".into()],
            templates: vec![ranki_model::CardTemplate::default()],
            css: String::new(),
            sort_field_index: 0,
            kind: ranki_model::NoteKind::Normal,
            original_stock_kind: None,
            field_tags: vec![],
            template_ordinals: vec![0],
            raw_config: None,
            raw_json: None,
        };
        assert!(
            crate::app::validate_draft(&deck, &model, &[String::from("Q")])
                .is_empty()
        );
        assert_eq!(MOCHA.base, gpui::rgb(0x1e1e2e));
        assert_ne!(UiAction::SaveNote, UiAction::CancelAdd);
    }

    #[test]
    fn note_type_tabs_are_distinct_and_ordered() {
        let tabs = super::note_type_tabs(&models_with_stock(Vec::new()));
        assert_eq!(
            tabs.iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["Basic", "Reversed", "Cloze", "Image Occlusion"]
        );
    }

    #[test]
    fn note_type_tabs_prefer_genanki_models() {
        let plain_basic = ranki_model::NoteModel {
            id: 9,
            ..ranki_model::StockModelKind::Basic.definition()
        };
        let genanki_basic = ranki_model::NoteModel {
            id: 10,
            name: "Basic (genanki)".into(),
            ..ranki_model::StockModelKind::Basic.definition()
        };
        let genanki_reversed = ranki_model::NoteModel {
            id: 11,
            name: "Basic (and reversed card) (genanki)".into(),
            ..ranki_model::StockModelKind::BasicAndReversed.definition()
        };
        let genanki_cloze = ranki_model::NoteModel {
            id: 12,
            name: "Cloze (genanki)".into(),
            ..ranki_model::StockModelKind::Cloze.definition()
        };
        let models = models_with_stock(vec![
            plain_basic,
            genanki_basic,
            genanki_reversed,
            genanki_cloze,
        ]);
        let tabs = super::note_type_tabs(&models);

        assert_eq!(tabs.len(), 4);
        assert_eq!(tabs[0].0, "Basic");
        assert_eq!(models[tabs[0].1].name, "Basic (genanki)");
        assert_eq!(tabs[1].0, "Reversed");
        assert_eq!(
            models[tabs[1].1].name,
            "Basic (and reversed card) (genanki)"
        );
        assert_eq!(tabs[2].0, "Cloze");
        assert_eq!(models[tabs[2].1].name, "Cloze (genanki)");
        assert!(tabs.iter().all(|(label, _)| !label.contains("genanki")));
        assert_eq!(tabs[0].1, 1);
        assert_eq!(super::preferred_model_index(&models, 0), Some(tabs[0].1));
        assert_eq!(
            super::preferred_model_index(&models, tabs[0].1),
            Some(tabs[0].1)
        );
    }

    #[test]
    fn custom_note_types_stay_distinct() {
        let first = ranki_model::NoteModel {
            id: 10,
            name: String::from("Vocabulary"),
            ..ranki_model::StockModelKind::Basic.definition()
        };
        let second = ranki_model::NoteModel {
            id: 11,
            name: String::from("Basic"),
            ..ranki_model::StockModelKind::Basic.definition()
        };
        let tabs = super::note_type_tabs(&[first, second]);
        assert_eq!(tabs.len(), 2);
        let labels = tabs
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"Vocabulary"));
        assert!(labels.contains(&"Basic"));
    }

    #[test]
    fn stock_tabs_reuse_installed_models() {
        let installed = ranki_model::stock_models()
            .into_iter()
            .next()
            .map(|mut model| {
                model.id = 42;
                model
            })
            .expect("stock basic");
        let models = models_with_stock(vec![installed]);
        let tabs = super::note_type_tabs(&models);
        assert_eq!(tabs.len(), 4);
        assert_eq!(tabs[0].0, "Basic");
        assert_eq!(models[tabs[0].1].id, 42);
        assert_eq!(models[tabs[1].1].id, 0);
    }

    #[test]
    fn tab_selection_switches_the_field_list() {
        let models = models_with_stock(Vec::new());
        let cloze = models
            .iter()
            .find(|model| model.kind == ranki_model::NoteKind::Cloze)
            .expect("stock cloze");
        assert_eq!(cloze.fields, ["Text", "Back Extra"]);
    }

    #[test]
    fn preview_tabs_need_field_text() {
        assert!(!super::has_preview_content(&[String::new()]));
        assert!(!super::has_preview_content(&[String::from("  ")]));
        assert!(super::has_preview_content(&[
            String::from(" "),
            String::from("問い")
        ]));
    }

    #[test]
    fn preview_media_shows_images_for_regular_cards() {
        use crate::card_view::Side;
        let basic = ranki_model::StockModelKind::Basic.definition();
        let fields = vec![
            String::from("<img src=\"front.png\">Question?"),
            String::from("<img src=\"back.png\">Answer"),
        ];
        let question =
            crate::card_view::preview_rendered(&basic, &fields, Side::Question);
        let answer =
            crate::card_view::preview_rendered(&basic, &fields, Side::Answer);
        let (front, back) =
            super::preview_media(Some(&question), Some(&answer));
        assert_eq!(
            front
                .iter()
                .map(|media| media.name.as_str())
                .collect::<Vec<_>>(),
            ["front.png"]
        );
        assert_eq!(
            back.iter()
                .map(|media| media.name.as_str())
                .collect::<Vec<_>>(),
            ["back.png"]
        );

        let cloze = ranki_model::StockModelKind::Cloze.definition();
        let fields = vec![
            String::from(
                "<img src=\"shared.png\">The {{c1::capital}} is Paris.",
            ),
            String::new(),
        ];
        let question =
            crate::card_view::preview_rendered(&cloze, &fields, Side::Question);
        let answer =
            crate::card_view::preview_rendered(&cloze, &fields, Side::Answer);
        let (front, back) =
            super::preview_media(Some(&question), Some(&answer));
        assert_eq!(
            front
                .iter()
                .map(|media| media.name.as_str())
                .collect::<Vec<_>>(),
            ["shared.png"]
        );
        assert_eq!(back, Vec::new());
    }

    #[test]
    fn cloze_tags_wrap_the_selected_text() {
        let (open, close) = super::cloze_tags(2);
        assert_eq!(open, "{{c2::");
        let (text, caret) =
            crate::text_field::wrap_selection("France", 0..6, (&open, &close));
        assert_eq!(text, "{{c2::France}}");
        assert_eq!(caret, 14);
    }

    #[test]
    fn formatting_markup_wraps_the_selected_text() {
        for (label, open, close, _, _) in super::MARKUP_BUTTONS {
            let (text, caret) =
                crate::text_field::wrap_selection("word", 0..4, (open, close));
            assert_eq!(text, format!("{open}word{close}"), "{label}");
            assert_eq!(caret, open.len() + 4 + close.len(), "{label}");
        }
        let image = crate::occlusion::image_field("diagram.png");
        let (text, caret) = crate::text_field::insert_at("front", 0..5, &image);
        assert_eq!(text, "<img src=\"diagram.png\">");
        assert_eq!(caret, text.len());
    }

    #[test]
    fn math_button_uses_inline_mathjax_delimiters() {
        let (_, open, close, _, _) = super::MARKUP_BUTTONS
            .iter()
            .find(|button| button.0 == "Math")
            .copied()
            .unwrap_or(("", "", "", false, None));
        assert_eq!((open, close), ("\\(", "\\)"));
    }

    #[test]
    fn color_markup_wraps_text_and_background_separately() {
        let (open, close) =
            super::color_tags(super::ColorTarget::Text, "#f38ba8");
        assert_eq!(open, "<span style=\"color: #f38ba8\">");
        assert_eq!(close, "</span>");
        let (open, close) =
            super::color_tags(super::ColorTarget::Background, "inherit");
        assert_eq!(open, "<span style=\"background-color: inherit\">");
        let (text, caret) =
            crate::text_field::wrap_selection("word", 0..4, (&open, &close));
        assert_eq!(
            text,
            "<span style=\"background-color: inherit\">word</span>"
        );
        assert_eq!(caret, text.len());
    }
}
