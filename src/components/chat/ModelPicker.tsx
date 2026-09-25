import { useState } from "react";
import * as api from "../../api";
import { resolveShownEffort, resolveShownModel } from "../../chatDraft";
import { useStore } from "../../store";
import { Icon } from "../icons";
import { EFFORT_LABELS, EffortPill, useEffortLevels } from "./effortLevels";

export function ModelPicker({ dropUp = false }: { dropUp?: boolean }) {
  const config = useStore((s) => s.config);
  const activeId = useStore((s) => s.activeConversationId);
  const activeProvider = useStore((s) => s.activeProvider());
  const draftModel = useStore((s) => s.draftModel);
  const draftEffort = useStore((s) => s.draftEffort);
  const setActiveModel = useStore((s) => s.setActiveModel);
  const setActiveEffort = useStore((s) => s.setActiveEffort);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const setView = useStore((s) => s.setView);
  const stageDraftProvider = useStore((s) => s.stageDraftProvider);
  const [open, setOpen] = useState(false);
  // Which provider's models the open menu lists. Browsing alone never
  // retargets a chat; only picking a model does.
  const [browseId, setBrowseId] = useState<string | null>(null);

  const providers = config?.providers ?? [];
  const browse =
    providers.find((p) => p.id === browseId) ?? activeProvider;

  const conversation = config?.conversations.find((c) => c.id === activeId);
  const currentModel = resolveShownModel(
    conversation,
    activeId,
    draftModel,
    config,
    activeProvider,
  );
  const effort = resolveShownEffort(activeId, conversation, draftEffort, config);
  const efforts = useEffortLevels(activeProvider?.kind, currentModel);

  const isDefaultModel = (model: string) =>
    !!activeProvider &&
    config?.settings.default_provider_id === activeProvider.id &&
    config?.settings.default_model === model;
  const isDefault =
    !!currentModel && isDefaultModel(currentModel) && (config?.settings.default_effort ?? null) === effort;

  const toggleDefault = async () => {
    if (!activeProvider || !currentModel) return;
    if (isDefault) {
      await api.settingsSet({ default_provider_id: "", default_model: "", default_effort: null });
    } else {
      await api.settingsSet({
        default_provider_id: activeProvider.id,
        default_model: currentModel,
        default_effort: effort,
      });
    }
    await refreshConfig();
  };

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
        onClick={() => {
          setOpen(!open);
          setBrowseId(null);
        }}
      >
        <span className="text-slate-400">{activeProvider.name}</span>
        <span className="max-w-56 truncate">{currentModel || "Choose a model"}</span>
        {effort && <span className="text-xs text-slate-400">· {EFFORT_LABELS[effort]}</span>}
        <Icon name="chevron" className={`h-3.5 w-3.5 text-slate-400 transition ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div
            className={`pop-in absolute left-0 z-40 max-h-80 w-72 overflow-y-auto rounded-xl border border-slate-200 bg-white py-1 shadow-xl dark:border-slate-700 dark:bg-slate-900 ${
              dropUp ? "bottom-full mb-1" : "top-full mt-1"
            }`}
          >
            {providers.length > 1 && (
              <div className="border-b border-slate-200 pb-1 dark:border-slate-700">
                <p className="px-3 pb-1 pt-1.5 text-xs font-medium text-slate-400">Provider</p>
                {providers.map((p) => (
                  <button
                    key={p.id}
                    className={`flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-sm transition hover:bg-slate-50 dark:hover:bg-slate-800 ${
                      p.id === browse?.id ? "font-semibold text-sky-600 dark:text-sky-400" : ""
                    }`}
                    onClick={() => {
                      setBrowseId(p.id);
                      // on the draft page this retargets the chat send() will
                      // create; mid-chat it is inert until a model is picked
                      stageDraftProvider(p.id);
                    }}
                  >
                    <span className="truncate">{p.name}</span>
                    <span className="shrink-0 text-xs text-slate-400">
                      {p.models.length > 0 ? p.models.length : "no models"}
                    </span>
                  </button>
                ))}
              </div>
            )}
            {browse && browse.models.length === 0 && (
              <p className="px-3 py-2 text-xs text-slate-400">
                No models fetched yet — refresh them in Settings → Providers.
              </p>
            )}
            {(browse?.models ?? []).map((model) => (
              <button
                key={model}
                className={`flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm transition hover:bg-slate-50 dark:hover:bg-slate-800 ${
                  model === currentModel ? "font-semibold text-sky-600 dark:text-sky-400" : ""
                }`}
                onClick={() => {
                  setOpen(false);
                  setBrowseId(null);
                  if (browse) setActiveModel(browse.id, model).catch((e) => console.error(e));
                }}
              >
                <span className="truncate">{model}</span>
                {isDefaultModel(model) && (
                  <span className="shrink-0 text-[10px] uppercase tracking-wide text-slate-400">default</span>
                )}
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
            {currentModel && (
              <div className="mt-1 flex items-center justify-between border-t border-slate-200 px-3 py-2 dark:border-slate-700">
                <span className="text-xs text-slate-400">Default for new chats</span>
                <button
                  className={`text-xs font-medium transition hover:underline ${
                    isDefault ? "text-slate-400" : "text-sky-600 dark:text-sky-400"
                  }`}
                  onClick={() => toggleDefault().catch((e) => console.error(e))}
                >
                  {isDefault ? "Remove" : "Set as default"}
                </button>
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}
