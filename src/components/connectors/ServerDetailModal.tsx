import { useEffect, useState } from "react";
import { useStore } from "../../store";
import * as api from "../../api";
import { Button, Modal } from "../modals/Modal";
import { Icon, StatusDot } from "../icons";

type Tab = "tools" | "resources" | "prompts" | "info" | "logs";

export function ServerDetailModal({
  serverId,
  onClose,
}: {
  serverId: string | null;
  onClose: () => void;
}) {
  const servers = useStore((s) => s.servers);
  const refreshServer = useStore((s) => s.refreshServer);
  const toast = useStore((s) => s.toast);
  const [tab, setTab] = useState<Tab>("tools");
  const [resourceText, setResourceText] = useState<{ uri: string; text: string } | null>(null);
  const [promptResult, setPromptResult] = useState<string | null>(null);
  const summary = servers.find((s) => s.id === serverId) ?? null;

  useEffect(() => {
    if (!serverId) return;
    setTab("tools");
    setResourceText(null);
    setPromptResult(null);
  }, [serverId]);

  if (!serverId || !summary) return null;

  const st = summary.status;
  const statusKind =
    st === "connected" ? "connected" : typeof st === "object" ? ("needs_auth" in st ? "needs_auth" : "error") : st;

  const refresh = async () => {
    try {
      await api.mcpRefresh(serverId);
      await refreshServer(serverId);
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  const readResource = async (uri: string) => {
    try {
      const contents = await api.mcpReadResource(serverId, uri);
      const first = Array.isArray(contents) ? contents[0] : contents;
      const text = first?.text ?? (first?.blob ? `(binary ${first.mimeType ?? "data"})` : JSON.stringify(first, null, 2));
      setResourceText({ uri, text: text ?? "" });
      setTab("resources");
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  const subscribe = async (uri: string) => {
    try {
      await api.mcpSubscribeResource(serverId, uri);
      toast("success", "Subscribed — you'll be notified when it changes.");
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  const getPrompt = async (name: string) => {
    try {
      const result = await api.mcpGetPrompt(serverId, name, {});
      const messages = result?.messages ?? [];
      const rendered = messages
        .map((m: any) => {
          const c = m?.content;
          const text = c?.text ?? (c?.type === "resource" ? c.resource?.text : JSON.stringify(c));
          return `[${m?.role}] ${text ?? ""}`;
        })
        .join("\n\n");
      setPromptResult(rendered || "(empty prompt)");
      setTab("prompts");
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  const tabs: { id: Tab; label: string; count?: number }[] = [
    { id: "tools", label: "Tools", count: summary.tools.length },
    { id: "resources", label: "Resources", count: summary.resources.length + summary.resource_templates.length },
    { id: "prompts", label: "Prompts", count: summary.prompts.length },
    { id: "info", label: "Info" },
    { id: "logs", label: "Logs" },
  ];

  return (
    <Modal
      open
      onClose={onClose}
      wide
      title={
        <span className="flex items-center gap-2">
          <StatusDot status={statusKind} />
          {summary.name}
        </span>
      }
      subtitle={summary.detail}
    >
      <div className="mb-4 flex items-center justify-between gap-2">
        <div className="flex gap-1 rounded-xl bg-slate-100 p-1 dark:bg-slate-800">
          {tabs.map((t) => (
            <button
              key={t.id}
              className={`rounded-lg px-2.5 py-1.5 text-xs font-medium transition ${
                tab === t.id ? "bg-white shadow-sm dark:bg-slate-700" : "text-slate-500"
              }`}
              onClick={() => setTab(t.id)}
            >
              {t.label}
              {t.count !== undefined && t.count > 0 && (
                <span className="ml-1 text-slate-400">{t.count}</span>
              )}
            </button>
          ))}
        </div>
        <Button variant="secondary" onClick={refresh}>
          <Icon name="refresh" className="h-3.5 w-3.5" />
          Refresh
        </Button>
      </div>

      {tab === "tools" && (
        <div className="space-y-2">
          {summary.tools.length === 0 && <Empty text="No tools (or not connected yet)." />}
          {summary.tools.map((tool) => (
            <div key={tool.qualified_name} className="rounded-xl border border-slate-200 p-3 dark:border-slate-700">
              <div className="flex items-center gap-2">
                <Icon name="wrench" className="h-4 w-4 text-slate-400" />
                <span className="font-mono text-[13px] font-semibold">{tool.qualified_name}</span>
                {tool.read_only_hint && (
                  <span className="rounded bg-emerald-100 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300">
                    read-only
                  </span>
                )}
              </div>
              {tool.title && <div className="mt-0.5 text-xs text-slate-500">{tool.title}</div>}
              {tool.description && (
                <p className="mt-1 text-xs leading-relaxed text-slate-500 dark:text-slate-400">
                  {tool.description}
                </p>
              )}
              {tool.input_schema && Object.keys(tool.input_schema).length > 0 && (
                <details className="mt-2">
                  <summary className="cursor-pointer text-[11px] font-medium text-slate-400">
                    Input schema
                  </summary>
                  <pre className="mt-1 max-h-40 overflow-auto rounded-lg bg-slate-50 p-2 text-[11px] dark:bg-slate-800">
                    {JSON.stringify(tool.input_schema, null, 2)}
                  </pre>
                </details>
              )}
            </div>
          ))}
        </div>
      )}

      {tab === "resources" && (
        <div className="space-y-2">
          {resourceText && (
            <div className="rounded-xl border border-sky-200 bg-sky-50 p-3 dark:border-sky-900 dark:bg-sky-950/40">
              <div className="mb-1 flex items-center justify-between">
                <span className="font-mono text-xs font-semibold">{resourceText.uri}</span>
                <button className="text-slate-400 hover:text-slate-600" onClick={() => setResourceText(null)}>
                  <Icon name="x" className="h-3.5 w-3.5" />
                </button>
              </div>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap text-xs">{resourceText.text}</pre>
            </div>
          )}
          {summary.resources.length === 0 && summary.resource_templates.length === 0 && (
            <Empty text="No resources exposed by this server." />
          )}
          {summary.resources.map((r: any, i: number) => (
            <div key={i} className="flex items-center justify-between gap-2 rounded-xl border border-slate-200 p-3 dark:border-slate-700">
              <div className="min-w-0">
                <div className="truncate text-sm font-medium">{r?.name ?? r?.uri}</div>
                <div className="truncate font-mono text-[11px] text-slate-400">{r?.uri}</div>
                {r?.description && <div className="mt-0.5 text-xs text-slate-400">{r.description}</div>}
              </div>
              <div className="flex shrink-0 gap-1.5">
                <Button variant="secondary" onClick={() => readResource(r.uri)}>
                  Read
                </Button>
                <Button variant="ghost" onClick={() => subscribe(r.uri)} title="Subscribe to updates">
                  <Icon name="refresh" className="h-3.5 w-3.5" />
                </Button>
              </div>
            </div>
          ))}
          {summary.resource_templates.map((r: any, i: number) => (
            <div key={`t${i}`} className="rounded-xl border border-dashed border-slate-200 p-3 dark:border-slate-700">
              <div className="text-sm font-medium">{r?.name ?? "Template"}</div>
              <div className="font-mono text-[11px] text-slate-400">{r?.uriTemplate}</div>
            </div>
          ))}
        </div>
      )}

      {tab === "prompts" && (
        <div className="space-y-2">
          {promptResult && (
            <pre className="max-h-56 overflow-auto whitespace-pre-wrap rounded-xl border border-sky-200 bg-sky-50 p-3 text-xs dark:border-sky-900 dark:bg-sky-950/40">
              {promptResult}
            </pre>
          )}
          {summary.prompts.length === 0 && <Empty text="No prompts exposed by this server." />}
          {summary.prompts.map((p: any, i: number) => (
            <div key={i} className="flex items-center justify-between gap-2 rounded-xl border border-slate-200 p-3 dark:border-slate-700">
              <div className="min-w-0">
                <div className="truncate text-sm font-medium">{p?.name}</div>
                {p?.description && <div className="text-xs text-slate-400">{p.description}</div>}
              </div>
              <Button variant="secondary" onClick={() => getPrompt(p.name)}>
                Preview
              </Button>
            </div>
          ))}
        </div>
      )}

      {tab === "info" && (
        <div className="space-y-3 text-sm">
          <InfoRow label="Status" value={statusKind} />
          <InfoRow label="Protocol version" value={summary.protocol_version ?? "—"} />
          <InfoRow
            label="Server"
            value={
              summary.server_info
                ? `${summary.server_info.name ?? "?"} ${summary.server_info.version ?? ""}`
                : "—"
            }
          />
          {summary.instructions && (
            <div>
              <div className="mb-1 text-xs font-semibold uppercase tracking-wide text-slate-400">
                Server instructions
              </div>
              <p className="rounded-xl bg-slate-50 p-3 text-xs leading-relaxed dark:bg-slate-800">
                {summary.instructions}
              </p>
            </div>
          )}
          {summary.capabilities && (
            <div>
              <div className="mb-1 text-xs font-semibold uppercase tracking-wide text-slate-400">
                Capabilities
              </div>
              <pre className="max-h-48 overflow-auto rounded-xl bg-slate-50 p-3 text-[11px] dark:bg-slate-800">
                {JSON.stringify(summary.capabilities, null, 2)}
              </pre>
            </div>
          )}
        </div>
      )}

      {tab === "logs" && (
        <pre className="max-h-80 overflow-y-auto rounded-xl bg-slate-950 p-3 font-mono text-[11px] leading-relaxed text-slate-300">
          {summary.logs.length > 0 ? summary.logs.join("\n") : "No output captured."}
        </pre>
      )}
    </Modal>
  );
}

function InfoRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between rounded-xl bg-slate-50 px-3 py-2 dark:bg-slate-800">
      <span className="text-xs font-semibold uppercase tracking-wide text-slate-400">{label}</span>
      <span className="text-xs">{value}</span>
    </div>
  );
}

function Empty({ text }: { text: string }) {
  return <p className="rounded-xl bg-slate-50 px-3 py-6 text-center text-xs text-slate-400 dark:bg-slate-800">{text}</p>;
}
