use gpui::AppContext as _;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use ranki_model::Store as _;
use ranki_model::{
    Card, CardQueue, CardTemplate, CardType, Deck, DeckStats, Id, NewNote,
    Note, NoteKind, NoteModel, ReviewDecision, StockModelKind, StoreError,
};

use crate::Options;

use super::{
    DeckRow, EmptyKind, MaskShape, MouseButton, NormalizedRect, OcclusionDrag,
    OcclusionMask, OcclusionState, Phase, ToastKind, UiAction, View,
    build_deck_rows, collect_tags, compute_visible_deck_rows, deck_meta,
    due_total, duration_label, empty_copy, empty_kind, filter_decks,
    focused_delta, forecast_bar_height, forecast_total, model_index,
    models_with_stock, next_due_deck, next_view, prepare_deck_rows,
    queue_counts, refreshed_card, session_progress, suggest_tags,
    sum_leaf_counts, validate_draft, validate_occlusion,
};
use gpui::{SharedString, point, px, size};

struct ModalStore;

impl ranki_model::Store for ModalStore {
    fn decks(&self) -> Result<Vec<Deck>, StoreError> {
        Ok(vec![Deck {
            id: 1,
            name: String::from("Default"),
            description: String::new(),
        }])
    }

    fn create_deck(&mut self, name: &str) -> Result<Deck, StoreError> {
        if name == "taken" {
            return Err(StoreError::InvalidData(String::from(
                "deck already exists: taken",
            )));
        }
        Ok(Deck {
            id: 2,
            name: name.to_owned(),
            description: String::new(),
        })
    }

    fn models(&self) -> Result<Vec<NoteModel>, StoreError> {
        Ok(ranki_model::stock_models())
    }

    fn tags(&self) -> Result<Vec<String>, StoreError> {
        Ok(["japan", "japanese", "verb", "noun"]
            .iter()
            .map(|tag| (*tag).to_owned())
            .collect())
    }

    fn find_cards(&self, _: &str) -> Result<Vec<Id>, StoreError> {
        Ok(Vec::new())
    }

    fn browse_cards(
        &self,
        _: &str,
    ) -> Result<Vec<ranki_model::BrowseCard>, StoreError> {
        Ok(Vec::new())
    }

    fn due_cards(
        &self,
        _: Option<Id>,
        _: usize,
        _: i64,
    ) -> Result<Vec<Card>, StoreError> {
        Ok(Vec::new())
    }

    fn card(&self, _: Id) -> Result<Option<Card>, StoreError> {
        Ok(None)
    }

    fn note(&self, _: Id) -> Result<Option<Note>, StoreError> {
        Ok(None)
    }

    fn add_note(&mut self, _: &NewNote) -> Result<Vec<Id>, StoreError> {
        Ok(Vec::new())
    }

    fn apply_review(&mut self, _: &ReviewDecision) -> Result<Id, StoreError> {
        Ok(0)
    }

    fn stats(
        &self,
        _: Option<Id>,
        _: i64,
    ) -> Result<ranki_model::DeckStats, StoreError> {
        Ok(ranki_model::DeckStats::default())
    }

    fn collection_modified(&self) -> Result<i64, StoreError> {
        Ok(0)
    }

    fn collection_created(&self) -> Result<i64, StoreError> {
        Ok(0)
    }
}

struct BrowseStore {
    decks: Vec<Deck>,
    models: Vec<NoteModel>,
    cards: HashMap<Id, Card>,
    notes: HashMap<Id, Note>,
    ids: Vec<Id>,
    card_reads: Arc<AtomicUsize>,
    find_reads: Arc<AtomicUsize>,
    fail_reads: bool,
    fail_browse: bool,
}

impl BrowseStore {
    /// Adds one card with its note and returns its id.
    fn push_card(&mut self, deck_id: Id) -> Id {
        let index = self.ids.len();
        let id = i64::try_from(index + 1).expect("id");
        self.ids.push(id);
        let _ = self.cards.insert(
            id,
            Card {
                id,
                note_id: id,
                deck_id,
                ordinal: 0,
                modified: 0,
                usn: 0,
                card_type: CardType::New,
                queue: CardQueue::New,
                due: 0,
                interval: 0,
                factor: 0,
                reps: 0,
                lapses: 0,
                left: 0,
                original_due: 0,
                original_deck_id: 0,
                flags: 0,
                data: String::new(),
            },
        );
        let _ = self.notes.insert(
            id,
            Note {
                id,
                guid: String::new(),
                model_id: 0,
                modified: 0,
                usn: 0,
                tags: Vec::new(),
                fields: vec![format!("Question {index}"), "Answer".into()],
                sort_field: String::new(),
                checksum: 0,
                flags: 0,
                data: String::new(),
            },
        );
        id
    }

    fn with_cards(count: usize) -> Self {
        let mut store = Self {
            decks: vec![Deck {
                id: 10,
                name: String::from("Default"),
                description: String::new(),
            }],
            models: ranki_model::stock_models(),
            cards: HashMap::new(),
            notes: HashMap::new(),
            ids: Vec::new(),
            card_reads: Arc::new(AtomicUsize::new(0)),
            find_reads: Arc::new(AtomicUsize::new(0)),
            fail_browse: false,
            fail_reads: false,
        };
        for _ in 0..count {
            let _ = store.push_card(10);
        }
        store
    }
}

impl ranki_model::Store for BrowseStore {
    fn decks(&self) -> Result<Vec<Deck>, StoreError> {
        Ok(self.decks.clone())
    }

    fn models(&self) -> Result<Vec<NoteModel>, StoreError> {
        Ok(self.models.clone())
    }

    fn find_cards(&self, _: &str) -> Result<Vec<Id>, StoreError> {
        Ok(self.ids.clone())
    }

    fn card(&self, id: Id) -> Result<Option<Card>, StoreError> {
        let _ = self.card_reads.fetch_add(1, Ordering::Relaxed);
        if self.fail_reads {
            return Err(StoreError::Backend(String::from("disk is on fire")));
        }
        Ok(self.cards.get(&id).cloned())
    }

    fn note(&self, id: Id) -> Result<Option<Note>, StoreError> {
        Ok(self.notes.get(&id).cloned())
    }

    fn create_deck(&mut self, name: &str) -> Result<Deck, StoreError> {
        ModalStore.create_deck(name)
    }

    fn browse_cards(
        &self,
        query: &str,
    ) -> Result<Vec<ranki_model::BrowseCard>, StoreError> {
        self.find_reads.fetch_add(1, Ordering::Relaxed);
        if self.fail_browse {
            return Err(StoreError::Backend(String::from(
                "search index is offline",
            )));
        }
        let ids = match query {
            "none" => &[][..],
            "one" => &self.ids[..self.ids.len().min(1)],
            _ => &self.ids[..],
        };
        Ok(ids
            .iter()
            .filter_map(|id| self.cards.get(id))
            .map(|card| ranki_model::BrowseCard {
                card_id: card.id,
                deck_id: card.deck_id,
                sort_field: String::new(),
            })
            .collect())
    }

    fn due_cards(
        &self,
        _: Option<Id>,
        _: usize,
        _: i64,
    ) -> Result<Vec<Card>, StoreError> {
        Ok(Vec::new())
    }

    fn add_note(&mut self, _: &NewNote) -> Result<Vec<Id>, StoreError> {
        Ok(Vec::new())
    }

    fn apply_review(&mut self, _: &ReviewDecision) -> Result<Id, StoreError> {
        Ok(0)
    }

    fn stats(
        &self,
        _: Option<Id>,
        _: i64,
    ) -> Result<ranki_model::DeckStats, StoreError> {
        Ok(ranki_model::DeckStats::default())
    }

    fn collection_modified(&self) -> Result<i64, StoreError> {
        Ok(0)
    }

    fn collection_created(&self) -> Result<i64, StoreError> {
        Ok(0)
    }
}

fn browser_test_row(front: String) -> crate::browser_view::BrowserRow {
    let card = Card {
        id: 1,
        note_id: 1,
        deck_id: 1,
        ordinal: 0,
        modified: 0,
        usn: 0,
        card_type: CardType::New,
        queue: CardQueue::New,
        due: 0,
        interval: 0,
        factor: 0,
        reps: 0,
        lapses: 0,
        left: 0,
        original_due: 0,
        original_deck_id: 0,
        flags: 0,
        data: String::new(),
    };
    let note = Note {
        id: 1,
        guid: String::new(),
        model_id: 0,
        modified: 0,
        usn: 0,
        tags: Vec::new(),
        fields: vec![front, String::from("Answer!")],
        sort_field: String::new(),
        checksum: 0,
        flags: 0,
        data: String::new(),
    };
    let decks = [Deck {
        id: 1,
        name: String::from("Default"),
        description: String::new(),
    }];
    crate::browser_view::browser_row(
        &card,
        &note,
        &decks,
        &ranki_model::stock_models(),
    )
    .expect("browser row")
}

#[gpui::test]
fn browse_panels_fill_the_window_for_any_card(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));

    let mut panel_heights = |front: &str| {
        cx.update_entity(&root, |root, cx| {
            root.view = View::Browse;
            root.browse.rows = [(1, browser_test_row(front.to_owned()))]
                .into_iter()
                .collect();
            root.browse.selected = Some(1);
            cx.notify();
        });
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
            root.clone()
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            cx.debug_bounds("browse-table").expect("table").size.height,
            cx.debug_bounds("browse-detail")
                .expect("detail")
                .size
                .height,
        )
    };

    let short = panel_heights("Question?");
    let long = panel_heights(&"word ".repeat(300));
    assert_eq!(short, long);
    assert!(
        short.0 > px(600.),
        "table should fill the window: {short:?}"
    );
    assert!(
        short.1 > px(600.),
        "preview should fill the window: {short:?}"
    );
}

