//! OpenAI `/responses` streaming adapter.
//!
//! Gateways can serve one catalogue over several wires, and OpenCode Go routes
//! its Grok and GPT models here instead of `/chat/completions`. The body shape
//! differs from chat completions in three ways: the system prompt travels as a
//! top-level `instructions` string, tools are declared flat (no nested
//! `function` object), and a replayed tool call is its own input item.

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{
    ensure_ok, gateway_headers, http_client, stream_sse, ChatOptions, LlmProvider, Msg,
    ProviderEvent, StopReason, ToolDef,
};

pub struct ResponsesProvider {
    pub base_url: String,
    pub api_key: Option<String>,
    /// Display name used in error messages.
    pub name: String,
}

impl ResponsesProvider {
    fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}{path}")
    }

    /// System turns become the top-level `instructions` string.
    fn instructions(messages: &[Msg]) -> String {
        messages
            .iter()
            .filter_map(|m| match m {
                Msg::System { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn messages_to_input(messages: &[Msg]) -> Vec<Value> {
        let mut out = Vec::new();
        for msg in messages {
            match msg {
                Msg::System { .. } => {}
                Msg::User { text, .. } => out.push(json!({
                    "role": "user",
                    "content": [{ "type": "input_text", "text": text }],
                })),
                Msg::Assistant {
                    text, tool_calls, ..
                } => {
                    if !text.is_empty() {
                        out.push(json!({
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": text }],
                        }));
                    }
                    for tc in tool_calls {
                        out.push(json!({
                            "type": "function_call",
                            "id": format!("fc_{}", sanitize_id(&tc.id)),
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": tc.arguments.to_string(),
                        }));
                    }
                }
                Msg::ToolResult {
                    call_id,
                    text,
                    is_error,
                } => out.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": if *is_error { format!("Error: {text}") } else { text.clone() },
                })),
            }
        }
        out
    }

    fn tools_to_wire(tools: &[ToolDef]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect()
    }

    fn build_body(messages: &[Msg], tools: &[ToolDef], opts: &ChatOptions) -> Value {
        let mut body = json!({
            "model": opts.model,
            "input": Self::messages_to_input(messages),
            "stream": true,
            // Ducky keeps the transcript, so the gateway must not keep one.
            "store": false,
        });
        let instructions = Self::instructions(messages);
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        if !tools.is_empty() {
            body["tools"] = json!(Self::tools_to_wire(tools));
            body["tool_choice"] = json!("auto");
        }
        if let Some(max) = opts.max_tokens {
            body["max_output_tokens"] = json!(max);
        }
        if let Some(t) = opts.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(effort) = opts.effort {
            body["reasoning"] = json!({ "effort": effort.as_str() });
        }
        body
    }

    fn request(&self, opts: &ChatOptions) -> reqwest::RequestBuilder {
        let mut req = http_client()
            .post(self.endpoint("/responses"))
            .header(reqwest::header::USER_AGENT, super::USER_AGENT);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        for (name, value) in gateway_headers(&self.base_url, opts.session_id.as_deref()) {
            req = req.header(name, value);
        }
        req
    }
}

/// The Responses API ties a replayed call to its output item by an id that must
/// start with `fc_` and stay within 64 characters.
fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_end_matches('_');
    trimmed.chars().take(61).collect()
}

/// Accumulated state for one streamed turn.
#[derive(Default)]
struct Turn {
    /// output_index -> (call_id, name, arguments buffer)
    tool_calls: std::collections::BTreeMap<usize, (String, String, String)>,
    usage_in: Option<u64>,
    usage_out: Option<u64>,
    /// The response stopped early (e.g. `max_output_tokens`).
    incomplete: bool,
    /// A failure reported inside the stream.
    failure: Option<String>,
}

