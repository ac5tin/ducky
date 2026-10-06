//! Update checks, atomic swap and rollback (design §7).
//!
//! Fetch and validate into staging before the live `package/` directory is
//! touched. A kill between the two renames is repaired at the start of the
//! next `apply` or `rollback`. A failed record write puts that directory back,
//! then returns, so the record still describes the revision on disk.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::install::{
    fetch_to_staging, package_dir, resolve_version, tree_hash, AvailableUpdate, InstallRecord,
    InstallStore, PluginStatus,
};
use super::layout::discover;
use super::manifest;
use super::marketplace::{
    git_guarded, git_target, join_diagnostics, parse_git_remote, Entry, PluginSource, Registry,
};

/// Result of a successful swap. The command layer reconnects `enabled_servers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub from: Option<String>,
    pub to: Option<String>,
    pub modified_locally: bool,
    /// `plugin:<id>:<name>` ids that were enabled before the swap.
    pub enabled_servers: Vec<String>,
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
    recover_missing_package(&plugin_dir, &live, record)?;
    let modified_locally = locally_modified(record, &live);
    if modified_locally && !force {
        record.status = PluginStatus::ModifiedLocally;
        return Err(match write_record(record, plugins_dir) {
            Ok(()) => "plugin was modified locally".to_string(),
            Err(err) => format!("plugin was modified locally ({err})"),
        });
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
        previous_dir_suffix(from.as_deref().unwrap_or("unknown"))
    ));
    // An existing previous dir is held until the new `package/` rename
    // succeeds. A kill between the renames is repaired on the next call.
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
    recover_missing_package(&plugin_dir, &live, record)?;
    let previous = plugin_dir.join(format!(
        "package.previous-{}",
        previous_dir_suffix(record.previous_version.as_deref().unwrap_or("unknown"))
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
    // The URL came from a source, never from us: it goes after `--`, so a
    // leading `-` is a URL and not an option (final-review Critical).
    let mut refs = vec![
        format!("refs/heads/{git_ref}"),
        format!("refs/tags/{git_ref}"),
        format!("refs/tags/{git_ref}^{{}}"),
    ];
    if git_ref == "HEAD" {
        refs.push("HEAD".to_string());
    }
    let mut positionals: Vec<&OsStr> = vec![OsStr::new(url)];
    positionals.extend(refs.iter().map(|git_ref| OsStr::new(git_ref.as_str())));
    let mut command = git_guarded(None, &["ls-remote"], &positionals);
    command.env("GIT_TERMINAL_PROMPT", "0");
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
    resolve_remote_sha(&output.stdout, git_ref)
}

/// Pick one SHA. Line order is not a contract, and an annotated tag's first
/// line is the tag object, not the commit `install` stored.
fn resolve_remote_sha(stdout: &[u8], git_ref: &str) -> Result<String, String> {
    let stdout = String::from_utf8_lossy(stdout);
    let mut refs = std::collections::BTreeMap::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(sha) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        if sha.len() == 40 && sha.chars().all(|ch| ch.is_ascii_hexdigit()) {
            refs.insert(name.to_string(), sha.to_string());
        }
    }
    let head = format!("refs/heads/{git_ref}");
    let peeled = format!("refs/tags/{git_ref}^{{}}");
    let tag = format!("refs/tags/{git_ref}");
    if let Some(sha) = refs.get(&head) {
        return Ok(sha.clone());
    }
    if let Some(sha) = refs.get(&peeled) {
        return Ok(sha.clone());
    }
    if let Some(sha) = refs.get(&tag) {
        return Ok(sha.clone());
    }
    if git_ref == "HEAD" {
        if let Some(sha) = refs.get("HEAD") {
            return Ok(sha.clone());
        }
    }
    Err(format!("git ls-remote returned no commit for {git_ref}"))
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

/// Directory suffix for `package.previous-<suffix>`. The same function
/// builds the name and compares it, so a sanitized version cannot drift
/// from `previous_version`.
fn previous_dir_suffix(version: &str) -> String {
    let unsafe_name = version.is_empty()
        || version == "."
        || version == ".."
        || version.ends_with(' ')
        || version.ends_with('.')
        || version
            .chars()
            .any(|ch| ch == '/' || ch == '\\' || ch == '\0');
    if unsafe_name {
        "unknown".to_string()
    } else {
        version.to_string()
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

/// Put a killed swap back before `apply` or `rollback` does anything else.
///
/// 1. Missing `package/` and `package.rollback-tmp` → that dir is the new
///    bytes. Rename it to `package/` and leave every other dir alone.
/// 2. Any `package.held-*`. If `package/` and the matching
///    `package.previous-*` both exist, that previous dir is the recorded
///    rollback source. Every held dir is superseded residue: remove it, or
///    rename it to `package.superseded-*`, then continue. Do not rename a
///    held dir onto that previous dir. Otherwise one held dir is the
///    rollback source. If `package/` is missing, rename the matching
///    previous dir back to `package/` first, then rename the held dir to
///    that name. More than one held dir, and no matching previous beside a
///    live package, is named and left in place.
/// 3. One `package.previous-*` whose suffix differs → mid-apply kill.
///    Rename it to `package/`.
/// 4. One matching suffix and no held dir → the recorded rollback source.
///    Refuse.
/// 5. More than one `package.previous-*` → rename nothing and name them.
fn recover_missing_package(
    plugin_dir: &Path,
    live: &Path,
    record: &InstallRecord,
) -> Result<(), String> {
    if !live.is_dir() {
        let aside = live.with_file_name("package.rollback-tmp");
        if aside.is_dir() {
            return std::fs::rename(&aside, live).map_err(|err| {
                format!(
                    "cannot restore {} to {}: {err}",
                    aside.display(),
                    live.display()
                )
            });
        }
    }
    let held = match list_held(plugin_dir) {
        Ok(held) => held,
        Err(_) if !plugin_dir.exists() => Vec::new(),
        Err(err) => return Err(err),
    };
    if !held.is_empty() {
        return restore_held_rollback_source(plugin_dir, live, record, &held);
    }
    if live.is_dir() {
        return Ok(());
    }
    let previous = match list_previous(plugin_dir) {
        Ok(previous) => previous,
        Err(_) if !plugin_dir.exists() => {
            return Err(format!("package {} is missing", live.display()));
        }
        Err(err) => return Err(err),
    };
    match previous.as_slice() {
        [] => Err(format!("package {} is missing", live.display())),
        [one] => restore_unless_rollback_source(one, live, record),
        many => {
            let names = many
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "package {} is missing; more than one previous revision: {names}",
                live.display()
            ))
        }
    }
}

fn restore_unless_rollback_source(
    previous: &Path,
    live: &Path,
    record: &InstallRecord,
) -> Result<(), String> {
    let dir_suffix = previous
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("package.previous-"));
    // None is not a recorded rollback source. A kill before the record
    // write still restores. Compare suffixes, not the raw version.
    // ponytail: a matching suffix refuses only when no `package.held-*`
    // exists. That check runs first. Unsafe versions still share `unknown`.
    if let (Some(dir_suffix), Some(recorded)) = (dir_suffix, record.previous_version.as_deref()) {
        if dir_suffix == previous_dir_suffix(recorded) {
            return Err(format!(
                "package {} is missing; its only copy is the recorded rollback source {}",
                live.display(),
                previous.display()
            ));
        }
    }
    std::fs::rename(previous, live).map_err(|err| {
        format!(
            "cannot restore {} to {}: {err}",
            previous.display(),
            live.display()
        )
    })
}

