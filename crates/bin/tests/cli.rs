//! End-to-end coverage through the built binary.

#![expect(clippy::unwrap_used, reason = "test assertions")]

use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use ranki_model::Card;
use ranki_model::Note;
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

    fn failure(&self, args: &[&str]) -> String {
        let output = self.command().args(args).output().unwrap();
        assert!(
            !output.status.success(),
            "command unexpectedly succeeded: {args:?}"
        );
        String::from_utf8(output.stderr).unwrap()
    }
}

fn media_names(dir: &Path) -> Vec<String> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut names = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
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

#[test]
fn connect_creates_a_missing_collection_and_reports_its_address() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    assert!(!app.collection().exists());

    let mut child = app
        .command()
        .args(["connect", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stderr).read_line(&mut line);
        let _ = sender.send((read, line));
    });
    let (read, line) = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();

    read.unwrap();
    assert_eq!(line.trim(), format!("listening on 127.0.0.1:{port}"));
    assert!(app.collection().is_file());
}

#[test]
fn corrupt_collection_error_names_the_collection_path() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let collection = app.collection();
    fs::create_dir_all(collection.parent().unwrap()).unwrap();
    fs::write(&collection, b"not a sqlite database").unwrap();

    let stderr = app.failure(&["decks"]);
    assert!(stderr.contains("cannot open collection"), "{stderr}");
    assert!(
        stderr.contains(&collection.display().to_string()),
        "{stderr}"
    );
}

#[test]
fn deck_names_work_for_add_note_due_stats_and_show() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    app.run(&["deck-create", "Study::Languages"]);

    let add_args = [
        "add-note",
        "--model",
        "Basic",
        "--deck",
        "study::languages",
        "--field",
        "named deck front",
        "--field",
        "answer",
    ];
    let card_id = app.run(&add_args).trim().parse::<i64>().unwrap();
    assert!(card_id > 0);

    assert_eq!(
        app.run(&["search", "named deck"]).trim(),
        card_id.to_string()
    );

    let due = app.run(&["due", "--deck", "STUDY::LANGUAGES"]);
    let prefix = format!("{card_id}\t");
    assert!(due.lines().any(|line| line.starts_with(&prefix)), "{due}");

    let stats = app.run(&["stats", "--deck", "study::languages"]);
    assert!(stats.contains("total=1"), "{stats}");

    let shown = app.run(&["show", &card_id.to_string()]);
    assert!(shown.contains(&format!("card\t{card_id}\t")), "{shown}");
    assert!(shown.contains("Study::Languages"), "{shown}");
    assert!(shown.contains("Basic"), "{shown}");
    assert!(shown.contains("Front\tnamed deck front"), "{shown}");
    assert!(shown.contains("Back\tanswer"), "{shown}");
}

#[test]
fn empty_default_deck_resolves_by_name_and_id() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    app.run(&["deck-create", "Other"]);
    let decks = app.run(&["decks"]);
    assert!(!decks.contains("Default"), "{decks}");

    let card_id = app
        .run(&[
            "add-note",
            "--model",
            "Basic",
            "--deck",
            "default",
            "--field",
            "default deck front",
            "--field",
            "answer",
        ])
        .trim()
        .parse::<i64>()
        .unwrap();
    assert!(card_id > 0);
    assert!(app.run(&["stats", "--deck", "1"]).contains("total=1"));
    let due = app.run(&["due", "--deck", "Default"]);
    assert!(due.contains(&card_id.to_string()), "{due}");
    let shown = app.run(&["show", &card_id.to_string()]);
    assert!(shown.contains("Default"), "{shown}");
}

#[test]
fn explicit_sync_reports_missing_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let output = app
        .command()
        .env_remove("RANKI_SYNC_HKEY")
        .env_remove("RANKI_SYNC_USERNAME")
        .env_remove("RANKI_SYNC_PASSWORD")
        .args(["sync"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("no sync credentials are available"),
        "{stderr}"
    );
}

