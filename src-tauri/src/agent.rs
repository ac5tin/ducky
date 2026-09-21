//! The chat engine: streams a provider turn, executes approved MCP tool
//! calls, feeds results back to the model and repeats until the model is
//! done. Everything streams to the webview as `BackendEvent`s.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::config::Store;
use crate::events::{BackendEvent, EventSink};
use crate::mcp::bridge::{ApprovalDecision, SamplingBackend};
use crate::mcp::manager::{content_to_text, McpManager};
use crate::providers::{
    build_provider, LlmProvider, Msg, ProviderEvent, StopReason, ToolCall, ToolDef,
};

/// Maximum subagent nesting depth. The main agent runs at depth 0; a
/// subagent spawned from depth `n` runs at depth `n + 1` and may not spawn
/// once its own depth reaches this limit.
const MAX_SUBAGENT_DEPTH: u32 = 3;
/// Maximum subagents running concurrently within one assistant message.
/// Extra `ducky__subagent` calls in the same message run sequentially.
const MAX_CONCURRENT_SUBAGENTS: usize = 4;

/// Which kind of run is executing the loop. Subagent runs share the tool
/// pipeline and approvals but stream into their parent's tool card instead
/// of the conversation, and never touch the conversation file.
#[derive(Clone)]
enum RunScope {
    Main,
    Subagent { tool_call_id: String, depth: u32 },
}

impl RunScope {
    fn parent_tool_call_id(&self) -> Option<&str> {
        match self {
            RunScope::Main => None,
            RunScope::Subagent { tool_call_id, .. } => Some(tool_call_id),
        }
    }

    fn depth(&self) -> u32 {
        match self {
            RunScope::Main => 0,
            RunScope::Subagent { depth, .. } => *depth,
        }
    }

    fn is_main(&self) -> bool {
        matches!(self, RunScope::Main)
    }
}

pub struct Agent {
    pub store: Arc<Store>,
    pub manager: Arc<McpManager>,
    pub bridge: Arc<crate::mcp::bridge::InteractiveBridge>,
    pub sink: Arc<dyn EventSink>,
}

