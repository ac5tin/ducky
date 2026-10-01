//! Plugin manifest, path containment, layout and marketplace tests
//! (design §1–§2, §5, §13).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::diagnostics::DiagLevel;
use super::layout::{discover, PluginTransport, RemoteKind};
use super::manifest::{load, Layout};
use super::marketplace::{
    parse_git_remote, parse_registry, parse_source, refresh, Change, HttpClient, HttpResponse,
    MarketplaceRecord, MarketplaceStore, PluginSource,
};

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

#[test]
#[cfg(unix)]
fn resolve_within_maybe_missing_rejects_symlink_dotdot() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    symlink(outside.path(), &root.join("link"));

    // Reviewer input: `root/link` points outside, target `root/link/../secret`.
    // Lexical `..` removal reports `<root>/secret`. The filesystem parent does not.
    let target = root.join("link/../secret");
    let canonical_root = std::fs::canonicalize(&root).unwrap();
    let filesystem_parent = std::fs::canonicalize(root.join("link/..")).unwrap();
    assert!(
        !filesystem_parent.starts_with(&canonical_root),
        "fixture must escape: {filesystem_parent:?}"
    );
    assert!(super::path::resolve_within_maybe_missing(&root, &target).is_none());

    // A real directory's `..` still resolves inside. Refusing every `..`
    // would hide the symlink bug behind a broader refusal.
    std::fs::create_dir(root.join("a")).unwrap();
    let kept = super::path::resolve_within_maybe_missing(&root, &root.join("a/../fresh")).unwrap();
    assert_eq!(kept, canonical_root.join("fresh"));

    // Symlink ancestor whose resolved parent stays inside, but is not the lexical parent.
    std::fs::create_dir_all(root.join("inside/nested")).unwrap();
    symlink(&root.join("inside/nested"), &root.join("nested-link"));
    let resolved =
        super::path::resolve_within_maybe_missing(&root, &root.join("nested-link/../back/newfile"))
            .unwrap();
    assert_eq!(
        resolved,
        std::fs::canonicalize(root.join("inside"))
            .unwrap()
            .join("back/newfile")
    );
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
    write(
        outside.path(),
        "plugin.json",
        &agent_manifest("escapee", ""),
    );
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
    assert!(matches!(
        server("local").transport,
        PluginTransport::Stdio { .. }
    ));
    assert!(matches!(
        server("http").transport,
        PluginTransport::Remote {
            kind: RemoteKind::StreamableHttp,
            ..
        }
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
        let servers =
            format!(r#"{{"s": {{"type": "stdio", "command": "node", "env": {{"{key}": "/x"}}}}}}"#);

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
    // These strings reach the containment check. A bare `../outside` fails
    // the form check first, so deleting the check would still pass.
    for cwd in [
        "./../outside",
        "${PLUGIN_ROOT}/../outside",
        "${PLUGIN_DATA}/../outside",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let servers = format!(r#"{{"s": {{"type": "stdio", "command": "node", "cwd": {cwd:?}}}}}"#);

        let found = discover_agent(tmp.path(), Some(&servers));

        assert!(
            found.servers.is_empty(),
            "{cwd} must be rejected, got {:?}",
            found.servers
        );
        assert!(found
            .diagnostics
            .iter()
            .any(|d| d.level == DiagLevel::Warning && d.target == "s"));
    }

    let tmp = tempfile::tempdir().unwrap();
    let found = discover_agent(
        tmp.path(),
        Some(r#"{"ok": {"type": "stdio", "command": "node", "cwd": "./sub"}}"#),
    );
    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "ok");
}

#[test]
fn mismatched_mcp_schema_disables_mcp_only() {
    let tmp = tempfile::tempdir().unwrap();
    write_skill(tmp.path(), "skills/a", "a");
    write(
        tmp.path(),
        "plugin.json",
        &agent_manifest("demo-plugin", ""),
    );
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
        "empty-user": {"type": "sse", "url": "https://:pass@example.com/mcp"},
        "fake-loopback": {"type": "streamable-http", "url": "http://127.evil.com/mcp"},
        "nip": {"type": "streamable-http", "url": "http://127.0.0.1.nip.io/mcp"},
        "loopback": {"type": "streamable-http", "url": "http://localhost:3000/mcp"},
        "ipv4-loopback": {"type": "streamable-http", "url": "http://127.0.0.1/mcp"},
        "ipv6-loopback": {"type": "streamable-http", "url": "http://[::1]/mcp"}
    }"#;

    let found = discover_agent(tmp.path(), Some(servers));

    let mut names: Vec<&str> = found.servers.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["ipv4-loopback", "ipv6-loopback", "loopback"]);
    let warned: Vec<&str> = found
        .diagnostics
        .iter()
        .filter(|d| d.level == DiagLevel::Warning)
        .map(|d| d.target.as_str())
        .collect();
    for name in [
        "plain-http",
        "with-userinfo",
        "empty-user",
        "fake-loopback",
        "nip",
    ] {
        assert!(
            warned.contains(&name),
            "{name} must be rejected, warned={warned:?}"
        );
    }
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
    write(
        tmp.path(),
        ".claude-plugin/plugin.json",
        r#"{"name": "legacy"}"#,
    );
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

/// Pinned claim for a `${PLUGIN_DATA}` cwd. Parse time does not prove
/// file-system containment.
const DATA_CWD_SYNTACTIC: &str = "syntactic check only; the authoritative canonicalised containment check runs at spawn once the data directory exists";

#[test]
fn plugin_data_cwd_trailing_space_is_syntactic() {
    for cwd in [
        "${PLUGIN_DATA}/.. ",
        "${PLUGIN_DATA}/foo//bar",
        "${PLUGIN_DATA}/C:/Windows",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let servers = format!(r#"{{"s": {{"type": "stdio", "command": "node", "cwd": {cwd:?}}}}}"#);

        let found = discover_agent(tmp.path(), Some(&servers));

        assert!(found.servers.is_empty(), "{cwd:?} must be rejected");
        let message = found
            .diagnostics
            .iter()
            .find(|d| d.target == "s")
            .map(|d| d.message.as_str())
            .unwrap_or("");
        assert_eq!(message, DATA_CWD_SYNTACTIC, "{cwd:?}");
    }

    let tmp = tempfile::tempdir().unwrap();
    let found = discover_agent(
        tmp.path(),
        Some(r#"{"s": {"type": "stdio", "command": "node", "cwd": "${PLUGIN_DATA}/sub"}}"#),
    );
    assert_eq!(found.servers.len(), 1);
}

#[test]
fn command_must_be_one_token() {
    for (command, ok) in [
        ("../bin", false),
        ("/bin/sh", false),
        ("./../outside", false),
        ("node -c", false),
        ("node", true),
        ("./server.js", true),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let servers = format!(r#"{{"s": {{"type": "stdio", "command": {command:?}}}}}"#);

        let found = discover_agent(tmp.path(), Some(&servers));

        assert_eq!(
            !found.servers.is_empty(),
            ok,
            "{command:?} servers={:?} diags={:?}",
            found.servers,
            found.diagnostics
        );
        if ok {
            match &found.servers[0].transport {
                PluginTransport::Stdio {
                    command: stored, ..
                } => assert_eq!(stored, command),
                other => panic!("expected stdio, got {other:?}"),
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn claude_mcp_json_outside_symlink_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        ".claude-plugin/plugin.json",
        r#"{"name": "legacy", "mcpServers": {"inline": {"type": "stdio", "command": "node"}}}"#,
    );
    write(
        outside.path(),
        "escaped.json",
        r#"{"mcpServers": {"escaped": {"type": "stdio", "command": "node"}}}"#,
    );
    symlink(
        &outside.path().join("escaped.json"),
        &tmp.path().join(".mcp.json"),
    );
    let manifest = load(tmp.path()).unwrap();

    let found = discover(tmp.path(), &manifest);

    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "inline");
    assert!(found.diagnostics.iter().any(|d| {
        d.target == ".mcp.json"
            && d.message.contains("outside")
            && !d.message.contains("MCP disabled")
    }));
}

// ---------------------------------------------------------------------------
// Marketplace registries (design §5)
// ---------------------------------------------------------------------------

/// A registry body with `name` and a `plugins` array written from `plugins`.
fn registry_body(name: &str, plugins: &str) -> String {
    format!(r#"{{"name": "{name}", "plugins": [{plugins}]}}"#)
}

#[test]
fn probes_claude_path_first() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        ".claude-plugin/marketplace.json",
        &registry_body("claude", r#"{"name": "a", "source": "./a"}"#),
    );
    write(tmp.path(), "marketplace.json", &registry_body("root", ""));
    write(
        tmp.path(),
        ".ducky/marketplace.json",
        &registry_body("ducky", ""),
    );

    let registry = parse_registry(tmp.path()).unwrap();

    assert_eq!(registry.name, "claude");
    assert!(registry
        .registry_path
        .ends_with(".claude-plugin/marketplace.json"));
    assert_eq!(registry.entries.len(), 1);
}

#[test]
fn probes_root_marketplace_json() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "marketplace.json",
        &registry_body("root", r#"{"name": "a", "source": "./a"}"#),
    );

    let registry = parse_registry(tmp.path()).unwrap();

    assert_eq!(registry.name, "root");
    assert!(registry.registry_path.ends_with("marketplace.json"));
    assert!(!registry
        .registry_path
        .ends_with(".claude-plugin/marketplace.json"));
}

#[test]
fn probes_ducky_path() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        ".ducky/marketplace.json",
        &registry_body("ducky", ""),
    );

    let registry = parse_registry(tmp.path()).unwrap();

    assert_eq!(registry.name, "ducky");
    assert!(registry.registry_path.ends_with(".ducky/marketplace.json"));
}

#[test]
fn no_registry_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();

    let diagnostics = parse_registry(tmp.path()).unwrap_err();

    assert!(diagnostics.iter().any(
        |d| d.level == DiagLevel::Error && d.message.contains("no marketplace registry found")
    ));
}

#[test]
fn plugins_not_an_array_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "marketplace.json",
        r#"{"name": "m", "plugins": {}}"#,
    );

    let diagnostics = parse_registry(tmp.path()).unwrap_err();

    assert!(diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Error && d.message.contains("plugins")));
}

