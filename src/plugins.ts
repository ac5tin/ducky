// Pure plugin helpers: display labels, catalog filtering, progress keying and
// the install flow's testable seam. No Tauri imports, so `node --test` can
// drive this module directly (like `subagents.ts` and `slashCommands.ts`).
import type {
  CatalogEntry,
  MarketplaceSummary,
  PluginDetail,
  PluginInstallRequest,
  PluginProgressEvent,
  PluginServerView,
  PluginSource,
  PluginSummary,
  PluginUpdateAvailableEvent,
  PluginUpdateInfo,
  ServerOrigin,
  SkillSummary,
} from "./types";

/**
 * The row's status chip label. An error always wins: a plugin that failed to
 * load is not "Update available", however stale its package is.
 */
export function pluginStatusLabel(
  p: Pick<PluginSummary, "status" | "diagnostics" | "available_update">,
): string {
  if (
    p.status === "error" ||
    (p.diagnostics ?? []).some((d) => d.level === "error")
  ) {
    return "Error";
  }
  if (p.status === "invalid") return "Invalid";
  if (p.status === "modified_locally") return "Modified locally";
  if (p.status === "update_available" || p.available_update) {
    return "Update available";
  }
  return p.status === "enabled" ? "Enabled" : "Disabled";
}

/** One status chip's tone, for the row's colour. */
export type PluginStatusTone = "ok" | "warn" | "error" | "muted";

/** The tone that goes with `pluginStatusLabel` — the same precedence. */
export function pluginStatusTone(
  p: Pick<PluginSummary, "status" | "diagnostics" | "available_update">,
): PluginStatusTone {
  switch (pluginStatusLabel(p)) {
    case "Error":
    case "Invalid":
      return "error";
    case "Update available":
    case "Modified locally":
      return "warn";
    case "Enabled":
      return "ok";
    default:
      return "muted";
  }
}

/** One in-flight plugin operation's progress text. */
export interface PluginProgressRow {
  /** `fetch`, `validate` or `place` (design §11). */
  phase: string;
  /** The backend's human-readable step text. */
  detail: string;
}

/**
 * Apply one `plugin_progress` event. The key is the install record id the
 * backend stamps on every phase (Task 7's fix), never a source or entry name.
 */
export function withPluginProgress(
  current: Record<string, PluginProgressRow>,
  event: PluginProgressEvent,
): Record<string, PluginProgressRow> {
  return {
    ...current,
    [event.plugin_id]: { phase: event.phase, detail: event.detail },
  };
}

/** Drop one plugin's progress row once its operation ends (success or fail). */
export function withoutPluginProgress(
  current: Record<string, PluginProgressRow>,
  pluginId: string,
): Record<string, PluginProgressRow> {
  if (!(pluginId in current)) return current;
  const next = { ...current };
  delete next[pluginId];
  return next;
}

/** The gerund shown next to one install phase (design §11). */
export function progressPhaseLabel(phase: string): string {
  switch (phase) {
    case "fetch":
      return "Fetching";
    case "validate":
      return "Validating";
    case "place":
      return "Placing";
    default:
      return "Working";
  }
}

/** One in-flight operation as a display row. */
export interface PluginProgressView extends PluginProgressRow {
  id: string;
  label: string;
}

const PHASE_ORDER = ["fetch", "validate", "place"];

/** In-flight operations in phase order; ties keep insertion order. */
export function progressRowViews(
  progress: Record<string, PluginProgressRow>,
): PluginProgressView[] {
  const rank = (phase: string) => {
    const index = PHASE_ORDER.indexOf(phase);
    return index === -1 ? PHASE_ORDER.length : index;
  };
  return Object.entries(progress)
    .map(([id, row]) => ({ id, ...row, label: progressPhaseLabel(row.phase) }))
    .sort((a, b) => rank(a.phase) - rank(b.phase));
}

/**
 * Patch the row named by a `plugin_update_available` event, so the chip and
 * the Update button appear without a round trip. A status that outranks
 * "update available" (error, invalid, modified locally) is kept — the same
 * order `pluginStatusLabel` renders.
 */
