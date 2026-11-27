use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::str;
use std::sync::OnceLock;

use ranki_model::NewNote;
use ranki_model::Store as _;
use rusqlite::Connection;
use rusqlite::MAIN_DB;
use rusqlite::OptionalExtension;
use rusqlite::backup::Backup;
use rusqlite::backup::Progress;
use rusqlite::backup::StepResult;
use rusqlite::params;
use rusqlite::params_from_iter;
use serde_json::Value;
use tempfile::Builder;
use tempfile::NamedTempFile;
use zip::CompressionMethod;
use zip::ZipArchive;
use zip::ZipWriter;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zstd::stream::decode_all;
use zstd::stream::encode_all;

use crate::FormatError;
use crate::error::db_error;
use crate::error::io_error;
use crate::error::json_error;
use crate::error::zip_error;
use crate::media::MediaEntry;
use crate::media::MediaFile;

use crate::media::sanitize_filename;
use crate::media::sha1_bytes;
use crate::media::write_new_file;
use crate::schema;

const ZSTD_MAGIC: &[u8] = &[0x28, 0xB5, 0x2F, 0xFD];
const DAY_MILLIS: i64 = 86_400_000;
const FUTURE_ID_SLACK_MILLIS: i64 = DAY_MILLIS * 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageVersion {
    Legacy1,
    Legacy2,
    Latest,
}

impl PackageVersion {
    const fn collection_name(self) -> &'static str {
        match self {
            Self::Legacy1 => "collection.anki2",
            Self::Legacy2 => "collection.anki21",
            Self::Latest => "collection.anki21b",
        }
    }

    const fn meta_byte(self) -> u8 {
        match self {
            Self::Legacy1 => 1,
            Self::Legacy2 => 2,
            Self::Latest => 3,
        }
    }

    const fn from_byte(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::Legacy1),
            2 => Some(Self::Legacy2),
            3 => Some(Self::Latest),
            _ => None,
        }
    }

    fn zstd(self) -> bool {
        self == Self::Latest
    }

    fn protobuf_media_map(self) -> bool {
        self == Self::Latest
    }
}

pub struct ApkgPackage {
    pub collection: NamedTempFile,
    pub media: Vec<MediaEntry>,
}

pub fn read_apkg(
    package: &Path,
    scratch_dir: &Path,
) -> Result<ApkgPackage, FormatError> {
    let file = File::open(package).map_err(io_error(format!(
        "cannot open package {}",
        package.display()
    )))?;
    let mut archive = ZipArchive::new(file).map_err(zip_error(format!(
        "malformed apkg package {}",
        package.display()
    )))?;
    let names = entry_names(&archive)?;
    let version = read_version(&mut archive, &names)?;
    let collection_name = version.collection_name();
    if !names.contains(collection_name) {
        return Err(FormatError::new(format!(
            "package is missing {collection_name}"
        )));
    }
    let collection_bytes = decode_versioned(
        read_bytes(&mut archive, collection_name)?,
        version.zstd(),
        "collection",
    )?;
    let media = read_media_entries(&mut archive, &names, version)?;
    let collection = write_temp_collection(&collection_bytes, scratch_dir)?;

    Ok(ApkgPackage { collection, media })
}

#[expect(
    clippy::too_many_arguments,
    reason = "database, media, destination, and package version are separate inputs"
)]
#[expect(
    clippy::too_many_lines,
    reason = "writing the package entries is linear and clearer inline"
)]
pub fn write_apkg_bytes_with_version(
    database: &[u8],
    media_files: &[MediaFile],
    destination: &Path,
    version: PackageVersion,
) -> Result<(), FormatError> {
    schema::ensure_parent(destination)?;
    let dummy = dummy_collection()?;
    let prepared = media_files
        .iter()
        .map(|media| {
            let bytes = fs::read(&media.path).map_err(io_error(format!(
                "cannot read media file {}",
                media.path.display()
            )))?;
            Ok((media.name.clone(), bytes))
        })
        .collect::<Result<Vec<(String, Vec<u8>)>, FormatError>>()?;

    let file = File::create(destination).map_err(io_error(format!(
        "cannot create package {}",
        destination.display()
    )))?;
    let mut writer = ZipWriter::new(BufWriter::new(file));
    write_entry(
        &mut writer,
        "meta",
        &[0x08, version.meta_byte()],
        CompressionMethod::Stored,
    )?;
    let collection_bytes = if version.zstd() {
        encode_zstd(database)?
    } else {
        database.to_vec()
    };
    let collection_method = if version.zstd() {
        CompressionMethod::Stored
    } else {
        CompressionMethod::Deflated
    };
    write_entry(
        &mut writer,
        version.collection_name(),
        &collection_bytes,
        collection_method,
    )?;
    write_entry(
        &mut writer,
        PackageVersion::Legacy1.collection_name(),
        &dummy,
        CompressionMethod::Stored,
    )?;
    for (index, (name, bytes)) in prepared.iter().enumerate() {
        let _ = name;
        let encoded = if version.zstd() {
            encode_zstd(bytes)?
        } else {
            bytes.clone()
        };
        write_entry(
            &mut writer,
            &index.to_string(),
            &encoded,
            CompressionMethod::Stored,
        )?;
    }
    let mapping = if version.protobuf_media_map() {
        encode_media_entries(&prepared)?
    } else {
        serde_json::to_vec(&legacy_media_mapping(&prepared))
            .map_err(json_error("cannot encode media mapping"))?
    };
    let encoded_mapping = if version.zstd() {
        encode_zstd(&mapping)?
    } else {
        mapping
    };
    write_entry(
        &mut writer,
        "media",
        &encoded_mapping,
        CompressionMethod::Stored,
    )?;

    let output = writer
        .finish()
        .map_err(zip_error("cannot finish apkg package"))?;
    output.into_inner().map_err(|error| {
        FormatError::with_source(
            format!("cannot flush package {}", destination.display()),
            error.into_error(),
        )
    })?;
    Ok(())
}

pub fn extract_colpkg(
    package: &Path,
    destination_dir: &Path,
) -> Result<PathBuf, FormatError> {
    let file = File::open(package).map_err(io_error(format!(
        "cannot open package {}",
        package.display()
    )))?;
    let mut archive = ZipArchive::new(file).map_err(zip_error(format!(
        "malformed colpkg package {}",
        package.display()
    )))?;
    let names = entry_names(&archive)?;
    let version = read_version(&mut archive, &names)?;
    let collection_name = version.collection_name();
    if !names.contains(collection_name) {
        return Err(FormatError::new(format!(
            "package is missing {collection_name}"
        )));
    }

    fs::create_dir_all(destination_dir).map_err(io_error(format!(
        "cannot create destination {}",
        destination_dir.display()
    )))?;
    let destination = destination_dir.join(collection_name);
    if destination.exists() {
        return Err(FormatError::new(format!(
            "collection already exists: {}",
            destination.display()
        )));
    }
    let collection_bytes = decode_versioned(
        read_bytes(&mut archive, collection_name)?,
        version.zstd(),
        "collection",
    )?;
    write_new_file(
        &destination,
        &collection_bytes,
        &destination.display().to_string(),
    )?;

    let media_dir = destination_dir.join("media");
    fs::create_dir_all(&media_dir).map_err(io_error(format!(
        "cannot create media directory {}",
        media_dir.display()
    )))?;
    let extracted = read_media_entries(&mut archive, &names, version)?;

    for media in extracted {
        let target = media_dir.join(sanitize_filename(&media.name));
        if !target.exists() {
            write_new_file(
                &target,
                &media.bytes,
                &target.display().to_string(),
            )?;
        }
    }

    for entry in names.iter().filter(|name| is_media_entry(name)) {
        let relative = entry.trim_start_matches("media/");
        extract_media_file(&mut archive, &media_dir, (entry, relative))?;
    }

    if names.contains("meta") {
        let meta_path = destination_dir.join("meta");
        if !meta_path.exists() {
            let bytes = read_bytes(&mut archive, "meta")?;
            write_new_file(
                &meta_path,
                &bytes,
                &meta_path.display().to_string(),
            )?;
        }
    }
    Ok(destination)
}

