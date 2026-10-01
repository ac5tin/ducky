//! Plugin manifest, path containment and layout tests (design §1–§2, §13).

use std::path::Path;

use super::diagnostics::DiagLevel;
use super::layout::{discover, PluginTransport, RemoteKind};
use super::manifest::{load, Layout};

const SPEC_100: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// Write `body` to `dir/rel`, creating parent directories.
fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Create `link` as a symlink pointing at `target`.
#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Write a minimal valid `SKILL.md` into `skills-relative` directory `dir`.
fn write_skill(root: &Path, dir: &str, name: &str) {
    write(
        root,
        &format!("{dir}/SKILL.md"),
        &format!("---\nname: {name}\ndescription: demo\n---\nBody."),
    );
}

/// An Agent Plugins plugin with optional `mcpServers` JSON, discovered.
fn discover_agent(root: &Path, servers: Option<&str>) -> super::layout::Discovered {
    write(root, "plugin.json", &agent_manifest("demo-plugin", ""));
    if let Some(servers) = servers {
        write(
            root,
            "mcp.json",
            &format!(r#"{{"$schema": "{SPEC_100}", "mcpServers": {servers}}}"#),
        );
    }
    let manifest = load(root).unwrap();
    discover(root, &manifest)
}

/// An Agent Plugins manifest with `extra` appended to the object.
fn agent_manifest(name: &str, extra: &str) -> String {
    format!(r#"{{"$schema": "{SPEC_100}", "name": "{name}"{extra}}}"#)
}

#[test]
fn loads_minimal_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "plugin.json",
        &agent_manifest("minimal-plugin", ""),
    );

    let manifest = load(tmp.path()).unwrap();

    assert_eq!(manifest.name, "minimal-plugin");
    assert_eq!(manifest.layout, Layout::AgentPlugins);
    assert!(manifest.extensions.is_empty());
    assert!(manifest.unsupported.is_empty());
}

#[test]
fn rejects_missing_schema() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "plugin.json", r#"{"name": "no-schema"}"#);

    let diagnostics = load(tmp.path()).unwrap_err();

    assert!(diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Error && d.target == "plugin.json"));
}

#[test]
fn rejects_unknown_schema_version() {
    let tmp = tempfile::tempdir().unwrap();
    let body = r#"{"$schema": "https://agent-plugins.org/schemas/2.0.0/plugin.schema.json", "name": "future"}"#;
    write(tmp.path(), "plugin.json", body);

    let diagnostics = load(tmp.path()).unwrap_err();

    assert!(diagnostics
        .iter()
        .any(|d| d.message.contains("unsupported")));
}

#[test]
fn rejects_bad_name() {
    for name in ["My-Plugin", "-start", "has--double", "a..b", ""] {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "plugin.json", &agent_manifest(name, ""));
        assert!(
            load(tmp.path()).is_err(),
            "expected {name:?} to be rejected"
        );
    }
}

#[test]
fn unknown_top_level_key_is_non_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "plugin.json",
        &agent_manifest("ok-plugin", r#", "futureField": 1"#),
    );

    let manifest = load(tmp.path()).unwrap();

    assert!(manifest
        .unsupported
        .iter()
        .any(|d| d.level == DiagLevel::Warning));
}

#[test]
fn non_object_extensions_is_non_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "plugin.json",
        &agent_manifest("ext-plugin", r#", "extensions": []"#),
    );

    let manifest = load(tmp.path()).unwrap();

    assert!(manifest.extensions.is_empty());
    assert!(manifest
        .unsupported
        .iter()
        .any(|d| d.level == DiagLevel::Warning));
}

#[test]
fn bad_author_field_is_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "plugin.json",
        &agent_manifest(
            "bad-author",
            r#", "author": {"name": "x", "url": "y", "extra": 1}"#,
        ),
    );

    let diagnostics = load(tmp.path()).unwrap_err();

    assert!(diagnostics.iter().any(|d| d.level == DiagLevel::Error));
}

#[test]
fn non_semver_version_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "plugin.json",
        &agent_manifest("versioned", r#", "version": "banana""#),
    );

    let manifest = load(tmp.path()).unwrap();

    assert_eq!(manifest.version.as_deref(), Some("banana"));
}