impl Turn {
    /// Handle one streaming event; returns an event to forward, if any.
    fn handle_event(&mut self, v: &Value) -> anyhow::Result<Option<ProviderEvent>> {
        let event = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event {
            "response.output_text.delta" => Ok(delta(v, "delta", ProviderEvent::TextDelta)),
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                Ok(delta(v, "delta", ProviderEvent::ReasoningDelta))
            }
            "response.output_item.added" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
                    return Ok(None);
                }
                let index = v.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let call_id = item
                    .get("call_id")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.tool_calls
                    .insert(index, (call_id.clone(), name.clone(), String::new()));
                Ok(Some(ProviderEvent::ToolCallBegin {
                    index,
                    id: call_id,
                    name,
                }))
            }
            "response.function_call_arguments.delta" => {
                let Some(fragment) = v.get("delta").and_then(|d| d.as_str()) else {
                    return Ok(None);
                };
                if fragment.is_empty() {
                    return Ok(None);
                }
                let index = v.get("output_index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                if let Some(entry) = self.tool_calls.get_mut(&index) {
                    entry.2.push_str(fragment);
                }
                Ok(Some(ProviderEvent::ToolCallArgsDelta {
                    index,
                    fragment: fragment.to_string(),
                }))
            }
            "response.completed" => {
                let usage = v.pointer("/response/usage").cloned().unwrap_or(Value::Null);
                self.usage_in = usage.get("input_tokens").and_then(|u| u.as_u64());
                self.usage_out = usage.get("output_tokens").and_then(|u| u.as_u64());
                Ok(None)
            }
            "response.incomplete" => {
                let usage = v.pointer("/response/usage").cloned().unwrap_or(Value::Null);
                self.usage_in = usage.get("input_tokens").and_then(|u| u.as_u64());
                self.usage_out = usage.get("output_tokens").and_then(|u| u.as_u64());
                self.incomplete = true;
                Ok(None)
            }
            "response.failed" | "response.error" | "error" => {
                self.failure = Some(
                    v.pointer("/response/error/message")
                        .or_else(|| v.pointer("/error/message"))
                        .or_else(|| v.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("the provider reported a failure")
                        .to_string(),
                );
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

fn delta(v: &Value, key: &str, wrap: fn(String) -> ProviderEvent) -> Option<ProviderEvent> {
    v.get(key)
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
        .map(|d| wrap(d.to_string()))
}

#[async_trait]
impl LlmProvider for ResponsesProvider {
    async fn stream_chat(
        &self,
        messages: &[Msg],
        tools: &[ToolDef],
        opts: &ChatOptions,
        tx: mpsc::Sender<ProviderEvent>,
    ) -> anyhow::Result<StopReason> {
        let body = Self::build_body(messages, tools, opts);
        let response = self.request(opts).json(&body).send().await?;
        let response = ensure_ok(response, &self.name).await?;

        let mut turn = Turn::default();
        stream_sse(response, &tx, |data| {
            let v: Value = serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("Bad stream chunk from {}: {e}", self.name))?;
            turn.handle_event(&v)
        })
        .await?;

        if let Some(detail) = turn.failure {
            return Err(anyhow::anyhow!("{} error: {detail}", self.name));
        }
        if let Some(u) = turn.usage_in {
            tx.send(ProviderEvent::Usage {
                input: Some(u),
                output: turn.usage_out,
            })
            .await
            .ok();
        }

        // Tool calls stream as fragments, so the arguments are already known by
        // the time the turn ends; a call the gateway reports twice keeps the
        // first entry.
        if !turn.tool_calls.is_empty() {
            return Ok(StopReason::ToolUse);
        }
        Ok(if turn.incomplete {
            StopReason::Length
        } else {
            StopReason::EndTurn
        })
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        // The catalogue is a property of the gateway, not of one wire, and the
        // chat-completions adapter already lists it.
        let mut models: Vec<String> = Vec::new();
        let response = http_client()
            .get(self.endpoint("/models"))
            .header(reqwest::header::USER_AGENT, super::USER_AGENT)
            .bearer_auth(self.api_key.clone().unwrap_or_default())
            .send()
            .await?;
        let response = ensure_ok(response, &self.name).await?;
        let v: Value = response.json().await?;
        if let Some(data) = v.get("data").and_then(|d| d.as_array()) {
            for m in data {
                if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                    models.push(id.to_string());
                }
            }
        }
        models.sort();
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ToolCall;

    fn opts() -> ChatOptions {
        ChatOptions {
            model: "gpt-6-luna".into(),
            max_tokens: None,
            temperature: None,
            effort: None,
            session_id: None,
        }
    }

    #[test]
    fn system_text_becomes_instructions_and_users_become_input_items() {
        let messages = vec![
            Msg::System {
                text: "be nice".into(),
            },
            Msg::User {
                text: "hello".into(),
                ts: None,
            },
        ];
        let body = ResponsesProvider::build_body(&messages, &[], &opts());
        assert_eq!(body["instructions"], "be nice");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][0]["text"], "hello");
        // no system item among the input entries
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn assistant_tool_calls_and_results_replay_as_items() {
        let messages = vec![
            Msg::Assistant {
                text: "let me check".into(),
                tool_calls: vec![ToolCall {
                    id: "call/1".into(),
                    name: "fs__read".into(),
                    arguments: json!({ "path": "a.txt" }),
                }],
                ts: None,
            },
            Msg::ToolResult {
                call_id: "call/1".into(),
                text: "data".into(),
                is_error: true,
            },
        ];
        let input = ResponsesProvider::messages_to_input(&messages);
        assert_eq!(input[0]["role"], "assistant");
        assert_eq!(input[0]["content"][0]["type"], "output_text");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call/1");
        assert_eq!(input[1]["name"], "fs__read");
        // the Responses API requires an item id that starts with fc_
        assert_eq!(input[1]["id"], "fc_call_1");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["output"], "Error: data");
    }

    #[test]
    fn tools_are_declared_flat() {
        let tools = vec![ToolDef {
            name: "fs__read".into(),
            description: "read".into(),
            parameters: json!({ "type": "object" }),
        }];
        let body = ResponsesProvider::build_body(&[], &tools, &opts());
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "fs__read");
        assert!(body["tools"][0].get("function").is_none());
    }

    #[test]
    fn effort_maps_to_the_reasoning_object() {
        let opts = ChatOptions {
            effort: Some(crate::config::EffortLevel::High),
            ..opts()
        };
        let body = ResponsesProvider::build_body(&[], &[], &opts);
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn item_ids_stay_within_the_fc_prefix_and_length_limit() {
        assert_eq!(sanitize_id("call/1"), "call_1");
        let long = "x".repeat(200);
        let id = sanitize_id(&long);
        assert!(id.len() <= 61);
        assert!(format!("fc_{id}").len() <= 64);
    }

    #[test]
    fn text_reasoning_and_tool_fragments_stream_through() {
        let mut turn = Turn::default();
        let text = json!({"type": "response.output_text.delta", "delta": "Hi"});
        assert!(matches!(
            turn.handle_event(&text).unwrap(),
            Some(ProviderEvent::TextDelta(t)) if t == "Hi"
        ));
        let reasoning = json!({"type": "response.reasoning_summary_text.delta", "delta": "hm"});
        assert!(matches!(
            turn.handle_event(&reasoning).unwrap(),
            Some(ProviderEvent::ReasoningDelta(_))
        ));

        let added = json!({
            "type": "response.output_item.added",
            "output_index": 1,
            "item": {"type": "function_call", "call_id": "call_9", "name": "fs__read"}
        });
        assert!(matches!(
            turn.handle_event(&added).unwrap(),
            Some(ProviderEvent::ToolCallBegin { index: 1, id, name })
                if id == "call_9" && name == "fs__read"
        ));

        // a non-tool item must not start a tool call
        let message_item = json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"type": "message"}
        });
        assert!(turn.handle_event(&message_item).unwrap().is_none());

        let args = json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 1,
            "delta": "{\"path\":\"a\"}"
        });
        assert!(matches!(
            turn.handle_event(&args).unwrap(),
            Some(ProviderEvent::ToolCallArgsDelta { index: 1, fragment }) if fragment == "{\"path\":\"a\"}"
        ));
        assert_eq!(turn.tool_calls.get(&1).unwrap().2, "{\"path\":\"a\"}");
    }

    #[test]
    fn completed_reads_usage_and_failed_carries_the_message() {
        let mut turn = Turn::default();
        let done = json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": 12, "output_tokens": 34}}
        });
        assert!(turn.handle_event(&done).unwrap().is_none());
        assert_eq!(turn.usage_in, Some(12));
        assert_eq!(turn.usage_out, Some(34));

        let mut turn = Turn::default();
        let failed = json!({
            "type": "response.failed",
            "response": {"error": {"message": "model not found"}}
        });
        turn.handle_event(&failed).unwrap();
        assert_eq!(turn.failure.as_deref(), Some("model not found"));

        let mut turn = Turn::default();
        let incomplete = json!({"type": "response.incomplete", "response": {}});
        turn.handle_event(&incomplete).unwrap();
        assert!(turn.incomplete);
    }
}