#[gpui::test]
fn browse_loads_cards_past_the_first_page(cx: &mut gpui::TestAppContext) {
    let mut store = BrowseStore {
        decks: vec![
            Deck {
                id: 10,
                name: String::from("WorldFlags"),
                description: String::new(),
            },
            Deck {
                id: 11,
                name: String::from("personal"),
                description: String::new(),
            },
        ],
        models: ranki_model::stock_models(),
        cards: HashMap::new(),
        notes: HashMap::new(),
        ids: Vec::new(),
        card_reads: Arc::new(AtomicUsize::new(0)),
        find_reads: Arc::new(AtomicUsize::new(0)),
        fail_browse: false,
        fail_reads: false,
    };
    for (deck_id, count) in [(10, 201), (11, 1)] {
        for _ in 0..count {
            let _ = store.push_card(deck_id);
        }
    }
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, super::Root::refresh_browse);
    cx.update_entity(&root, |root, _| {
        assert_eq!(root.browse.hits.len(), 202);
        assert!(root.browse.rows.is_empty());
        let groups = root
            .browse
            .entries
            .iter()
            .filter_map(|entry| match entry {
                crate::browser_view::BrowseEntry::Group(group) => {
                    Some((group.path.as_str(), group.count))
                }
                crate::browser_view::BrowseEntry::Card { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(groups, [("WorldFlags", 201), ("personal", 1)]);
    });
}

#[gpui::test]
fn browse_reads_only_the_cards_it_shows(cx: &mut gpui::TestAppContext) {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut store = BrowseStore {
        decks: vec![Deck {
            id: 10,
            name: String::from("Default"),
            description: String::new(),
        }],
        models: ranki_model::stock_models(),
        cards: HashMap::new(),
        notes: HashMap::new(),
        ids: Vec::new(),
        card_reads: Arc::clone(&reads),
        find_reads: Arc::new(AtomicUsize::new(0)),
        fail_browse: false,
        fail_reads: false,
    };
    for _ in 0..2_000 {
        let _ = store.push_card(10);
    }
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "opening browse must not read cards"
    );

    cx.update_entity(&root, |root, cx| {
        root.view = View::Browse;
        let _ = root.browse.expanded.insert(String::from("Default"));
        root.rebuild_browse_entries();
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let visible = reads.load(Ordering::Relaxed);
    assert!(
        visible > 0 && visible < 64,
        "only the visible lines are read: {visible}"
    );
    assert!(
        cx.debug_bounds("browse-row-2000").is_none(),
        "offscreen cards must not be built"
    );
    assert!(cx.debug_bounds("browse-row-1").is_some());

    cx.update_entity(&root, |root, cx| {
        root.browse_smooth
            .handle()
            .set_offset(point(px(0.), px(-78_100.)));
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let scrolled = reads.load(Ordering::Relaxed);
    assert!(
        scrolled > visible && scrolled < visible + 128,
        "scrolling reads the lines it reveals: {scrolled}"
    );
    assert!(
        cx.debug_bounds("browse-row-1500").is_some(),
        "a card deep in the table must appear when scrolled down"
    );
}

#[gpui::test]
fn browse_rows_fill_the_table_at_the_bottom(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(BrowseStore::with_cards(2_000)),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1400.), px(900.)));
    cx.update_entity(&root, |root, cx| {
        root.view = View::Browse;
        let _ = root.browse.expanded.insert(String::from("Default"));
        root.rebuild_browse_entries();
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let max = cx.read_entity(&root, |root, _| {
        f32::from(root.browse_smooth.handle().max_offset().height)
    });
    cx.update_entity(&root, |root, _| {
        root.browse_smooth
            .handle()
            .set_offset(point(px(0.), px(-max)));
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let row = cx
        .debug_bounds("browse-row-2000")
        .expect("the last card at the bottom");
    let table = cx.debug_bounds("browse-table").expect("browse table");
    assert_eq!(row.size.height, px(52.0), "rows must not shrink in flex");
    assert!(
        row.bottom() >= table.bottom() - px(2.0),
        "the last row must reach the bottom of the table"
    );
}

#[gpui::test]
fn create_deck_modal_closes_only_on_outside_click(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    cx.dispatch_action(super::CreateDeck);
    cx.run_until_parked();
    assert!(cx.update(|_, app| root.read(app).create_modal));

    let center = cx.update(|window, _| {
        let size = window.viewport_size();
        point(
            px(f32::from(size.width) / 2.),
            px(f32::from(size.height) / 2.),
        )
    });
    cx.simulate_click(center, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.update(|_, app| root.read(app).create_modal),
        "clicking inside the modal must keep it open"
    );

    cx.simulate_click(point(px(4.), px(4.)), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        !cx.update(|_, app| root.read(app).create_modal),
        "clicking outside the modal must close it"
    );
}
#[gpui::test]
fn escape_closes_the_create_deck_modal(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    cx.dispatch_action(super::CreateDeck);
    cx.run_until_parked();
    assert!(cx.update(|_, app| root.read(app).create_modal));

    cx.simulate_keystrokes("escape");

    assert!(!cx.update(|_, app| root.read(app).create_modal));
}
#[gpui::test]
fn opening_statistics_uses_the_cached_summary(cx: &mut gpui::TestAppContext) {
    let reads = Arc::new(AtomicUsize::new(0));
    let modified = Arc::new(AtomicI64::new(7));
    let mut options = Options::default();
    options.paths.collection =
        std::path::PathBuf::from("no-such-collection.anki2");
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(StatisticsStore {
                    reads: Arc::clone(&reads),
                    modified: Arc::clone(&modified),
                    fail: Arc::new(AtomicBool::new(false)),
                }),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    let before = reads.load(Ordering::Relaxed);
    assert!(before > 0, "startup builds the cached summary");

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::OpenStatistics, window, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(reads.load(Ordering::Relaxed), before);
    assert_eq!(cx.update(|_, app| root.read(app).view), View::Statistics);
    assert!(cx.update(|_, app| root.read(app).toast.is_none()));
}
#[gpui::test]
fn create_deck_error_lives_by_the_input_until_the_name_changes(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();

    cx.update(|window, app| {
        root.update(app, |root, cx| {
            root.open_create_modal(window, cx);
            root.create_field.update(cx, |field, cx| {
                field.set_text("taken", cx);
            });
        });
    });
    cx.run_until_parked();

    cx.update(|window, app| {
        root.update(app, |root, cx| root.create_deck(window, cx));
    });
    cx.run_until_parked();

    let error = cx.update(|_, app| root.read(app).create_error.clone());
    assert_eq!(
        error.as_deref(),
        Some("Could not create deck: invalid data: deck already exists: taken")
    );
    assert!(cx.update(|_, app| root.read(app).toast.is_none()));

    cx.update(|_, app| {
        root.update(app, |root, cx| {
            root.create_field.update(cx, |field, cx| {
                field.set_text("taken again", cx);
            });
        });
    });
    cx.run_until_parked();
    assert!(cx.update(|_, app| root.read(app).create_error.is_none()));
}

#[gpui::test]
fn dragging_the_divider_resizes_the_rail_between_icons_and_the_cap(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let press = |cx: &mut gpui::VisualTestContext| {
        let bar = cx.debug_bounds("nav-divider").expect("divider bounds");
        assert_eq!(bar.size.width, px(crate::theme::NAV_DIVIDER_WIDTH));
        bar.center()
    };
    let width = |cx: &mut gpui::VisualTestContext| {
        cx.read_entity(&root, |root, _| root.nav_width)
    };

    let start = press(cx);
    assert_eq!(width(cx), crate::theme::NAV_WIDTH);
    let middle = crate::theme::NAV_DIVIDER_WIDTH / 2.0;
    assert_eq!(start.x, px(crate::theme::NAV_WIDTH - middle));

    mouse_drag(
        cx,
        start,
        point(start.x + px(40.), start.y),
        gpui::MouseButton::Left,
    );
    assert_eq!(width(cx), crate::theme::NAV_WIDTH + 40.);

    let start = press(cx);
    mouse_drag(
        cx,
        start,
        point(start.x + px(400.), start.y),
        gpui::MouseButton::Left,
    );
    assert_eq!(width(cx), crate::theme::NAV_WIDTH_MAX);
    assert!(!cx.read_entity(&root, |root, _| root.nav_compact()));

    let start = press(cx);
    mouse_drag(cx, start, point(px(10.), start.y), gpui::MouseButton::Left);
    assert_eq!(width(cx), crate::theme::NAV_WIDTH_MIN);
    assert!(cx.read_entity(&root, |root, _| root.nav_compact()));
    assert_eq!(press(cx).x, px(crate::theme::NAV_WIDTH_MIN - middle));
}

#[gpui::test]
fn phone_width_switches_to_bottom_navigation(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(390.), px(844.)));
    cx.draw(point(px(0.), px(0.)), size(px(390.), px(844.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert!(cx.debug_bounds("mobile-nav").is_some());
    assert!(cx.debug_bounds("nav-divider").is_none());
    let tab = cx.debug_bounds("nav-Browse").expect("browse tab");
    assert!(tab.size.height >= px(44.));
    assert!(tab.size.width >= px(44.));
    assert!(tab.bottom() <= px(844.));

    cx.simulate_click(tab.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.read_entity(&root, |root, _| root.view),
        super::View::Browse
    );

    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("nav-divider").is_some());
}

#[gpui::test]
fn deck_picker_floats_above_the_form(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        cx.notify();
    });
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let button = cx
        .debug_bounds("add-deck-picker")
        .expect("deck picker button bounds");

    let center = button.center();
    cx.simulate_mouse_move(center, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(
        center,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(
        center,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let after = cx
        .debug_bounds("add-deck-picker")
        .expect("deck picker button bounds after opening");
    assert_eq!(button.origin, after.origin);
    assert_eq!(button.size, after.size);

    let popup = cx
        .debug_bounds("add-deck-picker-popup")
        .expect("deck picker popup bounds");
    assert!(
        popup.size.width <= px(360.),
        "popup was {} wide",
        popup.size.width
    );
    assert!(popup.origin.y >= after.bottom());
}

#[gpui::test]
fn long_deck_names_are_truncated_in_the_picker(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.decks = vec![Deck {
            id: 1,
            name: String::from(
                "rbbk::operating_systems_and_networking_fundamentals",
            ),
            description: String::new(),
        }];
        root.stats = vec![ranki_model::DeckStats::default()];
        root.view = super::View::AddCard;
        root.add.deck_picker = true;
        root.add.deck_picker_anchor = point(px(270.), px(224.));
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let popup = cx
        .debug_bounds("add-deck-picker-popup")
        .expect("deck picker popup bounds");
    let name = cx
        .debug_bounds("add-deck-picker-name")
        .expect("deck picker name bounds");
    let due = cx
        .debug_bounds("add-deck-picker-due")
        .expect("deck picker due bounds");

    assert!(
        name.origin.x + name.size.width <= due.origin.x,
        "name overflowed into the due count"
    );
    assert!(
        due.origin.x + due.size.width <= popup.origin.x + popup.size.width,
        "due count overflowed the popup"
    );
}
#[gpui::test]
fn deck_picker_clicks_do_not_reach_the_editor(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        root.seed_fields("untouched", cx);
        root.add.deck_picker = true;
        root.add.deck_picker_anchor = point(px(270.), px(224.));
        cx.notify();
    });
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let row = cx
        .debug_bounds("add-deck-0")
        .expect("first deck row bounds");
    let bold = cx.debug_bounds("add-format-B").expect("bold button bounds");
    assert!(
        row.intersects(&bold),
        "row {row:?} must overlap bold {bold:?}"
    );
    let click = point(
        px(f32::midpoint(
            f32::from(row.origin.x.max(bold.origin.x)),
            f32::from(row.bottom_right().x.min(bold.bottom_right().x)),
        )),
        px(f32::midpoint(
            f32::from(row.origin.y.max(bold.origin.y)),
            f32::from(row.bottom_right().y.min(bold.bottom_right().y)),
        )),
    );
    cx.simulate_mouse_move(click, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(
        click,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(
        click,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();

    let (text, picker_open) = cx.read_entity(&root, |root, cx| {
        (root.add.fields[0].read(cx).text(), root.add.deck_picker)
    });
    assert!(!picker_open, "selecting a deck must close the picker");
    assert_eq!(text, "untouched");
}

fn click_test_point(
    cx: &mut gpui::VisualTestContext,
    at: gpui::Point<gpui::Pixels>,
) {
    cx.simulate_mouse_move(at, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(
        at,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn mouse_drag(
    cx: &mut gpui::VisualTestContext,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
    button: gpui::MouseButton,
) {
    cx.simulate_mouse_move(from, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(from, button, gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_move(to, Some(button), gpui::Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(to, button, gpui::Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

#[gpui::test]
fn occlusion_canvas_moves_resizes_and_removes_masks(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).expect("media directory");
    image::RgbaImage::new(200, 100)
        .save(media.join("diagram.png"))
        .expect("test image");
    let options = Options {
        paths: ranki_model::DataPaths {
            collection: dir.path().join("collection.anki2"),
            media_dir: media,
            sync_state: dir.path().join("ranki-sync.json"),
        },
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options,
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        let occlusion = root
            .models
            .iter()
            .position(ranki_model::NoteModel::is_image_occlusion)
            .expect("stock occlusion");
        root.select_model(occlusion, cx);
        root.add.occlusion.image = Some(String::from("diagram.png"));
        root.add.occlusion.masks = vec![OcclusionMask {
            shape: MaskShape::Rect,
            rect: NormalizedRect {
                left: 0.2,
                top: 0.2,
                width: 0.4,
                height: 0.4,
            },
        }];
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let bounds = cx.read_entity(&root, |root, _| {
        root.add.occlusion.bounds.expect("canvas bounds")
    });
    let at = |x: f32, y: f32| {
        point(
            px(f32::from(bounds.origin.x) + f32::from(bounds.size.width) * x),
            px(f32::from(bounds.origin.y) + f32::from(bounds.size.height) * y),
        )
    };

    mouse_drag(cx, at(0.3, 0.3), at(0.4, 0.35), gpui::MouseButton::Left);
    let moved =
        cx.read_entity(&root, |root, _| root.add.occlusion.masks[0].rect);
    assert!((moved.left - 0.3).abs() < 1e-5, "{moved:?}");
    assert!((moved.top - 0.25).abs() < 1e-5, "{moved:?}");
    assert_eq!(
        cx.read_entity(&root, |root, _| root.add.occlusion.masks.len()),
        1
    );

    mouse_drag(cx, at(0.35, 0.3), at(0.25, 0.2), gpui::MouseButton::Right);
    let resized =
        cx.read_entity(&root, |root, _| root.add.occlusion.masks[0].rect);
    assert!((resized.left - 0.25).abs() < 1e-5, "{resized:?}");
    assert!((resized.top - 0.2).abs() < 1e-5, "{resized:?}");
    assert!((resized.width - 0.45).abs() < 1e-5, "{resized:?}");
    assert_eq!(
        cx.read_entity(&root, |root, _| root.add.occlusion.masks.len()),
        1
    );

    let click = at(0.475, 0.425);
    cx.simulate_mouse_move(click, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(
        click,
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(
        click,
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(
        cx.read_entity(&root, |root, _| root.add.occlusion.masks.len()),
        0
    );
}

#[gpui::test]
fn deck_picker_dismisses_on_outside_click(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        cx.notify();
    });
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let button = cx
        .debug_bounds("add-deck-picker")
        .expect("deck picker button bounds")
        .center();

    click_test_point(cx, button);
    assert!(cx.read_entity(&root, |root, _| root.add.deck_picker));

    click_test_point(cx, point(px(1100.), px(600.)));
    assert!(!cx.read_entity(&root, |root, _| root.add.deck_picker));

    click_test_point(cx, button);
    assert!(cx.read_entity(&root, |root, _| root.add.deck_picker));

    click_test_point(cx, button);
    assert!(
        !cx.read_entity(&root, |root, _| root.add.deck_picker),
        "the dismissing press must not reopen the picker"
    );
}

#[gpui::test]
fn escape_closes_the_deck_picker(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        root.add.deck_picker = true;
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("escape");

    let state =
        cx.read_entity(&root, |root, _| (root.view, root.add.deck_picker));
    assert_eq!(state.0, super::View::AddCard);
    assert!(!state.1);
}
#[gpui::test]
fn deck_picker_closes_when_the_form_scrolls(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        root.add.deck_picker = true;
        root.add.deck_picker_anchor = point(px(270.), px(224.));
        cx.notify();
    });
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let popup = cx
        .debug_bounds("add-deck-picker-popup")
        .expect("deck picker popup bounds");
    let scroll = |cx: &mut gpui::VisualTestContext, at| {
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-20.))),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();
    };

    scroll(
        cx,
        point(popup.origin.x + px(160.), popup.origin.y + px(70.)),
    );
    assert!(
        cx.read_entity(&root, |root, _| root.add.deck_picker),
        "scrolling the deck list must keep the picker open"
    );

    scroll(cx, point(px(1100.), px(600.)));
    assert!(!cx.read_entity(&root, |root, _| root.add.deck_picker));
}

#[gpui::test]
fn deck_scroll_animates_toward_the_wheel_target(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, _| {
        root.options.reduce_motion = Some(false);
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.decks = (0..200i64)
            .map(|index| Deck {
                id: index.saturating_add(1),
                name: format!("Deck {index:03}"),
                description: String::new(),
            })
            .collect();
        root.stats = vec![DeckStats::default(); root.decks.len()];
        root.rebuild_deck_rows();
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("deck-row-Deck 199").is_none(),
        "offscreen decks must not be built"
    );
    let row = cx
        .debug_bounds("deck-row-Deck 000")
        .expect("first deck row")
        .center();
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: row,
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-240.))),
        modifiers: gpui::Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });

    let mut target =
        cx.read_entity(&root, |root, _| root.decks_scroll.target());

    assert!(target >= 240.0, "wheel step should move the target");

    cx.update_entity(&root, |_, cx| cx.notify());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let first_frame =
        cx.read_entity(&root, |root, _| root.decks_scroll.offset());
    assert!(
        (0.0..=target).contains(&first_frame),
        "one frame should stay within the transition"
    );

    cx.update_entity(&root, |root, cx| {
        root.options.reduce_motion = Some(true);
        root.decks_scroll.wheel(240.0);
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        !cx.read_entity(&root, |root, _| root.decks_scroll.animating()),
        "reduced motion must stop the animation loop"
    );
    cx.update_entity(&root, |root, cx| {
        root.options.reduce_motion = Some(false);
        root.decks_scroll.wheel(240.0);
        cx.notify();
    });
    target = cx.read_entity(&root, |root, _| root.decks_scroll.target());

    cx.update_entity(&root, |root, _| root.decks_scroll.settle());
    let settled = cx.read_entity(&root, |root, _| root.decks_scroll.offset());
    assert_eq!(
        settled, target,
        "the animation should settle exactly ({settled} vs {target})"
    );

    cx.update_entity(&root, |root, cx| {
        root.decks_scroll
            .handle()
            .set_offset(point(px(0.), px(-20_000.)));
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("deck-row-Deck 199").is_some(),
        "the last deck must appear when scrolled to the bottom"
    );
}

#[gpui::test]
fn dragging_the_scrollbar_moves_the_deck_list(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.decks = (0..200i64)
            .map(|index| Deck {
                id: index.saturating_add(1),
                name: format!("Deck {index:03}"),
                description: String::new(),
            })
            .collect();
        root.stats = vec![DeckStats::default(); root.decks.len()];
        root.rebuild_deck_rows();
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let thumb = cx
        .debug_bounds("scrollbar-decks-thumb")
        .expect("deck scrollbar thumb")
        .center();

    cx.simulate_mouse_move(thumb, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(
        thumb,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    let dragged = point(thumb.x, thumb.y + px(80.));
    cx.simulate_mouse_move(
        dragged,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::none(),
    );
    cx.simulate_mouse_up(
        dragged,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );

    let offset = cx.read_entity(&root, |root, _| {
        -f32::from(root.decks_scroll.handle().offset().y)
    });
    assert!(offset > 0.0, "dragging the thumb must scroll the list");
}

#[gpui::test]
fn grabbing_the_thumb_off_center_keeps_the_grab_point(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.decks = (0..200i64)
            .map(|index| Deck {
                id: index.saturating_add(1),
                name: format!("Deck {index:03}"),
                description: String::new(),
            })
            .collect();
        root.stats = vec![DeckStats::default(); root.decks.len()];
        root.rebuild_deck_rows();
        root.decks_scroll
            .handle()
            .set_offset(point(px(0.), px(-1000.)));
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let thumb = cx
        .debug_bounds("scrollbar-decks-thumb")
        .expect("deck scrollbar thumb");
    let track = cx
        .debug_bounds("scrollbar-decks-track")
        .expect("deck scrollbar track");
    let (initial, max_offset) = cx.read_entity(&root, |root, _| {
        (
            -f32::from(root.decks_scroll.handle().offset().y),
            f32::from(root.decks_scroll.handle().max_offset().height),
        )
    });
    let travel = f32::from(track.size.height) - f32::from(thumb.size.height);
    let grab = point(
        thumb.center().x,
        thumb.origin.y + thumb.size.height - px(4.),
    );
    let dragged = point(grab.x, grab.y + px(8.));
    mouse_drag(cx, grab, dragged, gpui::MouseButton::Left);
    let actual = cx.read_entity(&root, |root, _| {
        -f32::from(root.decks_scroll.handle().offset().y)
    });
    let expected = initial + 8.0 / travel * max_offset;
    assert!(
        (actual - expected).abs() < 8.0,
        "off-center grab jumped: {actual} vs {expected}"
    );
}

#[gpui::test]
fn deck_picker_scrollbar_receives_drags(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        root.decks = (0..200i64)
            .map(|index| Deck {
                id: index.saturating_add(1),
                name: format!("Picker deck {index:03}"),
                description: String::new(),
            })
            .collect();
        root.add.deck_picker = true;
        root.add.deck_picker_anchor = point(px(270.), px(224.));
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let thumb = cx
        .debug_bounds("scrollbar-deck-picker-thumb")
        .expect("deck picker scrollbar thumb")
        .center();
    let left = point(thumb.x - px(40.), thumb.y + px(10.));
    cx.simulate_mouse_move(thumb, None, gpui::Modifiers::none());
    cx.simulate_mouse_down(
        thumb,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_move(
        left,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let dragged = point(left.x, left.y + px(20.));
    cx.simulate_mouse_move(
        dragged,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(
        dragged,
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );

    let offset = cx.read_entity(&root, |root, _| {
        -f32::from(root.deck_picker_scroll.offset().y)
    });
    assert!(
        offset > 0.0,
        "dragging must continue after the pointer leaves the strip"
    );

    let track = cx
        .debug_bounds("scrollbar-deck-picker-track")
        .expect("deck picker scrollbar track");
    let before = cx.read_entity(&root, |root, _| root.add.deck_id);
    click_test_point(
        cx,
        point(
            track.center().x,
            track.origin.y + track.size.height - px(8.),
        ),
    );
    let after = cx.read_entity(&root, |root, _| root.add.deck_id);
    assert_eq!(
        after, before,
        "a scrollbar press must not select a deck row"
    );
    assert!(
        cx.read_entity(&root, |root, _| root.add.deck_picker),
        "a scrollbar press must keep the picker open"
    );
}

#[test]
fn session_progress_never_fills_an_empty_queue() {
    assert_eq!(session_progress(0, 0), 0.0);
    assert_eq!(session_progress(0, 20), 0.0);
    assert_eq!(session_progress(10, 20), 0.5);
    assert_eq!(session_progress(20, 20), 1.0);
    assert_eq!(session_progress(21, 20), 1.0);
}

#[gpui::test]
fn repeated_hard_answers_leave_progress_unchanged(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });

    let mut grade = |rating| {
        cx.update_entity(&root, |root, cx| {
            root.review.phase = Phase::Answer;
            root.review.last_grade_at = None;
            root.grade(rating, cx);
        });
        cx.read_entity(&root, |root, _| {
            (root.progress_fraction(), root.review.answered)
        })
    };
    assert_eq!(grade(ranki_model::Rating::Hard), (0.0, 1));
    assert_eq!(grade(ranki_model::Rating::Hard), (0.0, 2));
    assert_eq!(grade(ranki_model::Rating::Easy), (1.0, 3));
}

#[gpui::test]
fn ten_minute_learning_step_waits_and_records_answer_time(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let collection = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&collection).expect("collection");
    let ids: Vec<i64> = ["First?", "Second?"]
        .iter()
        .map(|front| {
            store
                .add_note(&NewNote {
                    model_id: 1,
                    deck_id: 1,
                    fields: vec![(*front).into(), String::from("Answer!")],
                    tags: Vec::new(),
                })
                .expect("note")[0]
        })
        .collect();
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });
    let first = cx
        .read_entity(&root, |root, _| {
            root.review.current.as_ref().map(|card| card.id)
        })
        .expect("current card");
    assert_eq!(first, ids[0]);

    cx.update_entity(&root, |root, cx| {
        root.review.phase = Phase::Answer;
        root.review.last_grade_at = None;
        root.review.focused_elapsed = Duration::from_secs(7);
        root.grade(ranki_model::Rating::Good, cx);
    });

    let reopened = ranki_storage::Collection::open(&collection).expect("open");
    let stepped = reopened.card(first).expect("card").expect("present");
    let now = super::Root::now();
    assert_eq!(stepped.queue, ranki_model::CardQueue::Learning);
    assert!(stepped.due >= now + 500);
    let history = reopened.review_history().expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].time_ms, 7_000);
    assert_eq!(history[0].interval, -600);
    let next = cx
        .read_entity(&root, |root, _| {
            root.review.current.as_ref().map(|card| card.id)
        })
        .expect("next card");
    assert_eq!(next, ids[1]);
}

#[gpui::test]
fn gui_answer_time_uses_the_effective_preset_cap(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let collection = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&collection).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let mut presets = store.presets().expect("presets");
    presets[0].max_taken_secs = 2;
    store.write_preset(&presets[0]).expect("write preset");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });
    cx.update_entity(&root, |root, cx| {
        root.review.phase = Phase::Answer;
        root.review.last_grade_at = None;
        root.review.focused_elapsed = Duration::from_secs(7);
        root.grade(ranki_model::Rating::Good, cx);
    });
    let reopened = ranki_storage::Collection::open(&collection).expect("open");
    let history = reopened.review_history().expect("history");
    assert_eq!(history[0].time_ms, 2_000);
}
/// Grades the last card of a session and reports sync and media runs.
fn session_sync_counts(
    cx: &mut gpui::TestAppContext,
    outcome: ranki_sync::SyncOutcome,
) -> (usize, usize) {
    let settled = outcome.is_settled();
    let dir = tempfile::tempdir().expect("temp dir");
    let collection = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&collection).expect("collection");
    for front in ["First?", "Second?"] {
        let _ = store
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![front.to_owned(), String::from("Answer!")],
                tags: Vec::new(),
            })
            .expect("note");
    }
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        sync: ranki_model::SyncSettings {
            enabled: true,
            ..Default::default()
        },
        ..Options::default()
    };
    let syncs = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&syncs);
    let media = Arc::new(AtomicUsize::new(0));
    let refreshed = Arc::clone(&media);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let mut root = super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        );
        root.sync.signed_in = true;
        root.sync_run = Arc::new(move |_, _, _, _, _| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(outcome.clone())
        });
        root.media_run = Arc::new(move |_, _, _, _, _, _| {
            refreshed.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });
        root
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });

    cx.update_entity(&root, |root, cx| {
        root.review.phase = Phase::Answer;
        root.review.last_grade_at = None;
        root.grade(ranki_model::Rating::Easy, cx);
    });
    cx.run_until_parked();
    assert_eq!(syncs.load(Ordering::Relaxed), 0);

    cx.update_entity(&root, |root, cx| {
        root.review.phase = Phase::Answer;
        root.review.last_grade_at = None;
        root.grade(ranki_model::Rating::Easy, cx);
    });
    cx.run_until_parked();
    let quiet = cx.read_entity(&root, |root, _| root.toast.is_none());
    assert_eq!(quiet, settled);
    (syncs.load(Ordering::Relaxed), media.load(Ordering::Relaxed))
}

#[gpui::test]
fn finishing_a_session_starts_one_quiet_sync(cx: &mut gpui::TestAppContext) {
    let (syncs, media) =
        session_sync_counts(cx, ranki_sync::SyncOutcome::Uploaded);
    assert_eq!(syncs, 1);
    assert_eq!(media, 1);
}

#[gpui::test]
fn a_conflict_does_not_start_a_media_refresh(cx: &mut gpui::TestAppContext) {
    let (syncs, media) = session_sync_counts(
        cx,
        ranki_sync::SyncOutcome::Conflict {
            reason: String::from("both collections changed"),
        },
    );
    assert_eq!(syncs, 1);
    assert_eq!(media, 0);
}
#[test]
fn duration_labels_read_like_a_session_summary() {
    assert_eq!(duration_label(Duration::from_secs(0)), "0s");
    assert_eq!(duration_label(Duration::from_secs(59)), "59s");
    assert_eq!(duration_label(Duration::from_secs(60)), "1m 0s");
    assert_eq!(duration_label(Duration::from_secs(125)), "2m 5s");
    assert_eq!(duration_label(Duration::from_mins(65)), "1h 5m");
}

#[test]
fn queue_counts_include_the_current_card() {
    let make = |id: Id, kind| Card {
        id,
        note_id: 1,
        deck_id: 1,
        ordinal: 0,
        modified: 0,
        usn: 0,
        card_type: kind,
        queue: ranki_model::CardQueue::New,
        due: 0,
        interval: 0,
        factor: 0,
        reps: 0,
        lapses: 0,
        left: 0,
        original_due: 0,
        original_deck_id: 0,
        flags: 0,
        data: String::new(),
    };
    let mut queue = VecDeque::new();
    queue.push_back(make(2, CardType::Learning));
    queue.push_back(make(3, CardType::Review));
    let mut pending = VecDeque::new();
    pending.push_back(make(4, CardType::Learning));
    let stats = queue_counts(Some(&make(1, CardType::New)), &queue, &pending);
    assert_eq!(stats.new_cards, 1);
    assert_eq!(stats.learning_cards, 2);
    assert_eq!(stats.review_cards, 1);
    assert_eq!(stats.total_cards, 4);
    let empty = queue_counts(None, &VecDeque::new(), &VecDeque::new());
    assert_eq!(empty, ranki_model::DeckStats::default());
}

#[test]
fn review_uses_anki_fsrs_defaults() {
    let scheduler = super::from_settings(&Options::default());
    assert_eq!(scheduler.params(), &ranki_core::FsrsParams::default());
    assert_eq!(scheduler.preset().desired_retention, 0.9);
}

#[test]
fn grading_uses_the_stored_card_revision() {
    let current = Card {
        id: 7,
        note_id: 1,
        deck_id: 1,
        ordinal: 0,
        modified: 100,
        usn: -1,
        card_type: CardType::Learning,
        queue: CardQueue::Learning,
        due: 101_000,
        interval: -60,
        factor: 2_500,
        reps: 1,
        lapses: 0,
        left: 1_001,
        original_due: 0,
        original_deck_id: 0,
        flags: 0,
        data: String::new(),
    };
    let mut saved = current.clone();
    saved.usn = 4;
    assert_eq!(refreshed_card(&current, |_| Some(saved.clone())).usn, 4);
    assert_eq!(refreshed_card(&current, |_| None), current);
}

#[test]
fn config_options_keep_their_defaults() {
    let options = Options::default();
    assert_eq!(options.font_size, 16.0);
    assert_eq!(options.desired_retention, 0.9);
    assert_eq!(options.review_batch_size, 20);
    assert!(options.show_keyboard_hints);
}

#[expect(
    clippy::too_many_arguments,
    reason = "test fixture with one argument per queue"
)]
fn deck(
    id: i64,
    name: &str,
    new: usize,
    learning: usize,
    review: usize,
) -> (Deck, ranki_model::DeckStats) {
    (
        Deck {
            id,
            name: name.to_owned(),
            description: String::new(),
        },
        ranki_model::DeckStats {
            new_cards: new,
            learning_cards: learning,
            review_cards: review,
            total_cards: new + learning + review,
        },
    )
}

#[test]
fn deck_tree_builds_stable_indented_rows() {
    let decks = [
        deck(1, "A", 1, 1, 4),
        deck(2, "A::B", 2, 0, 3),
        deck(3, "Z", 0, 0, 0),
    ];
    let stats = decks.iter().map(|(_, stats)| *stats).collect::<Vec<_>>();
    let rows = build_deck_rows(
        &decks.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>(),
        &stats,
        &std::collections::HashSet::default(),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].path, "A");
    assert!(!rows[0].virtual_group);
    assert_eq!(rows[0].stats.review_cards, 4);
    assert_eq!(rows[1].path, "Z");
}

#[test]
fn expanded_tree_shows_children() {
    let decks = [deck(1, "A", 0, 0, 0), deck(2, "A::B", 1, 2, 3)];
    let stats = decks.iter().map(|(_, stats)| *stats).collect::<Vec<_>>();
    let mut expanded = std::collections::HashSet::new();
    let _ = expanded.insert(String::from("A"));
    let rows = build_deck_rows(
        &decks.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>(),
        &stats,
        &expanded,
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].depth, 1);
    assert_eq!(rows[1].name, "B");
}
#[test]
fn review_keys_are_bound_where_they_are_advertised() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let root = [gpui::KeyContext::parse("ranki").expect("ranki context")];
    for (key, expected) in [("enter", 1), ("r", 1), ("f5", 1)] {
        let stroke = gpui::Keystroke::parse(key).expect("keystroke");
        let (bindings, _) = keymap.bindings_for_input(&[stroke], &root);
        assert_eq!(bindings.len(), expected, "{key}");
    }
}

