// Global app store: state, backend event wiring, and actions.
import { create } from "zustand";
import { check as updaterCheck, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import * as api from "./api";
import { resolveDraftModel } from "./chatDraft";
import { expandInitPrompt, parseSlashCommand } from "./slashCommands";
import { dispatchTerminalEvent } from "./terminalBus";
import type {
  AppConfig,
  AuthReason,
  BackendEvent,
  ConnectorSuggestion,
  EffortLevel,
  ProviderConfig,
  ProviderPreset,
  RawMessage,
  ServerSummary,
  ToolCallState,
} from "./types";

// ---------------------------------------------------------------------------
// Display model
// ---------------------------------------------------------------------------

export interface UserItem {
  kind: "user";
  id: string;
  text: string;
  ts?: string;
}

export interface AssistantItem {
  kind: "assistant";
  id: string;
  text: string;
  ts?: string;
  reasoning?: string;
  streaming?: boolean;
  error?: string;
}

export interface ToolItem {
  kind: "tool";
  id: string;
  state: ToolCallState;
}

export type ChatItem = UserItem | AssistantItem | ToolItem;

/** A message submitted while its conversation was mid-turn; held until the
 * turn ends, then sent FIFO (one per turn end). */
export interface QueuedMessage {
  id: string;
  text: string;
  ts: string;
}

export interface Toast {
  id: string;
  kind: "error" | "info" | "success";
  text: string;
}

export interface ApprovalRequest {
  request_id: string;
  server_title: string;
  tool: string;
  args: any;
  read_only_hint?: boolean;
}

export interface ElicitationRequest {
  request_id: string;
  server_title: string;
  mode: "form" | "url";
  message: string;
  schema?: any;
  url?: string;
}

export interface SamplingRequest {
  request_id: string;
  server_title: string;
  system_prompt?: string;
  messages: any[];
  max_tokens?: number;
}

export type View = "chat" | "connectors" | "settings" | "onboarding";

export interface UpdateState {
  status: "idle" | "checking" | "available" | "downloading" | "ready";
  version: string;
  notes: string | null;
  downloaded: number;
  contentLength: number | null;
  /** True once the user answered "Later"; hides the prompt until a newer version. */
  promptDismissed: boolean;
}

// Shared init promise: concurrent init() calls (StrictMode double-effect) must
// not register a second backend listener, or every delta is handled twice.
let initPromise: Promise<void> | null = null;
// Lazy chat creation: the draft page has no record yet; this flag stops a
// double-submit during the create round-trip from spawning two records.
let creatingDraft = false;
// The update found by the last check; kept outside the store because it holds
// the download handle, not display state.
let pendingUpdate: Update | null = null;
let updateTimer: ReturnType<typeof setInterval> | null = null;

/** (Re)arm the periodic update check; 0 hours means startup-only. */
function scheduleUpdateChecks(hours: number) {
  if (updateTimer) clearInterval(updateTimer);
  updateTimer = null;
  if (hours > 0) {
    updateTimer = setInterval(() => {
      void useStore.getState().checkForUpdates(false);
    }, hours * 3_600_000);
  }
}
const media =
  typeof window === "undefined"
    ? null
    : window.matchMedia("(prefers-color-scheme: dark)");

function applyTheme(theme: "system" | "light" | "dark") {
  if (typeof document === "undefined") return;
  const dark = theme === "dark" || (theme === "system" && media?.matches);
  document.documentElement.classList.toggle("dark", !!dark);
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
}

media?.addEventListener("change", () => {
  const t = useStore.getState().config?.settings.theme ?? "system";
  if (t === "system") applyTheme("system");
});

interface StoreState {
  ready: boolean;
  config: AppConfig | null;
  presets: ProviderPreset[];
  suggestions: ConnectorSuggestion[];
  servers: ServerSummary[];
  version: string;
  /** The machine's home directory; the default working directory. */
  homeDir: string;

  view: View;
  activeConversationId: string | null;
  /** Model staged on the draft page; null = no pick, defaults apply. */
  draftModel: string | null;
  /** Effort staged on the draft page; undefined = untouched (creation seeds
   * settings.default_effort), null = explicit "Default". */
  draftEffort: EffortLevel | null | undefined;
  items: ChatItem[];
  streaming: boolean;
  busyConversationIds: Set<string>;
  /** Conversations currently running a /compact summary. */
  compactingConversationIds: Set<string>;
  /** Undone prompts waiting to be restored into the composer, by conversation. */
  restoredDrafts: Record<string, string>;
  /** Per-conversation FIFO of messages waiting for the current turn to end. */
  messageQueues: Record<string, QueuedMessage[]>;
  titleGeneratingIds: Set<string>;
  /** Last provider token usage per conversation. */
  usageByConversation: Record<string, { input?: number; output?: number }>;

  /** Conversations whose terminal panel is open; the PTY lives in the backend. */
  terminalOpenIds: Set<string>;
  /** Terminal panel height in px (shared across conversations). */
  terminalHeight: number;

  approvals: ApprovalRequest[];
  elicitations: ElicitationRequest[];
  samplings: SamplingRequest[];
  toasts: Toast[];
  update: UpdateState;

  init: () => Promise<void>;
  setView: (v: View) => void;

  toast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: string) => void;

  checkForUpdates: (manual: boolean) => Promise<void>;
  downloadUpdate: () => Promise<void>;
  restartForUpdate: () => Promise<void>;
  dismissUpdate: () => void;

  refreshConfig: () => Promise<void>;
  refreshServers: () => Promise<void>;
  refreshServer: (id: string) => Promise<void>;

  openConversation: (id: string) => Promise<void>;
  newConversation: () => Promise<void>;
  deleteConversation: (id: string) => Promise<void>;
  renameConversation: (id: string, title: string) => Promise<void>;
  generateTitle: (id: string) => Promise<void>;
  cancelTitle: (id: string) => Promise<void>;
  setActiveModel: (providerId: string, model: string) => Promise<void>;
  setActiveEffort: (effort: EffortLevel | null) => Promise<void>;
  setActiveMcpIds: (mcpIds: string[] | null) => Promise<void>;

  send: (text: string) => Promise<void>;
  stop: () => void;
  removeQueued: (id: string) => void;
  /** `/compact`: summarise the conversation's history. */
  compactConversation: (id: string, instructions?: string) => Promise<void>;
  /** `/undo`: drop the last turn and revert its file changes. */
  undoConversation: (id: string) => Promise<void>;
  /** Re-read a conversation's transcript from the backend into `items`. */
  reloadItems: (id: string) => Promise<void>;
  clearRestoredDraft: (id: string) => void;

  toggleTerminal: (id?: string) => void;
  setTerminalHeight: (height: number) => void;

  respondApproval: (
    requestId: string,
    decision: "allow_once" | "always_allow" | "deny",
  ) => Promise<void>;
  respondElicitation: (
    requestId: string,
    action: "accept" | "decline" | "cancel",
    content?: any,
  ) => Promise<void>;
  respondSampling: (requestId: string, approve: boolean) => Promise<void>;

  activeProvider: () => ProviderConfig | null;
}

