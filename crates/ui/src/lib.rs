//! Wayland-native Ranki UI built with GPUI.
//!
//! The binary supplies the shared settings and a store. This crate owns the
//! window, its view state, and the card renderer.
#![expect(
    clippy::absolute_paths,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::clone_on_ref_ptr,
    clippy::inconsistent_struct_constructor,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::match_same_arms,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_ref_mut,
    clippy::needless_pass_by_value,
    clippy::option_if_let_else,
    clippy::shadow_reuse,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unreadable_literal,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::excessive_nesting,
    reason = "GPUI view builders and pure helper models favor direct element trees and index math over extra abstraction"
)]

mod add_card_view;
mod app;
mod assets;
mod bench;
mod browser_view;
mod card_view;
mod components;
mod decks_view;
mod nav;
pub use ranki_model::occlusion;
mod review_view;
mod scrollbar;
mod settings_view;
mod smooth_scroll;
mod statistics_view;
mod sync_view;
mod text_field;
mod theme;

use gpui::{
    App, AppContext, Application, Bounds, Window, WindowBounds, WindowOptions,
    px, size,
};
use ranki_model::Store;
use std::sync::Arc;

pub use bench::{BenchApp, BenchKey};
pub use ranki_model::{Settings, ThemeChoice};

/// Writes settings changed in the window back to the shared config file.
pub type SettingsSaver =
    Arc<dyn Fn(&Settings) -> Result<(), String> + Send + Sync>;

/// Settings for the review window, shared with the command line.
pub type Options = Settings;

/// Opens the native window and runs the GPUI event loop.
pub fn run(
    store: Box<dyn Store>,
    options: Options,
    save_settings: SettingsSaver,
    sync_at_startup: bool,
) {
    let media_dir = options.paths.media_dir.clone();
    Application::new()
        .with_assets(assets::Assets { media_dir })
        .run(move |app: &mut App| {
            app.bind_keys(app::key_bindings());
            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), app);
            let window = app.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(320.), px(480.))),
                    app_id: Some("ranki".into()),
                    ..Default::default()
                },
                |window, cx| {
                    // Close the window now. Shutdown work runs after the
                    // event loop, so it must not keep the window on screen.
                    window.on_window_should_close(cx, |window, app| {
                        close_window(window, app);
                        false
                    });
                    let root = cx.new(|cx| {
                        let root = app::Root::new(
                            app::Launch {
                                store,
                                options,
                                save_settings,
                            },
                            window,
                            cx,
                        );
                        app::Root::observe_appearance(window, cx);
                        root
                    });
                    root.update(cx, |root, cx| {
                        app::Root::start_external_change_poll(cx);
                        if sync_at_startup {
                            root.sync_at_startup(cx);
                        }
                    });
                    root
                },
            );
            if let Err(error) = window {
                eprintln!("ranki: cannot open review window: {error}");
                app.quit();
                return;
            }
            app.activate(true);
        });
}

fn close_window(window: &mut Window, app: &App) {
    window.remove_window();
    app.quit();
}

#[cfg(test)]
mod tests {
    use super::close_window;
    use gpui::{AppContext as _, EmptyView, TestAppContext};

    #[gpui::test]
    fn close_request_removes_the_window(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, _| EmptyView);
        cx.update_window(*window, |_, window, app| close_window(window, app))
            .unwrap();
        assert!(cx.windows().is_empty());
    }
}
