import { useState } from "react";
import type { ToolCallState, ContentBlockValue } from "../../types";
import { Icon, StatusDot } from "../icons";
import { Markdown } from "../Markdown";

const statusLabels: Record<ToolCallState["status"], string> = {
  pending_approval: "Waiting for your approval",
  running: "Running…",
  awaiting_input: "Waiting for your input",
  done: "Done",
  denied: "Denied",
  error: "Failed",
};

export function ToolCallCard({ state }: { state: ToolCallState }) {
  const [open, setOpen] = useState(state.status !== "done");
  const status = state.status;
  const icon =
    status === "done"
      ? "check"
      : status === "running" || status === "awaiting_input"
        ? "spinner"
        : status === "pending_approval"
          ? "shield"
          : status === "denied"
            ? "x"
            : "warning";
  const tone =
    status === "done"
      ? "text-emerald-600 dark:text-emerald-400"
      : status === "error" || status === "denied"
        ? "text-rose-600 dark:text-rose-400"
        : status === "pending_approval"
          ? "text-amber-600 dark:text-amber-400"
          : "text-sky-600 dark:text-sky-400";

  return (
    <div className="overflow-hidden rounded-xl border border-slate-200 bg-white text-sm shadow-sm dark:border-slate-800 dark:bg-slate-900">
      <button
        className="flex w-full items-center gap-2.5 px-3.5 py-2.5 text-left"
        onClick={() => setOpen(!open)}
      >
        <Icon
          name={icon}
          className={`h-4 w-4 shrink-0 ${tone} ${status === "running" || status === "awaiting_input" ? "animate-spin" : ""}`}
        />
        <span className="min-w-0 flex-1 truncate font-mono text-[13px] font-medium">
          {state.tool ?? "tool"}
        </span>
        {state.server_title && (
          <span className="hidden items-center gap-1.5 rounded-md bg-slate-100 px-2 py-0.5 text-xs text-slate-500 sm:inline-flex dark:bg-slate-800 dark:text-slate-400">
            <StatusDot status="connected" />
            {state.server_title}
          </span>
        )}
        <span className={`shrink-0 text-xs font-medium ${tone}`}>{statusLabels[status]}</span>
        <Icon
          name="chevron"
          className={`h-3.5 w-3.5 shrink-0 text-slate-400 transition ${open ? "rotate-180" : ""}`}
        />
      </button>

      {open && (
        <div className="border-t border-slate-100 px-3.5 py-3 dark:border-slate-800">
          {state.args !== undefined && (
            <>
              <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400">
                Arguments
              </div>
              <pre className="mb-3 max-h-40 overflow-auto rounded-lg bg-slate-50 p-2.5 text-xs dark:bg-slate-800/80">
                {JSON.stringify(state.args, null, 2)}
              </pre>
            </>
          )}
          {state.content && state.content.length > 0 && (
            <RichContent blocks={state.content} />
          )}
          {state.result_text && (
            <>
              <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400">
                Result
              </div>
              <div
                className={`rounded-lg px-3 py-2 ${
                  state.is_error
                    ? "bg-rose-50 text-rose-800 dark:bg-rose-950/40 dark:text-rose-200"
                    : "bg-slate-50 dark:bg-slate-800/80"
                }`}
              >
                <Markdown text={state.result_text} />
              </div>
            </>
          )}
          {state.structured !== undefined && state.structured !== null && (
            <>
              <div className="mb-1 mt-3 text-[11px] font-semibold uppercase tracking-wide text-slate-400">
                Structured output
              </div>
              <pre className="max-h-56 overflow-auto rounded-lg bg-slate-50 p-2.5 text-xs dark:bg-slate-800/80">
                {JSON.stringify(state.structured, null, 2)}
              </pre>
            </>
          )}
          {!state.result_text && !state.content && status === "done" && (
            <p className="text-xs text-slate-400">No output.</p>
          )}
        </div>
      )}
    </div>
  );
}

function RichContent({ blocks }: { blocks: ContentBlockValue[] }) {
  return (
    <div className="mb-3 space-y-2">
      {blocks.map((block, i) => {
        if (block.type === "text" && typeof block.text === "string") {
          return <Markdown key={i} text={block.text} />;
        }
        if (block.type === "image") {
          const mime = (block as any).mime_type ?? (block as any).mimeType ?? "image/png";
          return (
            <img
              key={i}
              src={`data:${mime};base64,${block.data}`}
              alt="Tool result"
              className="max-h-72 rounded-lg border border-slate-200 dark:border-slate-700"
            />
          );
        }
        if (block.type === "resource_link") {
          return (
            <div key={i} className="rounded-lg bg-slate-50 px-3 py-2 text-xs dark:bg-slate-800/80">
              🔗 {(block as any).name ?? ""} — <span className="font-mono">{(block as any).uri}</span>
            </div>
          );
        }
        if (block.type === "resource" && (block as any).resource) {
          const res = (block as any).resource;
          if (res.text !== undefined) {
            return (
              <pre key={i} className="max-h-56 overflow-auto rounded-lg bg-slate-50 p-2.5 text-xs dark:bg-slate-800/80">
                {res.text}
              </pre>
            );
          }
          return (
            <div key={i} className="rounded-lg bg-slate-50 px-3 py-2 text-xs dark:bg-slate-800/80">
              📎 {res.uri} (binary)
            </div>
          );
        }
        return (
          <pre key={i} className="max-h-40 overflow-auto rounded-lg bg-slate-50 p-2.5 text-xs dark:bg-slate-800/80">
            {JSON.stringify(block, null, 2)}
          </pre>
        );
      })}
    </div>
  );
}