fn list_previous(plugin_dir: &Path) -> Result<Vec<PathBuf>, String> {
    list_prefixed(plugin_dir, "package.previous-")
}

fn list_held(plugin_dir: &Path) -> Result<Vec<PathBuf>, String> {
    list_prefixed(plugin_dir, "package.held-")
}

fn list_prefixed(plugin_dir: &Path, prefix: &str) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(plugin_dir)
        .map_err(|err| format!("cannot read {}: {err}", plugin_dir.display()))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("cannot read {}: {err}", plugin_dir.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(prefix) && entry.path().is_dir() {
            found.push(entry.path());
        }
    }
    Ok(found)
}

/// `held` is the dir `hold_existing` parked. If the matching
/// `package.previous-*` already sits beside `package/`, it is the recorded
/// rollback source and every held dir is residue. Otherwise put the single
/// held dir back at that previous path. Never delete a `package.previous-*`.
fn restore_held_rollback_source(
    plugin_dir: &Path,
    live: &Path,
    record: &InstallRecord,
    held: &[PathBuf],
) -> Result<(), String> {
    let suffix = previous_dir_suffix(record.previous_version.as_deref().unwrap_or("unknown"));
    let previous = plugin_dir.join(format!("package.previous-{suffix}"));
    // A matching previous dir beside the live package is the record's
    // rollback source. Held dirs are older residue. Do not rename onto it.
    if live.is_dir() && previous.is_dir() {
        for path in held {
            discard_superseded(path)?;
        }
        return Ok(());
    }
    if held.len() != 1 {
        let names = held
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "package {} has more than one held revision: {names}",
            live.display()
        ));
    }
    let held = &held[0];
    if !live.is_dir() {
        if !previous.is_dir() {
            return Err(format!(
                "package {} is missing; cannot restore the live tree from {}",
                live.display(),
                previous.display()
            ));
        }
        std::fs::rename(&previous, live).map_err(|err| {
            format!(
                "cannot restore {} to {}: {err}",
                previous.display(),
                live.display()
            )
        })?;
    }
    std::fs::rename(held, &previous).map_err(|err| {
        format!(
            "cannot restore {} to {}: {err}",
            held.display(),
            previous.display()
        )
    })
}

