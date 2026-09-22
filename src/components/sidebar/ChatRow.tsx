import { useDraggable, useDroppable } from "@dnd-kit/core";
import { chatDragId, chatZone } from "../../groups";
import { useStore } from "../../store";
import type { ConversationMeta } from "../../types";
import { Icon } from "../icons";
import { useInlineRename } from "./useInlineRename";

export function ChatRow({ chat }: { chat: ConversationMeta }) {
  const activeId = useStore((s) => s.activeConversationId);
  const openConversation = useStore((s) => s.openConversation);
  const deleteConversation = useStore((s) => s.deleteConversation);
  const renameConversation = useStore((s) => s.renameConversation);
  const generateTitle = useStore((s) => s.generateTitle);
  const cancelTitle = useStore((s) => s.cancelTitle);
  const titleGenerating = useStore((s) => s.titleGeneratingIds.has(chat.id));
  const rename = useInlineRename(chat.title, (title) => {
    renameConversation(chat.id, title).catch((e) => console.error(e));
  });

  const drag = useDraggable({
    id: chatDragId(chat.id),
    data: { kind: "chat" },
    disabled: rename.editing, // selecting text in the input must not drag
  });
  const drop = useDroppable({ id: chatZone(chat.id), data: { kind: "chat" } });
  const setRefs = (node: HTMLElement | null) => {
    drag.setNodeRef(node);
    drop.setNodeRef(node);
  };

  return (
    <div
      ref={setRefs}
      {...drag.attributes}
      {...drag.listeners}
      className={`group flex items-center gap-1 rounded-lg px-2 py-1.5 text-sm transition ${
        drag.isDragging ? "opacity-40" : ""
      } ${
        chat.id === activeId
          ? "bg-sky-100 text-sky-900 dark:bg-sky-900/40 dark:text-sky-100"
          : "hover:bg-slate-200/60 dark:hover:bg-slate-800"
      }`}
    >
      {rename.editing ? (
        <>
          <input
            className="min-w-0 flex-1 rounded bg-white px-1 text-sm outline-none ring-1 ring-sky-400 dark:bg-slate-900"
            {...rename.inputProps}
          />
          <button
            className="shrink-0 rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-700 dark:hover:bg-slate-700 dark:hover:text-slate-200"
            aria-label="Generate title"
            title="Generate title"
            onMouseDown={(e) => {
              e.preventDefault();
              rename.cancel();
              generateTitle(chat.id).catch((err) => console.error(err));
            }}
          >
            <Icon name="refresh" className="h-3.5 w-3.5" />
          </button>
        </>
      ) : titleGenerating ? (
        <button
          className="min-w-0 flex-1 cursor-pointer truncate text-left"
          title="Click to name this chat"
          onClick={() => {
            openConversation(chat.id).catch((e) => console.error(e));
            cancelTitle(chat.id).catch((e) => console.error(e));
            rename.start();
          }}
        >
          <span className="block h-3 w-28 animate-pulse rounded bg-slate-300 dark:bg-slate-600" />
        </button>
      ) : (
        <button
          className="min-w-0 flex-1 truncate text-left"
          onClick={() => openConversation(chat.id).catch((e) => console.error(e))}
          title={chat.title || "Untitled chat"}
        >
          {chat.title || "Untitled chat"}
        </button>
      )}
      <button
        className="hidden shrink-0 rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-700 group-hover:block dark:hover:bg-slate-700 dark:hover:text-slate-200"
        aria-label="Rename chat"
        onClick={() => {
          if (titleGenerating) cancelTitle(chat.id).catch((e) => console.error(e));
          rename.start();
        }}
      >
        <Icon name="pencil" className="h-3.5 w-3.5" />
      </button>
      <button
        className="hidden shrink-0 rounded p-1 text-slate-400 hover:bg-rose-100 hover:text-rose-600 group-hover:block dark:hover:bg-rose-950"
        aria-label="Delete chat"
        onClick={() => deleteConversation(chat.id).catch((e) => console.error(e))}
      >
        <Icon name="trash" className="h-3.5 w-3.5" />
      </button>
    </div>
  );
}
