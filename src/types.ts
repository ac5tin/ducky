// Shared types mirroring the Rust backend's serde JSON shapes.
// NOTE: struct payload fields are snake_case (serde default), while top-level
// invoke argument names are camelCase (Tauri converts them).

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export type ApiType = "open_ai" | "anthropic";
export type Theme = "system" | "light" | "dark";
export type ApprovalMode =
        | "always_ask"
        | "auto_approve_read_only"
        | "auto_approve_all";
/** How much the agent may change in a chat. */
export type AgentMode = "default" | "readonly" | "plan" | "auto";
export type SamplingMode = "ask" | "auto_approve" | "deny";
export type ToolRule = "allow" | "deny";
export type ToolDetailsMode = "auto" | "collapsed" | "expanded";
export type EffortLevel =
        | "none"
        | "minimal"
        | "low"
        | "medium"
        | "high"
        | "xhigh"
        | "max";

export interface ProviderConfig {
        id: string;
        kind: string;
        name: string;
        base_url: string;
        api_type: ApiType;
        default_model: string | null;
        models: string[];
        created_at: string;
}

export interface StdioTransport {
        type: "stdio";
        command: string;
        args: string[];
        env: Record<string, string>;
}

export interface HttpTransport {
        type: "http";
        url: string;
        headers: Record<string, string>;
}

export type McpTransport = StdioTransport | HttpTransport;

export type HttpAuth =
        | { type: "none" }
        | { type: "bearer"; token_ref: string }
        | { type: "oauth" };

export interface McpServerConfig {
        id: string;
        name: string;
        transport: McpTransport;
        auth: HttpAuth;
        enabled: boolean;
        auto_start: boolean;
        oauth_client_id: string | null;
        oauth_redirect_port: number | null;
        created_at: string;
}

export interface AppSettings {
        theme: Theme;
        tool_approval: ApprovalMode;
        sampling: SamplingMode;
        tool_rules: Record<string, ToolRule>;
        roots: string[];
        max_tool_iterations: number;
        show_reasoning: boolean;
        /** Default open/closed state of MCP tool input and output. */
        tool_details: ToolDetailsMode;
        /** Working directory for chats and spawned stdio servers; null = home. */
        working_dir: string | null;
        /** Provider new chats start with; null = keep current behavior. */
        default_provider_id: string | null;
        /** Model new chats start with, only used with default_provider_id. */
        default_model: string | null;
        /** Reasoning effort new chats start with; null = model default. */
        default_effort: EffortLevel | null;
        /** Mode new chats start in. */
        default_mode: AgentMode;
        /** Provider used to generate chat titles; null = the chat's provider. */
        title_provider_id: string | null;
        /** Model used to generate chat titles; only with title_provider_id. */
        title_model: string | null;
        /** Effort for title generation; null = model default (or the chat's, when title provider is unset). */
        title_effort: EffortLevel | null;
        /** What Ducky does when it detects a new GitHub release. */
        update_mode: "prompt" | "auto";
        /** Hours between update checks; 0 = check on startup only. */
        update_check_interval_hours: number;
        /** Custom prompt appended to the main agent's system message; empty = none. */
        system_prompt: string;
        /** Plugin settings (design §3). */
        plugins: PluginSettings;
}

export interface ConversationMeta {
        id: string;
        title: string;
        provider_id: string;
        model: string;
        effort: EffortLevel | null;
        mode: AgentMode;
        mcp_ids?: string[] | null;
        created_at: string;
        updated_at: string;
}

/** A reusable subagent definition (persona the model spawns via `agent`). */
export interface SubagentConfig {
        id: string;
        /** Display name; also the `agent` enum value. Unique case-insensitively. */
        name: string;
        /** When-to-use hint shown to the parent model. */
        description: string;
        /** Persona system prompt appended after the generic preamble. */
        system_prompt: string;
        /** Provider override; null = inherit the conversation's. */
        provider_id: string | null;
        /** Model override; only used together with provider_id. */
        model: string | null;
        /** Effort override; null = inherit the conversation's. */
        effort: EffortLevel | null;
        /** Tool allowlist entries (tool name, `server/tool`, or `server`/`server/*`); null = all tools. */
        tools: string[] | null;
        created_at: string;
}

/** `subagent_clone_from_plugin` result: the saved user definition plus the
 * mapping warnings (e.g. tools the plugin listed that this install lacks). */
