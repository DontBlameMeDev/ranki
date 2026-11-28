//! Anki card text rendering for the mobile UI.
//!
//! The desktop renderer keeps rich runs, math and occlusion overlays. A phone
//! screen only needs plain text and local images, so this module uses a much
//! smaller subset: template fields, cloze deletions, line breaks, entities and
//! media references from the same collection directory.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    fs::File,
    io::Cursor,
    io::Read,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use egui::{ColorImage, Context, Id, TextureHandle, TextureOptions, Ui};
use image::{GenericImageView, Limits};
use ranki_model::{Note, NoteModel, cloze_markers};

#[expect(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    reason = "flags select full card text, list previews, and the answer side"
)]
fn render_sides(
    model: &NoteModel,
    note: &Note,
    ordinal: u16,
    keep_images: bool,
    include_front: bool,
) -> (String, String) {
    let Some(template) = model.template_for(ordinal) else {
        let fallback = String::from("This note has no template.");
        return (fallback.clone(), fallback);
    };

    let fields = FieldValues::new(&model.fields, &note.fields);
    let hide_occlusion = model.is_image_occlusion();
    let question_env = RenderEnv {
        front_side: "",
        hide_occlusion,
    };
    let question_markup =
        render_template(&template.question_format, fields, question_env);
    let answer_env = RenderEnv {
        front_side: if include_front || model.is_cloze() {
            question_markup.as_str()
        } else {
            ""
        },
        hide_occlusion,
    };
    let answer_markup =
        render_template(&template.answer_format, fields, answer_env);

    let question = if model.is_cloze() {
        expand_clozes(&question_markup, ordinal, true)
    } else {
        question_markup
    };
    let answer = if model.is_cloze() {
        expand_clozes(&answer_markup, ordinal, false)
    } else {
        answer_markup
    };

    (
        strip_markup(&question, keep_images),
        strip_markup(&answer, keep_images),
    )
}

/// Renders the question and answer text for one card ordinal, including images.
#[must_use]
pub fn sides(model: &NoteModel, note: &Note, ordinal: u16) -> (String, String) {
    render_sides(model, note, ordinal, true, true)
}

/// Renders the answer side without repeating the question above it.
#[must_use]
pub fn sides_answer(
    model: &NoteModel,
    note: &Note,
    ordinal: u16,
) -> (String, String) {
    render_sides(model, note, ordinal, true, false)
}

/// Renders card text for list previews, without image markup.
#[must_use]
pub fn sides_text(
    model: &NoteModel,
    note: &Note,
    ordinal: u16,
) -> (String, String) {
    render_sides(model, note, ordinal, false, true)
}

/// Draws card text and any local images referenced by it.
pub fn render_side(ui: &mut Ui, text: &str, media_dir: &Path) {
    let mut cursor = 0usize;
    while let Some(media) = next_media(text, cursor) {
        show_text(ui, text.get(cursor..media.start).unwrap_or(""));
        if media.kind == MediaKind::Image {
            show_image(ui, media_dir, &media.name);
        } else {
            show_text(ui, format!("[sound:{}]", media.name).as_str());
        }
        cursor = media.end;
    }
    show_text(ui, text.get(cursor..).unwrap_or(""));
}

/// Converts Anki markup to a single readable string.
#[must_use]
pub fn plain(text: &str) -> String {
    strip_markup(text, false)
}

/// Returns image and sound file names in the order they appear.
#[must_use]
pub fn media_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = 0usize;
    while let Some(media) = next_media(text, cursor) {
        names.push(media.name);
        cursor = media.end;
    }
    names
}

#[derive(Clone, Copy)]
struct FieldValues<'a> {
    names: &'a [String],
    values: &'a [String],
}

impl<'a> FieldValues<'a> {
    const fn new(names: &'a [String], values: &'a [String]) -> Self {
        Self { names, values }
    }

    fn get(self, key: &str) -> Option<&'a str> {
        self.names
            .iter()
            .zip(self.values.iter())
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Copy)]
struct RenderEnv<'a> {
    front_side: &'a str,
    hide_occlusion: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TemplateNode {
    Text(String),
    Field(String),
    Conditional {
        key: String,
        negated: bool,
        children: Vec<Self>,
    },
}

