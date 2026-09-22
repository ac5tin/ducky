import { useMemo, useState } from "react";
import { buildSidebarView, groupBodyId } from "../groups";
import { useStore } from "../store";
import { Icon } from "./icons";
import { ChatRow } from "./sidebar/ChatRow";
import { GroupHeader } from "./sidebar/GroupHeader";

export function Sidebar() {
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const config = useStore((s) => s.config);
  const newConversation = useStore((s) => s.newConversation);
  const newConversationInGroup = useStore((s) => s.newConversationInGroup);
  const createGroup = useStore((s) => s.createGroup);
  const setAllGroupsCollapsed = useStore((s) => s.setAllGroupsCollapsed);
  const version = useStore((s) => s.version);
  const [renameAfterCreate, setRenameAfterCreate] = useState<string | null>(null);

  const groups = useMemo(() => config?.groups ?? [], [config]);
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
              aria-label="Collapse all groups"
              title="Collapse all"
              disabled={groups.length === 0}
              onClick={() => setAllGroupsCollapsed(true).catch((e) => console.error(e))}
            >
              <Icon name="chevron" className="h-3.5 w-3.5 -rotate-90" />
            </button>
            <button
              className={sectionButton}
              aria-label="Expand all groups"
              title="Expand all"
              disabled={groups.length === 0}
              onClick={() => setAllGroupsCollapsed(false).catch((e) => console.error(e))}
            >
              <Icon name="chevron" className="h-3.5 w-3.5 rotate-90" />
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
              startRenaming={renameAfterCreate === group.id}
              onStartRenamingDone={() => setRenameAfterCreate(null)}
            />
            {!group.collapsed && (
              <div id={groupBodyId(group.id)} className="ml-2 border-l border-slate-200 pl-1 dark:border-slate-800">
                {chats.map((chat) => (
                  <ChatRow key={chat.id} chat={chat} />
                ))}
                {chats.length === 0 && (
                  <button
                    className="w-full rounded-lg border border-dashed border-slate-300 px-2 py-1.5 text-xs text-slate-400 transition hover:border-sky-400 hover:text-sky-600 dark:border-slate-700"
                    onClick={() => newConversationInGroup(group.id).catch((e) => console.error(e))}
                  >
                    New chat, or drag here.
                  </button>
                )}
              </div>
            )}
          </div>
        ))}

        {sidebarView.ungrouped.map((chat) => (
          <ChatRow key={chat.id} chat={chat} />
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