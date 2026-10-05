import { useState } from "react";
import { useStore } from "../../store";
import * as api from "../../api";
import type { PluginSummary, UpdatePolicy } from "../../types";
import {
  INSTALL_TRUST_WARNING,
  formatVersion,
  pluginSkillRows,
  pluginStatusLabel,
  pluginStatusTone,
  pluginTrustSummary,
  progressPhaseLabel,
  serverTrustLine,
  type PluginStatusTone,
} from "../../plugins";
import { pluginSubagentShadowed } from "../../subagents";
import { Button, Modal } from "../modals/Modal";
import { Icon } from "../icons";

/** The Installed tab: one row per plugin, expandable to its components. */
export function InstalledList({
  onDiscover,
  onDetail,
}: {
  onDiscover: () => void;
  onDetail: (id: string) => void;
}) {
  const plugins = useStore((s) => s.plugins);

  if (plugins.length === 0) {
    return (
      <div className="mt-6 rounded-2xl border-2 border-dashed border-slate-200 p-10 text-center dark:border-slate-800">
        <Icon name="puzzle" className="mx-auto h-8 w-8 text-slate-300 dark:text-slate-600" />
        <p className="mt-3 text-sm font-medium">No plugins installed yet</p>
        <p className="mx-auto mt-1 max-w-sm text-xs leading-relaxed text-slate-400">
          Browse the catalog and install one. A new plugin stays disabled until
          you enable it.
        </p>
        <Button className="mt-4" variant="secondary" onClick={onDiscover}>
          Discover plugins
        </Button>
      </div>
    );
  }

  return (
    <div className="mt-5 space-y-3">
      {plugins.map((plugin) => (
        <PluginRow
          key={plugin.id}
          plugin={plugin}
          onDetail={() => onDetail(plugin.id)}
        />
      ))}
    </div>
  );
}