function rawToItems(
  raw: RawMessage[],
  toolStates: Map<string, ToolCallState>,
): ChatItem[] {
  const items: ChatItem[] = [];
  for (const msg of raw) {
    if (msg.kind === "user") {
      items.push({
        kind: "user",
        id: `u-${items.length}`,
        text: msg.text,
        ts: msg.ts ?? undefined,
      });
    } else if (msg.kind === "assistant") {
      items.push({
        kind: "assistant",
        id: `a-${items.length}`,
        text: msg.text,
        ts: msg.ts ?? undefined,
      });
      for (const call of msg.tool_calls ?? []) {
        const state = toolStates.get(call.id) ?? {
          tool_call_id: call.id,
          status: "done" as const,
          tool: call.name,
          args: call.arguments,
        };
        items.push({
          kind: "tool",
          id: call.id,
          state: {
            ...state,
            tool: state.tool ?? call.name,
            args: state.args ?? call.arguments,
          },
        });
      }
    } else if (msg.kind === "tool_result") {
      // attach result to the matching tool card if it has none yet
      for (let i = items.length - 1; i >= 0; i--) {
        const item = items[i];
        if (item.kind === "tool" && item.id === msg.call_id) {
          if (!item.state.result_text) {
            item.state.result_text = msg.text;
            item.state.is_error = msg.is_error;
            if (
              item.state.status === "pending_approval" ||
              item.state.status === "running"
            ) {
              item.state.status = msg.is_error ? "error" : "done";
            }
          }
          break;
        }
      }
    }
  }
  return items;
}

