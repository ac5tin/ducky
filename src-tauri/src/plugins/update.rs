//! Update checks, atomic swap and rollback (design §7).
//!
//! Fetch and validate into staging before the live `package/` directory is
//! touched. A failed record write puts that directory back, then returns, so
//! the record still describes the revision on disk.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::install::{
    fetch_to_staging, package_dir, resolve_version, AvailableUpdate, InstallRecord, InstallStore,
    PluginStatus,
};
use super::layout::discover;
use super::manifest;
use super::marketplace::{
    git_target, join_diagnostics, parse_git_remote, Entry, PluginSource, Registry,
};
use crate::snapshot::GIT_ENV_TO_CLEAR;

/// More files than this and the local-edit guard is skipped (design §7).
const MAX_TREE_FILES: usize = 2000;

/// Result of a successful swap. The command layer reconnects `enabled_servers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub from: Option<String>,
    pub to: Option<String>,
    pub modified_locally: bool,
    /// `plugin:<id>:<name>` ids that were enabled before the swap.
    pub enabled_servers: Vec<String>,
}

/// `sha256:<hex>` over the sorted relative paths and file bytes.
///
/// `None` when the tree exceeds [`MAX_TREE_FILES`] or cannot be read.
/// ponytail: both cases return None. `apply` treats `Some(recorded) != None`
/// as a local edit and refuses an unforced update.
pub fn tree_hash(package: &Path) -> Option<String> {
    let mut files = Vec::new();
    if !collect_files(package, package, &mut files) {
        return None;
    }
    files.sort();
    let mut hasher = Sha256::new();
    for relative in &files {
        let full = package.join(relative);
        let meta = std::fs::symlink_metadata(&full).ok()?;
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&full).ok()?;
            let target = target.to_string_lossy();
            hasher.update((target.len() as u64).to_le_bytes());
            hasher.update(target.as_bytes());
            continue;
        }
        let bytes = std::fs::read(&full).ok()?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Some(format!("sha256:{:x}", hasher.finalize()))
}

/// Compare one install record with the registry, or with `git ls-remote`.
///
/// A registry `version` wins when the entry has one (opaque string compare).
/// A versionless git source is checked with `git ls-remote <url> <ref>`
/// against `record.resolved_sha`. No clone.
pub fn check_one(
    record: &mut InstallRecord,
    registry: Option<&Registry>,
    plugins_dir: &Path,
) -> Result<Option<AvailableUpdate>, String> {
    package_dir(plugins_dir, &record.id)?;
    if let Some(version) = matching_version(record, registry) {
        record.last_checked_at = Some(now());
        if record.version.as_deref() == Some(version) {
            clear_available(record);
            return Ok(None);
        }
        return Ok(Some(note_update(
            record,
            AvailableUpdate {
                version: Some(version.to_string()),
                resolved_sha: None,
            },
        )));
    }
    if let Some((url, git_ref)) = ls_remote_target(&record.source) {
        let sha = git_ls_remote(&url, &git_ref)?;
        record.last_checked_at = Some(now());
        if record.resolved_sha.as_deref() == Some(sha.as_str()) {
            clear_available(record);
            return Ok(None);
        }
        return Ok(Some(note_update(
            record,
            AvailableUpdate {
                version: None,
                resolved_sha: Some(sha),
            },
        )));
    }
    record.last_checked_at = Some(now());
    clear_available(record);
    Ok(None)
}