#[test]
fn entry_without_source_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "marketplace.json",
        &registry_body("m", r#"{"name": "x"}"#),
    );

    let registry = parse_registry(tmp.path()).unwrap();

    let entry = &registry.entries[0];
    assert_eq!(entry.name, "x");
    assert!(!entry.available);
    assert!(entry
        .reason
        .as_deref()
        .unwrap_or_default()
        .contains("source"));
    assert!(matches!(&entry.source, PluginSource::Unsupported { .. }));
}

#[test]
fn github_source_without_repo_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "marketplace.json",
        &registry_body("m", r#"{"name": "x", "source": {"source": "github"}}"#),
    );

    let registry = parse_registry(tmp.path()).unwrap();

    let entry = &registry.entries[0];
    assert!(!entry.available);
    assert!(entry.reason.as_deref().unwrap_or_default().contains("repo"));
}

#[test]
fn all_source_discriminators_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = r#"
        {"name": "path", "source": "./p"},
        {"name": "github", "source": {"source": "github", "repo": "o/r", "path": "sub", "ref": "main", "sha": "abc123"}},
        {"name": "git", "source": {"source": "git", "url": "https://example.com/r.git"}},
        {"name": "subdir", "source": {"source": "git-subdir", "url": "https://example.com/r.git", "path": "sub/dir"}},
        {"name": "directory", "source": {"source": "directory", "path": "dir"}},
        {"name": "file", "source": {"source": "file", "path": "f.json"}},
        {"name": "url", "source": {"source": "url", "url": "https://example.com/registry.json"}}
    "#;
    write(tmp.path(), "marketplace.json", &registry_body("m", plugins));

    let registry = parse_registry(tmp.path()).unwrap();

    assert_eq!(registry.entries.len(), 7);
    assert!(registry.entries.iter().all(|entry| entry.available));
    let sources: Vec<PluginSource> = registry
        .entries
        .iter()
        .map(|entry| entry.source.clone())
        .collect();
    assert_eq!(
        sources,
        vec![
            PluginSource::Path {
                path: "./p".to_string()
            },
            PluginSource::Github {
                repo: "o/r".to_string(),
                path: Some("sub".to_string()),
                git_ref: Some("main".to_string()),
                sha: Some("abc123".to_string()),
            },
            PluginSource::Git {
                url: "https://example.com/r.git".to_string(),
                path: None,
                git_ref: None,
                sha: None,
            },
            PluginSource::GitSubdir {
                url: "https://example.com/r.git".to_string(),
                path: "sub/dir".to_string(),
                git_ref: None,
                sha: None,
            },
            PluginSource::Path {
                path: "dir".to_string()
            },
            PluginSource::Path {
                path: "f.json".to_string()
            },
            PluginSource::Url {
                url: "https://example.com/registry.json".to_string()
            },
        ]
    );
}

