import { useState } from "react";
import { useStore } from "../../store";
import { Button } from "./Modal";
import { Markdown } from "../Markdown";
import { Icon } from "../icons";

/** Review a plan presented in plan mode (ADR-0004-adjacent: plan approval). */
export function PlanModal() {
  const queue = useStore((s) => s.plans);
  const respond = useStore((s) => s.respondPlan);
  const current = queue[0];
  const [feedback, setFeedback] = useState("");
  if (!current) return null;

  const keepPlanning = () => {
    const text = feedback.trim();
    setFeedback("");
    respond(current.request_id, "revise", text || undefined).catch(() => {});
  };

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center p-6">
      <div className="fade-in absolute inset-0 bg-slate-950/50 backdrop-blur-[2px]" />
      <div className="pop-in relative flex max-h-[88vh] w-full max-w-3xl flex-col overflow-hidden rounded-2xl border border-slate-200 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-900">
        <div className="flex items-center gap-2.5 border-b border-slate-100 px-5 py-4 dark:border-slate-800">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-sky-100 text-sky-600 dark:bg-sky-900/50 dark:text-sky-300">
            <Icon name="file" className="h-4.5 w-4.5" />
          </span>
          <div className="min-w-0">
            <h2 className="text-base font-semibold">Review this implementation plan</h2>
            <p className="text-xs text-slate-500 dark:text-slate-400">
              Approving it ends plan mode and lets the agent start implementing.
            </p>
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4 text-sm">
          <Markdown text={current.plan} />
        </div>
        <div className="flex flex-col gap-2 border-t border-slate-100 px-5 py-3.5 dark:border-slate-800">
          <textarea
            value={feedback}
            onChange={(e) => setFeedback(e.target.value)}
            rows={2}
            placeholder="Tell the agent what to change (optional) — it revises and asks again"
            className="w-full resize-none rounded-lg border border-slate-200 bg-white px-3 py-2 text-sm outline-none transition focus:border-sky-400 focus:ring-2 focus:ring-sky-100 dark:border-slate-700 dark:bg-slate-800 dark:focus:border-sky-500 dark:focus:ring-sky-900/40"
          />
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={keepPlanning}>
              Keep planning
            </Button>
            <Button
              variant="primary"
              onClick={() => {
                setFeedback("");
                respond(current.request_id, "approve").catch(() => {});
              }}
            >
              Approve plan
            </Button>
          </div>
          <p className="text-right text-xs text-slate-400">
            You can also switch the mode yourself at any time to leave plan mode.
          </p>
        </div>
      </div>
    </div>
  );
}
