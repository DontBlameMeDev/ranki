//! Card browser: store-backed search, result table, and card preview.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{
    app::{Root, UiAction},
    card_view::{self, CardRequest, MediaRef, RenderedCard},
    components::{
        ButtonTone, Interaction, action_button, chip, icon, icon_tile,
        search_field_icon, secondary_button,
    },
    text_field::{EditHook, TextField},
    theme::{
        PAGE_X, PAGE_Y, SPACE_BLOCK, SPACE_GROUP, SPACE_GUTTER, SPACE_RELATED,
        SPACE_SECTION, Style, alpha, blend, interval_label, page_width,
    },
};
use gpui::{
    AppContext as _, ClickEvent, Context, Entity, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point,
    Render, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled,
    Window, div, point, px,
};
use ranki_model::{
    BrowseCard, Card, CardQueue, Deck, Id, Note, NoteModel, Store,
};

/// Tooltip for the icon-only card actions trigger.
struct CardActionsTooltip {
    colors: crate::theme::Palette,
}

impl Render for CardActionsTooltip {
    fn render(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("card-actions-tooltip")
            .debug_selector(|| "card-actions-tooltip".to_owned())
            .px(px(SPACE_GROUP))
            .py(px(SPACE_RELATED))
            .rounded(px(8.))
            .bg(self.colors.surface1)
            .text_size(px(12.))
            .text_color(self.colors.text)
            .child("Card actions")
    }
}
/// Rendered height of a browse table line.
const GROUP_HEIGHT: f32 = 48.0;
const CARD_HEIGHT: f32 = 52.0;
/// Rows outside this margin are built too, so wheel steps do not reveal gaps.
const ROW_OVERSCAN: f32 = 240.0;

/// One search result, with both sides rendered once for the table and preview.
#[derive(Clone, Debug)]
pub struct BrowserRow {
    pub card: Card,
    pub note: Note,
    pub front: String,
    pub back: String,
    pub front_media: Vec<MediaRef>,
    pub back_media: Vec<MediaRef>,
    pub deck: SharedString,
    pub tags: SharedString,
    pub model: SharedString,
    /// Table-cell text prepared once with the rendered sides.
    pub preview: SharedString,
    pub due: SharedString,
    pub key: SharedString,
    /// Prepared element key for the row's actions menu.
    pub menu_key: SharedString,
}

/// One expandable deck in the browse table.
#[derive(Clone, Debug)]
pub struct BrowseGroup {
    pub path: String,
    /// Cards in this deck and all of its subdecks.
    pub count: usize,
    pub has_children: bool,
    pub expanded: bool,
    pub key: SharedString,
    pub chevron_key: SharedString,
}

/// One visible line of the grouped browse table.
#[derive(Clone, Debug)]
pub enum BrowseEntry {
    Group(BrowseGroup),
    Card { row: usize },
}

/// One search match, before its card and note are read.
#[derive(Clone, Debug)]
pub struct BrowseHit {
    pub card_id: Id,
    /// Full deck path, including virtual parents, used to group results.
    pub deck_path: SharedString,
}

/// Search state. The generation drops results from an older query.
pub struct BrowseState {
    pub search: Entity<TextField>,
    /// Every match, in table order.
    pub hits: Vec<BrowseHit>,
    pub entries: Vec<BrowseEntry>,
    /// Rendered rows, filled only for the lines the table and preview show.
    pub rows: HashMap<Id, BrowserRow>,
    /// Deck paths opened by the user or by a search with matches.
    pub expanded: HashSet<String>,
    /// Query behind the current hits and expansion. A new query reopens paths.
    pub last_query: Option<String>,
    pub selected: Option<Id>,
    pub error: Option<String>,
    pub generation: u64,
    pub loading: bool,
    /// Query and collection stamp used to reuse the current result set.
    pub query: Option<String>,
    pub modified: Option<i64>,
    /// Last known table viewport height, used to clamp after result changes.
    pub viewport: Option<f32>,
}

/// Renders both sides of one card and resolves its deck and note type names.
#[must_use]
pub fn browser_row(
    card: &Card,
    note: &Note,
    decks: &[Deck],
    models: &[NoteModel],
) -> Option<BrowserRow> {
    let model = models.iter().find(|model| model.id == note.model_id)?;
    let found = decks.iter().find(|deck| deck.id == card.deck_id);
    let deck = found.map_or_else(
        || format!("Deck {}", card.deck_id),
        |deck| deck.name.clone(),
    );
    let front_rendered = card_view::render_card(
        model,
        note,
        CardRequest::question(card.ordinal),
    );
    let back_rendered = card_view::render_card(
        model,
        note,
        CardRequest::answer_only(card.ordinal),
    );
    let front = card_label(&front_rendered);
    let back = card_label(&back_rendered);
    Some(BrowserRow {
        card: card.clone(),
        note: note.clone(),
        preview: SharedString::from(preview_line(&front)),
        due: SharedString::from(due_label(card)),
        key: SharedString::from(format!("browse-row-{}", card.id)),
        menu_key: SharedString::from(format!("browse-menu-{}", card.id)),
        front,
        front_media: front_rendered.media,
        back,
        back_media: back_rendered.media,
        deck: SharedString::from(deck),
        tags: SharedString::from(note.tags.join(", ")),
        model: SharedString::from(model.name.clone()),
    })
}

/// Reads one card and its note, then renders the row the table shows.
pub fn load_browse_row(
    store: &dyn Store,
    decks: &[Deck],
    models: &[NoteModel],
    id: Id,
) -> Option<BrowserRow> {
    let card = store.card(id).ok().flatten()?;
    let note = store.note(card.note_id).ok().flatten()?;
    browser_row(&card, &note, decks, models)
}

/// Uses media names when a rendered side has no plain text, as with flag images.
fn card_label(rendered: &RenderedCard) -> String {
    if rendered.text.trim().is_empty() {
        rendered
            .media
            .iter()
            .map(|media| media.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        rendered.text.clone()
    }
}

/// Flattens rendered card text into a single bounded table cell.
#[must_use]
pub fn preview_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 120 {
        return flat;
    }
    let mut short = flat.chars().take(117).collect::<String>();
    short.push_str("...");
    short
}