#[test]
fn unsupported_source_kinds_are_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = r#"
        {"name": "a", "source": {"source": "npm", "package": "x"}},
        {"name": "b", "source": {"source": "archive", "url": "https://example.com/a.tar.gz"}},
        {"name": "c", "source": {"source": "command", "command": "install-me"}}
    "#;
    write(tmp.path(), "marketplace.json", &registry_body("m", plugins));

    let registry = parse_registry(tmp.path()).unwrap();

    assert_eq!(registry.entries.len(), 3);
    for (entry, kind) in registry.entries.iter().zip(["npm", "archive", "command"]) {
        assert!(!entry.available, "{kind} must be unavailable");
        let reason = entry.reason.as_deref().unwrap_or_default();
        assert!(reason.contains(kind), "reason {reason:?} must name {kind}");
        assert!(
            matches!(&entry.source, PluginSource::Unsupported { kind: found, .. } if found.as_str() == kind)
        );
    }
}

#[test]
fn unknown_entry_fields_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "marketplace.json",
        r#"{"name": "m", "futureTop": true, "plugins": [
            {"name": "x", "source": "./p", "futureField": {"nested": [1, 2, 3]}}
        ]}"#,
    );

    let registry = parse_registry(tmp.path()).unwrap();

    assert_eq!(registry.entries.len(), 1);
    let entry = &registry.entries[0];
    assert_eq!(entry.name, "x");
    assert!(entry.available);
    assert_eq!(
        entry.source,
        PluginSource::Path {
            path: "./p".to_string()
        }
    );
}