export const useStore = create<StoreState>((set, get) => ({
  ready: false,
  config: null,
  presets: [],
  suggestions: [],
  servers: [],
  version: "",
  homeDir: "",

  view: "chat",
  activeConversationId: null,
  draftModel: null,
  draftEffort: undefined,
  items: [],
  streaming: false,
  busyConversationIds: new Set(),
  compactingConversationIds: new Set(),
  restoredDrafts: {},
  messageQueues: {},
  titleGeneratingIds: new Set(),
  usageByConversation: {},

  terminalOpenIds: new Set(),
  terminalHeight: 300,

  approvals: [],
  elicitations: [],
  samplings: [],
  toasts: [],
  update: {
    status: "idle",
    version: "",
    notes: null,
    downloaded: 0,
    contentLength: null,
    promptDismissed: false,
  },

  async init() {
    if (!initPromise) {
      initPromise = (async () => {
        await api.listenBackend((event) => handleEvent(event, set, get));
        const boot = await api.getBootstrap();
        const info = await api.appInfo().catch(() => ({ version: "" }));
        applyTheme(boot.config.settings.theme);
        set({
          ready: true,
          config: boot.config,
          presets: boot.presets,
          suggestions: boot.suggestions,
          servers: boot.server_summaries,
          version: info.version,
          homeDir: boot.home_dir ?? "",
          view:
            boot.config.onboarding_complete && boot.config.providers.length > 0
              ? "chat"
              : "onboarding",
        });
        if (!import.meta.env.DEV) {
          scheduleUpdateChecks(
            boot.config.settings.update_check_interval_hours,
          );
          void get().checkForUpdates(false);
        }
      })().catch((e) => {
        initPromise = null;
        throw e;
      });
    }
    return initPromise;
  },

  setView(view) {
    set({ view });
  },

  toast(kind, text) {
    const id = Math.random().toString(36).slice(2);
    set((s) => ({ toasts: [...s.toasts, { id, kind, text }] }));
    setTimeout(() => get().dismissToast(id), kind === "error" ? 9000 : 4000);
  },
  dismissToast(id) {
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
  },

  async refreshConfig() {
    const config = await api.getConfig();
    applyTheme(config.settings.theme);
    set((s) => ({
      config: {
        ...config,
        conversations: config.conversations.map((c) => {
          if (c.title) return c;
          const local = s.config?.conversations.find((x) => x.id === c.id);
          return local?.title ? { ...c, title: local.title } : c;
        }),
      },
    }));
    // the check interval may have just changed
    scheduleUpdateChecks(config.settings.update_check_interval_hours);
  },

  async refreshServers() {
    set({ servers: await api.mcpSummaries() });
  },

  async refreshServer(id) {
    try {
      const summary = await api.mcpSummary(id);
      set((s) => ({
        servers: s.servers.map((srv) => (srv.id === id ? summary : srv)),
      }));
    } catch {
      /* server removed */
    }
  },

  async openConversation(id) {
    if (id === get().activeConversationId) {
      // clicking the chat we're already in: just show it — don't wipe items
      // and reload, which would reset scroll to the top
      set({ view: "chat" });
      return;
    }
    set({ activeConversationId: id, items: [], view: "chat" });
    try {
      const [, raw] = await api.conversationGet(id);
      const toolStates = new Map<string, ToolCallState>();
      // keep live tool states from an in-flight turn, if any
      for (const item of get().items) {
        if (item.kind === "tool") toolStates.set(item.id, item.state);
      }
      set({ items: rawToItems(raw, toolStates) });
    } catch (e) {
      get().toast("error", `Could not open that conversation: ${e}`);
    }
  },

  async newConversation() {
    // The draft page: no conversation record exists until the first message
    // is sent from it (see send()). A fresh draft starts from defaults.
    set({
      activeConversationId: null,
      draftModel: null,
      draftEffort: undefined,
      items: [],
      view: "chat",
    });
  },

  async renameConversation(id, title) {
    await api.conversationRename(id, title);
    await get().refreshConfig();
  },

  async generateTitle(id) {
    set((s) => {
      const titleGeneratingIds = new Set(s.titleGeneratingIds);
      titleGeneratingIds.add(id);
      return { titleGeneratingIds };
    });
    try {
      await api.conversationGenerateTitle(id);
    } catch (e) {
      set((s) => {
        const titleGeneratingIds = new Set(s.titleGeneratingIds);
        titleGeneratingIds.delete(id);
        return { titleGeneratingIds };
      });
      get().toast("error", `${e}`);
    }
  },

  async cancelTitle(id) {
    set((s) => {
      const titleGeneratingIds = new Set(s.titleGeneratingIds);
      titleGeneratingIds.delete(id);
      return { titleGeneratingIds };
    });
    await api.conversationCancelTitle(id).catch(() => {});
  },

  async deleteConversation(id) {
    await api.conversationDelete(id);
    set((s) => {
      const terminalOpenIds = new Set(s.terminalOpenIds);
      terminalOpenIds.delete(id);
      const titleGeneratingIds = new Set(s.titleGeneratingIds);
      titleGeneratingIds.delete(id);
      const { [id]: _queue, ...messageQueues } = s.messageQueues;
      return { terminalOpenIds, titleGeneratingIds, messageQueues };
    });
    const state = get();
    if (state.activeConversationId === id) {
      set({ activeConversationId: null, items: [] });
    }
    await state.refreshConfig();
  },

  async setActiveModel(providerId, model) {
    const id = get().activeConversationId;
    if (!id) {
      // draft page: stage the pick for the chat that send() will create
      set({ draftModel: model });
      return;
    }
    await api.conversationSetModel(id, providerId, model);
    await get().refreshConfig();
  },

  async setActiveEffort(effort) {
    const id = get().activeConversationId;
    if (!id) {
      set({ draftEffort: effort });
      return;
    }
    await api.conversationSetEffort(id, effort);
    await get().refreshConfig();
  },

  async setActiveMcpIds(mcpIds) {
    const id = get().activeConversationId;
    if (!id) return;
    await api.conversationSetMcpIds(id, mcpIds);
    await get().refreshConfig();
  },

  async send(text) {
    // slash commands never reach the model as chat text — and unlike real
    // messages they are never queued behind a running turn
    const command = parseSlashCommand(text);
    if (command) {
      if (command.unknown) {
        get().toast("error", `Unknown command: /${command.name}`);
        return;
      }
      switch (command.name) {
        case "init": {
          const workingDir = get().config?.settings.working_dir?.trim() || "~";
          await get().send(expandInitPrompt(workingDir, command.args));
          return;
        }
        case "compact": {
          const id = get().activeConversationId;
          if (!id) {
            get().toast("error", "Open a conversation first — /compact needs a chat.");
            return;
          }
          if (get().busyConversationIds.has(id)) {
            get().toast("error", "Wait for the response to finish before compacting.");
            return;
          }
          await get().compactConversation(id, command.args || undefined);
          return;
        }
        case "undo": {
          const id = get().activeConversationId;
          if (!id) {
            get().toast("info", "Nothing to undo yet.");
            return;
          }
          if (get().busyConversationIds.has(id)) {
            get().toast("error", "Wait for the response to finish before undoing.");
            return;
          }
          await get().undoConversation(id);
          return;
        }
      }
      return;
    }
    let id = get().activeConversationId;
    if (id && get().busyConversationIds.has(id)) {
      // mid-turn: hold the message, it is sent when the turn ends
      const convId = id;
      set((s) => ({
        messageQueues: {
          ...s.messageQueues,
          [convId]: [
            ...(s.messageQueues[convId] ?? []),
            {
              id: `u-queue-${++queueSeq}`,
              text,
              ts: new Date().toISOString(),
            },
          ],
        },
      }));
      return;
    }
    if (!id) {
      // lazy chat creation: the record is made only when the first message is
      // sent from the draft page
      if (creatingDraft) return;
      creatingDraft = true;
      try {
        const { config } = get();
        // an app-level default model wins over "whatever was used last"
        const defaultProvider = defaultProviderOf(config);
        const provider = defaultProvider ?? get().activeProvider();
        if (!provider) {
          get().toast(
            "error",
            "Add an AI provider first (Settings → Providers).",
          );
          return;
        }
        const model = resolveDraftModel(get().draftModel, config, provider);
        const meta = await api.conversationCreate(provider.id, model);
        const { draftEffort } = get();
        if (draftEffort !== undefined) {
          // non-fatal: the chat proceeds with the default effort if this fails
          await api.conversationSetEffort(meta.id, draftEffort).catch(() => {});
        }
        set({ draftModel: null, draftEffort: undefined });
        await get().refreshConfig();
        id = meta.id;
      } catch (e) {
        get().toast("error", `Could not start a new chat: ${e}`);
        return;
      } finally {
        creatingDraft = false;
      }
    }
    const convId = id;
    if (get().activeConversationId !== convId) {
      // first message from the draft page: the new chat becomes the active one
      set({ activeConversationId: convId });
    }
    await dispatchSend(convId, text, set, get);
  },

  stop() {
    const id = get().activeConversationId;
    if (id) api.chatCancel(id).catch(() => {});
  },

  removeQueued(id) {
    const convId = get().activeConversationId;
    if (!convId) return;
    set((s) => ({
      messageQueues: {
        ...s.messageQueues,
        [convId]: (s.messageQueues[convId] ?? []).filter((m) => m.id !== id),
      },
    }));
  },

  async compactConversation(id, instructions) {
    if (get().compactingConversationIds.has(id)) return;
    set((s) => ({
      compactingConversationIds: new Set(s.compactingConversationIds).add(id),
    }));
    try {
      await api.conversationCompact(id, instructions);
      await get().reloadItems(id);
      get().toast("success", "Conversation compacted.");
    } catch (e) {
      get().toast("error", `Could not compact: ${e}`);
    } finally {
      set((s) => {
        const next = new Set(s.compactingConversationIds);
        next.delete(id);
        return { compactingConversationIds: next };
      });
    }
  },

  async undoConversation(id) {
    try {
      const outcome = await api.conversationUndo(id);
      await get().reloadItems(id);
      if (outcome.undone_text.trim()) {
        set((s) => ({
          restoredDrafts: { ...s.restoredDrafts, [id]: outcome.undone_text },
        }));
      }
      const files = outcome.reverted_files.length;
      const filesNote =
        files > 0 ? ` and reverted ${files} file${files === 1 ? "" : "s"}` : "";
      if (outcome.file_warning) {
        get().toast("info", `Undone${filesNote}. ${outcome.file_warning}`);
      } else {
        get().toast("success", `Undone${filesNote}.`);
      }
    } catch (e) {
      get().toast("error", `${e}`);
    }
  },

  async reloadItems(id) {
    try {
      const [, raw] = await api.conversationGet(id);
      if (get().activeConversationId !== id) return;
      const toolStates = new Map<string, ToolCallState>();
      for (const item of get().items) {
        if (item.kind === "tool") toolStates.set(item.id, item.state);
      }
      set({ items: rawToItems(raw, toolStates) });
    } catch (e) {
      get().toast("error", `Could not refresh the conversation: ${e}`);
    }
  },

  clearRestoredDraft(id) {
    set((s) => {
      const { [id]: _restored, ...restoredDrafts } = s.restoredDrafts;
      return { restoredDrafts };
    });
  },

  toggleTerminal(id) {
    const target = id ?? get().activeConversationId;
    if (!target) return;
    set((s) => {
      const next = new Set(s.terminalOpenIds);
      if (next.has(target)) next.delete(target);
      else next.add(target);
      return { terminalOpenIds: next };
    });
  },

  setTerminalHeight(height) {
    const min = 120;
    const max = Math.max(min, Math.round(window.innerHeight * 0.7));
    set({ terminalHeight: Math.min(max, Math.max(min, Math.round(height))) });
  },

  async respondApproval(requestId, decision) {
    set((s) => ({
      approvals: s.approvals.filter((a) => a.request_id !== requestId),
    }));
    await api.approvalRespond(requestId, decision);
  },

  async respondElicitation(requestId, action, content) {
    set((s) => ({
      elicitations: s.elicitations.filter((e) => e.request_id !== requestId),
    }));
    await api.elicitationRespond(requestId, action, content);
  },

  async respondSampling(requestId, approve) {
    set((s) => ({
      samplings: s.samplings.filter((x) => x.request_id !== requestId),
    }));
    await api.samplingRespond(requestId, approve);
  },

  async checkForUpdates(manual) {
    // dev builds have no installed app to replace
    if (import.meta.env.DEV) {
      if (manual)
        get().toast("info", "Update checks only run in installed builds.");
      return;
    }
    const { status } = get().update;
    if (status === "checking" || status === "downloading") return;
    set((s) => ({ update: { ...s.update, status: "checking" } }));
    try {
      const update = await updaterCheck();
      if (!update) {
        set((s) => ({ update: { ...s.update, status: "idle" } }));
        if (manual) get().toast("success", "Ducky is up to date.");
        return;
      }
      pendingUpdate = update;
      const known = get().update.version === update.version;
      set((s) => ({
        update: {
          ...s.update,
          status: "available",
          version: update.version,
          notes: update.body ?? null,
          downloaded: 0,
          contentLength: null,
          // a periodic re-check must not resurrect a dismissed prompt;
          // a manual check always shows it again
          promptDismissed: manual || !known ? false : s.update.promptDismissed,
        },
      }));
      if (get().config?.settings.update_mode === "auto") {
        await get().downloadUpdate();
      }
    } catch (e) {
      set((s) => ({ update: { ...s.update, status: "idle" } }));
      if (manual) get().toast("error", `Could not check for updates: ${e}`);
      else console.warn("update check failed", e);
    }
  },

  async downloadUpdate() {
    const update = pendingUpdate;
    if (!update) return;
    set((s) => ({
      update: {
        ...s.update,
        status: "downloading",
        downloaded: 0,
        contentLength: null,
        promptDismissed: false,
      },
    }));
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          set((s) => ({
            update: {
              ...s.update,
              contentLength: event.data.contentLength ?? null,
            },
          }));
        } else if (event.event === "Progress") {
          set((s) => ({
            update: {
              ...s.update,
              downloaded: s.update.downloaded + event.data.chunkLength,
            },
          }));
        }
      });
      set((s) => ({ update: { ...s.update, status: "ready" } }));
    } catch (e) {
      // back to "available" so the user can retry from the prompt
      set((s) => ({ update: { ...s.update, status: "available" } }));
      get().toast("error", `Could not download the update: ${e}`);
    }
  },

  async restartForUpdate() {
    try {
      await relaunch();
    } catch (e) {
      get().toast("error", `Could not restart: ${e}`);
    }
  },

  dismissUpdate() {
    set((s) => ({ update: { ...s.update, promptDismissed: true } }));
  },

  activeProvider() {
    const { config, activeConversationId } = get();
    if (!config) return null;
    if (activeConversationId) {
      const conv = config.conversations.find(
        (c) => c.id === activeConversationId,
      );
      const provider = config.providers.find((p) => p.id === conv?.provider_id);
      if (provider) return provider;
    }
    return defaultProviderOf(config) ?? config.providers[0] ?? null;
  },
}));

