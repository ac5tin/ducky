import { useDraggable, useDroppable } from "@dnd-kit/core";
import { useCallback, useEffect, useState } from "react";
import {
  groupBodyId,
  groupDragId,
  groupHeaderZone,
  groupOverZone,
  normalizeGroupColor,
} from "../../groups";
import { useStore } from "../../store";
import type { ChatGroup } from "../../types";
import { Icon } from "../icons";
import { Button, Modal } from "../modals/Modal";
import { GroupColorPicker } from "./GroupColorPicker";
import { useInlineRename } from "./useInlineRename";

export function GroupHeader({
  group,
  count,
  startRenaming,
  onStartRenamingDone,
}: {
  group: ChatGroup;
  count: number;
  /** True for a group that was just created, so it opens in rename mode. */
  startRenaming: boolean;
  onStartRenamingDone: () => void;
}) {
  const renameGroup = useStore((s) => s.renameGroup);
  const deleteGroup = useStore((s) => s.deleteGroup);
  const setGroupCollapsed = useStore((s) => s.setGroupCollapsed);
  const newConversationInGroup = useStore((s) => s.newConversationInGroup);
  const [colorOpen, setColorOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const rename = useInlineRename(group.title, (title) => {
    renameGroup(group.id, title).catch((e) => console.error(e));
  });

  const drag = useDraggable({
    id: groupDragId(group.id),
    data: { kind: "group" },
    disabled: rename.editing, // the name input needs normal text selection
  });
  // Three droppables share the header node. The collision detection in
  // Sidebar keeps only one of them per drag kind, which is what disambiguates
  // them: a chat drag can only see `group-header`, a group drag only
  // `group-over`.
  const groupOver = useDroppable({ id: groupOverZone(group.id), data: { kind: "group-over" } });
  const header = useDroppable({ id: groupHeaderZone(group.id), data: { kind: "group-header" } });
  // Stable identity: React calls a changed ref callback with `null` and then
  // with the node again, so an inline arrow detaches and re-attaches all three
  // droppables on every render, and dnd-kit re-measures on every attach.
  const setRefs = useCallback(
    (node: HTMLElement | null) => {
      drag.setNodeRef(node);
      groupOver.setNodeRef(node);
      header.setNodeRef(node);
    },
    [drag.setNodeRef, groupOver.setNodeRef, header.setNodeRef],
  );

  // Freshly created groups are named in place. The flag is one-shot: the parent
  // clears it as soon as it has been consumed.
  useEffect(() => {
    if (!startRenaming) return;
    rename.start();
    onStartRenamingDone();
  }, [startRenaming]);

  const iconButton =
    "shrink-0 rounded p-1 text-slate-400 transition hover:bg-slate-200 hover:text-slate-700 dark:hover:bg-slate-700 dark:hover:text-slate-200";
  // The row's controls keep their space when the pointer is away, so nothing
  // moves under the cursor. `hidden` made them appear to the right of the
  // chevron and push it left, which parked "ungroup and delete" on the pixel
  // the chevron had just left. `invisible` reserves the space.
  //
  // `:focus-visible`, not `:focus-within`: a clicked button keeps focus after
  // the pointer leaves, so focus-within left the controls on screen until the
  // next click elsewhere. Only keyboard focus counts here, which is also what
  // puts the buttons in the tab order (`display: none` removes a control from
  // it).
  const revealOnHover = "invisible group-hover:visible group-has-[:focus-visible]:visible";

  return (
    <>
    <div
      ref={setRefs}
      {...drag.attributes}
      {...drag.listeners}
      className={`group flex items-center gap-1 rounded-lg px-2 py-1.5 text-sm text-slate-500 transition dark:text-slate-300 ${
        header.isOver || groupOver.isOver ? "ring-2 ring-sky-400" : ""
      } ${
        drag.isDragging ? "opacity-40" : "hover:bg-slate-200/60 dark:hover:bg-slate-800"
      }`}
    >
      <button
        className={iconButton}
        aria-expanded={!group.collapsed}
        aria-controls={groupBodyId(group.id)}
        aria-label={group.collapsed ? "Expand group" : "Collapse group"}
        title={group.collapsed ? "Expand group" : "Collapse group"}
        onClick={() => setGroupCollapsed(group.id, !group.collapsed).catch((e) => console.error(e))}
      >
        <Icon name="chevron" className={`h-3.5 w-3.5 transition ${group.collapsed ? "-rotate-90" : ""}`} />
      </button>

      <div className="relative shrink-0">
        <button
          className="flex h-4 w-4 items-center justify-center rounded"
          aria-label="Group colour"
          title="Group colour"
          onClick={() => setColorOpen((open) => !open)}
        >
          <span
            className="block h-2.5 w-2.5 rounded-full"
            style={{ backgroundColor: normalizeGroupColor(group.color) }}
          />
        </button>
        {colorOpen && <GroupColorPicker group={group} onClose={() => setColorOpen(false)} />}
      </div>

      {rename.editing ? (
        <input
          className="min-w-0 flex-1 rounded bg-white px-1 text-sm outline-none ring-1 ring-sky-400 dark:bg-slate-900"
          {...rename.inputProps}
        />
      ) : (
        <button
          className="min-w-0 flex-1 truncate text-left font-medium"
          title={group.title || "New group"}
          onClick={rename.start}
        >
          {group.title || "New group"}
        </button>
      )}

      <span className="shrink-0 text-[11px] text-slate-400">{count}</span>

      <button
        className={`${revealOnHover} ${iconButton}`}
        aria-label="New chat in group"
        title="New chat in group"
        onClick={() => newConversationInGroup(group.id).catch((e) => console.error(e))}
      >
        <Icon name="plus" className="h-3.5 w-3.5" />
      </button>

      <button
        className={`${revealOnHover} ${iconButton}`}
        aria-label="Rename group"
        title="Rename group"
        onClick={rename.start}
      >
        <Icon name="pencil" className="h-3.5 w-3.5" />
      </button>

      <button
        className={`${revealOnHover} text-slate-400 hover:bg-rose-100 hover:text-rose-600 dark:hover:bg-rose-950 ${iconButton}`}
        aria-label="Ungroup and delete"
        title="Ungroup and delete"
        onClick={() => setConfirmDelete(true)}
      >
        <Icon name="trash" className="h-3.5 w-3.5" />
      </button>
    </div>

      {/* A sibling of the header, not a child: the header carries the drag
          listeners, so a pointer-down inside the dialog would start a group
          drag. */}
      <Modal
        open={confirmDelete}
        onClose={() => setConfirmDelete(false)}
        title="Delete this group?"
        subtitle={`${group.title || "New group"} · ${count} ${count === 1 ? "chat" : "chats"}`}
      >
        <p className="text-sm leading-relaxed text-slate-600 dark:text-slate-300">
          The chats are not deleted. They leave the group and stay in your chat list as
          ungrouped chats.
        </p>
        <div className="mt-5 flex justify-end gap-2">
          <Button variant="secondary" onClick={() => setConfirmDelete(false)}>
            Cancel
          </Button>
          <Button
            variant="danger"
            onClick={() => {
              setConfirmDelete(false);
              deleteGroup(group.id).catch((e) => console.error(e));
            }}
          >
            Ungroup and delete
          </Button>
        </div>
      </Modal>
    </>
  );
}
