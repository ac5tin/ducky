//! Install records, staging, the version ladder, install and uninstall
//! (design §3, §6, §7).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::diagnostics::{DiagLevel, Diagnostic};
use super::layout::{discover, Discovered};
use super::manifest::{self, Layout};
use super::marketplace::{
    clone_repo, git_in, git_target, join_diagnostics, parse_git_remote, Entry, HttpClient,
    HttpResponse, PluginSource,
};
use super::path::resolve_within;

/// Update policy of one installed plugin (design §7).
///
/// Task 12's TypeScript types depend on the exact lowercase strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdatePolicy {
    #[default]
    Auto,
    Manual,
}

/// The update a check found, cached on the record (design §7).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub resolved_sha: Option<String>,
}

/// Lifecycle state of one installed plugin (design §11).
///
/// Task 12's TypeScript types depend on the exact snake_case strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    #[default]
    InstalledDisabled,
    Enabled,
    Invalid,
    UpdateAvailable,
    ModifiedLocally,
    Error,
}

/// One installed plugin, as persisted in `plugins/installed.json` (design §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub marketplace: Option<String>,
    pub source: PluginSource,
    #[serde(default)]
    pub version: Option<String>,
    pub installed_at: String,
    pub layout: Layout,
    #[serde(default)]
    pub enabled: bool,
    pub update_policy: UpdatePolicy,
    #[serde(default)]
    pub disabled_servers: Vec<String>,
    #[serde(default)]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub tree_hash: Option<String>,
    #[serde(default)]
    pub last_checked_at: Option<String>,
    #[serde(default)]
    pub available_update: Option<AvailableUpdate>,
    #[serde(default)]
    pub status: PluginStatus,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    /// The git commit this revision was resolved from; update checks compare
    /// against it (design §7). The plan's record sketch omitted it because the
    /// spec keeps `resolvedSha` inside `source`; the record needs it for
    /// versionless git updates, and `PluginSource` has no such field.
    #[serde(default)]
    pub resolved_sha: Option<String>,
    /// sha256 of a non-git revision, for the ladder's fourth rung (design §6).
    /// No source kind produces one today (the archive flow is out of scope).
    #[serde(default)]
    pub resolved_sha256: Option<String>,
    /// Unknown fields round-trip (design §3).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// `plugins/installed.json`: every install record (design §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallStore {
    #[serde(default = "store_version")]
    pub version: u32,
    #[serde(rename = "plugins", default)]
    pub records: Vec<InstallRecord>,
    /// Unknown fields round-trip (design §3).
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
    /// Load problems, kept beside the records and never persisted.
    #[serde(skip)]
    pub diagnostics: Vec<Diagnostic>,
}

fn store_version() -> u32 {
    1
}

impl Default for InstallStore {
    fn default() -> Self {
        Self {
            version: store_version(),
            records: Vec::new(),
            extra: BTreeMap::new(),
            diagnostics: Vec::new(),
        }
    }
}

impl InstallStore {
    /// Load `dir/installed.json`.
    ///
    /// A missing file is an empty store. A corrupt file is kept exactly as it
    /// is, the store starts empty, and an error diagnostic explains why: a
    /// parse failure must never destroy user data.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("installed.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(store) => store,
                Err(err) => Self::corrupt(format!("cannot parse installed.json: {err}")),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(err) => Self::corrupt(format!("cannot read installed.json: {err}")),
        }
    }

    fn corrupt(message: String) -> Self {
        let mut store = Self::default();
        store.diagnostics.push(Diagnostic {
            level: DiagLevel::Error,
            target: "installed.json".to_string(),
            message,
        });
        store
    }

    /// Write `dir/installed.json` through a unique temp file plus rename.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let file = dir.join("installed.json");
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

// ---------------------------------------------------------------------------
// Version ladder and ids (design §1, §6)

/// The version ladder: package `version` → registry entry `version` → resolved
/// git SHA → sha256 → the literal `"unknown"`.
pub fn resolve_version(
    manifest_version: Option<&str>,
    entry_version: Option<&str>,
    resolved_sha: Option<&str>,
    resolved_sha256: Option<&str>,
) -> Option<String> {
    let present = |value: Option<&str>| value.filter(|value| !value.is_empty()).map(str::to_string);
    present(manifest_version)
        .or_else(|| present(entry_version))
        .or_else(|| present(resolved_sha).map(|sha| truncate12(&sha)))
        .or_else(|| present(resolved_sha256).map(|sha| truncate12(&sha)))
        .or_else(|| Some("unknown".to_string()))
}

