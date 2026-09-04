//! Minimal HTML→text conversion for the builtin web tools. No DOM crate:
//! models and tool cards consume plain text, and best-effort stripping is
//! enough for that.

/// Decode the handful of entities that appear in real pages plus numeric
/// references (`&#39;`, `&#x27;`).
pub fn decode_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';') else {
            break;
        };
        let entity = &rest[1..end];
        if let Some(decoded) = decode_entity(entity) {
            out.push_str(&decoded);
            rest = &rest[end + 1..];
        } else {
            // not an entity we know; keep it verbatim
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    let named = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" | "rsquo" => "'",
        "nbsp" => " ",
        "hellip" => "…",
        "mdash" => "—",
        "ndash" => "–",
        "middot" => "·",
        "laquo" => "«",
        "raquo" => "»",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        _ => return decode_numeric(entity),
    };
    Some(named.to_string())
}

fn decode_numeric(entity: &str) -> Option<String> {
    let code = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else if let Some(dec) = entity.strip_prefix('#') {
        dec.parse::<u32>().ok()?
    } else {
        return None;
    };
    char::from_u32(code).map(|c| c.to_string())
}

/// Convert an HTML document to readable plain text: drop script/style
/// contents, turn block-level tags into line breaks, strip the remaining
/// tags, and decode entities.
pub fn html_to_text(html: &str) -> String {
    let without_scripts = strip_tag_blocks(html, &["script", "style", "noscript"]);
    let without_comments = strip_comments(&without_scripts);

    let mut text = String::with_capacity(without_comments.len());
    let mut rest = without_comments.as_str();
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            break; // unterminated tag; drop the rest
        };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        let tag_body = tag.strip_prefix('/').unwrap_or(tag);
        let name: String = tag_body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if BLOCK_TAGS.contains(&name.as_str()) {
            text.push('\n');
        }
    }
    text.push_str(rest);

    let decoded = decode_entities(&text);
    collapse_whitespace(&decoded)
}

/// Remove `<tag …>…</tag>` spans including their content.
fn strip_tag_blocks(html: &str, tags: &[&str]) -> String {
    let mut out = html.to_string();
    for tag in tags {
        loop {
            let Some((start, content_start)) = find_open_tag(&out, tag) else {
                break;
            };
            let Some(end) = out[content_start..].find(&format!("</{tag}")) else {
                // unterminated block: drop everything from the open tag
                out.truncate(start);
                break;
            };
            let end = content_start + end + tag.len() + 3;
            out.replace_range(start..end, "");
        }
    }
    out
}

fn strip_comments(html: &str) -> String {
    let mut out = html.to_string();
    while let Some(start) = out.find("<!--") {
        match out[start..].find("-->") {
            Some(end) => out.replace_range(start..start + end + 3, ""),
            None => {
                out.truncate(start);
                break;
            }
        }
    }
    out
}

/// Byte offset of `<tag` (word boundary) plus the end of its opening tag.
fn find_open_tag(html: &str, tag: &str) -> Option<(usize, usize)> {
    let hay = html.to_ascii_lowercase();
    let needle = format!("<{tag}");
    let mut search_from = 0;
    while let Some(rel) = hay[search_from..].find(&needle) {
        let start = search_from + rel;
        let after = &hay[start + needle.len()..];
        // require a tag boundary: whitespace or '>' (so `<style` doesn't match
        // `<stylesheet-x`)
        let boundary = after.starts_with(' ') || after.starts_with('>') || after.starts_with('\n');
        if boundary {
            let gt = hay[start..].find('>')? + start;
            return Some((start, gt + 1));
        }
        search_from = start + needle.len();
    }
    None
}

const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "tr",
    "table",
    "section",
    "article",
    "header",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "blockquote",
    "pre",
    "hr",
    "nav",
    "main",
    "aside",
];

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_blank = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !in_blank {
                out.push('\n');
                in_blank = true;
            }
        } else {
            out.push_str(trimmed);
            out.push('\n');
            in_blank = false;
        }
    }
    out.trim().to_string()
}

/// Extract the value of `attribute=` from inside a tag string like
/// `a class="result__a" href="…"`.
pub fn tag_attribute(tag: &str, attribute: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{attribute}=");
    let pos = lower.find(&needle)?;
    let rest = &tag[pos + needle.len()..];
    let mut chars = rest.chars();
    match chars.next()? {
        '"' => rest[1..].split('"').next().map(|s| s.to_string()),
        '\'' => rest[1..].split('\'').next().map(|s| s.to_string()),
        _ => rest.split_whitespace().next().map(|s| s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_entities() {
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &#39;q&#39;"),
            "a & b <c> 'q'"
        );
        assert_eq!(decode_entities("caf&#233; &#x27;"), "café '");
        assert_eq!(decode_entities("&unknown; stays"), "&unknown; stays");
        assert_eq!(decode_entities("no entities"), "no entities");
    }

    #[test]
    fn converts_simple_document() {
        let html = r#"<html><head><title>T</title><script>var x = 1;</script></head>
            <body><h1>Hello</h1><p>First &amp; foremost</p><!-- comment -->
            <div>Nested <b>bold</b> text</div></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("First & foremost"));
        assert!(text.contains("Nested bold text"));
        assert!(!text.contains("var x"));
        assert!(!text.contains("comment"));
        assert!(!text.contains("<"));
    }

    #[test]
    fn extracts_tag_attributes() {
        let tag = r#"a class="result__a" href="https://example.com/?a=1&b=2""#;
        assert_eq!(
            tag_attribute(tag, "href").as_deref(),
            Some("https://example.com/?a=1&b=2")
        );
        assert_eq!(tag_attribute(tag, "class").as_deref(), Some("result__a"));
        assert!(tag_attribute(tag, "missing").is_none());
    }

    #[test]
    fn script_block_boundaries_respected() {
        // `<stylesheet>` must not trigger the `style` stripper
        let html = "<p>keep</p><stylesheet-x>not a style</stylesheet-x><p>also keep</p>";
        assert_eq!(html_to_text(html), "keep\nnot a style\nalso keep");
    }
}