/// Remove a superseded held dir. If the delete fails, park it under a name
/// that recovery does not treat as a rollback source.
fn discard_superseded(held: &Path) -> Result<(), String> {
    if std::fs::remove_dir_all(held).is_ok() || !held.exists() {
        return Ok(());
    }
    let parked = held.with_file_name(format!(
        "package.superseded-{}-{}",
        std::process::id(),
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    // ponytail: if the parent is not writable, the rename fails and the
    // residue still blocks. No third strategy.
    std::fs::rename(held, &parked).map_err(|err| {
        format!(
            "cannot remove superseded {} or move it to {}: {err}",
            held.display(),
            parked.display()
        )
    })
}

/// `live` → `previous`, then `staged` → `live`. An existing previous dir is
/// held until the new `package/` rename succeeds, then removed. A delete
/// that fails is parked as `package.superseded-*`. Do not return `Ok` while
/// that dir is still `package.held-*`.
fn swap_in(live: &Path, previous: &Path, staged: &Path) -> Result<(), String> {
    if !live.is_dir() {
        return Err(format!("package {} is missing", live.display()));
    }
    if !staged.is_dir() {
        return Err(format!("staged package {} is missing", staged.display()));
    }
    let held = hold_existing(previous)?;
    if let Err(err) = std::fs::rename(live, previous) {
        restore_held(held.as_deref(), previous);
        return Err(format!(
            "cannot move {} to {}: {err}",
            live.display(),
            previous.display()
        ));
    }
    if let Err(err) = std::fs::rename(staged, live) {
        let restore_err = std::fs::rename(previous, live).err();
        restore_held(held.as_deref(), previous);
        if let Some(restore) = restore_err {
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
    if let Some(held) = held {
        discard_superseded(&held)?;
    }
    Ok(())
}

fn hold_existing(previous: &Path) -> Result<Option<PathBuf>, String> {
    if !previous.exists() {
        return Ok(None);
    }
    let held = previous.with_file_name(format!(
        "package.held-{}-{}",
        std::process::id(),
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::rename(previous, &held).map_err(|err| {
        format!(
            "cannot move {} to {}: {err}",
            previous.display(),
            held.display()
        )
    })?;
    Ok(Some(held))
}

fn restore_held(held: Option<&Path>, previous: &Path) {
    let Some(held) = held else {
        return;
    };
    if !held.exists() || previous.exists() {
        return;
    }
    let _ = std::fs::rename(held, previous);
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
    // Stale residue from a finished rollback. A missing `package/` is
    // recovered first; do not delete `package.rollback-tmp` in that case.
    if aside.exists() && live.is_dir() {
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
