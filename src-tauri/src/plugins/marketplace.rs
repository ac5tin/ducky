//! Marketplace registry parsing and normalisation (design §5).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::diagnostics::{DiagLevel, Diagnostic};
use super::path::resolve_within;

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
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => return Err(vec![error(rel, format!("invalid JSON: {err}"))]),
        };
        let object = match value.as_object() {
            Some(object) => object,
            None => {
                return Err(vec![error(
                    rel,
                    "marketplace registry must be a JSON object",
                )])
            }
        };
        return parse_object(object, rel, path);
    }
    Err(vec![error(
        PROBES[0],
        "no marketplace registry found (.claude-plugin/marketplace.json, marketplace.json, .ducky/marketplace.json)",
    )])
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
