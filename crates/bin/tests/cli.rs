//! End-to-end coverage through the built binary.

#![expect(clippy::unwrap_used, reason = "test assertions")]

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use ranki_model::Store;
use ranki_storage::Collection;
use tempfile::TempDir;

struct Isolated {
    data: PathBuf,
    config: PathBuf,
}

impl Isolated {
    fn new(dir: &TempDir, name: &str) -> Self {
        let root = dir.path().join(name);
        let data = root.join("xdg/data");
        let config = root.join("xdg/config");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(config.join("ranki")).unwrap();
        Self { data, config }
    }

    fn collection(&self) -> PathBuf {
        self.data.join("ranki/collection.anki2")
    }

    fn media(&self) -> PathBuf {
        self.data.join("ranki/collection.media")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ranki"));
        command
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_CONFIG_HOME", &self.config);
        command
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "command failed: {:?}\n{}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

#[test]
fn explicit_data_and_config_dirs_work_without_home() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("app-data");
    let config = dir.path().join("app-config");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("config.toml"), "[ui]\nthem = 'dark'\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_ranki"))
        .env_remove("HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("RANKI_DATA_DIR", &data)
        .env("RANKI_CONFIG_DIR", &config)
        .args(["decks"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(data.join("collection.anki2").is_file());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ui.them"), "{stderr}");
}

#[test]
fn full_cli_workflow_exports_and_reimports_a_collection() {
    let dir = tempfile::tempdir().unwrap();
    let source = Isolated::new(&dir, "source");
    let destination = Isolated::new(&dir, "destination");
    let source_collection = source.collection();

    let decks = source.run(&["decks"]);
    assert!(decks.contains("Default"), "{decks}");
    assert!(source_collection.is_file());

    let note_id = source.run(&[
        "add-note",
        "--model",
        "1",
        "--deck",
        "1",
        "--field",
        "integration front",
        "--field",
        "integration back",
        "--tag",
        "cli-test",
    ]);
    let card_id = note_id.trim().parse::<i64>().unwrap();
    assert!(card_id > 0);

    let found = source.run(&["search", "integration"]);
    assert_eq!(found.trim(), card_id.to_string());

    let due = source.run(&["due", "--limit", "5"]);
    assert!(due.contains(&card_id.to_string()), "{due}");

    source.run(&["answer", &card_id.to_string(), "3"]);

    let stats = source.run(&["stats"]);
    assert!(stats.contains("total=1"), "{stats}");

    let package = dir.path().join("backup.apkg");
    let package_arg = package.to_str().unwrap();
    source.run(&["export", package_arg]);
    assert!(package.is_file());

    let destination_collection = destination.collection();
    let report = destination.run(&["import", package_arg]);
    assert!(report.contains("notes=1"), "{report}");
    assert!(destination_collection.is_file());

    let fresh_stats = destination.run(&["stats"]);
    assert!(fresh_stats.contains("total=1"), "{fresh_stats}");
    assert_eq!(
        destination.run(&["search", "integration"]).trim(),
        found.trim()
    );

    let store = Collection::open(&destination_collection).unwrap();
    let imported = store.find_cards("integration").unwrap();
    assert_eq!(imported.len(), 1);
    let note = store
        .note(store.card(imported[0]).unwrap().unwrap().note_id)
        .unwrap()
        .unwrap();
    assert!(note.fields.iter().any(|field| field == "integration front"));
    assert!(note.tags.iter().any(|tag| tag == "cli-test"));
    drop(store);

    assert_eq!(source.run(&["check"]).trim(), "ok");
    assert_eq!(destination.run(&["check"]).trim(), "ok");
}

#[test]
fn missing_collection_is_created_only_in_the_isolated_data_root() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let path = app.collection();
    assert!(!path.exists());
    app.run(&["check"]);
    assert!(path.is_file());
    assert_eq!(path.parent(), Some(app.data.join("ranki").as_path()));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn invalid_rating_fails_without_creating_a_collection() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let output = app.command().args(["answer", "1", "9"]).output().unwrap();
    assert!(!output.status.success());
    assert!(!app.collection().exists());
}

#[test]
fn collection_works_without_home_when_data_home_is_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let output = app
        .command()
        .env_remove("HOME")
        .env_remove("RANKI_COLLECTION")
        .args(["decks"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(app.collection().is_file());
}

#[test]
fn collection_works_without_home_with_isolated_config() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    fs::write(
        app.config.join("ranki/config.toml"),
        "[ui]\ntheme = 'dark'\n",
    )
    .unwrap();
    let output = app
        .command()
        .env_remove("HOME")
        .env_remove("RANKI_COLLECTION")
        .args(["decks"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(app.collection().is_file());
}

#[test]
fn legacy_path_values_leave_an_anki_profile_untouched() {
    let dir = tempfile::tempdir().unwrap();
    for case in ["config", "environment"] {
        let app = Isolated::new(&dir, case);
        let decoy = dir.path().join(format!("{case}-anki"));
        fs::create_dir_all(&decoy).unwrap();
        let decoy_collection = decoy.join("collection.anki2");
        let decoy_media = decoy.join("collection.media");

        if case == "config" {
            fs::write(
                app.config.join("ranki/config.toml"),
                format!(
                    "[general]\ncollection = '{}'\nmedia_dir = '{}'\n",
                    decoy_collection.display(),
                    decoy_media.display()
                ),
            )
            .unwrap();
        }

        let mut command = app.command();
        if case == "environment" {
            command.env("RANKI_COLLECTION", &decoy_collection);
        }
        let output = command.args(["decks"]).output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(app.collection().is_file(), "{case}");
        assert!(!decoy_collection.exists(), "{case}");
        assert!(!decoy_media.exists(), "{case}");
        assert_eq!(fs::read_dir(&decoy).unwrap().count(), 0, "{case}");
    }
}

#[test]
fn unknown_config_keys_are_reported_by_full_path() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    fs::write(
        app.config.join("ranki/config.toml"),
        "[ui]\nthem = 'dark'\n",
    )
    .unwrap();
    let output = app.command().args(["decks"]).output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ui.them"), "{stderr}");
}

#[test]
fn deck_create_rename_and_remove_commands_update_the_collection() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let path = app.collection();

    let created = app.run(&["deck-create", "Study::Languages"]);
    let fields: Vec<_> = created.trim().split('\t').collect();
    assert_eq!(fields[0], "created");
    assert_eq!(fields[2], "Study::Languages");
    let id: i64 = fields[1].parse().unwrap();
    assert!(id > 0);

    let renamed = app.run(&["deck-rename", &id.to_string(), "Learning"]);
    assert_eq!(renamed.trim(), format!("renamed\t{id}\tLearning"));
    assert!(app.run(&["decks"]).contains("Learning"));

    let card_id = app
        .run(&[
            "add-note",
            "--model",
            "1",
            "--deck",
            &id.to_string(),
            "--field",
            "deck command",
            "--field",
            "answer",
        ])
        .trim()
        .parse::<i64>()
        .unwrap();
    assert!(card_id > 0);

    let failure = app
        .command()
        .args(["deck-remove", &id.to_string()])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(
        String::from_utf8_lossy(&failure.stderr).contains("--force"),
        "{}",
        String::from_utf8_lossy(&failure.stderr)
    );

    let removed = app.run(&["deck-remove", &id.to_string(), "--force"]);
    assert_eq!(removed.trim(), format!("removed\t{id}\tLearning"));
    let decks = app.run(&["decks"]);
    assert!(!decks.contains("Learning"), "{decks}");

    let store = Collection::open(&path).unwrap();
    assert_eq!(store.find_cards("deck command").unwrap(), [] as [i64; 0]);
}

#[test]
fn deck_remove_refuses_to_delete_the_default_deck() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let output = app
        .command()
        .args(["deck-remove", "1", "--force"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Default deck"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(app.run(&["decks"]).contains("Default"));
}

/// Every file under `root`, by relative path, with its bytes.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    files
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(root, &path, files);
            continue;
        }
        files.push((
            path.strip_prefix(root).unwrap().to_path_buf(),
            fs::read(&path).unwrap(),
        ));
    }
}

