import { useState } from "react";
import { useStore } from "../../store";
import * as api from "../../api";
import type { ServerSummary } from "../../types";
import { Button } from "../modals/Modal";
import { Icon, StatusDot } from "../icons";
import { AddConnectorModal } from "./AddConnectorModal";
import { ServerDetailModal } from "./ServerDetailModal";

function authReason(
  s: ServerSummary["status"],
): "missing" | "expired" | "scope" | null {
  return typeof s === "object" && "needs_auth" in s
    ? (s.needs_auth.reason ?? "missing")
    : null;
}

function statusLabel(s: ServerSummary["status"]): {
  text: string;
  kind: string;
} {
  if (s === "connected") return { text: "Connected", kind: "connected" };
  if (s === "connecting") return { text: "Connecting…", kind: "connecting" };
  if (typeof s === "object" && "needs_auth" in s) {
    const text =
      s.needs_auth.reason === "expired"
        ? "Session expired"
        : s.needs_auth.reason === "scope"
          ? "Additional permission needed"
          : "Sign in required";
    return { text, kind: "needs_auth" };
  }
  if (typeof s === "object" && "error" in s)
    return { text: "Error", kind: "error" };
  return { text: "Disconnected", kind: "disconnected" };
}

export function ConnectorsView() {
  const servers = useStore((s) => s.servers);
  const refreshServers = useStore((s) => s.refreshServers);
  const toast = useStore((s) => s.toast);
  const [adding, setAdding] = useState(false);
  const [detailId, setDetailId] = useState<string | null>(null);

  const toggleEnabled = async (s: ServerSummary) => {
    try {
      await api.mcpSetEnabled(s.id, !s.enabled);
      if (s.enabled) await api.mcpDisconnect(s.id);
      await refreshServers();
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  const connect = async (s: ServerSummary) => {
    try {
      const result = await api.mcpConnect(s.id);
      await refreshServers();
      if (result === "error") {
        toast(
          "error",
          `${s.name} failed to connect — see its detail view for logs.`,
        );
      }
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  const signIn = async (s: ServerSummary) => {
    try {
      toast(
        "info",
        "Complete the sign-in in your browser — Ducky will reconnect.",
      );
      await api.mcpOauthLogin(s.id);
      await refreshServers();
      toast("success", `${s.name} is signed in and reconnected.`);
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-4xl px-6 py-8">
        <div className="flex items-start justify-between">
          <div>
            <h1 className="text-xl font-bold">Connectors</h1>
            <p className="mt-1 max-w-xl text-sm leading-relaxed text-slate-500 dark:text-slate-400">
              Connectors are MCP servers that give your AI tools, data and
              prompts. Everything runs with your approval.
            </p>
          </div>
          <Button onClick={() => setAdding(true)}>
            <Icon name="plus" className="h-4 w-4" />
            Add connector
          </Button>
        </div>

        <div className="mt-6 space-y-3">
          {servers.length === 0 && (
            <div className="rounded-2xl border-2 border-dashed border-slate-200 p-10 text-center dark:border-slate-800">
              <Icon name="plug" className="mx-auto h-8 w-8 text-slate-300" />
              <p className="mt-3 text-sm font-medium">No connectors yet</p>
              <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-slate-400">
                Try “Filesystem” to let the AI read files, or “Fetch” to read
                web pages. You can also paste a config from another MCP app.
              </p>
              <Button
                className="mt-4"
                variant="secondary"
                onClick={() => setAdding(true)}
              >
                Add your first connector
              </Button>
            </div>
          )}

          {servers.map((s) => {
            const st = statusLabel(s.status);
            const needsAuth =
              typeof s.status === "object" && "needs_auth" in s.status;
            return (
              <div
                key={s.id}
                className="flex items-center gap-4 rounded-2xl border border-slate-200 bg-white p-4 shadow-sm transition hover:border-slate-300 dark:border-slate-800 dark:bg-slate-900"
              >
                <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-slate-100 text-slate-500 dark:bg-slate-800 dark:text-slate-300">
                  <Icon
                    name={s.transport_kind === "http" ? "globe" : "terminal"}
                    className="h-5 w-5"
                  />
                </span>
                <button
                  className="min-w-0 flex-1 text-left"
                  onClick={() => setDetailId(s.id)}
                >
                  <div className="flex items-center gap-2">
                    <StatusDot status={st.kind} />
                    <span className="truncate font-semibold">{s.name}</span>
                    <span className="rounded-md bg-slate-100 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-slate-400 dark:bg-slate-800">
                      {s.transport_kind}
                    </span>
                    {!s.enabled && (
                      <span className="rounded-md bg-slate-100 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-slate-400 dark:bg-slate-800">
                        off
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 truncate text-xs text-slate-400">
                    {st.text}
                    {typeof s.status === "object" &&
                      "error" in s.status &&
                      ` — ${s.status.error.message}`}
                    {" · "}
                    {s.tools.length} tools
                    {s.prompts.length > 0
                      ? ` · ${s.prompts.length} prompts`
                      : ""}
                  </div>
                </button>
                <div className="flex shrink-0 items-center gap-1.5">
                  {needsAuth && s.enabled && (
                    <Button variant="primary" onClick={() => signIn(s)}>
                      {authReason(s.status) === "missing"
                        ? "Sign in"
                        : "Re-authenticate"}
                    </Button>
                  )}
                  {!needsAuth && s.enabled && (
                    <Button
                      variant="secondary"
                      onClick={() =>
                        s.status === "connected"
                          ? api.mcpDisconnect(s.id).then(refreshServers)
                          : connect(s)
                      }
                    >
                      {s.status === "connected" ? "Disconnect" : "Connect"}
                    </Button>
                  )}
                  <label
                    className="ml-1 cursor-pointer"
                    title={s.enabled ? "Disable" : "Enable"}
                  >
                    <input
                      type="checkbox"
                      className="peer sr-only"
                      checked={s.enabled}
                      onChange={() => toggleEnabled(s)}
                    />
                    <span className="relative block h-5.5 w-10 rounded-full bg-slate-300 transition peer-checked:bg-sky-500 dark:bg-slate-700 after:absolute after:left-0.5 after:top-0.5 after:h-4.5 after:w-4.5 after:rounded-full after:bg-white after:transition peer-checked:after:translate-x-4" />
                  </label>
                  <Button
                    variant="ghost"
                    title="Remove"
                    onClick={async () => {
                      try {
                        await api.mcpRemove(s.id);
                        setDetailId((id) => (id === s.id ? null : id));
                        await refreshServers();
                      } catch (e) {
                        toast("error", `${e}`);
                      }
                    }}
                  >
                    <Icon name="trash" className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      <AddConnectorModal open={adding} onClose={() => setAdding(false)} />
      <ServerDetailModal
        serverId={detailId}
        onClose={() => setDetailId(null)}
      />
    </div>
  );
}
