//! Marketplace registry parsing, fetching and records (design §5).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::diagnostics::{DiagLevel, Diagnostic};
use super::path::resolve_within;
use crate::snapshot::GIT_ENV_TO_CLEAR;

/// Registry probe order; the first file that exists decides the result.
const PROBES: [&str; 3] = [
    ".claude-plugin/marketplace.json",
    "marketplace.json",
    ".ducky/marketplace.json",
];

/// A normalised marketplace registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Registry {
    pub name: String,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub entries: Vec<Entry>,
    pub registry_path: PathBuf,
}

/// One plugin offered by a registry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub icon: Option<String>,
    pub keywords: Vec<String>,
    pub source: PluginSource,
    /// False when the entry cannot be used as written; `reason` says why.
    pub available: bool,
    pub reason: Option<String>,
}

/// Where a plugin comes from, normalised across registry dialects.
///
/// Serialised with a `kind` discriminator for the persisted record shape
/// (design §3). `Unsupported` carries its own `kind`, which serde forbids
/// beside an internal tag, so that field is written as `sourceKind`;
/// unsupported sources are never persisted (they are always unavailable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PluginSource {
    Github {
        repo: String,
        path: Option<String>,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        sha: Option<String>,
    },
    Git {
        url: String,
        path: Option<String>,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        sha: Option<String>,
    },
    GitSubdir {
        url: String,
        path: String,
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        sha: Option<String>,
    },
    Url {
        url: String,
    },
    Path {
        path: String,
    },
    Unsupported {
        #[serde(rename = "sourceKind")]
        kind: String,
        detail: String,
    },
}

/// Load the first marketplace registry that exists under `root`.
///
/// Probe order is `.claude-plugin/marketplace.json`, `marketplace.json`,
/// `.ducky/marketplace.json`; the first file that exists decides the result.
/// Nothing is fetched. An entry that fails validation is kept with
/// `available: false` and a reason so the store can show it.
pub fn parse_registry(root: &Path) -> Result<Registry, Vec<Diagnostic>> {
    for rel in PROBES {
        let probe = root.join(rel);
        if !probe.is_file() {
            continue;
        }
        let Some(path) = resolve_within(root, &probe) else {
            return Err(vec![error(
                rel,
                "marketplace registry resolves outside the marketplace root",
            )]);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                return Err(vec![error(
                    rel,
                    format!("cannot read marketplace registry: {err}"),
                )])
            }
        };
        return parse_registry_text(&text, rel, path);
    }
    Err(vec![error(
        PROBES[0],
        "no marketplace registry found (.claude-plugin/marketplace.json, marketplace.json, .ducky/marketplace.json)",
    )])
}

/// Parse registry text that is not (necessarily) on disk yet.
///
/// `target` names the source in diagnostics: a probe-relative path for a
/// directory, a file path or URL otherwise.
fn parse_registry_text(
    text: &str,
    target: &str,
    registry_path: PathBuf,
) -> Result<Registry, Vec<Diagnostic>> {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => return Err(vec![error(target, format!("invalid JSON: {err}"))]),
    };
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            return Err(vec![error(
                target,
                "marketplace registry must be a JSON object",
            )])
        }
    };
    parse_object(object, target, registry_path)
}

/// Validate the top level and normalise every entry.
fn parse_object(
    object: &Map<String, Value>,
    target: &str,
    registry_path: PathBuf,
) -> Result<Registry, Vec<Diagnostic>> {
    let name = match object.get("name") {
        Some(Value::String(name)) if !name.is_empty() => name.clone(),
        Some(Value::String(_)) => {
            return Err(vec![error(target, "field `name` must not be empty")])
        }
        Some(_) => return Err(vec![error(target, "field `name` must be a string")]),
        None => return Err(vec![error(target, "missing required field: name")]),
    };

    let plugins = match object.get("plugins") {
        None => &[][..],
        Some(Value::Array(items)) => items.as_slice(),
        Some(_) => return Err(vec![error(target, "field `plugins` must be an array")]),
    };

    Ok(Registry {
        name,
        description: string_field(object, "description"),
        owner: string_or_name(object.get("owner")),
        entries: plugins.iter().map(parse_entry).collect(),
        registry_path,
    })
}

