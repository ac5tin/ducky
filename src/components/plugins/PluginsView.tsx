import { useEffect, useMemo, useState } from "react";
import { useStore } from "../../store";
import type { CatalogEntry } from "../../types";
import { filterCatalog, groupByCategory, progressRowViews } from "../../plugins";
import { Button } from "../modals/Modal";
import { Icon } from "../icons";
import { PluginCard } from "./PluginCard";
import { PluginDetailSheet } from "./PluginDetailSheet";
import { InstalledList } from "./InstalledList";
import { MarketplaceList } from "./MarketplaceList";

type Tab = "discover" | "installed" | "marketplaces";

const TABS: { id: Tab; label: string }[] = [
  { id: "discover", label: "Discover" },
  { id: "installed", label: "Installed" },
  { id: "marketplaces", label: "Marketplaces" },
];

/** The plugins view: Discover, Installed and Marketplaces (design §12). */
export function PluginsView() {
  const [tab, setTab] = useState<Tab>("discover");
  const [openEntry, setOpenEntry] = useState<CatalogEntry | null>(null);
  const [openPluginId, setOpenPluginId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState("all");

  const plugins = useStore((s) => s.plugins);
  const marketplaces = useStore((s) => s.marketplaces);
  const catalog = useStore((s) => s.catalog);
  const catalogLoading = useStore((s) => s.catalogLoading);
  const catalogError = useStore((s) => s.catalogError);
  const catalogMarketplace = useStore((s) => s.catalogMarketplace);
  const pluginProgress = useStore((s) => s.pluginProgress);
  const refreshPlugins = useStore((s) => s.refreshPlugins);
  const refreshSkills = useStore((s) => s.refreshSkills);
  const refreshMarketplaces = useStore((s) => s.refreshMarketplaces);
  const loadCatalog = useStore((s) => s.loadCatalog);
  const checkForPluginUpdates = useStore((s) => s.checkForPluginUpdates);
  const updateAllPlugins = useStore((s) => s.updateAllPlugins);
  const toast = useStore((s) => s.toast);

  useEffect(() => {
    void refreshPlugins().catch(() => {});
    void refreshSkills().catch(() => {});
    void refreshMarketplaces().catch(() => {});
    void loadCatalog(null);
  }, [refreshPlugins, refreshSkills, refreshMarketplaces, loadCatalog]);

  const categories = useMemo(() => groupByCategory(catalog), [catalog]);
  const shown = useMemo(
    () => filterCatalog(catalog, query, category),
    [catalog, query, category],
  );
  const progress = progressRowViews(pluginProgress);

  const check = async () => {
    const updates = await checkForPluginUpdates();
    if (updates.length === 0) toast("info", "Everything is up to date");
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-5xl px-6 py-8">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h1 className="text-xl font-bold">Plugins</h1>
            <p className="mt-1 max-w-xl text-sm leading-relaxed text-slate-500 dark:text-slate-400">
              Plugins add skills, subagents and MCP servers. A plugin stays off
              until you enable it, and each server starts separately.
            </p>
          </div>
          <div className="flex shrink-0 gap-2">
            <Button
              variant="secondary"
              disabled={plugins.length === 0}
              onClick={() => void check()}
            >
              <Icon name="refresh" className="h-4 w-4" />
              Check for updates
            </Button>
            <Button
              variant="secondary"
              disabled={plugins.length === 0}
              onClick={() => void updateAllPlugins()}
            >
              Update all
            </Button>
          </div>
        </div>

        <div
          className="mt-6 flex gap-1 border-b border-slate-200 dark:border-slate-800"
          role="tablist"
          aria-label="Plugin views"
        >
          {TABS.map((item) => (
            <button
              key={item.id}
              type="button"
              role="tab"
              aria-selected={tab === item.id}
              className={`-mb-px border-b-2 px-3.5 py-2 text-sm transition ${
                tab === item.id
                  ? "border-sky-500 font-medium text-slate-900 dark:text-white"
                  : "border-transparent text-slate-500 hover:text-slate-700 dark:text-slate-400 dark:hover:text-slate-200"
              }`}
              onClick={() => setTab(item.id)}
            >
              {item.label}
            </button>
          ))}
        </div>

        {tab === "discover" && (
          <>
            <div className="mt-5 flex flex-wrap gap-2">
              <select
                className={selectClass}
                value={catalogMarketplace ?? "all"}
                onChange={(e) =>
                  void loadCatalog(e.target.value === "all" ? null : e.target.value)
                }
              >
                <option value="all">All marketplaces</option>
                {marketplaces.map((marketplace) => (
                  <option key={marketplace.id} value={marketplace.id}>
                    {marketplace.name}
                  </option>
                ))}
              </select>
              <input
                className={`${selectClass} min-w-48 flex-1`}
                placeholder="Search plugins"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
              <select
                className={selectClass}
                value={category}
                onChange={(e) => setCategory(e.target.value)}
              >
                <option value="all">All categories</option>
                {categories
                  .filter((group) => group.category !== "")
                  .map((group) => (
                    <option key={group.category} value={group.category}>
                      {group.category}
                    </option>
                  ))}
              </select>
            </div>

            {progress.length > 0 && (
              <div className="mt-4 space-y-1.5">
                {progress.map((row) => (
                  <div
                    key={row.id}
                    className="flex items-center gap-2 rounded-xl border border-slate-200 px-3 py-2 text-xs dark:border-slate-700"
                  >
                    <Icon name="spinner" className="h-3.5 w-3.5 animate-spin text-sky-500" />
                    <span className="font-medium">{row.label}</span>
                    <span className="truncate text-slate-400">{row.detail}</span>
                  </div>
                ))}
              </div>
            )}

            {catalogError ? (
              <div className="mt-6 rounded-2xl border border-rose-200 bg-rose-50 p-6 dark:border-rose-900 dark:bg-rose-950/40">
                <p className="text-sm font-medium text-rose-700 dark:text-rose-300">
                  Could not load the catalog
                </p>
                <p className="mt-1 text-xs text-rose-600 dark:text-rose-400">
                  {catalogError}
                </p>
                <Button
                  className="mt-3"
                  variant="secondary"
                  onClick={() => void loadCatalog(catalogMarketplace)}
                >
                  Try again
                </Button>
              </div>
            ) : catalogLoading && catalog.length === 0 ? (
              <p className="mt-6 text-sm text-slate-400">Loading plugins…</p>
            ) : shown.length === 0 ? (
              <div className="mt-6 rounded-2xl border-2 border-dashed border-slate-200 p-10 text-center dark:border-slate-800">
                <Icon
                  name="puzzle"
                  className="mx-auto h-8 w-8 text-slate-300 dark:text-slate-600"
                />
                <p className="mt-3 text-sm font-medium">
                  {catalog.length === 0
                    ? "No plugins in this catalog yet"
                    : "No plugins match"}
                </p>
                <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-slate-400">
                  {catalog.length === 0
                    ? "Add a marketplace on the Marketplaces tab, or refresh it if its registry changed."
                    : "Try a different search or category."}
                </p>
              </div>
            ) : (
              <div className="mt-5 grid gap-3 sm:grid-cols-2">
                {shown.map((item) => (
                  <PluginCard
                    key={`${item.marketplace_id}:${item.name}`}
                    entry={item}
                    onOpen={() => setOpenEntry(item)}
                  />
                ))}
              </div>
            )}
          </>
        )}

        {tab === "installed" && (
          <InstalledList
            onDiscover={() => setTab("discover")}
            onDetail={setOpenPluginId}
          />
        )}

        {tab === "marketplaces" && <MarketplaceList />}
      </div>

      <PluginDetailSheet
        entry={openEntry}
        pluginId={openPluginId}
        onClose={() => {
          setOpenEntry(null);
          setOpenPluginId(null);
        }}
      />
    </div>
  );
}

const selectClass =
  "rounded-lg border border-slate-200 bg-white px-3 py-2 text-sm outline-none transition focus:border-sky-400 focus:ring-2 focus:ring-sky-100 dark:border-slate-700 dark:bg-slate-800 dark:focus:border-sky-500 dark:focus:ring-sky-900/40";
