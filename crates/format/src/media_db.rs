use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;

use rusqlite::Connection;

use crate::FormatError;
use crate::error::db_error;
use crate::schema::ensure_parent;

/// Anki's media database lives beside the media folder.
pub fn media_db_path(media_dir: &Path) -> PathBuf {
    let name = media_dir
        .file_name()
        .map_or_else(|| "collection.media".into(), OsStr::to_os_string);
    let mut db_name = name;
    db_name.push(".db2");
    match media_dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(db_name),
        _ => PathBuf::from(db_name),
    }
}

#[derive(Debug, Clone)]
pub struct MediaRecord {
    pub name: String,
    pub sha1: [u8; 20],
    pub mtime: i64,
    pub dirty: bool,
}

pub fn record_media(
    path: &Path,
    records: &[MediaRecord],
) -> Result<(), FormatError> {
    if records.is_empty() {
        return Ok(());
    }
    ensure_parent(path)?;
    let connection = Connection::open(path).map_err(db_error(format!(
        "cannot open media database {}",
        path.display()
    )))?;
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS media (
                fname TEXT NOT NULL PRIMARY KEY,
                csum TEXT,
                mtime INTEGER NOT NULL,
                dirty INTEGER NOT NULL
            ) WITHOUT ROWID;
            CREATE INDEX IF NOT EXISTS idx_media_dirty ON media (dirty) WHERE dirty = 1;
            CREATE TABLE IF NOT EXISTS meta (dirMod INTEGER, lastUsn INTEGER);
            INSERT INTO meta SELECT 0, 0 WHERE NOT EXISTS (SELECT 1 FROM meta);
            BEGIN;",
        )
        .map_err(db_error("cannot create media database schema"))?;
    for record in records {
        let checksum = hex(&record.sha1);
        connection
            .execute(
                "INSERT OR REPLACE INTO media (fname, csum, mtime, dirty) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    record.name,
                    checksum,
                    record.mtime,
                    i64::from(record.dirty)
                ],
            )
            .map_err(db_error("cannot record media file"))?;
    }
    connection
        .execute_batch("COMMIT")
        .map_err(db_error("cannot commit media database"))?;
    drop(connection);
    Ok(())
}

fn hex(bytes: &[u8; 20]) -> String {
    let mut output = String::with_capacity(40);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::MediaRecord;
    use super::media_db_path;
    use super::record_media;
    use rusqlite::Connection;

    #[test]
    fn records_anki_media_columns() {
        let dir = tempfile::tempdir().unwrap();
        let media_dir = dir.path().join("collection.media");
        let path = media_db_path(&media_dir);
        record_media(
            &path,
            &[MediaRecord {
                name: "pic.png".to_owned(),
                sha1: [7; 20],
                mtime: 123,
                dirty: false,
            }],
        )
        .unwrap();

        let connection = Connection::open(path).unwrap();
        let csum: String = connection
            .query_row(
                "SELECT csum FROM media WHERE fname = 'pic.png'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(csum, "07".repeat(20));
        let dirty: i64 = connection
            .query_row(
                "SELECT dirty FROM media WHERE fname = 'pic.png'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(dirty, 0);
    }
}
