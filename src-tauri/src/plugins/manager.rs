//! The plugin manager: install records, marketplaces, the derived index and
//! the events that keep the UI in step (design §3, §5, §6, §11).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use async_trait::async_trait;

use super::diagnostics::{DiagLevel, Diagnostic};
use super::install::{self, InstallRecord, InstallStore, PluginStatus, UpdatePolicy};
use super::layout::{
    discover, Discovered, PluginServer, PluginSubagent, PluginTransport, RemoteKind, SkillEntry,
};
use super::manifest::{self, Author, Layout};
use super::marketplace::{
    self, join_diagnostics, Entry, HttpClient, HttpResponse, MarketplaceRecord, MarketplaceStore,
    PluginSource, Registry,
};
use super::update;
use crate::config::{expand_tilde, PolicyDefault};
use crate::events::{BackendEvent, EventSink};

/// The bundled official marketplace id. It can never be removed or shadowed
/// by another add (design §5).
pub const BUNDLED_MARKETPLACE_ID: &str = "ducky-official";

/// A marketplace refreshed within this window is not refreshed again by an
/// update check (design §7).
const REFRESH_GRACE_SECONDS: i64 = 30;

/// The one-time warning the install sheet carries (design §6).
pub const TRUST_WARNING: &str = "enabling a plugin runs its code with your user account; MCP servers start local programs and reach the hosts listed above";

// ---------------------------------------------------------------------------
// Index and UI view types

/// One MCP server contributed by an enabled plugin (design §9).
#[derive(Debug, Clone, PartialEq)]
pub struct PluginServerRef {
    /// `plugin:<plugin-id>:<server-name>` (design §9).
    pub id: String,
    pub plugin_id: String,
    pub server: PluginServer,
}

/// The enabled components of every installed plugin, rebuilt on each reload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginIndex {
    pub plugins: Vec<InstalledPlugin>,
    /// Skills of enabled plugins only (design §4).
    pub skills: Vec<SkillEntry>,
    /// Servers of enabled plugins that are not switched off.
    pub servers: Vec<PluginServerRef>,
    /// Subagents of enabled plugins only (design §10).
    pub subagents: Vec<PluginSubagent>,
}

/// One installed plugin, as the UI sees it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub previous_version: Option<String>,
    pub marketplace: Option<String>,
    pub source: PluginSource,
    pub resolved_sha: Option<String>,
    pub resolved_sha256: Option<String>,
    pub layout: Layout,
    pub enabled: bool,
    pub update_policy: UpdatePolicy,
    pub disabled_servers: Vec<String>,
    pub status: PluginStatus,
    pub diagnostics: Vec<Diagnostic>,
    pub available_update: Option<super::install::AvailableUpdate>,
    pub last_checked_at: Option<String>,
    pub installed_at: String,
    pub tree_hash: Option<String>,
    pub package_dir: String,
    pub data_dir: String,
    pub skills: Vec<PluginSkillView>,
    pub servers: Vec<PluginServerView>,
    pub subagents: Vec<PluginSubagentView>,
}

/// `plugins_list` returns exactly this (the index entry).
pub type PluginSummary = InstalledPlugin;

/// One skill, listed on the plugin row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PluginSkillView {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub path: String,
}

/// One MCP server, listed on the plugin row. Header and env values never
/// reach the webview.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PluginServerView {
    pub name: String,
    pub id: String,
    /// True only when the plugin is enabled and this server is consented.
    pub enabled: bool,
    /// `stdio`, `streamable-http` or `sse`.
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
}

/// One subagent, listed on the plugin row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PluginSubagentView {
    pub name: String,
    pub description: String,
}

/// `plugin_detail`: the index entry plus the manifest's trust fields.
#[derive(Debug, Clone, Serialize)]
pub struct PluginDetail {
    #[serde(flatten)]
    pub plugin: InstalledPlugin,
    pub description: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub trust_warning: String,
}

/// `marketplaces_list` / `marketplace_add` / `marketplace_refresh`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MarketplaceSummary {
    pub id: String,
    pub name: String,
    pub source: PluginSource,
    pub registry_path: String,
    pub auto_refresh: bool,
    pub last_refreshed_at: Option<String>,
    pub resolved_sha: Option<String>,
    pub bundled: bool,
    pub hidden: bool,
    pub error: Option<String>,
    pub entry_count: usize,
}

/// What the add-marketplace form sends: one string in any accepted form.
#[derive(Debug, Clone, Deserialize)]
pub struct MarketplaceInput {
    pub source: String,
}

/// One normalised registry entry plus its install state (design §12).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CatalogEntry {
    pub marketplace_id: String,
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
    pub available: bool,
    pub reason: Option<String>,
    pub source_kind: String,
    /// The installed plugin's id when this entry is installed.
    pub installed: Option<String>,
    pub installed_version: Option<String>,
    pub update_available: bool,
}

/// One applied update or rollback.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PluginUpdateInfo {
    pub plugin_id: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

// ---------------------------------------------------------------------------
// Manager

/// Owns the two stores and the derived index.
pub struct PluginManager {
    pub data_dir: PathBuf,
    /// The machine home directory: the compat skill root lives under it.
    pub home: PathBuf,
    resource_dir: Option<PathBuf>,
    /// Holds `<plugin-id>/package` for every installed plugin.
    pub plugins_dir: PathBuf,
    /// The parent of every plugin's data directory. `uninstall` guards its
    /// data delete as a direct child of this root (ruling R25), so callers
    /// pass the parent, never `plugin-data/<id>`.
    pub plugin_data_dir: PathBuf,
    /// `<data_dir>/marketplaces`: the records and per-marketplace caches.
    pub marketplaces_dir: PathBuf,
    //
    // LOCK ORDER: marketplaces -> install -> index. Every path that takes
    // more than one of these locks takes them in this order, so two racing
    // commands cannot deadlock (ruling R33).
    //
    marketplaces: Mutex<MarketplaceStore>,
    install: Mutex<InstallStore>,
    index: RwLock<PluginIndex>,
    sink: Arc<dyn EventSink>,
    http: Arc<dyn HttpClient>,
}