#[test]
fn shorthand_owner_repo_marketplace_source() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "marketplace.json",
        &registry_body("m", r#"{"name": "x", "source": "owner/repo"}"#),
    );

    let registry = parse_registry(tmp.path()).unwrap();

    let entry = &registry.entries[0];
    assert!(!entry.available, "owner/repo is not a plugin source form");

    let shorthand = parse_source(&serde_json::json!("owner/repo"), true).unwrap();
    assert_eq!(
        shorthand,
        PluginSource::Github {
            repo: "owner/repo".to_string(),
            path: None,
            git_ref: None,
            sha: None,
        }
    );
    let bare_url = parse_source(&serde_json::json!("https://example.com/o/r.git"), true).unwrap();
    assert_eq!(
        bare_url,
        PluginSource::Git {
            url: "https://example.com/o/r.git".to_string(),
            path: None,
            git_ref: None,
            sha: None,
        }
    );
}

#[test]
fn plugin_source_round_trips_for_persistence() {
    let github = PluginSource::Github {
        repo: "o/r".to_string(),
        path: Some("sub".to_string()),
        git_ref: Some("main".to_string()),
        sha: Some("abc123".to_string()),
    };
    let json = serde_json::to_value(&github).unwrap();
    assert_eq!(json["kind"], "github");
    assert_eq!(json["ref"], "main");
    assert!(json.get("git_ref").is_none());
    assert_eq!(
        serde_json::from_value::<PluginSource>(json).unwrap(),
        github
    );

    let subdir = PluginSource::GitSubdir {
        url: "https://example.com/r.git".to_string(),
        path: "sub/dir".to_string(),
        git_ref: None,
        sha: None,
    };
    let json = serde_json::to_value(&subdir).unwrap();
    assert_eq!(json["kind"], "git-subdir");
    assert_eq!(
        serde_json::from_value::<PluginSource>(json).unwrap(),
        subdir
    );

    let unsupported = PluginSource::Unsupported {
        kind: "npm".to_string(),
        detail: "unsupported source kind: npm".to_string(),
    };
    let json = serde_json::to_value(&unsupported).unwrap();
    assert_eq!(json["kind"], "unsupported");
    assert_eq!(json["sourceKind"], "npm");
    assert_eq!(
        serde_json::from_value::<PluginSource>(json).unwrap(),
        unsupported
    );
}