#[test]
fn invalid_plugin_json_does_not_fall_through() {
    let tmp = tempfile::tempdir().unwrap();
    // Root manifest exists but is invalid...
    write(tmp.path(), "plugin.json", r#"{"name": "broken"}"#);
    // ...while a valid compat manifest sits beside it.
    write(
        tmp.path(),
        ".claude-plugin/plugin.json",
        r#"{"name": "valid-legacy"}"#,
    );

    assert!(load(tmp.path()).is_err());
}

#[test]
fn claude_layout_loads_without_schema() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        ".claude-plugin/plugin.json",
        r#"{"name": "legacy-plugin"}"#,
    );

    let manifest = load(tmp.path()).unwrap();

    assert_eq!(manifest.name, "legacy-plugin");
    assert_eq!(manifest.layout, Layout::ClaudeCode);
}

#[test]
fn zcode_layout_reads_inline_components() {
    let tmp = tempfile::tempdir().unwrap();
    let body = r#"{
        "name": "zcode-plugin",
        "mcpServers": {"local": {"command": "node", "args": ["server.js"]}},
        "agents": ["reviewer"]
    }"#;
    write(tmp.path(), ".zcode-plugin/plugin.json", body);

    let manifest = load(tmp.path()).unwrap();

    assert_eq!(manifest.layout, Layout::ClaudeCode);
    assert!(manifest.inline_servers.is_some());
    assert!(manifest.inline_subagents.is_some());
}

// --- Path containment (design §13) ---

#[test]
fn resolve_within_rejects_parent_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    write(&root, "a/b.txt", "inside");
    // The escape target must exist first (R7): canonicalize fails on a
    // missing file, so without this the test would pass for the wrong reason.
    write(tmp.path(), "b.txt", "outside");

    let root = root.as_path();
    assert!(super::path::resolve_within(root, &root.join("a/../b.txt")).is_none());
    // Same relative path without the `..` stays inside.
    assert!(super::path::resolve_within(root, &root.join("a/b.txt")).is_some());
}

#[test]
#[cfg(unix)]
fn resolve_within_allows_internal_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "a/real.txt", "x");
    symlink(&root.join("a/real.txt"), &root.join("link.txt"));

    let resolved = super::path::resolve_within(root, &root.join("link.txt")).unwrap();

    assert_eq!(
        resolved,
        std::fs::canonicalize(root.join("a/real.txt")).unwrap()
    );
}

#[test]
#[cfg(unix)]
fn resolve_within_rejects_escaping_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(outside.path(), "secret.txt", "x");
    symlink(&outside.path().join("secret.txt"), &root.join("link.txt"));

    assert!(super::path::resolve_within(root, &root.join("link.txt")).is_none());
}

#[test]
fn resolve_within_maybe_missing_handles_new_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let resolved = super::path::resolve_within_maybe_missing(root, &root.join("data/sub"));

    assert!(resolved.is_some());
}

// --- Component discovery (design §2) ---

#[test]
fn discovers_skills_and_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "skills/a", "a");

    let found = discover_agent(
        tmp.path(),
        Some(r#"{"local": {"type": "stdio", "command": "node", "args": ["server.js"]}}"#),
    );

    assert_eq!(found.skills.len(), 1);
    assert_eq!(found.skills[0].name, "a");
    assert_eq!(found.skills[0].description, "demo");
    assert_eq!(found.servers.len(), 1);
    assert!(found.diagnostics.is_empty());
}

#[test]
fn missing_locations_ok() {
    let tmp = tempfile::tempdir().unwrap();

    let found = discover_agent(tmp.path(), None);

    assert!(found.skills.is_empty());
    assert!(found.servers.is_empty());
    assert!(found.subagents.is_empty());
    assert!(found.diagnostics.is_empty());
}

#[test]
fn no_recursive_skill_search() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "skills/group/a", "a");

    let found = discover_agent(tmp.path(), None);

    assert!(found.skills.is_empty());
    assert!(found.diagnostics.is_empty());
}

#[test]
fn invalid_skill_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "skills/a", "b");

    let found = discover_agent(tmp.path(), None);

    assert!(found.skills.is_empty());
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning && d.target.contains("skills/a")));
}

#[test]
#[cfg(unix)]
fn rejecting_manifest_path_rejects_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "plugin.json", &agent_manifest("escapee", ""));
    symlink(
        &outside.path().join("plugin.json"),
        &tmp.path().join("plugin.json"),
    );

    let diagnostics = load(tmp.path()).unwrap_err();

    assert!(diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Error && d.message.contains("outside")));
}

// --- mcp.json validation (design §2, §7.2.1–§7.2.2) ---