export function withUpdateAvailable(
  plugins: PluginSummary[],
  event: PluginUpdateAvailableEvent,
): PluginSummary[] {
  if (!plugins.some((p) => p.id === event.plugin_id)) return plugins;
  const keep = new Set(["error", "invalid", "modified_locally"]);
  return plugins.map((p) => {
    if (p.id !== event.plugin_id) return p;
    return {
      ...p,
      status: keep.has(p.status) ? p.status : "update_available",
      available_update: { version: event.to, resolvedSha: null },
    };
  });
}

/** The searchable text of one catalog entry, lowercased once per query. */
function catalogHaystack(entry: CatalogEntry): string {
  return [
    entry.name,
    entry.display_name ?? "",
    entry.description ?? "",
    ...(entry.keywords ?? []),
    ...(entry.tags ?? []),
  ]
    .join(" ")
    .toLowerCase();
}

/**
 * Filter catalog entries by a free-text query and a category. The query is
 * trimmed and case-insensitive; an empty query keeps everything. `category`
 * of "" / null / "all" means every category.
 */
export function filterCatalog(
  entries: CatalogEntry[],
  query?: string | null,
  category?: string | null,
): CatalogEntry[] {
  const q = (query ?? "").trim().toLowerCase();
  const cat = (category ?? "").trim();
  const all = cat === "" || cat === "all";
  return entries.filter((entry) => {
    if (!all && (entry.category ?? "") !== cat) return false;
    if (!q) return true;
    return catalogHaystack(entry).includes(q);
  });
}

/** One category's entries, for the Discover category select. */
export interface CatalogCategoryGroup {
  /** The category label, or "" for entries with no category. */
  category: string;
  entries: CatalogEntry[];
}

/** One Discover card's single action button. */
export interface CatalogAction {
  kind: "install" | "installed" | "update" | "unavailable";
  label: string;
  disabled: boolean;
  /** Why the entry cannot be installed, when it cannot. */
  hint: string | null;
}

/**
 * The single action a catalog entry offers: an unavailable entry (with the
 * reason), an update, the installed badge, or Install. `busy` disables the
 * two buttons that start work.
 */
export function catalogEntryAction(
  entry: CatalogEntry,
  busy = false,
): CatalogAction {
  if (!entry.available) {
    return {
      kind: "unavailable",
      label: "Unavailable",
      disabled: true,
      hint: entry.reason,
    };
  }
  if (entry.update_available) {
    return { kind: "update", label: "Update", disabled: busy, hint: null };
  }
  if (entry.installed) {
    return { kind: "installed", label: "Installed", disabled: true, hint: null };
  }
  return { kind: "install", label: "Install", disabled: busy, hint: null };
}

/** Group entries by category, alphabetically; uncategorised entries last. */
export function groupByCategory(entries: CatalogEntry[]): CatalogCategoryGroup[] {
  const groups = new Map<string, CatalogEntry[]>();
  for (const entry of entries) {
    const category = entry.category ?? "";
    const bucket = groups.get(category);
    if (bucket) bucket.push(entry);
    else groups.set(category, [entry]);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => {
      if (a === "") return 1;
      if (b === "") return -1;
      return a.localeCompare(b);
    })
    .map(([category, group]) => ({ category, entries: group }));
}

/** Installed plugins a check found a newer revision for. */
export function availableUpdates(
  plugins: PluginSummary[],
): PluginSummary[] {
  return plugins.filter((p) => Boolean(p.available_update));
}

/** How many skills the plugin row carries; a missing list counts zero. */
export function skillCount(plugin: Pick<PluginSummary, "skills">): number {
  return plugin.skills?.length ?? 0;
}

/** A version for display; missing or blank versions render "unknown". */
export function formatVersion(v?: string | null): string {
  const trimmed = v?.trim();
  return trimmed ? trimmed : "unknown";
}

