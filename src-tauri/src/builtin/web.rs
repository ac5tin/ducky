//! Built-in web tools: fetch a URL as text and keyless DuckDuckGo search.

use std::time::Duration;

use serde_json::Value;

use super::html::{decode_entities, html_to_text, tag_attribute};

pub const FETCH: &str = "ducky__web_fetch";
pub const SEARCH: &str = "ducky__web_search";

const DEFAULT_MAX_CHARS: usize = 40_000;
const HARD_MAX_CHARS: usize = 200_000;
const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_MAX_RESULTS: usize = 8;
const HARD_MAX_RESULTS: usize = 20;
/// A plain browser UA plus browser-like Accept headers: the DuckDuckGo HTML
/// endpoint rejects requests that identify as automation (403).
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like \
                          Gecko) Chrome/124.0 Safari/537.36";

pub async fn execute(tool: &str, args: &Value) -> Result<String, String> {
    match tool {
        FETCH => fetch(args).await,
        SEARCH => search(args).await,
        _ => Err(format!("Unknown builtin web tool: {tool}")),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required string argument \"{key}\""))
}

fn client() -> reqwest::Client {
    crate::providers::http_client()
}

async fn fetch(args: &Value) -> Result<String, String> {
    let url = arg_str(args, "url")?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Only http:// and https:// URLs are supported.".into());
    }
    let max_chars = args
        .get("max_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_CHARS as u64)
        .clamp(1000, HARD_MAX_CHARS as u64) as usize;

    let response = client()
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,*/*",
        )
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("Request to {url} failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("{url} returned HTTP {status}"));
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let textual = content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript");
    if !textual {
        return Err(format!(
            "{url} returned non-text content ({content_type}); the builtin fetch tool only \
             handles text."
        ));
    }
    let is_html = content_type.contains("html");

    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("Reading {url} failed: {e}"))?
    {
        if body.len() as u64 + chunk.len() as u64 > MAX_BODY_BYTES {
            body.truncate(MAX_BODY_BYTES as usize);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    let raw = String::from_utf8_lossy(&body).into_owned();
    let text = if is_html { html_to_text(&raw) } else { raw };

    if text.chars().count() > max_chars {
        let cut = text
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        Ok(format!(
            "{}\n\n… truncated at {max_chars} characters (pass a higher max_chars up to \
             {HARD_MAX_CHARS} if you need more)",
            &text[..cut]
        ))
    } else {
        Ok(text)
    }
}

async fn search(args: &Value) -> Result<String, String> {
    let query = arg_str(args, "query")?;
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_RESULTS as u64)
        .clamp(1, HARD_MAX_RESULTS as u64) as usize;

    let query_string = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", query)
        .finish();
    let url = format!("https://html.duckduckgo.com/html/?{query_string}");
    let response = client()
        .get(&url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,*/*",
        )
        .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Search request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("DuckDuckGo returned HTTP {}", response.status()));
    }
    let body = response
        .text()
        .await
        .map_err(|e| format!("Reading search results failed: {e}"))?;
    let results = parse_ddg_results(&body, max_results);

    if results.is_empty() {
        return Ok(format!(
            "No results found for \"{query}\". If the query is valid, the search backend may \
             be unavailable or blocking automated requests — say so rather than inventing \
             results."
        ));
    }

    let mut out = format!("Web results for \"{query}\":\n");
    for (i, (title, link, snippet)) in results.iter().enumerate() {
        out.push_str(&format!("\n{}. {title}\n   {link}\n   {snippet}\n", i + 1));
    }
    Ok(out.trim().to_string())
}

/// Extract `(title, url, snippet)` triples from a DuckDuckGo HTML results
/// page. Best-effort: returns an empty vec when the markup isn't recognized.
fn parse_ddg_results(html: &str, max_results: usize) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    for (title, link) in extract_anchors(html, "result__a") {
        if results.len() >= max_results {
            break;
        }
        let link = unwrap_ddg_redirect(&link);
        let snippet = extract_anchors(html_after(html, &title), "result__snippet")
            .first()
            .map(|(text, _)| text.clone())
            .unwrap_or_default();
        results.push((title, link, snippet));
    }
    results
}

