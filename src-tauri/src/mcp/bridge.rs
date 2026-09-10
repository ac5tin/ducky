//! The interactive bridge: connects protocol-level client features
//! (elicitation, sampling) and tool approvals to the user via UI events and
//! one-shot response channels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CreateMessageRequestParams, CreateMessageResult, ElicitRequestParams, ElicitResult,
    ElicitationAction, Role, SamplingMessage, SamplingMessageContentBlock,
};
use rmcp::ErrorData as McpError;
use tokio::sync::oneshot;

use crate::config::{ApprovalMode, SamplingMode, Store};
use crate::events::{BackendEvent, EventSink};
use crate::providers::{build_provider, LlmProvider, Msg, ProviderEvent};

// ---------------------------------------------------------------------------
// Requests to the user
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AlwaysAllow,
    Deny,
}

/// Provider + model to use when fulfilling a sampling request.
#[derive(Clone)]
pub struct SamplingBackend {
    pub provider: Arc<dyn LlmProvider>,
    pub model: String,
}

type Pending<T> = Arc<Mutex<HashMap<String, oneshot::Sender<T>>>>;

pub struct InteractiveBridge {
    sink: Arc<dyn EventSink>,
    store: Arc<Store>,
    /// Conversation the current tool call belongs to (for UI attribution).
    conversation_ctx: Mutex<Option<String>>,
    /// Sampling backend for the active conversation (set by the agent).
    sampling_backend: Mutex<Option<SamplingBackend>>,
    approvals: Pending<ApprovalDecision>,
    elicitations: Pending<ElicitResult>,
    sampling_slots: Pending<bool>,
}