/// Normalise one `plugins[]` item; a broken entry stays visible.
fn parse_entry(value: &Value) -> Entry {
    let Some(object) = value.as_object() else {
        return Entry {
            name: String::new(),
            display_name: None,
            description: None,
            version: None,
            category: None,
            tags: Vec::new(),
            author: None,
            homepage: None,
            icon: None,
            keywords: Vec::new(),
            source: PluginSource::Unsupported {
                kind: "unknown".to_string(),
                detail: "entry must be a JSON object".to_string(),
            },
            available: false,
            reason: Some("entry must be a JSON object".to_string()),
        };
    };

    let name = string_field(object, "name").unwrap_or_default();
    let (source, mut reason) = match object.get("source") {
        Some(raw) => match parse_source(raw, false) {
            Ok(source) => (source, None),
            Err(detail) => (
                PluginSource::Unsupported {
                    kind: source_kind(raw),
                    detail: detail.clone(),
                },
                Some(detail),
            ),
        },
        None => {
            let detail = "entry has no `source`".to_string();
            (
                PluginSource::Unsupported {
                    kind: "unknown".to_string(),
                    detail: detail.clone(),
                },
                Some(detail),
            )
        }
    };
    if reason.is_none() {
        match &source {
            PluginSource::Unsupported { detail, .. } => reason = Some(detail.clone()),
            _ if name.is_empty() => reason = Some("entry has no `name`".to_string()),
            _ => {}
        }
    }

    Entry {
        name,
        display_name: string_field(object, "displayName")
            .or_else(|| string_field(object, "display_name")),
        description: string_field(object, "description"),
        version: string_field(object, "version"),
        category: string_field(object, "category"),
        tags: string_array_field(object, "tags"),
        author: string_or_name(object.get("author")),
        homepage: string_field(object, "homepage"),
        icon: string_field(object, "icon"),
        keywords: string_array_field(object, "keywords"),
        source,
        available: reason.is_none(),
        reason,
    }
}

/// Parse one `source` value.
///
/// `is_marketplace_source` additionally accepts the `owner/repo` and bare git
/// URL shorthands (design §5); plugin entries accept only the documented
/// source forms.
pub(super) fn parse_source(
    value: &Value,
    is_marketplace_source: bool,
) -> Result<PluginSource, String> {
    match value {
        Value::String(text) => parse_source_string(text, is_marketplace_source),
        Value::Object(map) => parse_source_object(map),
        _ => Err("source must be a string or an object".to_string()),
    }
}

fn parse_source_string(text: &str, is_marketplace_source: bool) -> Result<PluginSource, String> {
    if text.starts_with("./") {
        return Ok(PluginSource::Path {
            path: text.to_string(),
        });
    }
    if is_marketplace_source {
        if is_bare_git_url(text) {
            return Ok(PluginSource::Git {
                url: text.to_string(),
                path: None,
                git_ref: None,
                sha: None,
            });
        }
        if is_owner_repo(text) {
            return Ok(PluginSource::Github {
                repo: text.to_string(),
                path: None,
                git_ref: None,
                sha: None,
            });
        }
        return Err(format!(
            "invalid marketplace source `{text}`: expected ./path, owner/repo, or a git URL"
        ));
    }
    Err(format!(
        "invalid plugin source `{text}`: expected a ./relative path"
    ))
}

fn parse_source_object(map: &Map<String, Value>) -> Result<PluginSource, String> {
    let kind = match map.get("source") {
        Some(Value::String(kind)) => kind.clone(),
        Some(_) => return Err("field `source` must be a string".to_string()),
        None => return Err("source object is missing the `source` discriminator".to_string()),
    };

    match kind.as_str() {
        "github" => Ok(PluginSource::Github {
            repo: required_string(map, &kind, "repo")?,
            path: optional_string(map, "path")?,
            git_ref: optional_string(map, "ref")?,
            sha: optional_string(map, "sha")?,
        }),
        "git" => Ok(PluginSource::Git {
            url: required_string(map, &kind, "url")?,
            path: optional_string(map, "path")?,
            git_ref: optional_string(map, "ref")?,
            sha: optional_string(map, "sha")?,
        }),
        "git-subdir" => Ok(PluginSource::GitSubdir {
            url: required_string(map, &kind, "url")?,
            path: required_string(map, &kind, "path")?,
            git_ref: optional_string(map, "ref")?,
            sha: optional_string(map, "sha")?,
        }),
        "directory" | "file" => Ok(PluginSource::Path {
            path: required_string(map, &kind, "path")?,
        }),
        "url" => Ok(PluginSource::Url {
            url: required_string(map, &kind, "url")?,
        }),
        other => Ok(PluginSource::Unsupported {
            kind: other.to_string(),
            detail: format!("unsupported source kind: {other}"),
        }),
    }
}

