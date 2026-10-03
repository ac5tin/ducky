//! End-to-end tests: a real rmcp client (DuckyClientHandler + bridge) talking
//! to an in-process rmcp server over a tokio duplex transport — including the
//! MCP 2026-07-28 MRTR elicitation round-trip.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ElicitRequest,
    ElicitRequestParams, ElicitationSchema, Implementation, InputRequest, InputRequiredResult,
    ListToolsResult, PaginatedRequestParams, PrimitiveSchemaDefinition, ProtocolVersion,
    ServerCapabilities, ServerInfo, StringSchema, Tool,
};
use rmcp::service::{
    serve_client_with_lifecycle_and_ct, ClientLifecycleMode, RequestContext, RoleServer,
};
use rmcp::{ErrorData as McpError, ServiceExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::bridge::InteractiveBridge;
use super::handler::DuckyClientHandler;
use super::manager::{
    expand_placeholders, merge_servers, spawn_spec, AuthReason, McpManager, ServerOriginKind,
    ServerStatus,
};
use crate::config::{HttpAuth, McpServerConfig, McpTransport, Store};
use crate::events::{BackendEvent, CollectingSink};
use crate::plugins::layout::{PluginServer, PluginTransport, RemoteKind};
use crate::plugins::manager::{PluginIndex, PluginManager, PluginServerRef};

// ---------------------------------------------------------------------------
// Test server
// ---------------------------------------------------------------------------

/// A stateless server with:
/// - `echo` — returns the message
/// - `ask_name` — an MRTR tool: first response is `input_required` with an
///   elicitation; the retry (carrying `inputResponses`) completes the call.
struct EchoServer;

impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("echo-server", "1.0.0"))
            .with_instructions("Echoes and asks questions.")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let schema: Arc<serde_json::Map<String, serde_json::Value>> =
            serde_json::from_value(serde_json::json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"]
            }))
            .unwrap();
        let ask_schema: Arc<serde_json::Map<String, serde_json::Value>> = serde_json::from_value(
            serde_json::json!({"type": "object", "additionalProperties": false}),
        )
        .unwrap();
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("echo", "Echo the message back", schema),
            Tool::new("ask_name", "Ask the user for their name", ask_schema),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match request.name.as_ref() {
            "echo" => {
                let msg = request
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(
                    CallToolResult::success(vec![ContentBlock::text(format!("echo: {msg}"))])
                        .into(),
                )
            }
            "ask_name" => {
                if let Some(responses) = &request.input_responses {
                    // retry after elicitation — complete the call
                    let name = responses
                        .get("name")
                        .and_then(|v| v.get("content"))
                        .and_then(|c| c.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("stranger");
                    return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Hello, {name}!"
                    ))])
                    .into());
                }
                // first response — request input via MRTR elicitation
                let mut input_requests: BTreeMap<String, InputRequest> = BTreeMap::new();
                input_requests.insert(
                    "name".to_string(),
                    InputRequest::Elicitation(ElicitRequest::new(
                        ElicitRequestParams::FormElicitationParams {
                            meta: None,
                            message: "What is your name?".to_string(),
                            requested_schema: ElicitationSchema::builder()
                                .required_property(
                                    "name",
                                    PrimitiveSchemaDefinition::String(StringSchema::new()),
                                )
                                .build()
                                .unwrap(),
                        },
                    )),
                );
                Ok(CallToolResponse::InputRequired(
                    InputRequiredResult::from_input_requests(input_requests),
                ))
            }
            other => Err(McpError::invalid_request(
                format!("Unknown tool: {other}"),
                None,
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Test client wiring (mirrors what McpManager does)
// ---------------------------------------------------------------------------

fn test_handler(bridge: Arc<InteractiveBridge>, sink: Arc<CollectingSink>) -> DuckyClientHandler {
    DuckyClientHandler {
        server_id: Arc::from("test-server"),
        server_name: Arc::from("test_server"),
        server_title: Arc::from("Test Server"),
        bridge,
        store: Arc::new(
            Store::new(
                std::path::Path::new("/tmp/ducky-test-store"),
                std::env::temp_dir(),
            )
            .unwrap(),
        ),
        sink,
    }
}

async fn connect(
    handler: DuckyClientHandler,
    ct: CancellationToken,
) -> rmcp::service::RunningService<rmcp::RoleClient, DuckyClientHandler> {
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let server = EchoServer.serve(server_transport).await.unwrap();
        let _ = server.waiting().await;
    });
    serve_client_with_lifecycle_and_ct(
        handler,
        client_transport,
        ClientLifecycleMode::Auto {
            preferred_versions: vec![
                ProtocolVersion::V_2026_07_28,
                ProtocolVersion::V_2025_11_25,
                ProtocolVersion::V_2025_06_18,
            ],
            legacy_version: None,
        },
        ct,
    )
    .await
    .expect("client should connect")
}

