//! OpenAI-compatible `/chat/completions` streaming adapter.
//!
//! Used for OpenAI, Z.ai (both endpoints), OpenCode Zen, Ollama, LM Studio,
//! OpenRouter, Groq and any other compatible gateway.

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{
    ensure_ok, http_client, stream_sse, ChatOptions, LlmProvider, Msg, ProviderEvent, StopReason,
    ToolDef,
};

pub struct OpenAiProvider {
    pub base_url: String,
    pub api_key: Option<String>,
    /// Display name used in error messages.
    pub name: String,
}

impl OpenAiProvider {
    fn endpoint(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}{path}")
    }

    fn auth_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    fn messages_to_wire(messages: &[Msg]) -> Vec<Value> {
        let mut out = Vec::new();
        for msg in messages {
            match msg {
                Msg::System { text } => out.push(json!({"role": "system", "content": text})),
                Msg::User { text } => out.push(json!({"role": "user", "content": text})),
                Msg::Assistant { text, tool_calls } => {
                    let mut m = json!({"role": "assistant"});
                    m["content"] = if text.is_empty() {
                        Value::Null
                    } else {
                        json!(text)
                    };
                    if !tool_calls.is_empty() {
                        m["tool_calls"] = Value::Array(
                            tool_calls
                                .iter()
                                .map(|tc| {
                                    json!({
                                        "id": tc.id,
                                        "type": "function",
                                        "function": {
                                            "name": tc.name,
                                            "arguments": tc.arguments.to_string(),
                                        }
                                    })
                                })
                                .collect(),
                        );
                    }
                    out.push(m);
                }
                Msg::ToolResult { call_id, text, is_error } => {
                    let content = if *is_error { format!("Error: {text}") } else { text.clone() };
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": content,
                    }));
                }
            }
        }
        out
    }

    fn tools_to_wire(tools: &[ToolDef]) -> Option<Vec<Value>> {
        if tools.is_empty() {
            return None;
        }
        Some(
            tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect(),
        )
    }
}

/// Accumulated state for one streamed turn.
#[derive(Default)]
struct Turn {
    /// index -> (id, name, arguments buffer)
    tool_calls: std::collections::BTreeMap<usize, (String, String, String)>,
    finish_reason: Option<String>,
    usage_in: Option<u64>,
    usage_out: Option<u64>,
}

