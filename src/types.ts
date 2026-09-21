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
}

export interface ConversationMeta {
        id: string;
        title: string;
        provider_id: string;
        model: string;
        effort: EffortLevel | null;
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

export type BackendEvent =
        | { type: "chat_delta"; conversation_id: string; text: string }
        | { type: "reasoning_delta"; conversation_id: string; text: string }
        | {
                  type: "subagent_delta";
                  conversation_id: string;
                  tool_call_id: string;
                  text: string;
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
        | { type: "title_updated"; conversation_id: string; title: string };

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
