//! Optional `AnkiWeb` sign-in shown in the settings screen.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, Context, Div, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Window, div, px,
};
use ranki_storage::ReopeningCollection;

use ranki_sync::{
    Credentials, HttpTransport, MediaProgress, MediaProgressFn, RetryPolicy,
    SyncClient, SyncOutcome, SyncState, SyncTransport, TransportConfig,
};

use crate::{
    app::{Root, ToastKind, View},
    components::{ButtonTone, ClickHandler, action_button, dot, icon, panel},
    theme::{
        Palette, SPACE_BLOCK, SPACE_GROUP, SPACE_RELATED, SPACE_SECTION, Style,
        alpha,
    },
};

/// Reads the local sync state without changing it.
pub fn read_state(state_path: &Path) -> Option<SyncState> {
    SyncState::load(state_path).ok()
}

/// The step a running sync is on, shown in place of the panel's actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncActivity {
    SigningIn,
    Syncing,
    KeepingLocal,
    KeepingRemote,
    DownloadingMedia,
}

impl SyncActivity {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SigningIn => "Signing in and syncing",
            Self::Syncing => "Syncing with AnkiWeb",
            Self::KeepingLocal => "Keeping the local copy",
            Self::KeepingRemote => "Keeping the AnkiWeb copy",
            Self::DownloadingMedia => "Downloading media",
        }
    }
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "each flag controls a separate sync state"
)]
#[must_use]
pub fn should_auto_sync(
    enabled: bool,
    auto: bool,
    signed_in: bool,
    busy: bool,
    conflicted: bool,
) -> bool {
    enabled && auto && signed_in && !busy && !conflicted
}

/// Short age of a stored sync timestamp, for the panel and status text.
pub fn relative_time(now: i64, timestamp: i64) -> String {
    let elapsed = now.saturating_sub(timestamp).max(0);
    if elapsed < 60 {
        return String::from("just now");
    }
    if elapsed < 3_600 {
        let seconds = i32::try_from(elapsed).unwrap_or(i32::MAX);
        return format!("{} ago", crate::theme::interval_label(-seconds));
    }
    if elapsed < 86_400 {
        return format!("{}h ago", elapsed / 3_600);
    }
    let days = i32::try_from(elapsed / 86_400).unwrap_or(i32::MAX);
    format!("{} ago", crate::theme::interval_label(days))
}

/// Exchanges the password for a host key, then runs one sync immediately.
fn login_and_sync<T: SyncTransport>(
    client: &mut SyncClient<T>,
    username: String,
    password: String,
) -> Result<(String, Result<SyncOutcome, String>), String> {
    client
        .login(&Credentials::new(username.clone(), password))
        .map_err(|error| error.to_string())?;
    Ok((
        username,
        client.sync(None).map_err(|error| error.to_string()),
    ))
}

fn sync_message(outcome: &SyncOutcome) -> (ToastKind, String) {
    match outcome {
        SyncOutcome::Disabled => (
            ToastKind::Error,
            String::from("Signed in, but sync did not start"),
        ),
        SyncOutcome::NoChanges => (
            ToastKind::Success,
            String::from("Signed in. AnkiWeb is up to date"),
        ),
        SyncOutcome::Uploaded => (
            ToastKind::Success,
            String::from("Signed in. Collection uploaded"),
        ),
        SyncOutcome::Downloaded => (
            ToastKind::Success,
            String::from("Signed in. Collection downloaded"),
        ),
        SyncOutcome::Conflict { reason } => (
            ToastKind::Error,
            format!("Signed in. Sync needs a manual choice: {reason}"),
        ),
        SyncOutcome::RetryLater { reason } => (
            ToastKind::Error,
            format!("Signed in. Sync failed: {reason}. Use Sync now to retry"),
        ),
    }
}

/// Text kept in the panel after a failed collection sync.
fn sync_error(error: &str) -> String {
    format!("Sync failed: {error}")
}

/// Media runs get their own label so a media failure is not mistaken for a
/// failed collection sync.
fn media_sync_error(error: &str) -> String {
    format!("Media sync failed: {error}")
}

fn refresh_message(outcome: &SyncOutcome) -> (ToastKind, String) {
    match outcome {
        SyncOutcome::Disabled => {
            (ToastKind::Error, String::from("Sync did not start"))
        }
        SyncOutcome::NoChanges => {
            (ToastKind::Success, String::from("Already up to date"))
        }
        SyncOutcome::Uploaded => {
            (ToastKind::Success, String::from("Collection uploaded"))
        }
        SyncOutcome::Downloaded => {
            (ToastKind::Success, String::from("Collection downloaded"))
        }
        SyncOutcome::Conflict { reason } => (
            ToastKind::Error,
            format!("Sync needs a manual choice: {reason}"),
        ),
        SyncOutcome::RetryLater { reason } => (
            ToastKind::Error,
            format!("Sync failed: {reason}. Use Sync now to retry"),
        ),
    }
}

