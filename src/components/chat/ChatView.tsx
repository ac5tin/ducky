import { useEffect, useRef, useState } from "react";
import { useStore } from "../../store";
import { Icon } from "../icons";
import { Markdown } from "../Markdown";
import { ToolCallCard } from "./ToolCallCard";
import { ModelPicker } from "./ModelPicker";
import { WorkingDirChip } from "./WorkingDirChip";
import { TerminalPanel } from "./TerminalPanel";
import { rfc9557, shortTime } from "../../time";

export function ChatView() {
  const items = useStore((s) => s.items);
  const streaming = useStore((s) => s.streaming);
  const activeId = useStore((s) => s.activeConversationId);
  const newConversation = useStore((s) => s.newConversation);
  const toggleTerminal = useStore((s) => s.toggleTerminal);
  const terminalOpen = useStore(
    (s) =>
      !!s.activeConversationId && s.terminalOpenIds.has(s.activeConversationId),
  );
  const terminalHeight = useStore((s) => s.terminalHeight);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [showJump, setShowJump] = useState(false);
  const [, setTick] = useState(0);

  useEffect(() => {
    const id = setInterval(() => setTick((n) => n + 1), 60_000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 140;
    if (nearBottom || streaming) el.scrollTop = el.scrollHeight;
  }, [items, streaming]);

  if (!activeId) {
    return (
      <EmptyState
        onStart={() => newConversation().catch((e) => console.error(e))}
      />
    );
  }

  return (
    <div className="relative flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-slate-200 px-5 py-2.5 dark:border-slate-800">
        <div className="flex min-w-0 items-center gap-1">
          <ModelPicker />
          <WorkingDirChip />
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
        {streaming && (
          <span className="flex items-center gap-1.5 text-xs text-slate-400">
            <Icon name="spinner" className="h-3.5 w-3.5 animate-spin" />
            Working…
          </span>
        )}
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
            <MessageItem key={item.id} item={item} />
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
    </div>
  );
}

function MessageItem({ item }: { item: import("../../store").ChatItem }) {
  if (item.kind === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[85%]">
          <div className="whitespace-pre-wrap rounded-2xl rounded-br-md bg-sky-600 px-4 py-2.5 text-sm leading-relaxed text-white shadow-sm">
            {item.text}
          </div>
          <MessageTime ts={item.ts} align="right" />
        </div>
      </div>
    );
  }
  if (item.kind === "tool") {
    return <ToolCallCard state={item.state} />;
  }
  return (
    <div className="flex justify-start">
      <div className="max-w-[92%]">
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
        <MessageTime ts={item.ts} align="left" />
      </div>
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

function Composer() {
  const send = useStore((s) => s.send);
  const stop = useStore((s) => s.stop);
  const streaming = useStore((s) => s.streaming);
  const [text, setText] = useState("");
  const ref = useRef<HTMLTextAreaElement>(null);

  const submit = () => {
    const t = text.trim();
    if (!t || streaming) return;
    setText("");
    send(t).catch((e) => console.error(e));
  };

  return (
    <div className="border-t border-slate-200 bg-white/80 px-5 py-3.5 backdrop-blur dark:border-slate-800 dark:bg-slate-950/70">
      <div className="mx-auto flex max-w-3xl items-end gap-2">
        <textarea
          ref={ref}
          value={text}
          rows={1}
          placeholder="Ask anything — your connected tools are available automatically…"
          className="max-h-40 min-h-[44px] flex-1 resize-none rounded-xl border border-slate-200 bg-white px-3.5 py-2.5 text-sm outline-none transition focus:border-sky-400 focus:ring-2 focus:ring-sky-100 dark:border-slate-700 dark:bg-slate-900 dark:focus:border-sky-500 dark:focus:ring-sky-900/40"
          onChange={(e) => {
            setText(e.target.value);
            e.currentTarget.style.height = "auto";
            e.currentTarget.style.height = `${Math.min(e.currentTarget.scrollHeight, 160)}px`;
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
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
      </div>
    </div>
  );
}

function EmptyState({ onStart }: { onStart: () => void }) {
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
      <button
        className="mt-5 rounded-xl bg-sky-600 px-5 py-2.5 text-sm font-medium text-white shadow-sm transition hover:bg-sky-500"
        onClick={onStart}
      >
        Start a chat
      </button>
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
    </div>
  );
}