#[expect(
    clippy::excessive_nesting,
    reason = "protobuf field handling is clearer inline"
)]
fn read_version(
    archive: &mut ZipArchive<File>,
    names: &BTreeSet<String>,
) -> Result<PackageVersion, FormatError> {
    if !names.contains("meta") {
        return Ok(if names.contains("collection.anki21b") {
            PackageVersion::Latest
        } else if names.contains("collection.anki21") {
            PackageVersion::Legacy2
        } else {
            PackageVersion::Legacy1
        });
    }
    let bytes = read_bytes(archive, "meta")?;

    let mut offset = 0;
    let mut version = None;
    while offset < bytes.len() {
        let (tag, next) = read_varint(&bytes, offset)?;
        offset = next;
        let field = tag >> 3;
        match tag & 0x07 {
            0 => {
                let (value, next) = read_varint(&bytes, offset)?;
                offset = next;
                if field == 1 {
                    version = Some(
                        PackageVersion::from_byte(value).ok_or_else(|| {
                            FormatError::new(format!(
                                "unsupported package version: {value}"
                            ))
                        })?,
                    );
                }
            }
            1 => offset = offset.checked_add(8).ok_or_else(invalid_meta)?,
            2 => {
                let (length, next) = read_varint(&bytes, offset)?;
                offset = next
                    .checked_add(
                        usize::try_from(length).map_err(|_| invalid_meta())?,
                    )
                    .ok_or_else(invalid_meta)?;
                if offset > bytes.len() {
                    return Err(invalid_meta());
                }
            }
            5 => offset = offset.checked_add(4).ok_or_else(invalid_meta)?,
            _ => return Err(invalid_meta()),
        }
    }
    version.ok_or_else(|| FormatError::new("package meta has no version"))
}

fn invalid_meta() -> FormatError {
    FormatError::new("invalid package metadata")
}

fn read_varint(
    bytes: &[u8],
    mut offset: usize,
) -> Result<(u64, usize), FormatError> {
    let mut value = 0_u64;
    let mut shift = 0_u32;
    loop {
        let byte = *bytes.get(offset).ok_or_else(invalid_meta)?;
        offset = offset.checked_add(1).ok_or_else(invalid_meta)?;
        let part = u64::from(byte & 0x7F)
            .checked_shl(shift)
            .ok_or_else(invalid_meta)?;
        value = value.checked_add(part).ok_or_else(invalid_meta)?;
        if byte & 0x80 == 0 {
            return Ok((value, offset));
        }
        shift = shift.checked_add(7).ok_or_else(invalid_meta)?;
    }
}

fn append_varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        let byte = u8::try_from(value & 0x7F).unwrap_or_default();
        bytes.push(byte | 0x80);
        value >>= 7;
    }
    bytes.push(u8::try_from(value & 0x7F).unwrap_or_default());
}

fn append_bytes_field(bytes: &mut Vec<u8>, field: u32, value: &[u8]) {
    append_varint(bytes, u64::from(field) << 3 | 2);
    append_varint(bytes, u64::try_from(value.len()).unwrap_or_default());
    bytes.extend_from_slice(value);
}

fn append_varint_field(bytes: &mut Vec<u8>, field: u32, value: u64) {
    append_varint(bytes, u64::from(field) << 3);
    append_varint(bytes, value);
}

fn entry_names(
    archive: &ZipArchive<File>,
) -> Result<BTreeSet<String>, FormatError> {
    archive
        .file_names()
        .map(|name| {
            name.map(Cow::into_owned)
                .map_err(zip_error("cannot read package entry names"))
        })
        .collect()
}

fn is_media_entry(name: &str) -> bool {
    name.starts_with("media/") && !name.ends_with('/')
}

fn extract_media_file(
    archive: &mut ZipArchive<File>,
    media_dir: &Path,
    (entry, name): (&str, &str),
) -> Result<(), FormatError> {
    let filename = sanitize_filename(name);
    if filename.is_empty() {
        return Err(FormatError::new(format!(
            "invalid media filename in package: '{name}'"
        )));
    }
    let target = media_dir.join(filename);
    if target.exists() {
        return Ok(());
    }
    let bytes = maybe_decompress(read_bytes(archive, entry)?)?;
    write_new_file(&target, &bytes, &target.display().to_string())
}

fn read_bytes(
    archive: &mut ZipArchive<File>,
    name: &str,
) -> Result<Vec<u8>, FormatError> {
    let mut entry = archive
        .by_name(name)
        .map_err(zip_error(format!("cannot read package entry '{name}'")))?;
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(io_error(format!("cannot read package entry '{name}'")))?;
    Ok(bytes)
}

struct MappingEntry {
    index: usize,
    name: String,
    size: Option<u32>,
    sha1: Option<[u8; 20]>,
}

fn read_media_entries(
    archive: &mut ZipArchive<File>,
    names: &BTreeSet<String>,
    version: PackageVersion,
) -> Result<Vec<MediaEntry>, FormatError> {
    let bytes = match archive.by_name("media") {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(io_error("cannot read media mapping"))?;
            decode_versioned(bytes, version.zstd(), "media mapping")?
        }
        Err(ZipError::FileNotFound) => Vec::new(),
        Err(error) => {
            return Err(zip_error("cannot read media mapping")(error));
        }
    };
    let mapping = if version.protobuf_media_map() {
        parse_protobuf_mapping(&bytes)?
    } else {
        parse_legacy_mapping(&bytes)?
    };
    let mut media = Vec::new();
    for entry in mapping {
        if !names.contains(&entry.index.to_string()) {
            return Err(FormatError::new(format!(
                "media entry '{}' is missing from package",
                entry.index
            )));
        }
        let bytes = decode_versioned(
            read_bytes(archive, &entry.index.to_string())?,
            version.zstd(),
            "media file",
        )?;
        if let Some(size) = entry.size
            && u64::try_from(bytes.len()).unwrap_or_default() != u64::from(size)
        {
            return Err(FormatError::new(format!(
                "media file '{}' has the wrong size",
                entry.name
            )));
        }
        if let Some(expected) = entry.sha1
            && sha1_bytes(&bytes) != expected
        {
            return Err(FormatError::new(format!(
                "media file '{}' has the wrong checksum",
                entry.name
            )));
        }
        media.push(MediaEntry::new(entry.name, bytes));
    }
    Ok(media)
}