impl PluginManager {
    /// Build the manager, reload the index and log a load failure. Task 11
    /// seeds the bundled marketplace here, before the first reload.
    pub fn new(data_dir: &Path, home: &Path, resource_dir: Option<&Path>) -> Arc<Self> {
        Self::with_sink(data_dir, home, resource_dir, Arc::new(NullSink))
    }

    /// Build with an event sink but no HTTP client: local marketplaces work,
    /// a remote fetch fails with a clear error. The app injects the real
    /// client through [`Self::with_http`].
    pub fn with_sink(
        data_dir: &Path,
        home: &Path,
        resource_dir: Option<&Path>,
        sink: Arc<dyn EventSink>,
    ) -> Arc<Self> {
        Self::with_http(data_dir, home, resource_dir, sink, Arc::new(NullHttp))
    }

    /// The full constructor: `http` is the marketplace fetch client.
    pub fn with_http(
        data_dir: &Path,
        home: &Path,
        resource_dir: Option<&Path>,
        sink: Arc<dyn EventSink>,
        http: Arc<dyn HttpClient>,
    ) -> Arc<Self> {
        let plugins_dir = data_dir.join("plugins");
        let marketplaces_dir = data_dir.join("marketplaces");
        let manager = Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            home: home.to_path_buf(),
            resource_dir: resource_dir.map(Path::to_path_buf),
            marketplaces_dir: marketplaces_dir.clone(),
            plugins_dir: plugins_dir.clone(),
            plugin_data_dir: data_dir.join("plugin-data"),
            marketplaces: Mutex::new(MarketplaceStore::load(&marketplaces_dir)),
            install: Mutex::new(InstallStore::load(&plugins_dir)),
            index: RwLock::new(PluginIndex::default()),
            sink,
            http,
        });
        if let Err(err) = manager.reload() {
            tracing::warn!("cannot build the plugin index: {err}");
        }
        manager
    }

    /// Task 11's bundled seeding needs the resource directory.
    pub fn resource_dir(&self) -> Option<&Path> {
        self.resource_dir.as_deref()
    }

    pub fn index(&self) -> PluginIndex {
        self.index.read().unwrap().clone()
    }

    pub fn list(&self) -> Vec<PluginSummary> {
        self.index.read().unwrap().plugins.clone()
    }

    /// Rebuild the index from the install records and the packages on disk.
    ///
    /// A missing package keeps its record and reports `Error`; an invalid
    /// manifest reports `Invalid`; a healthy record keeps `UpdateAvailable`
    /// and `ModifiedLocally` and otherwise follows `enabled`. Debris in the
    /// plugins directory is ignored. Nothing here panics on a half-deleted
    /// state, and nothing is written.
    pub fn reload(&self) -> Result<(), String> {
        let store = self.install.lock().unwrap();
        if store.poisoned {
            let detail = store
                .diagnostics
                .iter()
                .map(|diag| diag.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!("corrupt install store: {detail}"));
        }
        let mut index = PluginIndex::default();
        for record in &store.records {
            let (plugin, discovered) = self.scan(record);
            if record.enabled {
                index.skills.extend(discovered.skills.iter().cloned());
                index.subagents.extend(discovered.subagents.iter().cloned());
                for server in &discovered.servers {
                    if !is_server_disabled(record, &server.name) {
                        index.servers.push(PluginServerRef {
                            id: format!("plugin:{}:{}", record.id, server.name),
                            plugin_id: record.id.clone(),
                            server: server.clone(),
                        });
                    }
                }
            }
            index.plugins.push(plugin);
        }
        // Debris in the plugins directory is ignored, with one warning naming it.
        if let Ok(entries) = std::fs::read_dir(&self.plugins_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == ".staging" || store.records.iter().any(|record| record.id == name) {
                    continue;
                }
                tracing::warn!("ignoring unknown entry in the plugins directory: {name}");
            }
        }
        // The install lock is held through the publish so a concurrent save
        // cannot slip between the scan and the write (order: install -> index).
        *self.index.write().unwrap() = index;
        drop(store);
        Ok(())
    }

    /// One plugin's full trust detail; `Ok(None)` when it is not installed.
    /// An unsafe record id is an error: no path is built from it.
    pub fn detail(&self, id: &str) -> Result<Option<PluginDetail>, String> {
        let plugin = {
            let index = self.index.read().unwrap();
            match index.plugins.iter().find(|plugin| plugin.id == id) {
                Some(plugin) => plugin.clone(),
                None => return Ok(None),
            }
        };
        // Never join a hand-edited id first: an unsafe id must be refused
        // before the package path is built (Task 5 containment).
        let package = install::package_dir(&self.plugins_dir, id)?.join("package");
        let (description, author, homepage, license) = match manifest::load(&package) {
            Ok(manifest) => (
                manifest.description,
                manifest.author.as_ref().and_then(author_name),
                manifest.homepage,
                manifest.license,
            ),
            Err(_) => (None, None, None, None),
        };
        Ok(Some(PluginDetail {
            plugin,
            description,
            author,
            homepage,
            license,
            trust_warning: TRUST_WARNING.to_string(),
        }))
    }

    /// Every registry entry of every marketplace (or of one), with the install
    /// state attached. A marketplace that cannot be read is skipped; its
    /// `MarketplaceSummary.error` carries the reason.
    pub fn catalog(&self, marketplace: Option<&str>) -> Result<Vec<CatalogEntry>, String> {
        let targets: Vec<MarketplaceRecord> = {
            let store = self.marketplaces.lock().unwrap();
            store
                .records
                .iter()
                .filter(|record| marketplace.is_none_or(|id| record.id == id))
                .cloned()
                .collect()
        };
        let records: Vec<InstallRecord> = self.install.lock().unwrap().records.clone();
        let mut entries = Vec::new();
        for record in &targets {
            let registry = match self.load_registry(record) {
                Ok(registry) => registry,
                Err(err) => {
                    tracing::warn!("marketplace `{}` cannot be read: {err}", record.id);
                    continue;
                }
            };
            for entry in registry.entries {
                let installed = records.iter().find(|item| {
                    item.marketplace.as_deref() == Some(record.id.as_str())
                        && (item.name == entry.name || item.id == entry.name)
                });
                entries.push(CatalogEntry {
                    marketplace_id: record.id.clone(),
                    name: entry.name,
                    display_name: entry.display_name,
                    description: entry.description,
                    version: entry.version,
                    category: entry.category,
                    tags: entry.tags,
                    author: entry.author,
                    homepage: entry.homepage,
                    icon: entry.icon,
                    keywords: entry.keywords,
                    available: entry.available,
                    reason: entry.reason,
                    source_kind: source_kind(&entry.source).to_string(),
                    installed: installed.map(|item| item.id.clone()),
                    installed_version: installed.and_then(|item| item.version.clone()),
                    update_available: installed
                        .is_some_and(|item| item.available_update.is_some()),
                });
            }
        }
        Ok(entries)
    }

    /// Install from a marketplace entry (`marketplace_id` + `name`) or from a
    /// direct source. Returns the new record id.
    ///
    /// `policy_default` overrides the content-derived policy when the setting
    /// is not `Content` (ruling R18). A fresh install's servers are recorded
    /// off until the user consents per server (design §6).
    pub fn install(
        &self,
        marketplace_id: Option<&str>,
        name: Option<&str>,
        source: Option<&PluginSource>,
        policy_default: PolicyDefault,
    ) -> Result<String, String> {
        let (resolved, entry, marketplace) = match (marketplace_id, name, source) {
            (Some(marketplace_id), Some(name), _) => {
                let registry = self.load_registry_for_id(marketplace_id)?;
                let entry = registry
                    .entries
                    .into_iter()
                    .find(|entry| entry.name == name)
                    .ok_or_else(|| {
                        format!("marketplace `{marketplace_id}` has no entry `{name}`")
                    })?;
                if !entry.available {
                    return Err(entry
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("entry `{name}` is not available")));
                }
                let root = self.marketplace_root(marketplace_id);
                (
                    resolve_entry_source(entry.source.clone(), root.as_deref()),
                    Some(entry),
                    Some(marketplace_id.to_string()),
                )
            }
            (None, None, Some(source)) => (source.clone(), None, None),
            _ => {
                return Err(
                    "provide a marketplace entry (marketplace + name) or a direct source"
                        .to_string(),
                )
            }
        };
        let id;
        {
            let mut store = self.install.lock().unwrap();
            let mut record = install::install(
                &mut store,
                &self.plugins_dir,
                &resolved,
                entry.as_ref(),
                marketplace.as_deref(),
            )?;
            match policy_default {
                PolicyDefault::Content => {}
                PolicyDefault::Auto => record.update_policy = UpdatePolicy::Auto,
                PolicyDefault::Manual => record.update_policy = UpdatePolicy::Manual,
            }
            record.disabled_servers = owned_servers(&self.plugins_dir, &record.id);
            id = record.id.clone();
            if let Some(slot) = store.records.iter_mut().find(|item| item.id == id) {
                *slot = record;
            }
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
        }
        // `install::install` fetches, validates and places in one call, so the
        // three phases are reported once it returns. Every phase carries the
        // record's real id: the UI attaches them to one row by id, and the
        // source name is not the id.
        self.progress(&id, "fetch", "fetching the plugin source");
        self.progress(&id, "validate", "manifest and components validated");
        self.progress(
            &id,
            "place",
            &format!("installed at {}", self.plugins_dir.join(&id).display()),
        );
        self.reload()?;
        self.changed("install");
        Ok(id)
    }

    /// Uninstall one plugin. Returns the `plugin:<id>:<server>` ids the
    /// package owned, so the MCP layer can disconnect them; nothing is
    /// disconnected here.
    pub fn uninstall(&self, id: &str, delete_data: bool) -> Result<Vec<String>, String> {
        let owned;
        {
            let mut store = self.install.lock().unwrap();
            // The data root is the parent (ruling R25).
            owned = install::uninstall(
                &mut store,
                &self.plugins_dir,
                &self.plugin_data_dir,
                id,
                delete_data,
            )?;
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
        }
        self.reload()?;
        self.changed("uninstall");
        Ok(owned)
    }

    pub fn set_enabled(&self, id: &str, on: bool) -> Result<(), String> {
        {
            let mut store = self.install.lock().unwrap();
            record_mut(&mut store, id)?.enabled = on;
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
        }
        self.reload()?;
        self.changed(if on { "enable" } else { "disable" });
        Ok(())
    }

    /// Toggle one server's one-time consent. `disabled_servers` is the only
    /// consent field on the record.
    pub fn set_server_enabled(&self, id: &str, server: &str, on: bool) -> Result<(), String> {
        {
            let mut store = self.install.lock().unwrap();
            record_ref(&store, id)?;
            if !owned_servers(&self.plugins_dir, id)
                .iter()
                .any(|name| name == server)
            {
                return Err(format!("plugin `{id}` has no MCP server `{server}`"));
            }
            let record = record_mut(&mut store, id)?;
            record.disabled_servers.retain(|name| name != server);
            if !on {
                record.disabled_servers.push(server.to_string());
            }
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
        }
        self.reload()?;
        self.changed(if on { "server_enable" } else { "server_disable" });
        Ok(())
    }

    pub fn set_policy(&self, id: &str, policy: UpdatePolicy) -> Result<(), String> {
        {
            let mut store = self.install.lock().unwrap();
            record_mut(&mut store, id)?.update_policy = policy;
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
        }
        self.reload()?;
        self.changed("policy");
        Ok(())
    }

    /// Check every installed plugin for updates.
    ///
    /// First refresh the marketplaces that back installed plugins unless one
    /// was refreshed less than 30 seconds ago; then compare each record. The
    /// results are cached on the records and emitted one by one.
    pub async fn check_updates(&self) -> Result<Vec<PluginUpdateInfo>, String> {
        let behind: Vec<String> = {
            let store = self.install.lock().unwrap();
            let mut ids: Vec<String> = store
                .records
                .iter()
                .filter_map(|record| record.marketplace.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        };
        let now = chrono::Utc::now();
        for id in &behind {
            let fresh = {
                let store = self.marketplaces.lock().unwrap();
                store
                    .records
                    .iter()
                    .find(|record| record.id == *id)
                    .and_then(|record| record.last_refreshed_at.as_deref())
                    .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
                    .is_some_and(|stamp| {
                        (now - stamp.with_timezone(&chrono::Utc)).num_seconds()
                            < REFRESH_GRACE_SECONDS
                    })
            };
            if !fresh {
                self.refresh_one(id).await;
            }
        }
        // Resolve the registries before taking the install lock (lock order).
        let mut registries: HashMap<String, Registry> = HashMap::new();
        for id in &behind {
            if let Ok(registry) = self.load_registry_for_id(id) {
                registries.insert(id.clone(), registry);
            }
        }
        let mut updates = Vec::new();
        {
            let mut store = self.install.lock().unwrap();
            for record in store.records.iter_mut() {
                let registry = record.marketplace.as_deref().and_then(|id| registries.get(id));
                match update::check_one(record, registry, &self.plugins_dir) {
                    Ok(Some(found)) => updates.push(PluginUpdateInfo {
                        plugin_id: record.id.clone(),
                        from: record.version.clone(),
                        to: found.version.clone().or(found.resolved_sha.clone()),
                    }),
                    Ok(None) => {}
                    Err(err) => tracing::warn!("update check for `{}` failed: {err}", record.id),
                }
            }
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
        }
        for info in &updates {
            self.emit(BackendEvent::PluginUpdateAvailable {
                plugin_id: info.plugin_id.clone(),
                from: info.from.clone(),
                to: info.to.clone(),
            });
        }
        self.changed("check_updates");
        self.reload()?;
        Ok(updates)
    }

    /// Apply one update. `force` overwrites a locally modified package; the
    /// UI asks first and passes `true` only after confirmation.
    pub fn update(&self, id: &str, force: bool) -> Result<PluginUpdateInfo, String> {
        let entry = self.marketplace_entry(id)?;
        let result;
        {
            let mut store = self.install.lock().unwrap();
            // Flush anything unsaved before `apply` reads the store from disk.
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
            let from = record_ref(&store, id)?.version.clone();
            // An update must not silently consent to servers the new version
            // adds; compare the old package with the new one (design §6).
            let before = owned_servers(&self.plugins_dir, id);
            let applied = {
                let record = record_mut(&mut store, id)?;
                update::apply(record, &self.plugins_dir, entry.as_ref(), force)
            };
            result = match applied {
                Ok(outcome) => {
                    let after = owned_servers(&self.plugins_dir, id);
                    {
                        let record = record_mut(&mut store, id)?;
                        seed_new_servers(record, &before, &after);
                    }
                    match store.save(&self.plugins_dir) {
                        Ok(()) => Ok(PluginUpdateInfo {
                            plugin_id: id.to_string(),
                            from,
                            to: outcome.to,
                        }),
                        Err(err) => Err(format!("cannot write the install record: {err}")),
                    }
                }
                // `apply` marks the record `ModifiedLocally` and writes it
                // before it fails, so the index is rebuilt on this path too;
                // otherwise `plugins_list` serves the old status until the
                // app restarts.
                Err(err) => Err(err),
            };
        }
        self.reload()?;
        self.changed("update");
        result
    }

    /// Apply every cached update. One plugin's failure never stops the rest.
    pub fn update_all(&self, force: bool) -> Result<Vec<PluginUpdateInfo>, String> {
        let ids: Vec<String> = {
            let store = self.install.lock().unwrap();
            store
                .records
                .iter()
                .filter(|record| record.available_update.is_some())
                .map(|record| record.id.clone())
                .collect()
        };
        let mut done = Vec::new();
        for id in ids {
            match self.update(&id, force) {
                Ok(info) => done.push(info),
                Err(err) => tracing::warn!("plugin `{id}` was not updated: {err}"),
            }
        }
        Ok(done)
    }

    /// Swap the one kept previous revision back into place.
    pub fn rollback(&self, id: &str) -> Result<PluginUpdateInfo, String> {
        let (from, to);
        {
            let mut store = self.install.lock().unwrap();
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
            let record = record_mut(&mut store, id)?;
            from = record.version.clone();
            update::rollback(record, &self.plugins_dir)?;
            to = record.version.clone();
            store
                .save(&self.plugins_dir)
                .map_err(|err| format!("cannot write the install record: {err}"))?;
        }
        self.reload()?;
        self.changed("rollback");
        Ok(PluginUpdateInfo {
            plugin_id: id.to_string(),
            from,
            to,
        })
    }

    /// The package or data directory of one installed plugin, for the opener.
    pub fn folder(&self, id: &str, which: Option<&str>) -> Result<PathBuf, String> {
        {
            let store = self.install.lock().unwrap();
            record_ref(&store, id)?;
        }
        // Build every path through `package_dir`: an unsafe hand-edited id is
        // refused before any join (Task 5 containment).
        let path = match which.unwrap_or("package") {
            "package" => install::package_dir(&self.plugins_dir, id)?.join("package"),
            "data" => install::package_dir(&self.plugin_data_dir, id)?,
            other => return Err(format!("unknown plugin folder `{other}`")),
        };
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        Ok(path)
    }

    // --- Marketplaces ---

    pub fn marketplaces(&self) -> Vec<MarketplaceSummary> {
        let records = self.marketplaces.lock().unwrap().records.clone();
        records.iter().map(|record| self.summary(record)).collect()
    }

    /// Add one marketplace. The bundled id is reserved; an existing id is
    /// refused. A local source is read now so the name and entry count are
    /// real without a network round trip.
    pub fn add_marketplace(
        &self,
        input: MarketplaceInput,
        name: Option<String>,
    ) -> Result<MarketplaceSummary, String> {
        let source = parse_marketplace_source(&input.source, &self.home)?;
        let fallback = source_name(&source);
        let explicit = name.as_deref().map(str::trim).filter(|name| !name.is_empty());
        let id = install::slug(explicit.unwrap_or(&fallback));
        if id.is_empty() {
            return Err("marketplace name must contain a letter or digit".to_string());
        }
        if id == BUNDLED_MARKETPLACE_ID {
            return Err(format!(
                "`{BUNDLED_MARKETPLACE_ID}` is reserved for the bundled marketplace"
            ));
        }
        let mut record = MarketplaceRecord {
            id,
            name: explicit.unwrap_or(&fallback).to_string(),
            source,
            registry_path: String::new(),
            auto_refresh: false,
            last_refreshed_at: None,
            resolved_sha: None,
            bundled: false,
            hidden: false,
            error: None,
            extra: Default::default(),
        };
        record.normalise_auto_refresh();
        if matches!(record.source, PluginSource::Path { .. }) {
            match self.load_registry(&record) {
                Ok(registry) => {
                    let root = match &record.source {
                        PluginSource::Path { path } => Some(PathBuf::from(path)),
                        _ => None,
                    };
                    if explicit.is_none() {
                        record.name = registry.name.clone();
                    }
                    record.registry_path = root
                        .as_deref()
                        .and_then(|root| registry.registry_path.strip_prefix(root).ok())
                        .map(|relative| relative.display().to_string())
                        .unwrap_or_else(|| registry.registry_path.display().to_string());
                    record.last_refreshed_at = Some(now());
                }
                Err(err) => record.error = Some(err),
            }
        }
        {
            let mut store = self.marketplaces.lock().unwrap();
            if store.records.iter().any(|item| item.id == record.id) {
                return Err(format!("marketplace `{}` already exists", record.id));
            }
            store.records.push(record.clone());
            store
                .save(&self.marketplaces_dir)
                .map_err(|err| format!("cannot write marketplaces.json: {err}"))?;
        }
        self.changed("marketplace_add");
        Ok(self.summary(&record))
    }

    /// Remove one marketplace. Refused while plugins installed from it remain
    /// unless `confirm` is set; the bundled marketplace can never be removed.
    /// Installed plugins become orphaned and keep updating from their own
    /// recorded source.
    pub fn remove_marketplace(&self, id: &str, confirm: bool) -> Result<(), String> {
        // Lock order: marketplaces -> install.
        let mut store = self.marketplaces.lock().unwrap();
        let Some(index) = store.records.iter().position(|record| record.id == id) else {
            return Err(format!("marketplace `{id}` is not known"));
        };
        // The reserved id is never removable, even when a hand-edited record
        // clears the `bundled` flag (design §5).
        if store.records[index].bundled || id == BUNDLED_MARKETPLACE_ID {
            return Err(format!("the bundled marketplace `{id}` cannot be removed"));
        }
        let users = {
            let install = self.install.lock().unwrap();
            install
                .records
                .iter()
                .filter(|record| record.marketplace.as_deref() == Some(id))
                .count()
        };
        if users > 0 && !confirm {
            return Err(format!(
                "marketplace `{id}` has {users} installed plugin(s); confirm to remove it anyway"
            ));
        }
        store.records.remove(index);
        store
            .save(&self.marketplaces_dir)
            .map_err(|err| format!("cannot write marketplaces.json: {err}"))?;
        drop(store);
        self.changed("marketplace_remove");
        Ok(())
    }

    /// Refresh one marketplace, or every marketplace. Each refresh emits
    /// `MarketplaceRefreshed` with its error (or `None`), so the UI updates
    /// even when a single marketplace fails.
    pub async fn refresh_marketplace(
        &self,
        id: Option<&str>,
    ) -> Result<Vec<MarketplaceSummary>, String> {
        let ids: Vec<String> = {
            let store = self.marketplaces.lock().unwrap();
            match id {
                Some(id) => {
                    if !store.records.iter().any(|record| record.id == id) {
                        return Err(format!("marketplace `{id}` is not known"));
                    }
                    vec![id.to_string()]
                }
                None => store
                    .records
                    .iter()
                    .map(|record| record.id.clone())
                    .collect(),
            }
        };
        let mut summaries = Vec::new();
        for id in &ids {
            if let Some(summary) = self.refresh_one(id).await {
                summaries.push(summary);
            }
        }
        self.changed("marketplace_refresh");
        Ok(summaries)
    }

    pub fn set_marketplace_auto_refresh(&self, id: &str, on: bool) -> Result<(), String> {
        {
            let mut store = self.marketplaces.lock().unwrap();
            let Some(record) = store.records.iter_mut().find(|record| record.id == id) else {
                return Err(format!("marketplace `{id}` is not known"));
            };
            record.auto_refresh = on;
            record.normalise_auto_refresh();
            store
                .save(&self.marketplaces_dir)
                .map_err(|err| format!("cannot write marketplaces.json: {err}"))?;
        }
        self.changed("marketplace_auto_refresh");
        Ok(())
    }

    // --- Internals ---

    /// Refresh one marketplace in place, save the record and emit the event.
    /// The record keeps its cached registry and error text on failure.
    async fn refresh_one(&self, id: &str) -> Option<MarketplaceSummary> {
        // A std lock guard must never live across an await.
        let record = {
            let store = self.marketplaces.lock().unwrap();
            store.records.iter().find(|record| record.id == id).cloned()
        };
        let mut record = record?;
        let result = marketplace::refresh(&mut record, &self.marketplaces_dir, self.http.as_ref()).await;
        {
            let mut store = self.marketplaces.lock().unwrap();
            if let Some(slot) = store.records.iter_mut().find(|item| item.id == id) {
                *slot = record.clone();
            }
            if let Err(err) = store.save(&self.marketplaces_dir) {
                tracing::warn!("cannot write marketplaces.json: {err}");
            }
        }
        if let Err(err) = result {
            tracing::warn!("marketplace `{id}` refresh failed: {err}");
        }
        self.emit(BackendEvent::MarketplaceRefreshed {
            marketplace_id: id.to_string(),
            error: record.error.clone(),
        });
        Some(self.summary(&record))
    }

    /// Every discovered component of one record, with the current status.
    fn scan(&self, record: &InstallRecord) -> (InstalledPlugin, Discovered) {
        let mut status = healthy_status(record);
        let mut diagnostics = Vec::new();
        let mut discovered = Discovered::default();
        match install::package_dir(&self.plugins_dir, &record.id) {
            Err(err) => {
                status = PluginStatus::Error;
                diagnostics.push(package_error(err));
            }
            Ok(dir) => {
                let package = dir.join("package");
                if !package.is_dir() {
                    status = PluginStatus::Error;
                    diagnostics.push(package_error(format!(
                        "package directory {} is missing",
                        package.display()
                    )));
                } else {
                    match manifest::load(&package) {
                        Err(errors) => {
                            status = PluginStatus::Invalid;
                            diagnostics = errors;
                        }
                        Ok(manifest) => {
                            discovered = discover(&package, &manifest);
                            diagnostics = discovered.diagnostics.clone();
                        }
                    }
                }
            }
        }
        (
            self.plugin_view(record, status, diagnostics, &discovered),
            discovered,
        )
    }

    fn plugin_view(
        &self,
        record: &InstallRecord,
        status: PluginStatus,
        diagnostics: Vec<Diagnostic>,
        discovered: &Discovered,
    ) -> InstalledPlugin {
        // An unsafe record id is refused before any join (Task 5 containment),
        // so the view never publishes a path outside the plugins directory.
        let package_dir = install::package_dir(&self.plugins_dir, &record.id)
            .map(|dir| dir.join("package").display().to_string())
            .unwrap_or_default();
        let data_dir = install::package_dir(&self.plugin_data_dir, &record.id)
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        InstalledPlugin {
            id: record.id.clone(),
            name: record.name.clone(),
            version: record.version.clone(),
            previous_version: record.previous_version.clone(),
            marketplace: record.marketplace.clone(),
            source: record.source.clone(),
            resolved_sha: record.resolved_sha.clone(),
            resolved_sha256: record.resolved_sha256.clone(),
            layout: record.layout,
            enabled: record.enabled,
            update_policy: record.update_policy,
            disabled_servers: record.disabled_servers.clone(),
            status,
            diagnostics,
            available_update: record.available_update.clone(),
            last_checked_at: record.last_checked_at.clone(),
            installed_at: record.installed_at.clone(),
            tree_hash: record.tree_hash.clone(),
            package_dir,
            data_dir,
            skills: discovered
                .skills
                .iter()
                .map(|skill| PluginSkillView {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    license: skill.license.clone(),
                    path: skill.dir.display().to_string(),
                })
                .collect(),
            servers: discovered
                .servers
                .iter()
                .map(|server| self.server_view(record, server))
                .collect(),
            subagents: discovered
                .subagents
                .iter()
                .map(|subagent| PluginSubagentView {
                    name: subagent.name.clone(),
                    description: subagent.description.clone(),
                })
                .collect(),
        }
    }

    fn server_view(&self, record: &InstallRecord, server: &PluginServer) -> PluginServerView {
        let mut view = PluginServerView {
            name: server.name.clone(),
            id: format!("plugin:{}:{}", record.id, server.name),
            enabled: record.enabled && !is_server_disabled(record, &server.name),
            transport: "stdio".to_string(),
            command: None,
            args: Vec::new(),
            url: None,
        };
        match &server.transport {
            PluginTransport::Stdio { command, args, .. } => {
                view.command = Some(command.clone());
                view.args = args.clone();
            }
            PluginTransport::Remote { kind, url, .. } => {
                view.transport = match kind {
                    RemoteKind::Sse => "sse".to_string(),
                    RemoteKind::StreamableHttp => "streamable-http".to_string(),
                };
                // The trust sheet shows the host, never the full URL: a query
                // string can carry a token (design §6).
                view.url = Some(display_host(url));
            }
        }
        view
    }

    /// The registry a marketplace record points at, read from its cache.
    fn load_registry(&self, record: &MarketplaceRecord) -> Result<Registry, String> {
        match &record.source {
            PluginSource::Path { path } => {
                let path = Path::new(path);
                if path.is_file() {
                    marketplace::parse_registry_file(path)
                } else {
                    marketplace::parse_registry(path).map_err(join_diagnostics)
                }
            }
            PluginSource::Url { .. } => marketplace::parse_registry_file(
                &self.marketplaces_dir.join(&record.id).join("registry.json"),
            ),
            PluginSource::Github { .. }
            | PluginSource::Git { .. }
            | PluginSource::GitSubdir { .. } => {
                let mut root = self.marketplaces_dir.join(&record.id).join("repo");
                if let Some(sub) = source_path(&record.source) {
                    root = root.join(sub);
                }
                marketplace::parse_registry(&root).map_err(join_diagnostics)
            }
            PluginSource::Unsupported { kind, .. } => {
                Err(format!("unsupported marketplace source kind: {kind}"))
            }
        }
    }

    fn load_registry_for_id(&self, id: &str) -> Result<Registry, String> {
        let record = {
            let store = self.marketplaces.lock().unwrap();
            store.records.iter().find(|record| record.id == id).cloned()
        };
        match record {
            Some(record) => self.load_registry(&record),
            None => Err(format!("marketplace `{id}` is not known")),
        }
    }

    /// The directory a marketplace's relative entry paths resolve against.
    /// `None` for an unknown or unsupported marketplace.
    fn marketplace_root(&self, id: &str) -> Option<PathBuf> {
        let record = {
            let store = self.marketplaces.lock().unwrap();
            store.records.iter().find(|record| record.id == id).cloned()
        }?;
        match &record.source {
            PluginSource::Path { path } => {
                let path = PathBuf::from(path);
                if path.is_file() {
                    path.parent().map(Path::to_path_buf)
                } else {
                    Some(path)
                }
            }
            PluginSource::Github { .. }
            | PluginSource::Git { .. }
            | PluginSource::GitSubdir { .. } => {
                let mut root = self.marketplaces_dir.join(&record.id).join("repo");
                if let Some(sub) = source_path(&record.source) {
                    root = root.join(sub);
                }
                Some(root)
            }
            PluginSource::Url { .. } => Some(self.marketplaces_dir.join(&record.id)),
            PluginSource::Unsupported { .. } => None,
        }
    }

    /// The registry entry that backs one installed plugin, if any.
    fn marketplace_entry(&self, id: &str) -> Result<Option<Entry>, String> {
        let (marketplace, name) = {
            let store = self.install.lock().unwrap();
            let record = record_ref(&store, id)?;
            (record.marketplace.clone(), record.name.clone())
        };
        let Some(marketplace) = marketplace else {
            return Ok(None);
        };
        // An orphaned marketplace falls back to the recorded source.
        let Ok(registry) = self.load_registry_for_id(&marketplace) else {
            return Ok(None);
        };
        Ok(registry
            .entries
            .into_iter()
            .find(|entry| entry.name == name || entry.name == id))
    }

    fn summary(&self, record: &MarketplaceRecord) -> MarketplaceSummary {
        let entry_count = self
            .load_registry(record)
            .map(|registry| registry.entries.len())
            .unwrap_or(0);
        MarketplaceSummary {
            id: record.id.clone(),
            name: record.name.clone(),
            source: record.source.clone(),
            registry_path: record.registry_path.clone(),
            auto_refresh: record.auto_refresh,
            last_refreshed_at: record.last_refreshed_at.clone(),
            resolved_sha: record.resolved_sha.clone(),
            bundled: record.bundled,
            hidden: record.hidden,
            error: record.error.clone(),
            entry_count,
        }
    }

    fn emit(&self, event: BackendEvent) {
        self.sink.emit(event);
    }

    fn changed(&self, reason: &str) {
        self.emit(BackendEvent::PluginsChanged {
            reason: reason.to_string(),
        });
    }

    fn progress(&self, plugin_id: &str, phase: &str, detail: &str) {
        self.emit(BackendEvent::PluginProgress {
            plugin_id: plugin_id.to_string(),
            phase: phase.to_string(),
            detail: detail.to_string(),
        });
    }
}

