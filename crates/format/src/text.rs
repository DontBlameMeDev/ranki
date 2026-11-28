use std::borrow::Cow;
use std::str::from_utf8;
use std::sync::LazyLock;

use regex::Regex;

use crate::media::percent_encode;

type Lookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

static HTML_MEDIA_TAGS: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(
        clippy::expect_used,
        reason = "the media tag pattern is a fixed literal"
    )]
    Regex::new(
        r#"(?xsi)
            <\b(?:img|audio|video|object|source)\b
            (?:
                \s+[^\s>]+\s*=\s*"[^"]*"
              |
                \s+[^\s>]+\s*=\s*'[^']*'
              |
                \s+[^\s>]+\s*=\s*[^\s>]+
              |
                \s+[^\s>]+
            )*?
            \s+(src|data)\s*=\s*
            (?:
                "([^"]+?)"
              |
                '([^']+?)'
              |
                ([^\s>]+)
            )
        "#,
    )
    .expect("valid media tag pattern")
});

static CSS_URLS: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(
        clippy::expect_used,
        reason = "the CSS url pattern is a fixed literal"
    )]
    Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^\s'")]+))\s*\)"#)
        .expect("valid CSS url pattern")
});

static SOUND_TAGS: LazyLock<Regex> = LazyLock::new(|| {
    #[expect(
        clippy::expect_used,
        reason = "the sound tag pattern is a fixed literal"
    )]
    Regex::new(r"(?is)\[sound:(.+?)\]").expect("valid sound tag pattern")
});

/// Removes HTML tags and decodes the entities Anki uses in note fields.
#[cfg(test)]
pub fn strip_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('<') {
        output.push_str(
            decode_entities(rest.get(..start).unwrap_or_default()).as_ref(),
        );
        let Some(after) = rest.get(start..).and_then(|tail| tail.find('>'))
        else {
            output.push_str(
                decode_entities(rest.get(start..).unwrap_or_default()).as_ref(),
            );
            return output;
        };
        let next = start
            .checked_add(1)
            .and_then(|value| value.checked_add(after))
            .unwrap_or(rest.len());
        rest = rest.get(next..).unwrap_or_default();
    }
    output.push_str(decode_entities(rest).as_ref());
    output
}

pub fn decode_entities(input: &str) -> Cow<'_, str> {
    if !input.contains('&') {
        return Cow::Borrowed(input);
    }
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('&') {
        output.push_str(rest.get(..start).unwrap_or_default());
        let tail = rest.get(start.saturating_add(1)..).unwrap_or_default();
        let Some(end) = tail.find(';') else {
            output.push('&');
            rest = tail;
            continue;
        };
        let entity = tail.get(..end).unwrap_or_default();
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" | "#160" => Some('\u{a0}'),
            _ => entity
                .strip_prefix('#')
                .and_then(|number| number.parse::<u32>().ok())
                .and_then(char::from_u32),
        };
        if let Some(character) = decoded {
            output.push(character);
        } else {
            output.push('&');
        }
        rest = tail.get(end.saturating_add(1)..).unwrap_or_default();
    }
    output.push_str(rest);
    Cow::Owned(output)
}

/// Encodes characters that would otherwise be read as HTML markup.
pub fn encode_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
    output
}

/// Returns every media filename mentioned by a media tag or sound tag.
pub fn media_reference_names(text: &str, allow_css: bool) -> Vec<String> {
    let mut names = Vec::new();
    let mut push = |raw: &str| {
        let decoded = decode_entities(raw);
        names.push(decoded.as_ref().to_owned());
        let unescaped = percent_decode(decoded.as_ref());
        if unescaped != decoded.as_ref() {
            names.push(unescaped);
        }
    };
    for captures in HTML_MEDIA_TAGS.captures_iter(text) {
        if let Some(name) = captures
            .get(2)
            .or_else(|| captures.get(3))
            .or_else(|| captures.get(4))
        {
            push(name.as_str());
        }
    }
    for captures in CSS_URLS.captures_iter(text).filter(|_| allow_css) {
        if let Some(name) = captures
            .get(1)
            .or_else(|| captures.get(2))
            .or_else(|| captures.get(3))
        {
            push(name.as_str());
        }
    }
    for captures in SOUND_TAGS.captures_iter(text) {
        if let Some(name) = captures.get(1) {
            push(name.as_str());
        }
    }
    names
}

