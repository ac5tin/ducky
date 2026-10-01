//! Plugin manifest loading and validation (design §1–§2).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::diagnostics::{DiagLevel, Diagnostic};

/// The only Agent Plugins schema version this client implements.
pub const SUPPORTED_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// Manifest probe order; the first file that exists decides the result.
const PROBES: [(&str, Layout); 3] = [
    ("plugin.json", Layout::AgentPlugins),
    (".claude-plugin/plugin.json", Layout::ClaudeCode),
    (".zcode-plugin/plugin.json", Layout::ClaudeCode),
];

/// Permitted top-level keys of an Agent Plugins manifest (closed set).
const AGENT_KEYS: [&str; 10] = [
    "$schema",
    "name",
    "version",
    "description",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
    "extensions",
];

/// Claude Code component fields Ducky does not implement (warn and ignore).
const UNSUPPORTED_CLAUDE_KEYS: [&str; 9] = [
    "commands",
    "hooks",
    "lspServers",
    "outputStyles",
    "workflows",
    "experimental",
    "channels",
    "settings",
    "userConfig",
];

/// Manifest layouts Ducky can load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// Root `plugin.json`, conformant with Agent Plugins 1.0.0.
    AgentPlugins,
    /// `.claude-plugin/plugin.json` or `.zcode-plugin/plugin.json`.
    ClaudeCode,
}

/// Manifest `author` object.
#[derive(Debug, Clone, PartialEq)]
pub struct Author {
    pub name: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
}

/// A parsed plugin manifest, normalised across layouts.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginManifest {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<Author>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    pub extensions: BTreeMap<String, Value>,
    pub layout: Layout,
    pub unsupported: Vec<Diagnostic>,
    pub inline_servers: Option<Value>,
    pub inline_subagents: Option<Value>,
}

/// Load the first manifest that exists under `root`.
///
/// Probe order is `plugin.json`, `.claude-plugin/plugin.json`,
/// `.zcode-plugin/plugin.json`. The first manifest file that exists decides
/// the result: an existing but invalid manifest rejects the plugin, and Ducky
/// never falls through to a compat layout (design §1).
pub fn load(root: &Path) -> Result<PluginManifest, Vec<Diagnostic>> {
    for (rel, layout) in PROBES {
        let path = root.join(rel);
        if !path.is_file() {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                return Err(vec![error(
                    rel,
                    format!("cannot read plugin manifest: {err}"),
                )])
            }
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(err) => return Err(vec![error(rel, format!("invalid JSON: {err}"))]),
        };
        let object = match value.as_object() {
            Some(object) => object,
            None => return Err(vec![error(rel, "plugin manifest must be a JSON object")]),
        };
        return match layout {
            Layout::AgentPlugins => validate_agent_plugins(object, rel),
            Layout::ClaudeCode => validate_claude_code(object, rel),
        };
    }
    Err(vec![error(
        "plugin.json",
        "no plugin manifest found (plugin.json, .claude-plugin/plugin.json, .zcode-plugin/plugin.json)",
    )])
}

/// Validate a conformant Agent Plugins manifest (design §2).
fn validate_agent_plugins(
    object: &Map<String, Value>,
    target: &str,
) -> Result<PluginManifest, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    let mut unsupported = Vec::new();

    for key in object.keys() {
        if !AGENT_KEYS.contains(&key.as_str()) {
            unsupported.push(warning(
                target,
                format!("unknown top-level key ignored: {key}"),
            ));
        }
    }

    match object.get("$schema") {
        None => errors.push(error(target, "missing required field: $schema")),
        Some(Value::String(schema)) if schema == SUPPORTED_SCHEMA => {}
        Some(value) => errors.push(error(
            target,
            format!("unsupported Agent Plugins version: {}", display(value)),
        )),
    }

    let name = match object.get("name") {
        Some(Value::String(name)) if valid_name(name) => name.clone(),
        Some(Value::String(name)) => {
            errors.push(error(target, format!("invalid plugin name: {name}")));
            String::new()
        }
        Some(_) => {
            errors.push(error(target, "field `name` must be a string"));
            String::new()
        }
        None => {
            errors.push(error(target, "missing required field: name"));
            String::new()
        }
    };

    let version = optional_string(object, "version", target, &mut errors);
    let description = optional_string(object, "description", target, &mut errors);
    let homepage = optional_string(object, "homepage", target, &mut errors);
    let repository = optional_string(object, "repository", target, &mut errors);
    let license = optional_string(object, "license", target, &mut errors);

    let keywords = match object.get("keywords") {
        None => Vec::new(),
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(_) => {
            errors.push(error(
                target,
                "field `keywords` must be an array of strings",
            ));
            Vec::new()
        }
    };

    let extensions = match object.get("extensions") {
        None => BTreeMap::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        Some(_) => {
            unsupported.push(warning(
                target,
                "field `extensions` must be an object; ignored",
            ));
            BTreeMap::new()
        }
    };

    let author = parse_author(object.get("author"), target, &mut errors);

    if !errors.is_empty() {
        errors.append(&mut unsupported);
        return Err(errors);
    }

    Ok(PluginManifest {
        name,
        version,
        description,
        author,
        homepage,
        repository,
        license,
        keywords,
        extensions,
        layout: Layout::AgentPlugins,
        unsupported,
        inline_servers: None,
        inline_subagents: None,
    })
}

