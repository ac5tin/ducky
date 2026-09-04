//! Anthropic `/v1/messages` streaming adapter (Claude).

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{
    ensure_ok, http_client, stream_sse, ChatOptions, LlmProvider, Msg, ProviderEvent, StopReason,
    ToolDef,
};

pub struct AnthropicProvider {
    pub base_url: String,
    pub api_key: String,
    /// Display name used in error messages.
    pub name: String,
}

const ANTHROPIC_VERSION: &str = "2023-06-01";

impl AnthropicProvider {
    fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}{path}")
    }

    /// Convert the unified message list to Anthropic's wire format.
    /// Returns `(system_prompt, messages)`.
    fn messages_to_wire(messages: &[Msg]) -> (String, Vec<Value>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut out: Vec<Value> = Vec::new();

        for msg in messages {
            match msg {
                Msg::System { text } => system_parts.push(text.clone()),
                Msg::User { text } => {
                    push_block(&mut out, "user", json!({"type": "text", "text": text}));
                }
                Msg::Assistant { text, tool_calls } => {
                    if !text.is_empty() {
                        push_block(&mut out, "assistant", json!({"type": "text", "text": text}));
                    }
                    for tc in tool_calls {
                        push_block(
                            &mut out,
                            "assistant",
                            json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": tc.arguments,
                            }),
                        );
                    }
                }
                Msg::ToolResult {
                    call_id,
                    text,
                    is_error,
                } => {
                    // tool results are user messages in Anthropic's protocol;
                    // consecutive results must share one user message
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": text,
                        "is_error": is_error,
                    });
                    if let Some(last) = out.last_mut() {
                        if last["role"] == "user" {
                            if let Some(arr) = last["content"].as_array_mut() {
                                // only merge when the last block is also a tool_result
                                if arr
                                    .last()
                                    .and_then(|b| b.get("type"))
                                    .and_then(|t| t.as_str())
                                    == Some("tool_result")
                                {
                                    arr.push(block);
                                    continue;
                                }
                            }
                        }
                    }
                    out.push(json!({"role": "user", "content": [block]}));
                }
            }
        }

        (system_parts.join("\n\n"), out)
    }

    fn tools_to_wire(tools: &[ToolDef]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    /// Anthropic expresses effort as an extended-thinking token budget;
    /// `None` (effort `none`) means no thinking block at all.
    fn thinking_budget(effort: crate::config::EffortLevel) -> Option<u32> {
        use crate::config::EffortLevel;
        match effort {
            EffortLevel::None => None,
            EffortLevel::Minimal => Some(1024),
            EffortLevel::Low => Some(2048),
            EffortLevel::Medium => Some(8192),
            EffortLevel::High => Some(16384),
            EffortLevel::XHigh => Some(32768),
            EffortLevel::Max => Some(65536),
        }
    }

    fn build_body(
        system: &str,
        wire_msgs: Vec<Value>,
        tools: &[ToolDef],
        opts: &ChatOptions,
    ) -> Value {
        let mut body = json!({
            "model": opts.model,
            "max_tokens": opts.max_tokens.unwrap_or(8192),
            "messages": wire_msgs,
            "stream": true,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !tools.is_empty() {
            body["tools"] = json!(Self::tools_to_wire(tools));
        }
        if let Some(budget) = opts.effort.and_then(Self::thinking_budget) {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            // thinking requires max_tokens > budget_tokens
            body["max_tokens"] = json!(budget + 8192);
            // and requires temperature to be unset or exactly 1 — omit it
        } else if let Some(t) = opts.temperature {
            body["temperature"] = json!(t);
        }
        body
    }
}

/// Append a content block, merging into the previous message when the role
/// matches (Anthropic requires strictly alternating roles).
fn push_block(out: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = out.last_mut() {
        if last["role"] == role {
            if let Some(arr) = last["content"].as_array_mut() {
                arr.push(block);
                return;
            }
        }
    }
    out.push(json!({"role": role, "content": [block]}));
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn stream_chat(
        &self,
        messages: &[Msg],
        tools: &[ToolDef],
        opts: &ChatOptions,
        tx: mpsc::Sender<ProviderEvent>,
    ) -> anyhow::Result<StopReason> {
        let (system, wire_msgs) = Self::messages_to_wire(messages);
        let body = Self::build_body(&system, wire_msgs, tools, opts);

        let response = http_client()
            .post(self.endpoint("/messages"))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;
        let response = ensure_ok(response, &self.name).await?;

        let mut stop_reason: Option<String> = None;
        let mut usage_in = None;
        let mut usage_out = None;

        stream_sse(response, &tx, |data| {
            let v: Value = serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("Bad stream chunk from {}: {e}", self.name))?;
            let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match event_type {
                "error" => {
                    let msg = v
                        .pointer("/error/message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("stream error");
                    Err(anyhow::anyhow!("{} error: {}", self.name, msg))
                }
                "message_start" => {
                    usage_in = v
                        .pointer("/message/usage/input_tokens")
                        .and_then(|u| u.as_u64());
                    Ok(None)
                }
                "content_block_start" => {
                    if v.pointer("/content_block/type").and_then(|t| t.as_str()) == Some("tool_use")
                    {
                        let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let id = v
                            .pointer("/content_block/id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = v
                            .pointer("/content_block/name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        Ok(Some(ProviderEvent::ToolCallBegin { index, id, name }))
                    } else {
                        Ok(None)
                    }
                }
                "content_block_delta" => {
                    let delta = v.get("delta").cloned().unwrap_or(Value::Null);
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            let t = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            if t.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some(ProviderEvent::TextDelta(t.to_string())))
                            }
                        }
                        Some("thinking_delta") => {
                            let t = delta.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
                            if t.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some(ProviderEvent::ReasoningDelta(t.to_string())))
                            }
                        }
                        Some("input_json_delta") => {
                            let frag = delta
                                .get("partial_json")
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            let index =
                                v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            if frag.is_empty() {
                                Ok(None)
                            } else {
                                Ok(Some(ProviderEvent::ToolCallArgsDelta {
                                    index,
                                    fragment: frag.to_string(),
                                }))
                            }
                        }
                        _ => Ok(None),
                    }
                }
                "message_delta" => {
                    if let Some(sr) = v.pointer("/delta/stop_reason").and_then(|s| s.as_str()) {
                        stop_reason = Some(sr.to_string());
                    }
                    if let Some(u) = v.pointer("/usage/output_tokens").and_then(|u| u.as_u64()) {
                        usage_out = Some(u);
                    }
                    Ok(None)
                }
                _ => Ok(None),
            }
        })
        .await?;

        if let Some(u) = usage_in {
            tx.send(ProviderEvent::Usage {
                input: Some(u),
                output: usage_out,
            })
            .await
            .ok();
        }

        Ok(match stop_reason.as_deref() {
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::Length,
            _ => StopReason::EndTurn,
        })
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let response = http_client()
            .get(self.endpoint("/models"))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await?;
        let response = ensure_ok(response, &self.name).await?;
        let v: Value = response.json().await?;
        let mut models: Vec<String> = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        models.sort();
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_messages() {
        let msgs = vec![
            Msg::System {
                text: "be nice".into(),
            },
            Msg::User {
                text: "hello".into(),
            },
            Msg::Assistant {
                text: "let me check".into(),
                tool_calls: vec![super::super::ToolCall {
                    id: "t1".into(),
                    name: "fs__read".into(),
                    arguments: json!({"path": "a.txt"}),
                }],
            },
            Msg::ToolResult {
                call_id: "t1".into(),
                text: "data".into(),
                is_error: true,
            },
        ];
        let (system, wire) = AnthropicProvider::messages_to_wire(&msgs);
        assert_eq!(system, "be nice");
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[1]["role"], "assistant");
        assert_eq!(wire[1]["content"][0]["type"], "text");
        assert_eq!(wire[1]["content"][1]["type"], "tool_use");
        assert_eq!(wire[1]["content"][1]["input"]["path"], "a.txt");
        assert_eq!(wire[2]["role"], "user");
        assert_eq!(wire[2]["content"][0]["type"], "tool_result");
        assert_eq!(wire[2]["content"][0]["is_error"], true);

        // consecutive tool results merge into one user message
        let msgs2 = vec![
            Msg::User { text: "go".into() },
            Msg::Assistant {
                text: String::new(),
                tool_calls: vec![
                    super::super::ToolCall {
                        id: "a".into(),
                        name: "x".into(),
                        arguments: json!({}),
                    },
                    super::super::ToolCall {
                        id: "b".into(),
                        name: "y".into(),
                        arguments: json!({}),
                    },
                ],
            },
            Msg::ToolResult {
                call_id: "a".into(),
                text: "1".into(),
                is_error: false,
            },
            Msg::ToolResult {
                call_id: "b".into(),
                text: "2".into(),
                is_error: false,
            },
        ];
        let (_, wire2) = AnthropicProvider::messages_to_wire(&msgs2);
        assert_eq!(
            wire2.len(),
            3,
            "tool results must merge into one user message"
        );
        assert_eq!(wire2[2]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn tools_convert() {
        let tools = vec![ToolDef {
            name: "n".into(),
            description: "d".into(),
            parameters: json!({"type": "object"}),
        }];
        let wire = AnthropicProvider::tools_to_wire(&tools);
        assert_eq!(wire[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn effort_maps_to_thinking_budget() {
        use crate::config::EffortLevel;
        let opts = |effort| ChatOptions {
            model: "claude-opus-4.7".into(),
            max_tokens: None,
            temperature: None,
            effort,
        };

        let body = AnthropicProvider::build_body("", vec![], &[], &opts(Some(EffortLevel::Medium)));
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
        assert_eq!(body["max_tokens"], 8192 + 8192);

        let body = AnthropicProvider::build_body("", vec![], &[], &opts(Some(EffortLevel::High)));
        assert_eq!(body["thinking"]["budget_tokens"], 16384);

        // effort "none" sends no thinking block at all
        let body = AnthropicProvider::build_body("", vec![], &[], &opts(Some(EffortLevel::None)));
        assert!(body.get("thinking").is_none());

        let body = AnthropicProvider::build_body("", vec![], &[], &opts(None));
        assert!(body.get("thinking").is_none());
        assert_eq!(body["max_tokens"], 8192);
    }
}
