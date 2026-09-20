//! `/compact`: replace a conversation's whole history with one structured
//! summary produced by the conversation's own model. The summary is carried
//! by a plain user message marked with [`COMPACT_MARKER`] so it needs no
//! changes to the persisted message format, survives reloads, and is picked
//! up as `<prior-summary>` by the next compaction.

use std::sync::Arc;

use crate::agent::Agent;
use crate::providers::{ChatOptions, LlmProvider, Msg, ProviderEvent};

/// Marks the user message that carries a compaction summary. Mirrored as
/// `COMPACT_SUMMARY_MARKER` in `src/slashCommands.ts`.
pub const COMPACT_MARKER: &str = "[Conversation compacted]";

const TOOL_RESULT_LIMIT: usize = 1000;
const TOOL_ARGS_LIMIT: usize = 400;

fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(limit).collect();
        t.push_str("…\n[truncated]");
        t
    }
}

/// Flatten history into the text handed to the summariser. The prior summary
/// carrier is skipped — it travels separately as `<prior-summary>`.
pub fn render_transcript(history: &[Msg]) -> String {
    let mut out = String::new();
    for msg in history {
        match msg {
            Msg::System { .. } => {}
            Msg::User { text, .. } => {
                if text.starts_with(COMPACT_MARKER) {
                    continue;
                }
                out.push_str("## User\n\n");
                out.push_str(text);
                out.push_str("\n\n");
            }
            Msg::Assistant { text, tool_calls, .. } => {
                if !text.is_empty() {
                    out.push_str("## Assistant\n\n");
                    out.push_str(text);
                    out.push_str("\n\n");
                }
                for call in tool_calls {
                    let args = serde_json::to_string(&call.arguments).unwrap_or_default();
                    out.push_str(&format!(
                        "## Assistant used tool `{}`\n\n{}\n\n",
                        call.name,
                        truncate(&args, TOOL_ARGS_LIMIT)
                    ));
                }
            }
            Msg::ToolResult {
                call_id,
                text,
                is_error,
            } => {
                out.push_str(&format!(
                    "## Tool result for `{call_id}`{}\n\n{}\n\n",
                    if *is_error { " (error)" } else { "" },
                    truncate(text, TOOL_RESULT_LIMIT)
                ));
            }
        }
    }
    out
}

/// The summary carried by the latest compaction, if the history has one.
pub fn prior_summary(history: &[Msg]) -> Option<String> {
    history.iter().rev().find_map(|m| match m {
        Msg::User { text, .. } if text.starts_with(COMPACT_MARKER) => {
            Some(text[COMPACT_MARKER.len()..].trim().to_string())
        }
        _ => None,
    })
}

/// Build the summariser request (system + single user message).
pub fn build_summarizer_messages(history: &[Msg], instructions: Option<&str>) -> Vec<Msg> {
    let mut user = String::new();
    if let Some(prior) = prior_summary(history) {
        user.push_str("<prior-summary>\n");
        user.push_str(&prior);
        user.push_str(
            "\n</prior-summary>\n\nThis is the summary of the conversation before the \
             <conversation> below. The <conversation> is more recent; anything you do not \
             carry into the new summary is lost.\n\n",
        );
    }
    user.push_str("<conversation>\n");
    user.push_str(&render_transcript(history));
    user.push_str("\n</conversation>");
    if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
        user.push_str(&format!(
            "\n\nThe user asked to focus especially on:\n{extra}"
        ));
    }
    user.push_str(
        "\n\nSummarise the conversation as a compact Markdown document with exactly these \
         sections:\n\n\
         ## Objective\nWhat the user is trying to accomplish.\n\
         ## Important Details\nDecisions, constraints, preferences and corrections the user gave.\n\
         ## Work State\n### Completed\n### Active\n### Blocked\n\
         ## Next Move\nThe most useful next step for the assistant.\n\
         ## Relevant Files\nExact file paths, commands and identifiers that were read or \
         modified.\n\n\
         Keep every section, writing \"(none)\" when empty. Use terse bullets, preserve exact \
         paths and identifiers, do not mention this summary, and respond in the same language \
         as the conversation.",
    );
    vec![
        Msg::System {
            text: "You are a context summarisation agent. You are given a conversation \
                   between a user and an AI coding assistant. Your goal is to produce a \
                   structured summary so another assistant can continue the work with full \
                   context. Do not continue the conversation. Do not respond to any questions \
                   in it. Only output the summary."
                .into(),
        },
        Msg::User { text: user, ts: None },
    ]
}

