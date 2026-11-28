#![cfg(not(any(target_os = "android", target_os = "ios")))]

//! Command line entry point for Ranki.

mod config;
mod connect;
mod error;
mod mcp;
mod optimize;
mod sync;

use clap::Parser;
use clap::Subcommand;
use ranki_core::FsrsScheduler;
use ranki_format::ExportOptions;
use ranki_format::UpdateCondition;
use ranki_format::export_apkg_with_options;
use ranki_format::export_colpkg_with_options;
use ranki_format::restore_colpkg;
use ranki_model::Id;
use ranki_model::NewNote;
use ranki_model::Rating;
use ranki_model::ReviewDecision;
use ranki_model::StockModelKind;
use ranki_model::Store;
use ranki_model::occlusion::{
    MaskShape, NormalizedRect, OcclusionMask, occlusion_field,
};
use ranki_model::parse_image_occlusion_field;
use ranki_storage::Collection;
#[cfg(feature = "desktop")]
use ranki_storage::ReopeningCollection;
use ranki_sync::SyncOutcome;
use ranki_sync::SyncTransport;
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fmt::Display;
use std::fs;
use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(feature = "desktop")]
use std::thread;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use thiserror::Error;

/// Errors surfaced by the binary and its two built-in interfaces.
#[derive(Debug, Error)]
pub(crate) enum CliError {
    #[error("{0}")]
    Message(String),
    #[error("storage: {0}")]
    Store(#[from] ranki_model::StoreError),
    #[error("format: {0}")]
    Format(#[from] ranki_format::FormatError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

impl From<CliError> for String {
    fn from(error: CliError) -> Self {
        error.to_string()
    }
}

pub(crate) type CliResult<T> = Result<T, CliError>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelRef {
    Id(Id),
    Name(String),
}

impl FromStr for ModelRef {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim().is_empty() {
            return Err("model cannot be empty".to_owned());
        }
        Ok(value
            .parse::<Id>()
            .map_or_else(|_| Self::Name(value.to_owned()), Self::Id))
    }
}

impl PartialEq<i64> for ModelRef {
    fn eq(&self, other: &i64) -> bool {
        matches!(self, Self::Id(id) if id == other)
    }
}

impl Display for ModelRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(formatter, "{id}"),
            Self::Name(name) => write!(formatter, "{name}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DeckRef {
    Id(Id),
    Name(String),
}

impl FromStr for DeckRef {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim().is_empty() {
            return Err("deck cannot be empty".to_owned());
        }
        Ok(value
            .parse::<Id>()
            .map_or_else(|_| Self::Name(value.to_owned()), Self::Id))
    }
}

impl Display for DeckRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(formatter, "{id}"),
            Self::Name(name) => write!(formatter, "{name}"),
        }
    }
}
#[derive(Debug, Parser)]
#[command(name = "ranki", version, about = "An Anki-compatible flashcard app")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mutually exclusive sync flags map directly onto CLI switches"
)]
struct Cli {
    /// The benchmark harness is shared with older commits that require this
    /// flag, so it parses here. It must name the collection inside the data
    /// directory and never selects storage.
    #[arg(
        long = "collection",
        global = true,
        hide = true,
        value_name = "PATH"
    )]
    bench_collection: Option<PathBuf>,
    #[arg(long, global = true)]
    mobile: bool,
    /// Turn syncing with `AnkiWeb` on for this command.
    #[arg(long, global = true, conflicts_with = "no_sync")]
    sync: bool,
    /// Turn syncing with `AnkiWeb` off for this command.
    #[arg(long, global = true, conflicts_with = "sync")]
    no_sync: bool,
    /// Sync at startup and shutdown when sync is enabled.
    #[arg(long, global = true, conflicts_with = "no_sync_auto")]
    sync_auto: bool,
    /// Skip the automatic startup and shutdown sync.
    #[arg(long, global = true, conflicts_with = "sync_auto")]
    no_sync_auto: bool,
    /// Use this `AnkiWeb` username instead of the configured one.
    #[arg(long, global = true, value_name = "USER")]
    sync_username: Option<String>,
    /// Read the `AnkiWeb` host key from this file.
    #[arg(long, global = true, value_name = "PATH")]
    sync_hkey_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[cfg(feature = "desktop")]
    /// Open the review window.
    Gui,
    /// List decks in the collection.
    Decks,
    /// Create a deck.
    DeckCreate { name: String },
    /// Rename a deck and its subdecks.
    DeckRename { id: Id, name: String },
    /// Remove a deck and its subdecks.
    DeckRemove {
        id: Id,
        /// Remove decks that contain cards.
        #[arg(long)]
        force: bool,
    },
    /// Show cards that are due.
    Due {
        #[arg(long, value_name = "ID_OR_NAME")]
        deck: Option<DeckRef>,
        /// Maximum number of cards to show (default: 20).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// Find cards with an Anki-style query.
    Search {
        /// Supports `deck:`, `tag:`, `note:`, `is:`, quoted text, and `*`.
        ///
        /// Prints one card id per line in search order.
        query: String,
    },
    /// Inspect a card, its note, and its field values.
    Show { card_id: Id },
    /// Add a note and its cards.
    AddNote {
        #[arg(
            long = "model",
            visible_alias = "model-id",
            value_name = "ID_OR_NAME"
        )]
        model: ModelRef,
        #[arg(
            long = "deck",
            visible_alias = "deck-id",
            value_name = "ID_OR_NAME"
        )]
        deck: DeckRef,
        /// Note fields in the model's field order; repeat once per field.
        #[arg(long = "field", value_name = "TEXT")]
        fields: Vec<String>,
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },

    /// Edit a note's fields, tags, and deck in place.
    NoteEdit {
        note_id: Id,
        /// Note fields in the model's field order; repeat once per field.
        #[arg(long = "field", value_name = "TEXT", required = true)]
        fields: Vec<String>,
        /// Replace all tags; repeat once per tag.
        #[arg(long = "tag", value_name = "TAG", conflicts_with = "clear_tags")]
        tags: Vec<String>,
        /// Remove all tags.
        #[arg(long)]
        clear_tags: bool,
        /// Move the note's cards to this deck.
        #[arg(long, visible_alias = "deck-id", value_name = "ID_OR_NAME")]
        deck: Option<DeckRef>,
        /// Reject the edit if the note changed since this modification time.
        #[arg(long, value_name = "SECONDS")]
        expected_modified: Option<i64>,
    },
    /// Add a built-in Image Occlusion note from an image and cloze markers.
    AddImageOcclusion {
        #[arg(long = "deck", value_name = "ID")]
        deck_id: Id,
        #[arg(long, value_name = "PATH")]
        image: PathBuf,
        #[arg(
            long = "occlusion",
            value_name = "MARKER",
            required_unless_present_any = ["rects", "ellipses"],
            conflicts_with_all = ["rects", "ellipses"],
        )]
        occlusions: Vec<String>,
        #[arg(
            long = "rect",
            value_name = "LEFT,TOP,WIDTH,HEIGHT",
            value_parser = parse_rect
        )]
        rects: Vec<MaskRect>,
        #[arg(
            long = "ellipse",
            value_name = "LEFT,TOP,WIDTH,HEIGHT",
            value_parser = parse_rect
        )]
        ellipses: Vec<MaskRect>,
        #[arg(long, value_name = "TEXT")]
        header: Option<String>,
        #[arg(long = "back-extra", value_name = "TEXT")]
        back_extra: Option<String>,
        #[arg(long, value_name = "TEXT")]
        comments: Option<String>,
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
    },
    /// List note models available in the collection, including stock types.
    Models,
    /// Answer a card with 1-4 or again, hard, good, or easy.
    Answer {
        card_id: Id,
        #[arg(value_parser = parse_rating)]
        rating: Rating,
    },
    /// Show card counts for a deck.
    Stats {
        #[arg(long, value_name = "ID_OR_NAME")]
        deck: Option<DeckRef>,
    },
    /// Import an Anki package into the collection.
    Import {
        package: PathBuf,
        /// Update matching notes: if-newer, always, or never.
        #[arg(
            long,
            value_name = "WHEN",
            default_value = "if-newer",
            value_parser = parse_update_condition
        )]
        update_notes: UpdateCondition,
        /// Update matching note types: if-newer, always, or never.
        #[arg(
            long,
            value_name = "WHEN",
            default_value = "if-newer",
            value_parser = parse_update_condition
        )]
        update_notetypes: UpdateCondition,
        /// Merge fields and templates of incompatible same-id note types.
        #[arg(long)]
        merge_notetypes: bool,
        /// Skip imported card scheduling and review history.
        #[arg(long)]
        no_scheduling: bool,
        /// Keep the target's deck presets instead of importing new ones.
        #[arg(long)]
        no_deck_configs: bool,
    },
    /// Export the collection as an Anki package.
    Export {
        package: PathBuf,
        /// Write a legacy package instead of the latest format.
        #[arg(long)]
        legacy: bool,
        /// Do not include scheduling information.
        #[arg(long)]
        no_scheduling: bool,
        /// Do not include deck configurations.
        #[arg(long)]
        no_deck_configs: bool,
        /// Do not include media files.
        #[arg(long)]
        no_media: bool,
        /// Overwrite an existing package.
        #[arg(long)]
        force: bool,
    },
    /// Export the collection as an Anki colpkg.
    ExportColpkg {
        package: PathBuf,
        /// Write a legacy package instead of the latest format.
        #[arg(long)]
        legacy: bool,
        /// Do not include scheduling information.
        #[arg(long)]
        no_scheduling: bool,
        /// Do not include deck configurations.
        #[arg(long)]
        no_deck_configs: bool,
        /// Do not include media files.
        #[arg(long)]
        no_media: bool,
        /// Overwrite an existing package.
        #[arg(long)]
        force: bool,
    },
    /// Check that the collection can be read.
    Check,
    /// Serve the AnkiConnect-compatible JSON-RPC API.
    Connect {
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
    /// Sync with `AnkiWeb` or show remote status.
    Sync {
        /// Show remote metadata without changing either collection.
        #[arg(long, conflicts_with_all = ["full_upload", "full_download"])]
        status: bool,
        /// Replace the remote collection with the local one.
        #[arg(long, conflicts_with_all = ["status", "full_download"])]
        full_upload: bool,
        /// Replace the local collection with the remote one.
        #[arg(long, conflicts_with_all = ["status", "full_upload"])]
        full_download: bool,
        /// Confirm a full upload or full download.
        #[arg(long)]
        yes: bool,
    },
    /// Serve the Model Context Protocol over stdio.
    Mcp,
}