#[test]
fn export_commands_report_success_and_require_force_for_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    app.run(&[
        "add-note",
        "--model",
        "Basic",
        "--deck",
        "1",
        "--field",
        "export front",
        "--field",
        "answer",
    ]);
    let apkg = dir.path().join("backup.apkg");
    let colpkg = dir.path().join("backup.colpkg");

    for (command, package) in [("export", &apkg), ("export-colpkg", &colpkg)] {
        let package_arg = package.to_str().unwrap();
        let output = app.run(&[command, package_arg]);
        assert!(output.contains("exported"), "{output}");
        assert!(output.contains(package_arg), "{output}");
        assert!(package.is_file());

        let stderr = app.failure(&[command, package_arg]);
        assert!(stderr.contains("--force"), "{stderr}");

        let output = app.run(&[command, package_arg, "--force"]);
        assert!(output.contains("exported"), "{output}");
    }
}

#[cfg(unix)]
#[test]
fn export_refuses_a_dangling_symlink_destination() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    app.run(&["decks"]);
    let target = dir.path().join("missing-target.apkg");
    let link = dir.path().join("backup.apkg");
    symlink(&target, &link).unwrap();

    let stderr = app.failure(&["export", link.to_str().unwrap()]);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert!(!target.exists());
}

#[test]
fn exports_refuse_the_active_collection_even_with_force() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    app.run(&[
        "add-note",
        "--model",
        "Basic",
        "--deck",
        "1",
        "--field",
        "keep this collection",
        "--field",
        "answer",
    ]);
    let collection = app.collection();
    let before = fs::read(&collection).unwrap();
    let package_arg = collection.to_str().unwrap();

    for command in ["export", "export-colpkg"] {
        let stderr = app.failure(&[command, package_arg, "--force"]);
        assert!(stderr.contains("active collection"), "{stderr}");
    }
    assert_eq!(fs::read(&collection).unwrap(), before);
}

#[test]
fn full_sync_requires_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    for flag in ["--full-upload", "--full-download"] {
        let stderr = app.failure(&["sync", flag]);
        assert!(stderr.contains("--yes"), "{stderr}");
    }
}

#[test]
fn help_documents_flags_and_search_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let due = app.run(&["due", "--help"]);
    assert!(due.contains("default: 20"), "{due}");
    let add = app.run(&["add-note", "--help"]);
    assert!(add.contains("field order"), "{add}");
    let remove = app.run(&["deck-remove", "--help"]);
    assert!(remove.contains("contain cards"), "{remove}");
    let search = app.run(&["search", "--help"]);
    for token in [
        "deck:",
        "tag:",
        "note:",
        "is:",
        "quoted text",
        "*",
        "one card id per line",
    ] {
        assert!(search.contains(token), "missing {token}: {search}");
    }
}

#[test]
fn image_occlusion_failures_clean_up_staged_and_reserved_media() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    app.run(&["decks"]);
    let image = dir.path().join("diagram.png");
    fs::write(&image, b"fake png").unwrap();
    let image_arg = image.to_str().unwrap();
    let media = app.data.join("ranki/collection.media");
    let invalid_deck = [
        "add-image-occlusion",
        "--deck",
        "999",
        "--image",
        image_arg,
        "--rect",
        "0.1,0.1,0.2,0.2",
    ];

    let stderr = app.failure(&invalid_deck);
    assert!(stderr.contains("deck 999"), "{stderr}");
    assert!(media_names(&media).is_empty(), "{:?}", media_names(&media));

    let valid = [
        "add-image-occlusion",
        "--deck",
        "1",
        "--image",
        image_arg,
        "--rect",
        "0.1,0.1,0.2,0.2",
    ];
    let first = app.run(&valid);
    assert!(first.trim().parse::<i64>().is_ok(), "{first}");
    assert_eq!(media_names(&media), ["diagram.png"]);

    fs::remove_file(media.join("diagram.png")).unwrap();
    let stderr = app.failure(&valid);
    assert!(stderr.contains("duplicate"), "{stderr}");
    assert!(media_names(&media).is_empty(), "{:?}", media_names(&media));
}

