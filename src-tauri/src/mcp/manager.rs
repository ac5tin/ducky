//! MCP connection manager: owns one rmcp client per configured server, keeps
//! a cached view of each server's tools/resources/prompts, and exposes
//! operations to the rest of the app.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, CompleteRequestParams, CompleteResult, ContentBlock,
    GetPromptRequestParams, GetPromptResult, JsonObject, ListToolsResult, NumberOrString,
    PaginatedRequestParams, ProgressToken, ProtocolVersion, ReadResourceRequestParams,
    ReadResourceResult, Reference, RequestMetaObject, ServerNotification, SubscriptionFilter, Tool,
};
use rmcp::service::{ClientLifecycleMode, Peer, RunningService, RoleClient};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::transport::TokioChildProcess;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::bridge::InteractiveBridge;
use super::handler::DuckyClientHandler;
use crate::config::{HttpAuth, McpServerConfig, McpTransport, Store};
use crate::events::{BackendEvent, EventSink};

// ---------------------------------------------------------------------------
// Public view types (serialised to the webview)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Disconnected,
    Connecting,
    Connected,
    /// The server answered 401/403: the user must authorise (OAuth) or fix
    /// their token.
    NeedsAuth { detail: Option<String> },
    Error { message: String },
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
        self.service
            .lock()
            .await
            .as_ref()
            .map(|s| s.peer().clone())
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
        service.read_resource(params).await.map_err(|e| e.to_string())
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
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

pub struct McpManager {
    store: Arc<Store>,
    bridge: Arc<InteractiveBridge>,
    sink: Arc<dyn EventSink>,
    handles: Mutex<HashMap<String, Arc<ServerHandle>>>,
    statuses: Mutex<HashMap<String, ServerStatus>>,
}

/// A transport ready to be served, in either flavour.
enum BuiltTransport {
    Stdio(TokioChildProcess),
    Http(StreamableHttpClientTransport<reqwest::Client>),
}

impl McpManager {
    pub fn new(store: Arc<Store>, bridge: Arc<InteractiveBridge>, sink: Arc<dyn EventSink>) -> Self {
        Self {
            store,
            bridge,
            sink,
            handles: Mutex::new(HashMap::new()),
            statuses: Mutex::new(HashMap::new()),
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
            ServerStatus::NeedsAuth { detail } => detail.clone(),
            _ => None,
        };
        self.statuses
            .lock()
            .unwrap()
            .insert(server_id.to_string(), status);
        self.sink.emit(BackendEvent::ServerStatus {
            server_id: server_id.to_string(),
            status: status_str.to_string(),
            detail,
        });
    }

