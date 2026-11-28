//! Loads the shared config schema from disk.

#[cfg(feature = "desktop")]
pub use ranki_model::UiState;
use ranki_model::config::parse;
pub use ranki_model::{Config, DataPaths, Settings};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use crate::Cli;

/// Reads the config file from an explicit directory or desktop defaults.
pub fn load() -> Config {
    let path = config_path(
        env::var_os("RANKI_CONFIG_DIR").as_deref(),
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    );
    path.map_or_else(Config::default, |config_file| read_config(&config_file))
}

/// Writes the shared config back after optimizer updates.
///
/// # Errors
/// Returns an error when the config file cannot be written or no path is set.
pub fn save(config: &Config) -> io::Result<()> {
    let path = config_path(
        env::var_os("RANKI_CONFIG_DIR").as_deref(),
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
    .ok_or_else(|| {
        io::Error::new(
            ErrorKind::NotFound,
            "cannot resolve the config path; set RANKI_CONFIG_DIR or HOME",
        )
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string(config).map_err(io::Error::other)?;
    fs::write(path, text)
}

/// Copies resolved study settings into the config saved to `config.toml`.
#[must_use]
pub fn apply_study_settings(config: &mut Config, settings: &Settings) -> bool {
    let study = settings.study.clone();
    let changed = config.study != study;
    config.study = study;
    changed
}

#[cfg(feature = "desktop")]
/// Reads the GUI's layout state from `ui-state.toml` beside the config file.
pub fn load_ui_state() -> UiState {
    config_path(
        env::var_os("RANKI_CONFIG_DIR").as_deref(),
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
    .map_or_else(UiState::default, |path| read_ui_state(&path))
}

#[cfg(feature = "desktop")]
/// Writes GUI layout state without touching the declarative config file.
pub fn save_ui_state(state: &UiState) -> io::Result<()> {
    let path = config_path(
        env::var_os("RANKI_CONFIG_DIR").as_deref(),
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
    .ok_or_else(|| {
        io::Error::new(
            ErrorKind::NotFound,
            "cannot resolve the config path; set RANKI_CONFIG_DIR or HOME",
        )
    })?;
    write_ui_state(&path, state)
}

#[cfg(feature = "desktop")]
fn read_ui_state(config: &Path) -> UiState {
    let path = ui_state_path(config);
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
            eprintln!(
                "ranki: ui state: cannot read {}: {error}",
                path.display()
            );
            UiState::default()
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => UiState::default(),
        Err(error) => {
            eprintln!(
                "ranki: ui state: cannot read {}: {error}",
                path.display()
            );
            UiState::default()
        }
    }
}

#[cfg(feature = "desktop")]
fn write_ui_state(config: &Path, state: &UiState) -> io::Result<()> {
    let path = ui_state_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string(state).map_err(io::Error::other)?)
}

#[cfg(feature = "desktop")]
fn ui_state_path(config: &Path) -> PathBuf {
    config.with_file_name("ui-state.toml")
}

/// Resolves the config file from an explicit directory or desktop defaults.
#[must_use]
pub fn config_path(
    config_dir: Option<&OsStr>,
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    config_dir
        .filter(|value| !value.is_empty())
        .map(|dir| PathBuf::from(dir).join("config.toml"))
        .or_else(|| {
            xdg.filter(|value| !value.is_empty())
                .map(|dir| PathBuf::from(dir).join("ranki/config.toml"))
        })
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|dir| PathBuf::from(dir).join(".config/ranki/config.toml"))
        })
}

/// Resolves Ranki's data directory from an explicit value or desktop defaults.
#[must_use]
pub fn data_dir_path(
    data_dir: Option<&OsStr>,
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    data_dir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            xdg.filter(|value| !value.is_empty())
                .map(|dir| PathBuf::from(dir).join("ranki"))
        })
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|dir| PathBuf::from(dir).join(".local/share/ranki"))
        })
}
fn read_config(path: &Path) -> Config {
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == ErrorKind::NotFound => Config::default(),
        Err(error) => {
            eprintln!("ranki: config: cannot read {}: {error}", path.display());
            Config::default()
        }
    }
}