impl Turn {
    /// Handle one streaming chunk; returns an event to forward, if any.
    fn handle_chunk(&mut self, v: &Value, provider: &str) -> anyhow::Result<Option<ProviderEvent>> {
        if let Some(usage) = v.get("usage") {
            self.usage_in = usage.get("prompt_tokens").and_then(|x| x.as_u64());
            self.usage_out = usage.get("completion_tokens").and_then(|x| x.as_u64());
        }
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown stream error");
            return Err(anyhow::anyhow!("{provider} stream error: {msg}"));
        }

        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return Ok(None);
        };
        if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.finish_reason = Some(fr.to_string());
        }
        let Some(delta) = choice.get("delta") else {
            return Ok(None);
        };
        if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                return Ok(Some(ProviderEvent::TextDelta(t.to_string())));
            }
        }
        // reasoning stream variants: reasoning_content (DeepSeek/Z.ai), reasoning (others)
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = delta.get(key).and_then(|c| c.as_str()) {
                if !t.is_empty() {
                    return Ok(Some(ProviderEvent::ReasoningDelta(t.to_string())));
                }
            }
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let entry = self
                    .tool_calls
                    .entry(index)
                    .or_insert_with(|| (String::new(), String::new(), String::new()));
                if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                    if !id.is_empty() {
                        entry.0 = id.to_string();
                    }
                }
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                        if !name.is_empty() {
                            entry.1 = name.to_string();
                        }
                    }
                    if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                        entry.2.push_str(args);
                    }
                }
            }
        }
        Ok(None)
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn stream_chat(
        &self,
        messages: &[Msg],
        tools: &[ToolDef],
        opts: &ChatOptions,
        tx: mpsc::Sender<ProviderEvent>,
    ) -> anyhow::Result<StopReason> {
        let mut body = json!({
            "model": opts.model,
            "messages": Self::messages_to_wire(messages),
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if let Some(tools) = Self::tools_to_wire(tools) {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(max) = opts.max_tokens {
            body["max_tokens"] = json!(max);
        }
        if let Some(t) = opts.temperature {
            body["temperature"] = json!(t);
        }

        let response = self
            .auth_headers(http_client().post(self.endpoint("/chat/completions")))
            .json(&body)
            .send()
            .await?;
        let response = ensure_ok(response, &self.name).await?;

        let mut turn = Turn::default();
        stream_sse(response, &tx, |data| {
            let v: Value = serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("Bad stream chunk from {}: {e}", self.name))?;
            turn.handle_chunk(&v, &self.name)
        })
        .await?;

        // Emit completed tool calls (OpenAI streams fragments, so the full
        // arguments are forwarded once at the end).
        for (index, (id, name, args_json)) in turn.tool_calls {
            let arguments: Value = if args_json.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&args_json).unwrap_or_else(|_| json!({ "raw": args_json }))
            };
            let id = if id.is_empty() { format!("call_{index}") } else { id };
            tx.send(ProviderEvent::ToolCallBegin { index, id, name })
                .await
                .ok();
            tx.send(ProviderEvent::ToolCallArgsDelta {
                index,
                fragment: arguments.to_string(),
            })
            .await
            .ok();
        }
        if let Some(u) = turn.usage_in {
            tx.send(ProviderEvent::Usage { input: Some(u), output: turn.usage_out })
                .await
                .ok();
        }

        Ok(match turn.finish_reason.as_deref() {
            Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
            Some("length") | Some("max_tokens") => StopReason::Length,
            _ => StopReason::EndTurn,
        })
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let response = self
            .auth_headers(http_client().get(self.endpoint("/models")))
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
    fn converts_messages_and_tools() {
        let msgs = vec![
            Msg::System { text: "be nice".into() },
            Msg::User { text: "hello".into() },
            Msg::Assistant {
                text: "".into(),
                tool_calls: vec![super::super::ToolCall {
                    id: "c1".into(),
                    name: "fs__read".into(),
                    arguments: json!({"path": "a.txt"}),
                }],
            },
            Msg::ToolResult { call_id: "c1".into(), text: "contents".into(), is_error: false },
        ];
        let wire = OpenAiProvider::messages_to_wire(&msgs);
        assert_eq!(wire[0]["role"], "system");
        assert_eq!(wire[2]["tool_calls"][0]["function"]["name"], "fs__read");
        assert_eq!(wire[3]["tool_call_id"], "c1");

        let tools = vec![ToolDef {
            name: "fs__read".into(),
            description: "read".into(),
            parameters: json!({"type": "object"}),
        }];
        let tw = OpenAiProvider::tools_to_wire(&tools).unwrap();
        assert_eq!(tw[0]["type"], "function");
        assert!(OpenAiProvider::tools_to_wire(&[]).is_none());
    }

    #[test]
    fn accumulates_tool_call_fragments() {
        let mut turn = Turn::default();
        let chunk = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        {"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{\"lo"}}
                    ]
                }
            }]
        });
        turn.handle_chunk(&chunk, "test").unwrap();
        let chunk2 = json!({
            "choices": [{
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "cation\":\"NYC\"}"}}]},
                "finish_reason": "tool_calls"
            }]
        });
        turn.handle_chunk(&chunk2, "test").unwrap();

        let (id, name, args) = turn.tool_calls.get(&0).unwrap().clone();
        assert_eq!(id, "call_1");
        assert_eq!(name, "get_weather");
        assert_eq!(args, "{\"location\":\"NYC\"}");
        assert_eq!(turn.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn text_and_reasoning_deltas() {
        let mut turn = Turn::default();
        let chunk = json!({
            "choices": [{"delta": {"content": "Hi", "reasoning_content": "thinking"}}]
        });
        assert!(matches!(
            turn.handle_chunk(&chunk, "t").unwrap(),
            Some(ProviderEvent::TextDelta(_))
        ));
        let chunk2 = json!({
            "choices": [{"delta": {"reasoning_content": " more"}}]
        });
        assert!(matches!(
            turn.handle_chunk(&chunk2, "t").unwrap(),
            Some(ProviderEvent::ReasoningDelta(_))
        ));
    }
}