#[test]
fn image_occlusion_keeps_an_existing_media_file() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let media = app.data.join("ranki/collection.media");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("diagram.png"), b"original").unwrap();
    let image = dir.path().join("diagram.png");
    fs::write(&image, b"replacement").unwrap();

    let stderr = app.failure(&[
        "add-image-occlusion",
        "--deck",
        "1",
        "--image",
        image.to_str().unwrap(),
        "--rect",
        "0.1,0.1,0.2,0.2",
    ]);
    assert!(stderr.contains("media file already exists"), "{stderr}");
    assert_eq!(fs::read(media.join("diagram.png")).unwrap(), b"original");
    assert_eq!(media_names(&media), ["diagram.png"]);
    assert!(app.run(&["stats"]).contains("total=0"));
}

#[test]
fn models_include_ordered_field_and_template_names_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let models = app.run(&["models"]);
    let basic: Vec<_> = models
        .lines()
        .filter(|line| line.contains("\tBasic\t"))
        .collect();
    assert_eq!(basic.len(), 1, "{models}");
    let columns: Vec<_> = basic[0].split('\t').collect();
    assert_eq!(columns[1], "Basic");
    assert_eq!(columns[4], "Front");
    assert_eq!(columns[5], "Back");
    assert_eq!(columns[6], "Card 1");
    assert!(!models.contains("stock\tBasic\t"), "{models}");
    assert!(models.contains("stock\tCloze\t"), "{models}");
    assert!(models.contains("stock\tImage Occlusion\t"), "{models}");
}

#[test]
fn note_edit_keeps_card_identity_and_review_history() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let card_id = add_basic_note(&app, "front", "back", &[]);
    app.run(&["answer", &card_id.to_string(), "3"]);

    let store = Collection::open(&app.collection()).unwrap();
    let card_before = store.card(card_id).unwrap().unwrap();
    let notes_before: Vec<_> = store
        .review_history()
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    drop(store);

    let (note, _) = stored_note_and_card(&app, card_id);
    app.run(&[
        "note-edit",
        &note.id.to_string(),
        "--field",
        "edited front",
        "--field",
        "back",
    ]);

    let store = Collection::open(&app.collection()).unwrap();
    let card_after = store.card(card_id).unwrap().unwrap();
    let notes_after: Vec<_> = store
        .review_history()
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(card_after.id, card_before.id);
    assert_eq!(card_after.reps, card_before.reps);
    assert_eq!(card_after.interval, card_before.interval);
    assert_eq!(card_after.due, card_before.due);
    assert_eq!(notes_after, notes_before);
    assert_eq!(notes_after.len(), 1);
}

#[test]
fn note_edit_moves_cards_to_a_deck_by_name_or_id() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let created = app.run(&["deck-create", "Target"]);
    let target_id: i64 =
        created.trim().split('\t').nth(1).unwrap().parse().unwrap();
    let card_id = add_basic_note(&app, "front", "back", &[]);
    let (note, _) = stored_note_and_card(&app, card_id);
    let note_id = note.id.to_string();

    app.run(&[
        "note-edit",
        &note_id,
        "--field",
        "front",
        "--field",
        "back",
        "--deck",
        "target",
    ]);
    let (_, card) = stored_note_and_card(&app, card_id);
    assert_eq!(card.deck_id, target_id);

    let created = app.run(&["deck-create", "Second"]);
    let second_id: i64 =
        created.trim().split('\t').nth(1).unwrap().parse().unwrap();
    app.run(&[
        "note-edit",
        &note_id,
        "--field",
        "front",
        "--field",
        "back",
        "--deck",
        &second_id.to_string(),
    ]);
    let (_, card) = stored_note_and_card(&app, card_id);
    assert_eq!(card.deck_id, second_id);
}

#[test]
fn note_edit_preserves_tags_when_omitted_and_clears_them_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let card_id = add_basic_note(&app, "old front", "back", &["keep", "other"]);
    let (note, _) = stored_note_and_card(&app, card_id);
    let note_id = note.id.to_string();

    app.run(&[
        "note-edit",
        &note_id,
        "--field",
        "front two",
        "--field",
        "back",
    ]);
    let (preserved, _) = stored_note_and_card(&app, card_id);
    assert_eq!(preserved.tags, ["keep", "other"]);

    app.run(&[
        "note-edit",
        &note_id,
        "--field",
        "front three",
        "--field",
        "back",
        "--clear-tags",
    ]);
    let (cleared, _) = stored_note_and_card(&app, card_id);
    assert_eq!(cleared.tags, [] as [String; 0]);

    let stderr = app.failure(&[
        "note-edit",
        &note_id,
        "--field",
        "front four",
        "--field",
        "back",
        "--tag",
        "x",
        "--clear-tags",
    ]);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    let (unchanged, _) = stored_note_and_card(&app, card_id);
    assert_eq!(unchanged.fields, ["front three", "back"]);
}