fn temp_bridge() -> (Arc<InteractiveBridge>, Arc<CollectingSink>) {
    let sink = Arc::new(CollectingSink::default());
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(dir.path(), dir.path().to_path_buf()).unwrap());
    std::mem::forget(dir); // keep config files alive for the test process
    (Arc::new(InteractiveBridge::new(sink.clone(), store)), sink)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connects_with_modern_protocol_and_lists_tools() {
    let (bridge, _sink) = temp_bridge();
    let handler = test_handler(bridge, Arc::new(CollectingSink::default()));
    let service = connect(handler, CancellationToken::new()).await;

    // modern servers expose their negotiated protocol version
    let info = service.peer_info().expect("peer info after discover");
    assert_eq!(info.protocol_version.as_str(), "2026-07-28");
    assert_eq!(
        info.server_info.as_ref().map(|s| s.name.as_str()),
        Some("echo-server")
    );

    let peer = service.peer().clone();
    let tools = peer.list_tools(None).await.expect("tools/list");
    let names: Vec<String> = tools.tools.iter().map(|t| t.name.to_string()).collect();
    assert!(names.contains(&"echo".to_string()));
    assert!(names.contains(&"ask_name".to_string()));

    let mut params = CallToolRequestParams::new("echo".to_string());
    params.arguments =
        Some(serde_json::from_value(serde_json::json!({ "message": "hi" })).unwrap());
    let result = service.call_tool(params).await.expect("call echo");
    assert_eq!(result.is_error, Some(false));
    let text = result
        .content
        .iter()
        .find_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .expect("text content");
    assert_eq!(text, "echo: hi");
}

