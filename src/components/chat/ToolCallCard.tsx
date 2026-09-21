import { memo, useState } from "react";
import type { ToolCallState, ContentBlockValue } from "../../types";
import { useStore } from "../../store";
import * as api from "../../api";
import { initialToolDetailsOpen } from "../../toolDetails";
import {
  SUBAGENT_TOOL,
  subagentHeader,
  subagentTask,
} from "../../subagents";
import { Icon, StatusDot } from "../icons";
import { Markdown } from "../Markdown";
import { Button } from "../modals/Modal";

const statusLabels: Record<ToolCallState["status"], string> = {
  pending_approval: "Waiting for your approval",
  running: "Running…",
  awaiting_input: "Waiting for your input",
  needs_auth: "Sign-in required",
  done: "Done",
  denied: "Denied",
  error: "Failed",
};

const taskStatusLabels: Record<string, string> = {
  working: "Running",
  input_required: "Waiting for your input",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
};

function authMessage(reason?: string | null): { line: string; button: string } {
  if (reason === "expired") {
    return {
      line: "Your session expired — sign in again.",
      button: "Re-authenticate",
    };
  }
  if (reason === "scope") {
    return {
      line: "The server needs additional permissions — sign in again to grant them.",
      button: "Re-authenticate",
    };
  }
  return { line: "Sign in to connect.", button: "Sign in" };
}