type ConditionalStack = Vec<(String, bool, Vec<TemplateNode>)>;

fn parse_template(source: &str) -> Vec<TemplateNode> {
    let mut roots = Vec::new();
    let mut stack: ConditionalStack = Vec::new();
    let mut cursor = 0usize;

    while let Some(relative) =
        source.get(cursor..).and_then(|rest| rest.find("{{"))
    {
        let start = cursor.saturating_add(relative);
        push_node(
            &mut stack,
            &mut roots,
            TemplateNode::Text(
                source.get(cursor..start).unwrap_or("").to_owned(),
            ),
        );
        let after_open = start.saturating_add(2);
        let after = source.get(after_open..).unwrap_or("");
        let Some(end_relative) = after.find("}}") else {
            break;
        };
        let end = after_open.saturating_add(end_relative);
        let raw = source.get(after_open..end).unwrap_or("").trim();
        cursor = end.saturating_add(2);

        if let Some(key) = raw.strip_prefix('/') {
            close_matching(key.trim(), &mut stack, &mut roots);
            continue;
        }
        if let Some(key) = raw.strip_prefix('#') {
            stack.push((key.trim().to_owned(), false, Vec::new()));
            continue;
        }
        if let Some(key) = raw.strip_prefix('^') {
            stack.push((key.trim().to_owned(), true, Vec::new()));
            continue;
        }
        push_node(&mut stack, &mut roots, TemplateNode::Field(field_key(raw)));
    }

    if let Some(rest) = source.get(cursor..)
        && !rest.is_empty()
    {
        push_node(&mut stack, &mut roots, TemplateNode::Text(rest.to_owned()));
    }
    while !stack.is_empty() {
        close_conditional(&mut stack, &mut roots);
    }
    roots
}

fn close_matching(
    key: &str,
    stack: &mut ConditionalStack,
    roots: &mut Vec<TemplateNode>,
) {
    if let Some(position) = stack
        .iter()
        .rposition(|(owner, _, _)| owner.eq_ignore_ascii_case(key))
    {
        while stack.len() > position {
            close_conditional(stack, roots);
        }
    } else {
        close_conditional(stack, roots);
    }
}

fn close_conditional(
    stack: &mut ConditionalStack,
    roots: &mut Vec<TemplateNode>,
) {
    if let Some((key, negated, children)) = stack.pop() {
        push_node(
            stack,
            roots,
            TemplateNode::Conditional {
                key,
                negated,
                children,
            },
        );
    }
}

fn push_node(
    stack: &mut ConditionalStack,
    roots: &mut Vec<TemplateNode>,
    node: TemplateNode,
) {
    if let Some((_, _, children)) = stack.last_mut() {
        children.push(node);
    } else {
        roots.push(node);
    }
}

fn field_key(raw: &str) -> String {
    match raw.split_once(':') {
        Some((filter, field))
            if filter.trim().eq_ignore_ascii_case("cloze") =>
        {
            field.trim().to_owned()
        }
        Some((filter, field)) if filter.trim().eq_ignore_ascii_case("text") => {
            format!("text:{}", field.trim())
        }
        Some((field, _)) => field.trim().to_owned(),
        None => raw.trim().to_owned(),
    }
}

fn render_template(
    source: &str,
    fields: FieldValues<'_>,
    env: RenderEnv<'_>,
) -> String {
    let nodes = parse_template(source);
    render_nodes(&nodes, fields, env)
}

fn render_nodes(
    nodes: &[TemplateNode],
    fields: FieldValues<'_>,
    env: RenderEnv<'_>,
) -> String {
    let mut output = String::new();
    for node in nodes {
        match node {
            TemplateNode::Text(text) => output.push_str(text),
            TemplateNode::Field(key) => {
                output.push_str(&field_text(key, fields, env));
            }
            TemplateNode::Conditional {
                key,
                negated,
                children,
            } => output.push_str(&render_conditional(
                key, *negated, children, fields, env,
            )),
        }
    }
    output
}
#[expect(
    clippy::too_many_arguments,
    reason = "conditional parts belong together"
)]
fn render_conditional(
    key: &str,
    negated: bool,
    children: &[TemplateNode],
    fields: FieldValues<'_>,
    env: RenderEnv<'_>,
) -> String {
    let value = fields.get(key).unwrap_or("");
    if field_present(value) == negated {
        String::new()
    } else {
        render_nodes(children, fields, env)
    }
}