/// Validate a Claude Code / ZCode manifest: `name` required, the rest optional.
fn validate_claude_code(
    object: &Map<String, Value>,
    target: &str,
) -> Result<PluginManifest, Vec<Diagnostic>> {
    let mut errors = Vec::new();

    let name = match object.get("name") {
        Some(Value::String(name)) if !name.is_empty() => name.clone(),
        Some(Value::String(_)) => {
            errors.push(error(target, "field `name` must not be empty"));
            String::new()
        }
        Some(_) => {
            errors.push(error(target, "field `name` must be a string"));
            String::new()
        }
        None => {
            errors.push(error(target, "missing required field: name"));
            String::new()
        }
    };

    let author = parse_author(object.get("author"), target, &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut unsupported = Vec::new();
    for key in UNSUPPORTED_CLAUDE_KEYS {
        if object.contains_key(key) {
            unsupported.push(warning(
                target,
                format!("unsupported Claude Code plugin field: {key}"),
            ));
        }
    }

    Ok(PluginManifest {
        name,
        version: string_field(object, "version"),
        description: string_field(object, "description"),
        author,
        homepage: string_field(object, "homepage"),
        repository: string_field(object, "repository"),
        license: string_field(object, "license"),
        keywords: string_array_field(object, "keywords"),
        extensions: BTreeMap::new(),
        layout: Layout::ClaudeCode,
        unsupported,
        inline_servers: object.get("mcpServers").cloned(),
        inline_subagents: object.get("agents").cloned(),
    })
}

/// Name constraints (§5.5): 1–64 chars, `[a-z0-9.-]`, first and last character
/// alphanumeric, no `--`, no `..`.
fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-';
    if !bytes.iter().all(|&b| allowed(b)) {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    !name.contains("--") && !name.contains("..")
}

/// A present string field, or a fatal diagnostic for another type.
fn optional_string(
    object: &Map<String, Value>,
    key: &str,
    target: &str,
    errors: &mut Vec<Diagnostic>,
) -> Option<String> {
    match object.get(key) {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            errors.push(error(target, format!("field `{key}` must be a string")));
            None
        }
    }
}

/// A present string array, or a fatal diagnostic for another type.
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

/// A lenient string field for compat layouts: wrong types are dropped.
fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `author` accepts only the `name`, `email` and `url` string keys.
fn parse_author(
    value: Option<&Value>,
    target: &str,
    errors: &mut Vec<Diagnostic>,
) -> Option<Author> {
    let map = match value? {
        Value::Object(map) => map,
        _ => {
            errors.push(error(target, "field `author` must be an object"));
            return None;
        }
    };
    let mut author = Author {
        name: None,
        email: None,
        url: None,
    };
    for (key, value) in map {
        let slot = match key.as_str() {
            "name" => &mut author.name,
            "email" => &mut author.email,
            "url" => &mut author.url,
            other => {
                errors.push(error(target, format!("unknown key in `author`: {other}")));
                continue;
            }
        };
        match value {
            Value::String(text) => *slot = Some(text.clone()),
            _ => errors.push(error(
                target,
                format!("field `author.{key}` must be a string"),
            )),
        }
    }
    Some(author)
}

fn error(target: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        level: DiagLevel::Error,
        target: target.to_string(),
        message: message.into(),
    }
}

fn warning(target: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        level: DiagLevel::Warning,
        target: target.to_string(),
        message: message.into(),
    }
}

/// Human-readable rendering of a JSON scalar for diagnostics.
fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
