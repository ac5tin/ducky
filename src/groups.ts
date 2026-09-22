// Pure chat-group view and drag-layout logic. No React, no Tauri imports.
import type { ChatGroup, ConversationMeta, GroupLayout } from "./types";

/** The five preset group colours, at the Tailwind 500 weight. Mirrors the
 *  `GROUP_COLORS` constant in `src-tauri/src/config.rs`. */
export const GROUP_COLORS = ["#64748b", "#ef4444", "#f59e0b", "#22c55e", "#0ea5e9"] as const;

export const DEFAULT_GROUP_COLOR: string = GROUP_COLORS[0];

/** Lowercase `#rrggbb`, or the gray preset when the value is not a hex colour.
 *  A hand-edited config must never break the dot. */
export function normalizeGroupColor(color: string | null | undefined): string {
  const raw = (color ?? "").trim();
  if (!raw.startsWith("#")) return DEFAULT_GROUP_COLOR;
  const hex = raw.slice(1);
  const expanded = hex.length === 3 ? [...hex].map((c) => c + c).join("") : hex;
  if (!/^[0-9a-fA-F]{6}$/.test(expanded)) return DEFAULT_GROUP_COLOR;
  return `#${expanded.toLowerCase()}`;
}

/** DOM id of a group's body, so a header can point `aria-controls` at it. */
export const groupBodyId = (groupId: string) => `group-body-${groupId}`;

export interface GroupNode {
  group: ChatGroup;
  chats: ConversationMeta[];
}

export interface SidebarView {
  /** Groups in their stored order, each with the chats it lists. */
  groups: GroupNode[];
  /** Chats no group lists, most recent first. */
  ungrouped: ConversationMeta[];
}

export function buildSidebarView(
  conversations: ConversationMeta[],
  groups: ChatGroup[],
): SidebarView {
  const byId = new Map(conversations.map((c) => [c.id, c]));
  const claimed = new Set<string>();
  const nodes = groups.map((group) => {
    const chats: ConversationMeta[] = [];
    for (const id of group.conversation_ids) {
      const chat = byId.get(id);
      if (!chat || claimed.has(id)) continue; // gone, or listed twice
      claimed.add(id);
      chats.push(chat);
    }
    return { group, chats };
  });
  const ungrouped = conversations
    .filter((c) => !claimed.has(c.id))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  return { groups: nodes, ungrouped };
}

// ---------------------------------------------------------------------------
// Drag and drop identifiers
// ---------------------------------------------------------------------------

export type DropTarget =
  | { kind: "chat"; conversationId: string }
  | { kind: "group-header"; groupId: string }
  | { kind: "group-footer"; groupId: string }
  | { kind: "group-empty"; groupId: string }
  | { kind: "group-over"; groupId: string };

export interface DragRef {
  kind: "chat" | "group";
  id: string;
}

/** `before` means the pointer moved up. */
export type DragDirection = "before" | "after";

export const chatDragId = (conversationId: string) => `drag-chat:${conversationId}`;
export const groupDragId = (groupId: string) => `drag-group:${groupId}`;

export const chatZone = (conversationId: string) => `chat:${conversationId}`;
export const groupHeaderZone = (groupId: string) => `group-header:${groupId}`;
export const groupFooterZone = (groupId: string) => `group-footer:${groupId}`;
export const groupEmptyZone = (groupId: string) => `group-empty:${groupId}`;
export const groupOverZone = (groupId: string) => `group-over:${groupId}`;

/** Splits a droppable id on its first colon, so a group id may contain one. */
function splitId(id: string): [string, string] | null {
  const at = id.indexOf(":");
  return at < 0 ? null : [id.slice(0, at), id.slice(at + 1)];
}

export function parseZone(id: string): DropTarget | null {
  const parts = splitId(id);
  if (!parts) return null;
  const [prefix, rest] = parts;
  switch (prefix) {
    case "chat":
      return { kind: "chat", conversationId: rest };
    case "group-header":
      return { kind: "group-header", groupId: rest };
    case "group-footer":
      return { kind: "group-footer", groupId: rest };
    case "group-empty":
      return { kind: "group-empty", groupId: rest };
    case "group-over":
      return { kind: "group-over", groupId: rest };
    default:
      return null;
  }
}

export function parseDragId(id: string): DragRef | null {
  const parts = splitId(id);
  if (!parts) return null;
  const [prefix, rest] = parts;
  if (prefix === "drag-chat") return { kind: "chat", id: rest };
  if (prefix === "drag-group") return { kind: "group", id: rest };
  return null;
}

// ---------------------------------------------------------------------------
// Drop layout
// ---------------------------------------------------------------------------

