import { useEffect, useRef } from "react";
import { Icon } from "../icons";

/** Autocomplete list shown while the user types an `@` file reference in the
 * composer. Pure presentation — the Composer owns filtering and keyboard
 * state; this renders the matches and reports picks. */
export function FileReferenceMenu({
  paths,
  activeIndex,
  onPick,
  onDismiss,
}: {
  paths: string[];
  activeIndex: number;
  onPick: (path: string) => void;
  onDismiss: () => void;
}) {
  const activeRow = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    activeRow.current?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  return (
    <>
      {/* click-away; keeping mousedown default so the textarea keeps focus */}
      <div
        className="fixed inset-0 z-30 cursor-default"
        aria-label="Close file list"
        onMouseDown={(e) => e.preventDefault()}
        onClick={onDismiss}
      />
      <div className="pop-in absolute bottom-full left-0 z-40 mb-1 max-h-80 w-80 overflow-y-auto rounded-xl border border-slate-200 bg-white py-1 shadow-xl dark:border-slate-700 dark:bg-slate-900">
        {paths.map((path, i) => (
          <button
            key={path}
            ref={i === activeIndex ? activeRow : undefined}
            type="button"
            className={`flex w-full items-center gap-2 px-3 py-2 text-left transition ${
              i === activeIndex ? "bg-slate-50 dark:bg-slate-800" : ""
            } hover:bg-slate-50 dark:hover:bg-slate-800`}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => onPick(path)}
          >
            <Icon
              name={path.endsWith("/") ? "folder" : "file"}
              className="h-3.5 w-3.5 shrink-0 text-slate-400 dark:text-slate-500"
            />
            <span className="truncate font-mono text-xs text-slate-700 dark:text-slate-200">
              {path}
            </span>
          </button>
        ))}
      </div>
    </>
  );
}