#[gpui::test]
fn space_ends_a_completed_session(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::Review;
        root.review.current = None;
        root.review.answered = 1;
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.simulate_keystrokes("enter");
    assert_eq!(
        cx.read_entity(&root, |root, _| root.view),
        super::View::Review
    );
    cx.simulate_keystrokes("space");
    assert_eq!(
        cx.read_entity(&root, |root, _| root.view),
        super::View::Decks
    );
}

#[gpui::test]
fn space_ends_a_caught_up_session(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_deck(0, window, cx));
    });
    cx.run_until_parked();
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    assert_eq!(cx.update(|_, cx| root.read(cx).view), View::Review);
    assert!(cx.update(|_, cx| root.read(cx).review.current.is_none()));

    cx.simulate_keystrokes("space");
    assert_eq!(cx.update(|_, cx| root.read(cx).view), View::Decks);
}

#[gpui::test]
fn space_after_grading_waits_for_debounce(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.review.current = None;
        root.review.answered = 1;
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update_entity(&root, |root, _| {
        root.review.last_grade_at = Some(std::time::Instant::now());
    });
    cx.simulate_keystrokes("space");
    assert_eq!(cx.update(|_, cx| root.read(cx).view), View::Review);

    cx.update_entity(&root, |root, _| root.review.last_grade_at = None);
    cx.simulate_keystrokes("space");
    assert_eq!(cx.update(|_, cx| root.read(cx).view), View::Decks);
}

#[test]
fn tab_moves_focus_in_and_out_of_text_inputs() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let root = [gpui::KeyContext::parse("ranki").expect("ranki context")];
    let typing = [
        gpui::KeyContext::parse("ranki").expect("ranki context"),
        gpui::KeyContext::parse("ranki_input").expect("input context"),
    ];
    let dialog = [
        gpui::KeyContext::parse("ranki").expect("ranki context"),
        gpui::KeyContext::parse("ranki_dialog").expect("dialog context"),
    ];
    for key in ["tab", "shift-tab"] {
        let stroke = gpui::Keystroke::parse(key).expect("keystroke");
        let (from_root, _) =
            keymap.bindings_for_input(std::slice::from_ref(&stroke), &root);
        let (from_input, _) =
            keymap.bindings_for_input(std::slice::from_ref(&stroke), &typing);
        assert_eq!(from_root.len(), 1, "{key}");
        assert_eq!(from_input.len(), 1, "{key}");
        let (in_dialog, _) =
            keymap.bindings_for_input(std::slice::from_ref(&stroke), &dialog);
        assert!(in_dialog.is_empty(), "{key}");
    }
}

#[test]
fn study_bindings_stay_out_of_text_input() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let typing = [
        gpui::KeyContext::parse("ranki").expect("ranki context"),
        gpui::KeyContext::parse("ranki_input").expect("input context"),
    ];
    let elsewhere = [gpui::KeyContext::parse("ranki").expect("ranki context")];
    let keystroke = gpui::Keystroke::parse("a").expect("keystroke");
    let (in_input, _) =
        keymap.bindings_for_input(std::slice::from_ref(&keystroke), &typing);
    assert!(in_input.is_empty());
    let (outside, _) =
        keymap.bindings_for_input(std::slice::from_ref(&keystroke), &elsewhere);
    assert_eq!(outside.len(), 1);
}

#[gpui::test]
fn typing_in_settings_input_does_not_change_view(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let mut options = Options::default();
    options.sync.enabled = true;
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::Settings;
        cx.notify();
    });
    let focus = cx.read_entity(&root, |root, cx| {
        root.sync.username.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&focus));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("a");

    let (view, text) = cx.read_entity(&root, |root, cx| {
        (root.view, root.sync.username.read(cx).text())
    });
    assert_eq!(view, super::View::Settings);
    assert_eq!(text, "a");
}

#[test]
fn theme_has_no_number_shortcuts() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let settings = [
        gpui::KeyContext::parse("ranki").expect("ranki context"),
        gpui::KeyContext::parse("ranki_settings").expect("settings context"),
    ];
    for key in ["1", "2", "3"] {
        let keystroke = gpui::Keystroke::parse(key).expect("keystroke");
        let (in_settings, _) = keymap
            .bindings_for_input(std::slice::from_ref(&keystroke), &settings);
        assert!(in_settings.is_empty());
    }
}

#[test]
fn statistics_key_is_only_bound_in_statistics() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let keystroke = gpui::Keystroke::parse("t").expect("keystroke");
    let stats = [gpui::KeyContext::parse(View::Statistics.key_context())
        .expect("stats context")];
    let review = [gpui::KeyContext::parse(View::Review.key_context())
        .expect("review context")];
    let typing = [
        gpui::KeyContext::parse(View::Statistics.key_context())
            .expect("stats context"),
        gpui::KeyContext::parse("ranki_input").expect("input context"),
    ];
    let (on_stats, _) =
        keymap.bindings_for_input(std::slice::from_ref(&keystroke), &stats);
    let (in_review, _) =
        keymap.bindings_for_input(std::slice::from_ref(&keystroke), &review);
    let (in_input, _) =
        keymap.bindings_for_input(std::slice::from_ref(&keystroke), &typing);
    assert_eq!(on_stats.len(), 1);
    assert!(in_review.is_empty());
    assert!(in_input.is_empty());
}

#[test]
fn colon_only_deck_names_build_one_row() {
    let decks = [deck(1, ":", 0, 0, 1)];
    let stats = decks.iter().map(|(_, stats)| *stats).collect::<Vec<_>>();
    let rows = build_deck_rows(
        &decks.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>(),
        &stats,
        &std::collections::HashSet::default(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, ":");
}

#[test]
fn child_listed_before_parent_replaces_the_virtual_group() {
    let decks = [deck(1, "A::B", 1, 0, 0), deck(2, "A", 0, 0, 0)];
    let stats = decks.iter().map(|(_, stats)| *stats).collect::<Vec<_>>();
    let mut expanded = std::collections::HashSet::new();
    let _ = expanded.insert(String::from("A"));
    let rows = build_deck_rows(
        &decks.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>(),
        &stats,
        &expanded,
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].path, "A");
    assert!(!rows[0].virtual_group);
    assert_eq!(rows[1].path, "A::B");
}
#[test]
fn leaf_totals_keep_cards_owned_by_a_parent_deck() {
    let decks = [deck(1, "A", 1, 0, 1), deck(2, "A::B", 1, 0, 0)];
    let stats = [
        ranki_model::DeckStats {
            new_cards: 2,
            total_cards: 2,
            ..Default::default()
        },
        ranki_model::DeckStats {
            new_cards: 1,
            total_cards: 1,
            ..Default::default()
        },
    ];
    let deck_list = decks
        .iter()
        .map(|(deck, _)| deck.clone())
        .collect::<Vec<_>>();
    let total = super::leaf_deck_totals(&deck_list, &stats);
    assert_eq!(due_total(total), 2);
    assert_eq!(total.total_cards, 2);
}

#[test]
fn explicit_parent_stats_are_not_added_to_children_twice() {
    let decks = [deck(1, "A", 1, 0, 0), deck(2, "A::B", 1, 0, 0)];
    let stats = [
        ranki_model::DeckStats {
            new_cards: 2,
            total_cards: 2,
            ..Default::default()
        },
        ranki_model::DeckStats {
            new_cards: 1,
            total_cards: 1,
            ..Default::default()
        },
    ];
    let rows = build_deck_rows(
        &decks
            .iter()
            .map(|(deck, _)| deck.clone())
            .collect::<Vec<_>>(),
        &stats,
        &std::collections::HashSet::default(),
    );
    assert_eq!(rows[0].stats.new_cards, 2);
    assert_eq!(rows[0].stats.total_cards, 2);
}

#[test]
fn leaf_sum_does_not_double_count_parents() {
    let decks = [deck(1, "A", 1, 1, 1), deck(2, "A::B", 2, 0, 3)];
    let stats = decks.iter().map(|(_, stats)| *stats).collect::<Vec<_>>();
    let mut expanded = std::collections::HashSet::new();
    let _ = expanded.insert(String::from("A"));
    let rows = build_deck_rows(
        &decks.iter().map(|(d, _)| d.clone()).collect::<Vec<_>>(),
        &stats,
        &expanded,
    );
    let total = sum_leaf_counts(&rows);
    assert_eq!(due_total(total), 5);
}

fn row(id: i64, path: &str, depth: usize) -> DeckRow {
    DeckRow {
        id: Some(id),
        path: path.into(),
        name: path.rsplit("::").next().unwrap_or(path).into(),
        depth,
        stats: ranki_model::DeckStats::default(),
        virtual_group: false,
        has_children: false,
        expanded: false,
        meta: String::new(),
        child_count: 0,
        deck_index: None,
        key: SharedString::default(),
        chevron_key: SharedString::default(),
        study_key: SharedString::default(),
        menu_key: SharedString::default(),
    }
}

#[test]
fn search_keeps_ancestors_and_sorts_stable() {
    let rows = vec![
        row(1, "A", 0),
        row(2, "A::B", 1),
        row(3, "A::B::C", 2),
        row(4, "A::X", 1),
        row(5, "Z", 0),
    ];
    let paths = |query: &str| {
        filter_decks(&rows, query)
            .iter()
            .map(|row| row.path.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(paths("b"), ["A", "A::B", "A::B::C"]);
    assert_eq!(paths("x"), ["A", "A::X"]);
    assert_eq!(paths("nothing"), Vec::<String>::new());
    assert_eq!(paths("").len(), 5);
}

#[test]
fn draft_validation_blocks_empty_and_cloze_without_marker() {
    let (deck, _) = deck(1, "Default", 0, 0, 0);
    let model = NoteModel {
        id: 1,
        name: "Cloze".into(),
        fields: vec!["Text".into()],
        templates: vec![CardTemplate::default()],
        css: String::new(),
        sort_field_index: 0,
        kind: NoteKind::Cloze,
        original_stock_kind: None,
        field_tags: vec![],
        template_ordinals: vec![0],
        raw_config: None,
        raw_json: None,
    };
    assert!(
        validate_draft(&deck, &model, &[String::new()]).contains_key("fields")
    );
    assert!(
        validate_draft(&deck, &model, &[String::from("plain text")])
            .contains_key("fields")
    );
    assert!(
        validate_draft(&deck, &model, &[String::from("{{c1::answer}}")])
            .is_empty()
    );
}

#[test]
fn empty_states_are_distinct() {
    assert_eq!(empty_kind(0, 0), EmptyKind::NoDecks);
    assert_eq!(empty_kind(1, 1), EmptyKind::None);
    assert_eq!(empty_kind(1, 0), EmptyKind::NoMatch);
    assert_eq!(empty_kind(1, 3), EmptyKind::None);
    assert_eq!(empty_copy(EmptyKind::NoDecks).0, "No decks yet");
}

#[test]
fn view_transitions_return_to_decks() {
    assert_eq!(next_view(View::Review, View::Decks), View::Decks);
    assert_eq!(next_view(View::Decks, View::AddCard), View::AddCard);
}

#[test]
fn phase_and_action_are_copy_comparable() {
    assert_ne!(Phase::Question, Phase::Answer);
    assert_ne!(UiAction::OpenAdd, UiAction::OpenDecks);
}

#[test]
fn next_due_deck_skips_selection() {
    let decks = [deck(1, "A", 0, 0, 0), deck(2, "B", 1, 0, 0)];
    let stats = decks.iter().map(|(_, s)| *s).collect::<Vec<_>>();
    let list = decks
        .iter()
        .map(|(deck, _)| deck.clone())
        .collect::<Vec<_>>();
    assert_eq!(next_due_deck(&list, &stats, Some(1)), None);
}

#[test]
fn focused_time_is_capped_and_ignores_blur() {
    let elapsed = std::time::Duration::from_secs(61);
    assert_eq!(
        focused_delta(elapsed, false, std::time::Duration::ZERO),
        std::time::Duration::ZERO
    );
    assert_eq!(
        focused_delta(elapsed, true, std::time::Duration::ZERO),
        std::time::Duration::from_secs(60)
    );
}

#[test]
fn deck_meta_counts_children_and_cards() {
    let row = DeckRow {
        id: None,
        path: "Japanese".into(),
        name: "Japanese".into(),
        depth: 0,
        stats: ranki_model::DeckStats {
            total_cards: 1284,
            ..Default::default()
        },
        virtual_group: true,
        has_children: true,
        expanded: false,
        meta: String::new(),
        child_count: 0,
        deck_index: None,
        key: SharedString::default(),
        chevron_key: SharedString::default(),
        study_key: SharedString::default(),
        menu_key: SharedString::default(),
    };
    let child = DeckRow {
        id: Some(2),
        path: "Japanese::Vocabulary N5".into(),
        name: "Vocabulary N5".into(),
        depth: 1,
        stats: ranki_model::DeckStats::default(),
        virtual_group: false,
        has_children: false,
        expanded: false,
        meta: String::new(),
        child_count: 0,
        deck_index: None,
        key: SharedString::default(),
        chevron_key: SharedString::default(),
        study_key: SharedString::default(),
        menu_key: SharedString::default(),
    };
    assert_eq!(
        deck_meta(&[row.clone(), child], &row),
        "1 subdeck · 1284 cards"
    );
    let solo = DeckRow {
        path: String::from("Solo"),
        stats: ranki_model::DeckStats {
            total_cards: 1,
            ..Default::default()
        },
        ..row
    };
    assert_eq!(deck_meta(std::slice::from_ref(&solo), &solo), "1 card");
}

#[test]
fn deck_rows_cache_display_metadata() {
    let entries = [deck(1, "A", 0, 0, 2), deck(2, "A::B", 0, 0, 3)];
    let decks = entries
        .iter()
        .map(|(deck, _)| deck.clone())
        .collect::<Vec<_>>();
    let stats = entries.iter().map(|(_, stats)| *stats).collect::<Vec<_>>();
    let expanded = ["A".to_owned()].into_iter().collect();
    let mut rows = build_deck_rows(&decks, &stats, &expanded);
    prepare_deck_rows(&decks, &mut rows);
    assert_eq!(rows[0].meta, "1 subdeck · 2 cards");
    assert_eq!(rows[0].child_count, 1);
    assert_eq!(rows[0].deck_index, Some(0));
    assert_eq!(rows[1].meta, "3 cards");
    assert_eq!(rows[1].child_count, 0);
    assert_eq!(rows[1].deck_index, Some(1));

    let filtered = compute_visible_deck_rows(&decks, &stats, &rows, "B");
    assert_eq!(filtered[0].path, "A");
    assert_eq!(filtered[0].meta, rows[0].meta);
    assert_eq!(filtered[0].child_count, rows[0].child_count);
    assert_eq!(filtered[0].deck_index, rows[0].deck_index);
    assert_eq!(filtered[1].meta, rows[1].meta);
}

#[test]
fn deck_element_keys_stay_stable_through_tree_changes() {
    let prepare = |decks: &[Deck],
                   stats: &[ranki_model::DeckStats],
                   expanded: &[&str]| {
        let expanded = expanded
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<std::collections::HashSet<_>>();
        let mut rows = build_deck_rows(decks, stats, &expanded);
        prepare_deck_rows(decks, &mut rows);
        rows
    };
    let check_unique = |rows: &[DeckRow]| {
        let mut paths = std::collections::HashSet::new();
        let mut chevrons = std::collections::HashSet::new();
        let mut studies = std::collections::HashSet::new();
        let mut menus = std::collections::HashSet::new();
        for row in rows {
            assert!(paths.insert(row.key.as_str()), "duplicate row key");
            assert!(
                chevrons.insert(row.chevron_key.as_str()),
                "duplicate chevron key"
            );
            assert!(
                studies.insert(row.study_key.as_str()),
                "duplicate study key"
            );
            if row.deck_index.is_some() {
                assert!(
                    menus.insert(row.menu_key.as_str()),
                    "duplicate menu key"
                );
            }
        }
    };
    let key = |rows: &[DeckRow], path: &str| {
        rows.iter()
            .find(|row| row.path == path)
            .expect("deck row")
            .key
            .to_string()
    };

    let entries = [
        deck(1, "Topic", 0, 0, 2),
        deck(2, "Topic::Child", 0, 0, 3),
        deck(3, "Zed", 0, 0, 1),
    ];
    let decks = entries
        .iter()
        .map(|(deck, _)| deck.clone())
        .collect::<Vec<_>>();
    let stats = entries.iter().map(|(_, stats)| *stats).collect::<Vec<_>>();
    let collapsed = prepare(&decks, &stats, &[]);
    let expanded = prepare(&decks, &stats, &["Topic"]);
    check_unique(&collapsed);
    check_unique(&expanded);
    assert_eq!(key(&collapsed, "Topic"), key(&expanded, "Topic"));
    assert_eq!(key(&collapsed, "Zed"), key(&expanded, "Zed"));

    let visible = compute_visible_deck_rows(&decks, &stats, &expanded, "Child");
    check_unique(&visible);
    let paths = visible
        .iter()
        .map(|row| row.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["Topic", "Topic::Child"]);
    assert_eq!(key(&visible, "Topic"), key(&expanded, "Topic"));
    assert_eq!(
        key(&visible, "Topic::Child"),
        key(&expanded, "Topic::Child")
    );

    let renamed_entries = [
        deck(1, "Topic", 0, 0, 2),
        deck(2, "Topic::Renamed", 0, 0, 3),
        deck(3, "Zed", 0, 0, 1),
    ];
    let renamed_decks = renamed_entries
        .iter()
        .map(|(deck, _)| deck.clone())
        .collect::<Vec<_>>();
    let renamed_stats = renamed_entries
        .iter()
        .map(|(_, stats)| *stats)
        .collect::<Vec<_>>();
    let renamed = prepare(&renamed_decks, &renamed_stats, &["Topic"]);
    check_unique(&renamed);
    assert_eq!(key(&renamed, "Zed"), key(&expanded, "Zed"));
    assert_eq!(key(&renamed, "Topic"), key(&expanded, "Topic"));
    assert_ne!(
        key(&renamed, "Topic::Renamed"),
        key(&expanded, "Topic::Child")
    );
    assert_eq!(key(&renamed, "Topic::Renamed"), "deck-row-Topic::Renamed");
    assert_eq!(
        renamed
            .iter()
            .find(|row| row.path == "Topic::Renamed")
            .expect("renamed row")
            .menu_key
            .as_str(),
        "decks-menu-2"
    );
}

#[test]
fn due_totals_add_all_queues() {
    let (_, stats) = deck(1, "A", 2, 3, 4);
    assert_eq!(due_total(stats), 9);
}

#[test]
fn forecast_bars_scale_to_the_busiest_day() {
    let days = [5, 2, 0, 3, 1, 0, 4];
    assert_eq!(forecast_total(&days), 15);
    assert_eq!(forecast_total(&[]), 0);
    assert_eq!(forecast_bar_height(0, 5), 6.0);
    assert_eq!(forecast_bar_height(2, 0), 6.0);
    assert_eq!(forecast_bar_height(1, 4), 28.5);
    assert_eq!(forecast_bar_height(4, 4), 60.0);
}

#[test]
fn stock_types_fill_in_missing_note_models() {
    let models = models_with_stock(Vec::new());
    let names = models
        .iter()
        .map(|model| model.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "Basic",
            "Basic (and reversed card)",
            "Cloze",
            "Image Occlusion"
        ]
    );
    assert!(models.iter().all(|model| model.id == 0));
    let installed = ranki_model::stock_models()
        .into_iter()
        .next()
        .map(|mut model| {
            model.id = 7;
            model
        })
        .expect("stock basic");
    let merged = models_with_stock(vec![installed]);
    assert_eq!(merged.len(), 4);
    assert_eq!(merged[0].id, 7);
}

#[test]
fn occlusion_drag_creates_one_mask_on_release() {
    let mut state = OcclusionState {
        bounds: Some(gpui::Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(200.), px(100.)),
        }),
        ..OcclusionState::default()
    };
    let start = state.pointer((125., 75.)).expect("start point");
    let end = state.pointer((175., 125.)).expect("end point");
    state.drag = Some(OcclusionDrag::Draw { start, end });
    let rect = state.drag_rect().expect("drag preview");
    assert_eq!((rect.left, rect.top), (0.125, 0.25));
    assert_eq!((rect.width, rect.height), (0.25, 0.5));
    state.finish_drag();
    assert_eq!(state.masks.len(), 1);
    assert_eq!(state.drag, None);
}

#[test]
fn left_drag_moves_the_mask_under_the_press() {
    let mut state = OcclusionState {
        image: Some("diagram.png".into()),
        bounds: Some(gpui::Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(200.), px(100.)),
        }),
        masks: vec![OcclusionMask {
            shape: MaskShape::Rect,
            rect: NormalizedRect {
                left: 0.2,
                top: 0.2,
                width: 0.3,
                height: 0.3,
            },
        }],
        ..OcclusionState::default()
    };
    assert!(state.press((160., 80.), MouseButton::Left));
    assert!(matches!(
        state.drag,
        Some(OcclusionDrag::Move { index: 0, .. })
    ));
    assert!(state.drag_to((180., 95.)));
    let moved = state.masks[0].rect;
    assert_eq!((moved.left, moved.width, moved.height), (0.3, 0.3, 0.3));
    assert!((moved.top - 0.35).abs() < 1e-5, "{moved:?}");
    assert!(!state.finish_drag());
    assert_eq!(state.masks.len(), 1);
}

