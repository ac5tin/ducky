//! Plugin manifest, path containment, layout and marketplace tests
//! (design §1–§2, §5, §13).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::diagnostics::{DiagLevel, Diagnostic};
use super::install::{
    assign_id, derive_policy, fetch_to_staging, install, package_dir, resolve_version, tree_hash,
    uninstall, AvailableUpdate, InstallRecord, InstallStore, PluginStatus, UpdatePolicy,
};
use super::layout::{discover, to_subagent_config, PluginSubagent, PluginTransport, RemoteKind};
use super::manager::{display_host, MarketplaceInput, PluginIndex, PluginManager};
use super::manifest::{load, Layout, PluginManifest};
use super::marketplace::{
    parse_git_remote, parse_registry, parse_source, refresh, Change, Entry, HttpClient,
    HttpResponse, MarketplaceRecord, MarketplaceStore, PluginSource, Registry,
};
use super::skills::{collect, load_body, prompt_block, summarize, ResolvedSkill, SkillOrigin};
use super::update::{apply, check_one, rollback};
use crate::config::{EffortLevel, PluginSettings, PolicyDefault};
use crate::events::{BackendEvent, CollectingSink};
use crate::mcp::manager::{merge_servers, plugin_spawn_spec, spawn_spec};

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

/// A plugin package under construction. The constructors write the manifest
/// into one of the three probe locations (design §1); the component methods
/// write the fixed locations (design §2). Every fixture in this suite goes
/// through this builder, so a layout change is one edit.
struct Conformant {
    root: PathBuf,
}

impl Conformant {
    /// The Agent Plugins manifest body the builder writes. `extra` is
    /// appended inside the JSON object, so a test can extend or break it.
    fn agent_json(name: &str, extra: &str) -> String {
        format!(r#"{{"$schema": "{SPEC_100}", "name": "{name}"{extra}}}"#)
    }

    /// A spec-layout package: root `plugin.json` with the required `$schema`,
    /// plus whichever fixed component locations the test adds.
    fn agent(root: &Path, name: &str, extra: &str) -> Self {
        Self::raw(root, &Self::agent_json(name, extra))
    }

    /// A package whose `plugin.json` is exactly `body` (invalid fixtures).
    fn raw(root: &Path, body: &str) -> Self {
        write(root, "plugin.json", body);
        Self::bare(root)
    }

    /// A package with no manifest yet, for tests that write or link it.
    fn bare(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// `.claude-plugin/plugin.json`, exactly `body`.
    fn claude(root: &Path, body: &str) -> Self {
        write(root, ".claude-plugin/plugin.json", body);
        Self::bare(root)
    }

    /// `.zcode-plugin/plugin.json`, exactly `body`.
    fn zcode(root: &Path, body: &str) -> Self {
        write(root, ".zcode-plugin/plugin.json", body);
        Self::bare(root)
    }

    /// `skills/<dir>/SKILL.md` with the standard frontmatter.
    fn skill(self, dir: &str, name: &str) -> Self {
        write_skill(&self.root, &format!("skills/{dir}"), name);
        self
    }

    /// `skills/<dir>/SKILL.md` with a caller-written body.
    fn skill_file(self, dir: &str, body: &str) -> Self {
        write(&self.root, &format!("skills/{dir}/SKILL.md"), body);
        self
    }

    /// `mcp.json` with the required `$schema` and the given servers object.
    fn mcp(self, servers: &str) -> Self {
        self.mcp_file(
            "mcp.json",
            &format!(r#"{{"$schema": "{SPEC_100}", "mcpServers": {servers}}}"#),
        )
    }

    /// An MCP file at a caller-chosen path (`mcp.json` for the spec layout,
    /// `.mcp.json` for the Claude layout).
    fn mcp_file(self, rel: &str, body: &str) -> Self {
        write(&self.root, rel, body);
        self
    }

    /// `mcp.json` exactly as given, for schema-mismatch fixtures.
    fn mcp_json(self, body: &str) -> Self {
        self.mcp_file("mcp.json", body)
    }

    fn load(&self) -> PluginManifest {
        load(&self.root).unwrap_or_else(|diagnostics| panic!("manifest must load: {diagnostics:?}"))
    }

    fn load_err(&self) -> Vec<Diagnostic> {
        load(&self.root).expect_err("manifest must be rejected")
    }

    fn discover(&self) -> super::layout::Discovered {
        let manifest = self.load();
        discover(&self.root, &manifest)
    }
}

/// An Agent Plugins plugin with optional `mcpServers` JSON, discovered. A
/// wrapper over [`Conformant`], so no test builds the package layout itself.
fn discover_agent(root: &Path, servers: Option<&str>) -> super::layout::Discovered {
    let package = Conformant::agent(root, "demo-plugin", "");
    let package = match servers {
        Some(servers) => package.mcp(servers),
        None => package,
    };
    package.discover()
}

#[test]
fn loads_minimal_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = Conformant::agent(tmp.path(), "minimal-plugin", "").load();

    assert_eq!(manifest.name, "minimal-plugin");
    assert_eq!(manifest.layout, Layout::AgentPlugins);
    assert!(manifest.extensions.is_empty());
    assert!(manifest.unsupported.is_empty());
}

#[test]
fn rejects_missing_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let diagnostics = Conformant::raw(tmp.path(), r#"{"name": "no-schema"}"#).load_err();

    assert!(diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Error && d.target == "plugin.json"));
}

#[test]
fn rejects_unknown_schema_version() {
    let tmp = tempfile::tempdir().unwrap();
    let body = r#"{"$schema": "https://agent-plugins.org/schemas/2.0.0/plugin.schema.json", "name": "future"}"#;
    let diagnostics = Conformant::raw(tmp.path(), body).load_err();

    assert!(diagnostics
        .iter()
        .any(|d| d.message.contains("unsupported")));
}

#[test]
fn rejects_bad_name() {
    for name in ["My-Plugin", "-start", "has--double", "a..b", ""] {
        let tmp = tempfile::tempdir().unwrap();
        Conformant::agent(tmp.path(), name, "");
        assert!(
            load(tmp.path()).is_err(),
            "expected {name:?} to be rejected"
        );
    }
}

#[test]
fn unknown_top_level_key_is_non_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = Conformant::agent(tmp.path(), "ok-plugin", r#", "futureField": 1"#).load();

    assert!(manifest
        .unsupported
        .iter()
        .any(|d| d.level == DiagLevel::Warning));
}

#[test]
fn non_object_extensions_is_non_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = Conformant::agent(tmp.path(), "ext-plugin", r#", "extensions": []"#).load();

    assert!(manifest.extensions.is_empty());
    assert!(manifest
        .unsupported
        .iter()
        .any(|d| d.level == DiagLevel::Warning));
}

#[test]
fn bad_author_field_is_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let diagnostics = Conformant::raw(
        tmp.path(),
        &Conformant::agent_json(
            "bad-author",
            r#", "author": {"name": "x", "url": "y", "extra": 1}"#,
        ),
    )
    .load_err();

    assert!(diagnostics.iter().any(|d| d.level == DiagLevel::Error));
}

#[test]
fn non_semver_version_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = Conformant::agent(tmp.path(), "versioned", r#", "version": "banana""#).load();

    assert_eq!(manifest.version.as_deref(), Some("banana"));
}

#[test]
fn invalid_plugin_json_does_not_fall_through() {
    let tmp = tempfile::tempdir().unwrap();
    // Root manifest exists but is invalid...
    Conformant::raw(tmp.path(), r#"{"name": "broken"}"#);
    // ...while a valid compat manifest sits beside it.
    Conformant::claude(tmp.path(), r#"{"name": "valid-legacy"}"#);

    assert!(load(tmp.path()).is_err());
}

#[test]
fn claude_layout_loads_without_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = Conformant::claude(tmp.path(), r#"{"name": "legacy-plugin"}"#).load();

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
    let manifest = Conformant::zcode(tmp.path(), body).load();

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

    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .skill("a", "a")
        .mcp(r#"{"local": {"type": "stdio", "command": "node", "args": ["server.js"]}}"#)
        .discover();

    assert_eq!(found.skills.len(), 1);
    assert_eq!(found.skills[0].name, "a");
    assert_eq!(found.skills[0].description, "demo");
    assert_eq!(found.servers.len(), 1);
    assert!(found.diagnostics.is_empty());
}

#[test]
fn parses_block_style_skill_allowed_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let package = Conformant::agent(tmp.path(), "block-skills", "").skill_file(
        "reviewer",
        "---\nname: reviewer\ndescription: demo\nallowed-tools:\n  - Read\n  - Grep\n---\nBody.",
    );

    let found = package.discover();

    assert_eq!(found.skills.len(), 1, "{:?}", found.diagnostics);
    assert_eq!(found.skills[0].allowed_tools.as_deref(), Some("Read, Grep"));
}

#[test]
fn missing_locations_ok() {
    let tmp = tempfile::tempdir().unwrap();

    let found = Conformant::agent(tmp.path(), "demo-plugin", "").discover();

    assert!(found.skills.is_empty());
    assert!(found.servers.is_empty());
    assert!(found.subagents.is_empty());
    assert!(found.diagnostics.is_empty());
}

#[test]
fn no_recursive_skill_search() {
    let tmp = tempfile::tempdir().unwrap();

    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .skill("group/a", "a")
        .discover();

    assert!(found.skills.is_empty());
    assert!(found.diagnostics.is_empty());
}

#[test]
fn invalid_skill_skipped() {
    let tmp = tempfile::tempdir().unwrap();

    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .skill("a", "b")
        .discover();

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
    Conformant::agent(outside.path(), "escapee", "");
    symlink(
        &outside.path().join("plugin.json"),
        &tmp.path().join("plugin.json"),
    );

    let diagnostics = Conformant::bare(tmp.path()).load_err();

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

    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .mcp(servers)
        .discover();

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

    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .mcp(servers)
        .discover();

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

        let found = Conformant::agent(tmp.path(), "demo-plugin", "")
            .mcp(&servers)
            .discover();

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

        let found = Conformant::agent(tmp.path(), "demo-plugin", "")
            .mcp(&servers)
            .discover();

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
    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .mcp(r#"{"ok": {"type": "stdio", "command": "node", "cwd": "./sub"}}"#)
        .discover();
    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "ok");
}

#[test]
fn mismatched_mcp_schema_disables_mcp_only() {
    let tmp = tempfile::tempdir().unwrap();
    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .skill("a", "a")
        .mcp_json(
            r#"{"$schema": "https://agent-plugins.org/schemas/2.0.0/plugin.schema.json", "mcpServers": {"local": {"type": "stdio", "command": "node"}}}"#,
        )
        .discover();

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

    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .mcp(servers)
        .discover();

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

    let found = Conformant::agent(tmp.path(), "demo-plugin", "")
        .mcp(servers)
        .discover();

    assert!(found.servers.is_empty());
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning && d.target == "s"));
}

#[test]
fn claude_mcp_json_aliases_normalize() {
    let tmp = tempfile::tempdir().unwrap();
    let package = Conformant::claude(tmp.path(), r#"{"name": "legacy"}"#).mcp_file(
        ".mcp.json",
        r#"{"mcpServers": {"local": {"type": "stdio", "command": "node", "cwd": "${CLAUDE_PLUGIN_ROOT}/tools"}}}"#,
    );

    let found = package.discover();

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

        let found = Conformant::agent(tmp.path(), "demo-plugin", "")
            .mcp(&servers)
            .discover();

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

        let found = Conformant::agent(tmp.path(), "demo-plugin", "")
            .mcp(&servers)
            .discover();

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
    let package = Conformant::claude(
        tmp.path(),
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

    let found = package.discover();

    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "inline");
    assert!(found.diagnostics.iter().any(|d| {
        d.target == ".mcp.json"
            && d.message.contains("outside")
            && !d.message.contains("MCP disabled")
    }));
}

// ---------------------------------------------------------------------------
// Conformance (Agent Plugins 1.0.0, design §2 and the mapping table)
//
// One test per mapping-table row, named for the requirement, so a spec
// regression is a named failure. Every package is built through [`Conformant`].
// ---------------------------------------------------------------------------

#[test]
fn conformance_loads_plugin_from_directory_path() {
    let tmp = tempfile::tempdir().unwrap();

    let manifest = Conformant::agent(tmp.path(), "conformance-basic", "").load();

    assert_eq!(manifest.name, "conformance-basic");
    assert_eq!(manifest.layout, Layout::AgentPlugins);
    assert!(manifest.unsupported.is_empty());
}

