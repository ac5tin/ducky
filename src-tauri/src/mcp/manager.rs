//! MCP connection manager: owns one rmcp client per configured server, keeps
//! a cached view of each server's tools/resources/prompts, and exposes
//! operations to the rest of the app.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams,
    CompleteRequestParams, CompleteResult, ContentBlock, GetPromptRequestParams, GetPromptResult,
    GetTaskParams, InputRequest, InputRequests, InputResponses, JsonObject, ListToolsResult,
    NumberOrString, PaginatedRequestParams, ProgressToken, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResult, Reference, RequestMetaObject,
    ServerNotification, SubscriptionFilter, TaskPayload, TaskStatus, Tool, UpdateTaskParams,
    DEFAULT_MRTR_MAX_ROUNDS,
};

#[allow(deprecated)] // SEP-2577; rmcp 3.1.4 still exposes the compatibility API.
use rmcp::model::{ListRootsResult, Root};
use rmcp::service::{ClientLifecycleMode, Peer, RoleClient, RunningService};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::transport::TokioChildProcess;
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::auth_client::{AuthHttpClient, AuthNotifier};
use super::bridge::InteractiveBridge;
use super::handler::DuckyClientHandler;
use crate::config::{HttpAuth, McpServerConfig, McpTransport, Store};
use crate::events::{BackendEvent, EventSink};
use crate::plugins::layout::PluginTransport;
use crate::plugins::manager::{PluginIndex, PluginManager};

// ---------------------------------------------------------------------------
// Public view types (serialised to the webview)
// ---------------------------------------------------------------------------

/// Why a server needs (re-)authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthReason {
    /// No stored session — sign in for the first time.
    Missing,
    /// The stored session was rejected (expired/revoked refresh token).
    Expired,
    /// The server demands additional scopes (step-up authorization).
    Scope,
}

impl AuthReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthReason::Missing => "missing",
            AuthReason::Expired => "expired",
            AuthReason::Scope => "scope",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Disconnected,
    Connecting,
    Connected,
    /// The server answered 401/403: the user must authorise (OAuth) or fix
    /// their token.
    NeedsAuth {
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<AuthReason>,
    },
    Error {
        message: String,
    },
}

/// Where a server row comes from: the user's own config or an enabled plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerOriginKind {
    User,
    Plugin,
}

/// The plugin a server came from; both fields are `None` for user servers
/// (design §9). Task 13 badges rows from this.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerOrigin {
    pub kind: ServerOriginKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_name: Option<String>,
}

impl ServerOrigin {
    fn user() -> Self {
        Self {
            kind: ServerOriginKind::User,
            plugin_id: None,
            plugin_name: None,
        }
    }
}

/// A user server, or one contributed by an enabled plugin (design §9).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedServer {
    pub config: McpServerConfig,
    pub origin: ServerOrigin,
    pub launch: Option<PluginLaunch>,
}

/// What a plugin stdio server needs beyond its config: the package and data
/// roots, plus the manifest's raw, still-unexpanded `cwd` (design §2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginLaunch {
    pub root: PathBuf,
    pub data: PathBuf,
    pub cwd: Option<String>,
}

/// A fully resolved plugin stdio launch (design §2/§9).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolEntry {
    pub server_id: String,
    pub name: String,
    /// Name shown to the model: `<server-slug>__<tool>`.
    pub qualified_name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
    pub annotations: Option<serde_json::Value>,
    pub read_only_hint: Option<bool>,
    pub icons: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerSummary {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport_kind: String,
    pub detail: String,
    pub status: ServerStatus,
    pub tools: Vec<ToolEntry>,
    pub resources: Vec<serde_json::Value>,
    pub resource_templates: Vec<serde_json::Value>,
    pub prompts: Vec<serde_json::Value>,
    pub server_info: Option<serde_json::Value>,
    pub instructions: Option<String>,
    pub protocol_version: Option<String>,
    pub capabilities: Option<serde_json::Value>,
    pub logs: Vec<String>,
    /// User config or plugin-provided (design §9).
    pub origin: ServerOrigin,
}

/// Progress snapshot of a server-side task (MCP tasks extension), surfaced
/// to the chat while a tool call runs.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    pub task_id: String,
    pub status: String,
    pub status_message: Option<String>,
}

fn task_status_str(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Working => "working",
        TaskStatus::InputRequired => "input_required",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        _ => "working",
    }
}

/// Everything we know about a connected server.
#[derive(Default)]
pub struct ServerData {
    pub tools: Vec<ToolEntry>,
    pub resources: Vec<serde_json::Value>,
    pub resource_templates: Vec<serde_json::Value>,
    pub prompts: Vec<serde_json::Value>,
    pub server_info: Option<serde_json::Value>,
    pub instructions: Option<String>,
    pub protocol_version: Option<String>,
    pub capabilities: Option<serde_json::Value>,
}

pub struct ServerHandle {
    pub cfg: McpServerConfig,
    store: Arc<Store>,
    bridge: Arc<InteractiveBridge>,
    service: tokio::sync::Mutex<Option<RunningService<RoleClient, DuckyClientHandler>>>,
    pub data: Mutex<ServerData>,
    pub ct: CancellationToken,
    pub logs: Mutex<VecDeque<String>>,
    /// Live per-resource subscriptions (uri -> cancel token).
    pub resource_subs: Mutex<HashMap<String, CancellationToken>>,
}

impl ServerHandle {
    fn log(&self, line: impl Into<String>) {
        let mut logs = self.logs.lock().unwrap();
        logs.push_back(line.into());
        while logs.len() > 300 {
            logs.pop_front();
        }
    }

    /// A cloneable handle for sending requests to the server, if connected.
    pub async fn peer(&self) -> Option<Peer<RoleClient>> {
        self.service.lock().await.as_ref().map(|s| s.peer().clone())
    }

    pub async fn peer_info(&self) -> Option<Arc<rmcp::model::ServerPeerInfo>> {
        self.service
            .lock()
            .await
            .as_ref()
            .and_then(|s| s.peer_info())
    }