/** One plugin or marketplace source as a single display line. */
export function sourceLabel(source: PluginSource): string {
  switch (source.kind) {
    case "github":
      return source.path ? `${source.repo} · ${source.path}` : source.repo;
    case "git":
      return source.path ? `${source.url} · ${source.path}` : source.url;
    case "git-subdir":
      return `${source.url} · ${source.path}`;
    case "url":
      return source.url;
    case "path":
      return source.path;
    case "unsupported":
      return `${source.sourceKind} (unsupported)`;
  }
}

/**
 * The plugin that owns a connector row, or null for a user server. A plugin
 * origin without an id is not treated as a plugin row: the row routes by the
 * plugin id, never by a display name.
 */
export function pluginServerOwner(
  origin?: ServerOrigin | null,
): { id: string; name: string } | null {
  if (origin?.kind !== "plugin" || !origin.plugin_id) return null;
  return { id: origin.plugin_id, name: origin.plugin_name || origin.plugin_id };
}

/**
 * One server's trust line: the exact command and args for a local server, or
 * the host the backend published for a remote one. Only these fields can
 * appear — env and header values never reach the webview.
 */
export function serverTrustLine(
  server: Pick<PluginServerView, "transport" | "command" | "args" | "url">,
): string {
  const command = [server.command, ...server.args].filter(Boolean).join(" ");
  if (command) return `${server.transport} — ${command}`;
  return `${server.transport} — ${server.url ?? "host unknown"}`;
}

/** One plugin skill with its provenance and shadow state. */
export interface PluginSkillRow {
  /** The spec §4 stable id: `plugin:<plugin-id>:<name>`. */
  id: string;
  name: string;
  description: string;
  /** The id that won the name, when a higher-precedence root shadows this. */
  shadowed: string | null;
}

/**
 * A plugin's skills joined with the resolved skills list, so shadowing comes
 * from the same list the prompt block uses. A skill of a disabled plugin is
 * not in that list; `shadowed` stays null. The chip shows `id`, the stable
 * form the tools accept, not the short human provenance label.
 */
export function pluginSkillRows(
  plugin: Pick<PluginSummary, "id" | "skills">,
  resolved: SkillSummary[],
): PluginSkillRow[] {
  return (plugin.skills ?? []).map((skill) => {
    const id = `plugin:${plugin.id}:${skill.name}`;
    const found = resolved.find((candidate) => candidate.id === id);
    return {
      id,
      name: skill.name,
      description: skill.description,
      shadowed: found?.shadowed ?? null,
    };
  });
}

/**
 * Whether the Marketplaces tab may offer Remove. The backend refuses the
 * bundled id even when a hand-edited record says `bundled: false`
 * (`manager.rs`), so the control follows the same belt-and-braces rule.
 */
export function marketplaceRemovable(
  marketplace: Pick<MarketplaceSummary, "id" | "bundled">,
): boolean {
  return !marketplace.bundled && marketplace.id !== "ducky-official";
}

/** The spec §6 enable-consent summary: what a plugin will run. */
export interface PluginTrustSummary {
  warning: string;
  servers: { name: string; line: string }[];
  subagents: string[];
}

/** Build the enable-consent body from the plugin's components. */
export function pluginTrustSummary(
  plugin: Pick<PluginSummary, "servers" | "subagents">,
  warning: string,
): PluginTrustSummary {
  return {
    warning,
    servers: (plugin.servers ?? []).map((server) => ({
      name: server.name,
      line: serverTrustLine(server),
    })),
    subagents: (plugin.subagents ?? []).map((subagent) => subagent.name),
  };
}

/** A settings interval in hours as milliseconds; 0 (or less) = no timer. */
export function hoursToInterval(hours: number): number | null {
  return Number.isFinite(hours) && hours > 0 ? hours * 3_600_000 : null;
}

/**
 * Whether one plugin maintenance timer may run: never in a dev build, and
 * only for a positive hour count. The store arms both intervals through this
 * predicate, so a caller cannot start a timer that ADR-0002 says dev builds
 * must skip — `refreshConfig()` runs after every completed turn, so a dev
 * check at the `init()` call site alone does not hold.
 */