#[test]
fn conformance_closed_manifest_with_non_fatal_exceptions() {
    // An unknown top-level key is reported and ignored, never fatal.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = Conformant::agent(tmp.path(), "closed-manifest", r#", "futureField": 1"#).load();
    assert!(manifest
        .unsupported
        .iter()
        .any(|d| d.level == DiagLevel::Warning && d.message.contains("futureField")));

    // Everything else in the closed set is fatal to the plugin: a missing or
    // unrecognised `$schema`, a bad name, and an extra key inside `author`.
    let fatal = [
        r#"{"name": "no-schema"}"#.to_string(),
        r#"{"$schema": "https://agent-plugins.org/schemas/2.0.0/plugin.schema.json", "name": "future"}"#
            .to_string(),
        Conformant::agent_json("Bad-Name", ""),
        Conformant::agent_json("bad-author", r#", "author": {"name": "x", "extra": 1}"#),
    ];
    for body in fatal {
        let tmp = tempfile::tempdir().unwrap();
        let diagnostics = Conformant::raw(tmp.path(), &body).load_err();
        assert!(
            diagnostics.iter().any(|d| d.level == DiagLevel::Error),
            "{body} must be rejected: {diagnostics:?}"
        );
    }
}

#[test]
fn conformance_ignores_unimplemented_extensions() {
    let tmp = tempfile::tempdir().unwrap();
    let extension = serde_json::json!({"anything": [1, 2, 3], "nested": {"a": true}});
    let package = Conformant::agent(
        tmp.path(),
        "foreign-extension",
        &format!(r#", "extensions": {{"com.example.other": {extension}}}"#),
    );

    let manifest = package.load();
    let found = package.discover();

    // The member is kept verbatim and never validated or diagnosed.
    assert_eq!(manifest.extensions["com.example.other"], extension);
    assert!(found.diagnostics.is_empty(), "{:?}", found.diagnostics);
    assert!(found.skills.is_empty() && found.servers.is_empty() && found.subagents.is_empty());
}

#[test]
fn conformance_discovers_fixed_component_locations() {
    let tmp = tempfile::tempdir().unwrap();
    let package = Conformant::agent(tmp.path(), "locations", "")
        .skill("alpha", "alpha")
        // A nested SKILL.md is not at a fixed location: no recursive search.
        .skill("group/nested", "nested")
        .mcp(r#"{"local": {"type": "stdio", "command": "node"}}"#);

    let found = package.discover();

    assert_eq!(
        found
            .skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
    assert_eq!(found.servers.len(), 1);
    assert!(found.diagnostics.is_empty(), "{:?}", found.diagnostics);
}

#[test]
fn conformance_supports_all_three_mcp_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let found = Conformant::agent(tmp.path(), "transports", "")
        .mcp(
            r#"{
                "local": {"type": "stdio", "command": "node"},
                "http": {"type": "streamable-http", "url": "https://example.com/mcp"},
                "legacy": {"type": "sse", "url": "https://example.com/sse"}
            }"#,
        )
        .discover();

    assert_eq!(found.servers.len(), 3);
    let kind = |name: &str| {
        found
            .servers
            .iter()
            .find(|s| s.name == name)
            .map(|s| match &s.transport {
                PluginTransport::Stdio { .. } => "stdio",
                PluginTransport::Remote { kind, .. } => match kind {
                    RemoteKind::StreamableHttp => "streamable-http",
                    RemoteKind::Sse => "sse",
                },
            })
    };
    assert_eq!(kind("local"), Some("stdio"));
    assert_eq!(kind("http"), Some("streamable-http"));
    assert_eq!(kind("legacy"), Some("sse"));
    assert!(found.diagnostics.is_empty(), "{:?}", found.diagnostics);
}

#[test]
fn conformance_provides_and_expands_plugin_vars() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("plugin-root");
    let data = tmp.path().join("plugin-data");
    let package = Conformant::agent(&root, "vars", "").mcp(
        r#"{"local": {"type": "stdio", "command": "./bin/tool", "args": ["${PLUGIN_ROOT}/a", "${PLUGIN_DATA}/b"], "env": {"CACHE": "${PLUGIN_DATA}/cache"}}}"#,
    );

    // Parse keeps the placeholders; expansion happens at launch.
    let found = package.discover();
    assert_eq!(found.servers.len(), 1);
    let PluginTransport::Stdio { args, env, .. } = &found.servers[0].transport else {
        panic!("expected a stdio server");
    };
    assert_eq!(
        args,
        &[
            "${PLUGIN_ROOT}/a".to_string(),
            "${PLUGIN_DATA}/b".to_string()
        ]
    );
    assert_eq!(env["CACHE"], "${PLUGIN_DATA}/cache");

    write(&root, "bin/tool", "");
    let spec = spawn_spec("./bin/tool", args, env, None, &root, &data).unwrap();

    assert_eq!(spec.args[0], format!("{}/a", root.display()));
    assert_eq!(spec.args[1], format!("{}/b", data.display()));
    let value = |key: &str| {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let expected_cache = format!("{}/cache", data.display());
    assert_eq!(value("CACHE"), Some(expected_cache.as_str()));
    assert_eq!(value("PLUGIN_ROOT"), Some(root.to_string_lossy().as_ref()));
    assert_eq!(value("PLUGIN_DATA"), Some(data.to_string_lossy().as_ref()));
}

#[test]
fn conformance_command_is_single_token_and_root_is_default_cwd() {
    // `command` is one executable token; a shell string invalidates the server.
    let tmp = tempfile::tempdir().unwrap();
    let found = Conformant::agent(tmp.path(), "command", "")
        .mcp(r#"{"shell": {"type": "stdio", "command": "node -c"}}"#)
        .discover();
    assert!(found.servers.is_empty());
    assert!(found.diagnostics.iter().any(|d| d.target == "shell"));

    // No `cwd` means the plugin root. Driven through discovery and the
    // manager's own launch resolution, so a caller that stops passing the
    // discovered `None` to the spawn fails this test.
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    Conformant::agent(&source, "default-cwd", "")
        .mcp(r#"{"demo": {"type": "stdio", "command": "./bin/tool"}}"#);
    write(&source, "bin/tool", "");
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    let id = manager
        .install(
            None,
            None,
            Some(&path_source(&source)),
            PolicyDefault::Content,
        )
        .unwrap();
    manager.set_enabled(&id, true).unwrap();
    manager.set_server_enabled(&id, "demo", true).unwrap();

    let resolved = merge_servers(&[], &manager.index(), &manager.data_dir);
    let server = resolved
        .iter()
        .find(|server| server.launch.is_some())
        .expect("the enabled server must resolve");
    let launch = server.launch.as_ref().unwrap();
    let crate::config::McpTransport::Stdio { command, args, env } = &server.config.transport else {
        panic!("expected a stdio server");
    };
    let spec = plugin_spawn_spec(command, args, env, launch).unwrap();

    assert!(launch.cwd.is_none());
    assert_eq!(spec.cwd, std::fs::canonicalize(&launch.root).unwrap());
}

#[test]
fn conformance_path_containment_ladder() {
    // A server `cwd` that escapes its root invalidates that server only.
    let tmp = tempfile::tempdir().unwrap();
    let found = Conformant::agent(tmp.path(), "cwd-ladder", "")
        .mcp(r#"{"escapee": {"type": "stdio", "command": "node", "cwd": "./../outside"}}"#)
        .discover();
    assert!(found.servers.is_empty());
    assert!(found.diagnostics.iter().any(|d| d.target == "escapee"));

    #[cfg(unix)]
    {
        // A manifest outside the root rejects the whole plugin.
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        Conformant::agent(outside.path(), "outside-manifest", "");
        symlink(
            &outside.path().join("plugin.json"),
            &tmp.path().join("plugin.json"),
        );
        let diagnostics = Conformant::bare(tmp.path()).load_err();
        assert!(diagnostics.iter().any(|d| d.message.contains("outside")));

        // A component location outside the root invalidates that component
        // type only: the skills still load.
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        Conformant::bare(outside.path()).mcp_file(
            "mcp.json",
            r#"{"mcpServers": {"outside": {"type": "stdio", "command": "node"}}}"#,
        );
        let package = Conformant::agent(tmp.path(), "mcp-ladder", "").skill("kept", "kept");
        symlink(
            &outside.path().join("mcp.json"),
            &tmp.path().join("mcp.json"),
        );

        let found = package.discover();

        assert_eq!(found.skills.len(), 1);
        assert!(found.servers.is_empty());
        assert!(found
            .diagnostics
            .iter()
            .any(|d| d.target == "mcp.json" && d.message.contains("outside")));

        // A SKILL.md outside the root skips that skill, not the plugin.
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_skill(outside.path(), "evil", "evil");
        let package = Conformant::agent(tmp.path(), "skill-ladder", "").skill("good", "good");
        symlink(
            &outside.path().join("evil"),
            &tmp.path().join("skills/evil"),
        );

        let found = package.discover();

        assert_eq!(
            found
                .skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["good"]
        );
        assert!(found
            .diagnostics
            .iter()
            .any(|d| d.target == "skills/evil/SKILL.md" && d.message.contains("outside")));
    }
}

#[test]
fn conformance_missing_locations_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();

    let found = Conformant::agent(tmp.path(), "no-components", "").discover();

    assert!(found.skills.is_empty());
    assert!(found.servers.is_empty());
    assert!(found.subagents.is_empty());
    assert!(found.diagnostics.is_empty(), "{:?}", found.diagnostics);
}

#[test]
fn conformance_per_server_failure_isolation() {
    let tmp = tempfile::tempdir().unwrap();

    let found = Conformant::agent(tmp.path(), "server-isolation", "")
        .mcp(
            r#"{
                "good": {"type": "stdio", "command": "node"},
                "bad": {"type": "stdio", "command": "node", "bogus": 1}
            }"#,
        )
        .discover();

    assert_eq!(found.servers.len(), 1);
    assert_eq!(found.servers[0].name, "good");
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning && d.target == "bad"));
}

#[test]
fn conformance_component_failure_isolation() {
    // A bad mcp.json disables MCP only: the skills still load.
    let tmp = tempfile::tempdir().unwrap();
    let found = Conformant::agent(tmp.path(), "mcp-failure", "")
        .skill("kept", "kept")
        .mcp_json(
            r#"{"$schema": "https://agent-plugins.org/schemas/2.0.0/plugin.schema.json", "mcpServers": {}}"#,
        )
        .discover();
    assert_eq!(found.skills.len(), 1);
    assert!(found.servers.is_empty());
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Warning));

    // A bad skill is skipped: the servers still load.
    let tmp = tempfile::tempdir().unwrap();
    let found = Conformant::agent(tmp.path(), "skill-failure", "")
        .skill("a", "b")
        .mcp(r#"{"kept": {"type": "stdio", "command": "node"}}"#)
        .discover();
    assert!(found.skills.is_empty());
    assert_eq!(found.servers.len(), 1);
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.target == "skills/a/SKILL.md"));
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
async fn git_refresh_escaping_subdir_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    git_registry_repo(&source, &one_entry());
    let dir = tmp.path().join("marketplaces");
    // The path exists, but outside any checkout: containment — not existence —
    // must be what refuses it (`refresh_git` -> `read_git_registry`).
    write(
        &dir.join("escape-market"),
        "outside/marketplace.json",
        &registry_body("outside", &one_entry()),
    );
    let mut rec = marketplace_record(
        "escape-market",
        PluginSource::Git {
            url: file_url(&source),
            path: Some("../outside".to_string()),
            git_ref: None,
            sha: None,
        },
    );
    let http = FakeHttp::new(Vec::new());

    let err = refresh(&mut rec, &dir, &http).await.unwrap_err();

    assert!(err.contains("outside the root"), "{err}");
    assert!(
        rec.resolved_sha.is_none(),
        "a refused refresh records no sha"
    );
    assert!(rec.last_refreshed_at.is_none());
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

// ---------------------------------------------------------------------------
// Install records, staging, version ladder and uninstall (design §3, §6, §7)
// ---------------------------------------------------------------------------

/// A minimal valid local plugin package with one skill.
fn plugin_package(root: &Path, name: &str) -> Conformant {
    Conformant::agent(root, name, "").skill("demo", "demo")
}

/// An install record with every optional field empty.
fn install_record(id: &str, name: &str, source: PluginSource) -> InstallRecord {
    InstallRecord {
        id: id.to_string(),
        name: name.to_string(),
        marketplace: None,
        source,
        version: Some("1.0.0".to_string()),
        installed_at: "2026-10-01T00:00:00Z".to_string(),
        layout: Layout::AgentPlugins,
        enabled: false,
        update_policy: UpdatePolicy::Auto,
        disabled_servers: Vec::new(),
        previous_version: None,
        tree_hash: None,
        last_checked_at: None,
        available_update: None,
        status: PluginStatus::InstalledDisabled,
        diagnostics: Vec::new(),
        resolved_sha: None,
        resolved_sha256: None,
        extra: Default::default(),
    }
}

/// A `Path` source for a local package, installed by copy.
fn path_source(path: &Path) -> PluginSource {
    PluginSource::Path {
        path: path.display().to_string(),
    }
}

#[test]
fn ladder_prefers_manifest_version() {
    let sha = "9f2c8b1d4e6a77c3e5f0b1a2c3d4e5f60718293a";

    assert_eq!(
        resolve_version(Some("1.2.0"), Some("1.3.0"), Some(sha), None),
        Some("1.2.0".to_string())
    );
}

#[test]
fn ladder_falls_back_to_entry() {
    let sha = "9f2c8b1d4e6a77c3e5f0b1a2c3d4e5f60718293a";

    assert_eq!(
        resolve_version(None, Some("1.3.0"), Some(sha), None),
        Some("1.3.0".to_string())
    );
}

#[test]
fn ladder_falls_back_to_sha12() {
    let sha = "9f2c8b1d4e6a77c3e5f0b1a2c3d4e5f60718293a";
    let sha256 = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f80912";

    assert_eq!(
        resolve_version(None, None, Some(sha), None),
        Some(sha[..12].to_string())
    );
    assert_eq!(
        resolve_version(None, None, None, Some(sha256)),
        Some(sha256[..12].to_string())
    );
}

#[test]
fn ladder_unknown_last() {
    assert_eq!(
        resolve_version(None, None, None, None),
        Some("unknown".to_string())
    );
}

#[test]
fn id_collision_appends_fingerprint() {
    let fingerprint = "9f2c8b1d4e6a77c3e5f0b1a2c3d4e5f60718293a";

    let free = assign_id("acme", Layout::AgentPlugins, &[], fingerprint);
    assert_eq!(free, "acme");

    // Two records named `acme` from different sources: the second id carries
    // the first 6 hex characters of the resolved source fingerprint.
    let existing = vec![install_record(
        "acme",
        "acme",
        PluginSource::Path {
            path: "/tmp/one".to_string(),
        },
    )];
    let collision = assign_id("acme", Layout::AgentPlugins, &existing, fingerprint);
    assert_eq!(collision, format!("acme-{}", &fingerprint[..6]));
}

#[test]
fn policy_is_auto_for_skills_only() {
    let tmp = tempfile::tempdir().unwrap();
    Conformant::agent(tmp.path(), "skills-only", "").skill("demo", "demo");

    let manifest = load(tmp.path()).unwrap();
    let discovered = discover(tmp.path(), &manifest);

    assert_eq!(discovered.servers.len() + discovered.subagents.len(), 0);
    assert_eq!(derive_policy(&discovered), UpdatePolicy::Auto);
}

#[test]
fn policy_is_manual_with_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let discovered = discover_agent(
        tmp.path(),
        Some(r#"{"local": {"type": "stdio", "command": "node"}}"#),
    );

    assert_eq!(discovered.servers.len(), 1);
    assert_eq!(derive_policy(&discovered), UpdatePolicy::Manual);
}

#[test]
fn policy_is_manual_with_extensions() {
    let tmp = tempfile::tempdir().unwrap();
    Conformant::agent(tmp.path(), "with-subagent", "");
    write(
        tmp.path(),
        "app.ducky/subagents/reviewer.md",
        "---\nname: reviewer\ndescription: reviews code\n---\nBody.",
    );

    let manifest = load(tmp.path()).unwrap();
    let discovered = discover(tmp.path(), &manifest);

    assert_eq!(discovered.subagents.len(), 1);
    assert_eq!(derive_policy(&discovered), UpdatePolicy::Manual);
}

// --- Plugin subagents (design §10, task 10) ---

/// A plugin subagent as discovery hands it to the mapper, with sane defaults.
fn plugin_subagent(name: &str) -> PluginSubagent {
    PluginSubagent {
        plugin_id: "acme".into(),
        name: name.into(),
        description: "reviews code".into(),
        system_prompt: "Review.".into(),
        model: None,
        effort: None,
        tools: None,
        source_path: std::path::PathBuf::from("subagents/reviewer.md"),
    }
}

/// The live builtin registry's names, as `to_subagent_config` expects them.
fn builtin_tool_names() -> Vec<String> {
    crate::builtin::tools()
        .iter()
        .map(|t| t.name.to_string())
        .collect()
}

#[test]
fn parses_app_ducky_subagents_dir() {
    let tmp = tempfile::tempdir().unwrap();
    Conformant::agent(tmp.path(), "with-subagent", "");
    write(
        tmp.path(),
        "app.ducky/subagents/reviewer.md",
        "---\nname: reviewer\ndescription: reviews code\nmodel: claude-sonnet-4\neffort: high\ntools: ducky__fs_read, ducky__fs_search\n---\nYou review code.",
    );

    let manifest = load(tmp.path()).unwrap();
    let found = discover(tmp.path(), &manifest);

    assert_eq!(found.subagents.len(), 1, "{:?}", found.diagnostics);
    let sub = &found.subagents[0];
    assert_eq!(sub.name, "reviewer");
    assert_eq!(sub.description, "reviews code");
    assert_eq!(sub.system_prompt, "You review code.");
    assert_eq!(sub.model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(sub.effort, Some(EffortLevel::High));
    assert_eq!(
        sub.tools,
        Some(vec![
            "ducky__fs_read".to_string(),
            "ducky__fs_search".to_string()
        ])
    );
    assert!(
        sub.source_path.ends_with("app.ducky/subagents/reviewer.md"),
        "{:?}",
        sub.source_path
    );
}

#[test]
fn parses_block_style_tool_list() {
    let tmp = tempfile::tempdir().unwrap();
    Conformant::agent(tmp.path(), "block-tools", "");
    write(
        tmp.path(),
        "app.ducky/subagents/reviewer.md",
        "---\nname: reviewer\ndescription: reviews code\ntools:\n    - ducky__fs_read\n    - ducky__fs_search\n---\nReview.",
    );

    let manifest = load(tmp.path()).unwrap();
    let found = discover(tmp.path(), &manifest);

    assert_eq!(found.subagents.len(), 1, "{:?}", found.diagnostics);
    assert_eq!(
        found.subagents[0].tools,
        Some(vec![
            "ducky__fs_read".to_string(),
            "ducky__fs_search".to_string()
        ])
    );
}

#[test]
fn block_list_skips_empty_items_and_stays_present() {
    let (frontmatter, _) =
        super::layout::parse_frontmatter("---\ntools:\n  - \n  - ducky__fs_read\n---\nBody")
            .unwrap();
    assert_eq!(
        frontmatter.get("tools").map(String::as_str),
        Some("ducky__fs_read")
    );

    let (empty, _) = super::layout::parse_frontmatter("---\ntools:\n  - \n---\nBody").unwrap();
    assert_eq!(
        empty.get("tools").map(String::as_str),
        Some(""),
        "an empty list is present-but-empty, never absent"
    );
}

#[test]
fn parses_inline_extension_subagents() {
    let tmp = tempfile::tempdir().unwrap();
    Conformant::agent(
        tmp.path(),
        "inline-subagents",
        r#", "extensions": {"app.ducky": {"subagents": [{"name": "reviewer", "description": "reviews code", "system_prompt": "Review.", "effort": "low", "tools": ["ducky__fs_read"]}]}}"#,
    );

    let manifest = load(tmp.path()).unwrap();
    let found = discover(tmp.path(), &manifest);

    assert_eq!(found.subagents.len(), 1, "{:?}", found.diagnostics);
    let sub = &found.subagents[0];
    assert_eq!(sub.name, "reviewer");
    assert_eq!(sub.description, "reviews code");
    assert_eq!(sub.system_prompt, "Review.");
    assert_eq!(sub.effort, Some(EffortLevel::Low));
    assert_eq!(sub.tools, Some(vec!["ducky__fs_read".to_string()]));
    assert!(
        sub.source_path.as_os_str().is_empty(),
        "inline subagents have no file: {:?}",
        sub.source_path
    );
}

#[test]
fn parses_claude_agents_md() {
    let tmp = tempfile::tempdir().unwrap();
    let package = Conformant::claude(tmp.path(), r#"{"name": "legacy-plugin"}"#);
    write(
        tmp.path(),
        "agents/reviewer.md",
        "---\nname: reviewer\ndescription: reviews code\ntools: Read, Grep\n---\nReview.",
    );

    let manifest = package.load();
    assert_eq!(manifest.layout, Layout::ClaudeCode);
    let found = package.discover();

    assert_eq!(found.subagents.len(), 1, "{:?}", found.diagnostics);
    assert_eq!(
        found.subagents[0].tools,
        Some(vec!["Read".to_string(), "Grep".to_string()])
    );
}

#[test]
fn unknown_tool_dropped_with_diagnostic() {
    let mut sub = plugin_subagent("reviewer");
    sub.tools = Some(vec!["ducky__nope".into(), "ducky__fs_read".into()]);

    let (def, diagnostics) = to_subagent_config(&sub, &builtin_tool_names(), &[]);

    assert_eq!(def.tools, Some(vec!["ducky__fs_read".to_string()]));
    assert!(
        diagnostics
            .iter()
            .any(|d| { d.level == DiagLevel::Warning && d.message.contains("ducky__nope") }),
        "{diagnostics:?}"
    );
}

#[test]
fn empty_after_filter_means_no_tools() {
    let mut sub = plugin_subagent("reviewer");
    sub.tools = Some(vec!["ducky__nope".into()]);

    let (def, _diagnostics) = to_subagent_config(&sub, &builtin_tool_names(), &[]);

    assert_eq!(def.tools, Some(Vec::new()), "empty must not become None");
}

#[test]
fn control_tools_are_refused() {
    let mut sub = plugin_subagent("reviewer");
    sub.tools = Some(vec![
        crate::builtin::PRESENT_PLAN.to_string(),
        crate::builtin::SET_MODE.to_string(),
        "ducky__fs_read".to_string(),
    ]);

    let (def, diagnostics) = to_subagent_config(&sub, &builtin_tool_names(), &[]);

    assert_eq!(def.tools, Some(vec!["ducky__fs_read".to_string()]));
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains(crate::builtin::PRESENT_PLAN)),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains(crate::builtin::SET_MODE)),
        "{diagnostics:?}"
    );
}

