import { useRef, useState } from "react";
import { useStore } from "../store";
import { Icon } from "./icons";

export function Sidebar() {
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const config = useStore((s) => s.config);
  const activeId = useStore((s) => s.activeConversationId);
  const openConversation = useStore((s) => s.openConversation);
  const newConversation = useStore((s) => s.newConversation);
  const deleteConversation = useStore((s) => s.deleteConversation);
  const renameConversation = useStore((s) => s.renameConversation);
  const version = useStore((s) => s.version);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const skipBlur = useRef(false);

  const commitRename = (id: string, current: string) => {
    const trimmed = draft.trim();
    if (trimmed && trimmed !== (current || "")) {
      renameConversation(id, trimmed).catch((e) => console.error(e));
    }
    setEditingId(null);
  };

  const conversations = [...(config?.conversations ?? [])].sort((a, b) =>
    b.updated_at.localeCompare(a.updated_at),
  );

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

      <div className="mt-4 min-h-0 flex-1 overflow-y-auto px-2">
        <div className="px-2 pb-1 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
          Chats
        </div>
        {conversations.length === 0 && (
          <p className="px-2 py-2 text-xs leading-relaxed text-slate-400">
            No chats yet. Start one above — your assistant can use connected
            tools right away.
          </p>
        )}
        {conversations.map((c) => (
          <div
            key={c.id}
            className={`group flex items-center gap-1 rounded-lg px-2 py-1.5 text-sm transition ${
              c.id === activeId
                ? "bg-sky-100 text-sky-900 dark:bg-sky-900/40 dark:text-sky-100"
                : "hover:bg-slate-200/60 dark:hover:bg-slate-800"
            }`}
          >
            {editingId === c.id ? (
              <input
                autoFocus
                className="min-w-0 flex-1 rounded bg-white px-1 text-sm outline-none ring-1 ring-sky-400 dark:bg-slate-900"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    skipBlur.current = true;
                    commitRename(c.id, c.title);
                  } else if (e.key === "Escape") {
                    skipBlur.current = true;
                    setEditingId(null);
                  }
                }}
                onBlur={() => {
                  if (skipBlur.current) {
                    skipBlur.current = false;
                    return;
                  }
                  commitRename(c.id, c.title);
                }}
              />
            ) : (
              <button
                className="min-w-0 flex-1 truncate text-left"
                onClick={() =>
                  openConversation(c.id).catch((e) => console.error(e))
                }
                title={c.title || "Untitled chat"}
              >
                {c.title || "Untitled chat"}
              </button>
            )}
            <button
              className="hidden shrink-0 rounded p-1 text-slate-400 hover:bg-slate-200 hover:text-slate-700 group-hover:block dark:hover:bg-slate-700 dark:hover:text-slate-200"
              aria-label="Rename chat"
              onClick={() => {
                setEditingId(c.id);
                setDraft(c.title);
              }}
            >
              <Icon name="pencil" className="h-3.5 w-3.5" />
            </button>
            <button
              className="hidden shrink-0 rounded p-1 text-slate-400 hover:bg-rose-100 hover:text-rose-600 group-hover:block dark:hover:bg-rose-950"
              aria-label="Delete chat"
              onClick={() =>
                deleteConversation(c.id).catch((e) => console.error(e))
              }
            >
              <Icon name="trash" className="h-3.5 w-3.5" />
            </button>
          </div>
        ))}
      </div>

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
