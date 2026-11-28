//! Root application state, keyboard dispatcher, and store-backed actions.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    env,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions,
    Pixels, Point, Render, ScrollAnchor, ScrollHandle, SharedString, Window,
    WindowAppearance, div, point, prelude::*, px,
};
use ranki_core::{FsrsScheduler, from_settings};
use ranki_model::{
    Card, CardQueue, CardType, Deck, DeckStats, DeckSteps, Id, NewNote, Note,
    NoteModel, Rating, ReviewDecision, StockModelKind, Store, Timing, UiState,
    UiView,
};
use ranki_sync::{MediaProgress, SyncOutcome};

use crate::{
    Options, SettingsSaver, ThemeChoice,
    add_card_view::{ColorPicker, ColorTarget},
    browser_view::{BROWSE_DETAIL_MIN_WIDTH, BrowseState, browse_expand_paths},
    card_view::{self, MediaKind, MediaRef, RenderedCard},
    occlusion::{
        self, MaskShape, NormalizedRect, OcclusionMask, image_occlusion_fields,
    },
    scrollbar::{self, ScrollbarDrag},
    smooth_scroll::SmoothScroll,
    statistics_view::{Statistics, build_statistics_timed},
    sync_view::SyncActivity,
    text_field::{EditHook, TextField},
    theme::{self, Palette, Theme, alpha},
};

/// Anki shows learning cards due within this window when the queue runs dry.
pub const LEARN_AHEAD_SECS: i64 = 20 * 60;
pub const DEBOUNCE: Duration = Duration::from_millis(120);
pub const FORECAST_DAYS: usize = 7;
const TOAST_LIFETIME: Duration = Duration::from_secs(5);
const MAX_REVIEW_TIME: Duration = Duration::from_secs(60);
pub const DECK_SUMMARY_LABEL: &str = "Due today (all decks)";
const TAG_SUGGESTION_LIMIT: usize = 6;

mod actions;
pub use actions::*;

/// Which full-view screen is visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Decks,
    Browse,
    Review,
    AddCard,
    Settings,
    Statistics,
}

impl View {
    const fn key_context(self) -> &'static str {
        match self {
            Self::Review => "ranki ranki_review",
            Self::Decks => "ranki ranki_decks",
            Self::Browse => "ranki ranki_browse",
            Self::AddCard | Self::Settings | Self::Statistics => "ranki",
        }
    }
}

impl From<UiView> for View {
    fn from(saved: UiView) -> Self {
        match saved {
            UiView::Decks => Self::Decks,
            UiView::Browse => Self::Browse,
            UiView::Review => Self::Review,
            UiView::AddCard => Self::AddCard,
            UiView::Settings => Self::Settings,
            UiView::Statistics => Self::Statistics,
        }
    }
}

impl From<View> for UiView {
    fn from(view: View) -> Self {
        match view {
            View::Decks => Self::Decks,
            View::Browse => Self::Browse,
            View::Review => Self::Review,
            View::AddCard => Self::AddCard,
            View::Settings => Self::Settings,
            View::Statistics => Self::Statistics,
        }
    }
}
/// Review content phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Question,
    Answer,
}

/// One explicit deck-table row, including virtual `A::B` parents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeckRow {
    pub id: Option<Id>,
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub stats: DeckStats,
    pub virtual_group: bool,
    pub has_children: bool,
    pub expanded: bool,
    /// Table metadata built once with the tree instead of on every frame.
    pub meta: String,
    pub child_count: usize,
    pub deck_index: Option<usize>,
    /// Element keys built once with the tree instead of on every frame.
    pub key: SharedString,
    pub chevron_key: SharedString,
    pub study_key: SharedString,
    pub menu_key: SharedString,
}

/// Deck rename state.
#[derive(Clone, Debug)]
pub struct DeckEdit {
    pub id: Id,
}

/// Open deck actions menu and the window point that opened it.
#[derive(Clone, Copy, Debug)]
pub struct DeckMenu {
    pub id: Id,
    pub anchor: Point<Pixels>,
}
/// Open card actions menu and the window point that opened it.
#[derive(Clone, Copy, Debug)]
pub struct CardMenu {
    pub id: Id,
    pub anchor: Point<Pixels>,
}

/// Card being edited in the add-card form.
#[derive(Clone, Copy, Debug)]
pub struct CardEdit {
    pub card_id: Id,
    pub note_id: Id,
}

/// Pending destructive action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirmation {
    RemoveDeck { id: Id, name: String },
    RemoveCard { id: Id, front: String },
    Reload,
}

/// Tone of a toast. Errors keep the red role, everything else reports a
/// completed action and must not look like a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub created_at: Instant,
    /// Monotonic id so the toast's fade replays for each new message.
    pub seq: u64,
}

/// `AnkiWeb` sign-in state kept only for the lifetime of the window.
pub struct SyncUiState {
    pub username: Entity<TextField>,
    pub password: Entity<TextField>,
    pub signed_in: bool,
    pub running: Option<SyncActivity>,
    /// File counts from the media refresh that follows a sync.
    pub media: Option<MediaProgress>,
    pub conflict: Option<String>,
    /// Failure from the last attempt, kept on screen after the toast fades.
    pub error: Option<String>,
    /// Cached timestamp from the sync state file, read once at startup.
    pub last_success: Option<i64>,
}
impl SyncUiState {
    pub(crate) const fn busy(&self) -> bool {
        self.running.is_some()
    }
}
impl Toast {
    /// A toast outlives its text only by the timer that clears it.
    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created_at) >= TOAST_LIFETIME
    }
}

/// Image-occlusion editor state.
#[derive(Default)]
pub struct OcclusionState {
    /// Media file name chosen for the `Image` field.
    pub image: Option<String>,
    /// Natural image size, used to align masks inside a letterboxed canvas.
    pub image_size: Option<(f32, f32)>,
    pub masks: Vec<OcclusionMask>,
    /// Drag in progress, in image-relative coordinates.
    pub drag: Option<OcclusionDrag>,
    /// Screen rectangle of the drawing area, from the last prepaint.
    pub bounds: Option<Bounds<Pixels>>,
}

/// What a held pointer is doing on the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OcclusionDrag {
    Draw {
        start: (f32, f32),
        end: (f32, f32),
    },
    Move {
        index: usize,
        start: (f32, f32),
        rect: NormalizedRect,
    },
    Resize {
        index: usize,
        press: (f32, f32),
        anchor: (f32, f32),
        moves_right: bool,
        moves_down: bool,
        moved: bool,
    },
}

/// Sidebar drag in progress: where the pointer pressed and the width then.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavDrag {
    pub start_x: f32,
    pub start_width: f32,
}

/// Screen distance before a right press counts as a resize instead of a click.
const RESIZE_SLOP: f32 = 3.0;

impl OcclusionState {
    /// Rectangle of the fitted image inside the drawing area.
    pub(crate) fn image_rect(&self) -> Option<NormalizedRect> {
        let bounds = self.bounds?;
        let Some(size) = self.image_size else {
            return Some(NormalizedRect {
                left: 0.0,
                top: 0.0,
                width: 1.0,
                height: 1.0,
            });
        };
        Some(occlusion::contain_rect(
            size,
            (f32::from(bounds.size.width), f32::from(bounds.size.height)),
        ))
    }

    fn raw_pointer(&self, position: (f32, f32)) -> Option<(f32, f32)> {
        let bounds = self.bounds?;
        let rect = self.image_rect()?;
        let width = rect.width * f32::from(bounds.size.width);
        let height = rect.height * f32::from(bounds.size.height);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let origin_x = f32::from(bounds.origin.x)
            + rect.left * f32::from(bounds.size.width);
        let origin_y = f32::from(bounds.origin.y)
            + rect.top * f32::from(bounds.size.height);
        Some((
            (position.0 - origin_x) / width,
            (position.1 - origin_y) / height,
        ))
    }

    fn pointer(&self, position: (f32, f32)) -> Option<(f32, f32)> {
        self.raw_pointer(position)
            .map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
    }

    /// Topmost mask under an image-relative point, if any.
    #[must_use]
    pub fn mask_at(&self, point: (f32, f32)) -> Option<usize> {
        self.masks
            .iter()
            .enumerate()
            .rev()
            .find(|(_, mask)| mask.rect.contains(point))
            .map(|(index, _)| index)
    }

    /// Removes one mask.
    fn remove_mask(&mut self, index: usize) -> bool {
        if index >= self.masks.len() {
            return false;
        }
        let _ = self.masks.remove(index);
        true
    }

    /// Drops the image and the masks that belonged to it.
    fn clear_image(&mut self) {
        self.image = None;
        self.image_size = None;
        self.masks.clear();
        self.drag = None;
    }

    /// Current draw preview as a mask, for the dashed outline.
    #[must_use]
    pub fn drag_rect(&self) -> Option<NormalizedRect> {
        match self.drag? {
            OcclusionDrag::Draw { start, end } => {
                NormalizedRect::from_points(start, end)
            }
            OcclusionDrag::Move { .. } | OcclusionDrag::Resize { .. } => None,
        }
    }

    /// Starts a draw, move, or resize from a pointer press.
    fn press(&mut self, position: (f32, f32), button: MouseButton) -> bool {
        if self.image.is_none() {
            return false;
        }
        let (Some(raw), Some(point)) =
            (self.raw_pointer(position), self.pointer(position))
        else {
            return false;
        };
        let inside =
            (0.0..=1.0).contains(&raw.0) && (0.0..=1.0).contains(&raw.1);
        let hit = inside.then(|| self.mask_at(raw)).flatten();
        self.drag = match button {
            MouseButton::Left => Some(if let Some(index) = hit {
                OcclusionDrag::Move {
                    index,
                    start: point,
                    rect: self.masks[index].rect,
                }
            } else {
                OcclusionDrag::Draw {
                    start: point,
                    end: point,
                }
            }),
            MouseButton::Right => hit.map(|index| {
                let rect = self.masks[index].rect;
                let right_side = point.0 >= rect.left + rect.width / 2.0;
                let bottom_side = point.1 >= rect.top + rect.height / 2.0;
                OcclusionDrag::Resize {
                    index,
                    press: position,
                    anchor: (
                        if right_side {
                            rect.left
                        } else {
                            rect.left + rect.width
                        },
                        if bottom_side {
                            rect.top
                        } else {
                            rect.top + rect.height
                        },
                    ),
                    moves_right: right_side,
                    moves_down: bottom_side,
                    moved: false,
                }
            }),
            MouseButton::Middle | MouseButton::Navigate(_) => None,
        };
        self.drag.is_some()
    }

    /// Applies a held pointer's movement to the active drag.
    fn drag_to(&mut self, position: (f32, f32)) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        let Some(pointer) = self.pointer(position) else {
            return false;
        };
        match drag {
            OcclusionDrag::Draw { start, .. } => {
                self.drag = Some(OcclusionDrag::Draw {
                    start,
                    end: pointer,
                });
                true
            }
            OcclusionDrag::Move { index, start, rect } => {
                if let Some(mask) = self.masks.get_mut(index) {
                    mask.rect.left = (rect.left + pointer.0 - start.0)
                        .clamp(0.0, 1.0 - rect.width);
                    mask.rect.top = (rect.top + pointer.1 - start.1)
                        .clamp(0.0, 1.0 - rect.height);
                }
                true
            }
            OcclusionDrag::Resize {
                index,
                press,
                anchor,
                moves_right,
                moves_down,
                moved,
            } => {
                let moved = moved
                    || f32::hypot(position.0 - press.0, position.1 - press.1)
                        >= RESIZE_SLOP;
                if moved {
                    let width = (if moves_right {
                        pointer.0 - anchor.0
                    } else {
                        anchor.0 - pointer.0
                    })
                    .max(occlusion::MIN_MASK_SIZE);
                    let height = (if moves_down {
                        pointer.1 - anchor.1
                    } else {
                        anchor.1 - pointer.1
                    })
                    .max(occlusion::MIN_MASK_SIZE);
                    let rect = NormalizedRect {
                        left: (if moves_right {
                            anchor.0
                        } else {
                            anchor.0 - width
                        })
                        .clamp(0.0, 1.0 - width),
                        top: (if moves_down {
                            anchor.1
                        } else {
                            anchor.1 - height
                        })
                        .clamp(0.0, 1.0 - height),
                        width,
                        height,
                    };
                    if let Some(mask) = self.masks.get_mut(index) {
                        mask.rect = rect;
                    }
                }
                self.drag = Some(OcclusionDrag::Resize {
                    index,
                    press,
                    anchor,
                    moves_right,
                    moves_down,
                    moved,
                });
                moved
            }
        }
    }

    /// Commits the drag. A right press that never moved removes its mask.
    fn finish_drag(&mut self) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        let changed = match drag {
            OcclusionDrag::Draw { start, end } => {
                if let Some(rect) = NormalizedRect::from_points(start, end) {
                    self.masks.push(OcclusionMask {
                        shape: MaskShape::Rect,
                        rect,
                    });
                    true
                } else {
                    false
                }
            }
            OcclusionDrag::Resize {
                index,
                moved: false,
                ..
            } => self.remove_mask(index),
            OcclusionDrag::Move { .. }
            | OcclusionDrag::Resize { moved: true, .. } => false,
        };
        self.drag = None;
        changed
    }

    fn cancel_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }
}

/// Add-card form state.
pub struct AddState {
    pub deck_id: Option<Id>,
    pub model_index: usize,
    pub fields: Vec<Entity<TextField>>,
    pub tags: Entity<TextField>,
    pub tag_chips: Vec<String>,

    /// Tags already used in the collection, offered while typing.
    pub known_tags: Vec<String>,
    pub tag_suggestion: usize,
    pub tag_suggestions_dismissed: bool,
    pub errors: BTreeMap<String, String>,
    pub media_filter: Entity<TextField>,
    pub deck_filter: Entity<TextField>,
    pub deck_picker: bool,
    pub deck_picker_anchor: Point<Pixels>,
    pub deck_picker_dismissed_press: Option<MouseDownEvent>,
    pub color_hex: Entity<TextField>,
    pub color_picker: Option<ColorPicker>,
    pub color_picker_dismissed_press: Option<MouseDownEvent>,
    pub color_picker_dismissed_target: Option<ColorTarget>,
    pub occlusion: OcclusionState,
}

/// What one grade can be reversed into: the card before the write, the review
/// log that recorded it, and the rating the user picked.
#[derive(Clone, Debug)]
pub struct UndoEntry {
    pub card: Card,
    pub revlog_id: Id,
    pub rating: Rating,
    /// Time the undone card contributed to the session total.
    pub spent: Duration,
    /// Whether this grade left the card in the session for another step.
    pub requeued: bool,
}

/// Rendered halves of one card, prepared before a session transition.
struct LoadedCard {
    note: Option<Note>,
    question: Option<RenderedCard>,
    answer: Option<RenderedCard>,
    answer_media: Vec<MediaRef>,
    occlusion_image: Option<MediaRef>,
    occlusion_image_size: Option<(f32, f32)>,
}

/// Review-session state owned by the root entity.
#[derive(Default)]
pub struct ReviewState {
    pub queue: VecDeque<Card>,
    /// Learning cards waiting for their next step inside this session.
    pub pending: VecDeque<Card>,
    pub seen: HashSet<Id>,
    pub current: Option<Card>,
    pub note: Option<Note>,
    pub question: Option<RenderedCard>,
    pub answer: Option<RenderedCard>,
    pub answer_media: Vec<MediaRef>,
    pub occlusion_image: Option<MediaRef>,
    pub occlusion_image_size: Option<(f32, f32)>,
    pub phase: Phase,
    pub focused_elapsed: Duration,
    pub last_tick: Option<Instant>,
    pub last_grade_at: Option<Instant>,
    pub answered: usize,
    /// Cards finished for session progress. Learning repeats stay pending.
    pub completed: usize,
    pub session_total: usize,
    pub again_count: usize,
    pub scope_all: bool,
    /// Visible time spent on graded cards, accumulated per answer.
    pub studied: Duration,
    /// Fraction the progress bar had before the last grade, for its transition.
    pub progress_from: f32,
    /// Last grade this session, cleared once it is undone or replaced.
    pub last_grade: Option<UndoEntry>,
    pub previews: [i32; 4],
    /// Monotonic key for stage and progress animations, including undo.
    pub animation_seq: u64,
    /// Session-end sync must be repeated after an undo rewrites the log.
    pub session_dirty: bool,
    /// Step delays from the current card's deck preset.
    pub steps: DeckSteps,
    pub more_due: bool,
    pub load_errors: usize,
    pub load_error: Option<String>,
    /// `collapseTime` resolved once when the session starts.
    pub learn_ahead_secs: Option<i64>,
}

/// Root entity. The store boundary stays identical to the previous UI.
pub struct Root {
    pub(crate) store: Box<dyn Store>,
    pub(crate) options: Options,
    pub(crate) focus: FocusHandle,
    pub(crate) dialog_focus: FocusHandle,
    pub(crate) settings_focus: FocusHandle,
    pub(crate) system_dark: bool,
    pub(crate) view: View,
    pub(crate) previous_view: View,
    pub(crate) decks: Vec<Deck>,
    pub(crate) models: Vec<NoteModel>,
    pub(crate) stats: Vec<DeckStats>,
    /// Cards scheduled in each of the next seven Anki days, today first.
    pub(crate) forecast: Vec<usize>,
    pub(crate) statistics: Statistics,
    /// Collection stamp the cached statistics were built from.
    pub(crate) statistics_modified: i64,
    /// Day boundary the cached statistics were built for.
    pub(crate) statistics_next_day_at: i64,
    /// Ignores background refreshes started before the latest click.
    pub(crate) statistics_generation: u64,
    /// Failure from the last statistics build, kept on screen until a retry.
    pub(crate) statistics_error: Option<String>,
    /// Anki day shown in the heatmap's hover details.
    pub(crate) heatmap_hover: Option<i64>,
    pub(crate) deck_rows: Vec<DeckRow>,
    pub(crate) deck_rows_rev: u64,
    pub(crate) visible_rows: Vec<DeckRow>,
    pub(crate) visible_rows_key: (String, u64),
    pub(crate) leaf_totals: DeckStats,
    pub(crate) selected: Option<usize>,
    pub(crate) expanded: HashSet<String>,
    pub(crate) deck_search: Entity<TextField>,
    pub(crate) browse: BrowseState,
    pub(crate) create_field: Entity<TextField>,
    pub(crate) create_modal: bool,
    pub(crate) create_error: Option<String>,
    pub(crate) rename_field: Entity<TextField>,
    pub(crate) deck_edit: Option<DeckEdit>,
    pub(crate) deck_menu: Option<DeckMenu>,
    pub(crate) deck_menu_dismissed_press: Option<MouseDownEvent>,
    pub(crate) card_menu: Option<CardMenu>,
    pub(crate) card_menu_dismissed_press: Option<MouseDownEvent>,
    pub(crate) card_edit: Option<CardEdit>,
    pub(crate) confirmation: Option<Confirmation>,
    pub(crate) review: ReviewState,
    pub(crate) add: AddState,
    pub(crate) scheduler: FsrsScheduler,
    pub(crate) toast: Option<Toast>,
    pub(crate) help_open: bool,
    pub(crate) dev_reveal: bool,
    pub(crate) external_change: bool,
    pub(crate) import_running: bool,
    pub(crate) browse_ready: bool,
    pub(crate) statistics_ready: bool,
    pub(crate) collection_modified: i64,
    pub(crate) collection_start: i64,
    pub(crate) decks_scroll: SmoothScroll,
    pub(crate) statistics_scroll: ScrollHandle,
    pub(crate) settings_scroll: ScrollHandle,
    pub(crate) add_scroll: ScrollHandle,
    pub(crate) deck_picker_scroll: ScrollHandle,
    pub(crate) browse_scroll: ScrollHandle,
    pub(crate) browse_detail_scroll: ScrollHandle,
    pub(crate) review_stage_scroll: ScrollHandle,
    /// `ScrollAnchor` for the answer group inside the review stage.
    pub(crate) review_answer_anchor: Option<ScrollAnchor>,
    /// Unscrolled origin of the answer group, mirrored from its prepaint.
    pub(crate) review_answer_origin: Rc<Cell<Option<Point<Pixels>>>>,
    pub(crate) review_answer_scroll_pending: bool,
    pub(crate) scrollbar_drag: Option<ScrollbarDrag>,
    pub(crate) scrollbar_layouts: BTreeMap<&'static str, (Bounds<Pixels>, f32)>,
    pub(crate) browse_smooth: SmoothScroll,
    pub(crate) sync: SyncUiState,
    pub(crate) startup_sync_result: Option<Arc<Mutex<Option<SyncOutcome>>>>,
    pub(crate) sync_run: Arc<crate::sync_view::SyncRun>,
    pub(crate) media_run: Arc<crate::sync_view::MediaRun>,
    pub(crate) save_settings: SettingsSaver,
    pub(crate) images: Entity<card_view::LruImageCache>,
    pub(crate) font_family: &'static str,
    pub(crate) review_button_focus: RefCell<Vec<FocusHandle>>,
    pub(crate) last_change_check: Instant,
    pub(crate) nav_width: f32,
    pub(crate) nav_drag: Option<NavDrag>,
}

