use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use sha1::Digest;
use sha1::Sha1;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::is_nfc;

use crate::FormatError;
use crate::error::io_error;
use crate::media_db::MediaRecord;

use crate::media_db::record_media;
use crate::text::rewrite_media_references;

const MAX_MEDIA_FILENAME_LENGTH: usize = 120;

/// A media file stored in a package.
#[derive(Debug, Clone)]
pub struct MediaEntry {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl MediaEntry {
    pub const fn new(name: String, bytes: Vec<u8>) -> Self {
        Self { name, bytes }
    }
}

/// A filename change that must be applied to note fields.
#[derive(Debug, Clone)]
pub struct MediaRename {
    pub from: String,
    pub to: String,
}

/// A media file found in a media directory during export.
#[derive(Debug, Clone)]
pub struct MediaFile {
    pub name: String,
    pub path: PathBuf,
}

/// Normalizes a media filename the way Anki does.
#[expect(
    clippy::manual_pattern_char_comparison,
    reason = "an array pattern matches a sequence, not either separator"
)]
pub fn sanitize_filename(name: &str) -> String {
    let normalized: Cow<'_, str> = if is_nfc(name) {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(name.nfc().collect())
    };
    let base = normalized
        .rsplit(|character| character == '/' || character == '\\')
        .next()
        .unwrap_or(&normalized);
    let cleaned: String = base
        .chars()
        .filter(|character| !is_disallowed_char(*character))
        .map(|character| {
            if character == '\u{a0}' {
                ' '
            } else {
                character
            }
        })
        .collect();
    let device = append_device_suffix(&cleaned);
    let trailing = append_trailing_suffix(&device);
    truncate_filename(&trailing)
}

const fn is_disallowed_char(character: char) -> bool {
    matches!(
        character,
        '[' | ']' | '<' | '>' | ':' | '"' | '/' | '?' | '*' | '^' | '\\' | '|'
    ) || character.is_ascii_control()
}

fn append_device_suffix(name: &str) -> String {
    let (stem, extension) = name.split_once('.').unwrap_or((name, ""));
    let device = stem.to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && device.as_bytes().get(3).is_some_and(u8::is_ascii_digit)
            && device.as_bytes().get(3) != Some(&b'0'));
    if reserved {
        if extension.is_empty() {
            format!("{name}_")
        } else {
            format!("{stem}_.{extension}")
        }
    } else {
        name.to_owned()
    }
}

fn append_trailing_suffix(name: &str) -> String {
    if name.ends_with('.') || name.ends_with(' ') {
        format!("{name}_")
    } else {
        name.to_owned()
    }
}

fn truncate_filename(name: &str) -> String {
    if name.len() <= MAX_MEDIA_FILENAME_LENGTH {
        return name.to_owned();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, extension),
        _ => (name, ""),
    };
    let ext = truncate_boundary(extension, 10);
    let stem_length = MAX_MEDIA_FILENAME_LENGTH
        .saturating_sub(ext.len())
        .saturating_sub(if ext.is_empty() { 1 } else { 2 });
    let base = truncate_boundary(stem, stem_length);
    let mut output = if ext.is_empty() {
        base.to_owned()
    } else {
        format!("{base}.{ext}")
    };
    if output.ends_with('.') || output.ends_with(' ') {
        output.push('_');
    }
    output
}

fn truncate_boundary(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.get(..end).unwrap_or_default()
}

pub fn collect_media_files(
    media_dir: &Path,
) -> Result<Vec<MediaFile>, FormatError> {
    if !media_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let entries = fs::read_dir(media_dir).map_err(io_error(format!(
        "cannot read media directory {}",
        media_dir.display()
    )))?;
    for directory_entry in entries {
        let entry = directory_entry.map_err(io_error(format!(
            "cannot read media directory {}",
            media_dir.display()
        )))?;
        let file_type = entry.file_type().map_err(io_error(format!(
            "cannot inspect media file {}",
            entry.path().display()
        )))?;
        if !file_type.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return Err(FormatError::new(format!(
                "media filename is not valid UTF-8: {}",
                entry.path().display()
            )));
        };
        files.push(MediaFile {
            name: name.to_owned(),
            path: entry.path(),
        });
    }
    files.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(files)
}