/// Fetch the next revision, validate it, and swap it into `package/`.
///
/// `force` is false for an automatic update. A local edit blocks every
/// unforced update; a confirmed manual update passes `force: true`.
/// Returns the server ids that were enabled before the swap. Does not call
/// the MCP layer and does not run plugin code.
pub fn apply(
    record: &mut InstallRecord,
    plugins_dir: &Path,
    entry: Option<&Entry>,
    force: bool,
) -> Result<UpdateOutcome, String> {
    let plugin_dir = package_dir(plugins_dir, &record.id)?;
    let live = plugin_dir.join("package");
    let modified_locally = locally_modified(record, &live);
    if modified_locally && !force {
        record.status = PluginStatus::ModifiedLocally;
        let _ = write_record(record, plugins_dir);
        return Err("plugin was modified locally".to_string());
    }
    // Know the record can be saved before the live directory moves.
    ensure_record_slot(record, plugins_dir)?;

    let source = match entry {
        Some(entry) => entry.source.clone(),
        None => record.source.clone(),
    };
    let staging = staging_root(plugins_dir);
    let _guard = StagingGuard {
        root: staging.clone(),
    };
    let staged = staging.join("package");
    let mut resolved_sha = None;
    let mut resolved_sha256 = None;
    fetch_to_staging(&source, &staged, &mut resolved_sha, &mut resolved_sha256)?;
    let manifest = manifest::load(&staged).map_err(join_diagnostics)?;
    let discovered = discover(&staged, &manifest);

    let enabled_servers = enabled_server_ids(&live, &record.id, &record.disabled_servers);
    let from = record.version.clone();
    let previous = plugin_dir.join(format!(
        "package.previous-{}",
        revision_label(from.as_deref())
    ));
    // Second rename failure puts `package/` back. The old bytes stay at
    // `previous` until that restore, so a hole requires both renames to fail.
    swap_in(&live, &previous, &staged)?;

    let version = resolve_version(
        manifest.version.as_deref(),
        entry.and_then(|entry| entry.version.as_deref()),
        resolved_sha.as_deref(),
        resolved_sha256.as_deref(),
    );
    let mut updated = record.clone();
    updated.name = manifest.name;
    updated.layout = manifest.layout;
    updated.version = version.clone();
    updated.previous_version = from.clone();
    updated.tree_hash = tree_hash(&live);
    updated.resolved_sha = resolved_sha;
    updated.resolved_sha256 = resolved_sha256;
    updated.available_update = None;
    updated.diagnostics = discovered.diagnostics;
    updated.last_checked_at = Some(now());
    updated.status = if updated.enabled {
        PluginStatus::Enabled
    } else {
        PluginStatus::InstalledDisabled
    };
    if entry.is_some() {
        updated.source = source;
    }
    if let Err(err) = write_record(&updated, plugins_dir) {
        // The in-memory record still describes the old revision. Put that
        // revision back at `package/` before returning.
        if let Err(undo) = undo_swap(&live, &previous) {
            return Err(format!(
                "{err} (failed to restore the previous package: {undo})"
            ));
        }
        return Err(err);
    }
    *record = updated;
    // ponytail: a failed delete of an older previous dir is ignored. The live
    // package and the record already match; the next successful update retries.
    delete_other_previous(&plugin_dir, &previous);
    Ok(UpdateOutcome {
        from,
        to: version,
        modified_locally,
        enabled_servers,
    })
}

/// Swap `package.previous-<version>` back to `package/` and clear
/// `previous_version`. One previous revision, not a history.
pub fn rollback(record: &mut InstallRecord, plugins_dir: &Path) -> Result<(), String> {
    let plugin_dir = package_dir(plugins_dir, &record.id)?;
    let live = plugin_dir.join("package");
    let previous = plugin_dir.join(format!(
        "package.previous-{}",
        revision_label(record.previous_version.as_deref())
    ));
    if record.previous_version.is_none() || !previous.is_dir() {
        return Err("no previous revision to roll back".to_string());
    }
    ensure_record_slot(record, plugins_dir)?;

    let aside = swap_back(&live, &previous)?;
    let mut updated = record.clone();
    updated.version = record.previous_version.clone();
    updated.previous_version = None;
    updated.tree_hash = tree_hash(&live);
    // The old commit is not stored separately, and the package has no `.git`.
    // Clear the post-update sha so the next check can see the newer commit.
    updated.resolved_sha = None;
    updated.resolved_sha256 = None;
    updated.available_update = None;
    updated.status = if updated.enabled {
        PluginStatus::Enabled
    } else {
        PluginStatus::InstalledDisabled
    };
    if let Err(err) = write_record(&updated, plugins_dir) {
        if let Err(undo) = undo_rollback(&live, &previous, &aside) {
            return Err(format!(
                "{err} (failed to restore the updated package: {undo})"
            ));
        }
        return Err(err);
    }
    *record = updated;
    let _ = std::fs::remove_dir_all(&aside);
    Ok(())
}

