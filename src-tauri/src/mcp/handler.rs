//! Ducky's MCP client handler: declares capabilities and routes server
//! requests (elicitation, sampling, roots) through the interactive bridge.

#![allow(deprecated)] // SEP-2577; rmcp 3.1.4 still exposes these compatibility APIs.

use std::sync::Arc;

use rmcp::handler::client::ClientHandler;
use rmcp::model::{
    ClientCapabilities, ClientInfo, CreateMessageRequestParams, CreateMessageResult,
    ElicitRequestParams, ElicitResult, ElicitationCapability, ExtensionCapabilities,
    FormElicitationCapability, Implementation, ListRootsResult, LoggingMessageNotificationParam,
    NumberOrString, ProgressNotificationParam, ProgressToken, ResourceUpdatedNotificationParam,
    Root, RootsCapabilities, SamplingCapability, UrlElicitationCapability,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient};
use rmcp::ErrorData as McpError;

use super::bridge::InteractiveBridge;
use crate::config::Store;
use crate::events::{BackendEvent, EventSink};

/// Everything a `DuckyClientHandler` needs. Cheap to clone.
#[derive(Clone)]
pub struct DuckyClientHandler {
    pub server_id: Arc<str>,
    pub server_name: Arc<str>,
    pub server_title: Arc<str>,
    pub bridge: Arc<InteractiveBridge>,
    pub store: Arc<Store>,
    pub sink: Arc<dyn EventSink>,
}

impl ClientHandler for DuckyClientHandler {
    // The 2026-07-28 client: form + URL elicitation, sampling and roots kept
    // for backwards compatibility (all negotiated per request via
    // `_meta.io.modelcontextprotocol/clientCapabilities`).
    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        self.bridge
            .run_elicitation(&self.server_id, &self.server_title, request)
            .await
    }

    async fn create_message(
        &self,
        params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, McpError> {
        self.bridge
            .run_sampling(&self.server_id, &self.server_title, &params)
            .await
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, McpError> {
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
        Ok(ListRootsResult::new(list))
    }

    async fn on_progress(
        &self,
        notification: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        // progress tokens are tool-call ids we issued
        let token = match &notification.progress_token {
            ProgressToken(NumberOrString::Number(n)) => n.to_string(),
            ProgressToken(NumberOrString::String(s)) => s.to_string(),
        };
        self.sink.emit(BackendEvent::Progress {
            conversation_id: None,
            tool_call_id: Some(token),
            progress: notification.progress,
            total: notification.total,
            message: notification.message.clone(),
        });
    }

    async fn on_logging_message(
        &self,
        notification: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        // deprecated protocol logging — surface as a server log line
        self.emit_log(format!("[{:?}] {}", notification.level, notification.data));
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.sink.emit(BackendEvent::ServerDataChanged {
            server_id: self.server_id.to_string(),
            what: "tools".into(),
        });
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.sink.emit(BackendEvent::ServerDataChanged {
            server_id: self.server_id.to_string(),
            what: "prompts".into(),
        });
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.sink.emit(BackendEvent::ServerDataChanged {
            server_id: self.server_id.to_string(),
            what: "resources".into(),
        });
    }

    async fn on_resource_updated(
        &self,
        notification: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.sink.emit(BackendEvent::ResourceUpdated {
            server_id: self.server_id.to_string(),
            uri: notification.uri,
        });
    }

    fn get_info(&self) -> ClientInfo {
        let mut capabilities = ClientCapabilities::default();
        capabilities.roots = Some(RootsCapabilities::default());
        capabilities.sampling = Some(SamplingCapability::default());
        let mut elicitation = ElicitationCapability::default();
        elicitation.form = Some(FormElicitationCapability::default());
        elicitation.url = Some(UrlElicitationCapability::default());
        capabilities.elicitation = Some(elicitation);

        // Opt in to the official tasks extension (SEP-2663): tools may return
        // task handles that Ducky polls via tasks/get.
        let mut extensions = ExtensionCapabilities::new();
        if let Ok(tasks) = serde_json::from_value(serde_json::json!({})) {
            extensions.insert("io.modelcontextprotocol/tasks".to_string(), tasks);
        }
        capabilities.extensions = Some(extensions);

        ClientInfo::new(
            capabilities,
            Implementation::new(
                "Ducky",
                concat!(env!("CARGO_PKG_VERSION"), " — friendly MCP client"),
            ),
        )
    }
}

impl DuckyClientHandler {
    fn emit_log(&self, line: String) {
        self.sink.emit(BackendEvent::ServerDataChanged {
            server_id: self.server_id.to_string(),
            what: format!("log:{line}"),
        });
    }
}
