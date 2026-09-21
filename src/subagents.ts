// Subagent (`ducky__subagent`) display helpers. Pure functions so the store
// can batch subagent deltas per animation frame like chat deltas.
import type { ChatItem } from "./store";

export const SUBAGENT_TOOL = "ducky__subagent";

/** Header identity for a subagent card: a name and a one-line brief. */
export interface SubagentHeader {
  name: string;
  brief: string;
}

/** Display name + concise brief for a spawn. `name`/`description` are
 * optional schema params; the name falls back to the configured `agent`
 * type's name, then "Subagent"; the brief falls back to the task's first
 * line. */
export function subagentHeader(args: unknown): SubagentHeader {
  const a = (args && typeof args === "object" ? args : {}) as {
    task?: unknown;
    name?: unknown;
    description?: unknown;
    agent?: unknown;
  };
  const name =
    typeof a.name === "string" && a.name.trim()
      ? a.name.trim()
      : typeof a.agent === "string" && a.agent.trim()
        ? a.agent.trim()
        : "Subagent";
  let brief = "";
  if (typeof a.description === "string" && a.description.trim()) {
    brief = a.description.trim();
  } else if (typeof a.task === "string" && a.task.trim()) {
    brief = a.task.trim().split("\n")[0] ?? "";
  }
  if (brief.length > 80) brief = `${brief.slice(0, 79)}…`;
  return { name, brief };
}

/** The full task text delegated to the subagent, if parseable. */
export function subagentTask(args: unknown): string {
  if (args && typeof args === "object" && "task" in args) {
    const task = (args as { task?: unknown }).task;
    if (typeof task === "string") return task;
  }
  return "";
}

/** Which configured agent type a spawn resolved to, for card labeling.
 * Live meta is authoritative (a present meta with no agent = generic);
 * replayed or pre-approval cards fall back to the raw `agent` argument. */
export function subagentType(
  meta: { agent?: string | null } | null | undefined,
  args: unknown,
): string {
  if (meta) return meta.agent ?? "Generic";
  const a = (args && typeof args === "object" ? args : {}) as {
    agent?: unknown;
  };
  return typeof a.agent === "string" && a.agent.trim()
    ? a.agent.trim()
    : "Generic";
}

/** Activity line for a tool the subagent itself is running. */
export function subagentActivityLabel(
  tool: string | undefined,
  status: string,
): string {
  return `${tool ?? "tool"} · ${status}`;
}

/** Append `deltas` (tool_call_id → text chunk) onto the matching tool items'
 * `subagent_text`. Returns the input array unchanged when nothing applies, so
 * callers skip needless store updates. */
export function applySubagentDeltas(
  items: ChatItem[],
  deltas: Record<string, string>,
): ChatItem[] {
  const ids = Object.keys(deltas);
  if (ids.length === 0) return items;
  let changed = false;
  const next = items.map((item) => {
    if (item.kind !== "tool" || deltas[item.id] === undefined) return item;
    changed = true;
    return {
      ...item,
      state: {
        ...item.state,
        subagent_text: (item.state.subagent_text ?? "") + deltas[item.id],
      },
    };
  });
  return changed ? next : items;
}