export interface ClonedSubagent {
        subagent: SubagentConfig;
        warnings: string[];
}

/** A named, coloured group of chats. `conversation_ids` is both the member
 *  list and its display order — a chat belongs to the group that lists it. */
export interface ChatGroup {
        id: string;
        title: string;
        collapsed: boolean;
        /** Dot colour as `#rrggbb`, lowercase. */
        color: string;
        conversation_ids: string[];
}

/** The order-only payload for `group_apply_layout`. */
export interface GroupLayout {
        id: string;
        conversation_ids: string[];
}

export interface AppConfig {
        version: number;
        onboarding_complete: boolean;
        providers: ProviderConfig[];
        mcp_servers: McpServerConfig[];
        subagents: SubagentConfig[];
        /** Whether the default subagents have been seeded once. */
        subagents_seeded: boolean;
        settings: AppSettings;
        conversations: ConversationMeta[];
        groups: ChatGroup[];
}

export interface ProviderPreset {
        id: string;
        label: string;
        tagline: string;
        base_url: string;
        api_type: ApiType;
        needs_key: boolean;
        key_url: string;
        default_model: string;
}

export interface ConnectorSuggestion {
        name: string;
        description: string;
        command: string;
        args: string[];
}

// ---------------------------------------------------------------------------
// MCP server view
// ---------------------------------------------------------------------------

export type AuthReason = "missing" | "expired" | "scope";

export type ServerStatus =
        | "disconnected"
        | "connecting"
        | "connected"
        | { needs_auth: { detail: string | null; reason?: AuthReason | null } }
        | { error: { message: string } };

export interface ToolEntry {
        server_id: string;
        name: string;
        qualified_name: string;
        title: string | null;
        description: string | null;
        input_schema: any;
        output_schema: any | null;
        annotations: any | null;
        read_only_hint: boolean | null;
        icons: any | null;
}

/** Where a connector row came from: the user's own config or an enabled plugin. */
export interface ServerOrigin {
        kind: "user" | "plugin";
        plugin_id?: string;
        plugin_name?: string;
}

export interface ServerSummary {
        id: string;
        name: string;
        enabled: boolean;
        transport_kind: "stdio" | "http";
        detail: string;
        status: ServerStatus;
        tools: ToolEntry[];
        resources: any[];
        resource_templates: any[];
        prompts: any[];
        server_info: any | null;
        instructions: string | null;
        protocol_version: string | null;
        capabilities: any | null;
        logs: string[];
        /** User config or plugin-provided (`ServerOrigin` in `mcp/manager.rs`). */
        origin: ServerOrigin;
}

// ---------------------------------------------------------------------------
// Chat messages (persisted shape)
// ---------------------------------------------------------------------------

export interface ToolCall {
        id: string;
        name: string;
        arguments: any;
}

export type RawMessage =
        | { kind: "system"; text: string }
        | { kind: "user"; text: string; ts?: string | null }
        | {
                  kind: "assistant";
                  text: string;
                  tool_calls: ToolCall[];
                  ts?: string | null;
          }
        | {
                  kind: "tool_result";
                  call_id: string;
                  text: string;
                  is_error: boolean;
          };

/** A message submitted while its conversation was mid-turn; the backend
 * delivers it into the running turn at the next assistant-turn boundary. */
export interface SteeringMessage {
        id: string;
        text: string;
        ts: string;
}

// ---------------------------------------------------------------------------
// Backend events
// ---------------------------------------------------------------------------

export type TaskStatus =
        | "working"
        | "input_required"
        | "completed"
        | "failed"
        | "cancelled";

export interface ToolCallState {
        tool_call_id: string;
        status:
                | "pending_approval"
                | "running"
                | "awaiting_input"
                | "needs_auth"
                | "done"
                | "denied"
                | "error";
        server?: string;
        server_title?: string;
        tool?: string;
        args?: any;
        result_text?: string;
        structured?: any;
        content?: ContentBlockValue[];
        is_error?: boolean;
        /** Live progress of a server-side task (MCP tasks extension). */
        task?: {
                task_id: string;
                status: TaskStatus;
                status_message?: string | null;
        };
        /** Live transcript of a `ducky__subagent` run (session-only). */
        subagent_text?: string;
        /** Last tool the subagent is running, e.g. `ducky__fs_read · running`. */
        subagent_activity?: string;
        /** Inherited run config of a `ducky__subagent` spawn (session-only). */
        subagent?: {
                provider_id: string;
                model: string;
                effort?: string | null;
                /** Agent type the spawn resolved to; null = generic. */
                agent?: string | null;
        };
}

