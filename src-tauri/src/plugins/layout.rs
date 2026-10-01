//! Component discovery (design §2): skills, MCP servers and subagents, each
//! isolated behind the failure-boundary ladder of §2 so the narrowest
//! applicable boundary fails.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::diagnostics::{DiagLevel, Diagnostic};
use super::manifest::{Layout, PluginManifest, SUPPORTED_SCHEMA};
use super::path::{resolve_within, resolve_within_maybe_missing};

/// Everything discovered in one plugin package.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Discovered {
    pub skills: Vec<SkillEntry>,
    pub servers: Vec<PluginServer>,
    pub subagents: Vec<PluginSubagent>,
    pub diagnostics: Vec<Diagnostic>,
}

/// One skill directory with its parsed `SKILL.md` frontmatter.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub allowed_tools: Option<String>,
    pub dir: PathBuf,
}

/// One MCP server declared by the plugin; placeholders are still unexpanded.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginServer {
    pub name: String,
    pub transport: PluginTransport,
}

/// Transport of a [`PluginServer`], validated at discovery time.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
    },
    Remote {
        kind: RemoteKind,
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// Remote transport kind (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    StreamableHttp,
    Sse,
}

/// One plugin subagent, frontmatter only; task 10 maps it to `SubagentConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginSubagent {
    pub name: String,
    pub description: String,
    pub frontmatter: BTreeMap<String, String>,
    pub body: String,
}

/// Discover the components of one plugin package (design §2).
///
/// Every failure boundary is isolated: an invalid skill is skipped, an invalid
/// server is skipped, a broken `mcp.json` disables MCP for this plugin only,
/// and none of it blocks the other component types.
pub fn discover(root: &Path, manifest: &PluginManifest) -> Discovered {
    let mut found = Discovered::default();
    discover_skills(root, &mut found);
    match manifest.layout {
        Layout::AgentPlugins => agent_mcp(root, &mut found),
        Layout::ClaudeCode => claude_mcp(root, manifest, &mut found),
    }
    discover_subagents(root, manifest, &mut found);
    found
}

// --- Skills ---

fn discover_skills(root: &Path, out: &mut Discovered) {
    let skills = root.join("skills");
    if !skills.is_dir() {
        if skills.exists() {
            out.diagnostics
                .push(warning("skills", "`skills` is not a directory; skills disabled for this plugin"));
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(&skills) else {
        out.diagnostics
            .push(warning("skills", "cannot read `skills`; skills disabled for this plugin"));
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        // Immediate child directories only; no recursive search (§6.1).
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = dir.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let skill_md = dir.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let target = format!("skills/{name}/SKILL.md");
        if resolve_within(root, &skill_md).is_none() {
            out.diagnostics.push(warning(
                &target,
                "SKILL.md resolves outside the plugin root; skill skipped",
            ));
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&skill_md) else {
            out.diagnostics
                .push(warning(&target, "cannot read SKILL.md; skill skipped"));
            continue;
        };
        let Some(frontmatter) = parse_frontmatter(&text) else {
            out.diagnostics
                .push(warning(&target, "SKILL.md has no YAML frontmatter; skill skipped"));
            continue;
        };
        let (frontmatter, _body) = frontmatter;
        let skill_name = frontmatter.get("name").filter(|n| !n.is_empty());
        let description = frontmatter.get("description").filter(|d| !d.is_empty());
        let (Some(skill_name), Some(description)) = (skill_name, description) else {
            out.diagnostics.push(warning(
                &target,
                "SKILL.md needs a non-empty `name` and `description`; skill skipped",
            ));
            continue;
        };
        if skill_name.as_str() != name {
            out.diagnostics.push(warning(
                &target,
                format!(
                    "frontmatter name `{skill_name}` does not match the directory name `{name}`; skill skipped"
                ),
            ));
            continue;
        }
        out.skills.push(SkillEntry {
            name: skill_name.clone(),
            description: description.clone(),
            license: frontmatter.get("license").cloned(),
            compatibility: frontmatter.get("compatibility").cloned(),
            allowed_tools: frontmatter.get("allowed-tools").cloned(),
            dir,
        });
    }
}

// --- MCP ---

fn agent_mcp(root: &Path, out: &mut Discovered) {
    let path = root.join("mcp.json");
    if !path.is_file() {
        if path.exists() {
            out.diagnostics.push(warning(
                "mcp.json",
                "`mcp.json` is not a file; MCP disabled for this plugin",
            ));
        }
        return;
    }
    if resolve_within(root, &path).is_none() {
        out.diagnostics.push(warning(
            "mcp.json",
            "`mcp.json` resolves outside the plugin root; MCP disabled for this plugin",
        ));
        return;
    }
    let parsed = std::fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|err| err.to_string()));
    let value = match parsed {
        Ok(value) => value,
        Err(err) => {
            out.diagnostics.push(warning(
                "mcp.json",
                format!("invalid `mcp.json`; MCP disabled for this plugin: {err}"),
            ));
            return;
        }
    };
    if value.get("$schema").and_then(Value::as_str) != Some(SUPPORTED_SCHEMA) {
        out.diagnostics.push(warning(
            "mcp.json",
            format!(
                "unsupported `mcp.json` $schema; MCP disabled for this plugin (expected {SUPPORTED_SCHEMA})"
            ),
        ));
        return;
    }
    let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else {
        out.diagnostics.push(warning(
            "mcp.json",
            "`mcpServers` must be an object; MCP disabled for this plugin",
        ));
        return;
    };
    for (name, server) in servers {
        match parse_server(root, name, server) {
            Ok(parsed) => out.servers.push(parsed),
            Err(message) => out.diagnostics.push(warning(name, message)),
        }
    }
}