fn parse_legacy_mapping(
    bytes: &[u8],
) -> Result<Vec<MappingEntry>, FormatError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let map: BTreeMap<String, String> = serde_json::from_slice(bytes)
        .map_err(|_| FormatError::new("invalid media mapping in package"))?;
    let mut entries = Vec::new();
    for (raw_index, name) in map {
        let index = raw_index.parse::<usize>().map_err(|_| {
            FormatError::new("invalid media entry index in package")
        })?;
        if sanitize_filename(&name).is_empty() {
            return Err(FormatError::new("invalid media filename in package"));
        }
        entries.push(MappingEntry {
            index,
            name,
            size: None,
            sha1: None,
        });
    }
    entries.sort_by_key(|entry| entry.index);
    Ok(entries)
}

fn parse_protobuf_mapping(
    bytes: &[u8],
) -> Result<Vec<MappingEntry>, FormatError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let (tag, next) = read_varint(bytes, offset)?;
        offset = next;
        if tag >> 3 != 1 || tag & 0x07 != 2 {
            return Err(invalid_media_mapping());
        }
        let (raw_length, next) = read_varint(bytes, offset)?;
        offset = next;
        let length =
            usize::try_from(raw_length).map_err(|_| invalid_media_mapping())?;
        let end = offset
            .checked_add(length)
            .ok_or_else(invalid_media_mapping)?;
        let message =
            bytes.get(offset..end).ok_or_else(invalid_media_mapping)?;
        offset = end;
        entries.push(parse_media_entry(message, entries.len())?);
    }
    Ok(entries)
}

#[expect(
    clippy::excessive_nesting,
    reason = "protobuf field handling is clearer inline"
)]
fn parse_media_entry(
    message: &[u8],
    fallback_index: usize,
) -> Result<MappingEntry, FormatError> {
    let mut offset = 0;
    let mut name = None;
    let mut size = None;
    let mut sha1 = None;
    let mut index = None;
    while offset < message.len() {
        let (tag, next) = read_varint(message, offset)?;
        offset = next;
        let field = tag >> 3;
        match tag & 0x07 {
            0 => {
                let (value, next) = read_varint(message, offset)?;
                offset = next;
                if field == 2 {
                    size = Some(
                        u32::try_from(value)
                            .map_err(|_| invalid_media_mapping())?,
                    );
                } else if field == 255 {
                    index = Some(
                        usize::try_from(value)
                            .map_err(|_| invalid_media_mapping())?,
                    );
                }
            }
            2 => {
                let (raw_length, next) = read_varint(message, offset)?;
                offset = next;
                let length = usize::try_from(raw_length)
                    .map_err(|_| invalid_media_mapping())?;
                let end = offset
                    .checked_add(length)
                    .ok_or_else(invalid_media_mapping)?;
                let value = message
                    .get(offset..end)
                    .ok_or_else(invalid_media_mapping)?;
                offset = end;
                match field {
                    1 => {
                        name = Some(
                            str::from_utf8(value)
                                .map_err(|_| invalid_media_mapping())?
                                .to_owned(),
                        );
                    }
                    3 => {
                        let mut hash = [0_u8; 20];
                        if value.len() != hash.len() {
                            return Err(invalid_media_mapping());
                        }
                        hash.copy_from_slice(value);
                        sha1 = Some(hash);
                    }
                    _ => {}
                }
            }
            _ => return Err(invalid_media_mapping()),
        }
    }
    let original_name = name.ok_or_else(invalid_media_mapping)?;
    if sanitize_filename(&original_name).is_empty() {
        return Err(invalid_media_mapping());
    }
    let file_size = size.unwrap_or_default();
    let file_sha1 = sha1.ok_or_else(invalid_media_mapping)?;
    Ok(MappingEntry {
        index: index.unwrap_or(fallback_index),
        name: original_name,
        size: Some(file_size),
        sha1: Some(file_sha1),
    })
}

fn invalid_media_mapping() -> FormatError {
    FormatError::new("invalid media mapping in package")
}

fn legacy_media_mapping(
    media_files: &[(String, Vec<u8>)],
) -> BTreeMap<String, String> {
    media_files
        .iter()
        .enumerate()
        .map(|(index, (name, _))| (index.to_string(), name.clone()))
        .collect()
}