/// Runs the binary as if `home` were the only storage input.
fn ranki_with_home(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ranki"));
    command
        .env("HOME", home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("RANKI_DATA_DIR")
        .env_remove("RANKI_CONFIG_DIR")
        .env_remove("RANKI_COLLECTION");
    command
}

#[test]
fn ranki_leaves_a_local_anki_profile_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let anki = home.join(".local/share/Anki2");
    fs::create_dir_all(anki.join("User 1/collection.media")).unwrap();
    fs::create_dir_all(anki.join("addons21")).unwrap();
    fs::write(anki.join("prefs21.db"), b"real Anki prefs").unwrap();
    fs::write(anki.join("User 1/collection.anki2"), b"real Anki data").unwrap();
    fs::write(anki.join("User 1/collection.media/pic.png"), b"real media")
        .unwrap();
    let before = snapshot(&anki);

    let output = ranki_with_home(&home).args(["decks"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(home.join(".local/share/ranki/collection.anki2").is_file());
    assert_eq!(snapshot(&anki), before);

    let output = ranki_with_home(&home)
        .args(["--collection"])
        .arg(anki.join("User 1/collection.anki2"))
        .args(["decks"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--collection"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(snapshot(&anki), before);
}

#[test]
fn export_flags_control_envelope_and_media() {
    let dir = tempfile::tempdir().unwrap();
    let source = Isolated::new(&dir, "flags-source");
    let destination = Isolated::new(&dir, "flags-destination");
    let package = dir.path().join("flags.apkg");
    let package_arg = package.to_str().unwrap();

    source.run(&[
        "add-note",
        "--model",
        "1",
        "--deck",
        "1",
        "--field",
        "<img src=flag.png> flagged front",
        "--field",
        "back",
    ]);
    fs::create_dir_all(source.media()).unwrap();
    fs::write(source.media().join("flag.png"), b"image").unwrap();
    source.run(&[
        "export",
        package_arg,
        "--legacy",
        "--no-scheduling",
        "--no-deck-configs",
        "--no-media",
    ]);

    let bytes = fs::read(&package).unwrap();
    let names = String::from_utf8_lossy(&bytes);
    assert!(names.contains("collection.anki21"));
    assert!(names.contains("collection.anki2"));
    assert!(!names.contains("collection.anki21b"));

    let report = destination.run(&["import", package_arg]);
    assert!(report.contains("media=0"), "{report}");
    let media = destination.media();
    assert_eq!(fs::read_dir(&media).unwrap().count(), 0);
    assert_ne!(destination.run(&["search", "flagged"]).trim(), "");
}

#[test]
fn colpkg_import_replaces_collection_and_restores_conflicting_media() {
    let dir = tempfile::tempdir().unwrap();
    let source = Isolated::new(&dir, "restore-source");
    let destination = Isolated::new(&dir, "restore-destination");
    let package = dir.path().join("backup.colpkg");
    let package_arg = package.to_str().unwrap();

    source.run(&[
        "add-note",
        "--model",
        "1",
        "--deck",
        "1",
        "--field",
        "<img src=pic.png> restored front",
        "--field",
        "back",
    ]);
    fs::create_dir_all(source.media()).unwrap();
    fs::write(source.media().join("pic.png"), b"new").unwrap();
    source.run(&["export-colpkg", package_arg]);

    destination.run(&[
        "add-note",
        "--model",
        "1",
        "--deck",
        "1",
        "--field",
        "old front",
        "--field",
        "back",
    ]);
    fs::create_dir_all(destination.media()).unwrap();
    fs::write(destination.media().join("pic.png"), b"old").unwrap();

    let output = destination.run(&["import", package_arg]);
    assert!(output.contains("restored media=1"), "{output}");

    let backup = destination.collection().with_extension("pre-restore.anki2");
    assert!(backup.is_file());
    let backup_bytes = fs::read(&backup).unwrap();
    let backup_text = String::from_utf8_lossy(&backup_bytes);
    assert!(backup_text.contains("old front"));
    assert!(!backup_text.contains("restored front"));

    let collection = destination.collection();
    let restored_bytes = fs::read(&collection).unwrap();
    let restored = String::from_utf8_lossy(&restored_bytes);
    assert!(restored.contains("restored front"));
    assert!(!restored.contains("old front"));

    let media = destination.media();
    let files = fs::read_dir(&media)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|path| fs::read(path).unwrap() == b"old"));
    assert!(files.iter().any(|path| fs::read(path).unwrap() == b"new"));

    let db = media.with_extension("media.db2");
    let db_bytes = fs::read(db).unwrap();
    let db_text = String::from_utf8_lossy(&db_bytes);
    assert!(db_text.contains("CREATE TABLE media"));
    assert!(db_text.contains("pic-"), "{db_text}");
}