/// Sink used when no UI is attached (`PluginManager::new`).
struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: BackendEvent) {}
}

/// HTTP client used when the caller injects none. Constructing a manager must
/// never build a TLS client, because that needs a global crypto provider that
/// only the app installs; a remote fetch fails with a clear error instead.
struct NullHttp;

#[async_trait]
impl HttpClient for NullHttp {
    async fn get_conditional(
        &self,
        url: &str,
        _etag: Option<&str>,
        _last_modified: Option<&str>,
    ) -> Result<HttpResponse, String> {
        Err(format!("no HTTP client is configured (cannot fetch {url})"))
    }
}

// ---------------------------------------------------------------------------
// Helpers

fn record_ref<'a>(store: &'a InstallStore, id: &str) -> Result<&'a InstallRecord, String> {
    store
        .records
        .iter()
        .find(|record| record.id == id)
        .ok_or_else(|| format!("plugin `{id}` is not installed"))
}

fn record_mut<'a>(store: &'a mut InstallStore, id: &str) -> Result<&'a mut InstallRecord, String> {
    store
        .records
        .iter_mut()
        .find(|record| record.id == id)
        .ok_or_else(|| format!("plugin `{id}` is not installed"))
}

/// A healthy record keeps a cached update state; otherwise its status follows
/// the enable flag.
fn healthy_status(record: &InstallRecord) -> PluginStatus {
    match record.status {
        PluginStatus::UpdateAvailable | PluginStatus::ModifiedLocally => record.status,
        _ if record.enabled => PluginStatus::Enabled,
        _ => PluginStatus::InstalledDisabled,
    }
}