#[tokio::test]
async fn mrtr_elicitation_roundtrip() {
    let (bridge, sink) = temp_bridge();
    let handler = test_handler(bridge.clone(), sink.clone());
    let service = connect(handler, CancellationToken::new()).await;

    let mut params = CallToolRequestParams::new("ask_name".to_string());
    params.arguments = Some(serde_json::Map::new());

    // Drive the tool call in the background; it will emit an elicitation
    // request to the sink and block until we resolve it.
    let call = tokio::spawn(async move { service.call_tool(params).await });

    // wait for the elicitation event
    let request_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            {
                let events = sink.events.lock().unwrap();
                if let Some(BackendEvent::ElicitationRequested {
                    request_id,
                    mode,
                    message,
                    ..
                }) = events.iter().rev().find_map(|e| match e {
                    BackendEvent::ElicitationRequested { .. } => Some(e.clone()),
                    _ => None,
                }) {
                    assert_eq!(mode, "form");
                    assert_eq!(message, "What is your name?");
                    return request_id;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("elicitation request emitted");

    // the user answers the form
    assert!(bridge.resolve_elicitation(
        &request_id,
        rmcp::model::ElicitResult::new(rmcp::model::ElicitationAction::Accept)
            .with_content(serde_json::json!({ "name": "Ducky" })),
    ));

    // the tool call completes with the elicited data
    let result = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("call finishes")
        .expect("join")
        .expect("call ok");
    let text = result
        .content
        .iter()
        .find_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .expect("text content");
    assert_eq!(text, "Hello, Ducky!");
}

#[tokio::test]
async fn elicitation_decline_surfaces_to_server() {
    let (bridge, sink) = temp_bridge();
    let handler = test_handler(bridge.clone(), sink.clone());
    let service = connect(handler, CancellationToken::new()).await;

    let mut params = CallToolRequestParams::new("ask_name".to_string());
    params.arguments = Some(serde_json::Map::new());

    let call = tokio::spawn(async move { service.call_tool(params).await });

    let request_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = {
                let events = sink.events.lock().unwrap();
                events.iter().rev().find_map(|e| match e {
                    BackendEvent::ElicitationRequested { request_id, .. } => {
                        Some(request_id.clone())
                    }
                    _ => None,
                })
            };
            if let Some(id) = found {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("elicitation request emitted");

    // declining yields an explicit decline response to the server
    assert!(bridge.resolve_elicitation(
        &request_id,
        rmcp::model::ElicitResult::new(rmcp::model::ElicitationAction::Decline),
    ));

    // the server responds (with a generic greeting) and the call completes
    let result = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("call finishes")
        .expect("join")
        .expect("call ok");
    let text = result
        .content
        .iter()
        .find_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .expect("text content");
    assert_eq!(text, "Hello, stranger!");
}

#[tokio::test]
async fn notify_auth_completed_wakes_waiter() {
    let sink = Arc::new(CollectingSink::default());
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(dir.path(), dir.path().to_path_buf()).unwrap());
    let bridge = Arc::new(InteractiveBridge::new(sink.clone(), store.clone()));
    let manager = Arc::new(McpManager::new(store, bridge, sink, plugins_for(dir.path())));

    let waiter = tokio::spawn({
        let m = manager.clone();
        async move { m.wait_for_auth("s", 0, Duration::from_secs(2)).await }
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let m = manager.clone();
    let notified = tokio::spawn(async move {
        m.notify_auth_completed("s");
    });
    tokio::time::timeout(Duration::from_secs(1), notified)
        .await
        .expect("notify_auth_completed deadlocked")
        .expect("notify task");
    let result = tokio::time::timeout(Duration::from_secs(1), waiter).await;
    assert!(result.is_ok(), "waiter did not resume");
    assert!(result.unwrap().unwrap().is_ok());
}

#[tokio::test]
async fn oauth_connect_reaches_server_before_needs_auth() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let response = concat!(
            "HTTP/1.1 401 Unauthorized\r\n",
            "WWW-Authenticate: Bearer resource_metadata=\"https://auth.example/.well-known\"\r\n",
            "Content-Length: 0\r\n",
            "Connection: close\r\n\r\n"
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = tx.send(());
    });

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(dir.path(), dir.path().to_path_buf()).unwrap());
    store
        .config
        .lock()
        .unwrap()
        .mcp_servers
        .push(McpServerConfig {
            id: "oauth-server".into(),
            name: "OAuth Server".into(),
            transport: McpTransport::Http {
                url: format!("http://{addr}/mcp"),
                headers: HashMap::new(),
            },
            auth: HttpAuth::OAuth,
            enabled: true,
            auto_start: true,
            oauth_client_id: None,
            oauth_redirect_port: None,
            created_at: "now".into(),
        });
    let sink = Arc::new(CollectingSink::default());
    let bridge = Arc::new(InteractiveBridge::new(sink.clone(), store.clone()));
    let manager = McpManager::new(store, bridge, sink, plugins_for(dir.path()));

    let status = manager.connect("oauth-server").await.unwrap();
    assert!(matches!(
        status,
        ServerStatus::NeedsAuth {
            reason: Some(AuthReason::Missing),
            ..
        }
    ));
    tokio::time::timeout(Duration::from_secs(1), rx)
        .await
        .expect("connect should reach the server")
        .expect("server request signal");
}

/// A stdio server that exits immediately (e.g. `npx` on a missing package)
/// must surface its stderr in both the error detail and the summary logs —
/// that output is the only real diagnosis (npm's E404, crash traces, …).
#[cfg(unix)] // spawns `sh`
#[tokio::test]
async fn stdio_connect_failure_quotes_stderr_tail() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(dir.path(), dir.path().to_path_buf()).unwrap());
    store
        .config
        .lock()
        .unwrap()
        .mcp_servers
        .push(McpServerConfig {
            id: "die-server".into(),
            name: "Die Server".into(),
            transport: McpTransport::Stdio {
                command: "sh".into(),
                args: vec!["-c".into(), "echo ducky-boom-404 >&2".into()],
                env: HashMap::new(),
            },
            auth: HttpAuth::None,
            enabled: true,
            auto_start: false,
            oauth_client_id: None,
            oauth_redirect_port: None,
            created_at: "now".into(),
        });
    let sink = Arc::new(CollectingSink::default());
    let bridge = Arc::new(InteractiveBridge::new(sink.clone(), store.clone()));
    let manager = McpManager::new(store, bridge, sink, plugins_for(dir.path()));

    let status = manager.connect("die-server").await.unwrap();
    let ServerStatus::Error { message } = status else {
        panic!("expected error status, got {status:?}")
    };
    assert!(
        message.contains("ducky-boom-404"),
        "error should quote stderr; got: {message}"
    );
    let summary = manager.summary("die-server");
    assert!(
        summary.logs.iter().any(|l| l.contains("ducky-boom-404")),
        "summary logs should carry stderr; got: {:?}",
        summary.logs
    );

    // removing the server drops the captured output along with its status
    manager.forget("die-server");
    assert!(manager.summary("die-server").logs.is_empty());
}

