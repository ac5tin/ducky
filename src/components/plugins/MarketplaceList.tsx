import { useState } from "react";
import { useStore } from "../../store";
import type { MarketplaceSummary } from "../../types";
import { marketplaceRemovable, sourceLabel } from "../../plugins";
import { Button, Modal } from "../modals/Modal";
import { Icon } from "../icons";
import { AddMarketplaceModal } from "./AddMarketplaceModal";

/** The Marketplaces tab: add, refresh, auto-refresh and remove. */
export function MarketplaceList() {
  const marketplaces = useStore((s) => s.marketplaces);
  const plugins = useStore((s) => s.plugins);
  const refreshMarketplace = useStore((s) => s.refreshMarketplace);
  const removeMarketplace = useStore((s) => s.removeMarketplace);
  const setMarketplaceAutoRefresh = useStore((s) => s.setMarketplaceAutoRefresh);
  const [adding, setAdding] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState<MarketplaceSummary | null>(null);

  const dependents = confirmRemove
    ? plugins.filter((plugin) => plugin.marketplace === confirmRemove.id)
    : [];

  const remove = (marketplace: MarketplaceSummary) => {
    const used = plugins.filter((plugin) => plugin.marketplace === marketplace.id);
    if (used.length > 0) setConfirmRemove(marketplace);
    else void removeMarketplace(marketplace.id);
  };

  return (
    <div className="mt-5 space-y-3">
      <div className="flex items-start justify-between gap-4">
        <p className="max-w-xl text-sm leading-relaxed text-slate-500 dark:text-slate-400">
          A marketplace is a registry of plugins. Ducky ships with the official
          one, which cannot be removed.
        </p>
        <Button className="shrink-0" onClick={() => setAdding(true)}>
          <Icon name="plus" className="h-4 w-4" />
          Add marketplace
        </Button>
      </div>

      {marketplaces.length === 0 ? (
        <div className="rounded-2xl border-2 border-dashed border-slate-200 p-10 text-center dark:border-slate-800">
          <Icon name="globe" className="mx-auto h-8 w-8 text-slate-300 dark:text-slate-600" />
          <p className="mt-3 text-sm font-medium">No marketplaces</p>
          <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-slate-400">
            Add one to browse its plugins in Discover.
          </p>
        </div>
      ) : (
        marketplaces.map((marketplace) => (
          <div
            key={marketplace.id}
            className="rounded-2xl border border-slate-200 p-4 dark:border-slate-700"
          >
            <div className="flex items-start gap-3">
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-1.5">
                  <span className="font-semibold">{marketplace.name}</span>
                  {marketplace.bundled && (
                    <span
                      className={chipClass}
                      title="Ships with Ducky and cannot be removed"
                    >
                      Bundled · read-only
                    </span>
                  )}
                  {marketplace.hidden && <span className={chipClass}>Hidden</span>}
                </div>
                <div className="mt-0.5 break-all text-xs text-slate-400">
                  {sourceLabel(marketplace.source)}
                </div>
                <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-slate-400">
                  <span>
                    {marketplace.entry_count} plugin
                    {marketplace.entry_count === 1 ? "" : "s"}
                  </span>
                  <span>{refreshedLabel(marketplace.last_refreshed_at)}</span>
                  {marketplace.registry_path && (
                    <span className="break-all">
                      Registry: {marketplace.registry_path}
                    </span>
                  )}
                </div>
                {marketplace.error && (
                  <p className="mt-2 text-xs text-rose-600 dark:text-rose-400">
                    {marketplace.error}
                  </p>
                )}
              </div>

              <div className="flex shrink-0 items-center gap-1.5">
                <Button
                  variant="secondary"
                  onClick={() => void refreshMarketplace(marketplace.id)}
                >
                  Refresh
                </Button>
                {marketplaceRemovable(marketplace) && (
                  <Button
                    variant="ghost"
                    title="Remove"
                    onClick={() => remove(marketplace)}
                  >
                    <Icon name="trash" className="h-4 w-4" />
                  </Button>
                )}
                <label
                  className="ml-1 cursor-pointer"
                  title={marketplace.auto_refresh ? "Auto-refresh on" : "Auto-refresh off"}
                >
                  <input
                    type="checkbox"
                    className="peer sr-only"
                    checked={marketplace.auto_refresh}
                    aria-label={`Auto-refresh ${marketplace.name}`}
                    onChange={() =>
                      void setMarketplaceAutoRefresh(
                        marketplace.id,
                        !marketplace.auto_refresh,
                      )
                    }
                  />
                  <span className={switchClass} />
                </label>
              </div>
            </div>
          </div>
        ))
      )}

      <Modal
        open={confirmRemove !== null}
        onClose={() => setConfirmRemove(null)}
        title="Remove this marketplace?"
        subtitle={confirmRemove?.name}
      >
        <div className="space-y-3">
          <p className="text-sm text-slate-600 dark:text-slate-300">
            {dependents.length === 1
              ? "1 installed plugin came"
              : `${dependents.length} installed plugins came`}{" "}
            from this marketplace. They keep working, but they become orphaned
            and update from their own recorded source.
          </p>
          <ul className="space-y-1 text-xs text-slate-500 dark:text-slate-400">
            {dependents.map((plugin) => (
              <li key={plugin.id}>{plugin.name}</li>
            ))}
          </ul>
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={() => setConfirmRemove(null)}>
              Cancel
            </Button>
            <Button
              variant="danger"
              onClick={() => {
                const marketplace = confirmRemove;
                setConfirmRemove(null);
                if (marketplace) void removeMarketplace(marketplace.id, true);
              }}
            >
              Remove marketplace
            </Button>
          </div>
        </div>
      </Modal>

      <AddMarketplaceModal open={adding} onClose={() => setAdding(false)} />
    </div>
  );
}

function refreshedLabel(value: string | null): string {
  if (!value) return "Never refreshed";
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? `Refreshed ${value}`
    : `Refreshed ${date.toLocaleString()}`;
}

const chipClass =
  "rounded-full bg-slate-100 px-2 py-0.5 text-[11px] text-slate-500 dark:bg-slate-800 dark:text-slate-400";

const switchClass =
  "relative block h-5.5 w-10 rounded-full bg-slate-300 transition peer-checked:bg-sky-500 dark:bg-slate-700 after:absolute after:left-0.5 after:top-0.5 after:h-4.5 after:w-4.5 after:rounded-full after:bg-white after:transition peer-checked:after:translate-x-4";
