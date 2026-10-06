// Thin wrappers over the Tauri command surface.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentMode,
  AppConfig,
  BackendEvent,
  CatalogEntry,
  ChatGroup,
  ClonedSubagent,
  ConnectorSuggestion,
  ConversationMeta,
  EffortLevel,
  GroupLayout,
  MarketplaceInput,
  MarketplaceSummary,
  McpServerConfig,
  PluginDetail,
  PluginInstallRequest,
  PluginSettings,
  PluginSummary,
  PluginUpdateInfo,
  SkillSummary,
  ToolDetailsMode,
  UpdatePolicy,
  ProviderConfig,
  ProviderPreset,
  RawMessage,
  ServerSummary,
  SubagentConfig,
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
  default_mode?: AgentMode;
  title_provider_id?: string;
  title_model?: string;
  /** null clears title effort; undefined leaves it unchanged. */
  title_effort?: EffortLevel | null;
  update_mode?: "prompt" | "auto";
  update_check_interval_hours?: number;
  system_prompt?: string;
  /** Replaces the whole `plugins` object; send the current value with one field changed. */
  plugins?: PluginSettings;
}) => invoke<void>("settings_set", { settings });

export const toolRuleSet = (key: string, rule: ToolRule | null) =>
  invoke<void>("tool_rule_set", { key, rule });

// ---------------------------------------------------------------------------
// Subagents
// ---------------------------------------------------------------------------

export const subagentAdd = (def: SubagentConfig) =>
  invoke<SubagentConfig>("subagent_add", { def });

export const subagentUpdate = (def: SubagentConfig) =>
  invoke<SubagentConfig>("subagent_update", { def });

export const subagentRemove = (id: string) =>
  invoke<void>("subagent_remove", { id });

/** Re-insert missing default subagents (never overwrites existing entries). */
export const subagentRestoreDefaults = () =>
  invoke<number>("subagent_restore_defaults");

/** Copy one plugin-provided subagent into the user's own definitions. */
export const subagentCloneFromPlugin = (pluginId: string, name: string) =>
  invoke<ClonedSubagent>("subagent_clone_from_plugin", { pluginId, name });

// ---------------------------------------------------------------------------
// Conversations & chat
// ---------------------------------------------------------------------------

export const conversationCreate = (
  providerId: string,
  model: string,
  groupId?: string | null,
) => invoke<ConversationMeta>("conversation_create", { providerId, model, groupId });

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

export const conversationSetMode = (id: string, mode: AgentMode) =>
  invoke<void>("conversation_set_mode", { id, mode });

export const conversationSetMcpIds = (id: string, mcpIds: string[] | null) =>
  invoke<void>("conversation_set_mcp_ids", { id, mcpIds });

export const groupCreate = (title?: string) =>
  invoke<ChatGroup>("group_create", { title: title ?? null });

export const groupRename = (id: string, title: string) =>
  invoke<void>("group_rename", { id, title });

export const groupSetCollapsed = (id: string, collapsed: boolean) =>
  invoke<void>("group_set_collapsed", { id, collapsed });

export const groupSetColor = (id: string, color: string) =>
  invoke<void>("group_set_color", { id, color });

export const groupDelete = (id: string) => invoke<void>("group_delete", { id });

export const groupApplyLayout = (groups: GroupLayout[]) =>
  invoke<void>("group_apply_layout", { groups });

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

export const chatSteer = (conversationId: string, id: string, text: string) =>
  invoke<void>("chat_steer", { conversationId, id, text });

export const chatUnsteer = (conversationId: string, id: string) =>
  invoke<void>("chat_unsteer", { conversationId, id });

export const chatCancel = (conversationId: string) =>
  invoke<void>("chat_cancel", { conversationId });

/** `/compact`: replace the conversation's history with a summary. */
export const conversationCompact = (
  conversationId: string,
  instructions?: string,
) =>
  invoke<void>("conversation_compact", {
    conversationId,
    instructions: instructions || null,
  });

export interface UndoOutcome {
  /** The removed user message, for the composer to restore. */
  undone_text: string;
  /** Repository-relative paths restored or deleted by the file revert. */
  reverted_files: string[];
  /** How many transcript messages were removed. */
  truncated: number;
  /** Set when the messages were removed but files could not be reverted. */
  file_warning: string | null;
}

