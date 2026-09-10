import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useStore } from "../../store";
import * as api from "../../api";
import type { EffortLevel, ProviderConfig } from "../../types";
import { Button, Field, Modal, inputClass } from "../modals/Modal";
import { Icon } from "../icons";
import {
  EFFORT_LABELS,
  EffortPill,
  useEffortLevels,
} from "../chat/effortLevels";

export function SettingsView() {
  const config = useStore((s) => s.config);
  const version = useStore((s) => s.version);
  const homeDir = useStore((s) => s.homeDir);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const toast = useStore((s) => s.toast);
  const setView = useStore((s) => s.setView);
  const [editingProvider, setEditingProvider] = useState<ProviderConfig | null>(
    null,
  );
  const [addingProvider, setAddingProvider] = useState(false);

  if (!config) return null;
  const settings = config.settings;

  const patch = async (p: Parameters<typeof api.settingsSet>[0]) => {
    await api.settingsSet(p);
    await refreshConfig();
  };

  const changeWorkingDir = async () => {
    const path = await open({
      directory: true,
      defaultPath: settings.working_dir ?? (homeDir || undefined),
    });
    if (typeof path === "string" && path !== settings.working_dir) {
      await patch({ working_dir: path }).catch((e) => toast("error", `${e}`));
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl space-y-6 px-6 py-8">
        <h1 className="text-xl font-bold">Settings</h1>

        {/* Providers */}
        <Section
          title="AI providers"
          description="Where your model comes from. Add as many as you like and switch per chat."
        >
          <div className="space-y-2">
            {config.providers.map((p) => (
              <div
                key={p.id}
                className="flex items-center justify-between gap-3 rounded-xl border border-slate-200 p-3.5 dark:border-slate-700"
              >
                <div className="min-w-0">
                  <div className="font-semibold">{p.name}</div>
                  <div className="truncate text-xs text-slate-400">
                    {p.base_url} · {p.models.length} models
                    {p.default_model ? ` · default: ${p.default_model}` : ""}
                  </div>
                </div>
                <div className="flex shrink-0 gap-1.5">
                  <Button
                    variant="secondary"
                    onClick={() => setEditingProvider(p)}
                  >
                    Edit
                  </Button>
                  <Button
                    variant="ghost"
                    onClick={async () => {
                      await api.providerDelete(p.id);
                      await refreshConfig();
                    }}
                  >
                    <Icon name="trash" className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            ))}
            <Button variant="secondary" onClick={() => setAddingProvider(true)}>
              <Icon name="plus" className="h-4 w-4" />
              Add provider
            </Button>
          </div>
        </Section>

        {/* Default model */}
        <Section
          title="Default model"
          description="New chats start with this model and reasoning effort. Without a default, they remember what you used last."
        >
          <DefaultModelSection />
        </Section>

        <Section
          title="Chat title model"
          description="After the first message, this model names the chat. Same as the chat uses that chat's model and effort."
        >
          <TitleModelSection />
        </Section>

        {/* Permissions */}
        <Section
          title="Tool permissions"
          description="Tools can read data and take actions. Choose how much Ducky should check with you first."
        >
          <div className="space-y-2">
            {(
              [
                [
                  "always_ask",
                  "Ask before every tool",
                  "Safest. You approve each tool call with its arguments.",
                ],
                [
                  "auto_approve_read_only",
                  "Auto-approve read-only tools",
                  "Tools the server marks as read-only run automatically; the rest ask.",
                ],
                [
                  "auto_approve_all",
                  "Auto-approve everything",
                  "Nothing asks. Only comfortable with fully trusted servers.",
                ],
              ] as const
            ).map(([value, label, hint]) => (
              <label
                key={value}
                className={`flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition ${
                  settings.tool_approval === value
                    ? "border-sky-500 bg-sky-50/60 dark:bg-sky-950/30"
                    : "border-slate-200 dark:border-slate-700"
                }`}
              >
                <input
                  type="radio"
                  className="mt-1"
                  checked={settings.tool_approval === value}
                  onChange={() =>
                    patch({ tool_approval: value }).catch((e) =>
                      toast("error", `${e}`),
                    )
                  }
                />
                <span>
                  <span className="block text-sm font-medium">{label}</span>
                  <span className="text-xs text-slate-400">{hint}</span>
                </span>
              </label>
            ))}
          </div>

          <div className="mt-4">
            <div className="mb-1.5 text-sm font-medium">Sampling requests</div>
            <p className="mb-2 text-xs text-slate-400">
              Some servers ask your model to help them (e.g. to summarise).
              Decide if that's OK.
            </p>
            <select
              className={inputClass}
              value={settings.sampling}
              onChange={(e) =>
                patch({ sampling: e.target.value as any }).catch((err) =>
                  toast("error", `${err}`),
                )
              }
            >
              <option value="ask">Ask me each time</option>
              <option value="auto_approve">Always allow</option>
              <option value="deny">Always deny</option>
            </select>
          </div>

          {Object.keys(settings.tool_rules).length > 0 && (
            <div className="mt-4">
              <div className="mb-1.5 text-sm font-medium">Per-tool rules</div>
              <div className="space-y-1.5">
                {Object.entries(settings.tool_rules).map(([key, rule]) => (
                  <div
                    key={key}
                    className="flex items-center justify-between rounded-lg bg-slate-50 px-3 py-2 text-xs dark:bg-slate-800"
                  >
                    <span className="font-mono">{key}</span>
                    <span className="flex items-center gap-2">
                      <span
                        className={`rounded px-1.5 py-0.5 font-medium ${
                          rule === "allow"
                            ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300"
                            : "bg-rose-100 text-rose-700 dark:bg-rose-950 dark:text-rose-300"
                        }`}
                      >
                        {rule}
                      </span>
                      <button
                        className="text-slate-400 hover:text-slate-600"
                        onClick={async () => {
                          await api.toolRuleSet(key, null);
                          await refreshConfig();
                        }}
                      >
                        <Icon name="x" className="h-3.5 w-3.5" />
                      </button>
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </Section>

        {/* Working directory */}
        <Section
          title="Working directory"
          description="New chats start here. Local MCP servers launch in this folder, and the AI treats it as the base for file paths."
        >
          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3 rounded-xl bg-slate-50 px-3 py-2 text-sm dark:bg-slate-800">
              <span className="flex min-w-0 items-center gap-2">
                <Icon
                  name="folder"
                  className="h-4 w-4 shrink-0 text-slate-400"
                />
                <span className="truncate font-mono">
                  {settings.working_dir ?? homeDir}
                </span>
              </span>
              {settings.working_dir && (
                <span className="shrink-0 rounded bg-amber-100 px-1.5 py-0.5 text-xs font-medium text-amber-700 dark:bg-amber-950 dark:text-amber-300">
                  custom
                </span>
              )}
            </div>
            <div className="flex gap-2">
              <Button variant="secondary" onClick={changeWorkingDir}>
                <Icon name="folder" className="h-4 w-4" />
                Change…
              </Button>
              {settings.working_dir && (
                <Button
                  variant="ghost"
                  onClick={() =>
                    patch({ working_dir: null }).catch((e) =>
                      toast("error", `${e}`),
                    )
                  }
                >
                  Reset to home folder
                </Button>
              )}
            </div>
            <p className="text-xs text-slate-400">
              Changing this restarts connected local MCP servers so they pick up
              the new folder.
            </p>
          </div>
        </Section>

        {/* Folders */}
        <Section
          title="Shared folders"
          description="Folders offered to servers that ask for workspace access (the legacy MCP “roots” feature)."
        >
          <div className="space-y-2">
            {settings.roots.map((r) => (
              <div
                key={r}
                className="flex items-center justify-between rounded-xl bg-slate-50 px-3 py-2 text-sm dark:bg-slate-800"
              >
                <span className="flex items-center gap-2">
                  <Icon name="folder" className="h-4 w-4 text-slate-400" />
                  {r}
                </span>
                <button
                  className="text-slate-400 hover:text-rose-600"
                  onClick={() =>
                    patch({ roots: settings.roots.filter((x) => x !== r) })
                  }
                >
                  <Icon name="x" className="h-4 w-4" />
                </button>
              </div>
            ))}
            <Button
              variant="secondary"
              onClick={async () => {
                const path = await open({ directory: true });
                if (
                  typeof path === "string" &&
                  !settings.roots.includes(path)
                ) {
                  await patch({ roots: [...settings.roots, path] });
                }
              }}
            >
              <Icon name="folder" className="h-4 w-4" />
              Add folder
            </Button>
          </div>
        </Section>

        {/* Advanced */}
        <Section title="Advanced">
          <div className="space-y-4">
            <div>
              <div className="mb-1.5 text-sm font-medium">
                Show the AI's reasoning as it streams
              </div>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={settings.show_reasoning}
                  onChange={(e) => patch({ show_reasoning: e.target.checked })}
                />
                Show “thinking” text from models that provide it
              </label>
            </div>
            <div>
              <div className="mb-1.5 text-sm font-medium">
                MCP tool input and output
              </div>
              <p className="mb-2 text-xs text-slate-400">
                You can still click a card to show or hide it.
              </p>
              <div className="space-y-2">
                {(
                  [
                    [
                      "auto",
                      "Auto",
                      "Open while a call runs; closed when you load a finished call.",
                    ],
                    ["collapsed", "Always hide", "Start closed."],
                    [
                      "expanded",
                      "Always show",
                      "Start open, including finished calls.",
                    ],
                  ] as const
                ).map(([value, label, hint]) => (
                  <label
                    key={value}
                    className={`flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition ${
                      (settings.tool_details ?? "auto") === value
                        ? "border-sky-500 bg-sky-50/60 dark:bg-sky-950/30"
                        : "border-slate-200 dark:border-slate-700"
                    }`}
                  >
                    <input
                      type="radio"
                      className="mt-1"
                      checked={(settings.tool_details ?? "auto") === value}
                      onChange={() =>
                        patch({ tool_details: value }).catch((e) =>
                          toast("error", `${e}`),
                        )
                      }
                    />
                    <span>
                      <span className="block text-sm font-medium">{label}</span>
                      <span className="text-xs text-slate-400">{hint}</span>
                    </span>
                  </label>
                ))}
              </div>
            </div>
            <div>
              <div className="mb-1.5 text-sm font-medium">
                Tool-call rounds per message
              </div>
              <input
                className={inputClass + " w-32"}
                type="number"
                min={1}
                max={100}
                value={settings.max_tool_iterations}
                onChange={(e) =>
                  patch({
                    max_tool_iterations: Number(e.target.value) || 25,
                  }).catch((err) => toast("error", `${err}`))
                }
              />
              <p className="mt-1 text-xs text-slate-400">
                How many tool steps the AI may take for one request.
              </p>
            </div>
            <div>
              <div className="mb-1.5 text-sm font-medium">Appearance</div>
              <select
                className={inputClass + " w-44"}
                value={settings.theme}
                onChange={(e) => patch({ theme: e.target.value as any })}
              >
                <option value="system">Match system</option>
                <option value="light">Light</option>
                <option value="dark">Dark</option>
              </select>
            </div>
          </div>
        </Section>

        {/* About */}
        <Section title="About">
          <div className="space-y-1 text-sm text-slate-500 dark:text-slate-400">
            <p className="flex items-center gap-2 font-medium text-slate-700 dark:text-slate-200">
              <span className="flex h-6 w-6 items-center justify-center rounded-lg bg-sky-500 text-white">
                <Icon name="duck" className="h-4 w-4" />
              </span>
              Ducky v{version}
            </p>
            <p>MCP specification: 2026-07-28 (full client support).</p>
            <p>
              Built with Rust + Tauri 2. Your keys and tokens stay on this
              computer, in a private file only Ducky reads.
            </p>
          </div>
        </Section>
      </div>

      {/* provider editor */}
      {editingProvider && (
        <ProviderEditorModal
          provider={editingProvider}
          onClose={() => setEditingProvider(null)}
        />
      )}
      {addingProvider && (
        <Modal
          open
          onClose={() => setAddingProvider(false)}
          title="Add a provider"
        >
          <p className="text-sm leading-relaxed text-slate-500 dark:text-slate-400">
            The guided wizard picks the right settings for each provider.
          </p>
          <div className="mt-4 flex justify-end gap-2">
            <Button
              variant="secondary"
              onClick={() => setAddingProvider(false)}
            >
              Cancel
            </Button>
            <Button
              onClick={() => {
                setAddingProvider(false);
                setView("onboarding");
              }}
            >
              Open wizard
            </Button>
          </div>
        </Modal>
      )}
    </div>
  );
}

function Section({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-2xl border border-slate-200 bg-white p-5 shadow-sm dark:border-slate-800 dark:bg-slate-900">
      <h2 className="font-semibold">{title}</h2>
      {description && (
        <p className="mb-4 mt-1 max-w-2xl text-xs leading-relaxed text-slate-400">
          {description}
        </p>
      )}
      {description ? children : <div className="mt-4">{children}</div>}
    </section>
  );
}

function DefaultModelSection() {
  const config = useStore((s) => s.config);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const toast = useStore((s) => s.toast);

  const defProviderId = config?.settings.default_provider_id ?? null;
  const defModel = config?.settings.default_model ?? null;
  const provider = defProviderId
    ? (config?.providers.find((p) => p.id === defProviderId) ?? null)
    : null;
  // the model new chats would actually resolve to — effort pills reflect it
  const resolvedModel =
    (provider && defModel) ||
    provider?.default_model ||
    provider?.models[0] ||
    "";
  const efforts = useEffortLevels(provider?.kind, resolvedModel || undefined);
  const effort = config?.settings.default_effort ?? null;

  if (!config) return null;

  const save = async (
    providerId: string,
    modelId: string,
    eff: EffortLevel | null,
  ) => {
    try {
      await api.settingsSet({
        default_provider_id: providerId,
        default_model: modelId,
        default_effort: eff,
      });
      await refreshConfig();
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  return (
    <div className="space-y-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <div>
          <div className="mb-1.5 text-sm font-medium">Provider</div>
          <select
            className={inputClass}
            value={defProviderId ?? ""}
            onChange={(e) => save(e.target.value, "", null)}
          >
            <option value="">None — remember last used</option>
            {config.providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </div>
        <div>
          <div className="mb-1.5 text-sm font-medium">Model</div>
          <select
            className={inputClass}
            value={defModel ?? ""}
            disabled={!provider}
            onChange={(e) => save(defProviderId ?? "", e.target.value, effort)}
          >
            <option value="">Provider default</option>
            {(provider?.models ?? []).map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
        </div>
      </div>
      {provider && efforts.length > 0 && (
        <div>
          <div className="mb-1.5 text-sm font-medium">Reasoning effort</div>
          <div className="flex flex-wrap gap-1">
            <EffortPill
              selected={effort === null}
              label="Default"
              onClick={() => save(defProviderId ?? "", defModel ?? "", null)}
            />
            {efforts.map((level) => (
              <EffortPill
                key={level}
                selected={effort === level}
                label={EFFORT_LABELS[level]}
                onClick={() => save(defProviderId ?? "", defModel ?? "", level)}
              />
            ))}
          </div>
          <p className="mt-1.5 text-xs text-slate-400">
            Applies to {resolvedModel || "the provider's default model"}.
          </p>
        </div>
      )}
    </div>
  );
}

function TitleModelSection() {
  const config = useStore((s) => s.config);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const toast = useStore((s) => s.toast);

  const providerId = config?.settings.title_provider_id ?? null;
  const modelId = config?.settings.title_model ?? null;
  const provider = providerId
    ? (config?.providers.find((p) => p.id === providerId) ?? null)
    : null;
  const resolvedModel =
    (provider && modelId) ||
    provider?.default_model ||
    provider?.models[0] ||
    "";
  const efforts = useEffortLevels(provider?.kind, resolvedModel || undefined);
  const effort = config?.settings.title_effort ?? null;

  if (!config) return null;

  const save = async (
    nextProviderId: string,
    nextModelId: string,
    eff: EffortLevel | null,
  ) => {
    try {
      await api.settingsSet({
        title_provider_id: nextProviderId,
        title_model: nextModelId,
        title_effort: eff,
      });
      await refreshConfig();
    } catch (e) {
      toast("error", `${e}`);
    }
  };

  return (
    <div className="space-y-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <div>
          <div className="mb-1.5 text-sm font-medium">Provider</div>
          <select
            className={inputClass}
            value={providerId ?? ""}
            onChange={(e) => save(e.target.value, "", null)}
          >
            <option value="">Same as the chat</option>
            {config.providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </div>
        <div>
          <div className="mb-1.5 text-sm font-medium">Model</div>
          <select
            className={inputClass}
            value={modelId ?? ""}
            disabled={!provider}
            onChange={(e) => save(providerId ?? "", e.target.value, effort)}
          >
            <option value="">Provider default</option>
            {(provider?.models ?? []).map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
        </div>
      </div>
      {provider && efforts.length > 0 && (
        <div>
          <div className="mb-1.5 text-sm font-medium">Reasoning effort</div>
          <div className="flex flex-wrap gap-1">
            <EffortPill
              selected={effort === null}
              label="Default"
              onClick={() => save(providerId ?? "", modelId ?? "", null)}
            />
            {efforts.map((level) => (
              <EffortPill
                key={level}
                selected={effort === level}
                label={EFFORT_LABELS[level]}
                onClick={() => save(providerId ?? "", modelId ?? "", level)}
              />
            ))}
          </div>
          <p className="mt-1.5 text-xs text-slate-400">
            Applies to {resolvedModel || "the provider's default model"}.
          </p>
        </div>
      )}
    </div>
  );
}

function ProviderEditorModal({
  provider,
  onClose,
}: {
  provider: ProviderConfig;
  onClose: () => void;
}) {
  const refreshConfig = useStore((s) => s.refreshConfig);
  const toast = useStore((s) => s.toast);
  const [name, setName] = useState(provider.name);
  const [baseUrl, setBaseUrl] = useState(provider.base_url);
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState(provider.default_model ?? "");
  const [models, setModels] = useState(provider.models);
  const [busy, setBusy] = useState(false);

  const save = async () => {
    setBusy(true);
    try {
      await api.providerUpdate({
        id: provider.id,
        name: name.trim(),
        base_url: baseUrl.trim(),
        default_model: model,
        api_key: apiKey.trim() ? apiKey.trim() : undefined,
      });
      await refreshConfig();
      onClose();
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  const test = async () => {
    setBusy(true);
    try {
      await api.providerUpdate({
        id: provider.id,
        name: name.trim(),
        base_url: baseUrl.trim(),
        api_key: apiKey.trim() ? apiKey.trim() : undefined,
      });
      const list = await api.providerTest(provider.id);
      setModels(list);
      await refreshConfig();
      toast("success", `Connected — ${list.length} models available.`);
    } catch (e) {
      toast("error", `${e}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal open onClose={onClose} title={`Edit ${provider.name}`} wide>
      <div className="space-y-4">
        <Field label="Name">
          <input
            className={inputClass}
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </Field>
        <Field label="Base URL">
          <input
            className={inputClass}
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
          />
        </Field>
        <Field label="API key" hint="Leave blank to keep the saved key.">
          <input
            className={inputClass}
            type="password"
            value={apiKey}
            placeholder="unchanged"
            onChange={(e) => setApiKey(e.target.value)}
          />
        </Field>
        <Field label="Default model">
          {models.length > 0 ? (
            <select
              className={inputClass}
              value={model}
              onChange={(e) => setModel(e.target.value)}
            >
              <option value="">(none)</option>
              {models.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          ) : (
            <input
              className={inputClass}
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="model-id"
            />
          )}
        </Field>
        <div className="flex items-center justify-between">
          <Button variant="secondary" disabled={busy} onClick={test}>
            <Icon name="refresh" className="h-4 w-4" />
            Test connection
          </Button>
          <Button disabled={busy} onClick={save}>
            Save
          </Button>
        </div>
      </div>
    </Modal>
  );
}