#[test]
fn id_is_namespaced() {
    let (def, diagnostics) =
        to_subagent_config(&plugin_subagent("reviewer"), &builtin_tool_names(), &[]);
    assert_eq!(def.id, "plugin:acme:reviewer");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let mut spaced = plugin_subagent("Code Reviewer");
    spaced.plugin_id = "acme/tools".into();
    let (def, _diagnostics) = to_subagent_config(&spaced, &builtin_tool_names(), &[]);
    assert_eq!(def.id, "plugin:acme/tools:code-reviewer");
    assert_eq!(def.name, "code-reviewer");
}

#[test]
fn install_writes_record_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let plugins = tmp.path().join("plugins");

    let mut store = InstallStore::load(&plugins);
    let record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();

    assert!(!record.enabled);
    assert_eq!(record.status, PluginStatus::InstalledDisabled);
    assert_eq!(record.id, "demo-plugin");
    assert!(plugins
        .join(&record.id)
        .join("package/plugin.json")
        .is_file());

    let staging = plugins.join(".staging");
    assert!(
        !staging.exists()
            || std::fs::read_dir(&staging)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false),
        "staging is not empty after a successful install"
    );

    let reloaded = InstallStore::load(&plugins);
    assert_eq!(reloaded.records, vec![record]);
}

#[test]
fn install_rejects_invalid_package() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    Conformant::agent(&source, "Bad--Name", "");
    let plugins = tmp.path().join("plugins");

    let mut store = InstallStore::load(&plugins);
    let result = install(&mut store, &plugins, &path_source(&source), None, None);

    assert!(result.is_err());
    assert!(store.records.is_empty());
    let leftovers: Vec<String> = std::fs::read_dir(&plugins)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".staging")
        .collect();
    assert!(leftovers.is_empty(), "package debris: {leftovers:?}");
    let staging = plugins.join(".staging");
    assert!(staging
        .read_dir()
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true));
}

#[test]
fn install_twice_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let plugins = tmp.path().join("plugins");
    let source = path_source(&source);

    let mut store = InstallStore::load(&plugins);
    install(&mut store, &plugins, &source, None, None).unwrap();
    let second = install(&mut store, &plugins, &source, None, None);

    let err = second.unwrap_err();
    assert!(err.contains("already installed"), "unexpected error: {err}");
    assert_eq!(store.records.len(), 1);
}

#[test]
fn uninstall_keeps_data_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let plugins = tmp.path().join("plugins");
    let data = tmp.path().join("plugin-data/demo-plugin");

    let mut store = InstallStore::load(&plugins);
    let record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    write(&data, "state.json", "{}");

    uninstall(&mut store, &plugins, &data, &record.id, false).unwrap();

    assert!(data.join("state.json").is_file(), "plugin data was deleted");
    assert!(store.records.is_empty());
    assert!(!plugins.join(&record.id).exists());
}

#[test]
fn uninstall_deletes_data_on_request() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let plugins = tmp.path().join("plugins");
    let data_root = tmp.path().join("plugin-data");
    let data = data_root.join("demo-plugin");

    let mut store = InstallStore::load(&plugins);
    let record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    write(&data, "state.json", "{}");

    uninstall(&mut store, &plugins, &data_root, &record.id, true).unwrap();

    assert!(!data.exists(), "plugin data survived a requested delete");
    assert!(data_root.is_dir(), "parent data directory was deleted");
    assert!(!plugins.join(&record.id).exists());
}

#[test]
fn install_from_source_without_marketplace() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    Conformant::agent(&repo, "git-plugin", r#", "version": "1.2.0""#).skill("demo", "demo");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "plugin"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let plugins = tmp.path().join("plugins");

    let mut store = InstallStore::load(&plugins);
    let record = install(&mut store, &plugins, &git_source(&repo), None, None).unwrap();

    assert_eq!(record.marketplace, None);
    assert_eq!(record.version.as_deref(), Some("1.2.0"));
    assert_eq!(record.resolved_sha.as_deref(), Some(head.as_str()));
    assert!(plugins
        .join(&record.id)
        .join("package/plugin.json")
        .is_file());
    assert!(
        !plugins.join(&record.id).join("package/.git").exists(),
        "the installed package kept a git checkout"
    );
}

#[test]
fn uninstall_returns_owned_server_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    Conformant::agent(&source, "with-server", "").mcp(
        r#"{"local-validator": {"type": "stdio", "command": "node"}}"#,
    );
    let plugins = tmp.path().join("plugins");
    let data = tmp.path().join("plugin-data/with-server");

    let mut store = InstallStore::load(&plugins);
    let record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let ids = uninstall(&mut store, &plugins, &data, &record.id, false).unwrap();

    assert_eq!(ids, vec![format!("plugin:{}:local-validator", record.id)]);
}

#[test]
fn install_checks_out_the_named_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    Conformant::agent(&repo, "pinned", "");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "first"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    write(&repo, "marker.txt", "second revision");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "second"]);

    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let source = PluginSource::Git {
        url: file_url(&repo),
        path: None,
        git_ref: None,
        sha: Some(first.clone()),
    };
    let record = install(&mut store, &plugins, &source, None, None).unwrap();

    assert_eq!(record.resolved_sha.as_deref(), Some(first.as_str()));
    assert!(
        !plugins.join(&record.id).join("package/marker.txt").exists(),
        "install did not check out the named sha"
    );
}

#[test]
fn fetch_to_staging_records_git_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    Conformant::agent(&repo, "staged", "");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "plugin"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let target = tmp.path().join("staging/package");
    let mut sha = None;
    let mut sha256 = None;

    fetch_to_staging(&git_source(&repo), &target, &mut sha, &mut sha256).unwrap();

    assert_eq!(sha.as_deref(), Some(head.as_str()));
    assert_eq!(sha256, None);
    assert!(target.join("plugin.json").is_file());
    assert!(!target.join(".git").exists());
}

#[test]
fn fetch_to_staging_takes_git_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    Conformant::agent(&repo.join("packages/plug"), "sub", "");
    write(&repo, "README.md", "not the package");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "plugin"]);
    let target = tmp.path().join("staging/package");
    let source = PluginSource::GitSubdir {
        url: file_url(&repo),
        path: "packages/plug".to_string(),
        git_ref: None,
        sha: None,
    };
    let mut sha = None;
    let mut sha256 = None;

    fetch_to_staging(&source, &target, &mut sha, &mut sha256).unwrap();

    assert!(target.join("plugin.json").is_file());
    assert!(!target.join("packages").exists());
    assert!(sha.is_some());
    assert!(!target.join(".git").exists(), "subdir package kept .git");
    assert!(
        !target.with_file_name(".git-clone").exists(),
        "git clone was left beside the package"
    );
}

#[test]
fn status_and_policy_serialise_to_the_pinned_strings() {
    let statuses = [
        (PluginStatus::InstalledDisabled, "installed_disabled"),
        (PluginStatus::Enabled, "enabled"),
        (PluginStatus::Invalid, "invalid"),
        (PluginStatus::UpdateAvailable, "update_available"),
        (PluginStatus::ModifiedLocally, "modified_locally"),
        (PluginStatus::Error, "error"),
    ];
    for (status, expected) in statuses {
        assert_eq!(serde_json::to_value(status).unwrap(), expected);
    }
    assert_eq!(serde_json::to_value(UpdatePolicy::Auto).unwrap(), "auto");
    assert_eq!(
        serde_json::to_value(UpdatePolicy::Manual).unwrap(),
        "manual"
    );
}

#[test]
fn corrupt_install_store_keeps_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("installed.json"), "{ not json").unwrap();

    let store = InstallStore::load(tmp.path());

    assert!(store.records.is_empty());
    assert!(store
        .diagnostics
        .iter()
        .any(|d| d.level == DiagLevel::Error));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("installed.json")).unwrap(),
        "{ not json",
        "load overwrote the corrupt file"
    );
}

#[test]
fn concurrent_record_writes_never_corrupt() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    InstallStore::load(&dir).save(&dir).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let unparsable = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let reader = {
        let dir = dir.clone();
        let stop = Arc::clone(&stop);
        let unparsable = Arc::clone(&unparsable);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(text) = std::fs::read_to_string(dir.join("installed.json")) {
                    if serde_json::from_str::<serde_json::Value>(&text).is_err() {
                        unparsable.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
    };

    // 4 writers x 50 interleaved add/remove saves: a shared temp name or a
    // non-atomic write must fail the all-Ok and parse assertions below.
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let dir = dir.clone();
            let failed = Arc::clone(&failed);
            std::thread::spawn(move || {
                for n in 0..50 {
                    let id = format!("p{w}-{n}");
                    let mut store = InstallStore::load(&dir);
                    store.records.retain(|record| record.id != id);
                    store.records.push(install_record(
                        &id,
                        &id,
                        PluginSource::Git {
                            url: format!("https://example.com/{w}.git"),
                            path: None,
                            git_ref: None,
                            sha: None,
                        },
                    ));
                    if store.save(&dir).is_err() {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
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
        failed.load(Ordering::Relaxed),
        0,
        "a concurrent save failed"
    );
    assert_eq!(
        unparsable.load(Ordering::Relaxed),
        0,
        "a concurrent save left an unparsable file"
    );
    let text = std::fs::read_to_string(dir.join("installed.json")).unwrap();
    let store: InstallStore = serde_json::from_str(&text).expect("final file parses");
    let mut ids: Vec<_> = store
        .records
        .iter()
        .map(|record| record.id.clone())
        .collect();
    let count = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        count,
        "duplicate records after concurrent writes"
    );
    for record in &store.records {
        assert!(!record.id.is_empty());
        assert!(matches!(&record.source, PluginSource::Git { .. }));
        assert_eq!(record.version.as_deref(), Some("1.0.0"));
    }
}

/// A Claude Code package. `name` is not an Agent Plugins name.
fn claude_package(root: &Path, name: &str) {
    Conformant::claude(root, &format!(r#"{{"name": "{name}"}}"#));
}

/// Install `keeper`, then a Claude package whose name slugs to an unsafe id.
fn unsafe_claude_name_does_not_wipe_plugins(name: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let keeper_src = tmp.path().join("keeper-src");
    plugin_package(&keeper_src, "keeper");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    install(&mut store, &plugins, &path_source(&keeper_src), None, None).unwrap();
    let snapshot = std::fs::read(plugins.join("installed.json")).unwrap();
    let source = tmp.path().join("bad-src");
    claude_package(&source, name);

    match install(&mut store, &plugins, &path_source(&source), None, None) {
        Ok(record) => {
            assert!(!record.id.is_empty(), "empty plugin id for name {name}");
            assert_ne!(record.id, ".");
            assert_ne!(record.id, "..");
            assert!(
                !record.id.contains('/') && !record.id.contains('\\'),
                "{}",
                record.id
            );
            uninstall(
                &mut store,
                &plugins,
                &tmp.path().join("data"),
                &record.id,
                false,
            )
            .unwrap();
        }
        Err(err) => {
            assert!(err.contains(name), "{err}");
            assert!(err.contains("plugin id"), "{err}");
            assert_eq!(
                std::fs::read(plugins.join("installed.json")).unwrap(),
                snapshot,
                "rejected install rewrote installed.json"
            );
        }
    }
    assert!(
        plugins.join("keeper/package/plugin.json").is_file(),
        "sibling plugin was deleted"
    );
    assert!(
        plugins.join("installed.json").is_file(),
        "installed.json was deleted"
    );
    let text = std::fs::read_to_string(plugins.join("installed.json")).unwrap();
    assert!(
        text.contains("keeper"),
        "installed.json lost the other plugin: {text}"
    );
}

#[test]
fn dashed_claude_name_does_not_wipe_plugins() {
    unsafe_claude_name_does_not_wipe_plugins("---");
}

#[test]
fn dotdot_claude_name_does_not_wipe_plugins() {
    unsafe_claude_name_does_not_wipe_plugins("..");
}

/// A hand-edited id must not be joined onto `plugins/` and deleted.
fn uninstall_of_unsafe_id_keeps_siblings(id: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let keeper_src = tmp.path().join("keeper-src");
    plugin_package(&keeper_src, "keeper");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    install(&mut store, &plugins, &path_source(&keeper_src), None, None).unwrap();
    store
        .records
        .push(install_record(id, "bad", path_source(&keeper_src)));
    store.save(&plugins).unwrap();
    let marker = tmp.path().join("outside-marker");
    std::fs::write(&marker, "keep").unwrap();

    let result = uninstall(&mut store, &plugins, &tmp.path().join("data"), id, false);
    assert!(
        marker.is_file(),
        "uninstall of `{id}` deleted the parent of plugins/"
    );
    assert!(
        plugins.join("keeper/package/plugin.json").is_file(),
        "sibling plugin was deleted"
    );
    assert!(
        plugins.join("installed.json").is_file(),
        "installed.json was deleted"
    );
    let text = std::fs::read_to_string(plugins.join("installed.json")).unwrap();
    assert!(text.contains("keeper"), "{text}");
    assert!(
        store.records.iter().any(|record| record.id == id),
        "unsafe id was removed from the store"
    );
    let err = result.expect_err("unsafe id must be refused");
    assert!(!err.is_empty(), "{err}");
}

#[test]
fn uninstall_of_dotdot_id_keeps_other_plugins() {
    uninstall_of_unsafe_id_keeps_siblings("..");
}

#[test]
fn uninstall_of_empty_id_keeps_other_plugins() {
    uninstall_of_unsafe_id_keeps_siblings("");
}

const UNSAFE_IDS: [&str; 13] = [
    "", ".", "..", " ", ". ", ".. ", "...", "foo.", "foo ", "a/b", "a\\b", "C:foo", "C:..",
];

#[test]
fn package_dir_rejects_unsafe_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    for id in UNSAFE_IDS {
        let err = package_dir(&plugins, id).expect_err(id);
        assert!(!err.is_empty(), "{id}: {err}");
    }
}

#[test]
fn package_dir_accepts_ordinary_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    assert_eq!(package_dir(&plugins, "acme").unwrap(), plugins.join("acme"));
    assert_eq!(
        package_dir(&plugins, "acme-tools-1.2").unwrap(),
        plugins.join("acme-tools-1.2")
    );
}

fn keeper_bytes(plugins: &Path) -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(plugins.join("installed.json")).unwrap(),
        std::fs::read(plugins.join("keeper/package/plugin.json")).unwrap(),
    )
}

fn uninstall_of_hand_edited_id_keeps_bytes(id: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let keeper_src = tmp.path().join("keeper-src");
    plugin_package(&keeper_src, "keeper");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    install(&mut store, &plugins, &path_source(&keeper_src), None, None).unwrap();
    store
        .records
        .push(install_record(id, "bad", path_source(&keeper_src)));
    store.save(&plugins).unwrap();
    let before = keeper_bytes(&plugins);

    let err = uninstall(&mut store, &plugins, &tmp.path().join("data"), id, false).expect_err(id);
    assert!(!err.is_empty(), "{id}: {err}");
    assert_eq!(
        keeper_bytes(&plugins),
        before,
        "id `{id}` changed the store"
    );
    assert!(
        store.records.iter().any(|record| record.id == id),
        "unsafe id `{id}` was removed from the store"
    );
}

#[test]
fn uninstall_of_unsafe_ids_leaves_store_byte_identical() {
    for id in UNSAFE_IDS {
        uninstall_of_hand_edited_id_keeps_bytes(id);
    }
}

#[test]
fn install_of_empty_slug_names_manifest_and_skips_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let keeper_src = tmp.path().join("keeper-src");
    plugin_package(&keeper_src, "keeper");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    install(&mut store, &plugins, &path_source(&keeper_src), None, None).unwrap();
    let staging = plugins.join(".staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).unwrap();
    }
    let before = keeper_bytes(&plugins);
    let source = tmp.path().join("bad-src");
    claude_package(&source, "---");

    let err = install(&mut store, &plugins, &path_source(&source), None, None).unwrap_err();
    assert!(err.contains("---"), "{err}");
    assert!(err.contains("plugin id"), "{err}");
    assert_eq!(store.records.len(), 1, "rejected install wrote a record");
    assert_eq!(keeper_bytes(&plugins), before);
    assert!(
        !staging.exists(),
        "rejected install created plugins/.staging"
    );
}

