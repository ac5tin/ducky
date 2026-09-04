import { useEffect, useState } from "react";
import * as api from "../../api";
import type { EffortLevel } from "../../types";

export const EFFORT_LABELS: Record<EffortLevel, string> = {
  none: "None",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "XHigh",
  max: "Max",
};

// levels per provider kind + model, so re-opening the picker is instant
const effortCache = new Map<string, EffortLevel[]>();

/** Effort levels the model supports per models.dev; empty = hide the selector. */
export function useEffortLevels(kind: string | undefined, model: string | undefined): EffortLevel[] {
  const [levels, setLevels] = useState<EffortLevel[]>([]);

  useEffect(() => {
    if (!kind || !model) {
      setLevels([]);
      return;
    }
    const key = `${kind}/${model}`;
    const cached = effortCache.get(key);
    if (cached) {
      setLevels(cached);
      return;
    }
    let cancelled = false;
    api
      .effortLevels(kind, model)
      .then((levels) => {
        effortCache.set(key, levels);
        if (!cancelled) setLevels(levels);
      })
      .catch(() => {
        if (!cancelled) setLevels([]);
      });
    return () => {
      cancelled = true;
    };
  }, [kind, model]);

  return levels;
}

export function EffortPill({
  label,
  selected,
  disabled,
  onClick,
}: {
  label: string;
  selected: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      disabled={disabled}
      className={`rounded-md px-2 py-1 text-xs transition disabled:opacity-40 ${
        selected
          ? "bg-sky-50 font-medium text-sky-600 dark:bg-slate-800 dark:text-sky-400"
          : "text-slate-600 hover:bg-slate-100 dark:text-slate-300 dark:hover:bg-slate-800"
      }`}
      onClick={onClick}
    >
      {label}
    </button>
  );
}