/// The `source` discriminator of a raw object, for diagnostics.
fn source_kind(value: &Value) -> String {
    value
        .as_object()
        .and_then(|map| map.get("source"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// A required non-empty string key of a source object.
fn required_string(map: &Map<String, Value>, kind: &str, key: &str) -> Result<String, String> {
    match map.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(Value::String(_)) => Err(format!("source kind `{kind}` has an empty `{key}`")),
        Some(_) => Err(format!(
            "source kind `{kind}` field `{key}` must be a string"
        )),
        None => Err(format!("source kind `{kind}` requires `{key}`")),
    }
}

/// An optional string key of a source object; wrong types fail the entry.
fn optional_string(map: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("field `{key}` must be a string")),
    }
}

/// A lenient string field: wrong types are ignored.
fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A lenient string array field: non-strings are ignored.
fn string_array_field(object: &Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A display string that may be written as an object with a `name` key
/// (Claude Code `owner` and `author` shapes).
fn string_or_name(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => map.get("name").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// A git URL written as a bare string shorthand.
fn is_bare_git_url(text: &str) -> bool {
    ["http://", "https://", "git://", "ssh://", "git@"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// An `owner/repo` shorthand: exactly one slash, no scheme, no whitespace.
fn is_owner_repo(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(char::is_whitespace)
        && !text.contains("://")
        && text.matches('/').count() == 1
        && text.split('/').all(|part| !part.is_empty())
}

fn error(target: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        level: DiagLevel::Error,
        target: target.to_string(),
        message: message.into(),
    }
}

// ---------------------------------------------------------------------------
// Records (design §3)

/// One known marketplace, as persisted in `marketplaces.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceRecord {
    pub id: String,
    pub name: String,
    pub source: PluginSource,
    #[serde(default)]
    pub registry_path: String,
    /// Off by default: a record that does not say otherwise is never refreshed
    /// in the background.
    #[serde(default)]
    pub auto_refresh: bool,
    #[serde(default)]
    pub last_refreshed_at: Option<String>,
    #[serde(default)]
    pub resolved_sha: Option<String>,
    #[serde(default)]
    pub bundled: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// Unknown fields round-trip (design §3).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl MarketplaceRecord {
    /// A local source is never auto-refreshed: "refresh" for it means re-read.
    pub fn normalise_auto_refresh(&mut self) {
        if matches!(self.source, PluginSource::Path { .. }) {
            self.auto_refresh = false;
        }
    }
}

/// `marketplaces.json`: every known marketplace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceStore {
    #[serde(default = "store_version")]
    pub version: u32,
    #[serde(rename = "marketplaces", default)]
    pub records: Vec<MarketplaceRecord>,
    /// Unknown fields round-trip (design §3).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn store_version() -> u32 {
    1
}

impl Default for MarketplaceStore {
    fn default() -> Self {
        Self {
            version: store_version(),
            records: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl MarketplaceStore {
    /// Load `dir/marketplaces.json`; a missing or unreadable file is empty.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("marketplaces.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write `dir/marketplaces.json` through a temp file plus rename.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let file = dir.join("marketplaces.json");
        let tmp = temp_sibling(&file);
        let body = serde_json::to_vec_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if let Err(err) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &file)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        Ok(())
    }
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A `.tmp` sibling with a per-process unique name: two concurrent saves must
/// not write the same temp file.
fn temp_sibling(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(name)
}

/// Write `bytes` to `path` through a unique temp file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = temp_sibling(path);
    if let Err(err) = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {err}", path.display()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fetching (design §5)

/// One HTTP response, status included; the refresh decides what it means.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// A conditional HTTP fetch, injected so refresh is testable without a network.
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn get_conditional(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<HttpResponse, String>;
}

/// Production client: the `catalog.rs:108` builder pattern.
#[derive(Debug, Clone)]
pub struct ReqwestClient {
    client: reqwest::Client,
}

impl ReqwestClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for ReqwestClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpClient for ReqwestClient {
    async fn get_conditional(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<HttpResponse, String> {
        let mut request = self.client.get(url);
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(last_modified) = last_modified {
            request = request.header("If-Modified-Since", last_modified);
        }
        let response = request
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let etag = header("etag");
        let last_modified = header("last-modified");
        let body = if status == 304 {
            None
        } else {
            Some(
                response
                    .text()
                    .await
                    .map_err(|err| format!("cannot read response body: {err}"))?,
            )
        };
        Ok(HttpResponse {
            status,
            body,
            etag,
            last_modified,
        })
    }
}

/// Split a trailing `#ref` off a git remote or `owner/repo` shorthand.
pub fn parse_git_remote(url: &str) -> (String, Option<String>) {
    match url.split_once('#') {
        Some((base, "")) => (base.to_string(), None),
        Some((base, git_ref)) => (base.to_string(), Some(git_ref.to_string())),
        None => (url.to_string(), None),
    }
}

/// What one refresh did to a marketplace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Unchanged,
    Updated,
}

/// The result of one refresh.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshOutcome {
    pub change: Change,
    pub resolved_sha: Option<String>,
    pub entry_count: usize,
}

/// Refresh one marketplace in place.
///
/// On failure the record keeps its `last_refreshed_at` and cached registry;
/// only `error` is set, and the failure is returned.
pub async fn refresh(
    record: &mut MarketplaceRecord,
    dir: &Path,
    http: &dyn HttpClient,
) -> Result<RefreshOutcome, String> {
    record.normalise_auto_refresh();
    match refresh_inner(record, dir, http).await {
        Ok(outcome) => {
            record.error = None;
            Ok(outcome)
        }
        Err(err) => {
            record.error = Some(err.clone());
            Err(err)
        }
    }
}

async fn refresh_inner(
    record: &mut MarketplaceRecord,
    dir: &Path,
    http: &dyn HttpClient,
) -> Result<RefreshOutcome, String> {
    match record.source.clone() {
        PluginSource::Path { path } => refresh_path(record, Path::new(&path)),
        PluginSource::Url { url } => refresh_url(record, dir, http, &url).await,
        source @ (PluginSource::Github { .. }
        | PluginSource::Git { .. }
        | PluginSource::GitSubdir { .. }) => refresh_git(record, dir, &source),
        PluginSource::Unsupported { kind, .. } => {
            Err(format!("cannot refresh unsupported source kind: {kind}"))
        }
    }
}

/// Re-read a local registry in place; nothing is copied or fetched.
fn refresh_path(record: &mut MarketplaceRecord, source: &Path) -> Result<RefreshOutcome, String> {
    let registry = if source.is_file() {
        parse_registry_file(source)?
    } else {
        parse_registry(source).map_err(join_diagnostics)?
    };
    record.registry_path = display_registry_path(&registry, source);
    record.resolved_sha = None;
    record.last_refreshed_at = Some(now());
    Ok(RefreshOutcome {
        change: Change::Updated,
        resolved_sha: None,
        entry_count: registry.entries.len(),
    })
}

/// HTTP validators for the cached URL snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryMeta {
    etag: Option<String>,
    last_modified: Option<String>,
}

async fn refresh_url(
    record: &mut MarketplaceRecord,
    dir: &Path,
    http: &dyn HttpClient,
    url: &str,
) -> Result<RefreshOutcome, String> {
    let snapshot_dir = dir.join(&record.id);
    let snapshot = snapshot_dir.join("registry.json");
    // Validators only help while the cached body is readable; a missing or
    // corrupt snapshot refetches in full instead of answering 304 forever.
    let cached = parse_registry_file(&snapshot).ok();
    let meta = if cached.is_some() {
        load_meta(&snapshot_dir.join("registry.meta.json"))
    } else {
        RegistryMeta::default()
    };
    let response = http
        .get_conditional(url, meta.etag.as_deref(), meta.last_modified.as_deref())
        .await?;
    if response.status == 304 {
        let registry =
            cached.ok_or_else(|| format!("HTTP 304 without a cached registry for {url}"))?;
        record.registry_path = "registry.json".to_string();
        record.resolved_sha = None;
        record.last_refreshed_at = Some(now());
        return Ok(RefreshOutcome {
            change: Change::Unchanged,
            resolved_sha: None,
            entry_count: registry.entries.len(),
        });
    }
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "registry request failed with HTTP {}",
            response.status
        ));
    }
    let body = response
        .body
        .ok_or_else(|| "registry response had no body".to_string())?;
    // Only a body that parses replaces the cached snapshot.
    let registry = parse_registry_text(&body, url, snapshot.clone()).map_err(join_diagnostics)?;
    std::fs::create_dir_all(&snapshot_dir)
        .map_err(|err| format!("cannot create {}: {err}", snapshot_dir.display()))?;
    write_atomic(&snapshot, body.as_bytes())?;
    let meta = RegistryMeta {
        etag: response.etag,
        last_modified: response.last_modified,
    };
    write_atomic(
        &snapshot_dir.join("registry.meta.json"),
        &serde_json::to_vec_pretty(&meta).map_err(|err| err.to_string())?,
    )?;
    record.registry_path = "registry.json".to_string();
    record.resolved_sha = None;
    record.last_refreshed_at = Some(now());
    Ok(RefreshOutcome {
        change: Change::Updated,
        resolved_sha: None,
        entry_count: registry.entries.len(),
    })
}