fn parse_update_condition(value: &str) -> Result<UpdateCondition, String> {
    value.parse()
}

fn parse_rating(value: &str) -> Result<Rating, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "again" => return Ok(Rating::Again),
        "hard" => return Ok(Rating::Hard),
        "good" => return Ok(Rating::Good),
        "easy" => return Ok(Rating::Easy),
        _ => {}
    }
    let code = value.parse::<u8>().map_err(|_| invalid_rating(value))?;
    Rating::from_code(code).ok_or_else(|| invalid_rating(value))
}

fn invalid_rating(value: &str) -> String {
    format!(
        "invalid rating '{value}', expected 1, 2, 3, 4, again, hard, good, or easy"
    )
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .without_time()
        .init();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ranki: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> CliResult<()> {
    let mut config = config::load();
    let mut transport = sync::lazy_http_transport(&config);
    let data_dir = config::data_dir_path(
        env::var_os("RANKI_DATA_DIR").as_deref(),
        env::var_os("XDG_DATA_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
    .ok_or_else(|| {
        CliError::Message(
            "cannot resolve Ranki's data directory; set RANKI_DATA_DIR or HOME"
                .into(),
        )
    })?;
    let paths = ranki_model::DataPaths::new(data_dir);
    if let Some(requested) = cli.bench_collection.as_deref()
        && requested != paths.collection
    {
        return Err(CliError::Message(format!(
            "--collection is reserved for the benchmark harness and must be {}",
            paths.collection.display()
        )));
    }
    run_with_sync(cli, &mut config, &mut transport, paths)
}

/// CLI entry point with an injectable sync transport. Tests never reach HTTP.
#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::excessive_nesting,
    reason = "CLI dispatch maps each subcommand once and takes injected test seams"
)]
fn run_with_sync(
    cli: Cli,
    config: &mut config::Config,
    transport: &mut dyn SyncTransport,
    paths: ranki_model::DataPaths,
) -> CliResult<()> {
    if matches!(
        &cli.command,
        Some(
            Command::Sync {
                full_upload: true,
                ..
            } | Command::Sync {
                full_download: true,
                ..
            },
        )
    ) && !matches!(&cli.command, Some(Command::Sync { yes: true, .. }))
    {
        return Err(CliError::Message(String::from(
            "sync: --full-upload and --full-download require --yes",
        )));
    }
    let settings = config::resolve_settings(&cli, config, paths);
    let manual_sync = matches!(cli.command, Some(Command::Sync { .. }));
    let (auth, auto_sync_enabled) =
        sync::startup_credentials(&settings, manual_sync);
    let auto_sync =
        auto_sync_enabled && automatic_sync_allowed(cli.command.as_ref());

    let scheduler = ranki_core::from_settings(&settings);
    let collection_path = settings.paths.collection.as_path();
    let media_dir = settings.paths.media_dir.as_path();
    #[cfg(feature = "desktop")]
    let command = cli.command.unwrap_or(Command::Gui);
    #[cfg(not(feature = "desktop"))]
    let command = cli.command.unwrap_or(Command::Decks);
    #[cfg(feature = "desktop")]
    let defer_startup_sync = matches!(&command, Command::Gui)
        && !cli.mobile
        && auth.is_none()
        && sync::has_stored_key(&settings);
    #[cfg(not(feature = "desktop"))]
    let defer_startup_sync = false;

    let mut startup_sync = if should_startup_sync(auto_sync, defer_startup_sync)
    {
        sync_once_and_warn(transport, &settings, auth.as_ref(), true)
    } else {
        None
    };
    if startup_sync.is_some() {
        maybe_optimize(config, &settings);
    }
    let startup_sync_result = Arc::new(Mutex::new(None::<SyncOutcome>));
    #[cfg(feature = "desktop")]
    let mut final_settings = settings.clone();
    #[cfg(not(feature = "desktop"))]
    let final_settings = settings.clone();
    #[cfg(feature = "desktop")]
    let warn_shutdown_sync = !matches!(&command, Command::Gui);
    #[cfg(not(feature = "desktop"))]
    let warn_shutdown_sync = true;
    let restore_command = matches!(&command, Command::Import { package, .. } if is_colpkg(package));
    let result = match command {
        Command::Mcp => mcp::serve(&settings.paths, scheduler),
        #[cfg(feature = "desktop")]
        Command::Gui => {
            match open_gui(
                &settings,
                config,
                cli.mobile,
                defer_startup_sync,
                &startup_sync_result,
            ) {
                Ok(ui_settings) => {
                    if let Some(settings_from_ui) = ui_settings {
                        final_settings = settings_from_ui;
                    }
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
        Command::Import {
            package,
            update_notes,
            update_notetypes,
            merge_notetypes,
            no_scheduling,
            no_deck_configs,
        } => {
            let result = if is_colpkg(&package) {
                restore_package(
                    collection_path,
                    media_dir,
                    &settings.paths.sync_state,
                    &package,
                )
            } else {
                let options = ranki_format::ImportOptions {
                    update_notes,
                    update_notetypes,
                    merge_notetypes,
                    with_scheduling: !no_scheduling,
                    with_deck_configs: !no_deck_configs,
                };
                import_package(collection_path, media_dir, &package, options)
            };
            if result.is_ok() {
                maybe_optimize(config, &settings);
            }
            result
        }
        Command::Export {
            package,
            legacy,
            no_scheduling,
            no_deck_configs,
            no_media,
            force,
        } => export_package(
            collection_path,
            media_dir,
            &package,
            ExportOptions {
                legacy,
                with_scheduling: !no_scheduling,
                with_deck_configs: !no_deck_configs,
                with_media: !no_media,
                overwrite: force,
            },
        ),
        Command::ExportColpkg {
            package,
            legacy,
            no_scheduling,
            no_deck_configs,
            no_media,
            force,
        } => export_colpkg_package(
            collection_path,
            media_dir,
            &package,
            ExportOptions {
                legacy,
                with_scheduling: !no_scheduling,
                with_deck_configs: !no_deck_configs,
                with_media: !no_media,
                overwrite: force,
            },
        ),
        Command::Check => check(collection_path),
        Command::Connect { port } => {
            let collection = open_or_create(collection_path)?;
            drop(collection);
            eprintln!("listening on 127.0.0.1:{port}");
            connect::serve(collection_path, port, media_dir, &scheduler)
        }
        Command::Sync {
            status,
            full_upload,
            full_download,
            ..
        } => {
            let mode = if status {
                sync::SyncMode::Status
            } else if full_upload {
                sync::SyncMode::FullUpload
            } else if full_download {
                sync::SyncMode::FullDownload
            } else {
                sync::SyncMode::Safe
            };
            let result = sync::manual_sync(
                sync::TransportRef(transport),
                &settings,
                auth.as_ref(),
                mode,
            );
            if result.is_ok() && mode != sync::SyncMode::Status {
                maybe_optimize(config, &settings);
            }
            result
                .map(|text| println!("{text}"))
                .map_err(|error| CliError::Message(format!("sync: {error}")))
        }
        Command::Decks
        | Command::DeckCreate { .. }
        | Command::DeckRename { .. }
        | Command::DeckRemove { .. }
        | Command::Due { .. }
        | Command::Search { .. }
        | Command::Show { .. }
        | Command::AddNote { .. }
        | Command::NoteEdit { .. }
        | Command::AddImageOcclusion { .. }
        | Command::Models
        | Command::Answer { .. }
        | Command::Stats { .. } => {
            let mut collection = open_or_create(collection_path)?;
            run_store_command(&mut collection, command, &scheduler, media_dir)
        }
    };
    if defer_startup_sync && let Ok(guard) = startup_sync_result.lock() {
        startup_sync.clone_from(&guard);
    }
    let (shutdown_auth, shutdown_sync) = if auto_sync
        && !restore_command
        && final_settings.sync.enabled
        && final_settings.sync.auto
    {
        sync::startup_credentials(&final_settings, false)
    } else {
        (None, false)
    };
    let startup_settled = startup_sync.as_ref().is_some_and(|outcome| {
        !matches!(outcome, SyncOutcome::RetryLater { .. })
    });
    if shutdown_sync && startup_settled {
        let _ = sync_once_and_warn(
            transport,
            &final_settings,
            shutdown_auth.as_ref(),
            warn_shutdown_sync,
        );
    }
    result
}

const fn automatic_sync_allowed(command: Option<&Command>) -> bool {
    !matches!(command, Some(Command::Mcp))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the transport, settings, credentials, and warning flag are distinct inputs"
)]
fn sync_once_and_warn(
    transport: &mut dyn SyncTransport,
    settings: &config::Settings,
    auth: Option<&sync::SyncAuth>,
    warn: bool,
) -> Option<SyncOutcome> {
    match sync::sync_once(sync::TransportRef(transport), settings, auth) {
        Ok(outcome) => {
            if warn {
                sync::warn_outcome(&outcome);
            }
            Some(outcome)
        }
        Err(error) => {
            if warn {
                eprintln!("ranki: sync: {error}");
            }
            None
        }
    }
}
/// The window runs its own background sync when its credentials are in the state file.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "auto-sync and deferred startup are separate inputs"
)]
const fn should_startup_sync(auto_sync: bool, deferred: bool) -> bool {
    auto_sync && !deferred
}

fn maybe_optimize(config: &mut config::Config, settings: &config::Settings) {
    if !config.scheduler.auto_optimize {
        return;
    }
    let Ok(now) = now_secs() else {
        return;
    };
    if !settings.paths.collection.exists() {
        return;
    }
    let Ok(collection) = Collection::open(&settings.paths.collection) else {
        eprintln!("ranki: auto-optimize: cannot open the collection");
        return;
    };
    let before = config.clone();
    let Ok(timing) = collection.timing(now) else {
        eprintln!("ranki: auto-optimize: cannot read collection timing");
        return;
    };
    optimize::maybe_auto_optimize(&collection, config, &timing);
    if *config != before
        && let Err(error) = config::save(config)
    {
        eprintln!("ranki: config: cannot save: {error}");
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "store dispatch maps each subcommand once and takes injected test seams"
)]
fn run_store_command(
    collection: &mut Collection,
    command: Command,
    scheduler: &FsrsScheduler,
    media_dir: &Path,
) -> CliResult<()> {
    match command {
        Command::Decks => {
            for deck in collection.decks()? {
                println!("{}\t{}", deck.id, deck.name);
            }
            Ok(())
        }
        Command::DeckCreate { .. }
        | Command::DeckRename { .. }
        | Command::DeckRemove { .. } => run_deck_command(collection, command),
        Command::Due { deck, limit } => {
            print_due(collection, deck.as_ref(), limit)
        }
        Command::Search { query } => print_search(collection, &query),
        Command::Show { card_id } => print_card(collection, card_id),
        Command::AddNote {
            model,
            deck,
            fields,
            tags,
        } => {
            let model_id = resolve_model(collection, &model)?;
            let deck_id = resolve_deck(collection, &deck)?;
            let model = collection
                .models()?
                .into_iter()
                .find(|entry| entry.id == model_id)
                .ok_or_else(|| {
                    CliError::Message(format!("model not found: {model_id}"))
                })?;
            if fields.len() != model.fields.len() {
                return Err(CliError::Message(format!(
                    "{} fields for {}; expected {} in order: {}",
                    fields.len(),
                    model.name,
                    model.fields.len(),
                    model.fields.join(", ")
                )));
            }
            let note = NewNote {
                model_id,
                deck_id,
                fields,
                tags,
            };
            for id in collection.add_note(&note)? {
                println!("{id}");
            }
            Ok(())
        }
        Command::NoteEdit {
            note_id,
            fields,
            tags,
            clear_tags,
            deck,
            expected_modified,
        } => print_note_edit(
            collection,
            note_id,
            fields,
            tags,
            clear_tags,
            deck.as_ref(),
            expected_modified,
        ),
        Command::AddImageOcclusion {
            deck_id,
            image,
            occlusions,
            rects,
            ellipses,
            header,
            back_extra,
            comments,
            tags,
        } => add_image_occlusion(
            collection,
            media_dir,
            deck_id,
            &image,
            &occlusions,
            &shapes(&rects, &ellipses),
            header.as_deref(),
            back_extra.as_deref(),
            comments.as_deref(),
            &tags,
        ),
        Command::Models => print_models(collection),
        Command::Answer { card_id, rating } => {
            print_answer(collection, card_id, rating, scheduler)
        }
        Command::Stats { deck } => print_stats(collection, deck.as_ref()),
        #[cfg(feature = "desktop")]
        Command::Gui => Ok(()),
        Command::Import { .. }
        | Command::Export { .. }
        | Command::ExportColpkg { .. }
        | Command::Check
        | Command::Connect { .. }
        | Command::Sync { .. }
        | Command::Mcp => Ok(()),
    }
}

fn print_due(
    collection: &Collection,
    deck: Option<&DeckRef>,
    limit: Option<usize>,
) -> CliResult<()> {
    let deck_id = deck
        .map(|selected| resolve_deck(collection, selected))
        .transpose()?;
    let cards =
        collection.due_cards(deck_id, limit.unwrap_or(20), now_secs()?)?;
    for card in cards {
        println!("{}\t{}\t{}", card.id, card.deck_id, card.due);
    }
    Ok(())
}

fn print_search(collection: &Collection, query: &str) -> CliResult<()> {
    for id in collection.find_cards(query)? {
        println!("{id}");
    }
    Ok(())
}

fn print_card(collection: &Collection, card_id: Id) -> CliResult<()> {
    let card = collection.card(card_id)?.ok_or_else(|| {
        CliError::Message(format!("card not found: {card_id}"))
    })?;
    let note = collection.note(card.note_id)?.ok_or_else(|| {
        CliError::Message(format!("note not found: {}", card.note_id))
    })?;
    let deck = deck_by_id(collection, card.deck_id)?;
    let model = collection
        .models()?
        .into_iter()
        .find(|model| model.id == note.model_id)
        .ok_or_else(|| {
            CliError::Message(format!("model not found: {}", note.model_id))
        })?;
    println!(
        "card\t{}\tnote\t{}\tdeck\t{}\tordinal\t{}\tdue\t{}",
        card.id, card.note_id, card.deck_id, card.ordinal, card.due
    );
    println!("deck\t{}\t{}", deck.id, deck.name);
    println!(
        "note\t{}\tmodel\t{}\t{}\tmodified\t{}",
        note.id, model.id, model.name, note.modified
    );
    for (name, value) in model.fields.iter().zip(&note.fields) {
        println!("field\t{name}\t{value}");
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "editing maps one argument per note-edit option"
)]
fn print_note_edit(
    collection: &mut Collection,
    note_id: Id,
    fields: Vec<String>,
    tags: Vec<String>,
    clear_tags: bool,
    deck: Option<&DeckRef>,
    expected_modified: Option<i64>,
) -> CliResult<()> {
    let mut note = collection.note(note_id)?.ok_or_else(|| {
        CliError::Message(format!("note not found: {note_id}"))
    })?;
    if expected_modified.is_some_and(|expected| expected != note.modified) {
        return Err(CliError::Message(String::from(
            "note changed; read it again",
        )));
    }
    let Some(model) = collection
        .models()?
        .into_iter()
        .find(|model| model.id == note.model_id)
    else {
        return Err(CliError::Message(format!(
            "model not found: {}",
            note.model_id
        )));
    };
    if fields.len() != model.fields.len() {
        return Err(CliError::Message(format!(
            "{} fields for {}; expected {} in order: {}",
            fields.len(),
            model.name,
            model.fields.len(),
            model.fields.join(", ")
        )));
    }
    if fields.first().is_none_or(|field| field.trim().is_empty()) {
        return Err(CliError::Message(String::from(
            "first field must not be empty",
        )));
    }
    note.fields = fields;
    if clear_tags {
        note.tags.clear();
    } else if !tags.is_empty() {
        note.tags = tags;
    }
    let deck_id = deck
        .as_ref()
        .map(|selected| resolve_deck(collection, selected))
        .transpose()?;
    collection.update_note_checked(&note, deck_id, expected_modified)?;
    let modified = collection
        .note(note_id)?
        .map(|saved| saved.modified)
        .ok_or_else(|| {
            CliError::Message(format!("note not found: {note_id}"))
        })?;
    println!("edited\t{note_id}\t{modified}");
    Ok(())
}

fn print_models(collection: &Collection) -> CliResult<()> {
    let models = collection.models()?;
    for model in &models {
        print_model_line(&model.id.to_string(), model);
    }
    let installed_names: Vec<String> = models
        .iter()
        .map(|model| model.name.to_ascii_lowercase())
        .collect();
    let installed_kinds: Vec<StockModelKind> =
        models.iter().filter_map(StockModelKind::of).collect();
    for kind in StockModelKind::ALL {
        let name = kind.name();
        if installed_names
            .iter()
            .any(|installed| installed == &name.to_ascii_lowercase())
            || installed_kinds.contains(&kind)
        {
            continue;
        }
        let model = kind.definition();
        print_model_line("stock", &model);
    }
    Ok(())
}

fn print_model_line(id: &str, model: &ranki_model::NoteModel) {
    print!(
        "{id}\t{}\t{}\t{}",
        model.name,
        model.fields.len(),
        model.templates.len()
    );
    for field in &model.fields {
        print!("\t{field}");
    }
    for template in &model.templates {
        print!("\t{}", template.name);
    }
    println!();
}

#[expect(
    clippy::too_many_arguments,
    reason = "answering needs the store, card, rating, and scheduler"
)]
fn print_answer(
    collection: &mut Collection,
    card_id: Id,
    rating: Rating,
    scheduler: &FsrsScheduler,
) -> CliResult<()> {
    let decision =
        answer_card(collection, card_id, rating, now_secs()?, scheduler, 0)?;
    println!("{}\t{}", card_id, decision.card.due);
    Ok(())
}