#[test]
fn right_drag_resizes_from_the_pressed_corner() {
    let mut state = OcclusionState {
        image: Some("diagram.png".into()),
        bounds: Some(gpui::Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(200.), px(100.)),
        }),
        masks: vec![OcclusionMask {
            shape: MaskShape::Rect,
            rect: NormalizedRect {
                left: 0.2,
                top: 0.2,
                width: 0.4,
                height: 0.4,
            },
        }],
        ..OcclusionState::default()
    };
    assert!(state.press((150., 75.), MouseButton::Right));
    assert!(matches!(
        state.drag,
        Some(OcclusionDrag::Resize { index: 0, .. })
    ));
    assert!(state.drag_to((120., 60.)));
    let resized = state.masks[0].rect;
    assert!((resized.left - 0.1).abs() < 1e-5, "{resized:?}");
    assert!((resized.top - 0.1).abs() < 1e-5, "{resized:?}");
    assert!((resized.width - 0.5).abs() < 1e-5, "{resized:?}");
    assert!((resized.height - 0.5).abs() < 1e-5, "{resized:?}");
    assert!(state.drag_to((220., 115.)));
    let clamped = state.masks[0].rect;
    assert!((clamped.left - 0.59).abs() < 1e-5, "{clamped:?}");
    assert!((clamped.top - 0.59).abs() < 1e-5, "{clamped:?}");
    assert!((clamped.width - 0.01).abs() < 1e-5, "{clamped:?}");
    assert!((clamped.height - 0.01).abs() < 1e-5, "{clamped:?}");
    assert!(!state.finish_drag());
    assert_eq!(state.masks.len(), 1);
}

#[test]
fn right_click_without_dragging_removes_the_mask() {
    let mut state = OcclusionState {
        image: Some("diagram.png".into()),
        bounds: Some(gpui::Bounds {
            origin: point(px(100.), px(50.)),
            size: size(px(200.), px(100.)),
        }),
        masks: vec![OcclusionMask {
            shape: MaskShape::Rect,
            rect: NormalizedRect {
                left: 0.2,
                top: 0.2,
                width: 0.4,
                height: 0.4,
            },
        }],
        ..OcclusionState::default()
    };
    assert!(state.press((150., 75.), MouseButton::Right));
    assert!(!state.drag_to((151., 75.)));
    assert_eq!(state.masks[0].rect.left, 0.2);
    assert!(state.finish_drag());
    assert_eq!(state.masks.len(), 0);
    assert_eq!(state.drag, None);
}

#[test]
fn occlusion_pointer_uses_the_fitted_image_area() {
    let state = OcclusionState {
        image_size: Some((200.0, 100.0)),
        bounds: Some(gpui::Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(400.), px(400.)),
        }),
        masks: vec![OcclusionMask {
            shape: MaskShape::Rect,
            rect: NormalizedRect {
                left: 0.0,
                top: 0.0,
                width: 0.2,
                height: 1.0,
            },
        }],
        ..OcclusionState::default()
    };
    let rect = state.image_rect().expect("fitted image");
    assert_eq!((rect.top, rect.height), (0.25, 0.5));
    assert_eq!(state.pointer((200.0, 100.0)), Some((0.5, 0.0)));
    assert_eq!(state.pointer((200.0, 300.0)), Some((0.5, 1.0)));
    let outside = state.raw_pointer((40.0, 50.0)).expect("raw point");
    assert!(outside.1 < 0.0);
    assert_eq!(state.mask_at(state.pointer((40.0, 50.0)).unwrap()), Some(0));
    assert_eq!(state.mask_at(outside), None);
}

#[test]
fn mask_hit_test_finds_the_newest_overlap_and_clears_with_the_image() {
    let mask = |left, top| OcclusionMask {
        shape: MaskShape::Rect,
        rect: NormalizedRect {
            left,
            top,
            width: 0.4,
            height: 0.4,
        },
    };
    let mut state = OcclusionState {
        image: Some("diagram.png".into()),
        masks: vec![mask(0.1, 0.1), mask(0.3, 0.3)],
        ..OcclusionState::default()
    };
    assert_eq!(state.mask_at((0.2, 0.2)), Some(0));
    assert_eq!(state.mask_at((0.35, 0.35)), Some(1));
    assert_eq!(state.mask_at((0.9, 0.9)), None);
    assert!(state.remove_mask(1));
    assert_eq!(state.masks.len(), 1);
    assert!(!state.remove_mask(5));
    state.clear_image();
    assert_eq!(state.image, None);
    assert_eq!(state.masks.len(), 0);
}

#[test]
fn occlusion_validation_needs_an_image_and_a_mask() {
    let deck = Deck {
        id: 1,
        name: "Default".into(),
        description: String::new(),
    };
    let mut model = ranki_model::StockModelKind::ImageOcclusion.definition();
    model.id = 5;
    let errors = validate_occlusion(&deck, &model, None, &[]);
    assert_eq!(
        errors.get("occlusion").map(String::as_str),
        Some("Upload an image first")
    );
    assert_eq!(
        errors.get("masks").map(String::as_str),
        Some("Draw at least one mask on the image")
    );
    let mask = crate::occlusion::default_mask(0);
    let errors = validate_occlusion(&deck, &model, Some("a.png"), &[mask]);
    assert!(errors.is_empty(), "{errors:?}");
    let placeholder = models_with_stock(Vec::new())
        .into_iter()
        .find(ranki_model::NoteModel::is_image_occlusion)
        .expect("stock occlusion");
    let errors =
        validate_occlusion(&deck, &placeholder, Some("a.png"), &[mask]);
    assert_eq!(
        errors.get("model").map(String::as_str),
        Some("This note type is not in the collection")
    );
}

#[gpui::test]
fn saving_an_occlusion_note_clears_the_chosen_image(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let store =
        Box::new(ranki_storage::Collection::create(&path).expect("collection"));
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store,
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        let occlusion = root
            .models
            .iter()
            .position(ranki_model::NoteModel::is_image_occlusion)
            .expect("stock occlusion");
        root.select_model(occlusion, cx);
        root.add.occlusion.image = Some(String::from("diagram.png"));
        root.add.occlusion.masks = vec![crate::occlusion::default_mask(0)];
        cx.notify();
    });
    assert!(
        cx.read_entity(&root, |root, _| root.add.occlusion.image.is_some())
    );

    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.save_note(window, cx));
    });

    let state = cx.read_entity(&root, |root, _| {
        (
            root.add.occlusion.image.clone(),
            root.add.occlusion.masks.len(),
        )
    });
    assert_eq!(state.0, None);
    assert_eq!(state.1, 0);
}

#[gpui::test]
fn image_button_removes_the_chosen_image(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = super::View::AddCard;
        let occlusion = root
            .models
            .iter()
            .position(ranki_model::NoteModel::is_image_occlusion)
            .expect("stock occlusion");
        root.add.model_index = occlusion;
        root.add.occlusion.image = Some(String::from("diagram.png"));
        root.add.occlusion.masks = vec![crate::occlusion::default_mask(0)];
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let button = cx
        .debug_bounds("io-image")
        .expect("unified image button bounds");
    click_test_point(cx, button.center());

    let state = cx.read_entity(&root, |root, _| {
        (
            root.add.occlusion.image.clone(),
            root.add.occlusion.masks.len(),
        )
    });
    assert_eq!(state, (None, 0));
}

#[test]
fn installing_a_stock_type_keeps_its_tab_selected() {
    let mut installed = ranki_model::StockModelKind::Cloze.definition();
    installed.id = 77;
    let models = models_with_stock(vec![installed]);
    assert_eq!(model_index(&models, "Cloze"), Some(0));
    assert_eq!(models[0].id, 77);
    assert_eq!(model_index(&models, "cloze"), Some(0));
    assert_eq!(model_index(&models, "Nope"), None);
    assert_eq!(models.len(), 4);
    let missing = models_with_stock(Vec::new());
    assert_eq!(model_index(&missing, "Cloze"), Some(2));
    assert_eq!(missing[2].id, 0);
}

#[test]
fn tag_field_commits_words_into_chips() {
    let mut chips = Vec::new();
    assert_eq!(collect_tags(&mut chips, "japan"), "japan");
    assert_eq!(chips.len(), 0);
    assert_eq!(collect_tags(&mut chips, "japan "), "");
    assert_eq!(chips, ["japan"]);
    assert_eq!(collect_tags(&mut chips, "japan verb"), "verb");
    assert_eq!(chips, ["japan"]);
    assert_eq!(collect_tags(&mut chips, "verb "), "");
    assert_eq!(chips, ["japan", "verb"]);
    assert_eq!(collect_tags(&mut chips, " 	 "), "");
    assert_eq!(chips, ["japan", "verb"]);
}

#[test]
fn tag_suggestions_match_the_word_being_typed() {
    let known = ["japan", "Japanese", "verb", "noun"]
        .iter()
        .map(|tag| (*tag).to_owned())
        .collect::<Vec<_>>();
    let chips = [String::from("noun")];
    assert_eq!(suggest_tags(&known, &chips, "ja", 6), ["japan", "Japanese"]);
    assert_eq!(suggest_tags(&known, &chips, "VERB", 6), ["verb"]);
    assert_eq!(suggest_tags(&known, &chips, "", 6), Vec::<String>::new());
    assert_eq!(
        suggest_tags(&known, &chips, "missing", 6),
        Vec::<String>::new()
    );
    assert_eq!(suggest_tags(&known, &chips, "a", 1), ["japan"]);
}

#[gpui::test]
fn tag_suggestions_open_while_typing_and_commit_a_tag(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: std::sync::Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    let preset = [
        "long-existing-language-tag",
        "another-long-existing-tag",
        "third-long-existing-tag",
    ];
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = View::AddCard;
        root.add.tag_chips =
            preset.iter().map(|tag| (*tag).to_owned()).collect();
        cx.notify();
    });
    let focus = cx.read_entity(&root, |root, cx| {
        root.add.tags.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&focus));
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
            root.clone()
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    };
    draw(cx);

    cx.simulate_keystrokes("j a");
    draw(cx);
    assert!(
        cx.debug_bounds("add-tag-suggestions-popup").is_some(),
        "typing should open the tag popup"
    );
    let input = cx.debug_bounds("add-tags").expect("tag input bounds");
    let first = cx
        .debug_bounds("add-tag-suggestion-0")
        .expect("first suggestion bounds");
    assert!(first.origin.y >= input.bottom());

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.read_entity(&root, |root, _| root.add.tag_suggestions_dismissed),
        "escape dismisses the popup"
    );
    cx.simulate_keystrokes("backspace backspace j a");
    cx.run_until_parked();
    draw(cx);
    assert!(
        !cx.read_entity(&root, |root, _| root.add.tag_suggestions_dismissed),
        "typing reopens the popup"
    );

    cx.simulate_keystrokes("down");
    assert_eq!(
        cx.read_entity(&root, |root, _| root.add.tag_suggestion),
        1,
        "down highlights the next suggestion"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    draw(cx);
    let (chips, text) = cx.read_entity(&root, |root, cx| {
        (root.add.tag_chips.clone(), root.add.tags.read(cx).text())
    });
    let mut expected = preset
        .iter()
        .map(|tag| (*tag).to_owned())
        .collect::<Vec<_>>();
    expected.push(String::from("japanese"));
    assert_eq!(chips, expected);
    assert_eq!(text, "");

    cx.simulate_keystrokes("v e r");
    draw(cx);
    let row = cx
        .debug_bounds("add-tag-suggestion-0")
        .expect("verb suggestion bounds");
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    draw(cx);
    let (chips, text) = cx.read_entity(&root, |root, cx| {
        (root.add.tag_chips.clone(), root.add.tags.read(cx).text())
    });
    expected.push(String::from("verb"));
    assert_eq!(chips, expected);
    assert_eq!(text, "");
    assert!(
        cx.read_entity(&root, |root, _| root.add.tag_suggestions_dismissed),
        "choosing a tag closes the popup"
    );
}

#[test]
fn learning_cards_wait_for_their_step_and_the_main_queue() {
    let now = 1_700_000_000;
    let card = |id, queue, due| Card {
        id,
        note_id: 0,
        deck_id: 0,
        ordinal: 0,
        modified: now,
        usn: -1,
        card_type: CardType::Learning,
        queue,
        due,
        interval: -600,
        factor: 2_500,
        reps: 1,
        lapses: 0,
        left: 1_001,
        original_due: 0,
        original_deck_id: 0,
        flags: 0,
        data: String::new(),
    };
    let mut pending = VecDeque::new();
    // A review due stays behind whatever main queue holds it; only intraday
    // learning cards are pulled forward by the learn-ahead window.
    for entry in [
        card(2, CardQueue::Review, 0),
        card(1, CardQueue::Learning, now + 10 * 60),
        card(4, CardQueue::Learning, now + 60),
        card(3, CardQueue::Learning, now + 21 * 60),
    ] {
        super::Root::insert_pending(&mut pending, entry);
    }
    let step_due = |cutoff| {
        super::Root::due_learning_card(&pending, cutoff).map(|card| card.id)
    };
    assert_eq!(step_due(now), None, "a step in the future is not due yet");
    assert_eq!(step_due(now + 30), None);
    assert_eq!(step_due(now + 60), Some(4));
    assert_eq!(step_due(now + 10 * 60), Some(4));
    assert_eq!(
        step_due(now + super::LEARN_AHEAD_SECS),
        Some(4),
        "the learn-ahead window never overtakes an earlier step"
    );
    let ordered: Vec<_> = pending.iter().map(|card| card.id).collect();
    assert_eq!(ordered, [2, 4, 1, 3]);
}

#[gpui::test]
fn again_waits_behind_the_cards_already_due(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    for front in ["First?", "Second?"] {
        store
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![front.to_owned(), String::from("Answer!")],
                tags: Vec::new(),
            })
            .expect("note");
    }
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });
    let current = |cx: &mut gpui::TestAppContext| {
        cx.read_entity(&root, |root, _| {
            root.review.current.as_ref().map(|card| card.id)
        })
    };
    let grade = |cx: &mut gpui::TestAppContext, rating| {
        cx.update_entity(&root, |root, cx| {
            root.review.phase = Phase::Answer;
            root.review.last_grade_at = None;
            root.grade(rating, cx);
        });
    };
    let first = current(cx).expect("first card");

    grade(cx, ranki_model::Rating::Again);
    assert!(current(cx).is_some(), "the next card is ready");
    assert_ne!(
        current(cx),
        Some(first),
        "a step due in a minute must not come back before the waiting cards"
    );
    let pending = cx.read_entity(&root, |root, _| root.review.pending.len());
    assert_eq!(pending, 1, "the answered card waits for its step");

    grade(cx, ranki_model::Rating::Easy);
    assert_eq!(
        current(cx),
        Some(first),
        "the learn-ahead window brings the step back once nothing is left"
    );
}

#[gpui::test]
fn clicking_create_button_saves_the_deck(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let store =
        Box::new(ranki_storage::Collection::create(&path).expect("collection"));
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store,
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.create_modal = true;
        root.create_field.update(cx, |field, cx| {
            field.set_text("Click Deck", cx);
        });
        cx.notify();
    });
    let focus = cx.read_entity(&root, |root, cx| {
        root.create_field.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&focus));
    cx.draw(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(1280.), gpui::px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let button = cx
        .debug_bounds("create-deck-primary")
        .expect("create button bounds");
    cx.simulate_mouse_move(button.center(), None, gpui::Modifiers::none());
    cx.simulate_mouse_down(
        button.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_mouse_up(
        button.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );

    cx.run_until_parked();
    let names = cx.read_entity(&root, |root, _| {
        root.decks
            .iter()
            .map(|deck| deck.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(names, ["Click Deck"]);
}
#[gpui::test]
fn clicking_a_deck_row_studies_that_deck(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let cards = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    assert_eq!(cards.len(), 1);
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row = cx
        .debug_bounds("deck-row-Default")
        .expect("deck row bounds");
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let state = cx.read_entity(&root, |root, _| {
        (
            root.view,
            root.selected,
            root.review.scope_all,
            root.review.session_total,
            root.review.current.is_some(),
        )
    });
    assert_eq!(state.0, View::Review);
    assert_eq!(state.1, Some(0));
    assert!(!state.2);
    assert_eq!(state.3, 1);
    assert!(state.4);
}

#[gpui::test]
fn space_reveals_after_clicking_a_deck_row(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    cx.update(|cx| cx.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row = cx
        .debug_bounds("deck-row-Default")
        .expect("deck row bounds");
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("space");

    let phase = cx.read_entity(&root, |root, _| root.review.phase);
    assert_eq!(phase, Phase::Answer);
}
#[gpui::test]
fn space_reveals_after_pointer_leaves_and_returns(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    for question in ["First question?", "Second question?"] {
        store
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![String::from(question), String::from("Answer!")],
                tags: Vec::new(),
            })
            .expect("note");
    }
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    cx.update(|cx| cx.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row = cx
        .debug_bounds("deck-row-Default")
        .expect("deck row bounds");
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let state = cx.read_entity(&root, |root, _| {
        (root.view, root.review.current.is_some(), root.review.phase)
    });
    assert_eq!(state.0, View::Review);
    assert!(state.1, "a card must be loaded");
    assert_eq!(state.2, Phase::Question);

    let reveal = point(px(640.), px(680.));
    cx.simulate_click(reveal, gpui::Modifiers::none());
    assert_eq!(
        cx.read_entity(&root, |root, _| root.review.phase),
        Phase::Answer
    );

    cx.update_entity(&root, |root, _| root.review.last_grade_at = None);
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let good = point(px(775.), px(680.));
    cx.simulate_click(good, gpui::Modifiers::none());
    assert_eq!(
        cx.read_entity(&root, |root, _| root.review.phase),
        Phase::Question
    );

    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    let reveal = point(px(640.), px(680.));
    cx.simulate_mouse_move(reveal, None, gpui::Modifiers::none());
    cx.simulate_event(gpui::MouseExitEvent {
        position: reveal,
        pressed_button: None,
        modifiers: gpui::Modifiers::none(),
    });
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });
    cx.simulate_mouse_move(reveal, None, gpui::Modifiers::none());
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear();
    });

    cx.simulate_keystrokes("space");

    assert_eq!(
        cx.read_entity(&root, |root, _| root.review.phase),
        Phase::Answer
    );
}

#[gpui::test]
fn space_reveals_after_study_all_from_focused_search(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    cx.update(|cx| cx.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let search = cx.read_entity(&root, |root, cx| {
        root.deck_search.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&search));
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });
    cx.run_until_parked();
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(!cx.update(|window, _| search.is_focused(window)));
    assert_eq!(cx.read_entity(&root, |root, _| root.view), View::Review);
    assert!(cx.read_entity(&root, |root, _| root.review.current.is_some()));

    cx.simulate_keystrokes("space");

    let phase = cx.read_entity(&root, |root, _| root.review.phase);
    assert_eq!(phase, Phase::Answer);
}