#[expect(
    clippy::excessive_nesting,
    reason = "media copying has a fixed set of filename and collision cases"
)]
pub fn copy_media_with_db(
    media_dir: &Path,
    media_db: &Path,
    entries: &[MediaEntry],
) -> Result<Vec<MediaRename>, FormatError> {
    fs::create_dir_all(media_dir).map_err(io_error(format!(
        "cannot create media directory {}",
        media_dir.display()
    )))?;

    let exact_names: HashSet<&str> =
        entries.iter().map(|entry| entry.name.as_str()).collect();
    let mut cache = MediaCache::new(media_dir);
    let mut renames = Vec::new();
    let mut records = Vec::new();
    for entry in entries {
        let normalized = sanitize_filename(&entry.name);
        if normalized.is_empty() {
            return Err(FormatError::new(format!(
                "invalid media filename in package: '{}'",
                entry.name
            )));
        }
        let preferred = normalized.to_lowercase();
        let hash = sha1_bytes(&entry.bytes);
        let final_name = unique_name(&preferred, hash, &mut cache)?;
        let destination = media_dir.join(&final_name);
        if !cache.contains(&final_name) {
            write_new_file(
                &destination,
                &entry.bytes,
                &destination.display().to_string(),
            )?;
            cache.record(&final_name, hash);
        }
        records.push(MediaRecord {
            name: final_name.clone(),
            sha1: hash,
            mtime: file_mtime(&destination)?,
            dirty: true,
        });

        let mut sources = vec![entry.name.as_str()];
        for alias in [normalized.as_str(), preferred.as_str()] {
            if !sources.contains(&alias) && !exact_names.contains(alias) {
                sources.push(alias);
            }
        }
        for source in sources {
            if source != final_name {
                renames.push(MediaRename {
                    from: source.to_owned(),
                    to: final_name.clone(),
                });
            }
        }
    }
    record_media(media_db, &records)?;
    Ok(renames)
}

fn file_mtime(path: &Path) -> Result<i64, FormatError> {
    let modified = fs::metadata(path)
        .map_err(io_error(format!(
            "cannot inspect media file {}",
            path.display()
        )))?
        .modified()
        .map_err(io_error(format!(
            "cannot inspect media file {}",
            path.display()
        )))?;
    let elapsed = modified.duration_since(UNIX_EPOCH).map_err(|_| {
        FormatError::new("media file time is before the Unix epoch")
    })?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| FormatError::new("media file time is out of range"))
}

pub fn sha1_bytes(bytes: &[u8]) -> [u8; 20] {
    let digest = Sha1::digest(bytes);
    let mut output = [0_u8; 20];
    output.copy_from_slice(digest.as_slice());
    output
}

#[cfg(test)]
pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(40);
    for byte in sha1_bytes(bytes) {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

pub fn write_new_file(
    path: &Path,
    bytes: &[u8],
    context: &str,
) -> Result<(), FormatError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error(format!("cannot create {context}")))?;
    file.write_all(bytes)
        .map_err(io_error(format!("cannot write {context}")))
}

struct MediaCache {
    directory: PathBuf,
    files: HashMap<String, Option<[u8; 20]>>,
}

impl MediaCache {
    fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            files: HashMap::new(),
        }
    }

    fn hash(&mut self, name: &str) -> Result<Option<[u8; 20]>, FormatError> {
        if let Some(cached) = self.files.get(name) {
            return Ok(*cached);
        }
        let path = self.directory.join(name);
        let hash = match fs::read(&path) {
            Ok(bytes) => Some(sha1_bytes(&bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(FormatError::with_source(
                    format!("cannot inspect media file {}", path.display()),
                    error,
                ));
            }
        };
        let _ = self.files.insert(name.to_owned(), hash);
        Ok(hash)
    }

    fn contains(&self, name: &str) -> bool {
        self.files.get(name).is_some_and(Option::is_some)
    }

    fn record(&mut self, name: &str, hash: [u8; 20]) {
        let _ = self.files.insert(name.to_owned(), Some(hash));
    }
}

fn unique_name(
    preferred: &str,
    hash: [u8; 20],
    cache: &mut MediaCache,
) -> Result<String, FormatError> {
    let existing = cache.hash(preferred)?;
    if existing.is_none_or(|current| current == hash) {
        return Ok(preferred.to_owned());
    }
    let hash_name = hash_suffix_name(preferred, &hash);
    if cache
        .hash(&hash_name)?
        .is_none_or(|current| current == hash)
    {
        return Ok(hash_name);
    }
    for suffix in 1..=u32::MAX {
        let candidate = numbered_name(&hash_name, suffix);
        if cache
            .hash(&candidate)?
            .is_none_or(|current| current == hash)
        {
            return Ok(candidate);
        }
    }
    Err(FormatError::new(format!(
        "cannot find a free media filename for '{preferred}'"
    )))
}

fn numbered_name(name: &str, suffix: u32) -> String {
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    let ext = truncate_boundary(extension, 10);
    let suffix_text = format!("_{suffix}");
    let ext_budget = ext.len().saturating_add(usize::from(!ext.is_empty()));
    let stem_length = MAX_MEDIA_FILENAME_LENGTH
        .saturating_sub(suffix_text.len())
        .saturating_sub(ext_budget);
    let base = truncate_boundary(stem, stem_length);
    if ext.is_empty() {
        format!("{base}{suffix_text}")
    } else {
        format!("{base}{suffix_text}.{ext}")
    }
}