/** `/undo`: drop the last user turn and revert its file changes. */
export const conversationUndo = (conversationId: string) =>
  invoke<UndoOutcome>("conversation_undo", { conversationId });

/** Remove a selected raw user-message index and later messages, then restore its files. */
export const conversationTruncate = (
  conversationId: string,
  messageIndex: number,
) =>
  invoke<UndoOutcome>("conversation_truncate", {
    conversationId,
    messageIndex,
  });

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

export const planRespond = (
  requestId: string,
  decision: "approve" | "revise",
  feedback?: string,
) =>
  invoke<boolean>("plan_respond", {
    response: {
      request_id: requestId,
      decision,
      feedback: feedback || null,
    },
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

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

/** Installed plugins, their enabled state and their contributed skills. */
export const pluginsList = () => invoke<PluginSummary[]>("plugins_list");

/**
 * Every skill the model can use, in precedence order, with provenance and
 * shadowing. The system-prompt block is built from the same list.
 */
export const skillsList = () => invoke<SkillSummary[]>("skills_list");

/**
 * One plugin, full component and trust detail. Resolves to `null` when the id
 * is not in the index; an unsafe id rejects with an error — the two are not
 * the same and a caller must not render "not found" for a rejection.
 */
export const pluginDetail = (id: string) =>
  invoke<PluginDetail | null>("plugin_detail", { id });

/** Install from a marketplace entry (`marketplace` + `name`) or a `source`. */
export const pluginInstall = (req: PluginInstallRequest) =>
  invoke<PluginDetail>("plugin_install", req);

/** Uninstall; returns the `plugin:<id>:<server>` ids the package owned. */
export const pluginUninstall = (id: string, deleteData: boolean) =>
  invoke<string[]>("plugin_uninstall", { id, deleteData });

export const pluginSetEnabled = (id: string, enabled: boolean) =>
  invoke<void>("plugin_set_enabled", { id, enabled });

export const pluginSetServerEnabled = (
  id: string,
  server: string,
  enabled: boolean,
) => invoke<void>("plugin_set_server_enabled", { id, server, enabled });

export const pluginSetUpdatePolicy = (id: string, policy: UpdatePolicy) =>
  invoke<void>("plugin_set_update_policy", { id, policy });

export const pluginCheckUpdates = () =>
  invoke<PluginUpdateInfo[]>("plugin_check_updates");

export const pluginUpdate = (id: string, force = false) =>
  invoke<PluginUpdateInfo>("plugin_update", { id, force });

export const pluginUpdateAll = (force = false) =>
  invoke<PluginUpdateInfo[]>("plugin_update_all", { force });

export const pluginRollback = (id: string) =>
  invoke<PluginUpdateInfo>("plugin_rollback", { id });

/** Reveal the package (`which = "package"`, the default) or data directory. */
export const pluginOpenFolder = (id: string, which: "package" | "data" = "package") =>
  invoke<void>("plugin_open_folder", { id, which });

/** Reveal the plugins folder that holds every installed package. */
export const pluginOpenRootFolder = () =>
  invoke<void>("plugin_open_folder", { id: null, which: "root" });

export const marketplacesList = () =>
  invoke<MarketplaceSummary[]>("marketplaces_list");

export const marketplaceAdd = (input: MarketplaceInput, name?: string) =>
  invoke<MarketplaceSummary>("marketplace_add", { input, name });

export const marketplaceRemove = (id: string, confirm = false) =>
  invoke<void>("marketplace_remove", { id, confirm });

/** Refresh one marketplace, or every one when `id` is null. */
export const marketplaceRefresh = (id: string | null = null) =>
  invoke<MarketplaceSummary[]>("marketplace_refresh", { id });

export const marketplaceSetAutoRefresh = (id: string, enabled: boolean) =>
  invoke<void>("marketplace_set_auto_refresh", { id, enabled });

/** Normalised registry entries plus install state; null = every marketplace. */
export const marketplaceCatalog = (marketplace: string | null = null) =>
  invoke<CatalogEntry[]>("marketplace_catalog", { marketplace });