export type ContentBlockValue =
        | { type: "text"; text: string }
        | { type: "image"; data: string; mime_type?: string; mimeType?: string }
        | { type: "audio"; data: string; mime_type?: string }
        | {
                  type: "resource_link";
                  uri: string;
                  name?: string;
                  description?: string;
          }
        | {
                  type: "resource";
                  resource: {
                          uri: string;
                          text?: string;
                          blob?: string;
                          mime_type?: string;
                          mimeType?: string;
                  };
          }
        | { type: string; [k: string]: any };

/** A plan presented for review in plan mode. */
export interface PlanRequest {
        request_id: string;
        conversation_id: string | null;
        plan: string;
}

export type BackendEvent =
        | { type: "chat_delta"; conversation_id: string; text: string }
        | { type: "reasoning_delta"; conversation_id: string; text: string }
        | {
                  type: "subagent_delta";
                  conversation_id: string;
                  tool_call_id: string;
                  text: string;
          }
        | {
                  type: "steering_delivered";
                  conversation_id: string;
                  id: string;
                  text: string;
                  ts: string;
          }
        | { type: "message_done"; conversation_id: string; message_id: string }
        | { type: "chat_error"; conversation_id: string; error: string }
        | {
                  type: "usage";
                  conversation_id: string;
                  input?: number | null;
                  output?: number | null;
          }
        | ({
                  type: "tool_call_update";
                  conversation_id: string;
                  tool_call_id: string;
                  status: ToolCallState["status"];
                  /** Set when the call belongs to a subagent run: the
                   * `ducky__subagent` call that spawned it. */
                  parent_tool_call_id?: string;
          } & Partial<ToolCallState>)
        | {
                  type: "approval_requested";
                  request_id: string;
                  conversation_id?: string;
                  server: string;
                  server_title: string;
                  tool: string;
                  args: any;
                  read_only_hint?: boolean;
          }
        | {
                  type: "elicitation_requested";
                  request_id: string;
                  conversation_id?: string;
                  server: string;
                  server_title: string;
                  mode: "form" | "url";
                  message: string;
                  schema?: any;
                  url?: string;
          }
        | {
                  type: "sampling_requested";
                  request_id: string;
                  conversation_id?: string;
                  server: string;
                  server_title: string;
                  system_prompt?: string;
                  messages: any[];
                  max_tokens?: number;
          }
        | {
                  type: "plan_presented";
                  request_id: string;
                  conversation_id: string | null;
                  plan: string;
          }
        | { type: "mode_changed"; conversation_id: string; mode: AgentMode }
        | {
                  type: "server_status";
                  server_id: string;
                  status: string;
                  detail?: string;
                  /** For `needs_auth`: why sign-in is required. */
                  reason?: AuthReason;
          }
        | { type: "server_data_changed"; server_id: string; what: string }
        | {
                  type: "task_update";
                  conversation_id: string | null;
                  tool_call_id: string | null;
                  task_id: string;
                  status: TaskStatus;
                  status_message: string | null;
          }
        | {
                  type: "progress";
                  conversation_id?: string;
                  tool_call_id?: string;
                  progress: number;
                  total?: number;
                  message?: string;
          }
        | { type: "resource_updated"; server_id: string; uri: string }
        | {
                  type: "terminal_output";
                  conversation_id: string;
                  /** base64-encoded PTY bytes */
                  data: string;
                  seq: number;
          }
        | {
                  type: "terminal_closed";
                  conversation_id: string;
                  exit_code: number | null;
          }
        | { type: "title_generating"; conversation_id: string }
        | { type: "title_updated"; conversation_id: string; title: string }
        | { type: "plugins_changed"; reason: string }
        | {
                  type: "plugin_update_available";
                  plugin_id: string;
                  from: string | null;
                  to: string | null;
          }
        | ({
                  type: "plugin_progress";
          } & PluginProgressEvent)
        | {
                  type: "marketplace_refreshed";
                  marketplace_id: string;
                  error: string | null;
          };