fn hash_suffix_name(preferred: &str, hash: &[u8; 20]) -> String {
    let (stem, extension) = match preferred.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, extension),
        _ => (preferred, ""),
    };
    let ext = truncate_boundary(extension, 10);
    let stem_length = MAX_MEDIA_FILENAME_LENGTH
        .saturating_sub(41)
        .saturating_sub(ext.len())
        .saturating_sub(2);
    let base = truncate_boundary(stem, stem_length);
    let hash_text = {
        let mut output = String::with_capacity(40);
        for byte in hash {
            let _ = write!(output, "{byte:02x}");
        }
        output
    };
    if ext.is_empty() {
        format!("{base}-{hash_text}")
    } else {
        format!("{base}-{hash_text}.{ext}")
    }
}

/// Matches media renames inside HTML and sound references.
#[derive(Debug, Default)]
pub struct ReferenceMatcher {
    renames: HashMap<String, String>,
    lowercase: HashMap<String, String>,
}

impl ReferenceMatcher {
    #[expect(
        clippy::excessive_nesting,
        reason = "rename keys and their encoded forms are added together"
    )]
    pub fn new(renames: &[MediaRename]) -> Self {
        let exact: HashSet<&str> =
            renames.iter().map(|rename| rename.from.as_str()).collect();
        let mut lower_counts: HashMap<String, usize> = HashMap::new();
        for rename in renames {
            let count =
                lower_counts.entry(rename.from.to_lowercase()).or_default();
            *count = count.saturating_add(1);
        }
        let mut map = HashMap::new();
        let mut lowercase = HashMap::new();
        for rename in renames {
            let _ = map.insert(rename.from.clone(), rename.to.clone());
            let key = rename.from.to_lowercase();
            if lower_counts.get(&key).copied().unwrap_or_default() == 1 {
                let _ = lowercase.insert(key, rename.to.clone());
            }
            let encoded = percent_encode(&rename.from);
            if encoded != rename.from && !exact.contains(encoded.as_str()) {
                let _ = map.insert(encoded.clone(), rename.to.clone());
                let encoded_key = encoded.to_lowercase();
                if lower_counts.get(&encoded_key).copied().unwrap_or_default()
                    == 1
                {
                    let _ = lowercase.insert(encoded_key, rename.to.clone());
                }
            }
        }
        Self {
            renames: map,
            lowercase,
        }
    }

    pub fn rewrite<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.rewrite_with(text, false)
    }

    pub fn rewrite_css<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.rewrite_with(text, true)
    }

    #[expect(
        clippy::excessive_nesting,
        reason = "lookup table fallback is clearer inline"
    )]
    fn rewrite_with<'a>(&self, text: &'a str, allow_css: bool) -> Cow<'a, str> {
        rewrite_media_references(
            text,
            &|name| {
                self.renames.get(name).cloned().or_else(|| {
                    self.lowercase.get(&name.to_lowercase()).cloned()
                })
            },
            allow_css,
        )
    }
}