fn matching_version<'a>(record: &InstallRecord, registry: Option<&'a Registry>) -> Option<&'a str> {
    let registry = registry?;
    registry
        .entries
        .iter()
        .find(|entry| entry.name == record.name || entry.name == record.id)
        .and_then(|entry| entry.version.as_deref())
        .filter(|version| !version.is_empty())
}

fn note_update(record: &mut InstallRecord, update: AvailableUpdate) -> AvailableUpdate {
    record.available_update = Some(update.clone());
    record.status = PluginStatus::UpdateAvailable;
    update
}

fn clear_available(record: &mut InstallRecord) {
    record.available_update = None;
    if record.status == PluginStatus::UpdateAvailable {
        record.status = if record.enabled {
            PluginStatus::Enabled
        } else {
            PluginStatus::InstalledDisabled
        };
    }
}

fn ls_remote_target(source: &PluginSource) -> Option<(String, String)> {
    match source {
        PluginSource::Github { .. } | PluginSource::Git { .. } | PluginSource::GitSubdir { .. } => {
            let (url, _, git_ref) = git_target(source).ok()?;
            Some((url, git_ref.unwrap_or_else(|| "HEAD".to_string())))
        }
        PluginSource::Url { url } => {
            let (base, fragment) = parse_git_remote(url);
            Some((base, fragment.unwrap_or_else(|| "HEAD".to_string())))
        }
        PluginSource::Path { .. } | PluginSource::Unsupported { .. } => None,
    }
}

fn git_ls_remote(url: &str, git_ref: &str) -> Result<String, String> {
    let mut command = Command::new("git");
    for var in GIT_ENV_TO_CLEAR {
        command.env_remove(var);
    }
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.arg("ls-remote").arg(url).arg(git_ref);
    let output = command
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git ls-remote failed for {url}")
        } else {
            stderr
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sha = stdout.split_whitespace().next().unwrap_or("");
    if sha.len() != 40 || !sha.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!("git ls-remote returned no commit for {git_ref}"));
    }
    Ok(sha.to_string())
}

fn locally_modified(record: &InstallRecord, package: &Path) -> bool {
    let Some(recorded) = record.tree_hash.as_deref() else {
        return false;
    };
    if !package.is_dir() {
        return false;
    }
    match tree_hash(package) {
        Some(current) => current != recorded,
        None => true,
    }
}

fn enabled_server_ids(package: &Path, id: &str, disabled: &[String]) -> Vec<String> {
    let Ok(manifest) = manifest::load(package) else {
        return Vec::new();
    };
    discover(package, &manifest)
        .servers
        .into_iter()
        .filter(|server| !is_disabled(&server.name, id, disabled))
        .map(|server| format!("plugin:{id}:{}", server.name))
        .collect()
}

fn is_disabled(name: &str, id: &str, disabled: &[String]) -> bool {
    let full = format!("plugin:{id}:{name}");
    disabled.iter().any(|item| item == name || item == &full)
}

fn revision_label(version: Option<&str>) -> String {
    let raw = version.unwrap_or("unknown");
    let unsafe_name = raw.is_empty()
        || raw == "."
        || raw == ".."
        || raw.ends_with(' ')
        || raw.ends_with('.')
        || raw.chars().any(|ch| ch == '/' || ch == '\\' || ch == '\0');
    if unsafe_name {
        "unknown".to_string()
    } else {
        raw.to_string()
    }
}

fn ensure_record_slot(record: &InstallRecord, plugins_dir: &Path) -> Result<(), String> {
    let store = load_store(plugins_dir)?;
    if store.records.iter().any(|item| item.id == record.id) {
        Ok(())
    } else {
        Err(format!(
            "plugin `{}` is not in the install record",
            record.id
        ))
    }
}

fn write_record(record: &InstallRecord, plugins_dir: &Path) -> Result<(), String> {
    let mut store = load_store(plugins_dir)?;
    let Some(slot) = store.records.iter_mut().find(|item| item.id == record.id) else {
        return Err(format!(
            "plugin `{}` is not in the install record",
            record.id
        ));
    };
    *slot = record.clone();
    store
        .save(plugins_dir)
        .map_err(|err| format!("cannot write the install record: {err}"))
}

fn load_store(plugins_dir: &Path) -> Result<InstallStore, String> {
    let store = InstallStore::load(plugins_dir);
    if store.poisoned {
        let detail = store
            .diagnostics
            .iter()
            .map(|diag| diag.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("corrupt install store: {detail}"));
    }
    Ok(store)
}

