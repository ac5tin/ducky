import { useEffect, useState } from "react";
import * as api from "../../api";
import { useStore } from "../../store";
import type { EffortLevel } from "../../types";
import { Icon } from "../icons";

const EFFORT_LABELS: Record<EffortLevel, string> = {
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

export function ModelPicker() {
  const config = useStore((s) => s.config);
  const activeId = useStore((s) => s.activeConversationId);
  const activeProvider = useStore((s) => s.activeProvider());
  const setActiveModel = useStore((s) => s.setActiveModel);
  const setActiveEffort = useStore((s) => s.setActiveEffort);
  const setView = useStore((s) => s.setView);
  const [open, setOpen] = useState(false);
  const [efforts, setEfforts] = useState<EffortLevel[]>([]);

  const conversation = config?.conversations.find((c) => c.id === activeId);
  const currentModel = conversation?.model || activeProvider?.default_model || activeProvider?.models[0] || "";
  const effort = conversation?.effort ?? null;

  useEffect(() => {
    if (!activeProvider || !currentModel) return;
    const key = `${activeProvider.kind}/${currentModel}`;
    const cached = effortCache.get(key);
    if (cached) {
      setEfforts(cached);
      return;
    }
    let cancelled = false;
    api
      .effortLevels(activeProvider.kind, currentModel)
      .then((levels) => {
        effortCache.set(key, levels);
        if (!cancelled) setEfforts(levels);
      })
      .catch(() => {
        if (!cancelled) setEfforts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [activeProvider?.kind, activeProvider?.id, currentModel]);

  if (!activeProvider) {
    return (
      <button
        className="text-sm text-slate-400 underline-offset-2 hover:underline"
        onClick={() => setView("settings")}
      >
        No AI provider configured — set one up →
      </button>
    );
  }

  return (
    <div className="relative">
      <button
        className="flex items-center gap-2 rounded-lg px-2.5 py-1.5 text-sm font-medium transition hover:bg-slate-100 dark:hover:bg-slate-800"
        onClick={() => setOpen(!open)}
      >
        <span className="text-slate-400">{activeProvider.name}</span>
        <span className="max-w-56 truncate">{currentModel || "Choose a model"}</span>
        {effort && <span className="text-xs text-slate-400">· {EFFORT_LABELS[effort]}</span>}
        <Icon name="chevron" className={`h-3.5 w-3.5 text-slate-400 transition ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div className="pop-in absolute left-0 top-full z-40 mt-1 max-h-80 w-72 overflow-y-auto rounded-xl border border-slate-200 bg-white py-1 shadow-xl dark:border-slate-700 dark:bg-slate-900">
            {activeProvider.models.length === 0 && (
              <p className="px-3 py-2 text-xs text-slate-400">
                No models fetched yet — refresh them in Settings → Providers.
              </p>
            )}
            {activeProvider.models.map((model) => (
              <button
                key={model}
                className={`flex w-full items-center justify-between px-3 py-2 text-left text-sm transition hover:bg-slate-50 dark:hover:bg-slate-800 ${
                  model === currentModel ? "font-semibold text-sky-600 dark:text-sky-400" : ""
                }`}
                onClick={() => {
                  setOpen(false);
                  if (activeProvider) setActiveModel(activeProvider.id, model).catch((e) => console.error(e));
                }}
              >
                <span className="truncate">{model}</span>
                {model === currentModel && <Icon name="check" className="h-4 w-4 shrink-0" />}
              </button>
            ))}
            {efforts.length > 0 && (
              <div className="mt-1 border-t border-slate-200 px-3 pb-2 pt-2 dark:border-slate-700">
                <p className="pb-1.5 text-xs font-medium text-slate-400">Reasoning effort</p>
                <div className="flex flex-wrap gap-1">
                  <EffortPill selected={effort === null} label="Default" onClick={() => setActiveEffort(null)} />
                  {efforts.map((level) => (
                    <EffortPill
                      key={level}
                      selected={effort === level}
                      label={EFFORT_LABELS[level]}
                      onClick={() => setActiveEffort(level)}
                    />
                  ))}
                </div>
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}

function EffortPill({ label, selected, onClick }: { label: string; selected: boolean; onClick: () => void }) {
  return (
    <button
      className={`rounded-md px-2 py-1 text-xs transition ${
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
