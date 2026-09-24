import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useStore } from "../../store";
import {
  COMPACT_SUMMARY_MARKER,
  INIT_PROMPT_MARKER,
  filterCommands,
  type SlashCommand,
} from "../../slashCommands";
import { nextMode, resolveShownMode } from "../../modes";
import {
  messageActionKinds,
  type MessageActionKind,
} from "../../chatMessageActions";
import { Icon } from "../icons";
import { Markdown } from "../Markdown";
import { ToolCallCard } from "./ToolCallCard";
import { ModelPicker } from "./ModelPicker";
import { ModePicker } from "./ModePicker";
import { WorkingDirChip } from "./WorkingDirChip";
import { SlashCommandMenu } from "./SlashCommandMenu";
import { TerminalPanel } from "./TerminalPanel";
import { TokenMeter } from "./tokenUsage";
import { ChatConnectorsDialog } from "./ChatConnectorsDialog";
import { rfc9557, shortTime } from "../../time";

export function ChatView() {
  const items = useStore((s) => s.items);
  const activeId = useStore((s) => s.activeConversationId);
  const steering = useStore((s) =>
    s.activeConversationId ? s.steeringQueues[s.activeConversationId] : undefined,
  );
  const editMessage = useStore((s) => s.editMessage);
  const removeSteering = useStore((s) => s.removeSteering);
  const streaming = useStore(
    (s) =>
      !!s.activeConversationId &&
      s.busyConversationIds.has(s.activeConversationId),
  );
  const generateTitle = useStore((s) => s.generateTitle);
  const titleGenerating = useStore(
    (s) =>
      !!s.activeConversationId &&
      s.titleGeneratingIds.has(s.activeConversationId),
  );
  const toggleTerminal = useStore((s) => s.toggleTerminal);
  const terminalOpen = useStore(
    (s) =>
      !!s.activeConversationId && s.terminalOpenIds.has(s.activeConversationId),
  );
  const terminalHeight = useStore((s) => s.terminalHeight);
  const handleEditMessage = useCallback(
    (messageIndex: number | undefined, text: string) =>
      editMessage(messageIndex, text),
    [editMessage],
  );
  const scrollRef = useRef<HTMLDivElement>(null);
  // null until this instance has rendered a conversation once — a remount
  // (view switches unmount ChatView) must still jump to the latest message
  const prevActiveId = useRef<string | null>(null);
  // Set when the conversation changes; the next items render must land at the
  // bottom (latest message), even if messages load async after the switch.
  const jumpToBottom = useRef(false);
  const [showJump, setShowJump] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [connectorsOpen, setConnectorsOpen] = useState(false);
  const [, setTick] = useState(0);

  useEffect(() => {
    const id = setInterval(() => setTick((n) => n + 1), 60_000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    if (prevActiveId.current !== activeId) {
      prevActiveId.current = activeId;
      jumpToBottom.current = true;
    }
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 140;
    const jump = jumpToBottom.current && items.length > 0;
    if (jump) jumpToBottom.current = false;
    if (jump || nearBottom || streaming) {
      el.scrollTop = el.scrollHeight;
      if (jump) {
        // one frame later: markdown/images settle the real height after paint
        requestAnimationFrame(() => {
          const el2 = scrollRef.current;
          if (el2) el2.scrollTop = el2.scrollHeight;
        });
      }
    }
  }, [items, steering, streaming, activeId]);

  if (!activeId) {
    return <EmptyState />;
  }

  return (
    <div className="relative flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-slate-200 px-5 py-2.5 dark:border-slate-800">
        <div className="flex min-w-0 items-center gap-1">
          <ModelPicker />
          <WorkingDirChip />
          <ModePicker />
          <button
            className={`rounded-lg p-1.5 transition ${
              terminalOpen
                ? "bg-sky-50 text-sky-600 dark:bg-sky-950/60 dark:text-sky-400"
                : "text-slate-400 hover:bg-slate-100 hover:text-slate-600 dark:hover:bg-slate-800 dark:hover:text-slate-300"
            }`}
            aria-label="Toggle terminal"
            title="Toggle terminal"
            onClick={() => toggleTerminal()}
          >
            <Icon name="terminal" className="h-4 w-4" />
          </button>
        </div>
        <div className="flex items-center gap-3">
          <TokenMeter />
          {streaming && (
            <span className="flex items-center gap-1.5 text-xs text-slate-400">
              <Icon name="spinner" className="h-3.5 w-3.5 animate-spin" />
              Working…
            </span>
          )}
          <div className="relative">
            <button
              className="rounded-lg p-1.5 text-slate-400 transition hover:bg-slate-100 hover:text-slate-600 dark:hover:bg-slate-800 dark:hover:text-slate-300"
              aria-label="Chat actions"
              title="Chat actions"
              onClick={() => setMenuOpen(!menuOpen)}
            >
              <Icon name="more" className="h-4 w-4" />
            </button>
            {menuOpen && (
              <>
                <button
                  type="button"
                  className="fixed inset-0 z-30 cursor-default"
                  aria-label="Close menu"
                  onClick={() => setMenuOpen(false)}
                />
                <div className="pop-in absolute right-0 top-full z-40 mt-1 w-44 overflow-hidden rounded-xl border border-slate-200 bg-white py-1 shadow-xl dark:border-slate-700 dark:bg-slate-900">
                  <button
                    className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm transition hover:bg-slate-50 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent dark:hover:bg-slate-800 dark:disabled:hover:bg-transparent"
                    disabled={titleGenerating || items.length === 0}
                    title={
                      titleGenerating
                        ? "Generating…"
                        : items.length === 0
                          ? "Send a message first"
                          : "Generate title"
                    }
                    onClick={() => {
                      setMenuOpen(false);
                      generateTitle(activeId).catch((e) => console.error(e));
                    }}
                  >
                    <Icon name="refresh" className="h-4 w-4 text-slate-400" />
                    Generate Title
                  </button>
                  <button
                    className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm transition hover:bg-slate-50 dark:hover:bg-slate-800"
                    onClick={() => {
                      setMenuOpen(false);
                      setConnectorsOpen(true);
                    }}
                  >
                    <Icon name="plug" className="h-4 w-4 text-slate-400" />
                    Connectors
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>

      <div
        ref={scrollRef}
        onScroll={(e) => {
          const el = e.currentTarget;
          setShowJump(el.scrollHeight - el.scrollTop - el.clientHeight > 300);
        }}
        className="min-h-0 flex-1 overflow-y-auto"
      >
        <div className="mx-auto flex max-w-3xl flex-col gap-4 px-5 py-6">
          {items.map((item) => (
            <MessageItem
              key={item.id}
              item={item}
              onEdit={handleEditMessage}
              editDisabled={streaming || !!steering?.length}
            />
          ))}
          {steering?.map((m) => (
            <SteeringBubble
              key={m.id}
              message={m}
              onRemove={() => removeSteering(m.id)}
            />
          ))}
          <div className="h-2" />
        </div>
      </div>

      {showJump && (
        <button
          className="absolute right-6 rounded-full border border-slate-200 bg-white p-2 shadow-md transition hover:bg-slate-50 dark:border-slate-700 dark:bg-slate-800"
          style={{ bottom: terminalOpen ? terminalHeight + 116 : 112 }}
          aria-label="Jump to latest"
          onClick={() => {
            const el = scrollRef.current;
            if (el) el.scrollTop = el.scrollHeight;
          }}
        >
          <Icon name="chevron" className="h-4 w-4" />
        </button>
      )}

      {terminalOpen && activeId && <TerminalPanel conversationId={activeId} />}

      <Composer />
      <ChatConnectorsDialog
        open={connectorsOpen}
        onClose={() => setConnectorsOpen(false)}
      />
    </div>
  );
}

// Memoized: streaming replaces only the last item's object, so every finished
// message must not re-render (and re-parse its markdown) on each token.
const MessageItem = memo(function MessageItem({
  item,
  onEdit,
  editDisabled,
}: {
  item: import("../../store").ChatItem;
  onEdit: (messageIndex: number | undefined, text: string) => Promise<boolean>;
  editDisabled: boolean;
}) {
  if (item.kind === "user") {
    if (item.text.startsWith(COMPACT_SUMMARY_MARKER)) {
      return (
        <CompactSummaryCard
          summary={item.text.slice(COMPACT_SUMMARY_MARKER.length).trim()}
          ts={item.ts}
        />
      );
    }
    if (item.text.startsWith(INIT_PROMPT_MARKER)) {
      return <RanInitCard text={item.text} />;
    }
    return (
      <UserMessageBubble
        item={item}
        onEdit={onEdit}
        editDisabled={editDisabled}
      />
    );
  }
  if (item.kind === "tool") {
    return <ToolCallCard state={item.state} />;
  }
  return (
    <div className="flex justify-start">
      <div className="group max-w-[92%]">
        {item.reasoning && (
          <details className="mb-2 rounded-xl border border-slate-200 bg-slate-50 px-3 py-2 text-xs text-slate-500 dark:border-slate-800 dark:bg-slate-900 dark:text-slate-400">
            <summary className="cursor-pointer select-none font-medium">
              Thought for a bit
            </summary>
            <div className="mt-1.5 whitespace-pre-wrap">{item.reasoning}</div>
          </details>
        )}
        <div
          className={`rounded-2xl rounded-bl-md bg-slate-100 px-4 py-3 shadow-sm dark:bg-slate-800/80 ${
            item.streaming ? "caret" : ""
          }`}
        >
          {item.text ? (
            <Markdown text={item.text} />
          ) : (
            !item.streaming && (
              <span className="text-sm text-slate-400">(empty response)</span>
            )
          )}
          {item.error && (
            <div className="mt-2 flex items-start gap-2 rounded-lg bg-rose-50 px-3 py-2 text-sm text-rose-700 dark:bg-rose-950/50 dark:text-rose-300">
              <Icon name="warning" className="mt-0.5 h-4 w-4 shrink-0" />
              <span>{item.error}</span>
            </div>
          )}
        </div>
        <MessageActions
          text={item.text}
          kinds={messageActionKinds(item)}
          align="left"
        />
        <MessageTime ts={item.ts} align="left" />
      </div>
    </div>
  );
});

function UserMessageBubble({
  item,
  onEdit,
  editDisabled,
}: {
  item: import("../../store").UserItem;
  onEdit: (messageIndex: number | undefined, text: string) => Promise<boolean>;
  editDisabled: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(item.text);
  const [submitting, setSubmitting] = useState(false);
  const ref = useRef<HTMLTextAreaElement>(null);
  const canEdit = !editDisabled;

  const startEditing = () => {
    if (!canEdit) return;
    setText(item.text);
    setEditing(true);
    requestAnimationFrame(() => {
      ref.current?.focus();
      ref.current?.setSelectionRange(0, ref.current.value.length);
    });
  };

  const cancelEditing = () => {
    if (submitting) return;
    setEditing(false);
    setText(item.text);
  };

  const rerun = async () => {
    const next = text.trim();
    if (!canEdit || !next || submitting) return;
    setSubmitting(true);
    try {
      const ok = await onEdit(item.messageIndex, next);
      if (ok) setEditing(false);
    } catch (e) {
      console.error(e);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="flex justify-end">
      <div className="group max-w-[85%]">
        {editing ? (
          <div className="rounded-2xl rounded-br-md bg-sky-600 p-2 text-sm text-white shadow-sm">
            <textarea
              ref={ref}
              value={text}
              rows={3}
              aria-label="Edit message"
              className="max-h-48 min-h-20 w-full resize-y rounded-xl border border-sky-300 bg-white px-3 py-2 text-slate-900 outline-none focus:border-sky-200 focus:ring-2 focus:ring-sky-200/60 dark:border-sky-700 dark:bg-slate-950 dark:text-slate-100 dark:focus:border-sky-400"
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  cancelEditing();
                }
                if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                  e.preventDefault();
                  void rerun();
                }
              }}
            />
            <div className="mt-2 flex items-end justify-between gap-3">
              <span className="text-[11px] leading-snug text-sky-100">
                Re-running removes this message and later turns, and restores their file changes.
              </span>
              <div className="flex shrink-0 gap-1.5">
                <button
                  type="button"
                  className="rounded-lg px-2.5 py-1.5 text-xs font-medium text-sky-100 transition hover:bg-sky-500 disabled:opacity-50"
                  disabled={submitting}
                  onClick={cancelEditing}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  className="flex items-center gap-1.5 rounded-lg bg-white px-2.5 py-1.5 text-xs font-semibold text-sky-700 transition hover:bg-sky-50 disabled:cursor-not-allowed disabled:opacity-50"
                  disabled={submitting || editDisabled || !text.trim()}
                  onClick={() => void rerun()}
                >
                  {submitting && <Icon name="spinner" className="h-3 w-3 animate-spin" />}
                  Edit & re-run
                </button>
              </div>
            </div>
          </div>
        ) : (
          <div className="whitespace-pre-wrap rounded-2xl rounded-br-md bg-sky-600 px-4 py-2.5 text-sm leading-relaxed text-white shadow-sm">
            {item.text}
          </div>
        )}
        {!editing && (
          <MessageActions
            text={item.text}
            kinds={messageActionKinds(item)}
            align="right"
            onEdit={startEditing}
            editDisabled={!canEdit}
          />
        )}
        <MessageTime ts={item.ts} align="right" />
      </div>
    </div>
  );
}

function MessageActions({
  text,
  kinds,
  align,
  onEdit,
  editDisabled = false,
}: {
  text: string;
  kinds: MessageActionKind[];
  align: "left" | "right";
  onEdit?: () => void;
  editDisabled?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  if (kinds.length === 0) return null;

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div
      className={`mt-1 flex min-h-6 items-center gap-1 opacity-0 transition-opacity group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100 pointer-events-none ${
        align === "right" ? "justify-end" : "justify-start"
      }`}
    >
      {kinds.includes("copy") && (
        <button
          type="button"
          className="rounded-md p-1 text-slate-400 transition hover:bg-slate-100 hover:text-slate-600 dark:hover:bg-slate-800 dark:hover:text-slate-300"
          aria-label={copied ? "Copied" : "Copy message"}
          title={copied ? "Copied" : "Copy message"}
          onClick={(e) => {
            e.stopPropagation();
            void copy();
          }}
        >
          <Icon name={copied ? "check" : "copy"} className="h-3.5 w-3.5" />
        </button>
      )}
      {kinds.includes("edit") && onEdit && (
        <button
          type="button"
          className="rounded-md p-1 text-slate-400 transition hover:bg-slate-100 hover:text-slate-600 disabled:cursor-not-allowed disabled:opacity-40 dark:hover:bg-slate-800 dark:hover:text-slate-300"
          aria-label="Edit message"
          title={editDisabled ? "Wait for the response to finish" : "Edit message"}
          disabled={editDisabled}
          onClick={(e) => {
            e.stopPropagation();
            onEdit();
          }}
        >
          <Icon name="pencil" className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}

function MessageTime({ ts, align }: { ts?: string; align: "left" | "right" }) {
  if (!ts) return null;
  return (
    <time
      className={`mt-1 block px-1 text-[11px] text-slate-400 dark:text-slate-500 ${
        align === "right" ? "text-right" : "text-left"
      }`}
      dateTime={ts}
      title={rfc9557(ts)}
    >
      {shortTime(ts)}
    </time>
  );
}

/** The single message a compacted conversation starts from: the summary that
 * replaced everything before it. */
function CompactSummaryCard({
  summary,
  ts,
}: {
  summary: string;
  ts?: string;
}) {
  return (
    <div className="flex justify-center">
      <div className="w-full max-w-[85%] rounded-xl border border-slate-200 bg-slate-50 px-4 py-2.5 text-sm dark:border-slate-800 dark:bg-slate-900">
        <details>
          <summary className="cursor-pointer select-none text-xs font-medium text-slate-500 dark:text-slate-400">
            <span className="inline-flex items-center gap-1.5">
              <Icon name="box" className="h-3.5 w-3.5" />
              Conversation compacted — summary of everything before this point
            </span>
          </summary>
          <div className="mt-2 whitespace-pre-wrap leading-relaxed text-slate-600 dark:text-slate-300">
            {summary}
          </div>
        </details>
        <MessageTime ts={ts} align="left" />
      </div>
    </div>
  );
}

/** The canned /init prompt, shown collapsed so the transcript stays readable. */
function RanInitCard({ text }: { text: string }) {
  return (
    <div className="flex justify-end">
      <div className="max-w-[85%]">
        <details className="rounded-2xl rounded-br-md bg-sky-600 px-4 py-2.5 text-sm text-white shadow-sm">
          <summary className="cursor-pointer select-none font-medium">
            <span className="inline-flex items-center gap-1.5">
              <Icon name="file" className="h-3.5 w-3.5" />
              Ran /init — create or update AGENTS.md
            </span>
          </summary>
          <div className="mt-1.5 whitespace-pre-wrap text-xs leading-relaxed text-sky-100">
            {text}
          </div>
        </details>
      </div>
    </div>
  );
}

function SteeringBubble({
  message,
  onRemove,
}: {
  message: import("../../types").SteeringMessage;
  onRemove: () => void;
}) {
  return (
    <div className="flex justify-end">
      <div className="max-w-[85%]">
        <div className="whitespace-pre-wrap rounded-2xl rounded-br-md border border-dashed border-sky-400/70 bg-sky-600/40 px-4 py-2.5 text-sm leading-relaxed text-sky-50">
          {message.text}
        </div>
        <div className="mt-1 flex items-center justify-end gap-1.5 px-1 text-[11px] text-slate-400 dark:text-slate-500">
          <Icon name="clock" className="h-3 w-3" />
          <span>Steering</span>
          <button
            className="rounded p-0.5 transition hover:text-rose-500"
            aria-label="Remove steering message"
            title="Remove steering message"
            onClick={onRemove}
          >
            <Icon name="x" className="h-3 w-3" />
          </button>
        </div>
      </div>
    </div>
  );
}

function Composer({ autoFocus = false }: { autoFocus?: boolean }) {
  const send = useStore((s) => s.send);
  const stop = useStore((s) => s.stop);
  const activeId = useStore((s) => s.activeConversationId);
  const config = useStore((s) => s.config);
  const draftMode = useStore((s) => s.draftMode);
  const setMode = useStore((s) => s.setMode);
  const streaming = useStore(
    (s) =>
      !!s.activeConversationId &&
      s.busyConversationIds.has(s.activeConversationId),
  );
  const compacting = useStore(
    (s) =>
      !!s.activeConversationId &&
      s.compactingConversationIds.has(s.activeConversationId),
  );
  const restoredDraft = useStore((s) =>
    s.activeConversationId ? s.restoredDrafts[s.activeConversationId] : undefined,
  );
  const clearRestoredDraft = useStore((s) => s.clearRestoredDraft);
  const [text, setText] = useState("");
  // Esc hides the popup for the current "/" token; leaving or clearing the
  // slash context reopens it
  const [dismissed, setDismissed] = useState(false);
  const [menuIndex, setMenuIndex] = useState(0);
  const ref = useRef<HTMLTextAreaElement>(null);

  // /undo puts the removed prompt back into the composer
  useEffect(() => {
    if (restoredDraft === undefined || !activeId) return;
    clearRestoredDraft(activeId);
    setText(restoredDraft);
    requestAnimationFrame(() => {
      const el = ref.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
      el.style.height = "auto";
      el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
    });
  }, [restoredDraft, activeId, clearRestoredDraft]);

  const slashMatches = useMemo(() => {
    const t = text.trim();
    if (!t.startsWith("/") || /\s/.test(t)) return [];
    return filterCommands(t);
  }, [text]);
  const menuOpen = slashMatches.length > 0 && !dismissed && !compacting;
  const activeMatch = Math.min(menuIndex, slashMatches.length - 1);

  const completeCommand = (command: SlashCommand) => {
    setText(`/${command.name} `);
    setDismissed(false);
    requestAnimationFrame(() => {
      const el = ref.current;
      if (!el) return;
      el.focus();
      el.style.height = "auto";
      el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
    });
  };

  const submit = () => {
    const t = text.trim();
    if (!t || compacting) return;
    // while the agent is responding the store steers the message into the
    // running turn instead
    setText("");
    send(t).catch((e) => console.error(e));
  };

  const cycleMode = () => {
    const conversation = config?.conversations.find((c) => c.id === activeId);
    const next = nextMode(resolveShownMode(conversation, draftMode, config));
    setMode(next).catch(() => {});
  };

  return (
    <div className="border-t border-slate-200 bg-white/80 px-5 py-3.5 backdrop-blur dark:border-slate-800 dark:bg-slate-950/70">
      <div className="relative mx-auto flex max-w-3xl items-end gap-2">
        {menuOpen && (
          <SlashCommandMenu
            commands={slashMatches}
            activeIndex={activeMatch}
            hasConversation={!!activeId}
            onPick={completeCommand}
            onDismiss={() => setDismissed(true)}
          />
        )}
        {compacting ? (
          // /compact in flight: the input is unavailable until it lands
          <div
            className="flex min-h-[44px] flex-1 items-center gap-2.5 rounded-xl border border-sky-200 bg-sky-50 px-3.5 py-2.5 text-sm font-medium text-sky-700 dark:border-sky-900 dark:bg-sky-950/60 dark:text-sky-300"
            role="status"
          >
            <Icon name="spinner" className="h-4 w-4 animate-spin" />
            Compacting the conversation…
          </div>
        ) : (
          <>
            <textarea
              ref={ref}
              value={text}
              autoFocus={autoFocus}
              rows={1}
              placeholder={
                streaming
                  ? "Steer the response — it enters after the current step…"
                  : "Ask anything — your connected tools are available automatically…"
              }
              className="max-h-40 min-h-[44px] flex-1 resize-none rounded-xl border border-slate-200 bg-white px-3.5 py-2.5 text-sm outline-none transition focus:border-sky-400 focus:ring-2 focus:ring-sky-100 dark:border-slate-700 dark:bg-slate-900 dark:focus:border-sky-500 dark:focus:ring-sky-900/40"
              onChange={(e) => {
                setText(e.target.value);
                if (!e.target.value.trim().startsWith("/")) setDismissed(false);
                e.currentTarget.style.height = "auto";
                e.currentTarget.style.height = `${Math.min(e.currentTarget.scrollHeight, 160)}px`;
              }}
              onKeyDown={(e) => {
                if (e.key === "Tab" && e.shiftKey) {
                  // Shift+Tab cycles the agent mode, before the slash menu's
                  // plain-Tab completion below
                  e.preventDefault();
                  cycleMode();
                  return;
                }
                if (menuOpen && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
                  e.preventDefault();
                  const step = e.key === "ArrowDown" ? 1 : slashMatches.length - 1;
                  setMenuIndex((activeMatch + step) % slashMatches.length);
                  return;
                }
                if (menuOpen && e.key === "Tab") {
                  e.preventDefault();
                  completeCommand(slashMatches[activeMatch]);
                  return;
                }
                if (menuOpen && e.key === "Escape") {
                  e.preventDefault();
                  setDismissed(true);
                  return;
                }
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (
                    menuOpen &&
                    `/${slashMatches[activeMatch].name}` !== text.trim()
                  ) {
                    // still typing a command name — complete it, don't submit
                    completeCommand(slashMatches[activeMatch]);
                    return;
                  }
                  submit();
                }
              }}
            />
            {streaming ? (
              <button
                className="flex h-11 w-11 items-center justify-center rounded-xl bg-slate-800 text-white transition hover:bg-slate-700 dark:bg-slate-700"
                aria-label="Stop"
                onClick={stop}
              >
                <Icon name="stop" className="h-4 w-4" />
              </button>
            ) : (
              <button
                className="flex h-11 w-11 items-center justify-center rounded-xl bg-sky-600 text-white shadow-sm transition hover:bg-sky-500 disabled:opacity-40"
                aria-label="Send"
                disabled={!text.trim()}
                onClick={submit}
              >
                <Icon name="send" className="h-4.5 w-4.5" />
              </button>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function EmptyState() {
  const provider = useStore((s) => s.activeProvider());
  const examples = [
    "Summarise the files in my project folder",
    "Search the web for the latest Rust release notes",
    "What tools do you have access to right now?",
  ];
  return (
    <div className="flex h-full flex-col items-center justify-center px-6 text-center">
      <div className="mb-4 flex h-20 w-20 items-center justify-center rounded-3xl bg-sky-500 text-white shadow-lg">
        <Icon name="duck" className="h-11 w-11" />
      </div>
      <h1 className="text-xl font-semibold">Hi! I'm Ducky</h1>
      <p className="mt-1.5 max-w-md text-sm leading-relaxed text-slate-500 dark:text-slate-400">
        {provider
          ? `Connected to ${provider.name}. Start a chat and I'll use your connected tools when they help.`
          : "Add an AI provider in Settings to get started, then connect a few tools."}
      </p>
      <div className="mt-8 grid max-w-xl gap-2 sm:grid-cols-1">
        {examples.map((e) => (
          <div
            key={e}
            className="rounded-xl border border-slate-200 px-4 py-2.5 text-sm text-slate-500 dark:border-slate-800 dark:text-slate-400"
          >
            “{e}”
          </div>
        ))}
      </div>
      <div className="mt-6 w-full max-w-3xl">
        <div className="mb-2 flex items-center justify-center gap-1">
          <ModelPicker dropUp />
          <WorkingDirChip />
          <ModePicker dropUp />
        </div>
        <Composer autoFocus />
      </div>
    </div>
  );
}