fn encode_media_entries(
    media_files: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, FormatError> {
    let mut bytes = Vec::new();
    for (name, data) in media_files {
        let hash = sha1_bytes(data);
        let size = u32::try_from(data.len()).map_err(|_| {
            FormatError::new("media file is too large for a package")
        })?;
        let mut entry = Vec::new();
        append_bytes_field(&mut entry, 1, name.as_bytes());
        append_varint_field(&mut entry, 2, u64::from(size));
        append_bytes_field(&mut entry, 3, &hash);
        append_bytes_field(&mut bytes, 1, &entry);
    }
    Ok(bytes)
}

fn encode_zstd(bytes: &[u8]) -> Result<Vec<u8>, FormatError> {
    encode_all(bytes, 0).map_err(io_error("cannot compress package data"))
}

fn decode_versioned(
    bytes: Vec<u8>,
    compressed: bool,
    context: &str,
) -> Result<Vec<u8>, FormatError> {
    if compressed {
        decode_all(bytes.as_slice())
            .map_err(io_error(format!("cannot decompress package {context}")))
    } else {
        Ok(bytes)
    }
}

pub fn maybe_decompress(bytes: Vec<u8>) -> Result<Vec<u8>, FormatError> {
    if bytes.starts_with(ZSTD_MAGIC) {
        decode_all(bytes.as_slice())
            .map_err(io_error("cannot decompress zstd package data"))
    } else {
        Ok(bytes)
    }
}

fn write_temp_collection(
    bytes: &[u8],
    scratch_dir: &Path,
) -> Result<NamedTempFile, FormatError> {
    let mut file = Builder::new()
        .prefix(".ranki-import-")
        .suffix(".anki2")
        .tempfile_in(scratch_dir)
        .map_err(io_error("cannot create temporary collection"))?;
    file.write_all(bytes)
        .map_err(io_error("cannot write temporary collection"))?;
    file.flush()
        .map_err(io_error("cannot flush temporary collection"))?;
    Ok(file)
}

pub fn collection_schema_version(
    collection: &Path,
) -> Result<i64, FormatError> {
    let connection = Connection::open_with_flags(
        collection,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(db_error(format!(
        "cannot open collection {}",
        collection.display()
    )))?;
    let version: Option<i64> = connection
        .query_row("SELECT ver FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(db_error("cannot read collection schema version"))?;
    version.ok_or_else(|| FormatError::new("collection has no metadata row"))
}

pub fn choose_version(
    schema_version: i64,
    force_legacy: bool,
) -> Result<PackageVersion, FormatError> {
    match schema_version {
        11 => Ok(PackageVersion::Legacy2),
        18 if force_legacy => Err(FormatError::new(
            "a schema-18 collection cannot be exported as a legacy package",
        )),
        18 => Ok(PackageVersion::Latest),
        other => Err(FormatError::new(format!(
            "unsupported collection schema version: {other}"
        ))),
    }
}

pub fn validate_collection_ids(
    connection: &Connection,
) -> Result<(), FormatError> {
    let id: Option<i64> = connection
        .query_row("SELECT id FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(db_error("cannot read collection identifier"))?;
    if id.is_none() {
        return Err(FormatError::new("collection has no identifier"));
    }
    let now = schema::current_time()?
        .checked_mul(1000)
        .and_then(|millis| millis.checked_add(1_000))
        .and_then(|millis| millis.checked_add(FUTURE_ID_SLACK_MILLIS))
        .ok_or_else(|| FormatError::new("system clock out of range"))?;
    for table in ["notes", "cards", "revlog"] {
        if !table_exists(connection, table)? {
            continue;
        }
        let invalid_id: Option<i64> = connection
            .query_row(
                &format!(
                    "SELECT id FROM {table} WHERE id <= 0 OR id > ?1 LIMIT 1"
                ),
                params![now],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error(format!(
                "cannot validate {table} identifiers"
            )))?;
        if let Some(invalid) = invalid_id {
            return Err(FormatError::new(format!(
                "invalid {table} identifier: {invalid}"
            )));
        }
    }
    Ok(())
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the two export choices map directly onto CLI flags"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "connection, two export choices, and pruning are separate inputs"
)]
pub fn normalize_export_collection(
    connection: &Connection,
    with_scheduling: bool,
    with_deck_configs: bool,
    prune: bool,
    preserve_filtered: bool,
) -> Result<(), FormatError> {
    if plan_normalization(
        connection,
        with_scheduling,
        with_deck_configs,
        prune,
        preserve_filtered,
    )?
    .is_empty()
    {
        return Ok(());
    }
    apply_normalization(
        connection,
        with_scheduling,
        with_deck_configs,
        prune,
        preserve_filtered,
    )
}

#[derive(Debug, Default)]
struct NormalizationPlan {
    changes: usize,
}

impl NormalizationPlan {
    const fn add(&mut self, count: usize) {
        self.changes = self.changes.saturating_add(count);
    }

    const fn is_empty(&self) -> bool {
        self.changes == 0
    }
}

fn exists(connection: &Connection, sql: &str) -> Result<bool, FormatError> {
    connection
        .query_row(sql, [], |row| row.get::<_, bool>(0))
        .map_err(db_error("cannot plan export normalization"))
}

fn json_column(
    connection: &Connection,
    key: &str,
) -> Result<Option<Value>, FormatError> {
    let raw: Option<String> = connection
        .query_row(&format!("SELECT {key} FROM col WHERE id = 1"), [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(db_error("cannot plan export normalization"))?;
    Ok(raw.and_then(|text| serde_json::from_str::<Value>(&text).ok()))
}

#[expect(
    clippy::excessive_nesting,
    reason = "normalization planning mirrors the fixed write logic"
)]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the two export choices map directly onto CLI flags"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "connection, three export choices, and filtering mode are separate inputs"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the plan mirrors all normalization writes in one place"
)]
fn plan_normalization(
    connection: &Connection,
    with_scheduling: bool,
    with_deck_configs: bool,
    prune: bool,
    preserve_filtered: bool,
) -> Result<NormalizationPlan, FormatError> {
    let mut plan = NormalizationPlan::default();
    for sql in [
        "SELECT EXISTS(SELECT 1 FROM graves)",
        "SELECT EXISTS(SELECT 1 FROM cards WHERE nid NOT IN (SELECT id FROM notes))",
        "SELECT EXISTS(SELECT 1 FROM revlog WHERE cid NOT IN (SELECT id FROM cards))",
    ] {
        if exists(connection, sql)? {
            plan.add(1);
        }
    }
    if !with_scheduling
        && exists(
            connection,
            "SELECT EXISTS(SELECT 1 FROM revlog) OR EXISTS(
                SELECT 1 FROM cards WHERE type != 0 OR queue != 0 OR ivl != 0
                    OR factor != 0 OR reps != 0 OR lapses != 0 OR left != 0
                    OR odid != 0 OR odue != 0 OR flags != 0 OR data != ''
                    OR due != (
                        SELECT ordered.position - 1
                        FROM (SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS position FROM cards) AS ordered
                        WHERE ordered.id = cards.id
                    ))",
        )?
    {
        plan.add(1);
    }
    {
        let mut statement = connection
            .prepare("SELECT tags FROM notes")
            .map_err(db_error("cannot plan export normalization"))?;
        let tags = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_error("cannot plan export normalization"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error("cannot plan export normalization"))?;
        if tags.iter().any(|value| {
            let kept = value
                .split_whitespace()
                .filter(|tag| {
                    !tag.eq_ignore_ascii_case("marked")
                        && !tag.eq_ignore_ascii_case("leech")
                })
                .collect::<Vec<_>>()
                .join(" ");
            let canonical = if kept.is_empty() {
                String::new()
            } else {
                format!(" {kept} ")
            };
            canonical != *value
        }) {
            plan.add(1);
        }
    }
    if exists(
        connection,
        "SELECT EXISTS(SELECT 1 FROM notes WHERE usn < 0)
            OR EXISTS(SELECT 1 FROM cards WHERE usn < 0)
            OR EXISTS(SELECT 1 FROM revlog WHERE usn < 0)
            OR EXISTS(SELECT 1 FROM graves WHERE usn < 0)",
    )? {
        plan.add(1);
    }
    let used_decks = ids_from(connection, "SELECT DISTINCT did FROM cards")?;
    if let Some(Value::Object(object)) = json_column(connection, "decks")? {
        for (_, deck) in &object {
            if !with_scheduling
                && ["lrnToday", "revToday", "newToday", "timeToday"]
                    .iter()
                    .any(|key| {
                        deck.get(*key) != Some(&serde_json::json!([0, 0]))
                    })
            {
                plan.add(1);
            }
            let filtered = deck
                .get("dyn")
                .is_some_and(|value| value.as_i64().unwrap_or_default() != 0);
            if !preserve_filtered && filtered {
                plan.add(1);
            }
            if !with_deck_configs
                && deck.get("conf").and_then(conf_id) != Some(1)
            {
                plan.add(1);
            }
        }
        if prune && with_deck_configs {
            let expected = object
                .iter()
                .filter(|(key, _)| {
                    key.parse::<i64>().is_ok_and(|id| used_decks.contains(&id))
                })
                .filter_map(|(_, deck)| deck.get("conf").and_then(conf_id))
                .collect::<BTreeSet<_>>();
            let configured = json_column(connection, "dconf")?
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default()
                .keys()
                .filter_map(|key| key.parse::<i64>().ok())
                .collect::<BTreeSet<_>>();
            if configured != expected {
                plan.add(1);
            }
        }
    }
    let default_config = ranki_storage::schema11_default_deck_config(0);
    if !with_deck_configs {
        let expected = serde_json::json!({"1": default_config});
        if json_column(connection, "dconf")?.as_ref() != Some(&expected) {
            plan.add(1);
        }
    }
    if prune {
        let used_models =
            ids_from(connection, "SELECT DISTINCT mid FROM notes")?;
        for (key, expected) in
            [("models", &used_models), ("decks", &used_decks)]
        {
            if let Some(Value::Object(object)) = json_column(connection, key)? {
                let keys = object
                    .keys()
                    .filter_map(|key| key.parse::<i64>().ok())
                    .collect::<BTreeSet<_>>();
                if keys != *expected {
                    plan.add(1);
                }
            }
        }
        for (table, ids) in
            [("notetypes", &used_models), ("decks", &used_decks)]
        {
            if !table_exists(connection, table)? {
                continue;
            }
            let dirty = if ids.is_empty() {
                exists(
                    connection,
                    &format!("SELECT EXISTS(SELECT 1 FROM {table})"),
                )?
            } else {
                let mut statement = connection
                    .prepare(&format!(
                        "SELECT EXISTS(SELECT 1 FROM {table} WHERE id NOT IN ({}))",
                        placeholders(ids.len())
                    ))
                    .map_err(db_error("cannot plan export normalization"))?;
                statement
                    .query_row(params_from_iter(ids.iter().copied()), |row| {
                        row.get::<_, bool>(0)
                    })
                    .map_err(db_error("cannot plan export normalization"))?
            };
            if dirty {
                plan.add(1);
            }
        }
    }
    Ok(plan)
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the two export choices map directly onto CLI flags"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "connection, three export choices, and filtering mode are separate inputs"
)]
fn apply_normalization(
    connection: &Connection,
    with_scheduling: bool,
    with_deck_configs: bool,
    prune: bool,
    preserve_filtered: bool,
) -> Result<(), FormatError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(db_error("cannot start export normalization"))?;
    transaction
        .execute_batch(
            "DELETE FROM graves;
             DELETE FROM cards WHERE nid NOT IN (SELECT id FROM notes);
             DELETE FROM revlog WHERE cid NOT IN (SELECT id FROM cards);",
        )
        .map_err(db_error("cannot remove unused export records"))?;
    if !with_scheduling {
        transaction
            .execute_batch(
                "DELETE FROM revlog;
                 UPDATE cards SET did = odid WHERE odid != 0;
                 UPDATE cards SET type = 0, queue = 0,
                     due = (
                         SELECT ordered.position - 1
                         FROM (SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS position FROM cards) AS ordered
                         WHERE ordered.id = cards.id
                     ),
                     ivl = 0, factor = 0, reps = 0, lapses = 0, left = 0,
                     odid = 0, odue = 0, flags = 0, data = '';",
            )
            .map_err(db_error("cannot reset exported scheduling"))?;
        remove_system_tags(&transaction)?;
    }
    let filtered = normalize_decks(
        &transaction,
        with_scheduling,
        with_deck_configs,
        preserve_filtered,
    )?;
    if !preserve_filtered && with_scheduling && !filtered.is_empty() {
        let list = placeholders(filtered.len());
        transaction
            .execute(
                &format!(
                    "UPDATE cards SET
                        due = CASE WHEN odue > 0 THEN odue ELSE due END,
                        odid = 0, odue = 0
                     WHERE did IN ({list})"
                ),
                rusqlite::params_from_iter(filtered.iter().copied()),
            )
            .map_err(db_error("cannot restore cards from filtered decks"))?;
    }
    if prune {
        prune_metadata(&transaction, with_deck_configs)?;
    } else if !with_deck_configs {
        let mut configs = BTreeMap::new();
        configs.insert(
            "1".to_owned(),
            ranki_storage::schema11_default_deck_config(0),
        );
        transaction
            .execute(
                "UPDATE col SET dconf = ?1 WHERE id = 1",
                params![
                    serde_json::to_string(&configs)
                        .map_err(json_error("cannot encode deck configs"))?
                ],
            )
            .map_err(db_error("cannot clear exported deck configs"))?;
    }
    transaction
        .commit()
        .map_err(db_error("cannot commit export normalization"))
}

