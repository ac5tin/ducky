// Pure plugin helpers: display labels, catalog filtering, progress keying and
// the install flow's testable seam. No Tauri imports, so `node --test` can
// drive this module directly (like `subagents.ts` and `slashCommands.ts`).
import type {
  CatalogEntry,
  PluginDetail,
  PluginInstallRequest,
  PluginProgressEvent,
  PluginSummary,
  PluginUpdateAvailableEvent,
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