#[test]
fn uninstall_delete_data_unsafe_id_stays_inside_data_dir() {
    for id in UNSAFE_IDS {
        let tmp = tempfile::tempdir().unwrap();
        let keeper_src = tmp.path().join("keeper-src");
        plugin_package(&keeper_src, "keeper");
        let plugins = tmp.path().join("plugins");
        let mut store = InstallStore::load(&plugins);
        install(&mut store, &plugins, &path_source(&keeper_src), None, None).unwrap();
        store
            .records
            .push(install_record(id, "bad", path_source(&keeper_src)));
        store.save(&plugins).unwrap();
        let before = keeper_bytes(&plugins);
        let outside = tmp.path().join("outside-marker");
        std::fs::write(&outside, "keep").unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let inside = data.join("sibling.txt");
        std::fs::write(&inside, "keep").unwrap();

        let err = uninstall(&mut store, &plugins, &data, id, true).expect_err(id);
        assert!(!err.is_empty(), "{id}: {err}");
        assert!(outside.is_file(), "id `{id}` deleted outside data_dir");
        assert!(
            inside.is_file(),
            "id `{id}` deleted a sibling inside data_dir"
        );
        assert_eq!(
            keeper_bytes(&plugins),
            before,
            "id `{id}` changed the store"
        );
    }
}

#[test]
fn uninstall_keeps_record_when_package_delete_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let dir = plugins.join(&record.id);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, "not a directory").unwrap();
    let before = std::fs::read(plugins.join("installed.json")).unwrap();

    let err = uninstall(
        &mut store,
        &plugins,
        &tmp.path().join("data"),
        &record.id,
        false,
    )
    .unwrap_err();
    assert!(
        err.contains("cannot remove") || err.contains(&record.id),
        "{err}"
    );
    assert!(
        store.records.iter().any(|item| item.id == record.id),
        "record was dropped before the directory delete failed"
    );
    assert_eq!(
        std::fs::read(plugins.join("installed.json")).unwrap(),
        before,
        "installed.json lost the record"
    );
}

#[test]
fn save_refuses_corrupt_install_store() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let truncated = b"{ not json";
    std::fs::write(plugins.join("installed.json"), truncated).unwrap();

    let store = InstallStore::load(&plugins);
    let err = store.save(&plugins).unwrap_err();
    assert!(err.to_string().contains("installed.json"), "{err}");
    assert_eq!(
        std::fs::read(plugins.join("installed.json")).unwrap(),
        truncated
    );
}

#[test]
fn install_refuses_corrupt_store_without_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let truncated = b"{ not json";
    std::fs::write(plugins.join("installed.json"), truncated).unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    assert!(!plugins.join(".staging").exists());

    let mut store = InstallStore::load(&plugins);
    let err = install(&mut store, &plugins, &path_source(&source), None, None).unwrap_err();
    assert!(err.contains("corrupt"), "{err}");
    assert!(err.contains("installed.json"), "{err}");
    assert_eq!(
        std::fs::read(plugins.join("installed.json")).unwrap(),
        truncated
    );
    assert!(
        !plugins.join(".staging").exists(),
        "install fetched before rejecting the corrupt store"
    );
}

#[test]
fn fetch_to_staging_missing_subdir_removes_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    write(&repo, "README.md", "no package");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "empty"]);
    let target = tmp.path().join("staging/package");
    let source = PluginSource::GitSubdir {
        url: file_url(&repo),
        path: "missing/plug".to_string(),
        git_ref: None,
        sha: None,
    };
    let mut sha = None;
    let mut sha256 = None;

    let err = fetch_to_staging(&source, &target, &mut sha, &mut sha256).unwrap_err();
    assert!(err.contains("missing"), "{err}");
    assert!(
        !target.with_file_name(".git-clone").exists(),
        "git clone was left beside the package"
    );
}

#[test]
#[cfg(unix)]
fn install_rejects_escaping_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside-secret.txt");
    std::fs::write(&outside, "secret-bytes").unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "leaky");
    symlink(&outside, &source.join("leak"));
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);

    let err = install(&mut store, &plugins, &path_source(&source), None, None).unwrap_err();
    assert!(err.contains("outside") || err.contains("link"), "{err}");
    assert!(
        !plugins.join("leaky/package/leak").exists(),
        "escaping link was installed"
    );
    assert!(store.records.is_empty());
}

#[test]
#[cfg(unix)]
fn install_keeps_in_root_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "linked");
    symlink(Path::new("plugin.json"), &source.join("alias.json"));
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);

    let record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let link = plugins.join(&record.id).join("package/alias.json");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "in-root link was copied as a file"
    );
}

// ---------------------------------------------------------------------------
// Update engine (design §7)
// ---------------------------------------------------------------------------

/// A local package whose manifest version and marker file the update tests read.
fn versioned_package(root: &Path, name: &str, version: &str, marker: &str) -> Conformant {
    let package =
        Conformant::agent(root, name, &format!(r#", "version": "{version}""#)).skill("demo", "demo");
    write(root, "marker.txt", marker);
    package
}

/// No manifest version. The ladder's last rung is the literal `unknown`.
fn unversioned_package(root: &Path, name: &str, marker: &str) {
    Conformant::agent(root, name, "").skill("demo", "demo");
    write(root, "marker.txt", marker);
}

fn test_entry(name: &str, version: Option<&str>, source: PluginSource) -> Entry {
    Entry {
        name: name.to_string(),
        display_name: None,
        description: None,
        version: version.map(str::to_string),
        category: None,
        tags: Vec::new(),
        author: None,
        homepage: None,
        icon: None,
        keywords: Vec::new(),
        source,
        available: true,
        reason: None,
    }
}

fn test_registry(entry: Entry) -> Registry {
    Registry {
        name: "test".to_string(),
        description: None,
        owner: None,
        entries: vec![entry],
        registry_path: std::path::PathBuf::new(),
    }
}

#[test]
fn tree_hash_stable_and_detects_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let package = tmp.path();
    write(package, "nested/a.txt", "alpha");
    write(package, "b.txt", "beta");

    let first = tree_hash(package).expect("hash");
    assert!(first.starts_with("sha256:"));
    assert_eq!(first.len(), "sha256:".len() + 64);
    assert!(first["sha256:".len()..]
        .chars()
        .all(|c| c.is_ascii_hexdigit()));
    assert_eq!(tree_hash(package).as_deref(), Some(first.as_str()));

    std::fs::write(package.join("nested/a.txt"), "changed").unwrap();
    assert_ne!(tree_hash(package).as_deref(), Some(first.as_str()));
}

#[test]
fn tree_hash_none_when_too_large() {
    let tmp = tempfile::tempdir().unwrap();
    for index in 0..2000 {
        write(tmp.path(), &format!("f{index}.txt"), "x");
    }
    assert!(tree_hash(tmp.path()).is_some(), "2000 files still hash");

    write(tmp.path(), "f2000.txt", "x");
    assert_eq!(tree_hash(tmp.path()), None);
}

#[test]
fn update_detected_on_registry_version_bump() {
    let tmp = tempfile::tempdir().unwrap();
    let mut record = install_record("demo", "demo", path_source(tmp.path()));
    record.version = Some("1.0.0".to_string());
    let registry = test_registry(test_entry("demo", Some("1.1.0"), path_source(tmp.path())));

    let found = check_one(&mut record, Some(&registry), tmp.path()).unwrap();

    assert_eq!(
        found.and_then(|update| update.version).as_deref(),
        Some("1.1.0")
    );
    assert_eq!(record.status, PluginStatus::UpdateAvailable);
    assert_eq!(
        record
            .available_update
            .as_ref()
            .and_then(|update| update.version.as_deref()),
        Some("1.1.0")
    );
    assert!(record.last_checked_at.is_some());
}

#[test]
fn no_update_when_versions_equal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut record = install_record("demo", "demo", path_source(tmp.path()));
    record.version = Some("1.0.0".to_string());
    record.status = PluginStatus::UpdateAvailable;
    record.available_update = Some(AvailableUpdate {
        version: Some("0.9.0".to_string()),
        resolved_sha: None,
    });
    let registry = test_registry(test_entry("demo", Some("1.0.0"), path_source(tmp.path())));

    let found = check_one(&mut record, Some(&registry), tmp.path()).unwrap();

    assert_eq!(found, None);
    assert_eq!(record.available_update, None);
    assert_ne!(record.status, PluginStatus::UpdateAvailable);
}

#[test]
fn update_detected_by_ls_remote_sha() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    Conformant::agent(&repo, "remote-plugin", "");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "first"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-b", "update"]);
    write(&repo, "marker.txt", "second");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "second"]);
    let second = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "main"]);

    let source = PluginSource::Git {
        url: file_url(&repo),
        path: None,
        git_ref: Some("update".to_string()),
        sha: None,
    };
    let mut record = install_record("remote-plugin", "remote-plugin", source);
    record.version = Some(first.chars().take(12).collect());
    record.resolved_sha = Some(first);

    let found = check_one(&mut record, None, tmp.path()).unwrap();

    let sha = found.and_then(|update| update.resolved_sha).expect("sha");
    assert_eq!(sha, second);
    assert_eq!(sha.len(), 40);
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(record.status, PluginStatus::UpdateAvailable);
    assert_eq!(
        record
            .available_update
            .as_ref()
            .and_then(|update| update.resolved_sha.as_deref()),
        Some(sha.as_str())
    );
}

#[test]
fn apply_swaps_and_keeps_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let before = tree_hash(&package);
    record.tree_hash = before.clone();
    write(&id_dir.join("package.previous-0.9.0"), "stale.txt", "stale");
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");

    let outcome = apply(&mut record, &plugins, None, false).unwrap();

    assert_eq!(outcome.from.as_deref(), Some("1.0.0"));
    assert_eq!(outcome.to.as_deref(), Some("1.1.0"));
    assert!(!outcome.modified_locally);
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package.previous-1.0.0/marker.txt")).unwrap(),
        "old-bytes"
    );
    assert!(!id_dir.join("package.previous-0.9.0").exists());
    let refreshed = record.tree_hash.clone().expect("tree_hash refreshed");
    assert!(refreshed.starts_with("sha256:"));
    assert_ne!(Some(refreshed.clone()), before);
    assert_eq!(tree_hash(&package).as_deref(), Some(refreshed.as_str()));
    assert_eq!(record.version.as_deref(), Some("1.1.0"));
    assert_eq!(record.previous_version.as_deref(), Some("1.0.0"));
    let loaded = InstallStore::load(&plugins);
    let saved = loaded
        .records
        .iter()
        .find(|item| item.id == record.id)
        .unwrap();
    assert_eq!(saved.version.as_deref(), Some("1.1.0"));
    assert_eq!(saved.previous_version.as_deref(), Some("1.0.0"));
    assert_eq!(saved.tree_hash.as_deref(), Some(refreshed.as_str()));
}

#[test]
fn apply_is_atomic_on_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let package = plugins.join(&record.id).join("package");
    record.tree_hash = tree_hash(&package);
    let before_version = record.version.clone();
    let before_sha = record.resolved_sha.clone();
    let bad = tmp.path().join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    let entry = test_entry("demo-plugin", Some("9.9.9"), path_source(&bad));

    let err = apply(&mut record, &plugins, Some(&entry), false).unwrap_err();

    assert!(!err.is_empty(), "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert!(!plugins
        .join(&record.id)
        .join("package.previous-1.0.0")
        .exists());
    assert_eq!(record.version, before_version);
    assert_eq!(record.resolved_sha, before_sha);
    assert_eq!(record.previous_version, None);
    let loaded = InstallStore::load(&plugins);
    assert_eq!(loaded.records[0].version.as_deref(), Some("1.0.0"));
}

#[test]
fn apply_blocked_when_modified_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let package = plugins.join(&record.id).join("package");
    record.tree_hash = tree_hash(&package);
    record.update_policy = UpdatePolicy::Auto;
    std::fs::write(package.join("marker.txt"), "local-edit").unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "upstream");

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(err.contains("modified locally"), "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "local-edit"
    );
    assert_eq!(record.status, PluginStatus::ModifiedLocally);
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert!(!plugins
        .join(&record.id)
        .join("package.previous-1.0.0")
        .exists());
}

#[test]
fn apply_overwrites_when_forced() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let package = plugins.join(&record.id).join("package");
    record.tree_hash = tree_hash(&package);
    record.update_policy = UpdatePolicy::Auto;
    std::fs::write(package.join("marker.txt"), "local-edit").unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "upstream");

    let outcome = apply(&mut record, &plugins, None, true).unwrap();

    assert!(outcome.modified_locally);
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "upstream"
    );
    assert_eq!(
        std::fs::read_to_string(
            plugins
                .join(&record.id)
                .join("package.previous-1.0.0/marker.txt")
        )
        .unwrap(),
        "local-edit"
    );
}

#[test]
fn rollback_restores_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    record.tree_hash = tree_hash(&package);
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();

    rollback(&mut record, &plugins).unwrap();

    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert_eq!(record.previous_version, None);
    assert!(!id_dir.join("package.previous-1.0.0").exists());
    let loaded = InstallStore::load(&plugins);
    assert_eq!(loaded.records[0].version.as_deref(), Some("1.0.0"));
    assert_eq!(loaded.records[0].previous_version, None);
}

#[test]
fn rollback_without_previous_is_a_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let package = plugins.join(&record.id).join("package");

    let err = rollback(&mut record, &plugins).unwrap_err();

    assert!(err.to_lowercase().contains("previous"), "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert_eq!(record.previous_version, None);
}

#[test]
fn apply_reconnects_only_owned_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "with-server", "1.0.0", "old").mcp(
        r#"{"alpha": {"type": "stdio", "command": "node"}, "beta": {"type": "stdio", "command": "node"}}"#,
    );
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    record.disabled_servers = vec!["beta".to_string(), "not-owned".to_string()];
    Conformant::bare(&source).mcp(r#"{"gamma": {"type": "stdio", "command": "node"}}"#);

    let outcome = apply(&mut record, &plugins, None, false).unwrap();

    assert_eq!(
        outcome.enabled_servers,
        vec![format!("plugin:{}:alpha", record.id)]
    );
}

fn count_previous(plugin_dir: &Path) -> usize {
    count_prefixed(plugin_dir, "package.previous-")
}

fn count_held(plugin_dir: &Path) -> usize {
    count_prefixed(plugin_dir, "package.held-")
}

fn count_prefixed(plugin_dir: &Path, prefix: &str) -> usize {
    std::fs::read_dir(plugin_dir)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry.file_name().to_string_lossy().starts_with(prefix) && entry.path().is_dir()
        })
        .count()
}

/// `package/` was renamed to `package.previous-<version>` and the process died.
fn kill_after_first_rename(plugin_dir: &Path, version: &str) {
    let live = plugin_dir.join("package");
    let previous = plugin_dir.join(format!("package.previous-{version}"));
    std::fs::rename(&live, &previous).unwrap();
    assert!(!live.exists(), "kill simulation left package/");
}

#[test]
fn check_one_annotated_tag_matches_peeled_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    git(&repo, &["config", "tag.gpgsign", "false"]);
    versioned_package(&repo, "tagged-plugin", "1.0.0", "first");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "first"]);
    git(&repo, &["tag", "-a", "v1.2.3", "-m", "annotated"]);
    let commit = git(&repo, &["rev-parse", "v1.2.3^{}"]);
    let tag_object = git(&repo, &["rev-parse", "v1.2.3"]);
    assert_ne!(commit, tag_object, "fixture is not an annotated tag");

    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let source = PluginSource::Git {
        url: file_url(&repo),
        path: None,
        git_ref: Some("v1.2.3".to_string()),
        sha: None,
    };
    let mut record = install(&mut store, &plugins, &source, None, None).unwrap();
    assert_eq!(record.resolved_sha.as_deref(), Some(commit.as_str()));

    let found = check_one(&mut record, None, &plugins).unwrap();
    assert_eq!(found, None, "annotated tag looked like an update");
    assert_ne!(record.status, PluginStatus::UpdateAvailable);

    write(&repo, "marker.txt", "second");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "second"]);
    git(&repo, &["tag", "-d", "v1.2.3"]);
    git(&repo, &["tag", "-a", "v1.2.3", "-m", "moved"]);
    let second = git(&repo, &["rev-parse", "v1.2.3^{}"]);
    assert_ne!(second, commit);

    let found = check_one(&mut record, None, &plugins).unwrap();
    assert_eq!(
        found.and_then(|update| update.resolved_sha).as_deref(),
        Some(second.as_str())
    );
    assert_eq!(record.status, PluginStatus::UpdateAvailable);
}