/// Short status of a card in the browser table.
#[must_use]
pub const fn queue_label(queue: CardQueue) -> &'static str {
    match queue {
        CardQueue::New => "New",
        CardQueue::Learning | CardQueue::DayLearning => "Learning",
        CardQueue::Review => "Review",
        CardQueue::Suspended => "Suspended",
        CardQueue::Buried | CardQueue::ManuallyBuried => "Buried",
        CardQueue::Preview => "Preview",
    }
}

/// Due cell: the current interval for review cards, a dash for inactive ones.
#[must_use]
pub fn due_label(card: &Card) -> String {
    match card.queue {
        CardQueue::Review => interval_label(card.interval),
        CardQueue::Suspended
        | CardQueue::Buried
        | CardQueue::ManuallyBuried
        | CardQueue::Preview => String::from("-"),
        CardQueue::New | CardQueue::Learning | CardQueue::DayLearning => {
            String::from("Today")
        }
    }
}

/// Sorts by the note's sort field, then id, so the table order is stable.
pub fn sort_browse_cards(cards: &mut [BrowseCard]) {
    cards.sort_by(|left, right| {
        left.sort_field
            .cmp(&right.sort_field)
            .then_with(|| left.card_id.cmp(&right.card_id))
    });
}

/// Paths from the root down to each matching card's deck. A search opens
/// exactly these groups, so matches are visible without extra clicks.
#[must_use]
pub fn browse_expand_paths(hits: &[BrowseHit]) -> HashSet<String> {
    let mut paths = HashSet::new();
    for hit in hits {
        let path = hit.deck_path.as_str();
        let _ = paths.insert(path.to_owned());
        for (index, _) in path.match_indices("::") {
            let _ = paths.insert(path[..index].to_owned());
        }
    }
    paths
}

/// Builds the visible table lines: deck groups and, for open groups, their
/// cards. Cards sit only under their own deck, never under parent groups.
#[must_use]
pub fn build_browse_entries(
    hits: &[BrowseHit],
    expanded: &HashSet<String>,
) -> Vec<BrowseEntry> {
    let mut cards_by_path: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, hit) in hits.iter().enumerate() {
        cards_by_path
            .entry(hit.deck_path.as_str())
            .or_default()
            .push(index);
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (path, cards) in &cards_by_path {
        let _ = counts.insert(*path, cards.len());
    }
    for (path, cards) in &cards_by_path {
        for (index, _) in path.match_indices("::") {
            let count = counts.entry(&path[..index]).or_insert(0);
            *count = count.saturating_add(cards.len());
        }
    }
    let mut paths = cards_by_path
        .keys()
        .map(|path| (*path).to_owned())
        .collect::<HashSet<_>>();
    for path in cards_by_path.keys() {
        for (index, _) in path.match_indices("::") {
            let _ = paths.insert(path[..index].to_owned());
        }
    }
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut roots = Vec::new();
    for path in paths {
        match path.rsplit_once("::") {
            Some((parent, _)) => children
                .entry(parent.to_owned())
                .or_default()
                .push(path.clone()),
            None => roots.push(path),
        }
    }
    roots.sort();
    for siblings in children.values_mut() {
        siblings.sort();
    }
    let mut entries = Vec::with_capacity(hits.len().saturating_mul(2));
    let mut stack = roots.into_iter().rev().collect::<Vec<_>>();
    while let Some(path) = stack.pop() {
        let cards = cards_by_path
            .get(path.as_str())
            .cloned()
            .unwrap_or_default();
        let has_children = children.contains_key(&path);
        let open = expanded.contains(&path);
        entries.push(BrowseEntry::Group(BrowseGroup {
            count: counts.get(path.as_str()).copied().unwrap_or(0),
            path: path.clone(),
            has_children,
            expanded: open,
            key: SharedString::from(format!("browse-group-{path}")),
            chevron_key: SharedString::from(format!("browse-chevron-{path}")),
        }));
        if !open {
            continue;
        }
        for index in cards {
            entries.push(BrowseEntry::Card { row: index });
        }
        if let Some(kids) = children.get(&path) {
            stack.extend(kids.iter().rev().cloned());
        }
    }
    entries
}

/// Line range to build for the current scroll position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrowseWindow {
    pub first: usize,
    pub last: usize,
    /// Combined height of the lines before `first`.
    pub before: f32,
    /// Combined height of the lines from `last` on.
    pub after: f32,
    pub height: f32,
}

fn browse_entry_height(entry: &BrowseEntry) -> f32 {
    match entry {
        BrowseEntry::Group(_) => GROUP_HEIGHT,
        BrowseEntry::Card { .. } => CARD_HEIGHT,
    }
}

/// Picks the visible lines plus a fixed overscan. Spacers keep the scroll
/// height of the lines that are left out.
#[must_use]
pub fn browse_visible_window(
    entries: &[BrowseEntry],
    offset: f32,
    viewport: f32,
    header: f32,
) -> BrowseWindow {
    let mut tops = Vec::with_capacity(entries.len().saturating_add(1));
    let mut height = 0.0;
    tops.push(height);
    for entry in entries {
        height += browse_entry_height(entry);
        tops.push(height);
    }
    let top = offset - header - ROW_OVERSCAN;
    let bottom = offset - header + viewport + ROW_OVERSCAN;
    let mut first = 0;
    while first < entries.len() && tops[first + 1] <= top {
        first += 1;
    }
    let mut last = first;
    while last < entries.len() && tops[last] < bottom {
        last += 1;
    }
    BrowseWindow {
        first,
        last,
        before: tops[first],
        after: height - tops[last],
        height,
    }
}