// ---------------------------------------------------------------------------
// Backend event handling
// ---------------------------------------------------------------------------

/** The provider whose model new chats start with, if it still exists. */
function defaultProviderOf(config: AppConfig | null): ProviderConfig | null {
  const id = config?.settings.default_provider_id;
  if (!id || !config) return null;
  return config.providers.find((p) => p.id === id) ?? null;
}

type SetFn = typeof useStore.setState;
type GetFn = () => StoreState;

// Streamed text is batched per animation frame: the provider emits one chunk
// per few characters, and applying each one straight to the store re-rendered
// (and re-parsed) the whole conversation per chunk — quadratic in response
// length, worst on markdown tables.
let pending: { convId: string; text: string; reasoning: string } | null = null;
let pendingFrame = 0;

const scheduleFrame: (cb: () => void) => number =
  typeof requestAnimationFrame === "function"
    ? (cb) => requestAnimationFrame(cb)
    : (cb) => setTimeout(cb, 0);

function enqueueDelta(
  convId: string,
  text: string,
  reasoning: string,
  set: SetFn,
  get: GetFn,
) {
  if (pending && pending.convId !== convId) flushPending(set, get);
  pending ??= { convId, text: "", reasoning: "" };
  pending.text += text;
  pending.reasoning += reasoning;
  if (!pendingFrame) pendingFrame = scheduleFrame(() => flushPending(set, get));
}