/// The replacement history: one user message carrying the summary.
pub fn apply(summary: &str) -> Vec<Msg> {
    vec![Msg::User {
        text: format!("{COMPACT_MARKER}\n\n{}", summary.trim()),
        ts: Some(chrono::Utc::now().to_rfc3339()),
    }]
}

/// One provider call that collects the summary (the `stream_title` pattern).
async fn summarize(
    provider: Arc<dyn LlmProvider>,
    options: ChatOptions,
    history: &[Msg],
    instructions: Option<&str>,
) -> Result<String, String> {
    let messages = build_summarizer_messages(history, instructions);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ProviderEvent>(64);
    let call = provider.stream_chat(&messages, &[], &options, tx);
    let mut text = String::new();
    let mut call = std::pin::pin!(call);
    let mut ok = false;
    loop {
        tokio::select! {
            // `biased` with rx polled first is load-bearing: once the call
            // future completes it must never be polled again (that panics
            // with "async fn resumed after completion"). By the time it has
            // returned, its `tx` is dropped, so recv() resolves Ready(None)
            // and breaks the loop before the call branch is ever considered.
            biased;
            ev = rx.recv() => {
                match ev {
                    Some(ProviderEvent::TextDelta(t)) => text.push_str(&t),
                    Some(_) => {}
                    None => break,
                }
            }
            result = &mut call => {
                result.map_err(|e| e.to_string())?;
                ok = true;
            }
        }
    }
    while let Ok(ev) = rx.try_recv() {
        if let ProviderEvent::TextDelta(t) = ev {
            text.push_str(&t);
        }
    }
    if !ok {
        return Err("The provider stream ended unexpectedly".into());
    }
    Ok(text)
}

