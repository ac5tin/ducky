// Global app store: state, backend event wiring, and actions.
import { create } from "zustand";
import * as api from "./api";
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

// Shared init promise: concurrent init() calls (StrictMode double-effect) must
// not register a second backend listener, or every delta is handled twice.
let initPromise: Promise<void> | null = null;
// Lazy chat creation: the draft page has no record yet; this flag stops a
// double-submit during the create round-trip from spawning two records.
let creatingDraft = false;
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
  items: ChatItem[];
  streaming: boolean;
  busyConversationIds: Set<string>;
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

  init: () => Promise<void>;
  setView: (v: View) => void;

  toast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: string) => void;

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
  items: [],
  streaming: false,
  busyConversationIds: new Set(),
  titleGeneratingIds: new Set(),
  usageByConversation: {},

  terminalOpenIds: new Set(),
  terminalHeight: 300,

  approvals: [],
  elicitations: [],
  samplings: [],
  toasts: [],

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
    // is sent from it (see send()).
    set({ activeConversationId: null, items: [], view: "chat" });
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
      return { terminalOpenIds, titleGeneratingIds };
    });
    const state = get();
    if (state.activeConversationId === id) {
      set({ activeConversationId: null, items: [] });
    }
    await state.refreshConfig();
  },

  async setActiveModel(providerId, model) {
    const id = get().activeConversationId;
    if (!id) return;
    await api.conversationSetModel(id, providerId, model);
    await get().refreshConfig();
  },

  async setActiveEffort(effort) {
    const id = get().activeConversationId;
    if (!id) return;
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
    let id = get().activeConversationId;
    if (id && (get().streaming || get().busyConversationIds.has(id))) return;
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
        const model =
          (defaultProvider && config?.settings.default_model) ||
          provider.default_model ||
          provider.models[0] ||
          "";
        const meta = await api.conversationCreate(provider.id, model);
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
    const untitled =
      get().items.every((i) => i.kind !== "user") &&
      !get().config?.conversations.find((c) => c.id === convId)?.title;
    set((s) => ({
      items: [
        ...s.items,
        {
          kind: "user",
          id: `u-live-${Date.now()}`,
          text,
          ts: new Date().toISOString(),
        },
      ],
      activeConversationId: convId,
      streaming: true,
      busyConversationIds: new Set([...s.busyConversationIds, convId]),
      titleGeneratingIds: untitled
        ? new Set([...s.titleGeneratingIds, convId])
        : s.titleGeneratingIds,
    }));
    try {
      await api.chatSend(convId, text);
    } catch (e) {
      get().toast("error", `${e}`);
      set((s) => {
        const titleGeneratingIds = new Set(s.titleGeneratingIds);
        titleGeneratingIds.delete(convId);
        return { streaming: false, titleGeneratingIds };
      });
    }
  },

  stop() {
    const id = get().activeConversationId;
    if (id) api.chatCancel(id).catch(() => {});
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
      if (event.conversation_id !== get().activeConversationId) return;
      set((s) => {
        const items = s.items.map((item) =>
          item.kind === "assistant" && item.streaming
            ? { ...item, streaming: false }
            : item,
        );
        const busy = new Set(s.busyConversationIds);
        busy.delete(event.conversation_id);
        return { items, streaming: false, busyConversationIds: busy };
      });
      get()
        .refreshConfig()
        .catch(() => {});
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
      if (event.conversation_id !== get().activeConversationId) {
        get().toast("error", event.error);
        return;
      }
      set((s) => {
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
        const busy = new Set(s.busyConversationIds);
        busy.delete(event.conversation_id);
        return { items, streaming: false, busyConversationIds: busy };
      });
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