// ---------------------------------------------------------------------------
// Terminal
// ---------------------------------------------------------------------------

/** Response of terminal_create: PTY is live, plus buffered output (base64). */
export interface TerminalCreated {
        running: boolean;
        scrollback: string;
        /** Live terminal_output events with seq < last_seq are already in scrollback. */
        last_seq: number;
}

// ---------------------------------------------------------------------------
// Schema types for elicitation forms
// ---------------------------------------------------------------------------

export interface ElicitationFieldSchema {
        type: string;
        title?: string;
        description?: string;
        format?: string;
        enum?: string[];
        oneOf?: { const: string; title: string }[];
        anyOf?: { const: string; title: string }[];
        minimum?: number;
        maximum?: number;
        minLength?: number;
        maxLength?: number;
        default?: any;
        items?: {
                type?: string;
                enum?: string[];
                anyOf?: { const: string; title: string }[];
        };
        minItems?: number;
        maxItems?: number;
}

export interface ElicitationSchemaShape {
        type?: string;
        properties?: Record<string, ElicitationFieldSchema>;
        required?: string[];
}

// ---------------------------------------------------------------------------
// Plugins and skills
// ---------------------------------------------------------------------------

/**
 * Where a plugin or marketplace came from (`PluginSource`). Serialized with
 * the tag `kind` and kebab-case variants; the git ref is written `ref`.
 *
 * `path`, `ref` and `sha` are `string | null`, **not** optional: the Rust
 * `PluginSource` declares them `Option<T>` without `#[serde(default)]`
 * (`src-tauri/src/plugins/marketplace.rs`), so serde requires the key and
 * rejects a payload that omits it with `missing field` — never a null
 * default. `plugin_install` deserializes this shape straight from the
 * webview, so a key that TypeScript lets you omit is a runtime rejection.
 */
export type PluginSource =
        | {
                  kind: "github";
                  repo: string;
                  path: string | null;
                  ref: string | null;
                  sha: string | null;
          }
        | {
                  kind: "git";
                  url: string;
                  path: string | null;
                  ref: string | null;
                  sha: string | null;
          }
        | {
                  kind: "git-subdir";
                  url: string;
                  path: string;
                  ref: string | null;
                  sha: string | null;
          }
        | { kind: "url"; url: string }
        | { kind: "path"; path: string }
        | { kind: "unsupported"; sourceKind: string; detail: string };

/** How the package is laid out on disk. */
export type PluginLayout = "agent-plugins" | "claude-code";

/** Update policy of one installed plugin (design §7). */
export type UpdatePolicy = "auto" | "manual";

/** Lifecycle state of one installed plugin (design §11). */
export type PluginStatus =
        | "installed_disabled"
        | "enabled"
        | "invalid"
        | "update_available"
        | "modified_locally"
        | "error";

/** Severity of one plugin diagnostic. */
export type PluginDiagnosticLevel = "error" | "warning" | "info";

/** One diagnosis about a plugin package or component. */
export interface PluginDiagnostic {
        level: PluginDiagnosticLevel;
        target: string;
        message: string;
}

/**
 * The update an update check found, cached on the install record. Serialized
 * camelCase: the SHA field is `resolvedSha`.
 */
export interface AvailableUpdate {
        version: string | null;
        resolvedSha: string | null;
}

/** One skill contributed by a plugin, as `plugins_list` reports it. */
export interface PluginSkillSummary {
        name: string;
        description: string;
        license?: string | null;
        /** Absolute path of the skill directory. */
        path: string;
}

/** One subagent contributed by a plugin, as `plugins_list` reports it. */
export interface PluginSubagentSummary {
        name: string;
        description: string;
        /** The registry key: `to_subagent_config` slugs the display name. */
        slug: string;
}

/**
 * One MCP server a plugin contributes. `url` is host[:port] only — the backend
 * never sends a query string, and env/header values never reach the webview.
 */
export interface PluginServerView {
        name: string;
        id: string;
        /** True only when the plugin is enabled and this server is consented. */
        enabled: boolean;
        /** `stdio`, `streamable-http` or `sse`. */
        transport: string;
        command: string | null;
        args: string[];
        url: string | null;
}

/**
 * One component a plugin contributes. The three shapes are told apart by the
 * fields they carry: `path` (skill), `slug` (subagent), `transport` (server).
 */