pub fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let byte = *byte;
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
        {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::Cow;
    use super::MediaEntry;
    use super::MediaRename;
    use super::ReferenceMatcher;
    use super::copy_media_with_db;
    use super::sanitize_filename;
    use super::sha1_bytes;

    #[test]
    fn normalizes_unicode_device_names_and_length() {
        assert_eq!(sanitize_filename("cafe\u{301}.mp3"), "café.mp3");
        assert_eq!(sanitize_filename("con.jpg[]><:\"?*^|"), "con_.jpg");
        assert_eq!(sanitize_filename("com1.txt"), "com1_.txt");
        assert_eq!(sanitize_filename("test."), "test._");
        assert_eq!(sanitize_filename(" lead.png"), " lead.png");
        let long = format!("{}.jpg", "x".repeat(200));
        let normalized = sanitize_filename(&long);
        assert!(normalized.len() <= 120, "{normalized}");
        assert_eq!(
            Path::new(&normalized)
                .extension()
                .and_then(|ext| ext.to_str()),
            Some("jpg")
        );
    }

    #[test]
    fn conflicting_media_uses_sha1_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        fs::create_dir_all(&media).unwrap();
        fs::write(media.join("pic.png"), b"old").unwrap();
        let db = dir.path().join("media.db2");

        let renames = copy_media_with_db(
            &media,
            &db,
            &[MediaEntry::new("pic.png".to_owned(), b"new".to_vec())],
        )
        .unwrap();

        let hashed_name = format!("pic-{}.png", super::sha1_hex(b"new"));
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0].from, "pic.png");
        assert_eq!(renames[0].to, hashed_name);
        assert!(media.join(&hashed_name).is_file());
        assert!(hashed_name.len() <= 120, "{hashed_name}");
        assert_eq!(fs::read(media.join(&hashed_name)).unwrap(), b"new");
    }

    #[test]
    fn hash_suffix_keeps_names_within_the_length_limit() {
        let long_name =
            sanitize_filename(&format!("{}.abcdefghij", "x".repeat(200)));
        let hashed = super::hash_suffix_name(&long_name, &[1; 20]);
        assert!(hashed.len() <= 120, "{hashed}");
        assert!(hashed.ends_with(".abcdefghij"));
    }

    #[test]
    fn exact_names_win_over_encoded_aliases() {
        let renames = vec![
            MediaRename {
                from: "pic%3Aa.png".to_owned(),
                to: "first.png".to_owned(),
            },
            MediaRename {
                from: "pic:a.png".to_owned(),
                to: "second.png".to_owned(),
            },
        ];
        let matcher = ReferenceMatcher::new(&renames);
        assert_eq!(
            matcher.rewrite("<img src=pic%3Aa.png>").as_ref(),
            "<img src=first.png>"
        );
        assert_eq!(
            matcher.rewrite("<img src=pic:a.png>").as_ref(),
            "<img src=second.png>"
        );
    }

    #[test]
    fn encoded_aliases_use_raw_destinations() {
        let matcher = ReferenceMatcher::new(&[MediaRename {
            from: "My Photo.png".to_owned(),
            to: "my-photo.png".to_owned(),
        }]);
        assert_eq!(
            matcher.rewrite("<img src=My%20Photo.png>").as_ref(),
            "<img src=my-photo.png>"
        );
    }

    #[test]
    fn ambiguous_case_variants_are_not_rewritten() {
        let matcher = ReferenceMatcher::new(&[
            MediaRename {
                from: "Pic.png".to_owned(),
                to: "first.png".to_owned(),
            },
            MediaRename {
                from: "pic.png".to_owned(),
                to: "second.png".to_owned(),
            },
        ]);
        assert_eq!(
            matcher.rewrite("<img src=PIC.PNG>").as_ref(),
            "<img src=PIC.PNG>"
        );
    }

    #[test]
    fn case_variant_references_resolve() {
        let matcher = ReferenceMatcher::new(&[MediaRename {
            from: "Pic.png".to_owned(),
            to: "pic-hash.png".to_owned(),
        }]);
        assert_eq!(
            matcher.rewrite("<img src=PIC.PNG>").as_ref(),
            "<img src=pic-hash.png>"
        );
    }

    #[test]
    fn hash_collision_falls_back_to_numbered_name() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        fs::create_dir_all(&media).unwrap();
        let incoming = b"new";
        let hashed = super::hash_suffix_name("pic.png", &sha1_bytes(incoming));
        fs::write(media.join("pic.png"), b"old").unwrap();
        fs::write(media.join(&hashed), b"other").unwrap();
        let renamed = copy_media_with_db(
            &media,
            &dir.path().join("media.db2"),
            &[MediaEntry::new("pic.png".to_owned(), incoming.to_vec())],
        )
        .unwrap();
        let final_name = &renamed[0].to;
        assert_ne!(final_name, &hashed);
        assert_eq!(fs::read(media.join(final_name)).unwrap(), incoming);
    }

    #[test]
    fn numbered_suffix_reserves_extension_and_budget() {
        let mut base = "x".repeat(118);
        for suffix in 1..=9 {
            let name = super::numbered_name(&base, suffix);
            assert!(name.len() <= 120, "{name}");
            assert!(name.ends_with(&format!("_{suffix}")));
            base = name;
        }
    }

    #[test]
    fn matcher_only_rewrites_media_references() {
        let renames = vec![
            MediaRename {
                from: "pic:a.png".to_owned(),
                to: "pic_a.png".to_owned(),
            },
            MediaRename {
                from: "pic:a.png.bak".to_owned(),
                to: "backup.png".to_owned(),
            },
        ];
        let matcher = ReferenceMatcher::new(&renames);
        let text =
            "pic:a.png plain <img src=\"pic:a.png.bak\"> [sound:pic%3Aa.png]";
        assert_eq!(
            matcher.rewrite(text).as_ref(),
            "pic:a.png plain <img src=\"backup.png\"> [sound:pic_a.png]"
        );
        assert!(matches!(matcher.rewrite("plain text"), Cow::Borrowed(_)));
    }

    #[test]
    fn sha1_matches_anki() {
        assert_eq!(
            super::sha1_hex(b"hello"),
            "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"
        );
        assert_eq!(sha1_bytes(b"hello").len(), 20);
    }
}