/** Applied synchronously by any non-delta event: text streamed so far must land
 * before whatever the event does, or it would append after a tool card or a
 * finished message. */
function flushPending(set: SetFn, get: GetFn) {
  pendingFrame = 0;
  const p = pending;
  pending = null;
  if (!p || p.convId !== get().activeConversationId) return;
  set((s) => {
    const items = [...s.items];
    const last = items[items.length - 1];
    if (last?.kind === "assistant" && last.streaming) {
      items[items.length - 1] = {
        ...last,
        text: last.text + p.text,
        reasoning: p.reasoning
          ? (last.reasoning ?? "") + p.reasoning
          : last.reasoning,
      };
    } else {
      items.push({
        kind: "assistant",
        id: `a-live-${Date.now()}`,
        text: p.text,
        ts: new Date().toISOString(),
        reasoning: p.reasoning || undefined,
        streaming: true,
      });
    }
    return { items };
  });
}

// --- message queue ------------------------------------------------------------
// Messages submitted mid-turn live in messageQueues and are sent FIFO, one per
// turn end (message_done / chat_error), via drainQueue.

let queueSeq = 0;

/** Starts a turn for `convId`. Transcript updates, the `streaming` mirror and
 * the title flag apply only to the active conversation — a background drain
 * (queue firing in a chat the user switched away from) must not touch the
 * visible transcript. */