/// Per-round system message (never persisted). Rebuilt each round so
/// working-directory changes apply mid-turn. Subagent runs get a preamble
/// describing their contract: isolated context, autonomous, final answer.
fn system_message(cwd: &std::path::Path, scope: &RunScope) -> Msg {
    let grounding = format!(
        "Working directory: {}. Resolve relative file paths the user mentions \
         against this directory. Built-in file tools are confined to the \
         working directory. Base every claim about the file system on actual \
         tool output — never invent, simulate, or guess tool results. If no \
         available tool can answer a question, say so plainly instead of making \
         up an answer.",
        cwd.display()
    );
    let text = match scope {
        RunScope::Main => grounding,
        RunScope::Subagent { .. } => format!(
            "You are a subagent: an autonomous helper spawned by another agent to \
             complete one specific task. The task description is your entire context — \
             you cannot see the conversation that spawned you and cannot ask it \
             questions, so make reasonable autonomous decisions instead. Work with the \
             available tools and finish with a complete, self-contained final answer; \
             the agent that spawned you only sees that final answer.\n\n{grounding}"
        ),
    };
    Msg::System { text }
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
        parent_tool_call_id: Option<&str>,
    ) {
        let mut event = serde_json::json!({
            "type": "tool_call_update",
            "conversation_id": conversation_id,
            "tool_call_id": tool_call_id,
            "status": status,
        });
        if let Some(parent) = parent_tool_call_id {
            event["parent_tool_call_id"] = serde_json::json!(parent);
        }
        if let (Some(base), Some(extra)) = (event.as_object_mut(), patch.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        if let Ok(parsed) = serde_json::from_value::<BackendEvent>(event) {
            self.sink.emit(parsed);
        }
    }

    /// Stream one text chunk to wherever this run's output goes: the
    /// conversation for main runs, the parent tool card for subagents.
    fn emit_text_delta(&self, conversation_id: &str, scope: &RunScope, text: &str) {
        match scope {
            RunScope::Main => self.sink.emit(BackendEvent::ChatDelta {
                conversation_id: conversation_id.to_string(),
                text: text.to_string(),
            }),
            RunScope::Subagent { tool_call_id, .. } => {
                self.sink.emit(BackendEvent::SubagentDelta {
                    conversation_id: conversation_id.to_string(),
                    tool_call_id: tool_call_id.clone(),
                    text: text.to_string(),
                })
            }
        }
    }

    pub(crate) fn provider_for(
        &self,
        provider_id: &str,
    ) -> Result<(Arc<dyn LlmProvider>, String, String), String> {
        #[cfg(test)]
        {
            // scripted provider injected by the agent tests
            if let Some(p) = crate::agent_tests::provider_override() {
                return Ok((p, String::new(), "Mock".to_string()));
            }
        }
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
        history.push(Msg::User {
            text: user_text,
            ts: Some(chrono::Utc::now().to_rfc3339()),
        });

        let max_iterations = self
            .store
            .config
            .lock()
            .unwrap()
            .settings
            .max_tool_iterations;
        // attribute interactive requests (approvals, elicitations, sampling)
        // to this conversation for the duration of the turn; subagent runs
        // acquire the same slot and release it via refcount
        let _ctx = self.bridge.acquire_conversation_ctx(conversation_id);

        self.agent_loop(
            conversation_id,
            provider_id,
            model,
            history,
            max_iterations,
            ct,
            &RunScope::Main,
        )
        .await
        .map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    async fn agent_loop(
        &self,
        conversation_id: &str,
        provider_id: &str,
        model: &str,
        history: &mut Vec<Msg>,
        max_iterations: u32,
        ct: &CancellationToken,
        scope: &RunScope,
    ) -> Result<Option<String>, String> {
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

            // Collect tools from connected servers this chat allows, plus the builtins.
            let allow = {
                let cfg = self.store.config.lock().unwrap();
                cfg.conversations
                    .iter()
                    .find(|c| c.id == conversation_id)
                    .cloned()
            };
            let tool_entries: Vec<_> = self
                .manager
                .aggregated_tools()
                .into_iter()
                .filter(|t| {
                    allow
                        .as_ref()
                        .map(|m| m.allows_mcp(&t.server_id))
                        .unwrap_or(true)
                })
                .collect();
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
            snapshot.insert(0, system_message(&cwd, scope));
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
                                self.emit_text_delta(conversation_id, scope, &t);
                            }
                            Some(ProviderEvent::ReasoningDelta(t)) => {
                                if scope.is_main()
                                    && self.store.config.lock().unwrap().settings.show_reasoning
                                {
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
                            Some(ProviderEvent::Usage { input, output }) => {
                                self.sink.emit(BackendEvent::Usage {
                                    conversation_id: conversation_id.to_string(),
                                    input,
                                    output,
                                });
                            }
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
                match ev {
                    ProviderEvent::TextDelta(t) => {
                        text.push_str(&t);
                        self.emit_text_delta(conversation_id, scope, &t);
                    }
                    ProviderEvent::Usage { input, output } => {
                        self.sink.emit(BackendEvent::Usage {
                            conversation_id: conversation_id.to_string(),
                            input,
                            output,
                        });
                    }
                    _ => {}
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
                ts: Some(chrono::Utc::now().to_rfc3339()),
            });
            if scope.is_main() {
                self.persist(conversation_id, history)?;
            }

            if stop != StopReason::ToolUse || calls.is_empty() {
                return Ok(Some(text));
            }

            // Subagent calls are approved up front and spawned as tasks so
            // several can run concurrently; other tools — and subagent calls
            // past the concurrency cap — execute sequentially. Either way the
            // results are appended to the history in call order.
            let mut spawned: HashMap<String, tokio::task::JoinHandle<String>> = HashMap::new();
            for call in &calls {
                if spawned.len() >= MAX_CONCURRENT_SUBAGENTS {
                    break;
                }
                if call.name != crate::builtin::SUBAGENT {
                    continue;
                }
                let Ok(task) = self
                    .prepare_subagent(conversation_id, call, scope, ct)
                    .await
                else {
                    continue; // denied or invalid — settled in the ordered pass
                };
                let handle =
                    self.spawn_subagent_task(conversation_id, task, call.id.clone(), scope.depth(), ct);
                spawned.insert(call.id.clone(), handle);
            }

            // Execute each tool call in order.
            for call in calls {
                if ct.is_cancelled() {
                    return Err("cancelled".into());
                }
                let tool_result = if let Some(handle) = spawned.remove(&call.id) {
                    self.join_subagent(
                        conversation_id,
                        &call.id,
                        handle.await,
                        scope.parent_tool_call_id(),
                    )
                } else if call.name == crate::builtin::SUBAGENT {
                    // past the concurrency cap: gate, spawn, wait inline
                    match self.prepare_subagent(conversation_id, &call, scope, ct).await {
                        Ok(task) => {
                            let handle = self.spawn_subagent_task(
                                conversation_id,
                                task,
                                call.id.clone(),
                                scope.depth(),
                                ct,
                            );
                            self.join_subagent(
                                conversation_id,
                                &call.id,
                                handle.await,
                                scope.parent_tool_call_id(),
                            )
                        }
                        Err(text) => text,
                    }
                } else {
                    self.execute_tool(conversation_id, &call, scope, ct).await
                };
                history.push(Msg::ToolResult {
                    call_id: call.id.clone(),
                    text: tool_result,
                    is_error: false,
                });
                if scope.is_main() {
                    self.persist(conversation_id, history)?;
                }
            }
        }
        Err(format!(
            "Stopped after {max_iterations} tool rounds — increase the limit in Settings if this task needs more."
        ))
    }

    /// Consent gate shared by every tool execution. `Ok(())` = allowed;
    /// `Err(text)` is the tool-result text for a denial (card updated).
    #[allow(clippy::too_many_arguments)]
    async fn gate_approval(
        &self,
        conversation_id: &str,
        tool_call_id: &str,
        entry: &crate::mcp::manager::ToolEntry,
        server_title: &str,
        args: &serde_json::Value,
        parent: Option<&str>,
        ct: &CancellationToken,
    ) -> Result<(), String> {
        let decision = tokio::select! {
            _ = ct.cancelled() => ApprovalDecision::Deny,
            d = self.bridge.request_approval(
                &entry.server_id,
                server_title,
                &entry.qualified_name,
                args,
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
                Ok(())
            }
            ApprovalDecision::Deny => {
                self.emit_tool_update(
                    conversation_id,
                    tool_call_id,
                    "denied",
                    serde_json::json!({ "result_text": "The user denied this tool call." }),
                    parent,
                );
                Err("The user denied permission for this tool call. Do not retry it; \
                        continue without it or ask the user what to do instead."
                    .to_string())
            }
            ApprovalDecision::AllowOnce => Ok(()),
        }
    }

    /// Approval gate + card updates for one `ducky__subagent` call, done
    /// before the call is spawned so parallel spawns still ask in order.
    /// `Ok(task)` = approved and may run; `Err(text)` = settled (denied,
    /// invalid arguments or past the nesting limit), with the card updated.
    async fn prepare_subagent(
        &self,
        conversation_id: &str,
        call: &ToolCall,
        scope: &RunScope,
        ct: &CancellationToken,
    ) -> Result<String, String> {
        let parent = scope.parent_tool_call_id();
        let (entry, server_title) = match self.resolve_tool(&call.name) {
            Ok(v) => v,
            Err(e) => {
                self.emit_tool_update(
                    conversation_id,
                    &call.id,
                    "error",
                    serde_json::json!({ "tool": call.name, "result_text": e, "is_error": true }),
                    parent,
                );
                return Err(format!("Error: {e}"));
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
            parent,
        );

        // consent; a denial already carries the settled tool-result text
        self.gate_approval(
            conversation_id,
            &call.id,
            &entry,
            &server_title,
            &call.arguments,
            parent,
            ct,
        )
        .await?;

        if scope.depth() >= MAX_SUBAGENT_DEPTH {
            let msg = format!(
                "subagent nesting limit reached ({} levels). Handle this task yourself.",
                MAX_SUBAGENT_DEPTH
            );
            self.emit_tool_update(
                conversation_id,
                &call.id,
                "error",
                serde_json::json!({ "result_text": msg, "is_error": true }),
                parent,
            );
            return Err(format!("Error: {msg}"));
        }

        let task = call
            .arguments
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let Some(task) = task else {
            let msg = "missing required `task` string argument".to_string();
            self.emit_tool_update(
                conversation_id,
                &call.id,
                "error",
                serde_json::json!({ "result_text": msg, "is_error": true }),
                parent,
            );
            return Err(format!("Error: {msg}"));
        };

        self.emit_tool_update(conversation_id, &call.id, "running", serde_json::json!({}), parent);
        Ok(task)
    }

    /// Spawn one approved subagent as its own task. Every subagent run goes
    /// through a task boundary — this is also what lets `agent_loop` await
    /// subagent results without a recursive (and therefore unsized/!Send)
    /// future.
    fn spawn_subagent_task(
        &self,
        conversation_id: &str,
        task: String,
        call_id: String,
        depth: u32,
        ct: &CancellationToken,
    ) -> tokio::task::JoinHandle<String> {
        let agent = Agent {
            store: self.store.clone(),
            manager: self.manager.clone(),
            bridge: self.bridge.clone(),
            sink: self.sink.clone(),
        };
        let conversation = conversation_id.to_string();
        let ct = ct.clone();
        tokio::spawn(async move {
            agent.run_subagent(&conversation, task, &call_id, depth, &ct).await
        })
    }

    /// Collect a finished subagent task into its tool-result text, turning a
    /// panicked task into an error card like any other tool failure.
    fn join_subagent(
        &self,
        conversation_id: &str,
        call_id: &str,
        result: Result<String, tokio::task::JoinError>,
        parent: Option<&str>,
    ) -> String {
        match result {
            Ok(text) => text,
            Err(e) => {
                let msg = format!("subagent task failed: {e}");
                self.emit_tool_update(
                    conversation_id,
                    call_id,
                    "error",
                    serde_json::json!({ "result_text": msg, "is_error": true }),
                    parent,
                );
                format!("Error: {msg}")
            }
        }
    }

    /// Run one approved subagent to completion and return its result text.
    /// The subagent inherits the conversation's provider, model and effort
    /// (read from config each round, like the main loop), starts from a
    /// fresh context containing only its task, and never touches the
    /// conversation file — only its final answer flows back as the tool
    /// result. Cancelling `ct` cancels the subagent via a child token.
    async fn run_subagent(
        &self,
        conversation_id: &str,
        task: String,
        tool_call_id: &str,
        depth: u32,
        ct: &CancellationToken,
    ) -> String {
        // inherit the conversation's current provider + model at spawn time
        let (provider_id, model) = {
            let cfg = self.store.config.lock().unwrap();
            match cfg.conversations.iter().find(|c| c.id == conversation_id) {
                Some(meta) => (meta.provider_id.clone(), meta.model.clone()),
                None => (String::new(), String::new()),
            }
        };
        let max_iterations = self
            .store
            .config
            .lock()
            .unwrap()
            .settings
            .max_tool_iterations;
        let child = ct.child_token();
        let _ctx = self.bridge.acquire_conversation_ctx(conversation_id);
        let scope = RunScope::Subagent {
            tool_call_id: tool_call_id.to_string(),
            depth: depth + 1,
        };
        let mut history = vec![Msg::User {
            text: task,
            ts: Some(chrono::Utc::now().to_rfc3339()),
        }];
        let outcome = self
            .agent_loop(conversation_id, &provider_id, &model, &mut history, max_iterations, &child, &scope)
            .await;
        match outcome {
            Ok(Some(text)) if !text.trim().is_empty() => {
                self.emit_tool_update(
                    conversation_id,
                    tool_call_id,
                    "done",
                    serde_json::json!({ "result_text": text }),
                    None,
                );
                text
            }
            Ok(_) => {
                let text =
                    "(The subagent finished without producing a final answer.)".to_string();
                self.emit_tool_update(
                    conversation_id,
                    tool_call_id,
                    "done",
                    serde_json::json!({ "result_text": text }),
                    None,
                );
                text
            }
            Err(e) => {
                let text = if child.is_cancelled() {
                    "cancelled by user".to_string()
                } else {
                    e
                };
                self.emit_tool_update(
                    conversation_id,
                    tool_call_id,
                    "error",
                    serde_json::json!({ "result_text": text, "is_error": true }),
                    None,
                );
                format!("Error: {text}")
            }
        }
    }

    /// Approve (if needed) and run a single tool call, streaming card updates.
    async fn execute_tool(
        &self,
        conversation_id: &str,
        call: &ToolCall,
        scope: &RunScope,
        ct: &CancellationToken,
    ) -> String {
        let parent = scope.parent_tool_call_id();
        let (entry, server_title) = match self.resolve_tool(&call.name) {
            Ok(v) => v,
            Err(e) => {
                self.emit_tool_update(
                    conversation_id,
                    &call.id,
                    "error",
                    serde_json::json!({ "tool": call.name, "result_text": e, "is_error": true }),
                    parent,
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
            parent,
        );

        // consent
        #[allow(clippy::question_mark)] // fn returns the denial text, not a Result
        if let Err(denial) = self
            .gate_approval(
                conversation_id,
                &call.id,
                &entry,
                &server_title,
                &call.arguments,
                parent,
                ct,
            )
            .await
        {
            return denial;
        }

        self.emit_tool_update(conversation_id, &call.id, "running", serde_json::json!({}), parent);

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
                        parent,
                    );
                    text
                }
                Err(e) => {
                    self.emit_tool_update(
                        conversation_id,
                        &call.id,
                        "error",
                        serde_json::json!({ "result_text": e, "is_error": true }),
                        parent,
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
                        parent,
                    );
                    return format!("Error: {e}");
                }
            };
            if !matches!(status, crate::mcp::manager::ServerStatus::Connected) {
                // A sign-in wall gets an inline re-auth card; the call
                // continues automatically once the user signs in. Anything
                // else is a plain failure.
                match self
                    .await_reauth(conversation_id, &call.id, &entry, &server_title, parent, ct)
                    .await
                {
                    Ok(()) => {
                        self.emit_tool_update(
                            conversation_id,
                            &call.id,
                            "running",
                            serde_json::json!({}),
                            parent,
                        );
                    }
                    Err(e) => {
                        self.emit_tool_update(
                            conversation_id,
                            &call.id,
                            "error",
                            serde_json::json!({ "result_text": e, "is_error": true }),
                            parent,
                        );
                        return format!("Error: {e}");
                    }
                }
            }
        }

        // Live progress for server-side tasks (tasks extension).
        let sink = self.sink.clone();
        let conversation = conversation_id.to_string();
        let tool_call_id = call.id.clone();
        let on_task = move |snapshot: crate::mcp::manager::TaskSnapshot| {
            sink.emit(crate::events::BackendEvent::TaskUpdate {
                conversation_id: Some(conversation.clone()),
                tool_call_id: Some(tool_call_id.clone()),
                task_id: snapshot.task_id,
                status: snapshot.status,
                status_message: snapshot.status_message,
            });
        };

        let attempt = || async {
            tokio::select! {
                _ = ct.cancelled() => Err("cancelled by user".to_string()),
                r = self.manager.call_tool(
                    &entry.server_id,
                    &entry.name,
                    call.arguments.clone(),
                    Some(call.id.clone()),
                    Some(&on_task),
                    ct.clone(),
                ) => r,
            }
        };

        let mut result = attempt().await;

        // The server hit an auth wall mid-session: offer inline sign-in and
        // retry the call once.
        if result.is_err()
            && matches!(
                self.manager.status(&entry.server_id),
                crate::mcp::manager::ServerStatus::NeedsAuth { .. }
            )
        {
            match self
                .await_reauth(conversation_id, &call.id, &entry, &server_title, parent, ct)
                .await
            {
                Ok(()) => {
                    self.emit_tool_update(
                        conversation_id,
                        &call.id,
                        "running",
                        serde_json::json!({}),
                        parent,
                    );
                    result = attempt().await;
                }
                Err(e) => result = Err(e),
            }
        }

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
                    parent,
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
                    parent,
                );
                format!("Error: {e}")
            }
        }
    }

    /// Surface an inline sign-in card and wait (bounded) for the user to
    /// complete browser sign-in, then make sure the server is connected again.
    async fn await_reauth(
        &self,
        conversation_id: &str,
        tool_call_id: &str,
        entry: &crate::mcp::manager::ToolEntry,
        server_title: &str,
        parent: Option<&str>,
        ct: &CancellationToken,
    ) -> Result<(), String> {
        let generation = self.manager.auth_generation(&entry.server_id);
        self.emit_tool_update(
            conversation_id,
            tool_call_id,
            "needs_auth",
            serde_json::json!({
                "server": entry.server_id,
                "server_title": server_title,
            }),
            parent,
        );
        tokio::select! {
            _ = ct.cancelled() => return Err("cancelled by user".to_string()),
            r = self.manager.wait_for_auth(
                &entry.server_id,
                generation,
                std::time::Duration::from_secs(5 * 60),
            ) => r?,
        }
        // Sign-in completed and the login command reconnects, but make sure
        // (connect is idempotent).
        match self.manager.status(&entry.server_id) {
            crate::mcp::manager::ServerStatus::Connected => Ok(()),
            _ => {
                let status = self.manager.connect(&entry.server_id).await?;
                if matches!(status, crate::mcp::manager::ServerStatus::Connected) {
                    Ok(())
                } else {
                    Err(format!(
                        "Signed in, but {server_title} could not be reached. Open it on the \
                         Connectors page for details."
                    ))
                }
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
            meta.clone()
        };
        let payload: Vec<serde_json::Value> = history.iter().map(|m| m.as_json()).collect();
        // keep the /undo records `chat_send` captured for this conversation
        let undo = self.store.load_undo_records(conversation_id);
        self.store
            .save_conversation(&meta, &payload, &undo)
            .map_err(|e| e.to_string())
    }

    fn current_title(&self, conversation_id: &str) -> String {
        self.store
            .config
            .lock()
            .unwrap()
            .conversations
            .iter()
            .find(|c| c.id == conversation_id)
            .map(|c| c.title.clone())
            .unwrap_or_default()
    }

    /// Generate a title from `user_text`. Does not write into chat history.
    /// `force` overwrites an existing title; auto-gen does not.
    pub async fn generate_title(
        &self,
        conversation_id: &str,
        user_text: &str,
        force: bool,
        ct: CancellationToken,
    ) {
        if ct.is_cancelled() {
            self.sink.emit(BackendEvent::TitleUpdated {
                conversation_id: conversation_id.to_string(),
                title: self.current_title(conversation_id),
            });
            return;
        }
        self.sink.emit(BackendEvent::TitleGenerating {
            conversation_id: conversation_id.to_string(),
        });

        let generated = self.stream_title(conversation_id, user_text, &ct).await;
        if ct.is_cancelled() {
            self.sink.emit(BackendEvent::TitleUpdated {
                conversation_id: conversation_id.to_string(),
                title: self.current_title(conversation_id),
            });
            return;
        }

        let title = match generated {
            Ok(raw) => {
                let s = crate::title::sanitize_title(&raw);
                if s.is_empty() {
                    crate::title::fallback_title(user_text)
                } else {
                    s
                }
            }
            Err(_) => crate::title::fallback_title(user_text),
        };

        let kept = {
            let mut cfg = self.store.config.lock().unwrap();
            let Some(meta) = cfg
                .conversations
                .iter_mut()
                .find(|c| c.id == conversation_id)
            else {
                return;
            };
            if !force && !meta.title.is_empty() {
                Some(meta.title.clone())
            } else {
                meta.title = title.clone();
                None
            }
        };
        let title = match kept {
            Some(existing) => existing,
            None => {
                let _ = self.store.save_config();
                title
            }
        };
        self.sink.emit(BackendEvent::TitleUpdated {
            conversation_id: conversation_id.to_string(),
            title,
        });
    }

    async fn stream_title(
        &self,
        conversation_id: &str,
        user_text: &str,
        ct: &CancellationToken,
    ) -> Result<String, String> {
        let (settings, meta) = {
            let cfg = self.store.config.lock().unwrap();
            let meta = cfg
                .conversations
                .iter()
                .find(|c| c.id == conversation_id)
                .cloned()
                .ok_or("conversation missing")?;
            (cfg.settings.clone(), meta)
        };
        let resolved = crate::title::resolve_title_model(&settings, &meta);
        let (provider, default_model, _) = self.provider_for(&resolved.provider_id)?;
        let model = if resolved.model.is_empty() {
            default_model
        } else {
            resolved.model
        };
        let options = crate::providers::ChatOptions {
            model,
            max_tokens: Some(64),
            temperature: Some(0.3),
            effort: resolved.effort,
        };
        let messages = vec![
            Msg::System {
                text:
                    "Write a short chat title (max 8 words). Reply with the title only. No quotes."
                        .into(),
            },
            Msg::User {
                text: user_text.to_string(),
                ts: None,
            },
        ];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ProviderEvent>(64);
        let call = provider.stream_chat(&messages, &[], &options, tx);
        let mut text = String::new();
        let mut call = std::pin::pin!(call);
        let mut stop_ok = false;
        loop {
            tokio::select! {
                biased;
                _ = ct.cancelled() => return Err("cancelled".into()),
                ev = rx.recv() => {
                    match ev {
                        Some(ProviderEvent::TextDelta(t)) => text.push_str(&t),
                        Some(_) => {}
                        None => break,
                    }
                }
                result = &mut call => {
                    result.map_err(|e| e.to_string())?;
                    stop_ok = true;
                }
            }
        }
        while let Ok(ev) = rx.try_recv() {
            if let ProviderEvent::TextDelta(t) = ev {
                text.push_str(&t);
            }
        }
        if !stop_ok {
            return Err("The provider stream ended unexpectedly".into());
        }
        Ok(text)
    }
}
