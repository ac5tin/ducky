// Subagent (`ducky__subagent`) display helpers. Pure functions so the store
// can batch subagent deltas per animation frame like chat deltas.
import type { ChatItem } from "./store";

export const SUBAGENT_TOOL = "ducky__subagent";

/** One-line task summary for the subagent card header. */
export function subagentTaskSnippet(args: unknown): string {
  if (args && typeof args === "object" && "task" in args) {
    const task = (args as { task?: unknown }).task;
    if (typeof task === "string" && task.trim()) {
      const line = task.trim().split("\n")[0] ?? "";
      return line.length > 80 ? `${line.slice(0, 79)}…` : line;
    }
  }
  return "";
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