export function shouldSchedulePluginMaintenance(input: {
  dev: boolean;
  hours: number;
}): boolean {
  return !input.dev && hoursToInterval(input.hours) !== null;
}

/** The api surface the install flow needs, so tests can inject a fake. */
export interface PluginInstallApi {
  pluginInstall: (req: PluginInstallRequest) => Promise<PluginDetail>;
}

/**
 * Run one install with the api injected: `setCatalogLoading(true)` while the
 * fetch is in flight, `false` after, whatever the outcome. The store action
 * wraps this with the slice refresh and the toast.
 */
export async function runInstallPlugin(
  api: PluginInstallApi,
  req: PluginInstallRequest,
  setCatalogLoading: (loading: boolean) => void,
): Promise<PluginDetail> {
  setCatalogLoading(true);
  try {
    return await api.pluginInstall(req);
  } finally {
    setCatalogLoading(false);
  }
}

/** The api the install action needs: the install plus the reloads it drives. */
export interface PluginInstallActionApi extends PluginInstallApi {
  pluginsList: () => Promise<PluginSummary[]>;
  skillsList: () => Promise<SkillSummary[]>;
  marketplaceCatalog: (marketplace: string | null) => Promise<CatalogEntry[]>;
}

/** What one install action leaves for the store to apply, in one update. */
export interface PluginInstallActionOutcome {
  installed: PluginDetail;
  plugins: PluginSummary[];
  skills: SkillSummary[];
  catalog: CatalogEntry[];
}

/**
 * The install action's body with the api injected, so `node --test` can drive
 * it without Tauri. The loading flag wraps the install alone; a rejection
 * propagates before any reload runs, so a failed install reloads nothing.
 */
export async function runInstallPluginAction(
  api: PluginInstallActionApi,
  req: PluginInstallRequest,
  marketplace: string | null,
  setCatalogLoading: (loading: boolean) => void,
): Promise<PluginInstallActionOutcome> {
  const installed = await runInstallPlugin(api, req, setCatalogLoading);
  const [plugins, skills, catalog] = await Promise.all([
    api.pluginsList(),
    api.skillsList(),
    api.marketplaceCatalog(marketplace),
  ]);
  return { installed, plugins, skills, catalog };
}

/** The api one plugin-server switch needs, so tests can inject a fake. */
export interface PluginServerToggleApi {
  setServerEnabled: (
    id: string,
    server: string,
    enabled: boolean,
  ) => Promise<void>;
  connect: (serverId: string) => Promise<string>;
  disconnect: (serverId: string) => Promise<void>;
  isLive: (serverId: string) => boolean;
  toast: (message: string) => void;
}

/**
 * One plugin-server switch (design §6/§9). The consent write owns the record:
 * nothing else runs when it fails. A successful enable connects the server
 * unless it is already live; a successful disable always disconnects it (the
 * backend disconnect is infallible and clears a not-connected id too).
 *
 * Returns true when the consent write succeeded, so the caller reloads its
 * slices even when the connect or disconnect failed.
 */
export async function runPluginServerToggle(
  api: PluginServerToggleApi,
  id: string,
  server: string,
  enabled: boolean,
): Promise<boolean> {
  try {
    await api.setServerEnabled(id, server, enabled);
  } catch (err) {
    api.toast(`Could not ${enabled ? "start" : "stop"} the server: ${err}`);
    return false;
  }
  const serverId = `plugin:${id}:${server}`;
  if (!enabled) {
    try {
      await api.disconnect(serverId);
    } catch (err) {
      api.toast(`Could not stop ${server}: ${err}`);
    }
    return true;
  }
  if (api.isLive(serverId)) return true;
  try {
    const result = await api.connect(serverId);
    if (result === "error") {
      api.toast(`${server} failed to connect — see its detail view for logs.`);
    }
  } catch (err) {
    api.toast(`Could not connect ${server}: ${err}`);
  }
  return true;
}