fn is_server_disabled(record: &InstallRecord, name: &str) -> bool {
    let full = format!("plugin:{}:{}", record.id, name);
    record
        .disabled_servers
        .iter()
        .any(|item| item == name || item == &full)
}

/// Every server in `after` that was not in `before` is switched off until the
/// user consents to it (design §6). A server the user already enabled stays
/// enabled.
fn seed_new_servers(record: &mut InstallRecord, before: &[String], after: &[String]) {
    for name in after {
        if !before.contains(name) && !record.disabled_servers.contains(name) {
            record.disabled_servers.push(name.clone());
        }
    }
}

fn package_error(message: String) -> Diagnostic {
    Diagnostic {
        level: DiagLevel::Error,
        target: "package".to_string(),
        message,
    }
}

/// Every server name the package declares, read from disk.
fn owned_servers(plugins_dir: &Path, id: &str) -> Vec<String> {
    let Ok(dir) = install::package_dir(plugins_dir, id) else {
        return Vec::new();
    };
    let package = dir.join("package");
    let Ok(manifest) = manifest::load(&package) else {
        return Vec::new();
    };
    discover(&package, &manifest)
        .servers
        .into_iter()
        .map(|server| server.name)
        .collect()
}

fn author_name(author: &Author) -> Option<String> {
    author
        .name
        .clone()
        .or_else(|| author.email.clone())
        .or_else(|| author.url.clone())
}