/// Rewrites only filenames inside media tags, leaving ordinary text alone.
#[expect(
    clippy::excessive_nesting,
    reason = "each match type follows the same fixed rewrite pattern"
)]
pub fn rewrite_media_references<'a>(
    text: &'a str,
    lookup: &Lookup<'_>,
    allow_css: bool,
) -> Cow<'a, str> {
    let html =
        HTML_MEDIA_TAGS.replace_all(text, |captures: &regex::Captures| {
            let whole = captures.get(0).map_or("", |value| value.as_str());
            for group in 2..=4 {
                let Some(name) = captures.get(group) else {
                    continue;
                };
                let old = name.as_str();
                if let Some(replacement) = reference_replacement(old, lookup) {
                    return whole.replace(old, &replacement);
                }
            }
            whole.to_owned()
        });
    let css = if allow_css {
        CSS_URLS.replace_all(&html, |captures: &regex::Captures| {
            replace_capture(captures, lookup)
        })
    } else {
        html
    };
    let sound = SOUND_TAGS.replace_all(&css, |captures: &regex::Captures| {
        replace_capture(captures, lookup)
    });
    if sound == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(sound.into_owned())
    }
}

fn replace_capture(captures: &regex::Captures, lookup: &Lookup) -> String {
    let whole = captures.get(0).map_or("", |value| value.as_str());
    for group in [1, 2, 3] {
        let Some(name) = captures.get(group) else {
            continue;
        };
        if let Some(value) = reference_replacement(name.as_str(), lookup) {
            return whole.replace(name.as_str(), &value);
        }
    }
    whole.to_owned()
}

fn reference_replacement(old: &str, lookup: &Lookup) -> Option<String> {
    let decoded = decode_entities(old);
    let replacement = lookup(decoded.as_ref())
        .or_else(|| lookup(old))
        .or_else(|| lookup(&percent_decode(decoded.as_ref())))?;
    if decoded.as_ref() != old {
        Some(encode_entities(&replacement))
    } else if old.contains('%') || old.chars().any(char::is_whitespace) {
        Some(percent_encode(&replacement))
    } else {
        Some(replacement)
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while let Some(byte) = bytes.get(cursor).copied() {
        let next = cursor.saturating_add(1);
        let end = next.saturating_add(2);
        if byte == b'%'
            && let Some(hex) = bytes.get(next..end)
            && let Ok(octet) =
                u8::from_str_radix(from_utf8(hex).unwrap_or_default(), 16)
        {
            output.push(octet);
            cursor = end;
        } else {
            output.push(byte);
            cursor = next;
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::media_reference_names;
    use super::rewrite_media_references;
    use super::strip_html;

    #[test]
    fn strips_tags_and_decodes_entities() {
        assert_eq!(strip_html("<b>A&#233;</b> &amp; tea"), "Aé & tea");
    }

    #[test]
    fn finds_only_media_references() {
        let fields = "<img src='one.png'> [sound:two.mp3] plain one.png";
        assert_eq!(
            media_reference_names(fields, false),
            ["one.png", "two.mp3"]
        );
    }

    #[test]
    fn accepts_whitespace_around_media_attributes() {
        assert_eq!(
            media_reference_names("<img src = \"pic.png\" alt='x'>", false),
            ["pic.png"]
        );
    }

    #[test]
    fn ignores_similarly_named_attributes() {
        assert_eq!(
            media_reference_names(
                "<img data-src=placeholder.png src=real.png>",
                false,
            ),
            ["real.png"]
        );
    }

    #[test]
    fn trims_unquoted_css_url_whitespace() {
        assert_eq!(media_reference_names("url(logo.png )", true), ["logo.png"]);
        assert_eq!(
            media_reference_names("url(\"my file.png\")", true),
            ["my file.png"]
        );
    }

    #[test]
    fn sound_markers_are_case_insensitive() {
        assert_eq!(
            media_reference_names("[SOUND:clip.mp3]", false),
            ["clip.mp3"]
        );
    }

    #[test]
    fn css_urls_are_not_matched_in_note_text() {
        assert_eq!(
            media_reference_names("Use url(logo.png) in CSS", false),
            Vec::<String>::new()
        );
        assert_eq!(media_reference_names("url(logo.png)", true), ["logo.png"]);
    }

    #[test]
    fn rewrites_only_media_references() {
        let lookup =
            |name: &str| (name == "one.png").then(|| "new.png".to_owned());
        let text =
            "one.png <img src=one.png> <div>one.png</div> [sound:one.png]";
        assert_eq!(
            rewrite_media_references(text, &lookup, false),
            "one.png <img src=new.png> <div>one.png</div> [sound:new.png]"
        );
    }
}
