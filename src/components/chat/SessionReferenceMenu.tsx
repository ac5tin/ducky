import type { ConversationMeta } from "../../types";

/** Autocomplete list shown while the user types a `#` chat reference in the
 * composer. Pure presentation — the Composer owns filtering and keyboard
 * state; this renders the matches and reports picks. */
export function SessionReferenceMenu({
  chats,
  activeIndex,
  onPick,
  onDismiss,
}: {
  chats: ConversationMeta[];
  activeIndex: number;
  onPick: (chat: ConversationMeta) => void;
  onDismiss: () => void;
}) {
  return (
    <>
      {/* click-away; keeping mousedown default so the textarea keeps focus */}
      <div
        className="fixed inset-0 z-30 cursor-default"
        aria-label="Close chat list"
        onMouseDown={(e) => e.preventDefault()}
        onClick={onDismiss}
      />
      <div className="pop-in absolute bottom-full left-0 z-40 mb-1 w-80 overflow-hidden rounded-xl border border-slate-200 bg-white py-1 shadow-xl dark:border-slate-700 dark:bg-slate-900">
        {chats.map((chat, i) => (
          <button
            key={chat.id}
            type="button"
            className={`flex w-full flex-col items-start gap-0.5 px-3 py-2 text-left transition ${
              i === activeIndex ? "bg-slate-50 dark:bg-slate-800" : ""
            } hover:bg-slate-50 dark:hover:bg-slate-800`}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => onPick(chat)}
          >
            <span className="w-full truncate text-sm font-medium text-slate-700 dark:text-slate-200">
              {chat.title.trim() || "Untitled chat"}
            </span>
            <span className="font-mono text-xs text-slate-400 dark:text-slate-500">
              #chat_{chat.id.slice(0, 8)}
            </span>
          </button>
        ))}
      </div>
    </>
  );
}