async function dispatchSend(
  convId: string,
  text: string,
  set: SetFn,
  get: GetFn,
) {
  const active = convId === get().activeConversationId;
  if (active) {
    const untitled =
      get().items.every((i) => i.kind !== "user") &&
      !get().config?.conversations.find((c) => c.id === convId)?.title;
    set((s) => ({
      items: [
        ...s.items,
        {
          kind: "user" as const,
          id: `u-live-${Date.now()}`,
          text,
          ts: new Date().toISOString(),
        },
      ],
      streaming: true,
      busyConversationIds: new Set([...s.busyConversationIds, convId]),
      titleGeneratingIds: untitled
        ? new Set([...s.titleGeneratingIds, convId])
        : s.titleGeneratingIds,
    }));
  } else {
    set((s) => ({
      busyConversationIds: new Set([...s.busyConversationIds, convId]),
    }));
  }
  try {
    await api.chatSend(convId, text);
  } catch (e) {
    get().toast("error", `${e}`);
    set((s) => {
      const busyConversationIds = new Set(s.busyConversationIds);
      busyConversationIds.delete(convId);
      const titleGeneratingIds = new Set(s.titleGeneratingIds);
      titleGeneratingIds.delete(convId);
      return {
        streaming: active ? false : s.streaming,
        busyConversationIds,
        titleGeneratingIds,
      };
    });
    // the turn never started, so no message_done will arrive to drain the rest
    drainQueue(convId, set, get);
  }
}