    /// MRTR-aware `tools/call` (drives elicitation/sampling/roots rounds via
    /// the client handler). Prefer this over the raw peer method.
    pub async fn call_tool_mrtr(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, String> {
        let guard = self.service.lock().await;
        let service = guard.as_ref().ok_or("Server is not connected")?;
        service.call_tool(params).await.map_err(|e| e.to_string())
    }

    /// MRTR-aware `resources/read`.
    pub async fn read_resource_mrtr(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, String> {
        let guard = self.service.lock().await;
        let service = guard.as_ref().ok_or("Server is not connected")?;
        service
            .read_resource(params)
            .await
            .map_err(|e| e.to_string())
    }

    /// MRTR-aware `prompts/get`.
    pub async fn get_prompt_mrtr(
        &self,
        params: GetPromptRequestParams,
    ) -> Result<GetPromptResult, String> {
        let guard = self.service.lock().await;
        let service = guard.as_ref().ok_or("Server is not connected")?;
        service.get_prompt(params).await.map_err(|e| e.to_string())
    }

    /// `tools/call` with MRTR input rounds and full tasks-extension support:
    /// when the server materializes a task, poll `tasks/get` (honoring its
    /// poll interval) and surface progress through `on_task`. Cancellation is
    /// cooperative and sends `tasks/cancel`.
    pub async fn call_tool_with_tasks(
        &self,
        mut params: CallToolRequestParams,
        on_task: &(dyn Fn(TaskSnapshot) + Send + Sync),
        ct: &CancellationToken,
    ) -> Result<CallToolResult, String> {
        let guard = self.service.lock().await;
        let service = guard.as_ref().ok_or("Server is not connected")?;
        let peer = service.peer().clone();

        for _round in 0..DEFAULT_MRTR_MAX_ROUNDS {
            let response = {
                let call = peer.call_tool_once(params.clone());
                tokio::select! {
                    _ = ct.cancelled() => return Err("cancelled by user".to_string()),
                    r = call => r.map_err(|e| e.to_string())?,
                }
            };
            match response {
                CallToolResponse::Complete(result) => return Ok(result),
                CallToolResponse::Task(task) => {
                    return self.poll_task(&peer, task.task.task_id, on_task, ct).await;
                }
                CallToolResponse::InputRequired(result) => {
                    let had_requests = result
                        .input_requests
                        .as_ref()
                        .is_some_and(|r| !r.is_empty());
                    if !had_requests && result.request_state.is_none() {
                        return Err("The server sent an invalid input_required response".into());
                    }
                    let responses = self
                        .fulfill_input_requests(result.input_requests.unwrap_or_default())
                        .await?;
                    if !had_requests {
                        // Server asked us to wait on request state alone.
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    params.input_responses = (!responses.is_empty()).then_some(responses);
                    params.request_state = result.request_state;
                }
                _ => return Err("The server returned an unexpected response".to_string()),
            }
        }
        Err(format!(
            "The server kept asking for input without completing the call \
             ({DEFAULT_MRTR_MAX_ROUNDS} rounds)"
        ))
    }

    /// Poll a server-side task to completion.
    async fn poll_task(
        &self,
        peer: &Peer<RoleClient>,
        task_id: String,
        on_task: &(dyn Fn(TaskSnapshot) + Send + Sync),
        ct: &CancellationToken,
    ) -> Result<CallToolResult, String> {
        // Generous ceiling; a server TTL usually ends the task sooner.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30 * 60);
        let mut interval = Duration::from_millis(500);
        let mut last_status = String::new();
        let mut last_message: Option<String> = None;
        loop {
            if tokio::time::Instant::now() >= deadline {
                let _ = peer
                    .cancel_task(CancelTaskParams::new(task_id.clone()))
                    .await;
                return Err("The task did not finish within 30 minutes".into());
            }
            let result = {
                let call = peer.get_task(GetTaskParams::new(task_id.clone()));
                tokio::select! {
                    _ = ct.cancelled() => {
                        let _ = peer.cancel_task(CancelTaskParams::new(task_id.clone())).await;
                        return Err("cancelled by user".to_string());
                    }
                    r = call => r.map_err(|e| e.to_string())?,
                }
            };
            let detailed = result.task;
            if let Some(hint) = detailed.task.poll_interval_ms {
                interval = Duration::from_millis(hint.clamp(200, 15_000));
            }
            let status = task_status_str(detailed.status());
            let message = detailed.task.status_message.clone();
            if status != last_status || message != last_message {
                on_task(TaskSnapshot {
                    task_id: task_id.clone(),
                    status: status.to_string(),
                    status_message: message.clone(),
                });
                last_status = status.to_string();
                last_message = message;
            }
            match detailed.payload {
                TaskPayload::Working => {}
                TaskPayload::InputRequired { input_requests } => {
                    let responses = self.fulfill_input_requests(input_requests).await?;
                    peer.update_task(UpdateTaskParams::new(task_id.clone(), responses))
                        .await
                        .map_err(|e| e.to_string())?;
                }
                TaskPayload::Completed { result } => {
                    return serde_json::from_value::<CallToolResult>(serde_json::Value::Object(
                        result,
                    ))
                    .map_err(|e| format!("The server returned an invalid task result: {e}"));
                }
                TaskPayload::Failed { error } => {
                    let message = error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown error");
                    return Err(format!("The server reported the task failed: {message}"));
                }
                TaskPayload::Cancelled => {
                    return Err("The task was cancelled".into());
                }
                _ => {}
            }
            tokio::select! {
                _ = ct.cancelled() => {
                    let _ = peer.cancel_task(CancelTaskParams::new(task_id.clone())).await;
                    return Err("cancelled by user".to_string());
                }
                _ = tokio::time::sleep(interval) => {}
            }
        }
    }

    /// Answer server-initiated input requests (elicitation / sampling / roots)
    /// through the interactive bridge.
    #[allow(deprecated)] // SEP-2577; rmcp 3.1.4 still exposes the compatibility API.
    async fn fulfill_input_requests(
        &self,
        requests: InputRequests,
    ) -> Result<InputResponses, String> {
        let mut out = InputResponses::new();
        for (key, request) in requests {
            let value = match request {
                InputRequest::Elicitation(req) => {
                    let result = self
                        .bridge
                        .run_elicitation(&self.cfg.id, &self.cfg.name, req.params)
                        .await
                        .map_err(|e| e.to_string())?;
                    serde_json::to_value(result).map_err(|e| e.to_string())?
                }
                InputRequest::CreateMessage(req) => {
                    let result = self
                        .bridge
                        .run_sampling(&self.cfg.id, &self.cfg.name, &req.params)
                        .await
                        .map_err(|e| e.to_string())?;
                    serde_json::to_value(result).map_err(|e| e.to_string())?
                }
                InputRequest::ListRoots(_) => {
                    let roots = self.store.config.lock().unwrap().settings.roots.clone();
                    let list = roots
                        .into_iter()
                        .map(|path| {
                            let mut root = Root::new(format!("file://{path}"));
                            root.name = std::path::Path::new(&path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string());
                            root
                        })
                        .collect();
                    serde_json::to_value(ListRootsResult::new(list)).map_err(|e| e.to_string())?
                }
                _ => return Err("The server requested an unsupported kind of input".to_string()),
            };
            out.insert(key, value);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// Shared auth-state plumbing. The HTTP transport wrapper (running in any
/// task) reports re-auth requirements here, and the login command signals
/// completion here so chat tool calls can resume.
struct AuthCoordinator {
    sink: Arc<dyn EventSink>,
    statuses: Arc<Mutex<HashMap<String, ServerStatus>>>,
    auth_gens: Mutex<HashMap<String, watch::Sender<u64>>>,
}

impl AuthCoordinator {
    fn emit_status(
        &self,
        server_id: &str,
        status: &str,
        detail: Option<String>,
        reason: Option<AuthReason>,
    ) {
        self.sink.emit(BackendEvent::ServerStatus {
            server_id: server_id.to_string(),
            status: status.to_string(),
            detail,
            reason: reason.map(|r| r.as_str().to_string()),
        });
    }

    /// Flip a connected/connecting server to `needs_auth` (e.g. a mid-session
    /// 401). Never clobbers a terminal state or an in-flight transition.
    fn notify_needs_auth(&self, server_id: &str, reason: AuthReason, detail: Option<String>) {
        {
            let mut statuses = self.statuses.lock().unwrap();
            if !matches!(
                statuses.get(server_id),
                Some(ServerStatus::Connected) | Some(ServerStatus::Connecting)
            ) {
                return;
            }
            statuses.insert(
                server_id.to_string(),
                ServerStatus::NeedsAuth {
                    detail: detail.clone(),
                    reason: Some(reason),
                },
            );
        }
        self.emit_status(server_id, "needs_auth", detail, Some(reason));
    }

    fn auth_generation(&self, server_id: &str) -> u64 {
        let gens = self.auth_gens.lock().unwrap();
        gens.get(server_id).map(|tx| *tx.borrow()).unwrap_or(0)
    }

    /// Bump the auth generation: everyone waiting for sign-in resumes.
    fn notify_auth_completed(&self, server_id: &str) {
        let tx = {
            let mut gens = self.auth_gens.lock().unwrap();
            gens.entry(server_id.to_string())
                .or_insert_with(|| watch::channel(0u64).0)
                .clone()
        };
        tx.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Resolve once sign-in completes (generation advances past `previous`).
    async fn wait_for_auth(
        &self,
        server_id: &str,
        previous: u64,
        timeout: Duration,
    ) -> Result<(), String> {
        let mut rx = {
            let mut gens = self.auth_gens.lock().unwrap();
            gens.entry(server_id.to_string())
                .or_insert_with(|| watch::channel(0u64).0)
                .subscribe()
        };
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if *rx.borrow_and_update() > previous {
                return Ok(());
            }
            tokio::time::timeout_at(deadline, rx.changed())
                .await
                .map_err(|_| "Timed out waiting for sign-in".to_string())?
                .map_err(|e| e.to_string())?;
        }
    }
}

pub struct McpManager {
    store: Arc<Store>,
    bridge: Arc<InteractiveBridge>,
    sink: Arc<dyn EventSink>,
    /// Source of the plugin index and of the plugin package/data roots.
    plugins: Arc<PluginManager>,
    handles: Mutex<HashMap<String, Arc<ServerHandle>>>,
    statuses: Arc<Mutex<HashMap<String, ServerStatus>>>,
    auth: Arc<AuthCoordinator>,
    /// stderr tail from the most recent *failed* connect attempt per server,
    /// served by `summary()` so the detail view has logs to show.
    connect_logs: Mutex<HashMap<String, VecDeque<String>>>,
}

/// A transport ready to be served, in either flavour.
enum BuiltTransport {
    Stdio(TokioChildProcess),
    Http(StreamableHttpClientTransport<AuthHttpClient>),
}

/// Rolling tail of a stdio server's stderr for connect-failure diagnostics.
type StderrTail = Arc<Mutex<VecDeque<String>>>;

impl McpManager {
    pub fn new(
        store: Arc<Store>,
        bridge: Arc<InteractiveBridge>,
        sink: Arc<dyn EventSink>,
        plugins: Arc<PluginManager>,
    ) -> Self {
        let statuses = Arc::new(Mutex::new(HashMap::new()));
        let auth = Arc::new(AuthCoordinator {
            sink: sink.clone(),
            statuses: statuses.clone(),
            auth_gens: Mutex::new(HashMap::new()),
        });
        Self {
            store,
            bridge,
            sink,
            plugins,
            handles: Mutex::new(HashMap::new()),
            statuses,
            auth,
            connect_logs: Mutex::new(HashMap::new()),
        }
    }

    fn set_status(&self, server_id: &str, status: ServerStatus) {
        let status_str = match &status {
            ServerStatus::Disconnected => "disconnected",
            ServerStatus::Connecting => "connecting",
            ServerStatus::Connected => "connected",
            ServerStatus::NeedsAuth { .. } => "needs_auth",
            ServerStatus::Error { .. } => "error",
        };
        let detail = match &status {
            ServerStatus::Error { message } => Some(message.clone()),
            ServerStatus::NeedsAuth { detail, .. } => detail.clone(),
            _ => None,
        };
        let reason = match &status {
            ServerStatus::NeedsAuth { reason, .. } => *reason,
            _ => None,
        };
        self.statuses
            .lock()
            .unwrap()
            .insert(server_id.to_string(), status);
        self.auth.emit_status(server_id, status_str, detail, reason);
    }

    /// Entry point for the transport wrapper and the background refresher:
    /// a connected server hit an auth wall mid-session.
    pub fn mark_needs_auth(&self, server_id: &str, reason: AuthReason, detail: Option<String>) {
        self.auth.notify_needs_auth(server_id, reason, detail);
    }

    /// Current auth generation, for `wait_for_auth`.
    pub fn auth_generation(&self, server_id: &str) -> u64 {
        self.auth.auth_generation(server_id)
    }

    /// Called after a successful OAuth login so waiters can resume.
    pub fn notify_auth_completed(&self, server_id: &str) {
        self.auth.notify_auth_completed(server_id);
    }

    /// Wait until sign-in completes for a server (or time out).
    pub async fn wait_for_auth(
        &self,
        server_id: &str,
        previous: u64,
        timeout: Duration,
    ) -> Result<(), String> {
        self.auth.wait_for_auth(server_id, previous, timeout).await
    }

    pub fn status(&self, server_id: &str) -> ServerStatus {
        self.statuses
            .lock()
            .unwrap()
            .get(server_id)
            .cloned()
            .unwrap_or(ServerStatus::Disconnected)
    }

    /// Shared config/secrets store.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Installed plugins, marketplaces and the derived component index.
    pub fn plugins(&self) -> &Arc<PluginManager> {
        &self.plugins
    }

    pub fn get(&self, server_id: &str) -> Option<Arc<ServerHandle>> {
        self.handles.lock().unwrap().get(server_id).cloned()
    }

    pub fn server_slug(cfg: &McpServerConfig) -> String {
        let raw = cfg.name.trim().to_lowercase();
        let mut slug: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if slug.is_empty() {
            slug = "server".into();
        }
        slug
    }

    // -- connection ---------------------------------------------------------

    /// Connect (or reconnect) the server with the given id. Non-fatal
    /// outcomes (needs auth, error) are reported via status events; the
    /// returned status mirrors what was reported.
    pub async fn connect(&self, server_id: &str) -> Result<ServerStatus, String> {
        // User servers ∪ the enabled plugin servers (design §9).
        let resolved = self
            .resolved_servers()
            .into_iter()
            .find(|server| server.config.id == server_id);
        let Some(resolved) = resolved else {
            return Err("Unknown server id".into());
        };
        let cfg = &resolved.config;

        // Drop any existing connection first.
        self.disconnect(server_id).await;
        self.set_status(server_id, ServerStatus::Connecting);
        // a fresh attempt invalidates the previous failure's captured output
        self.connect_logs.lock().unwrap().remove(server_id);

        // Make sure OAuth tokens are fresh (no-op for other auth modes).
        if let McpTransport::Http { .. } = &cfg.transport {
            if let HttpAuth::OAuth = &cfg.auth {
                // Missing OAuth tokens are valid for lazy authentication. If a
                // session exists, refresh it before connecting as before.
                if self.store.oauth_tokens(server_id).is_some() {
                    if let Err(e) = crate::oauth::ensure_fresh_token(&self.store, cfg).await {
                        match e {
                            crate::oauth::AuthFailure::ReauthRequired(detail) => {
                                self.set_status(
                                    server_id,
                                    ServerStatus::NeedsAuth {
                                        detail: Some(detail),
                                        reason: Some(AuthReason::Expired),
                                    },
                                );
                                return Ok(self.status(server_id));
                            }
                            crate::oauth::AuthFailure::Transient(message) => {
                                self.set_status(server_id, ServerStatus::Error { message });
                                return Ok(self.status(server_id));
                            }
                        }
                    }
                }
            }
        }

        let handler = DuckyClientHandler {
            server_id: Arc::from(server_id),
            server_name: Arc::from(Self::server_slug(cfg).as_str()),
            server_title: Arc::from(cfg.name.as_str()),
            bridge: self.bridge.clone(),
            store: self.store.clone(),
            sink: self.sink.clone(),
        };

        let ct = CancellationToken::new();
        let (built, stderr_tail) = match self.build_transport(&resolved) {
            Ok(t) => t,
            Err(e) => {
                self.set_status(server_id, ServerStatus::Error { message: e });
                return Ok(self.status(server_id));
            }
        };

        let lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![
                ProtocolVersion::V_2026_07_28,
                ProtocolVersion::V_2025_11_25,
                ProtocolVersion::V_2025_06_18,
                ProtocolVersion::V_2025_03_26,
                ProtocolVersion::V_2024_11_05,
            ],
            legacy_version: None,
        };

        let handshake = async {
            match built {
                BuiltTransport::Stdio(transport) => {
                    rmcp::service::serve_client_with_lifecycle_and_ct(
                        handler.clone(),
                        transport,
                        lifecycle,
                        ct.clone(),
                    )
                    .await
                }
                BuiltTransport::Http(transport) => {
                    rmcp::service::serve_client_with_lifecycle_and_ct(
                        handler.clone(),
                        transport,
                        lifecycle,
                        ct.clone(),
                    )
                    .await
                }
            }
        };
        let service = match tokio::time::timeout(Duration::from_secs(20), handshake).await {
            Ok(inner) => inner,
            Err(_) => {
                ct.cancel();
                let message = self
                    .capture_failure_output(
                        server_id,
                        "Timed out connecting to the server".into(),
                        stderr_tail.as_ref(),
                    )
                    .await;
                self.set_status(server_id, ServerStatus::Error { message });
                return Ok(self.status(server_id));
            }
        };

        let service = match service {
            Ok(s) => s,
            Err(e) => {
                // A missing OAuth token is allowed through initialize. If the
                // server rejects that request, the wrapper reports its local
                // re-auth error after observing the HTTP challenge.
                let oauth_without_tokens = matches!(&cfg.auth, HttpAuth::OAuth)
                    && self.store.oauth_tokens(server_id).is_none();
                if e.is_authorization_required()
                    || (oauth_without_tokens && e.to_string().contains("Not signed in"))
                {
                    let challenge = e.auth_challenge().unwrap_or_default();
                    let (detail, reason) = summarise_challenge(challenge);
                    self.set_status(
                        server_id,
                        ServerStatus::NeedsAuth {
                            detail: Some(detail),
                            reason: Some(reason),
                        },
                    );
                    return Ok(self.status(server_id));
                }
                let message = self
                    .capture_failure_output(server_id, describe_init_error(&e), stderr_tail.as_ref())
                    .await;
                self.set_status(server_id, ServerStatus::Error { message });
                return Ok(self.status(server_id));
            }
        };

        let handle = Arc::new(ServerHandle {
            cfg: cfg.clone(),
            store: self.store.clone(),
            bridge: self.bridge.clone(),
            service: tokio::sync::Mutex::new(Some(service)),
            data: Mutex::new(ServerData::default()),
            ct: ct.clone(),
            logs: Mutex::new(VecDeque::new()),
            resource_subs: Mutex::new(HashMap::new()),
        });
        handle.log(format!(
            "connected to {}",
            match &cfg.transport {
                McpTransport::Stdio { command, .. } => command.clone(),
                McpTransport::Http { url, .. } => url.clone(),
            }
        ));

        // Publish the session before listing so OAuth waiters can retry as
        // soon as initialize finishes. Listing is best-effort.
        self.handles
            .lock()
            .unwrap()
            .insert(server_id.to_string(), handle.clone());
        self.set_status(server_id, ServerStatus::Connected);
        self.spawn_list_changed_subscription(handle.clone());

        match tokio::time::timeout(Duration::from_secs(8), self.refresh_data(&handle)).await {
            Ok(Ok(data)) => {
                *handle.data.lock().unwrap() = data;
            }
            Ok(Err(e)) => handle.log(format!("listing failed: {e}")),
            Err(_) => handle.log("listing timed out"),
        }
        Ok(ServerStatus::Connected)
    }

    pub async fn disconnect(&self, server_id: &str) {
        let handle = self.handles.lock().unwrap().remove(server_id);
        if let Some(h) = handle {
            for (_, token) in h.resource_subs.lock().unwrap().drain() {
                token.cancel();
            }
            h.ct.cancel();
            let service = h.service.lock().await.take();
            if let Some(mut service) = service {
                let _ = service.close_with_timeout(Duration::from_secs(2)).await;
            }
        }
        self.statuses
            .lock()
            .unwrap()
            .insert(server_id.to_string(), ServerStatus::Disconnected);
        self.auth.emit_status(server_id, "disconnected", None, None);
    }

    /// After a failed connect: let the stderr drain catch up, remember the
    /// tail for the detail view, and quote the last few lines in the error.
    async fn capture_failure_output(
        &self,
        server_id: &str,
        message: String,
        tail: Option<&StderrTail>,
    ) -> String {
        let Some(tail) = tail else {
            return message;
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        let lines: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
        if lines.is_empty() {
            return message;
        }
        {
            let mut map = self.connect_logs.lock().unwrap();
            let entry = map.entry(server_id.to_string()).or_default();
            entry.extend(lines.iter().cloned());
            while entry.len() > 300 {
                entry.pop_front();
            }
        }
        let quoted: Vec<&str> = lines.iter().rev().take(5).rev().map(|l| l.as_str()).collect();
        format!("{message}\n\nLast server output:\n{}", quoted.join("\n"))
    }

    /// Drop manager-side state for a server that was removed from the config.
    pub fn forget(&self, server_id: &str) {
        self.connect_logs.lock().unwrap().remove(server_id);
        self.statuses.lock().unwrap().remove(server_id);
    }

    /// Connect every enabled server that is not already connected: user
    /// servers plus the enabled plugin servers (design §9, ruling 10).
    pub async fn connect_enabled(&self) {
        let ids: Vec<String> = self
            .resolved_servers()
            .into_iter()
            .filter(|server| server.config.enabled && server.config.auto_start)
            .map(|server| server.config.id)
            .collect();
        for id in ids {
            if !matches!(
                self.status(&id),
                ServerStatus::Connected | ServerStatus::Connecting
            ) {
                let _ = self.connect(&id).await;
            }
        }
    }

    /// User servers ∪ enabled plugin servers, resolved fresh from config and
    /// the live plugin index (design §9).
    fn resolved_servers(&self) -> Vec<ResolvedServer> {
        // Snapshot the user config before touching the plugin index: never
        // hold the config lock while cloning the (large) plugin index.
        let user: Vec<McpServerConfig> = {
            let config = self.store.config.lock().unwrap();
            config.mcp_servers.clone()
        };
        merge_servers(&user, &self.plugins.index(), &self.plugins.data_dir)
    }

    fn build_transport(
        &self,
        resolved: &ResolvedServer,
    ) -> Result<(BuiltTransport, Option<StderrTail>), String> {
        let cfg = &resolved.config;
        match &cfg.transport {
            McpTransport::Stdio { command, args, env } => {
                if let Some(launch) = &resolved.launch {
                    // Plugin servers get their own launch rules (design §2/§9):
                    // placeholder expansion, the plugin root as the cwd
                    // default, and no login-shell PATH augmentation.
                    let env: BTreeMap<String, String> =
                        env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                    let spec = spawn_spec(
                        command,
                        args,
                        &env,
                        launch.cwd.as_deref(),
                        &launch.root,
                        &launch.data,
                    )?;
                    let mut cmd = tokio::process::Command::new(&spec.program);
                    cmd.args(&spec.args);
                    for (key, value) in &spec.env {
                        cmd.env(key, value);
                    }
                    cmd.current_dir(&spec.cwd);
                    return self.spawn_stdio(cmd, command, &cfg.id);
                }

                let cwd = self
                    .store
                    .config
                    .lock()
                    .unwrap()
                    .settings
                    .effective_working_dir(&self.store.home_dir);
                // servers get plain args (no shell), so expand a leading `~`
                // ourselves — e.g. the built-in Filesystem suggestion
                let expand =
                    |s: &String| crate::config::expand_tilde(s, &self.store.home_dir).into_owned();
                let args: Vec<String> = args.iter().map(&expand).collect();
                let env: HashMap<String, String> =
                    env.iter().map(|(k, v)| (k.clone(), expand(v))).collect();

                let mut cmd = tokio::process::Command::new(command);
                cmd.args(&args).envs(&env).current_dir(&cwd);
                // an explicit per-server PATH wins over our augmentation
                if !env.contains_key("PATH") {
                    augment_command_path(&mut cmd);
                }
                self.spawn_stdio(cmd, command, &cfg.id)
            }
            McpTransport::Http { url, headers } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone());
                let mut custom = HashMap::new();
                for (k, v) in headers {
                    let name = http::header::HeaderName::from_bytes(k.as_bytes())
                        .map_err(|e| format!("Invalid header name {k}: {e}"))?;
                    let value = http::header::HeaderValue::from_str(v)
                        .map_err(|e| format!("Invalid header value for {k}: {e}"))?;
                    custom.insert(name, value);
                }
                config = config.custom_headers(custom);
                // Session ids can expire server-side; re-initialize instead
                // of failing the request.
                config = config.reinit_on_expired_session(true);

                // Credentials are resolved per request by the wrapper; when
                // the server rejects them mid-session the wrapper reports
                // back through the coordinator.
                let auth = self.auth.clone();
                let server_id = cfg.id.clone();
                let notify: AuthNotifier = Arc::new(move |reason, detail| {
                    auth.notify_needs_auth(&server_id, reason, detail);
                });
                Ok((
                    BuiltTransport::Http(StreamableHttpClientTransport::with_client(
                        AuthHttpClient::new(cfg.clone(), self.store.clone(), notify),
                        config,
                    )),
                    None,
                ))
            }
        }
    }

    /// Spawn a stdio child, keep a tail of its stderr for failure diagnostics
    /// and forward the lines as events. Shared by the user and plugin paths.
    fn spawn_stdio(
        &self,
        cmd: tokio::process::Command,
        command: &str,
        server_id: &str,
    ) -> Result<(BuiltTransport, Option<StderrTail>), String> {
        let (child, stderr) = TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not start `{command}`: {e}.{}", spawn_hint(command)))?;
        // keep a tail of the server's stderr so a failed handshake can
        // quote it (npm 404s, missing-runtime errors, crash traces)
        let tail: StderrTail = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = stderr {
            let server_id = server_id.to_string();
            let sink = self.sink.clone();
            let tail = tail.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // forward server logs as an event the UI
                    // can render in the connector detail view
                    sink.emit(BackendEvent::ServerDataChanged {
                        server_id: server_id.clone(),
                        what: format!("log:{line}"),
                    });
                    let mut t = tail.lock().unwrap();
                    t.push_back(line);
                    while t.len() > 300 {
                        t.pop_front();
                    }
                }
            });
        }
        Ok((BuiltTransport::Stdio(child), Some(tail)))
    }

    fn spawn_list_changed_subscription(&self, handle: Arc<ServerHandle>) {
        let h = handle.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let Some(peer) = h.peer().await else { return };
            let mut filter = SubscriptionFilter::default();
            filter.tools_list_changed = Some(true);
            filter.prompts_list_changed = Some(true);
            filter.resources_list_changed = Some(true);
            let result = peer.listen(filter).await;
            let mut sub = match result {
                Ok(s) => {
                    h.log("subscription stream established");
                    s
                }
                Err(_) => {
                    // Legacy server or transport without notifications —
                    // handler-level notifications still cover list changes.
                    return;
                }
            };
            loop {
                match sub.next().await {
                    Ok(Some(notification)) => {
                        let what = match &notification {
                            ServerNotification::ToolListChangedNotification(_) => "tools",
                            ServerNotification::PromptListChangedNotification(_) => "prompts",
                            ServerNotification::ResourceListChangedNotification(_) => "resources",
                            ServerNotification::ResourceUpdatedNotification(n) => {
                                sink.emit(BackendEvent::ResourceUpdated {
                                    server_id: h.cfg.id.clone(),
                                    uri: n.params.uri.clone(),
                                });
                                continue;
                            }
                            _ => continue,
                        };
                        sink.emit(BackendEvent::ServerDataChanged {
                            server_id: h.cfg.id.clone(),
                            what: what.into(),
                        });
                    }
                    Ok(None) => break, // graceful end
                    Err(_) => break,   // transport closed
                }
            }
        });
    }

    // -- listings ------------------------------------------------------------

    /// (Re-)fetch tools, resources, templates and prompts.
    pub async fn refresh_data(&self, handle: &Arc<ServerHandle>) -> Result<ServerData, String> {
        let peer = handle.peer().await.ok_or("Server is not connected")?;
        let mut data = ServerData::default();

        let info = handle.peer_info().await;
        let has_resources = info
            .as_ref()
            .is_some_and(|i| i.capabilities.resources.is_some());
        let has_prompts = info
            .as_ref()
            .is_some_and(|i| i.capabilities.prompts.is_some());
        if let Some(info) = info {
            data.protocol_version = Some(info.protocol_version.as_str().to_string());
            data.capabilities = serde_json::to_value(&info.capabilities).ok();
            data.instructions = info.instructions.clone();
            data.server_info = info
                .server_info
                .as_ref()
                .and_then(|i| serde_json::to_value(i).ok());
        }

        // tools (paged)
        let slug = Self::server_slug(&handle.cfg);
        let mut tools: Vec<Tool> = Vec::new();
        let mut cursor = None;
        loop {
            let params = PaginatedRequestParams::default().with_cursor(cursor);
            let result: ListToolsResult = peer
                .list_tools(Some(params))
                .await
                .map_err(|e| format!("tools/list failed: {e}"))?;
            tools.extend(result.tools.iter().cloned());
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        data.tools = tools
            .into_iter()
            .map(|t| tool_entry(&handle.cfg.id, &slug, t))
            .collect();

        // Skip list RPCs the server did not advertise — a missing capability
        // often means the call never returns, which used to block reconnect.
        if has_resources {
            let mut resources = Vec::new();
            let mut cursor = None;
            loop {
                let params = PaginatedRequestParams::default().with_cursor(cursor);
                match peer.list_resources(Some(params)).await {
                    Ok(result) => {
                        resources.extend(serialize_all(&result.resources));
                        cursor = result.next_cursor;
                        if cursor.is_none() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            data.resources = resources;

            let mut templates = Vec::new();
            let mut cursor = None;
            loop {
                let params = PaginatedRequestParams::default().with_cursor(cursor);
                match peer.list_resource_templates(Some(params)).await {
                    Ok(result) => {
                        templates.extend(serialize_all(&result.resource_templates));
                        cursor = result.next_cursor;
                        if cursor.is_none() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            data.resource_templates = templates;
        }

        if has_prompts {
            let mut prompts = Vec::new();
            let mut cursor = None;
            loop {
                let params = PaginatedRequestParams::default().with_cursor(cursor);
                match peer.list_prompts(Some(params)).await {
                    Ok(result) => {
                        prompts.extend(serialize_all(&result.prompts));
                        cursor = result.next_cursor;
                        if cursor.is_none() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            data.prompts = prompts;
        }

        Ok(data)
    }

    pub async fn refresh_server(&self, server_id: &str) -> Result<ServerSummary, String> {
        let handle = self.get(server_id).ok_or("Server is not connected")?;
        let data = self.refresh_data(&handle).await?;
        *handle.data.lock().unwrap() = data;
        Ok(self.summary(server_id))
    }

    // -- operations ----------------------------------------------------------

    /// Call a tool on a server. MRTR (elicitation/sampling during the call)
    /// and the tasks extension (server-side async work) are handled here;
    /// task progress is surfaced through `on_task`.
    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: serde_json::Value,
        progress_token: Option<String>,
        on_task: Option<&(dyn Fn(TaskSnapshot) + Send + Sync)>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, String> {
        let handle = self
            .get(server_id)
            .ok_or_else(|| "Server is not connected".to_string())?;
        let args: Option<JsonObject> = match arguments {
            serde_json::Value::Null => None,
            serde_json::Value::Object(map) => Some(map),
            other => Some(
                serde_json::from_value(other)
                    .map_err(|e| format!("Arguments must be an object: {e}"))?,
            ),
        };
        let mut params = CallToolRequestParams::new(tool_name.to_string());
        params.arguments = args;
        if let Some(token) = progress_token {
            params.meta = Some(RequestMetaObject::with_progress_token(ProgressToken(
                NumberOrString::String(token.into()),
            )));
        }
        let noop = |_: TaskSnapshot| {};
        let on_task: &(dyn Fn(TaskSnapshot) + Send + Sync) = match on_task {
            Some(cb) => cb,
            None => &noop,
        };
        handle.call_tool_with_tasks(params, on_task, &ct).await
    }

    pub async fn read_resource(
        &self,
        server_id: &str,
        uri: &str,
    ) -> Result<ReadResourceResult, String> {
        let handle = self.get(server_id).ok_or("Server is not connected")?;
        handle
            .read_resource_mrtr(ReadResourceRequestParams::new(uri))
            .await
    }

    pub async fn get_prompt(
        &self,
        server_id: &str,
        name: &str,
        arguments: HashMap<String, String>,
    ) -> Result<GetPromptResult, String> {
        let handle = self.get(server_id).ok_or("Server is not connected")?;
        let args: JsonObject = arguments
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        let mut params = GetPromptRequestParams::new(name);
        params.arguments = if args.is_empty() { None } else { Some(args) };
        handle.get_prompt_mrtr(params).await
    }

    pub async fn complete(
        &self,
        server_id: &str,
        reference: serde_json::Value,
        argument_name: String,
        argument_value: String,
    ) -> Result<CompleteResult, String> {
        let handle = self.get(server_id).ok_or("Server is not connected")?;
        let peer = handle.peer().await.ok_or("Server is not connected")?;
        let reference: Reference = serde_json::from_value(reference)
            .map_err(|e| format!("Invalid completion reference: {e}"))?;
        peer.complete(CompleteRequestParams::new(
            reference,
            rmcp::model::ArgumentInfo::new(argument_name, argument_value),
        ))
        .await
        .map_err(|e| e.to_string())
    }

    /// Subscribe to updates for one specific resource (modern servers).
    pub async fn subscribe_resource(&self, server_id: &str, uri: &str) -> Result<(), String> {
        let handle = self.get(server_id).ok_or("Server is not connected")?;
        let peer = handle.peer().await.ok_or("Server is not connected")?;
        let mut filter = SubscriptionFilter::default();
        filter.resource_subscriptions = Some(vec![uri.to_string()]);
        let sub = peer.listen(filter).await.map_err(|e| e.to_string())?;

        let ct = CancellationToken::new();
        let ct_task = ct.clone();
        let sid = server_id.to_string();
        let target = uri.to_string();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let mut sub = sub;
            loop {
                tokio::select! {
                    _ = ct_task.cancelled() => {
                        let _ = sub.cancel().await;
                        break;
                    }
                    n = sub.next() => match n {
                        Ok(Some(ServerNotification::ResourceUpdatedNotification(updated))) => {
                            if updated.params.uri == target {
                                sink.emit(BackendEvent::ResourceUpdated {
                                    server_id: sid.clone(),
                                    uri: target.clone(),
                                });
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        });
        handle
            .resource_subs
            .lock()
            .unwrap()
            .insert(uri.to_string(), ct);
        Ok(())
    }

    pub fn unsubscribe_resource(&self, server_id: &str, uri: &str) {
        if let Some(handle) = self.get(server_id) {
            if let Some(token) = handle.resource_subs.lock().unwrap().remove(uri) {
                token.cancel();
            }
        }
    }

    /// Human-readable view of one server for the UI.
    pub fn summary(&self, server_id: &str) -> ServerSummary {
        let resolved = self.resolved_servers();
        let found = resolved
            .iter()
            .find(|server| server.config.id == server_id);
        self.summary_of(found, server_id)
    }

    fn summary_of(&self, resolved: Option<&ResolvedServer>, server_id: &str) -> ServerSummary {
        let (name, enabled, transport_kind, detail, origin) = match resolved {
            Some(server) => {
                let c = &server.config;
                (
                    c.name.clone(),
                    c.enabled,
                    match &c.transport {
                        McpTransport::Stdio { .. } => "stdio".to_string(),
                        McpTransport::Http { .. } => "http".to_string(),
                    },
                    summary_detail(server),
                    server.origin.clone(),
                )
            }
            None => (
                "unknown".into(),
                false,
                "stdio".into(),
                String::new(),
                ServerOrigin::user(),
            ),
        };

        let (tools, resources, templates, prompts, info, instructions, version, caps, logs) =
            match self.get(server_id) {
                Some(h) => {
                    let d = h.data.lock().unwrap();
                    let logs = h.logs.lock().unwrap().iter().cloned().collect();
                    (
                        d.tools.clone(),
                        d.resources.clone(),
                        d.resource_templates.clone(),
                        d.prompts.clone(),
                        d.server_info.clone(),
                        d.instructions.clone(),
                        d.protocol_version.clone(),
                        d.capabilities.clone(),
                        logs,
                    )
                }
                None => {
                    // a failed connect keeps its stderr tail so the detail
                    // view has something to show
                    let logs = self
                        .connect_logs
                        .lock()
                        .unwrap()
                        .get(server_id)
                        .map(|q| q.iter().cloned().collect())
                        .unwrap_or_default();
                    (
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        None,
                        None,
                        None,
                        None,
                        logs,
                    )
                }
            };

        ServerSummary {
            id: server_id.to_string(),
            name,
            enabled,
            transport_kind,
            detail,
            status: self.status(server_id),
            tools,
            resources,
            resource_templates: templates,
            prompts,
            server_info: info,
            instructions,
            protocol_version: version,
            capabilities: caps,
            logs,
            origin,
        }
    }

    pub fn summaries(&self) -> Vec<ServerSummary> {
        let resolved = self.resolved_servers();
        resolved
            .iter()
            .map(|server| self.summary_of(Some(server), &server.config.id))
            .collect()
    }

    /// All tools from all connected & enabled servers, qualified for the model.
    pub fn aggregated_tools(&self) -> Vec<ToolEntry> {
        let mut out = Vec::new();
        let handles = self.handles.lock().unwrap().clone();
        for (_, h) in handles {
            if !h.cfg.enabled {
                continue;
            }
            out.extend(h.data.lock().unwrap().tools.clone());
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tool_entry(server_id: &str, slug: &str, tool: Tool) -> ToolEntry {
    let read_only_hint = tool.annotations.as_ref().and_then(|a| a.read_only_hint);
    ToolEntry {
        server_id: server_id.to_string(),
        name: tool.name.to_string(),
        qualified_name: format!("{slug}__{}", tool.name),
        title: tool.title.clone(),
        description: tool.description.as_ref().map(|d| d.to_string()),
        input_schema: serde_json::to_value(&*tool.input_schema).unwrap_or_default(),
        output_schema: tool
            .output_schema
            .as_ref()
            .map(|s| serde_json::to_value(&**s).unwrap_or_default()),
        annotations: tool
            .annotations
            .as_ref()
            .map(|a| serde_json::to_value(a).unwrap_or_default()),
        read_only_hint,
        icons: tool
            .icons
            .as_ref()
            .map(|i| serde_json::to_value(i).unwrap_or_default()),
    }
}

fn serialize_all<T: Serialize>(items: &[T]) -> Vec<serde_json::Value> {
    items
        .iter()
        .map(|i| serde_json::to_value(i).unwrap_or_default())
        .collect()
}

// ---------------------------------------------------------------------------
// Plugin server resolution and launch (design §2/§9, rulings R4/R12)
// ---------------------------------------------------------------------------

/// User servers ∪ the enabled plugin servers (design §9).
///
/// A user server always wins a display-name collision (the plugin server is
/// shadowed here, with a warning); ids never collide because plugin ids carry
/// the `plugin:<plugin-id>:` prefix. A plugin whose record id is not a safe
/// directory name is skipped rather than path-joined (Task 5 containment).
pub(crate) fn merge_servers(
    user: &[McpServerConfig],
    index: &PluginIndex,
    data_dir: &Path,
) -> Vec<ResolvedServer> {
    let mut resolved: Vec<ResolvedServer> = user
        .iter()
        .map(|config| ResolvedServer {
            config: config.clone(),
            origin: ServerOrigin::user(),
            launch: None,
        })
        .collect();

    for server_ref in &index.servers {
        let package =
            crate::plugins::install::package_dir(&data_dir.join("plugins"), &server_ref.plugin_id)
                .ok()
                .map(|dir| dir.join("package"));
        let data = crate::plugins::install::package_dir(
            &data_dir.join("plugin-data"),
            &server_ref.plugin_id,
        );
        let (Some(root), Ok(data)) = (package, data) else {
            tracing::warn!(
                server = %server_ref.id,
                "skipping a plugin server whose plugin id is not a safe directory name"
            );
            continue;
        };

        let shadowed = resolved.iter().find(|known| {
            known.config.id == server_ref.id
                || (known.origin.kind == ServerOriginKind::User
                    && known
                        .config
                        .name
                        .eq_ignore_ascii_case(&server_ref.server.name))
        });
        if let Some(known) = shadowed {
            let shadowed_by = match known.origin.kind {
                ServerOriginKind::User => "user server",
                ServerOriginKind::Plugin => "plugin server",
            };
            tracing::warn!(
                server = %server_ref.id,
                shadowed_by = %known.config.id,
                "plugin server is shadowed by a {shadowed_by} with the same id or name"
            );
            continue;
        }

        let (transport, cwd) = match &server_ref.server.transport {
            PluginTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => (
                McpTransport::Stdio {
                    command: command.clone(),
                    args: args.clone(),
                    env: env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                },
                cwd.clone(),
            ),
            // Both remote kinds reuse the existing client: rmcp negotiates the
            // legacy SSE fallback on the same transport (design §9).
            PluginTransport::Remote { url, headers, .. } => (
                McpTransport::Http {
                    url: url.clone(),
                    headers: headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                },
                None,
            ),
        };
        let plugin_name = index
            .plugins
            .iter()
            .find(|plugin| plugin.id == server_ref.plugin_id)
            .map(|plugin| plugin.name.clone())
            .unwrap_or_else(|| server_ref.plugin_id.clone());

        resolved.push(ResolvedServer {
            config: McpServerConfig {
                id: server_ref.id.clone(),
                name: server_ref.server.name.clone(),
                transport,
                // Plugin servers never carry user OAuth/Bearer state.
                auth: HttpAuth::None,
                // Presence in `index.servers` already means enabled and
                // consented, so the runtime treats it like an enabled user
                // server (ruling 10: it auto-connects at launch).
                enabled: true,
                auto_start: true,
                oauth_client_id: None,
                oauth_redirect_port: None,
                created_at: String::new(),
            },
            origin: ServerOrigin {
                kind: ServerOriginKind::Plugin,
                plugin_id: Some(server_ref.plugin_id.clone()),
                plugin_name: Some(plugin_name),
            },
            launch: Some(PluginLaunch { root, data, cwd }),
        });
    }
    resolved
}

/// Single non-recursive textual replacement of the two spec placeholders
/// (design §2). Nothing else is expanded, unknown spellings stay literal, and
/// text introduced by a replacement is never rescanned.
pub(crate) fn expand_placeholders(value: &str, root: &Path, data: &Path) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        if let Some(after) = tail.strip_prefix("${PLUGIN_ROOT}") {
            out.push_str(&root.to_string_lossy());
            rest = after;
        } else if let Some(after) = tail.strip_prefix("${PLUGIN_DATA}") {
            out.push_str(&data.to_string_lossy());
            rest = after;
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn is_reserved_env_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("PLUGIN_ROOT") || key.eq_ignore_ascii_case("PLUGIN_DATA")
}

/// `command` is one executable token (design §2): a `./` path resolves inside
/// the plugin root, a bare name is left to the platform's executable search.
fn resolve_program(command: &str, root: &Path) -> Result<PathBuf, String> {
    if let Some(rest) = command.strip_prefix("./") {
        if rest.is_empty() {
            return Err("command must be a bare name or a `./` path inside the plugin root".into());
        }
        return crate::plugins::path::resolve_within_maybe_missing(root, &root.join(rest))
            .ok_or_else(|| format!("command `{command}` escapes the plugin root"));
    }
    if command.is_empty()
        || command.contains('/')
        || command.contains('\\')
        || command.chars().any(char::is_whitespace)
    {
        return Err("command must be a bare name or a `./` path inside the plugin root".into());
    }
    Ok(PathBuf::from(command))
}

/// Resolve a plugin server's stdio launch (design §2, rulings R4/R12).
///
/// The parse-time checks in `plugins::layout` are syntactic only: the
/// containment proof for `cwd` is made here, after `PLUGIN_DATA` exists and
/// the expanded result is canonicalized, so a symlink inside the data
/// directory cannot lead the launch outside it.
pub(crate) fn spawn_spec(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&str>,
    root: &Path,
    data: &Path,
) -> Result<CommandSpec, String> {
    // Reserved names are refused in any casing, on every platform (ruling R8),
    // before any filesystem work.
    for key in env.keys() {
        if is_reserved_env_key(key) {
            return Err(format!(
                "`{key}` is reserved: Ducky sets PLUGIN_ROOT and PLUGIN_DATA for plugin servers"
            ));
        }
    }

    // §9.1: the data directory exists before spawn; it is the containment root
    // for a `${PLUGIN_DATA}` cwd.
    std::fs::create_dir_all(data).map_err(|err| {
        format!(
            "cannot create the plugin data directory {}: {err}",
            data.display()
        )
    })?;

    let program = resolve_program(command, root)?;
    let args: Vec<String> = args
        .iter()
        .map(|arg| expand_placeholders(arg, root, data))
        .collect();

    // The server env overlays the inherited environment; the two reserved
    // variables are set last, replacing any same-named entry.
    let mut env_out: Vec<(String, String)> = env
        .iter()
        .filter(|(key, _)| !is_reserved_env_key(key))
        .map(|(key, value)| (key.clone(), expand_placeholders(value, root, data)))
        .collect();
    env_out.push(("PLUGIN_ROOT".to_string(), root.to_string_lossy().into_owned()));
    env_out.push(("PLUGIN_DATA".to_string(), data.to_string_lossy().into_owned()));

    let data_rooted = cwd.is_some_and(|raw| raw.starts_with("${PLUGIN_DATA}"));
    let containment_root = if data_rooted { data } else { root };
    let expanded_cwd = match cwd {
        Some(raw) => {
            let expanded = PathBuf::from(expand_placeholders(raw, root, data));
            if expanded.is_absolute() {
                expanded
            } else {
                // A `./` cwd joins the plugin root, exactly like a `./`
                // program in `resolve_program` (design §2). It must never
                // resolve against the process working directory.
                root.join(expanded)
            }
        }
        None => root.to_path_buf(),
    };
    let canonical_root = std::fs::canonicalize(containment_root).map_err(|err| {
        format!("cannot resolve {}: {err}", containment_root.display())
    })?;
    let resolved_cwd =
        crate::plugins::path::resolve_within_maybe_missing(&canonical_root, &expanded_cwd)
            .ok_or_else(|| {
                format!(
                    "the server's `cwd` {} escapes the plugin directory",
                    expanded_cwd.display()
                )
            })?;
    // A data-rooted cwd is Ducky's to provide, so create it; a plugin-rooted
    // one must already exist in the (read-only) package.
    if data_rooted {
        std::fs::create_dir_all(&resolved_cwd).map_err(|err| {
            format!(
                "cannot create the server working directory {}: {err}",
                resolved_cwd.display()
            )
        })?;
    }

    Ok(CommandSpec {
        program,
        args,
        env: env_out,
        cwd: resolved_cwd,
    })
}

/// The subtitle a server row shows. A plugin-provided URL can carry a
/// query-string token, so - like the plugin view (design §6) - it shows only
/// the host; a user's own URL stays as configured.
pub(crate) fn summary_detail(server: &ResolvedServer) -> String {
    match &server.config.transport {
        McpTransport::Stdio { command, args, .. } => {
            if args.is_empty() {
                command.clone()
            } else {
                format!("{command} {}", args.join(" "))
            }
        }
        McpTransport::Http { url, .. } => match server.origin.kind {
            ServerOriginKind::Plugin => crate::plugins::manager::display_host(url),
            ServerOriginKind::User => url.clone(),
        },
    }
}

fn summarise_challenge(challenge: &str) -> (String, AuthReason) {
    let lower = challenge.to_lowercase();
    if lower.contains("insufficient_scope") {
        (
            format!(
                "The server needs additional permissions. Sign in again to grant them. ({challenge})"
            ),
            AuthReason::Scope,
        )
    } else {
        (
            "This server requires you to sign in before connecting.".to_string(),
            AuthReason::Missing,
        )
    }
}

/// Runtime-specific guidance for a spawn failure, so the hint matches how the
/// command is usually installed (npx → Node.js, uvx → uv).
fn spawn_hint(command: &str) -> &'static str {
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    match base {
        "node" | "npm" | "npx" | "bun" | "bunx" => {
            " Make sure it is installed and on your PATH (Node.js for npx)."
        }
        "uv" | "uvx" => " Make sure uv (Python) is installed and on your PATH.",
        _ => " Make sure the command exists and is on your PATH.",
    }
}

/// Prepend the login shell's PATH to stdio server spawns. A desktop-launched
/// app inherits the GUI session's minimal PATH, which misses version-manager
/// toolchains (nvm/fnm/volta) that only extend PATH in shell profiles.
/// No-op outside Unix. See docs/adr/0003-stdio-server-spawn-path.md.
#[cfg(unix)]
fn augment_command_path(cmd: &mut tokio::process::Command) {
    if let Some(path) = augmented_path() {
        cmd.env("PATH", path);
    }
}

#[cfg(not(unix))]
fn augment_command_path(_cmd: &mut tokio::process::Command) {}

/// The login-shell PATH plus the inherited one as fallback, or None when the
/// login shell couldn't be sourced.
#[cfg(unix)]
fn augmented_path() -> Option<String> {
    let login = login_shell_path()?;
    let inherited = std::env::var("PATH").unwrap_or_default();
    Some(merge_paths(&login, &inherited))
}

/// `$SHELL -l`'s PATH, sourced once per process (spawning a shell is slow).
#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    static LOGIN_PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    LOGIN_PATH
        .get_or_init(|| {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let output = std::process::Command::new(shell)
                .args(["-l", "-c", "printf %s \"$PATH\""])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let path = String::from_utf8(output.stdout).ok()?.trim().to_owned();
            if path.contains('/') {
                Some(path)
            } else {
                None
            }
        })
        .clone()
}

#[cfg(unix)]
pub(super) fn merge_paths(login: &str, inherited: &str) -> String {
    if inherited.is_empty() {
        login.to_owned()
    } else {
        format!("{login}:{inherited}")
    }
}

fn describe_init_error(e: &rmcp::service::ClientInitializeError) -> String {
    use rmcp::service::ClientInitializeError as E;
    match e {
        E::ConnectionClosed(msg) => {
            format!("The server closed the connection immediately ({msg}).")
        }
        E::NoCompatibleProtocolVersion {
            client_supported,
            server_supported,
        } => format!(
            "No compatible MCP protocol version (we support {:?}, server supports {:?}).",
            client_supported
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>(),
            server_supported
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
        ),
        E::JsonRpcError(err) => format!("The server rejected the connection: {err}"),
        E::LegacyFallbackFailed { discover, fallback } => format!(
            "Could not reach the server as a modern (2026-07-28) or legacy client. \
             discover: {discover}; initialize: {fallback}"
        ),
        other => format!("Connection failed: {other}"),
    }
}

/// Convert tool result content to a plain text view for the model.
pub fn content_to_text(content: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::Image(img) => parts.push(format!(
                "[image: {} ({} bytes)]",
                img.mime_type,
                img.data.len()
            )),
            ContentBlock::Audio(a) => parts.push(format!("[audio: {}]", a.mime_type)),
            ContentBlock::ResourceLink(link) => {
                parts.push(format!("[resource link: {} — {}]", link.name, link.uri));
            }
            ContentBlock::Resource(res) => match &res.resource {
                rmcp::model::ResourceContents::TextResourceContents { uri, text, .. } => {
                    parts.push(format!("[embedded resource {uri}]\n{text}"));
                }
                rmcp::model::ResourceContents::BlobResourceContents { uri, blob, .. } => {
                    parts.push(format!(
                        "[embedded resource {uri} (binary, {} bytes)]",
                        blob.len()
                    ));
                }
                _ => {}
            },
            _ => {}
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one place a server tool's `readOnlyHint` becomes the input the mode
    /// gate and the offered list both read: `Some(true)` is what lets a
    /// read-only mode offer and run that tool (ADR-0004). An explicit `false`
    /// and an absent hint must both stay untrusted, so pin the derivation.
    #[test]
    fn tool_entry_takes_the_read_only_hint_from_the_annotations() {
        let schema: Arc<serde_json::Map<String, serde_json::Value>> = serde_json::Map::new().into();
        let entry = |hint: Option<bool>| {
            let mut tool = Tool::new("read_thing", "Read a thing", schema.clone());
            if let Some(hint) = hint {
                tool = tool.with_annotations(rmcp::model::ToolAnnotations::new().read_only(hint));
            }
            tool_entry("srv", "srv", tool)
        };

        let trusted = entry(Some(true));
        assert_eq!(trusted.read_only_hint, Some(true));
        assert_eq!(trusted.qualified_name, "srv__read_thing");
        assert_eq!(entry(Some(false)).read_only_hint, Some(false));
        assert_eq!(entry(None).read_only_hint, None);
    }
}