#[gpui::test]
fn pending_reveal_scroll_does_not_survive_a_reset(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.reveal(cx);
    });

    cx.update_entity(&root, |root, cx| {
        let card = Card {
            id: 1,
            note_id: 1,
            deck_id: 1,
            ordinal: 0,
            modified: 0,
            usn: 0,
            card_type: CardType::New,
            queue: CardQueue::New,
            due: 0,
            interval: 0,
            factor: 0,
            reps: 0,
            lapses: 0,
            left: 0,
            original_due: 0,
            original_deck_id: 0,
            flags: 0,
            data: String::new(),
        };
        root.apply_loaded(
            &card,
            super::LoadedCard {
                note: None,
                question: Some(crate::card_view::RenderedCard {
                    text: "Question line\n".repeat(60),
                    ..Default::default()
                }),
                answer: Some(crate::card_view::RenderedCard {
                    text: "Answer".into(),
                    ..Default::default()
                }),
                answer_media: Vec::new(),
                occlusion_image: None,
                occlusion_image_size: None,
            },
            cx,
        );
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let (offset, max) = cx.read_entity(&root, |root, _| {
        let handle = &root.review_stage_scroll;
        (
            f32::from(handle.offset().y),
            f32::from(handle.max_offset().height),
        )
    });
    assert!(max > 0.0, "the long question must overflow the stage");
    assert_eq!(offset, 0.0, "the next card must start at the top");
    cx.update_entity(&root, |root, cx| {
        root.reveal(cx);
        let card = Card {
            id: 2,
            note_id: 1,
            deck_id: 1,
            ordinal: 0,
            modified: 0,
            usn: 0,
            card_type: CardType::New,
            queue: CardQueue::New,
            due: 0,
            interval: 0,
            factor: 0,
            reps: 0,
            lapses: 0,
            left: 0,
            original_due: 0,
            original_deck_id: 0,
            flags: 0,
            data: String::new(),
        };
        root.apply_loaded(
            &card,
            super::LoadedCard {
                note: None,
                question: Some(crate::card_view::RenderedCard {
                    text: "Question line\n".repeat(60),
                    ..Default::default()
                }),
                answer: Some(crate::card_view::RenderedCard {
                    text: "Answer".into(),
                    ..Default::default()
                }),
                answer_media: Vec::new(),
                occlusion_image: None,
                occlusion_image_size: None,
            },
            cx,
        );
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let offset = cx.read_entity(&root, |root, _| {
        f32::from(root.review_stage_scroll.offset().y)
    });
    assert_eq!(offset, 0.0, "a loaded card must clear a pending reveal");
    cx.update_entity(&root, |root, cx| {
        root.reveal(cx);
        root.toggle_reveal(cx);
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let offset = cx.read_entity(&root, |root, _| {
        f32::from(root.review_stage_scroll.offset().y)
    });
    assert_eq!(offset, 0.0, "a reset before drawing must clear the reveal");
}

#[gpui::test]
fn deck_row_controls_do_not_start_a_session(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store.create_deck("Topic::Child").expect("deck");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let chevron = cx
        .debug_bounds("decks-chevron-Topic")
        .expect("chevron bounds");
    cx.simulate_click(chevron.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let (view, expanded) = cx.read_entity(&root, |root, _| {
        (root.view, root.expanded.contains("Topic"))
    });
    assert_eq!(view, View::Decks);
    assert!(expanded);

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let menu = cx.debug_bounds("decks-menu-1").expect("menu bounds");
    cx.simulate_click(menu.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let (view, menu_open) =
        cx.read_entity(&root, |root, _| (root.view, root.deck_menu.is_some()));
    assert_eq!(view, View::Decks);
    assert!(menu_open);
}
#[gpui::test]
fn right_clicking_a_deck_opens_its_actions_menu(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let cards = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    assert_eq!(cards.len(), 1);
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let row = cx
        .debug_bounds("deck-row-Default")
        .expect("deck row bounds");
    cx.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    cx.simulate_mouse_up(
        row.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let state = cx.read_entity(&root, |root, _| {
        (root.view, root.deck_menu.map(|menu| menu.id))
    });
    assert_eq!(state.0, View::Decks);
    assert_eq!(state.1, Some(1));
    assert!(cx.debug_bounds("deck-actions-menu").is_some());
}
#[gpui::test]
fn exporting_a_deck_asks_for_a_save_path(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update_entity(&root, |root, cx| {
        root.deck_menu = Some(super::DeckMenu {
            id: 1,
            anchor: point(px(100.), px(100.)),
        });
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });

    let export = cx
        .debug_bounds("decks-export")
        .expect("deck export menu item bounds");
    cx.simulate_click(export.center(), gpui::Modifiers::none());

    assert!(cx.did_prompt_for_new_path());
    assert!(cx.read_entity(&root, |root, _| root.deck_menu.is_none()));
}

#[test]
fn export_file_name_is_safe_for_deck_paths() {
    assert_eq!(
        super::Root::export_file_name("Visual Deck"),
        "Visual Deck.apkg"
    );
    assert_eq!(
        super::Root::export_file_name("Language::French/1"),
        "Language - French_1.apkg"
    );
    assert_eq!(super::Root::export_file_name("::"), "deck.apkg");
}
#[gpui::test]
fn escape_closes_a_deck_actions_menu(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    cx.update(|cx| cx.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update_entity(&root, |root, cx| {
        root.deck_menu = Some(super::DeckMenu {
            id: 1,
            anchor: point(px(100.), px(100.)),
        });
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });

    cx.simulate_keystrokes("escape");

    assert!(cx.read_entity(&root, |root, _| root.deck_menu.is_none()));
}
#[gpui::test]
fn clicking_the_rename_field_keeps_the_decks_view(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            let deck = root.decks[0].clone();
            root.start_rename(&deck, window, cx);
        });
    });
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let field = cx
        .debug_bounds("deck-rename-field")
        .expect("rename field bounds");
    cx.simulate_click(field.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let (view, editing) =
        cx.read_entity(&root, |root, _| (root.view, root.deck_edit.is_some()));
    assert_eq!(view, View::Decks);
    assert!(editing);
}
#[gpui::test]
fn popup_actions_do_not_study_the_deck_below(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    for name in ["aa", "bb", "cc", "dd", "ee"] {
        let _ = store.create_deck(name).expect("deck");
    }
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let save_settings: crate::SettingsSaver = Arc::new(|_| Ok(()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings,
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let row = cx.debug_bounds("deck-row-bb").expect("deck row bounds");
    let deck_id = cx.read_entity(&root, |root, _| root.decks[1].id);
    cx.update_entity(&root, |root, cx| {
        root.deck_menu = Some(super::DeckMenu {
            id: deck_id,
            anchor: point(row.center().x, row.origin.y - px(40.)),
        });
        cx.notify();
    });
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let overlap = |a: gpui::Bounds<gpui::Pixels>,
                   b: gpui::Bounds<gpui::Pixels>|
     -> Option<gpui::Point<gpui::Pixels>> {
        let left = a.origin.x.max(b.origin.x);
        let right = a.bottom_right().x.min(b.bottom_right().x);
        let top = a.origin.y.max(b.origin.y);
        let bottom = a.bottom_right().y.min(b.bottom_right().y);
        if left < right && top < bottom {
            Some(point(
                px(f32::midpoint(f32::from(left), f32::from(right))),
                px(f32::midpoint(f32::from(top), f32::from(bottom))),
            ))
        } else {
            None
        }
    };
    let rows = [
        "deck-row-aa",
        "deck-row-bb",
        "deck-row-cc",
        "deck-row-dd",
        "deck-row-ee",
    ];
    let remove = cx.debug_bounds("decks-remove").expect("remove bounds");
    let click = rows
        .iter()
        .filter_map(|selector| cx.debug_bounds(selector))
        .find_map(|bounds| overlap(remove, bounds))
        .expect("remove must overlap a deck row");
    click_test_point(cx, click);
    let (view, confirmation) =
        cx.read_entity(&root, |root, _| (root.view, root.confirmation.clone()));
    assert_eq!(view, View::Decks, "menu click must not study a deck");
    assert!(
        matches!(confirmation, Some(super::Confirmation::RemoveDeck { .. })),
        "menu click must open the remove confirmation"
    );

    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let confirm = cx.debug_bounds("confirm-primary").expect("confirm bounds");
    let click = rows
        .iter()
        .filter_map(|selector| cx.debug_bounds(selector))
        .find_map(|bounds| overlap(confirm, bounds))
        .expect("confirmation must overlap a deck row");
    click_test_point(cx, click);
    let (view, confirmation) =
        cx.read_entity(&root, |root, _| (root.view, root.confirmation.clone()));
    assert_eq!(view, View::Decks, "dialog click must not study a deck");
    assert!(
        confirmation.is_none(),
        "dialog click must still confirm the removal"
    );
}

fn recording_saver() -> (crate::SettingsSaver, Arc<Mutex<Option<Options>>>) {
    let latest = Arc::new(Mutex::new(None));
    let for_saver = Arc::clone(&latest);
    let saver: crate::SettingsSaver = Arc::new(move |settings| {
        *for_saver.lock().expect("settings lock") = Some(settings.clone());
        Ok(())
    });
    (saver, latest)
}

#[gpui::test]
fn saved_window_layout_is_restored(cx: &mut gpui::TestAppContext) {
    let mut options = Options::default();
    let state = &mut options.ui_state;
    state.view = ranki_model::UiView::Browse;
    state.previous_view = ranki_model::UiView::AddCard;
    state.theme = Some(ranki_model::ThemeChoice::Light);
    state.nav_width = Some(140.0);
    state.selected_deck = Some(1);
    state.expanded_decks = vec![String::from("Default")];
    state.deck_query = String::from("deck term");
    state.browse_query = String::from("card term");
    state.review_all_decks = true;

    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    let restored = cx.read_entity(&root, |root, cx| {
        (
            root.view,
            root.previous_view,
            root.options.theme,
            root.nav_width,
            root.selected,
            root.expanded.contains("Default"),
            root.deck_search.read(cx).text(),
            root.browse.search.read(cx).text(),
            root.review.scope_all,
        )
    });
    assert_eq!(restored.0, View::Browse);
    assert_eq!(restored.1, View::AddCard);
    assert_eq!(restored.2, ranki_model::ThemeChoice::Light);
    assert_eq!(restored.3, 140.0);
    assert_eq!(restored.4, Some(0));
    assert!(restored.5);
    assert_eq!(restored.6, "deck term");
    assert_eq!(restored.7, "card term");
    assert!(restored.8);
}

#[gpui::test]
fn layout_changes_are_saved_and_restored(cx: &mut gpui::TestAppContext) {
    let (saver, latest) = recording_saver();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: saver,
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::OpenSettings, window, cx);
            root.toggle_deck(String::from("Topic"), cx);
            root.set_theme(ranki_model::ThemeChoice::Dark, cx);
        });
    });

    let recorded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings");
    assert_eq!(recorded.ui_state.view, ranki_model::UiView::Settings);
    assert!(
        recorded
            .ui_state
            .expanded_decks
            .contains(&String::from("Topic"))
    );
    assert_eq!(
        recorded.ui_state.theme,
        Some(ranki_model::ThemeChoice::Dark)
    );

    let (restarted, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: recorded,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    let restored = cx.read_entity(&restarted, |root, _| {
        (
            root.view,
            root.options.theme,
            root.expanded.contains("Topic"),
        )
    });
    assert_eq!(restored.0, View::Settings);
    assert_eq!(restored.1, ranki_model::ThemeChoice::Dark);
    assert!(restored.2);
}

#[gpui::test]
fn search_queries_are_saved_on_edit(cx: &mut gpui::TestAppContext) {
    let (saver, latest) = recording_saver();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: saver,
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.deck_search
            .update(cx, |field, cx| field.insert_text("deck term", cx));
        root.browse
            .search
            .update(cx, |field, cx| field.insert_text("card term", cx));
    });
    cx.run_until_parked();

    let recorded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings");
    assert_eq!(recorded.ui_state.deck_query, "deck term");
    assert_eq!(recorded.ui_state.browse_query, "card term");
}

#[gpui::test]
fn editing_a_browse_card_saves_the_add_card_view(
    cx: &mut gpui::TestAppContext,
) {
    let (saver, latest) = recording_saver();
    let mut store = BrowseStore {
        decks: vec![Deck {
            id: 10,
            name: String::from("Default"),
            description: String::new(),
        }],
        models: ranki_model::stock_models(),
        cards: HashMap::new(),
        notes: HashMap::new(),
        ids: Vec::new(),
        card_reads: Arc::new(AtomicUsize::new(0)),
        find_reads: Arc::new(AtomicUsize::new(0)),
        fail_browse: false,
        fail_reads: false,
    };
    let _ = store.push_card(10);
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: Options::default(),
                save_settings: saver,
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.start_card_edit(1, window, cx);
        });
    });

    let recorded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings");
    assert_eq!(recorded.ui_state.view, ranki_model::UiView::AddCard);
    assert_eq!(recorded.ui_state.previous_view, ranki_model::UiView::Browse);
}

#[gpui::test]
fn editing_a_card_reports_a_store_read_failure(cx: &mut gpui::TestAppContext) {
    let mut store = BrowseStore {
        decks: vec![Deck {
            id: 10,
            name: String::from("Default"),
            description: String::new(),
        }],
        models: ranki_model::stock_models(),
        cards: HashMap::new(),
        notes: HashMap::new(),
        ids: Vec::new(),
        card_reads: Arc::new(AtomicUsize::new(0)),
        find_reads: Arc::new(AtomicUsize::new(0)),
        fail_browse: false,
        fail_reads: true,
    };
    let _ = store.push_card(10);
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.start_card_edit(1, window, cx);
        });
    });

    let toast = cx
        .update(|_, app| root.read(app).toast.clone())
        .expect("error toast");
    assert!(matches!(toast.kind, super::ToastKind::Error));
    assert_eq!(
        toast.text,
        "Could not load card: storage failure: disk is on fire"
    );
    assert!(cx.update(|_, app| root.read(app).card_edit.is_none()));
}

#[gpui::test]
fn reloading_the_collection_saves_the_reset_view(
    cx: &mut gpui::TestAppContext,
) {
    let (saver, latest) = recording_saver();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: saver,
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::Statistics;
        root.reload_collection(cx);
    });

    let recorded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings");
    assert_eq!(recorded.ui_state.view, ranki_model::UiView::Decks);
}

#[gpui::test]
fn color_picker_applies_markup_to_the_editor_field(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.view = View::AddCard;
            root.seed_fields("Paris", cx);
            let field = root.add.fields[0].clone();
            root.toggle_color_picker(
                crate::add_card_view::ColorTarget::Text,
                field.clone(),
                point(px(300.), px(360.)),
                window,
                cx,
            );
            assert!(root.add.color_picker.is_some());
            root.apply_color(
                crate::add_card_view::ColorTarget::Text,
                field,
                "#f38ba8",
                window,
                cx,
            );
            root.apply_color(
                crate::add_card_view::ColorTarget::Text,
                root.add.fields[0].clone(),
                "cba6f7",
                window,
                cx,
            );
            assert!(
                root.add.fields[0]
                    .read(cx)
                    .focus_handle()
                    .is_focused(window),
                "applying a color keeps the editor focused"
            );
        });
    });
    let (text, open) = cx.read_entity(&root, |root, cx| {
        (
            root.add.fields[0].read(cx).text(),
            root.add.color_picker.is_some(),
        )
    });
    assert_eq!(
        text,
        "Paris<span style=\"color: #f38ba8\"><span style=\"color: #cba6f7\"></span></span>"
    );
    assert!(!open, "applying a color closes the picker");
}

#[gpui::test]
fn scrolling_the_form_closes_the_color_picker(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(gpui::size(px(1280.), px(800.)));
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.view = View::AddCard;
            let field = root.add.fields[0].clone();
            root.toggle_color_picker(
                crate::add_card_view::ColorTarget::Text,
                field,
                point(px(300.), px(360.)),
                window,
                cx,
            );
        });
    });
    cx.draw(
        point(px(0.), px(0.)),
        gpui::size(px(1280.), px(800.)),
        |_, _| root.clone(),
    );
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: point(px(1100.), px(600.)),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-20.))),
        modifiers: gpui::Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();
    let (open, focused) = cx.update(|window, cx| {
        let state = root.read(cx);
        (
            state.add.color_picker.is_some(),
            state.focus.is_focused(window),
        )
    });
    assert!(!open, "scrolling the form closes the color picker");
    assert!(focused, "closing the picker restores root focus");
}

#[test]
fn import_accepts_only_anki_package_extensions() {
    assert!(super::is_anki_package(std::path::Path::new("core.apkg")));
    assert!(super::is_anki_package(std::path::Path::new("core.APKG")));
    assert!(super::is_anki_package(std::path::Path::new(
        "backup.colpkg"
    )));
    assert!(super::is_anki_package(std::path::Path::new(
        "backup.COLpkg"
    )));
    assert!(!super::is_anki_package(std::path::Path::new("notes.txt")));
    assert!(!super::is_anki_package(std::path::Path::new("apkg")));
}

#[cfg(target_os = "linux")]
#[test]
fn import_picker_filters_to_anki_packages() {
    let filter = super::anki_package_filter();
    assert_eq!(filter.label(), "Anki packages");
    assert_eq!(filter.pattern_filters(), super::ANKI_PACKAGE_GLOBS);
    assert_eq!(filter.mimetype_filters(), [] as [&str; 0]);
}
fn open_browse_store(
    cx: &mut gpui::TestAppContext,
    store: BrowseStore,
) -> (gpui::Entity<super::Root>, &mut gpui::VisualTestContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = View::Browse;
        root.refresh_browse(cx);
        cx.notify();
    });
    (root, cx)
}

#[gpui::test]
fn empty_browse_shows_cards_and_leaf_chevrons(cx: &mut gpui::TestAppContext) {
    let mut store = BrowseStore::with_cards(3);
    store.decks[0].name = String::from("Languages::French");
    let (root, cx) = open_browse_store(cx, store);
    cx.update_entity(&root, |root, _| {
        assert_eq!(root.browse.selected, Some(1));
        assert!(root.browse.expanded.contains("Languages::French"));
        let cards = root
            .browse
            .entries
            .iter()
            .filter(|entry| {
                matches!(entry, crate::browser_view::BrowseEntry::Card { .. })
            })
            .count();
        assert_eq!(cards, 3);
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("browse-row-1").is_some(),
        "cards must show on the first empty query"
    );
    assert!(
        cx.debug_bounds("browse-chevron-Languages::French")
            .is_some(),
        "a leaf deck with cards must be collapsible"
    );
    assert!(cx.debug_bounds("browse-detail").is_some());
}

#[gpui::test]
fn clear_search_stays_visible_when_browse_fails(cx: &mut gpui::TestAppContext) {
    let mut store = BrowseStore::with_cards(1);
    store.fail_browse = true;
    let (root, cx) = open_browse_store(cx, store);
    cx.update_entity(&root, |root, cx| {
        root.browse.search.update(cx, |field, cx| {
            field.seed("missing", cx);
        });
        root.refresh_browse(cx);
        assert!(
            root.browse
                .error
                .as_deref()
                .is_some_and(|error| error.contains("search index is offline"))
        );
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(
        cx.debug_bounds("secondary-Clear search").is_some(),
        "an error must not hide the way out of a failed query"
    );
    assert!(cx.debug_bounds("browse-empty-add").is_none());
}

#[gpui::test]
fn changing_a_browse_card_resets_the_preview_scroll(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = open_browse_store(cx, BrowseStore::with_cards(3));
    cx.update_entity(&root, |root, cx| {
        root.browse_detail_scroll
            .set_offset(point(px(0.), px(-400.)));
        assert_eq!(root.browse_detail_scroll.offset().y, px(-400.));
        root.select_browse_row(2, cx);
        assert_eq!(root.browse_detail_scroll.offset().y, px(0.));
        assert_eq!(root.browse.selected, Some(2));
    });
    cx.update_entity(&root, |root, cx| {
        root.browse.search.update(cx, |field, cx| {
            field.seed("one", cx);
        });
        root.browse.selected = Some(2);
        root.browse_detail_scroll
            .set_offset(point(px(0.), px(-400.)));
        root.refresh_browse(cx);
        assert_eq!(root.browse.selected, Some(1));
        assert_eq!(root.browse_detail_scroll.offset().y, px(0.));
    });
}

#[gpui::test]
fn browse_queries_reset_scroll_but_refreshes_keep_collapsed_groups(
    cx: &mut gpui::TestAppContext,
) {
    let mut store = BrowseStore::with_cards(60);
    store.decks.push(Deck {
        id: 11,
        name: String::from("Other"),
        description: String::new(),
    });
    for _ in 0..60 {
        let _ = store.push_card(11);
    }
    let (root, cx) = open_browse_store(cx, store);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update_entity(&root, |root, cx| {
        root.toggle_browse_group(String::from("Default"), cx);
        root.browse_smooth.wheel(500.0);
        root.browse_smooth.settle();
        assert!((root.browse_smooth.offset() - 500.0).abs() < 0.01);
        root.refresh_browse(cx);
        assert!(!root.browse.expanded.contains("Default"));
        assert!(root.browse.expanded.contains("Other"));
        assert!((root.browse_smooth.offset() - 500.0).abs() < 0.01);
    });
    cx.update_entity(&root, |root, cx| {
        root.browse.search.update(cx, |field, cx| {
            field.seed("one", cx);
        });
        root.refresh_browse(cx);
        assert_eq!(root.browse_smooth.offset(), 0.0);
        assert_eq!(root.browse.expanded.len(), 1);
        assert!(root.browse.expanded.contains("Default"));
    });
}

#[gpui::test]
fn refreshing_browse_clears_a_stale_card_menu(cx: &mut gpui::TestAppContext) {
    let (root, cx) = open_browse_store(cx, BrowseStore::with_cards(1));
    cx.update_entity(&root, |root, cx| {
        root.card_menu = Some(super::CardMenu {
            id: 1,
            anchor: point(px(100.), px(100.)),
        });
        root.card_menu_dismissed_press = Some(gpui::MouseDownEvent {
            position: point(px(100.), px(100.)),
            button: gpui::MouseButton::Left,
            modifiers: gpui::Modifiers::none(),
            click_count: 1,
            first_mouse: true,
        });
        root.refresh_browse(cx);
        assert!(root.card_menu.is_none());
        assert!(root.card_menu_dismissed_press.is_none());
    });
}

#[gpui::test]
fn card_menu_clicks_keep_the_row_selection(cx: &mut gpui::TestAppContext) {
    let (root, cx) = open_browse_store(cx, BrowseStore::with_cards(2));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row = cx.debug_bounds("browse-row-1").expect("first card row");
    let anchor = point(row.center().x + px(80.) - px(15.), row.top() - px(19.));
    cx.update_entity(&root, |root, cx| {
        root.browse.selected = Some(2);
        root.card_menu = Some(super::CardMenu { id: 1, anchor });
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let menu = cx
        .debug_bounds("card-actions-menu")
        .expect("card actions menu");
    let click = point(menu.left() + px(20.), menu.top() + px(20.));
    assert!(
        menu.intersects(&row),
        "the menu must cover the row to test the click"
    );
    cx.simulate_click(click, gpui::Modifiers::none());
    assert_eq!(
        cx.read_entity(&root, |root, _| root.browse.selected),
        Some(2)
    );
}

#[gpui::test]
fn failed_browse_rows_show_an_error_placeholder(cx: &mut gpui::TestAppContext) {
    let mut store = BrowseStore::with_cards(2);
    store.fail_reads = true;
    let (root, cx) = open_browse_store(cx, store);
    cx.update_entity(&root, |root, cx| {
        root.refresh_browse(cx);
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        cx.read_entity(&root, |root, _| root.browse.error.clone())
            .as_deref(),
        Some("Could not load some cards")
    );
    assert!(cx.debug_bounds("browse-failed-row-0").is_some());
}

#[gpui::test]
fn browse_debounce_shows_a_pending_indicator(cx: &mut gpui::TestAppContext) {
    let (root, cx) = open_browse_store(cx, BrowseStore::with_cards(1));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("browse-search-pending").is_none());
    cx.update_entity(&root, |root, cx| {
        root.schedule_browse_refresh(cx);
        assert!(root.browse.loading);
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("browse-search-pending").is_some());
    cx.update_entity(&root, |root, cx| {
        root.refresh_browse(cx);
        assert!(!root.browse.loading);
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(!cx.read_entity(&root, |root, _| root.browse.loading));
}

#[gpui::test]
fn typing_in_browse_does_not_save_on_every_keystroke(
    cx: &mut gpui::TestAppContext,
) {
    let (save_settings, latest) = recording_saver();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(BrowseStore::with_cards(1)),
                options: Options::default(),
                save_settings,
            },
            window,
            cx,
        )
    });
    latest.lock().expect("settings lock").take();
    cx.update_entity(&root, |root, cx| {
        root.browse.search.update(cx, |field, cx| {
            field.set_text("x", cx);
        });
    });
    cx.run_until_parked();
    assert!(latest.lock().expect("settings lock").is_none());
}

#[gpui::test]
fn up_and_down_move_browse_selection(cx: &mut gpui::TestAppContext) {
    let (root, cx) = open_browse_store(cx, BrowseStore::with_cards(80));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert_eq!(
        cx.read_entity(&root, |root, _| root.browse.selected),
        Some(1)
    );
    for action in [UiAction::BrowseDown, UiAction::BrowseDown] {
        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.dispatch(action, window, cx);
            });
        });
    }
    assert_eq!(
        cx.read_entity(&root, |root, _| root.browse.selected),
        Some(3)
    );
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::BrowseUp, window, cx);
        });
    });
    assert_eq!(
        cx.read_entity(&root, |root, _| root.browse.selected),
        Some(2)
    );

    for _ in 0..30 {
        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.dispatch(UiAction::BrowseDown, window, cx);
            });
        });
    }
    cx.update_entity(&root, |root, _| root.browse_smooth.settle());
    assert!(
        cx.read_entity(&root, |root, _| root.browse_smooth.offset()) > 0.0,
        "the selected card must be scrolled into view"
    );
}

