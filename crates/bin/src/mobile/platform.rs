//! App paths and native entry points for the mobile build.

use std::env;
#[cfg(not(target_os = "android"))]
use std::ffi::OsString;
use std::fs;
#[cfg(target_os = "android")]
use std::path::Path;
use std::path::PathBuf;

use ranki_model::config::DataPaths;
use ranki_storage::Collection;
use ranki_storage::ReopeningCollection;

use crate::mobile::MobileError;
use crate::mobile::Result;
use crate::mobile::state::AppState;
use crate::mobile::ui::MobileApp;

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

const APP_ID: &str = "ranki";
const WINDOW_WIDTH: f32 = 390.0;
const WINDOW_HEIGHT: f32 = 844.0;

/// Builds the phone-sized window options for eframe.
#[must_use]
pub fn native_options() -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_inner_size(dev_viewport_size()),
        ..Default::default()
    };
    if env::var_os("RANKI_MOBILE_DEV_VIEW").is_some() {
        options.viewport = options.viewport.with_decorations(false);
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        options.renderer = eframe::Renderer::Wgpu;
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        options.renderer = eframe::Renderer::Glow;
    }
    options
}

fn dev_viewport_size() -> [f32; 2] {
    [
        dev_dimension("RANKI_MOBILE_DEV_WIDTH", WINDOW_WIDTH),
        dev_dimension("RANKI_MOBILE_DEV_HEIGHT", WINDOW_HEIGHT),
    ]
}

fn dev_dimension(name: &str, default: f32) -> f32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value >= 240.0 && *value <= 3840.0)
        .unwrap_or(default)
}

/// Resolves the data and configuration directories.
///
/// Explicit `RANKI_DATA_DIR` and `RANKI_CONFIG_DIR` values win. Android gets
/// its directories from `internal_data_path` in [`run_with_android_app`], so
/// it never needs `HOME`. Other platforms follow XDG and then `HOME`.
#[must_use]
pub fn initial_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    #[cfg(target_os = "android")]
    {
        (
            explicit_dir("RANKI_DATA_DIR"),
            explicit_dir("RANKI_CONFIG_DIR"),
        )
    }
    #[cfg(not(target_os = "android"))]
    {
        (
            explicit_dir("RANKI_DATA_DIR")
                .or_else(|| nested_dir(env::var_os("XDG_DATA_HOME"), "ranki"))
                .or_else(|| {
                    nested_dir(env::var_os("HOME"), ".local/share/ranki")
                }),
            explicit_dir("RANKI_CONFIG_DIR")
                .or_else(|| nested_dir(env::var_os("XDG_CONFIG_HOME"), "ranki"))
                .or_else(|| nested_dir(env::var_os("HOME"), ".config/ranki")),
        )
    }
}

fn explicit_dir(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(target_os = "android"))]
fn nested_dir(base: Option<OsString>, suffix: &str) -> Option<PathBuf> {
    base.filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(value).join(suffix))
}

/// Opens the collection, loads settings, and runs the eframe event loop.
///
/// # Errors
/// Returns an error when no data directory can be resolved, storage cannot be
/// created or opened, or eframe cannot start the window.
pub fn run() -> eframe::Result<()> {
    run_with_options(native_options())
}

#[cfg(target_os = "android")]
/// Starts the mobile app inside an Android native activity.
///
/// # Errors
/// Returns an error when no data directory can be resolved, storage cannot be
/// created or opened, or eframe cannot start the renderer.
pub fn run_with_android_app(app: AndroidApp) -> eframe::Result<()> {
    if let Some(root) = app.internal_data_path() {
        set_android_dir("RANKI_DATA_DIR", &root.join("ranki"));
        set_android_dir("RANKI_CONFIG_DIR", &root.join("config"));
    }
    let mut options = native_options();
    options.android_app = Some(app);
    run_with_options(options)
}

#[cfg(target_os = "android")]
fn set_android_dir(name: &str, path: &Path) {
    if env::var_os(name).is_none() {
        // SAFETY: this runs from the native entry point before the event loop
        // starts, so no other thread reads these variables yet.
        unsafe { env::set_var(name, path) };
    }
}

fn run_with_options(options: eframe::NativeOptions) -> eframe::Result<()> {
    match build_app() {
        Ok(app) => eframe::run_native(
            APP_ID,
            options,
            Box::new(move |_| Ok(Box::new(app))),
        ),
        Err(error) => {
            let message = error.to_string();
            eframe::run_native(
                APP_ID,
                options,
                Box::new(move |_| Ok(Box::new(StartupError { message }))),
            )
        }
    }
}

/// Visible replacement when the collection or settings cannot be opened.
struct StartupError {
    message: String,
}

impl eframe::App for StartupError {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(48.0);
            ui.heading("Ranki could not start");
            ui.add_space(12.0);
            ui.label(&self.message);
            ui.add_space(20.0);
            ui.label(
                "Check that the app data directory is writable, then reopen Ranki.",
            );
        });
    }
}

fn build_app() -> Result<MobileApp> {
    let (maybe_data_dir, maybe_config_dir) = initial_paths();
    let data_dir = maybe_data_dir.ok_or_else(|| {
        MobileError::Message(String::from(
            "cannot resolve the data directory; set RANKI_DATA_DIR or HOME",
        ))
    })?;
    let config_dir = maybe_config_dir.ok_or_else(|| {
        MobileError::Message(String::from(
            "cannot resolve the config directory; set RANKI_CONFIG_DIR or HOME",
        ))
    })?;

    fs::create_dir_all(&data_dir)?;
    fs::create_dir_all(&config_dir)?;

    let paths = DataPaths::new(data_dir);
    fs::create_dir_all(&paths.media_dir)?;

    let collection_path = paths.collection.clone();
    let collection = if collection_path.exists() {
        Collection::open(&collection_path)?
    } else {
        Collection::create(&collection_path)?
    };
    drop(collection);

    let store = ReopeningCollection::new(&collection_path);
    let state = AppState::open(store, paths, &config_dir)?;
    Ok(MobileApp::new(state))
}