fn claude_mcp(root: &Path, manifest: &PluginManifest, out: &mut Discovered) {
    // `.mcp.json` plus the manifest's inline `mcpServers`; the manifest wins
    // on a name collision. No `$schema` requirement in this layout.
    let mut raw: BTreeMap<String, Value> = BTreeMap::new();
    let path = root.join(".mcp.json");
    if !path.is_file() && path.exists() {
        out.diagnostics.push(warning(
            ".mcp.json",
            "`.mcp.json` is not a file; MCP disabled for this plugin",
        ));
    } else if path.is_file() {
        let parsed = std::fs::read_to_string(&path)
            .map_err(|err| err.to_string())
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|err| err.to_string()));
        match parsed {
            Err(err) => out.diagnostics.push(warning(
                ".mcp.json",
                format!("invalid `.mcp.json`; MCP disabled for this plugin: {err}"),
            )),
            Ok(value) => {
                match normalize_value(&value).get("mcpServers").and_then(Value::as_object) {
                    Some(servers) => {
                        for (name, server) in servers {
                            raw.insert(name.clone(), server.clone());
                        }
                    }
                    None => out.diagnostics.push(warning(
                        ".mcp.json",
                        "`.mcp.json` must contain an `mcpServers` object; MCP disabled for this plugin",
                    )),
                }
            }
        }
    }
    if let Some(Value::Object(inline)) = manifest.inline_servers.as_ref() {
        for (name, value) in inline {
            // Manifest wins on a name collision.
            raw.insert(name.clone(), normalize_value(value));
        }
    }
    for (name, server) in &raw {
        match parse_server(root, name, server) {
            Ok(parsed) => out.servers.push(parsed),
            Err(message) => out.diagnostics.push(warning(name, message)),
        }
    }
}

/// Validate one server entry against the closed union of §7.2.1. Any failure
/// invalidates exactly this server.
fn parse_server(root: &Path, name: &str, value: &Value) -> Result<PluginServer, String> {
    let object = value.as_object().ok_or("server entry must be a JSON object")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or("missing required field: type")?;
    match kind {
        "stdio" => {
            reject_unknown_fields(object, &["type", "command", "args", "env", "cwd"])?;
            let command = object
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| !command.is_empty())
                .ok_or("stdio server requires a non-empty string `command`")?;
            let args = string_list(object.get("args"))
                .ok_or("field `args` must be an array of strings")?;
            let env = string_map(object.get("env"))
                .ok_or("field `env` must be an object of strings")?;
            for key in env.keys() {
                // Reserved keys are rejected case-insensitively on every
                // platform so the rule is testable on Linux and correct on
                // Windows (case-insensitive filesystems).
                if key.eq_ignore_ascii_case("PLUGIN_ROOT")
                    || key.eq_ignore_ascii_case("PLUGIN_DATA")
                {
                    return Err(format!("reserved environment key: {key}"));
                }
            }
            let cwd = match object.get("cwd") {
                None => None,
                Some(Value::String(cwd)) => {
                    validate_cwd(root, cwd)?;
                    Some(cwd.clone())
                }
                Some(_) => return Err("field `cwd` must be a string".into()),
            };
            Ok(PluginServer {
                name: name.to_string(),
                transport: PluginTransport::Stdio {
                    command: command.to_string(),
                    args,
                    env,
                    cwd,
                },
            })
        }
        "streamable-http" | "sse" => {
            reject_unknown_fields(object, &["type", "url", "headers"])?;
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .ok_or("remote server requires a string `url`")?;
            validate_url(url)?;
            let headers = string_map(object.get("headers"))
                .ok_or("field `headers` must be an object of strings")?;
            validate_headers(&headers)?;
            Ok(PluginServer {
                name: name.to_string(),
                transport: PluginTransport::Remote {
                    kind: if kind == "sse" {
                        RemoteKind::Sse
                    } else {
                        RemoteKind::StreamableHttp
                    },
                    url: url.to_string(),
                    headers,
                },
            })
        }
        other => Err(format!("unsupported server type: {other}")),
    }
}