#[test]
fn note_edit_rejects_a_stale_expected_modified() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let card_id = add_basic_note(&app, "stale front", "back", &[]);
    let (note, _) = stored_note_and_card(&app, card_id);
    let note_id = note.id.to_string();
    let stale = (note.modified - 1).to_string();

    let stderr = app.failure(&[
        "note-edit",
        &note_id,
        "--field",
        "changed front",
        "--field",
        "back",
        "--expected-modified",
        &stale,
    ]);
    assert!(stderr.contains("note changed"), "{stderr}");
    let (unchanged, _) = stored_note_and_card(&app, card_id);
    assert_eq!(unchanged.fields, ["stale front", "back"]);

    let shown = app.run(&["show", &card_id.to_string()]);
    let modified = shown_note_modified(&shown).to_string();
    let output = app.run(&[
        "note-edit",
        &note_id,
        "--field",
        "fresh front",
        "--field",
        "back",
        "--expected-modified",
        &modified,
    ]);
    assert!(output.starts_with("edited\t"), "{output}");
}

#[test]
fn note_edit_updates_fields_replaces_tags_and_reports_modified() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let card_id = add_basic_note(&app, "old front", "old back", &["old"]);
    let (before, card_before) = stored_note_and_card(&app, card_id);

    let shown = app.run(&["show", &card_id.to_string()]);
    assert_eq!(shown_note_modified(&shown), before.modified);

    let note_id = before.id.to_string();
    let expected = before.modified.to_string();
    let output = app.run(&[
        "note-edit",
        &note_id,
        "--field",
        "new front",
        "--field",
        "new back",
        "--tag",
        "alpha",
        "--tag",
        "beta",
        "--expected-modified",
        &expected,
    ]);
    let columns: Vec<_> = output.trim().split('\t').collect();
    assert_eq!(columns.len(), 3, "{output}");
    assert_eq!(columns[0], "edited");
    assert_eq!(columns[1], note_id);
    let modified: i64 = columns[2].parse().unwrap();

    let (after, card_after) = stored_note_and_card(&app, card_id);
    assert_eq!(after.fields, ["new front", "new back"]);
    assert_eq!(after.tags, ["alpha", "beta"]);
    assert_eq!(after.modified, modified);
    assert_eq!(card_after.id, card_before.id);
    assert_eq!(card_after.note_id, after.id);
    assert_eq!(card_after.deck_id, card_before.deck_id);
}

#[test]
fn note_edit_validates_note_fields_and_field_contract() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let card_id = add_basic_note(&app, "valid front", "back", &[]);
    let (note, _) = stored_note_and_card(&app, card_id);
    let note_id = note.id.to_string();

    let stderr = app.failure(&[
        "note-edit",
        "999999",
        "--field",
        "front",
        "--field",
        "back",
    ]);
    assert!(stderr.contains("note not found: 999999"), "{stderr}");

    let stderr = app.failure(&["note-edit", &note_id, "--field", "only"]);
    assert!(
        stderr.contains("1 fields for Basic; expected 2 in order: Front, Back"),
        "{stderr}"
    );

    let stderr = app.failure(&[
        "note-edit",
        &note_id,
        "--field",
        "one",
        "--field",
        "two",
        "--field",
        "three",
    ]);
    assert!(
        stderr.contains("3 fields for Basic; expected 2 in order: Front, Back"),
        "{stderr}"
    );

    let stderr = app.failure(&[
        "note-edit",
        &note_id,
        "--field",
        "   ",
        "--field",
        "back",
    ]);
    assert!(stderr.contains("first field must not be empty"), "{stderr}");

    let stderr = app.failure(&["note-edit", &note_id]);
    assert!(stderr.contains("--field"), "{stderr}");

    let (unchanged, _) = stored_note_and_card(&app, card_id);
    assert_eq!(unchanged.fields, ["valid front", "back"]);
}