fn field_text(
    key: &str,
    fields: FieldValues<'_>,
    env: RenderEnv<'_>,
) -> String {
    if key.eq_ignore_ascii_case("FrontSide") {
        env.front_side.to_owned()
    } else if env.hide_occlusion && key.eq_ignore_ascii_case("Occlusion") {
        String::new()
    } else if let Some((filter, name)) = key.split_once(':') {
        if filter.trim().eq_ignore_ascii_case("text") {
            strip_tags(fields.get(name.trim()).unwrap_or(""), false)
        } else {
            fields.get(key).unwrap_or("").to_owned()
        }
    } else {
        fields.get(key).unwrap_or("").to_owned()
    }
}

fn field_present(value: &str) -> bool {
    !(value.trim().is_empty()
        || (plain(value).is_empty() && media_names(value).is_empty()))
}

fn expand_clozes(text: &str, ordinal: u16, question: bool) -> String {
    let mut output = String::new();
    let mut cursor = 0usize;
    let number = u32::from(ordinal.saturating_add(1));
    for marker in cloze_markers(text) {
        if marker.start < cursor {
            continue;
        }
        output.push_str(text.get(cursor..marker.start).unwrap_or(""));
        if question && marker.numbers.contains(&number) {
            output.push_str("_____");
        } else {
            let answer = marker
                .body
                .split_once("::")
                .map_or(marker.body, |(value, _)| value);
            output.push_str(&expand_clozes(answer, ordinal, question));
        }
        cursor = marker.end;
    }
    output.push_str(text.get(cursor..).unwrap_or(""));
    output
}

fn strip_markup(input: &str, keep_images: bool) -> String {
    normalize_newlines(&decode_entities(&strip_tags(input, keep_images)))
}

fn strip_tags(input: &str, keep_images: bool) -> String {
    let mut output = String::new();
    let mut cursor = 0usize;
    while let Some(rest) = input.get(cursor..) {
        if let Some(end) = skip_block(rest, "<script", "</script>")
            .or_else(|| skip_block(rest, "<style", "</style>"))
            .or_else(|| skip_block(rest, "<button", "</button>"))
        {
            cursor = cursor.saturating_add(end);
            continue;
        }
        if starts_with_ci(rest, "<!--") {
            let end = rest
                .find("-->")
                .map_or(rest.len(), |at| at.saturating_add(3));
            cursor = cursor.saturating_add(end);
            continue;
        }

        let tag_opens = rest.starts_with('<')
            && rest
                .get(1..)
                .and_then(|tail| tail.chars().next())
                .is_some_and(|character| {
                    character.is_ascii_alphabetic()
                        || matches!(character, '/' | '!' | '?')
                });
        if tag_opens {
            let end =
                rest.find('>').map_or(rest.len(), |at| at.saturating_add(1));
            let tag = rest.get(..end).unwrap_or("");
            write_tag(&mut output, tag, keep_images);
            cursor = cursor.saturating_add(end);
            continue;
        }

        let Some(character) = rest.chars().next() else {
            break;
        };
        output.push(character);
        cursor = cursor.saturating_add(character.len_utf8());
    }
    output
}

enum TagAction {
    Keep,
    LineBreak,
    Skip,
}

fn write_tag(output: &mut String, tag: &str, keep_images: bool) {
    let lower = tag.to_ascii_lowercase();
    let action = if lower.starts_with("<img") && keep_images {
        TagAction::Keep
    } else if is_line_break(&lower) {
        TagAction::LineBreak
    } else {
        TagAction::Skip
    };
    match action {
        TagAction::Keep => output.push_str(tag),
        TagAction::LineBreak => output.push('\n'),
        TagAction::Skip => {}
    }
}

fn skip_block(rest: &str, open: &str, close: &str) -> Option<usize> {
    if !starts_with_ci(rest, open) {
        return None;
    }
    let at = find_ci(rest, close)?;
    Some(at.saturating_add(close.len()))
}