fn print_stats(
    collection: &Collection,
    deck: Option<&DeckRef>,
) -> CliResult<()> {
    let deck_id = deck
        .map(|selected| resolve_deck(collection, selected))
        .transpose()?;
    let stats = collection.stats(deck_id, now_secs()?)?;
    println!(
        "new={} learning={} review={} total={}",
        stats.new_cards,
        stats.learning_cards,
        stats.review_cards,
        stats.total_cards
    );
    Ok(())
}

#[cfg(feature = "desktop")]
#[cfg(feature = "desktop")]
#[expect(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "GUI startup owns settings, config, layout, and sync scheduling inputs"
)]
fn open_gui(
    settings: &config::Settings,
    config: &config::Config,
    mobile: bool,
    sync_at_startup: bool,
    startup_sync_result: &Arc<Mutex<Option<SyncOutcome>>>,
) -> CliResult<Option<config::Settings>> {
    if mobile {
        #[cfg(feature = "mobile")]
        {
            use ranki::mobile::platform;
            platform::run()
                .map_err(|error| CliError::Message(error.to_string()))?;
            return Ok(None);
        }
        #[cfg(not(feature = "mobile"))]
        {
            return Err(CliError::Message(String::from(
                "the mobile layout requires --features mobile",
            )));
        }
    }
    let collection = open_or_create(&settings.paths.collection)?;
    drop(collection);
    let store = ReopeningCollection::new(&settings.paths.collection);
    let mut options = settings.clone();
    options.ui_state = config::load_ui_state();
    let saved = Arc::new(Mutex::new(config.clone()));
    let optimize_config = Arc::clone(&saved);
    let optimize_settings = settings.clone();
    thread::spawn(move || {
        let Ok(mut guard) = optimize_config.lock() else {
            return;
        };
        maybe_optimize(&mut guard, &optimize_settings);
    });
    let latest = Arc::new(Mutex::new(None::<config::Settings>));
    let latest_for_saver = Arc::clone(&latest);
    let save_settings: ranki_ui::SettingsSaver = Arc::new(move |current| {
        {
            let mut latest_guard = latest_for_saver
                .lock()
                .map_err(|_| String::from("settings lock poisoned"))?;
            *latest_guard = Some(current.clone());
        }
        let state_result = config::save_ui_state(&current.ui_state);
        let mut config_guard = saved
            .lock()
            .map_err(|_| String::from("config lock poisoned"))?;
        let mut updated = config_guard.clone();
        let sync_changed = updated.sync.enabled != current.sync.enabled
            || updated.sync.username != current.sync.username;
        let study_changed = config::apply_study_settings(&mut updated, current);
        let config_result = if sync_changed || study_changed {
            updated.sync.enabled = current.sync.enabled;
            updated.sync.username.clone_from(&current.sync.username);
            config::save(&updated).map(|()| *config_guard = updated)
        } else {
            Ok(())
        };
        state_result
            .and(config_result)
            .map_err(|error| error.to_string())
    });
    ranki_ui::run(
        Box::new(store),
        options,
        save_settings,
        sync_at_startup,
        Arc::clone(startup_sync_result),
    );
    let latest_guard = latest.lock().map_err(|_| {
        CliError::Message(String::from("settings lock poisoned"))
    })?;
    Ok(latest_guard.clone())
}