#[test]
fn note_edit_without_a_deck_leaves_split_cards_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let created = app.run(&["deck-create", "Other"]);
    let other_id: i64 =
        created.trim().split('\t').nth(1).unwrap().parse().unwrap();
    let added = app.run(&[
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
    let card_id = added.lines().next().unwrap().parse().unwrap();
    let (note, _) = stored_note_and_card(&app, card_id);
    let mut store = Collection::open(&app.collection()).unwrap();
    let mut cards = store.find_cards("").unwrap();
    cards.sort_unstable();
    assert_eq!(cards.len(), 2);
    let mut card = store.card(cards[1]).unwrap().unwrap();
    card.deck_id = other_id;
    store.update_card(&card, card.modified).unwrap();
    drop(store);

    app.run(&[
        "note-edit",
        &note.id.to_string(),
        "--field",
        "edited front",
        "--field",
        "back",
    ]);

    let store = Collection::open(&app.collection()).unwrap();
    let mut decks: Vec<_> = cards
        .iter()
        .map(|id| store.card(*id).unwrap().unwrap().deck_id)
        .collect();
    decks.sort_unstable();
    assert_eq!(decks, [1, other_id]);
}

#[test]
fn occlusion_markers_cannot_be_mixed_with_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let output = app
        .command()
        .args([
            "add-image-occlusion",
            "--deck",
            "1",
            "--image",
            "/tmp/diagram.png",
            "--occlusion",
            "{{c1::image-occlusion:rect:left=.1:top=.1:width=.2:height=.2}}",
            "--rect",
            "0.1,0.1,0.2,0.2",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("cannot be used with"), "{stderr}");
}

#[test]
fn rating_words_are_accepted_for_answer() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let card_id = app
        .run(&[
            "add-note",
            "--model",
            "Basic",
            "--deck",
            "1",
            "--field",
            "rated front",
            "--field",
            "answer",
        ])
        .trim()
        .parse::<i64>()
        .unwrap();

    let output = app.run(&["answer", &card_id.to_string(), "GOOD"]);
    assert!(output.contains(&card_id.to_string()), "{output}");
}

#[test]
fn unknown_deck_names_fail_instead_of_empty_results() {
    let dir = tempfile::tempdir().unwrap();
    let app = Isolated::new(&dir, "app");
    let cases: [&[&str]; 3] = [
        &[
            "add-note", "--model", "Basic", "--deck", "Nope", "--field",
            "question", "--field", "answer",
        ],
        &["due", "--deck", "Nope"],
        &["stats", "--deck", "Nope"],
    ];
    for args in cases {
        let stderr = app.failure(args);
        assert!(stderr.contains("deck not found: Nope"), "{stderr}");
    }
    assert!(app.run(&["stats"]).contains("total=0"));
}

#[expect(
    clippy::too_many_arguments,
    reason = "the fixture builds one note from its explicit parts"
)]
fn add_basic_note(
    app: &Isolated,
    front: &str,
    back: &str,
    tags: &[&str],
) -> i64 {
    let mut args = vec![
        "add-note", "--model", "Basic", "--deck", "1", "--field", front,
        "--field", back,
    ];
    for tag in tags {
        args.push("--tag");
        args.push(tag);
    }
    app.run(&args).trim().parse().unwrap()
}

fn stored_note_and_card(app: &Isolated, card_id: i64) -> (Note, Card) {
    let store = Collection::open(&app.collection()).unwrap();
    let card = store.card(card_id).unwrap().unwrap();
    let note = store.note(card.note_id).unwrap().unwrap();
    (note, card)
}

fn shown_note_modified(output: &str) -> i64 {
    let line = output
        .lines()
        .find(|line| line.starts_with("note\t"))
        .unwrap();
    let columns: Vec<_> = line.split('\t').collect();
    assert_eq!(columns.get(5), Some(&"modified"), "{line}");
    columns.get(6).unwrap().parse().unwrap()
}
