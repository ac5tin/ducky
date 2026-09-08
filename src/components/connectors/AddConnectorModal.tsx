import { useState } from "react";
import { useStore } from "../../store";
import * as api from "../../api";
import { Button, Field, Modal, inputClass } from "../modals/Modal";
import { Icon } from "../icons";

type Tab = "popular" | "command" | "url" | "import";

export function AddConnectorModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const suggestions = useStore((s) => s.suggestions);
  const refreshServers = useStore((s) => s.refreshServers);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const toast = useStore((s) => s.toast);
  const [tab, setTab] = useState<Tab>("popular");

  // shared
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

  // command tab
  const [command, setCommand] = useState("");
  const [argsText, setArgsText] = useState("");
  const [envText, setEnvText] = useState("");

  // url tab
  const [url, setUrl] = useState("");
  const [headersText, setHeadersText] = useState("");
  const [authKind, setAuthKind] = useState<"none" | "bearer" | "oauth">("none");
  const [bearerToken, setBearerToken] = useState("");
  const [oauthClientId, setOauthClientId] = useState("");
  const [oauthSecret, setOauthSecret] = useState("");
  const [oauthPort, setOauthPort] = useState("");

  // import tab
  const [importText, setImportText] = useState("");
  const [preview, setPreview] = useState<{ name: string; transport: any }[]>([]);

  const reset = () => {
    setName("");
    setCommand("");
    setArgsText("");
    setEnvText("");
    setUrl("");
    setHeadersText("");
    setAuthKind("none");
    setBearerToken("");
    setOauthClientId("");
    setOauthSecret("");
    setOauthPort("");
    setImportText("");
    setPreview([]);
  };

  const parseEnv = (text: string): Record<string, string> => {
    const out: Record<string, string> = {};
    for (const line of text.split("\n")) {
      const idx = line.indexOf("=");
      if (idx > 0) out[line.slice(0, idx).trim()] = line.slice(idx + 1).trim();
    }
    return out;
  };

  const finishAdd = async (serverName: string, connect: boolean) => {
    await refreshServers();
    await refreshConfig();
    if (connect) {
      try {
        const result = await api.mcpConnect(
          (await api.mcpSummaries()).find((s) => s.name === serverName)?.id ?? "",
        );
        await refreshServers();
        if (result === "connected") toast("success", `${serverName} connected!`);
        else if (result === "needs_auth") toast("info", `${serverName} needs a sign-in — press “Sign in”.`);
        else toast("error", `${serverName} didn't connect — open it for details.`);
      } catch (e) {
        toast("error", `${e}`);
      }
    }
  };

  const addSuggestion = async (s: { name: string; command: string; args: string[] }) => {
    setBusy(true);
    try {
      await api.mcpAdd({
        name: s.name,
        transport: { type: "stdio", command: s.command, args: s.args, env: {} },
        enabled: true,
      });
      await finishAdd(s.name, true);
      onClose();
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const addCommand = async () => {
    setBusy(true);
    try {
      await api.mcpAdd({
        name: name.trim(),
        transport: {
          type: "stdio",
          command: command.trim(),
          args: argsText.trim() ? argsText.trim().split(/\s+/) : [],
          env: parseEnv(envText),
        },
        enabled: true,
      });
      await finishAdd(name.trim(), true);
      reset();
      onClose();
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const addUrl = async () => {
    setBusy(true);
    try {
      const cfg = await api.mcpAdd({
        name: name.trim(),
        transport: { type: "http", url: url.trim(), headers: parseEnv(headersText) },
        auth:
          authKind === "oauth"
            ? { type: "oauth" }
            : authKind === "bearer"
              ? { type: "bearer", token_ref: "" }
              : { type: "none" },
        enabled: true,
      });
      if (authKind === "oauth") {
        await api.mcpSetOauthConfig(
          cfg.id,
          oauthClientId.trim() || null,
          oauthSecret.trim() || null,
          oauthPort.trim() ? Number(oauthPort.trim()) : null,
        );
      }
      if (authKind === "bearer" && bearerToken.trim()) {
        await api.mcpSetBearerToken(cfg.id, bearerToken.trim());
      }
      await finishAdd(name.trim(), authKind !== "oauth");
      if (authKind === "oauth") {
        await api.mcpOauthLogin(cfg.id).catch((e) => toast("error", `${e}`));
      }
      reset();
      onClose();
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const doImport = async () => {
    setBusy(true);
    try {
      const list = await api.mcpImportPreview(importText);
      setPreview(list);
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const confirmImport = async () => {
    setBusy(true);
    try {
      const count = await api.mcpImportAdd(preview);
      await refreshServers();
      await refreshConfig();
      toast("success", `Imported ${count} connector${count === 1 ? "" : "s"}.`);
      reset();
      onClose();
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const tabs: { id: Tab; label: string }[] = [
    { id: "popular", label: "Popular" },
    { id: "command", label: "Local command" },
    { id: "url", label: "Remote URL" },
    { id: "import", label: "Import JSON" },
  ];

  return (
    <Modal open={open} onClose={onClose} title="Add a connector" wide>
      <div className="mb-4 flex gap-1 rounded-xl bg-slate-100 p-1 dark:bg-slate-800">
        {tabs.map((t) => (
          <button
            key={t.id}
            className={`flex-1 rounded-lg px-2 py-1.5 text-xs font-medium transition ${
              tab === t.id
                ? "bg-white shadow-sm dark:bg-slate-700"
                : "text-slate-500 hover:text-slate-700 dark:hover:text-slate-200"
            }`}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      {tab === "popular" && (
        <div className="space-y-2">
          {suggestions.map((s) => (
            <div
              key={s.name}
              className="flex items-center justify-between gap-3 rounded-xl border border-slate-200 p-3 dark:border-slate-700"
            >
              <div>
                <div className="text-sm font-semibold">{s.name}</div>
                <div className="text-xs text-slate-400">{s.description}</div>
                <code className="mt-1 block text-[11px] text-slate-400">
                  {s.command} {s.args.join(" ")}
                </code>
              </div>
              <Button disabled={busy} onClick={() => addSuggestion(s)}>
                Add
              </Button>
            </div>
          ))}
          <p className="pt-1 text-xs text-slate-400">
            These run locally via npx — Node.js must be installed.
          </p>
        </div>
      )}

      {tab === "command" && (
        <div className="space-y-4">
          <Field label="Name" hint="Shown to you and to the AI.">
            <input className={inputClass} value={name} onChange={(e) => setName(e.target.value)} placeholder="My tools" />
          </Field>
          <Field label="Command" hint="e.g. npx, python, /usr/bin/someserver">
            <input className={inputClass} value={command} onChange={(e) => setCommand(e.target.value)} placeholder="npx" />
          </Field>
          <Field label="Arguments" hint="Space-separated. Use quotes for values with spaces if importing JSON instead.">
            <input
              className={inputClass}
              value={argsText}
              onChange={(e) => setArgsText(e.target.value)}
              placeholder="-y @modelcontextprotocol/server-filesystem ~/Documents"
            />
          </Field>
          <Field label="Environment variables (one per line)" hint="KEY=value">
            <textarea
              className={inputClass + " h-20 font-mono text-xs"}
              value={envText}
              onChange={(e) => setEnvText(e.target.value)}
              placeholder="API_TOKEN=abc123"
            />
          </Field>
          <div className="flex justify-end">
            <Button disabled={busy || !name.trim() || !command.trim()} onClick={addCommand}>
              Add & connect
            </Button>
          </div>
        </div>
      )}

      {tab === "url" && (
        <div className="space-y-4">
          <Field label="Name">
            <input className={inputClass} value={name} onChange={(e) => setName(e.target.value)} placeholder="Cloud tools" />
          </Field>
          <Field label="Server URL" hint="A Streamable HTTP MCP endpoint.">
            <input
              className={inputClass}
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://mcp.example.com/mcp"
            />
          </Field>
          <Field label="Custom headers (one per line)" hint="Header: value">
            <textarea
              className={inputClass + " h-16 font-mono text-xs"}
              value={headersText}
              onChange={(e) => setHeadersText(e.target.value)}
              placeholder="X-Team-Id: 42"
            />
          </Field>
          <Field label="Authentication">
            <div className="grid grid-cols-3 gap-1.5">
              {(
                [
                  ["none", "None"],
                  ["bearer", "Token"],
                  ["oauth", "Sign in (OAuth)"],
                ] as const
              ).map(([k, label]) => (
                <button
                  key={k}
                  className={`rounded-lg border px-2 py-1.5 text-xs font-medium transition ${
                    authKind === k
                      ? "border-sky-500 bg-sky-50 text-sky-700 dark:bg-sky-950/50 dark:text-sky-300"
                      : "border-slate-200 dark:border-slate-700"
                  }`}
                  onClick={() => setAuthKind(k)}
                >
                  {label}
                </button>
              ))}
            </div>
          </Field>
          {authKind === "bearer" && (
            <Field label="Bearer token" hint="Stored privately on this computer.">
              <input
                className={inputClass}
                type="password"
                value={bearerToken}
                onChange={(e) => setBearerToken(e.target.value)}
                placeholder="paste token…"
              />
            </Field>
          )}
          {authKind === "oauth" && (
            <div className="rounded-xl border border-slate-200 p-3 text-xs dark:border-slate-700">
              <p className="mb-2 leading-relaxed text-slate-500 dark:text-slate-400">
                Ducky will open your browser to sign in (OAuth 2.1 with PKCE). Most
                servers work out of the box; only fill these if the server gave you a
                pre-registered client id.
              </p>
              <div className="grid grid-cols-2 gap-2">
                <input
                  className={inputClass}
                  value={oauthClientId}
                  onChange={(e) => setOauthClientId(e.target.value)}
                  placeholder="Client id (optional)"
                />
                <input
                  className={inputClass}
                  value={oauthPort}
                  onChange={(e) => setOauthPort(e.target.value)}
                  placeholder="Callback port (optional)"
                />
                <input
                  className={inputClass + " col-span-2"}
                  type="password"
                  value={oauthSecret}
                  onChange={(e) => setOauthSecret(e.target.value)}
                  placeholder="Client secret (optional — only for pre-registered confidential clients)"
                />
              </div>
            </div>
          )}
          <div className="flex justify-end">
            <Button disabled={busy || !name.trim() || !url.trim()} onClick={addUrl}>
              Add & connect
            </Button>
          </div>
        </div>
      )}

      {tab === "import" && (
        <div className="space-y-3">
          <Field
            label="Paste config JSON"
            hint="Works with the `mcpServers` format used by Claude Desktop, Cursor and most MCP apps."
          >
            <textarea
              className={inputClass + " h-40 font-mono text-xs"}
              value={importText}
              onChange={(e) => setImportText(e.target.value)}
              placeholder={'{\n  "mcpServers": {\n    "filesystem": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem"] }\n  }\n}'}
            />
          </Field>
          {preview.length > 0 && (
            <div className="rounded-xl border border-slate-200 p-3 dark:border-slate-700">
              <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-slate-400">
                Found {preview.length} server{preview.length === 1 ? "" : "s"}
              </div>
              <ul className="space-y-1 text-sm">
                {preview.map((p) => (
                  <li key={p.name} className="flex items-center gap-2">
                    <Icon name="check" className="h-3.5 w-3.5 text-emerald-500" />
                    <span className="font-medium">{p.name}</span>
                    <span className="text-xs text-slate-400">
                      ({p.transport.type === "http" ? p.transport.url : p.transport.command})
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <div className="flex justify-end gap-2">
            {preview.length === 0 ? (
              <Button disabled={busy || !importText.trim()} onClick={doImport}>
                Preview
              </Button>
            ) : (
              <Button disabled={busy} onClick={confirmImport}>
                Import {preview.length} server{preview.length === 1 ? "" : "s"}
              </Button>
            )}
          </div>
        </div>
      )}
    </Modal>
  );
}