/** The api one update or rollback needs to stop and restart its servers. */
export interface PluginSwapLifecycleApi {
  disconnect: (serverId: string) => Promise<void>;
  connect: (serverId: string) => Promise<string>;
  toast: (message: string) => void;
}

/**
 * Stop a plugin's live servers, run one package swap, then restart the servers
 * that were enabled before it (design §7). The swap's own return names those
 * servers, so a server the new revision removed is not guessed at. A failed
 * swap still restarts what was stopped, so a failed update never leaves the
 * plugin dead.
 */
export async function runPluginSwap(
  api: PluginSwapLifecycleApi,
  live: string[],
  swap: () => Promise<PluginUpdateInfo>,
): Promise<PluginUpdateInfo> {
  await stopPluginServers(api, live);
  let info: PluginUpdateInfo;
  try {
    info = await swap();
  } catch (err) {
    await restartPluginServers(api, live);
    throw err;
  }
  await restartPluginServers(api, info.enabled_servers);
  return info;
}

/** Live `plugin:<id>:<server>` ids for a batch of plugins, keyed by plugin. */
export type PluginLiveServers = Map<string, string[]>;

/**
 * The `plugin_update_all` shape of the same swap (design §7). The stopped
 * servers of a plugin the batch did not return belong to a failed update, and
 * they restart too: a failure keeps the plugin working on the old revision.
 */
export async function runPluginBatchSwap(
  api: PluginSwapLifecycleApi,
  live: PluginLiveServers,
  swap: () => Promise<PluginUpdateInfo[]>,
): Promise<PluginUpdateInfo[]> {
  const stopped = [...live.values()].flat();
  await stopPluginServers(api, stopped);
  let updates: PluginUpdateInfo[];
  try {
    updates = await swap();
  } catch (err) {
    await restartPluginServers(api, stopped);
    throw err;
  }
  const done = new Set(updates.map((info) => info.plugin_id));
  const reconnect = updates.flatMap((info) => info.enabled_servers);
  for (const [pluginId, ids] of live) {
    if (!done.has(pluginId)) reconnect.push(...ids);
  }
  await restartPluginServers(api, reconnect);
  return updates;
}

async function stopPluginServers(
  api: PluginSwapLifecycleApi,
  serverIds: string[],
): Promise<void> {
  for (const serverId of serverIds) {
    try {
      await api.disconnect(serverId);
    } catch (err) {
      api.toast(`Could not stop ${serverId}: ${err}`);
    }
  }
}

/**
 * The plugins an automatic pass updates, in check order: policy `auto` and
 * not locally modified (design §7). The local-modification flag comes from
 * the check's tree-hash comparison — the same verdict `apply` refuses on —
 * and the check never stores it, so the status a check overwrites cannot
 * re-enable the update. A manual plugin stays a toast action; a locally
 * modified one is skipped before any server is stopped, because an unforced
 * update refuses it.
 */
export function autoUpdateIds(
  updates: Pick<PluginUpdateInfo, "plugin_id" | "modified_locally">[],
  plugins: Pick<PluginSummary, "id" | "update_policy" | "status">[],
): string[] {
  const ids: string[] = [];
  for (const update of updates) {
    const plugin = plugins.find((p) => p.id === update.plugin_id);
    if (!plugin) continue;
    if (plugin.update_policy !== "auto") continue;
    if (update.modified_locally) continue;
    if (plugin.status === "modified_locally") continue;
    ids.push(update.plugin_id);
  }
  return ids;
}

async function restartPluginServers(
  api: PluginSwapLifecycleApi,
  serverIds: string[],
): Promise<void> {
  for (const serverId of serverIds) {
    try {
      const result = await api.connect(serverId);
      if (result === "error") {
        api.toast(
          `${serverId} failed to reconnect — see its detail view for logs.`,
        );
      }
    } catch (err) {
      api.toast(`Could not reconnect ${serverId}: ${err}`);
    }
  }
}