#[cfg(unix)]
#[test]
fn merge_paths_puts_login_shell_first() {
    use super::manager::merge_paths;
    assert_eq!(
        merge_paths("/nvm/bin", "/usr/bin:/bin"),
        "/nvm/bin:/usr/bin:/bin"
    );
    assert_eq!(merge_paths("/nvm/bin", ""), "/nvm/bin");
}

// ---------------------------------------------------------------------------
// Plugin server merge and launch rules (design §2/§9, rulings R4/R12)
// ---------------------------------------------------------------------------

fn stdio_config(id: &str, name: &str) -> McpServerConfig {
    McpServerConfig {
        id: id.into(),
        name: name.into(),
        transport: McpTransport::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "pkg".into()],
            env: HashMap::new(),
        },
        auth: HttpAuth::None,
        enabled: true,
        auto_start: true,
        oauth_client_id: None,
        oauth_redirect_port: None,
        created_at: "now".into(),
    }
}

fn plugin_stdio_ref(id: &str, plugin_id: &str, name: &str, cwd: Option<&str>) -> PluginServerRef {
    PluginServerRef {
        id: id.into(),
        plugin_id: plugin_id.into(),
        server: PluginServer {
            name: name.into(),
            transport: PluginTransport::Stdio {
                command: "./bin/validator".into(),
                args: vec!["--cache".into(), "${PLUGIN_DATA}/cache".into()],
                env: BTreeMap::new(),
                cwd: cwd.map(str::to_string),
            },
        },
    }
}

fn plugin_remote_ref(id: &str, plugin_id: &str, name: &str) -> PluginServerRef {
    PluginServerRef {
        id: id.into(),
        plugin_id: plugin_id.into(),
        server: PluginServer {
            name: name.into(),
            transport: PluginTransport::Remote {
                kind: RemoteKind::StreamableHttp,
                url: "https://example.com/mcp".into(),
                headers: BTreeMap::from([("X-Plug".to_string(), "1".to_string())]),
            },
        },
    }
}

fn plugins_for(dir: &std::path::Path) -> Arc<PluginManager> {
    PluginManager::new(dir, dir, None)
}

#[test]
fn merge_keeps_user_servers_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let user = stdio_config("user-1", "Filesystem");
    let merged = merge_servers(std::slice::from_ref(&user), &PluginIndex::default(), dir.path());
    assert_eq!(merged.len(), 1);
    let row = &merged[0];
    assert_eq!(row.config.id, user.id);
    assert_eq!(row.config.name, user.name);
    assert_eq!(row.config.enabled, user.enabled);
    assert_eq!(row.config.auto_start, user.auto_start);
    assert!(matches!(
        &row.config.transport,
        McpTransport::Stdio { command, args, .. }
            if command == "npx" && args == &["-y".to_string(), "pkg".to_string()]
    ));
    assert_eq!(row.origin.kind, ServerOriginKind::User);
    assert_eq!(row.origin.plugin_id, None);
    assert_eq!(row.origin.plugin_name, None);
    assert!(row.launch.is_none());
}

