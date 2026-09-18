// Thin wrappers over the Tauri command surface.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppConfig,
  BackendEvent,
  ConnectorSuggestion,
  ConversationMeta,
  EffortLevel,
  McpServerConfig,
  ToolDetailsMode,
  ProviderConfig,
  ProviderPreset,
  RawMessage,
  ServerSummary,
  TerminalCreated,
  ToolRule,
} from "./types";

export const EVENT_CHANNEL = "backend://event";

export async function listenBackend(
  handler: (event: BackendEvent) => void,
): Promise<UnlistenFn> {
  return listen<BackendEvent>(EVENT_CHANNEL, (e) => handler(e.payload));
}

// ---------------------------------------------------------------------------
// Bootstrap / config
// ---------------------------------------------------------------------------

export interface Bootstrap {
  config: AppConfig;
  server_summaries: ServerSummary[];
  presets: ProviderPreset[];
  suggestions: ConnectorSuggestion[];
  /** The machine's home directory; the default working directory. */
  home_dir: string;
}

export const getBootstrap = () => invoke<Bootstrap>("get_bootstrap");
export const getConfig = () => invoke<AppConfig>("get_config");
export const appInfo = () =>
  invoke<{ name: string; version: string; mcp_spec: string }>("app_info");

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

export interface NewProvider {
  kind: string;
  name: string;
  base_url: string;
  api_type: "open_ai" | "anthropic";
  api_key?: string;
}

export const providerAdd = (provider: NewProvider) =>
  invoke<ProviderConfig>("provider_add", { provider });

export const providerUpdate = (update: {
  id: string;
  name?: string;
  base_url?: string;
  default_model?: string;
  api_key?: string;
}) => invoke<void>("provider_update", { update });

export const providerDelete = (id: string) =>
  invoke<void>("provider_delete", { id });

export const providerTest = (id: string) =>
  invoke<string[]>("provider_test", { id });

export const providerHasKey = (id: string) =>
  invoke<boolean>("provider_has_key", { id });

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

export const settingsSet = (settings: {
  theme?: "system" | "light" | "dark";
  tool_approval?: "always_ask" | "auto_approve_read_only" | "auto_approve_all";
  sampling?: "ask" | "auto_approve" | "deny";
  max_tool_iterations?: number;
  show_reasoning?: boolean;
  tool_details?: ToolDetailsMode;
  roots?: string[];
  working_dir?: string | null;
  default_provider_id?: string;
  default_model?: string;
  /** null clears the default effort; undefined leaves it unchanged. */
  default_effort?: EffortLevel | null;
  title_provider_id?: string;
  title_model?: string;
  /** null clears title effort; undefined leaves it unchanged. */
  title_effort?: EffortLevel | null;
  update_mode?: "prompt" | "auto";
  update_check_interval_hours?: number;
}) => invoke<void>("settings_set", { settings });

export const toolRuleSet = (key: string, rule: ToolRule | null) =>
  invoke<void>("tool_rule_set", { key, rule });

// ---------------------------------------------------------------------------
// Conversations & chat
// ---------------------------------------------------------------------------

export const conversationCreate = (providerId: string, model: string) =>
  invoke<ConversationMeta>("conversation_create", { providerId, model });

export const conversationDelete = (id: string) =>
  invoke<void>("conversation_delete", { id });

export const conversationRename = (id: string, title: string) =>
  invoke<void>("conversation_rename", { id, title });

export const conversationGenerateTitle = (id: string) =>
  invoke<void>("conversation_generate_title", { id });

export const conversationCancelTitle = (id: string) =>
  invoke<void>("conversation_cancel_title", { id });

export const conversationSetModel = (
  id: string,
  providerId: string,
  model: string,
) => invoke<void>("conversation_set_model", { id, providerId, model });

export const conversationSetEffort = (id: string, effort: EffortLevel | null) =>
  invoke<void>("conversation_set_effort", { id, effort });

export const conversationSetMcpIds = (id: string, mcpIds: string[] | null) =>
  invoke<void>("conversation_set_mcp_ids", { id, mcpIds });

/** Effort levels the model supports per models.dev; empty = hide the selector. */
export const effortLevels = (kind: string, model: string) =>
  invoke<EffortLevel[]>("effort_levels", { kind, model });

/** Context window in tokens from models.dev; null if unknown. */
export const contextLimit = (kind: string, model: string) =>
  invoke<number | null>("context_limit", { kind, model });