#[expect(
    clippy::too_many_arguments,
    reason = "CLI import takes collection, media, package, and options"
)]
fn import_package(
    path: &Path,
    media_dir: &Path,
    package: &Path,
    options: ranki_format::ImportOptions,
) -> CliResult<()> {
    let report = ranki_format::import_apkg_with_options(
        package, path, media_dir, options,
    )?;
    println!(
        "imported notes={} (new={} updated={} duplicate={} conflicting={}) cards={} media={}",
        report.notes,
        report.note_outcomes.new,
        report.note_outcomes.updated,
        report.note_outcomes.duplicate,
        report.note_outcomes.conflicting,
        report.cards,
        report.media
    );
    Ok(())
}

fn is_colpkg(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("colpkg"))
}

#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, sync state, and package are separate inputs"
)]
fn restore_package(
    path: &Path,
    media_dir: &Path,
    sync_state: &Path,
    package: &Path,
) -> CliResult<()> {
    let report = restore_colpkg(package, path, media_dir)?;
    let backup = report.backup.clone();
    match &backup {
        Some(path) => println!(
            "restored media={} backup={}",
            report.media,
            path.display()
        ),
        None => println!("restored media={}", report.media),
    }
    if let Err(error) = invalidate_sync_state(sync_state) {
        eprintln!(
            "ranki: restore committed; sync state could not be invalidated: {error}"
        );
        if backup.is_some() {
            eprintln!(
                "ranki: do not retry the restore; the backup above holds the previous collection"
            );
        } else {
            eprintln!(
                "ranki: do not retry the restore; no previous collection was backed up"
            );
        }
        return Err(error);
    }
    Ok(())
}

