/** Collapse an explicit allow-list to `null` when it matches every globally enabled id. */
function collapse(
  set: Set<string>,
  globallyEnabled: string[],
): string[] | null {
  const next = globallyEnabled.filter((id) => set.has(id));
  if (next.length === globallyEnabled.length) return null;
  return next;
}

/** Next per-chat allow-list after toggling one globally enabled connector. `null` = all on. */
export function nextMcpIds(
  current: string[] | null | undefined,
  globallyEnabled: string[],
  id: string,
  on: boolean,
): string[] | null {
  const set = new Set(current ?? globallyEnabled);
  if (on) set.add(id);
  else set.delete(id);
  return collapse(set, globallyEnabled);
}

/** `null` = all on, `[]` = all off. */
export function allMcpIds(
  _globallyEnabled: string[],
  on: boolean,
): string[] | null {
  return on ? null : [];
}