/// Parse a registry JSON file at an exact path.
///
/// The probe order applies to directories; a `Path` marketplace source that
/// is a file, a URL snapshot and the bundled resource are read exactly here.
pub(crate) fn parse_registry_file(path: &Path) -> Result<Registry, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    parse_registry_text(&text, &path.display().to_string(), path.to_path_buf())
        .map_err(join_diagnostics)
}

fn load_meta(path: &Path) -> RegistryMeta {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Clone the registry work tree, then read the registry from it.
fn refresh_git(
    record: &mut MarketplaceRecord,
    dir: &Path,
    source: &PluginSource,
) -> Result<RefreshOutcome, String> {
    let (url, sub_path, git_ref) = git_target(source)?;
    let repo = dir.join(&record.id).join("repo");
    // Read HEAD before the fetch so a bad new commit can be checked out back.
    let prior_sha = if repo.join(".git").is_dir() {
        Some(git_in(&repo, &["rev-parse", "HEAD"])?)
    } else {
        None
    };
    if prior_sha.is_some() {
        update_repo(&repo, git_ref.as_deref())?;
    } else {
        clone_repo(&url, git_ref.as_deref(), &repo)?;
    }
    let (registry, registry_root) = match read_git_registry(&repo, sub_path.as_deref()) {
        Ok(found) => found,
        Err(err) => {
            if let Some(prior) = prior_sha.as_deref() {
                if let Err(restore) = git_in(&repo, &["checkout", "--detach", prior]) {
                    return Err(format!(
                        "{err} (failed to restore previous commit {prior}: {restore})"
                    ));
                }
            }
            return Err(err);
        }
    };
    let sha = git_in(&repo, &["rev-parse", "HEAD"])?;
    let changed = record.resolved_sha.as_deref() != Some(sha.as_str());
    record.registry_path = display_registry_path(&registry, &registry_root);
    record.resolved_sha = Some(sha.clone());
    record.last_refreshed_at = Some(now());
    Ok(RefreshOutcome {
        change: if changed {
            Change::Updated
        } else {
            Change::Unchanged
        },
        resolved_sha: Some(sha),
        entry_count: registry.entries.len(),
    })
}

/// The clone URL, optional subdirectory and optional ref of a git source.
pub(crate) fn git_target(
    source: &PluginSource,
) -> Result<(String, Option<String>, Option<String>), String> {
    match source {
        PluginSource::Github {
            repo,
            path,
            git_ref,
            ..
        } => {
            let (base, fragment) = parse_git_remote(repo);
            let url = if base.contains("://") || base.starts_with("git@") {
                base
            } else {
                format!("https://github.com/{base}.git")
            };
            Ok((url, path.clone(), git_ref.clone().or(fragment)))
        }
        PluginSource::Git {
            url, path, git_ref, ..
        } => {
            let (base, fragment) = parse_git_remote(url);
            Ok((base, path.clone(), git_ref.clone().or(fragment)))
        }
        PluginSource::GitSubdir {
            url, path, git_ref, ..
        } => {
            let (base, fragment) = parse_git_remote(url);
            Ok((base, Some(path.clone()), git_ref.clone().or(fragment)))
        }
        _ => Err("source is not git-backed".to_string()),
    }
}

/// Read the registry after a clone or fetch.
///
/// A path that does not exist is missing in this revision. A path that
/// exists but leaves the checkout is an escape. Those are different errors.
fn read_git_registry(repo: &Path, sub_path: Option<&str>) -> Result<(Registry, PathBuf), String> {
    let wanted = match sub_path {
        Some(path) => repo.join(path),
        None => repo.to_path_buf(),
    };
    if !wanted.exists() {
        return Err(format!(
            "registry path {} is missing in this revision",
            wanted.display()
        ));
    }
    let registry_root = resolve_within(repo, &wanted).ok_or_else(|| {
        format!(
            "registry path {} resolves outside the root",
            wanted.display()
        )
    })?;
    let registry = parse_registry(&registry_root).map_err(join_diagnostics)?;
    Ok((registry, registry_root))
}

/// Clone `url` into `repo`, retrying once; a half clone is removed each time.
///
/// Spec §5 / R17: try `--filter=blob:none` first. If git rejects that flag,
/// fall back to a plain `--depth 1` clone.
pub(crate) fn clone_repo(url: &str, git_ref: Option<&str>, repo: &Path) -> Result<(), String> {
    match clone_attempts(url, git_ref, repo, true) {
        Ok(()) => Ok(()),
        Err(err) if filter_flag_rejected(&err) => {
            let _ = std::fs::remove_dir_all(repo);
            clone_attempts(url, git_ref, repo, false)
        }
        Err(err) => Err(err),
    }
}

fn clone_attempts(
    url: &str,
    git_ref: Option<&str>,
    repo: &Path,
    filtered: bool,
) -> Result<(), String> {
    let mut last_error = String::new();
    for _ in 0..2 {
        if let Some(parent) = repo.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        }
        let _ = std::fs::remove_dir_all(repo);
        let mut command = git_command();
        command.arg("clone").arg("--depth").arg("1");
        if filtered {
            command.arg("--filter=blob:none");
        }
        if let Some(git_ref) = git_ref {
            command.arg("--branch").arg(git_ref);
        }
        command.arg(url).arg(repo);
        match git_output(command) {
            Ok(_) => {
                note_clone_form(filtered);
                return Ok(());
            }
            Err(err) => {
                last_error = err;
                let _ = std::fs::remove_dir_all(repo);
                if filtered && filter_flag_rejected(&last_error) {
                    return Err(last_error);
                }
            }
        }
    }
    Err(last_error)
}