/// The host (with an explicit port) of a remote server URL, for the trust
/// sheet. The full URL is never shown: a query string can carry a token
/// (design §6). A URL that cannot be parsed yields no host at all.
pub(crate) fn display_host(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return String::new();
    };
    match (parsed.host_str(), parsed.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        (None, _) => String::new(),
    }
}

fn source_kind(source: &PluginSource) -> &'static str {
    match source {
        PluginSource::Github { .. } => "github",
        PluginSource::Git { .. } => "git",
        PluginSource::GitSubdir { .. } => "git-subdir",
        PluginSource::Url { .. } => "url",
        PluginSource::Path { .. } => "path",
        PluginSource::Unsupported { .. } => "unsupported",
    }
}

fn source_path(source: &PluginSource) -> Option<&str> {
    match source {
        PluginSource::Github { path, .. } | PluginSource::Git { path, .. } => path.as_deref(),
        PluginSource::GitSubdir { path, .. } => Some(path.as_str()),
        _ => None,
    }
}

/// A short name for the source, used for the record id and for progress when
/// no entry name is known.
fn source_name(source: &PluginSource) -> String {
    let tail = |value: &str| value.rsplit(['/', ':']).next().unwrap_or_default().to_string();
    let strip = |value: String| {
        value
            .trim_end_matches(".git")
            .trim_end_matches(".json")
            .to_string()
    };
    match source {
        PluginSource::Github { repo, .. } => strip(tail(repo)),
        PluginSource::Git { url, .. }
        | PluginSource::GitSubdir { url, .. }
        | PluginSource::Url { url } => strip(tail(url)),
        PluginSource::Path { path } => {
            let path = Path::new(path);
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if matches!(name.as_str(), "marketplace.json" | ".claude-plugin" | ".ducky") {
                path.parent()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or(name)
            } else {
                strip(name)
            }
        }
        PluginSource::Unsupported { .. } => String::new(),
    }
}