impl Root {
    /// Refreshes the table after each edit, debounced by the search clock.
    pub(crate) fn browse_edit_hook(cx: &mut Context<Self>) -> EditHook {
        let weak = cx.entity().downgrade();
        Arc::new(move |app| {
            let weak = weak.clone();
            app.defer(move |app| {
                let _ = weak.update(app, |root, cx| {
                    root.schedule_browse_refresh(cx);
                });
            });
        })
    }
    pub(crate) fn schedule_browse_refresh(&mut self, cx: &mut Context<Self>) {
        self.browse.generation = self.browse.generation.wrapping_add(1);
        let generation = self.browse.generation;
        self.browse.loading = true;
        self.browse.error = None;
        let timer = cx.background_executor().timer(super::app::DEBOUNCE);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |root, cx| {
                if root.browse.generation == generation {
                    root.refresh_browse(cx);
                    root.persist_state(cx);
                }
            });
        })
        .detach();
    }

    /// Reads the current query and rebuilds the deck groups and match list.
    ///
    /// Cards are not read here: the table renders the lines it shows.
    pub(crate) fn refresh_browse(&mut self, cx: &mut Context<Self>) {
        let query = self.browse.search.read(cx).text();
        self.card_menu = None;
        self.card_menu_dismissed_press = None;
        if self.browse.query.as_deref() == Some(query.as_str())
            && self.browse.modified == Some(self.collection_modified)
        {
            self.browse.loading = false;
            return;
        }
        let query_changed =
            self.browse.last_query.as_deref() != Some(query.as_str());
        if query_changed {
            self.browse.last_query = Some(query.clone());
            self.browse_smooth.clamp(0.0);
        }
        let mut matches = match self.store.browse_cards(&query) {
            Ok(matches) => matches,
            Err(error) => {
                self.browse.hits.clear();
                self.browse.entries.clear();
                self.browse.rows.clear();
                if query_changed {
                    self.browse.expanded.clear();
                }
                self.browse.selected = None;
                self.browse.error =
                    Some(format!("Could not search cards: {error}"));
                self.browse.query = None;
                self.browse.modified = None;
                self.browse.loading = false;
                cx.notify();
                return;
            }
        };
        sort_browse_cards(&mut matches);
        let paths = self
            .decks
            .iter()
            .map(|deck| (deck.id, SharedString::from(deck.name.clone())))
            .collect::<HashMap<_, _>>();
        let hits = matches
            .into_iter()
            .map(|card| BrowseHit {
                deck_path: paths.get(&card.deck_id).cloned().unwrap_or_else(
                    || SharedString::from(format!("Deck {}", card.deck_id)),
                ),
                card_id: card.card_id,
            })
            .collect::<Vec<_>>();
        let selected = if hits
            .iter()
            .any(|hit| self.browse.selected == Some(hit.card_id))
        {
            self.browse.selected
        } else {
            hits.first().map(|hit| hit.card_id)
        };
        if self.browse.selected != selected {
            self.browse.selected = selected;
            self.browse_detail_scroll
                .set_offset(point(Pixels::ZERO, px(0.)));
        }
        if query_changed {
            self.browse.expanded = browse_expand_paths(&hits);
        }
        self.browse.hits = hits;
        self.browse.rows.clear();
        self.rebuild_browse_entries();
        self.browse.query = Some(query);
        self.browse.modified = Some(self.collection_modified);
        self.browse.error = None;
        self.browse.loading = false;
        cx.notify();
    }

    /// Rebuilds the visible table lines after results or expansion change.
    pub(crate) fn rebuild_browse_entries(&mut self) {
        self.browse.entries =
            build_browse_entries(&self.browse.hits, &self.browse.expanded);
        let height = self
            .browse
            .entries
            .iter()
            .map(browse_entry_height)
            .sum::<f32>();
        let max = (height - self.browse.viewport.unwrap_or(0.0)).max(0.0);
        self.browse_smooth.clamp(max);
    }

    /// Renders the cached row for one card, reading it when it is missing.
    pub(crate) fn ensure_browse_row(&mut self, id: Id) -> Option<&BrowserRow> {
        if !self.browse.rows.contains_key(&id) {
            let row =
                load_browse_row(&*self.store, &self.decks, &self.models, id)?;
            let _ = self.browse.rows.insert(id, row);
        }
        self.browse.rows.get(&id)
    }

    /// Reads the rows for the lines the table shows and for the preview.
    fn load_browse_rows(
        &mut self,
        visible: &BrowseWindow,
        cx: &mut Context<Self>,
    ) {
        let mut needed = Vec::new();
        if let Some(id) = self.browse.selected {
            needed.push(id);
        }
        for entry in &self.browse.entries[visible.first..visible.last] {
            if let BrowseEntry::Card { row } = entry {
                needed.push(self.browse.hits[*row].card_id);
            }
        }
        needed.retain(|id| !self.browse.rows.contains_key(id));
        needed.sort_unstable();
        needed.dedup();
        if !needed.is_empty() {
            let loaded =
                self.store.cards_with_notes(&needed).unwrap_or_default();
            for (card, note) in loaded {
                if let Some(row) =
                    browser_row(&card, &note, &self.decks, &self.models)
                {
                    let _ = self.browse.rows.insert(card.id, row);
                }
            }
            if needed.iter().any(|id| !self.browse.rows.contains_key(id))
                && self.browse.error.is_none()
            {
                self.browse.error =
                    Some(String::from("Could not load some cards"));
                cx.notify();
            }
        }
    }

    pub(crate) fn toggle_browse_group(
        &mut self,
        path: String,
        cx: &mut Context<Self>,
    ) {
        if !self.browse.expanded.remove(&path) {
            let _ = self.browse.expanded.insert(path);
        }
        self.rebuild_browse_entries();
        cx.notify();
    }

    pub(crate) fn select_browse_row(&mut self, id: Id, cx: &mut Context<Self>) {
        if self.browse.selected != Some(id) {
            self.browse.selected = Some(id);
            self.browse_detail_scroll
                .set_offset(point(Pixels::ZERO, px(0.)));
            cx.notify();
        }
    }

    /// Moves the selection through the cards that are open in the table.
    pub(crate) fn move_browse_selection(
        &mut self,
        step: isize,
        cx: &mut Context<Self>,
    ) {
        let cards = self
            .browse
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                BrowseEntry::Card { row } => {
                    Some((index, self.browse.hits[*row].card_id))
                }
                BrowseEntry::Group(_) => None,
            })
            .collect::<Vec<_>>();
        if cards.is_empty() {
            return;
        }
        let selected = cards
            .iter()
            .position(|(_, id)| Some(*id) == self.browse.selected);
        let next = match selected {
            Some(position) if step < 0 => {
                position.saturating_sub(step.unsigned_abs())
            }
            Some(position) => position
                .saturating_add(step.unsigned_abs())
                .min(cards.len() - 1),
            None if step < 0 => cards.len() - 1,
            None => 0,
        };
        let (entry, id) = cards[next];
        self.scroll_browse_entry_into_view(entry);
        if self.browse.selected != Some(id) {
            self.browse.selected = Some(id);
            self.browse_detail_scroll
                .set_offset(point(Pixels::ZERO, px(0.)));
            cx.notify();
        }
    }

    fn scroll_browse_entry_into_view(&mut self, entry: usize) {
        let header = self
            .browse_smooth
            .handle()
            .bounds_for_item(0)
            .map_or(2.0 * SPACE_RELATED + 16.0, |bounds| {
                f32::from(bounds.size.height)
            });
        let viewport = self.browse.viewport.unwrap_or(0.0);
        let top = self.browse.entries[..entry]
            .iter()
            .map(browse_entry_height)
            .sum::<f32>();
        let bottom = top + browse_entry_height(&self.browse.entries[entry]);
        let offset = self.browse_smooth.offset();
        let target = if top + header < offset {
            top + header
        } else if bottom + header > offset + viewport {
            bottom + header - viewport
        } else {
            return;
        };
        self.browse_smooth.wheel(target.max(0.0) - offset);
    }

    /// Opens or closes the card actions menu for one result.
    pub(crate) fn toggle_card_menu(
        &mut self,
        id: Id,
        anchor: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let open = self.card_menu.is_some_and(|menu| menu.id == id);
        self.card_menu = if open {
            None
        } else {
            Some(crate::app::CardMenu { id, anchor })
        };
        cx.notify();
    }

    pub(crate) fn render_browse(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let width =
            page_width(f32::from(window.viewport_size().width), self.nav_width);
        let columns = BrowseColumns::for_width(width);
        let show_detail_side = width >= 900.0;
        let viewport_height = f32::from(window.viewport_size().height);
        let compact = viewport_height < 600.0;
        let stacked_detail_height = if compact {
            116.0
        } else {
            (viewport_height * 0.28).clamp(120.0, 220.0)
        };

        let smooth = self.motion_ms(1) > 0;
        if self.browse_smooth.advance(window, smooth) {
            cx.notify();
        }
        let colors = self.colors();
        let style = Style::new(colors, self.theme());
        let query = self.browse.search.read(cx).text();
        let selected = self.browse.selected;
        let error = self.browse.error.clone();
        let pending = self.browse.loading;
        let mut content = div()
            .id("browse-scroll")
            .size_full()
            .flex()
            .flex_col()
            .gap(px(if compact {
                SPACE_RELATED
            } else {
                SPACE_SECTION
            }))
            .px(px(PAGE_X))
            .py(px(if compact { SPACE_BLOCK } else { PAGE_Y }))
            .child(
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
                            .child("Browse"),
                    )
                    .child(
                        Interaction::raised(colors)
                            .apply(
                                div()
                                    .id("browse-search")
                                    .flex()
                                    .items_center()
                                    .gap(px(SPACE_GROUP))
                                    .flex_initial()
                                    .w(px((width - 180.0).clamp(160.0, 460.0)))
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
                                    let field = this.browse.search.clone();
                                    window.focus(field.read(cx).focus_handle());
                                },
                            ))
                            .child(search_field_icon(
                                self.browse.search.read(cx).focus_handle(),
                                18.,
                                colors,
                                window,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(self.browse.search.clone())
                                    .children(pending.then(|| {
                                        div()
                                            .id("browse-search-pending")
                                            .debug_selector(|| {
                                                "browse-search-pending"
                                                    .to_owned()
                                            })
                                            .flex_none()
                                            .text_size(px(12.))
                                            .text_color(colors.subtext)
                                            .child("Searching...")
                                    })),
                            ),
                    ),
            );

        let mut table = div()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(BROWSE_TABLE_MIN_HEIGHT))
            .id("browse-table")
            .debug_selector(|| "browse-table".to_owned())
            .flex()
            .flex_col()
            .rounded(px(20.))
            .bg(colors.mantle)
            .border_1()
            .border_color(colors.surface0)
            .overflow_scroll()
            .track_scroll(&self.browse_scroll);
        if smooth {
            table = table.on_scroll_wheel(cx.listener(
                |this, event: &ScrollWheelEvent, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    this.browse_smooth.wheel(-f32::from(delta.y));
                    window.refresh();
                    cx.stop_propagation();
                },
            ));
        }
        table = table.child(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_GROUP))
                .px(px(SPACE_BLOCK))
                .py(px(SPACE_RELATED))
                .flex_none()
                .text_size(px(12.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.subtext)
                .children(
                    [
                        Some(browse_cell("QUESTION", 0, colors)),
                        Some(browse_cell("DECK", 1, colors)),
                        columns.tags.then(|| browse_cell("TAGS", 2, colors)),
                        columns
                            .type_and_state
                            .then(|| browse_cell("TYPE", 3, colors)),
                        columns
                            .type_and_state
                            .then(|| browse_cell("STATE", 4, colors)),
                        Some(browse_cell("DUE", 5, colors)),
                        Some(div().w(px(BROWSE_ACTION_WIDTH)).flex_none()),
                    ]
                    .into_iter()
                    .flatten(),
                ),
        );
        let handle = self.browse_smooth.handle();
        let viewport = f32::from(handle.bounds().size.height);
        let viewport_changed = match self.browse.viewport {
            Some(previous) => (viewport - previous).abs() > 0.5,
            None => true,
        };
        self.browse.viewport = Some(viewport);
        if viewport_changed {
            cx.notify();
        }
        let header = handle.bounds_for_item(0).map_or_else(
            || 2.0 * SPACE_RELATED + f32::from(window.line_height()),
            |bounds| f32::from(bounds.size.height),
        );
        let visible = browse_visible_window(
            &self.browse.entries,
            self.browse_smooth.offset(),
            viewport,
            header,
        );
        self.load_browse_rows(&visible, cx);
        if visible.first > 0 {
            table = table.child(div().h(px(visible.before)).flex_none());
        }
        for entry in &self.browse.entries[visible.first..visible.last] {
            if let BrowseEntry::Group(group) = entry {
                table = table.child(self.render_browse_group(group, cx));
                continue;
            }
            let BrowseEntry::Card { row: index } = entry else {
                continue;
            };
            let Some(row) =
                self.browse.rows.get(&self.browse.hits[*index].card_id)
            else {
                let key =
                    SharedString::from(format!("browse-failed-row-{index}"));
                table = table.child(
                    div()
                        .id(key.clone())
                        .debug_selector(|| key.to_string())
                        .h(px(CARD_HEIGHT))
                        .flex_none()
                        .flex()
                        .items_center()
                        .px(px(SPACE_BLOCK))
                        .text_size(px(13.))
                        .text_color(colors.red)
                        .child("Could not load card"),
                );
                continue;
            };
            let id = row.card.id;
            let key = row.key.clone();
            let menu_key = row.menu_key.clone();
            let active = selected == Some(id);
            let state = queue_label(row.card.queue);
            let color = crate::theme::queue_color(
                u8::try_from(row.card.queue.code()).unwrap_or(3),
                self.theme(),
            );
            let hover_bg = if active {
                blend(colors.surface0, colors.surface1, 0.5)
            } else {
                colors.surface0
            };
            let press_bg = if active {
                blend(colors.surface1, colors.crust, 0.14)
            } else {
                blend(colors.surface0, colors.surface1, 0.7)
            };
            let mut line = div()
                .id(key.clone())
                .debug_selector(|| key.to_string())
                .flex()
                .items_center()
                .gap(px(SPACE_GROUP))
                .h(px(CARD_HEIGHT))
                .flex_none()
                .px(px(SPACE_BLOCK))
                .pl(px(SPACE_BLOCK + SPACE_SECTION))
                .border_t_1()
                .border_color(colors.surface0)
                .bg(if active {
                    colors.surface0
                } else {
                    colors.mantle
                })
                .cursor_pointer()
                .focusable()
                .tab_stop(true)
                .hover(move |style| style.bg(hover_bg))
                .active(move |style| style.bg(press_bg))
                .focus(move |style| {
                    style.bg(blend(hover_bg, colors.text, 0.08))
                })
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.toggle_card_menu(id, event.position, cx);
                    }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_browse_row(id, cx);
                }));
            if active {
                line =
                    line.border_l_2().border_color(colors.mauve).border_t_0();
            }
            line = line
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(row.preview.clone()),
                )
                .children(
                    [
                        Some(browse_cell(row.deck.clone(), 1, colors)),
                        columns
                            .tags
                            .then(|| browse_cell(row.tags.clone(), 2, colors)),
                        columns
                            .type_and_state
                            .then(|| browse_cell(row.model.clone(), 3, colors)),
                        columns.type_and_state.then(|| {
                            browse_cell(state, 4, colors)
                                .text_color(color)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                        }),
                        Some(browse_cell(row.due.clone(), 5, colors)),
                    ]
                    .into_iter()
                    .flatten(),
                )
                .child({
                    let menu_open =
                        self.card_menu.is_some_and(|menu| menu.id == id);
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
                            .id(menu_key.clone())
                            .debug_selector(|| menu_key.to_string())
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(BROWSE_ACTION_WIDTH))
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
                            let same_press = match event {
                                ClickEvent::Mouse(click) => this
                                    .card_menu_dismissed_press
                                    .as_ref()
                                    .is_some_and(|down| {
                                        down.button == click.down.button
                                            && down.position
                                                == click.down.position
                                            && down.click_count
                                                == click.down.click_count
                                            && down.first_mouse
                                                == click.down.first_mouse
                                    }),
                                ClickEvent::Keyboard(_) => false,
                            };
                            cx.stop_propagation();
                            this.card_menu_dismissed_press = None;
                            if !same_press {
                                this.toggle_card_menu(id, event.position(), cx);
                            }
                        },
                    ))
                    .tooltip(move |_, cx| {
                        let colors = colors;
                        cx.new(|_| CardActionsTooltip { colors }).into()
                    })
                    .child(icon(
                        "icon-more",
                        18.,
                        colors.overlay,
                    ))
                });
            table = table.child(line);
        }
        if visible.last < self.browse.entries.len() {
            table = table.child(div().h(px(visible.after)).flex_none());
        }
        if self.browse.hits.is_empty() {
            table = table.child(self.render_browse_empty(
                error.as_deref(),
                &query,
                style,
            ));
        }
        let detail = Some(self.render_browse_detail(
            selected,
            style,
            show_detail_side,
            stacked_detail_height,
        ));
        let body = div().flex().flex_1().min_h(px(0.)).gap(px(if compact {
            SPACE_RELATED
        } else {
            SPACE_SECTION
        }));
        let body = if show_detail_side {
            body.child(table).children(detail)
        } else {
            body.flex_col().child(table).children(detail)
        };
        content = content.child(body);
        gpui::image_cache(self.images.clone())
            .size_full()
            .child(content)
    }
    fn render_browse_group(
        &self,
        group: &BrowseGroup,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = self.colors();
        let path = group.path.clone();
        let row_path = path.clone();
        let bg = blend(colors.mantle, colors.surface0, 0.45);
        let hover_bg = blend(bg, colors.surface1, 0.5);
        let press_bg = blend(bg, colors.crust, 0.12);
        let mut line = div()
            .id(group.key.clone())
            .debug_selector(|| group.key.to_string())
            .flex()
            .items_center()
            .gap(px(SPACE_GROUP))
            .h(px(GROUP_HEIGHT))
            .flex_none()
            .px(px(SPACE_BLOCK))
            .border_t_1()
            .border_color(colors.surface0)
            .bg(bg)
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .hover(move |style| style.bg(hover_bg))
            .active(move |style| style.bg(press_bg))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_browse_group(path.clone(), cx);
            }));
        if group.has_children || group.count > 0 {
            line = line.child(
                Interaction::surface(colors).apply(
                    div()
                        .id(group.chevron_key.clone())
                        .debug_selector(|| group.chevron_key.to_string())
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(24.))
                        .h(px(24.))
                        .flex_none()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_browse_group(row_path.clone(), cx);
                        }))
                        .child(icon(
                            if group.expanded {
                                "icon-chevron-down"
                            } else {
                                "icon-chevron-right"
                            },
                            18.,
                            colors.overlay,
                        )),
                ),
            );
        } else {
            line = line.child(div().w(px(24.)).flex_none());
        }
        line.child(icon_tile(
            if group.has_children {
                "icon-layers"
            } else {
                "icon-book"
            },
            colors.mauve,
        ))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(SPACE_GROUP))
                .child(
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .font_family(crate::theme::font_for_text(&group.path))
                        .child(group.path.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(colors.subtext)
                        .child(format!("{} cards", group.count)),
                ),
        )
    }

    fn render_browse_empty(
        &self,
        error: Option<&str>,
        query: &str,
        style: Style,
    ) -> impl IntoElement {
        let colors = style.colors;
        let (title, detail) = if let Some(error) = error {
            ("Could not search", error)
        } else if query.trim().is_empty() {
            (
                "No cards yet",
                "Add a card to start browsing your collection",
            )
        } else {
            ("No cards match", "Try a different search")
        };
        let mut empty = div()
            .flex()
            .flex_1()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SPACE_RELATED))
            .py(px(SPACE_GUTTER))
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(colors.subtext)
                    .child(detail.to_owned()),
            );
        if !query.trim().is_empty() {
            empty = empty.child(secondary_button(
                "Clear search",
                colors,
                self.action_click(UiAction::ClearBrowseSearch),
            ));
        } else if error.is_none() {
            empty = empty.child(action_button(
                "browse-empty-add",
                "Add card",
                ButtonTone::Primary,
                style,
                self.action_click(UiAction::OpenAdd),
            ));
        }
        empty
    }
    fn render_browse_detail(
        &self,
        selected: Option<Id>,
        style: Style,
        side: bool,
        stacked_height: f32,
    ) -> impl IntoElement {
        let colors = style.colors;
        let mut detail = div()
            .id("browse-detail")
            .debug_selector(|| "browse-detail".to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_BLOCK))
            .p(px(SPACE_SECTION))
            .rounded(px(20.))
            .bg(colors.mantle)
            .border_1()
            .border_color(colors.surface0)
            .overflow_scroll()
            .track_scroll(&self.browse_detail_scroll);
        if side {
            detail = detail.w(px(BROWSE_PREVIEW_WIDTH));
        } else {
            detail = detail.w_full().h(px(stacked_height)).flex_none();
        }
        let Some(row) = selected.and_then(|id| self.browse.rows.get(&id))
        else {
            return detail.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.))
                    .text_color(colors.subtext)
                    .child("Select a card to preview it"),
            );
        };
        detail = detail
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Card preview"),
            )
            .child(browse_meta("Deck", row.deck.as_str(), colors))
            .child(browse_meta("Note type", row.model.as_str(), colors))
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.subtext)
                    .child("QUESTION"),
            )
            .child(div().text_size(px(15.)).child(row.front.clone()))
            .children(card_view::media_column(
                &row.front_media,
                &self.options.paths.media_dir,
                colors,
                180.0,
            ))
            .child(div().h(px(1.)).bg(colors.surface0))
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(colors.subtext)
                    .child("ANSWER"),
            )
            .child(div().text_size(px(15.)).child(row.back.clone()))
            .children(card_view::media_column(
                &row.back_media,
                &self.options.paths.media_dir,
                colors,
                180.0,
            ));
        if !row.note.tags.is_empty() {
            detail = detail.child(
                div().flex().flex_wrap().gap(px(SPACE_GROUP)).children(
                    row.note.tags.iter().map(|tag| chip(tag.clone(), colors)),
                ),
            );
        }
        detail.child(browse_meta("Reviews", &row.card.reps.to_string(), colors))
    }
}