/// The first 12 characters, never splitting a character.
fn truncate12(value: &str) -> String {
    value.chars().take(12).collect()
}

/// The record id for a package (design §1).
///
/// The manifest name is the base, slugged for the compat layout. When that id
/// is taken, the first 6 characters of the resolved source fingerprint keep
/// the two sources apart.
pub fn assign_id(
    name: &str,
    layout: Layout,
    existing: &[InstallRecord],
    fingerprint: &str,
) -> String {
    let base = match layout {
        Layout::AgentPlugins => name.to_string(),
        Layout::ClaudeCode => slug(name),
    };
    let taken = |candidate: &str| existing.iter().any(|record| record.id == candidate);
    if !taken(&base) {
        return base;
    }
    let prefix: String = fingerprint.chars().take(6).collect();
    let mut candidate = if prefix.is_empty() {
        format!("{base}-2")
    } else {
        format!("{base}-{prefix}")
    };
    let mut suffix = 2;
    while taken(&candidate) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

/// Lowercase a compat manifest name and collapse unsupported runs to `-`.
fn slug(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|ch| {
            let ch = ch.to_ascii_lowercase();
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    mapped
        .trim_matches(['-', '.'])
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// The content-derived default policy (design §7): anything code-bearing is
/// manual.
pub fn derive_policy(discovered: &Discovered) -> UpdatePolicy {
    if discovered.servers.is_empty() && discovered.subagents.is_empty() {
        UpdatePolicy::Auto
    } else {
        UpdatePolicy::Manual
    }
}

// ---------------------------------------------------------------------------
// Fetching into staging (controller ruling R1)

/// Removes its staging directory on every exit path, success or failure.
struct StagingGuard {
    root: PathBuf,
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// `plugins/.staging/<pid>-<nanos>-<seq>/`: unique per call, never shared.
fn staging_root(plugins_dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    plugins_dir.join(".staging").join(format!(
        "{}-{}-{}",
        std::process::id(),
        nanos,
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// `install` never fetches over HTTP: every supported source kind is git or
/// the filesystem. This client keeps the shared staging signature (R1) — it
/// must not build a TLS stack, which panics outside the Tauri runtime — and
/// fails loudly if an HTTP-backed source kind ever reaches it.
struct NoHttp;

#[async_trait::async_trait]
impl HttpClient for NoHttp {
    async fn get_conditional(
        &self,
        url: &str,
        _etag: Option<&str>,
        _last_modified: Option<&str>,
    ) -> Result<HttpResponse, String> {
        Err(format!("no HTTP client during install: {url}"))
    }
}

/// Fetch one plugin source into `target`, a package path inside a unique
/// staging directory. Install and update share this code path (R1).
///
/// `resolved_sha` is set for git-backed sources (the checked-out commit).
/// `resolved_sha256` is reserved for non-git revisions; no source kind
/// produces one today (the archive flow is out of scope), and the parameter
/// is part of the shared signature for Task 6.
pub(crate) fn fetch_to_staging(
    source: &PluginSource,
    target: &Path,
    resolved_sha: &mut Option<String>,
    resolved_sha256: &mut Option<String>,
    _http: &dyn HttpClient,
) -> Result<(), String> {
    *resolved_sha = None;
    *resolved_sha256 = None;
    match source {
        PluginSource::Path { path } => copy_package(Path::new(path), target),
        PluginSource::Github { .. } | PluginSource::Git { .. } | PluginSource::GitSubdir { .. } => {
            fetch_git(source, target, resolved_sha)
        }
        // Registry entries document `url` as git over HTTPS (design §5).
        PluginSource::Url { url } => {
            let (base, fragment) = parse_git_remote(url);
            clone_repo(&base, fragment.as_deref(), target)?;
            *resolved_sha = Some(git_in(target, &["rev-parse", "HEAD"])?);
            remove_git_dir(target)
        }
        PluginSource::Unsupported { kind, .. } => {
            Err(format!("cannot install unsupported source kind: {kind}"))
        }
    }
}

/// Clone a git-backed source; `path` selects a package subdirectory.
fn fetch_git(
    source: &PluginSource,
    target: &Path,
    resolved_sha: &mut Option<String>,
) -> Result<(), String> {
    let (url, sub_path, git_ref) = git_target(source)?;
    let sha = source_sha(source);
    match sub_path {
        None => {
            clone_repo(&url, git_ref.as_deref(), target)?;
            if let Some(sha) = sha {
                checkout_sha(target, sha)?;
            }
            *resolved_sha = Some(git_in(target, &["rev-parse", "HEAD"])?);
            remove_git_dir(target)
        }
        Some(sub) => {
            let clone = target.with_file_name(".git-clone");
            clone_repo(&url, git_ref.as_deref(), &clone)?;
            if let Some(sha) = sha {
                checkout_sha(&clone, sha)?;
            }
            let wanted = clone.join(&sub);
            if !wanted.exists() {
                return Err(format!("plugin path {sub} is missing in this revision"));
            }
            let package = resolve_within(&clone, &wanted)
                .ok_or_else(|| format!("plugin path {sub} resolves outside the repository"))?;
            *resolved_sha = Some(git_in(&clone, &["rev-parse", "HEAD"])?);
            std::fs::rename(&package, target)
                .map_err(|err| format!("cannot move {} into place: {err}", package.display()))
        }
    }
}

/// Check out an exact commit, fetched after the shallow clone.
fn checkout_sha(repo: &Path, sha: &str) -> Result<(), String> {
    if git_in(repo, &["fetch", "--depth", "1", "origin", sha]).is_err() {
        git_in(repo, &["fetch", "origin", sha])?;
    }
    git_in(repo, &["checkout", "--detach", "FETCH_HEAD"]).map(|_| ())
}

/// The exact commit a git source names, when it names one.
fn source_sha(source: &PluginSource) -> Option<&str> {
    match source {
        PluginSource::Github { sha, .. }
        | PluginSource::Git { sha, .. }
        | PluginSource::GitSubdir { sha, .. } => sha.as_deref(),
        _ => None,
    }
}

/// An installed package needs no git metadata: updates re-fetch from source.
fn remove_git_dir(package: &Path) -> Result<(), String> {
    let git = package.join(".git");
    if git.exists() {
        std::fs::remove_dir_all(&git)
            .map_err(|err| format!("cannot remove {}: {err}", git.display()))?;
    }
    Ok(())
}

/// Copy a local path into staging. The top-level path is canonicalized, so
/// installing through a symlink copies the real package; nested symlinks are
/// kept as links and containment is enforced later, per design §13.
fn copy_package(source: &Path, target: &Path) -> Result<(), String> {
    let source = std::fs::canonicalize(source)
        .map_err(|err| format!("cannot read source path {}: {err}", source.display()))?;
    copy_tree(&source, target)
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(source)
        .map_err(|err| format!("cannot read {}: {err}", source.display()))?;
    if meta.file_type().is_symlink() {
        return copy_link(source, target);
    }
    if meta.is_dir() {
        std::fs::create_dir_all(target)
            .map_err(|err| format!("cannot create {}: {err}", target.display()))?;
        let entries = std::fs::read_dir(source)
            .map_err(|err| format!("cannot read {}: {err}", source.display()))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("cannot read {}: {err}", source.display()))?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    std::fs::copy(source, target)
        .map(|_| ())
        .map_err(|err| format!("cannot copy {}: {err}", source.display()))
}

#[cfg(unix)]
fn copy_link(source: &Path, target: &Path) -> Result<(), String> {
    let link = std::fs::read_link(source)
        .map_err(|err| format!("cannot read link {}: {err}", source.display()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    std::os::unix::fs::symlink(&link, target)
        .map_err(|err| format!("cannot copy link {}: {err}", source.display()))
}

#[cfg(not(unix))]
fn copy_link(source: &Path, target: &Path) -> Result<(), String> {
    // Windows symlinks need a privilege; follow the link instead.
    let resolved = std::fs::canonicalize(source)
        .map_err(|err| format!("cannot read link {}: {err}", source.display()))?;
    copy_tree(&resolved, target)
}

// ---------------------------------------------------------------------------
// Install and uninstall (design §6)

/// Install one plugin source.
///
/// Fetch, validate, move into place, write the record disabled. Nothing is
/// executed, no MCP server is connected and no config is touched.
pub fn install(
    store: &mut InstallStore,
    plugins_dir: &Path,
    source: &PluginSource,
    entry: Option<&Entry>,
    marketplace: Option<&str>,
) -> Result<InstallRecord, String> {
    if let Some(existing) = store.records.iter().find(|record| &record.source == source) {
        return Err(format!("{} is already installed", existing.id));
    }

    let staging = staging_root(plugins_dir);
    let _guard = StagingGuard {
        root: staging.clone(),
    };
    let package = staging.join("package");

    let mut resolved_sha = None;
    let mut resolved_sha256 = None;
    fetch_to_staging(
        source,
        &package,
        &mut resolved_sha,
        &mut resolved_sha256,
        &NoHttp,
    )?;

    let manifest = manifest::load(&package).map_err(join_diagnostics)?;
    let discovered = discover(&package, &manifest);

    let fingerprint = resolved_sha
        .clone()
        .or_else(|| resolved_sha256.clone())
        .unwrap_or_else(|| match source {
            PluginSource::Path { path } => path_fingerprint(Path::new(path)),
            _ => String::new(),
        });
    let id = assign_id(
        &manifest.name,
        manifest.layout,
        &store.records,
        &fingerprint,
    );
    let version = resolve_version(
        manifest.version.as_deref(),
        entry.and_then(|entry| entry.version.as_deref()),
        resolved_sha.as_deref(),
        resolved_sha256.as_deref(),
    );

    let destination = plugins_dir.join(&id);
    let destination_created = !destination.exists();
    std::fs::create_dir_all(&destination)
        .map_err(|err| format!("cannot create {}: {err}", destination.display()))?;
    let package_destination = destination.join("package");
    if let Err(err) = std::fs::rename(&package, &package_destination) {
        if destination_created {
            let _ = std::fs::remove_dir_all(&destination);
        }
        return Err(format!(
            "cannot move the package into {}: {err}",
            package_destination.display()
        ));
    }

    let record = InstallRecord {
        id,
        name: manifest.name,
        marketplace: marketplace.map(str::to_string),
        source: source.clone(),
        version,
        installed_at: chrono::Utc::now().to_rfc3339(),
        layout: manifest.layout,
        enabled: false,
        update_policy: derive_policy(&discovered),
        disabled_servers: Vec::new(),
        previous_version: None,
        tree_hash: None,
        last_checked_at: None,
        available_update: None,
        status: PluginStatus::InstalledDisabled,
        diagnostics: discovered.diagnostics,
        resolved_sha,
        resolved_sha256,
        extra: BTreeMap::new(),
    };

    store.records.push(record.clone());
    if let Err(err) = store.save(plugins_dir) {
        store.records.pop();
        let _ = std::fs::remove_dir_all(&destination);
        return Err(format!("cannot write the install record: {err}"));
    }
    Ok(record)
}

/// Uninstall one plugin.
///
/// Returns the `plugin:<id>:<server>` ids the package declared, so the caller
/// can disconnect them; the MCP manager is not touched here. The package and
/// its record go; `data_dir` (the plugin's own data directory) stays unless
/// `delete_data` is set.
pub fn uninstall(
    store: &mut InstallStore,
    plugins_dir: &Path,
    data_dir: &Path,
    id: &str,
    delete_data: bool,
) -> Result<Vec<String>, String> {
    let Some(index) = store.records.iter().position(|record| record.id == id) else {
        return Err(format!("plugin `{id}` is not installed"));
    };
    let server_ids = owned_server_ids(&plugins_dir.join(id).join("package"), id);

    let record = store.records.remove(index);
    if let Err(err) = store.save(plugins_dir) {
        store.records.insert(index, record);
        return Err(format!("cannot write the install record: {err}"));
    }

    let directory = plugins_dir.join(id);
    if directory.exists() {
        std::fs::remove_dir_all(&directory)
            .map_err(|err| format!("cannot remove {}: {err}", directory.display()))?;
    }
    if delete_data && data_dir.exists() {
        std::fs::remove_dir_all(data_dir)
            .map_err(|err| format!("cannot remove {}: {err}", data_dir.display()))?;
    }
    Ok(server_ids)
}

/// The server ids a package declares, named for the plugin layer.
fn owned_server_ids(package: &Path, id: &str) -> Vec<String> {
    let Ok(manifest) = manifest::load(package) else {
        return Vec::new();
    };
    discover(package, &manifest)
        .servers
        .iter()
        .map(|server| format!("plugin:{id}:{}", server.name))
        .collect()
}

/// SHA-256 of the canonical source path, the id fingerprint for a local
/// source (design §1: "the sha256 of the local path, when there is no SHA").
fn path_fingerprint(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}