/// Normalise what the add-marketplace form sends.
///
/// `owner/repo` is GitHub; a `.json` HTTPS URL is a registry URL, any other
/// URL is git; a path-like string is a local source.
fn parse_marketplace_source(input: &str, home: &Path) -> Result<PluginSource, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("enter a marketplace source".to_string());
    }
    let path_like = input.starts_with('/')
        || input.starts_with("./")
        || input.starts_with("../")
        || input.starts_with('~')
        || is_windows_path(input);
    if path_like {
        return Ok(PluginSource::Path {
            path: expand_tilde(input, home).into_owned(),
        });
    }
    if input.starts_with("git@")
        || input.starts_with("http://")
        || input.starts_with("https://")
        || input.starts_with("git://")
        || input.starts_with("ssh://")
    {
        if input.ends_with(".json") {
            return Ok(PluginSource::Url {
                url: input.to_string(),
            });
        }
        return Ok(PluginSource::Git {
            url: input.to_string(),
            path: None,
            git_ref: None,
            sha: None,
        });
    }
    if !input.contains(char::is_whitespace)
        && input.matches('/').count() == 1
        && input.split('/').all(|part| !part.is_empty())
    {
        return Ok(PluginSource::Github {
            repo: input.to_string(),
            path: None,
            git_ref: None,
            sha: None,
        });
    }
    Err(format!(
        "cannot read marketplace source `{input}`: expected owner/repo, a git URL, an HTTPS registry URL, or a local path"
    ))
}

fn is_windows_path(input: &str) -> bool {
    let bytes = input.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// A marketplace entry's `./relative` path means "inside the marketplace
/// root" (design §5); absolute paths pass through.
fn resolve_entry_source(source: PluginSource, root: Option<&Path>) -> PluginSource {
    match source {
        PluginSource::Path { path } if !Path::new(&path).is_absolute() => match root {
            Some(root) => PluginSource::Path {
                path: root.join(path).display().to_string(),
            },
            None => PluginSource::Path { path },
        },
        other => other,
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