export const conversationGet = (
  id: string,
): Promise<[ConversationMeta, RawMessage[]]> =>
  invoke("conversation_get", { id });

export const chatSend = (conversationId: string, text: string) =>
  invoke<void>("chat_send", { conversationId, text });

export const chatCancel = (conversationId: string) =>
  invoke<void>("chat_cancel", { conversationId });

// ---------------------------------------------------------------------------
// Per-conversation terminal
// ---------------------------------------------------------------------------

export const terminalCreate = (conversationId: string) =>
  invoke<TerminalCreated>("terminal_create", { conversationId });

export const terminalWrite = (conversationId: string, data: string) =>
  invoke<void>("terminal_write", { conversationId, data });

export const terminalResize = (
  conversationId: string,
  cols: number,
  rows: number,
) => invoke<void>("terminal_resize", { conversationId, cols, rows });

export const terminalClose = (conversationId: string) =>
  invoke<void>("terminal_close", { conversationId });

// ---------------------------------------------------------------------------
// Interactive responses
// ---------------------------------------------------------------------------

export const approvalRespond = (
  requestId: string,
  decision: "allow_once" | "always_allow" | "deny",
) =>
  invoke<boolean>("approval_respond", {
    response: { request_id: requestId, decision },
  });

export const elicitationRespond = (
  requestId: string,
  action: "accept" | "decline" | "cancel",
  content?: any,
) =>
  invoke<boolean>("elicitation_respond", {
    response: { request_id: requestId, action, content },
  });

export const samplingRespond = (requestId: string, approve: boolean) =>
  invoke<boolean>("sampling_respond", { requestId, approve });

// ---------------------------------------------------------------------------
// MCP connectors
// ---------------------------------------------------------------------------

export const mcpAdd = (server: {
  name: string;
  transport: any;
  auth?: any;
  enabled?: boolean;
}) => invoke<McpServerConfig>("mcp_add", { server });

export const mcpUpdate = (server: McpServerConfig) =>
  invoke<void>("mcp_update", { server });

export const mcpRemove = (id: string) => invoke<void>("mcp_remove", { id });

export const mcpSetEnabled = (id: string, enabled: boolean) =>
  invoke<void>("mcp_set_enabled", { id, enabled });

export const mcpConnect = (id: string) => invoke<string>("mcp_connect", { id });

export const mcpDisconnect = (id: string) =>
  invoke<void>("mcp_disconnect", { id });

export const mcpSummaries = () => invoke<ServerSummary[]>("mcp_summaries");

export const mcpSummary = (id: string) =>
  invoke<ServerSummary>("mcp_summary", { id });

export const mcpRefresh = (id: string) =>
  invoke<ServerSummary>("mcp_refresh", { id });

export const mcpReadResource = (serverId: string, uri: string) =>
  invoke<any>("mcp_read_resource", { serverId, uri });

export const mcpGetPrompt = (
  serverId: string,
  name: string,
  args: Record<string, string>,
) => invoke<any>("mcp_get_prompt", { serverId, name, arguments: args });

export const mcpSubscribeResource = (serverId: string, uri: string) =>
  invoke<void>("mcp_subscribe_resource", { serverId, uri });

export const mcpUnsubscribeResource = (serverId: string, uri: string) =>
  invoke<void>("mcp_unsubscribe_resource", { serverId, uri });

export const mcpImportPreview = (text: string) =>
  invoke<{ name: string; transport: any }[]>("mcp_import_preview", { text });

export const mcpImportAdd = (servers: { name: string; transport: any }[]) =>
  invoke<number>("mcp_import_add", { servers });

export const mcpOauthLogin = (id: string) =>
  invoke<void>("mcp_oauth_login", { id });

export const mcpOauthLogout = (id: string) =>
  invoke<void>("mcp_oauth_logout", { id });

export const mcpSetBearerToken = (id: string, token: string) =>
  invoke<void>("mcp_set_bearer_token", { id, token });

export const mcpHasAuth = (id: string) =>
  invoke<string>("mcp_has_auth", { id });

export const mcpSetOauthConfig = (
  id: string,
  clientId: string | null,
  clientSecret: string | null,
  redirectPort: number | null,
) =>
  invoke<void>("mcp_set_oauth_config", {
    id,
    clientId,
    clientSecret,
    redirectPort,
  });