/// Glob patterns shown by the Anki package picker and checked after selection.
const ANKI_PACKAGE_GLOBS: [&str; 2] = ["*.apkg", "*.colpkg"];

fn is_anki_package(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("apkg")
                || extension.eq_ignore_ascii_case("colpkg")
        })
}

#[cfg(target_os = "linux")]
fn anki_package_filter() -> ashpd::desktop::file_chooser::FileFilter {
    use ashpd::desktop::file_chooser::FileFilter;

    let mut filter = FileFilter::new("Anki packages");
    for glob in ANKI_PACKAGE_GLOBS {
        filter = filter.glob(glob);
    }
    filter
}

#[cfg(target_os = "linux")]
async fn choose_import_path(
    _cx: &mut gpui::AsyncApp,
) -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::file_chooser::OpenFileRequest;

    let request = match OpenFileRequest::default()
        .title("Import Anki package")
        .accept_label("Import")
        .modal(true)
        .filter(anki_package_filter())
        .send()
        .await
    {
        Ok(request) => request,
        Err(ashpd::Error::Response(_)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let selected = match request.response() {
        Ok(selected) => selected,
        Err(ashpd::Error::Response(_)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    Ok(selected
        .uris()
        .first()
        .and_then(|uri| uri.to_file_path().ok()))
}

#[cfg(not(target_os = "linux"))]
async fn choose_import_path(
    cx: &mut gpui::AsyncApp,
) -> Result<Option<PathBuf>, String> {
    let receiver = cx
        .update(|app| {
            app.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Import Anki package".into()),
            })
        })
        .map_err(|error| error.to_string())?;
    match receiver.await {
        Ok(Ok(Some(paths))) => Ok(paths.into_iter().next()),
        Ok(Ok(None)) => Ok(None),
        Ok(Err(error)) | Err(error) => Err(error.to_string()),
    }
}

/// Values the window needs before the root entity exists.
pub struct Launch {
    pub store: Box<dyn Store>,
    pub options: Options,
    pub save_settings: SettingsSaver,
}

impl Root {
    pub(crate) const fn style(&self) -> crate::theme::Style {
        crate::theme::Style::new(self.colors(), self.theme())
    }
}

impl Root {
    /// True when the rail is too narrow for labels, keycaps, and the wordmark.
    pub(crate) fn nav_compact(&self) -> bool {
        self.nav_width < theme::NAV_COMPACT_WIDTH
    }

    /// Grabs the rail divider at the pointer.
    pub(crate) fn start_nav_drag(
        &mut self,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.nav_drag = Some(NavDrag {
            start_x: f32::from(event.position.x),
            start_width: self.nav_width,
        });
        cx.notify();
    }

    /// Follows the pointer while the divider is held.
    pub(crate) fn drag_nav(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.nav_drag else {
            return;
        };
        self.nav_width =
            theme::nav_width(drag.start_width + (x - drag.start_x));
        cx.notify();
    }

    /// Releases the divider.
    pub(crate) fn end_nav_drag(&mut self, cx: &mut Context<Self>) {
        if self.nav_drag.take().is_some() {
            cx.notify();
            self.persist_state(cx);
        }
    }
}

impl Root {
    fn state_snapshot(&self, cx: &App) -> UiState {
        let mut expanded = self.expanded.iter().cloned().collect::<Vec<_>>();
        expanded.sort();
        UiState {
            view: self.view.into(),
            previous_view: self.previous_view.into(),
            theme: Some(self.options.theme),
            nav_width: Some(self.nav_width),
            selected_deck: self
                .selected
                .and_then(|index| self.decks.get(index))
                .map(|deck| deck.id),
            expanded_decks: expanded,
            deck_query: self.deck_search.read(cx).text(),
            browse_query: self.browse.search.read(cx).text(),
            review_all_decks: self.review.scope_all,
        }
    }

    pub(crate) fn persist_state(&mut self, cx: &mut Context<Self>) {
        self.options.ui_state = self.state_snapshot(cx);
        let saver = Arc::clone(&self.save_settings);
        if let Err(error) = saver(&self.options) {
            self.show_toast(
                ToastKind::Error,
                format!("Could not save window state: {error}"),
                cx,
            );
        }
    }

    fn begin_scrollbar_drag(
        &mut self,
        id: &'static str,
        handle: &ScrollHandle,
        y: f32,
    ) {
        let Some(geometry) = scrollbar::geometry(handle) else {
            return;
        };
        let offset = -f32::from(handle.offset().y);
        self.scrollbar_drag =
            Some(ScrollbarDrag::new(id, handle, y, offset, &geometry));
    }

    fn press_scrollbar_track(
        &mut self,
        id: &'static str,
        handle: &ScrollHandle,
        y: f32,
    ) {
        let Some(geometry) = scrollbar::geometry(handle) else {
            return;
        };
        let offset = scrollbar::offset_for_click(&geometry, y);
        handle.set_offset(point(Pixels::ZERO, px(-offset)));
        self.scrollbar_drag =
            Some(ScrollbarDrag::new(id, handle, y, offset, &geometry));
    }

    pub(crate) fn drag_scrollbar(&mut self, y: f32, window: &mut Window) {
        let Some(drag) = self.scrollbar_drag.as_mut() else {
            return;
        };
        let offset = drag.offset_at(y);
        drag.handle.set_offset(point(Pixels::ZERO, px(-offset)));
        window.refresh();
    }

    pub(crate) fn end_scrollbar_drag(
        &mut self,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.scrollbar_drag.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    fn pointer_on_scrollbar(
        &self,
        event: &MouseDownEvent,
        window: &Window,
    ) -> bool {
        let x = f32::from(event.position.x);
        let y = f32::from(event.position.y);
        self.active_scrollbars(window).iter().any(|(_, handle)| {
            scrollbar::geometry(handle).is_some_and(|geometry| {
                x >= geometry.left
                    && x <= geometry.left + scrollbar::WIDTH
                    && y >= geometry.track_top
                    && y <= geometry.track_top + geometry.track_height
            })
        })
    }
    pub(crate) fn active_scrollbars(
        &self,
        window: &Window,
    ) -> Vec<(&'static str, &ScrollHandle)> {
        let mut bars: Vec<(&'static str, &ScrollHandle)> = Vec::new();
        match self.view {
            View::Decks => bars.push(("decks", self.decks_scroll.handle())),
            View::Browse => {
                bars.push(("browse", &self.browse_scroll));
                let width = crate::theme::page_width(
                    f32::from(window.viewport_size().width),
                    self.nav_width,
                );
                if self.browse.selected.is_some()
                    && width >= BROWSE_DETAIL_MIN_WIDTH
                {
                    bars.push(("browse-detail", &self.browse_detail_scroll));
                }
            }
            View::Review => {
                if self.review.current.is_some() {
                    bars.push(("review-stage", &self.review_stage_scroll));
                }
            }
            View::AddCard => {
                bars.push(("add", &self.add_scroll));
                if self.add.deck_picker {
                    bars.push(("deck-picker", &self.deck_picker_scroll));
                }
            }
            View::Settings => bars.push(("settings", &self.settings_scroll)),
            View::Statistics => {
                bars.push(("statistics", &self.statistics_scroll));
            }
        }
        bars
    }

    pub(crate) fn render_scrollbar(
        &self,
        id: &'static str,
        handle: &ScrollHandle,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let geometry = scrollbar::geometry(handle)?;

        let colors = self.colors();
        let dragging = self
            .scrollbar_drag
            .as_ref()
            .is_some_and(|drag| drag.id == id);
        let thumb_fill = if dragging {
            colors.mauve
        } else {
            alpha(colors.mauve, 0.55)
        };

        let track_handle = handle.clone();
        let track = cx.listener(
            move |this: &mut Self, event: &MouseDownEvent, _, cx| {
                if this
                    .scrollbar_drag
                    .as_ref()
                    .is_some_and(|drag| drag.id == id)
                {
                    return;
                }
                this.press_scrollbar_track(
                    id,
                    &track_handle,
                    f32::from(event.position.y),
                );
                cx.notify();
            },
        );
        let thumb_handle = handle.clone();
        let thumb = cx.listener(
            move |this: &mut Self, event: &MouseDownEvent, _, cx| {
                this.begin_scrollbar_drag(
                    id,
                    &thumb_handle,
                    f32::from(event.position.y),
                );
                cx.notify();
            },
        );

        Some(
            div()
                .id(SharedString::from(format!("scrollbar-{id}-track")))
                .debug_selector(move || format!("scrollbar-{id}-track"))
                .absolute()
                .left(px(geometry.left))
                .top(px(geometry.track_top))
                .w(px(scrollbar::WIDTH))
                .h(px(geometry.track_height))
                .rounded(px(5.))
                .bg(alpha(colors.surface1, 0.35))
                .cursor_pointer()
                .block_mouse_except_scroll()
                .on_mouse_down(MouseButton::Left, track)
                .on_mouse_move(cx.listener(
                    |this: &mut Self, event: &MouseMoveEvent, window, _| {
                        this.drag_scrollbar(
                            f32::from(event.position.y),
                            window,
                        );
                    },
                ))
                .child(
                    div()
                        .id(SharedString::from(format!("scrollbar-{id}-thumb")))
                        .debug_selector(move || format!("scrollbar-{id}-thumb"))
                        .absolute()
                        .left(px(
                            (scrollbar::WIDTH - scrollbar::THUMB_WIDTH) / 2.0
                        ))
                        .top(px(geometry.thumb_top - geometry.track_top))
                        .w(px(scrollbar::THUMB_WIDTH))
                        .h(px(geometry.thumb_height))
                        .rounded_full()
                        .bg(thumb_fill)
                        .hover(move |style| style.bg(alpha(colors.mauve, 0.85)))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, thumb)
                        .on_mouse_move(cx.listener(
                            |this: &mut Self,
                             event: &MouseMoveEvent,
                             window,
                             _| {
                                this.drag_scrollbar(
                                    f32::from(event.position.y),
                                    window,
                                );
                            },
                        )),
                )
                .into_any_element(),
        )
    }
}

impl Focusable for Root {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Uses the stored card when available so a requeued card carries its saved
/// revision into the next grade.
fn refreshed_card(
    current: &Card,
    load: impl FnOnce(Id) -> Option<Card>,
) -> Card {
    load(current.id).unwrap_or_else(|| current.clone())
}
impl Root {
    #[must_use]
    pub fn new(
        launch: Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Launch {
            store,
            mut options,
            save_settings,
        } = launch;
        let saved = options.ui_state.clone();
        if let Some(theme) = saved.theme {
            options.theme = theme;
        }
        let focus = cx.focus_handle();
        let dialog_focus = cx.focus_handle();
        let settings_focus = cx.focus_handle();
        window.focus(&focus);
        let system_dark = !matches!(
            window.appearance(),
            WindowAppearance::Light | WindowAppearance::VibrantLight
        );
        let hook = Self::edit_hook(cx);
        let make_field = |cx: &mut Context<TextField>,
                          placeholder: &'static str| {
            let mut field = TextField::new(cx, placeholder);
            field.set_edit_hook(hook.clone());
            field
        };
        let deck_hook = Self::deck_search_edit_hook(cx);
        let deck_search = cx.new(|cx| {
            let mut field = make_field(cx, "Search decks");
            field.set_edit_hook(deck_hook);
            field
        });
        let browse_search = cx.new(|cx| {
            TextField::new(cx, "Search name, tag:any_tag or deck:any_deck")
        });
        let browse_hook = Self::browse_edit_hook(cx);
        browse_search.update(cx, |field, _| field.set_edit_hook(browse_hook));
        let create_hook = Self::create_field_hook(cx);
        let create_field = cx.new(|cx| {
            let mut field = make_field(cx, "Deck name");
            field.set_edit_hook(create_hook);
            field
        });
        let color_hex = cx.new(|cx| make_field(cx, "rrggbb"));
        let deck_filter = cx.new(|cx| make_field(cx, "Search decks"));
        let media_filter = cx.new(|cx| make_field(cx, "Filter images"));
        let rename_field = cx.new(|cx| make_field(cx, "New name"));
        let tags = cx.new(|cx| make_field(cx, "Add a tag"));
        let tag_hook = Self::tag_hook(cx);
        tags.update(cx, |field, _| field.set_edit_hook(tag_hook));
        let sync_username = cx.new(|cx| make_field(cx, "AnkiWeb username"));
        let sync_password = cx.new(|cx| {
            let mut field = TextField::new(cx, "AnkiWeb password");
            field.set_secure(true, cx);
            field
        });
        let sync_state =
            crate::sync_view::read_state(&options.paths.sync_state);
        let signed_in = sync_state
            .as_ref()
            .is_some_and(|state| !state.host_key.is_empty());
        let browse_scroll = ScrollHandle::new();
        let mut root = Self {
            scheduler: from_settings(&options),
            store,
            options,
            focus,
            dialog_focus,
            settings_focus,
            system_dark,
            view: View::Decks,
            previous_view: View::Decks,
            decks: Vec::new(),
            models: Vec::new(),
            stats: Vec::new(),
            forecast: Vec::new(),
            statistics: Statistics::default(),
            statistics_modified: 0,
            statistics_next_day_at: 0,
            statistics_generation: 0,
            statistics_error: None,
            heatmap_hover: None,
            deck_rows: Vec::new(),
            deck_rows_rev: 0,
            visible_rows: Vec::new(),
            visible_rows_key: (String::new(), 0),
            leaf_totals: DeckStats::default(),
            selected: None,
            expanded: HashSet::new(),
            deck_search,
            browse: BrowseState {
                search: browse_search,
                hits: Vec::new(),
                entries: Vec::new(),
                rows: HashMap::new(),
                expanded: HashSet::new(),
                last_query: None,
                selected: None,
                error: None,
                generation: 0,
                loading: false,
                query: None,
                modified: None,
                viewport: None,
            },
            create_field,
            create_modal: false,
            create_error: None,
            rename_field,
            deck_edit: None,
            deck_menu: None,
            deck_menu_dismissed_press: None,
            card_menu: None,
            card_menu_dismissed_press: None,
            card_edit: None,
            confirmation: None,
            review: ReviewState::default(),
            add: AddState {
                deck_id: None,
                model_index: 0,
                fields: Vec::new(),
                tags,
                tag_chips: Vec::new(),

                known_tags: Vec::new(),
                tag_suggestion: 0,
                tag_suggestions_dismissed: false,
                errors: BTreeMap::new(),
                media_filter,
                deck_filter,
                deck_picker: false,
                deck_picker_anchor: point(px(0.), px(0.)),
                deck_picker_dismissed_press: None,
                color_hex,
                color_picker: None,
                color_picker_dismissed_press: None,
                color_picker_dismissed_target: None,
                occlusion: OcclusionState::default(),
            },
            toast: None,
            help_open: false,
            dev_reveal: env::var("RANKI_UI_DEV_REVEAL")
                .is_ok_and(|value| value == "1"),
            external_change: false,
            import_running: false,
            browse_ready: false,
            statistics_ready: false,
            collection_modified: 0,
            collection_start: 0,
            decks_scroll: SmoothScroll::new(),
            statistics_scroll: ScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            add_scroll: ScrollHandle::new(),
            deck_picker_scroll: ScrollHandle::new(),
            browse_scroll: browse_scroll.clone(),
            browse_detail_scroll: ScrollHandle::new(),
            review_stage_scroll: ScrollHandle::new(),
            review_answer_anchor: None,
            review_answer_origin: Rc::new(Cell::new(None)),
            review_answer_scroll_pending: false,
            scrollbar_drag: None,
            scrollbar_layouts: BTreeMap::new(),
            browse_smooth: SmoothScroll::with_handle(browse_scroll),
            startup_sync_result: None,
            sync: SyncUiState {
                username: sync_username,
                password: sync_password,
                signed_in,
                running: None,
                media: None,
                conflict: None,
                error: None,
                last_success: sync_state
                    .as_ref()
                    .and_then(|state| state.last_success),
            },
            sync_run: Arc::new(crate::sync_view::run_sync),
            media_run: Arc::new(crate::sync_view::run_media),
            save_settings,
            images: card_view::LruImageCache::new(cx),
            review_button_focus: RefCell::new(Vec::new()),
            last_change_check: Instant::now(),
            nav_width: theme::NAV_WIDTH,
            nav_drag: None,
            font_family: theme::ui_font_family(),
        };
        root.review_answer_anchor =
            Some(ScrollAnchor::for_handle(root.review_stage_scroll.clone()));
        root.view = saved.view.into();
        root.previous_view = saved.previous_view.into();
        root.reload(cx, true);
        root.nav_width = theme::nav_width(
            saved
                .nav_width
                .filter(|width| width.is_finite())
                .unwrap_or(theme::NAV_WIDTH),
        );
        root.expanded = saved
            .expanded_decks
            .into_iter()
            .filter(|path| !path.is_empty())
            .collect();
        root.rebuild_deck_rows();
        root.review.scope_all = saved.review_all_decks;
        if let Some(id) = saved.selected_deck {
            root.selected = root
                .decks
                .iter()
                .position(|deck| deck.id == id)
                .or(root.selected);
        }
        root.deck_search
            .update(cx, |field, cx| field.seed(saved.deck_query, cx));
        root.refresh_visible_deck_rows(cx);
        root.browse
            .search
            .update(cx, |field, cx| field.seed(saved.browse_query, cx));
        if root.view == View::Browse {
            root.refresh_browse(cx);
            root.browse_ready = true;
        }
        if let (Ok(deck_name), Ok(path)) = (
            env::var("RANKI_UI_DEV_EXPORT_DECK"),
            env::var("RANKI_UI_DEV_EXPORT_PATH"),
        ) && let Some(deck) = root
            .decks
            .iter()
            .find(|deck| deck.name == deck_name)
            .cloned()
        {
            let collection = root.options.paths.collection.clone();
            let media_dir = root.options.paths.media_dir.clone();
            let weak = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |root, cx| {
                    let result = ranki_format::export_deck_apkg(
                        &collection,
                        &media_dir,
                        Path::new(&path),
                        deck.id,
                    );
                    match result {
                        Ok(()) => root.show_toast(
                            ToastKind::Success,
                            format!("Exported {} to {path}", deck.name),
                            cx,
                        ),
                        Err(error) => root.show_toast(
                            ToastKind::Error,
                            format!("Could not export {}: {error}", deck.name),
                            cx,
                        ),
                    }
                });
            });
        }
        if root.view == View::Review {
            match root.selected {
                Some(index) if !saved.review_all_decks => {
                    root.study_deck(index, window, cx);
                }
                _ => root.study_all(window, cx),
            }
        }
        root.reset_fields(cx);
        root.refresh_field_colors(cx);
        if let Ok(prefill) = env::var("RANKI_UI_DEV_FIELDS") {
            root.seed_fields(&prefill, cx);
        }
        if let Ok(spec) = env::var("RANKI_UI_DEV_SELECT_WORD")
            && let Some((index, offset)) = spec.split_once(':')
            && let Ok(index) = index.parse::<usize>()
            && let Ok(offset) = offset.parse::<usize>()
            && let Some(field) = root.add.fields.get(index).cloned()
        {
            window.focus(field.read(cx).focus_handle());
            field.update(cx, |field, cx| field.select_word_at(offset, cx));
        }
        if let Ok(tags) = env::var("RANKI_UI_DEV_TAGS") {
            root.add.tag_chips =
                tags.split_whitespace().map(str::to_owned).collect();
        }
        if let Ok(query) = env::var("RANKI_UI_DEV_TAG_QUERY") {
            root.view = View::AddCard;
            root.add.tags.update(cx, |field, cx| field.seed(query, cx));
            let focus = root.add.tags.read(cx).focus_handle().clone();
            window.focus(&focus);
        }
        if let Ok(spec) = env::var("RANKI_UI_DEV_SCROLL")
            && let Some((delay, distance)) = spec.split_once(':')
            && let Ok(delay) = delay.parse::<u64>()
            && let Ok(distance) = distance.parse::<f32>()
        {
            let timer =
                cx.background_executor().timer(Duration::from_millis(delay));
            cx.spawn(async move |this, cx| {
                timer.await;
                let _ = this.update(cx, |root, cx| {
                    root.decks_scroll.wheel(distance);
                    cx.notify();
                });
            })
            .detach();
        }
        if let Ok(distance) = env::var("RANKI_UI_DEV_BROWSE_SCROLL")
            && let Ok(distance) = distance.parse::<f32>()
        {
            let timer =
                cx.background_executor().timer(Duration::from_millis(300));
            cx.spawn(async move |this, cx| {
                timer.await;
                let _ = this.update(cx, |root, cx| {
                    root.browse_smooth.wheel(distance);
                    cx.notify();
                });
            })
            .detach();
        }
        match env::var("RANKI_UI_DEV_VIEW").as_deref() {
            Ok("decks") => root.view = View::Decks,
            Ok("statistics") => {
                root.view = View::Statistics;
                root.schedule_statistics_refresh(cx);
                root.statistics_ready = true;
            }
            Ok("add") => root.view = View::AddCard,
            Ok("settings") => root.view = View::Settings,
            Ok("browse") => {
                root.view = View::Browse;
                root.refresh_browse(cx);
                root.browse_ready = true;
            }
            _ => {}
        }
        if root.dev_reveal {
            if let Ok(spec) = env::var("RANKI_UI_DEV_BROWSE_QUERY") {
                let (query, expand) =
                    spec.split_once(':').unwrap_or((&spec, "1"));
                root.view = View::Browse;
                root.browse.search.update(cx, |field, cx| {
                    field.seed(query.to_owned(), cx);
                });
                root.refresh_browse(cx);
                root.browse_ready = true;
                if expand != "0" {
                    root.browse.expanded =
                        browse_expand_paths(&root.browse.hits);
                    root.rebuild_browse_entries();
                }
            }
            if let Ok(paths) = env::var("RANKI_UI_DEV_OPEN_BROWSE_GROUP") {
                root.view = View::Browse;
                root.refresh_browse(cx);
                for path in paths.split('|').filter(|path| !path.is_empty()) {
                    let _ = root.browse.expanded.insert(path.to_owned());
                }
                root.rebuild_browse_entries();
                root.browse_ready = true;
            }
        }
        match env::var("RANKI_UI_DEV_VIEW").as_deref() {
            Ok("question") => {
                root.study_all(window, cx);
                root.review.phase = Phase::Question;
            }
            Ok("answer") => {
                root.study_all(window, cx);
                root.review.phase = Phase::Answer;
            }
            _ => {}
        }
        if let Ok(offset) = env::var("RANKI_UI_DEV_STATS_HOVER")
            && let Ok(offset) = offset.parse::<i64>()
            && let Some(today) = root.statistics.days.last().map(|day| day.day)
        {
            root.heatmap_hover = Some(today.saturating_sub(offset));
        }
        if let Ok(path) = env::var("RANKI_UI_DEV_STUDY_DECK")
            && let Some(id) = root
                .deck_rows
                .iter()
                .find(|row| row.path == path)
                .and_then(|row| row.id)
            && let Some(index) =
                root.decks.iter().position(|deck| deck.id == id)
        {
            root.study_deck(index, window, cx);
        }
        if let Ok(rating) = env::var("RANKI_UI_DEV_GRADE")
            && let Some(rating) = match rating.as_str() {
                "again" => Some(Rating::Again),
                "hard" => Some(Rating::Hard),
                "good" => Some(Rating::Good),
                "easy" => Some(Rating::Easy),
                _ => None,
            }
            && root.review.current.is_some()
        {
            if root.review.phase != Phase::Answer {
                root.reveal(cx);
            }
            root.review.last_grade_at = None;
            root.grade(rating, cx);
        }
        if let Ok(theme) = env::var("RANKI_UI_DEV_THEME") {
            let choice = match theme.as_str() {
                "light" => Some(ThemeChoice::Light),
                "dark" => Some(ThemeChoice::Dark),
                "system" => Some(ThemeChoice::System),
                _ => None,
            };
            if let Some(choice) = choice {
                root.set_theme(choice, cx);
            }
        }
        if let Ok(width) = env::var("RANKI_UI_DEV_NAV_WIDTH")
            && let Ok(width) = width.parse::<f32>()
        {
            root.nav_width = theme::nav_width(width);
        }
        if let Ok(activity) = env::var("RANKI_UI_DEV_SYNC_RUNNING") {
            root.sync.signed_in = true;
            root.options.sync.enabled = true;
            root.sync.running = Some(match activity.as_str() {
                "signing-in" => SyncActivity::SigningIn,
                "keep-local" => SyncActivity::KeepingLocal,
                "keep-remote" => SyncActivity::KeepingRemote,
                "downloading-media" => SyncActivity::DownloadingMedia,
                _ => SyncActivity::Syncing,
            });
        }
        if let Ok(media) = env::var("RANKI_UI_DEV_SYNC_MEDIA")
            && let Some((done, total)) = media.split_once('/')
            && let (Ok(done), Ok(total)) =
                (done.parse::<usize>(), total.parse::<usize>())
        {
            root.sync.media = Some(MediaProgress { done, total });
        }
        if env::var("RANKI_UI_DEV_SYNC_CONFLICT")
            .is_ok_and(|value| value == "1")
        {
            root.sync.signed_in = true;
            root.options.sync.enabled = true;
            root.sync.conflict = Some(String::from(
                "both collections changed since the last sync",
            ));
        }
        if let Ok(path) = env::var("RANKI_UI_DEV_IMPORT") {
            root.import_path(PathBuf::from(path), cx);
        }
        if let Ok(error) = env::var("RANKI_UI_DEV_SYNC_ERROR") {
            root.sync.signed_in = true;
            root.options.sync.enabled = true;
            root.sync.error = Some(error);
        }
        if env::var("RANKI_UI_DEV_SYNC_SIGN_IN").is_ok_and(|value| value == "1")
        {
            root.view = View::Settings;
            let username =
                env::var("RANKI_UI_DEV_SYNC_USER").unwrap_or_default();
            let password =
                env::var("RANKI_UI_DEV_SYNC_PASS").unwrap_or_default();
            root.sync
                .username
                .update(cx, |field, cx| field.seed(&username, cx));
            root.sync
                .password
                .update(cx, |field, cx| field.seed(&password, cx));
            root.sign_in(cx);
        }
        if env::var("RANKI_UI_DEV_SYNC_RUNNING").is_ok()
            || env::var("RANKI_UI_DEV_SYNC_CONFLICT")
                .is_ok_and(|value| value == "1")
            || env::var("RANKI_UI_DEV_SYNC_ERROR").is_ok()
        {
            root.view = View::Settings;
        }
        if root.view == View::Decks
            && env::var("RANKI_UI_DEV_CREATE").is_ok_and(|value| value == "1")
        {
            root.create_modal = true;
            window.focus(root.create_field.read(cx).focus_handle());
        }
        if root.view == View::Decks
            && env::var("RANKI_UI_DEV_CREATE_ERROR")
                .is_ok_and(|value| value == "1")
        {
            root.create_modal = true;
            root.create_field.update(cx, |field, cx| {
                field.seed("Visual Deck", cx);
            });
            root.set_create_error(
                "Could not create deck: invalid data: deck already exists: Visual Deck",
                cx,
            );
            window.focus(root.create_field.read(cx).focus_handle());
        }
        if env::var("RANKI_UI_DEV_MENU").is_ok_and(|value| value == "1") {
            root.view = View::Decks;
            let viewport = window.viewport_size();
            root.deck_menu = root.decks.first().map(|deck| DeckMenu {
                id: deck.id,
                anchor: point(viewport.width - px(79.), px(423.)),
            });
        }
        if env::var("RANKI_UI_DEV_CONFIRM")
            .is_ok_and(|value| value == "remove-deck")
            && let Some(deck) = root.decks.first()
        {
            root.confirmation = Some(Confirmation::RemoveDeck {
                id: deck.id,
                name: deck.name.clone(),
            });
        }
        if env::var("RANKI_UI_DEV_BROWSE_MENU").is_ok_and(|value| value == "1")
        {
            root.view = View::Browse;
            root.refresh_browse(cx);
            let viewport = window.viewport_size();
            if let Some(id) = root.browse.hits.first().map(|hit| hit.card_id) {
                let _ = root.ensure_browse_row(id);
                root.card_menu = Some(CardMenu {
                    id,
                    anchor: point(viewport.width - px(408.), px(250.)),
                });
            }
        }
        if let Ok(index) = env::var("RANKI_UI_DEV_SELECT_ALL")
            && let Ok(index) = index.parse::<usize>()
            && let Some(field) = root.add.fields.get(index).cloned()
        {
            window.focus(field.read(cx).focus_handle());
            field.update(cx, super::text_field::TextField::select_all);
        }
        if env::var("RANKI_UI_DEV_BROWSE_EDIT").is_ok_and(|value| value == "1")
        {
            root.view = View::Browse;
            root.refresh_browse(cx);
            if let Some(id) = root.browse.hits.first().map(|hit| hit.card_id) {
                root.start_card_edit(id, window, cx);
            }
        }
        if env::var("RANKI_UI_DEV_BROWSE_REMOVE")
            .is_ok_and(|value| value == "1")
        {
            root.view = View::Browse;
            root.refresh_browse(cx);
            if let Some(id) = root.browse.hits.first().map(|hit| hit.card_id) {
                let front = root
                    .ensure_browse_row(id)
                    .map(|row| crate::browser_view::preview_line(&row.front))
                    .unwrap_or_default();
                root.confirmation =
                    Some(Confirmation::RemoveCard { id, front });
                window.focus(&root.dialog_focus);
            }
        }
        if env::var("RANKI_UI_DEV_DECK_PICKER").is_ok_and(|value| value == "1")
        {
            root.view = View::AddCard;
            root.add.deck_picker = true;
            root.add.deck_picker_anchor =
                point(px(root.nav_width + theme::PAGE_X + 12.), px(172.));
            window.focus(root.add.deck_filter.read(cx).focus_handle());
        }
        if env::var("RANKI_UI_DEV_COLOR_PICKER").is_ok_and(|value| value == "1")
            && let Some(field) = root.add.fields.first().cloned()
        {
            root.view = View::AddCard;
            root.toggle_color_picker(
                ColorTarget::Text,
                field,
                point(px(root.nav_width + theme::PAGE_X + 280.), px(362.)),
                window,
                cx,
            );
        }
        if let Ok(seed) = env::var("RANKI_UI_DEV_OCCLUSION") {
            root.seed_occlusion(&seed, cx);
            if let Ok(prefill) = env::var("RANKI_UI_DEV_FIELDS") {
                root.seed_fields(&prefill, cx);
            }
        }
        if let Some(count) = env::var("RANKI_UI_DEV_GRADE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
        {
            let weak = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = weak.update(cx, |root, cx| {
                    for _ in 0..count {
                        if root.review.current.is_none() {
                            break;
                        }
                        root.review.phase = Phase::Answer;
                        root.review.last_grade_at = None;
                        root.grade(Rating::Good, cx);
                    }
                });
            });
        }
        if let Ok(spec) = env::var("RANKI_UI_DEV_TOAST") {
            let (kind, text) = spec.split_once(':').map_or(
                (ToastKind::Error, spec.as_str()),
                |(kind, text)| {
                    let kind = if kind.eq_ignore_ascii_case("success") {
                        ToastKind::Success
                    } else {
                        ToastKind::Error
                    };
                    (kind, text)
                },
            );
            root.show_toast(kind, text, cx);
        }
        if env::var("RANKI_UI_DEV_SAVE").is_ok_and(|value| value == "1") {
            root.view = View::AddCard;
            let weak = cx.entity().downgrade();
            window.defer(cx, move |window, cx| {
                let _ = weak.update(cx, |root, cx| {
                    root.view = View::AddCard;
                    root.save_note(window, cx);
                });
            });
        }
        root
    }

    /// Fills the add-card fields from tab separated text.
    fn seed_fields(&mut self, prefill: &str, cx: &mut Context<Self>) {
        for (field, value) in self.add.fields.iter().zip(prefill.split('\t')) {
            field.update(cx, |field, cx| {
                field.seed(value.to_owned(), cx);
            });
        }
    }

    /// Seeds the occlusion editor from `image;l,t,w,h;l,t,w,h`.
    ///
    /// Masks go through the same path as drawn ones, so a scripted run saves
    /// exactly what a pointer run would.
    fn seed_occlusion(&mut self, seed: &str, cx: &mut Context<Self>) {
        let Some(model) =
            self.models.iter().position(NoteModel::is_image_occlusion)
        else {
            return;
        };
        self.select_model(model, cx);
        let mut parts = seed.split(';');
        if let Some(image) = parts.next().map(str::trim)
            && !image.is_empty()
        {
            self.choose_occlusion_image(image.to_owned(), cx);
        }
        for part in parts {
            let values = part
                .split(',')
                .filter_map(|value| value.trim().parse::<f32>().ok())
                .collect::<Vec<_>>();
            let Some(rect) = NormalizedRect::from_points(
                (
                    values.first().copied().unwrap_or(0.0),
                    values.get(1).copied().unwrap_or(0.0),
                ),
                (
                    values.first().copied().unwrap_or(0.0)
                        + values.get(2).copied().unwrap_or(0.0),
                    values.get(1).copied().unwrap_or(0.0)
                        + values.get(3).copied().unwrap_or(0.0),
                ),
            ) else {
                continue;
            };
            self.add_occlusion_mask(
                OcclusionMask {
                    shape: MaskShape::Rect,
                    rect,
                },
                cx,
            );
        }
    }
    /// Keeps the chip list in step with the tag field as words are typed.
    fn tag_hook(cx: &mut Context<Self>) -> EditHook {
        let weak = cx.entity().downgrade();
        Arc::new(move |app| {
            let weak = weak.clone();
            app.defer(move |app| {
                let _ = weak.update(app, |root, cx| {
                    root.sync_tags(cx);
                    if root.view == View::AddCard {
                        root.add.errors.clear();
                    }
                    root.add.tag_suggestion = 0;
                    root.add.tag_suggestions_dismissed = false;
                    cx.notify();
                });
            });
        })
    }

    pub(crate) fn sync_tags(&mut self, cx: &mut Context<Self>) {
        let text = self.add.tags.read(cx).text();
        let remainder = collect_tags(&mut self.add.tag_chips, &text);
        if remainder != text {
            self.add
                .tags
                .update(cx, |field, cx| field.seed(remainder, cx));
        }
    }

    fn refresh_known_tags(&mut self) {
        if let Ok(tags) = self.store.tags() {
            self.add.known_tags = tags;
        }
    }

    pub(crate) fn tag_suggestions(&self, cx: &Context<Self>) -> Vec<String> {
        let query = self.add.tags.read(cx).text();
        suggest_tags(
            &self.add.known_tags,
            &self.add.tag_chips,
            &query,
            TAG_SUGGESTION_LIMIT,
        )
    }

    pub(crate) fn choose_tag_suggestion(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tag) = self.tag_suggestions(cx).get(index).cloned() else {
            return;
        };
        if !self.add.tag_chips.contains(&tag) {
            self.add.tag_chips.push(tag);
        }
        self.add.tags.update(cx, |field, cx| field.seed("", cx));
        self.add.tag_suggestion = 0;
        self.add.tag_suggestions_dismissed = true;
        let focus = self.add.tags.read(cx).focus_handle().clone();
        window.focus(&focus);
        cx.notify();
    }

    fn edit_hook(cx: &mut Context<Self>) -> EditHook {
        let weak = cx.entity().downgrade();
        Arc::new(move |app| {
            let weak = weak.clone();
            app.defer(move |app| {
                let _ = weak.update(app, |root, cx| {
                    if root.view == View::AddCard {
                        root.add.errors.clear();
                    }
                    cx.notify();
                });
            });
        })
    }

    fn deck_search_edit_hook(cx: &mut Context<Self>) -> EditHook {
        let weak = cx.entity().downgrade();
        Arc::new(move |app| {
            let weak = weak.clone();
            app.defer(move |app| {
                let _ = weak.update(app, |root, cx| {
                    root.persist_state(cx);
                    cx.notify();
                });
            });
        })
    }

    /// A create-deck error stays until the name changes.
    fn create_field_hook(cx: &mut Context<Self>) -> EditHook {
        let weak = cx.entity().downgrade();
        Arc::new(move |app| {
            let weak = weak.clone();
            app.defer(move |app| {
                let _ = weak.update(app, |root, cx| {
                    if root.create_error.take().is_some() {
                        cx.notify();
                    }
                });
            });
        })
    }

    pub(crate) fn observe_appearance(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.observe_window_appearance(window, |this, window, cx| {
            this.system_dark = !matches!(
                window.appearance(),
                WindowAppearance::Light | WindowAppearance::VibrantLight
            );
            this.refresh_field_colors(cx);
            cx.notify();
        })
        .detach();
    }

    pub(crate) const fn theme(&self) -> Theme {
        theme::resolve_theme(self.system_dark, self.options.theme)
    }

    pub(crate) const fn colors(&self) -> Palette {
        theme::palette(self.theme())
    }

    pub(crate) fn set_theme(
        &mut self,
        choice: ThemeChoice,
        cx: &mut Context<Self>,
    ) {
        if self.options.theme == choice {
            return;
        }
        self.options.theme = choice;
        self.refresh_field_colors(cx);
        cx.notify();
        self.persist_state(cx);
    }

    pub(crate) fn set_reduce_motion(
        &mut self,
        choice: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        if self.options.reduce_motion == choice {
            return;
        }
        self.options.reduce_motion = choice;
        cx.notify();
        self.persist_state(cx);
    }

    pub(crate) fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .try_into()
            .unwrap_or(0)
    }

    fn record_local_write(&mut self) {
        if let Ok(stamp) = self.store.collection_modified() {
            self.collection_modified = stamp;
        }
        self.last_change_check = Instant::now();
    }

    fn poll_external_change(&mut self, cx: &mut Context<Self>) -> bool {
        if self.external_change {
            return true;
        }
        if self.last_change_check.elapsed() < Duration::from_secs(1) {
            return false;
        }
        self.last_change_check = Instant::now();
        if self
            .store
            .collection_modified()
            .is_ok_and(|stamp| stamp != self.collection_modified)
        {
            self.external_change = true;
            self.show_toast(
                ToastKind::Error,
                "Collection changed outside Ranki. Reload before grading, undoing, or saving notes.",
                cx,
            );
            cx.notify();
        }
        self.external_change
    }

    pub(crate) fn start_external_change_poll(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, Self::poll_external_change).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn reload_collection(&mut self, cx: &mut Context<Self>) {
        self.deck_edit = None;
        if self.import_running {
            self.show_toast(
                ToastKind::Error,
                "Import in progress. Wait for it to finish before reloading.",
                cx,
            );
            return;
        }
        self.review = ReviewState::default();
        self.view = View::Decks;
        self.previous_view = View::Decks;
        self.add.deck_picker = false;
        self.add.color_picker = None;
        self.images.update(cx, |cache, _| cache.forget_failed());
        self.reload(cx, false);
        self.statistics_ready = false;
        self.browse_ready = false;
        self.persist_state(cx);
    }

    fn request_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.import_running {
            self.show_toast(
                ToastKind::Error,
                "Import in progress. Wait for it to finish before reloading.",
                cx,
            );
            return;
        }
        if self.confirmation.is_some() {
            return;
        }
        if self.review.current.is_some() || self.review.answered > 0 {
            self.confirmation = Some(Confirmation::Reload);
            window.focus(&self.dialog_focus);
        } else {
            self.reload_collection(cx);
        }
        cx.notify();
    }
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>, initial: bool) {
        self.heatmap_hover = None;
        let selected_id = self
            .selected
            .and_then(|index| self.decks.get(index))
            .map(|deck| deck.id);
        let draft_fields = self
            .add
            .fields
            .iter()
            .map(|field| field.read(cx).text())
            .collect::<Vec<_>>();
        let previous_fields = self
            .models
            .get(self.add.model_index)
            .map(|model| model.fields.clone())
            .unwrap_or_default();
        match self.store.decks() {
            Ok(decks) => self.decks = decks,
            Err(error) => {
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not load decks: {error}"),
                    cx,
                );
            }
        }
        let chosen = self
            .models
            .get(self.add.model_index)
            .map(|model| model.name.clone());
        match self.store.models() {
            Ok(models) => self.models = models_with_stock(models),
            Err(error) => {
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not load note types: {error}"),
                    cx,
                );
            }
        }
        self.refresh_known_tags();
        if let Some(name) = chosen.as_ref()
            && let Some(index) = model_index(&self.models, name)
        {
            self.add.model_index = index;
        }
        if self.add.model_index >= self.models.len() {
            self.add.model_index = 0;
        }
        if let Some(preferred) = crate::add_card_view::preferred_model_index(
            &self.models,
            self.add.model_index,
        ) {
            self.add.model_index = preferred;
        }
        self.refresh_stats();
        let background_summary =
            initial && self.options.paths.collection.exists();
        if !background_summary {
            self.refresh_statistics(cx);
        }
        self.collection_start = self.store.collection_created().unwrap_or(0);
        self.collection_modified =
            self.store.collection_modified().unwrap_or(0);
        if background_summary {
            self.schedule_statistics_refresh(cx);
        } else {
            self.refresh_statistics(cx);
        }
        self.external_change = false;
        if let Some(id) = selected_id {
            self.selected = self.decks.iter().position(|deck| deck.id == id);
        }
        if self.selected.is_none() && !self.decks.is_empty() {
            self.selected = Some(0);
        }
        if self.add.deck_id.is_some() && self.add_deck_index().is_none() {
            self.add.deck_id = self
                .selected
                .and_then(|index| self.decks.get(index))
                .map(|deck| deck.id);
        }
        if self.add.deck_id.is_none() && !self.decks.is_empty() {
            self.add.deck_id = self
                .selected
                .and_then(|index| self.decks.get(index))
                .map(|deck| deck.id);
        }
        self.reset_fields(cx);
        let model_fields = self
            .models
            .get(self.add.model_index)
            .map(|model| model.fields.clone())
            .unwrap_or_default();
        for (index, field) in self.add.fields.iter().enumerate() {
            let Some(name) = model_fields.get(index) else {
                continue;
            };
            let Some(old_index) = previous_fields
                .iter()
                .position(|old| old.eq_ignore_ascii_case(name))
            else {
                continue;
            };
            if let Some(value) = draft_fields.get(old_index) {
                field.update(cx, |field, cx| field.seed(value.clone(), cx));
            }
        }
        cx.notify();
    }

    /// Recomputes due counts, the forecast, and deck rows.
    fn refresh_stats(&mut self) {
        self.stats = vec![ranki_model::DeckStats::default(); self.decks.len()];
        let now = Self::now();
        self.stats = match self.store.deck_stats(now) {
            Ok(stats) => {
                let lookup = stats.into_iter().collect::<HashMap<_, _>>();
                self.decks
                    .iter()
                    .map(|deck| {
                        lookup.get(&deck.id).copied().unwrap_or_default()
                    })
                    .collect()
            }
            Err(_) => self
                .decks
                .iter()
                .map(|deck| {
                    self.store.stats(Some(deck.id), now).unwrap_or_default()
                })
                .collect(),
        };
        self.forecast = self
            .store
            .due_forecast(FORECAST_DAYS, now)
            .unwrap_or_default();
        self.rebuild_deck_rows();
    }

    /// Rebuilds the collection-wide review summary from the store's log.
    ///
    /// A reload with the same collection stamp and day boundary keeps the
    /// summary it already has.
    fn refresh_statistics(&mut self, cx: &mut Context<Self>) {
        let timing = self.timing(Self::now());
        let modified = self.store.collection_modified().unwrap_or(0);
        if modified == self.statistics_modified
            && timing.next_day_at == self.statistics_next_day_at
        {
            return;
        }
        self.statistics_generation = self.statistics_generation.wrapping_add(1);
        match self.store.review_history() {
            Ok(history) => {
                self.statistics = build_statistics_timed(&history, &timing);
                self.statistics_modified = modified;
                self.statistics_next_day_at = timing.next_day_at;
                self.statistics_error = None;
            }
            Err(error) => {
                self.statistics_error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    /// Refreshes the review summary after the statistics view is on screen.
    ///
    /// Reading a large review log can take hundreds of milliseconds, so the
    /// click only switches views. A separate store reads the log on a
    /// background thread and closes the file between calls.
    pub(crate) fn schedule_statistics_refresh(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let timing = self.timing(Self::now());
        let modified = self.store.collection_modified().unwrap_or(0);
        if self.statistics_error.is_none()
            && modified == self.statistics_modified
            && timing.next_day_at == self.statistics_next_day_at
        {
            return;
        }
        let generation = self.statistics_generation.wrapping_add(1);
        self.statistics_generation = generation;
        let collection = self.options.paths.collection.clone();
        let task = cx.background_executor().spawn(async move {
            let store = ranki_storage::ReopeningCollection::new(&collection);
            let modified = Store::collection_modified(&store)?;
            let history = Store::review_history(&store)?;
            Ok::<_, ranki_model::StoreError>((
                build_statistics_timed(&history, &timing),
                modified,
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |root, cx| {
                if root.statistics_generation != generation {
                    return;
                }
                match result {
                    Ok((statistics, modified)) => {
                        root.statistics = statistics;
                        root.statistics_modified = modified;
                        root.statistics_next_day_at = timing.next_day_at;
                        root.statistics_error = None;
                        cx.notify();
                    }
                    Err(error) => {
                        root.statistics_error = Some(error.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Reports a completed action. The toast retires on its own after
    /// [`TOAST_LIFETIME`], or sooner when dismissed.
    pub(crate) fn set_toast(
        &mut self,
        kind: ToastKind,
        text: impl Into<String>,
    ) {
        static NEXT: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        self.toast = Some(Toast {
            kind,
            text: text.into(),
            created_at: Instant::now(),
            seq: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        });
    }

    /// Clears the toast and arms its dismissal, so the window redraws even
    /// when the pointer and keyboard stay idle.
    pub(crate) fn show_toast(
        &mut self,
        kind: ToastKind,
        text: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.set_toast(kind, text);
        let created_at = self.toast.as_ref().map(|toast| toast.created_at);
        let Some(created_at) = created_at else {
            return;
        };
        let timer = cx.background_executor().timer(TOAST_LIFETIME);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |root, cx| {
                let same = root
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.created_at == created_at);
                if same {
                    root.clear_toast();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn clear_toast(&mut self) {
        self.toast = None;
    }

    /// Clears a toast only when the message still belongs to this task.
    pub(crate) fn clear_toast_if(&mut self, seq: Option<u64>) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| Some(toast.seq) == seq)
        {
            self.toast = None;
        }
    }

    /// Errors from the create-deck dialog belong next to its input, not in a
    /// toast behind the modal.
    pub(crate) fn set_create_error(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.create_error = Some(message.into());
        self.clear_toast();
        cx.notify();
    }

    pub(crate) fn live_toast(&self) -> Option<&Toast> {
        self.toast
            .as_ref()
            .filter(|toast| !toast.expired(Instant::now()))
    }

    /// Motion is on unless the setting or the desktop preference disables it.
    pub(crate) fn motion_ms(&self, milliseconds: u64) -> u64 {
        if theme::reduced_motion(self.options.reduce_motion) {
            0
        } else {
            milliseconds
        }
    }

    /// How long the current card has been on screen, for the session summary.
    pub(crate) fn session_elapsed(&self) -> Duration {
        let live = if self.review.current.is_some() {
            self.review.focused_elapsed
        } else {
            Duration::ZERO
        };
        self.review.studied.saturating_add(live)
    }

    fn reset_fields(&mut self, cx: &mut Context<Self>) {
        let model = self.models.get(self.add.model_index);
        let names = model.map_or_else(Vec::new, |item| item.fields.clone());
        let hook = Self::edit_hook(cx);
        self.add.fields = names
            .into_iter()
            .map(|name| {
                cx.new(|cx| {
                    let mut field = TextField::new(cx, name);
                    field.set_edit_hook(hook.clone());
                    field
                })
            })
            .collect();
        self.refresh_field_colors(cx);
    }

    pub(crate) fn select_model(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.card_edit.is_some() {
            return;
        }
        let Some(model) = self.models.get(index).cloned() else {
            return;
        };
        self.add.model_index = index;
        self.add.errors.clear();
        if model.id > 0 {
            self.reset_fields(cx);
            cx.notify();
            return;
        }
        match StockModelKind::of(&model) {
            Some(kind) => match self.store.ensure_model(kind) {
                Ok(installed) => {
                    self.reload(cx, false);
                    if let Some(found) =
                        model_index(&self.models, &installed.name)
                    {
                        self.add.model_index = found;
                    }
                    self.reset_fields(cx);
                    self.clear_toast();
                }
                Err(error) => {
                    self.add.errors.insert(
                        "model".into(),
                        format!("Could not install {}: {error}", model.name),
                    );
                }
            },
            None => {
                self.add.errors.insert(
                    "model".into(),
                    "This note type is not in the collection".into(),
                );
            }
        }
        cx.notify();
    }
    pub(crate) fn add_deck_index(&self) -> Option<usize> {
        self.add
            .deck_id
            .and_then(|id| self.decks.iter().position(|deck| deck.id == id))
    }

    /// Deck indices currently visible in the add-card picker.
    pub(crate) fn add_deck_matches(&self, cx: &Context<Self>) -> Vec<usize> {
        let query = self.add.deck_filter.read(cx).text();
        let needle = query.trim().to_lowercase();
        self.decks
            .iter()
            .enumerate()
            .filter(|(_, deck)| {
                needle.is_empty() || contains_lowercase(&deck.name, &needle)
            })
            .map(|(index, _)| index)
            .collect()
    }
    pub(crate) fn select_add_deck(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if index >= self.decks.len() {
            return;
        }
        self.add.deck_id = self.decks.get(index).map(|deck| deck.id);
        self.add.deck_picker = false;
        let _ = self.add.errors.remove("deck");
        cx.notify();
    }

    pub(crate) fn toggle_add_deck_picker(
        &mut self,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add.deck_picker = !self.add.deck_picker;
        if self.add.deck_picker {
            self.add.deck_picker_anchor = anchor;
            self.add.deck_picker_dismissed_press = None;
            let filter = self.add.deck_filter.read(cx).focus_handle().clone();
            window.focus(&filter);
        } else {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    pub(crate) fn toggle_color_picker(
        &mut self,
        target: ColorTarget,
        field: Entity<TextField>,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .add
            .color_picker
            .as_ref()
            .is_some_and(|picker| picker.target == target);
        if open {
            self.close_color_picker(window, cx);
            return;
        }
        self.add.deck_picker = false;
        self.add.color_hex.update(cx, TextField::clear);
        self.add.color_picker = Some(ColorPicker {
            target,
            field,
            anchor,
        });
        self.add.color_picker_dismissed_press = None;
        self.add.color_picker_dismissed_target = None;
        let hex = self.add.color_hex.read(cx).focus_handle().clone();
        window.focus(&hex);
        cx.notify();
    }

    pub(crate) fn close_color_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.add.color_picker.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        }
    }

    /// Opens a picker for `.apkg` and `.colpkg` files, then imports the choice.
    pub(crate) fn import_deck(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let seq = this
                .update(cx, |root, cx| {
                    root.show_toast(
                        ToastKind::Success,
                        "Importing package...",
                        cx,
                    );
                    root.toast.as_ref().map(|toast| toast.seq)
                })
                .ok()
                .flatten();
            let chosen = choose_import_path(cx).await;
            let _ = this.update(cx, |root, cx| match chosen {
                Ok(Some(path)) => root.import_path(path, cx),
                Ok(None) => root.clear_toast_if(seq),
                Err(error) => root.show_toast(
                    ToastKind::Error,
                    format!("Could not choose package: {error}"),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Imports one chosen package into the app collection and reloads the UI.
    pub(crate) fn import_path(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.import_running {
            self.show_toast(
                ToastKind::Error,
                "An import is already running. Wait for it to finish.",
                cx,
            );
            return;
        }
        if !is_anki_package(&path) {
            self.show_toast(
                ToastKind::Error,
                format!("Not an Anki package: {}", path.display()),
                cx,
            );
            return;
        }
        let collection = self.options.paths.collection.clone();
        let media_dir = self.options.paths.media_dir.clone();
        let name = path.display().to_string();
        let import = cx.background_executor().spawn(async move {
            ranki_format::import_apkg(&path, &collection, &media_dir)
        });
        self.import_running = true;
        self.show_toast(ToastKind::Success, format!("Importing {name}..."), cx);
        cx.spawn(async move |this, cx| {
            let result = import.await;
            let _ = this.update(cx, |root, cx| {
                root.import_running = false;
                match result {
                    Ok(report) => {
                        root.reload_collection(cx);
                        root.show_toast(
                            ToastKind::Success,
                            format!(
                                "Imported {} notes, {} cards, {} media from {name}",
                                report.notes, report.cards, report.media
                            ),
                            cx,
                        );
                    }
                    Err(error) => root.show_toast(
                        ToastKind::Error,
                        format!("Could not import {name}: {error}"),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    fn export_file_name(deck_name: &str) -> String {
        let safe: String = deck_name
            .trim()
            .replace("::", " - ")
            .chars()
            .map(|character| {
                if character.is_alphanumeric()
                    || matches!(character, ' ' | '-' | '_' | '.')
                {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let stem = safe.trim_matches(|character| {
            matches!(character, '.' | ' ' | '-' | '_')
        });
        if stem.is_empty() {
            "deck.apkg".to_owned()
        } else {
            format!("{stem}.apkg")
        }
    }

    /// Exports a deck and its subdecks to a path chosen in a save dialog.
    pub(crate) fn export_deck(&mut self, deck: &Deck, cx: &mut Context<Self>) {
        let collection = self.options.paths.collection.clone();
        let media_dir = self.options.paths.media_dir.clone();
        let deck_id = deck.id;
        let deck_name = deck.name.clone();
        self.show_toast(
            ToastKind::Success,
            format!("Exporting {deck_name}..."),
            cx,
        );
        let seq = self.toast.as_ref().map(|toast| toast.seq);
        let suggested = Self::export_file_name(&deck.name);
        let directory = collection
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let receiver = cx.prompt_for_new_path(directory, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let chosen = match receiver.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => {
                    let _ = this.update(cx, |root, cx| {
                        root.clear_toast_if(seq);
                        cx.notify();
                    });
                    return;
                }
                Ok(Err(error)) => {
                    let _ = this.update(cx, |root, cx| {
                        root.show_toast(
                            ToastKind::Error,
                            format!("Could not choose export path: {error}"),
                            cx,
                        );
                    });
                    return;
                }
                Err(error) => {
                    let _ = this.update(cx, |root, cx| {
                        root.show_toast(
                            ToastKind::Error,
                            format!("Could not choose export path: {error}"),
                            cx,
                        );
                    });
                    return;
                }
            };
            let export_path = chosen.clone();
            let export = cx.background_executor().spawn(async move {
                ranki_format::export_deck_apkg(
                    &collection,
                    &media_dir,
                    &export_path,
                    deck_id,
                )
            });
            let result = export.await;
            let _ = this.update(cx, |root, cx| match result {
                Ok(()) => root.show_toast(
                    ToastKind::Success,
                    format!("Exported {deck_name} to {}", chosen.display()),
                    cx,
                ),
                Err(error) => root.show_toast(
                    ToastKind::Error,
                    format!("Could not export {deck_name}: {error}"),
                    cx,
                ),
            });
        })
        .detach();
    }
    /// Copies a chosen image into the media directory for the occlusion editor.
    pub(crate) fn upload_occlusion_image(&mut self, cx: &mut Context<Self>) {
        let media_dir = self.options.paths.media_dir.clone();
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose image".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = match receiver.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => occlusion::import_image(&path, &media_dir)
                        .map(Some)
                        .map_err(|error| error.to_string()),
                    None => Ok(None),
                },
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update(cx, |root, cx| match result {
                Ok(Some(name)) => root.choose_occlusion_image(name, cx),
                Ok(None) => {}
                Err(error) => root.show_toast(
                    ToastKind::Error,
                    format!("Could not upload image: {error}"),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Copies a chosen image into the media directory and inserts its Anki
    /// reference at the caret of the given field.
    pub(crate) fn upload_field_image(
        &mut self,
        field: Entity<TextField>,
        cx: &mut Context<Self>,
    ) {
        let media_dir = self.options.paths.media_dir.clone();
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose image".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = match receiver.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => occlusion::import_image(&path, &media_dir)
                        .map(Some)
                        .map_err(|error| error.to_string()),
                    None => Ok(None),
                },
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update(cx, |root, cx| match result {
                Ok(Some(name)) => {
                    Self::insert_field_image(field, name, cx);
                }
                Ok(None) => {}
                Err(error) => root.show_toast(
                    ToastKind::Error,
                    format!("Could not upload image: {error}"),
                    cx,
                ),
            });
        })
        .detach();
    }

    pub(crate) fn insert_field_image(
        field: Entity<TextField>,
        name: String,
        cx: &mut Context<Self>,
    ) {
        let markup = occlusion::image_field(&name);
        field.update(cx, |field, cx| {
            field.insert_text(&markup, cx);
        });
        cx.notify();
    }

    pub(crate) fn choose_occlusion_image(
        &mut self,
        name: String,
        cx: &mut Context<Self>,
    ) {
        if self.add.occlusion.image.is_some()
            && self.add.occlusion.image.as_deref() != Some(name.as_str())
        {
            self.add.occlusion.masks.clear();
        }
        let size = card_view::media_path(&self.options.paths.media_dir, &name)
            .and_then(|path| image::image_dimensions(path).ok())
            .map(|(width, height)| {
                (
                    f32::from(u16::try_from(width).unwrap_or(u16::MAX)),
                    f32::from(u16::try_from(height).unwrap_or(u16::MAX)),
                )
            });
        self.add.occlusion.image_size = size;
        self.add.occlusion.image = Some(name);
        let _ = self.add.errors.remove("occlusion");
        cx.notify();
    }

    pub(crate) fn remove_occlusion_image(&mut self, cx: &mut Context<Self>) {
        self.add.occlusion.clear_image();
        let _ = self.add.errors.remove("occlusion");
        let _ = self.add.errors.remove("masks");
        cx.notify();
    }

    pub(crate) fn add_occlusion_mask(
        &mut self,
        mask: OcclusionMask,
        cx: &mut Context<Self>,
    ) {
        self.add.occlusion.masks.push(mask);
        let _ = self.add.errors.remove("masks");
        cx.notify();
    }

    /// Adds a default mask without requiring a pointer drag.
    pub(crate) fn add_default_occlusion_mask(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let index = self.add.occlusion.masks.len();
        self.add_occlusion_mask(occlusion::default_mask(index), cx);
    }

    /// Removes one mask from its explicit remove control.
    pub(crate) fn remove_occlusion_mask(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.add.occlusion.remove_mask(index) {
            let _ = self.add.errors.remove("masks");
            cx.notify();
        }
    }

    /// Keyboard counterpart of the remove control on the newest mask.
    pub(crate) fn remove_last_occlusion_mask(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        let index = self.add.occlusion.masks.len().saturating_sub(1);
        self.remove_occlusion_mask(index, cx);
    }

    /// Records the drawing area so pointer positions can be normalized.
    pub(crate) fn set_occlusion_bounds(
        &mut self,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.add.occlusion.bounds != Some(bounds) {
            self.add.occlusion.bounds = Some(bounds);
            cx.notify();
        }
    }

    pub(crate) fn occlusion_press(
        &mut self,
        position: (f32, f32),
        button: MouseButton,
        cx: &mut Context<Self>,
    ) {
        if self.add.occlusion.press(position, button) {
            cx.notify();
        }
    }

    pub(crate) fn occlusion_drag_to(
        &mut self,
        position: (f32, f32),
        cx: &mut Context<Self>,
    ) {
        if self.add.occlusion.drag_to(position) {
            cx.notify();
        }
    }

    pub(crate) fn occlusion_release(&mut self, cx: &mut Context<Self>) {
        if self.add.occlusion.drag.is_some() {
            if self.add.occlusion.finish_drag() {
                let _ = self.add.errors.remove("masks");
            }
            cx.notify();
        }
    }

    pub(crate) fn cancel_occlusion_drag(&mut self, cx: &mut Context<Self>) {
        if self.add.occlusion.cancel_drag() {
            cx.notify();
        }
    }
    /// Image Occlusion field values in the stock model order.
    pub(crate) fn occlusion_fields(
        &self,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        let field = |index: usize, cx: &mut Context<Self>| {
            self.add
                .fields
                .get(index)
                .map_or_else(String::new, |entry| entry.read(cx).text())
        };
        let header = field(2, cx);
        let back_extra = field(3, cx);
        let comments = field(4, cx);
        let image = self.add.occlusion.image.clone().unwrap_or_default();
        image_occlusion_fields(
            &header,
            &back_extra,
            &comments,
            &image,
            &self.add.occlusion.masks,
        )
    }

    pub(crate) fn study_deck(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deck_edit = None;
        if let Some(deck) = self.decks.get(index) {
            window.focus(&self.focus);
            self.selected = Some(index);
            self.review = ReviewState::default();
            let now = Self::now();
            match self.store.due_cards(
                Some(deck.id),
                self.options.review_batch_size,
                now,
            ) {
                Ok(cards) => self.seed_session(cards),
                Err(error) => {
                    self.review.load_error = Some(error.to_string());
                    self.show_toast(
                        ToastKind::Error,
                        format!("Could not load cards: {error}"),
                        cx,
                    );
                }
            }
            self.previous_view = View::Decks;
            self.view = View::Review;
            self.ensure_current(cx);
            cx.notify();
            self.persist_state(cx);
        }
    }

    pub(crate) fn study_all(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deck_edit = None;
        window.focus(&self.focus);
        self.review = ReviewState::default();
        self.review.scope_all = true;
        let now = Self::now();
        match self
            .store
            .due_cards(None, self.options.review_batch_size, now)
        {
            Ok(cards) => self.seed_session(cards),
            Err(error) => {
                self.review.load_error = Some(error.to_string());
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not load cards: {error}"),
                    cx,
                );
            }
        }
        self.previous_view = View::Decks;
        self.view = View::Review;
        self.ensure_current(cx);
        cx.notify();
        self.persist_state(cx);
    }

    /// Splits a fetched batch the way Anki splits its queues. Intraday learning
    /// cards wait in `pending` until their step is due, so an answer that comes
    /// back in a minute does not jump ahead of the cards already waiting.
    fn seed_session(&mut self, cards: Vec<Card>) {
        self.review.learn_ahead_secs = self.store.learn_ahead_secs().ok();
        for card in cards {
            if card.queue == CardQueue::Learning {
                Self::insert_pending(&mut self.review.pending, card);
            } else {
                self.review.queue.push_back(card);
            }
        }
        self.review.session_total = self
            .review
            .queue
            .len()
            .saturating_add(self.review.pending.len());
    }

    /// Places a card in the intraday learning queue, ordered the way Anki sorts
    /// it: previously-attempted cards first, then by due time.
    fn insert_pending(pending: &mut VecDeque<Card>, card: Card) {
        let key = |entry: &Card| (entry.reps == 0, entry.due);
        let index = pending.partition_point(|entry| key(entry) < key(&card));
        pending.insert(index, card);
    }

    /// Returns an answered learning card to the intraday queue. A step that is
    /// already due while nothing else is waiting is pushed behind the card it
    /// would have replaced, so it is not shown twice in a row.
    fn requeue_learning(&mut self, mut card: Card) {
        let cutoff = Self::now().saturating_add(
            self.review.learn_ahead_secs.unwrap_or(LEARN_AHEAD_SECS),
        );
        if card.due <= cutoff
            && self.review.queue.is_empty()
            && let Some(next) = self.review.pending.front()
            && next.due >= card.due
            && next.due.saturating_add(1) < cutoff
        {
            card.due = next.due.saturating_add(1);
        }
        Self::insert_pending(&mut self.review.pending, card);
    }

    /// Restarts the session the summary just ended, with the same scope.
    pub(crate) fn study_again(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected {
            Some(index) if !self.review.scope_all => {
                self.study_deck(index, window, cx);
            }
            _ => self.study_all(window, cx),
        }
    }

    pub(crate) fn session_complete(&self) -> bool {
        self.view == View::Review
            && self.review.current.is_none()
            && self.review.answered > 0
    }

    pub(crate) fn end_session(&mut self, cx: &mut Context<Self>) {
        self.review.queue.clear();
        self.review.pending.clear();
        self.review.current = None;
        self.review.last_grade = None;
        self.review.session_dirty = false;
        self.review.phase = Phase::Question;
        self.view = View::Decks;
        self.refresh_stats();
        cx.notify();
    }

    /// Placeholder ink must follow the palette, including live appearance swaps.
    pub(crate) fn refresh_field_colors(&mut self, cx: &mut Context<Self>) {
        let color = gpui::Hsla::from(theme::placeholder_color(self.theme()));
        let fields = [
            &self.deck_search,
            &self.browse.search,
            &self.create_field,
            &self.rename_field,
            &self.add.tags,
            &self.add.media_filter,
            &self.add.deck_filter,
        ];
        for field in fields {
            field.update(cx, |field, _| field.set_placeholder_color(color));
        }
        for field in &self.add.fields {
            field.update(cx, |field, _| field.set_placeholder_color(color));
        }
    }

    /// Picks the earliest intraday learning card due by `cutoff_secs`.
    ///
    /// Learning dues are Unix seconds and `pending` is kept in Anki's learning
    /// order, so the first card inside the cutoff is the one to show. Day
    /// learning and review dues are day numbers and never live here.
    fn due_learning_card(
        pending: &VecDeque<Card>,
        cutoff_secs: i64,
    ) -> Option<Card> {
        pending
            .iter()
            .find(|card| {
                card.queue == CardQueue::Learning && card.due <= cutoff_secs
            })
            .cloned()
    }

    fn take_learning_card(
        &mut self,
        cutoff_secs: i64,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(card) =
            Self::due_learning_card(&self.review.pending, cutoff_secs)
        else {
            return false;
        };
        self.review.pending.retain(|entry| entry.id != card.id);
        let _ = self.review.seen.remove(&card.id);
        if !self.load_card(&card, cx) {
            self.review.load_errors = self.review.load_errors.saturating_add(1);
        }
        true
    }

    fn ensure_current(&mut self, cx: &mut Context<Self>) {
        let now = Self::now();
        while self.review.current.is_none() && self.take_learning_card(now, cx)
        {
        }
        while self.review.current.is_none()
            && let Some(card) = self.review.queue.pop_front()
        {
            if self.review.seen.contains(&card.id) {
                continue;
            }
            let _ = self.review.seen.insert(card.id);
            if !self.load_card(&card, cx) {
                self.review.load_errors =
                    self.review.load_errors.saturating_add(1);
                continue;
            }
            break;
        }
        while self.review.current.is_none()
            && self.take_learning_card(
                now.saturating_add(
                    self.review.learn_ahead_secs.unwrap_or(LEARN_AHEAD_SECS),
                ),
                cx,
            )
        {}
        if self.review.current.is_none() {
            self.review.question = None;
            self.review.answer = None;
            self.review.note = None;
            self.review.progress_from = self.progress_fraction();
            if self.review.load_error.is_none() {
                let scope = if self.review.scope_all {
                    None
                } else {
                    self.selected
                        .and_then(|index| self.decks.get(index))
                        .map(|deck| deck.id)
                };
                self.review.more_due = self
                    .store
                    .due_cards(scope, 1, Self::now())
                    .is_ok_and(|cards| !cards.is_empty());
            }
            self.refresh_stats();
        }
    }

    /// Renders one card into the review session. Returns false when its note
    /// cannot be loaded, so the caller can try the next queued card.
    fn load_card(&mut self, card: &Card, cx: &mut Context<Self>) -> bool {
        let Some(loaded) = self.render_state(card, cx) else {
            return false;
        };
        self.apply_loaded(card, loaded, cx);
        true
    }

    /// Reads and renders a card without touching the session. Callers prepare
    /// the next stage before a write, so a failed read cannot leave storage
    /// and the visible card disagreeing.
    fn render_state(
        &mut self,
        card: &Card,
        cx: &mut Context<Self>,
    ) -> Option<LoadedCard> {
        let Some(note) = self.store.note(card.note_id).ok().flatten() else {
            self.show_toast(
                ToastKind::Error,
                "This card's note cannot be read. Refresh the collection.",
                cx,
            );
            return None;
        };
        let Some(model) =
            self.models.iter().find(|model| model.id == note.model_id)
        else {
            self.show_toast(
                ToastKind::Error,
                "This card's note type is missing. Refresh the collection.",
                cx,
            );
            return None;
        };
        let question = card_view::render_card(
            model,
            &note,
            card_view::CardRequest::question(card.ordinal),
        );
        let answer = card_view::render_card(
            model,
            &note,
            card_view::CardRequest::answer_only(card.ordinal),
        );
        let answer_media =
            card_view::answer_only_media(&question.media, &answer.media);
        let occlusion_image = note.fields.get(1).and_then(|field| {
            card_view::media_refs(field)
                .into_iter()
                .find(|media| media.kind == MediaKind::Image)
        });
        let occlusion_image_size = if question.masks.is_empty() {
            None
        } else {
            occlusion_image
                .as_ref()
                .and_then(|media| {
                    card_view::media_path(
                        &self.options.paths.media_dir,
                        &media.name,
                    )
                })
                .and_then(|path| image::image_dimensions(path).ok())
                .map(|(width, height)| {
                    (
                        f32::from(u16::try_from(width).unwrap_or(u16::MAX)),
                        f32::from(u16::try_from(height).unwrap_or(u16::MAX)),
                    )
                })
        };
        Some(LoadedCard {
            note: Some(note),
            question: Some(question),
            answer: Some(answer),
            answer_media,
            occlusion_image,
            occlusion_image_size,
        })
    }

    fn reset_review_scroll(&mut self) {
        self.review_stage_scroll = ScrollHandle::new();
        self.review_answer_anchor =
            Some(ScrollAnchor::for_handle(self.review_stage_scroll.clone()));
        self.review_answer_origin.set(None);
        self.review_answer_scroll_pending = false;
    }

    /// Ask the next frames to bring the answer's top into view.
    fn request_answer_scroll(&mut self) {
        self.review_answer_origin.set(None);
        self.review_answer_scroll_pending = true;
    }

    /// Puts a prepared card on the stage and restarts its timers.
    fn apply_loaded(
        &mut self,
        card: &Card,
        loaded: LoadedCard,
        _cx: &mut Context<Self>,
    ) {
        self.reset_review_scroll();
        self.review.phase = if self.dev_reveal {
            Phase::Answer
        } else {
            Phase::Question
        };
        self.review.current = Some(card.clone());
        self.review.note = loaded.note;
        self.review.question = loaded.question;
        self.review.answer = loaded.answer;
        self.review.answer_media = loaded.answer_media;
        self.review.occlusion_image = loaded.occlusion_image;
        self.review.occlusion_image_size = loaded.occlusion_image_size;
        self.review.last_tick = Some(Instant::now());
        self.review.steps =
            self.store.deck_steps(card.deck_id).unwrap_or_default();
        self.refresh_previews();
        if self.dev_reveal {
            self.request_answer_scroll();
        }
    }

    pub(crate) fn reveal(&mut self, cx: &mut Context<Self>) {
        if self.view != View::Review || self.review.current.is_none() {
            return;
        }
        match self.review.phase {
            Phase::Question => {
                self.review.phase = Phase::Answer;
                self.review.last_tick = Some(Instant::now());
                self.request_answer_scroll();
                cx.notify();
            }
            Phase::Answer => self.grade(ranki_model::Rating::Good, cx),
        }
    }

    pub(crate) fn toggle_reveal(&mut self, cx: &mut Context<Self>) {
        if self.view != View::Review || self.review.current.is_none() {
            return;
        }
        let revealing = self.review.phase == Phase::Question;
        self.review.phase = match self.review.phase {
            Phase::Question => Phase::Answer,
            Phase::Answer => Phase::Question,
        };
        if revealing {
            self.request_answer_scroll();
        } else {
            self.reset_review_scroll();
        }
        self.review.last_tick = Some(Instant::now());
        cx.notify();
    }

    pub(crate) fn timing(&self, now: i64) -> Timing {
        self.store
            .timing(now)
            .unwrap_or_else(|_| Timing::v1(now, self.collection_start))
    }
    fn refresh_previews(&mut self) {
        let Some(card) = self.review.current.as_ref() else {
            return;
        };
        let now = Self::now();
        let timing = self.timing(now);
        let last_review = self.store.last_review_secs(card.id).unwrap_or(None);
        let scheduler = self.scheduler.with_steps(&self.review.steps);
        self.review.previews =
            scheduler.preview_all_timed(card, &timing, last_review);
    }

    pub(crate) fn grade(&mut self, rating: Rating, cx: &mut Context<Self>) {
        if self.view != View::Review {
            return;
        }
        if self.review.phase != Phase::Answer {
            self.reveal(cx);
            return;
        }
        let Some(current) = self.review.current.clone() else {
            return;
        };
        let now = Instant::now();
        if self
            .review
            .last_grade_at
            .is_some_and(|last| now.saturating_duration_since(last) < DEBOUNCE)
        {
            return;
        }
        let card =
            refreshed_card(&current, |id| self.store.card(id).ok().flatten());
        let max_taken = self.store.max_taken_secs(card.deck_id).unwrap_or(60);
        let scheduler = self
            .scheduler
            .with_steps(&self.review.steps)
            .with_max_taken_secs(max_taken);
        let now_secs = Self::now();
        let timing = self.timing(now_secs);
        let last_review = self.store.last_review_secs(card.id).unwrap_or(None);
        let answer_ms = u32::try_from(self.review.focused_elapsed.as_millis())
            .unwrap_or(u32::MAX);
        let Some(decision) = scheduler.schedule_timed(
            &card,
            rating,
            &timing,
            last_review,
            answer_ms,
        ) else {
            self.show_toast(
                ToastKind::Error,
                "This card is no longer due. Refresh the collection.",
                cx,
            );
            self.review.phase = Phase::Question;
            self.reset_review_scroll();
            cx.notify();
            return;
        };
        let decision: ReviewDecision = decision;
        let before_progress = self.progress_fraction();
        if self.poll_external_change(cx) {
            return;
        }
        let log_id = match self.store.apply_review(&decision) {
            Ok(id) => id,
            Err(error) => {
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not save review: {error}"),
                    cx,
                );
                cx.notify();
                return;
            }
        };

        self.review.animation_seq = self.review.animation_seq.wrapping_add(1);
        self.record_local_write();
        self.review.answered = self.review.answered.saturating_add(1);
        if rating == Rating::Again {
            self.review.again_count = self.review.again_count.saturating_add(1);
        }
        self.review.progress_from = before_progress;
        self.review.studied = self
            .review
            .studied
            .saturating_add(self.review.focused_elapsed);
        let requeued = decision.card.queue == CardQueue::Learning;
        if !requeued {
            self.review.completed = self.review.completed.saturating_add(1);
        }
        self.review.last_grade = Some(UndoEntry {
            card,
            revlog_id: log_id,
            rating,
            spent: self.review.focused_elapsed,
            requeued,
        });
        if requeued {
            let _ = self.review.seen.remove(&decision.card.id);
            self.requeue_learning(decision.card);
        }
        self.review.focused_elapsed = Duration::ZERO;
        self.review.current = None;
        self.review.note = None;
        self.review.question = None;
        self.review.answer = None;
        self.review.last_grade_at = Some(now);
        self.review.last_tick = None;
        self.ensure_current(cx);
        cx.notify();
        if self.session_complete() {
            self.review.session_dirty = true;
            self.sync_after_session(cx);
        }
    }

    /// Fraction of the session that is finished, clamped for both ends of the
    /// queue. Repeats stay pending, so they leave the bar unchanged.
    pub(crate) fn progress_fraction(&self) -> f32 {
        session_progress(self.review.completed, self.review.session_total)
    }

    /// Repeats the session sync after undo rewrites what it may have uploaded.
    fn repeat_session_sync(&mut self, cx: &mut Context<Self>) {
        if !self.sync.busy() {
            self.sync_after_session(cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                match this.update(cx, |root, _| !root.sync.busy()) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
            let _ = this.update(cx, Self::sync_after_session);
        })
        .detach();
    }

    /// Reverses the last grade in this session: the review log is deleted, the
    /// card returns to the question it was graded from, and the session counts
    /// step back. Undo is one step deep, matching the single footer action.
    pub(crate) fn undo_grade(&mut self, cx: &mut Context<Self>) {
        if self.view != View::Review {
            return;
        }
        let Some(entry) = self.review.last_grade.clone() else {
            return;
        };
        if self.poll_external_change(cx) {
            return;
        }
        match self.store.undo_review(&entry.card, entry.revlog_id) {
            Ok(false) => {
                self.review.last_grade = None;
                self.show_toast(
                    ToastKind::Error,
                    "That review is already gone",
                    cx,
                );
                cx.notify();
            }
            Ok(true) => {
                self.record_local_write();
                let before = self.progress_fraction();
                self.review.last_grade = None;
                if self.review.session_dirty {
                    self.review.session_dirty = false;
                    self.repeat_session_sync(cx);
                }
                self.review.answered = self.review.answered.saturating_sub(1);
                if entry.rating == Rating::Again {
                    self.review.again_count =
                        self.review.again_count.saturating_sub(1);
                }
                self.review.progress_from = before;
                if let Some(current) = self.review.current.clone() {
                    let _ = self.review.seen.remove(&current.id);
                    self.review.pending.retain(|card| card.id != current.id);
                    if current.id != entry.card.id {
                        if current.queue == CardQueue::Learning {
                            Self::insert_pending(
                                &mut self.review.pending,
                                current,
                            );
                        } else {
                            self.review.queue.push_front(current);
                        }
                    }
                }
                self.review.pending.retain(|card| card.id != entry.card.id);
                if !entry.requeued {
                    self.review.completed =
                        self.review.completed.saturating_sub(1);
                }
                let _ = self.review.seen.remove(&entry.card.id);
                let Some(restored) =
                    self.store.card(entry.card.id).ok().flatten()
                else {
                    self.show_toast(
                        ToastKind::Error,
                        "Could not reload the undone card",
                        cx,
                    );
                    return;
                };
                let Some(loaded) = self.render_state(&restored, cx) else {
                    return;
                };
                self.review.animation_seq =
                    self.review.animation_seq.wrapping_add(1);
                self.apply_loaded(&restored, loaded, cx);
                self.review.last_grade_at = None;
                self.review.focused_elapsed = entry.spent;
                self.review.studied =
                    self.review.studied.saturating_sub(entry.spent);
                self.refresh_stats();
                cx.notify();
            }
            Err(error) => {
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not undo: {error}"),
                    cx,
                );
                cx.notify();
            }
        }
    }

    pub(crate) fn confirm(&mut self, cx: &mut Context<Self>) {
        self.deck_edit = None;
        let Some(confirmation) = self.confirmation.take() else {
            return;
        };
        match &confirmation {
            Confirmation::RemoveDeck { id, .. } => {
                match self.store.remove_deck(*id) {
                    Ok(()) => {
                        self.show_toast(ToastKind::Success, "Deck removed", cx);
                        self.reload(cx, false);
                    }
                    Err(error) => {
                        self.confirmation = Some(confirmation.clone());
                        self.show_toast(
                            ToastKind::Error,
                            format!("Could not remove deck: {error}"),
                            cx,
                        );
                    }
                }
            }
            Confirmation::RemoveCard { id, .. } => {
                match self.store.remove_card(*id) {
                    Ok(()) => {
                        self.clear_toast();
                        self.forget_card(*id, cx);
                        if self
                            .card_edit
                            .is_some_and(|edit| edit.card_id == *id)
                        {
                            self.discard_card_edit(cx);
                        }
                        self.browse.selected = None;
                        self.refresh_stats();
                        self.record_local_write();
                        self.refresh_browse(cx);
                        self.refresh_known_tags();
                        self.show_toast(ToastKind::Success, "Card removed", cx);
                    }
                    Err(error) => {
                        self.confirmation = Some(confirmation.clone());
                        self.show_toast(
                            ToastKind::Error,
                            format!("Could not remove card: {error}"),
                            cx,
                        );
                    }
                }
            }
            Confirmation::Reload => self.reload_collection(cx),
        }
        cx.notify();
    }

    pub(crate) fn open_create_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deck_edit = None;
        self.create_field.update(cx, TextField::clear);
        self.create_error = None;
        self.create_modal = true;
        window.focus(self.create_field.read(cx).focus_handle());
        cx.notify();
    }

    pub(crate) fn create_deck(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.create_field.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.set_create_error("Deck name cannot be empty", cx);
            return;
        }
        match self.store.create_deck(&name) {
            Ok(deck) => {
                for (index, _) in deck.name.match_indices("::") {
                    let _ = self.expanded.insert(deck.name[..index].to_owned());
                }
                self.create_field
                    .update(cx, super::text_field::TextField::clear);
                self.create_error = None;
                self.create_modal = false;
                self.clear_toast();
                self.reload(cx, false);
                self.selected =
                    self.decks.iter().position(|item| item.id == deck.id);
                self.add.deck_id = Some(deck.id);
                self.show_toast(ToastKind::Success, "Deck created", cx);
                self.persist_state(cx);
                window.focus(&self.focus);
            }
            Err(error) => {
                self.set_create_error(
                    format!("Could not create deck: {error}"),
                    cx,
                );
            }
        }
    }

    pub(crate) fn cancel_create(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_field.update(cx, TextField::clear);
        self.create_error = None;
        self.create_modal = false;
        window.focus(&self.focus);
        cx.notify();
    }

    pub(crate) fn start_rename(
        &mut self,
        deck: &Deck,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deck_edit = Some(DeckEdit { id: deck.id });
        self.rename_field
            .update(cx, |field, cx| field.set_text(deck.name.clone(), cx));
        self.rename_field.update(cx, TextField::select_all);
        window.focus(self.rename_field.read(cx).focus_handle());
        cx.notify();
    }

    pub(crate) fn apply_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.deck_edit.clone() else {
            return;
        };
        let name = self.rename_field.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.show_toast(ToastKind::Error, "Deck name cannot be empty", cx);
            cx.notify();
            return;
        }
        let old_name = self
            .decks
            .iter()
            .find(|deck| deck.id == edit.id)
            .map(|deck| deck.name.clone());
        match self.store.rename_deck(edit.id, &name) {
            Ok(_) => {
                self.deck_edit = None;
                if let Some(old) = old_name.as_deref() {
                    self.rename_expanded_paths(old, &name);
                }
                self.clear_toast();
                self.reload(cx, false);
                self.show_toast(ToastKind::Success, "Deck renamed", cx);
                self.persist_state(cx);
                window.focus(&self.focus);
            }
            Err(error) => {
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not rename deck: {error}"),
                    cx,
                );
            }
        }
    }

    fn rename_expanded_paths(&mut self, old: &str, new: &str) {
        if old == new {
            return;
        }
        let prefix = format!("{old}::");
        let mut renamed = HashSet::with_capacity(self.expanded.len());
        for path in self.expanded.drain() {
            if path == old {
                let _ = renamed.insert(new.to_owned());
            } else if let Some(rest) = path.strip_prefix(&prefix) {
                let _ = renamed.insert(format!("{new}::{rest}"));
            } else {
                let _ = renamed.insert(path);
            }
        }
        self.expanded = renamed;
    }

    /// Drops the edit draft and restores the empty add-card form.
    pub(crate) fn discard_card_edit(&mut self, cx: &mut Context<Self>) {
        if self.card_edit.take().is_none() {
            return;
        }
        self.add.tag_chips.clear();
        self.add.tags.update(cx, TextField::clear);
        self.add.occlusion = OcclusionState::default();
        self.add.errors.clear();
        self.reset_fields(cx);
    }

    /// Opens the existing note in the add-card form.
    pub(crate) fn start_card_edit(
        &mut self,
        card_id: Id,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let card = match self.store.card(card_id) {
            Ok(Some(card)) => card,
            Ok(None) => return,
            Err(error) => {
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not load card: {error}"),
                    cx,
                );
                return;
            }
        };
        let note = match self.store.note(card.note_id) {
            Ok(Some(note)) => note,
            Ok(None) => return,
            Err(error) => {
                self.show_toast(
                    ToastKind::Error,
                    format!("Could not load note: {error}"),
                    cx,
                );
                return;
            }
        };
        let Some(model_index) = self
            .models
            .iter()
            .position(|model| model.id == note.model_id)
        else {
            self.show_toast(
                ToastKind::Error,
                "This note type is not in the collection",
                cx,
            );
            return;
        };
        if self.models[model_index].is_image_occlusion() {
            self.show_toast(
                ToastKind::Error,
                "Image occlusion cards cannot be edited here",
                cx,
            );
            return;
        }
        self.discard_card_edit(cx);
        self.card_edit = Some(CardEdit {
            card_id,
            note_id: note.id,
        });
        self.add.model_index = model_index;
        self.add.deck_id = Some(card.deck_id);
        self.add.deck_picker = false;
        self.add.color_picker = None;
        self.add.errors.clear();
        self.add.tag_chips.clone_from(&note.tags);
        self.add
            .tags
            .update(cx, |field, cx| field.seed(String::new(), cx));
        self.reset_fields(cx);
        for (field, value) in self.add.fields.iter().zip(note.fields.iter()) {
            field.update(cx, |field, cx| field.seed(value.clone(), cx));
        }
        self.previous_view = View::Browse;
        self.view = View::AddCard;
        self.card_menu = None;
        window.focus(&self.focus);
        cx.notify();
        self.persist_state(cx);
    }

    /// Removes a card from a session that may still hold it in its queues.
    fn forget_card(&mut self, id: Id, cx: &mut Context<Self>) {
        self.review.queue.retain(|card| card.id != id);
        self.review.pending.retain(|card| card.id != id);
        self.review.seen.remove(&id);
        let was_current = self
            .review
            .current
            .as_ref()
            .is_some_and(|card| card.id == id);
        if was_current {
            self.review.current = None;
            self.review.note = None;
            self.review.question = None;
            self.review.answer = None;
            self.review.phase = Phase::Question;
            self.ensure_current(cx);
        }
    }

    /// Brings the first invalid control into view and focuses it.
    fn reveal_first_invalid(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_scroll.set_offset(point(px(0.), px(0.)));
        let field = if let Some(key) =
            self.add.errors.keys().find(|key| key.starts_with("field:"))
        {
            key.strip_prefix("field:")
                .and_then(|index| index.parse::<usize>().ok())
        } else if self.add.errors.contains_key("fields")
            || self.add.errors.contains_key("model")
        {
            Some(0)
        } else if self.add.errors.contains_key("occlusion")
            || self.add.errors.contains_key("masks")
        {
            Some(2)
        } else {
            None
        };
        if let Some(index) = field
            && let Some(handle) = self.add.fields.get(index)
        {
            let handle = handle.read(cx).focus_handle().clone();
            window.focus(&handle);
            return;
        }
        if self.add.errors.contains_key("deck") {
            self.add.deck_picker = true;
            let filter = self.add.deck_filter.read(cx).focus_handle().clone();
            window.focus(&filter);
            return;
        }
        window.focus(&self.focus);
    }

    pub(crate) fn save_note(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view != View::AddCard {
            return;
        }
        let deck_index = self.add_deck_index();
        let Some(deck) =
            deck_index.and_then(|index| self.decks.get(index)).cloned()
        else {
            self.add
                .errors
                .insert("deck".into(), "Choose a deck first".into());
            self.reveal_first_invalid(window, cx);
            cx.notify();
            return;
        };
        let Some(model) = self.models.get(self.add.model_index).cloned() else {
            self.add
                .errors
                .insert("model".into(), "Choose a note type first".into());
            self.reveal_first_invalid(window, cx);
            cx.notify();
            return;
        };
        let fields = if model.is_image_occlusion() {
            self.occlusion_fields(cx)
        } else {
            self.add
                .fields
                .iter()
                .map(|field| field.read(cx).text())
                .collect::<Vec<_>>()
        };
        let mut tags = self.add.tag_chips.clone();
        for tag in self.add.tags.read(cx).text().split_whitespace() {
            if !tags.iter().any(|known| known == tag) {
                tags.push(tag.to_owned());
            }
        }
        let errors = if model.is_image_occlusion() {
            validate_occlusion(
                &deck,
                &model,
                self.add.occlusion.image.as_deref(),
                &self.add.occlusion.masks,
            )
        } else {
            validate_draft(&deck, &model, &fields)
        };
        self.add.errors = errors;
        if !self.add.errors.is_empty() {
            self.reveal_first_invalid(window, cx);
            cx.notify();
            return;
        }
        let edit = self.card_edit;
        if self.poll_external_change(cx) {
            return;
        }
        if let Some(edit) = edit {
            let mut saved = match self.store.note(edit.note_id) {
                Ok(Some(note)) => note,
                Ok(None) => {
                    self.show_toast(
                        ToastKind::Error,
                        "That card no longer exists",
                        cx,
                    );
                    return;
                }
                Err(error) => {
                    self.show_toast(
                        ToastKind::Error,
                        format!("Could not load card: {error}"),
                        cx,
                    );
                    return;
                }
            };
            saved.model_id = model.id;
            saved.fields = fields;
            saved.tags = tags;
            match self.store.update_note(&saved, deck.id) {
                Ok(()) => {
                    self.clear_toast();
                    self.card_edit = None;
                    self.add.tag_chips.clear();
                    self.add.tags.update(cx, TextField::clear);
                    self.add.occlusion = OcclusionState::default();
                    self.add.errors.clear();
                    self.reset_fields(cx);
                    self.refresh_stats();
                    self.record_local_write();
                    self.refresh_browse(cx);
                    self.refresh_known_tags();
                    self.view = View::Browse;
                    self.show_toast(ToastKind::Success, "Card updated", cx);
                }
                Err(error) => self.show_toast(
                    ToastKind::Error,
                    format!("Could not update card: {error}"),
                    cx,
                ),
            }
        } else {
            let note = NewNote {
                model_id: model.id,
                deck_id: deck.id,
                fields,
                tags,
            };
            match self.store.add_note(&note) {
                Ok(ids) => {
                    let count = ids.len();
                    self.clear_toast();
                    for field in &self.add.fields {
                        field.update(cx, |field, cx| {
                            field.seed("", cx);
                        });
                    }
                    self.add.tags.update(cx, |field, cx| {
                        field.seed("", cx);
                    });
                    self.add.errors.clear();
                    self.add.tag_chips.clear();
                    self.refresh_stats();
                    self.add.occlusion = OcclusionState::default();
                    self.record_local_write();
                    self.refresh_known_tags();
                    self.show_toast(
                        ToastKind::Success,
                        format!(
                            "Added {count} {}",
                            if count == 1 { "card" } else { "cards" }
                        ),
                        cx,
                    );
                }
                Err(error) => {
                    if matches!(
                        &error,
                        ranki_model::StoreError::InvalidData(message)
                            if message.eq_ignore_ascii_case("duplicate")
                    ) {
                        self.add.errors.insert(
                            "field:0".into(),
                            "A note with this first field already exists"
                                .into(),
                        );
                        self.reveal_first_invalid(window, cx);
                    } else {
                        self.show_toast(
                            ToastKind::Error,
                            format!("Could not add note: {error}"),
                            cx,
                        );
                    }
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn rebuild_deck_rows(&mut self) {
        self.deck_rows =
            build_deck_rows(&self.decks, &self.stats, &self.expanded);
        prepare_deck_rows(&self.decks, &mut self.deck_rows);
        self.deck_rows_rev = self.deck_rows_rev.wrapping_add(1);
        self.leaf_totals = leaf_deck_totals(&self.decks, &self.stats);
    }

    pub(crate) fn refresh_visible_deck_rows(&mut self, cx: &App) {
        let query = self.deck_search.read(cx).text();
        if self.visible_rows_key.0 == query
            && self.visible_rows_key.1 == self.deck_rows_rev
        {
            return;
        }
        self.visible_rows = compute_visible_deck_rows(
            &self.decks,
            &self.stats,
            &self.deck_rows,
            &query,
        );
        self.visible_rows_key = (query, self.deck_rows_rev);
    }

    pub(crate) fn toggle_deck(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            let _ = self.expanded.insert(path);
        }
        self.deck_menu = None;
        self.rebuild_deck_rows();
        cx.notify();
        self.persist_state(cx);
    }

    pub(crate) fn tick_focus(&mut self, window: &mut Window) {
        let now = Instant::now();
        let active = window.is_window_active()
            && self.view == View::Review
            && self.review.current.is_some();
        let Some(last) = self.review.last_tick else {
            self.review.last_tick = Some(now);
            return;
        };
        let elapsed = now.saturating_duration_since(last);
        self.review.focused_elapsed =
            focused_delta(elapsed, active, self.review.focused_elapsed);
        self.review.last_tick = Some(now);
    }

    /// A graded card unmounts its rating button; keep keys on the app.
    fn restore_review_focus(&self, window: &mut Window) {
        if self.view == View::Review
            && self
                .review_button_focus
                .borrow()
                .iter()
                .any(|handle| handle.is_focused(window))
        {
            window.focus(&self.focus);
        }
    }

    pub(crate) fn dispatch(
        &mut self,
        action: UiAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tick_focus(window);
        if self.card_menu.take().is_some() {
            self.card_menu_dismissed_press = None;
            cx.notify();
        }
        if self.deck_menu.take().is_some() {
            self.deck_menu_dismissed_press = None;
            cx.notify();
        }
        let had_dialog = self.confirmation.is_some();
        // A focused review button handles Enter and Space itself. The root
        // reveal must not run a second action for the same keystroke.
        let button_focused = self
            .review_button_focus
            .borrow()
            .iter()
            .any(|handle| handle.is_focused(window));
        match action {
            UiAction::Reveal => {
                if !button_focused {
                    self.reveal(cx);
                }
            }
            UiAction::Space => {
                let caught_up = self.view == View::Review
                    && self.review.current.is_none()
                    && (self.session_complete()
                        || (self.review.answered == 0
                            && self.review.load_errors == 0
                            && self.review.load_error.is_none()));
                if !button_focused
                    && caught_up
                    && self
                        .review
                        .last_grade_at
                        .is_none_or(|last| last.elapsed() >= DEBOUNCE)
                {
                    self.end_session(cx);
                } else if !button_focused {
                    self.reveal(cx);
                }
            }
            UiAction::Grade(rating) => {
                self.grade(rating, cx);
                self.restore_review_focus(window);
            }
            UiAction::Retry => {
                self.toggle_reveal(cx);
                self.restore_review_focus(window);
            }
            UiAction::Reload => self.request_reload(window, cx),
            UiAction::RetryStatistics => {
                if self.view == View::Statistics {
                    self.schedule_statistics_refresh(cx);
                    cx.notify();
                }
            }
            UiAction::Undo => {
                self.undo_grade(cx);
                self.restore_review_focus(window);
            }
            UiAction::StudyAgain => {
                if self.view == View::Review
                    && self.review.current.is_none()
                    && (self.review.answered > 0
                        || self.review.load_errors > 0
                        || self.review.load_error.is_some())
                {
                    self.study_again(window, cx);
                }
            }
            UiAction::EndSession => {
                if self.confirmation.is_some()
                    || self.help_open
                    || self.create_modal
                {
                    self.confirmation = None;
                    self.help_open = false;
                    if self.create_modal {
                        self.cancel_create(window, cx);
                    }
                    cx.notify();
                } else if self.view == View::Review {
                    self.end_session(cx);
                } else if self.view == View::AddCard
                    && (self.add.deck_picker || self.add.color_picker.is_some())
                {
                    self.add.deck_picker = false;
                    window.focus(&self.focus);
                    self.close_color_picker(window, cx);
                    cx.notify();
                } else if self.view == View::AddCard
                    && (self.add.tag_suggestions_dismissed
                        || self.tag_suggestions(cx).is_empty())
                {
                    self.dispatch(UiAction::CancelAdd, window, cx);
                } else if self.deck_edit.take().is_some() {
                    window.focus(&self.focus);
                    cx.notify();
                }
            }
            UiAction::OpenDecks => {
                self.deck_edit = None;
                self.view = next_view(self.view, View::Decks);
                self.refresh_stats();
                window.focus(&self.focus);
                cx.notify();
            }
            UiAction::OpenStatistics => {
                self.deck_edit = None;
                self.view = next_view(self.view, View::Statistics);
                self.heatmap_hover = None;
                self.refresh_stats();
                self.schedule_statistics_refresh(cx);
                self.statistics_ready = true;
                cx.notify();
            }
            UiAction::OpenBrowse => {
                self.deck_edit = None;
                self.view = View::Browse;
                let field = self.browse.search.clone();
                self.refresh_browse(cx);
                self.browse_ready = true;
                cx.notify();
                window.defer(cx, move |window, cx| {
                    window.focus(field.read(cx).focus_handle());
                });
            }
            UiAction::OpenAdd => {
                self.deck_edit = None;
                self.discard_card_edit(cx);
                if self.view != View::AddCard {
                    if let Some(deck) =
                        self.selected.and_then(|index| self.decks.get(index))
                    {
                        self.add.deck_id = Some(deck.id);
                    }
                    self.add.deck_picker = false;
                    self.add.color_picker = None;
                    self.previous_view = self.view;
                }
                self.view = next_view(self.view, View::AddCard);
                self.add.errors.clear();
                window.focus(&self.focus);
                cx.notify();
                let first = if self
                    .models
                    .get(self.add.model_index)
                    .is_some_and(NoteModel::is_image_occlusion)
                {
                    2
                } else {
                    0
                };
                let field = self.add.fields.get(first).cloned();
                window.defer(cx, move |window, cx| {
                    if let Some(field) = field {
                        window.focus(field.read(cx).focus_handle());
                    }
                });
            }
            UiAction::OpenSettings => {
                self.deck_edit = None;
                self.view = next_view(self.view, View::Settings);
                window.focus(&self.settings_focus);
                cx.notify();
            }
            UiAction::SetTheme(choice) => {
                self.set_theme(choice, cx);
            }
            UiAction::SetReduceMotion(choice) => {
                if self.view == View::Settings {
                    window.focus(&self.settings_focus);
                }
                self.set_reduce_motion(choice, cx);
            }
            UiAction::CreateDeck => self.open_create_modal(window, cx),
            UiAction::SubmitDeck => {
                if self.create_modal {
                    self.create_deck(window, cx);
                } else if self.view == View::AddCard
                    && self.add.deck_picker
                    && let Some(index) =
                        self.add_deck_matches(cx).into_iter().next()
                {
                    self.select_add_deck(index, cx);
                    window.focus(&self.focus);
                } else if self.view == View::Decks && self.deck_edit.is_some() {
                    self.apply_rename(window, cx);
                }
            }
            UiAction::SaveNote => self.save_note(window, cx),
            UiAction::CancelAdd => {
                self.discard_card_edit(cx);
                self.view = next_view(self.view, self.previous_view);
                self.add.errors.clear();
                cx.notify();
            }
            UiAction::ToggleHelp => {
                self.help_open = !self.help_open;
                cx.notify();
            }
            UiAction::Confirm => {
                if self.confirmation.is_some() {
                    self.confirm(cx);
                } else if self.create_modal {
                    self.create_deck(window, cx);
                }
            }
            UiAction::CancelDialog => {
                self.confirmation = None;
                self.close_color_picker(window, cx);
                let leaving_rename = self.deck_edit.take().is_some();
                if self.help_open {
                    self.help_open = false;
                    let focus = self.focus.clone();
                    window.focus(&focus);
                    cx.defer_in(window, move |_, window, _| {
                        window.focus(&focus);
                    });
                }
                if self.create_modal {
                    self.cancel_create(window, cx);
                } else {
                    cx.notify();
                }
                if leaving_rename {
                    window.focus(&self.focus);
                }
            }
            UiAction::NextDeck => {
                self.refresh_stats();
                let next =
                    next_due_deck(&self.decks, &self.stats, self.selected);
                if let Some(index) = next {
                    self.study_deck(index, window, cx);
                }
            }
            UiAction::FocusDeckSearch => {
                self.view = View::Decks;
                let field = self.deck_search.clone();
                cx.notify();
                window.defer(cx, move |window, cx| {
                    window.focus(field.read(cx).focus_handle());
                });
            }
            UiAction::ClearSearch => {
                self.deck_search
                    .update(cx, super::text_field::TextField::clear);
                window.focus(self.deck_search.read(cx).focus_handle());
                self.view = View::Decks;
                cx.notify();
            }
            UiAction::ClearBrowseSearch => {
                self.browse
                    .search
                    .update(cx, super::text_field::TextField::clear);
                self.refresh_browse(cx);
                window.focus(self.browse.search.read(cx).focus_handle());
                cx.notify();
            }
            UiAction::BrowseUp => {
                if self.view == View::Browse {
                    self.move_browse_selection(-1, cx);
                }
            }
            UiAction::BrowseDown => {
                if self.view == View::Browse {
                    self.move_browse_selection(1, cx);
                }
            }
            UiAction::SyncSignIn => self.sign_in(cx),
            UiAction::SyncSignOut => self.sign_out(cx),
            UiAction::SyncNow => self.sync_now(cx),
            UiAction::ToggleAutoSync => self.toggle_auto_sync(cx),
            UiAction::SyncKeepLocal => self.resolve_sync_conflict(true, cx),
            UiAction::SyncKeepRemote => self.resolve_sync_conflict(false, cx),
        }
        if self.view == View::Review
            && self.confirmation.is_none()
            && !self.create_modal
            && !matches!(action, UiAction::Reveal | UiAction::Space)
        {
            window.focus(&self.focus);
        }
        if self.state_snapshot(cx) != self.options.ui_state {
            self.persist_state(cx);
        }
        if self.confirmation.is_some() != had_dialog {
            if self.confirmation.is_some() {
                window.focus(&self.dialog_focus);
            } else {
                window.focus(&self.focus);
            }
        }
    }
}

impl Render for Root {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        self.tick_focus(window);

        let view = self.view;
        if view == View::Decks {
            self.refresh_visible_deck_rows(cx);
        }
        if view == View::Browse && !self.browse_ready {
            self.refresh_browse(cx);
            self.browse_ready = true;
        }
        if view == View::Statistics && !self.statistics_ready {
            self.refresh_statistics(cx);
            self.statistics_ready = true;
        }
        let colors = self.colors();
        let main = match view {
            View::Decks => self.render_decks(window, cx).into_any_element(),
            View::Browse => self.render_browse(window, cx).into_any_element(),
            View::Review => self.render_review(window, cx).into_any_element(),
            View::AddCard => {
                self.render_add_card(window, cx).into_any_element()
            }
            View::Settings => self.render_settings().into_any_element(),
            View::Statistics => self
                .render_statistics_with_error(
                    window,
                    self.statistics_error.as_deref(),
                    cx,
                )
                .into_any_element(),
        };
        let mobile_nav = view != View::Review
            && theme::is_mobile_width(f32::from(window.viewport_size().width));
        let root_base = div()
            .id("root")
            .size_full()
            .flex()
            .key_context(if view == View::AddCard {
                "ranki ranki_occlusion"
            } else {
                view.key_context()
            })
            .bg(colors.base)
            .text_color(colors.text)
            .font_family(self.font_family)
            .track_focus(&self.focus)
            .capture_any_mouse_down(cx.listener(
                |this, event: &MouseDownEvent, window, cx| {
                    if this.pointer_on_scrollbar(event, window) {
                        return;
                    }
                    let menu = this.deck_menu_dismissed_press.take().is_some()
                        || this.card_menu_dismissed_press.take().is_some();
                    let deck =
                        this.add.deck_picker_dismissed_press.take().is_some();
                    let color =
                        this.add.color_picker_dismissed_press.take().is_some();
                    let _ = this.add.color_picker_dismissed_target.take();
                    let picker = deck || color;
                    if menu || picker {
                        cx.notify();
                    }
                },
            ))
            .on_mouse_move(cx.listener(
                |this, event: &MouseMoveEvent, window, cx| {
                    this.drag_scrollbar(f32::from(event.position.y), window);
                    if event.dragging() {
                        this.drag_nav(f32::from(event.position.x), cx);
                    }
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    let scrollbar = this.end_scrollbar_drag(cx);
                    this.end_nav_drag(cx);
                    let menu = this.deck_menu_dismissed_press.take().is_some()
                        || this.card_menu_dismissed_press.take().is_some();
                    let picker =
                        this.add.deck_picker_dismissed_press.take().is_some();
                    let color =
                        this.add.color_picker_dismissed_press.take().is_some();
                    let _ = this.add.color_picker_dismissed_target.take();
                    let picker = picker || color;
                    if menu || picker || scrollbar {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.end_scrollbar_drag(cx);
                    this.end_nav_drag(cx);
                }),
            )
            .on_action(cx.listener(|this, action: &Reveal, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &Space, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &GradeAgain, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &GradeHard, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &GradeGood, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &GradeEasy, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &Retry, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &Reload, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &Undo, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|_, _: &NextField, window, _| {
                window.focus_next();
            }))
            .on_action(cx.listener(|_, _: &PreviousField, window, _| {
                window.focus_prev();
            }))
            .on_action(cx.listener(|this, action: &StudyAgain, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &EndSession, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &OpenDecks, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(
                |this, action: &OpenStatistics, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(|this, _: &RetryStatistics, window, cx| {
                this.dispatch(UiAction::RetryStatistics, window, cx);
            }))
            .on_action(cx.listener(|this, action: &OpenBrowse, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &OpenAdd, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(
                |this, action: &OpenSettings, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, action: &UseLightTheme, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, action: &UseDarkTheme, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, action: &UseSystemTheme, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(|this, action: &CreateDeck, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &SubmitDeck, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &SaveNote, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, _: &AddOcclusionMask, _, cx| {
                let occlusion_model = this
                    .models
                    .get(this.add.model_index)
                    .is_some_and(ranki_model::NoteModel::is_image_occlusion);
                if this.view == View::AddCard
                    && occlusion_model
                    && this.add.occlusion.image.is_some()
                {
                    this.add_default_occlusion_mask(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RemoveOcclusionMask, _, cx| {
                if this.view == View::AddCard {
                    this.remove_last_occlusion_mask(cx);
                }
            }))
            .on_action(cx.listener(
                |this, action: &UseReducedMotion, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, action: &UseFullMotion, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, action: &UseSystemMotion, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(|this, action: &CancelAdd, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &ToggleHelp, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(
                |this, action: &ConfirmDialog, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, action: &CancelDialog, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(|this, action: &ClearSearch, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(
                |this, action: &FocusDeckSearch, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(
                |this, action: &ClearBrowseSearch, window, cx| {
                    this.dispatch(UiAction::from(action), window, cx);
                },
            ))
            .on_action(cx.listener(|this, action: &BrowseUp, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &BrowseDown, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, action: &NextDeck, window, cx| {
                this.dispatch(UiAction::from(action), window, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncSignIn, window, cx| {
                this.dispatch(UiAction::SyncSignIn, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncSignOut, window, cx| {
                this.dispatch(UiAction::SyncSignOut, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncNow, window, cx| {
                this.dispatch(UiAction::SyncNow, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleAutoSync, window, cx| {
                this.dispatch(UiAction::ToggleAutoSync, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncKeepLocal, window, cx| {
                this.dispatch(UiAction::SyncKeepLocal, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SyncKeepRemote, window, cx| {
                this.dispatch(UiAction::SyncKeepRemote, window, cx);
            }));
        let nav = if view == View::Review {
            None
        } else {
            Some(self.render_nav(window, cx).into_any_element())
        };
        let page = div()
            .flex_1()
            .overflow_hidden()
            .when(mobile_nav, |this| this.pb(px(theme::MOBILE_NAV_HEIGHT)))
            .child(main);
        let overlays = self.render_overlay(window, cx);
        let root = if mobile_nav {
            root_base.child(page).children(nav).children(overlays)
        } else {
            root_base.children(nav).child(page).children(overlays)
        };
        let bars: Vec<_> = self
            .active_scrollbars(window)
            .into_iter()
            .map(|(id, handle)| (id, handle.clone()))
            .collect();
        for (id, handle) in &bars {
            self.scrollbar_layouts.insert(
                *id,
                (handle.bounds(), f32::from(handle.max_offset().height)),
            );
        }
        if !bars.is_empty() {
            let weak = cx.entity().downgrade();
            window.on_next_frame(move |_, app_cx| {
                let _ = weak.update(app_cx, |this, cx| {
                    let mut changed = false;
                    for (id, handle) in bars {
                        let layout = (
                            handle.bounds(),
                            f32::from(handle.max_offset().height),
                        );
                        if this.scrollbar_layouts.get(id) != Some(&layout) {
                            this.scrollbar_layouts.insert(id, layout);
                            changed = true;
                        }
                    }
                    if changed {
                        cx.notify();
                    }
                });
            });
        }
        root
    }
}

/// Adds one deck's counts onto another, saturating at the type limits.
fn add_deck_stats(total: &mut DeckStats, counts: DeckStats) {
    total.new_cards = total.new_cards.saturating_add(counts.new_cards);
    total.learning_cards =
        total.learning_cards.saturating_add(counts.learning_cards);
    total.review_cards = total.review_cards.saturating_add(counts.review_cards);
    total.total_cards = total.total_cards.saturating_add(counts.total_cards);
}

/// Sums only decks that are not prefixes of another deck.
#[must_use]
pub fn leaf_deck_totals(decks: &[Deck], stats: &[DeckStats]) -> DeckStats {
    let names = decks
        .iter()
        .map(|deck| deck.name.as_str())
        .collect::<HashSet<_>>();
    let mut total = DeckStats::default();
    for (index, deck) in decks.iter().enumerate() {
        let is_nested = deck
            .name
            .match_indices("::")
            .any(|(index, _)| names.contains(&deck.name[..index]));
        if is_nested {
            continue;
        }
        let Some(stat) = stats.get(index) else {
            continue;
        };
        total.new_cards = total.new_cards.saturating_add(stat.new_cards);
        total.learning_cards =
            total.learning_cards.saturating_add(stat.learning_cards);
        total.review_cards =
            total.review_cards.saturating_add(stat.review_cards);
        total.total_cards = total.total_cards.saturating_add(stat.total_cards);
    }
    total
}

/// One deck tree node while rows are built.
struct DeckNode<'a> {
    path: String,
    deck: Option<&'a Deck>,
    own: DeckStats,
    total: DeckStats,
    parent: Option<usize>,
}

/// Builds deck rows with virtual parents and stable depth-first ordering.
#[must_use]
pub fn build_deck_rows(
    decks: &[Deck],
    stats: &[DeckStats],
    expanded: &HashSet<String>,
) -> Vec<DeckRow> {
    let capacity = decks.len().saturating_mul(2);
    let mut nodes: Vec<DeckNode<'_>> = Vec::with_capacity(capacity);
    let mut positions: HashMap<String, usize> =
        HashMap::with_capacity(capacity);
    for (deck, stat) in decks.iter().zip(stats.iter()) {
        let path = deck.name.as_str();
        if path.is_empty() {
            continue;
        }
        let mut prefix_end = 0usize;
        let mut segments = path.split("::").peekable();
        while let Some(segment) = segments.next() {
            if prefix_end > 0 {
                prefix_end = prefix_end.saturating_add(2);
            }
            prefix_end = prefix_end.saturating_add(segment.len());
            let Some(prefix) = path.get(..prefix_end) else {
                break;
            };
            let leaf = segments.peek().is_none();
            match positions.get(prefix).copied() {
                Some(position) if leaf => {
                    let placeholder = nodes
                        .get(position)
                        .is_some_and(|node| node.deck.is_none());
                    if placeholder {
                        if let Some(node) = nodes.get_mut(position) {
                            node.deck = Some(deck);
                            node.own = *stat;
                        }
                    } else {
                        nodes.push(DeckNode {
                            path: prefix.to_owned(),
                            deck: Some(deck),
                            own: *stat,
                            total: DeckStats::default(),
                            parent: None,
                        });
                    }
                }
                Some(_) => {}
                None => {
                    let position = nodes.len();
                    nodes.push(DeckNode {
                        path: prefix.to_owned(),
                        deck: leaf.then_some(deck),
                        own: if leaf { *stat } else { DeckStats::default() },
                        total: DeckStats::default(),
                        parent: None,
                    });
                    let _ = positions.insert(prefix.to_owned(), position);
                }
            }
        }
    }
    nodes.sort_by(|left, right| left.path.cmp(&right.path));
    positions.clear();
    for (position, node) in nodes.iter().enumerate() {
        let _ = positions.insert(node.path.clone(), position);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (position, node) in nodes.iter_mut().enumerate() {
        let parent = node
            .path
            .rsplit_once("::")
            .and_then(|(parent, _)| positions.get(parent).copied());
        node.parent = parent;
        match parent.and_then(|parent| children.get_mut(parent)) {
            Some(list) => list.push(position),
            None => roots.push(position),
        }
    }
    for position in (0..nodes.len()).rev() {
        let Some(node) = nodes.get(position) else {
            continue;
        };
        let total = if node.deck.is_some() {
            node.own
        } else {
            node.total
        };
        let parent = node.parent;
        if let Some(node) = nodes.get_mut(position) {
            node.total = total;
        }
        if let Some(node) = parent.and_then(|parent| nodes.get_mut(parent)) {
            add_deck_stats(&mut node.total, total);
        }
    }
    let mut rows = Vec::with_capacity(nodes.len());
    let mut stack = roots
        .iter()
        .rev()
        .map(|position| (*position, 0usize))
        .collect::<Vec<_>>();
    while let Some((position, depth)) = stack.pop() {
        let Some(node) = nodes.get(position) else {
            continue;
        };
        let has_children =
            children.get(position).is_some_and(|list| !list.is_empty());
        let name = node
            .path
            .rsplit("::")
            .next()
            .unwrap_or(&node.path)
            .to_owned();
        let open = expanded.contains(&node.path);
        rows.push(DeckRow {
            id: node.deck.map(|deck| deck.id),
            path: node.path.clone(),
            name,
            depth,
            stats: node.total,
            virtual_group: node.deck.is_none(),
            has_children,
            expanded: open,
            meta: String::new(),
            child_count: 0,
            deck_index: None,
            key: SharedString::default(),
            chevron_key: SharedString::default(),
            study_key: SharedString::default(),
            menu_key: SharedString::default(),
        });
        if open && let Some(list) = children.get(position) {
            for child in list.iter().rev() {
                stack.push((*child, depth.saturating_add(1)));
            }
        }
    }
    rows
}

/// Fills the row fields that depend on the finished tree, not on each frame.
pub fn prepare_deck_rows(decks: &[Deck], rows: &mut [DeckRow]) {
    let by_path = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.path.clone(), index))
        .collect::<HashMap<_, _>>();
    let by_id = decks
        .iter()
        .enumerate()
        .map(|(index, deck)| (deck.id, index))
        .collect::<HashMap<_, _>>();
    let mut children = vec![0usize; rows.len()];
    for row in rows.iter() {
        if let Some((parent, _)) = row.path.rsplit_once("::")
            && let Some(&index) = by_path.get(parent)
        {
            children[index] = children[index].saturating_add(1);
        }
    }
    for (row, count) in rows.iter_mut().zip(children) {
        row.child_count = count;
        row.meta = deck_meta_counts(count, row.stats.total_cards);
        row.deck_index = row.id.and_then(|id| by_id.get(&id).copied());
        row.key = SharedString::from(format!("deck-row-{}", row.path));
        row.chevron_key =
            SharedString::from(format!("decks-chevron-{}", row.path));
        row.study_key = SharedString::from(format!("decks-study-{}", row.path));
        row.menu_key =
            SharedString::from(format!("decks-menu-{}", row.id.unwrap_or(0)));
    }
}

#[must_use]
pub const fn due_total(stats: DeckStats) -> usize {
    stats
        .new_cards
        .saturating_add(stats.learning_cards)
        .saturating_add(stats.review_cards)
}

/// Total cards in the forecast window.
#[must_use]
pub fn forecast_total(counts: &[usize]) -> usize {
    counts
        .iter()
        .fold(0_usize, |total, count| total.saturating_add(*count))
}

/// Height of one forecast bar. Zero days keep a small baseline.
#[must_use]
pub fn forecast_bar_height(count: usize, peak: usize) -> f32 {
    if count == 0 || peak == 0 {
        return 6.0;
    }
    18.0 + (count as f32 / peak as f32) * 42.0
}

/// Graded share of the session. An empty queue is never full.
#[must_use]
pub fn session_progress(answered: usize, total: usize) -> f32 {
    if total == 0 {
        return 0.0;
    }
    let answered = u32::try_from(answered).unwrap_or(u32::MAX);
    let total = u32::try_from(total).unwrap_or(u32::MAX);
    (answered as f32 / total as f32).clamp(0.0, 1.0)
}

/// Remaining cards in the session, split by queue. Learning cards waiting for
/// their next step and the current card count as remaining, so the header never
/// claims a card is done while it will appear again.
#[must_use]
pub fn queue_counts(
    current: Option<&Card>,
    queue: &VecDeque<Card>,
    pending: &VecDeque<Card>,
) -> DeckStats {
    let mut stats = DeckStats::default();
    for card in current
        .into_iter()
        .chain(queue.iter())
        .chain(pending.iter())
    {
        stats.total_cards = stats.total_cards.saturating_add(1);
        match card.card_type {
            CardType::New => {
                stats.new_cards = stats.new_cards.saturating_add(1);
            }
            CardType::Learning | CardType::Relearning => {
                stats.learning_cards = stats.learning_cards.saturating_add(1);
            }
            CardType::Review => {
                stats.review_cards = stats.review_cards.saturating_add(1);
            }
        }
    }
    stats
}

/// Study time for the session summary: seconds under a minute, then minutes
/// with a seconds remainder, then whole hours.
#[must_use]
pub fn duration_label(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3_600 {
        return format!("{}m {}s", seconds / 60, seconds % 60);
    }
    format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60)
}

/// Sums only real leaf decks so parents never double count.
#[must_use]
#[cfg(test)]
pub fn sum_leaf_counts(rows: &[DeckRow]) -> DeckStats {
    let mut total = DeckStats::default();
    for row in rows {
        if row.has_children || row.virtual_group {
            continue;
        }
        total.new_cards = total.new_cards.saturating_add(row.stats.new_cards);
        total.learning_cards = total
            .learning_cards
            .saturating_add(row.stats.learning_cards);
        total.review_cards =
            total.review_cards.saturating_add(row.stats.review_cards);
        total.total_cards =
            total.total_cards.saturating_add(row.stats.total_cards);
    }
    total
}

/// Matches `needle` inside `haystack` the way a lowercased search would.
pub fn contains_lowercase(haystack: &str, needle: &str) -> bool {
    if haystack.is_ascii() && needle.is_ascii() {
        let window = needle.len();
        return window > 0
            && haystack
                .as_bytes()
                .windows(window)
                .any(|part| part.eq_ignore_ascii_case(needle.as_bytes()));
    }
    haystack.to_lowercase().contains(needle)
}

/// Keeps a matching row and every ancestor of a match.
#[must_use]
pub fn filter_decks(rows: &[DeckRow], query: &str) -> Vec<DeckRow> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    let mut keep = HashSet::new();
    for row in rows {
        if contains_lowercase(&row.path, &needle) {
            let _ = keep.insert(row.path.as_str());
            for (index, _) in row.path.match_indices("::") {
                let _ = keep.insert(&row.path[..index]);
            }
        }
    }
    rows.iter()
        .filter(|row| keep.contains(row.path.as_str()))
        .cloned()
        .collect()
}

/// Rows shown in the deck table for the current search query.
#[must_use]
fn compute_visible_deck_rows(
    decks: &[Deck],
    stats: &[DeckStats],
    rows: &[DeckRow],
    query: &str,
) -> Vec<DeckRow> {
    if query.trim().is_empty() {
        return rows.to_vec();
    }
    let mut expanded = decks
        .iter()
        .map(|deck| deck.name.clone())
        .collect::<HashSet<_>>();
    for deck in decks {
        for (index, _) in deck.name.match_indices("::") {
            let _ = expanded.insert(deck.name[..index].to_owned());
        }
    }
    let mut rows = build_deck_rows(decks, stats, &expanded);
    prepare_deck_rows(decks, &mut rows);
    filter_decks(&rows, query)
}

/// Resolves a note-type tab to a model from the store.
#[must_use]
pub fn resolve_model(models: &[NoteModel], index: usize) -> Option<&NoteModel> {
    models.get(index)
}

/// Short label for a note-type tab.
#[must_use]
pub fn note_type_name(model: &NoteModel) -> String {
    let name = model.name.to_lowercase();
    if model.is_image_occlusion() || name.contains("occlusion") {
        String::from("Image Occlusion")
    } else if name.contains("cloze") {
        String::from("Cloze")
    } else if name.contains("reversed") {
        String::from("Reversed")
    } else if name.starts_with("basic") {
        String::from("Basic")
    } else {
        model.name.clone()
    }
}

/// Inline add-card validation.
#[must_use]
pub fn validate_draft(
    deck: &Deck,
    model: &NoteModel,
    fields: &[String],
) -> BTreeMap<String, String> {
    let mut errors = BTreeMap::new();
    if deck.name.trim().is_empty() || deck.id <= 0 {
        let _ =
            errors.insert("deck".to_owned(), "Choose a deck first".to_owned());
    }
    if model.id <= 0 {
        let _ = errors.insert(
            "model".to_owned(),
            "This note type is not in the collection".to_owned(),
        );
    }
    if fields.is_empty() || fields.iter().all(|value| value.trim().is_empty()) {
        let _ = errors.insert(
            "field:0".to_owned(),
            "Enter text in at least one field".to_owned(),
        );
    }
    if model.is_cloze()
        && fields.iter().any(|value| !value.trim().is_empty())
        && !fields.iter().any(|value| {
            ranki_model::cloze_ordinals(std::slice::from_ref(value))
                .iter()
                .any(|ordinal| *ordinal < 500)
        })
    {
        let _ = errors.insert(
            "fields".to_owned(),
            "Cloze notes need a {{c1::answer}} marker".to_owned(),
        );
    }
    if model.card_ordinals(fields).is_empty() {
        let _ = errors.insert(
            "fields".to_owned(),
            "This note type would not create any cards".to_owned(),
        );
    }
    errors
}

/// Inline validation for the image-occlusion editor.
#[must_use]
pub fn validate_occlusion(
    deck: &Deck,
    model: &NoteModel,
    image: Option<&str>,
    masks: &[OcclusionMask],
) -> BTreeMap<String, String> {
    let mut errors = BTreeMap::new();
    if deck.name.trim().is_empty() || deck.id <= 0 {
        let _ =
            errors.insert("deck".to_owned(), "Choose a deck first".to_owned());
    }
    if model.id <= 0 {
        let _ = errors.insert(
            "model".to_owned(),
            "This note type is not in the collection".to_owned(),
        );
    }
    if image.is_none_or(str::is_empty) {
        let _ = errors
            .insert("occlusion".to_owned(), "Upload an image first".to_owned());
    }
    if masks.is_empty() {
        let _ = errors.insert(
            "masks".to_owned(),
            "Draw at least one mask on the image".to_owned(),
        );
    }
    errors
}

/// Index of the note type with this name, used after a reload.
#[must_use]
pub fn model_index(models: &[NoteModel], name: &str) -> Option<usize> {
    models
        .iter()
        .position(|model| model.name.eq_ignore_ascii_case(name))
}

#[must_use]
pub fn next_due_deck(
    decks: &[Deck],
    stats: &[DeckStats],
    selected: Option<usize>,
) -> Option<usize> {
    let current = selected.unwrap_or(usize::MAX);
    (0..decks.len()).find(|index| {
        *index != current
            && stats.get(*index).is_some_and(|value| due_total(*value) > 0)
    })
}

/// Installed note types plus every stock type the collection is missing.
///
/// Missing stock types keep placeholder id 0, so the add-card screen can show
/// their fields before the collection has the model.
#[must_use]
pub fn models_with_stock(installed: Vec<NoteModel>) -> Vec<NoteModel> {
    let mut models = installed;
    for kind in ranki_model::StockModelKind::ALL {
        let known = models
            .iter()
            .any(|model| ranki_model::StockModelKind::of(model) == Some(kind));
        if !known {
            models.push(kind.definition());
        }
    }
    models
}

/// Moves completed words from the tag field into the chip list.
///
/// The trailing word stays in the field until a space commits it, so the field
/// only ever holds the word being typed.
#[must_use]
pub fn collect_tags(chips: &mut Vec<String>, text: &str) -> String {
    let words = text.split_whitespace().collect::<Vec<_>>();
    let pending = !text.ends_with(char::is_whitespace);
    let (complete, remainder) = match words.split_last() {
        Some((last, rest)) if pending => (rest, *last),
        Some(_) => (&words[..], ""),
        None => (&[][..], ""),
    };
    for word in complete {
        if !chips.iter().any(|known| known == word) {
            chips.push((*word).to_owned());
        }
    }
    remainder.to_owned()
}

/// Tags that match the word being typed, excluding tags already on the note.
#[must_use]
pub fn suggest_tags(
    known: &[String],
    chips: &[String],
    query: &str,
    limit: usize,
) -> Vec<String> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    known
        .iter()
        .filter(|tag| !chips.iter().any(|chip| chip == *tag))
        .filter(|tag| tag.to_lowercase().contains(&needle))
        .take(limit)
        .cloned()
        .collect()
}

#[cfg(test)]
/// Deck row metadata: child count for parents and total cards for every row.
#[must_use]
pub fn deck_meta(rows: &[DeckRow], row: &DeckRow) -> String {
    let prefix = format!("{}::", row.path);
    let children = rows
        .iter()
        .filter(|other| {
            other.depth == row.depth.saturating_add(1)
                && other.path.starts_with(&prefix)
        })
        .count();
    deck_meta_counts(children, row.stats.total_cards)
}

fn deck_meta_counts(children: usize, cards: usize) -> String {
    let cards_label = if cards == 1 {
        String::from("1 card")
    } else {
        format!("{cards} cards")
    };
    if children == 0 {
        return cards_label;
    }
    let subdecks = if children == 1 {
        String::from("1 subdeck")
    } else {
        format!("{children} subdecks")
    };
    format!("{subdecks} · {cards_label}")
}

#[must_use]
pub fn focused_delta(
    elapsed: Duration,
    focused: bool,
    total: Duration,
) -> Duration {
    if !focused {
        return total;
    }
    total.saturating_add(elapsed).min(MAX_REVIEW_TIME)
}

#[must_use]
pub const fn rating_name(rating: Rating) -> &'static str {
    match rating {
        Rating::Again => "Again",
        Rating::Hard => "Hard",
        Rating::Good => "Good",
        Rating::Easy => "Easy",
    }
}

#[must_use]
pub const fn empty_kind(deck_count: usize, matching_rows: usize) -> EmptyKind {
    if deck_count == 0 {
        EmptyKind::NoDecks
    } else if matching_rows == 0 {
        EmptyKind::NoMatch
    } else {
        EmptyKind::None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyKind {
    None,
    NoDecks,
    NoMatch,
}

#[must_use]
pub const fn empty_copy(kind: EmptyKind) -> (&'static str, &'static str) {
    match kind {
        EmptyKind::NoDecks => {
            ("No decks yet", "Create a deck to start studying")
        }
        EmptyKind::NoMatch => ("No decks match", "Try a different search"),
        EmptyKind::None => ("", ""),
    }
}

#[must_use]
pub const fn next_view(current: View, target: View) -> View {
    match (current, target) {
        (View::Review, View::Decks) => View::Decks,
        (_, target) => target,
    }
}

/// Study shortcuts stay out of text input, dialogs, and the settings screen.
const STUDY_CONTEXT: &str =
    "ranki && !ranki_input && !ranki_dialog && !ranki_settings";
/// Settings shortcuts only apply while the settings screen is mounted.
const SETTINGS_CONTEXT: &str =
    "ranki_settings && !ranki_input && !ranki_dialog";
const STATISTICS_CONTEXT: &str =
    "ranki && !ranki_review && !ranki_input && !ranki_dialog";
const DECKS_CONTEXT: &str = "ranki_decks && !ranki_input && !ranki_dialog";

/// Tab navigation stays out of modal dialogs.
const FOCUS_CONTEXT: &str = "ranki && !ranki_dialog";
const RELOAD_CONTEXT: &str = "ranki && !ranki_input && !ranki_dialog";
const END_SESSION_CONTEXT: &str = "ranki && !ranki_dialog";
/// Browse navigation stays out of the search field and dialogs.
const BROWSE_CONTEXT: &str =
    "ranki && ranki_browse && !ranki_input && !ranki_dialog";
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("space", Reveal, Some(STUDY_CONTEXT)),
        KeyBinding::new(
            "space",
            Space,
            Some(
                "ranki && ranki_review && !ranki_input && !ranki_dialog && !ranki_settings",
            ),
        ),
        KeyBinding::new("1", GradeAgain, Some(STUDY_CONTEXT)),
        KeyBinding::new("2", GradeHard, Some(STUDY_CONTEXT)),
        KeyBinding::new("3", GradeGood, Some(STUDY_CONTEXT)),
        KeyBinding::new("4", GradeEasy, Some(STUDY_CONTEXT)),
        KeyBinding::new("r", Retry, Some(STUDY_CONTEXT)),
        KeyBinding::new("enter", Reveal, Some(STUDY_CONTEXT)),
        KeyBinding::new("f5", Reload, Some(RELOAD_CONTEXT)),
        KeyBinding::new("ctrl-z", Undo, Some(STUDY_CONTEXT)),
        KeyBinding::new("s", StudyAgain, Some(STUDY_CONTEXT)),
        KeyBinding::new("d", OpenDecks, Some(STUDY_CONTEXT)),
        KeyBinding::new("b", OpenBrowse, Some(STUDY_CONTEXT)),
        KeyBinding::new("a", OpenAdd, Some(STUDY_CONTEXT)),
        KeyBinding::new(",", OpenSettings, Some(STUDY_CONTEXT)),
        KeyBinding::new("?", ToggleHelp, Some(STUDY_CONTEXT)),
        KeyBinding::new("shift-?", ToggleHelp, Some(STUDY_CONTEXT)),
        KeyBinding::new("f1", ToggleHelp, Some(STUDY_CONTEXT)),
        KeyBinding::new("t", OpenStatistics, Some(STATISTICS_CONTEXT)),
        KeyBinding::new("n", NextDeck, Some(STUDY_CONTEXT)),
        KeyBinding::new("up", BrowseUp, Some(BROWSE_CONTEXT)),
        KeyBinding::new("down", BrowseDown, Some(BROWSE_CONTEXT)),
        KeyBinding::new("d", OpenDecks, Some(SETTINGS_CONTEXT)),
        KeyBinding::new("a", OpenAdd, Some(SETTINGS_CONTEXT)),
        KeyBinding::new("/", FocusDeckSearch, Some(DECKS_CONTEXT)),
        KeyBinding::new("c", CreateDeck, Some(DECKS_CONTEXT)),
        KeyBinding::new("escape", EndSession, Some(END_SESSION_CONTEXT)),
        KeyBinding::new("tab", NextField, Some(FOCUS_CONTEXT)),
        KeyBinding::new("shift-tab", PreviousField, Some(FOCUS_CONTEXT)),
        KeyBinding::new("cmd-enter", SaveNote, Some("ranki")),
        KeyBinding::new("ctrl-enter", SaveNote, Some("ranki")),
        KeyBinding::new("enter", ConfirmDialog, Some("ranki_dialog")),
        KeyBinding::new("escape", CancelDialog, Some("ranki_dialog")),
        KeyBinding::new("enter", SubmitDeck, Some("ranki_input")),
        KeyBinding::new("ctrl-enter", SaveNote, Some("ranki_input")),
        KeyBinding::new("cmd-enter", SaveNote, Some("ranki_input")),
        KeyBinding::new(
            "ctrl-m",
            AddOcclusionMask,
            Some("ranki_occlusion && !ranki_input"),
        ),
        KeyBinding::new(
            "ctrl-shift-m",
            RemoveOcclusionMask,
            Some("ranki_occlusion && !ranki_input"),
        ),
        KeyBinding::new(
            "enter",
            ApplyCustomColor,
            Some("ranki_color_input > ranki_input"),
        ),
        KeyBinding::new(
            "enter",
            AcceptTagSuggestion,
            Some("ranki_tag_input > ranki_input"),
        ),
        KeyBinding::new(
            "escape",
            DismissTagSuggestions,
            Some("ranki_tag_input > ranki_input"),
        ),
    ];
    bindings.extend(crate::text_field::key_bindings());
    bindings
}

#[cfg(test)]
mod tests;