function cloneGroups(groups: ChatGroup[]): ChatGroup[] {
  return groups.map((g) => ({ ...g, conversation_ids: [...g.conversation_ids] }));
}

/** Drops the chat from every group without touching anything else. */
function forget(groups: ChatGroup[], conversationId: string): void {
  for (const group of groups) {
    group.conversation_ids = group.conversation_ids.filter((c) => c !== conversationId);
  }
}

/** Removes the chat from wherever it is and inserts it at `index` of `groupId`
 *  (`null` index = the end, `null` group = ungrouped). */
function moveConversation(
  groups: ChatGroup[],
  conversationId: string,
  groupId: string | null,
  index: number | null,
): ChatGroup[] {
  forget(groups, conversationId);
  if (groupId === null) return groups;
  const group = groups.find((g) => g.id === groupId);
  if (!group) return groups;
  group.conversation_ids.splice(index ?? group.conversation_ids.length, 0, conversationId);
  return groups;
}

/** Moves a whole group next to `anchorGroupId` (`null` = the end of the list). */
function moveGroup(
  groups: ChatGroup[],
  activeGroupId: string,
  anchorGroupId: string | null,
  after: boolean,
): ChatGroup[] {
  if (anchorGroupId === activeGroupId) return groups; // dropped on itself
  const from = groups.findIndex((g) => g.id === activeGroupId);
  if (from < 0) return groups;
  const [active] = groups.splice(from, 1);
  const anchor = anchorGroupId === null ? -1 : groups.findIndex((g) => g.id === anchorGroupId);
  groups.splice(anchor < 0 ? groups.length : anchor + (after ? 1 : 0), 0, active);
  return groups;
}

function dropChat(
  groups: ChatGroup[],
  conversationId: string,
  target: DropTarget,
  direction: DragDirection,
): ChatGroup[] {
  switch (target.kind) {
    case "chat": {
      if (target.conversationId === conversationId) return groups; // dropped on itself
      // Forget first: the target's index is only final once the dragged chat
      // has left the list it might share with that target.
      forget(groups, conversationId);
      const owner = groups.find((g) => g.conversation_ids.includes(target.conversationId));
      if (!owner) return groups; // ungrouped: it stays out
      const at =
        owner.conversation_ids.indexOf(target.conversationId) + (direction === "after" ? 1 : 0);
      owner.conversation_ids.splice(at, 0, conversationId);
      return groups;
    }
    case "group-header": {
      const group = groups.find((g) => g.id === target.groupId);
      // Moving up over a header, or any move over a collapsed group, ungroups.
      if (!group || group.collapsed || direction === "before") {
        return moveConversation(groups, conversationId, null, null);
      }
      return moveConversation(groups, conversationId, group.id, 0);
    }
    case "group-footer":
      // Moving up over the footer means the end of the group; down means out.
      return direction === "before"
        ? moveConversation(groups, conversationId, target.groupId, null)
        : moveConversation(groups, conversationId, null, null);
    case "group-empty":
      return moveConversation(groups, conversationId, target.groupId, 0);
    case "group-over":
      return groups; // not a chat target
  }
}

function dropGroup(
  groups: ChatGroup[],
  activeGroupId: string,
  target: DropTarget,
  direction: DragDirection,
): ChatGroup[] {
  const after = direction === "after";
  switch (target.kind) {
    case "group-over":
      return moveGroup(groups, activeGroupId, target.groupId, after);
    case "chat": {
      // A chat inside a group means "next to that chat's group". An ungrouped
      // chat is not a position in the group list, so the group goes to the end.
      const owner = groups.find((g) => g.conversation_ids.includes(target.conversationId));
      return moveGroup(groups, activeGroupId, owner?.id ?? null, after);
    }
    default:
      return groups; // headers, footers and empty zones are not group targets
  }
}

/** The next group layout for a drop. Returns the input layout with `groups` in
 *  the new order when the target does not apply, so the caller's signature
 *  check turns it into a no-op. */
export function computeDropLayout(
  view: SidebarView,
  active: DragRef,
  target: DropTarget,
  direction: DragDirection,
): ChatGroup[] {
  const groups = cloneGroups(view.groups.map((node) => node.group));
  return active.kind === "chat"
    ? dropChat(groups, active.id, target, direction)
    : dropGroup(groups, active.id, target, direction);
}

/** Order and membership only — the payload `group_apply_layout` takes. */
export function toLayout(groups: ChatGroup[]): GroupLayout[] {
  return groups.map((g) => ({ id: g.id, conversation_ids: [...g.conversation_ids] }));
}

/** Cheap "did this drag change anything" comparison. */
export function layoutSignature(groups: ChatGroup[]): string {
  return JSON.stringify(toLayout(groups));
}