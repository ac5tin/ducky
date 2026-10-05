import { useState } from "react";
import { useStore } from "../../store";
import type { CatalogEntry } from "../../types";
import { catalogEntryAction, formatVersion } from "../../plugins";
import { Button } from "../modals/Modal";
import { Icon } from "../icons";

/**
 * One catalog entry. The card opens the detail sheet; the single action
 * button installs, updates or reports the installed state.
 */
export function PluginCard({
  entry,
  onOpen,
}: {
  entry: CatalogEntry;
  onOpen: () => void;
}) {
  const installPlugin = useStore((s) => s.installPlugin);
  const updatePlugin = useStore((s) => s.updatePlugin);
  const catalogLoading = useStore((s) => s.catalogLoading);
  const [pending, setPending] = useState(false);
  const action = catalogEntryAction(entry, pending || catalogLoading);
  const name = entry.display_name || entry.name;

  const run = async () => {
    if (action.kind !== "install" && action.kind !== "update") return;
    setPending(true);
    try {
      if (action.kind === "update" && entry.installed) {
        await updatePlugin(entry.installed);
      } else {
        await installPlugin({
          marketplace: entry.marketplace_id,
          name: entry.name,
        });
      }
    } finally {
      setPending(false);
    }
  };

  return (
    <div
      onClick={onOpen}
      className="flex cursor-pointer flex-col rounded-2xl border border-slate-200 p-4 text-left transition hover:border-sky-300 hover:shadow-sm dark:border-slate-700 dark:hover:border-sky-700"
    >
      <div className="flex items-start gap-3">
        <div className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-slate-100 dark:bg-slate-800">
          <Icon name="puzzle" className="h-5 w-5 text-slate-500 dark:text-slate-400" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            {/* the card's click is a mouse convenience; this is the control */}
            <button
              type="button"
              className="truncate text-left font-semibold"
              onClick={(e) => {
                e.stopPropagation();
                onOpen();
              }}
            >
              {name}
            </button>
            <span className="shrink-0 text-[11px] text-slate-400">
              {formatVersion(entry.version)}
            </span>
          </div>
          {entry.author && (
            <div className="truncate text-[11px] text-slate-400">by {entry.author}</div>
          )}
          <p className="mt-1 line-clamp-2 text-xs leading-relaxed text-slate-500 dark:text-slate-400">
            {entry.description || "No description."}
          </p>
        </div>
      </div>

      <div className="mt-3 flex flex-wrap gap-1">
        {entry.category && <span className={chipClass}>{entry.category}</span>}
        {entry.tags.slice(0, 3).map((tag) => (
          <span key={tag} className={chipClass}>
            {tag}
          </span>
        ))}
      </div>

      <div className="mt-3 flex items-center justify-between gap-2">
        <span className="truncate text-[11px] text-slate-400">
          {entry.marketplace_id}
        </span>
        {/* the card owns the click that opens the sheet; the action must not
            bubble into it */}
        <span role="presentation" onClick={(e) => e.stopPropagation()}>
          <Button
            variant={action.kind === "update" ? "primary" : "secondary"}
            disabled={action.disabled}
            title={action.hint ?? undefined}
            onClick={() => void run()}
          >
            {action.label}
          </Button>
        </span>
      </div>
    </div>
  );
}

const chipClass =
  "rounded-full bg-slate-100 px-2 py-0.5 text-[11px] text-slate-500 dark:bg-slate-800 dark:text-slate-400";