fn invalidate_sync_state(path: &Path) -> CliResult<()> {
    if !path.is_file() {
        return Ok(());
    }
    let bytes = fs::read(path)?;
    let Ok(mut state) = serde_json::from_slice::<serde_json::Value>(&bytes)
    else {
        return Ok(());
    };
    if let Some(object) = state.as_object_mut() {
        let _ = object.insert("last_scm".to_owned(), serde_json::json!(0));
        let _ =
            object.insert("last_local_mod".to_owned(), serde_json::json!(0));
    }
    fs::write(path, serde_json::to_string(&state)?)?;
    Ok(())
}

type Exporter = fn(
    &Path,
    &Path,
    &Path,
    ExportOptions,
) -> Result<(), ranki_format::FormatError>;

#[expect(
    clippy::too_many_arguments,
    reason = "the export helper takes collection, media, package, options, and exporter"
)]
fn export_with_checkpoint(
    path: &Path,
    media_dir: &Path,
    package: &Path,
    options: ExportOptions,
    exporter: Exporter,
) -> CliResult<()> {
    if resolved_path(package) == resolved_path(path) {
        return Err(CliError::Message(format!(
            "export destination is the active collection: {}",
            package.display()
        )));
    }
    if fs::symlink_metadata(package).is_ok() && !options.overwrite {
        return Err(existing_export_error(package));
    }
    let collection = open_or_create(path)?;
    collection.checkpoint()?;
    collection.close()?;
    if let Err(error) = exporter(path, media_dir, package, options) {
        if !options.overwrite && fs::symlink_metadata(package).is_ok() {
            return Err(existing_export_error(package));
        }
        return Err(CliError::Message(error.to_string()));
    }
    println!("exported\t{}", package.display());
    Ok(())
}

fn existing_export_error(package: &Path) -> CliError {
    CliError::Message(format!(
        "export destination already exists: {}; pass --force to overwrite",
        package.display()
    ))
}

fn resolved_path(path: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(path) {
        return resolved;
    }
    let ancestor = path
        .ancestors()
        .find(|candidate| {
            candidate.as_os_str().is_empty()
                || fs::canonicalize(candidate).is_ok()
        })
        .unwrap_or_else(|| Path::new("."));
    let base = if ancestor.as_os_str().is_empty() {
        env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        fs::canonicalize(ancestor).unwrap_or_else(|_| ancestor.to_path_buf())
    };
    let suffix = path.strip_prefix(ancestor).unwrap_or(path);
    let mut resolved = base;
    for component in suffix.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(part) => resolved.push(part),
            Component::RootDir | Component::Prefix(_) => {
                resolved.push(component.as_os_str());
            }
        }
    }
    resolved
}

#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, package, and options are separate inputs"
)]
fn export_package(
    path: &Path,
    media_dir: &Path,
    package: &Path,
    options: ExportOptions,
) -> CliResult<()> {
    export_with_checkpoint(
        path,
        media_dir,
        package,
        options,
        export_apkg_with_options,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "collection, media, package, and options are separate inputs"
)]
fn export_colpkg_package(
    path: &Path,
    media_dir: &Path,
    package: &Path,
    options: ExportOptions,
) -> CliResult<()> {
    export_with_checkpoint(
        path,
        media_dir,
        package,
        options,
        export_colpkg_with_options,
    )
}

fn run_deck_command(
    collection: &mut Collection,
    command: Command,
) -> CliResult<()> {
    match command {
        Command::DeckCreate { name } => {
            let deck = collection.create_deck(&name)?;
            println!("created\t{}\t{}", deck.id, deck.name);
        }
        Command::DeckRename { id, name } => {
            let deck = collection.rename_deck(id, &name)?;
            println!("renamed\t{}\t{}", deck.id, deck.name);
        }
        Command::DeckRemove { id, force } => {
            let deck = deck_by_id(collection, id)?;
            require_force(collection, &deck, force)?;
            collection.remove_deck(id)?;
            println!("removed\t{}\t{}", deck.id, deck.name);
        }
        #[cfg(feature = "desktop")]
        Command::Gui => {}
        Command::Decks
        | Command::Due { .. }
        | Command::Search { .. }
        | Command::Show { .. }
        | Command::AddNote { .. }
        | Command::NoteEdit { .. }
        | Command::AddImageOcclusion { .. }
        | Command::Models
        | Command::Answer { .. }
        | Command::Stats { .. }
        | Command::Import { .. }
        | Command::Export { .. }
        | Command::ExportColpkg { .. }
        | Command::Check
        | Command::Connect { .. }
        | Command::Sync { .. }
        | Command::Mcp => {}
    }
    Ok(())
}

fn require_force(
    collection: &Collection,
    deck: &ranki_model::Deck,
    force: bool,
) -> CliResult<()> {
    if force {
        return Ok(());
    }
    let cards = collection.stats(Some(deck.id), now_secs()?)?.total_cards;
    if cards == 0 {
        return Ok(());
    }
    Err(CliError::Message(format!(
        "deck {} ({}) has {cards} cards; pass --force to remove it",
        deck.id, deck.name
    )))
}
fn deck_by_id(collection: &Collection, id: Id) -> CliResult<ranki_model::Deck> {
    collection
        .deck_list()?
        .into_iter()
        .find(|deck| deck.id == id)
        .ok_or_else(|| CliError::Message(format!("deck not found: {id}")))
}

fn resolve_deck(collection: &Collection, deck: &DeckRef) -> CliResult<Id> {
    let id = match deck {
        DeckRef::Id(id) => *id,
        DeckRef::Name(name) => collection
            .deck_list()?
            .into_iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .map(|entry| entry.id)
            .ok_or_else(|| {
                CliError::Message(format!("deck not found: {deck}"))
            })?,
    };
    if collection.deck_subtree(id)?.is_empty() {
        return Err(CliError::Message(format!("deck not found: {deck}")));
    }
    Ok(id)
}

fn resolve_model(
    collection: &mut Collection,
    model: &ModelRef,
) -> CliResult<Id> {
    match model {
        ModelRef::Id(id) => Ok(*id),
        ModelRef::Name(name) => {
            let existing = collection.models()?;
            if let Some(model) = existing
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(name))
            {
                return Ok(model.id);
            }
            let kind = StockModelKind::from_name(name).ok_or_else(|| {
                CliError::Message(format!("unknown note type: {name}"))
            })?;
            Ok(collection.ensure_model(kind)?.id)
        }
    }
}

/// `--rect`/`--ellipse` coordinates: normalized, comma separated.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MaskRect {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

fn parse_rect(value: &str) -> Result<MaskRect, String> {
    let parts = value
        .split(',')
        .map(|part| part.trim().parse::<f32>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            format!("expected LEFT,TOP,WIDTH,HEIGHT in 0..1, got {value}")
        })?;
    let [left, top, width, height] = parts.as_slice() else {
        return Err(format!(
            "expected LEFT,TOP,WIDTH,HEIGHT in 0..1, got {value}"
        ));
    };
    let inside = |number: &f32| (0.0..=1.0).contains(number);
    if !inside(left)
        || !inside(top)
        || !inside(width)
        || !inside(height)
        || *width <= 0.0
        || *height <= 0.0
    {
        return Err(format!("mask coordinates must stay in 0..1: {value}"));
    }
    Ok(MaskRect {
        left: *left,
        top: *top,
        width: *width,
        height: *height,
    })
}

/// Rectangles first, then ellipses, in the order the flags were given.
fn shapes(rects: &[MaskRect], ellipses: &[MaskRect]) -> Vec<OcclusionMask> {
    let rect = |mask: &MaskRect, shape: MaskShape| OcclusionMask {
        shape,
        rect: NormalizedRect {
            left: mask.left,
            top: mask.top,
            width: mask.width,
            height: mask.height,
        },
    };
    rects
        .iter()
        .map(|mask| rect(mask, MaskShape::Rect))
        .chain(ellipses.iter().map(|mask| rect(mask, MaskShape::Ellipse)))
        .collect()
}

fn occlusion_text(
    occlusions: &[String],
    masks: &[OcclusionMask],
) -> CliResult<String> {
    let built = occlusion_field(masks);
    let raw = occlusions.join("<br>");
    let occlusion = [built, raw]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("<br>");
    let parsed = parse_image_occlusion_field(&occlusion);
    if parsed.shapes.is_empty() {
        return Err(CliError::Message(
            "no image-occlusion markers found".to_owned(),
        ));
    }
    Ok(occlusion)
}