// ---------------------------------------------------------------------------
// Marketplace fetch, refresh and records (design §5)
// ---------------------------------------------------------------------------

/// One recorded conditional GET from [`FakeHttp`].
type HttpCall = (String, Option<String>, Option<String>);

/// A scripted [`HttpClient`]: responses are handed out in order.
struct FakeHttp {
    responses: Mutex<Vec<HttpResponse>>,
    calls: Mutex<Vec<HttpCall>>,
}

impl FakeHttp {
    fn new(responses: Vec<HttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<HttpCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl HttpClient for FakeHttp {
    async fn get_conditional(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<HttpResponse, String> {
        self.calls.lock().unwrap().push((
            url.to_string(),
            etag.map(str::to_string),
            last_modified.map(str::to_string),
        ));
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err("FakeHttp has no response left".to_string());
        }
        Ok(responses.remove(0))
    }
}

/// Run git in `cwd`, failing the test on a non-zero exit.
fn git(cwd: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `git init` a throwaway repo that does not use the developer's signing config.
fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "ducky-test@example.com"]);
    git(dir, &["config", "user.name", "Ducky Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// A real repository at `dir` with one commit holding `marketplace.json`.
fn git_registry_repo(dir: &Path, plugins: &str) -> String {
    git_init(dir);
    write(
        dir,
        "marketplace.json",
        &registry_body("git-marketplace", plugins),
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", "registry"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A `file://` clone URL for a local repository.
fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

/// A record with every optional field empty.
fn marketplace_record(id: &str, source: PluginSource) -> MarketplaceRecord {
    MarketplaceRecord {
        id: id.to_string(),
        name: id.to_string(),
        source,
        registry_path: String::new(),
        auto_refresh: true,
        last_refreshed_at: None,
        resolved_sha: None,
        bundled: false,
        hidden: false,
        error: None,
        extra: Default::default(),
    }
}

/// A `Git` source pointing at a real local repository.
fn git_source(path: &Path) -> PluginSource {
    PluginSource::Git {
        url: file_url(path),
        path: None,
        git_ref: None,
        sha: None,
    }
}

/// A `Url` source for the fake HTTP client.
fn url_source(url: &str) -> PluginSource {
    PluginSource::Url {
        url: url.to_string(),
    }
}

/// A registry fixture with one plugin entry.
fn one_entry() -> String {
    r#"{"name": "one", "source": "./one"}"#.to_string()
}

#[test]
fn parse_git_remote_splits_ref() {
    assert_eq!(
        parse_git_remote("owner/repo"),
        ("owner/repo".to_string(), None)
    );
    assert_eq!(
        parse_git_remote("owner/repo#main"),
        ("owner/repo".to_string(), Some("main".to_string()))
    );
    assert_eq!(
        parse_git_remote("https://example.com/r.git#v1"),
        (
            "https://example.com/r.git".to_string(),
            Some("v1".to_string())
        )
    );
    assert_eq!(
        parse_git_remote("https://example.com/r.git"),
        ("https://example.com/r.git".to_string(), None)
    );
    assert_eq!(
        parse_git_remote("https://example.com/r.git#"),
        ("https://example.com/r.git".to_string(), None)
    );
}

#[tokio::test]
async fn git_refresh_records_resolved_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    let sha = git_registry_repo(&source, &one_entry());
    let dir = tmp.path().join("marketplaces");
    let mut rec = marketplace_record("git-marketplace", git_source(&source));
    let http = FakeHttp::new(Vec::new());

    let outcome = refresh(&mut rec, &dir, &http).await.unwrap();

    assert_eq!(outcome.change, Change::Updated);
    assert_eq!(outcome.entry_count, 1);
    let resolved = rec.resolved_sha.clone().expect("resolved_sha recorded");
    assert_eq!(resolved, sha);
    assert_eq!(resolved.len(), 40);
    assert!(resolved.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(rec.error, None);
    assert!(rec.last_refreshed_at.is_some());
    assert_eq!(rec.registry_path, "marketplace.json");
    assert!(dir.join("git-marketplace/repo/marketplace.json").is_file());

    let mut store = MarketplaceStore::default();
    store.records.push(rec.clone());
    store.save(&dir).unwrap();
    let loaded = MarketplaceStore::load(&dir);
    assert_eq!(loaded.records.len(), 1);
    assert_eq!(loaded.records[0].resolved_sha, Some(sha));
}

#[tokio::test]
async fn git_refresh_second_time_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    git_registry_repo(&source, &one_entry());
    let dir = tmp.path().join("marketplaces");
    let mut rec = marketplace_record("git-marketplace", git_source(&source));
    let http = FakeHttp::new(Vec::new());

    let first = refresh(&mut rec, &dir, &http).await.unwrap();
    assert_eq!(first.change, Change::Updated);

    let second = refresh(&mut rec, &dir, &http).await.unwrap();

    assert_eq!(second.change, Change::Unchanged);
    assert_eq!(second.resolved_sha, first.resolved_sha);
    assert_eq!(second.entry_count, 1);
}

#[tokio::test]
async fn git_refresh_failure_sets_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("gone");
    let dir = tmp.path().join("marketplaces");
    let mut rec = marketplace_record("missing", git_source(&missing));
    let http = FakeHttp::new(Vec::new());

    let err = refresh(&mut rec, &dir, &http).await.unwrap_err();

    assert!(!err.is_empty());
    assert_eq!(rec.error.as_deref(), Some(err.as_str()));
    assert_eq!(rec.last_refreshed_at, None);
    assert_eq!(rec.resolved_sha, None);
    assert!(
        !dir.join("missing/repo").exists(),
        "a failed clone leaves no half clone"
    );
}

#[tokio::test]
async fn git_refresh_bad_commit_keeps_previous_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let body = registry_body("git-marketplace", &one_entry());
    git_registry_repo(&source, &one_entry());
    let dir = tmp.path().join("marketplaces");
    let mut rec = marketplace_record("git-marketplace", git_source(&source));
    let http = FakeHttp::new(Vec::new());
    refresh(&mut rec, &dir, &http).await.unwrap();
    let sha = rec.resolved_sha.clone().expect("first sha");
    let refreshed_at = rec.last_refreshed_at.clone();

    write(&source, "marketplace.json", "{not json");
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-m", "broken registry"]);

    let err = refresh(&mut rec, &dir, &http).await.unwrap_err();

    assert!(err.contains("invalid JSON"), "{err}");
    assert_eq!(rec.resolved_sha.as_deref(), Some(sha.as_str()));
    assert_eq!(rec.last_refreshed_at, refreshed_at);
    assert_eq!(
        std::fs::read_to_string(dir.join("git-marketplace/repo/marketplace.json")).unwrap(),
        body
    );
    assert_eq!(
        git(&dir.join("git-marketplace/repo"), &["rev-parse", "HEAD"]),
        sha
    );
}

#[tokio::test]
async fn git_refresh_missing_subdir_keeps_previous_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let body = registry_body("sub-market", &one_entry());
    git_init(&source);
    write(&source, "sub/marketplace.json", &body);
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-m", "registry"]);
    let dir = tmp.path().join("marketplaces");
    let mut rec = marketplace_record(
        "sub-market",
        PluginSource::GitSubdir {
            url: file_url(&source),
            path: "sub".to_string(),
            git_ref: None,
            sha: None,
        },
    );
    let http = FakeHttp::new(Vec::new());
    refresh(&mut rec, &dir, &http).await.unwrap();
    let sha = rec.resolved_sha.clone().expect("first sha");

    std::fs::remove_dir_all(source.join("sub")).unwrap();
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-m", "drop sub"]);

    let err = refresh(&mut rec, &dir, &http).await.unwrap_err();

    assert!(err.contains("missing in this revision"), "{err}");
    assert!(!err.contains("escapes"), "{err}");
    assert!(!err.contains("outside"), "{err}");
    assert_eq!(rec.resolved_sha.as_deref(), Some(sha.as_str()));
    assert_eq!(
        std::fs::read_to_string(dir.join("sub-market/repo/sub/marketplace.json")).unwrap(),
        body
    );
}

#[tokio::test]
async fn git_refresh_first_clone_uses_blob_none_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    git_registry_repo(&source, &one_entry());
    let dir = tmp.path().join("marketplaces");
    let mut rec = marketplace_record("filtered", git_source(&source));
    let http = FakeHttp::new(Vec::new());