fn placeholders(count: usize) -> String {
    (0..count).map(|_| "?").collect::<Vec<_>>().join(", ")
}

fn remove_system_tags(connection: &Connection) -> Result<(), FormatError> {
    let mut statement = connection
        .prepare("SELECT id, tags FROM notes")
        .map_err(db_error("cannot read exported tags"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db_error("cannot read exported tags"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read exported tags"))?;
    drop(statement);
    let mut update = connection
        .prepare("UPDATE notes SET tags = ?1 WHERE id = ?2")
        .map_err(db_error("cannot write exported tags"))?;
    for (id, tags) in rows {
        let kept = tags
            .split_whitespace()
            .filter(|tag| {
                !tag.eq_ignore_ascii_case("marked")
                    && !tag.eq_ignore_ascii_case("leech")
            })
            .collect::<Vec<_>>()
            .join(" ");
        let normalized_tags = if kept.is_empty() {
            String::new()
        } else {
            format!(" {kept} ")
        };
        if normalized_tags != tags {
            update
                .execute(params![normalized_tags, id])
                .map_err(db_error("cannot write exported tags"))?;
        }
    }
    Ok(())
}

#[expect(
    clippy::excessive_nesting,
    reason = "deck metadata handling is clearer inline"
)]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the two export choices map directly onto CLI flags"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "connection and three export choices are separate inputs"
)]
fn normalize_decks(
    connection: &Connection,
    with_scheduling: bool,
    with_deck_configs: bool,
    preserve_filtered: bool,
) -> Result<Vec<i64>, FormatError> {
    let stored: Option<String> = connection
        .query_row("SELECT decks FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(db_error("cannot read exported decks"))?;
    let Some(decks_json) = stored.filter(|value| !value.trim().is_empty())
    else {
        return Ok(Vec::new());
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&decks_json) else {
        return Ok(Vec::new());
    };
    let mut filtered = Vec::new();
    if let Some(object) = value.as_object_mut() {
        for (key, entry) in object.iter_mut() {
            let is_filtered = entry
                .get("dyn")
                .is_some_and(|value| value.as_i64().unwrap_or_default() != 0);
            if !with_scheduling && let Some(deck) = entry.as_object_mut() {
                for key in ["lrnToday", "revToday", "newToday", "timeToday"] {
                    let _ =
                        deck.insert(key.to_owned(), serde_json::json!([0, 0]));
                }
            }
            if !is_filtered || preserve_filtered {
                continue;
            }
            if let Ok(id) = key.parse::<i64>() {
                filtered.push(id);
            }
            if let Some(filtered) = entry.as_object_mut() {
                let _ = filtered.insert("dyn".to_owned(), Value::from(0));
                for key in ["lrnToday", "revToday", "newToday", "timeToday"] {
                    let _ = filtered
                        .insert(key.to_owned(), serde_json::json!([0, 0]));
                }
                let _ = filtered.insert("extendNew".to_owned(), Value::from(0));
                let _ = filtered.insert("extendRev".to_owned(), Value::from(0));
                if with_deck_configs
                    && filtered.get("conf").and_then(conf_id).is_none()
                {
                    let _ = filtered.insert("conf".to_owned(), Value::from(1));
                }
            }
        }
        if !with_deck_configs {
            for entry in object.values_mut() {
                if let Some(deck) = entry.as_object_mut() {
                    let _ = deck.insert("conf".to_owned(), Value::from(1));
                }
            }
        }
    }
    let encoded = serde_json::to_string(&value)
        .map_err(json_error("cannot write exported decks"))?;
    if encoded != decks_json {
        connection
            .execute("UPDATE col SET decks = ?1 WHERE id = 1", params![encoded])
            .map_err(db_error("cannot write exported decks"))?;
    }
    Ok(filtered)
}

fn prune_metadata(
    connection: &Connection,
    with_deck_configs: bool,
) -> Result<(), FormatError> {
    let used_models = ids_from(connection, "SELECT DISTINCT mid FROM notes")?;
    let used_decks = ids_from(connection, "SELECT DISTINCT did FROM cards")?;
    let used_original = ids_from(
        connection,
        "SELECT DISTINCT odid FROM cards WHERE odid != 0",
    )?;
    let mut decks = used_decks.clone();
    decks.extend(used_original);
    if let Some(models) = used_models.first() {
        let _ = models;
    }
    let names = deck_name_index(connection)?;
    let by_name: BTreeMap<String, i64> =
        names.iter().map(|(id, name)| (name.clone(), *id)).collect();
    for deck in used_decks.iter().copied().collect::<Vec<_>>() {
        add_parent_decks(&mut decks, &names, &by_name, deck);
    }
    prune_json_ids(connection, "decks", &decks)?;
    prune_json_ids(connection, "models", &used_models)?;
    let configs = if with_deck_configs {
        json_config_ids(connection, "decks", &decks)?
    } else {
        BTreeSet::from([1_i64])
    };
    prune_json_ids(connection, "dconf", &configs)?;
    if !with_deck_configs {
        let mut defaults = BTreeMap::new();
        defaults.insert(
            "1".to_owned(),
            ranki_storage::schema11_default_deck_config(0),
        );
        connection
            .execute(
                "UPDATE col SET dconf = ?1 WHERE id = 1",
                params![
                    serde_json::to_string(&defaults)
                        .map_err(json_error("cannot encode deck configs"))?
                ],
            )
            .map_err(db_error("cannot reset exported deck configs"))?;
    }
    if table_exists(connection, "notetypes")? {
        let list = placeholders(used_models.len());
        connection
            .execute(
                &format!("DELETE FROM notetypes WHERE id NOT IN ({list})"),
                rusqlite::params_from_iter(used_models.iter().copied()),
            )
            .map_err(db_error("cannot prune exported note types"))?;
        connection
            .execute_batch(
                "DELETE FROM fields WHERE ntid NOT IN (SELECT id FROM notetypes);
                 DELETE FROM templates WHERE ntid NOT IN (SELECT id FROM notetypes);",
            )
            .map_err(db_error("cannot prune exported note type fields"))?;
    }
    if table_exists(connection, "decks")? && !decks.is_empty() {
        let list = placeholders(decks.len());
        connection
            .execute(
                &format!("DELETE FROM decks WHERE id NOT IN ({list})"),
                rusqlite::params_from_iter(decks.iter().copied()),
            )
            .map_err(db_error("cannot prune exported decks"))?;
    }
    Ok(())
}

fn deck_name_index(
    connection: &Connection,
) -> Result<BTreeMap<i64, String>, FormatError> {
    let stored: Option<String> = connection
        .query_row("SELECT decks FROM col WHERE id = 1", [], |row| row.get(0))
        .optional()
        .map_err(db_error("cannot read deck names"))?;
    let Some(json_text) = stored else {
        return Ok(BTreeMap::new());
    };
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&json_text)
    else {
        return Ok(BTreeMap::new());
    };
    Ok(object
        .into_iter()
        .filter_map(|(id, deck)| {
            let deck_id = id.parse::<i64>().ok()?;
            let name = deck.get("name")?.as_str()?.to_owned();
            Some((deck_id, name))
        })
        .collect())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the deck sets, lookup maps, and starting deck are separate inputs"
)]
fn add_parent_decks(
    decks: &mut BTreeSet<i64>,
    names: &BTreeMap<i64, String>,
    by_name: &BTreeMap<String, i64>,
    deck_id: i64,
) {
    let Some(name) = names.get(&deck_id) else {
        return;
    };
    let mut parts = name.split("::").collect::<Vec<_>>();
    while parts.len() > 1 {
        parts.pop();
        let parent = parts.join("::");
        if let Some(id) = by_name.get(&parent) {
            let _ = decks.insert(*id);
        }
    }
}