fn browse_cell(
    text: impl Into<SharedString>,
    column: usize,
    colors: crate::theme::Palette,
) -> gpui::Div {
    let mut cell = div()
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_size(px(12.))
        .text_color(colors.subtext)
        .child(text.into());
    if column == 0 {
        cell = cell.flex_1().min_w(px(0.));
    } else {
        cell = cell.w(px(BROWSE_COLUMNS[column]));
    }
    cell
}

fn browse_meta(
    label: &str,
    value: &str,
    colors: crate::theme::Palette,
) -> gpui::Stateful<gpui::Div> {
    let selector = SharedString::from(format!("browse-meta-{label}"));
    div()
        .id(selector.clone())
        .debug_selector(move || selector.to_string())
        .flex()
        .justify_between()
        .gap(px(SPACE_RELATED))
        .text_size(px(12.))
        .child(
            div()
                .flex_none()
                .text_color(colors.subtext)
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_right()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(value.to_owned()),
        )
}

pub const BROWSE_DETAIL_MIN_WIDTH: f32 = 520.;
const BROWSE_TABLE_MIN_HEIGHT: f32 = 140.;
const BROWSE_PREVIEW_WIDTH: f32 = 320.;
const BROWSE_COLUMNS: [f32; 6] = [1.0, 105., 90., 75., 65., 50.];
const BROWSE_ACTION_WIDTH: f32 = 30.;