#[test]
fn browse_arrow_keys_stay_out_of_inputs_and_dialogs() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let browse =
        [gpui::KeyContext::parse("ranki ranki_browse")
            .expect("browse context")];
    let typing = [
        gpui::KeyContext::parse("ranki ranki_browse").expect("browse context"),
        gpui::KeyContext::parse("ranki_input").expect("input context"),
    ];
    let dialog = [
        gpui::KeyContext::parse("ranki ranki_browse").expect("browse context"),
        gpui::KeyContext::parse("ranki_dialog").expect("dialog context"),
    ];
    for key in ["up", "down"] {
        let stroke = gpui::Keystroke::parse(key).expect("keystroke");
        let (in_browse, _) =
            keymap.bindings_for_input(std::slice::from_ref(&stroke), &browse);
        assert_eq!(in_browse.len(), 1, "{key}");
        let (in_input, _) =
            keymap.bindings_for_input(std::slice::from_ref(&stroke), &typing);
        assert!(in_input.is_empty(), "{key}");
        let (in_dialog, _) =
            keymap.bindings_for_input(std::slice::from_ref(&stroke), &dialog);
        assert!(in_dialog.is_empty(), "{key}");
    }
}

#[gpui::test]
fn narrow_browse_keeps_a_bounded_preview(cx: &mut gpui::TestAppContext) {
    let (_root, cx) = open_browse_store(cx, BrowseStore::with_cards(1));
    cx.simulate_resize(size(px(640.), px(800.)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let detail = cx.debug_bounds("browse-detail").expect("stacked preview");
    assert!(detail.size.width > px(300.));
    assert!(detail.size.height <= px(221.));
    assert!(detail.size.height >= px(100.));
}

#[gpui::test]
fn short_browse_windows_keep_three_rows_visible(cx: &mut gpui::TestAppContext) {
    let (_root, cx) = open_browse_store(cx, BrowseStore::with_cards(6));
    cx.simulate_resize(size(px(720.), px(540.)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let table = cx.debug_bounds("browse-table").expect("browse table");
    let detail = cx.debug_bounds("browse-detail").expect("stacked preview");
    assert!(table.size.height >= px(140.));
    assert!(detail.size.height <= px(220.));
    for key in ["browse-row-1", "browse-row-2", "browse-row-3"] {
        let row = cx.debug_bounds(key).unwrap_or_else(|| panic!("{key}"));
        assert!(
            row.bottom() <= table.bottom() + px(2.),
            "{key} must stay inside the table: row {row:?}, table {table:?}"
        );
    }
}

#[gpui::test]
fn long_browse_metadata_stays_inside_the_preview(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = open_browse_store(cx, BrowseStore::with_cards(1));
    cx.update_entity(&root, |root, cx| {
        let mut row = browser_test_row(String::from("Question"));
        row.deck = SharedString::from("Study::Languages::".repeat(20));
        let _ = root.browse.rows.insert(1, row);
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let detail = cx.debug_bounds("browse-detail").expect("side preview");
    let meta = cx.debug_bounds("browse-meta-Deck").expect("deck metadata");
    assert!(detail.size.width <= px(321.));
    assert!(meta.size.width <= detail.size.width);
    assert!(meta.size.height <= px(30.));
}

fn collection_options(dir: &std::path::Path, review: bool) -> Options {
    Options {
        paths: ranki_model::DataPaths::new(dir),
        ui_state: ranki_model::UiState {
            view: if review {
                ranki_model::UiView::Review
            } else {
                ranki_model::UiView::Decks
            },
            ..ranki_model::UiState::default()
        },
        ..Options::default()
    }
}

#[gpui::test]
fn escape_in_reload_dialog_keeps_a_live_session(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: collection_options(dir.path(), true),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    assert_eq!(cx.update(|_, cx| root.read(cx).view), View::Review);
    assert!(cx.update(|_, cx| root.read(cx).review.current.is_some()));

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::Reload, window, cx);
        });
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    assert!(cx.update(|_, cx| {
        matches!(
            root.read(cx).confirmation,
            Some(super::Confirmation::Reload)
        )
    }));

    cx.simulate_keystrokes("escape");
    let (view, confirmation, current) = cx.read_entity(&root, |root, _| {
        (
            root.view,
            root.confirmation.clone(),
            root.review.current.is_some(),
        )
    });
    assert_eq!(view, View::Review, "cancel must not end the live session");
    assert!(confirmation.is_none());
    assert!(current, "the card must stay on the review stage");

    cx.simulate_keystrokes("escape");
    assert_eq!(
        cx.update(|_, cx| root.read(cx).view),
        View::Decks,
        "escape ends the session once the dialog is gone"
    );
}

#[gpui::test]
fn first_grade_after_loading_is_not_debounced(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    for front in ["First?", "Second?"] {
        let _ = store
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![front.to_owned(), String::from("Answer!")],
                tags: Vec::new(),
            })
            .expect("note");
    }
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: collection_options(dir.path(), true),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| root.read(cx).review.current.is_some()));
    assert!(
        cx.update(|_, cx| root.read(cx).review.last_grade_at.is_none()),
        "loading a card must not arm the repeat guard"
    );

    let grade = |cx: &mut gpui::VisualTestContext| {
        cx.update_entity(&root, |root, cx| {
            root.review.phase = Phase::Answer;
            root.grade(ranki_model::Rating::Again, cx);
        });
        cx.read_entity(&root, |root, _| root.review.answered)
    };
    assert_eq!(grade(cx), 1, "the first grade must land immediately");
    assert_eq!(grade(cx), 1, "an immediate repeat must still be dropped");
    cx.update_entity(&root, |root, _| root.review.last_grade_at = None);
    assert_eq!(grade(cx), 2);
}

#[test]
fn reload_and_escape_bindings_respect_modal_contexts() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let plain = [gpui::KeyContext::parse("ranki").expect("ranki context")];
    let dialog = [
        gpui::KeyContext::parse("ranki").expect("ranki context"),
        gpui::KeyContext::parse("ranki_dialog").expect("dialog context"),
    ];
    let typing = [
        gpui::KeyContext::parse("ranki").expect("ranki context"),
        gpui::KeyContext::parse("ranki_input").expect("input context"),
    ];
    let bindings = |key: &str, contexts: &[gpui::KeyContext]| {
        let stroke = gpui::Keystroke::parse(key).expect("keystroke");
        keymap
            .bindings_for_input(std::slice::from_ref(&stroke), contexts)
            .0
            .len()
    };
    assert_eq!(bindings("f5", &plain), 1);
    assert_eq!(bindings("f5", &dialog), 0);
    assert_eq!(bindings("f5", &typing), 0);
    assert_eq!(bindings("escape", &plain), 1);
    let dialog_bindings = keymap.bindings_for_input(
        std::slice::from_ref(
            &gpui::Keystroke::parse("escape").expect("keystroke"),
        ),
        &dialog,
    );
    assert!(
        !dialog_bindings
            .0
            .iter()
            .any(|binding| { binding.action().name().contains("EndSession") }),
        "escape in a dialog must only cancel"
    );
}

#[gpui::test]
fn request_reload_preserves_a_destructive_confirmation(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, _| {
        root.review.answered = 2;
        root.confirmation = Some(super::Confirmation::RemoveDeck {
            id: 7,
            name: String::from("Important"),
        });
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.request_reload(window, cx));
    });
    let confirmation =
        cx.read_entity(&root, |root, _| root.confirmation.clone());
    assert_eq!(
        confirmation,
        Some(super::Confirmation::RemoveDeck {
            id: 7,
            name: String::from("Important"),
        })
    );
}

#[gpui::test]
fn failed_removals_keep_their_confirmation(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    let deck = super::Confirmation::RemoveDeck {
        id: 42,
        name: String::from("Default"),
    };
    cx.update_entity(&root, |root, _| root.confirmation = Some(deck.clone()));
    cx.update_entity(&root, super::Root::confirm);
    assert_eq!(
        cx.read_entity(&root, |root, _| root.confirmation.clone()),
        Some(deck)
    );

    let card = super::Confirmation::RemoveCard {
        id: 9,
        front: String::from("Question?"),
    };
    cx.update_entity(&root, |root, _| root.confirmation = Some(card.clone()));
    cx.update_entity(&root, super::Root::confirm);
    assert_eq!(
        cx.read_entity(&root, |root, _| root.confirmation.clone()),
        Some(card)
    );
}

#[gpui::test]
fn external_change_aborts_grade_undo_and_save_note(
    cx: &mut gpui::TestAppContext,
) {
    let (store, stamp, writes) = ExternalChangeStore::new();
    let card = store.card.clone();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });
    assert!(cx.update(|_, cx| root.read(cx).review.current.is_some()));

    stamp.store(2, Ordering::Relaxed);
    cx.update_entity(&root, |root, _| {
        root.last_change_check = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("time");
    });
    cx.update_entity(&root, |root, cx| {
        root.review.phase = Phase::Answer;
        root.grade(ranki_model::Rating::Again, cx);
    });
    assert_eq!(writes.load(Ordering::Relaxed), 0, "grade must not write");
    assert!(cx.update(|_, cx| root.read(cx).external_change));
    let toast = cx.update(|_, cx| {
        root.read(cx).toast.as_ref().map(|toast| toast.text.clone())
    });
    assert!(
        toast.as_ref().is_some_and(|text| text.contains("Reload")),
        "{toast:?}"
    );

    stamp.store(1, Ordering::Relaxed);
    cx.update_entity(&root, |root, _| {
        root.external_change = false;
        root.review.last_grade = Some(super::UndoEntry {
            card: card.clone(),
            revlog_id: 1,
            rating: ranki_model::Rating::Again,
            spent: Duration::ZERO,
            requeued: false,
        });
        root.last_change_check = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("time");
    });
    stamp.store(2, Ordering::Relaxed);
    cx.update_entity(&root, super::Root::undo_grade);
    assert_eq!(writes.load(Ordering::Relaxed), 0, "undo must not write");

    stamp.store(1, Ordering::Relaxed);
    cx.update_entity(&root, |root, _| {
        root.external_change = false;
        root.view = View::AddCard;
        root.add.deck_id = Some(1);
        root.last_change_check = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("time");
    });
    stamp.store(2, Ordering::Relaxed);
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            let fields = root.add.fields.clone();
            for field in fields {
                field.update(cx, |field, cx| {
                    field.insert_text("Question", cx);
                });
            }
            root.save_note(window, cx);
        });
    });
    assert_eq!(writes.load(Ordering::Relaxed), 0, "note must not write");
}

#[gpui::test]
fn focused_again_button_handles_enter_and_space_once(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    for front in ["First?", "Second?"] {
        let _ = store
            .add_note(&NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![front.to_owned(), String::from("Answer!")],
                tags: Vec::new(),
            })
            .expect("note");
    }
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: collection_options(dir.path(), true),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    assert_eq!(
        cx.update(|_, cx| root.read(cx).review.phase),
        Phase::Answer,
        "Enter reveals the question before grading"
    );
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("tab");
    assert!(
        !cx.update(|window, cx| root.read(cx).focus.is_focused(window)),
        "the rating button must own the focus"
    );
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").expect("keystroke"),
    });
    let (answered, rating, phase) = cx.read_entity(&root, |root, _| {
        (
            root.review.answered,
            root.review.last_grade.as_ref().map(|entry| entry.rating),
            root.review.phase,
        )
    });
    assert_eq!(answered, 1);
    assert_eq!(rating, Some(ranki_model::Rating::Again));
    assert_eq!(phase, Phase::Question, "Enter must not reveal twice");

    cx.update_entity(&root, |root, _| {
        root.review.last_grade_at = None;
        root.review.phase = Phase::Answer;
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let again_focus = cx.update(|_, cx| {
        root.read(cx)
            .review_button_focus
            .borrow()
            .get(2)
            .cloned()
            .expect("Again focus handle")
    });
    cx.update(|window, _| window.focus(&again_focus));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("space");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("space").expect("keystroke"),
    });
    let (answered, rating, phase) = cx.read_entity(&root, |root, _| {
        (
            root.review.answered,
            root.review.last_grade.as_ref().map(|entry| entry.rating),
            root.review.phase,
        )
    });
    assert_eq!(answered, 2);
    assert_eq!(rating, Some(ranki_model::Rating::Again));
    assert_eq!(phase, Phase::Question, "Space must not reveal twice");
}

#[gpui::test]
fn browse_and_statistics_wait_for_their_view(cx: &mut gpui::TestAppContext) {
    let store = BrowseStore::with_cards(2);
    let finds = Arc::clone(&store.find_reads);
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    assert_eq!(finds.load(Ordering::Relaxed), 0, "Browse was not shown");

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::OpenBrowse, window, cx);
        });
    });
    assert_eq!(finds.load(Ordering::Relaxed), 1);
    assert_eq!(cx.update(|_, cx| root.read(cx).browse.hits.len()), 2);

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::OpenStatistics, window, cx);
        });
    });
    assert_eq!(
        cx.read_entity(&root, |root, _| (root.view, root.statistics_ready)),
        (View::Statistics, true)
    );
    cx.run_until_parked();
}

#[gpui::test]
fn a_saved_browse_or_statistics_view_still_loads_on_start(
    cx: &mut gpui::TestAppContext,
) {
    let store = BrowseStore::with_cards(2);
    let finds = Arc::clone(&store.find_reads);
    let mut options = Options::default();
    options.ui_state.view = ranki_model::UiView::Browse;
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    assert!(finds.load(Ordering::Relaxed) > 0);
    assert_eq!(cx.update(|_, cx| root.read(cx).browse.hits.len()), 2);

    let store = BrowseStore::with_cards(1);
    let mut options = Options::default();
    options.ui_state.view = ranki_model::UiView::Statistics;
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    assert_eq!(cx.update(|_, cx| root.read(cx).view), View::Statistics);
    assert!(cx.update(|_, cx| root.read(cx).statistics_ready));
}

#[gpui::test]
fn import_guard_blocks_duplicates_and_reload(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.review.answered = 1;
        root.import_running = true;
        root.import_path(PathBuf::from("deck.apkg"), cx);
    });
    let toast = cx.update(|_, cx| {
        root.read(cx).toast.as_ref().map(|toast| toast.text.clone())
    });
    assert!(
        toast
            .as_ref()
            .is_some_and(|text| text.contains("already running")),
        "{toast:?}"
    );
    assert!(cx.update(|_, cx| root.read(cx).import_running));

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::Reload, window, cx);
            root.reload_collection(cx);
        });
    });
    let (view, confirmation) =
        cx.read_entity(&root, |root, _| (root.view, root.confirmation.clone()));
    assert_eq!(view, View::Review, "reload must wait for the import");
    assert!(confirmation.is_none());
    let toast = cx.update(|_, cx| {
        root.read(cx).toast.as_ref().map(|toast| toast.text.clone())
    });
    assert!(
        toast
            .as_ref()
            .is_some_and(|text| text.contains("Wait for it to finish")),
        "{toast:?}"
    );
}

#[gpui::test]
fn reduce_motion_setting_is_saved_and_applies(cx: &mut gpui::TestAppContext) {
    let (saver, latest) = recording_saver();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: saver,
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.view = View::Settings;
            root.dispatch(UiAction::SetReduceMotion(Some(true)), window, cx);
        });
    });
    assert_eq!(cx.update(|_, cx| root.read(cx).motion_ms(240)), 0);

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::SetReduceMotion(Some(false)), window, cx);
        });
    });
    cx.dispatch_action(super::UseReducedMotion);
    cx.run_until_parked();
    assert!(
        cx.read_entity(&root, |root, _| root.options.reduce_motion
            == Some(true)),
        "the settings action must reach the app"
    );
    let recorded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings");
    assert_eq!(recorded.reduce_motion, Some(true));

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::SetReduceMotion(Some(false)), window, cx);
        });
    });
    assert_eq!(cx.update(|_, cx| root.read(cx).motion_ms(240)), 240);
    let recorded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings");
    assert_eq!(recorded.reduce_motion, Some(false));

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::SetReduceMotion(None), window, cx);
        });
    });
    let recorded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings");
    assert_eq!(recorded.reduce_motion, None, "System must stay explicit");

    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    assert!(cx.debug_bounds("motion-Some(true)").is_some());
    assert!(cx.debug_bounds("motion-None").is_some());
}

fn test_card(id: Id, queue: CardQueue) -> Card {
    Card {
        id,
        note_id: id,
        deck_id: 1,
        ordinal: 0,
        modified: 0,
        usn: 0,
        card_type: CardType::New,
        queue,
        due: 0,
        interval: 0,
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

#[gpui::test]
fn statistics_failure_keeps_the_last_good_summary(
    cx: &mut gpui::TestAppContext,
) {
    let fail = Arc::new(AtomicBool::new(true));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(StatisticsStore {
                    reads: Arc::new(AtomicUsize::new(0)),
                    modified: Arc::new(AtomicI64::new(7)),
                    fail: Arc::clone(&fail),
                }),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.statistics.total_reviews = 9;
        root.refresh_statistics(cx);
    });
    assert_eq!(
        cx.read_entity(&root, |root, _| root.statistics.total_reviews),
        9,
        "a failed build must keep the cached summary"
    );
    assert_eq!(
        cx.read_entity(&root, |root, _| root.statistics_error.clone()),
        Some(String::from("invalid data: offline"))
    );

    fail.store(false, Ordering::Relaxed);
    cx.update_entity(&root, super::Root::refresh_statistics);
    let (total, error) = cx.read_entity(&root, |root, _| {
        (root.statistics.total_reviews, root.statistics_error.clone())
    });
    assert_eq!(total, 0);
    assert_eq!(error, None);
}

#[gpui::test]
fn statistics_retry_stays_on_statistics(cx: &mut gpui::TestAppContext) {
    let mut options = Options::default();
    options.paths.collection =
        std::path::PathBuf::from("no-such-collection.anki2");
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(StatisticsStore {
                    reads: Arc::new(AtomicUsize::new(0)),
                    modified: Arc::new(AtomicI64::new(7)),
                    fail: Arc::new(AtomicBool::new(false)),
                }),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, _| {
        root.statistics.total_reviews = 4;
        root.statistics_modified = 0;
        root.statistics_next_day_at = 0;
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::OpenStatistics, window, cx);
        });
    });
    cx.run_until_parked();

    let (view, total, error) = cx.read_entity(&root, |root, _| {
        (
            root.view,
            root.statistics.total_reviews,
            root.statistics_error.clone(),
        )
    });
    assert_eq!(view, View::Statistics);
    assert_eq!(total, 4, "a background failure must keep the cache");
    assert!(error.is_some());

    let before = cx.read_entity(&root, |root, _| root.statistics_generation);
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(UiAction::RetryStatistics, window, cx);
        });
    });
    cx.run_until_parked();
    let (view, generation, error) = cx.read_entity(&root, |root, _| {
        (
            root.view,
            root.statistics_generation,
            root.statistics_error.clone(),
        )
    });
    assert_eq!(view, View::Statistics, "retry must not navigate to Decks");
    assert_eq!(generation, before.wrapping_add(1));
    assert!(error.is_some());
}

#[gpui::test]
fn auto_sync_toggle_persists_and_rolls_back(cx: &mut gpui::TestAppContext) {
    let fail = Arc::new(AtomicBool::new(false));
    let recorded = Arc::new(Mutex::new(None));
    let for_saver = Arc::clone(&recorded);
    let saver_fail = Arc::clone(&fail);
    let saver: crate::SettingsSaver = Arc::new(move |settings| {
        *for_saver.lock().expect("settings lock") = Some(settings.clone());
        if saver_fail.load(Ordering::Relaxed) {
            Err(String::from("disk full"))
        } else {
            Ok(())
        }
    });
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: saver,
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.sync.signed_in = true;
        root.options.sync.auto = false;
        root.toggle_auto_sync(cx);
    });
    assert!(cx.read_entity(&root, |root, _| root.options.sync.auto));
    assert!(
        recorded
            .lock()
            .expect("settings lock")
            .as_ref()
            .is_some_and(|settings| settings.sync.auto)
    );
    assert!(
        cx.read_entity(&root, |root, _| root.toast.is_none()),
        "unexpected toast: {:?}",
        cx.read_entity(&root, |root, _| root.toast.clone())
    );

    fail.store(true, Ordering::Relaxed);
    cx.update_entity(&root, super::Root::toggle_auto_sync);
    assert!(
        cx.read_entity(&root, |root, _| root.options.sync.auto),
        "a failed save must restore the previous value"
    );
    assert_eq!(
        cx.read_entity(&root, |root, _| root.toast.clone())
            .map(|toast| toast.text),
        Some(String::from("Could not save sync setting: disk full"))
    );
}

