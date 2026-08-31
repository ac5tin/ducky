import { useStore } from "../../store";
import { Button } from "./Modal";
import { Icon } from "../icons";

/** Server asks Ducky's model to generate a completion (MCP sampling). */
export function SamplingModal() {
  const queue = useStore((s) => s.samplings);
  const respond = useStore((s) => s.respondSampling);
  const current = queue[0];
  if (!current) return null;

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center p-6">
      <div className="fade-in absolute inset-0 bg-slate-950/50 backdrop-blur-[2px]" />
      <div className="pop-in relative w-full max-w-lg overflow-hidden rounded-2xl border border-slate-200 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-900">
        <div className="flex items-center gap-2.5 border-b border-slate-100 px-5 py-4 dark:border-slate-800">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-violet-100 text-violet-600 dark:bg-violet-900/50 dark:text-violet-300">
            <Icon name="chat" className="h-4.5 w-4.5" />
          </span>
          <div>
            <h2 className="text-base font-semibold">
              “{current.server_title}” wants to use your AI
            </h2>
            <p className="text-xs text-slate-500 dark:text-slate-400">
              MCP sampling request{current.max_tokens ? ` · up to ${current.max_tokens} tokens` : ""}
            </p>
          </div>
        </div>
        <div className="max-h-[55vh] overflow-y-auto px-5 py-4">
          <p className="mb-3 text-sm text-slate-600 dark:text-slate-300">
            The server asked your model to write something. Here's exactly what will be
            sent:
          </p>
          {current.system_prompt && (
            <div className="mb-3">
              <div className="mb-1 text-xs font-semibold uppercase tracking-wide text-slate-400">
                System prompt
              </div>
              <pre className="max-h-28 overflow-y-auto whitespace-pre-wrap rounded-lg bg-slate-100 p-2.5 text-xs dark:bg-slate-800">
                {current.system_prompt}
              </pre>
            </div>
          )}
          <div className="space-y-2">
            {current.messages.map((m, i) => {
              const text = typeof m?.content === "string"
                ? m.content
                : Array.isArray(m?.content)
                  ? m.content
                      .map((c: any) => (typeof c === "string" ? c : c?.text ?? JSON.stringify(c)))
                      .join("\n")
                  : JSON.stringify(m);
              return (
                <div key={i}>
                  <div className="mb-1 text-xs font-semibold uppercase tracking-wide text-slate-400">
                    {m?.role ?? "message"}
                  </div>
                  <pre className="max-h-32 overflow-y-auto whitespace-pre-wrap rounded-lg bg-slate-100 p-2.5 text-xs dark:bg-slate-800">
                    {text}
                  </pre>
                </div>
              );
            })}
          </div>
        </div>
        <div className="flex justify-end gap-2 border-t border-slate-100 px-5 py-3.5 dark:border-slate-800">
          <Button variant="secondary" onClick={() => respond(current.request_id, false)}>
            Deny
          </Button>
          <Button variant="primary" onClick={() => respond(current.request_id, true)}>
            Allow this request
          </Button>
        </div>
      </div>
    </div>
  );
}