#[test]
fn merge_adds_enabled_plugin_servers() {
    let dir = tempfile::tempdir().unwrap();
    let index = PluginIndex {
        servers: vec![plugin_stdio_ref(
            "plugin:acme:validator",
            "acme",
            "validator",
            None,
        )],
        ..Default::default()
    };
    let merged = merge_servers(&[], &index, dir.path());
    assert_eq!(merged.len(), 1);
    let row = &merged[0];
    assert_eq!(row.config.id, "plugin:acme:validator");
    assert_eq!(row.config.name, "validator");
    assert!(row.config.enabled && row.config.auto_start);
    assert!(matches!(row.config.auth, HttpAuth::None));
    assert_eq!(row.origin.kind, ServerOriginKind::Plugin);
    assert_eq!(row.origin.plugin_id.as_deref(), Some("acme"));
    assert_eq!(row.origin.plugin_name.as_deref(), Some("acme"));
    let launch = row.launch.as_ref().expect("plugin server must carry a launch");
    assert_eq!(launch.root, dir.path().join("plugins/acme/package"));
    assert_eq!(launch.data, dir.path().join("plugin-data/acme"));
}

#[test]
fn merge_maps_remote_plugin_servers_to_http_auth_none() {
    let dir = tempfile::tempdir().unwrap();
    let index = PluginIndex {
        servers: vec![plugin_remote_ref("plugin:acme:remote", "acme", "remote")],
        ..Default::default()
    };
    let merged = merge_servers(&[], &index, dir.path());
    let row = merged.first().expect("remote plugin server merged");
    assert!(matches!(row.config.auth, HttpAuth::None));
    let McpTransport::Http { url, headers } = &row.config.transport else {
        panic!("remote plugin server must map to the HTTP transport");
    };
    assert_eq!(url, "https://example.com/mcp");
    assert_eq!(headers.get("X-Plug").map(String::as_str), Some("1"));
}

#[test]
fn user_server_wins_name_collision() {
    let dir = tempfile::tempdir().unwrap();
    let exact = PluginIndex {
        servers: vec![plugin_stdio_ref(
            "plugin:acme:validator",
            "acme",
            "validator",
            None,
        )],
        ..Default::default()
    };
    let merged = merge_servers(&[stdio_config("user-1", "validator")], &exact, dir.path());
    assert_eq!(merged.len(), 1, "the plugin server must be shadowed");
    assert_eq!(merged[0].config.id, "user-1");
    assert_eq!(merged[0].origin.kind, ServerOriginKind::User);

    // the comparison is case-insensitive, like the rest of the UI's naming
    let cased = PluginIndex {
        servers: vec![plugin_stdio_ref(
            "plugin:acme:validator",
            "acme",
            "VALIDATOR",
            None,
        )],
        ..Default::default()
    };
    let merged = merge_servers(&[stdio_config("user-1", "validator")], &cased, dir.path());
    assert_eq!(merged.len(), 1, "a case-different name still collides");
    assert_eq!(merged[0].config.id, "user-1");
}

#[test]
fn chat_allow_list_still_filters_plugin_ids() {
    let meta: crate::config::ConversationMeta = serde_json::from_value(serde_json::json!({
        "id": "c1",
        "title": "t",
        "provider_id": "p",
        "model": "m",
        "mcp_ids": ["plugin:acme:validator"],
        "created_at": "now",
        "updated_at": "now"
    }))
    .unwrap();
    assert!(meta.allows_mcp("plugin:acme:validator"));
    assert!(!meta.allows_mcp("plugin:acme:other"));
    assert!(!meta.allows_mcp("user-1"));
}

#[test]
fn expand_placeholders_single_pass() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("${PLUGIN_DATA}").join("plugin");
    std::fs::create_dir_all(&root).unwrap();
    let data = dir.path().join("data");

    let expanded = expand_placeholders("${PLUGIN_ROOT}/x", &root, &data);
    assert_eq!(expanded, format!("{}/x", root.display()));
    // The replacement text itself carries ${PLUGIN_DATA}; it must not be
    // rescanned, so the literal survives inside the expanded value.
    assert!(
        expanded.contains("${PLUGIN_DATA}"),
        "introduced text must never be rescanned: {expanded}"
    );

    let both = expand_placeholders("${PLUGIN_ROOT}-${PLUGIN_DATA}", &root, &data);
    assert_eq!(both, format!("{}-{}", root.display(), data.display()));
}

