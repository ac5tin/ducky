import { useEffect, useState } from "react";
import * as api from "../../api";
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
  const parts = [`${used.toLocaleString()} in`];
  if (usage.output != null) parts.push(`${usage.output.toLocaleString()} out`);

  return (
    <span
      className="tabular-nums text-xs text-slate-400 dark:text-slate-500"
      title={parts.join(" · ")}
      aria-label={
        limit == null
          ? `${label} tokens used`
          : `${label} of context window used`
      }
    >
      {label}
    </span>
  );
}
