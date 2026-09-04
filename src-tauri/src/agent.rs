//! The chat engine: streams a provider turn, executes approved MCP tool
//! calls, feeds results back to the model and repeats until the model is
//! done. Everything streams to the webview as `BackendEvent`s.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::config::Store;
use crate::events::{BackendEvent, EventSink};
use crate::mcp::bridge::{ApprovalDecision, SamplingBackend};
use crate::mcp::manager::{content_to_text, McpManager};
use crate::providers::{
    build_provider, LlmProvider, Msg, ProviderEvent, StopReason, ToolCall, ToolDef,
};

pub struct Agent {
    pub store: Arc<Store>,
    pub manager: Arc<McpManager>,
    pub bridge: Arc<crate::mcp::bridge::InteractiveBridge>,
    pub sink: Arc<dyn EventSink>,
}

/// Runtime state for a conversation that is currently generating.
pub struct ConversationRuntime {
    pub ct: CancellationToken,
}

impl Agent {
    fn emit_tool_update(
        &self,
        conversation_id: &str,
        tool_call_id: &str,
        status: &str,
        patch: serde_json::Value,
    ) {
        let mut event = serde_json::json!({
            "type": "tool_call_update",
            "conversation_id": conversation_id,
            "tool_call_id": tool_call_id,
            "status": status,
        });
        if let (Some(base), Some(extra)) = (event.as_object_mut(), patch.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        if let Ok(parsed) = serde_json::from_value::<BackendEvent>(event) {
            self.sink.emit(parsed);
        }
    }

    fn provider_for(
        &self,
        provider_id: &str,
    ) -> Result<(Arc<dyn LlmProvider>, String, String), String> {
        let cfg = self.store.config.lock().unwrap().clone();
        let provider_cfg = cfg
            .providers
            .iter()
            .find(|p| p.id == provider_id)
            .ok_or("The provider for this conversation is no longer configured")?;
        let key = self.store.provider_key(&provider_cfg.id);
        let model = provider_cfg
            .default_model
            .clone()
            .filter(|m| !m.is_empty())
            .or_else(|| provider_cfg.models.first().cloned())
            .unwrap_or_default();
        Ok((
            build_provider(provider_cfg, key.as_deref()),
            model,
            provider_cfg.name.clone(),
        ))
    }

    /// Resolve a qualified tool name to its server + raw tool entry. Builtins
    /// are checked first so a user server can't shadow or spoof them.
    fn resolve_tool(
        &self,
        qualified: &str,
    ) -> Result<(crate::mcp::manager::ToolEntry, String), String> {
        if let Some(tool) = crate::builtin::lookup(qualified) {
            return Ok((
                crate::mcp::manager::ToolEntry {
                    server_id: crate::builtin::SERVER_ID.to_string(),
                    name: qualified.to_string(),
                    qualified_name: qualified.to_string(),
                    title: None,
                    description: Some(tool.description.to_string()),
                    input_schema: tool.schema.clone(),
                    output_schema: None,
                    annotations: None,
                    read_only_hint: Some(tool.read_only),
                    icons: None,
                },
                crate::builtin::SERVER_TITLE.to_string(),
            ));
        }
        let tools = self.manager.aggregated_tools();
        tools
            .into_iter()
            .find(|t| t.qualified_name == qualified)
            .map(|t| {
                let server_title = self.server_title(&t.server_id);
                (t, server_title)
            })
            .ok_or_else(|| format!("Unknown tool: {qualified}"))
    }

    fn server_title(&self, server_id: &str) -> String {
        let cfg = self.store.config.lock().unwrap();
        cfg.mcp_servers
            .iter()
            .find(|s| s.id == server_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| server_id.to_string())
    }

    /// Run one user turn to completion. Errors stream as `chat_error`.
    pub async fn run_turn(
        &self,
        conversation_id: String,
        provider_id: String,
        model: String,
        mut history: Vec<Msg>,
        user_text: String,
        ct: CancellationToken,
    ) {
        let result = self
            .run_turn_inner(
                &conversation_id,
                &provider_id,
                &model,
                &mut history,
                user_text,
                &ct,
            )
            .await;

        // persist the final history regardless of outcome
        if let Err(e) = self.persist(&conversation_id, &history) {
            tracing::warn!("failed to persist conversation: {e}");
        }

        match result {
            Ok(()) => {
                let message_id = Store::new_id();
                self.sink.emit(BackendEvent::MessageDone {
                    conversation_id,
                    message_id,
                });
            }
            Err(e) => {
                if ct.is_cancelled() {
                    self.sink.emit(BackendEvent::MessageDone {
                        conversation_id,
                        message_id: Store::new_id(),
                    });
                } else {
                    self.sink.emit(BackendEvent::ChatError {
                        conversation_id,
                        error: e,
                    });
                }
            }
        }
    }

    async fn run_turn_inner(
        &self,
        conversation_id: &str,
        provider_id: &str,
        model: &str,
        history: &mut Vec<Msg>,
        user_text: String,
        ct: &CancellationToken,
    ) -> Result<(), String> {
        history.push(Msg::User { text: user_text });

        let max_iterations = self
            .store
            .config
            .lock()
            .unwrap()
            .settings
            .max_tool_iterations;
        // attribute interactive requests (approvals, elicitations, sampling)
        // to this conversation for the duration of the turn
        self.bridge
            .set_conversation_ctx(Some(conversation_id.to_string()));

        let outcome = self
            .loop_turn(
                conversation_id,
                provider_id,
                model,
                history,
                max_iterations,
                ct,
            )
            .await;

        self.bridge.set_conversation_ctx(None);
        self.bridge.set_sampling_backend(None);
        outcome
    }

    async fn loop_turn(
        &self,
        conversation_id: &str,
        provider_id: &str,
        model: &str,
        history: &mut Vec<Msg>,
        max_iterations: u32,
        ct: &CancellationToken,
    ) -> Result<(), String> {
        for _ in 0..max_iterations {
            let (provider, default_model, _provider_name) = self.provider_for(provider_id)?;
            let model = if model.is_empty() {
                default_model
            } else {
                model.to_string()
            };

            // Give the interactive bridge a way to fulfil sampling requests
            // with the model the user is actually talking to.
            self.bridge.set_sampling_backend(Some(SamplingBackend {
                provider: provider.clone(),
                model: model.clone(),
            }));

            // Collect tools from every connected server, plus the builtins.
            let tool_entries = self.manager.aggregated_tools();
            let mut tools: Vec<ToolDef> = tool_entries
                .iter()
                .map(|t| ToolDef {
                    name: t.qualified_name.clone(),
                    description: t.description.clone().unwrap_or_default(),
                    parameters: t.input_schema.clone(),
                })
                .collect();
            tools.extend(crate::builtin::tool_defs());

            // Stream one assistant turn. The provider sees a snapshot of the
            // history plus a system message with the current working
            // directory (injected per turn, never persisted) so we can mutate
            // it freely while events stream in.
            let cwd = {
                let cfg = self.store.config.lock().unwrap();
                cfg.settings.effective_working_dir(&self.store.home_dir)
            };
            let mut snapshot = history.clone();
            snapshot.insert(
                0,
                Msg::System {
                    text: format!(
                        "Working directory: {}. Resolve relative file paths the user mentions \
                         against this directory. Built-in file tools are confined to the \
                         working directory. Base every claim about the file system on actual \
                         tool output — never invent, simulate, or guess tool results. If no \
                         available tool can answer a question, say so plainly instead of making \
                         up an answer.",
                        cwd.display()
                    ),
                },
            );
            let effort = {
                let cfg = self.store.config.lock().unwrap();
                cfg.conversations
                    .iter()
                    .find(|c| c.id == conversation_id)
                    .and_then(|c| c.effort)
            };
            let options = crate::providers::ChatOptions {
                model: model.clone(),
                max_tokens: None,
                temperature: None,
                effort,
            };
            let (tx, mut rx) = tokio::sync::mpsc::channel::<ProviderEvent>(256);
            let provider_call = provider.stream_chat(&snapshot, &tools, &options, tx);

            let mut text = String::new();
            let mut tool_calls: BTreeMap<usize, (String, String, String)> = BTreeMap::new();
            let mut stop: Option<StopReason> = None;

            let mut call = std::pin::pin!(provider_call);
            loop {
                tokio::select! {
                    biased;
                    _ = ct.cancelled() => {
                        return Err("cancelled".into());
                    }
                    ev = rx.recv() => {
                        match ev {
                            Some(ProviderEvent::TextDelta(t)) => {
                                text.push_str(&t);
                                self.sink.emit(BackendEvent::ChatDelta {
                                    conversation_id: conversation_id.to_string(),
                                    text: t,
                                });
                            }
                            Some(ProviderEvent::ReasoningDelta(t)) => {
                                if self.store.config.lock().unwrap().settings.show_reasoning {
                                    self.sink.emit(BackendEvent::ReasoningDelta {
                                        conversation_id: conversation_id.to_string(),
                                        text: t,
                                    });
                                }
                            }
                            Some(ProviderEvent::ToolCallBegin { index, id, name }) => {
                                let entry = tool_calls
                                    .entry(index)
                                    .or_insert_with(|| (id.clone(), name.clone(), String::new()));
                                entry.0 = id;
                                entry.1 = name;
                            }
                            Some(ProviderEvent::ToolCallArgsDelta { index, fragment }) => {
                                tool_calls
                                    .entry(index)
                                    .or_insert_with(|| (Store::new_id(), String::new(), String::new()))
                                    .2
                                    .push_str(&fragment);
                            }
                            Some(ProviderEvent::Usage { .. }) => {}
                            None => break,
                        }
                    }
                    result = &mut call => {
                        stop = Some(result.map_err(|e| e.to_string())?);
                    }
                }
            }
            // drain remaining events
            while let Ok(ev) = rx.try_recv() {
                if let ProviderEvent::TextDelta(t) = ev {
                    text.push_str(&t);
                    self.sink.emit(BackendEvent::ChatDelta {
                        conversation_id: conversation_id.to_string(),
                        text: t,
                    });
                }
            }
            let stop = stop.ok_or("The provider stream ended unexpectedly")?;

            let calls: Vec<ToolCall> = tool_calls
                .into_iter()
                .map(|(index, (id, name, args_json))| {
                    let arguments: serde_json::Value = if args_json.trim().is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&args_json)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": args_json }))
                    };
                    let id = if id.is_empty() {
                        format!("call_{index}")
                    } else {
                        id
                    };
                    ToolCall {
                        id,
                        name,
                        arguments,
                    }
                })
                .collect();

            history.push(Msg::Assistant {
                text: text.clone(),
                tool_calls: calls.clone(),
            });
            self.persist(conversation_id, history)?;

            if stop != StopReason::ToolUse || calls.is_empty() {
                return Ok(());
            }

            // Execute each tool call in order.
            for call in calls {
                if ct.is_cancelled() {
                    return Err("cancelled".into());
                }
                let tool_result = self.execute_tool(conversation_id, &call, ct).await;
                history.push(Msg::ToolResult {
                    call_id: call.id.clone(),
                    text: tool_result,
                    is_error: false,
                });
                self.persist(conversation_id, history)?;
            }
        }
        Err(format!(
            "Stopped after {max_iterations} tool rounds — increase the limit in Settings if this task needs more."
        ))
    }

    /// Approve (if needed) and run a single tool call, streaming card updates.
    async fn execute_tool(
        &self,
        conversation_id: &str,
        call: &ToolCall,
        ct: &CancellationToken,
    ) -> String {
        let (entry, server_title) = match self.resolve_tool(&call.name) {
            Ok(v) => v,
            Err(e) => {
                self.emit_tool_update(
                    conversation_id,
                    &call.id,
                    "error",
                    serde_json::json!({ "tool": call.name, "result_text": e, "is_error": true }),
                );
                return format!("Error: {e}");
            }
        };

        self.emit_tool_update(
            conversation_id,
            &call.id,
            "pending_approval",
            serde_json::json!({
                "server": entry.server_id,
                "server_title": server_title,
                "tool": entry.qualified_name,
                "args": call.arguments,
            }),
        );

        // consent
        let decision = tokio::select! {
            _ = ct.cancelled() => ApprovalDecision::Deny,
            d = self.bridge.request_approval(
                &entry.server_id,
                &server_title,
                &entry.qualified_name,
                &call.arguments,
                entry.read_only_hint,
            ) => d,
        };
        match decision {
            ApprovalDecision::AlwaysAllow => {
                let mut cfg = self.store.config.lock().unwrap();
                cfg.settings.tool_rules.insert(
                    format!("{}/{}", entry.server_id, entry.qualified_name),
                    crate::config::ToolRule::Allow,
                );
                drop(cfg);
                let _ = self.store.save_config();
            }
            ApprovalDecision::Deny => {
                self.emit_tool_update(
                    conversation_id,
                    &call.id,
                    "denied",
                    serde_json::json!({ "result_text": "The user denied this tool call." }),
                );
                return "The user denied permission for this tool call. Do not retry it; \
                        continue without it or ask the user what to do instead."
                    .to_string();
            }
            ApprovalDecision::AllowOnce => {}
        }

        self.emit_tool_update(conversation_id, &call.id, "running", serde_json::json!({}));

        // Builtins run in-process — no server to connect. Result text goes
        // straight to the model and the tool card.
        if crate::builtin::is_builtin(&call.name) {
            let cwd = {
                let cfg = self.store.config.lock().unwrap();
                cfg.settings.effective_working_dir(&self.store.home_dir)
            };
            let result = tokio::select! {
                _ = ct.cancelled() => Err("cancelled by user".to_string()),
                r = crate::builtin::execute(&call.name, &call.arguments, &cwd) => r,
            };
            return match result {
                Ok(text) => {
                    self.emit_tool_update(
                        conversation_id,
                        &call.id,
                        "done",
                        serde_json::json!({ "result_text": text }),
                    );
                    text
                }
                Err(e) => {
                    self.emit_tool_update(
                        conversation_id,
                        &call.id,
                        "error",
                        serde_json::json!({ "result_text": e, "is_error": true }),
                    );
                    format!("Error: {e}")
                }
            };
        }

        // make sure the server is connected (auto-connect on demand)
        if self.manager.get(&entry.server_id).is_none() {
            let status = match self.manager.connect(&entry.server_id).await {
                Ok(s) => s,
                Err(e) => {
                    self.emit_tool_update(
                        conversation_id,
                        &call.id,
                        "error",
                        serde_json::json!({ "result_text": e, "is_error": true }),
                    );
                    return format!("Error: {e}");
                }
            };
            if !matches!(status, crate::mcp::manager::ServerStatus::Connected) {
                let message =
                    format!("The MCP server for this tool is not connected (status: {status:?}).");
                self.emit_tool_update(
                    conversation_id,
                    &call.id,
                    "error",
                    serde_json::json!({ "result_text": message, "is_error": true }),
                );
                return format!("Error: {message}");
            }
        }

        let result = tokio::select! {
            _ = ct.cancelled() => Err("cancelled by user".to_string()),
            r = self.manager.call_tool(
                &entry.server_id,
                &entry.name,
                call.arguments.clone(),
                Some(call.id.clone()),
            ) => r,
        };

        match result {
            Ok(result) => {
                let text = content_to_text(&result.content);
                let structured = result
                    .structured_content
                    .as_ref()
                    .map(|s| serde_json::to_value(s).unwrap_or_default());
                let is_error = result.is_error.unwrap_or(false);
                let content = serde_json::to_value(&result.content).unwrap_or_default();
                self.emit_tool_update(
                    conversation_id,
                    &call.id,
                    if is_error { "error" } else { "done" },
                    serde_json::json!({
                        "result_text": text,
                        "structured": structured,
                        "content": content,
                        "is_error": is_error,
                    }),
                );
                if is_error {
                    format!("The tool reported an error: {text}")
                } else {
                    text
                }
            }
            Err(e) => {
                self.emit_tool_update(
                    conversation_id,
                    &call.id,
                    "error",
                    serde_json::json!({ "result_text": e, "is_error": true }),
                );
                format!("Error: {e}")
            }
        }
    }

    fn persist(&self, conversation_id: &str, history: &[Msg]) -> Result<(), String> {
        let meta = {
            let mut cfg = self.store.config.lock().unwrap();
            let meta = cfg
                .conversations
                .iter_mut()
                .find(|c| c.id == conversation_id)
                .ok_or("conversation missing")?;
            meta.updated_at = chrono::Utc::now().to_rfc3339();
            if meta.title.is_empty() {
                if let Some(Msg::User { text }) =
                    history.iter().find(|m| matches!(m, Msg::User { .. }))
                {
                    meta.title = text.chars().take(48).collect();
                }
            }
            meta.clone()
        };
        let payload: Vec<serde_json::Value> = history.iter().map(|m| m.as_json()).collect();
        self.store
            .save_conversation(&meta, &payload)
            .map_err(|e| e.to_string())
    }
}
