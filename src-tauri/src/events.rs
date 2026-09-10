//! Events pushed from the Rust backend to the webview, plus the `EventSink`
//! abstraction that keeps the MCP layer testable without a Tauri runtime.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendEvent {
    /// Assistant streamed some text.
    ChatDelta {
        conversation_id: String,
        text: String,
    },
    /// Assistant streamed hidden reasoning (shown when enabled).
    ReasoningDelta {
        conversation_id: String,
        text: String,
    },
    /// A full assistant turn (text + tool calls) is complete.
    MessageDone {
        conversation_id: String,
        message_id: String,
    },
    /// The chat turn failed.
    ChatError {
        conversation_id: String,
        error: String,
    },
    /// Token usage for the last model call in a conversation.
    Usage {
        conversation_id: String,
        input: Option<u64>,
        output: Option<u64>,
    },
    /// Tool call lifecycle updates. `status` is one of
    /// `pending_approval | running | awaiting_input | done | denied | error`.
    ToolCallUpdate {
        conversation_id: String,
        tool_call_id: String,
        status: String,
        server: Option<String>,
        server_title: Option<String>,
        tool: Option<String>,
        args: Option<serde_json::Value>,
        result_text: Option<String>,
        structured: Option<serde_json::Value>,
        content: Option<serde_json::Value>,
        is_error: Option<bool>,
    },
    /// Ask the user to approve a tool execution.
    ApprovalRequested {
        request_id: String,
        conversation_id: Option<String>,
        server: String,
        server_title: String,
        tool: String,
        args: serde_json::Value,
        read_only_hint: Option<bool>,
    },
    /// Server needs input from the user (MCP elicitation, form or URL mode).
    ElicitationRequested {
        request_id: String,
        conversation_id: Option<String>,
        server: String,
        server_title: String,
        mode: String,
        message: String,
        schema: Option<serde_json::Value>,
        url: Option<String>,
    },
    /// Server asks Ducky's model to generate a completion (MCP sampling).
    SamplingRequested {
        request_id: String,
        conversation_id: Option<String>,
        server: String,
        server_title: String,
        system_prompt: Option<String>,
        messages: Vec<serde_json::Value>,
        max_tokens: Option<u32>,
    },
    /// Connection state of an MCP server changed. For `needs_auth`, `reason`
    /// is one of `missing | expired | scope`.
    ServerStatus {
        server_id: String,
        status: String,
        detail: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Something on a server changed and lists should be refetched.
    ServerDataChanged { server_id: String, what: String },
    /// Progress on a long-running request.
    Progress {
        conversation_id: Option<String>,
        tool_call_id: Option<String>,
        progress: f64,
        total: Option<f64>,
        message: Option<String>,
    },
    /// Live status of a server-side task (MCP tasks extension). `status` is
    /// one of `working | input_required | completed | failed | cancelled`.
    TaskUpdate {
        conversation_id: Option<String>,
        tool_call_id: Option<String>,
        task_id: String,
        status: String,
        status_message: Option<String>,
    },
    /// A subscribed resource changed.
    ResourceUpdated { server_id: String, uri: String },
    /// Raw PTY output for a conversation terminal, base64-encoded. `seq`
    /// orders chunks within a session; output with `seq < last_seq` from
    /// `terminal_create` is already contained in the returned scrollback.
    TerminalOutput {
        conversation_id: String,
        data: String,
        seq: u64,
    },
    /// The terminal's shell exited and its session was torn down.
    TerminalClosed {
        conversation_id: String,
        exit_code: Option<i32>,
    },
}

/// Abstraction over the Tauri event emitter so the MCP layer can be tested.
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: BackendEvent);
}

/// Sink that collects events into a shared buffer (tests / headless mode).
#[derive(Clone, Default)]
pub struct CollectingSink {
    pub events: std::sync::Arc<std::sync::Mutex<Vec<BackendEvent>>>,
}

impl EventSink for CollectingSink {
    fn emit(&self, event: BackendEvent) {
        self.events.lock().unwrap().push(event);
    }
}