fn retained_conflict(
    result: &Result<SyncOutcome, String>,
    fallback: Option<String>,
) -> Option<String> {
    if result.is_err() || matches!(result, Ok(SyncOutcome::RetryLater { .. })) {
        fallback
    } else {
        None
    }
}

fn build_client(
    collection: &Path,
    media_dir: &Path,
    state_path: &Path,
    retries: u8,
    backoff_ms: u64,
) -> Result<SyncClient<HttpTransport>, String> {
    let transport = HttpTransport::with_config(
        ranki_sync::DEFAULT_ENDPOINT,
        TransportConfig {
            retry_policy: RetryPolicy {
                retries,
                backoff: Duration::from_millis(backoff_ms),
            },
            ..TransportConfig::default()
        },
    );
    SyncClient::new(transport, collection, media_dir, state_path)
        .map_err(|error| error.to_string())
}

/// Runs one collection sync with the window's transport settings.
/// Runs one collection sync with the window's transport settings.
pub type SyncRun = dyn Fn(&Path, &Path, &Path, u8, u64) -> Result<SyncOutcome, String>
    + Send
    + Sync;

pub fn run_sync(
    collection: &Path,
    media_dir: &Path,
    state_path: &Path,
    retries: u8,
    backoff_ms: u64,
) -> Result<SyncOutcome, String> {
    let mut client =
        build_client(collection, media_dir, state_path, retries, backoff_ms)?;
    client.sync(None).map_err(|error| error.to_string())
}

/// Refreshes media with the window's transport settings, reporting progress.
pub type MediaRun = dyn Fn(&Path, &Path, &Path, u8, u64, MediaProgressFn) -> Result<(), String>
    + Send
    + Sync;

pub fn run_media(
    collection: &Path,
    media_dir: &Path,
    state_path: &Path,
    retries: u8,
    backoff_ms: u64,
    progress: MediaProgressFn,
) -> Result<(), String> {
    let mut client =
        build_client(collection, media_dir, state_path, retries, backoff_ms)?;
    client.set_media_progress(progress);
    client.sync_media().map_err(|error| error.to_string())
}

