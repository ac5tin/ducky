import { useState } from "react";
import { useStore } from "../../store";
import { MODE_META, MODE_ORDER, resolveShownMode } from "../../modes";
import type { AgentMode } from "../../types";
import { Icon } from "../icons";

/** The agent mode control: default, read-only, plan or auto. */
export function ModePicker({ dropUp = false }: { dropUp?: boolean }) {
  const config = useStore((s) => s.config);
  const activeId = useStore((s) => s.activeConversationId);
  const draftMode = useStore((s) => s.draftMode);
  const setMode = useStore((s) => s.setMode);
  const toast = useStore((s) => s.toast);
  const [open, setOpen] = useState(false);

  const conversation = config?.conversations.find((c) => c.id === activeId);
  const mode = resolveShownMode(conversation, draftMode, config);
  const meta = MODE_META[mode];

  const choose = (next: AgentMode) => {
    setOpen(false);
    setMode(next).catch((e) => toast("error", `${e}`));
  };

  return (
    <div className="relative">
      <button
        className="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm font-medium transition hover:bg-slate-100 dark:hover:bg-slate-800"
        onClick={() => setOpen(!open)}
        title={meta.description}
      >
        <Icon name={meta.icon} className="h-3.5 w-3.5 text-slate-400" />
        <span>{meta.label}</span>
        <Icon
          name="chevron"
          className={`h-3.5 w-3.5 text-slate-400 transition ${open ? "rotate-180" : ""}`}
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
              Agent mode · Shift+Tab cycles
            </p>
            {MODE_ORDER.map((m) => (
              <button
                key={m}
                className={`flex w-full items-start gap-2 px-3 py-2 text-left transition hover:bg-slate-50 dark:hover:bg-slate-800 ${
                  m === mode ? "font-semibold text-sky-600 dark:text-sky-400" : ""
                }`}
                onClick={() => choose(m)}
              >
                <Icon name={MODE_META[m].icon} className="mt-0.5 h-4 w-4 shrink-0 opacity-70" />
                <span className="min-w-0 flex-1">
                  <span className="block text-sm">{MODE_META[m].label}</span>
                  <span className="block text-xs font-normal text-slate-400">
                    {MODE_META[m].description}
                  </span>
                </span>
                {m === mode && <Icon name="check" className="mt-0.5 h-4 w-4 shrink-0" />}
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}