#[gpui::test]
fn remote_download_keeps_the_view_and_drafts_but_ends_review(
    cx: &mut gpui::TestAppContext,
) {
    let directory = tempfile::tempdir().expect("temp dir");
    let collection = directory.path().join("collection.anki2");
    if ranki_storage::Collection::create(&collection).is_err() {
        return;
    }
    let mut options = Options::default();
    options.paths.collection = collection.clone();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ranki_storage::ReopeningCollection::new(
                    &collection,
                )),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });

    cx.update_entity(&root, |root, cx| {
        root.view = View::Settings;
        root.previous_view = View::Browse;
        if let Some(field) = root.add.fields.first().cloned() {
            field.update(cx, |field, cx| {
                field.seed(String::from("kept draft"), cx);
            });
        }
        root.browse.search.update(cx, |field, cx| {
            field.seed(String::from("kept query"), cx);
        });
        root.reopen_collection(cx);
    });
    let (view, previous, query) = cx.read_entity(&root, |root, cx| {
        (
            root.view,
            root.previous_view,
            root.browse.search.read(cx).text(),
        )
    });
    assert_eq!(view, View::Settings);
    assert_eq!(previous, View::Browse);
    assert_eq!(query, "kept query");
    let draft = cx.read_entity(&root, |root, cx| {
        root.add
            .fields
            .first()
            .map(|field| field.read(cx).text())
            .unwrap_or_default()
    });
    assert_eq!(draft, "kept draft");

    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.review.answered = 3;
        root.reopen_collection(cx);
    });
    let (view, current, queued, toast) = cx.read_entity(&root, |root, _| {
        (
            root.view,
            root.review.current.is_some(),
            !root.review.queue.is_empty(),
            root.toast.clone().map(|toast| toast.text),
        )
    });
    assert_eq!(view, View::Decks);
    assert!(!current && !queued, "stale review cards must be cleared");
    assert!(toast.is_some_and(|text| text.contains("study session ended")));
}

#[gpui::test]
fn changing_the_theme_keeps_the_current_focus(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    let focus = cx.read_entity(&root, |root, cx| {
        root.sync.username.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&focus));
    assert!(cx.update(|window, _| focus.is_focused(window)));

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.dispatch(
                UiAction::SetTheme(ranki_model::ThemeChoice::Dark),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.read_entity(&root, |root, _| root.options.theme),
        ranki_model::ThemeChoice::Dark
    );
    assert!(
        cx.update(|window, _| focus.is_focused(window)),
        "theme changes must not move focus to the settings panel"
    );
}

#[gpui::test]
fn revealing_a_long_answer_anchors_the_stage_to_the_answer_top(
    cx: &mut gpui::TestAppContext,
) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.review.current = Some(Card {
            id: 1,
            note_id: 1,
            deck_id: 1,
            ordinal: 0,
            modified: 0,
            usn: 0,
            card_type: CardType::New,
            queue: CardQueue::New,
            due: 0,
            interval: 0,
            factor: 0,
            reps: 0,
            lapses: 0,
            left: 0,
            original_due: 0,
            original_deck_id: 0,
            flags: 0,
            data: String::new(),
        });
        root.review.question = Some(crate::card_view::RenderedCard {
            text: "Question?".into(),
            ..Default::default()
        });
        root.review.answer = Some(crate::card_view::RenderedCard {
            text: "Answer line\n".repeat(60),
            ..Default::default()
        });
        root.review.phase = Phase::Question;
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let before = cx.read_entity(&root, |root, _| {
        f32::from(root.review_stage_scroll.offset().y)
    });
    assert_eq!(before, 0.0, "the question starts at the top");

    cx.update_entity(&root, super::Root::reveal);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
        let _ = window.draw(cx);
    });
    let (offset, max, expected) = cx.read_entity(&root, |root, _| {
        let handle = &root.review_stage_scroll;
        let origin = root
            .review_answer_origin
            .get()
            .expect("the answer was prepainted");
        (
            f32::from(handle.offset().y),
            f32::from(handle.max_offset().height),
            f32::from(handle.bounds().origin.y - origin.y),
        )
    });
    assert!(max > 0.0, "the long answer must overflow the stage");
    assert_eq!(
        offset, expected,
        "revealing must stop with the answer group at the viewport top"
    );
    assert!(offset > -max, "the answer top is not the answer bottom");
}

#[gpui::test]
fn escape_closes_help_before_ending_review(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.review.current = Some(test_card(1, CardQueue::New));
        root.help_open = true;
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });

    cx.simulate_keystrokes("escape");
    assert!(!cx.read_entity(&root, |root, _| root.help_open));
    assert_eq!(cx.read_entity(&root, |root, _| root.view), View::Review);
    cx.run_until_parked();
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.simulate_keystrokes("escape");
    assert_eq!(cx.read_entity(&root, |root, _| root.view), View::Decks);
}

#[gpui::test]
fn load_failures_after_grading_keep_the_summary(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.review.current = None;
        root.review.answered = 2;
        root.review.load_errors = 3;
        cx.notify();
    });
    assert!(cx.read_entity(&root, |root, _| root.session_complete()));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    cx.update(|_, cx| cx.bind_keys(super::key_bindings()));
    cx.simulate_keystrokes("space");
    assert_eq!(
        cx.read_entity(&root, |root, _| root.view),
        View::Decks,
        "Space follows the summary's Back to decks action"
    );

    cx.update_entity(&root, |root, _| {
        root.view = View::Review;
        root.review.answered = 0;
    });
    assert!(!cx.read_entity(&root, |root, _| root.session_complete()));
    assert_eq!(cx.read_entity(&root, |root, _| root.review.load_errors), 3);
}

#[gpui::test]
fn unschedulable_answers_return_to_the_question(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::Review;
        root.review.current = Some(test_card(1, CardQueue::Suspended));
        root.review.phase = Phase::Answer;
        root.review.last_grade_at = None;
        root.grade(ranki_model::Rating::Good, cx);
    });
    let (phase, toast, sequence) = cx.read_entity(&root, |root, _| {
        (
            root.review.phase,
            root.toast.as_ref().map(|toast| toast.text.clone()),
            root.toast.as_ref().map(|toast| toast.seq),
        )
    });
    assert_eq!(
        phase,
        Phase::Question,
        "a rejected grade leaves no rating row"
    );
    assert_eq!(
        toast.as_deref(),
        Some("This card is no longer due. Refresh the collection.")
    );

    cx.update_entity(&root, |root, cx| {
        root.review.last_grade_at = None;
        root.grade(ranki_model::Rating::Good, cx);
    });
    assert_eq!(
        cx.read_entity(&root, |root, _| root.toast.as_ref().map(|t| t.seq)),
        sequence,
        "a second grade must reveal, not repeat the error toast"
    );
}

#[gpui::test]
fn undo_after_a_session_sync_repeats_the_sync(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let collection = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&collection).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        sync: ranki_model::SyncSettings {
            enabled: true,
            ..Default::default()
        },
        ..Options::default()
    };
    let syncs = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&syncs);
    let (root, cx) = cx.add_window_view(|window, cx| {
        let mut root = super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        );
        root.sync.signed_in = true;
        root.sync_run = Arc::new(move |_, _, _, _, _| {
            counted.fetch_add(1, Ordering::Relaxed);
            Ok(ranki_sync::SyncOutcome::Uploaded)
        });
        root
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });
    cx.update_entity(&root, |root, cx| {
        root.review.phase = Phase::Answer;
        root.review.last_grade_at = None;
        root.grade(ranki_model::Rating::Easy, cx);
    });
    cx.run_until_parked();
    assert_eq!(syncs.load(Ordering::Relaxed), 1);
    let (dirty, sequence) = cx.read_entity(&root, |root, _| {
        (root.review.session_dirty, root.review.animation_seq)
    });
    assert!(dirty, "a graded session needs a repeat sync after undo");

    cx.update_entity(&root, super::Root::undo_grade);
    cx.run_until_parked();
    assert_eq!(
        syncs.load(Ordering::Relaxed),
        2,
        "undo must push the corrected log again"
    );
    let (dirty, after) = cx.read_entity(&root, |root, _| {
        (root.review.session_dirty, root.review.animation_seq)
    });
    assert!(!dirty);
    assert!(after > sequence, "undo must restart the review animations");
}

#[gpui::test]
fn summary_keys_start_study_and_leave_the_session(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let dir = tempfile::tempdir().expect("temp dir");
    let collection = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&collection).expect("collection");
    let _ = store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| root.study_all(window, cx));
    });
    cx.update_entity(&root, |root, cx| {
        root.review.phase = Phase::Answer;
        root.review.last_grade_at = None;
        root.grade(ranki_model::Rating::Easy, cx);
    });
    cx.run_until_parked();
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });

    cx.simulate_keystrokes("s");
    let (view, answered) =
        cx.read_entity(&root, |root, _| (root.view, root.review.answered));
    assert_eq!(view, View::Review);
    assert_eq!(answered, 0, "S restarts the study session");

    cx.simulate_keystrokes("escape");
    assert_eq!(cx.read_entity(&root, |root, _| root.view), View::Decks);
}

#[gpui::test]
fn inline_rename_focuses_and_selects_the_field(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            let deck = root.decks[0].clone();
            root.start_rename(&deck, window, cx);
        });
    });
    let focus = cx.read_entity(&root, |root, cx| {
        root.rename_field.read(cx).focus_handle().clone()
    });
    assert!(cx.update(|window, _| focus.is_focused(window)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("study");
    assert_eq!(
        cx.read_entity(&root, |root, cx| root.rename_field.read(cx).text()),
        "study",
        "typing must replace the selected deck name"
    );
}

#[gpui::test]
fn enter_applies_inline_rename_and_reports_it(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store.create_deck("Topic::Child").expect("deck");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let parent = cx.read_entity(&root, |root, _| {
        root.decks
            .iter()
            .find(|deck| deck.name == "Topic")
            .cloned()
            .expect("parent deck")
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.start_rename(&parent, window, cx);
        });
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_keystrokes("study");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let state = cx.update(|window, app| {
        let root = root.read(app);
        (
            root.decks
                .iter()
                .map(|deck| deck.name.clone())
                .collect::<Vec<_>>(),
            root.deck_edit.is_some(),
            root.toast.clone(),
            root.focus.is_focused(window),
        )
    });
    assert!(state.0.contains(&String::from("study")));
    assert!(state.0.contains(&String::from("study::Child")));
    assert!(!state.1, "applying a rename leaves edit mode");
    assert!(matches!(
        state.2.as_ref().map(|toast| toast.kind),
        Some(super::ToastKind::Success)
    ));
    assert_eq!(
        state.2.as_ref().map(|toast| toast.text.as_str()),
        Some("Deck renamed")
    );
    assert!(state.3, "renaming returns focus to the app");
}

#[gpui::test]
fn escape_and_cancel_leave_inline_rename(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            let deck = root.decks[0].clone();
            root.start_rename(&deck, window, cx);
        });
    });
    cx.simulate_keystrokes("escape");
    assert!(cx.read_entity(&root, |root, _| root.deck_edit.is_none()));
    let focus = cx.read_entity(&root, |root, _| root.focus.clone());
    assert!(cx.update(|window, _| focus.is_focused(window)));

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            let deck = root.decks[0].clone();
            root.start_rename(&deck, window, cx);
        });
    });
    cx.dispatch_action(super::CancelDialog);
    assert!(cx.read_entity(&root, |root, _| root.deck_edit.is_none()));
    assert!(cx.update(|window, _| focus.is_focused(window)));
}

#[gpui::test]
fn inline_rename_does_not_leak_into_review_or_confirmations(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    store
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec![String::from("Question?"), String::from("Answer!")],
            tags: Vec::new(),
        })
        .expect("note");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let deck = cx.read_entity(&root, |root, _| root.decks[0].clone());
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.start_rename(&deck, window, cx);
            root.rename_field.update(cx, |field, cx| {
                field.set_text("Changed", cx);
            });
            root.study_deck(0, window, cx);
        });
    });
    assert!(cx.read_entity(&root, |root, _| root.deck_edit.is_none()));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let state = cx.read_entity(&root, |root, _| {
        (root.view, root.review.phase, root.decks[0].name.clone())
    });
    assert_eq!(state.0, View::Review);
    assert_eq!(state.1, super::Phase::Answer);
    assert_eq!(state.2, "Default", "Enter must reveal, not rename");

    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.end_session(cx);
            root.deck_edit = Some(super::DeckEdit { id: deck.id });
            root.confirmation = Some(super::Confirmation::Reload);
            root.dispatch(super::UiAction::Confirm, window, cx);
        });
    });
    let state = cx.read_entity(&root, |root, _| {
        (root.deck_edit.is_some(), root.confirmation.is_some())
    });
    assert!(!state.0, "confirmation clears stale rename state");
    assert!(!state.1, "confirmation is handled before rename state");
}

#[gpui::test]
fn deck_remove_confirmation_focuses_dialog_and_enter_confirms(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let topic = store.create_deck("Topic").expect("deck");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update_entity(&root, |root, cx| {
        root.deck_menu = Some(super::DeckMenu {
            id: topic.id,
            anchor: point(px(100.), px(100.)),
        });
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let remove = cx
        .debug_bounds("decks-remove")
        .expect("deck remove menu item");
    cx.simulate_click(remove.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let dialog = cx.read_entity(&root, |root, _| root.dialog_focus.clone());
    assert!(cx.update(|window, _| dialog.is_focused(window)));

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let state = cx.update(|_, app| {
        let root = root.read(app);
        (
            root.confirmation.is_none(),
            root.decks
                .iter()
                .map(|deck| deck.name.clone())
                .collect::<Vec<_>>(),
            root.toast.clone(),
        )
    });
    assert!(state.0, "Enter confirms the deck removal");
    assert_eq!(state.1, [String::from("Default")]);
    assert_eq!(
        state.2.as_ref().map(|toast| toast.text.as_str()),
        Some("Deck removed")
    );
}

#[test]
fn deck_shortcuts_are_bound_only_outside_inputs_and_dialogs() {
    let mut keymap = gpui::Keymap::default();
    keymap.add_bindings(super::key_bindings());
    let contexts = |names: &[&str]| {
        names
            .iter()
            .map(|name| gpui::KeyContext::parse(name).expect("context"))
            .collect::<Vec<_>>()
    };
    let decks = contexts(&["ranki", "ranki_decks"]);
    let typing = contexts(&["ranki", "ranki_decks", "ranki_input"]);
    let dialog = contexts(&["ranki", "ranki_decks", "ranki_dialog"]);
    let elsewhere = contexts(&["ranki"]);
    for key in ["/", "c"] {
        let stroke = gpui::Keystroke::parse(key).expect("keystroke");
        assert_eq!(
            keymap
                .bindings_for_input(std::slice::from_ref(&stroke), &decks)
                .0
                .len(),
            1,
            "{key} must be bound on the decks screen"
        );
        assert!(
            keymap
                .bindings_for_input(std::slice::from_ref(&stroke), &typing)
                .0
                .is_empty(),
            "{key} must stay out of text fields"
        );
        assert!(
            keymap
                .bindings_for_input(std::slice::from_ref(&stroke), &dialog)
                .0
                .is_empty(),
            "{key} must stay out of dialogs"
        );
        assert!(
            keymap
                .bindings_for_input(std::slice::from_ref(&stroke), &elsewhere)
                .0
                .is_empty(),
            "{key} must stay out of other views"
        );
    }
}

#[gpui::test]
fn slash_focuses_deck_search_and_clear_search_keeps_it(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let root_focus = cx.read_entity(&root, |root, _| root.focus.clone());
    cx.update(|window, _| window.focus(&root_focus));
    cx.simulate_keystrokes("/");
    cx.run_until_parked();
    let search = cx.read_entity(&root, |root, cx| {
        root.deck_search.read(cx).focus_handle().clone()
    });
    assert!(cx.update(|window, _| search.is_focused(window)));
    assert_eq!(
        cx.read_entity(&root, |root, cx| root.deck_search.read(cx).text()),
        ""
    );

    cx.update_entity(&root, |root, cx| {
        root.deck_search
            .update(cx, |field, cx| field.set_text("nothing", cx));
    });
    cx.dispatch_action(super::ClearSearch);
    cx.run_until_parked();
    let (text, focused) = cx.update(|window, app| {
        let root = root.read(app);
        (root.deck_search.read(app).text(), search.is_focused(window))
    });
    assert_eq!(text, "");
    assert!(focused, "clearing the search keeps the field focused");

    cx.update(|window, _| window.focus(&root_focus));
    cx.simulate_keystrokes("c");
    cx.run_until_parked();
    assert!(cx.update(|_, app| root.read(app).create_modal));
}

#[gpui::test]
fn creating_a_deck_selects_it_for_add_card(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let _ = store.create_deck("Topic").expect("deck");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.selected = Some(0);
            root.add.deck_id = Some(root.decks[0].id);
            root.create_field.update(cx, |field, cx| {
                field.set_text("Fresh", cx);
            });
            root.create_deck(window, cx);
        });
    });
    let state = cx.update(|_, app| {
        let root = root.read(app);
        (
            root.selected
                .and_then(|index| root.decks.get(index))
                .map(|deck| deck.name.clone()),
            root.add
                .deck_id
                .and_then(|id| root.decks.iter().find(|deck| deck.id == id))
                .map(|deck| deck.name.clone()),
            root.toast.clone(),
        )
    });
    assert_eq!(state.0.as_deref(), Some("Fresh"));
    assert_eq!(state.1.as_deref(), Some("Fresh"));
    assert_eq!(
        state.2.as_ref().map(|toast| toast.text.as_str()),
        Some("Deck created")
    );
}

#[gpui::test]
fn renaming_a_parent_rewrites_and_saves_expanded_paths(
    cx: &mut gpui::TestAppContext,
) {
    let (saver, latest) = recording_saver();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    for name in [
        "Topic",
        "Topic::Child",
        "Topic::Child::Grand",
        "Other::Leaf",
    ] {
        let _ = store.create_deck(name).expect("deck");
    }
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: saver,
            },
            window,
            cx,
        )
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.expanded = ["Topic", "Topic::Child", "Other"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            let parent = root
                .decks
                .iter()
                .find(|deck| deck.name == "Topic")
                .cloned()
                .expect("parent deck");
            root.start_rename(&parent, window, cx);
            root.rename_field.update(cx, |field, cx| {
                field.set_text("Study", cx);
            });
            root.apply_rename(window, cx);
        });
    });
    let mut expanded = cx.read_entity(&root, |root, _| {
        root.expanded.iter().cloned().collect::<Vec<_>>()
    });
    expanded.sort();
    assert_eq!(expanded, ["Other", "Study", "Study::Child"]);

    let mut saved_expanded = latest
        .lock()
        .expect("settings lock")
        .clone()
        .expect("recorded settings")
        .ui_state
        .expanded_decks;
    saved_expanded.sort();
    assert_eq!(saved_expanded, ["Other", "Study", "Study::Child"]);
    let names = cx.read_entity(&root, |root, _| {
        root.decks
            .iter()
            .map(|deck| deck.name.clone())
            .collect::<Vec<_>>()
    });
    assert!(names.contains(&String::from("Study::Child::Grand")));
}

#[gpui::test]
fn toggling_a_chevron_closes_the_deck_menu(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("collection.anki2");
    let mut store =
        ranki_storage::Collection::create(&path).expect("collection");
    let topic = store.create_deck("Topic::Child").expect("deck");
    let options = Options {
        paths: ranki_model::DataPaths::new(dir.path()),
        ..Options::default()
    };
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update_entity(&root, |root, cx| {
        root.deck_menu = Some(super::DeckMenu {
            id: topic.id,
            anchor: point(px(100.), px(100.)),
        });
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let chevron = cx
        .debug_bounds("decks-chevron-Topic")
        .expect("chevron bounds");
    cx.simulate_click(chevron.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let state = cx.read_entity(&root, |root, _| {
        (root.deck_menu.is_none(), root.expanded.contains("Topic"))
    });
    assert!(state.0, "toggling a row closes its stale menu");
    assert!(state.1);
}

#[gpui::test]
fn scrolling_decks_closes_the_deck_menu(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.update_entity(&root, |root, cx| {
        root.deck_menu = Some(super::DeckMenu {
            id: 1,
            anchor: point(px(100.), px(100.)),
        });
        cx.notify();
    });
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: point(px(1100.), px(600.)),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-20.))),
        modifiers: gpui::Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();
    assert!(cx.read_entity(&root, |root, _| root.deck_menu.is_none()));
}