#[test]
fn check_one_prefers_branch_over_same_name_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    git(&repo, &["config", "tag.gpgsign", "false"]);
    versioned_package(&repo, "shared-ref", "1.0.0", "branch-bytes");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "branch"]);
    git(&repo, &["branch", "release"]);
    let branch = git(&repo, &["rev-parse", "refs/heads/release"]);
    write(&repo, "marker.txt", "tag-bytes");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "tag target"]);
    git(
        &repo,
        &["tag", "-a", "release", "-m", "collides with the branch"],
    );
    let tag_commit = git(&repo, &["rev-parse", "refs/tags/release^{}"]);
    assert_ne!(branch, tag_commit);

    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let source = PluginSource::Git {
        url: file_url(&repo),
        path: None,
        git_ref: Some("release".to_string()),
        sha: None,
    };
    let mut record = install(&mut store, &plugins, &source, None, None).unwrap();
    assert_eq!(
        record.resolved_sha.as_deref(),
        Some(branch.as_str()),
        "install did not check out the branch"
    );

    let found = check_one(&mut record, None, &plugins).unwrap();
    assert_eq!(found, None, "same-name tag looked like an update");
    assert_ne!(record.status, PluginStatus::UpdateAvailable);
}

#[test]
fn ls_remote_missing_ref_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    Conformant::agent(&repo, "remote-plugin", "");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "first"]);
    let source = PluginSource::Git {
        url: file_url(&repo),
        path: None,
        git_ref: Some("missing-ref".to_string()),
        sha: None,
    };
    let mut record = install_record("remote-plugin", "remote-plugin", source);
    record.resolved_sha = Some("a".repeat(40));

    let err = check_one(&mut record, None, tmp.path()).unwrap_err();

    assert!(err.contains("missing-ref"), "{err}");
    assert_ne!(record.status, PluginStatus::UpdateAvailable);
    assert!(record.available_update.is_none());
}

#[test]
fn ls_remote_dead_remote_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let source = PluginSource::Git {
        url: file_url(&tmp.path().join("missing-repo")),
        path: None,
        git_ref: Some("HEAD".to_string()),
        sha: None,
    };
    let mut record = install_record("remote-plugin", "remote-plugin", source);

    let err = check_one(&mut record, None, tmp.path()).unwrap_err();

    assert!(!err.is_empty(), "{err}");
    assert_ne!(record.status, PluginStatus::UpdateAvailable);
}

#[test]
fn check_one_head_ref_tracks_the_default_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    Conformant::agent(&repo, "remote-plugin", "");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "first"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    let source = PluginSource::Git {
        url: file_url(&repo),
        path: None,
        git_ref: None,
        sha: None,
    };
    let mut record = install_record("remote-plugin", "remote-plugin", source);
    record.resolved_sha = Some(first);

    assert_eq!(check_one(&mut record, None, tmp.path()).unwrap(), None);

    write(&repo, "marker.txt", "second");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-m", "second"]);
    let second = git(&repo, &["rev-parse", "HEAD"]);

    let found = check_one(&mut record, None, tmp.path()).unwrap();
    assert_eq!(
        found.and_then(|update| update.resolved_sha).as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn apply_restores_package_after_killed_swap() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let id_dir = plugins.join(&record.id);
    kill_after_first_rename(&id_dir, "1.0.0");
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");

    apply(&mut record, &plugins, None, false).unwrap();

    let package = id_dir.join("package");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package.previous-1.0.0/marker.txt")).unwrap(),
        "old-bytes"
    );
    assert_eq!(count_previous(&id_dir), 1);
    assert_eq!(record.version.as_deref(), Some("1.1.0"));
}

#[test]
fn rollback_restores_package_after_killed_swap() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    kill_after_first_rename(&id_dir, "1.0.0");

    let result = rollback(&mut record, &plugins);

    assert!(
        package.join("marker.txt").is_file(),
        "rollback left package missing: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
}

#[test]
fn apply_names_every_previous_when_package_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let older = id_dir.join("package.previous-1.0.0");
    let newer = id_dir.join("package.previous-9.9.9");
    std::fs::rename(&package, &older).unwrap();
    write(&newer, "marker.txt", "newer-bytes");

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(err.contains(&older.display().to_string()), "{err}");
    assert!(err.contains(&newer.display().to_string()), "{err}");
    assert!(!package.exists(), "a previous directory was restored");
    assert!(older.is_dir());
    assert!(newer.is_dir());
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
}

#[test]
fn rollback_finishes_after_kill_inside_swap_back() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let aside = id_dir.join("package.rollback-tmp");
    std::fs::rename(&package, &aside).unwrap();
    assert_eq!(
        std::fs::read_to_string(aside.join("marker.txt")).unwrap(),
        "new-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package.previous-1.0.0/marker.txt")).unwrap(),
        "old-bytes"
    );

    let result = rollback(&mut record, &plugins);

    assert_eq!(record.version.as_deref(), Some("1.0.0"), "{result:?}");
    assert!(result.is_ok(), "{result:?}");
    assert!(record.previous_version.is_none());
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert!(!aside.exists());
    assert_eq!(count_previous(&id_dir), 0);
    let loaded = InstallStore::load(&plugins);
    let saved = loaded
        .records
        .iter()
        .find(|item| item.id == record.id)
        .unwrap();
    assert_eq!(saved.version.as_deref(), Some("1.0.0"));
    assert!(saved.previous_version.is_none());
    assert_eq!(saved.tree_hash.as_deref(), tree_hash(&package).as_deref());
}

#[test]
fn rollback_refuses_when_only_copy_is_rollback_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-1.0.0");
    let held = id_dir.join("held-new");
    std::fs::rename(&package, &held).unwrap();

    let rollback_err = rollback(&mut record, &plugins).unwrap_err();
    let apply_err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(
        rollback_err.contains("recorded rollback source"),
        "{rollback_err}"
    );
    assert!(
        apply_err.contains("recorded rollback source"),
        "{apply_err}"
    );
    assert!(previous.is_dir(), "rollback source was consumed");
    assert!(!package.exists());
    assert_eq!(record.version.as_deref(), Some("1.1.0"));
    assert_eq!(record.previous_version.as_deref(), Some("1.0.0"));
    let loaded = InstallStore::load(&plugins);
    let saved = loaded
        .records
        .iter()
        .find(|item| item.id == record.id)
        .unwrap();
    assert_eq!(saved.version.as_deref(), Some("1.1.0"));
    assert_eq!(saved.previous_version.as_deref(), Some("1.0.0"));

    std::fs::rename(&held, &package).unwrap();
    rollback(&mut record, &plugins).unwrap();
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert!(record.previous_version.is_none());
}

/// Hide `package/` and assert recovery will not consume the only previous dir.
fn expect_rollback_source_kept(
    plugins: &Path,
    record: &mut InstallRecord,
    previous: &Path,
    old_marker: &str,
) {
    let package = plugins.join(&record.id).join("package");
    let held = package.with_file_name("held-new");
    let version = record.version.clone();
    let previous_version = record.previous_version.clone();
    std::fs::rename(&package, &held).unwrap();

    let rollback_err = rollback(record, plugins).unwrap_err();
    let apply_err = apply(record, plugins, None, false).unwrap_err();

    assert!(
        rollback_err.contains("recorded rollback source"),
        "{rollback_err}"
    );
    assert!(
        rollback_err.contains(&previous.display().to_string()),
        "{rollback_err}"
    );
    assert!(
        apply_err.contains("recorded rollback source"),
        "{apply_err}"
    );
    assert!(previous.is_dir(), "rollback source was consumed");
    assert!(!package.exists());
    assert_eq!(record.version, version);
    assert_eq!(record.previous_version, previous_version);
    let loaded = InstallStore::load(plugins);
    let saved = loaded
        .records
        .iter()
        .find(|item| item.id == record.id)
        .unwrap();
    assert_eq!(saved.version, version);
    assert_eq!(saved.previous_version, previous_version);

    std::fs::rename(&held, &package).unwrap();
    rollback(record, plugins).unwrap();
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        old_marker
    );
    assert_eq!(record.version, previous_version);
    assert!(record.previous_version.is_none());
    assert!(!previous.exists());
}

#[test]
fn recovery_refuses_unknown_version_rollback_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    unversioned_package(&source, "demo-plugin", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    assert_eq!(record.version.as_deref(), Some("unknown"));
    assert!(record.resolved_sha.is_none());
    assert!(record.resolved_sha256.is_none());
    unversioned_package(&source, "demo-plugin", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();

    let id_dir = plugins.join(&record.id);
    let previous = id_dir.join("package.previous-unknown");
    assert!(previous.is_dir(), "suffix drifted from the ladder value");
    assert_eq!(count_previous(&id_dir), 1);
    assert_eq!(record.previous_version.as_deref(), Some("unknown"));
    assert_eq!(record.version.as_deref(), Some("unknown"));
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "old-bytes"
    );

    expect_rollback_source_kept(&plugins, &mut record, &previous, "old-bytes");
}

#[test]
fn recovery_refuses_sanitized_version_rollback_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0/beta", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    assert_eq!(record.version.as_deref(), Some("1.0/beta"));
    versioned_package(&source, "demo-plugin", "2.0", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();

    let id_dir = plugins.join(&record.id);
    let previous = id_dir.join("package.previous-unknown");
    assert!(previous.is_dir(), "unsafe version was not sanitized");
    assert_eq!(count_previous(&id_dir), 1);
    assert_eq!(record.previous_version.as_deref(), Some("1.0/beta"));
    assert_eq!(record.version.as_deref(), Some("2.0"));
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "old-bytes"
    );

    expect_rollback_source_kept(&plugins, &mut record, &previous, "old-bytes");
}

#[test]
fn recover_restores_sanitized_previous_when_version_differs() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0/beta", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let id_dir = plugins.join(&record.id);
    let live = id_dir.join("package");
    let previous = id_dir.join("package.previous-unknown");
    std::fs::rename(&live, &previous).unwrap();
    record.previous_version = Some("0.9.0".to_string());
    std::fs::remove_dir_all(&source).unwrap();

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert_eq!(
        std::fs::read_to_string(id_dir.join("package/marker.txt")).unwrap(),
        "old-bytes",
        "{err}"
    );
    assert!(!previous.exists(), "{err}");
    assert_eq!(record.version.as_deref(), Some("1.0/beta"), "{err}");
    assert_eq!(record.previous_version.as_deref(), Some("0.9.0"));
}

#[test]
fn recover_restores_previous_when_label_differs_from_record() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let id_dir = plugins.join(&record.id);
    kill_after_first_rename(&id_dir, "1.0.0");
    record.previous_version = Some("0.9.0".to_string());
    std::fs::remove_dir_all(&source).unwrap();

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert_eq!(
        std::fs::read_to_string(id_dir.join("package/marker.txt")).unwrap(),
        "old-bytes",
        "{err}"
    );
    assert_eq!(record.version.as_deref(), Some("1.0.0"), "{err}");
    assert_eq!(record.previous_version.as_deref(), Some("0.9.0"));
}

/// Successful `1.0.0` → `1.1.0` update. The rollback source is only in `package.held-7-3`.
fn park_rollback_in_held(root: &Path) -> (std::path::PathBuf, InstallRecord) {
    let source = root.join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = root.join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    let id_dir = plugins.join(&record.id);
    std::fs::rename(
        id_dir.join("package.previous-1.0.0"),
        id_dir.join("package.held-7-3"),
    )
    .unwrap();
    assert_eq!(record.version.as_deref(), Some("1.1.0"));
    assert_eq!(record.previous_version.as_deref(), Some("1.0.0"));
    assert!(!id_dir.join("package.previous-1.0.0").exists());
    (plugins, record)
}

/// Kill after `staged` → `package/` and before the held delete.
/// `package/` is the unrecorded new tree. `package.previous-unknown` is the
/// recorded live tree. `package.held-99-1` is the older rollback source.
/// The suffix matches `previous_version`.
fn collision_kill_after_staged(root: &Path) -> (std::path::PathBuf, InstallRecord) {
    let source = root.join("source");
    unversioned_package(&source, "demo-plugin", "old-bytes");
    let plugins = root.join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    unversioned_package(&source, "demo-plugin", "live-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-unknown");
    let held = id_dir.join("package.held-99-1");
    assert_eq!(record.version.as_deref(), Some("unknown"));
    assert_eq!(record.previous_version.as_deref(), Some("unknown"));
    std::fs::rename(&previous, &held).unwrap();
    std::fs::rename(&package, &previous).unwrap();
    let staged = root.join("killed-staged");
    unversioned_package(&staged, "demo-plugin", "new-bytes");
    std::fs::rename(&staged, &package).unwrap();
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "live-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(held.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    (plugins, record)
}

fn saved_plugin<'a>(id: &str, store: &'a InstallStore) -> &'a InstallRecord {
    store.records.iter().find(|item| item.id == id).unwrap()
}

/// Kill after the live rename, before the staged rename. Both versions are
/// `unknown`. `package.previous-unknown` holds the live tree. `package.held-*`
/// holds the rollback source. A matching suffix must not refuse this window.
#[test]
fn recovery_restores_live_tree_when_unknown_suffixes_collide() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    unversioned_package(&source, "demo-plugin", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    unversioned_package(&source, "demo-plugin", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();

    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-unknown");
    let held = id_dir.join("package.held-99-1");
    assert_eq!(record.version.as_deref(), Some("unknown"));
    assert_eq!(record.previous_version.as_deref(), Some("unknown"));
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    std::fs::rename(&previous, &held).unwrap();
    std::fs::rename(&package, &previous).unwrap();
    std::fs::remove_dir_all(&source).unwrap();
    let version = record.version.clone();
    let previous_version = record.previous_version.clone();

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(!err.contains("recorded rollback source"), "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes",
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "old-bytes",
        "{err}"
    );
    assert_eq!(count_held(&id_dir), 0, "{err}");
    assert!(!held.exists(), "{err}");
    assert_eq!(record.version, version);
    assert_eq!(record.previous_version, previous_version);
    let loaded = InstallStore::load(&plugins);
    let saved = saved_plugin(&record.id, &loaded);
    assert_eq!(saved.version, version);
    assert_eq!(saved.previous_version, previous_version);

    rollback(&mut record, &plugins).unwrap();
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert_eq!(record.version.as_deref(), Some("unknown"));
    assert!(record.previous_version.is_none());
    assert!(!previous.exists());
}

/// Kill after `hold_existing`, before the live rename. `package/` is intact.
/// The `1.0.0` rollback source is only in `package.held-7-3`.
#[test]
fn apply_restores_held_rollback_source_before_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let (plugins, mut record) = park_rollback_in_held(tmp.path());
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-1.0.0");
    let held = id_dir.join("package.held-7-3");
    std::fs::remove_dir_all(tmp.path().join("source")).unwrap();
    let version = record.version.clone();
    let previous_version = record.previous_version.clone();

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(previous.is_dir(), "held was not restored: {err}");
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "old-bytes",
        "{err}"
    );
    assert!(!held.exists(), "{err}");
    assert_eq!(count_held(&id_dir), 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes"
    );
    assert_eq!(record.version, version);
    assert_eq!(record.previous_version, previous_version);
    let loaded = InstallStore::load(&plugins);
    let saved = saved_plugin(&record.id, &loaded);
    assert_eq!(saved.version, version);
    assert_eq!(saved.previous_version, previous_version);
}

#[test]
fn rollback_restores_held_rollback_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (plugins, mut record) = park_rollback_in_held(tmp.path());
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let held = id_dir.join("package.held-7-3");

    let result = rollback(&mut record, &plugins);

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert!(record.previous_version.is_none());
    assert!(!held.exists());
    assert_eq!(count_held(&id_dir), 0);
    assert_eq!(count_previous(&id_dir), 0);
    let loaded = InstallStore::load(&plugins);
    let saved = saved_plugin(&record.id, &loaded);
    assert_eq!(saved.version.as_deref(), Some("1.0.0"));
    assert!(saved.previous_version.is_none());
}