/** Sends the next queued message for `convId`, if any. One per call — the
 * sent message re-marks the conversation busy, so the next one waits for that
 * turn's message_done. */
function drainQueue(convId: string, set: SetFn, get: GetFn) {
  const queue = get().messageQueues[convId];
  if (!queue?.length) return;
  const [next, ...rest] = queue;
  set({ messageQueues: { ...get().messageQueues, [convId]: rest } });
  void dispatchSend(convId, next.text, set, get);
}

function handleEvent(event: BackendEvent, set: SetFn, get: GetFn) {
  if (event.type !== "chat_delta" && event.type !== "reasoning_delta") {
    flushPending(set, get);
  }
  switch (event.type) {
    case "chat_delta": {
      if (event.conversation_id !== get().activeConversationId) return;
      enqueueDelta(event.conversation_id, event.text, "", set, get);
      break;
    }
    case "reasoning_delta": {
      if (event.conversation_id !== get().activeConversationId) return;
      enqueueDelta(event.conversation_id, "", event.text, set, get);
      break;
    }
    case "message_done": {
      const active = event.conversation_id === get().activeConversationId;
      set((s) => {
        const busy = new Set(s.busyConversationIds);
        busy.delete(event.conversation_id);
        if (!active) {
          // a turn ending in the background still clears its busy flag and
          // may fire the next queued message — but never the visible chat
          return { busyConversationIds: busy };
        }
        const items = s.items.map((item) =>
          item.kind === "assistant" && item.streaming
            ? { ...item, streaming: false }
            : item,
        );
        return { items, streaming: false, busyConversationIds: busy };
      });
      if (active) {
        get()
          .refreshConfig()
          .catch(() => {});
      }
      drainQueue(event.conversation_id, set, get);
      break;
    }
    case "usage": {
      set((s) => ({
        usageByConversation: {
          ...s.usageByConversation,
          [event.conversation_id]: {
            input: event.input ?? undefined,
            output: event.output ?? undefined,
          },
        },
      }));
      break;
    }
    case "chat_error": {
      const active = event.conversation_id === get().activeConversationId;
      if (!active) get().toast("error", event.error);
      set((s) => {
        const busy = new Set(s.busyConversationIds);
        busy.delete(event.conversation_id);
        if (!active) return { busyConversationIds: busy };
        const items = [...s.items];
        const last = items[items.length - 1];
        if (last?.kind === "assistant" && last.streaming) {
          items[items.length - 1] = {
            ...last,
            streaming: false,
            error: event.error,
          };
        } else {
          items.push({
            kind: "assistant",
            id: `a-err-${Date.now()}`,
            text: "",
            ts: new Date().toISOString(),
            error: event.error,
          });
        }
        return { items, streaming: false, busyConversationIds: busy };
      });
      drainQueue(event.conversation_id, set, get);
      break;
    }
    case "tool_call_update": {
      if (event.conversation_id !== get().activeConversationId) return;
      set((s) => {
        const items = [...s.items];
        const idx = items.findIndex(
          (item) => item.kind === "tool" && item.id === event.tool_call_id,
        );
        const state: ToolCallState = {
          ...(idx >= 0 ? (items[idx] as ToolItem).state : {}),
          ...stripEvent(event),
        };
        if (event.status === "running") {
          // a fresh attempt invalidates progress from any previous task run
          delete state.task;
        }
        if (idx >= 0) {
          items[idx] = { kind: "tool", id: event.tool_call_id, state };
        } else {
          items.push({ kind: "tool", id: event.tool_call_id, state });
        }
        return { items };
      });
      break;
    }
    case "task_update": {
      // Live progress for a server-side task during a tool call; the tool
      // call's own status is driven by tool_call_update events.
      if (!event.tool_call_id) return;
      if (
        event.conversation_id &&
        event.conversation_id !== get().activeConversationId
      )
        return;
      set((s) => {
        const items = [...s.items];
        const idx = items.findIndex(
          (item) => item.kind === "tool" && item.id === event.tool_call_id,
        );
        if (idx < 0) return {};
        const item = items[idx] as ToolItem;
        items[idx] = {
          ...item,
          state: {
            ...item.state,
            task: {
              task_id: event.task_id,
              status: event.status,
              status_message: event.status_message,
            },
          },
        };
        return { items };
      });
      break;
    }
    case "approval_requested": {
      set((s) => ({
        approvals: [
          ...s.approvals,
          {
            request_id: event.request_id,
            server_title: event.server_title,
            tool: event.tool,
            args: event.args,
            read_only_hint: event.read_only_hint,
          },
        ],
      }));
      break;
    }
    case "elicitation_requested": {
      set((s) => ({
        elicitations: [
          ...s.elicitations,
          {
            request_id: event.request_id,
            server_title: event.server_title,
            mode: event.mode,
            message: event.message,
            schema: event.schema,
            url: event.url,
          },
        ],
      }));
      break;
    }
    case "sampling_requested": {
      set((s) => ({
        samplings: [
          ...s.samplings,
          {
            request_id: event.request_id,
            server_title: event.server_title,
            system_prompt: event.system_prompt,
            messages: event.messages,
            max_tokens: event.max_tokens,
          },
        ],
      }));
      break;
    }
    case "server_status": {
      set((s) => ({
        servers: s.servers.map((srv) =>
          srv.id === event.server_id
            ? {
                ...srv,
                status: statusFromEvent(
                  event.status,
                  event.detail,
                  event.reason,
                ),
              }
            : srv,
        ),
      }));
      if (event.status === "error" && event.detail) {
        get().toast("error", event.detail);
      }
      break;
    }
    case "server_data_changed": {
      const { server_id, what } = event;
      if (what.startsWith("log:")) {
        const line = what.slice(4);
        set((s) => ({
          servers: s.servers.map((srv) =>
            srv.id === server_id
              ? { ...srv, logs: [...srv.logs, line].slice(-300) }
              : srv,
          ),
        }));
      } else {
        get()
          .refreshServer(server_id)
          .catch(() => {});
      }
      break;
    }
    case "progress":
    case "resource_updated":
      // surfaced via the connector inspector on refresh; no global state needed
      break;
    case "terminal_output":
    case "terminal_closed":
      // high-frequency / panel-local: handled by TerminalPanel via the bus
      dispatchTerminalEvent(event);
      break;
    case "title_generating": {
      set((s) => {
        const titleGeneratingIds = new Set(s.titleGeneratingIds);
        titleGeneratingIds.add(event.conversation_id);
        return { titleGeneratingIds };
      });
      break;
    }
    case "title_updated": {
      set((s) => {
        const titleGeneratingIds = new Set(s.titleGeneratingIds);
        titleGeneratingIds.delete(event.conversation_id);
        const config = s.config
          ? {
              ...s.config,
              conversations: s.config.conversations.map((c) =>
                c.id === event.conversation_id
                  ? { ...c, title: event.title }
                  : c,
              ),
            }
          : s.config;
        return { titleGeneratingIds, config };
      });
      break;
    }
  }
}

function stripEvent(
  event: Extract<BackendEvent, { type: "tool_call_update" }>,
): ToolCallState {
  return {
    tool_call_id: event.tool_call_id,
    status: event.status,
    server: event.server,
    server_title: event.server_title,
    tool: event.tool,
    args: event.args,
    result_text: event.result_text,
    structured: event.structured,
    content: event.content,
    is_error: event.is_error,
  };
}

function statusFromEvent(
  status: string,
  detail?: string,
  reason?: AuthReason,
): ServerSummary["status"] {
  switch (status) {
    case "connected":
      return "connected";
    case "connecting":
      return "connecting";
    case "needs_auth":
      return { needs_auth: { detail: detail ?? null, reason: reason ?? null } };
    case "error":
      return { error: { message: detail ?? "Connection failed" } };
    default:
      return "disconnected";
  }
}
