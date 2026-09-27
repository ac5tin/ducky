import { useEffect, useState } from "react";
import * as api from "../../api";
import { Modal } from "../modals/Modal";
import { useStore } from "../../store";

const limitCache = new Map<string, number | null>();

function useContextLimit(
  kind: string | undefined,
  model: string | undefined,
): number | null {
  const [limit, setLimit] = useState<number | null>(null);

  useEffect(() => {
    if (!kind || !model) {
      setLimit(null);
      return;
    }
    const key = `${kind}/${model}`;
    if (limitCache.has(key)) {
      setLimit(limitCache.get(key) ?? null);
      return;
    }
    let cancelled = false;
    api
      .contextLimit(kind, model)
      .then((n) => {
        limitCache.set(key, n);
        if (!cancelled) setLimit(n);
      })
      .catch(() => {
        if (!cancelled) setLimit(null);
      });
    return () => {
      cancelled = true;
    };
  }, [kind, model]);

  return limit;
}

function compactTokens(n: number): string {
  if (n >= 1_000_000) {
    const v = n / 1_000_000;
    return `${Number.isInteger(v) ? v.toFixed(0) : v.toFixed(1)}M`;
  }
  if (n >= 1000) {
    const v = n / 1000;
    return `${Number.isInteger(v) ? v.toFixed(0) : v.toFixed(1)}k`;
  }
  return String(n);
}

function usedPercent(used: number, limit: number): number {
  if (limit <= 0) return 0;
  return Math.round((used / limit) * 100);
}

export function TokenMeter() {
  const [open, setOpen] = useState(false);
  const activeId = useStore((s) => s.activeConversationId);
  const usage = useStore((s) =>
    s.activeConversationId
      ? s.usageByConversation[s.activeConversationId]
      : undefined,
  );
  const config = useStore((s) => s.config);
  const provider = useStore((s) => s.activeProvider());
  const conversation = config?.conversations.find((c) => c.id === activeId);
  const model =
    conversation?.model || provider?.default_model || provider?.models[0] || "";
  const used = usage?.input;
  const limit = useContextLimit(
    used != null ? provider?.kind : undefined,
    model,
  );

  if (used == null || !usage) return null;
  const label =
    limit == null
      ? compactTokens(used)
      : `${compactTokens(used)} / ${compactTokens(limit)} (${usedPercent(used, limit)}%)`;
  const cachedPct =
    usage.cached && used > 0 ? Math.round((usage.cached / used) * 100) : null;
  const parts = [`${used.toLocaleString()} in`];
  if (cachedPct != null) parts[0] += ` (${cachedPct}% cached)`;
  if (usage.output != null) parts.push(`${usage.output.toLocaleString()} out`);

  return (
    <>
      <button
        type="button"
        className="cursor-pointer rounded-sm tabular-nums text-xs text-slate-400 transition hover:text-slate-600 focus:outline-none focus-visible:ring-2 focus-visible:ring-slate-400 dark:text-slate-500 dark:hover:text-slate-300 dark:focus-visible:ring-slate-500"
        title={parts.join(" · ")}
        aria-label={
          limit == null
            ? `${label} tokens used, open token usage details`
            : `${label} of context window used, open token usage details`
        }
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        {label}
      </button>
      <TokenUsageDialog
        open={open}
        onClose={() => setOpen(false)}
        usage={usage}
        limit={limit}
        model={model}
        cachedPct={cachedPct}
      />
    </>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-4 py-1.5">
      <dt className="text-sm text-slate-500 dark:text-slate-400">{label}</dt>
      <dd className="tabular-nums text-sm font-medium">{value}</dd>
    </div>
  );
}

function TokenUsageDialog({
  open,
  onClose,
  usage,
  limit,
  model,
  cachedPct,
}: {
  open: boolean;
  onClose: () => void;
  usage: { input?: number; output?: number; cached?: number };
  limit: number | null;
  model: string;
  cachedPct: number | null;
}) {
  const input = usage.input ?? 0;
  const pct = limit != null ? usedPercent(input, limit) : null;
  return (
    <Modal open={open} onClose={onClose} title="Token usage" subtitle={`Last model call · ${model}`}>
      <dl>
        <Row label="Input tokens" value={input.toLocaleString()} />
        {cachedPct != null && (
          <>
            <Row label={`  Cached (${cachedPct}%)`} value={(usage.cached ?? 0).toLocaleString()} />
            <Row label="  Non-cached" value={(input - (usage.cached ?? 0)).toLocaleString()} />
          </>
        )}
        <Row label="Output tokens" value={(usage.output ?? 0).toLocaleString()} />
        {limit != null && (
          <Row label="Context window" value={limit.toLocaleString()} />
        )}
      </dl>
      {pct != null && (
        <div className="mt-4">
          <div className="mb-1 flex items-baseline justify-between gap-4">
            <span className="text-sm text-slate-500 dark:text-slate-400">Used</span>
            <span className="tabular-nums text-sm font-medium">{pct}%</span>
          </div>
          <div
            className="h-1.5 w-full overflow-hidden rounded-full bg-slate-200 dark:bg-slate-700"
            role="progressbar"
            aria-valuenow={pct}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-label="Context window used"
          >
            <div
              className="h-full rounded-full bg-slate-500 dark:bg-slate-400"
              style={{ width: `${Math.min(pct, 100)}%` }}
            />
          </div>
        </div>
      )}
    </Modal>
  );
}