fn starts_with_ci(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn find_ci(text: &str, needle: &str) -> Option<usize> {
    text.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
}

fn is_line_break(tag: &str) -> bool {
    const BREAKS: [&str; 16] = [
        "<br", "<hr", "<div", "</div", "<p", "</p", "<li", "</li", "<ul",
        "</ul", "<ol", "</ol", "<tr", "</tr", "<table", "</table",
    ];
    BREAKS.iter().any(|prefix| tag.starts_with(prefix))
}

fn decode_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while let Some(rest) = input.get(cursor..) {
        let Some(at) = rest.find('&') else {
            output.push_str(rest);
            break;
        };
        output.push_str(rest.get(..at).unwrap_or(""));
        cursor = cursor.saturating_add(at).saturating_add(1);
        let tail = input.get(cursor..).unwrap_or("");
        let Some(end) = tail.find(';').filter(|end| *end <= 12) else {
            output.push('&');
            continue;
        };
        let entity = tail.get(..end).unwrap_or("");
        if let Some(decoded) = decode_entity(entity) {
            output.push_str(&decoded);
            cursor = cursor.saturating_add(end).saturating_add(1);
        } else {
            output.push('&');
        }
    }
    output
}

fn decode_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => return Some(String::from("&")),
        "lt" => return Some(String::from("<")),
        "gt" => return Some(String::from(">")),
        "quot" => return Some(String::from("\"")),
        "apos" | "#39" => return Some(String::from("'")),
        "nbsp" => return Some(String::from(" ")),
        _ => {}
    }
    let digits = entity.strip_prefix('#')?;
    let code = digits.strip_prefix(['x', 'X']).map_or_else(
        || digits.parse::<u32>().ok(),
        |hex| u32::from_str_radix(hex, 16).ok(),
    )?;
    char::from_u32(code).map(String::from)
}

#[expect(
    clippy::excessive_nesting,
    reason = "newline normalization uses one pending blank flag"
)]
fn normalize_newlines(text: &str) -> String {
    let mut output = String::new();
    let mut pending_blank = false;
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            pending_blank = !output.is_empty();
            continue;
        }
        if !output.is_empty() {
            let breaks = if pending_blank { "\n\n" } else { "\n" };
            output.push_str(breaks);
        }
        pending_blank = false;
        output.push_str(line);
    }
    output.trim().to_owned()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaKind {
    Image,
    Sound,
}

struct MediaRef {
    start: usize,
    end: usize,
    name: String,
    kind: MediaKind,
}

fn next_media(text: &str, from: usize) -> Option<MediaRef> {
    let rest = text.get(from..)?;
    let image_hit = find_image_tag(rest).map(|at| (at, MediaKind::Image));
    let sound_hit = find_ci(rest, "[sound:").map(|at| (at, MediaKind::Sound));
    let (at, kind) = earliest_hit(image_hit, sound_hit)?;
    let start = from.saturating_add(at);
    if kind == MediaKind::Sound {
        return sound_media(text, start);
    }
    let (media, end) = image_media(text, start);
    media.or_else(|| next_media(text, end))
}

const fn earliest_hit(
    image_hit: Option<(usize, MediaKind)>,
    sound_hit: Option<(usize, MediaKind)>,
) -> Option<(usize, MediaKind)> {
    match (image_hit, sound_hit) {
        (Some(image), Some(sound)) => {
            Some(if image.0 <= sound.0 { image } else { sound })
        }
        (Some(image), None) => Some(image),
        (None, Some(sound)) => Some(sound),
        (None, None) => None,
    }
}

fn image_media(text: &str, start: usize) -> (Option<MediaRef>, usize) {
    let tail = text.get(start..).unwrap_or("");
    let end = tail
        .find('>')
        .map_or(text.len(), |at| start.saturating_add(at).saturating_add(1));
    let tag = text.get(start..end).unwrap_or("");
    let name = attribute(tag, "src")
        .map(|value| decode_entities(&value))
        .filter(|name| !name.is_empty());
    let media = name.map(|media_name| MediaRef {
        start,
        end,
        name: media_name,
        kind: MediaKind::Image,
    });
    (media, end)
}

