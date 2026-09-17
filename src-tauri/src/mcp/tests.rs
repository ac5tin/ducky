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
use super::manager::{AuthReason, McpManager, ServerStatus};
use crate::config::{HttpAuth, McpServerConfig, McpTransport, Store};
use crate::events::{BackendEvent, CollectingSink};

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
    let manager = Arc::new(McpManager::new(store, bridge, sink));

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
    let manager = McpManager::new(store, bridge, sink);

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
