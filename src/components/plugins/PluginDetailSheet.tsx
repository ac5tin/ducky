import { useEffect, useMemo, useState, type ReactNode } from "react";
import { useStore } from "../../store";
import * as api from "../../api";
import type { CatalogEntry, PluginDetail } from "../../types";
import {
  INSTALL_TRUST_WARNING,
  catalogEntryAction,
  formatVersion,
  pluginSkillRows,
  serverTrustLine,
  sourceLabel,
} from "../../plugins";
import { pluginSubagentShadowed } from "../../subagents";
import { Button, Modal } from "../modals/Modal";
import { Icon } from "../icons";

/**
 * One plugin, full detail. Opened from a Discover card (`entry`) or an
 * installed row (`pluginId`). An installed id is loaded through
 * `loadPluginDetail`: a rejection renders the error, and only a genuine
 * `null` renders "not found" — the two must never swap.
 */
export function PluginDetailSheet({
  entry,
  pluginId,
  onClose,
}: {
  entry: CatalogEntry | null;
  pluginId: string | null;
  onClose: () => void;
}) {
  const loadPluginDetail = useStore((s) => s.loadPluginDetail);
  const installPlugin = useStore((s) => s.installPlugin);
  const updatePlugin = useStore((s) => s.updatePlugin);
  const skills = useStore((s) => s.skills);
  const config = useStore((s) => s.config);
  const refreshConfig = useStore((s) => s.refreshConfig);
  const toast = useStore((s) => s.toast);

  const [installedId, setInstalledId] = useState<string | null>(null);
  const [detail, setDetail] = useState<PluginDetail | null | undefined>(
    undefined,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const id = installedId ?? pluginId;
  const open = Boolean(entry || id);

  useEffect(() => {
    if (!open) {
      setInstalledId(null);
      setDetail(undefined);
      setError(null);
      return;
    }
    if (!id) {
      setDetail(undefined);
      setError(null);
      return;
    }
    let live = true;
    setDetail(undefined);
    setError(null);
    loadPluginDetail(id)
      .then((loaded) => {
        if (live) setDetail(loaded);
      })
      .catch((err) => {
        // the store already toasted it; show it here too, never "not found"
        if (live) setError(String(err));
      });
    return () => {
      live = false;
    };
  }, [open, id, loadPluginDetail]);

  const skillRows = useMemo(
    () => (detail ? pluginSkillRows(detail, skills) : []),
    [detail, skills],
  );
  const userNames = useMemo(
    () => (config?.subagents ?? []).map((sub) => sub.name),
    [config],
  );

  const runEntryAction = async () => {
    if (!entry) return;
    setBusy(true);
    try {
      if (entry.update_available && entry.installed) {
        await updatePlugin(entry.installed);
        setInstalledId(entry.installed);
        return;
      }
      const installed = await installPlugin({
        marketplace: entry.marketplace_id,
        name: entry.name,
      });
      if (installed) setInstalledId(installed.id);
    } finally {
      setBusy(false);
    }
  };

  const clone = async (subagent: string) => {
    if (!detail) return;
    try {
      const { warnings } = await api.subagentCloneFromPlugin(detail.id, subagent);
      await refreshConfig();
      const note = warnings.length > 0 ? ` ${warnings.join("; ")}` : "";
      toast("success", `Cloned ${subagent} to your subagents.${note}`);
    } catch (err) {
      toast("error", `${err}`);
    }
  };

  // the entry action is only for a plugin that is not installed yet; once a
  // detail is loaded, the detail's own update state drives the footer
  const action = entry && !id ? catalogEntryAction(entry, busy) : null;
  const name = detail?.name ?? entry?.display_name ?? entry?.name ?? "";
  const warning = detail?.trust_warning ?? INSTALL_TRUST_WARNING;

  return (
    <Modal
      open={open}
      onClose={onClose}
      wide
      title={name}
      subtitle={detail?.description ?? entry?.description ?? undefined}
    >
      {error ? (
        <div className="rounded-xl border border-rose-200 bg-rose-50 p-4 dark:border-rose-900 dark:bg-rose-950/40">
          <p className="text-sm font-medium text-rose-700 dark:text-rose-300">
            Could not load this plugin
          </p>
          <p className="mt-1 text-xs text-rose-600 dark:text-rose-400">{error}</p>
        </div>
      ) : id && detail === undefined ? (
        <p className="text-sm text-slate-400">Loading…</p>
      ) : id && detail === null ? (
        <p className="text-sm text-slate-500 dark:text-slate-400">
          This plugin is no longer installed.
        </p>
      ) : (
        <div className="space-y-5">
          {/* spec §6: the trust posture travels with the plugin */}
          <div className="flex items-start gap-2 rounded-xl border border-amber-200 bg-amber-50 p-3.5 text-xs leading-relaxed text-amber-800 dark:border-amber-900 dark:bg-amber-950/40 dark:text-amber-200">
            <Icon name="warning" className="mt-0.5 h-4 w-4 shrink-0" />
            <span>{warning}</span>
          </div>

          <div className="grid gap-x-6 gap-y-2 text-xs sm:grid-cols-2">
            <Meta label="Version" value={formatVersion(detail?.version ?? entry?.version)} />
            <Meta
              label="Source"
              value={
                detail ? sourceLabel(detail.source) : (entry?.source_kind ?? "unknown")
              }
            />
            {detail?.resolved_sha && (
              <Meta label="Revision" value={detail.resolved_sha} />
            )}
            <Meta label="Author" value={detail?.author ?? entry?.author} />
            {detail?.license && <Meta label="License" value={detail.license} />}
            <Meta label="Homepage" value={detail?.homepage ?? entry?.homepage} />
            {detail && <Meta label="Layout" value={detail.layout} />}
          </div>

          {skillRows.length > 0 && (
            <Section title="Skills">
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
            </Section>
          )}

          {detail && detail.subagents.length > 0 && (
            <Section title="Subagents">
              {detail.subagents.map((sub) => (
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
                    Clone to my subagents
                  </Button>
                </div>
              ))}
            </Section>
          )}

          {detail && detail.servers.length > 0 && (
            <Section title="MCP servers">
              {detail.servers.map((server) => (
                <div
                  key={server.id}
                  className="rounded-xl border border-slate-200 p-3 dark:border-slate-700"
                >
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="text-sm font-medium">{server.name}</span>
                    <span className={chipClass}>
                      {server.enabled ? "Consented" : "Off"}
                    </span>
                  </div>
                  {/* command/args or the published host — nothing else is sent */}
                  <code className="mt-1 block break-all text-[11px] text-slate-500 dark:text-slate-400">
                    {serverTrustLine(server)}
                  </code>
                </div>
              ))}
            </Section>
          )}

          {detail && detail.diagnostics.length > 0 && (
            <Section title="Diagnostics">
              {detail.diagnostics.map((diag, index) => (
                <div key={`${diag.target}:${index}`} className="flex gap-2 text-xs">
                  <span className={`w-14 shrink-0 uppercase ${diagClass[diag.level]}`}>
                    {diag.level}
                  </span>
                  <span className="min-w-0 break-words">
                    <span className="font-medium">{diag.target}</span> — {diag.message}
                  </span>
                </div>
              ))}
            </Section>
          )}

          {action && action.kind !== "installed" && (
            <div className="flex justify-end">
              <Button
                disabled={action.disabled}
                title={action.hint ?? undefined}
                onClick={() => void runEntryAction()}
              >
                {action.label}
              </Button>
            </div>
          )}
          {detail?.available_update && (
            <div className="flex justify-end">
              <Button disabled={busy} onClick={() => void updatePlugin(detail.id)}>
                Update to {formatVersion(detail.available_update.version)}
              </Button>
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}

function Meta({ label, value }: { label: string; value: string | null | undefined }) {
  if (!value) return null;
  return (
    <div className="flex min-w-0 gap-2">
      <span className="w-16 shrink-0 text-slate-400">{label}</span>
      <span className="min-w-0 break-all text-slate-600 dark:text-slate-300">
        {value}
      </span>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h3 className="text-xs font-medium tracking-wide text-slate-400 uppercase">
        {title}
      </h3>
      <div className="mt-2 space-y-2">{children}</div>
    </section>
  );
}

const chipClass =
  "rounded-full bg-slate-100 px-2 py-0.5 text-[11px] text-slate-500 dark:bg-slate-800 dark:text-slate-400";

const diagClass = {
  error: "text-rose-600 dark:text-rose-400",
  warning: "text-amber-600 dark:text-amber-400",
  info: "text-slate-500 dark:text-slate-400",
};