fn ids_from(
    connection: &Connection,
    sql: &str,
) -> Result<BTreeSet<i64>, FormatError> {
    let mut statement = connection
        .prepare(sql)
        .map_err(db_error("cannot read export identifiers"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, Option<i64>>(0))
        .map_err(db_error("cannot read export identifiers"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error("cannot read export identifiers"))?;
    Ok(rows.into_iter().flatten().collect())
}

fn conf_id(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
}

fn json_config_ids(
    connection: &Connection,
    key: &str,
    ids: &BTreeSet<i64>,
) -> Result<BTreeSet<i64>, FormatError> {
    let stored: Option<String> = connection
        .query_row(&format!("SELECT {key} FROM col WHERE id = 1"), [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(db_error("cannot read exported metadata"))?;
    let Some(json_text) = stored else {
        return Ok(BTreeSet::new());
    };
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&json_text)
    else {
        return Ok(BTreeSet::new());
    };
    Ok(object
        .into_iter()
        .filter(|(id, _)| {
            id.parse::<i64>().is_ok_and(|value| ids.contains(&value))
        })
        .filter_map(|(_, deck)| deck.get("conf").and_then(conf_id))
        .collect())
}

fn prune_json_ids(
    connection: &Connection,
    key: &str,
    ids: &BTreeSet<i64>,
) -> Result<(), FormatError> {
    let stored: Option<String> = connection
        .query_row(&format!("SELECT {key} FROM col WHERE id = 1"), [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(db_error("cannot read exported metadata"))?;
    let Some(json_text) = stored.filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&json_text) else {
        return Ok(());
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    object.retain(|id, _| {
        id.parse::<i64>().is_ok_and(|value| ids.contains(&value))
    });
    let encoded = serde_json::to_string(&value)
        .map_err(json_error("cannot write exported metadata"))?;
    if encoded != json_text {
        connection
            .execute(
                &format!("UPDATE col SET {key} = ?1 WHERE id = 1"),
                params![encoded],
            )
            .map_err(db_error("cannot write exported metadata"))?;
    }
    Ok(())
}

fn table_exists(
    connection: &Connection,
    table: &str,
) -> Result<bool, FormatError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![table],
            |row| row.get(0),
        )
        .map_err(db_error("cannot inspect collection tables"))?;
    Ok(exists)
}

pub fn backup_collection(
    collection: &Path,
    backup: &Path,
) -> Result<(), FormatError> {
    let source = Connection::open(collection).map_err(db_error(format!(
        "cannot open collection {}",
        collection.display()
    )))?;
    source
        .backup(MAIN_DB, backup, None::<fn(Progress)>)
        .map_err(db_error("cannot back up collection"))
}

pub fn snapshot_collection(collection: &Path) -> Result<Vec<u8>, FormatError> {
    if !collection.is_file() {
        return Err(FormatError::new(format!(
            "collection does not exist: {}",
            collection.display()
        )));
    }
    let source = Connection::open(collection).map_err(db_error(format!(
        "cannot open collection {}",
        collection.display()
    )))?;
    let parent = collection
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let snapshot = Builder::new()
        .prefix(".ranki-export-")
        .suffix(".anki2")
        .tempfile_in(parent)
        .map_err(io_error("cannot create collection snapshot"))?;
    let mut snapshot_db = Connection::open(snapshot.path())
        .map_err(db_error("cannot open collection snapshot"))?;
    snapshot_db
        .pragma_update(None, "journal_mode", "MEMORY")
        .map_err(db_error("cannot configure collection snapshot"))?;
    snapshot_db
        .pragma_update(None, "synchronous", "OFF")
        .map_err(db_error("cannot configure collection snapshot"))?;
    {
        let backup = Backup::new(&source, &mut snapshot_db)
            .map_err(db_error("cannot snapshot collection"))?;
        let mut result = backup
            .step(100)
            .map_err(db_error("cannot snapshot collection"))?;
        while result == StepResult::More {
            result = backup
                .step(100)
                .map_err(db_error("cannot snapshot collection"))?;
        }
        if result != StepResult::Done {
            return Err(FormatError::new("collection snapshot is busy"));
        }
    }
    drop(source);
    ranki_storage::register_collations(&snapshot_db).map_err(|error| {
        FormatError::with_source("cannot register collection collations", error)
    })?;
    {
        let transaction = snapshot_db
            .transaction()
            .map_err(db_error("cannot normalize collection snapshot"))?;
        normalize_pending_usns(&transaction)?;
        normalize_col_metadata(&transaction)?;
        transaction
            .commit()
            .map_err(db_error("cannot commit collection snapshot"))?;
    }
    drop(snapshot_db);
    fs::read(snapshot.path())
        .map_err(io_error("cannot read collection snapshot"))
}

fn normalize_pending_usns(conn: &Connection) -> Result<(), FormatError> {
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .map_err(db_error("cannot inspect collection snapshot"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_error("cannot inspect collection snapshot"))?;
    let mut tables = Vec::new();
    for row in rows {
        tables
            .push(row.map_err(db_error("cannot inspect collection snapshot"))?);
    }
    drop(statement);
    for table in tables {
        if [
            "notes",
            "cards",
            "revlog",
            "graves",
            "config",
            "notetypes",
            "templates",
            "decks",
            "deck_config",
            "tags",
            "col",
        ]
        .contains(&table.as_str())
        {
            normalize_table_usns(conn, &table)?;
        }
    }
    Ok(())
}

fn normalize_table_usns(
    conn: &Connection,
    table: &str,
) -> Result<(), FormatError> {
    let pending: i64 = conn
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE usn < 0)"),
            [],
            |row| row.get(0),
        )
        .map_err(db_error("cannot normalize pending sync ids"))?;
    if pending == 0 {
        return Ok(());
    }
    conn.execute(&format!("UPDATE {table} SET usn = 0 WHERE usn < 0"), [])
        .map_err(db_error("cannot normalize pending sync ids"))?;
    Ok(())
}

fn normalize_col_metadata(conn: &Connection) -> Result<(), FormatError> {
    for key in ["conf", "models", "decks", "dconf", "tags"] {
        let raw: Option<String> = conn
            .query_row(
                &format!("SELECT {key} FROM col WHERE id = 1"),
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error("cannot read collection metadata"))?;
        let Some(value) = raw else {
            continue;
        };
        if !value.contains("\"usn\":-1") {
            continue;
        }
        let serialized = normalize_metadata(&value)?;
        if serialized != value {
            conn.execute(
                &format!("UPDATE col SET {key} = ?1 WHERE id = 1"),
                [serialized],
            )
            .map_err(db_error("cannot normalize pending sync ids"))?;
        }
    }
    Ok(())
}

fn normalize_metadata(raw: &str) -> Result<String, FormatError> {
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        return Ok(raw.to_owned());
    };
    serde_json::to_string(&normalize_usn(parsed))
        .map_err(json_error("cannot encode collection metadata"))
}

fn normalize_usn(value: Value) -> Value {
    match value {
        Value::Object(mut map) => {
            if map
                .get("usn")
                .is_some_and(|entry| entry.as_i64() == Some(-1))
            {
                let _ = map.insert("usn".to_owned(), Value::from(0));
            }
            Value::Object(
                map.into_iter()
                    .map(|(key, entry)| (key, normalize_usn(entry)))
                    .collect(),
            )
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(normalize_usn).collect())
        }
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)) => other,
    }
}