function PluginRow({
  plugin,
  onDetail,
}: {
  plugin: PluginSummary;
  onDetail: () => void;
}) {
  const skills = useStore((s) => s.skills);
  const config = useStore((s) => s.config);
  const progress = useStore((s) => s.pluginProgress[plugin.id]);
  const setPluginEnabled = useStore((s) => s.setPluginEnabled);
  const setPluginServerEnabled = useStore((s) => s.setPluginServerEnabled);
  const setPluginUpdatePolicy = useStore((s) => s.setPluginUpdatePolicy);
  const updatePlugin = useStore((s) => s.updatePlugin);
  const rollbackPlugin = useStore((s) => s.rollbackPlugin);
  const uninstallPlugin = useStore((s) => s.uninstallPlugin);
  const openPluginFolder = useStore((s) => s.openPluginFolder);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const toast = useStore((s) => s.toast);

  const [expanded, setExpanded] = useState(false);
  const [confirmEnable, setConfirmEnable] = useState(false);
  const [confirmUninstall, setConfirmUninstall] = useState(false);
  const [deleteData, setDeleteData] = useState(false);

  const trust = pluginTrustSummary(plugin, INSTALL_TRUST_WARNING);
  const userNames = (config?.subagents ?? []).map((sub) => sub.name);
  const skillRows = pluginSkillRows(plugin, skills);
  const errorDiagnostics = plugin.diagnostics.filter((d) => d.level === "error");

  const toggleEnabled = () => {
    if (plugin.enabled) {
      void setPluginEnabled(plugin.id, false);
      return;
    }
    // enabling is what grants code execution: confirm when there is code to run
    if (trust.servers.length > 0 || trust.subagents.length > 0) {
      setConfirmEnable(true);
      return;
    }
    void setPluginEnabled(plugin.id, true);
  };

  const clone = async (subagent: string) => {
    try {
      const { warnings } = await api.subagentCloneFromPlugin(plugin.id, subagent);
      await refreshConfig();
      const note = warnings.length > 0 ? ` ${warnings.join("; ")}` : "";
      toast("success", `Cloned ${subagent} to your subagents.${note}`);
    } catch (err) {
      toast("error", `${err}`);
    }
  };

  return (
    <div className="rounded-2xl border border-slate-200 dark:border-slate-700">
      <div className="p-4">
        <div className="flex items-start gap-3">
          <label
            className="mt-1 cursor-pointer"
            title={plugin.enabled ? "Disable" : "Enable"}
          >
            <input
              type="checkbox"
              className="peer sr-only"
              checked={plugin.enabled}
              onChange={toggleEnabled}
            />
            <span className={switchClass} />
          </label>

          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-1.5">
              <span className="font-semibold">{plugin.name}</span>
              <span className={chipClass}>v{formatVersion(plugin.version)}</span>
              <span className={chipClass}>
                {plugin.layout === "claude-code" ? "Claude Code" : "Agent Plugins"}
              </span>
              <span className={chipClass}>{plugin.marketplace ?? "direct"}</span>
              <span className={`rounded-full px-2 py-0.5 text-[11px] ${toneClass[pluginStatusTone(plugin)]}`}>
                {pluginStatusLabel(plugin)}
              </span>
            </div>
            <div className="mt-0.5 text-xs text-slate-400">
              {plugin.available_update
                ? `v${formatVersion(plugin.version)} → v${formatVersion(plugin.available_update.version)}`
                : `Installed ${plugin.installed_at.slice(0, 10)}`}
            </div>
            {errorDiagnostics.map((diag, index) => (
              <p key={index} className="mt-1 text-xs text-rose-600 dark:text-rose-400">
                {diag.message}
              </p>
            ))}
            {progress && (
              <p className="mt-1 flex items-center gap-1.5 text-xs text-sky-600 dark:text-sky-400">
                <Icon name="spinner" className="h-3.5 w-3.5 animate-spin" />
                {progressPhaseLabel(progress.phase)} — {progress.detail}
              </p>
            )}
          </div>
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-1.5 pl-10">
          {plugin.available_update && (
            <Button onClick={() => void updatePlugin(plugin.id)}>Update</Button>
          )}
          {plugin.previous_version && (
            <Button variant="secondary" onClick={() => void rollbackPlugin(plugin.id)}>
              Roll back to v{formatVersion(plugin.previous_version)}
            </Button>
          )}
          <Button variant="secondary" onClick={onDetail}>
            Details
          </Button>
          <Button variant="secondary" onClick={() => void openPluginFolder(plugin.id)}>
            Open folder
          </Button>
          <select
            className={selectClass}
            value={plugin.update_policy}
            title="Update policy"
            onChange={(e) =>
              void setPluginUpdatePolicy(plugin.id, e.target.value as UpdatePolicy)
            }
          >
            <option value="auto">Auto update</option>
            <option value="manual">Manual update</option>
          </select>
          <Button variant="ghost" onClick={() => setConfirmUninstall(true)}>
            Uninstall
          </Button>
          <Button variant="ghost" onClick={() => setExpanded((open) => !open)}>
            <Icon
              name="chevron"
              className={`h-4 w-4 transition ${expanded ? "-rotate-90" : ""}`}
            />
            {expanded ? "Hide components" : "Components"}
          </Button>
        </div>
      </div>

      {expanded && (
        <div className="space-y-3 border-t border-slate-100 p-4 dark:border-slate-800">
          {skillRows.length > 0 && (
            <div>
              <h4 className={sectionTitle}>Skills</h4>
              <div className="mt-1.5 space-y-1.5">
                {skillRows.map((skill) => (
                  <div
                    key={skill.id}
                    className="rounded-xl border border-slate-200 p-3 dark:border-slate-700"
                  >
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="text-sm font-medium">{skill.name}</span>
                      <span className={chipClass}>{skill.origin}</span>
                      {skill.shadowed && (
                        <span className={chipClass}>Shadowed by {skill.shadowed}</span>
                      )}
                    </div>
                    <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
                      {skill.description}
                    </p>
                  </div>
                ))}
              </div>
            </div>
          )}

          {plugin.subagents.length > 0 && (
            <div>
              <h4 className={sectionTitle}>Subagents</h4>
              <div className="mt-1.5 space-y-1.5">
                {plugin.subagents.map((sub) => (
                  <div
                    key={sub.slug}
                    className="flex items-start justify-between gap-3 rounded-xl border border-slate-200 p-3 dark:border-slate-700"
                  >
                    <div className="min-w-0">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span className="text-sm font-medium">{sub.name}</span>
                        {pluginSubagentShadowed(userNames, sub.slug) && (
                          <span className={chipClass}>Shadowed by yours</span>
                        )}
                      </div>
                      <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
                        {sub.description}
                      </p>
                    </div>
                    <Button variant="secondary" onClick={() => void clone(sub.name)}>
                      Clone
                    </Button>
                  </div>
                ))}
              </div>
            </div>
          )}

          {plugin.servers.length > 0 && (
            <div>
              <h4 className={sectionTitle}>MCP servers</h4>
              <p className="mt-1 text-xs text-slate-400">
                Every server starts separately and stays off until you switch it
                on. Servers run the plugin's code with your account.
              </p>
              <div className="mt-1.5 space-y-1.5">
                {plugin.servers.map((server) => (
                  <label
                    key={server.id}
                    className={`flex items-start justify-between gap-3 rounded-xl border border-slate-200 p-3 dark:border-slate-700 ${
                      plugin.enabled ? "cursor-pointer" : "opacity-60"
                    }`}
                  >
                    <div className="min-w-0">
                      <div className="text-sm font-medium">{server.name}</div>
                      {/* command/args or the published host — nothing else */}
                      <code className="mt-0.5 block break-all text-[11px] text-slate-500 dark:text-slate-400">
                        {serverTrustLine(server)}
                      </code>
                    </div>
                    <input
                      type="checkbox"
                      className="mt-1"
                      checked={server.enabled}
                      disabled={!plugin.enabled}
                      title={plugin.enabled ? `Start ${server.name}` : "Enable the plugin first"}
                      onChange={() =>
                        void setPluginServerEnabled(
                          plugin.id,
                          server.name,
                          !server.enabled,
                        )
                      }
                    />
                  </label>
                ))}
              </div>
            </div>
          )}

          {plugin.diagnostics.length > 0 && (
            <div>
              <h4 className={sectionTitle}>Diagnostics</h4>
              <div className="mt-1.5 space-y-1 text-xs">
                {plugin.diagnostics.map((diag, index) => (
                  <div key={`${diag.target}:${index}`} className="flex gap-2">
                    <span className={`w-14 shrink-0 uppercase ${diagClass[diag.level]}`}>
                      {diag.level}
                    </span>
                    <span className="min-w-0 break-words">
                      <span className="font-medium">{diag.target}</span> — {diag.message}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {skillRows.length === 0 &&
            plugin.subagents.length === 0 &&
            plugin.servers.length === 0 &&
            plugin.diagnostics.length === 0 && (
              <p className="text-xs text-slate-400">This plugin has no components.</p>
            )}
        </div>
      )}

      <Modal
        open={confirmEnable}
        onClose={() => setConfirmEnable(false)}
        title={`Enable ${plugin.name}?`}
        subtitle="Enabling runs the plugin's code with your user account."
      >
        <div className="space-y-3">
          <p className="text-xs leading-relaxed text-slate-500 dark:text-slate-400">
            {trust.warning}
          </p>
          {trust.servers.length > 0 && (
            <div>
              <h4 className={sectionTitle}>MCP servers it can start</h4>
              <ul className="mt-1 space-y-1.5 text-xs">
                {trust.servers.map((server) => (
                  <li key={server.name}>
                    <span className="font-medium">{server.name}</span>
                    <code className="mt-0.5 block break-all text-[11px] text-slate-500 dark:text-slate-400">
                      {server.line}
                    </code>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {trust.subagents.length > 0 && (
            <div>
              <h4 className={sectionTitle}>Subagents it adds</h4>
              <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">
                {trust.subagents.join(", ")}
              </p>
            </div>
          )}
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={() => setConfirmEnable(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => {
                setConfirmEnable(false);
                void setPluginEnabled(plugin.id, true);
              }}
            >
              Enable plugin
            </Button>
          </div>
        </div>
      </Modal>

      <Modal
        open={confirmUninstall}
        onClose={() => setConfirmUninstall(false)}
        title={`Uninstall ${plugin.name}?`}
        subtitle={
          plugin.marketplace ? `Installed from ${plugin.marketplace}` : "Direct install"
        }
      >
        <div className="space-y-4">
          <p className="text-sm text-slate-600 dark:text-slate-300">
            Its package is deleted and its servers are disconnected. You can
            install it again later.
          </p>
          <label className="flex items-start gap-2 text-sm">
            <input
              type="checkbox"
              className="mt-0.5"
              checked={deleteData}
              onChange={(e) => setDeleteData(e.target.checked)}
            />
            <span>
              Also delete this plugin's data
              <span className="mt-0.5 block text-xs text-slate-400">
                Leave this off to keep `plugin-data` — a reinstall adopts it.
              </span>
            </span>
          </label>
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={() => setConfirmUninstall(false)}>
              Cancel
            </Button>
            <Button
              variant="danger"
              onClick={() => {
                setConfirmUninstall(false);
                void uninstallPlugin(plugin.id, deleteData);
              }}
            >
              Uninstall
            </Button>
          </div>
        </div>
      </Modal>
    </div>
  );
}

const chipClass =
  "rounded-full bg-slate-100 px-2 py-0.5 text-[11px] text-slate-500 dark:bg-slate-800 dark:text-slate-400";

const sectionTitle = "text-xs font-medium tracking-wide text-slate-400 uppercase";

const selectClass =
  "rounded-lg border border-slate-200 bg-white px-2.5 py-1.5 text-xs outline-none transition focus:border-sky-400 dark:border-slate-700 dark:bg-slate-800";

const switchClass =
  "relative block h-5.5 w-10 rounded-full bg-slate-300 transition peer-checked:bg-sky-500 dark:bg-slate-700 after:absolute after:left-0.5 after:top-0.5 after:h-4.5 after:w-4.5 after:rounded-full after:bg-white after:transition peer-checked:after:translate-x-4";

const toneClass: Record<PluginStatusTone, string> = {
  ok: "bg-emerald-50 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-300",
  warn: "bg-amber-50 text-amber-700 dark:bg-amber-950/50 dark:text-amber-300",
  error: "bg-rose-50 text-rose-700 dark:bg-rose-950/50 dark:text-rose-300",
  muted: "bg-slate-100 text-slate-500 dark:bg-slate-800 dark:text-slate-400",
};

const diagClass = {
  error: "text-rose-600 dark:text-rose-400",
  warning: "text-amber-600 dark:text-amber-400",
  info: "text-slate-500 dark:text-slate-400",
};