/// Compact a conversation: replace its history with a summary. Undo records
/// are cleared — the message indexes they refer to no longer exist.
pub async fn run(
    agent: &Agent,
    conversation_id: &str,
    instructions: Option<&str>,
) -> Result<(), String> {
    let (provider_id, model, effort) = {
        let cfg = agent.store.config.lock().unwrap();
        let meta = cfg
            .conversations
            .iter()
            .find(|c| c.id == conversation_id)
            .ok_or("Unknown conversation")?;
        (
            meta.provider_id.clone(),
            meta.model.clone(),
            meta.effort,
        )
    };
    let history: Vec<Msg> = agent
        .store
        .load_conversation(conversation_id)
        .map(|(_, msgs)| msgs.iter().filter_map(Msg::from_json).collect())
        .unwrap_or_default();
    if history.is_empty() {
        return Err("Nothing to compact — the conversation is empty.".into());
    }

    let (provider, default_model, _) = agent.provider_for(&provider_id)?;
    let options = ChatOptions {
        model: if model.is_empty() {
            default_model
        } else {
            model
        },
        max_tokens: None,
        temperature: None,
        effort,
    };
    let summary = summarize(provider, options, &history, instructions).await?;
    if summary.trim().is_empty() {
        return Err("The model returned an empty summary; nothing was compacted.".into());
    }

    let messages: Vec<serde_json::Value> =
        apply(&summary).iter().map(|m| m.as_json()).collect();
    let meta = {
        let mut cfg = agent.store.config.lock().unwrap();
        let meta = cfg
            .conversations
            .iter_mut()
            .find(|c| c.id == conversation_id)
            .ok_or("conversation missing")?;
        meta.updated_at = chrono::Utc::now().to_rfc3339();
        meta.clone()
    };
    agent
        .store
        .save_conversation(&meta, &messages, &[])
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ToolCall;

    fn user(text: &str) -> Msg {
        Msg::User {
            text: text.into(),
            ts: None,
        }
    }

    /// A provider whose future finishes while events are still queued — the
    /// shape that made the un-`biased` select loop re-poll the completed
    /// future and panic with "async fn resumed after completion".
    struct FakeProvider;

    #[async_trait::async_trait]
    impl LlmProvider for FakeProvider {
        async fn stream_chat(
            &self,
            _messages: &[Msg],
            _tools: &[crate::providers::ToolDef],
            _opts: &ChatOptions,
            tx: tokio::sync::mpsc::Sender<ProviderEvent>,
        ) -> anyhow::Result<crate::providers::StopReason> {
            for i in 0..64 {
                tx.send(ProviderEvent::TextDelta(format!("chunk{i} ")))
                    .await
                    .unwrap();
            }
            Ok(crate::providers::StopReason::EndTurn)
        }

        async fn list_models(&self) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn summarize_never_repolls_the_completed_provider_future() {
        let provider: Arc<dyn LlmProvider> = Arc::new(FakeProvider);
        let options = ChatOptions {
            model: "fake".into(),
            max_tokens: None,
            temperature: None,
            effort: None,
        };
        let history = vec![user("hello"), user("and goodbye")];
        let summary = summarize(provider, options, &history, None)
            .await
            .unwrap();
        let expected: String = (0..64).map(|i| format!("chunk{i} ")).collect();
        assert_eq!(summary, expected);
    }

    #[test]
    fn transcript_renders_each_kind() {
        let history = vec![
            user("hello there"),
            Msg::Assistant {
                text: "let me look".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "ducky__fs_read".into(),
                    arguments: serde_json::json!({ "path": "a.txt" }),
                }],
                ts: None,
            },
            Msg::ToolResult {
                call_id: "c1".into(),
                text: "file contents".into(),
                is_error: false,
            },
        ];
        let t = render_transcript(&history);
        assert!(t.contains("## User\n\nhello there"));
        assert!(t.contains("## Assistant\n\nlet me look"));
        assert!(t.contains("ducky__fs_read"));
        assert!(t.contains("## Tool result for `c1`"));
    }

    #[test]
    fn transcript_truncates_long_tool_output_and_args() {
        let long = "x".repeat(5000);
        let history = vec![
            Msg::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "t".into(),
                    arguments: serde_json::json!({ "blob": "y".repeat(3000) }),
                }],
                ts: None,
            },
            Msg::ToolResult {
                call_id: "c1".into(),
                text: long.clone(),
                is_error: true,
            },
        ];
        let t = render_transcript(&history);
        assert!(t.contains("[truncated]"));
        assert!(t.contains("(error)"));
        assert!(!t.contains(&long));
    }

    #[test]
    fn prior_summary_roundtrip_and_reextract() {
        let history = vec![user("old question"), user("assistant asked things")];
        let applied = apply("Objective: fix the bug.");
        assert_eq!(applied.len(), 1);
        let carrier = applied.first().unwrap();
        let Msg::User { text, ts } = carrier else {
            panic!("carrier must be a user message");
        };
        assert!(text.starts_with(COMPACT_MARKER));
        assert!(ts.is_some());
        assert_eq!(
            prior_summary(&[carrier.clone()]).as_deref(),
            Some("Objective: fix the bug.")
        );
        // a re-compact of [carrier, …new messages] finds the prior summary
        let recombined = vec![carrier.clone(), user("what is next?")];
        assert_eq!(
            prior_summary(&recombined).as_deref(),
            Some("Objective: fix the bug.")
        );
    }

    #[test]
    fn summarizer_messages_shape() {
        let carrier = apply("earlier summary").first().unwrap().clone();
        let history = vec![carrier, user("continue the work")];
        let msgs = build_summarizer_messages(&history, Some("focus on auth"));
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0], Msg::System { .. }));
        let Msg::User { text, .. } = &msgs[1] else {
            panic!("expected user message");
        };
        assert!(text.contains("<prior-summary>\nearlier summary\n</prior-summary>"));
        assert!(text.contains("<conversation>"));
        // the prior carrier must not be duplicated inside <conversation>
        let conversation = text.split("<conversation>").nth(1).unwrap();
        assert!(!conversation.contains(COMPACT_MARKER));
        assert!(text.contains("focus especially on:\nfocus on auth"));
        assert!(text.contains("## Relevant Files"));
    }
}