export type PluginComponent =
        | PluginSkillSummary
        | PluginSubagentSummary
        | PluginServerView;

/** One installed plugin as `plugins_list` reports it (`PluginSummary`). */
export interface PluginSummary {
        id: string;
        name: string;
        version: string | null;
        previous_version: string | null;
        marketplace: string | null;
        source: PluginSource;
        resolved_sha: string | null;
        resolved_sha256: string | null;
        layout: PluginLayout;
        enabled: boolean;
        update_policy: UpdatePolicy;
        disabled_servers: string[];
        status: PluginStatus;
        diagnostics: PluginDiagnostic[];
        available_update: AvailableUpdate | null;
        last_checked_at: string | null;
        installed_at: string;
        tree_hash: string | null;
        package_dir: string;
        data_dir: string;
        skills: PluginSkillSummary[];
        servers: PluginServerView[];
        subagents: PluginSubagentSummary[];
        /** The spec §6 trust text; every plugin surface renders this one copy. */
        trust_warning: string;
}

/** `plugin_detail`: the index entry plus the manifest's trust fields. */
export interface PluginDetail extends PluginSummary {
        description: string | null;
        author: string | null;
        homepage: string | null;
        license: string | null;
}

/** What an install sends: a marketplace entry or a direct source. */
export type PluginInstallRequest = {
        marketplace?: string | null;
        name?: string | null;
        source?: PluginSource | null;
};

/** One applied update or rollback. */
export interface PluginUpdateInfo {
        plugin_id: string;
        from: string | null;
        to: string | null;
        /**
         * `plugin:<id>:<server>` ids that were enabled before the swap, so the
         * frontend can restart the servers the swap stopped (design §7).
         */
        enabled_servers: string[];
}

/** `marketplaces_list` / `marketplace_add` / `marketplace_refresh`. */
export interface MarketplaceSummary {
        id: string;
        name: string;
        source: PluginSource;
        registry_path: string;
        auto_refresh: boolean;
        last_refreshed_at: string | null;
        resolved_sha: string | null;
        bundled: boolean;
        hidden: boolean;
        error: string | null;
        entry_count: number;
}

/** What the add-marketplace form sends: one string in any accepted form. */
export interface MarketplaceInput {
        source: string;
        /**
         * An optional subdirectory inside a git source (`microsoft/Agents`
         * keeps its registry under `agent-plugins/`). A local path or an HTTPS
         * registry URL has no subdirectory; the backend ignores it there.
         */
        path?: string | null;
}

/** One normalised registry entry plus its install state (design §12). */
export interface CatalogEntry {
        marketplace_id: string;
        name: string;
        display_name: string | null;
        description: string | null;
        version: string | null;
        category: string | null;
        tags: string[];
        author: string | null;
        homepage: string | null;
        icon: string | null;
        keywords: string[];
        available: boolean;
        reason: string | null;
        source_kind: string;
        /** The installed plugin's id when this entry is installed. */
        installed: string | null;
        installed_version: string | null;
        update_available: boolean;
        /** The spec §6 trust text, so the sheet can show it before install. */
        trust_warning: string;
}

/** One `plugin_progress` event, as it arrives on `backend://event`. */
export interface PluginProgressEvent {
        plugin_id: string;
        /** `fetch`, `validate` or `place`. */
        phase: string;
        detail: string;
}

/** One `plugin_update_available` event, as it arrives on `backend://event`. */
export interface PluginUpdateAvailableEvent {
        plugin_id: string;
        from: string | null;
        to: string | null;
}

/** Settings → Plugins (design §3). */
export interface PluginSettings {
        /** Master switch for the skills block in the system prompt and the tools. */
        skills_enabled: boolean;
        /** Default policy for newly installed plugins. */
        policy_default: "content" | "auto" | "manual";
        /** Hours between marketplace refreshes; 0 = never refresh automatically. */
        marketplace_refresh_hours: number;
        /** Hours between update checks; 0 = never check automatically. */
        update_check_hours: number;
}

/**
 * One skill the model can use, as `skills_list` reports it: every root, in
 * precedence order, with provenance and the id that shadows it (if any).
 */
export interface SkillSummary {
        id: string;
        name: string;
        description: string;
        /** `user`, `workspace`, `agents`, or `plugin: <name>`. */
        origin: string;
        /** The id that won the name, when this one is shadowed. */
        shadowed: string | null;
}
