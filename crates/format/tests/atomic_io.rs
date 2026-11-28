#![expect(clippy::unwrap_used, reason = "test fixtures use unwrap for setup")]

use std::fs;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;

use ranki_format::export_apkg;
use ranki_format::import_apkg;
use ranki_model::Store;
use ranki_storage::Collection;
use zip::ZipArchive;

fn temp_packages(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy().starts_with(".ranki-export-")
            })
        })
        .collect();
    paths.sort();
    paths
}

#[cfg(unix)]
#[test]
fn failed_export_leaves_no_partial_package_and_retry_succeeds() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.anki2");
    {
        let _store = Collection::create(&source).unwrap();
    }
    {
        let mut store = Collection::open(&source).unwrap();
        store
            .add_note(&ranki_model::NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![
                    String::from("<img src=\"blocked.png\">"),
                    String::from("back"),
                ],
                tags: Vec::new(),
            })
            .unwrap();
    }
    let media = dir.path().join("media");
    fs::create_dir_all(&media).unwrap();
    let blocked = media.join("blocked.png");
    fs::write(&blocked, b"blocked").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o0)).unwrap();
    if fs::read(&blocked).is_ok() {
        eprintln!(
            "skipping unreadable media check: process can read mode 000 files"
        );
        return;
    }

    let destination = dir.path().join("deck.apkg");
    let error = export_apkg(&source, &media, &destination).unwrap_err();
    assert!(
        error.to_string().contains("cannot read media file"),
        "{error}"
    );
    assert!(!destination.exists());
    let leftovers = temp_packages(dir.path());
    assert!(leftovers.is_empty(), "{leftovers:?}");

    fs::write(&destination, b"old package").unwrap();
    let _error = export_apkg(&source, &media, &destination).unwrap_err();
    assert_eq!(fs::read(&destination).unwrap(), b"old package".to_vec());
    let leftovers = temp_packages(dir.path());
    assert!(leftovers.is_empty(), "{leftovers:?}");

    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o644)).unwrap();
    export_apkg(&source, &media, &destination).unwrap();
    assert!(ZipArchive::new(File::open(&destination).unwrap()).is_ok());
    let leftovers = temp_packages(dir.path());
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn failed_import_removes_only_media_created_by_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("shared.png"), b"shared bytes").unwrap();
    fs::write(media.join("keep.png"), b"keep").unwrap();

    let source = dir.path().join("source.anki2");
    {
        let mut store = Collection::create(&source).unwrap();
        store
            .add_note(&ranki_model::NewNote {
                model_id: 1,
                deck_id: 1,
                fields: vec![
                    String::from("<img src=\"shared.png\">"),
                    String::from("back"),
                ],
                tags: Vec::new(),
            })
            .unwrap();
    }
    let package = dir.path().join("source.apkg");
    export_apkg(&source, &media, &package).unwrap();

    let destination = dir.path().join("imported.anki2");
    fs::write(&destination, b"not a collection").unwrap();
    let error = import_apkg(&package, &destination, &media).unwrap_err();
    assert_ne!(error.to_string(), "");

    assert_eq!(
        fs::read(media.join("shared.png")).unwrap(),
        b"shared bytes".to_vec()
    );
    assert_eq!(fs::read(media.join("keep.png")).unwrap(), b"keep".to_vec());
    let created: Vec<_> = fs::read_dir(&media)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != "shared.png" && name != "keep.png")
        .collect();
    assert!(created.is_empty(), "{created:?}");
}
