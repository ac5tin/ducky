//! The bundled official marketplace (design §5).
//!
//! The registry ships as a Tauri resource and is seeded into
//! `marketplaces.json` on first load. The record is read-only: the manager
//! refuses to remove it, and `ensure_seeded` puts it back if it is deleted by
//! hand.

use std::path::{Path, PathBuf};

use super::marketplace::{
    parse_registry_file, MarketplaceRecord, MarketplaceStore, PluginSource, Registry,
};

/// The reserved id of the bundled marketplace.
pub const BUNDLED_ID: &str = "ducky-official";

/// The registry file name; the checked-in copy also lives here.
const RESOURCE_FILE: &str = "ducky-official.json";

/// The bundled path under the resource directory, as registered in
/// `tauri.conf.json` (`bundle.resources` keeps the directory structure).
const RESOURCE_SUBPATH: &str = "resources/ducky-official.json";

/// The name shown in the marketplace list.
const BUNDLED_NAME: &str = "Ducky Official";

/// The checked-in registry file: a dev build has no packaged resources, so
/// this is the fallback.
fn checked_in() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join(RESOURCE_FILE)
}

/// The file the bundled record points at when nothing readable is found.
/// The summary then carries a diagnostic instead of an entry list.
fn expected_file(resource_dir: Option<&Path>) -> PathBuf {
    match resource_dir {
        Some(dir) => dir.join(RESOURCE_SUBPATH),
        None => checked_in(),
    }
}

/// The registry file on disk: the packaged resource when present, or the
/// checked-in copy when the build has no resource directory (dev/test). A
/// present resource directory without the file is a packaging mistake: it
/// resolves to nothing so the diagnostic surfaces instead of the dev copy.
fn resolve_file(resource_dir: Option<&Path>) -> Option<PathBuf> {
    match resource_dir {
        Some(dir) => [dir.join(RESOURCE_SUBPATH), dir.join(RESOURCE_FILE)]
            .into_iter()
            .find(|candidate| candidate.is_file()),
        None => {
            let checked_in = checked_in();
            checked_in.is_file().then_some(checked_in)
        }
    }
}

/// Read the bundled registry. Errors carry the path, and never stop startup.
pub fn load_registry(resource_dir: Option<&Path>) -> Result<Registry, String> {
    match resolve_file(resource_dir) {
        Some(path) => parse_registry_file(&path),
        None => Err(format!(
            "bundled marketplace registry is missing: {}",
            expected_file(resource_dir).display()
        )),
    }
}

/// Seed the bundled marketplace into a loaded store.
///
/// Returns true when a record was pushed and the caller must save, false when
/// a record with [`BUNDLED_ID`] already exists. An existing record is left
/// exactly as it is, including hand edits; deleting it by hand re-seeds it on
/// the next load, which is what makes the bundled marketplace read-only.
/// A missing or malformed registry file still seeds the record: its `error`
/// carries the diagnosis and the marketplace list keeps working.
pub fn ensure_seeded(store: &mut MarketplaceStore, resource_dir: Option<&Path>) -> bool {
    if store.records.iter().any(|record| record.id == BUNDLED_ID) {
        return false;
    }
    let path = resolve_file(resource_dir).unwrap_or_else(|| expected_file(resource_dir));
    let mut record = MarketplaceRecord {
        id: BUNDLED_ID.to_string(),
        name: BUNDLED_NAME.to_string(),
        source: PluginSource::Path {
            path: path.display().to_string(),
        },
        registry_path: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        auto_refresh: false,
        last_refreshed_at: None,
        resolved_sha: None,
        bundled: true,
        hidden: true,
        error: None,
        extra: Default::default(),
    };
    if let Err(err) = load_registry(resource_dir) {
        tracing::warn!("{err}");
        record.error = Some(err);
    }
    store.records.push(record);
    true
}