    refresh(&mut rec, &dir, &http).await.unwrap();

    let out = std::process::Command::new("git")
        .current_dir(dir.join("filtered/repo"))
        .args(["config", "--get", "remote.origin.partialclonefilter"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "blob:none",
        "stderr={}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

#[tokio::test]
async fn url_refresh_stores_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let body = registry_body("remote", &one_entry());
    let http = FakeHttp::new(vec![HttpResponse {
        status: 200,
        body: Some(body.clone()),
        etag: Some("\"v1\"".to_string()),
        last_modified: Some("Mon, 01 Oct 2026 00:00:00 GMT".to_string()),
    }]);
    let mut rec = marketplace_record("remote", url_source("https://example.com/marketplace.json"));

    let outcome = refresh(&mut rec, tmp.path(), &http).await.unwrap();

    assert_eq!(outcome.change, Change::Updated);
    assert_eq!(outcome.entry_count, 1);
    assert_eq!(outcome.resolved_sha, None);
    assert_eq!(rec.error, None);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("remote/registry.json")).unwrap(),
        body
    );
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join("remote/registry.meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["etag"], "\"v1\"");
    assert_eq!(meta["lastModified"], "Mon, 01 Oct 2026 00:00:00 GMT");
    assert_eq!(http.calls()[0].1, None);
}