fn sound_media(text: &str, start: usize) -> Option<MediaRef> {
    let body_start = start.saturating_add("[sound:".len());
    let tail = text.get(body_start..)?;
    let end_relative = tail.find(']')?;
    let end = body_start.saturating_add(end_relative).saturating_add(1);
    let name = text
        .get(body_start..body_start.saturating_add(end_relative))
        .unwrap_or("");
    if name.is_empty() {
        return None;
    }
    Some(MediaRef {
        start,
        end,
        name: decode_entities(name),
        kind: MediaKind::Sound,
    })
}

fn find_image_tag(text: &str) -> Option<usize> {
    let mut cursor = 0usize;
    loop {
        let rest = text.get(cursor..)?;
        let at = find_ci(rest, "<img")?;
        let start = cursor.saturating_add(at);
        let boundary = text
            .get(start.saturating_add(4)..)
            .and_then(|tail| tail.chars().next());
        if boundary.is_none_or(|character| {
            character.is_ascii_whitespace() || matches!(character, '/' | '>')
        }) {
            return Some(start);
        }
        cursor = start.saturating_add(4);
    }
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = name.to_ascii_lowercase();
    let mut search = 0usize;
    loop {
        let at = lower.get(search..)?.find(&needle)?;
        let start = search.saturating_add(at);
        let before = tag.get(search..start)?;
        if before
            .chars()
            .next_back()
            .is_some_and(|previous| !previous.is_ascii_whitespace())
        {
            search = start.saturating_add(1);
            continue;
        }
        let after_name = start.saturating_add(needle.len());
        let remainder = lower.get(after_name..)?;
        let spaces =
            remainder.len().saturating_sub(remainder.trim_start().len());
        let equals = after_name.saturating_add(spaces);
        if lower.get(equals..equals.saturating_add(1)) != Some("=") {
            search = start.saturating_add(1);
            continue;
        }
        let tail = tag.get(equals.saturating_add(1)..)?;
        let trimmed = tail.trim_start();
        let quote = trimmed.chars().next();
        if quote == Some('"') || quote == Some('\'') {
            let mark = quote.unwrap_or('"');
            let body = trimmed.get(1..)?;
            let end = body.find(mark)?;
            return body.get(..end).map(str::to_owned);
        }
        let value = trimmed
            .split(|character: char| {
                character.is_ascii_whitespace() || character == '>'
            })
            .next()
            .unwrap_or("");
        if value.is_empty() {
            return None;
        }
        return Some(value.to_owned());
    }
}

fn media_path(media_dir: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.contains('\0')
        || name.contains('/')
        || name.contains('\\')
    {
        return None;
    }
    let path = Path::new(name);
    let mut components = path.components();
    let single = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none();
    if !single {
        return None;
    }
    let joined = media_dir.join(path);
    if let (Ok(root), Ok(file)) =
        (fs::canonicalize(media_dir), fs::canonicalize(&joined))
        && !file.starts_with(root)
    {
        return None;
    }
    Some(joined)
}

const MAX_MEDIA_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGE_SIDE: u32 = 2048;
const IMAGE_CACHE_CAPACITY: usize = 16;

#[derive(Clone)]
enum CachedImage {
    Ready(TextureHandle),
    Missing,
}

#[derive(Clone, Default)]
struct MediaCache {
    entries: HashMap<String, CachedImage>,
    order: VecDeque<String>,
}

impl MediaCache {
    fn insert(&mut self, key: String, image: CachedImage) {
        self.order.retain(|item| item != &key);
        self.order.push_back(key.clone());
        let _ = self.entries.insert(key, image);
        let overflow = self.order.len().saturating_sub(IMAGE_CACHE_CAPACITY);
        for oldest in self.order.drain(..overflow) {
            let _ = self.entries.remove(&oldest);
        }
    }
}

fn show_text(ui: &mut Ui, text: &str) {
    if !text.trim().is_empty() {
        ui.add(egui::Label::new(text.to_owned()).wrap());
    }
}