static DUMMY_COLLECTION: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();

fn dummy_collection() -> Result<Vec<u8>, FormatError> {
    match DUMMY_COLLECTION.get_or_init(|| {
        build_dummy_collection().map_err(|error| error.to_string())
    }) {
        Ok(bytes) => Ok(bytes.clone()),
        Err(error) => Err(FormatError::new(error.clone())),
    }
}

fn build_dummy_collection() -> Result<Vec<u8>, FormatError> {
    let directory = Builder::new()
        .prefix(".ranki-dummy-")
        .tempdir()
        .map_err(io_error("cannot create dummy collection"))?;
    let path = directory.path().join("collection.anki2");
    let mut collection =
        ranki_storage::Collection::create(&path).map_err(|error| {
            FormatError::with_source("cannot create dummy collection", error)
        })?;
    collection
        .add_note(&NewNote {
            model_id: 1,
            deck_id: 1,
            fields: vec!["compatibility".to_owned(), "note".to_owned()],
            tags: Vec::new(),
        })
        .map_err(|error| {
            FormatError::with_source("cannot fill dummy collection", error)
        })?;
    drop(collection);
    let bytes =
        fs::read(&path).map_err(io_error("cannot read dummy collection"))?;
    Ok(bytes)
}

#[expect(
    clippy::too_many_arguments,
    reason = "zip entry name, bytes, and compression are separate inputs"
)]
fn write_entry(
    writer: &mut ZipWriter<BufWriter<File>>,
    name: &str,
    bytes: &[u8],
    method: CompressionMethod,
) -> Result<(), FormatError> {
    let options = SimpleFileOptions::default().compression_method(method);
    writer
        .start_file(name, options)
        .map_err(zip_error(format!("cannot add package entry '{name}'")))?;
    writer
        .write_all(bytes)
        .map_err(io_error(format!("cannot write package entry '{name}'")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::schema::open_or_create;
    use ranki_model::Store as _;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use rusqlite::Connection;
    use rusqlite::hooks::AuthAction;
    use rusqlite::hooks::AuthContext;
    use rusqlite::hooks::Authorization;
    use serde_json::Value;

    use super::normalize_col_metadata;
    use super::normalize_pending_usns;

    fn metadata_db(value: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE col (
                id INTEGER PRIMARY KEY, conf TEXT NOT NULL,
                models TEXT NOT NULL, decks TEXT NOT NULL,
                dconf TEXT NOT NULL, tags TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute("INSERT INTO col VALUES (1, ?1, ?1, ?1, ?1, ?1)", [value])
            .unwrap();
        conn
    }

    fn col_value(conn: &Connection, key: &str) -> Value {
        let raw: String = conn
            .query_row(
                &format!("SELECT {key} FROM col WHERE id = 1"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn update_counter(conn: &mut Connection) -> Arc<AtomicUsize> {
        let updates = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&updates);
        conn.authorizer(Some(move |context: AuthContext<'_>| {
            let is_update = matches!(context.action, AuthAction::Update { .. });
            is_update.then(|| counted.fetch_add(1, Ordering::Relaxed));
            Authorization::Allow
        }))
        .unwrap();
        updates
    }

    #[test]
    fn clean_snapshot_skips_normalization_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clean.anki2");
        open_or_create(&path, None).unwrap();
        {
            let mut store = ranki_storage::Collection::open(&path).unwrap();
            store
                .add_note(&ranki_model::NewNote {
                    model_id: ranki_model::DEFAULT_MODEL_ID,
                    deck_id: ranki_model::DEFAULT_DECK_ID,
                    fields: vec!["front".to_owned(), "back".to_owned()],
                    tags: Vec::new(),
                })
                .unwrap();
        }
        let mut connection = open_or_create(&path, None).unwrap();
        let writes = update_counter(&mut connection);
        super::normalize_export_collection(
            &connection,
            true,
            true,
            false,
            false,
        )
        .unwrap();
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn dirty_snapshot_resets_study_counters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counters.anki2");
        open_or_create(&path, None).unwrap();
        {
            let mut store = ranki_storage::Collection::open(&path).unwrap();
            store
                .add_note(&ranki_model::NewNote {
                    model_id: ranki_model::DEFAULT_MODEL_ID,
                    deck_id: ranki_model::DEFAULT_DECK_ID,
                    fields: vec!["front".to_owned(), "back".to_owned()],
                    tags: Vec::new(),
                })
                .unwrap();
        }
        let connection = open_or_create(&path, None).unwrap();
        let raw: String = connection
            .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut decks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        decks["1"]["newToday"] = serde_json::json!([1, 2]);
        connection
            .execute(
                "UPDATE col SET decks = ?1 WHERE id = 1",
                [decks.to_string()],
            )
            .unwrap();
        super::normalize_export_collection(
            &connection,
            false,
            true,
            true,
            false,
        )
        .unwrap();
        let decks: serde_json::Value = serde_json::from_str(
            &connection
                .query_row("SELECT decks FROM col WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
        )
        .unwrap();
        assert_eq!(decks["1"]["newToday"], serde_json::json!([0, 0]));
    }

    #[test]
    fn dirty_snapshot_prunes_unreferenced_configs_when_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("configs.anki2");
        open_or_create(&path, None).unwrap();
        {
            let mut store = ranki_storage::Collection::open(&path).unwrap();
            store
                .add_note(&ranki_model::NewNote {
                    model_id: ranki_model::DEFAULT_MODEL_ID,
                    deck_id: ranki_model::DEFAULT_DECK_ID,
                    fields: vec!["front".to_owned(), "back".to_owned()],
                    tags: Vec::new(),
                })
                .unwrap();
        }
        let connection = open_or_create(&path, None).unwrap();
        let raw: String = connection
            .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut configs: serde_json::Value =
            serde_json::from_str(&raw).unwrap();
        configs["9"] = configs["1"].clone();
        connection
            .execute(
                "UPDATE col SET dconf = ?1 WHERE id = 1",
                [configs.to_string()],
            )
            .unwrap();
        super::normalize_export_collection(
            &connection,
            true,
            true,
            true,
            false,
        )
        .unwrap();
        let configs: serde_json::Value = serde_json::from_str(
            &connection
                .query_row("SELECT dconf FROM col WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
        )
        .unwrap();
        assert!(configs.get("9").is_none());
    }

    #[test]
    fn dirty_snapshot_canonicalizes_tag_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tags.anki2");
        open_or_create(&path, None).unwrap();
        {
            let mut store = ranki_storage::Collection::open(&path).unwrap();
            store
                .add_note(&ranki_model::NewNote {
                    model_id: ranki_model::DEFAULT_MODEL_ID,
                    deck_id: ranki_model::DEFAULT_DECK_ID,
                    fields: vec!["front".to_owned(), "back".to_owned()],
                    tags: vec!["foo".to_owned(), "bar".to_owned()],
                })
                .unwrap();
        }
        let connection = open_or_create(&path, None).unwrap();
        connection
            .execute("UPDATE notes SET tags = 'foo  bar'", [])
            .unwrap();
        super::normalize_export_collection(
            &connection,
            false,
            true,
            true,
            false,
        )
        .unwrap();
        let tags: String = connection
            .query_row("SELECT tags FROM notes LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tags, " foo bar ");
    }

    #[test]
    fn dirty_snapshot_still_runs_normalization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dirty.anki2");
        let connection = open_or_create(&path, None).unwrap();
        connection
            .execute(
                "INSERT INTO graves (usn, oid, type) VALUES (0, 99, 1)",
                [],
            )
            .unwrap();
        super::normalize_export_collection(
            &connection,
            true,
            true,
            true,
            false,
        )
        .unwrap();
        let graves: i64 = connection
            .query_row("SELECT COUNT(*) FROM graves", [], |row| row.get(0))
            .unwrap();
        assert_eq!(graves, 0);
    }

    #[test]
    fn pending_usn_normalization_skips_tables_without_negative_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, usn INTEGER NOT NULL);
             CREATE TABLE cards (id INTEGER PRIMARY KEY, usn INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO notes VALUES (1, 0)", []).unwrap();
        conn.execute("INSERT INTO cards VALUES (1, 2)", []).unwrap();
        let updates = update_counter(&mut conn);

        normalize_pending_usns(&conn).unwrap();

        assert_eq!(updates.load(Ordering::Relaxed), 0);
        let usn: i64 = conn
            .query_row("SELECT usn FROM notes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(usn, 0);
    }

    #[test]
    fn pending_usn_normalization_still_rewrites_negative_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, usn INTEGER NOT NULL);
             CREATE TABLE cards (id INTEGER PRIMARY KEY, usn INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO notes VALUES (1, -1)", [])
            .unwrap();
        conn.execute("INSERT INTO cards VALUES (1, 0)", []).unwrap();
        let updates = update_counter(&mut conn);

        normalize_pending_usns(&conn).unwrap();

        assert_eq!(updates.load(Ordering::Relaxed), 1);
        let usn: i64 = conn
            .query_row("SELECT usn FROM notes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(usn, 0);
    }

    #[test]
    fn metadata_normalization_skips_already_normalized_values() {
        let value = r#"{"nested":[{"keep":2,"usn":-1.0}],"usn":-1.0}"#;
        let mut conn = metadata_db(value);
        let updates = update_counter(&mut conn);

        normalize_col_metadata(&conn).unwrap();

        assert_eq!(updates.load(Ordering::Relaxed), 0);
        assert_eq!(
            col_value(&conn, "conf"),
            serde_json::from_str::<Value>(value).unwrap()
        );
    }

    #[test]
    fn metadata_normalization_still_rewrites_negative_usns() {
        let conn = metadata_db(r#"{"nested":[{"keep":2,"usn":-1}],"usn":-1}"#);

        normalize_col_metadata(&conn).unwrap();

        let expected = serde_json::json!({
            "nested": [{"keep": 2, "usn": 0}],
            "usn": 0,
        });
        assert_eq!(col_value(&conn, "conf"), expected);
        assert_eq!(col_value(&conn, "models"), expected);
        assert_eq!(col_value(&conn, "decks"), expected);
        assert_eq!(col_value(&conn, "dconf"), expected);
        assert_eq!(col_value(&conn, "tags"), expected);
    }
}