#[tokio::test]
async fn url_refresh_304_keeps_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let body = registry_body("remote", &one_entry());
    let http = FakeHttp::new(vec![
        HttpResponse {
            status: 200,
            body: Some(body.clone()),
            etag: Some("\"v1\"".to_string()),
            last_modified: Some("Mon, 01 Oct 2026 00:00:00 GMT".to_string()),
        },
        HttpResponse {
            status: 304,
            body: None,
            etag: None,
            last_modified: None,
        },
    ]);
    let mut rec = marketplace_record("remote", url_source("https://example.com/marketplace.json"));
    refresh(&mut rec, tmp.path(), &http).await.unwrap();

    let outcome = refresh(&mut rec, tmp.path(), &http).await.unwrap();

    assert_eq!(outcome.change, Change::Unchanged);
    assert_eq!(outcome.entry_count, 1);
    assert_eq!(rec.error, None);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("remote/registry.json")).unwrap(),
        body
    );
    assert_eq!(http.calls()[1].1.as_deref(), Some("\"v1\""));
    assert_eq!(
        http.calls()[1].2.as_deref(),
        Some("Mon, 01 Oct 2026 00:00:00 GMT")
    );
}

#[tokio::test]
async fn url_refresh_success_clears_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rec = marketplace_record("remote", url_source("https://example.com/marketplace.json"));
    let failing = FakeHttp::new(vec![HttpResponse {
        status: 500,
        body: None,
        etag: None,
        last_modified: None,
    }]);
    refresh(&mut rec, tmp.path(), &failing).await.unwrap_err();
    assert!(rec.error.is_some());

    let http = FakeHttp::new(vec![HttpResponse {
        status: 200,
        body: Some(registry_body("remote", &one_entry())),
        etag: None,
        last_modified: None,
    }]);
    refresh(&mut rec, tmp.path(), &http).await.unwrap();

    assert_eq!(rec.error, None);
}

#[tokio::test]
async fn url_refresh_failure_keeps_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let body = registry_body("remote", &one_entry());
    let http = FakeHttp::new(vec![HttpResponse {
        status: 200,
        body: Some(body.clone()),
        etag: Some("\"v1\"".to_string()),
        last_modified: None,
    }]);
    let mut rec = marketplace_record("remote", url_source("https://example.com/marketplace.json"));
    refresh(&mut rec, tmp.path(), &http).await.unwrap();
    let before = rec.clone();
    let failing = FakeHttp::new(vec![HttpResponse {
        status: 500,
        body: None,
        etag: None,
        last_modified: None,
    }]);

    let err = refresh(&mut rec, tmp.path(), &failing).await.unwrap_err();

    assert!(err.contains("500"), "unexpected error: {err}");
    assert_eq!(rec.error.as_deref(), Some(err.as_str()));
    assert_eq!(rec.last_refreshed_at, before.last_refreshed_at);
    assert_eq!(rec.resolved_sha, before.resolved_sha);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("remote/registry.json")).unwrap(),
        body
    );
}