/// `package.rollback-tmp` is the new bytes. Recovery must not touch held or previous.
#[test]
fn rollback_tmp_wins_over_held_and_previous() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-1.0.0");
    let aside = id_dir.join("package.rollback-tmp");
    let held = id_dir.join("package.held-1-1");
    std::fs::rename(&package, &aside).unwrap();
    write(&held, "marker.txt", "held-bytes");
    std::fs::remove_dir_all(&source).unwrap();

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes",
        "{err}"
    );
    assert!(!aside.exists(), "{err}");
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "old-bytes",
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(held.join("marker.txt")).unwrap(),
        "held-bytes",
        "{err}"
    );
    assert_eq!(record.version.as_deref(), Some("1.1.0"), "{err}");
    assert_eq!(record.previous_version.as_deref(), Some("1.0.0"), "{err}");
}

/// A leftover held dir beside the matching previous dir is residue.
/// Recovery removes it. It does not replace that previous dir, and it does
/// not block `apply` or `rollback`.
#[test]
fn recovery_drops_held_when_matching_previous_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "new-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-1.0.0");
    let held_a = id_dir.join("package.held-7-3");
    let held_b = id_dir.join("package.held-8-1");
    write(&held_a, "marker.txt", "stranded");
    write(&held_b, "marker.txt", "also-stranded");
    std::fs::remove_dir_all(&source).unwrap();
    let version = record.version.clone();
    let previous_version = record.previous_version.clone();

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(!err.contains("will not overwrite"), "{err}");
    assert!(!err.contains("more than one held"), "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes",
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "old-bytes",
        "{err}"
    );
    assert!(!held_a.exists(), "{err}");
    assert!(!held_b.exists(), "{err}");
    assert_eq!(count_held(&id_dir), 0, "{err}");
    assert_eq!(record.version, version, "{err}");
    assert_eq!(record.previous_version, previous_version, "{err}");

    rollback(&mut record, &plugins).unwrap();
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "old-bytes"
    );
    assert!(!previous.exists());
    assert_eq!(record.version.as_deref(), Some("1.0.0"));
    assert!(record.previous_version.is_none());
}

/// Collision kill after `staged` → `package/` and before the held delete.
/// Recovery drops the stale held dir and leaves the other two trees.
/// `apply` then continues. `force` is set because the killed `package/`
/// does not match `tree_hash` (design §7).
#[test]
fn apply_heals_collision_kill_before_held_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let (plugins, mut record) = collision_kill_after_staged(tmp.path());
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-unknown");
    let held = id_dir.join("package.held-99-1");
    std::fs::remove_dir_all(tmp.path().join("source")).unwrap();
    let version = record.version.clone();
    let previous_version = record.previous_version.clone();

    let err = apply(&mut record, &plugins, None, true).unwrap_err();

    assert!(!err.contains("will not overwrite"), "{err}");
    assert!(!err.contains("recorded rollback source"), "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes",
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "live-bytes",
        "{err}"
    );
    assert!(!held.exists(), "{err}");
    assert_eq!(count_held(&id_dir), 0, "{err}");
    assert_eq!(count_prefixed(&id_dir, "package.superseded-"), 0, "{err}");
    assert_eq!(record.version, version, "{err}");
    assert_eq!(record.previous_version, previous_version, "{err}");

    let tmp = tempfile::tempdir().unwrap();
    let (plugins, mut record) = collision_kill_after_staged(tmp.path());
    let id_dir = plugins.join(&record.id);
    let err = apply(&mut record, &plugins, None, false).unwrap_err();
    assert!(err.contains("modified locally"), "{err}");
    assert!(!err.contains("will not overwrite"), "{err}");
    assert_eq!(count_held(&id_dir), 0, "{err}");
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package/marker.txt")).unwrap(),
        "new-bytes",
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package.previous-unknown/marker.txt")).unwrap(),
        "live-bytes",
        "{err}"
    );
    unversioned_package(&tmp.path().join("source"), "demo-plugin", "fetched-bytes");

    apply(&mut record, &plugins, None, true).unwrap();

    assert_eq!(count_held(&id_dir), 0);
    assert!(!id_dir.join("package.held-99-1").exists());
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package/marker.txt")).unwrap(),
        "fetched-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package.previous-unknown/marker.txt")).unwrap(),
        "new-bytes"
    );
    rollback(&mut record, &plugins).unwrap();
    assert_eq!(
        std::fs::read_to_string(id_dir.join("package/marker.txt")).unwrap(),
        "new-bytes"
    );
}

/// Same three-tree kill. `rollback` continues and restores the previous
/// tree, not the held bytes.
#[test]
fn rollback_heals_collision_kill_before_held_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let (plugins, mut record) = collision_kill_after_staged(tmp.path());
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-unknown");
    let held = id_dir.join("package.held-99-1");

    rollback(&mut record, &plugins).unwrap();

    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "live-bytes"
    );
    assert!(!previous.exists());
    assert!(!held.exists());
    assert_eq!(count_held(&id_dir), 0);
    assert_eq!(record.version.as_deref(), Some("unknown"));
    assert!(record.previous_version.is_none());
    let loaded = InstallStore::load(&plugins);
    let saved = saved_plugin(&record.id, &loaded);
    assert_eq!(saved.version.as_deref(), Some("unknown"));
    assert!(saved.previous_version.is_none());
}

/// A collision update whose held delete fails must still finish, and later
/// `apply` and `rollback` must work. Mode `0o555` makes `remove_dir_all` fail.
#[cfg(unix)]
#[test]
fn collision_update_parks_undeletable_held() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    unversioned_package(&source, "demo-plugin", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    unversioned_package(&source, "demo-plugin", "live-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    let id_dir = plugins.join(&record.id);
    let previous = id_dir.join("package.previous-unknown");
    std::fs::set_permissions(&previous, std::fs::Permissions::from_mode(0o555)).unwrap();
    unversioned_package(&source, "demo-plugin", "new-bytes");

    apply(&mut record, &plugins, None, false).unwrap();

    let package = id_dir.join("package");
    let previous = id_dir.join("package.previous-unknown");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes"
    );
    assert_eq!(
        std::fs::read_to_string(previous.join("marker.txt")).unwrap(),
        "live-bytes"
    );
    assert_eq!(count_held(&id_dir), 0);
    assert_eq!(record.version.as_deref(), Some("unknown"));
    assert_eq!(record.previous_version.as_deref(), Some("unknown"));
    let superseded = std::fs::read_dir(&id_dir)
        .unwrap()
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("package.superseded-")
                && entry.path().is_dir()
        })
        .map(|entry| entry.path())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(superseded.join("marker.txt")).unwrap(),
        "old-bytes"
    );

    rollback(&mut record, &plugins).unwrap();
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "live-bytes"
    );
    assert!(record.previous_version.is_none());

    unversioned_package(&source, "demo-plugin", "again-bytes");
    apply(&mut record, &plugins, None, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "again-bytes"
    );
    assert_eq!(count_held(&id_dir), 0);
    let _ = std::fs::set_permissions(&superseded, std::fs::Permissions::from_mode(0o755));
}

#[test]
fn recovery_names_every_held_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let (plugins, mut record) = park_rollback_in_held(tmp.path());
    let id_dir = plugins.join(&record.id);
    let package = id_dir.join("package");
    let held_a = id_dir.join("package.held-7-3");
    let held_b = id_dir.join("package.held-8-1");
    write(&held_b, "marker.txt", "other");

    let err = rollback(&mut record, &plugins).unwrap_err();

    assert!(err.contains(&held_a.display().to_string()), "{err}");
    assert!(err.contains(&held_b.display().to_string()), "{err}");
    assert!(!id_dir.join("package.previous-1.0.0").exists(), "{err}");
    assert!(held_a.is_dir());
    assert!(held_b.is_dir());
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "new-bytes"
    );
    assert_eq!(record.version.as_deref(), Some("1.1.0"));
    assert_eq!(record.previous_version.as_deref(), Some("1.0.0"));
}

#[test]
fn install_persists_tree_hash_and_blocks_local_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let hash = record.tree_hash.clone().expect("install stores tree_hash");
    assert!(hash.starts_with("sha256:"), "{hash}");
    assert_eq!(hash.len(), "sha256:".len() + 64);
    let loaded = InstallStore::load(&plugins);
    assert_eq!(loaded.records[0].tree_hash.as_deref(), Some(hash.as_str()));

    let package = plugins.join(&record.id).join("package");
    std::fs::write(package.join("marker.txt"), "local-edit").unwrap();
    versioned_package(&source, "demo-plugin", "1.1.0", "upstream");

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(err.contains("modified locally"), "{err}");
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "local-edit"
    );
    assert_eq!(record.status, PluginStatus::ModifiedLocally);
    let loaded = InstallStore::load(&plugins);
    assert_eq!(loaded.records[0].status, PluginStatus::ModifiedLocally);
}

#[test]
fn modified_locally_returns_status_write_error() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let plugins = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins);
    let mut record = install(&mut store, &plugins, &path_source(&source), None, None).unwrap();
    let package = plugins.join(&record.id).join("package");
    record.tree_hash = tree_hash(&package);
    std::fs::write(package.join("marker.txt"), "local-edit").unwrap();
    store.records.clear();
    store.save(&plugins).unwrap();

    let err = apply(&mut record, &plugins, None, false).unwrap_err();

    assert!(err.contains("modified locally"), "{err}");
    assert!(err.contains("not in the install record"), "{err}");
    assert_eq!(record.status, PluginStatus::ModifiedLocally);
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "local-edit"
    );
}

// ---------------------------------------------------------------------------
// Plugin manager: index, marketplaces and settings (design §3, §5, §11)
// ---------------------------------------------------------------------------

/// A manager rooted at `tmp` with a collecting sink.
fn test_manager(tmp: &Path, sink: Arc<CollectingSink>) -> Arc<PluginManager> {
    PluginManager::with_sink(tmp, tmp, None, sink)
}

/// Install `source` as a package under `tmp/plugins`; the record starts disabled.
fn seed_installed(tmp: &Path, source: &Path) -> InstallRecord {
    let plugins_dir = tmp.join("plugins");
    let mut store = InstallStore::load(&plugins_dir);
    let record = install(&mut store, &plugins_dir, &path_source(source), None, None).unwrap();
    store.save(&plugins_dir).unwrap();
    record
}

/// Change one field of an install record on disk.
fn edit_record(tmp: &Path, id: &str, edit: impl FnOnce(&mut InstallRecord)) {
    let plugins_dir = tmp.join("plugins");
    let mut store = InstallStore::load(&plugins_dir);
    edit(store.records.iter_mut().find(|r| r.id == id).unwrap());
    store.save(&plugins_dir).unwrap();
}

/// A `mcp.json` declaring one stdio server named `demo`.
fn write_demo_server(root: &Path) {
    Conformant::bare(root).mcp(r#"{"demo": {"type": "stdio", "command": "node"}}"#);
}

#[tokio::test]
async fn reload_scans_records_into_index() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    write_demo_server(&source);
    let record = seed_installed(tmp.path(), &source);
    edit_record(tmp.path(), &record.id, |r| r.enabled = true);

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();

    let index = manager.index();
    assert_eq!(index.plugins.len(), 1);
    assert_eq!(index.plugins[0].id, record.id);
    assert!(index.plugins[0].enabled);
    assert_eq!(index.skills.len(), 1);
    assert_eq!(index.skills[0].name, "demo");
    assert_eq!(index.servers.len(), 1);
    assert_eq!(index.servers[0].id, format!("plugin:{}:demo", record.id));
    assert!(index.subagents.is_empty());
}

#[tokio::test]
async fn plugin_subagent_view_publishes_the_registry_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    write(
        &source,
        "app.ducky/subagents/reviewer.md",
        "---\nname: Code Reviewer\ndescription: reviews code\n---\nReview.",
    );
    seed_installed(tmp.path(), &source);
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();

    let list = manager.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].subagents.len(), 1, "{:?}", list[0].subagents);
    assert_eq!(list[0].subagents[0].name, "Code Reviewer");
    assert_eq!(
        list[0].subagents[0].slug, "code-reviewer",
        "the UI badges on the same key `merge_subagent_defs` uses"
    );
}

#[tokio::test]
async fn disabled_plugin_subagent_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    write(
        &source,
        "app.ducky/subagents/reviewer.md",
        "---\nname: reviewer\ndescription: reviews code\n---\nReview.",
    );
    let record = seed_installed(tmp.path(), &source);
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    // installed disabled: no plugin subagents reach the registry
    manager.reload().unwrap();
    assert!(manager.index().subagents.is_empty());

    edit_record(tmp.path(), &record.id, |r| r.enabled = true);
    // `reload` re-scans the in-memory records loaded at construction, so a
    // fresh manager reads the edited one (same pattern as the other tests).
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();
    let index = manager.index();
    assert_eq!(index.subagents.len(), 1);
    assert_eq!(index.subagents[0].name, "reviewer");
    assert_eq!(index.subagents[0].plugin_id, record.id);

    let (defs, diagnostics) = manager.resolved_subagents(&builtin_tool_names(), &[]);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].id, format!("plugin:{}:reviewer", record.id));
    assert_eq!(defs[0].name, "reviewer");
}

#[tokio::test]
async fn missing_package_reports_error_status() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let record = seed_installed(tmp.path(), &source);
    std::fs::remove_dir_all(tmp.path().join("plugins").join(&record.id)).unwrap();

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();

    let list = manager.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].status, PluginStatus::Error);
    assert!(
        list[0]
            .diagnostics
            .iter()
            .any(|d| d.message.to_lowercase().contains("missing")),
        "{:?}",
        list[0].diagnostics
    );
    let store = InstallStore::load(&tmp.path().join("plugins"));
    assert_eq!(store.records.len(), 1);
    assert_eq!(store.records[0].id, record.id);
}

#[tokio::test]
async fn disabled_plugin_skills_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    seed_installed(tmp.path(), &source);

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();

    let index = manager.index();
    assert_eq!(index.plugins.len(), 1);
    assert!(!index.plugins[0].enabled);
    assert!(index.skills.is_empty());
    assert_eq!(index.plugins[0].skills.len(), 1);
    assert_eq!(index.plugins[0].skills[0].name, "demo");
}

#[tokio::test]
async fn disabled_server_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    write_demo_server(&source);
    let record = seed_installed(tmp.path(), &source);
    edit_record(tmp.path(), &record.id, |r| {
        r.enabled = true;
        r.disabled_servers = vec!["demo".to_string()];
    });

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();

    let index = manager.index();
    assert!(index.servers.is_empty());
    assert_eq!(index.plugins[0].servers.len(), 1);
    assert!(!index.plugins[0].servers[0].enabled);
}

#[tokio::test]
async fn debris_in_plugins_dir_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    seed_installed(tmp.path(), &source);
    write(tmp.path(), "plugins/stray.txt", "junk");
    std::fs::create_dir_all(tmp.path().join("plugins/unknown-dir")).unwrap();

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();

    assert_eq!(manager.list().len(), 1);
}

#[tokio::test]
async fn add_marketplace_local_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let market = tmp.path().join("market");
    write(
        &market,
        ".claude-plugin/marketplace.json",
        r#"{"name": "Acme", "plugins": [{"name": "demo", "source": "./demo"}]}"#,
    );
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    let summary = manager
        .add_marketplace(
            MarketplaceInput {
                source: market.display().to_string(),
                path: None,
            },
            None,
        )
        .unwrap();

    assert_eq!(summary.id, "market");
    assert_eq!(summary.name, "Acme");
    assert!(tmp.path().join("marketplaces/marketplaces.json").is_file());

    let catalog = manager.catalog(None).unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].name, "demo");
    assert_eq!(catalog[0].marketplace_id, "market");
    assert_eq!(catalog[0].source_kind, "path");
    assert!(catalog[0].installed.is_none());
    assert!(!catalog[0].update_available);
}

#[tokio::test]
async fn add_marketplace_github_subdir_reaches_the_source_path() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    let summary = manager
        .add_marketplace(
            MarketplaceInput {
                source: "microsoft/Agents".to_string(),
                path: Some("agent-plugins".to_string()),
            },
            None,
        )
        .unwrap();

    match &summary.source {
        PluginSource::Github { repo, path, .. } => {
            assert_eq!(repo, "microsoft/Agents");
            assert_eq!(path.as_deref(), Some("agent-plugins"));
        }
        other => panic!("expected a github source, got {other:?}"),
    }
    // The path survives the store round trip that `refresh_git` reads back.
    let saved = MarketplaceStore::load(&tmp.path().join("marketplaces"));
    let record = saved
        .records
        .iter()
        .find(|record| record.id == summary.id)
        .expect("the added record is persisted");
    match &record.source {
        PluginSource::Github { path, .. } => {
            assert_eq!(path.as_deref(), Some("agent-plugins"));
        }
        other => panic!("expected a github source, got {other:?}"),
    }
}

