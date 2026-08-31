import { useStore } from "../../store";
import { Button } from "./Modal";
import { Icon } from "../icons";

/** Ask before a tool runs (MCP trust & safety: human in the loop). */
export function ApprovalModal() {
  const queue = useStore((s) => s.approvals);
  const respond = useStore((s) => s.respondApproval);
  const current = queue[0];
  if (!current) return null;

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center p-6">
      <div className="fade-in absolute inset-0 bg-slate-950/50 backdrop-blur-[2px]" />
      <div className="pop-in relative w-full max-w-lg overflow-hidden rounded-2xl border border-slate-200 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-900">
        <div className="flex items-center gap-2.5 border-b border-slate-100 px-5 py-4 dark:border-slate-800">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-amber-100 text-amber-600 dark:bg-amber-900/50 dark:text-amber-300">
            <Icon name="shield" className="h-4.5 w-4.5" />
          </span>
          <div className="min-w-0">
            <h2 className="text-base font-semibold">Allow this tool?</h2>
            <p className="truncate text-xs text-slate-500 dark:text-slate-400">
              “{current.server_title}” wants to run{" "}
              <span className="font-mono font-medium">{current.tool}</span>
            </p>
          </div>
        </div>
        <div className="px-5 py-4">
          {current.read_only_hint && (
            <p className="mb-3 inline-flex items-center gap-1.5 rounded-md bg-emerald-50 px-2 py-1 text-xs font-medium text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-300">
              <Icon name="check" className="h-3.5 w-3.5" />
              The server says this is read-only (treat as a hint, not a guarantee)
            </p>
          )}
          <div className="mb-1 text-xs font-semibold uppercase tracking-wide text-slate-400">
            Arguments
          </div>
          <pre className="max-h-48 overflow-auto rounded-lg bg-slate-100 p-3 text-xs leading-relaxed dark:bg-slate-800">
            {JSON.stringify(current.args ?? {}, null, 2)}
          </pre>
        </div>
        <div className="flex flex-col gap-2 border-t border-slate-100 px-5 py-3.5 dark:border-slate-800">
          <div className="flex justify-end gap-2">
            <Button variant="danger" onClick={() => respond(current.request_id, "deny")}>
              Deny
            </Button>
            <Button variant="secondary" onClick={() => respond(current.request_id, "always_allow")}>
              Always allow this tool
            </Button>
            <Button variant="primary" onClick={() => respond(current.request_id, "allow_once")}>
              Allow once
            </Button>
          </div>
          <p className="text-right text-xs text-slate-400">
            Tools can access data and take actions. Only allow what you understand.
          </p>
        </div>
      </div>
    </div>
  );
}