/// Columns that still fit beside the card preview text.
#[derive(Clone, Copy)]
struct BrowseColumns {
    tags: bool,
    type_and_state: bool,
}

impl BrowseColumns {
    fn for_width(width: f32) -> Self {
        Self {
            tags: width >= 860.0,
            type_and_state: width >= 720.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::SharedString;
    use std::collections::HashSet;

    use ranki_model::{
        BrowseCard, Card, CardQueue, CardTemplate, CardType, DEFAULT_DECK_ID,
        DEFAULT_MODEL_ID, Deck, Id, Note, NoteModel, StockModelKind,
    };

    use super::{
        BrowseColumns, BrowseEntry, BrowseGroup, BrowseHit, CARD_HEIGHT,
        GROUP_HEIGHT, browse_expand_paths, browse_visible_window, browser_row,
        build_browse_entries, due_label, preview_line, queue_label,
        sort_browse_cards,
    };

    #[test]
    fn browse_columns_drop_as_the_page_narrows() {
        let narrow = BrowseColumns::for_width(500.0);
        assert!(!narrow.tags && !narrow.type_and_state);
        let medium = BrowseColumns::for_width(750.0);
        assert!(!medium.tags && medium.type_and_state);
        let wide = BrowseColumns::for_width(1048.0);
        assert!(wide.tags && wide.type_and_state);
    }
    fn card_with_id(id: Id, queue: CardQueue, interval: i32) -> Card {
        Card {
            id,
            note_id: 1,
            deck_id: DEFAULT_DECK_ID,
            ordinal: 0,
            modified: 0,
            usn: 0,
            card_type: CardType::New,
            queue,
            due: 0,
            interval,
            factor: 0,
            reps: 0,
            lapses: 0,
            left: 0,
            original_due: 0,
            original_deck_id: 0,
            flags: 0,
            data: String::new(),
        }
    }

    fn model() -> NoteModel {
        NoteModel {
            id: DEFAULT_MODEL_ID,
            name: "Basic".to_owned(),
            fields: vec!["Front".to_owned(), "Back".to_owned()],
            templates: vec![CardTemplate {
                name: "Card 1".to_owned(),
                question_format: "{{Front}}".to_owned(),
                answer_format: "{{FrontSide}}<hr id=answer>{{Back}}".to_owned(),
                ord: 0,
            }],
            ..NoteModel::default()
        }
    }

    #[test]
    fn preview_line_flattens_and_bounds_rendered_text() {
        assert_eq!(preview_line(" One\n two "), "One two");
        assert_eq!(preview_line(&"word ".repeat(40)).chars().count(), 120);
    }

    #[test]
    fn queue_and_due_labels_cover_every_state() {
        assert_eq!(queue_label(CardQueue::New), "New");
        assert_eq!(queue_label(CardQueue::DayLearning), "Learning");
        assert_eq!(queue_label(CardQueue::Buried), "Buried");
        assert_eq!(due_label(&card_with_id(1, CardQueue::Review, 12)), "12d");
        assert_eq!(due_label(&card_with_id(1, CardQueue::Suspended, 12)), "-");
        assert_eq!(due_label(&card_with_id(1, CardQueue::New, 0)), "Today");
    }

    #[test]
    fn browse_entries_group_cards_by_deck_and_open_expanded_paths() {
        let decks = [
            Deck {
                id: 1,
                name: "Languages".to_owned(),
                description: String::new(),
            },
            Deck {
                id: 2,
                name: "Languages::French".to_owned(),
                description: String::new(),
            },
            Deck {
                id: 3,
                name: "Math".to_owned(),
                description: String::new(),
            },
        ];
        let mut hits = Vec::new();
        for (card_id, deck_id) in [(1, 2), (2, 3), (3, 2), (4, 1)] {
            let deck =
                decks.iter().find(|deck| deck.id == deck_id).expect("deck");
            hits.push(BrowseHit {
                card_id,
                deck_path: SharedString::from(deck.name.clone()),
            });
        }
        let expanded = browse_expand_paths(&hits);
        assert!(expanded.contains("Languages"));
        assert!(expanded.contains("Languages::French"));

        for _ in 0..16 {
            let entries = build_browse_entries(&hits, &expanded);
            let lines = entries
                .iter()
                .map(|entry| match entry {
                    BrowseEntry::Group(group) => {
                        format!("{}:{}", group.path, group.count)
                    }
                    BrowseEntry::Card { row } => {
                        format!("card:{}", hits[*row].card_id)
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(
                lines,
                [
                    "Languages:3",
                    "card:4",
                    "Languages::French:2",
                    "card:1",
                    "card:3",
                    "Math:1",
                    "card:2",
                ]
            );
        }

        let collapsed = build_browse_entries(&hits, &HashSet::new());
        assert_eq!(collapsed.len(), 2);
        assert!(
            matches!(collapsed[0], BrowseEntry::Group(ref group) if group.path == "Languages" && !group.expanded)
        );
    }

    #[test]
    fn browser_rows_show_the_question_answer_and_deck() {
        let deck = Deck {
            id: DEFAULT_DECK_ID,
            name: "Default".to_owned(),
            description: String::new(),
        };
        let note = Note {
            id: 1,
            guid: String::new(),
            model_id: DEFAULT_MODEL_ID,
            modified: 0,
            usn: 0,
            tags: Vec::new(),
            fields: vec!["Apple".to_owned(), "Answer".to_owned()],
            sort_field: String::new(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        let row = browser_row(
            &card_with_id(1, CardQueue::New, 0),
            &note,
            std::slice::from_ref(&deck),
            std::slice::from_ref(&model()),
        )
        .expect("browser row");
        assert_eq!(row.front, "Apple");
        assert_eq!(row.back, "Answer");
        assert_eq!(row.deck, "Default");
        assert_eq!(row.preview, "Apple");
        assert_eq!(row.due, "Today");
        assert_eq!(row.key.as_str(), "browse-row-1");
        assert_eq!(row.menu_key.as_str(), "browse-menu-1");
    }

    #[test]
    fn browse_cards_sort_by_sort_field_then_id() {
        let mut cards = vec![
            BrowseCard {
                card_id: 3,
                deck_id: DEFAULT_DECK_ID,
                sort_field: "Zebra".to_owned(),
            },
            BrowseCard {
                card_id: 1,
                deck_id: DEFAULT_DECK_ID,
                sort_field: "Apple".to_owned(),
            },
            BrowseCard {
                card_id: 2,
                deck_id: DEFAULT_DECK_ID,
                sort_field: "Apple".to_owned(),
            },
        ];
        sort_browse_cards(&mut cards);
        let ids = cards.iter().map(|card| card.card_id).collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2, 3]);
    }
    #[test]
    fn browser_rows_prepare_tags_for_the_table() {
        let deck = Deck {
            id: DEFAULT_DECK_ID,
            name: "Default".to_owned(),
            description: String::new(),
        };
        let note = Note {
            id: 6,
            guid: String::new(),
            model_id: DEFAULT_MODEL_ID,
            modified: 0,
            usn: 0,
            tags: vec!["spanish".to_owned(), "verb".to_owned()],
            fields: vec!["Hola".to_owned(), "Hello".to_owned()],
            sort_field: "Hola".to_owned(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        let row = browser_row(
            &card_with_id(6, CardQueue::New, 0),
            &note,
            std::slice::from_ref(&deck),
            std::slice::from_ref(&model()),
        )
        .expect("browser row");
        assert_eq!(row.tags, "spanish, verb");
    }

    #[test]
    fn browser_rows_prepare_unique_element_keys() {
        let deck = Deck {
            id: DEFAULT_DECK_ID,
            name: "Default".to_owned(),
            description: String::new(),
        };
        let model = model();
        let mut rows = Vec::new();
        for id in [7, 3, 11] {
            let note = Note {
                id,
                guid: String::new(),
                model_id: DEFAULT_MODEL_ID,
                modified: 0,
                usn: 0,
                tags: Vec::new(),
                fields: vec![format!("Question {id}"), "Answer".to_owned()],
                sort_field: String::new(),
                checksum: 0,
                flags: 0,
                data: String::new(),
            };
            rows.push(
                browser_row(
                    &card_with_id(id, CardQueue::New, 0),
                    &note,
                    std::slice::from_ref(&deck),
                    std::slice::from_ref(&model),
                )
                .expect("browser row"),
            );
        }
        let row_keys =
            rows.iter().map(|row| row.key.as_str()).collect::<Vec<_>>();
        let menu_keys = rows
            .iter()
            .map(|row| row.menu_key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(row_keys, ["browse-row-7", "browse-row-3", "browse-row-11"]);
        assert_eq!(
            menu_keys,
            ["browse-menu-7", "browse-menu-3", "browse-menu-11"]
        );
    }

    #[test]
    fn cloze_cards_show_the_active_cloze_on_the_front() {
        let deck = Deck {
            id: DEFAULT_DECK_ID,
            name: "Default".to_owned(),
            description: String::new(),
        };
        let mut model = StockModelKind::Cloze.definition();
        model.id = DEFAULT_MODEL_ID;
        let note = Note {
            id: 4,
            guid: String::new(),
            model_id: DEFAULT_MODEL_ID,
            modified: 0,
            usn: 0,
            tags: Vec::new(),
            fields: vec![
                "The {{c1::capital}} is Paris.".to_owned(),
                "Remember it".to_owned(),
            ],
            sort_field: String::new(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        let row = browser_row(
            &card_with_id(4, CardQueue::New, 0),
            &note,
            std::slice::from_ref(&deck),
            std::slice::from_ref(&model),
        )
        .unwrap();
        assert_eq!(row.front, "The [...] is Paris.");
    }

    #[test]
    fn browser_rows_keep_image_media_for_preview() {
        let deck = Deck {
            id: DEFAULT_DECK_ID,
            name: "Default".to_owned(),
            description: String::new(),
        };
        let note = Note {
            id: 5,
            guid: String::new(),
            model_id: DEFAULT_MODEL_ID,
            modified: 0,
            usn: 0,
            tags: Vec::new(),
            fields: vec![
                "<img src=\"Abkhazia.png\" />".to_owned(),
                "Abkhazia".to_owned(),
            ],
            sort_field: String::new(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        let row = browser_row(
            &card_with_id(5, CardQueue::New, 0),
            &note,
            std::slice::from_ref(&deck),
            std::slice::from_ref(&model()),
        )
        .unwrap();
        assert_eq!(row.front, "Abkhazia.png");
        assert_eq!(row.front_media.len(), 1);
        assert_eq!(
            row.front_media.first().map(|media| media.name.as_str()),
            Some("Abkhazia.png")
        );
        assert_eq!(row.back_media.len(), 0);
    }
    #[test]
    fn browse_window_keeps_only_nearby_lines() {
        let entries = (0..1_000)
            .map(|index| {
                if index % 5 == 0 {
                    BrowseEntry::Group(BrowseGroup {
                        path: format!("Deck {index}"),
                        count: 1,
                        has_children: false,
                        expanded: true,
                        key: SharedString::from(format!("group-{index}")),
                        chevron_key: SharedString::from(format!(
                            "chevron-{index}"
                        )),
                    })
                } else {
                    BrowseEntry::Card { row: index }
                }
            })
            .collect::<Vec<_>>();
        let height = |range: std::ops::Range<usize>| {
            entries[range]
                .iter()
                .map(|entry| match entry {
                    BrowseEntry::Group(_) => GROUP_HEIGHT,
                    BrowseEntry::Card { .. } => CARD_HEIGHT,
                })
                .sum::<f32>()
        };

        let top = browse_visible_window(&entries, 0.0, 520.0, 40.0);
        assert_eq!(top.first, 0);
        assert!(top.last > 0 && top.last < entries.len());
        assert_eq!(top.before, 0.0);
        assert!((top.height - top.after - height(0..top.last)).abs() < 0.01);

        let deep = browse_visible_window(&entries, 20_000.0, 520.0, 40.0);
        assert!(deep.first > 0);
        assert!(deep.last > deep.first && deep.last < entries.len());
        assert!((deep.before - height(0..deep.first)).abs() < 0.01);
        assert!(
            (deep.height
                - deep.before
                - deep.after
                - height(deep.first..deep.last))
            .abs()
                < 0.01
        );

        let all = browse_visible_window(&entries, 0.0, 100_000.0, 40.0);
        assert_eq!((all.first, all.last), (0, entries.len()));
        assert_eq!((all.before, all.after), (0.0, 0.0));

        let past = browse_visible_window(&entries, 100_000.0, 520.0, 40.0);
        assert_eq!((past.first, past.last), (entries.len(), entries.len()));
        assert_eq!(past.after, 0.0);
        assert!((past.before - past.height).abs() < 0.01);
    }
}