#[tokio::test]
async fn add_marketplace_path_applies_only_to_git_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let market = tmp.path().join("market");
    write(
        &market,
        "marketplace.json",
        r#"{"name": "Acme", "plugins": []}"#,
    );
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    let local = manager
        .add_marketplace(
            MarketplaceInput {
                source: market.display().to_string(),
                path: Some("agent-plugins".to_string()),
            },
            None,
        )
        .unwrap();
    match &local.source {
        PluginSource::Path { .. } => {}
        other => panic!("a local source has no subdirectory, got {other:?}"),
    }

    let remote = manager
        .add_marketplace(
            MarketplaceInput {
                source: "https://example.com/registry.json".to_string(),
                path: Some("agent-plugins".to_string()),
            },
            None,
        )
        .unwrap();
    match &remote.source {
        PluginSource::Url { .. } => {}
        other => panic!("a registry URL has no subdirectory, got {other:?}"),
    }
}

#[tokio::test]
async fn plugin_payloads_carry_the_trust_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let market = tmp.path().join("market");
    write(
        &market,
        "marketplace.json",
        r#"{"name": "Acme", "plugins": [{"name": "demo", "source": "./demo"}]}"#,
    );
    plugin_package(&market.join("demo"), "demo-plugin");
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager
        .add_marketplace(
            MarketplaceInput {
                source: market.display().to_string(),
                path: None,
            },
            None,
        )
        .unwrap();
    manager
        .install(Some("market"), Some("demo"), None, PolicyDefault::Content)
        .unwrap();

    let list = manager.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].trust_warning, super::manager::TRUST_WARNING);
    let detail = manager.detail(&list[0].id).unwrap().unwrap();
    assert_eq!(detail.plugin.trust_warning, super::manager::TRUST_WARNING);
    for entry in manager.catalog(None).unwrap() {
        assert_eq!(entry.trust_warning, super::manager::TRUST_WARNING);
    }
}

#[tokio::test]
async fn install_from_marketplace_resolves_relative_path() {
    let tmp = tempfile::tempdir().unwrap();
    let market = tmp.path().join("market");
    write(
        &market,
        ".claude-plugin/marketplace.json",
        r#"{"name": "Acme", "plugins": [{"name": "demo-entry", "source": "./packages/demo"}]}"#,
    );
    plugin_package(&market.join("packages/demo"), "demo-plugin");

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager
        .add_marketplace(
            MarketplaceInput {
                source: market.display().to_string(),
                path: None,
            },
            None,
        )
        .unwrap();

    let id = manager
        .install(
            Some("market"),
            Some("demo-entry"),
            None,
            PolicyDefault::Content,
        )
        .unwrap();

    assert_eq!(id, "demo-plugin");
    let list = manager.list();
    assert_eq!(list.len(), 1);
    match &list[0].source {
        PluginSource::Path { path } => {
            assert!(Path::new(path).is_absolute(), "{path}");
            assert!(path.contains("packages"), "{path}");
        }
        other => panic!("expected a path source, got {other:?}"),
    }
    assert!(tmp
        .path()
        .join("plugins/demo-plugin/package/plugin.json")
        .is_file());
}

#[tokio::test]
async fn add_marketplace_rejects_bundled_id() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    let err = manager
        .add_marketplace(
            MarketplaceInput {
                source: "acme/tools".to_string(),
                path: None,
            },
            Some(super::bundled::BUNDLED_ID.to_string()),
        )
        .unwrap_err();

    assert!(err.contains("ducky-official"), "{err}");
    // The seed is the only record; the refused add left it alone.
    assert_eq!(manager.marketplaces().len(), 1);
    assert_eq!(manager.marketplaces()[0].id, "ducky-official");
}

#[tokio::test]
async fn remove_marketplace_requires_confirm() {
    let tmp = tempfile::tempdir().unwrap();
    let market = tmp.path().join("market");
    write(
        &market,
        "marketplace.json",
        r#"{"name": "Acme", "plugins": []}"#,
    );

    // The manager loads its stores at construction: seed the installed plugin
    // from this marketplace first.
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let plugins_dir = tmp.path().join("plugins");
    let mut store = InstallStore::load(&plugins_dir);
    install(
        &mut store,
        &plugins_dir,
        &path_source(&source),
        None,
        Some("market"),
    )
    .unwrap();
    store.save(&plugins_dir).unwrap();

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    let summary = manager
        .add_marketplace(
            MarketplaceInput {
                source: market.display().to_string(),
                path: None,
            },
            None,
        )
        .unwrap();

    let err = manager.remove_marketplace(&summary.id, false).unwrap_err();
    assert!(err.contains("confirm"), "{err}");
    assert!(manager
        .marketplaces()
        .iter()
        .any(|item| item.id == summary.id));

    manager.remove_marketplace(&summary.id, true).unwrap();
    assert!(!manager
        .marketplaces()
        .iter()
        .any(|item| item.id == summary.id));
}

/// Hand-seed one marketplace record before the manager loads its stores.
fn seed_marketplace(tmp: &Path, id: &str, bundled: bool) {
    let mut store = MarketplaceStore::load(&tmp.join("marketplaces"));
    store.records.push(MarketplaceRecord {
        id: id.to_string(),
        name: "Ducky Official".to_string(),
        source: PluginSource::Path {
            path: tmp.join("bundled.json").display().to_string(),
        },
        registry_path: "bundled.json".to_string(),
        auto_refresh: false,
        last_refreshed_at: None,
        resolved_sha: None,
        bundled,
        hidden: false,
        error: None,
        extra: Default::default(),
    });
    store.save(&tmp.join("marketplaces")).unwrap();
}

#[tokio::test]
async fn bundled_marketplace_cannot_be_removed() {
    let tmp = tempfile::tempdir().unwrap();
    seed_marketplace(tmp.path(), "ducky-official", true);

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    let err = manager
        .remove_marketplace("ducky-official", true)
        .unwrap_err();
    assert!(err.contains("bundled"), "{err}");
    assert_eq!(manager.marketplaces().len(), 1);
}

/// The reserved id is refused even when a hand-edited record clears the
/// `bundled` flag (design §5, review fix round 1).
#[tokio::test]
async fn reserved_marketplace_id_cannot_be_removed() {
    let tmp = tempfile::tempdir().unwrap();
    seed_marketplace(tmp.path(), "ducky-official", false);

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    let err = manager
        .remove_marketplace(super::bundled::BUNDLED_ID, true)
        .unwrap_err();
    assert!(err.contains("bundled"), "{err}");
    assert_eq!(manager.marketplaces().len(), 1);
}

// ---------------------------------------------------------------------------
// Task 11: the bundled official marketplace (design §5)
// ---------------------------------------------------------------------------

#[test]
fn seeds_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("marketplaces");
    let mut store = MarketplaceStore::load(&dir);

    assert!(super::bundled::ensure_seeded(&mut store, None));
    assert!(!super::bundled::ensure_seeded(&mut store, None));
    assert_eq!(store.records.len(), 1);
    assert_eq!(store.records[0].id, "ducky-official");

    // A later startup loads the record from disk: no second seed, no rewrite
    // and a hand edit survives. A third call changes nothing on disk either.
    store.save(&dir).unwrap();
    let mut reloaded = MarketplaceStore::load(&dir);
    reloaded.records[0].name = "My Official".to_string();
    reloaded.save(&dir).unwrap();
    let bytes = std::fs::read(dir.join("marketplaces.json")).unwrap();
    assert!(!super::bundled::ensure_seeded(&mut reloaded, None));
    assert_eq!(reloaded.records.len(), 1);
    assert_eq!(reloaded.records[0].name, "My Official");
    assert_eq!(std::fs::read(dir.join("marketplaces.json")).unwrap(), bytes);
}

#[test]
fn checked_in_copy_is_only_used_without_a_resource_dir() {
    // A dev build has no packaged resource directory; the checked-in copy is
    // the fallback, so the bundled registry still loads.
    let registry = super::bundled::load_registry(None).unwrap();
    assert_eq!(registry.name, "Ducky Official");
    assert!(registry.registry_path.is_file());
    assert!(
        registry
            .registry_path
            .ends_with("resources/ducky-official.json"),
        "{}",
        registry.registry_path.display()
    );

    // A resource dir without the file is a packaging bug, not a dev build:
    // the missing-file error must surface, never the checked-in copy
    // (design §5, review fix round 1).
    let empty = tempfile::tempdir().unwrap();
    let err = super::bundled::load_registry(Some(empty.path())).unwrap_err();
    assert!(err.contains("missing"), "{err}");
    assert!(err.contains("ducky-official.json"), "{err}");
}

#[tokio::test]
async fn missing_packaged_file_is_not_masked_by_the_dev_copy() {
    // Packaging bug: a resource directory exists but the file is missing.
    // The manager must seed the record with the missing-file diagnostic
    // instead of silently reading the checked-in dev copy, which is only
    // for builds without a resource directory (design §5, review fix
    // round 1).
    let packaged = tempfile::tempdir().unwrap();
    let resources = packaged.path().join("resources");
    std::fs::create_dir_all(&resources).unwrap();
    let manager = PluginManager::with_sink(
        packaged.path(),
        packaged.path(),
        Some(resources.as_path()),
        Arc::new(CollectingSink::default()),
    );
    let record = manager
        .marketplaces()
        .into_iter()
        .find(|record| record.id == "ducky-official")
        .expect("the bundled record is seeded");
    let err = record
        .error
        .expect("a missing packaged file must be diagnosed, not masked");
    assert!(err.contains("missing"), "{err}");
    assert!(err.contains("ducky-official.json"), "{err}");
    assert_eq!(record.entry_count, 0);

    // The dev path (no resource dir) still reads the checked-in file.
    let dev = tempfile::tempdir().unwrap();
    let manager = test_manager(dev.path(), Arc::new(CollectingSink::default()));
    let record = manager
        .marketplaces()
        .into_iter()
        .find(|record| record.id == "ducky-official")
        .expect("the bundled record is seeded");
    assert!(record.error.is_none(), "{record:?}");
    match &record.source {
        PluginSource::Path { path } => {
            assert!(path.ends_with("resources/ducky-official.json"), "{path}")
        }
        other => panic!("expected a path source, got {other:?}"),
    }
}

#[test]
fn bundled_registry_parses() {
    // The shipped file is the normalised Ducky dialect: it parses unchanged,
    // with zero entries and therefore no diagnostics.
    let registry = super::bundled::load_registry(None).unwrap();
    assert_eq!(registry.name, "Ducky Official");
    assert!(registry.description.is_some());
    assert!(registry.entries.is_empty());
    assert!(
        registry
            .registry_path
            .ends_with("resources/ducky-official.json"),
        "{}",
        registry.registry_path.display()
    );
}

#[tokio::test]
async fn broken_resource_does_not_stop_startup() {
    // A packaging mistake must not brick the app: the manager still builds,
    // the marketplace is listed and its error carries the diagnosis.
    let tmp = tempfile::tempdir().unwrap();
    let resources = tmp.path().join("resources");
    std::fs::create_dir_all(&resources).unwrap();
    std::fs::write(resources.join("ducky-official.json"), "{not json").unwrap();

    let manager = PluginManager::with_sink(
        tmp.path(),
        tmp.path(),
        Some(resources.as_path()),
        Arc::new(CollectingSink::default()),
    );

    let record = manager
        .marketplaces()
        .into_iter()
        .find(|record| record.id == "ducky-official")
        .expect("the bundled record is seeded even when its file is broken");
    assert!(record.bundled);
    assert!(record.error.is_some(), "{record:?}");
    assert_eq!(record.entry_count, 0);
    assert!(manager.catalog(None).unwrap().is_empty());
}

#[tokio::test]
async fn seeded_record_is_bundled_and_readonly() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    let record = manager
        .marketplaces()
        .into_iter()
        .find(|record| record.id == "ducky-official")
        .expect("the bundled record is seeded at startup");
    assert!(record.bundled);
    assert!(record.hidden);

    let err = manager
        .remove_marketplace("ducky-official", true)
        .unwrap_err();
    assert!(err.contains("bundled"), "{err}");
    assert!(manager
        .marketplaces()
        .iter()
        .any(|item| item.id == "ducky-official"));
    drop(manager);

    // A second startup must not rewrite the file: hand-edit it into compact
    // JSON, and a rewrite would re-pretty-print it.
    let file = tmp.path().join("marketplaces/marketplaces.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    value["marketplaces"][0]["name"] = serde_json::json!("My Official");
    let compact = serde_json::to_string(&value).unwrap();
    std::fs::write(&file, &compact).unwrap();

    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    let record = manager
        .marketplaces()
        .into_iter()
        .find(|record| record.id == "ducky-official")
        .expect("the bundled record survives a restart");
    assert_eq!(record.name, "My Official");
    assert!(record.bundled);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), compact);
}

#[tokio::test]
async fn bundled_cannot_be_shadowed() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    let err = manager
        .add_marketplace(
            MarketplaceInput {
                source: "acme/tools".to_string(),
                path: None,
            },
            Some("ducky-official".to_string()),
        )
        .unwrap_err();

    assert!(err.contains("ducky-official"), "{err}");
    assert_eq!(manager.marketplaces().len(), 1);
    assert_eq!(manager.marketplaces()[0].id, "ducky-official");
}

#[tokio::test]
async fn settings_round_trip() {
    let defaults = PluginSettings::default();
    assert!(defaults.skills_enabled);
    assert_eq!(defaults.policy_default, PolicyDefault::Content);
    assert_eq!(defaults.marketplace_refresh_hours, 6);
    assert_eq!(defaults.update_check_hours, 6);

    let tmp = tempfile::tempdir().unwrap();
    let store = crate::config::Store::new(tmp.path(), tmp.path().to_path_buf()).unwrap();
    {
        let mut cfg = store.config.lock().unwrap();
        cfg.settings.plugins.skills_enabled = false;
        cfg.settings.plugins.policy_default = PolicyDefault::Manual;
        cfg.settings.plugins.marketplace_refresh_hours = 0;
        cfg.settings.plugins.update_check_hours = 12;
    }
    store.save_config().unwrap();

    let reloaded = crate::config::Store::new(tmp.path(), tmp.path().to_path_buf()).unwrap();
    let plugins = reloaded.config.lock().unwrap().settings.plugins.clone();
    assert!(!plugins.skills_enabled);
    assert_eq!(plugins.policy_default, PolicyDefault::Manual);
    assert_eq!(plugins.marketplace_refresh_hours, 0);
    assert_eq!(plugins.update_check_hours, 12);
}

#[tokio::test]
async fn install_emits_progress_and_keeps_servers_off() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    write_demo_server(&source);
    let sink = Arc::new(CollectingSink::default());
    let manager = test_manager(tmp.path(), sink.clone());

    let id = manager
        .install(
            None,
            None,
            Some(&path_source(&source)),
            PolicyDefault::Content,
        )
        .unwrap();
    assert_eq!(id, "demo-plugin");

    let events = sink.events.lock().unwrap().clone();
    // Every phase of one install carries the record's real id, so the UI can
    // attach all three to one row.
    let phases: Vec<(String, String)> = events
        .iter()
        .filter_map(|event| match event {
            BackendEvent::PluginProgress {
                plugin_id, phase, ..
            } => Some((plugin_id.clone(), phase.clone())),
            _ => None,
        })
        .collect();
    for phase in ["fetch", "validate", "place"] {
        let found = phases.iter().find(|(_, name)| name == phase);
        let (found_id, _) = found.unwrap_or_else(|| panic!("phase `{phase}`: {phases:?}"));
        assert_eq!(found_id, &id, "phase `{phase}`: {phases:?}");
    }
    assert!(events.iter().any(
        |event| matches!(event, BackendEvent::PluginsChanged { reason } if reason == "install")
    ));

    let list = manager.list();
    assert_eq!(list.len(), 1);
    assert!(!list[0].enabled);
    assert!(list[0].disabled_servers.contains(&"demo".to_string()));
}

/// A server the new revision adds is off until the user consents; a server
/// the user already enabled stays enabled (design §6).
#[tokio::test]
async fn update_switches_new_servers_off() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    write_demo_server(&source);
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    let id = manager
        .install(
            None,
            None,
            Some(&path_source(&source)),
            PolicyDefault::Content,
        )
        .unwrap();
    manager.set_enabled(&id, true).unwrap();
    manager.set_server_enabled(&id, "demo", true).unwrap();

    // The new revision adds "added".
    Conformant::bare(&source).mcp(
        r#"{"demo": {"type": "stdio", "command": "node"}, "added": {"type": "stdio", "command": "node"}}"#,
    );
    manager.update(&id, false).unwrap();

    let list = manager.list();
    assert!(
        !list[0].disabled_servers.contains(&"demo".to_string()),
        "{:?}",
        list[0].disabled_servers
    );
    assert!(
        list[0].disabled_servers.contains(&"added".to_string()),
        "{:?}",
        list[0].disabled_servers
    );

    let index = manager.index();
    assert_eq!(index.servers.len(), 1, "{:?}", index.servers);
    assert!(
        index.servers[0].id.ends_with(":demo"),
        "{}",
        index.servers[0].id
    );
}