/// `live` → `previous`, then `staged` → `live`. The first rename is undone
/// when the second fails, so `package/` is not left missing.
fn swap_in(live: &Path, previous: &Path, staged: &Path) -> Result<(), String> {
    if !live.is_dir() {
        return Err(format!("package {} is missing", live.display()));
    }
    if !staged.is_dir() {
        return Err(format!("staged package {} is missing", staged.display()));
    }
    if previous.exists() {
        std::fs::remove_dir_all(previous)
            .map_err(|err| format!("cannot remove {}: {err}", previous.display()))?;
    }
    std::fs::rename(live, previous).map_err(|err| {
        format!(
            "cannot move {} to {}: {err}",
            live.display(),
            previous.display()
        )
    })?;
    if let Err(err) = std::fs::rename(staged, live) {
        if let Err(restore) = std::fs::rename(previous, live) {
            return Err(format!(
                "cannot move the staged package into {}: {err} (failed to restore package: {restore})",
                live.display()
            ));
        }
        return Err(format!(
            "cannot move the staged package into {}: {err}",
            live.display()
        ));
    }
    Ok(())
}

fn undo_swap(live: &Path, previous: &Path) -> Result<(), String> {
    let discard = live.with_file_name("package.failed-update");
    if discard.exists() {
        std::fs::remove_dir_all(&discard)
            .map_err(|err| format!("cannot remove {}: {err}", discard.display()))?;
    }
    if live.exists() {
        std::fs::rename(live, &discard).map_err(|err| {
            format!(
                "cannot move {} to {}: {err}",
                live.display(),
                discard.display()
            )
        })?;
    }
    if let Err(err) = std::fs::rename(previous, live) {
        if discard.exists() {
            let _ = std::fs::rename(&discard, live);
        }
        return Err(format!(
            "cannot restore {} from {}: {err}",
            live.display(),
            previous.display()
        ));
    }
    let _ = std::fs::remove_dir_all(&discard);
    Ok(())
}

/// Move the current package aside and put `previous` at `package/`.
/// The aside path is kept until the record write succeeds.
fn swap_back(live: &Path, previous: &Path) -> Result<PathBuf, String> {
    let aside = live.with_file_name("package.rollback-tmp");
    if aside.exists() {
        std::fs::remove_dir_all(&aside)
            .map_err(|err| format!("cannot remove {}: {err}", aside.display()))?;
    }
    if live.exists() {
        std::fs::rename(live, &aside).map_err(|err| {
            format!(
                "cannot move {} to {}: {err}",
                live.display(),
                aside.display()
            )
        })?;
    }
    if let Err(err) = std::fs::rename(previous, live) {
        if aside.exists() {
            if let Err(restore) = std::fs::rename(&aside, live) {
                return Err(format!(
                    "cannot restore the previous package: {err} (failed to restore package: {restore})"
                ));
            }
        }
        return Err(format!("cannot restore the previous package: {err}"));
    }
    Ok(aside)
}

fn undo_rollback(live: &Path, previous: &Path, aside: &Path) -> Result<(), String> {
    if live.exists() {
        std::fs::rename(live, previous).map_err(|err| {
            format!(
                "cannot move {} to {}: {err}",
                live.display(),
                previous.display()
            )
        })?;
    }
    if aside.exists() {
        std::fs::rename(aside, live).map_err(|err| {
            format!(
                "cannot move {} to {}: {err}",
                aside.display(),
                live.display()
            )
        })?;
    }
    Ok(())
}

fn delete_other_previous(plugin_dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(plugin_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("package.previous-") && entry.path() != keep {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn collect_files(dir: &Path, root: &Path, files: &mut Vec<String>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        let file_type = meta.file_type();
        if file_type.is_dir() && !file_type.is_symlink() {
            if !collect_files(&path, root, files) {
                return false;
            }
            continue;
        }
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        if files.len() == MAX_TREE_FILES {
            return false;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        files.push(relative_key(relative));
    }
    true
}

fn relative_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

struct StagingGuard {
    root: PathBuf,
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

fn staging_root(plugins_dir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    plugins_dir.join(".staging").join(format!(
        "update-{}-{}-{}",
        std::process::id(),
        nanos,
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}