/// Resolves CLI sync flags over the config file.
#[must_use]
pub fn resolve_settings(
    cli: &Cli,
    config: &Config,
    paths: DataPaths,
) -> Settings {
    let mut settings = Settings::resolve(config, paths);
    if cli.sync {
        settings.sync.enabled = true;
    }
    if cli.no_sync {
        settings.sync.enabled = false;
    }
    if cli.sync_auto {
        settings.sync.auto = true;
    }
    if cli.no_sync_auto {
        settings.sync.auto = false;
    }
    if let Some(username) = cli.sync_username.clone() {
        settings.sync.username = Some(username);
    }
    if let Some(path) = cli.sync_hkey_file.clone() {
        settings.sync.hkey_file = Some(path);
    }
    settings
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    #[cfg(feature = "desktop")]
    use std::fs;
    use std::path::Path;

    use clap::Parser;

    #[cfg(feature = "desktop")]
    use super::UiState;
    use super::{
        Config, DataPaths, Settings, apply_study_settings, config_path,
        data_dir_path, resolve_settings,
    };
    use crate::Cli;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn config_path_prefers_an_explicit_directory() {
        let path = config_path(
            Some(OsStr::new("/app-config")),
            Some(OsStr::new("/tmp/xdg")),
            Some(OsStr::new("/home/user")),
        );
        assert_eq!(path.as_deref(), Some(Path::new("/app-config/config.toml")));
    }

    #[test]
    fn config_path_prefers_xdg_config_home() {
        let path = config_path(
            None,
            Some(OsStr::new("/tmp/xdg")),
            Some(OsStr::new("/home/user")),
        );
        assert_eq!(
            path.as_deref(),
            Some(Path::new("/tmp/xdg/ranki/config.toml"))
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn ui_state_lives_beside_config_and_survives_reload() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = dir.path().join("ranki/config.toml");
        assert_eq!(
            super::ui_state_path(&config),
            dir.path().join("ranki/ui-state.toml")
        );
        let state = UiState {
            view: ranki_model::UiView::Browse,
            nav_width: Some(140.0),
            expanded_decks: vec![String::from("Topic")],
            ..UiState::default()
        };
        super::write_ui_state(&config, &state).expect("write ui state");
        assert_eq!(super::read_ui_state(&config), state);
        fs::write(super::ui_state_path(&config), "view = 'nope'")
            .expect("write invalid ui state");
        assert_eq!(super::read_ui_state(&config), UiState::default());
    }

    #[test]
    fn config_path_falls_back_to_home() {
        let path = config_path(None, None, Some(OsStr::new("/home/user")));
        assert_eq!(
            path.as_deref(),
            Some(Path::new("/home/user/.config/ranki/config.toml"))
        );
        assert_eq!(config_path(None, None, None), None);
    }

    #[test]
    fn data_path_prefers_an_explicit_directory() {
        let path = data_dir_path(
            Some(OsStr::new("/app-data")),
            Some(OsStr::new("/data")),
            Some(OsStr::new("/home/user")),
        );
        assert_eq!(path.as_deref(), Some(Path::new("/app-data")));
    }

    #[test]
    fn data_path_prefers_xdg_data_home() {
        let path = data_dir_path(
            None,
            Some(OsStr::new("/data")),
            Some(OsStr::new("/home/user")),
        );
        assert_eq!(path.as_deref(), Some(Path::new("/data/ranki")));
    }

    #[test]
    fn data_path_falls_back_to_home() {
        let path = data_dir_path(None, None, Some(OsStr::new("/home/user")));
        assert_eq!(
            path.as_deref(),
            Some(Path::new("/home/user/.local/share/ranki"))
        );
        assert_eq!(data_dir_path(None, None, None), None);
    }

    #[test]
    fn resolved_settings_keep_app_owned_paths() {
        let paths = DataPaths::new("/data/ranki");
        let settings = resolve_settings(
            &cli(&["ranki", "mcp"]),
            &Config::default(),
            paths.clone(),
        );
        assert_eq!(settings.paths, paths);
    }
    #[test]
    fn sync_cli_flags_override_config() {
        let mut config = Config::default();
        config.sync.enabled = true;
        config.sync.auto = true;
        config.sync.username = Some(String::from("config-user"));
        let settings = resolve_settings(
            &cli(&[
                "ranki",
                "--no-sync",
                "--sync-username",
                "cli-user",
                "--sync-hkey-file",
                "/keys/ranki",
                "decks",
            ]),
            &config,
            DataPaths::default(),
        );
        assert!(!settings.sync.enabled);
        assert!(settings.sync.auto);
        assert_eq!(settings.sync.username.as_deref(), Some("cli-user"));
        assert_eq!(
            settings.sync.hkey_file.as_deref(),
            Some(Path::new("/keys/ranki"))
        );

        let settings = resolve_settings(
            &cli(&["ranki", "--sync", "--no-sync-auto", "decks"]),
            &config,
            DataPaths::default(),
        );
        assert!(settings.sync.enabled);
        assert!(!settings.sync.auto);
    }

    #[test]
    fn study_settings_are_written_into_config() {
        let mut config = Config::default();
        let mut settings = Settings::default();
        settings.study.leech_enabled = false;
        settings.study.again = String::from("a");
        settings.study.hard = String::from("h");
        settings.study.good = String::from("g");
        settings.study.easy = String::from("e");

        assert!(apply_study_settings(&mut config, &settings));
        assert_eq!(config.study, settings.study);
        assert!(!apply_study_settings(&mut config, &settings));
    }
}
