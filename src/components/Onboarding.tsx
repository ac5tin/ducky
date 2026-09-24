import { useState } from "react";
import { useStore } from "../store";
import { Button, Field, inputClass } from "./modals/Modal";
import { Icon } from "./icons";
import * as api from "../api";
import type { ApiType, ProviderPreset } from "../types";

/** First-run wizard: provider → key → optional first connector. */
export function Onboarding() {
  const config = useStore((s) => s.config);
  const presets = useStore((s) => s.presets);
  const suggestions = useStore((s) => s.suggestions);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const refreshServers = useStore((s) => s.refreshServers);
  const setView = useStore((s) => s.setView);
  const toast = useStore((s) => s.toast);
  const [step, setStep] = useState(0);
  const [preset, setPreset] = useState<ProviderPreset | null>(null);
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [addedServers, setAddedServers] = useState<string[]>([]);

  const resetProvider = (p: ProviderPreset) => {
    setPreset(p);
    setName(p.id === "custom" ? "My provider" : p.label);
    setBaseUrl(p.base_url);
    setApiKey("");
    setTestResult(null);
  };

  const finish = async () => {
    await refreshConfig();
    setView("chat");
  };

  const connectProvider = async () => {
    if (!preset) return;
    setTesting(true);
    setTestResult(null);
    try {
      const provider = await api.providerAdd({
        kind: preset.id,
        name: name.trim() || preset.label,
        base_url: baseUrl.trim(),
        api_type: preset.api_type as ApiType,
        api_key: apiKey.trim() || undefined,
      });
      const models = await api.providerTest(provider.id).catch(() => [] as string[]);
      await refreshConfig();
      if (models.length > 0) {
        setTestResult({ ok: true, text: `Connected — ${models.length} models available.` });
        setStep(3);
      } else {
        // provider saved; maybe offline/local without model listing
        setTestResult({
          ok: true,
          text: "Provider saved. (Couldn't list models automatically — you can type one in Settings.)",
        });
        setStep(3);
      }
    } catch (e) {
      setTestResult({ ok: false, text: `${e}` });
    } finally {
      setTesting(false);
    }
  };

  return (
    <div className="h-full overflow-y-auto bg-gradient-to-b from-sky-50 to-white dark:from-slate-900 dark:to-slate-950">
      <div className="mx-auto flex min-h-full max-w-xl flex-col justify-center px-6 py-10">
        {config?.onboarding_complete && (
          <div className="mb-4 flex justify-end">
            <Button variant="ghost" onClick={() => setView("settings")}>
              <Icon name="x" className="h-4 w-4" />
              Close
            </Button>
          </div>
        )}
        {/* step indicator */}
        <div className="mb-8 flex items-center gap-2">
          {[0, 1, 2, 3].map((i) => (
            <div
              key={i}
              className={`h-1.5 flex-1 rounded-full transition ${
                i <= step ? "bg-sky-500" : "bg-slate-200 dark:bg-slate-800"
              }`}
            />
          ))}
        </div>

        {step === 0 && (
          <div className="text-center">
            <div className="mx-auto mb-5 flex h-24 w-24 items-center justify-center rounded-[1.75rem] bg-sky-500 text-white shadow-xl shadow-sky-200 dark:shadow-sky-950">
              <Icon name="duck" className="h-14 w-14" />
            </div>
            <h1 className="text-2xl font-bold">Welcome to Ducky</h1>
            <p className="mx-auto mt-3 max-w-md text-sm leading-relaxed text-slate-500 dark:text-slate-400">
              Ducky connects your favourite AI — OpenAI, Claude, Z.ai, Ollama and more —
              to helpful <strong>tools</strong>: reading files, searching the web, and
              anything else you connect. Setup takes about a minute.
            </p>
            <Button className="mt-7 px-6 py-2.5" onClick={() => setStep(1)}>
              Let's go 🦆
            </Button>
          </div>
        )}

        {step === 1 && (
          <div>
            <h1 className="text-xl font-bold">Which AI should Ducky use?</h1>
            <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">
              You can add more anytime in Settings.
            </p>
            <div className="mt-5 grid gap-2.5 sm:grid-cols-2">
              {presets.map((p) => (
                <button
                  key={p.id}
                  className={`rounded-xl border p-3.5 text-left transition ${
                    preset?.id === p.id
                      ? "border-sky-500 bg-sky-50 ring-2 ring-sky-100 dark:bg-sky-950/40 dark:ring-sky-900"
                      : "border-slate-200 bg-white hover:border-sky-300 dark:border-slate-700 dark:bg-slate-900"
                  }`}
                  onClick={() => {
                    setPreset(p);
                    resetProvider(p);
                  }}
                >
                  <div className="flex items-center justify-between">
                    <span className="text-sm font-semibold">{p.label}</span>
                    {!p.needs_key && (
                      <span className="rounded-md bg-emerald-100 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:bg-emerald-950 dark:text-emerald-300">
                        local & free
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 text-xs text-slate-400">{p.tagline}</div>
                </button>
              ))}
            </div>
            <div className="mt-6 flex justify-between">
              <Button variant="ghost" onClick={() => setStep(0)}>
                Back
              </Button>
              <Button disabled={!preset} onClick={() => setStep(2)}>
                Continue
              </Button>
            </div>
          </div>
        )}

        {step === 2 && preset && (
          <div>
            <h1 className="text-xl font-bold">Connect {preset.label}</h1>
            <div className="mt-5 space-y-4">
              <Field label="Name">
                <input className={inputClass} value={name} onChange={(e) => setName(e.target.value)} />
              </Field>
              {(preset.id === "custom" || baseUrl !== preset.base_url) && (
                <Field
                  label="Server address (base URL)"
                  hint={preset.id === "custom" ? "Any OpenAI- or Anthropic-compatible endpoint." : undefined}
                >
                  <input className={inputClass} value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
                </Field>
              )}
              {preset.needs_key && (
                <Field
                  label="API key"
                  hint={
                    <>
                      Stored privately on this computer, never shared.{" "}
                      {preset.key_url && (
                        <a
                          className="text-sky-600 underline underline-offset-2 dark:text-sky-400"
                          onClick={(e) => {
                            e.preventDefault();
                            import("@tauri-apps/plugin-opener").then((m) => m.openUrl(preset.key_url));
                          }}
                        >
                          Get a key ↗
                        </a>
                      )}
                    </>
                  }
                >
                  <div className="relative">
                    <input
                      className={inputClass + " pr-16"}
                      type={showKey ? "text" : "password"}
                      placeholder="sk-…"
                      value={apiKey}
                      onChange={(e) => setApiKey(e.target.value)}
                      onKeyDown={(e) => e.key === "Enter" && connectProvider()}
                    />
                    <button
                      className="absolute right-2.5 top-1/2 -translate-y-1/2 text-xs font-medium text-slate-400 hover:text-slate-600"
                      onClick={() => setShowKey(!showKey)}
                    >
                      {showKey ? "Hide" : "Show"}
                    </button>
                  </div>
                </Field>
              )}
              {testResult && (
                <div
                  className={`rounded-xl px-3.5 py-2.5 text-sm ${
                    testResult.ok
                      ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-300"
                      : "bg-rose-50 text-rose-700 dark:bg-rose-950/50 dark:text-rose-300"
                  }`}
                >
                  {testResult.ok ? "✅ " : "⚠️ "}
                  {testResult.text}
                </div>
              )}
            </div>
            <div className="mt-6 flex items-center justify-between">
              <Button variant="ghost" onClick={() => setStep(1)}>
                Back
              </Button>
              <Button onClick={connectProvider} disabled={testing || !baseUrl.trim()}>
                {testing ? "Testing…" : preset.needs_key && !apiKey.trim() ? "Save (key optional)" : "Test & continue"}
              </Button>
            </div>
          </div>
        )}

        {step === 3 && (
          <div>
            <h1 className="text-xl font-bold">Add your first connector (optional)</h1>
            <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">
              Connectors are tool packs the AI can use. You can skip this and add more
              later.
            </p>
            <div className="mt-5 space-y-2.5">
              {suggestions.map((s) => {
                const added = addedServers.includes(s.name);
                return (
                  <div
                    key={s.name}
                    className="flex items-center justify-between gap-3 rounded-xl border border-slate-200 bg-white p-3.5 dark:border-slate-700 dark:bg-slate-900"
                  >
                    <div className="min-w-0">
                      <div className="text-sm font-semibold">{s.name}</div>
                      <div className="text-xs text-slate-400">{s.description}</div>
                    </div>
                    <Button
                      variant={added ? "success" : "secondary"}
                      disabled={added}
                      onClick={async () => {
                        try {
                          await api.mcpAdd({
                            name: s.name,
                            transport: { type: "stdio", command: s.command, args: s.args, env: {} },
                            enabled: true,
                          });
                          setAddedServers((prev) => [...prev, s.name]);
                          refreshServers().catch(() => {});
                          toast("success", `${s.name} added — it will start automatically.`);
                        } catch (e) {
                          toast("error", `${e}`);
                        }
                      }}
                    >
                      {added ? "Added ✓" : "Add"}
                    </Button>
                  </div>
                );
              })}
            </div>
            <div className="mt-7 flex justify-end">
              <Button onClick={finish}>Start chatting →</Button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