fn reject_unknown_fields(object: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown field: {key}"));
        }
    }
    Ok(())
}

/// A present array of strings, or `None` for any other type.
fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    match value {
        None => Some(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(_) => None,
    }
}

/// A present object of strings, or `None` for any other type.
fn string_map(value: Option<&Value>) -> Option<BTreeMap<String, String>> {
    match value {
        None => Some(BTreeMap::new()),
        Some(Value::Object(map)) => {
            let mut out = BTreeMap::new();
            for (key, item) in map {
                out.insert(key.clone(), item.as_str()?.to_string());
            }
            Some(out)
        }
        Some(_) => None,
    }
}

/// A `cwd` is valid only as `./…`, `${PLUGIN_ROOT}`/`${PLUGIN_ROOT}/…` or
/// `${PLUGIN_DATA}`/`${PLUGIN_DATA}/…`, and must stay inside its root after
/// expansion (§2 stdio launch rules). Validated at parse time, not spawn time.
fn validate_cwd(root: &Path, cwd: &str) -> Result<(), String> {
    if let Some(rest) = cwd.strip_prefix("./") {
        if rest.is_empty() {
            return Ok(());
        }
        return resolve_within_maybe_missing(root, &root.join(rest))
            .map(|_| ())
            .ok_or_else(|| "cwd escapes the plugin root".to_string());
    }
    if let Some(rest) = cwd.strip_prefix("${PLUGIN_ROOT}") {
        if rest.is_empty() {
            return Ok(());
        }
        let rel = rest
            .strip_prefix('/')
            .ok_or("cwd must be `${PLUGIN_ROOT}` or `${PLUGIN_ROOT}/…`")?;
        if rel.is_empty() {
            return Ok(());
        }
        return resolve_within_maybe_missing(root, &root.join(rel))
            .map(|_| ())
            .ok_or_else(|| "cwd escapes the plugin root".to_string());
    }
    if let Some(rest) = cwd.strip_prefix("${PLUGIN_DATA}") {
        if rest.is_empty() {
            return Ok(());
        }
        let rel = rest
            .strip_prefix('/')
            .ok_or("cwd must be `${PLUGIN_DATA}` or `${PLUGIN_DATA}/…`")?;
        // PLUGIN_DATA does not exist at parse time; Ducky creates the data
        // root itself, so containment is structural: no `..` may appear.
        if rel.split(['/', '\\']).any(|part| part == "..") {
            return Err("cwd escapes the plugin data root".into());
        }
        return Ok(());
    }
    Err("cwd must be `./…`, `${PLUGIN_ROOT}/…` or `${PLUGIN_DATA}/…`".into())
}

/// Remote URL rules (§2): absolute HTTP(S), no userinfo, no fragment,
/// non-loopback hosts must use HTTPS.
fn validate_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url)
        .map_err(|_| "url must be an absolute HTTP(S) URL".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("url must be an absolute HTTP(S) URL".into());
    }
    if !parsed.username().is_empty() {
        return Err("url must not contain userinfo".into());
    }
    if parsed.fragment().is_some() {
        return Err("url must not contain a fragment".into());
    }
    let host = parsed.host_str().unwrap_or_default();
    let loopback = host == "localhost" || host.starts_with("127.") || host == "[::1]";
    if parsed.scheme() != "https" && !loopback {
        return Err("non-loopback hosts require HTTPS".into());
    }
    Ok(())
}

/// Header names must be HTTP tokens, values valid field values, and a name
/// must not repeat under different casing (§2 remote server rules).
fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for (name, value) in headers {
        if !is_token(name) {
            return Err(format!("invalid header name: {name}"));
        }
        if !value.bytes().all(|byte| byte == b'\t' || (0x20..=0x7e).contains(&byte)) {
            return Err(format!("invalid header value for `{name}`"));
        }
        let lower = name.to_ascii_lowercase();
        if !seen.insert(lower) {
            return Err(format!("duplicate header name: {name}"));
        }
    }
    Ok(())
}