/// A local-edit refusal writes `ModifiedLocally` to the record before it
/// fails; the index is rebuilt before the error goes out, so the UI sees the
/// new status without a restart (review fix round 1).
#[tokio::test]
async fn local_edit_update_error_is_visible_without_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    versioned_package(&source, "demo-plugin", "1.0.0", "old-bytes");
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));

    let id = manager
        .install(
            None,
            None,
            Some(&path_source(&source)),
            PolicyDefault::Content,
        )
        .unwrap();
    let package = tmp.path().join("plugins").join(&id).join("package");
    write(&package, "marker.txt", "local-edit");
    versioned_package(&source, "demo-plugin", "1.1.0", "upstream");
    assert_eq!(manager.list()[0].status, PluginStatus::InstalledDisabled);

    let err = manager.update(&id, false).unwrap_err();
    assert!(err.contains("modified locally"), "{err}");

    assert_eq!(manager.list()[0].status, PluginStatus::ModifiedLocally);
    let store = InstallStore::load(&tmp.path().join("plugins"));
    assert_eq!(store.records[0].status, PluginStatus::ModifiedLocally);
    assert_eq!(
        std::fs::read_to_string(package.join("marker.txt")).unwrap(),
        "local-edit"
    );
}

/// A hand-edited unsafe id must not publish or reveal a path outside the
/// plugins directory, and must not break the other records (Task 5 class,
/// review fix round 1).
#[tokio::test]
async fn unsafe_id_is_refused_by_detail_folder_and_the_view() {
    for id in ["..", ".. "] {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        plugin_package(&source, "demo-plugin");
        let keeper = seed_installed(tmp.path(), &source);
        let plugins_dir = tmp.path().join("plugins");
        let mut store = InstallStore::load(&plugins_dir);
        store
            .records
            .push(install_record(id, "bad", path_source(&source)));
        store.save(&plugins_dir).unwrap();
        // `plugins/../package` exists, so revealing it would be visible.
        std::fs::create_dir_all(tmp.path().join("package")).unwrap();

        let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
        let list = manager.list();
        assert_eq!(list.len(), 2, "id `{id}`");
        let bad = list.iter().find(|plugin| plugin.id == id).unwrap();
        assert_eq!(bad.status, PluginStatus::Error, "id `{id}`");
        assert!(
            bad.diagnostics
                .iter()
                .any(|diag| diag.message.contains("not a safe directory name")),
            "id `{id}`: {:?}",
            bad.diagnostics
        );
        assert!(bad.package_dir.is_empty(), "id `{id}`: {}", bad.package_dir);
        assert!(bad.data_dir.is_empty(), "id `{id}`: {}", bad.data_dir);

        assert!(manager.detail(id).is_err(), "id `{id}`");
        assert!(manager.folder(id, None).is_err(), "id `{id}`");
        assert!(manager.folder(id, Some("data")).is_err(), "id `{id}`");

        // The other records stay intact and useful.
        let row = list.iter().find(|plugin| plugin.id == keeper.id).unwrap();
        assert_eq!(row.status, PluginStatus::InstalledDisabled, "id `{id}`");
        assert!(
            Path::new(&row.package_dir).join("plugin.json").is_file(),
            "id `{id}`: {}",
            row.package_dir
        );
        assert!(manager.detail(&keeper.id).unwrap().is_some(), "id `{id}`");
        assert!(manager.folder(&keeper.id, None).is_ok(), "id `{id}`");
    }
}

/// §6: the trust sheet shows a remote server's host, never the full URL — a
/// query string can carry a token.
#[tokio::test]
async fn remote_server_view_shows_host_only() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin").mcp(
        r#"{"remote": {"type": "streamable-http", "url": "https://mcp.example.com:8443/mcp?token=secret"}}"#,
    );
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager
        .install(
            None,
            None,
            Some(&path_source(&source)),
            PolicyDefault::Content,
        )
        .unwrap();

    let list = manager.list();
    let server = list[0]
        .servers
        .iter()
        .find(|server| server.name == "remote")
        .unwrap();
    assert_eq!(server.url.as_deref(), Some("mcp.example.com:8443"));
    let url = server.url.as_deref().unwrap();
    assert!(!url.contains("token") && !url.contains("secret"), "{url}");
}

/// The layout rejects a URL with userinfo before it can reach a view; the
/// host helper itself must still drop userinfo and the query.
#[test]
fn display_host_never_includes_userinfo_or_query() {
    assert_eq!(
        display_host("https://user:pass@mcp.example.com/mcp?token=secret#frag"),
        "mcp.example.com"
    );
    assert_eq!(
        display_host("https://user:pass@mcp.example.com:8443/mcp?token=secret"),
        "mcp.example.com:8443"
    );
}

// ---------------------------------------------------------------------------
// Skills at runtime: roots, precedence, prompt block and bodies (design §4, §8)
// ---------------------------------------------------------------------------

/// A synthetic resolved skill pointing at `dir`; nothing on disk is checked.
fn skill_at(name: &str, description: &str, dir: &Path, origin: SkillOrigin) -> ResolvedSkill {
    ResolvedSkill {
        id: origin.id_for(name),
        name: name.to_string(),
        description: description.to_string(),
        dir: dir.to_path_buf(),
        origin,
        shadowed: None,
    }
}

#[test]
fn precedence_user_beats_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let record = seed_installed(tmp.path(), &source);
    edit_record(tmp.path(), &record.id, |r| r.enabled = true);
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();
    let index = manager.index();

    // the user root wins the name `demo`
    write(
        tmp.path(),
        "skills/demo/SKILL.md",
        "---\nname: demo\ndescription: user demo\n---\nUser body.",
    );

    let skills = collect(
        &index,
        &tmp.path().join("skills"),
        &tmp.path().join("ws"),
        &tmp.path().join("agents"),
        true,
    );

    let user = skills
        .iter()
        .find(|s| s.id == "user:demo")
        .expect("the user skill wins");
    assert_eq!(user.description, "user demo");
    assert!(user.shadowed.is_none());
    let plugin = skills
        .iter()
        .find(|s| s.id == format!("plugin:{}:demo", record.id))
        .expect("the shadowed plugin skill is still listed");
    assert_eq!(plugin.shadowed.as_deref(), Some("user:demo"));
}

/// The `/skills` payload (`skills_list`) spans every root and carries the
/// shadow mark, exactly like the prompt block's list — nothing about it is
/// plugin-only.
#[test]
fn skills_list_payload_lists_every_root_with_shadowing() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    plugin_package(&source, "demo-plugin");
    let record = seed_installed(tmp.path(), &source);
    edit_record(tmp.path(), &record.id, |r| r.enabled = true);
    let manager = test_manager(tmp.path(), Arc::new(CollectingSink::default()));
    manager.reload().unwrap();

    // the user root wins the name `demo`; the other two filesystem roots
    // contribute skills of their own
    write(
        tmp.path(),
        "skills/demo/SKILL.md",
        "---\nname: demo\ndescription: user demo\n---\nUser body.",
    );
    write(
        tmp.path(),
        "ws/.ducky/skills/ws-only/SKILL.md",
        "---\nname: ws-only\ndescription: ws skill\n---\nBody.",
    );
    write(
        tmp.path(),
        ".agents/skills/agents-only/SKILL.md",
        "---\nname: agents-only\ndescription: agents skill\n---\nBody.",
    );

    let payload = summarize(&manager.resolved_skills(&tmp.path().join("ws"), true));

    let user = payload
        .iter()
        .find(|s| s.id == "user:demo")
        .expect("the user-root skill is in the payload");
    assert_eq!(user.origin, "user");
    assert_eq!(user.shadowed, None);

    let plugin = payload
        .iter()
        .find(|s| s.id == format!("plugin:{}:demo", record.id))
        .expect("the plugin skill is in the payload alongside the user one");
    assert_eq!(plugin.origin, "plugin: demo-plugin");
    assert_eq!(plugin.shadowed.as_deref(), Some("user:demo"));

    assert!(
        payload
            .iter()
            .any(|s| s.id == "workspace:ws-only" && s.origin == "workspace"),
        "{payload:?}"
    );
    assert!(
        payload
            .iter()
            .any(|s| s.id == "agents:agents-only" && s.origin == "agents"),
        "{payload:?}"
    );
}

#[test]
fn bare_name_resolves_by_precedence() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "user-root/deploy/SKILL.md",
        "---\nname: deploy\ndescription: user deploy\n---\nBody.",
    );
    write(
        tmp.path(),
        "ws/.ducky/skills/deploy/SKILL.md",
        "---\nname: deploy\ndescription: ws deploy\n---\nBody.",
    );

    let skills = collect(
        &PluginIndex::default(),
        &tmp.path().join("user-root"),
        &tmp.path().join("ws/.ducky/skills"),
        &tmp.path().join("agents"),
        true,
    );

    let resolved =
        crate::builtin::skills::resolve_id(&skills, "deploy").expect("bare name resolves");
    assert_eq!(resolved.id, "user:deploy");
    assert_eq!(resolved.description, "user deploy");
}

#[test]
fn prompt_block_caps_at_60() {
    let skills: Vec<ResolvedSkill> = (0..61)
        .map(|i| {
            skill_at(
                &format!("skill-{i}"),
                "does a thing",
                Path::new("/tmp"),
                SkillOrigin::User,
            )
        })
        .collect();

    let block = prompt_block(&skills, 60).expect("61 skills still render a block");

    let entries = block.lines().filter(|line| line.starts_with("- `")).count();
    assert_eq!(entries, 60, "{block}");
    assert!(block.contains("1 more"), "{block}");
    assert!(block.contains("/skills"), "{block}");
}

#[test]
fn prompt_block_truncates_on_char_boundary() {
    let long = "é".repeat(300);
    let skills = vec![skill_at("uni", &long, Path::new("/tmp"), SkillOrigin::User)];

    let block = prompt_block(&skills, 60).expect("block");

    let line = block
        .lines()
        .find(|line| line.starts_with("- `"))
        .expect("an entry line");
    let (_, rest) = line.split_once(" — ").expect("separator");
    let desc = rest.rsplit_once(" (").expect("origin suffix").0;
    // 200 characters, not 200 bytes; a byte split would have panicked above
    assert_eq!(desc.chars().count(), 200, "{line}");
    assert!(!desc.contains(&"é".repeat(201)));
}

#[test]
fn prompt_block_shows_a_bare_name_when_unique() {
    let unique = skill_at(
        "brainstorming",
        "turn an idea into a design",
        Path::new("/tmp"),
        SkillOrigin::User,
    );
    let block = prompt_block(std::slice::from_ref(&unique), 60).expect("block");
    assert!(
        block.contains("- `brainstorming` — turn an idea into a design (user)"),
        "{block}"
    );

    // a duplicate name cannot be told apart by its bare name: full ids
    let mut shadowed = skill_at(
        "brainstorming",
        "a plugin copy",
        Path::new("/tmp"),
        SkillOrigin::Plugin {
            id: "acme".into(),
            name: "acme".into(),
        },
    );
    shadowed.shadowed = Some("user:brainstorming".into());
    let block = prompt_block(&[unique, shadowed], 60).expect("block");
    assert!(block.contains("- `user:brainstorming`"), "{block}");
    assert!(block.contains("- `plugin:acme:brainstorming`"), "{block}");
    assert!(block.contains("shadowed by user:brainstorming"), "{block}");
}

#[test]
fn prompt_block_absent_when_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "skills/demo/SKILL.md",
        "---\nname: demo\ndescription: demo skill\n---\nBody.",
    );

    let skills = collect(
        &PluginIndex::default(),
        &tmp.path().join("skills"),
        &tmp.path().join("ws"),
        &tmp.path().join("agents"),
        false,
    );

    assert!(skills.is_empty(), "the master switch hides every root");
    assert!(prompt_block(&skills, 60).is_none());
}

#[test]
fn prompt_block_absent_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let skills = collect(
        &PluginIndex::default(),
        &tmp.path().join("skills"),
        &tmp.path().join("ws"),
        &tmp.path().join("agents"),
        true,
    );

    assert!(skills.is_empty());
    assert!(prompt_block(&skills, 60).is_none());
}

#[test]
fn load_body_strips_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "demo/SKILL.md",
        "---\nname: demo\ndescription: demo skill\n---\nBody line one.\nBody line two.",
    );
    let skill = skill_at(
        "demo",
        "demo skill",
        &tmp.path().join("demo"),
        SkillOrigin::User,
    );

    let body = load_body(&skill).unwrap();

    assert_eq!(body, "Body line one.\nBody line two.");
}

#[test]
fn load_body_is_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let big = "a".repeat(40 * 1024);
    write(
        tmp.path(),
        "demo/SKILL.md",
        &format!("---\nname: demo\ndescription: big\n---\n{big}"),
    );
    let skill = skill_at("demo", "big", &tmp.path().join("demo"), SkillOrigin::User);

    let body = load_body(&skill).unwrap();

    assert!(body.len() <= 32 * 1024, "{} bytes", body.len());
    assert!(body.contains("truncated"), "{}", &body[body.len() - 120..]);
}

#[test]
fn read_skill_file_refuses_escape() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "demo/SKILL.md",
        "---\nname: demo\ndescription: demo skill\n---\nBody.",
    );
    write(tmp.path(), "outside.txt", "secret");
    let skill = skill_at("demo", "demo", &tmp.path().join("demo"), SkillOrigin::User);

    assert!(
        crate::builtin::skills::read_skill_file(&skill, "../outside.txt").is_err(),
        "`..` must not escape"
    );
    assert!(
        crate::builtin::skills::read_skill_file(&skill, "/etc/hosts").is_err(),
        "an absolute path is refused even when it resolves inside"
    );
    assert!(
        crate::builtin::skills::read_skill_file(&skill, "missing.txt").is_err(),
        "a missing file is an error"
    );

    #[cfg(unix)]
    {
        symlink(
            &tmp.path().join("outside.txt"),
            &tmp.path().join("demo/link.txt"),
        );
        assert!(
            crate::builtin::skills::read_skill_file(&skill, "link.txt").is_err(),
            "a symlink escape must not be followed"
        );
    }
}

#[test]
fn read_skill_file_outside_reports_containment_even_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "demo/SKILL.md",
        "---\nname: demo\ndescription: demo skill\n---\nBody.",
    );
    write(tmp.path(), "secrets.json", "{}");
    let skill = skill_at("demo", "demo", &tmp.path().join("demo"), SkillOrigin::User);

    // containment is checked first, so the refusal does not reveal whether a
    // file elsewhere on disk exists
    let exists = crate::builtin::skills::read_skill_file(&skill, "../secrets.json").unwrap_err();
    assert!(exists.contains("outside the skill directory"), "{exists}");
    let missing = crate::builtin::skills::read_skill_file(&skill, "../ghost.json").unwrap_err();
    assert!(missing.contains("outside the skill directory"), "{missing}");

    // inside the directory, a missing file keeps the missing-file refusal
    let inside = crate::builtin::skills::read_skill_file(&skill, "missing.txt").unwrap_err();
    assert!(inside.contains("no such file"), "{inside}");
}

/// A `SKILL.md` symlinked outside its skill directory is skipped at discovery
/// and refused by the body reader, for the filesystem roots too (the plugin
/// layer already guards its own).
#[cfg(unix)]
#[test]
fn escaping_skill_md_symlink_is_skipped_and_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "skills/demo/README.md", "x");
    write(
        tmp.path(),
        "skills/secret.md",
        "---\nname: demo\ndescription: secret\n---\nSecret.",
    );
    symlink(
        &tmp.path().join("skills/secret.md"),
        &tmp.path().join("skills/demo/SKILL.md"),
    );
    let skill = skill_at(
        "demo",
        "secret",
        &tmp.path().join("skills/demo"),
        SkillOrigin::User,
    );

    let skills = collect(
        &PluginIndex::default(),
        &tmp.path().join("skills"),
        &tmp.path().join("ws"),
        &tmp.path().join("agents"),
        true,
    );
    assert!(
        skills.is_empty(),
        "discovery skipped the escape: {skills:?}"
    );

    let err = load_body(&skill).unwrap_err();
    assert!(err.contains("outside the skill directory"), "{err}");
}

#[test]
fn read_skill_file_reads_references() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "demo/references/REFERENCE.md",
        "# Reference\nHello.",
    );
    let skill = skill_at("demo", "demo", &tmp.path().join("demo"), SkillOrigin::User);

    let text = crate::builtin::skills::read_skill_file(&skill, "references/REFERENCE.md").unwrap();

    assert!(text.contains("Hello."), "{text}");
}

#[test]
fn unknown_skill_id_lists_close_matches() {
    let skills = vec![
        skill_at("deploy", "ship it", Path::new("/tmp"), SkillOrigin::User),
        skill_at(
            "debug",
            "find bugs",
            Path::new("/tmp"),
            SkillOrigin::Workspace,
        ),
    ];

    let err = crate::builtin::skills::execute(
        crate::builtin::skills::LOAD_SKILL,
        &serde_json::json!({ "id": "deplo" }),
        &skills,
    )
    .unwrap_err();

    assert!(err.contains("deploy"), "{err}");
}
