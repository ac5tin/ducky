//! Unified LLM provider layer.
//!
//! Every provider (OpenAI, Claude, Z.ai, Ollama, OpenRouter, Groq, LM Studio,
//! OpenCode Zen, or any custom endpoint) is normalised to the same
//! `LlmProvider` trait so the chat engine never cares which one is active.

pub mod anthropic;
pub mod openai;
pub mod responses;

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
    System {
        text: String,
    },
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ts: Option<String>,
    },
    /// Assistant turn: optional text plus any tool calls it made.
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ts: Option<String>,
    },
    /// Result of one tool call, fed back to the model.
    ToolResult {
        call_id: String,
        text: String,
        is_error: bool,
    },
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
    ToolCallBegin {
        index: usize,
        id: String,
        name: String,
    },
    /// Fragment of tool-call JSON arguments.
    ToolCallArgsDelta {
        index: usize,
        fragment: String,
    },
    Usage {
        input: Option<u64>,
        output: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    Length,
}

#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Reasoning effort requested for this turn (None = provider default).
    pub effort: Option<crate::config::EffortLevel>,
    /// Stable id for the whole conversation, forwarded only to gateways that
    /// route and cache per session. OpenCode's relay rejects requests without
    /// it (`400 MissingSessionID`). `None` for calls that belong to no
    /// conversation.
    pub session_id: Option<String>,
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

/// User agent sent to providers: OpenCode asks clients to identify themselves
/// rather than present as a generic HTTP library.
pub(crate) const USER_AGENT: &str = concat!("ducky/", env!("CARGO_PKG_VERSION"));

/// Wire protocol one request uses. A gateway may serve different models of one
/// catalogue over different wires, so this is resolved per model rather than
/// per connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    /// OpenAI-compatible `/chat/completions`.
    Chat,
    /// Anthropic `/messages`.
    Messages,
    /// OpenAI `/responses`.
    Responses,
}

impl Wire {
    /// The wire a connection speaks unless the model says otherwise.
    pub fn default_for(api_type: ApiType) -> Self {
        match api_type {
            ApiType::OpenAi => Wire::Chat,
            ApiType::Anthropic => Wire::Messages,
        }
    }
}

/// Gateways that serve one catalogue over more than one wire.
///
/// Everything else stays on the connection's own wire. models.dev publishes a
/// package per model for every provider, so following that map everywhere would
/// move OpenAI, xAI and OpenRouter traffic off `/chat/completions`.
fn serves_several_wires(kind: &str) -> bool {
    matches!(kind, "opencode" | "opencode-go")
}

/// Resolve the wire for one model. `catalog_wire` is the models.dev answer for
/// gateways that publish one. CommandCode has no catalog entry, and serves its
/// Claude models on `/messages` only, which its model ids make recognisable.
pub fn wire_for_model(cfg: &ProviderConfig, model: &str, catalog_wire: Option<Wire>) -> Wire {
    if cfg.kind == "commandcode" && model.trim().to_ascii_lowercase().starts_with("claude-") {
        return Wire::Messages;
    }
    if serves_several_wires(&cfg.kind) {
        if let Some(wire) = catalog_wire {
            return wire;
        }
    }
    Wire::default_for(cfg.api_type)
}

/// Headers a gateway needs in addition to authentication.
///
/// OpenCode's relay rejects a request that carries no session id
/// (`400 MissingSessionID`) and asks clients to identify themselves, so the
/// conversation id travels with the request. It goes only to that host, so no
/// other provider ever sees it; a lookalike host such as
/// `opencode.ai.example.test` does not match.
pub(crate) fn gateway_headers(
    base_url: &str,
    session_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let Some(session) = session_id.filter(|id| !id.is_empty()) else {
        return Vec::new();
    };
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    if !matches!(host.as_deref(), Some(h) if h == "opencode.ai" || h.ends_with(".opencode.ai")) {
        return Vec::new();
    }
    vec![
        ("x-opencode-session", session.to_string()),
        ("x-opencode-client", "ducky".to_string()),
    ]
}

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("reqwest client")
}

