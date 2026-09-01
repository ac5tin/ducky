//! Unified LLM provider layer.
//!
//! Every provider (OpenAI, Claude, Z.ai, Ollama, OpenRouter, Groq, LM Studio,
//! OpenCode Zen, or any custom endpoint) is normalised to the same
//! `LlmProvider` trait so the chat engine never cares which one is active.

pub mod anthropic;
pub mod openai;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::{ApiType, ProviderConfig};

// ---------------------------------------------------------------------------
// Shared chat model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Msg {
    System { text: String },
    User { text: String },
    /// Assistant turn: optional text plus any tool calls it made.
    Assistant { text: String, tool_calls: Vec<ToolCall> },
    /// Result of one tool call, fed back to the model.
    ToolResult { call_id: String, text: String, is_error: bool },
}

impl Msg {
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Msg serialises")
    }
    pub fn from_json(v: &serde_json::Value) -> Option<Msg> {
        serde_json::from_value(v.clone()).ok()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    /// Fully-qualified name shown to the model (e.g. `filesystem__read_file`).
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub parameters: serde_json::Value,
}

/// Incremental events streamed back by a provider.
#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    /// A tool call started (id + name known).
    ToolCallBegin { index: usize, id: String, name: String },
    /// Fragment of tool-call JSON arguments.
    ToolCallArgsDelta { index: usize, fragment: String },
    Usage { input: Option<u64>, output: Option<u64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    Length,
}

#[derive(Debug, Clone)]
pub struct ChatOptions {
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Reasoning effort requested for this turn (None = provider default).
    pub effort: Option<crate::config::EffortLevel>,
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self { model: String::new(), max_tokens: None, temperature: None, effort: None }
    }
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Stream one assistant turn. Events go to `tx`; the final `StopReason`
    /// is the return value.
    async fn stream_chat(
        &self,
        messages: &[Msg],
        tools: &[ToolDef],
        opts: &ChatOptions,
        tx: mpsc::Sender<ProviderEvent>,
    ) -> anyhow::Result<StopReason>;

    /// Fetch the model list offered by this provider.
    async fn list_models(&self) -> anyhow::Result<Vec<String>>;
}

// ---------------------------------------------------------------------------
// HTTP helpers shared by both adapters
// ---------------------------------------------------------------------------

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("reqwest client")
}

pub(crate) async fn ensure_ok(response: reqwest::Response, provider: &str) -> anyhow::Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let code = status.as_u16();
    let body = response.text().await.unwrap_or_default();
    let hint = match code {
        401 | 403 => " — your API key looks missing or invalid",
        404 => " — check the base URL",
        429 => " — rate limited, wait a moment and retry",
        _ => "",
    };
    // Try to extract an error message from common JSON shapes.
    let detail: String = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| {
                    e.get("message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| Some(e.to_string()))
                })
                .or_else(|| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(String::from)
                })
        })
        .unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.len() > 400 {
                format!("{}…", &trimmed[..400])
            } else if trimmed.is_empty() {
                status.to_string()
            } else {
                trimmed.to_string()
            }
        });
    Err(anyhow::anyhow!("{provider} error {code}{hint}: {detail}"))
}

/// Incremental SSE line extractor: feed raw bytes, get back complete
/// `data:` payloads (without the prefix). `[DONE]` is forwarded as-is.
pub(crate) fn feed_sse(buf: &mut String, chunk: &str, out: &mut Vec<String>) {
    buf.push_str(chunk);
    while let Some(pos) = buf.find('\n') {
        let line: String = buf.drain(..pos + 1).collect();
        let line = line.trim_end_matches(['\n', '\r']);
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim_start();
            if !data.is_empty() {
                out.push(data.to_string());
            }
        }
    }
}

/// Stream an SSE response, invoking `on_data` for every complete `data:`
/// line. The callback returns at most one event per line; events are sent
/// to `tx` in order. `[DONE]` terminates the stream (and is not forwarded).
pub(crate) async fn stream_sse<T, F>(
    response: reqwest::Response,
    tx: &mpsc::Sender<T>,
    mut on_data: F,
) -> anyhow::Result<()>
where
    T: Send,
    F: FnMut(&str) -> anyhow::Result<Option<T>>,
{
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut line_buf = String::new();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        let mut out = Vec::new();
        feed_sse(&mut line_buf, &String::from_utf8_lossy(&bytes), &mut out);
        for data in out {
            if data == "[DONE]" {
                return Ok(());
            }
            if let Some(event) = on_data(&data)? {
                tx.send(event)
                    .await
                    .map_err(|_| anyhow::anyhow!("event receiver dropped"))?;
            }
        }
    }
    Ok(())
}

pub fn build_provider(cfg: &ProviderConfig, api_key: Option<&str>) -> std::sync::Arc<dyn LlmProvider> {
    let key = api_key.map(|s| s.to_string());
    match cfg.api_type {
        ApiType::OpenAi => std::sync::Arc::new(openai::OpenAiProvider {
            base_url: cfg.base_url.clone(),
            api_key: key,
            name: cfg.name.clone(),
            // Z.ai needs `thinking` enabled for `reasoning_effort` to apply
            thinking_toggle: matches!(cfg.kind.as_str(), "zai" | "zai-coding"),
        }),
        ApiType::Anthropic => std::sync::Arc::new(anthropic::AnthropicProvider {
            base_url: cfg.base_url.clone(),
            api_key: key.unwrap_or_default(),
            name: cfg.name.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_feed_handles_split_chunks() {
        let mut buf = String::new();
        let mut out = Vec::new();
        feed_sse(&mut buf, "data: {\"a\":", &mut out);
        assert!(out.is_empty());
        feed_sse(&mut buf, "1}\n\ndata: [DONE]\n\n", &mut out);
        assert_eq!(out, vec!["{\"a\":1}", "[DONE]"]);
        feed_sse(&mut buf, "data: trailing", &mut out);
        assert!(out.len() == 2);
        feed_sse(&mut buf, "\n", &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], "trailing");
    }

    #[test]
    fn msg_serialises_stable() {
        let m = Msg::ToolResult { call_id: "x".into(), text: "hi".into(), is_error: false };
        let v = m.as_json();
        assert_eq!(v["kind"], "tool_result");
        let back = Msg::from_json(&v).unwrap();
        assert_eq!(back, m);
    }
}
