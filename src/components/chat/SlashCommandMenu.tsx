import type { SlashCommand } from "../../slashCommands";

/** Autocomplete list shown while the user types a "/" command in the
 * composer. Pure presentation — the Composer owns filtering and keyboard
 * state; this renders the matches and reports picks. */
export function SlashCommandMenu({
  commands,
  activeIndex,
  hasConversation,
  onPick,
  onDismiss,
}: {
  commands: SlashCommand[];
  activeIndex: number;
  hasConversation: boolean;
  onPick: (command: SlashCommand) => void;
  onDismiss: () => void;
}) {
  return (
    <>
      {/* click-away; keeping mousedown default so the textarea keeps focus */}
      <div
        className="fixed inset-0 z-30 cursor-default"
        aria-label="Close command list"
        onMouseDown={(e) => e.preventDefault()}
        onClick={onDismiss}
      />
      <div className="pop-in absolute bottom-full left-0 z-40 mb-1 w-72 overflow-hidden rounded-xl border border-slate-200 bg-white py-1 shadow-xl dark:border-slate-700 dark:bg-slate-900">
        {commands.map((command, i) => {
          const usable = !command.needsConversation || hasConversation;
          return (
            <button
              key={command.name}
              type="button"
              disabled={!usable}
              className={`flex w-full flex-col items-start gap-0.5 px-3 py-2 text-left transition ${
                i === activeIndex ? "bg-slate-50 dark:bg-slate-800" : ""
              } ${
                usable
                  ? "hover:bg-slate-50 dark:hover:bg-slate-800"
                  : "cursor-not-allowed opacity-50"
              }`}
              title={usable ? undefined : "Needs an open conversation"}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => {
                if (usable) onPick(command);
              }}
            >
              <span className="font-mono text-sm font-medium text-sky-600 dark:text-sky-400">
                /{command.name}
              </span>
              <span className="text-xs text-slate-400 dark:text-slate-500">
                {command.description}
                {!usable && " — open a chat first"}
              </span>
            </button>
          );
        })}
      </div>
    </>
  );
}