export const ToolCallCard = memo(function ToolCallCard({
  state,
}: {
  state: ToolCallState;
}) {
  const toolDetails = useStore(
    (s) => s.config?.settings.tool_details ?? "auto",
  );
  const [open, setOpen] = useState(() =>
    initialToolDetailsOpen(toolDetails, state.status),
  );
  const [authBusy, setAuthBusy] = useState(false);
  const server = useStore((s) =>
    s.servers.find((srv) => srv.id === state.server),
  );
  const refreshServer = useStore((s) => s.refreshServer);
  const toast = useStore((s) => s.toast);
  const status = state.status;
  const isSubagent = state.tool === SUBAGENT_TOOL;
  const header = isSubagent ? subagentHeader(state.args) : null;
  const taskText = isSubagent ? subagentTask(state.args) : "";
  const providerName = useStore((s) =>
    isSubagent
      ? s.config?.providers.find(
          (p) => p.id === state.subagent?.provider_id,
        )?.name
      : undefined,
  );
  const icon =
    status === "done"
      ? "check"
      : status === "running" || status === "awaiting_input"
        ? "spinner"
        : status === "pending_approval" || status === "needs_auth"
          ? "shield"
          : status === "denied"
            ? "x"
            : "warning";
  const tone =
    status === "done"
      ? "text-emerald-600 dark:text-emerald-400"
      : status === "error" || status === "denied"
        ? "text-rose-600 dark:text-rose-400"
        : status === "pending_approval" || status === "needs_auth"
          ? "text-amber-600 dark:text-amber-400"
          : "text-sky-600 dark:text-sky-400";

  const serverNeedsAuth =
    typeof server?.status === "object" && "needs_auth" in server.status;
  const serverAuthReason =
    typeof server?.status === "object" && "needs_auth" in server.status
      ? server.status.needs_auth.reason
      : null;
  const showAuthPanel =
    status === "needs_auth" || (status === "error" && serverNeedsAuth);
  const auth = authMessage(serverAuthReason);

  const signIn = async () => {
    if (!state.server) return;
    setAuthBusy(true);
    try {
      await api.mcpOauthLogin(state.server);
      await refreshServer(state.server).catch(() => {});
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setAuthBusy(false);
    }
  };

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
          {header ? (
            <>
              <span className="font-sans font-semibold">{header.name}</span>
              {header.brief && (
                <span className="font-sans font-normal text-slate-500 dark:text-slate-400">
                  {" "}
                  — {header.brief}
                </span>
              )}
            </>
          ) : (
            (state.tool ?? "tool")
          )}
        </span>
        {state.server_title && (
          <span className="hidden items-center gap-1.5 rounded-md bg-slate-100 px-2 py-0.5 text-xs text-slate-500 sm:inline-flex dark:bg-slate-800 dark:text-slate-400">
            <StatusDot status="connected" />
            {state.server_title}
          </span>
        )}
        <span className={`shrink-0 text-xs font-medium ${tone}`}>
          {statusLabels[status]}
        </span>
        <Icon
          name="chevron"
          className={`h-3.5 w-3.5 shrink-0 text-slate-400 transition ${open ? "rotate-180" : ""}`}
        />
      </button>

      {open && (
        <div className="border-t border-slate-100 px-3.5 py-3 dark:border-slate-800">
          {isSubagent && state.subagent && (
            <div className="mb-3 flex flex-wrap items-center gap-1.5 text-xs text-slate-500 dark:text-slate-400">
              {providerName && (
                <span className="rounded-md bg-slate-100 px-2 py-0.5 dark:bg-slate-800">
                  {providerName}
                </span>
              )}
              {state.subagent.model && (
                <span className="rounded-md bg-slate-100 px-2 py-0.5 font-mono dark:bg-slate-800">
                  {state.subagent.model}
                </span>
              )}
              <span className="rounded-md bg-slate-100 px-2 py-0.5 dark:bg-slate-800">
                effort: {state.subagent.effort ?? "default"}
              </span>
            </div>
          )}
          {isSubagent && state.subagent_activity && status === "running" && (
            <p className="mb-3 truncate font-mono text-[11px] text-slate-400">
              {state.subagent_activity}
            </p>
          )}
          {isSubagent && state.subagent_text && (
            <>
              <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400">
                Subagent transcript
              </div>
              <div
                className={`mb-3 max-h-72 overflow-auto rounded-lg bg-slate-50 px-3 py-2 dark:bg-slate-800/80 ${
                  status === "running" ? "caret" : ""
                }`}
              >
                <Markdown text={state.subagent_text} />
              </div>
            </>
          )}
          {showAuthPanel && (
            <div className="mb-3 rounded-xl border border-amber-200 bg-amber-50 p-3 dark:border-amber-900 dark:bg-amber-950/40">
              <p className="text-sm font-medium text-amber-900 dark:text-amber-200">
                Ducky needs you to sign in to{" "}
                {state.server_title ?? "this server"}.
              </p>
              <p className="mt-0.5 text-xs leading-relaxed text-amber-800 dark:text-amber-300">
                {auth.line}
              </p>
              <Button
                variant="primary"
                className="mt-2"
                disabled={authBusy || !state.server}
                onClick={signIn}
              >
                {authBusy ? "Waiting for sign-in…" : auth.button}
              </Button>
            </div>
          )}
          {state.task && (
            <div className="mb-3 flex items-center gap-2 rounded-lg bg-sky-50 px-3 py-2 text-xs text-sky-800 dark:bg-sky-950/40 dark:text-sky-200">
              <Icon
                name={
                  state.task.status === "working" ||
                  state.task.status === "input_required"
                    ? "spinner"
                    : state.task.status === "completed"
                      ? "check"
                      : "warning"
                }
                className={`h-3.5 w-3.5 shrink-0 ${
                  state.task.status === "working" ||
                  state.task.status === "input_required"
                    ? "animate-spin"
                    : ""
                }`}
              />
              <span>
                Task {taskStatusLabels[state.task.status] ?? state.task.status}
                {state.task.status_message
                  ? ` — ${state.task.status_message}`
                  : ""}
              </span>
            </div>
          )}
          {isSubagent && taskText && (
            <>
              <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-slate-400">
                Task
              </div>
              <p className="mb-3 whitespace-pre-wrap rounded-lg bg-slate-50 px-3 py-2 text-xs leading-relaxed text-slate-700 dark:bg-slate-800/80 dark:text-slate-300">
                {taskText}
              </p>
            </>
          )}
          {!isSubagent && state.args !== undefined && (
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
});

function RichContent({ blocks }: { blocks: ContentBlockValue[] }) {
  return (
    <div className="mb-3 space-y-2">
      {blocks.map((block, i) => {
        if (block.type === "text" && typeof block.text === "string") {
          return <Markdown key={i} text={block.text} />;
        }
        if (block.type === "image") {
          const mime =
            (block as any).mime_type ?? (block as any).mimeType ?? "image/png";
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
            <div
              key={i}
              className="rounded-lg bg-slate-50 px-3 py-2 text-xs dark:bg-slate-800/80"
            >
              🔗 {(block as any).name ?? ""} —{" "}
              <span className="font-mono">{(block as any).uri}</span>
            </div>
          );
        }
        if (block.type === "resource" && (block as any).resource) {
          const res = (block as any).resource;
          if (res.text !== undefined) {
            return (
              <pre
                key={i}
                className="max-h-56 overflow-auto rounded-lg bg-slate-50 p-2.5 text-xs dark:bg-slate-800/80"
              >
                {res.text}
              </pre>
            );
          }
          return (
            <div
              key={i}
              className="rounded-lg bg-slate-50 px-3 py-2 text-xs dark:bg-slate-800/80"
            >
              📎 {res.uri} (binary)
            </div>
          );
        }
        return (
          <pre
            key={i}
            className="max-h-40 overflow-auto rounded-lg bg-slate-50 p-2.5 text-xs dark:bg-slate-800/80"
          >
            {JSON.stringify(block, null, 2)}
          </pre>
        );
      })}
    </div>
  );
}
