import { useEffect, useRef, useState } from "react";
import { advisorChipState, type AdvisorChipState } from "../../advisorChip";
import { useStore } from "../../store";
import type { EffortLevel } from "../../types";
import { Icon } from "../icons";
import { EFFORT_LABELS, EffortPill, keptEffortForModel, useEffortLevelLoad } from "./effortLevels";

type Selection = {
  enabled: boolean;
  provider_id: string | null;
  model: string | null;
  effort: EffortLevel | null;
};

const TONE: Record<AdvisorChipState["mode"], string> = {
  off: "text-slate-400 hover:bg-slate-100 dark:hover:bg-slate-800",
  on: "text-sky-600 dark:text-sky-400 hover:bg-sky-50 dark:hover:bg-sky-950/60",
  unresolved:
    "text-amber-600 dark:text-amber-400 hover:bg-amber-50 dark:hover:bg-amber-950/40",
};

const selectClass =
  "w-full rounded-lg border border-slate-200 bg-white px-2 py-1.5 text-sm outline-none transition focus:border-sky-400 focus:ring-2 focus:ring-sky-100 dark:border-slate-700 dark:bg-slate-800 dark:focus:border-sky-500 dark:focus:ring-sky-900/40";

/**
 * The advisor control: whether this chat consults one, and which model and
 * effort it uses. The pick lives on the conversation, so every change writes
 * the whole provider/model/effort triple in one command.
 */
export function AdvisorChip({ dropUp = false }: { dropUp?: boolean }) {
  const config = useStore((s) => s.config);
  const activeId = useStore((s) => s.activeConversationId);
  const setActiveAdvisor = useStore((s) => s.setActiveAdvisor);
  const toast = useStore((s) => s.toast);
  const [open, setOpen] = useState(false);
  const modelPick = useRef(0);

  const providers = config?.providers ?? [];
  const conversation = config?.conversations.find((c) => c.id === activeId) ?? null;
  const state = config ? advisorChipState(conversation, config.settings, providers) : null;
  const provider = providers.find((p) => p.id === state?.provider_id);
  // what the label shows when no explicit model is stored; the provider name
  // is a last-resort label only
  const labelModel =
    state?.model ||
    provider?.default_model ||
    provider?.models[0] ||
    provider?.name ||
    "";
  // the effort pills need a real model id — a provider name is not one
  const effortModel =
    state?.model || provider?.default_model || provider?.models[0] || "";
  const { levels: efforts, status: effortStatus } = useEffortLevelLoad(
    provider?.kind,
    effortModel || undefined,
  );
  // a stored effort the loaded list does not contain has no selected pill,
  // and an empty list hides the row — show Default so the user can clear it.
  // loading and error also yield [], so do not treat those as a real empty list
  const effortOutsideList =
    effortStatus === "ready" &&
    !!state?.effort &&
    !efforts.includes(state.effort);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  if (!state) return null;

  const enabled = state.mode !== "off";
  const disabled = !activeId;
  const overriding = !!conversation && conversation.advisor_provider_id !== null;
  const current: Selection = {
    enabled,
    provider_id: state.provider_id,
    model: state.model || null,
    effort: state.effort,
  };
  const save = (next: Selection) => {
    setActiveAdvisor(next).catch((e) => toast("error", `${e}`));
  };

  let label: string;
  if (state.mode === "off") label = "Advisor off";
  else if (state.mode === "unresolved") label = "Advisor: pick a model";
  else if (state.effort) label = `Advisor: ${labelModel} · ${EFFORT_LABELS[state.effort]}`;
  else label = `Advisor: ${labelModel}`;

  return (
    <div className="relative">
      <button
        className={`flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm font-medium transition ${TONE[state.mode]}`}
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        title={label}
      >
        <Icon
          name={state.mode === "unresolved" ? "warning" : "chat"}
          className="h-3.5 w-3.5"
        />
        <span>{label}</span>
        <Icon
          name="chevron"
          className={`h-3.5 w-3.5 transition ${open ? "rotate-180" : ""}`}
        />
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-30" onClick={() => setOpen(false)} />
          <div
            className={`pop-in absolute left-0 z-40 w-80 rounded-xl border border-slate-200 bg-white py-1 shadow-xl dark:border-slate-700 dark:bg-slate-900 ${
              dropUp ? "bottom-full mb-1" : "top-full mt-1"
            }`}
          >
            <p className="px-3 py-1.5 text-[10px] uppercase tracking-wide text-slate-400">
              Advisor
            </p>
            <label className="flex items-center gap-2 px-3 py-2 text-sm">
              <input
                type="checkbox"
                checked={enabled}
                disabled={disabled}
                onChange={(e) => save({ ...current, enabled: e.target.checked })}
              />
              Consult an advisor in this chat
            </label>
            <div className="space-y-2 border-t border-slate-200 px-3 py-2 dark:border-slate-700">
              <label className="block">
                <span className="mb-1 block text-xs font-medium text-slate-400">
                  Provider
                </span>
                <select
                  className={selectClass}
                  value={provider?.id ?? ""}
                  disabled={disabled}
                  onChange={(e) =>
                    save({
                      ...current,
                      provider_id: e.target.value || null,
                      model: null,
                      effort: null,
                    })
                  }
                >
                  <option value="">Pick a provider</option>
                  {providers.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </label>
              <label className="block">
                <span className="mb-1 block text-xs font-medium text-slate-400">
                  Model
                </span>
                <select
                  className={selectClass}
                  value={state.model}
                  disabled={disabled || !provider}
                  onChange={(e) => {
                    const model = e.target.value || null;
                    const resolved =
                      model || provider?.default_model || provider?.models[0] || "";
                    const gen = ++modelPick.current;
                    const kept = current.effort;
                    void keptEffortForModel(provider?.kind, resolved, kept)
                      .then((effort) => {
                        if (gen !== modelPick.current) return;
                        save({ ...current, model, effort });
                      })
                      .catch(() => {
                        if (gen !== modelPick.current) return;
                        save({ ...current, model, effort: kept });
                      });
                  }}
                >
                  <option value="">
                    Provider default
                    {provider?.default_model ? ` — ${provider.default_model}` : ""}
                  </option>
                  {(provider?.models ?? []).map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
              </label>
              {(efforts.length > 0 || effortOutsideList) && (
                <div>
                  <p className="mb-1 text-xs font-medium text-slate-400">
                    Reasoning effort
                  </p>
                  <div className="flex flex-wrap gap-1">
                    <EffortPill
                      label="Default"
                      selected={state.effort === null}
                      disabled={disabled}
                      onClick={() => save({ ...current, effort: null })}
                    />
                    {efforts.map((level) => (
                      <EffortPill
                        key={level}
                        label={EFFORT_LABELS[level]}
                        selected={state.effort === level}
                        disabled={disabled}
                        onClick={() => save({ ...current, effort: level })}
                      />
                    ))}
                  </div>
                </div>
              )}
            </div>
            {overriding && !disabled && (
              <button
                className="w-full border-t border-slate-200 px-3 py-2 text-left text-xs font-medium text-sky-600 transition hover:bg-slate-50 dark:border-slate-700 dark:text-sky-400 dark:hover:bg-slate-800"
                onClick={() =>
                  save({
                    enabled: current.enabled,
                    provider_id: null,
                    model: null,
                    effort: null,
                  })
                }
              >
                Reset to default
              </button>
            )}
            <p className="px-3 pb-1 pt-1.5 text-xs text-slate-400">
              {disabled
                ? "Start the chat first."
                : "Defaults for new chats live in Settings → Advisor."}
            </p>
          </div>
        </>
      )}
    </div>
  );
}