#[test]
fn mcp_variants_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let servers = r#"{
        "local": {"type": "stdio", "command": "node", "args": ["server.js"]},
        "http": {"type": "streamable-http", "url": "https://example.com/mcp"},
        "legacy": {"type": "sse", "url": "https://example.com/sse"}
    }"#;

    let found = discover_agent(tmp.path(), Some(servers));

    assert_eq!(found.servers.len(), 3);
    let server = |name: &str| {
        found
            .servers
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("missing server {name}"))
    };
    assert!(matches!(server("local").transport, PluginTransport::Stdio { .. }));
    assert!(matches!(
        server("http").transport,
        PluginTransport::Remote { kind: RemoteKind::StreamableHttp, .. }
    ));
    assert!(found.diagnostics.is_empty());
}

#[test]
fn bad_server_entry_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let servers = r#"{
        "good": {"type": "stdio", "command": "node"},
        "bad": {"type": "stdio", "command": "node", "bogus": 1}
    }"#;

    let found = discover_agent(tmp.path(), Some(servers));

    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "good");
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning && d.target == "bad"));
}

#[test]
fn reserved_env_key_invalidates_server() {
    // R5: reserved keys are rejected case-insensitively on every platform.
    for key in ["PLUGIN_ROOT", "plugin_root", "Plugin_Root"] {
        let tmp = tempfile::tempdir().unwrap();
        let servers = format!(
            r#"{{"s": {{"type": "stdio", "command": "node", "env": {{"{key}": "/x"}}}}}}"#
        );

        let found = discover_agent(tmp.path(), Some(&servers));

        assert!(
            found.servers.is_empty(),
            "env key {key} must invalidate the server"
        );
        assert!(found
            .diagnostics
            .iter()
            .any(|d| d.level == DiagLevel::Warning && d.target == "s"));
    }
}

#[test]
fn cwd_escape_invalidates_server() {
    let tmp = tempfile::tempdir().unwrap();
    let servers = r#"{"s": {"type": "stdio", "command": "node", "cwd": "../outside"}}"#;

    let found = discover_agent(tmp.path(), Some(servers));

    assert!(found.servers.is_empty());
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning && d.target == "s"));
}

#[test]
fn mismatched_mcp_schema_disables_mcp_only() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "skills/a", "a");
    write(tmp.path(), "plugin.json", &agent_manifest("demo-plugin", ""));
    write(
        tmp.path(),
        "mcp.json",
        r#"{"$schema": "https://agent-plugins.org/schemas/2.0.0/plugin.schema.json", "mcpServers": {"local": {"type": "stdio", "command": "node"}}}"#,
    );
    let manifest = load(tmp.path()).unwrap();

    let found = discover(tmp.path(), &manifest);

    assert_eq!(found.skills.len(), 1);
    assert!(found.servers.is_empty());
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning));
}

#[test]
fn remote_url_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let servers = r#"{
        "plain-http": {"type": "streamable-http", "url": "http://example.com/mcp"},
        "with-userinfo": {"type": "sse", "url": "https://u:p@example.com/mcp"},
        "loopback": {"type": "streamable-http", "url": "http://localhost:3000/mcp"}
    }"#;

    let found = discover_agent(tmp.path(), Some(servers));

    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "loopback");
    let warned: Vec<&str> = found
        .diagnostics
        .iter()
        .filter(|d| d.level == DiagLevel::Warning)
        .map(|d| d.target.as_str())
        .collect();
    assert!(warned.contains(&"plain-http"));
    assert!(warned.contains(&"with-userinfo"));
}

#[test]
fn duplicate_header_casing_invalidates_server() {
    let tmp = tempfile::tempdir().unwrap();
    let servers = r#"{"s": {"type": "sse", "url": "https://example.com/mcp", "headers": {"X-A": "1", "x-a": "2"}}}"#;

    let found = discover_agent(tmp.path(), Some(servers));

    assert!(found.servers.is_empty());
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning && d.target == "s"));
}

#[test]
fn claude_mcp_json_aliases_normalize() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), ".claude-plugin/plugin.json", r#"{"name": "legacy"}"#);
    write(
        tmp.path(),
        ".mcp.json",
        r#"{"mcpServers": {"local": {"type": "stdio", "command": "node", "cwd": "${CLAUDE_PLUGIN_ROOT}/tools"}}}"#,
    );
    let manifest = load(tmp.path()).unwrap();

    let found = discover(tmp.path(), &manifest);

    assert_eq!(found.servers.len(), 1);
    match &found.servers[0].transport {
        PluginTransport::Stdio { cwd, .. } => {
            assert_eq!(cwd.as_deref(), Some("${PLUGIN_ROOT}/tools"))
        }
        other => panic!("expected stdio transport, got {other:?}"),
    }
}
