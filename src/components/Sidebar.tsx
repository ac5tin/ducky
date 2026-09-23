import {
  DndContext,
  DragOverlay,
  PointerSensor,
  closestCenter,
  useDroppable,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import type {
  CollisionDetection,
  DragEndEvent,
  DragMoveEvent,
  DragOverEvent,
  DragStartEvent,
} from "@dnd-kit/core";
import { useCallback, useMemo, useRef, useState } from "react";
import {
  buildSidebarView,
  chatZone,
  computeDropLayout,
  dropLandsInGroup,
  groupBodyId,
  groupEmptyZone,
  groupFooterZone,
  layoutSignature,
  normalizeGroupColor,
  parseDragId,
  parseZone,
} from "../groups";
import type { DragDirection, DragRef } from "../groups";
import { useStore } from "../store";
import type { ChatGroup } from "../types";
import { Icon } from "./icons";
import { ChatRow } from "./sidebar/ChatRow";
import { GroupHeader } from "./sidebar/GroupHeader";

/** The thin strip after a group's last row: drop here moving up to append to
 *  the group, moving down to leave it. */
function GroupFooterStrip({ group, dropInside }: { group: ChatGroup; dropInside: boolean }) {
  const footer = useDroppable({ id: groupFooterZone(group.id), data: { kind: "group-footer" } });
  return (
    <div ref={footer.setNodeRef} className="h-2.5">
      <div
        className={`h-0.5 rounded-full transition ${
          footer.isOver && dropInside ? "bg-sky-400" : "bg-transparent"
        }`}
      />
    </div>
  );
}

/** Shown by an empty group. Dropping here fills the group; clicking starts a
 *  chat in it. */
function GroupEmptyZone({ group }: { group: ChatGroup }) {
  const empty = useDroppable({ id: groupEmptyZone(group.id), data: { kind: "group-empty" } });
  const newConversationInGroup = useStore((s) => s.newConversationInGroup);
  return (
    <button
      ref={empty.setNodeRef}
      className={`w-full rounded-lg border border-dashed px-2 py-1.5 text-xs transition ${
        empty.isOver
          ? "border-sky-400 text-sky-600"
          : "border-slate-300 text-slate-400 hover:border-sky-400 hover:text-sky-600 dark:border-slate-700"
      }`}
      onClick={() => newConversationInGroup(group.id).catch((e) => console.error(e))}
    >
      New chat, or drag here.
    </button>
  );
}

export function Sidebar() {
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const config = useStore((s) => s.config);
  const newConversation = useStore((s) => s.newConversation);
  const createGroup = useStore((s) => s.createGroup);
  const setAllGroupsCollapsed = useStore((s) => s.setAllGroupsCollapsed);
  const version = useStore((s) => s.version);
  const [renameAfterCreate, setRenameAfterCreate] = useState<string | null>(null);

  const groups = useMemo(() => config?.groups ?? [], [config]);
  // One collapse/expand-all button instead of two: with any group open the
  // useful action is "collapse", and with every group shut it is "expand".
  const anyExpanded = groups.some((g) => !g.collapsed);
  const conversations = useMemo(() => config?.conversations ?? [], [config]);
  const sidebarView = useMemo(
    () => buildSidebarView(conversations, groups),
    [conversations, groups],
  );

  const handleCreateGroup = () => {
    createGroup()
      .then((id) => {
        if (id) setRenameAfterCreate(id);
      })
      .catch((e) => console.error(e));
  };

  const applyGroupLayout = useStore((s) => s.applyGroupLayout);
  const setGroupCollapsed = useStore((s) => s.setGroupCollapsed);

  const [activeDrag, setActiveDrag] = useState<DragRef | null>(null);
  const [overId, setOverId] = useState<string | null>(null);
  // The direction is held twice on purpose. The ref is what the drop maths
  // reads, so a late event sees the current value and never a stale render's;
  // the state is what the highlight renders from. It is written only when the
  // sign flips, so a drag costs a couple of renders — a per-move write is what
  // used to put the app in a render loop.
  const directionRef = useRef<DragDirection>("after");
  const [direction, setDirection] = useState<DragDirection>("after");
  const dragRef = useRef<DragRef | null>(null);

  const activeChat =
    activeDrag?.kind === "chat"
      ? conversations.find((c) => c.id === activeDrag.id) ?? null
      : null;
  const activeGroupNode =
    activeDrag?.kind === "group"
      ? sidebarView.groups.find((n) => n.group.id === activeDrag.id)
      : undefined;

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 6 } }));

  /** The group this drop would move the dragged chat into, or null when it would
   *  leave the chat out of every group or change nothing. The group header, the
   *  footer strip and the empty zone are gated on it, so a ring can never promise
   *  a move the drop will not make. A chat row rings on dnd-kit's own `isOver`
   *  instead: dropping on a row really does act on that row, except when the drop
   *  is a no-op. */
  const ringGroupId = useMemo(() => {
    if (!activeDrag || !overId) return null;
    const target = parseZone(overId);
    return target ? dropLandsInGroup(sidebarView, activeDrag, target, direction) : null;
  }, [activeDrag, overId, sidebarView, direction]);

  // Stable identity matters here: a new function every render makes dnd-kit
  // recompute collisions on each render, which feeds the update loop below.
  const collisionDetection: CollisionDetection = useCallback((args) => {
    const drag = dragRef.current;
    const containers = args.droppableContainers.filter((container) => {
      const id = String(container.id);
      // The dragged row is not a target: dropping a chat on itself does
      // nothing, so it must not light up as one.
      if (drag?.kind === "chat" && id === chatZone(drag.id)) return false;
      const kind = parseZone(id)?.kind;
      return drag?.kind === "group" ? kind === "group-over" || kind === "chat" : kind !== "group-over";
    });
    return closestCenter({ ...args, droppableContainers: containers });
  }, []);

  /** The layout this drop would produce, computed once at drop time from the
   *  layout on screen.
   *
   *  The list is deliberately NOT re-ordered while the drag runs. Moving rows
   *  mid-drag moves the drop zones under a stationary pointer, so the collision
   *  test flips between two zones and the preview flips with it, mounting and
   *  unmounting rows forever — React aborts that loop by unmounting the app.
   *  A highlight on the target zone (see `isOver` in the rows and headers) is
   *  the drag feedback instead. */
  const layoutFor = (drag: DragRef, overId: string | null): ChatGroup[] | null => {
    const target = overId ? parseZone(overId) : null;
    if (!target) return null;
    return computeDropLayout(sidebarView, drag, target, directionRef.current);
  };

  const onDragStart = (event: DragStartEvent) => {
    const drag = parseDragId(String(event.active.id));
    if (!drag) return;
    directionRef.current = "after";
    setDirection("after");
    setOverId(null);
    dragRef.current = drag;
    setActiveDrag(drag);
  };

  // dnd-kit fires this only when the target changes, so it is a cheap write.
  const onDragOver = (event: DragOverEvent) => {
    setOverId(event.over ? String(event.over.id) : null);
  };

  const onDragMove = (event: DragMoveEvent) => {
    const next: DragDirection = event.delta.y < 0 ? "before" : "after";
    if (next === directionRef.current) return;
    directionRef.current = next;
    setDirection(next);
  };

  const onDragEnd = (event: DragEndEvent) => {
    const drag = dragRef.current;
    const droppedOn = event.over ? String(event.over.id) : null;
    const target = droppedOn ? parseZone(droppedOn) : null;
    const next = drag ? layoutFor(drag, droppedOn) : null;
    dragRef.current = null;
    setActiveDrag(null);
    setOverId(null);
    if (!drag || !next) return;
    // A drop that changed nothing must not write to the config.
    const before = sidebarView.groups.map((node) => node.group);
    if (layoutSignature(next) === layoutSignature(before)) return;
    // A collapsed group shows no rows, so a chat dropped into it would look
    // like nothing happened: open the group it landed in. That write is
    // sequenced after the layout write, because each of these commands rewrites
    // the whole config file — fired together they can land out of order (the
    // view then disagrees with the file) or interleave inside the file.
    const openGroupId =
      target?.kind === "group-header" &&
      sidebarView.groups.find((node) => node.group.id === target.groupId)?.group.collapsed
        ? target.groupId
        : null;
    applyGroupLayout(next)
      .then(() => (openGroupId ? setGroupCollapsed(openGroupId, false) : undefined))
      .catch((e) => console.error(e));
  };

  const onDragCancel = () => {
    dragRef.current = null;
    setActiveDrag(null);
    setOverId(null);
  };

  const sectionButton =
    "rounded p-1 text-slate-400 transition hover:bg-slate-200 hover:text-slate-700 disabled:opacity-40 dark:hover:bg-slate-800 dark:hover:text-slate-200";

  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-slate-200 bg-slate-50 dark:border-slate-800 dark:bg-slate-900/60">
      <div className="flex items-center gap-2 px-4 pt-4">
        <span className="flex h-9 w-9 items-center justify-center rounded-xl bg-sky-500 text-white shadow-sm">
          <Icon name="duck" className="h-5.5 w-5.5" />
        </span>
        <div>
          <div className="font-semibold leading-tight">Ducky</div>
          <div className="text-[11px] text-slate-400">MCP client</div>
        </div>
      </div>

      <div className="px-3 pt-4">
        <button
          className="flex w-full items-center justify-center gap-2 rounded-xl bg-sky-600 px-3 py-2.5 text-sm font-medium text-white shadow-sm transition hover:bg-sky-500"
          onClick={() => newConversation().catch((e) => console.error(e))}
        >
          <Icon name="plus" className="h-4 w-4" />
          New chat
        </button>
      </div>

      <DndContext
        sensors={sensors}
        collisionDetection={collisionDetection}
        autoScroll={{ threshold: { x: 0.1, y: 0.1 } }}
        onDragStart={onDragStart}
        onDragOver={onDragOver}
        onDragMove={onDragMove}
        onDragEnd={onDragEnd}
        onDragCancel={onDragCancel}
      >
      <div className="mt-4 min-h-0 flex-1 overflow-y-auto px-2">
        <div className="flex items-center justify-between px-2 pb-1">
          <span className="text-[11px] font-semibold uppercase tracking-wider text-slate-400">
            Chats
          </span>
          <div className="flex items-center gap-0.5">
            <button
              className={sectionButton}
              aria-label="New group"
              title="New group"
              onClick={handleCreateGroup}
            >
              <Icon name="plus" className="h-3.5 w-3.5" />
            </button>
            <button
              className={sectionButton}
              aria-label={anyExpanded ? "Collapse all groups" : "Expand all groups"}
              title={anyExpanded ? "Collapse all" : "Expand all"}
              disabled={groups.length === 0}
              onClick={() => setAllGroupsCollapsed(anyExpanded).catch((e) => console.error(e))}
            >
              <Icon name="chevron" className={`h-3.5 w-3.5 ${anyExpanded ? "-rotate-90" : ""}`} />
            </button>
          </div>
        </div>

        {conversations.length === 0 && groups.length === 0 && (
          <p className="px-2 py-2 text-xs leading-relaxed text-slate-400">
            No chats yet. Start one above — your assistant can use connected
            tools right away.
          </p>
        )}

        {sidebarView.groups.map(({ group, chats }) => (
          <div key={group.id}>
            <GroupHeader
              group={group}
              count={chats.length}
              dropInside={ringGroupId === group.id}
              startRenaming={renameAfterCreate === group.id}
              onStartRenamingDone={() => setRenameAfterCreate(null)}
            />
            {!group.collapsed && (
              <div id={groupBodyId(group.id)} className="ml-2 border-l border-slate-200 pl-1 dark:border-slate-800">
                {chats.map((chat) => (
                  <ChatRow key={chat.id} chat={chat} />
                ))}
                {chats.length === 0 ? (
                  <GroupEmptyZone group={group} />
                ) : (
                  <GroupFooterStrip group={group} dropInside={ringGroupId === group.id} />
                )}
              </div>
            )}
          </div>
        ))}

        {sidebarView.ungrouped.map((chat) => (
          <ChatRow key={chat.id} chat={chat} />
        ))}
      </div>

      <DragOverlay dropAnimation={{ duration: 150, easing: "cubic-bezier(0.2, 0, 0, 1)" }}>
        {activeChat && (
          <div className="max-w-52 truncate rounded-lg bg-white px-2 py-1.5 text-sm shadow-lg ring-1 ring-slate-200 dark:bg-slate-900 dark:ring-slate-700">
            {activeChat.title || "Untitled chat"}
          </div>
        )}
        {activeGroupNode && (
          <div className="flex max-w-52 items-center gap-1.5 rounded-lg bg-white px-2 py-1.5 text-sm font-medium shadow-lg ring-1 ring-slate-200 dark:bg-slate-900 dark:ring-slate-700">
            <span
              className="block h-2.5 w-2.5 shrink-0 rounded-full"
              style={{ backgroundColor: normalizeGroupColor(activeGroupNode.group.color) }}
            />
            <span className="truncate">{activeGroupNode.group.title}</span>
            <span className="shrink-0 text-[11px] text-slate-400">{activeGroupNode.chats.length}</span>
          </div>
        )}
      </DragOverlay>
      </DndContext>

      <nav className="border-t border-slate-200 p-2 dark:border-slate-800">
        {[
          { id: "chat", label: "Chat", icon: "chat" },
          { id: "connectors", label: "Connectors", icon: "plug" },
          { id: "settings", label: "Settings", icon: "settings" },
        ].map((item) => (
          <button
            key={item.id}
            className={`flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition ${
              view === item.id
                ? "bg-slate-200/80 font-medium text-slate-900 dark:bg-slate-800 dark:text-white"
                : "text-slate-500 hover:bg-slate-200/50 hover:text-slate-700 dark:hover:bg-slate-800/60 dark:hover:text-slate-200"
            }`}
            onClick={() => setView(item.id as any)}
          >
            <Icon name={item.icon} className="h-4.5 w-4.5" />
            {item.label}
          </button>
        ))}
        <div className="px-3 pb-1 pt-2 text-[10px] text-slate-300 dark:text-slate-600">
          Ducky v{version} · MCP 2026-07-28
        </div>
      </nav>
    </aside>
  );
}