pub(crate) async fn ensure_ok(
    response: reqwest::Response,
    provider: &str,
) -> anyhow::Result<reqwest::Response> {
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
                .or_else(|| v.get("message").and_then(|m| m.as_str()).map(String::from))
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

/// Build the provider for the connection's default wire. Used where no model is
/// involved yet: listing models and testing a connection.
pub fn build_provider(
    cfg: &ProviderConfig,
    api_key: Option<&str>,
) -> std::sync::Arc<dyn LlmProvider> {
    build_wired_provider(cfg, api_key, Wire::default_for(cfg.api_type))
}

/// Build the provider for one wire of a connection.
pub fn build_wired_provider(
    cfg: &ProviderConfig,
    api_key: Option<&str>,
    wire: Wire,
) -> std::sync::Arc<dyn LlmProvider> {
    let key = api_key.map(|s| s.to_string());
    match wire {
        Wire::Chat => std::sync::Arc::new(openai::OpenAiProvider {
            base_url: cfg.base_url.clone(),
            api_key: key,
            name: cfg.name.clone(),
            // Z.ai needs `thinking` enabled for `reasoning_effort` to apply
            thinking_toggle: matches!(cfg.kind.as_str(), "zai" | "zai-coding"),
        }),
        Wire::Messages => std::sync::Arc::new(anthropic::AnthropicProvider {
            base_url: cfg.base_url.clone(),
            api_key: key.unwrap_or_default(),
            name: cfg.name.clone(),
            // CommandCode accepts these models only with `Authorization: Bearer`;
            // Anthropic itself and OpenCode read `x-api-key`.
            bearer: cfg.kind == "commandcode",
        }),
        Wire::Responses => std::sync::Arc::new(responses::ResponsesProvider {
            base_url: cfg.base_url.clone(),
            api_key: key,
            name: cfg.name.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wires_resolve_per_model() {
        let cfg = |kind: &str, api_type: ApiType| ProviderConfig {
            id: "p".into(),
            kind: kind.into(),
            name: "P".into(),
            base_url: "https://example.test/v1".into(),
            api_type,
            default_model: None,
            models: Vec::new(),
            created_at: "now".into(),
        };

        // the catalog answer wins when a gateway publishes one
        let go = cfg("opencode-go", ApiType::OpenAi);
        assert_eq!(
            wire_for_model(&go, "grok-4.7", Some(Wire::Responses)),
            Wire::Responses
        );
        assert_eq!(
            wire_for_model(&go, "minimax-m3", Some(Wire::Messages)),
            Wire::Messages
        );
        // no catalog answer: the connection's own wire
        assert_eq!(wire_for_model(&go, "glm-5.3", None), Wire::Chat);
        assert_eq!(wire_for_model(&go, "grok-4.7", None), Wire::Chat);

        // CommandCode serves Claude models on /messages only
        let cc = cfg("commandcode", ApiType::OpenAi);
        assert_eq!(
            wire_for_model(&cc, "claude-sonnet-5", None),
            Wire::Messages
        );
        assert_eq!(wire_for_model(&cc, "Claude-Opus-5", None), Wire::Messages);
        assert_eq!(wire_for_model(&cc, "gpt-6-sol", None), Wire::Chat);

        // only the multi-wire gateways follow the catalog: models.dev declares a
        // package for every provider, and following it here would move every
        // OpenAI, xAI and OpenRouter model off /chat/completions
        let xai = cfg("xai", ApiType::OpenAi);
        assert_eq!(
            wire_for_model(&xai, "grok-4.7", Some(Wire::Responses)),
            Wire::Chat
        );
        let openai = cfg("openai", ApiType::OpenAi);
        assert_eq!(
            wire_for_model(&openai, "gpt-6-sol", Some(Wire::Responses)),
            Wire::Chat
        );
        assert_eq!(
            wire_for_model(&cc, "gpt-6-sol", Some(Wire::Responses)),
            Wire::Chat
        );
        // a Claude model on a gateway with no Anthropic route stays put too
        assert_eq!(
            wire_for_model(&openai, "claude-sonnet-5", None),
            Wire::Chat
        );

        // other gateways keep their connection's wire
        let anthropic = cfg("anthropic", ApiType::Anthropic);
        assert_eq!(
            wire_for_model(&anthropic, "claude-opus-4.7", None),
            Wire::Messages
        );
    }

    #[test]
    fn gateway_headers_only_go_to_opencode() {
        let go = gateway_headers("https://opencode.ai/zen/go/v1", Some("conv-1"));
        assert_eq!(go[0], ("x-opencode-session", "conv-1".to_string()));
        assert_eq!(go[1], ("x-opencode-client", "ducky".to_string()));
        assert!(gateway_headers("https://opencode.ai/zen/v1", Some("c")).len() == 2);
        assert!(gateway_headers("https://api.x.ai/v1", Some("conv-1")).is_empty());
        assert!(gateway_headers("https://api.commandcode.ai/provider/v1", Some("c")).is_empty());
        assert!(gateway_headers("https://opencode.ai.example.test/v1", Some("c")).is_empty());
        assert!(gateway_headers("https://opencode.ai/zen/go/v1", None).is_empty());
        assert!(gateway_headers("https://opencode.ai/zen/go/v1", Some("")).is_empty());
    }

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
        let m = Msg::ToolResult {
            call_id: "x".into(),
            text: "hi".into(),
            is_error: false,
        };
        let v = m.as_json();
        assert_eq!(v["kind"], "tool_result");
        let back = Msg::from_json(&v).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn msg_preserves_ts_on_roundtrip() {
        let v = serde_json::json!({
            "kind": "user",
            "text": "hi",
            "ts": "2026-01-15T12:00:00Z"
        });
        let m = Msg::from_json(&v).expect("user msg");
        assert_eq!(m.as_json()["ts"], "2026-01-15T12:00:00Z");

        let a = serde_json::json!({
            "kind": "assistant",
            "text": "ok",
            "tool_calls": [],
            "ts": "2026-01-15T12:00:01Z"
        });
        let m = Msg::from_json(&a).expect("assistant msg");
        assert_eq!(m.as_json()["ts"], "2026-01-15T12:00:01Z");
    }

    #[test]
    fn msg_old_json_without_ts_still_loads() {
        let v = serde_json::json!({"kind": "user", "text": "hi"});
        let m = Msg::from_json(&v).expect("legacy user msg");
        assert!(m.as_json().get("ts").is_none());
    }
}