/// RFC 7230 `token`: the characters allowed in a header field name.
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// Claude Code placeholder aliases (§2) are normalised to the two spec
/// placeholders at parse time, so later tasks only ever see the spec spellings.
fn normalize_value(value: &Value) -> Value {
    const ALIASES: [(&str, &str); 4] = [
        ("${CLAUDE_PLUGIN_ROOT}", "${PLUGIN_ROOT}"),
        ("${CLAUDE_PLUGIN_DATA}", "${PLUGIN_DATA}"),
        ("${ZCODE_PLUGIN_ROOT}", "${PLUGIN_ROOT}"),
        ("${ZCODE_PLUGIN_DATA}", "${PLUGIN_DATA}"),
    ];
    match value {
        Value::String(text) => Value::String(
            ALIASES
                .iter()
                .fold(text.clone(), |acc, (from, to)| acc.replace(from, to)),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalize_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), normalize_value(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

// --- Subagents ---

fn discover_subagents(root: &Path, manifest: &PluginManifest, out: &mut Discovered) {
    subagent_files(root, manifest, out);
    subagent_inline(manifest, out);
}

fn subagent_files(root: &Path, manifest: &PluginManifest, out: &mut Discovered) {
    let dir = match manifest.layout {
        Layout::AgentPlugins => root.join("app.ducky/subagents"),
        Layout::ClaudeCode => root.join("agents"),
    };
    if !dir.is_dir() {
        if dir.exists() {
            out.diagnostics.push(warning(
                "subagents",
                "subagent location is not a directory; ignoring subagents",
            ));
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        out.diagnostics.push(warning(
            "subagents",
            "cannot read the subagent directory; ignoring subagents",
        ));
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(OsStr::to_str) != Some("md") {
            continue;
        }
        let target = rel_target(root, &path);
        if resolve_within(root, &path).is_none() {
            out.diagnostics.push(warning(
                &target,
                "subagent file resolves outside the plugin root; subagent skipped",
            ));
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            out.diagnostics
                .push(warning(&target, "cannot read subagent file; subagent skipped"));
            continue;
        };
        let Some((frontmatter, body)) = parse_frontmatter(&text) else {
            out.diagnostics
                .push(warning(&target, "no YAML frontmatter; subagent skipped"));
            continue;
        };
        let name = frontmatter.get("name").filter(|n| !n.is_empty());
        let description = frontmatter.get("description").filter(|d| !d.is_empty());
        let (Some(name), Some(description)) = (name, description) else {
            out.diagnostics.push(warning(
                &target,
                "subagent needs a non-empty `name` and `description`; subagent skipped",
            ));
            continue;
        };
        out.subagents.push(PluginSubagent {
            name: name.clone(),
            description: description.clone(),
            frontmatter,
            body,
        });
    }
}

fn subagent_inline(manifest: &PluginManifest, out: &mut Discovered) {
    let items = match manifest.layout {
        Layout::AgentPlugins => manifest
            .extensions
            .get("app.ducky")
            .and_then(|value| value.get("subagents"))
            .and_then(Value::as_array),
        Layout::ClaudeCode => manifest.inline_subagents.as_ref().and_then(Value::as_array),
    };
    let Some(items) = items else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object() else {
            out.diagnostics.push(warning(
                "subagents",
                "inline subagent must be an object; subagent skipped",
            ));
            continue;
        };
        let mut frontmatter = BTreeMap::new();
        for (key, value) in object {
            match value {
                Value::String(text) => {
                    frontmatter.insert(key.clone(), text.clone());
                }
                other => {
                    frontmatter.insert(key.clone(), other.to_string());
                }
            }
        }
        let name = frontmatter.get("name").filter(|n| !n.is_empty());
        let description = frontmatter.get("description").filter(|d| !d.is_empty());
        let (Some(name), Some(description)) = (name, description) else {
            out.diagnostics.push(warning(
                "subagents",
                "inline subagent needs a non-empty `name` and `description`; subagent skipped",
            ));
            continue;
        };
        out.subagents.push(PluginSubagent {
            name: name.clone(),
            description: description.clone(),
            frontmatter,
            body: String::new(),
        });
    }
}

// --- Frontmatter (R11: shared reader; task 9 relocates it to skills.rs) ---

/// Split `---`-delimited frontmatter from a Markdown document: returns the
/// scalar `key: value` pairs and the body, or `None` when there is no
/// frontmatter. Minimal YAML: one-line scalars, quotes stripped, everything
/// else ignored.
pub(crate) fn parse_frontmatter(text: &str) -> Option<(BTreeMap<String, String>, String)> {
    let mut lines = text.lines().enumerate();
    let (_, first) = lines.next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let mut map = BTreeMap::new();
    let mut closing = None;
    for (index, line) in lines {
        if line.trim_end() == "---" {
            closing = Some(index);
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            if !key.is_empty() {
                map.insert(key.to_string(), unquote(value.trim()));
            }
        }
    }
    let closing = closing?;
    let body = text
        .lines()
        .skip(closing + 1)
        .collect::<Vec<_>>()
        .join("\n");
    Some((map, body))
}

/// Strip one matching pair of single or double quotes.
fn unquote(value: &str) -> String {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

// --- Shared helpers ---

fn rel_target(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn warning(target: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        level: DiagLevel::Warning,
        target: target.into(),
        message: message.into(),
    }
}