/// Older git prints `unknown option` for `--filter`. A normal clone failure does not.
fn filter_flag_rejected(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    let unknown = lower.contains("unknown option")
        || lower.contains("unrecognized option")
        || lower.contains("unrecognised option");
    unknown && lower.contains("filter")
}

/// Name the clone form that ran (spec §5, controller ruling R17).
fn note_clone_form(filtered: bool) {
    if filtered {
        tracing::debug!("marketplace clone form: --depth 1 --filter=blob:none");
    } else {
        tracing::warn!("marketplace clone form: --depth 1 (git rejected --filter=blob:none)");
    }
}

/// Fetch the wanted ref and detach the work tree onto it.
fn update_repo(repo: &Path, git_ref: Option<&str>) -> Result<(), String> {
    git_in(
        repo,
        &["fetch", "--depth", "1", "origin", git_ref.unwrap_or("HEAD")],
    )?;
    git_in(repo, &["checkout", "--detach", "FETCH_HEAD"])?;
    Ok(())
}

/// A `git` command with the repository-redirecting environment removed
/// (the `snapshot.rs` pattern).
fn git_command() -> Command {
    let mut command = Command::new("git");
    for var in GIT_ENV_TO_CLEAR {
        command.env_remove(var);
    }
    command
}

/// Run `git -C repo …` and fail with the trimmed stderr.
pub(crate) fn git_in(repo: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = git_command();
    command.arg("-C").arg(repo).args(args);
    git_output(command)
}

fn git_output(mut command: Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// One string for the UI from a failed parse.
pub(crate) fn join_diagnostics(diagnostics: Vec<Diagnostic>) -> String {
    diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect::<Vec<_>>()
        .join("; ")
}

/// The probe path relative to its root, for display; absolute when it is not
/// below the root (a registry given as a file).
fn display_registry_path(registry: &Registry, root: &Path) -> String {
    registry
        .registry_path
        .strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(&registry.registry_path)
        .display()
        .to_string()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::filter_flag_rejected;

    #[test]
    fn old_git_filter_flag_rejection_is_distinct_from_clone_failure() {
        assert!(filter_flag_rejected(
            "error: unknown option `filter=blob:none'"
        ));
        assert!(filter_flag_rejected(
            "error: unrecognized option '--filter=blob:none'"
        ));
        assert!(filter_flag_rejected(
            "error: unrecognised option '--filter'"
        ));
        assert!(!filter_flag_rejected("fatal: repository 'x' not found"));
        assert!(!filter_flag_rejected("error: unknown option `branch'"));
    }
}