fn image_occlusion_model(collection: &mut Collection) -> CliResult<Id> {
    let model_id = resolve_model(
        collection,
        &ModelRef::Name("Image Occlusion".to_owned()),
    )?;
    let model = collection
        .models()?
        .into_iter()
        .find(|entry| entry.id == model_id)
        .ok_or_else(|| {
            CliError::Message("image occlusion model missing".into())
        })?;
    if model.fields.len() != 5 {
        return Err(CliError::Message(format!(
            "image occlusion model needs 5 fields, found {}",
            model.fields.len()
        )));
    }
    Ok(model_id)
}

fn safe_media_name(image: &Path) -> CliResult<&str> {
    let name = image
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| CliError::Message("invalid image file name".into()))?;
    if name.is_empty() || name.contains(['"', '<', '>']) {
        return Err(CliError::Message("unsafe image file name".into()));
    }
    Ok(name)
}

fn stage_image_temp(media_dir: &Path, image: &Path) -> CliResult<PathBuf> {
    let mut source = fs::File::open(image)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::Message(error.to_string()))?
        .as_nanos();
    for attempt in 0..100_u32 {
        let candidate = media_dir.join(format!(
            ".ranki-media-{}-{stamp}-{attempt}.tmp",
            process::id()
        ));
        let mut staged = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if let Err(error) = io::copy(&mut source, &mut staged) {
            let _ = fs::remove_file(&candidate);
            return Err(error.into());
        }
        return Ok(candidate);
    }
    Err(CliError::Message(
        "cannot allocate a temporary media file".into(),
    ))
}