    pub fn status(&self, server_id: &str) -> ServerStatus {
        self.statuses
            .lock()
            .unwrap()
            .get(server_id)
            .cloned()
            .unwrap_or(ServerStatus::Disconnected)
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
        let cfg = {
            let cfg_guard = self.store.config.lock().unwrap();
            cfg_guard
                .mcp_servers
                .iter()
                .find(|s| s.id == server_id)
                .cloned()
        };
        let Some(cfg) = cfg else {
            return Err("Unknown server id".into());
        };

        // Drop any existing connection first.
        self.disconnect(server_id).await;
        self.set_status(server_id, ServerStatus::Connecting);

        // Make sure OAuth tokens are fresh (no-op for other auth modes).
        if let McpTransport::Http { .. } = &cfg.transport {
            if let HttpAuth::OAuth = &cfg.auth {
                if let Err(e) = crate::oauth::ensure_fresh_token(&self.store, &cfg).await {
                    self.set_status(
                        server_id,
                        ServerStatus::NeedsAuth { detail: Some(e.to_string()) },
                    );
                    return Ok(self.status(server_id));
                }
            }
        }

        let handler = DuckyClientHandler {
            server_id: Arc::from(server_id),
            server_name: Arc::from(Self::server_slug(&cfg).as_str()),
            server_title: Arc::from(cfg.name.as_str()),
            bridge: self.bridge.clone(),
            store: self.store.clone(),
            sink: self.sink.clone(),
        };

        let ct = CancellationToken::new();
        let built = match self.build_transport(&cfg) {
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

        let service = match built {
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
        };

        let service = match service {
            Ok(s) => s,
            Err(e) => {
                if e.is_authorization_required() {
                    let detail = e.auth_challenge().map(summarise_challenge);
                    self.set_status(server_id, ServerStatus::NeedsAuth { detail });
                    return Ok(self.status(server_id));
                }
                self.set_status(server_id, ServerStatus::Error { message: describe_init_error(&e) });
                return Ok(self.status(server_id));
            }
        };

        let handle = Arc::new(ServerHandle {
            cfg: cfg.clone(),
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

        // Learn everything about the server.
        match self.refresh_data(&handle).await {
            Ok(data) => {
                *handle.data.lock().unwrap() = data;
            }
            Err(e) => {
                handle.log(format!("listing failed: {e}"));
            }
        }

        // Open a subscriptions/listen stream (modern servers only; legacy
        // servers push notifications through the client handler instead).
        self.spawn_list_changed_subscription(handle.clone());

        self.handles
            .lock()
            .unwrap()
            .insert(server_id.to_string(), handle);
        self.set_status(server_id, ServerStatus::Connected);
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
                let _ = service.cancel().await;
            }
        }
        self.statuses
            .lock()
            .unwrap()
            .insert(server_id.to_string(), ServerStatus::Disconnected);
        self.sink.emit(BackendEvent::ServerStatus {
            server_id: server_id.to_string(),
            status: "disconnected".into(),
            detail: None,
        });
    }

    /// Connect every enabled server that is not already connected.
    pub async fn connect_enabled(&self) {
        let ids: Vec<(String, bool)> = {
            let cfg = self.store.config.lock().unwrap();
            cfg.mcp_servers
                .iter()
                .map(|s| (s.id.clone(), s.enabled && s.auto_start))
                .collect()
        };
        for (id, should) in ids {
            if should
                && !matches!(
                    self.status(&id),
                    ServerStatus::Connected | ServerStatus::Connecting
                )
            {
                let _ = self.connect(&id).await;
            }
        }
    }

    fn build_transport(&self, cfg: &McpServerConfig) -> Result<BuiltTransport, String> {
        match &cfg.transport {
            McpTransport::Stdio { command, args, env } => {
                let mut cmd = tokio::process::Command::new(command);
                cmd.args(args).envs(env);
                let (child, stderr) = TokioChildProcess::builder(cmd)
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| {
                        format!(
                            "Could not start `{command}`: {e}. Make sure the command exists \
                             (and that Node.js is installed for npx-based servers)."
                        )
                    })?;
                if let Some(stderr) = stderr {
                    let server_id = cfg.id.clone();
                    let sink = self.sink.clone();
                    tokio::spawn(async move {
                        use tokio::io::AsyncBufReadExt;
                        let mut lines = tokio::io::BufReader::new(stderr).lines();
                        loop {
                            match lines.next_line().await {
                                Ok(Some(line)) => {
                                    // forward server logs as an event the UI
                                    // can render in the connector detail view
                                    sink.emit(BackendEvent::ServerDataChanged {
                                        server_id: server_id.clone(),
                                        what: format!("log:{line}"),
                                    });
                                }
                                _ => break,
                            }
                        }
                    });
                }
                Ok(BuiltTransport::Stdio(child))
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

                // attach bearer credentials when available
                let token = match &cfg.auth {
                    HttpAuth::Bearer { .. } => self.store.server_token(&cfg.id),
                    HttpAuth::OAuth => self.store.oauth_tokens(&cfg.id).map(|t| t.access_token),
                    HttpAuth::None => None,
                };
                if let Some(token) = token {
                    config = config.auth_header(format!("Bearer {token}"));
                }

                Ok(BuiltTransport::Http(StreamableHttpClientTransport::from_config(
                    config,
                )))
            }
        }
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

        if let Some(info) = handle.peer_info().await {
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

        // resources + templates (skip silently when the capability is absent)
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

        // prompts
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
    /// is handled inside rmcp via the interactive bridge.
    pub async fn call_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: serde_json::Value,
        progress_token: Option<String>,
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
        handle.call_tool_mrtr(params).await
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
        let cfg = {
            let cfg = self.store.config.lock().unwrap();
            cfg.mcp_servers
                .iter()
                .find(|s| s.id == server_id)
                .cloned()
        };
        let (name, enabled, transport_kind, detail) = match &cfg {
            Some(c) => (
                c.name.clone(),
                c.enabled,
                match &c.transport {
                    McpTransport::Stdio { .. } => "stdio".to_string(),
                    McpTransport::Http { .. } => "http".to_string(),
                },
                match &c.transport {
                    McpTransport::Stdio { command, args, .. } => {
                        if args.is_empty() {
                            command.clone()
                        } else {
                            format!("{command} {}", args.join(" "))
                        }
                    }
                    McpTransport::Http { url, .. } => url.clone(),
                },
            ),
            None => ("unknown".into(), false, "stdio".into(), String::new()),
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
                None => (
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    None,
                    None,
                    Vec::new(),
                ),
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
        }
    }

    pub fn summaries(&self) -> Vec<ServerSummary> {
        let ids: Vec<String> = {
            let cfg = self.store.config.lock().unwrap();
            cfg.mcp_servers.iter().map(|s| s.id.clone()).collect()
        };
        ids.into_iter().map(|id| self.summary(&id)).collect()
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

fn summarise_challenge(challenge: &str) -> String {
    let lower = challenge.to_lowercase();
    if lower.contains("insufficient_scope") {
        format!(
            "The server needs additional permissions. Sign in again to grant them. ({challenge})"
        )
    } else {
        "This server requires you to sign in before connecting.".to_string()
    }
}

fn describe_init_error(e: &rmcp::service::ClientInitializeError) -> String {
    use rmcp::service::ClientInitializeError as E;
    match e {
        E::ConnectionClosed(msg) => format!(
            "The server closed the connection immediately ({msg}). For npx-based servers, \
             check that Node.js is installed."
        ),
        E::NoCompatibleProtocolVersion {
            client_supported,
            server_supported,
        } => format!(
            "No compatible MCP protocol version (we support {:?}, server supports {:?}).",
            client_supported.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
            server_supported.iter().map(|v| v.as_str()).collect::<Vec<_>>()
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
            ContentBlock::Image(img) => {
                parts.push(format!("[image: {} ({} bytes)]", img.mime_type, img.data.len()))
            }
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