impl Root {
    /// Turns automatic syncing on or off, keeping the last saved value on error.
    pub(crate) fn toggle_auto_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync.busy() || !self.sync.signed_in {
            return;
        }
        let previous = self.options.sync.auto;
        self.options.sync.auto = !previous;
        if let Err(error) = (self.save_settings)(&self.options) {
            self.options.sync.auto = previous;
            self.show_toast(
                ToastKind::Error,
                format!("Could not save sync setting: {error}"),
                cx,
            );
        }
        cx.notify();
    }

    pub(crate) fn sign_out(&mut self, cx: &mut Context<Self>) {
        if self.sync.busy() {
            return;
        }
        if let Err(error) =
            ranki_sync::clear_host_key(&self.options.paths.sync_state)
        {
            self.show_toast(
                ToastKind::Error,
                format!("Could not sign out: {error}"),
                cx,
            );
            return;
        }
        self.sync.signed_in = false;
        self.sync.error = None;
        self.options.sync.enabled = false;
        self.options.sync.username = None;
        self.sync
            .username
            .update(cx, crate::text_field::TextField::clear);
        self.sync
            .password
            .update(cx, crate::text_field::TextField::clear);
        if let Err(error) = (self.save_settings)(&self.options) {
            self.show_toast(
                ToastKind::Error,
                format!("Signed out, but could not save settings: {error}"),
                cx,
            );
        } else {
            self.show_toast(
                ToastKind::Success,
                String::from("Signed out of AnkiWeb"),
                cx,
            );
        }
        cx.notify();
    }

    pub(crate) fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.sync.busy() {
            return;
        }
        let username = self.sync.username.read(cx).text().trim().to_owned();
        let password = self.sync.password.read(cx).text();
        if username.is_empty() || password.is_empty() {
            self.show_toast(
                ToastKind::Error,
                "Enter your AnkiWeb username and password",
                cx,
            );
            return;
        }
        self.sync.running = Some(SyncActivity::SigningIn);
        cx.notify();

        let collection = self.options.paths.collection.clone();
        let media_dir = self.options.paths.media_dir.clone();
        let sync_state = self.options.paths.sync_state.clone();
        let retries = self.options.sync.retries;
        let backoff_ms = self.options.sync.backoff_ms;
        let task = cx.background_executor().spawn(async move {
            let mut client = build_client(
                &collection,
                &media_dir,
                &sync_state,
                retries,
                backoff_ms,
            )?;
            login_and_sync(&mut client, username, password)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |root, cx| {
                match result {
                    Ok((username, sync)) => {
                        root.sync.signed_in = true;
                        root.options.sync.enabled = true;
                        root.options.sync.username = Some(username);
                        root.sync.password.update(cx, |field, cx| {
                            field.clear(cx);
                        });
                        root.finish_sync(sync, true, false, cx);
                        if let Err(error) = (root.save_settings)(&root.options)
                        {
                            root.show_toast(
                                ToastKind::Error,
                                format!("Could not save sync setting: {error}"),
                                cx,
                            );
                        }
                    }
                    Err(error) => {
                        root.sync.running = None;
                        root.show_toast(
                            ToastKind::Error,
                            format!("AnkiWeb sign-in failed: {error}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Syncs once after the last card of a study session is graded.
    pub(crate) fn sync_after_session(&mut self, cx: &mut Context<Self>) {
        if !should_auto_sync(
            self.options.sync.enabled,
            self.options.sync.auto,
            self.sync.signed_in,
            self.sync.busy(),
            self.sync.conflict.is_some(),
        ) {
            return;
        }
        self.sync.running = Some(SyncActivity::Syncing);
        cx.notify();

        let collection = self.options.paths.collection.clone();
        let media_dir = self.options.paths.media_dir.clone();
        let sync_state = self.options.paths.sync_state.clone();
        let retries = self.options.sync.retries;
        let backoff_ms = self.options.sync.backoff_ms;
        let runner = Arc::clone(&self.sync_run);
        let task = cx.background_executor().spawn(async move {
            runner(&collection, &media_dir, &sync_state, retries, backoff_ms)
        });
        Self::spawn_sync_task(task, false, None, true, cx);
    }

    pub(crate) fn sync_now(&mut self, cx: &mut Context<Self>) {
        self.start_sync(false, cx);
    }

    pub(crate) fn sync_at_startup(&mut self, cx: &mut Context<Self>) {
        if !should_auto_sync(
            self.options.sync.enabled,
            self.options.sync.auto,
            self.sync.signed_in,
            self.sync.busy(),
            self.sync.conflict.is_some(),
        ) {
            return;
        }
        self.start_sync(true, cx);
    }
    fn start_sync(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if self.sync.busy() || !self.sync.signed_in {
            return;
        }
        let reason = self.sync.conflict.clone();
        self.sync.running = Some(SyncActivity::Syncing);
        cx.notify();

        let collection = self.options.paths.collection.clone();
        let media_dir = self.options.paths.media_dir.clone();
        let sync_state = self.options.paths.sync_state.clone();
        let retries = self.options.sync.retries;
        let backoff_ms = self.options.sync.backoff_ms;
        let runner = Arc::clone(&self.sync_run);
        let task = cx.background_executor().spawn(async move {
            runner(&collection, &media_dir, &sync_state, retries, backoff_ms)
        });
        Self::spawn_sync_task(task, false, reason, quiet, cx);
    }

    pub(crate) fn resolve_sync_conflict(
        &mut self,
        keep_local: bool,
        cx: &mut Context<Self>,
    ) {
        if self.sync.busy() {
            return;
        }
        let reason = self.sync.conflict.clone();
        self.sync.running = Some(if keep_local {
            SyncActivity::KeepingLocal
        } else {
            SyncActivity::KeepingRemote
        });
        cx.notify();

        let collection = self.options.paths.collection.clone();
        let media_dir = self.options.paths.media_dir.clone();
        let sync_state = self.options.paths.sync_state.clone();
        let retries = self.options.sync.retries;
        let backoff_ms = self.options.sync.backoff_ms;
        let task = cx.background_executor().spawn(async move {
            let mut client = build_client(
                &collection,
                &media_dir,
                &sync_state,
                retries,
                backoff_ms,
            )?;
            if keep_local {
                client
                    .full_upload()
                    .map(|()| SyncOutcome::Uploaded)
                    .map_err(|error| error.to_string())
            } else {
                client
                    .full_download()
                    .map(|()| SyncOutcome::Downloaded)
                    .map_err(|error| error.to_string())
            }
        });
        Self::spawn_sync_task(task, false, reason, false, cx);
    }

    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "quiet and signed in describe separate sync states"
    )]
    fn spawn_sync_task(
        task: impl Future<Output = Result<SyncOutcome, String>> + Send + 'static,
        signed_in: bool,
        fallback_conflict: Option<String>,
        quiet: bool,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |root, cx| {
                let remaining_conflict =
                    retained_conflict(&result, fallback_conflict);
                root.finish_sync(result, signed_in, quiet, cx);
                if remaining_conflict.is_some() {
                    root.sync.conflict = remaining_conflict;
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "quiet and signed in describe separate sync states"
    )]
    fn finish_sync(
        &mut self,
        result: Result<SyncOutcome, String>,
        signed_in: bool,
        quiet: bool,
        cx: &mut Context<Self>,
    ) {
        self.sync.running = None;
        if let Some(slot) = self.startup_sync_result.take()
            && let Ok(outcome) = &result
            && let Ok(mut guard) = slot.lock()
        {
            *guard = Some(outcome.clone());
        }
        self.sync.conflict = None;
        self.sync.error = match &result {
            Err(error) => Some(sync_error(error)),
            Ok(SyncOutcome::RetryLater { reason }) => Some(sync_error(reason)),
            Ok(_) => None,
        };
        if let Ok(outcome) = &result
            && outcome.is_settled()
            && let Some(last_success) =
                read_state(&self.options.paths.sync_state)
                    .and_then(|state| state.last_success)
        {
            self.sync.last_success = Some(last_success);
        }
        let (kind, message) = match &result {
            Ok(outcome) => {
                if let SyncOutcome::Conflict { reason } = outcome {
                    self.sync.conflict = Some(reason.clone());
                }
                if signed_in {
                    sync_message(outcome)
                } else {
                    refresh_message(outcome)
                }
            }
            Err(error) => (
                ToastKind::Error,
                if signed_in {
                    format!("Signed in, but sync failed: {error}")
                } else {
                    format!("Sync failed: {error}")
                },
            ),
        };
        let needs_attention = match &result {
            Err(_) => true,
            Ok(
                SyncOutcome::Conflict { .. } | SyncOutcome::RetryLater { .. },
            ) => true,
            Ok(_) => false,
        };
        if !quiet || needs_attention {
            self.show_toast(kind, message, cx);
        }
        if matches!(result, Ok(SyncOutcome::Downloaded)) {
            self.reopen_collection(cx);
        }
        let settled = result.as_ref().is_ok_and(SyncOutcome::is_settled);
        if self.sync.signed_in && settled {
            self.start_media_sync(cx);
        }
    }

    /// Downloads media after the collection sync, off the main thread.
    fn start_media_sync(&mut self, cx: &mut Context<Self>) {
        let collection = self.options.paths.collection.clone();
        let media_dir = self.options.paths.media_dir.clone();
        let sync_state = self.options.paths.sync_state.clone();
        let retries = self.options.sync.retries;
        let backoff_ms = self.options.sync.backoff_ms;
        let runner = Arc::clone(&self.media_run);
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        self.sync.running = Some(SyncActivity::DownloadingMedia);
        self.sync.media = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let progress: MediaProgressFn = Arc::new(move |progress| {
                let _ = sender.unbounded_send(progress);
            });
            runner(
                &collection,
                &media_dir,
                &sync_state,
                retries,
                backoff_ms,
                progress,
            )
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let mut receiver = receiver;
            while let Some(progress) = receiver.next().await {
                let _ = this.update(cx, |root, cx| {
                    root.sync.media = Some(progress);
                    cx.notify();
                });
            }
            let result = task.await;
            let _ = this.update(cx, |root, cx| {
                root.sync.running = None;
                root.sync.media = None;
                match result {
                    Ok(()) => {
                        root.images
                            .update(cx, |cache, _| cache.forget_failed());
                    }
                    Err(error) => {
                        root.sync.error = Some(media_sync_error(&error));
                        root.show_toast(
                            ToastKind::Error,
                            media_sync_error(&error),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn reopen_collection(&mut self, cx: &mut Context<Self>) {
        let review_active = self.view == View::Review;
        self.store =
            Box::new(ReopeningCollection::new(&self.options.paths.collection));
        self.reload(cx, false);
        if review_active {
            self.end_session(cx);
            self.show_toast(
                ToastKind::Success,
                String::from(
                    "Collection downloaded. The study session ended because its cards changed.",
                ),
                cx,
            );
            self.persist_state(cx);
        } else {
            if self.view == View::Browse {
                self.refresh_browse(cx);
            }
            if self.view == View::Statistics {
                self.schedule_statistics_refresh(cx);
            }
            cx.notify();
        }
    }

    /// Running-sync indicator in the settings panel.
    pub(crate) fn render_sync_activity(&self) -> Option<Div> {
        let activity = self.sync.running?;
        Some(activity_line(
            activity,
            self.colors(),
            self.motion_ms(PULSE_MS),
            self.sync.media,
            "sync-panel-activity",
        ))
    }

    /// Keeps session progress visible while the study footer is on screen.
    pub(crate) fn render_review_sync(&self) -> Option<AnyElement> {
        let activity = self.sync.running?;
        let text = activity_text(activity, self.sync.media);
        Some(sync_dot(
            "sync-review-dot",
            text,
            self.colors(),
            self.motion_ms(PULSE_MS),
        ))
    }

    pub(crate) fn render_sync(&self) -> impl IntoElement {
        let colors = self.colors();
        let style = self.style();
        let signed_in = self.sync.signed_in;
        let running = self.sync.running;
        let sign_in = self.action_click(crate::app::UiAction::SyncSignIn);
        let sign_out = self.action_click(crate::app::UiAction::SyncSignOut);
        let sync_now = self.action_click(crate::app::UiAction::SyncNow);
        let auto_sync = self.action_click(crate::app::UiAction::ToggleAutoSync);
        let keep_local = self.action_click(crate::app::UiAction::SyncKeepLocal);
        let keep_remote =
            self.action_click(crate::app::UiAction::SyncKeepRemote);

        let mut header = div()
            .flex()
            .items_center()
            .gap(px(SPACE_RELATED))
            .child(icon("icon-clock", 18., colors.mauve))
            .child(
                div()
                    .flex_1()
                    .text_size(px(18.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("AnkiWeb sync"),
            );
        if signed_in && running.is_none() {
            header = header.child(action_button(
                "sync-sign-out",
                "Sign out",
                ButtonTone::Neutral,
                style,
                sign_out,
            ));
        }

        let body = if let Some(pill) = self.render_sync_activity() {
            pill
        } else if !signed_in {
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_BLOCK))
                .child(status_line(
                    "Sign in to keep this collection in step with AnkiWeb.",
                    colors,
                ))
                .child(labeled_input(
                    "Username",
                    self.sync.username.clone(),
                    colors,
                ))
                .child(labeled_input(
                    "Password",
                    self.sync.password.clone(),
                    colors,
                ))
                .child(div().flex().child(action_button(
                    "sync-sign-in",
                    "Sign in",
                    ButtonTone::Primary,
                    style,
                    sign_in,
                )))
        } else if let Some(reason) = self.sync.conflict.as_deref() {
            conflict_card(reason, colors, style, keep_local, keep_remote)
        } else {
            let last_sync = self
                .sync
                .last_success
                .map(|timestamp| relative_time(Self::now(), timestamp));
            let note = if self.options.sync.enabled && self.options.sync.auto {
                "Changes sync to AnkiWeb automatically."
            } else {
                "Use Sync now to send changes."
            };
            let mut detail = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(160.))
                .gap(px(SPACE_GROUP))
                .child(status_line(note, colors));
            if let Some(timestamp) = last_sync {
                detail = detail.child(status_line(
                    format!("Last synced {timestamp}"),
                    colors,
                ));
            }
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(px(SPACE_SECTION))
                .child(detail)
                .child({
                    let mut actions =
                        div().flex().items_center().gap(px(SPACE_RELATED));
                    if signed_in && running.is_none() {
                        let label = if self.options.sync.auto {
                            "Auto sync: on"
                        } else {
                            "Auto sync: off"
                        };
                        actions = actions.child(action_button(
                            "sync-auto",
                            label,
                            ButtonTone::Neutral,
                            style,
                            auto_sync,
                        ));
                    }
                    actions.child(action_button(
                        "sync-now",
                        "Sync now",
                        ButtonTone::Neutral,
                        style,
                        sync_now,
                    ))
                })
        };

        let mut column = div().flex().flex_col().gap(px(SPACE_RELATED));
        column = column.child(body);
        if let Some(error) = self.sync.error.as_deref()
            && running.is_none()
        {
            column = column.child(
                div()
                    .text_size(px(13.))
                    .text_color(colors.red)
                    .child(SharedString::from(error.to_owned())),
            );
        }

        panel(colors)
            .id("settings-sync")
            .child(header)
            .child(column)
    }
}

const PULSE_MS: u64 = 900;

/// Thirteen-point line that carries the panel's current state.
fn status_line(text: impl Into<SharedString>, colors: Palette) -> Div {
    div()
        .flex_1()
        .min_w(px(0.))
        .text_size(px(13.))
        .text_color(colors.subtext)
        .child(text.into())
}

/// The activity line's text, including media counts once they are known.
fn activity_text(
    activity: SyncActivity,
    media: Option<MediaProgress>,
) -> String {
    let mut label = activity.label().to_owned();
    if let Some(progress) = media.filter(|progress| progress.total > 0) {
        label = format!("{} ({} of {})", label, progress.done, progress.total);
    }
    format!("{label}...")
}

/// Replaces the actions while a sync runs, so a long upload still looks alive.
fn activity_line(
    activity: SyncActivity,
    colors: Palette,
    motion_ms: u64,
    media: Option<MediaProgress>,
    id: &'static str,
) -> Div {
    use gpui::AnimationExt as _;

    let label = activity_text(activity, media);

    let marker = if motion_ms == 0 {
        dot(colors.mauve, 8.).into_any_element()
    } else {
        dot(colors.mauve, 8.)
            .with_animation(
                id,
                gpui::Animation::new(Duration::from_millis(motion_ms))
                    .repeat()
                    .with_easing(|delta| {
                        0.35 + 0.65 * (delta * std::f32::consts::PI).sin()
                    }),
                gpui::Styled::opacity,
            )
            .into_any_element()
    };
    div()
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .gap(px(SPACE_GROUP))
        .text_size(px(13.))
        .text_color(colors.text)
        .child(marker)
        .child(SharedString::from(label))
}

/// Small status marker for the compact rail and the review footer.
fn sync_dot(
    id: &'static str,
    text: String,
    colors: Palette,
    motion_ms: u64,
) -> AnyElement {
    use gpui::AnimationExt as _;

    let marker = dot(colors.mauve, 8.);
    let marker = if motion_ms == 0 {
        marker.into_any_element()
    } else {
        marker
            .with_animation(
                id,
                gpui::Animation::new(Duration::from_millis(motion_ms))
                    .repeat()
                    .with_easing(|delta| {
                        0.35 + 0.65 * (delta * std::f32::consts::PI).sin()
                    }),
                gpui::Styled::opacity,
            )
            .into_any_element()
    };
    let label = SharedString::from(text);
    div()
        .debug_selector(move || id.to_owned())
        .id(SharedString::from(format!("{id}-tooltip")))
        .flex_none()
        .child(marker)
        .tooltip(move |_, cx| {
            cx.new(|_| SyncTooltip {
                text: label.clone(),
                colors,
            })
            .into()
        })
        .into_any_element()
}

struct SyncTooltip {
    text: SharedString,
    colors: Palette,
}

impl Render for SyncTooltip {
    fn render(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .p(px(SPACE_GROUP))
            .rounded(px(10.))
            .bg(self.colors.base)
            .border_1()
            .border_color(self.colors.surface1)
            .text_size(px(12.))
            .text_color(self.colors.text)
            .child(self.text.clone())
    }
}

/// The one decision the sync panel ever asks for.
fn conflict_card(
    reason: &str,
    colors: Palette,
    style: Style,
    keep_local: ClickHandler,
    keep_remote: ClickHandler,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_RELATED))
        .p(px(SPACE_BLOCK))
        .rounded(px(16.))
        .bg(alpha(colors.peach, 0.12))
        .border_1()
        .border_color(alpha(colors.peach, 0.4))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_GROUP))
                .text_size(px(14.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(dot(colors.peach, 8.))
                .child("Sync conflict"),
        )
        .child(div().text_size(px(13.)).child(capitalized(reason)))
        .child(
            div()
                .text_size(px(13.))
                .text_color(colors.subtext)
                .child("Choose the copy to keep. The other one is replaced."),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(SPACE_RELATED))
                .child(action_button(
                    "sync-keep-remote",
                    "Keep AnkiWeb copy",
                    ButtonTone::Primary,
                    style,
                    keep_remote,
                ))
                .child(action_button(
                    "sync-keep-local",
                    "Keep local copy",
                    ButtonTone::Neutral,
                    style,
                    keep_local,
                )),
        )
}

/// Upper-cases the first letter so a stored reason reads as a sentence.
fn capitalized(reason: &str) -> String {
    let mut characters = reason.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

fn labeled_input(
    label: &'static str,
    field: gpui::Entity<crate::text_field::TextField>,
    colors: Palette,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_GROUP))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(colors.subtext)
                .child(label),
        )
        .child(
            div()
                .flex()
                .px(px(SPACE_RELATED))
                .py(px(SPACE_RELATED))
                .rounded(px(12.))
                .border_1()
                .border_color(colors.surface1)
                .bg(colors.base)
                .text_size(px(14.))
                .child(div().flex_1().child(field)),
        )
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use ranki_sync::{
        MediaProgress, MockResponse, MockTransport, SyncClient, SyncMethod,
        SyncOutcome, SyncState, clear_host_key,
    };
    use serde_json::json;

    use super::{
        SyncActivity, activity_text, capitalized, login_and_sync, read_state,
        refresh_message, relative_time, retained_conflict, should_auto_sync,
        sync_message,
    };
    #[test]
    fn footer_reads_the_last_sync_from_the_state_file() {
        let Ok(directory) = tempfile::tempdir() else {
            return;
        };
        let state_path = directory.path().join("ranki-sync.json");
        let state = SyncState {
            host_key: String::from("stored-key"),
            last_success: Some(1_700_000_000),
            ..SyncState::default()
        };
        let Ok(text) = serde_json::to_string(&state) else {
            return;
        };
        if std::fs::write(&state_path, text).is_err() {
            return;
        }
        let loaded = read_state(&state_path).unwrap_or_default();
        assert_eq!(loaded.host_key, "stored-key");
        assert_eq!(loaded.last_success, Some(1_700_000_000));
    }

    #[test]
    fn retry_later_points_gui_users_to_sync_now() {
        let reason = String::from("offline");
        let (kind, text) = refresh_message(&SyncOutcome::RetryLater {
            reason: reason.clone(),
        });
        assert_eq!(kind, crate::app::ToastKind::Error);
        assert_eq!(text, "Sync failed: offline. Use Sync now to retry");
        let (_, signed_in) = sync_message(&SyncOutcome::RetryLater { reason });
        assert_eq!(
            signed_in,
            "Signed in. Sync failed: offline. Use Sync now to retry"
        );
    }

    #[gpui::test]
    fn media_failure_is_stored_as_a_media_error(cx: &mut gpui::TestAppContext) {
        let Ok(directory) = tempfile::tempdir() else {
            return;
        };
        let collection = directory.path().join("collection.anki2");
        if ranki_storage::Collection::create(&collection).is_err() {
            return;
        }
        let mut options = crate::Options::default();
        options.paths.collection = collection.clone();
        let (root, cx) = cx.add_window_view(|window, cx| {
            super::Root::new(
                crate::app::Launch {
                    store: Box::new(ranki_storage::ReopeningCollection::new(
                        &collection,
                    )),
                    options,
                    save_settings: std::sync::Arc::new(|_| Ok(())),
                },
                window,
                cx,
            )
        });
        cx.update_entity(&root, |root, cx| {
            root.sync.signed_in = true;
            root.media_run = std::sync::Arc::new(|_, _, _, _, _, _| {
                Err(String::from("media server unavailable"))
            });
            root.start_media_sync(cx);
        });
        cx.run_until_parked();
        let (error, toast) = cx.read_entity(&root, |root, _| {
            (
                root.sync.error.clone(),
                root.toast.clone().map(|toast| toast.text),
            )
        });
        let expected =
            Some(String::from("Media sync failed: media server unavailable"));
        assert_eq!(error, expected);
        assert_eq!(toast, expected);
    }

    #[test]
    fn last_sync_uses_short_relative_times() {
        assert_eq!(relative_time(1_700_000_010, 1_700_000_000), "just now");
        assert_eq!(relative_time(1_700_000_350, 1_700_000_000), "5m ago");
        assert_eq!(relative_time(1_700_007_200, 1_700_000_000), "2h ago");
        assert_eq!(relative_time(1_700_086_400, 1_700_000_000), "1d ago");
        assert_eq!(relative_time(1_000, 9_000), "just now");
    }

    #[test]
    fn sign_in_runs_a_sync_immediately() {
        let Ok(directory) = tempfile::tempdir() else {
            return;
        };
        let collection = directory.path().join("collection.anki2");
        if ranki_storage::Collection::create(&collection).is_err() {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let transport = MockTransport::new()
            .with_response(
                SyncMethod::HostKey,
                MockResponse::ok_json(&json!({ "key": "host-key" })).unwrap(),
            )
            .with_response(
                SyncMethod::Meta,
                MockResponse::ok_json(&json!({
                    "mod": 1,
                    "scm": 1,
                    "usn": 1,
                    "ts": now,
                    "empty": true,
                    "cont": true,
                    "media_usn": 0,
                }))
                .unwrap(),
            );
        let state_path = directory.path().join("ranki-sync.json");
        let Ok(mut client) = SyncClient::new(
            transport,
            &collection,
            directory.path(),
            &state_path,
        ) else {
            return;
        };

        let password = directory
            .path()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .chars()
            .rev()
            .collect();
        let result =
            login_and_sync(&mut client, String::from("user"), password);

        let Ok((username, outcome)) = result else {
            return;
        };
        assert_eq!(username, "user");
        assert_eq!(outcome, Ok(SyncOutcome::NoChanges));
        let requests = client.transport().requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, SyncMethod::HostKey);
        assert_eq!(requests[1].method, SyncMethod::Meta);
    }

    #[test]
    fn sync_outcome_messages_name_the_result() {
        let (_, uploaded) = sync_message(&SyncOutcome::Uploaded);
        assert_eq!(uploaded, "Signed in. Collection uploaded");
        let (_, downloaded) = sync_message(&SyncOutcome::Downloaded);
        assert_eq!(downloaded, "Signed in. Collection downloaded");
    }

    #[test]
    fn refresh_conflict_asks_for_a_choice() {
        let (kind, text) = refresh_message(&SyncOutcome::Conflict {
            reason: String::from("both collections changed"),
        });
        assert_eq!(kind, crate::app::ToastKind::Error);
        assert_eq!(
            text,
            "Sync needs a manual choice: both collections changed"
        );
    }

    #[test]
    fn unresolved_conflict_is_retained_after_failed_sync() {
        let fallback = Some(String::from("both collections changed"));
        let downloaded = Ok(SyncOutcome::Downloaded);
        assert_eq!(retained_conflict(&downloaded, fallback.clone()), None);

        let uploaded = Ok(SyncOutcome::Uploaded);
        assert_eq!(retained_conflict(&uploaded, fallback.clone()), None);

        let retried = Ok(SyncOutcome::RetryLater {
            reason: String::from("offline"),
        });
        assert_eq!(
            retained_conflict(&retried, fallback.clone()).as_deref(),
            Some("both collections changed")
        );

        let failed = Err(String::from("server rejected the collection"));
        assert_eq!(
            retained_conflict(&failed, fallback).as_deref(),
            Some("both collections changed")
        );
    }

    #[test]
    fn media_activity_reports_file_counts() {
        assert_eq!(
            activity_text(
                SyncActivity::DownloadingMedia,
                Some(MediaProgress {
                    done: 12,
                    total: 40
                })
            ),
            "Downloading media (12 of 40)..."
        );
        assert_eq!(
            activity_text(
                SyncActivity::DownloadingMedia,
                Some(MediaProgress { done: 0, total: 0 })
            ),
            "Downloading media..."
        );
        assert_eq!(
            activity_text(SyncActivity::SigningIn, None),
            "Signing in and syncing..."
        );
    }

    #[test]
    fn activity_labels_name_the_running_step() {
        let labels: Vec<&str> = [
            SyncActivity::SigningIn,
            SyncActivity::Syncing,
            SyncActivity::KeepingLocal,
            SyncActivity::KeepingRemote,
            SyncActivity::DownloadingMedia,
        ]
        .into_iter()
        .map(SyncActivity::label)
        .collect();
        assert_eq!(labels[0], "Signing in and syncing");
        assert_eq!(labels[1], "Syncing with AnkiWeb");
        assert_eq!(labels[2], "Keeping the local copy");
        assert_eq!(labels[4], "Downloading media");
        let unique: std::collections::HashSet<&str> =
            labels.iter().copied().collect();
        assert_eq!(unique.len(), labels.len());
    }

    #[test]
    fn conflict_reasons_start_a_sentence() {
        assert_eq!(
            capitalized("both collections changed since the last sync"),
            "Both collections changed since the last sync"
        );
        assert_eq!(capitalized(""), "");
    }

    #[test]
    fn background_sync_waits_for_a_signed_in_idle_session() {
        assert!(should_auto_sync(true, true, true, false, false));
        assert!(!should_auto_sync(false, true, true, false, false));
        assert!(!should_auto_sync(true, false, true, false, false));
        assert!(!should_auto_sync(true, true, false, false, false));
        assert!(!should_auto_sync(true, true, true, true, false));
        assert!(!should_auto_sync(true, true, true, false, true));
    }

    #[test]
    fn sign_out_clears_the_cached_host_key() {
        let Ok(directory) = tempfile::tempdir() else {
            return;
        };
        let state_path = directory.path().join("ranki-sync.json");
        let state = SyncState {
            host_key: String::from("stored-key"),
            last_mod: Some(7),
            last_scm: Some(8),
            ..SyncState::default()
        };
        let Ok(text) = serde_json::to_string(&state) else {
            return;
        };
        assert!(std::fs::write(&state_path, text).is_ok());
        assert!(clear_host_key(&state_path).is_ok());
        let saved = read_state(&state_path).unwrap_or_default();
        assert_eq!(saved.host_key, "");
        assert_eq!(saved.last_mod, Some(7));
        assert_eq!(saved.last_scm, Some(8));
        assert!(clear_host_key(&state_path).is_ok());
    }
}