#[test]
fn expand_leaves_unknown_placeholders_literal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let data = dir.path().join("data");
    assert_eq!(expand_placeholders("${OTHER}/x", &root, &data), "${OTHER}/x");
    assert_eq!(
        expand_placeholders("${PLUGIN_ROOT", &root, &data),
        "${PLUGIN_ROOT"
    );
    assert_eq!(
        expand_placeholders("$PLUGIN_ROOT", &root, &data),
        "$PLUGIN_ROOT"
    );
}

#[test]
fn expand_does_not_touch_command() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("plugin-data").join("acme");
    let spec = spawn_spec(
        "./bin/${PLUGIN_DATA}",
        &[],
        &BTreeMap::new(),
        None,
        &root,
        &data,
    )
    .unwrap();
    assert_eq!(
        spec.program,
        root.join("bin/${PLUGIN_DATA}"),
        "placeholders in `command` stay literal"
    );
}

#[test]
fn stdio_defaults_cwd_to_plugin_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("plugin-data").join("acme");
    let spec = spawn_spec("./bin/tool", &[], &BTreeMap::new(), None, &root, &data).unwrap();
    assert_eq!(spec.cwd, root);
    assert!(data.is_dir(), "spawn_spec must create PLUGIN_DATA");
}

#[test]
fn stdio_rejects_reserved_env_keys() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("plugin-data").join("acme");
    for key in ["PLUGIN_ROOT", "PLUGIN_DATA"] {
        let env = BTreeMap::from([(key.to_string(), "/tmp/evil".to_string())]);
        let err = spawn_spec("./bin/tool", &[], &env, None, &root, &data)
            .expect_err("a server may not declare a reserved variable");
        assert!(err.contains(key), "the error must name {key}: {err}");
    }
}

/// Ruling R8: the reserved-name rule is case-insensitive on every platform,
/// so the Windows-cased key is rejected on Linux too.
#[test]
fn windows_env_key_case_is_reserved() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("plugin-data").join("acme");
    for key in ["plugin_root", "Plugin_Data"] {
        let env = BTreeMap::from([(key.to_string(), "/tmp/evil".to_string())]);
        let err = spawn_spec("./bin/tool", &[], &env, None, &root, &data)
            .expect_err("a differently-cased reserved name is still reserved");
        assert!(err.contains(key), "the error must name {key}: {err}");
    }
}

#[test]
fn plugin_data_dir_created_before_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("plugin-data").join("acme");
    let spec = spawn_spec(
        "./bin/tool",
        &[],
        &BTreeMap::new(),
        Some("${PLUGIN_DATA}/sub"),
        &root,
        &data,
    )
    .unwrap();
    assert!(data.is_dir(), "PLUGIN_DATA must exist after spawn_spec");
    assert_eq!(spec.cwd, data.join("sub"));
    assert!(
        spec.cwd.is_dir(),
        "a ${{PLUGIN_DATA}}-rooted cwd must exist after spawn_spec"
    );
}

#[test]
fn claude_alias_expanded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let data = dir.path().join("data");
    // layout.rs normalises the claude/zcode aliases at parse time; raw alias
    // text that still reaches the spawn layer stays literal.
    assert_eq!(
        expand_placeholders("${CLAUDE_PLUGIN_ROOT}/x", &root, &data),
        "${CLAUDE_PLUGIN_ROOT}/x"
    );
}