impl InteractiveBridge {
    pub fn new(sink: Arc<dyn EventSink>, store: Arc<Store>) -> Self {
        Self {
            sink,
            store,
            conversation_ctx: Mutex::new(None),
            sampling_backend: Mutex::new(None),
            approvals: Arc::new(Mutex::new(HashMap::new())),
            elicitations: Arc::new(Mutex::new(HashMap::new())),
            sampling_slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Attribute subsequent interactive requests to a conversation.
    pub fn set_conversation_ctx(&self, conversation_id: Option<String>) {
        *self.conversation_ctx.lock().unwrap() = conversation_id;
    }

    /// Set the provider used to fulfil sampling requests.
    pub fn set_sampling_backend(&self, backend: Option<SamplingBackend>) {
        *self.sampling_backend.lock().unwrap() = backend;
    }

    fn conversation_ctx_opt(&self) -> Option<String> {
        self.conversation_ctx.lock().unwrap().clone()
    }

    /// Resolve every pending interactive request opened for a conversation
    /// (used when the user cancels a chat turn).
    pub fn cancel_for_conversation(&self, conversation_id: &str) {
        let in_ctx = self.conversation_ctx_opt().as_deref() == Some(conversation_id);
        let mut approvals = self.approvals.lock().unwrap();
        let keys: Vec<String> = approvals.keys().cloned().collect();
        // approvals do not carry conversation ids in the map; the agent cancels
        // its own context so cancel everything attributed to the live ctx
        if in_ctx {
            for k in keys {
                if let Some(tx) = approvals.remove(&k) {
                    let _ = tx.send(ApprovalDecision::Deny);
                }
            }
        }
        drop(approvals);
        let mut el = self.elicitations.lock().unwrap();
        for (_, tx) in el.drain() {
            let _ = tx.send(ElicitResult::new(ElicitationAction::Cancel));
        }
        drop(el);
        let mut s = self.sampling_slots.lock().unwrap();
        for (_, tx) in s.drain() {
            let _ = tx.send(false);
        }
    }

    // -- approvals ---------------------------------------------------------

    pub async fn request_approval(
        &self,
        server: &str,
        server_title: &str,
        tool: &str,
        args: &serde_json::Value,
        read_only_hint: Option<bool>,
    ) -> ApprovalDecision {
        // policy shortcuts
        let cfg = self.store.config.lock().unwrap().clone();
        let rule_key = format!("{server}/{tool}");
        if let Some(rule) = cfg.settings.tool_rules.get(&rule_key) {
            return match rule {
                crate::config::ToolRule::Allow => ApprovalDecision::AllowOnce,
                crate::config::ToolRule::Deny => ApprovalDecision::Deny,
            };
        }
        match cfg.settings.tool_approval {
            ApprovalMode::AutoApproveAll => return ApprovalDecision::AllowOnce,
            ApprovalMode::AutoApproveReadOnly if read_only_hint == Some(true) => {
                return ApprovalDecision::AllowOnce
            }
            _ => {}
        }

        let request_id = crate::config::Store::new_id();
        let (tx, rx) = oneshot::channel();
        self.approvals
            .lock()
            .unwrap()
            .insert(request_id.clone(), tx);
        self.sink.emit(BackendEvent::ApprovalRequested {
            request_id: request_id.clone(),
            conversation_id: self.conversation_ctx_opt(),
            server: server.to_string(),
            server_title: server_title.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
            read_only_hint,
        });
        match rx.await {
            Ok(decision) => decision,
            Err(_) => ApprovalDecision::Deny,
        }
    }

    /// Called from a Tauri command when the user answers an approval dialog.
    pub fn resolve_approval(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        if let Some(tx) = self.approvals.lock().unwrap().remove(request_id) {
            let _ = tx.send(decision);
            true
        } else {
            false
        }
    }

    // -- elicitation ---------------------------------------------------------

    pub async fn run_elicitation(
        &self,
        server: &str,
        server_title: &str,
        request: ElicitRequestParams,
    ) -> Result<ElicitResult, McpError> {
        let request_id = crate::config::Store::new_id();
        let (tx, rx) = oneshot::channel();
        self.elicitations
            .lock()
            .unwrap()
            .insert(request_id.clone(), tx);

        let (mode, message, schema, url) = match &request {
            ElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => {
                let schema =
                    serde_json::to_value(requested_schema).unwrap_or(serde_json::json!({}));
                ("form", message.clone(), Some(schema), None)
            }
            ElicitRequestParams::UrlElicitationParams { message, url, .. } => {
                ("url", message.clone(), None, Some(url.clone()))
            }
            other => {
                return Err(McpError::invalid_request(
                    format!("unsupported elicitation request: {other:?}"),
                    None,
                ))
            }
        };

        self.sink.emit(BackendEvent::ElicitationRequested {
            request_id: request_id.clone(),
            conversation_id: self.conversation_ctx_opt(),
            server: server.to_string(),
            server_title: server_title.to_string(),
            mode: mode.to_string(),
            message,
            schema,
            url,
        });

        Ok(match rx.await {
            Ok(result) => result,
            Err(_) => ElicitResult::new(ElicitationAction::Cancel),
        })
    }

    pub fn resolve_elicitation(&self, request_id: &str, result: ElicitResult) -> bool {
        if let Some(tx) = self.elicitations.lock().unwrap().remove(request_id) {
            let _ = tx.send(result);
            true
        } else {
            false
        }
    }

    // -- sampling ------------------------------------------------------------

    pub async fn run_sampling(
        &self,
        server: &str,
        server_title: &str,
        params: &CreateMessageRequestParams,
    ) -> Result<CreateMessageResult, McpError> {
        // consent (read the policy out of the lock before any await)
        let mode = self.store.config.lock().unwrap().settings.sampling;
        let approved = match mode {
            SamplingMode::AutoApprove => true,
            SamplingMode::Deny => false,
            SamplingMode::Ask => {
                let request_id = crate::config::Store::new_id();
                let (tx, rx) = oneshot::channel();
                self.sampling_slots
                    .lock()
                    .unwrap()
                    .insert(request_id.clone(), tx);

                let messages: Vec<serde_json::Value> = params
                    .messages
                    .iter()
                    .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
                    .collect();
                self.sink.emit(BackendEvent::SamplingRequested {
                    request_id: request_id.clone(),
                    conversation_id: self.conversation_ctx_opt(),
                    server: server.to_string(),
                    server_title: server_title.to_string(),
                    system_prompt: params.system_prompt.clone(),
                    messages,
                    max_tokens: Some(params.max_tokens),
                });
                matches!(rx.await, Ok(true))
            }
        };
        if !approved {
            return Err(McpError::invalid_request(
                "The user declined this sampling request.",
                None,
            ));
        }

        // resolve backend: active conversation's provider, else first provider
        let backend = { self.sampling_backend.lock().unwrap().clone() };
        let backend = backend.or_else(|| self.default_backend());
        let Some(backend) = backend else {
            return Err(McpError::internal_error(
                "No AI provider is configured, so Ducky cannot fulfil this sampling request.",
                None,
            ));
        };

        // convert sampling messages into the unified chat model
        let mut msgs: Vec<Msg> = Vec::new();
        if let Some(sys) = &params.system_prompt {
            if !sys.is_empty() {
                msgs.push(Msg::System { text: sys.clone() });
            }
        }
        for m in &params.messages {
            let text = sampling_message_text(m);
            match m.role {
                Role::User => msgs.push(Msg::User { text, ts: None }),
                Role::Assistant => msgs.push(Msg::Assistant {
                    text,
                    tool_calls: Vec::new(),
                    ts: None,
                }),
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        let opts = crate::providers::ChatOptions {
            model: backend.model.clone(),
            max_tokens: Some(params.max_tokens),
            temperature: None,
            effort: None,
        };
        let provider = backend.provider.clone();
        let handle = tokio::spawn(async move { provider.stream_chat(&msgs, &[], &opts, tx).await });
        let mut text = String::new();
        while let Some(ev) = rx.recv().await {
            if let ProviderEvent::TextDelta(t) = ev {
                text.push_str(&t);
            }
        }
        let stop = handle
            .await
            .map_err(|e| McpError::internal_error(format!("sampling task failed: {e}"), None))?
            .map_err(|e| McpError::internal_error(format!("sampling failed: {e}"), None))?;

        let result_msg = SamplingMessage::assistant_text(text);
        let mut out = CreateMessageResult::new(result_msg, backend.model.clone());
        out.stop_reason = Some(
            match stop {
                crate::providers::StopReason::EndTurn | crate::providers::StopReason::ToolUse => {
                    CreateMessageResult::STOP_REASON_END_TURN
                }
                crate::providers::StopReason::Length => {
                    CreateMessageResult::STOP_REASON_END_MAX_TOKEN
                }
            }
            .to_string(),
        );
        Ok(out)
    }

    fn default_backend(&self) -> Option<SamplingBackend> {
        let cfg = self.store.config.lock().unwrap().clone();
        let provider_cfg = cfg.providers.first()?;
        let key = self.store.provider_key(&provider_cfg.id);
        let model = provider_cfg
            .default_model
            .clone()
            .or_else(|| provider_cfg.models.first().cloned())
            .unwrap_or_default();
        Some(SamplingBackend {
            provider: build_provider(provider_cfg, key.as_deref()),
            model,
        })
    }

    // -- sampling responses (from UI) ---------------------------------------

    pub fn resolve_sampling(&self, request_id: &str, approved: bool) -> bool {
        if let Some(tx) = self.sampling_slots.lock().unwrap().remove(request_id) {
            let _ = tx.send(approved);
            true
        } else {
            false
        }
    }
}

/// Extract a plain-text prompt from a sampling message.
fn sampling_message_text(m: &SamplingMessage) -> String {
    let mut parts = Vec::new();
    for block in m.content.clone().into_vec() {
        let rendered: Option<String> = match &block {
            SamplingMessageContentBlock::Text(t) => Some(t.text.clone()),
            SamplingMessageContentBlock::Image(_) => Some("[image]".to_string()),
            SamplingMessageContentBlock::Audio(_) => Some("[audio]".to_string()),
            SamplingMessageContentBlock::ToolUse(tu) => Some(format!("[tool call: {}]", tu.name)),
            SamplingMessageContentBlock::ToolResult(tr) => {
                // best-effort: pull text out of the result content
                let content = serde_json::to_value(&tr.content).unwrap_or_default();
                Some(extract_text_from_json(&content).unwrap_or_else(|| "[tool result]".into()))
            }
            _ => None,
        };
        if let Some(p) = rendered {
            parts.push(p);
        }
    }
    parts.join("\n")
}

fn extract_text_from_json(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Array(arr) => {
            let texts: Vec<String> = arr.iter().filter_map(extract_text_from_json).collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        serde_json::Value::Object(obj) => {
            if obj.get("type").and_then(|t| t.as_str()) == Some("text") {
                obj.get("text").and_then(|t| t.as_str()).map(String::from)
            } else {
                obj.values().find_map(extract_text_from_json)
            }
        }
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::CollectingSink;

    fn bridge() -> Arc<InteractiveBridge> {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(tmp.path(), tmp.path().to_path_buf()).unwrap());
        // keep the tempdir alive by leaking in test scope
        std::mem::forget(tmp);
        Arc::new(InteractiveBridge::new(
            Arc::new(CollectingSink::default()),
            store,
        ))
    }

    #[tokio::test]
    async fn approval_roundtrip() {
        let sink = Arc::new(CollectingSink::default());
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(tmp.path(), tmp.path().to_path_buf()).unwrap());
        std::mem::forget(tmp);
        let b = Arc::new(InteractiveBridge::new(sink.clone(), store));

        let b2 = b.clone();
        let handle = tokio::spawn(async move {
            b2.request_approval(
                "srv",
                "Server",
                "read_file",
                &serde_json::json!({"p": 1}),
                None,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let events = sink.events.lock().unwrap();
        let request_id = events
            .iter()
            .find_map(|e| match e {
                BackendEvent::ApprovalRequested { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
            .expect("approval request was emitted");
        drop(events);

        assert!(b.resolve_approval(&request_id, ApprovalDecision::AllowOnce));
        assert_eq!(handle.await.unwrap(), ApprovalDecision::AllowOnce);
        // second resolve is a no-op
        assert!(!b.resolve_approval(&request_id, ApprovalDecision::Deny));
    }

    #[test]
    fn sampling_message_text_extracts() {
        let msg = SamplingMessage::new_multiple(
            Role::User,
            vec![
                SamplingMessageContentBlock::text("hello"),
                SamplingMessageContentBlock::text("world"),
            ],
        );
        assert_eq!(sampling_message_text(&msg), "hello\nworld");
    }
}