fn media_cache_key(media_dir: &Path, name: &str) -> String {
    let modified = media_path(media_dir, name)
        .and_then(|path| fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    modified.map_or_else(
        || format!("{}|{}", media_dir.display(), name),
        |stamp| format!("{}|{}|{}", media_dir.display(), name, stamp),
    )
}

fn show_image(ui: &mut Ui, media_dir: &Path, name: &str) {
    let key = media_cache_key(media_dir, name);
    let id = Id::new("ranki-card-images");
    let cached = ui
        .ctx()
        .memory(|memory| memory.data.get_temp::<MediaCache>(id))
        .and_then(|cache| cache.entries.get(&key).cloned());
    let image = cached.unwrap_or_else(|| {
        let loaded = load_image(ui.ctx(), media_dir, name)
            .map_or(CachedImage::Missing, CachedImage::Ready);
        ui.ctx().memory_mut(|memory| {
            let cache = memory.data.get_temp_mut_or_default::<MediaCache>(id);
            cache.insert(key, loaded.clone());
        });
        loaded
    });
    match image {
        CachedImage::Ready(texture) => {
            let image =
                egui::Image::from_texture((texture.id(), texture.size_vec2()))
                    .max_width(ui.available_width().max(96.0))
                    .max_height(280.0);
            ui.add(image);
        }
        CachedImage::Missing => {
            ui.add(egui::Label::new(format!("Image unavailable: {name}")));
        }
    }
}

fn load_image(
    ctx: &Context,
    media_dir: &Path,
    name: &str,
) -> Option<TextureHandle> {
    let path = media_path(media_dir, name)?;
    let file = File::open(path).ok()?;
    let limit = u64::try_from(MAX_MEDIA_BYTES.saturating_add(1)).ok()?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    if bytes.len() > MAX_MEDIA_BYTES {
        return None;
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(u64::try_from(MAX_MEDIA_BYTES).ok()?);
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let mut decoded = reader.decode().ok()?;
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let max_side = ctx
        .input(|input| {
            u32::try_from(input.max_texture_side).unwrap_or(MAX_IMAGE_SIDE)
        })
        .clamp(1, MAX_IMAGE_SIDE);
    if width > max_side || height > max_side {
        decoded = decoded.thumbnail(max_side, max_side);
    }
    let (width, height) = decoded.dimensions();
    let rgba = decoded.to_rgba8();
    let color = ColorImage::from_rgba_unmultiplied(
        [usize::try_from(width).ok()?, usize::try_from(height).ok()?],
        rgba.as_raw(),
    );
    Some(ctx.load_texture(name, color, TextureOptions::LINEAR))
}

#[cfg(test)]
mod tests {
    use super::{sides, sides_answer};
    use ranki_model::{Note, NoteModel, StockModelKind};

    fn basic() -> (NoteModel, Note) {
        let mut model = StockModelKind::Basic.definition();
        model.id = 1;
        let note = Note {
            id: 1,
            guid: String::from("abc"),
            model_id: 1,
            modified: 0,
            usn: 0,
            tags: Vec::new(),
            fields: vec![String::from("Question?"), String::from("Answer!")],
            sort_field: String::new(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        (model, note)
    }

    #[test]
    fn cloze_answers_keep_revealed_text_through_the_front_side() {
        let mut model = StockModelKind::Cloze.definition();
        model.id = 1;
        let template = model.templates.first_mut().expect("template");
        template.answer_format = String::from("{{FrontSide}}");
        let note = Note {
            id: 1,
            guid: String::from("abc"),
            model_id: 1,
            modified: 0,
            usn: 0,
            tags: Vec::new(),
            fields: vec![String::from("Capital: {{c1::Paris}}"), String::new()],
            sort_field: String::new(),
            checksum: 0,
            flags: 0,
            data: String::new(),
        };
        let (_, answer) = sides_answer(&model, &note, 0);
        assert!(answer.contains("Paris"));
    }

    #[test]
    fn answer_only_leaves_out_the_repeated_front() {
        let (model, note) = basic();
        let (_, full) = sides(&model, &note, 0);
        let (_, answer) = sides_answer(&model, &note, 0);
        assert!(full.contains("Question?"));
        assert!(!answer.contains("Question?"));
        assert!(answer.contains("Answer!"));
    }
}