#[tokio::test]
async fn path_source_rereads() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("local");
    write(
        &source,
        "marketplace.json",
        &registry_body("local", &one_entry()),
    );
    let mut rec = marketplace_record(
        "local",
        PluginSource::Path {
            path: source.to_string_lossy().to_string(),
        },
    );
    assert!(rec.auto_refresh);
    let http = FakeHttp::new(Vec::new());

    let first = refresh(&mut rec, tmp.path(), &http).await.unwrap();

    assert_eq!(first.change, Change::Updated);
    assert_eq!(first.entry_count, 1);
    assert_eq!(rec.registry_path, "marketplace.json");
    assert_eq!(rec.resolved_sha, None);
    assert!(
        !rec.auto_refresh,
        "local marketplaces are never auto-refreshed"
    );
    assert!(http.calls().is_empty(), "local sources do not use HTTP");

    write(
        &source,
        "marketplace.json",
        &registry_body(
            "local",
            &format!(
                "{}, {}",
                one_entry(),
                r#"{"name": "two", "source": "./two"}"#
            ),
        ),
    );
    let second = refresh(&mut rec, tmp.path(), &http).await.unwrap();

    assert_eq!(second.entry_count, 2);
}

#[test]
fn records_survive_unknown_fields() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("marketplaces.json"),
        r#"{
          "version": 1,
          "futureStoreField": {"nested": true},
          "marketplaces": [{
            "id": "acme",
            "name": "Acme",
            "source": {"kind": "url", "url": "https://example.com/marketplace.json"},
            "registryPath": "marketplace.json",
            "autoRefresh": true,
            "lastRefreshedAt": null,
            "resolvedSha": null,
            "bundled": false,
            "hidden": false,
            "error": null,
            "futureField": {"x": [1, 2, 3]}
          }]
        }"#,
    )
    .unwrap();

    let store = MarketplaceStore::load(tmp.path());

    assert_eq!(store.records.len(), 1);
    assert_eq!(
        store.records[0].source,
        url_source("https://example.com/marketplace.json")
    );
    assert_eq!(store.records[0].extra["futureField"]["x"][1], 2);
    assert_eq!(store.extra["futureStoreField"]["nested"], true);

    store.save(tmp.path()).unwrap();

    let saved: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join("marketplaces.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["futureStoreField"]["nested"], true);
    assert_eq!(saved["marketplaces"][0]["futureField"]["x"][2], 3);
    assert_eq!(saved["marketplaces"][0]["source"]["kind"], "url");
    assert_eq!(saved["marketplaces"][0]["registryPath"], "marketplace.json");
}

#[test]
fn records_missing_refresh_fields_still_load() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("marketplaces.json"),
        r#"{
          "version": 1,
          "marketplaces": [{
            "id": "acme",
            "name": "Acme",
            "source": {"kind": "url", "url": "https://example.com/marketplace.json"}
          }]
        }"#,
    )
    .unwrap();

    let store = MarketplaceStore::load(tmp.path());

    assert_eq!(
        store.records.len(),
        1,
        "missing refresh fields must not drop the store"
    );
    assert_eq!(store.records[0].id, "acme");
    assert_eq!(store.records[0].last_refreshed_at, None);
    assert_eq!(store.records[0].resolved_sha, None);
    assert_eq!(store.records[0].error, None);
}

#[test]
fn save_is_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    MarketplaceStore::default().save(&dir).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let unparsable = Arc::new(AtomicUsize::new(0));
    let reader = {
        let dir = dir.clone();
        let stop = Arc::clone(&stop);
        let unparsable = Arc::clone(&unparsable);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(text) = std::fs::read_to_string(dir.join("marketplaces.json")) {
                    if serde_json::from_str::<serde_json::Value>(&text).is_err() {
                        unparsable.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
    };

    let writers: Vec<_> = (0..4)
        .map(|w| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                for n in 0..50 {
                    let mut store = MarketplaceStore::default();
                    store.records.push(marketplace_record(
                        &format!("m{w}-{n}"),
                        url_source("https://example.com/marketplace.json"),
                    ));
                    store.save(&dir).unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    reader.join().unwrap();

    assert_eq!(
        unparsable.load(Ordering::Relaxed),
        0,
        "a concurrent save left an unparsable file"
    );
    let text = std::fs::read_to_string(dir.join("marketplaces.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&text).expect("final file parses");
}