#[gpui::test]
fn clicking_a_virtual_parent_toggles_expansion(cx: &mut gpui::TestAppContext) {
    let store = BrowseStore {
        decks: vec![Deck {
            id: 1,
            name: String::from("Topic::Child"),
            description: String::new(),
        }],
        models: ranki_model::stock_models(),
        cards: HashMap::new(),
        notes: HashMap::new(),
        ids: Vec::new(),
        card_reads: Arc::new(AtomicUsize::new(0)),
        find_reads: Arc::new(AtomicUsize::new(0)),
        fail_reads: false,
        fail_browse: false,
    };
    let options = Options::default();
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(store),
                options,
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let row = cx
        .debug_bounds("deck-row-Topic")
        .expect("virtual parent row");
    cx.simulate_click(row.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    let state = cx.read_entity(&root, |root, _| {
        (root.view, root.expanded.contains("Topic"))
    });
    assert_eq!(state.0, View::Decks);
    assert!(state.1, "a virtual parent row expands its children");
}

#[test]
fn deck_summary_labels_the_collection_wide_counts() {
    assert_eq!(super::DECK_SUMMARY_LABEL, "Due today (all decks)");
}

#[derive(Clone)]
struct EditorStore {
    decks: Vec<Deck>,
    models: Vec<NoteModel>,
    adds: Arc<AtomicUsize>,
    duplicate: bool,
    card_count: usize,
}

impl ranki_model::Store for EditorStore {
    fn decks(&self) -> Result<Vec<Deck>, StoreError> {
        Ok(self.decks.clone())
    }

    fn create_deck(&mut self, name: &str) -> Result<Deck, StoreError> {
        ModalStore.create_deck(name)
    }

    fn models(&self) -> Result<Vec<NoteModel>, StoreError> {
        Ok(self.models.clone())
    }

    fn ensure_model(
        &mut self,
        kind: StockModelKind,
    ) -> Result<NoteModel, StoreError> {
        if let Some(model) = self
            .models
            .iter()
            .find(|model| StockModelKind::of(model) == Some(kind))
        {
            return Ok(model.clone());
        }
        let mut model = kind.definition();
        model.id = i64::try_from(self.models.len() + 1).expect("model id");
        self.models.push(model.clone());
        Ok(model)
    }

    fn tags(&self) -> Result<Vec<String>, StoreError> {
        ModalStore.tags()
    }

    fn find_cards(&self, query: &str) -> Result<Vec<Id>, StoreError> {
        ModalStore.find_cards(query)
    }

    fn browse_cards(
        &self,
        query: &str,
    ) -> Result<Vec<ranki_model::BrowseCard>, StoreError> {
        ModalStore.browse_cards(query)
    }

    fn due_cards(
        &self,
        deck: Option<Id>,
        limit: usize,
        now: i64,
    ) -> Result<Vec<Card>, StoreError> {
        ModalStore.due_cards(deck, limit, now)
    }

    fn card(&self, id: Id) -> Result<Option<Card>, StoreError> {
        ModalStore.card(id)
    }

    fn note(&self, id: Id) -> Result<Option<Note>, StoreError> {
        ModalStore.note(id)
    }

    fn add_note(&mut self, _: &NewNote) -> Result<Vec<Id>, StoreError> {
        let _ = self.adds.fetch_add(1, Ordering::Relaxed);
        if self.duplicate {
            return Err(StoreError::InvalidData(String::from("duplicate")));
        }
        Ok((1..=i64::try_from(self.card_count).expect("count")).collect())
    }

    fn apply_review(
        &mut self,
        decision: &ReviewDecision,
    ) -> Result<Id, StoreError> {
        ModalStore.apply_review(decision)
    }

    fn stats(
        &self,
        deck: Option<Id>,
        now: i64,
    ) -> Result<ranki_model::DeckStats, StoreError> {
        ModalStore.stats(deck, now)
    }

    fn collection_modified(&self) -> Result<i64, StoreError> {
        ModalStore.collection_modified()
    }

    fn collection_created(&self) -> Result<i64, StoreError> {
        ModalStore.collection_created()
    }
}

#[derive(Default)]
struct StatisticsStore {
    reads: Arc<AtomicUsize>,
    modified: Arc<AtomicI64>,
    fail: Arc<AtomicBool>,
}

impl ranki_model::Store for StatisticsStore {
    fn decks(&self) -> Result<Vec<Deck>, StoreError> {
        ModalStore.decks()
    }

    fn create_deck(&mut self, name: &str) -> Result<Deck, StoreError> {
        ModalStore.create_deck(name)
    }

    fn models(&self) -> Result<Vec<NoteModel>, StoreError> {
        ModalStore.models()
    }

    fn find_cards(&self, query: &str) -> Result<Vec<Id>, StoreError> {
        ModalStore.find_cards(query)
    }

    fn browse_cards(
        &self,
        query: &str,
    ) -> Result<Vec<ranki_model::BrowseCard>, StoreError> {
        ModalStore.browse_cards(query)
    }

    fn due_cards(
        &self,
        deck_id: Option<Id>,
        limit: usize,
        now: i64,
    ) -> Result<Vec<Card>, StoreError> {
        ModalStore.due_cards(deck_id, limit, now)
    }

    fn card(&self, id: Id) -> Result<Option<Card>, StoreError> {
        ModalStore.card(id)
    }

    fn note(&self, id: Id) -> Result<Option<Note>, StoreError> {
        ModalStore.note(id)
    }

    fn add_note(&mut self, note: &NewNote) -> Result<Vec<Id>, StoreError> {
        ModalStore.add_note(note)
    }

    fn apply_review(
        &mut self,
        decision: &ReviewDecision,
    ) -> Result<Id, StoreError> {
        ModalStore.apply_review(decision)
    }

    fn stats(
        &self,
        deck_id: Option<Id>,
        now: i64,
    ) -> Result<ranki_model::DeckStats, StoreError> {
        ModalStore.stats(deck_id, now)
    }

    fn review_history(&self) -> Result<Vec<ranki_model::RevLog>, StoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.fail.load(Ordering::Relaxed) {
            return Err(StoreError::InvalidData(String::from("offline")));
        }
        Ok(Vec::new())
    }

    fn collection_modified(&self) -> Result<i64, StoreError> {
        Ok(self.modified.load(Ordering::Relaxed))
    }

    fn collection_created(&self) -> Result<i64, StoreError> {
        Ok(0)
    }
}

struct ExternalChangeStore {
    decks: Vec<Deck>,
    models: Vec<NoteModel>,
    card: Card,
    note: Note,
    stamp: Arc<AtomicI64>,
    writes: Arc<AtomicUsize>,
}

impl ranki_model::Store for ExternalChangeStore {
    fn decks(&self) -> Result<Vec<Deck>, StoreError> {
        Ok(self.decks.clone())
    }

    fn create_deck(&mut self, _: &str) -> Result<Deck, StoreError> {
        Err(StoreError::Unsupported(String::from("no writes in stub")))
    }

    fn models(&self) -> Result<Vec<NoteModel>, StoreError> {
        Ok(self.models.clone())
    }

    fn find_cards(&self, _: &str) -> Result<Vec<Id>, StoreError> {
        Ok(vec![self.card.id])
    }

    fn browse_cards(
        &self,
        _: &str,
    ) -> Result<Vec<ranki_model::BrowseCard>, StoreError> {
        Ok(Vec::new())
    }

    fn due_cards(
        &self,
        _: Option<Id>,
        _: usize,
        _: i64,
    ) -> Result<Vec<Card>, StoreError> {
        Ok(vec![self.card.clone()])
    }

    fn card(&self, id: Id) -> Result<Option<Card>, StoreError> {
        Ok((id == self.card.id).then(|| self.card.clone()))
    }

    fn note(&self, id: Id) -> Result<Option<Note>, StoreError> {
        Ok((id == self.note.id).then(|| self.note.clone()))
    }

    fn add_note(&mut self, _: &NewNote) -> Result<Vec<Id>, StoreError> {
        let _ = self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(vec![1])
    }

    fn update_note(&mut self, _: &Note, _: Id) -> Result<(), StoreError> {
        let _ = self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn apply_review(&mut self, _: &ReviewDecision) -> Result<Id, StoreError> {
        let _ = self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(1)
    }

    fn undo_review(&mut self, _: &Card, _: Id) -> Result<bool, StoreError> {
        let _ = self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    fn stats(
        &self,
        _: Option<Id>,
        _: i64,
    ) -> Result<ranki_model::DeckStats, StoreError> {
        Ok(ranki_model::DeckStats::default())
    }

    fn collection_modified(&self) -> Result<i64, StoreError> {
        Ok(self.stamp.load(Ordering::Relaxed))
    }

    fn collection_created(&self) -> Result<i64, StoreError> {
        Ok(0)
    }
}

impl EditorStore {
    fn new() -> Self {
        Self {
            decks: vec![Deck {
                id: 1,
                name: String::from("Default"),
                description: String::new(),
            }],
            models: vec![{
                let mut model = StockModelKind::Basic.definition();
                model.id = 1;
                model
            }],
            adds: Arc::new(AtomicUsize::new(0)),
            duplicate: false,
            card_count: 1,
        }
    }

    fn with_decks(names: &[&str]) -> Self {
        Self {
            decks: names
                .iter()
                .enumerate()
                .map(|(index, name)| Deck {
                    id: i64::try_from(index + 1).expect("deck id"),
                    name: (*name).to_owned(),
                    description: String::new(),
                })
                .collect(),
            ..Self::new()
        }
    }
}

impl ExternalChangeStore {
    fn new() -> (Self, Arc<AtomicI64>, Arc<AtomicUsize>) {
        let models = ranki_model::stock_models()
            .into_iter()
            .enumerate()
            .map(|(index, mut model)| {
                model.id = i64::try_from(index + 1).expect("model id");
                model
            })
            .collect::<Vec<_>>();
        let card = Card {
            id: 1,
            note_id: 1,
            deck_id: 1,
            ordinal: 0,
            modified: 0,
            usn: 0,
            card_type: CardType::New,
            queue: CardQueue::New,
            due: 0,
            interval: 0,
            factor: 0,
            reps: 0,
            lapses: 0,
            left: 0,
            original_due: 0,
            original_deck_id: 0,
            flags: 0,
            data: String::new(),
        };
        let note = Note {
            id: 1,
            guid: String::new(),
            model_id: 1,
            modified: 0,
            usn: 0,
            tags: Vec::new(),
            fields: vec![String::from("Question?"), String::from("Answer!")],
            sort_field: String::new(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        let stamp = Arc::new(AtomicI64::new(1));
        let writes = Arc::new(AtomicUsize::new(0));
        let store = Self {
            decks: vec![Deck {
                id: 1,
                name: String::from("Default"),
                description: String::new(),
            }],
            models,
            card,
            note,
            stamp: Arc::clone(&stamp),
            writes: Arc::clone(&writes),
        };
        (store, stamp, writes)
    }
}

fn editor_launch(store: EditorStore) -> super::Launch {
    super::Launch {
        store: Box::new(store),
        options: Options::default(),
        save_settings: Arc::new(|_| Ok(())),
    }
}

fn draw_editor(
    cx: &mut gpui::VisualTestContext,
    root: &gpui::Entity<super::Root>,
) {
    cx.simulate_resize(size(px(1280.), px(800.)));
    cx.draw(point(px(0.), px(0.)), size(px(1280.), px(800.)), |_, _| {
        root.clone()
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn open_editor(
    cx: &mut gpui::VisualTestContext,
    root: &gpui::Entity<super::Root>,
) {
    cx.update_entity(root, |root, cx| {
        root.view = View::AddCard;
        cx.notify();
    });
    draw_editor(cx, root);
}

fn field_focus(
    cx: &mut gpui::VisualTestContext,
    root: &gpui::Entity<super::Root>,
    index: usize,
) -> gpui::FocusHandle {
    cx.read_entity(root, |root, cx| {
        root.add.fields[index].read(cx).focus_handle().clone()
    })
}

#[gpui::test]
fn successful_add_clears_tag_chips(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(editor_launch(EditorStore::new()), window, cx)
    });
    open_editor(cx, &root);
    cx.update_entity(&root, |root, cx| {
        root.seed_fields("Paris", cx);
        root.add.tag_chips.push(String::from("leak"));
        cx.notify();
    });
    draw_editor(cx, &root);

    cx.dispatch_action(super::SaveNote);
    cx.run_until_parked();

    let (toast, chips) = cx.read_entity(&root, |root, _| {
        (
            root.toast
                .as_ref()
                .map(|toast| (toast.kind, toast.text.clone())),
            root.add.tag_chips.clone(),
        )
    });
    assert_eq!(
        toast,
        Some((ToastKind::Success, String::from("Added 1 card")))
    );
    assert!(chips.is_empty(), "tags do not leak into the next note");
}

#[gpui::test]
fn add_toast_agrees_with_the_card_count(cx: &mut gpui::TestAppContext) {
    let mut store = EditorStore::new();
    store.card_count = 2;
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(editor_launch(store), window, cx)
    });
    open_editor(cx, &root);
    cx.update_entity(&root, |root, cx| {
        root.seed_fields("Paris", cx);
        cx.notify();
    });
    cx.dispatch_action(super::SaveNote);
    cx.run_until_parked();

    let toast = cx.read_entity(&root, |root, _| {
        root.toast.as_ref().map(|toast| toast.text.clone())
    });
    assert_eq!(toast.as_deref(), Some("Added 2 cards"));
}

#[gpui::test]
fn failed_save_shows_inline_error_that_clears_on_edit(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    open_editor(cx, &root);
    cx.update_entity(&root, |root, _| {
        root.add_scroll.set_offset(point(px(0.), px(400.)));
    });
    cx.dispatch_action(super::SaveNote);
    cx.run_until_parked();
    draw_editor(cx, &root);

    assert!(
        cx.read_entity(&root, |root, _| root
            .add
            .errors
            .contains_key("field:0"))
    );
    assert!(
        cx.debug_bounds("add-field-error-0").is_some(),
        "the first empty field explains the error"
    );
    let (offset, focused) = cx.update(|window, cx| {
        let root = root.read(cx);
        (
            root.add_scroll.offset().y,
            root.add.fields[0]
                .read(cx)
                .focus_handle()
                .is_focused(window),
        )
    });
    assert_eq!(offset, px(0.), "the form scrolls back to the invalid field");
    assert!(focused, "the invalid field takes focus");

    let handle = field_focus(cx, &root, 0);
    cx.update(|window, _| window.focus(&handle));
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert!(
        cx.read_entity(&root, |root, _| root.add.errors.is_empty()),
        "editing clears the validation error"
    );
    cx.update_entity(&root, |root, cx| {
        root.add.deck_id = None;
        root.add_scroll.set_offset(point(px(0.), px(400.)));
        cx.notify();
    });
    cx.dispatch_action(super::SaveNote);
    cx.run_until_parked();
    let (picker, offset, filter_focused) = cx.update(|window, cx| {
        let root = root.read(cx);
        (
            root.add.deck_picker,
            root.add_scroll.offset().y,
            root.add
                .deck_filter
                .read(cx)
                .focus_handle()
                .is_focused(window),
        )
    });
    assert!(picker, "the deck picker opens on a deck error");
    assert_eq!(offset, px(0.), "the form scrolls to the deck control");
    assert!(filter_focused, "the deck filter takes focus");
}

#[gpui::test]
fn open_add_focuses_the_first_field(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    draw_editor(cx, &root);
    cx.dispatch_action(super::OpenAdd);
    cx.run_until_parked();
    draw_editor(cx, &root);

    let focused = cx.update(|window, cx| {
        root.read(cx).add.fields[0]
            .read(cx)
            .focus_handle()
            .is_focused(window)
    });
    assert!(focused, "OpenAdd focuses the first editable field");
}

#[gpui::test]
fn clicking_save_keeps_the_editor_focused(cx: &mut gpui::TestAppContext) {
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(editor_launch(EditorStore::new()), window, cx)
    });
    open_editor(cx, &root);
    let handle = field_focus(cx, &root, 0);
    cx.update(|window, _| window.focus(&handle));

    let button = cx
        .debug_bounds("add-save")
        .expect("save button bounds")
        .center();
    cx.simulate_click(button, gpui::Modifiers::none());
    cx.run_until_parked();

    let focused = cx.update(|window, cx| {
        root.read(cx).add.fields[0]
            .read(cx)
            .focus_handle()
            .is_focused(window)
    });
    assert!(focused, "mouse save keeps the previous field focused");
}

#[gpui::test]
fn duplicate_first_field_is_explained_inline_and_focused(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let mut store = EditorStore::new();
    store.duplicate = true;
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(editor_launch(store), window, cx)
    });
    open_editor(cx, &root);
    cx.update_entity(&root, |root, cx| {
        root.seed_fields("Paris", cx);
        cx.notify();
    });
    draw_editor(cx, &root);
    let tags = cx.read_entity(&root, |root, cx| {
        root.add.tags.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&tags));

    cx.dispatch_action(super::SaveNote);
    cx.run_until_parked();
    draw_editor(cx, &root);

    let message = cx
        .read_entity(&root, |root, _| root.add.errors.get("field:0").cloned());
    assert_eq!(
        message.as_deref(),
        Some("A note with this first field already exists")
    );
    assert!(
        cx.debug_bounds("add-field-error-0").is_some(),
        "the duplicate is shown on the field"
    );
    let focused = cx.update(|window, cx| {
        root.read(cx).add.fields[0]
            .read(cx)
            .focus_handle()
            .is_focused(window)
    });
    assert!(focused, "the duplicate field takes focus");
}

#[gpui::test]
fn occlusion_controls_add_and_remove_masks(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            super::Launch {
                store: Box::new(ModalStore),
                options: Options::default(),
                save_settings: Arc::new(|_| Ok(())),
            },
            window,
            cx,
        )
    });
    cx.update_entity(&root, |root, cx| {
        root.view = View::AddCard;
        let occlusion = root
            .models
            .iter()
            .position(ranki_model::NoteModel::is_image_occlusion)
            .expect("stock occlusion");
        root.select_model(occlusion, cx);
        root.add.occlusion.image = Some(String::from("diagram.png"));
        cx.notify();
    });
    draw_editor(cx, &root);

    cx.dispatch_action(super::AddOcclusionMask);
    cx.run_until_parked();
    draw_editor(cx, &root);

    let mask = cx.read_entity(&root, |root, _| {
        root.add.occlusion.masks.first().copied()
    });
    assert!(mask.is_some(), "Add mask adds a default mask");
    let rect = mask.expect("mask").rect;
    assert!(rect.width > 0.0 && rect.height > 0.0);
    assert!(cx.debug_bounds("io-add-mask").is_some());
    assert!(cx.debug_bounds("io-occlusion-hint").is_some());

    let remove = cx
        .debug_bounds("io-remove-mask-1")
        .expect("remove control bounds")
        .center();
    cx.simulate_click(remove, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(
        cx.read_entity(&root, |root, _| root.add.occlusion.masks.is_empty()),
        "the explicit remove control deletes its mask"
    );

    cx.dispatch_action(super::AddOcclusionMask);
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert!(
        cx.read_entity(&root, |root, _| root.add.occlusion.masks.is_empty()),
        "Ctrl+Shift+M removes the last mask"
    );
}

#[gpui::test]
fn enter_applies_the_custom_color(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(editor_launch(EditorStore::new()), window, cx)
    });
    open_editor(cx, &root);
    cx.update_entity(&root, |root, cx| {
        root.seed_fields("Paris", cx);
        cx.notify();
    });
    draw_editor(cx, &root);
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            let field = root.add.fields[0].clone();
            root.toggle_color_picker(
                crate::add_card_view::ColorTarget::Text,
                field,
                point(px(300.), px(360.)),
                window,
                cx,
            );
        });
    });
    draw_editor(cx, &root);

    cx.simulate_keystrokes("f 3 8 b a 8");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let (text, open) = cx.read_entity(&root, |root, cx| {
        (
            root.add.fields[0].read(cx).text(),
            root.add.color_picker.is_some(),
        )
    });
    assert_eq!(
        text,
        format!(
            "Paris<span style=\"color: #{}\"></span>",
            concat!("f3", "8b", "a8")
        )
    );
    assert!(!open, "the picker closes after applying the color");
}

#[gpui::test]
fn escape_cancels_add_after_nested_dismissals(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(editor_launch(EditorStore::new()), window, cx)
    });
    open_editor(cx, &root);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_ne!(
        cx.read_entity(&root, |root, _| root.view),
        View::AddCard,
        "escape cancels the add form"
    );

    open_editor(cx, &root);
    cx.update_entity(&root, |root, cx| {
        root.add.deck_picker = true;
        cx.notify();
    });
    draw_editor(cx, &root);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let (view, picker) =
        cx.read_entity(&root, |root, _| (root.view, root.add.deck_picker));
    assert_eq!(view, View::AddCard);
    assert!(!picker, "the first escape only closes the picker");

    let tags = cx.read_entity(&root, |root, cx| {
        root.add.tags.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&tags));
    cx.simulate_keystrokes("j a");
    cx.run_until_parked();
    draw_editor(cx, &root);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let (view, dismissed) = cx.read_entity(&root, |root, _| {
        (root.view, root.add.tag_suggestions_dismissed)
    });
    assert_eq!(view, View::AddCard);
    assert!(dismissed, "the first escape only closes suggestions");
}

#[gpui::test]
fn enter_in_deck_filter_chooses_the_first_match(cx: &mut gpui::TestAppContext) {
    cx.update(|app| app.bind_keys(super::key_bindings()));
    let (root, cx) = cx.add_window_view(|window, cx| {
        super::Root::new(
            editor_launch(EditorStore::with_decks(&["Alpha", "Beta"])),
            window,
            cx,
        )
    });
    open_editor(cx, &root);
    cx.update_entity(&root, |root, cx| {
        root.add.deck_picker = true;
        root.add.deck_filter.update(cx, |field, cx| {
            field.seed("bet", cx);
        });
        cx.notify();
    });
    draw_editor(cx, &root);
    let filter = cx.read_entity(&root, |root, cx| {
        root.add.deck_filter.read(cx).focus_handle().clone()
    });
    cx.update(|window, _| window.focus(&filter));

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let (selected, picker) = cx
        .read_entity(&root, |root, _| (root.add.deck_id, root.add.deck_picker));
    assert_eq!(selected, Some(2), "the first visible deck is chosen");
    assert!(!picker, "choosing a deck closes the picker");
}
