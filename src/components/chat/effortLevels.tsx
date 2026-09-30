import { useEffect, useState } from "react";
import { supportedEffort } from "../../advisorChip";
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

export type EffortLevelLoad = "loading" | "ready" | "error";

/**
 * Effort levels for one model. `error` and a missing model both yield `[]`,
 * so callers must read `status` before treating `[]` as "this model has no
 * effort control".
 */
export function useEffortLevelLoad(
  kind: string | undefined,
  model: string | undefined,
): { levels: EffortLevel[]; status: EffortLevelLoad } {
  const [levels, setLevels] = useState<EffortLevel[]>([]);
  const [status, setStatus] = useState<EffortLevelLoad>("loading");

  useEffect(() => {
    if (!kind || !model) {
      setLevels([]);
      setStatus("ready");
      return;
    }
    const key = `${kind}/${model}`;
    const cached = effortCache.get(key);
    if (cached) {
      setLevels(cached);
      setStatus("ready");
      return;
    }
    setStatus("loading");
    let cancelled = false;
    api
      .effortLevels(kind, model)
      .then((next) => {
        effortCache.set(key, next);
        if (!cancelled) {
          setLevels(next);
          setStatus("ready");
        }
      })
      .catch(() => {
        if (!cancelled) {
          setLevels([]);
          setStatus("error");
        }
      });
    return () => {
      cancelled = true;
    };
  }, [kind, model]);

  return { levels, status };
}

/** Effort levels the model supports per models.dev; empty = hide the selector. */
export function useEffortLevels(kind: string | undefined, model: string | undefined): EffortLevel[] {
  return useEffortLevelLoad(kind, model).levels;
}

/**
 * Effort to store after a model change. A failed lookup keeps `effort`:
 * an empty list from a thrown request is not "this model rejects it".
 */
export async function keptEffortForModel(
  kind: string | undefined,
  model: string,
  effort: EffortLevel | null,
): Promise<EffortLevel | null> {
  if (effort === null || !kind) return effort;
  const levels = await api.effortLevels(kind, model);
  return supportedEffort(effort, levels);
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
