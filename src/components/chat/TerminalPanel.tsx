// Bottom terminal panel for a conversation: an xterm.js emulator wired to a
// PTY that lives in the Rust backend. Closing the panel (or switching
// conversations) keeps the shell running; reopening replays the backend
// scrollback and continues streaming live output.
import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import type { ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import * as api from "../../api";
import { subscribeTerminal, type TerminalEvent } from "../../terminalBus";
import { useStore } from "../../store";
import { Icon } from "../icons";

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

function isDark(theme: "system" | "light" | "dark"): boolean {
  return (
    theme === "dark" ||
    (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches)
  );
}

function terminalTheme(dark: boolean): ITheme {
  return dark
    ? {
        background: "#0f172a",
        foreground: "#e2e8f0",
        cursor: "#38bdf8",
        cursorAccent: "#0f172a",
        selectionBackground: "#334155",
      }
    : {
        background: "#ffffff",
        foreground: "#0f172a",
        cursor: "#0284c7",
        cursorAccent: "#ffffff",
        selectionBackground: "#bae6fd",
      };
}

/** Serializes startup against live output: chunks are buffered until
 * `terminal_create` resolves, then the scrollback is replayed and buffered
 * chunks with seq >= last_seq (not already in it) are flushed after it. */
interface OutputGate {
  ready: boolean;
  lastSeq: number;
  pending: Extract<TerminalEvent, { type: "terminal_output" }>[];
}

export function TerminalPanel({ conversationId }: { conversationId: string }) {
  const height = useStore((s) => s.terminalHeight);
  const setTerminalHeight = useStore((s) => s.setTerminalHeight);
  const toggleTerminal = useStore((s) => s.toggleTerminal);
  const theme = useStore((s) => s.config?.settings.theme ?? "system");
  const workingDir = useStore((s) => s.config?.settings.working_dir ?? null);
  const homeDir = useStore((s) => s.homeDir);
  const [exited, setExited] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const gateRef = useRef<OutputGate>({ ready: false, lastSeq: 0, pending: [] });
  const exitedRef = useRef(false);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    exitedRef.current = false;
    setExited(false);
    gateRef.current = { ready: false, lastSeq: 0, pending: [] };

    const term = new Terminal({
      fontFamily:
        'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      lineHeight: 1.25,
      cursorBlink: true,
      scrollback: 5000,
      theme: terminalTheme(isDark(useStore.getState().config?.settings.theme ?? "system")),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    fitRef.current = fit;
    termRef.current = term;

    const fitAndResize = () => {
      try {
        fit.fit();
        api.terminalResize(conversationId, term.cols, term.rows).catch(() => {});
      } catch {
        // fit() throws while the container has no layout yet; ResizeObserver retries
      }
    };
    fitAndResize();

    const write = (bytes: Uint8Array) => term.write(bytes);
    const handleOutput = (event: Extract<TerminalEvent, { type: "terminal_output" }>) => {
      const gate = gateRef.current;
      if (!gate.ready) {
        gate.pending.push(event);
      } else if (event.seq >= gate.lastSeq) {
        write(base64ToBytes(event.data));
      }
    };

    const unsubscribe = subscribeTerminal((event) => {
      if (event.conversation_id !== conversationId) return;
      if (event.type === "terminal_output") {
        handleOutput(event);
      } else {
        exitedRef.current = true;
        setExited(true);
        gateRef.current.pending = [];
        term.write(base64ToBytes(btoa("\r\n\x1b[90m[process exited]\x1b[0m\r\n")));
      }
    });

    term.onData((data) => {
      if (!exitedRef.current) {
        api.terminalWrite(conversationId, data).catch(() => {});
      }
    });

    let disposed = false;
    const attach = () =>
      api
        .terminalCreate(conversationId)
        .then((created) => {
          if (disposed) return;
          const gate = gateRef.current;
          // Replay history first; the fresh scrollback may include chunks that
          // are also sitting in pending, which the seq check drops.
          gate.lastSeq = created.last_seq;
          if (created.scrollback) write(base64ToBytes(created.scrollback));
          gate.ready = true;
          for (const event of gate.pending.splice(0)) handleOutput(event);
          fitAndResize();
          term.focus();
        })
        .catch((e) => {
          if (!disposed) term.writeln(`Failed to start terminal: ${e}`);
        });
    attach();

    const observer = new ResizeObserver(() => fitAndResize());
    observer.observe(container);

    return () => {
      disposed = true;
      unsubscribe();
      observer.disconnect();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
    // The whole emulator is per-conversation; rebuild it when switching.
  }, [conversationId]);

  // Track light/dark (including system) changes for the live panel.
  useEffect(() => {
    const apply = () => {
      if (termRef.current) termRef.current.options.theme = terminalTheme(isDark(theme));
    };
    apply();
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);

  const dragRef = useRef<{ startY: number; startHeight: number } | null>(null);

  const restart = () => {
    exitedRef.current = false;
    setExited(false);
    gateRef.current = { ready: false, lastSeq: 0, pending: [] };
    // Same attach flow as mount, minus the replay: the emulator already holds
    // the old history and the new shell only adds to it.
    api
      .terminalCreate(conversationId)
      .then((created) => {
        const gate = gateRef.current;
        gate.lastSeq = created.last_seq;
        gate.ready = true;
        const term = termRef.current;
        for (const event of gate.pending.splice(0)) {
          if (event.seq >= gate.lastSeq) {
            term?.write(base64ToBytes(event.data));
          }
        }
        term?.focus();
      })
      .catch(() => {});
  };

  const cwdLabel = (() => {
    const dir = workingDir ?? homeDir;
    if (!dir) return "";
    return homeDir && dir.startsWith(homeDir) ? `~${dir.slice(homeDir.length)}` : dir;
  })();

  return (
    <div
      className="flex shrink-0 flex-col border-t border-slate-200 bg-white dark:border-slate-800 dark:bg-slate-900"
      style={{ height }}
    >
      <div
        className="flex h-1.5 shrink-0 cursor-row-resize bg-slate-100 transition-colors hover:bg-sky-200 dark:bg-slate-800 dark:hover:bg-sky-900"
        onPointerDown={(e) => {
          dragRef.current = {
            startY: e.clientY,
            startHeight: useStore.getState().terminalHeight,
          };
          e.currentTarget.setPointerCapture(e.pointerId);
        }}
        onPointerMove={(e) => {
          const drag = dragRef.current;
          if (drag) setTerminalHeight(drag.startHeight + (drag.startY - e.clientY));
        }}
        onPointerUp={() => {
          dragRef.current = null;
        }}
        aria-label="Resize terminal"
      />
      <div className="flex shrink-0 items-center justify-between pl-3 pr-1.5 py-1">
        <div className="flex min-w-0 items-center gap-1.5 text-xs text-slate-400 dark:text-slate-500">
          <Icon name="terminal" className="h-3.5 w-3.5 shrink-0" />
          <span className="shrink-0 font-medium text-slate-500 dark:text-slate-400">
            Terminal
          </span>
          {cwdLabel && <span className="truncate">{cwdLabel}</span>}
          {exited && (
            <span className="shrink-0 rounded bg-slate-100 px-1.5 py-px font-medium text-slate-500 dark:bg-slate-800 dark:text-slate-400">
              exited
            </span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          {exited && (
            <button
              className="rounded-md px-2 py-0.5 text-xs font-medium text-sky-600 transition hover:bg-sky-50 dark:text-sky-400 dark:hover:bg-sky-950/50"
              onClick={restart}
            >
              Restart
            </button>
          )}
          <button
            className="rounded-md p-1 text-slate-400 transition hover:bg-slate-100 hover:text-slate-600 dark:hover:bg-slate-800 dark:hover:text-slate-300"
            aria-label="Close terminal"
            title="Close terminal"
            onClick={() => toggleTerminal()}
          >
            <Icon name="x" className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
      <div ref={containerRef} className="min-h-0 flex-1 px-2 pb-1.5" />
    </div>
  );
}