/// All `<a ...>text</a>` elements whose tag carries `marker` as a class,
/// returned as `(inner_text, href)`.
fn extract_anchors(html: &str, marker: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let needle = format!("<a ");
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(&needle) {
        let tag_start = search_from + rel;
        let Some(tag_end_rel) = lower[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_end_rel;
        let tag = &html[tag_start..tag_end];
        search_from = tag_end;

        let class = tag_attribute(tag, "class").unwrap_or_default();
        if !class.split_whitespace().any(|c| c == marker) {
            continue;
        }
        let Some(close_rel) = lower[tag_end..].find("</a>") else {
            break;
        };
        let inner = decode_entities(&html[tag_end + 1..tag_end + close_rel]);
        let href = tag_attribute(tag, "href")
            .map(|h| decode_entities(&h))
            .unwrap_or_default();
        out.push((inner.trim().to_string(), href));
    }
    out
}

/// DuckDuckGo links come back as `//duckduckgo.com/l/?uddg=<encoded real url>`.
fn unwrap_ddg_redirect(href: &str) -> String {
    let Some(query_start) = href.find('?') else {
        return href.to_string();
    };
    for (key, value) in url::form_urlencoded::parse(href[query_start + 1..].as_bytes()) {
        if key == "uddg" {
            return value.into_owned();
        }
    }
    href.to_string()
}

/// Find the byte offset just past the first occurrence of `needle` so snippet
/// lookup starts near its result title rather than always from the top.
fn html_after<'a>(html: &'a str, needle: &str) -> &'a str {
    match html.find(needle) {
        Some(pos) => &html[pos + needle.len()..],
        None => html,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r##"<html><body>
        <div class="result results_links">
            <h2 class="result__title">
                <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Frust-book&amp;rut=abc">The Rust Book &amp; Guide</a>
            </h2>
            <a class="result__snippet" href="#">Learn Rust &#39;properly&#39; — start here</a>
        </div>
        <div class="result">
            <h2><a class="result__a" href="https://direct.org/page">Direct Link</a></h2>
            <a class="result__snippet">Second snippet</a>
        </div>
        </body></html>"##;

    #[test]
    fn parses_ddg_results() {
        let results = parse_ddg_results(FIXTURE, 8);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "The Rust Book & Guide");
        assert_eq!(results[0].1, "https://example.com/rust-book");
        assert_eq!(results[0].2, "Learn Rust 'properly' — start here");
        assert_eq!(results[1].1, "https://direct.org/page");
        assert_eq!(results[1].2, "Second snippet");
    }

    #[test]
    fn respects_max_results() {
        assert_eq!(parse_ddg_results(FIXTURE, 1).len(), 1);
    }

    #[test]
    fn empty_page_yields_no_results() {
        assert!(parse_ddg_results("<html>nothing here</html>", 8).is_empty());
    }

    #[test]
    fn unwraps_redirects() {
        assert_eq!(
            unwrap_ddg_redirect("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.b%2Fc%3Fd%3D1&rut=x"),
            "https://a.b/c?d=1"
        );
        assert_eq!(
            unwrap_ddg_redirect("https://plain.example/x"),
            "https://plain.example/x"
        );
    }

    /// Live smoke test (network): `cargo test -- --ignored`
    #[tokio::test]
    #[ignore = "hits the live DuckDuckGo endpoint"]
    async fn live_search_and_fetch() {
        // the app installs this in run(); test binaries must do it themselves
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("install rustls ring provider");

        let out = execute(
            SEARCH,
            &serde_json::json!({"query": "rust programming language", "max_results": 3}),
        )
        .await
        .unwrap();
        assert!(out.contains("https://"), "search returned no URLs: {out}");

        let page = execute(FETCH, &serde_json::json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(
            page.to_lowercase().contains("example domain"),
            "fetch got: {page}"
        );
    }
}