#[test]
fn spawn_injects_reserved_env_last() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("plugin-data").join("acme");
    let env = BTreeMap::from([
        ("FOO".to_string(), "${PLUGIN_ROOT}/x".to_string()),
        ("BAR".to_string(), "${PLUGIN_DATA}/y".to_string()),
    ]);
    let spec = spawn_spec("./bin/tool", &[], &env, None, &root, &data).unwrap();
    assert_eq!(
        spec.env.get(spec.env.len() - 2),
        Some(&("PLUGIN_ROOT".to_string(), root.display().to_string()))
    );
    assert_eq!(
        spec.env.last(),
        Some(&("PLUGIN_DATA".to_string(), data.display().to_string()))
    );
    assert_eq!(
        spec.env.iter().find(|(key, _)| key == "FOO").map(|(_, value)| value.as_str()),
        Some(format!("{}/x", root.display()).as_str())
    );
    assert_eq!(
        spec.env.iter().find(|(key, _)| key == "BAR").map(|(_, value)| value.as_str()),
        Some(format!("{}/y", data.display()).as_str())
    );
}

/// Ruling R12: the parse-time `cwd` check in `layout.rs` is syntactic only.
/// A symlink planted inside the data directory that points outside it defeats
/// a syntactic check; the canonicalized containment check must reject it.
#[cfg(unix)]
#[test]
fn plugin_cwd_symlink_outside_data_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("plugin-data").join("acme");
    std::fs::create_dir_all(&data).unwrap();
    let outside = root.join("package-outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, data.join("escape")).unwrap();

    let err = spawn_spec(
        "./bin/tool",
        &[],
        &BTreeMap::new(),
        Some("${PLUGIN_DATA}/escape"),
        &root,
        &data,
    )
    .expect_err("a symlink out of the data directory must be rejected");
    assert!(err.contains("cwd"), "the error must name the cwd: {err}");
}

/// A user server keeps today's spawn path: configured working directory,
/// `~` expansion in args, and no plugin variables injected.
#[cfg(unix)] // spawns `sh`
#[tokio::test]
async fn user_server_spawn_keeps_working_dir_tilde_and_no_plugin_env() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let store = Arc::new(Store::new(dir.path(), home.clone()).unwrap());
    {
        let mut cfg = store.config.lock().unwrap();
        cfg.settings.working_dir = Some(work.display().to_string());
        cfg.mcp_servers.push(McpServerConfig {
            id: "user-echo".into(),
            name: "User Echo".into(),
            transport: McpTransport::Stdio {
                command: "sh".into(),
                args: vec![
                    "-c".into(),
                    "printf 'cwd=%s\\narg=%s\\nroot=%s\\n' \"$(pwd -P)\" \"$1\" \"${PLUGIN_ROOT-unset}\" 1>&2; exit 1".into(),
                    "sh".into(),
                    "~/sentinel".into(),
                ],
                env: HashMap::new(),
            },
            auth: HttpAuth::None,
            enabled: true,
            auto_start: true,
            oauth_client_id: None,
            oauth_redirect_port: None,
            created_at: "now".into(),
        });
    }
    let sink = Arc::new(CollectingSink::default());
    let bridge = Arc::new(InteractiveBridge::new(sink.clone(), store.clone()));
    let manager = McpManager::new(store, bridge, sink, plugins_for(dir.path()));

    let status = manager.connect("user-echo").await.unwrap();
    let ServerStatus::Error { message } = status else {
        panic!("expected the immediately-exiting server to error, got {status:?}")
    };
    assert!(
        message.contains(&format!("cwd={}", work.display())),
        "the user working dir must be used: {message}"
    );
    assert!(
        message.contains(&format!("arg={}", home.join("sentinel").display())),
        "`~` in args must still expand to the home dir: {message}"
    );
    assert!(
        message.contains("root=unset"),
        "user servers must not get plugin variables: {message}"
    );
}

#[cfg(windows)]
#[test]
fn windows_cmd_script_command_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("tool.cmd"), "@echo off\r\n").unwrap();
    let data = root.join("plugin-data").join("acme");
    let spec = spawn_spec(
        "./bin/tool.cmd",
        &["--flag".into(), "value".into()],
        &BTreeMap::new(),
        None,
        &root,
        &data,
    )
    .unwrap();
    assert!(
        spec.program.ends_with("tool.cmd"),
        "the .cmd program must resolve inside the plugin root: {:?}",
        spec.program
    );
    assert_eq!(spec.args, vec!["--flag".to_string(), "value".to_string()]);
}