fn reserve_media_name(
    media_dir: &Path,
    media_name: &str,
) -> CliResult<PathBuf> {
    let destination = media_dir.join(media_name);
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
    {
        Ok(_) => Ok(destination),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(CliError::Message(format!(
                "media file already exists: {media_name}"
            )))
        }
        Err(error) => Err(error.into()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the image occlusion command maps one flag per note field"
)]
fn add_image_occlusion(
    collection: &mut Collection,
    media_dir: &Path,
    deck_id: Id,
    image: &Path,
    occlusions: &[String],
    masks: &[OcclusionMask],
    header: Option<&str>,
    back_extra: Option<&str>,
    comments: Option<&str>,
    tags: &[String],
) -> CliResult<()> {
    let occlusion = occlusion_text(occlusions, masks)?;
    let model_id = image_occlusion_model(collection)?;
    let media_name = safe_media_name(image)?;
    fs::create_dir_all(media_dir)?;
    let reserved = reserve_media_name(media_dir, media_name)?;
    let staged = match stage_image_temp(media_dir, image) {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&reserved);
            return Err(error);
        }
    };
    let result = (|| {
        let fields = vec![
            occlusion,
            format!("<img src=\"{media_name}\">"),
            header.unwrap_or_default().to_owned(),
            back_extra.unwrap_or_default().to_owned(),
            comments.unwrap_or_default().to_owned(),
        ];
        let note = NewNote {
            model_id,
            deck_id,
            fields,
            tags: tags.to_vec(),
        };
        fs::copy(&staged, &reserved)?;
        let ids = collection.add_note(&note)?;
        let _ = fs::remove_file(&staged);
        for id in ids {
            println!("{id}");
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
        let _ = fs::remove_file(&reserved);
    }
    result
}

pub(crate) fn open_or_create(path: &Path) -> CliResult<Collection> {
    if path.exists() {
        return Collection::open(path).map_err(|error| {
            CliError::Message(format!(
                "cannot open collection {}: {error}",
                path.display()
            ))
        });
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(Collection::create(path)?)
}

fn unix_i64(value: u128) -> CliResult<i64> {
    i64::try_from(value)
        .map_err(|_| CliError::Message("timestamp too large".into()))
}

pub(crate) fn now_secs() -> CliResult<i64> {
    let pinned = env::var("RANKI_DEV_NOW_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok());
    if let Some(seconds) = pinned {
        return Ok(seconds);
    }
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::Message(error.to_string()))?;
    unix_i64(duration.as_secs().into())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the shared answer helper needs store, card, rating, time, and scheduler"
)]
pub(crate) fn answer_card<S: Store>(
    store: &mut S,
    card_id: Id,
    rating: Rating,
    now: i64,
    scheduler: &FsrsScheduler,
    answer_ms: u32,
) -> CliResult<ReviewDecision> {
    let card = store.card(card_id)?.ok_or_else(|| {
        CliError::Message(format!("card not found: {card_id}"))
    })?;
    let timing = store.timing(now)?;
    let last_review_secs = store.last_review_secs(card_id)?;
    let deck_scheduler =
        ranki_core::scheduler_for_card(scheduler, store, &card);
    let decision = deck_scheduler
        .schedule_timed(&card, rating, &timing, last_review_secs, answer_ms)
        .ok_or_else(|| {
            CliError::Message(format!("card {card_id} is not due"))
        })?;
    store.apply_review(&decision)?;
    Ok(decision)
}

fn check(path: &Path) -> CliResult<()> {
    let collection = open_or_create(path)?;
    collection.decks()?;
    collection.models()?;
    collection.stats(None, now_secs()?)?;
    collection.collection_modified()?;
    collection.checkpoint()?;
    collection.close()?;
    println!("ok");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use clap::Parser;
    use ranki_format::export_colpkg;
    use ranki_model::NewNote;
    use ranki_model::Store as _;
    use ranki_storage::Collection;
    use ranki_sync::MockResponse;
    use ranki_sync::MockTransport;
    use ranki_sync::SyncMethod;

    use super::Cli;
    use super::Command;
    use super::now_secs;
    use super::parse_rating;
    use super::run_with_sync;
    use super::should_startup_sync;
    use ranki_model::Rating;

    fn uploaded() -> MockResponse {
        MockResponse::ok_bytes(b"OK".to_vec()).unwrap()
    }

    fn remote_meta() -> MockResponse {
        MockResponse::ok_json(&serde_json::json!({
            "mod": 1_700_000_000_000_i64,
            "scm": 1_700_000_000_000_i64,
            "usn": 1,
            "ts": now_secs().unwrap(),
            "empty": false,
            "cont": true,
        }))
        .unwrap()
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn gui_startup_defers_auto_sync_to_the_window() {
        assert!(should_startup_sync(true, false));
        assert!(!should_startup_sync(true, true));
        assert!(!should_startup_sync(false, false));
    }

    #[test]
    fn auto_sync_runs_at_startup_and_rereads_state_after_the_command() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        assert!(crate::open_or_create(&paths.collection).is_ok());
        let mut config = ranki_model::Config::default();
        config.sync.enabled = true;
        config.scheduler.auto_optimize = false;
        let state = serde_json::json!({
            "version": 1,
            "endpoint": ranki_sync::DEFAULT_ENDPOINT,
            "host_key": "stored-hkey",
            "last_mod": null,
            "last_scm": 1_700_000_000_000_i64,
            "last_usn": 0,
            "last_media_usn": 0,
            "last_success": null,
            "failures": 0,
        });
        fs::write(&paths.sync_state, state.to_string()).unwrap();
        let response = MockResponse::ok_json(&serde_json::json!({
            "mod": 1_700_000_000_000_i64,
            "scm": 1_700_000_000_000_i64,
            "usn": 7,
            "ts": now_secs().unwrap(),
            "empty": true,
        }))
        .unwrap();
        let mut transport = MockTransport::new()
            .with_response(SyncMethod::Meta, response.clone())
            .with_response(SyncMethod::Meta, response);
        let cli = parse(&["ranki", "decks"]);
        run_with_sync(cli, &mut config, &mut transport, paths).unwrap();
        assert_eq!(transport.request_count(), 2);
        assert_eq!(
            transport
                .requests()
                .iter()
                .map(|request| request.method)
                .collect::<Vec<_>>(),
            vec![SyncMethod::Meta, SyncMethod::Meta]
        );
    }
    #[test]
    fn failed_startup_sync_does_not_retry_at_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        assert!(crate::open_or_create(&paths.collection).is_ok());
        let mut config = ranki_model::Config::default();
        config.sync.enabled = true;
        config.scheduler.auto_optimize = false;
        fs::write(
            &paths.sync_state,
            serde_json::json!({
                "version": 1,
                "endpoint": ranki_sync::DEFAULT_ENDPOINT,
                "host_key": "stored-hkey",
                "last_mod": null,
                "last_scm": 1_700_000_000_000_i64,
                "last_usn": 0,
                "last_media_usn": 0,
                "last_success": null,
                "failures": 0
            })
            .to_string(),
        )
        .unwrap();
        let mut transport = MockTransport::new()
            .with_failure(SyncMethod::Meta, ranki_sync::SyncError::Timeout)
            .with_retry_policy(ranki_sync::RetryPolicy {
                retries: 2,
                backoff: Duration::ZERO,
            });

        run_with_sync(
            parse(&["ranki", "decks"]),
            &mut config,
            &mut transport,
            paths,
        )
        .unwrap();

        assert_eq!(transport.request_count(), 3);
    }

    #[test]
    fn auto_sync_uploads_at_shutdown_after_a_command_changes_the_collection() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        assert!(crate::open_or_create(&paths.collection).is_ok());
        fs::write(
            &paths.sync_state,
            serde_json::json!({
                "version": 1,
                "endpoint": ranki_sync::DEFAULT_ENDPOINT,
                "host_key": "stored-hkey",
                "last_mod": null,
                "last_scm": null,
                "last_usn": 0,
                "last_media_usn": 0,
                "last_success": null,
                "failures": 0,
            })
            .to_string(),
        )
        .unwrap();
        let mut seed = ranki_sync::SyncClient::new(
            MockTransport::new()
                .with_response(SyncMethod::Upload, uploaded())
                .with_response(SyncMethod::Meta, remote_meta()),
            &paths.collection,
            &paths.media_dir,
            &paths.sync_state,
        )
        .unwrap();
        seed.full_upload().unwrap();
        let mut transport = MockTransport::new()
            .with_response(SyncMethod::Meta, remote_meta())
            .with_response(SyncMethod::Meta, remote_meta())
            .with_response(SyncMethod::Upload, uploaded())
            .with_response(SyncMethod::Meta, remote_meta());
        let mut config = ranki_model::Config::default();
        config.sync.enabled = true;
        config.scheduler.auto_optimize = false;
        let cli = parse(&[
            "ranki",
            "add-note",
            "--model",
            "Basic",
            "--deck",
            "1",
            "--field",
            "Question?",
            "--field",
            "Answer!",
        ]);
        run_with_sync(cli, &mut config, &mut transport, paths).unwrap();
        let methods: Vec<_> = transport
            .requests()
            .iter()
            .map(|request| request.method)
            .collect();
        assert_eq!(
            methods,
            vec![
                SyncMethod::Meta,
                SyncMethod::Meta,
                SyncMethod::Upload,
                SyncMethod::Meta,
            ]
        );
    }

    #[test]
    fn colpkg_restore_does_not_upload_at_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        assert!(crate::open_or_create(&paths.collection).is_ok());
        let source = directory.path().join("source.anki2");
        {
            let mut store = Collection::create(&source).unwrap();
            store
                .add_note(&NewNote {
                    model_id: ranki_model::DEFAULT_MODEL_ID,
                    deck_id: ranki_model::DEFAULT_DECK_ID,
                    fields: vec!["restore me".to_owned(), "back".to_owned()],
                    tags: Vec::new(),
                })
                .unwrap();
        }
        let package = directory.path().join("backup.colpkg");
        export_colpkg(&source, &paths.media_dir, &package).unwrap();
        fs::write(
            &paths.sync_state,
            serde_json::json!({
                "version": 1,
                "endpoint": ranki_sync::DEFAULT_ENDPOINT,
                "host_key": "stored-hkey",
                "last_mod": null,
                "last_scm": null,
                "last_usn": 0,
                "last_media_usn": 0,
                "last_success": null,
                "failures": 0,
            })
            .to_string(),
        )
        .unwrap();
        let mut seed = ranki_sync::SyncClient::new(
            MockTransport::new()
                .with_response(SyncMethod::Upload, uploaded())
                .with_response(SyncMethod::Meta, remote_meta()),
            &paths.collection,
            &paths.media_dir,
            &paths.sync_state,
        )
        .unwrap();
        seed.full_upload().unwrap();
        let mut transport = MockTransport::new()
            .with_response(SyncMethod::Meta, remote_meta())
            .with_response(SyncMethod::Meta, remote_meta())
            .with_response(SyncMethod::Meta, remote_meta());
        let sync_state = paths.sync_state.clone();
        let next_paths = paths.clone();
        let mut config = ranki_model::Config::default();
        config.sync.enabled = true;
        config.scheduler.auto_optimize = false;
        let cli = parse(&["ranki", "import", package.to_str().unwrap()]);
        run_with_sync(cli, &mut config, &mut transport, paths).unwrap();
        assert!(
            !transport
                .requests()
                .iter()
                .any(|request| request.method == SyncMethod::Upload)
        );
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(&sync_state).unwrap()).unwrap();
        assert_eq!(state["last_scm"], 0);
        assert_eq!(state["last_local_mod"], 0);

        let mut next_transport =
            MockTransport::new().with_response(SyncMethod::Meta, remote_meta());
        let mut next_config = config.clone();
        run_with_sync(
            parse(&["ranki", "decks"]),
            &mut next_config,
            &mut next_transport,
            next_paths,
        )
        .unwrap();
        assert!(
            !next_transport
                .requests()
                .iter()
                .any(|request| request.method == SyncMethod::Upload)
        );
    }

    #[test]
    fn sync_disabled_commands_never_use_the_transport() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        assert!(crate::open_or_create(&paths.collection).is_ok());
        let mut config = ranki_model::Config::default();
        fs::write(
            &paths.sync_state,
            r#"{"version":1,"host_key":"stored-hkey"}"#,
        )
        .unwrap();
        let mut transport = MockTransport::new();
        let cli = parse(&["ranki", "decks"]);
        run_with_sync(cli, &mut config, &mut transport, paths).unwrap();
        assert_eq!(transport.request_count(), 0);
    }

    #[test]
    fn no_subcommand_selects_the_default_command() {
        #[cfg(feature = "desktop")]
        assert!(matches!(
            parse(&["ranki"]).command,
            None | Some(Command::Gui)
        ));
        #[cfg(not(feature = "desktop"))]
        assert!(matches!(
            parse(&["ranki"]).command,
            None | Some(Command::Decks)
        ));
    }

    #[test]
    fn only_the_benchmark_collection_flag_parses() {
        assert!(
            Cli::try_parse_from([
                "ranki",
                "--collection",
                "/tmp/demo.anki2",
                "decks"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["ranki", "mcp", "--media-dir", "/tmp/media"])
                .is_err()
        );
    }

    #[test]
    fn add_note_collects_repeated_fields_and_tags() {
        let cli = parse(&[
            "ranki", "add-note", "--model", "1", "--deck", "2", "--field",
            "front", "--field", "back", "--tag", "one", "--tag", "two",
        ]);
        let Some(Command::AddNote {
            model,
            deck,
            fields,
            tags,
        }) = cli.command
        else {
            panic!("wrong command");
        };
        assert_eq!(model, 1);
        assert_eq!(deck, super::DeckRef::Id(2));
        assert_eq!(fields, ["front", "back"]);
        assert_eq!(tags, ["one", "two"]);
    }

    #[test]
    fn add_note_accepts_stock_model_names() {
        let cli = parse(&[
            "ranki",
            "add-note",
            "--model",
            "Basic (and reversed card)",
            "--deck",
            "1",
            "--field",
            "front",
            "--field",
            "back",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::AddNote {
                model: super::ModelRef::Name(name),
                ..
            }) if name == "Basic (and reversed card)"
        ));
    }

    #[test]
    fn add_image_occlusion_collects_markers_and_fields() {
        let cli = parse(&[
            "ranki",
            "add-image-occlusion",
            "--deck",
            "1",
            "--image",
            "/tmp/diagram.png",
            "--occlusion",
            "{{c1::image-occlusion:rect:left=.1:top=.2:width=.3:height=.4}}",
            "--occlusion",
            "{{c2::image-occlusion:ellipse:left=.5:top=.6:width=.2:height=.2}}",
            "--header",
            "Diagram",
            "--back-extra",
            "More",
            "--tag",
            "io",
        ]);
        let Some(Command::AddImageOcclusion {
            image,
            occlusions,
            header,
            back_extra,
            tags,
            ..
        }) = cli.command
        else {
            panic!("wrong command");
        };
        assert_eq!(image, Path::new("/tmp/diagram.png"));
        assert_eq!(occlusions.len(), 2);
        assert!(occlusions[1].contains("c2"));
        assert_eq!(header.as_deref(), Some("Diagram"));
        assert_eq!(back_extra.as_deref(), Some("More"));
        assert_eq!(tags, ["io"]);
    }

    #[test]
    fn add_image_occlusion_accepts_rects_and_ellipses() {
        let cli = parse(&[
            "ranki",
            "add-image-occlusion",
            "--deck",
            "1",
            "--image",
            "/tmp/diagram.png",
            "--rect",
            "0.1,0.2,0.3,0.4",
            "--ellipse",
            "0.5,0.5,0.2,0.2",
            "--rect",
            "0,0,1,1",
        ]);
        let Some(Command::AddImageOcclusion {
            rects,
            ellipses,
            occlusions,
            ..
        }) = cli.command
        else {
            panic!("wrong command");
        };
        assert_eq!(occlusions.len(), 0);
        assert_eq!(rects.len(), 2);
        assert_eq!(ellipses.len(), 1);
        let masks = super::shapes(&rects, &ellipses);
        assert_eq!(
            super::occlusion_field(&masks),
            "{{c1::image-occlusion:rect:left=0.1:top=0.2:width=0.3:height=0.4}}<br>\
             {{c2::image-occlusion:rect:left=0:top=0:width=1:height=1}}<br>\
             {{c3::image-occlusion:ellipse:left=0.5:top=0.5:width=0.2:height=0.2}}"
        );
        let parsed = ranki_model::parse_image_occlusion_field(
            &super::occlusion_field(&masks),
        );
        assert_eq!(parsed.shapes.len(), 3);
        assert_eq!(parsed.shapes[2].ordinals, [3]);
    }

    #[test]
    fn add_image_occlusion_rejects_out_of_range_rects() {
        assert!(super::parse_rect("0.1,0.2,0.3").is_err());
        assert!(super::parse_rect("0.1,0.2,0.3,1.4").is_err());
        assert!(super::parse_rect("0.1,0.2,0,0.3").is_err());
        assert!(super::parse_rect("left,top,width,height").is_err());
        assert_eq!(
            super::parse_rect(" 0.25 , 0.5, 0.125, 0.25 ").map(|mask| (
                mask.left,
                mask.top,
                mask.width,
                mask.height
            )),
            Ok((0.25, 0.5, 0.125, 0.25))
        );
    }

    #[test]
    fn add_image_occlusion_needs_markers_or_shapes() {
        assert_eq!(super::shapes(&[], &[]).len(), 0);
        assert_eq!(super::occlusion_field(&[]), "");
        assert!(
            Cli::try_parse_from([
                "ranki",
                "add-image-occlusion",
                "--deck",
                "1",
                "--image",
                "/tmp/diagram.png",
            ])
            .is_err()
        );
    }

    #[test]
    fn models_command_is_available() {
        assert!(matches!(
            parse(&["ranki", "models"]).command,
            Some(Command::Models)
        ));
    }

    #[test]
    fn due_accepts_optional_deck_and_limit() {
        let cli = parse(&["ranki", "due", "--deck", "7", "--limit", "5"]);
        assert!(matches!(
            cli.command,
            Some(Command::Due {
                deck: Some(super::DeckRef::Id(7)),
                limit: Some(5)
            })
        ));
    }

    #[test]
    fn deck_flags_accept_names_as_well_as_ids() {
        let cli = parse(&[
            "ranki",
            "add-note",
            "--model",
            "1",
            "--deck",
            "Study::Languages",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::AddNote {
                deck: super::DeckRef::Name(ref name),
                ..
            }) if name == "Study::Languages"
        ));
        let cli = parse(&["ranki", "stats", "--deck", "languages"]);
        assert!(matches!(
            cli.command,
            Some(Command::Stats {
                deck: Some(super::DeckRef::Name(ref name)),
            }) if name == "languages"
        ));
    }

    #[test]
    fn answer_accepts_digits_and_rating_words() {
        for (input, expected) in [
            ("1", Rating::Again),
            ("2", Rating::Hard),
            ("3", Rating::Good),
            ("4", Rating::Easy),
            ("again", Rating::Again),
            ("hard", Rating::Hard),
            ("GOOD", Rating::Good),
            (" easy ", Rating::Easy),
        ] {
            assert_eq!(parse_rating(input).unwrap(), expected);
        }
        assert!(parse_rating("0").is_err());
        assert!(parse_rating("5").is_err());
        assert!(parse_rating("nope").is_err());
    }

    #[test]
    fn connect_defaults_to_port_8765() {
        let cli = parse(&["ranki", "connect"]);
        assert!(matches!(cli.command, Some(Command::Connect { port: 8765 })));
    }

    #[test]
    fn data_layout_is_separate_from_anki() {
        let data_dir = super::config::data_dir_path(
            Some(OsStr::new("/app-data")),
            Some(OsStr::new("/data")),
            Some(OsStr::new("/home/user")),
        )
        .unwrap();
        assert_eq!(data_dir, Path::new("/app-data"));
        let paths = ranki_model::DataPaths::new(data_dir);
        assert_eq!(paths.collection, Path::new("/app-data/collection.anki2"));
        assert_eq!(paths.media_dir, Path::new("/app-data/collection.media"));
        assert_eq!(paths.sync_state, Path::new("/app-data/ranki-sync.json"));
    }

    #[test]
    fn sync_command_parses_escape_hatches() {
        assert!(matches!(
            parse(&["ranki", "sync"]).command,
            Some(Command::Sync {
                status: false,
                full_upload: false,
                full_download: false,
                yes: false,
            })
        ));
        assert!(matches!(
            parse(&["ranki", "sync", "--status"]).command,
            Some(Command::Sync { status: true, .. })
        ));
        assert!(matches!(
            parse(&["ranki", "sync", "--full-upload"]).command,
            Some(Command::Sync {
                full_upload: true,
                ..
            })
        ));
        assert!(matches!(
            parse(&["ranki", "sync", "--full-download"]).command,
            Some(Command::Sync {
                full_download: true,
                ..
            })
        ));
    }

    #[test]
    fn sync_full_commands_require_yes() {
        let directory = tempfile::tempdir().unwrap();
        let paths = ranki_model::DataPaths::new(directory.path());
        let mut config = ranki_model::Config::default();
        let mut transport = MockTransport::new();
        for args in [
            ["ranki", "sync", "--full-upload"],
            ["ranki", "sync", "--full-download"],
        ] {
            let cli = parse(&args);
            let error =
                run_with_sync(cli, &mut config, &mut transport, paths.clone())
                    .unwrap_err();
            assert!(error.to_string().contains("--yes"), "{error}");
        }
        assert_eq!(transport.request_count(), 0);
        let cli = parse(&["ranki", "sync", "--full-upload", "--yes"]);
        assert!(matches!(
            cli.command,
            Some(Command::Sync {
                full_upload: true,
                yes: true,
                ..
            })
        ));
    }

    #[test]
    fn mcp_is_excluded_from_automatic_sync() {
        assert!(!super::automatic_sync_allowed(Some(&Command::Mcp)));
        assert!(super::automatic_sync_allowed(Some(&Command::Decks)));
        assert!(super::automatic_sync_allowed(Some(&Command::Sync {
            status: false,
            full_upload: false,
            full_download: false,
            yes: false,
        })));
        assert!(super::automatic_sync_allowed(None));
    }

    #[cfg(unix)]
    #[test]
    fn cli_sync_command_uses_the_injected_transport() {
        let directory = tempfile::tempdir().unwrap();
        let key_file = directory.path().join("hkey");
        fs::write(&key_file, "test-hkey").unwrap();
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))
            .unwrap();
        let mut config = ranki_model::Config::default();
        config.sync.hkey_file = Some(key_file.to_string_lossy().into_owned());
        let paths = ranki_model::DataPaths::new(directory.path());
        let cli = parse(&["ranki", "sync", "--status"]);
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .unwrap_or(0);
        let mut transport = MockTransport::new().with_response(
            SyncMethod::Meta,
            MockResponse::ok_json(&serde_json::json!({
                "mod": 1, "scm": 1, "usn": 1, "ts": now, "empty": true,
            }))
            .unwrap(),
        );
        run_with_sync(cli, &mut config, &mut transport, paths.clone()).unwrap();
        assert_eq!(transport.request_count(), 1);
        assert_eq!(transport.requests()[0].method, SyncMethod::Meta);
        assert!(paths.sync_state.is_file());
    }
}
