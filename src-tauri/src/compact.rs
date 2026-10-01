//! `/compact`: replace a conversation's whole history with one structured
//! summary produced by the conversation's own model. The summary is carried
//! by a plain user message marked with [`COMPACT_MARKER`] so it needs no
//! changes to the persisted message format, survives reloads, and is picked
//! up as `<prior-summary>` by the next compaction.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::Agent;
use crate::providers::{ChatOptions, LlmProvider, Msg};

/// Marks the user message that carries a compaction summary. Mirrored as
/// `COMPACT_SUMMARY_MARKER` in `src/slashCommands.ts`.
pub const COMPACT_MARKER: &str = "[Conversation compacted]";

/// Brief-transcript caps, in tokens unless the name says chars. Same shape as
/// pi-blackhole's `brief.ts`: one line per message, clipped.
const USER_TOKENS: usize = 256;
const ASSISTANT_TOKENS: usize = 200;
const ERROR_CHARS: usize = 150;
const BRIEF_LINES: usize = 120;
/// Rough characters per token, used to turn the token caps into char caps.
const CHARS_PER_TOKEN: usize = 4;
const MAX_MODIFIED: usize = 20;
const MAX_READ: usize = 10;
const MAX_PREFERENCES: usize = 10;
const MAX_UNRESOLVED: usize = 5;

/// What a tool call did to the workspace, from its name alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Write,
    Read,
    Ignore,
}

/// Tool names that write, then tool names that read. First match wins, so the
/// write verbs are listed first (`edit_file` is a write, not a read). Matched
/// against the last `__`-separated segment, so MCP servers count too.
const WRITE_VERBS: [&str; 10] = [
    "write", "edit", "create", "mkdir", "delete", "remove", "move", "rename", "patch", "apply",
];
const READ_VERBS: [&str; 12] = [
    "read", "get", "list", "search", "fetch", "find", "view", "grep", "glob", "query", "inspect",
    "describe",
];

/// The deterministic half of a summary: exact paths and verbatim user wording,
/// never paraphrased by a model.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct Extracted {
    pub modified: Vec<String>,
    pub read: Vec<String>,
    pub preferences: Vec<String>,
    pub unresolved: Vec<String>,
}

/// Keys whose first non-empty string value names a file. Checked in order,
/// because MCP servers disagree on the spelling.
const PATH_KEYS: [&str; 9] = [
    "path",
    "file_path",
    "filepath",
    "file",
    "filename",
    "dir",
    "directory",
    "source",
    "destination",
];
/// Extra keys used for the one-liner in the brief transcript.
const BRIEF_KEYS: [&str; 6] = ["pattern", "query", "url", "command", "name", "id"];

/// Phrases that mark a sentence as a standing user preference.
const PREFERENCE_MARKERS: [&str; 6] = [
    "prefer",
    "don't want",
    "do not want",
    "always use",
    "never use",
    "please use",
];

pub fn tool_effect(name: &str) -> Effect {
    // the last `__` segment carries the verb, so MCP names work too
    let segment = name.rsplit("__").next().unwrap_or(name).to_ascii_lowercase();
    if WRITE_VERBS.iter().any(|v| segment.contains(v)) {
        Effect::Write
    } else if READ_VERBS.iter().any(|v| segment.contains(v)) {
        Effect::Read
    } else {
        Effect::Ignore
    }
}

/// Collapse a message to one line: whitespace runs become single spaces.
fn clip_flat(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        flat
    } else {
        let mut out: String = flat.chars().take(limit).collect();
        out.push('…');
        out
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn first_string(args: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let obj = args.as_object()?;
    keys.iter().find_map(|key| {
        obj.get(*key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// Keep the newest `n` entries; older ones are dropped from the front.
fn cap(v: &mut Vec<String>, n: usize) {
    if v.len() > n {
        let drop = v.len() - n;
        v.drain(..drop);
    }
}

fn push_unique(v: &mut Vec<String>, item: String) {
    if !item.is_empty() && !v.contains(&item) {
        v.push(item);
    }
}

fn trim_caps(ex: &mut Extracted) {
    cap(&mut ex.modified, MAX_MODIFIED);
    cap(&mut ex.read, MAX_READ);
    cap(&mut ex.preferences, MAX_PREFERENCES);
    cap(&mut ex.unresolved, MAX_UNRESOLVED);
}

/// The first sentence of a user message that reads as a standing preference.
fn first_preference(text: &str) -> Option<String> {
    for sentence in text.split(['.', '!', '\n', ';']) {
        let s = sentence.trim();
        let len = s.chars().count();
        if !(5..=200).contains(&len) || s.ends_with('?') {
            continue;
        }
        let lower = s.to_ascii_lowercase();
        if PREFERENCE_MARKERS.iter().any(|m| lower.contains(m)) {
            return Some(s.to_string());
        }
    }
    None
}

/// Pull the `- item` lines of one `## <header>` block out of a summary.
fn section_items(text: &str, header: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim_end() == header {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        if let Some(item) = line.trim().strip_prefix("- ") {
            if item != "(none)" {
                out.push(item.to_string());
            }
        }
    }
    out
}

/// Deterministic pass over one window of history. No model is involved, so
/// the paths and the user's own wording survive unchanged.
pub fn extract(history: &[Msg]) -> Extracted {
    let mut ex = Extracted::default();
    // tool name and identifying argument per call id, so a result can be
    // matched back to the call that produced it
    let mut calls: HashMap<&str, (&str, String)> = HashMap::new();
    // (tool, key argument, message) for errors not yet resolved
    let mut errors: Vec<(String, String, String)> = Vec::new();
    for msg in history {
        match msg {
            Msg::User { text, .. } => {
                if text.starts_with(COMPACT_MARKER) {
                    continue;
                }
                if let Some(p) = first_preference(text) {
                    push_unique(&mut ex.preferences, p);
                }
            }
            Msg::Assistant { tool_calls, .. } => {
                for call in tool_calls {
                    let key = first_string(&call.arguments, &PATH_KEYS).unwrap_or_default();
                    calls.insert(call.id.as_str(), (call.name.as_str(), key.clone()));
                    let path = (!key.is_empty()).then_some(key);
                    match (tool_effect(&call.name), path) {
                        (Effect::Write, Some(p)) => push_unique(&mut ex.modified, p),
                        (Effect::Read, Some(p)) => push_unique(&mut ex.read, p),
                        _ => {}
                    }
                }
            }
            Msg::ToolResult {
                call_id,
                text,
                is_error,
            } => {
                let (tool, key) = calls
                    .get(call_id.as_str())
                    .map(|(n, k)| (n.to_string(), k.clone()))
                    .unwrap_or_else(|| (call_id.clone(), String::new()));
                if *is_error {
                    let line = first_line(text);
                    if !line.is_empty() {
                        errors.push((tool, key, line));
                    }
                } else {
                    // a later success on the same tool and argument clears it
                    errors.retain(|(t, k, _)| !(t == &tool && k == &key));
                }
            }
            Msg::System { .. } => {}
        }
    }
    // a path that was written is not also reported as merely read
    ex.read.retain(|p| !ex.modified.contains(p));
    ex.unresolved = errors.into_iter().map(|(_, _, line)| line).collect();
    trim_caps(&mut ex);
    ex
}

/// Carry the deterministic sections of an earlier summary into this one, so a
/// second compaction does not throw away older files and preferences.
pub fn merge_prior(extracted: &mut Extracted, prior: &str) {
    for item in section_items(prior, "## Files And Changes") {
        if let Some(path) = item.strip_prefix("modified: ") {
            push_unique(&mut extracted.modified, path.to_string());
        } else if let Some(path) = item.strip_prefix("read: ") {
            push_unique(&mut extracted.read, path.to_string());
        }
    }
    for item in section_items(prior, "## User Preferences") {
        push_unique(&mut extracted.preferences, item);
    }
    for item in section_items(prior, "## Outstanding Context") {
        push_unique(&mut extracted.unresolved, item);
    }
    extracted.read.retain(|p| !extracted.modified.contains(p));
    trim_caps(extracted);
}

/// One line per message, clipped, with consecutive identical tool calls
/// collapsed and only the tail kept. This is the compressed evidence the
/// summariser reasons over.
pub fn render_brief(history: &[Msg]) -> String {
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    for msg in history {
        match msg {
            Msg::User { text, .. } => {
                if text.starts_with(COMPACT_MARKER) {
                    continue;
                }
                push_collapsed(
                    &mut lines,
                    format!("[user] {}", clip_flat(text, USER_TOKENS * CHARS_PER_TOKEN)),
                );
            }
            Msg::Assistant {
                text, tool_calls, ..
            } => {
                if !text.trim().is_empty() {
                    push_collapsed(
                        &mut lines,
                        format!(
                            "[assistant] {}",
                            clip_flat(text, ASSISTANT_TOKENS * CHARS_PER_TOKEN)
                        ),
                    );
                }
                for call in tool_calls {
                    names.insert(call.id.as_str(), call.name.as_str());
                    let arg = first_string(&call.arguments, &PATH_KEYS)
                        .or_else(|| first_string(&call.arguments, &BRIEF_KEYS));
                    let line = match arg {
                        Some(a) => format!("* {} \"{}\"", call.name, clip_flat(&a, 80)),
                        None => format!("* {}", call.name),
                    };
                    push_collapsed(&mut lines, line);
                }
            }
            Msg::ToolResult {
                call_id,
                text,
                is_error,
            } => {
                if !is_error {
                    continue;
                }
                let tool = names.get(call_id.as_str()).copied().unwrap_or("tool");
                push_collapsed(
                    &mut lines,
                    format!("[tool_error] {tool}: {}", clip_flat(text, ERROR_CHARS)),
                );
            }
            Msg::System { .. } => {}
        }
    }
    if lines.len() > BRIEF_LINES {
        let drop = lines.len() - BRIEF_LINES;
        lines.drain(..drop);
    }
    lines.join("\n")
}

/// Split a `… xN` repeat suffix, so the counter can be incremented.
fn split_repeat(line: &str) -> (&str, usize) {
    if let Some((base, tail)) = line.rsplit_once(" x") {
        if !base.is_empty() && tail.parse::<usize>().is_ok() {
            return (base, tail.parse().unwrap_or(1));
        }
    }
    (line, 1)
}

fn push_collapsed(lines: &mut Vec<String>, line: String) {
    if let Some(last) = lines.last_mut() {
        let (base, count) = split_repeat(last);
        if base == line {
            *last = format!("{base} x{}", count + 1);
            return;
        }
    }
    lines.push(line);
}

/// Index of the newest real user message. The cut drops everything before it
/// and keeps it plus everything after, so the request the user just made
/// never disappears into a summary. `None` = nothing worth compacting.
pub fn cut_index(history: &[Msg]) -> Option<usize> {
    let cut = history.iter().rposition(
        |m| matches!(m, Msg::User { text, .. } if !text.starts_with(COMPACT_MARKER)),
    )?;
    (cut > 0).then_some(cut)
}

/// The replacement history: the carrier plus the kept tail.
pub fn apply_with_tail(history: &[Msg], summary: &str) -> Vec<Msg> {
    let mut out = history.to_vec();
    install(&mut out, summary);
    out
}

fn file_lines(ex: &Extracted) -> Vec<String> {
    let mut out: Vec<String> = ex.modified.iter().map(|p| format!("modified: {p}")).collect();
    out.extend(ex.read.iter().map(|p| format!("read: {p}")));
    out
}

fn push_items(out: &mut String, items: &[String]) {
    if items.is_empty() {
        out.push_str("(none)\n");
        return;
    }
    for item in items {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
}

/// The blocks the model is never allowed to write, because it could drop a
/// path or reword a correction.
fn deterministic_block(ex: &Extracted) -> String {
    let mut out = String::new();
    out.push_str("## Files And Changes\n");
    push_items(&mut out, &file_lines(ex));
    out.push_str("\n## User Preferences\n");
    push_items(&mut out, &ex.preferences);
    out.push_str("\n## Outstanding Context\n");
    push_items(&mut out, &ex.unresolved);
    out
}

/// Final summary text: the model's semantic sections, then the deterministic
/// blocks, then the brief transcript.
pub fn assemble(semantic: &str, extracted: &Extracted, brief: &str) -> String {
    let mut out = String::new();
    out.push_str(semantic.trim());
    out.push_str("\n\n");
    out.push_str(&deterministic_block(extracted));
    if !brief.trim().is_empty() {
        out.push_str("\n## Transcript\n");
        out.push_str(brief.trim());
        out.push('\n');
    }
    out
}

/// What a compaction needs from a history: where the cut falls, the
/// deterministic blocks, the previous summary, and the compressed evidence
/// the model reasons over.
pub struct Window {
    pub cut: usize,
    pub extracted: Extracted,
    pub prior: Option<String>,
    pub brief: String,
}

/// Split a history for compaction. `None` when everything is newer than the
/// newest user turn, so there is nothing to drop.
pub fn window(history: &[Msg]) -> Option<Window> {
    let cut = cut_index(history)?;
    let dropped = &history[..cut];
    let prior = prior_summary(dropped);
    let mut extracted = extract(dropped);
    if let Some(p) = &prior {
        merge_prior(&mut extracted, p);
    }
    Some(Window {
        cut,
        extracted,
        prior,
        brief: render_brief(dropped),
    })
}

/// Install a finished summary: the carrier plus everything from the newest
/// user turn on. No-op when there is nothing to compact.
pub fn install(history: &mut Vec<Msg>, summary: &str) {
    if let Some(cut) = cut_index(history) {
        let mut next = apply(summary);
        next.extend_from_slice(&history[cut..]);
        *history = next;
    }
}

/// Whether the loop should compact before the next model call. Pure, so every
/// guard is testable without a provider or a catalog.
pub fn auto_compact_due(
    enabled: bool,
    ratio: f64,
    limit: Option<u64>,
    used: Option<u64>,
    already_compacted_this_turn: bool,
) -> bool {
    if !enabled || already_compacted_this_turn {
        return false;
    }
    // no known window: never guess one
    let Some(limit) = limit.filter(|l| *l > 0) else {
        return false;
    };
    let Some(used) = used else {
        return false;
    };
    used as f64 >= limit as f64 * ratio
}

/// Rough token count for a history, about four characters per token. Used
/// until the provider reports real usage for a conversation.
pub fn estimate_tokens(history: &[Msg]) -> u64 {
    let chars: usize = history
        .iter()
        .map(|m| match m {
            Msg::System { text } | Msg::User { text, .. } => text.chars().count(),
            Msg::Assistant {
                text, tool_calls, ..
            } => {
                text.chars().count()
                    + tool_calls
                        .iter()
                        .map(|c| c.name.chars().count() + c.arguments.to_string().len())
                        .sum::<usize>()
            }
            Msg::ToolResult { text, .. } => text.chars().count(),
        })
        .sum();
    (chars / CHARS_PER_TOKEN) as u64
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

/// Build the summariser request (system + single user message). The model
/// writes only the four semantic sections: the deterministic blocks are
/// appended afterwards by [`assemble`], so it cannot drop a path.
pub fn build_summarizer_messages(window: &Window, instructions: Option<&str>) -> Vec<Msg> {
    let mut user = String::new();
    if let Some(prior) = &window.prior {
        user.push_str("<prior-summary>\n");
        user.push_str(prior);
        user.push_str(
            "\n</prior-summary>\n\nThis is the summary of the conversation before the \
             <conversation> below. The <conversation> is more recent; anything you do not \
             carry into the new summary is lost.\n\n",
        );
    }
    user.push_str("<conversation>\n");
    user.push_str(&window.brief);
    user.push_str("\n</conversation>\n");
    user.push_str(
        "\nAlready extracted from the conversation and preserved verbatim; do not repeat it:\n\n",
    );
    user.push_str(&deterministic_block(&window.extracted));
    if let Some(extra) = instructions.filter(|s| !s.trim().is_empty()) {
        user.push_str(&format!("\nThe user asked to focus especially on:\n{extra}\n"));
    }
    user.push_str(
        "\nSummarise the conversation as a compact Markdown document with exactly these \
         sections:\n\n\
         ## Objective\nWhat the user is trying to accomplish, including any change of plan.\n\
         ## Decisions and Constraints\nDecisions made, constraints, and corrections the user gave.\n\
         ## Work State\n### Completed\n### Active\n### Blocked\n\
         ## Next Move\nThe most useful next step for the assistant.\n\n\
         Keep every section, writing \"(none)\" when empty. Use terse bullets, preserve exact \
         paths, identifiers and commands, do not invent anything that is not in the \
         conversation, do not mention this summary, and respond in the same language as the \
         conversation.",
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
        Msg::User {
            text: user,
            ts: None,
        },
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
    window: &Window,
    instructions: Option<&str>,
) -> Result<String, String> {
let messages = build_summarizer_messages(window, instructions);
    crate::providers::collect_stream_text(provider, &messages, &[], &options).await
}

/// Summarise one dropped window with the conversation's own model and return
/// the assembled summary text. Shared by `/compact` and the auto trigger.
pub async fn summarize_window(
    agent: &Agent,
    conversation_id: &str,
    window: &Window,
    instructions: Option<&str>,
) -> Result<String, String> {
    let (provider_id, model, effort) = {
        let cfg = agent.store.config.lock().unwrap();
        let meta = cfg
            .conversations
            .iter()
            .find(|c| c.id == conversation_id)
            .ok_or("Unknown conversation")?;
        (meta.provider_id.clone(), meta.model.clone(), meta.effort)
    };
    let (provider, default_model, _) = agent.provider_for(&provider_id, &model).await?;
    let options = ChatOptions {
        model: if model.is_empty() {
            default_model
        } else {
            model
        },
        max_tokens: None,
        temperature: None,
        effort,
        session_id: Some(conversation_id.to_string()),
    };
    let semantic = summarize(provider, options, window, instructions).await?;
    if semantic.trim().is_empty() {
        return Err("The model returned an empty summary; nothing was compacted.".into());
    }
    Ok(assemble(&semantic, &window.extracted, &window.brief))
}

/// Compact a conversation: replace its history with a summary. Undo records
/// are cleared — the message indexes they refer to no longer exist.
pub async fn run(
    agent: &Agent,
    conversation_id: &str,
    instructions: Option<&str>,
) -> Result<(), String> {
    let history: Vec<Msg> = agent
        .store
        .load_conversation(conversation_id)
        .map(|(_, msgs)| msgs.iter().filter_map(Msg::from_json).collect())
        .unwrap_or_default();
    if history.is_empty() {
        return Err("Nothing to compact — the conversation is empty.".into());
    }
    let window = window(&history)
        .ok_or("Nothing to compact yet — send another message first.")?;
    let summary = summarize_window(agent, conversation_id, &window, instructions).await?;
    let messages: Vec<serde_json::Value> = apply_with_tail(&history, &summary)
        .iter()
        .map(|m| m.as_json())
        .collect();
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
    use crate::providers::{ProviderEvent, ToolCall};

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
            session_id: None,
        };
        let history = vec![user("hello"), user("and goodbye")];
        let w = window(&history).unwrap();
        let summary = summarize(provider, options, &w, None).await.unwrap();
        let expected: String = (0..64).map(|i| format!("chunk{i} ")).collect();
        assert_eq!(summary, expected);
    }

    #[test]
    fn brief_records_the_kinds_it_sees() {
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
        let t = render_brief(&history);
        assert!(t.contains("[user] hello there"));
        assert!(t.contains("[assistant] let me look"));
        assert!(t.contains("* ducky__fs_read \"a.txt\""));
        // a successful tool result is compressed away: the files section and
        // the model's own sections carry that state instead
        assert!(!t.contains("file contents"));
    }

    #[test]
    fn brief_truncates_long_tool_errors() {
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
        let t = render_brief(&history);
        assert!(t.contains("[tool_error] t: "), "got: {t}");
        assert!(!t.contains(&long));
        assert!(t.len() <= ERROR_CHARS + 64, "got {} chars", t.len());
    }

    #[test]
    fn prior_summary_roundtrip_and_reextract() {
        let applied = apply("Objective: fix the bug.");
        assert_eq!(applied.len(), 1);
        let carrier = applied.first().unwrap();
        let Msg::User { text, ts } = carrier else {
            panic!("carrier must be a user message");
        };
        assert!(text.starts_with(COMPACT_MARKER));
        assert!(ts.is_some());
        assert_eq!(
            prior_summary(std::slice::from_ref(carrier)).as_deref(),
            Some("Objective: fix the bug.")
        );
        // a re-compact of [carrier, …new messages] finds the prior summary
        let recombined = vec![carrier.clone(), user("what is next?")];
        assert_eq!(
            prior_summary(&recombined).as_deref(),
            Some("Objective: fix the bug.")
        );
    }

    fn assistant(text: &str) -> Msg {
        Msg::Assistant {
            text: text.into(),
            tool_calls: Vec::new(),
            ts: None,
        }
    }

    fn tool_call(id: &str, name: &str, args: serde_json::Value) -> Msg {
        Msg::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args,
            }],
            ts: None,
        }
    }

    fn tool_result(id: &str, text: &str, is_error: bool) -> Msg {
        Msg::ToolResult {
            call_id: id.into(),
            text: text.into(),
            is_error,
        }
    }

    #[test]
    fn cut_keeps_the_newest_user_turn() {
        let history = vec![
            user("first ask"),
            assistant("first answer"),
            user("second ask"),
            assistant("second answer"),
        ];
        assert_eq!(cut_index(&history), Some(2));
        let applied = apply_with_tail(&history, "SUMMARY");
        assert_eq!(applied.len(), 3);
        assert!(matches!(&applied[0], Msg::User { text, .. } if text.starts_with(COMPACT_MARKER)));
        assert!(matches!(&applied[1], Msg::User { text, .. } if text == "second ask"));
        assert!(matches!(&applied[2], Msg::Assistant { text, .. } if text == "second answer"));
    }

    #[test]
    fn cut_is_none_without_a_second_user_turn() {
        assert_eq!(cut_index(&[user("only ask")]), None);
        assert_eq!(cut_index(&[user("only ask"), assistant("answer")]), None);
        assert_eq!(
            cut_index(&[
                user("only ask"),
                assistant("answer"),
                tool_result("c1", "out", false)
            ]),
            None
        );
    }

    #[test]
    fn cut_does_not_count_the_carrier_as_a_user_turn() {
        let mut history = apply("older summary");
        history.push(user("new ask"));
        history.push(assistant("new answer"));
        assert_eq!(cut_index(&history), Some(1));
        let applied = apply_with_tail(&history, "NEW");
        assert_eq!(applied.len(), 3);
        assert!(matches!(&applied[1], Msg::User { text, .. } if text == "new ask"));
        assert!(matches!(&applied[2], Msg::Assistant { text, .. } if text == "new answer"));
    }

    #[test]
    fn cut_is_none_when_the_carrier_is_the_newest_user_message() {
        assert_eq!(cut_index(&apply("older summary")), None);
    }

    #[test]
    fn tool_effect_maps_ducky_and_mcp_names() {
        assert_eq!(tool_effect("ducky__fs_write"), Effect::Write);
        assert_eq!(tool_effect("ducky__fs_mkdir"), Effect::Write);
        assert_eq!(tool_effect("filesystem__write_file"), Effect::Write);
        assert_eq!(tool_effect("filesystem__edit_file"), Effect::Write);
        assert_eq!(tool_effect("github__create_or_update_file"), Effect::Write);
        assert_eq!(tool_effect("ducky__fs_read"), Effect::Read);
        assert_eq!(tool_effect("ducky__fs_list"), Effect::Read);
        assert_eq!(tool_effect("ducky__fs_search"), Effect::Read);
        assert_eq!(tool_effect("ducky__web_fetch"), Effect::Read);
        assert_eq!(tool_effect("ducky__read_session_context"), Effect::Read);
        assert_eq!(tool_effect("github__get_file_contents"), Effect::Read);
        assert_eq!(tool_effect("ducky__present_plan"), Effect::Ignore);
        assert_eq!(tool_effect("ducky__subagent"), Effect::Ignore);
    }

    #[test]
    fn extract_collects_paths_preferences_and_unresolved_errors() {
        let history = vec![
            user("please always use pnpm, not npm"),
            tool_call(
                "c1",
                "ducky__fs_write",
                serde_json::json!({ "path": "src/store.ts" }),
            ),
            tool_result("c1", "ok", false),
            tool_call(
                "c2",
                "filesystem__read_file",
                serde_json::json!({ "file_path": "src/api.ts" }),
            ),
            tool_result("c2", "contents", false),
            tool_call(
                "c3",
                "ducky__fs_write",
                serde_json::json!({ "path": "src/ghost.ts" }),
            ),
            tool_result("c3", "ENOENT: no such file", true),
        ];
        let ex = extract(&history);
        assert_eq!(ex.modified, vec!["src/store.ts", "src/ghost.ts"]);
        assert_eq!(ex.read, vec!["src/api.ts"]);
        assert_eq!(ex.preferences.len(), 1);
        assert!(ex.preferences[0].contains("always use pnpm"));
        assert_eq!(ex.unresolved.len(), 1);
        assert!(ex.unresolved[0].contains("ENOENT"));
    }

    #[test]
    fn extract_prefers_modified_over_read_for_the_same_path() {
        let history = vec![
            tool_call(
                "c1",
                "ducky__fs_read",
                serde_json::json!({ "path": "src/store.ts" }),
            ),
            tool_result("c1", "contents", false),
            tool_call(
                "c2",
                "ducky__fs_write",
                serde_json::json!({ "path": "src/store.ts" }),
            ),
            tool_result("c2", "ok", false),
        ];
        let ex = extract(&history);
        assert_eq!(ex.modified, vec!["src/store.ts"]);
        assert!(ex.read.is_empty());
    }

    #[test]
    fn extract_drops_errors_a_later_success_resolves() {
        let history = vec![
            tool_call(
                "c1",
                "ducky__fs_read",
                serde_json::json!({ "path": "a.ts" }),
            ),
            tool_result("c1", "boom", true),
            tool_call(
                "c2",
                "ducky__fs_read",
                serde_json::json!({ "path": "a.ts" }),
            ),
            tool_result("c2", "ok", false),
        ];
        assert!(extract(&history).unresolved.is_empty());
    }

    #[test]
    fn brief_collapses_consecutive_identical_tool_calls() {
        let history = vec![
            tool_call(
                "c1",
                "ducky__fs_read",
                serde_json::json!({ "path": "a.ts" }),
            ),
            tool_call(
                "c2",
                "ducky__fs_read",
                serde_json::json!({ "path": "a.ts" }),
            ),
            tool_call(
                "c3",
                "ducky__fs_read",
                serde_json::json!({ "path": "a.ts" }),
            ),
        ];
        let brief = render_brief(&history);
        assert!(brief.contains("ducky__fs_read \"a.ts\" x3"), "got: {brief}");
    }

    #[test]
    fn brief_clips_a_long_user_message_to_its_token_budget() {
        let long = "x".repeat(50_000);
        let brief = render_brief(&[user(&long)]);
        assert!(brief.starts_with("[user] "), "got: {brief}");
        assert!(
            brief.len() <= USER_TOKENS * CHARS_PER_TOKEN + 64,
            "got {} chars",
            brief.len()
        );
        assert!(brief.ends_with('…'), "got: {brief}");
    }

    #[test]
    fn brief_keeps_only_the_last_lines() {
        let mut history = vec![user("the very first ask")];
        for i in 0..200 {
            history.push(user(&format!("line {i}")));
        }
        let brief = render_brief(&history);
        assert!(brief.lines().count() <= BRIEF_LINES, "{} lines", brief.lines().count());
        assert!(brief.contains("line 199"));
        assert!(!brief.contains("the very first ask"));
    }

    #[test]
    fn brief_skips_the_carrier_and_records_tool_errors() {
        let mut history = apply("older summary");
        history.push(tool_call(
            "c9",
            "ducky__fs_write",
            serde_json::json!({ "path": "a.ts" }),
        ));
        history.push(tool_result("c9", "ENOENT: no such file", true));
        let brief = render_brief(&history);
        assert!(!brief.contains(COMPACT_MARKER));
        assert!(brief.contains("[tool_error] ducky__fs_write: ENOENT"), "got: {brief}");
    }

    #[test]
    fn assemble_appends_deterministic_sections_after_the_model_text() {
        let ex = Extracted {
            modified: vec!["src/store.ts".into()],
            read: vec!["src/api.ts".into()],
            preferences: vec!["always use pnpm".into()],
            unresolved: vec!["ENOENT: no such file".into()],
        };
        let out = assemble("## Objective\nShip it.", &ex, "[user] hello");
        let objective = out.find("## Objective").unwrap();
        let files = out.find("## Files And Changes").unwrap();
        assert!(objective < files);
        assert!(out.contains("- modified: src/store.ts"));
        assert!(out.contains("- read: src/api.ts"));
        assert!(out.contains("- always use pnpm"));
        assert!(out.contains("- ENOENT: no such file"));
        assert!(out.contains("[user] hello"));
        assert!(!out.contains("## Commits"));
    }

    #[test]
    fn assemble_writes_none_for_empty_sections() {
        let out = assemble("## Objective\n(none)", &Extracted::default(), "");
        assert!(out.contains("## Files And Changes\n(none)"), "got: {out}");
        assert!(out.contains("## User Preferences\n(none)"), "got: {out}");
        assert!(out.contains("## Outstanding Context\n(none)"), "got: {out}");
    }

    #[test]
    fn merge_prior_keeps_files_and_preferences_from_an_earlier_summary() {
        let prior = "## Objective\nold\n\n## Files And Changes\n- modified: src/old.ts\n\n\
                     ## User Preferences\n- always use pnpm\n\n## Outstanding Context\n- boom\n";
        let mut ex = Extracted {
            modified: vec!["src/new.ts".into()],
            ..Default::default()
        };
        merge_prior(&mut ex, prior);
        assert_eq!(ex.modified, vec!["src/new.ts", "src/old.ts"]);
        assert_eq!(ex.preferences, vec!["always use pnpm"]);
        assert_eq!(ex.unresolved, vec!["boom"]);
    }

    #[test]
    fn window_merges_the_prior_summary_from_the_carrier() {
        let mut history = apply("## Files And Changes\n- modified: src/old.ts\n");
        history.push(user("new ask"));
        history.push(assistant("new answer"));
        history.push(user("newest ask"));
        let w = window(&history).expect("compactable");
        assert_eq!(w.cut, 3);
        assert_eq!(w.prior.as_deref(), Some("## Files And Changes\n- modified: src/old.ts"));
        assert!(w.extracted.modified.contains(&"src/old.ts".to_string()));
        assert!(w.brief.contains("[user] new ask"), "got: {}", w.brief);
    }

    #[test]
    fn window_is_none_when_there_is_nothing_older() {
        assert!(window(&[user("only ask")]).is_none());
        assert!(window(&apply("older summary")).is_none());
    }

    #[test]
    fn install_replaces_the_dropped_window_and_keeps_the_tail() {
        let mut next = vec![user("old ask"), assistant("old answer"), user("newest ask")];
        install(&mut next, "SUMMARY");
        assert_eq!(next.len(), 2);
        assert!(matches!(&next[0], Msg::User { text, .. } if text.starts_with(COMPACT_MARKER)));
        assert!(matches!(&next[1], Msg::User { text, .. } if text == "newest ask"));
    }

    #[test]
    fn auto_compact_fires_at_the_threshold() {
        assert!(auto_compact_due(true, 0.8, Some(100_000), Some(80_000), false));
        assert!(auto_compact_due(true, 0.8, Some(100_000), Some(95_000), false));
        assert!(!auto_compact_due(true, 0.8, Some(100_000), Some(79_999), false));
    }

    #[test]
    fn auto_compact_stays_off_when_disabled_unknown_or_already_done() {
        // switched off in Settings
        assert!(!auto_compact_due(false, 0.8, Some(100_000), Some(95_000), false));
        // the model is not in the catalog, so its window is unknown
        assert!(!auto_compact_due(true, 0.8, None, Some(95_000), false));
        // a zero window would otherwise fire on every round trip
        assert!(!auto_compact_due(true, 0.8, Some(0), Some(95_000), false));
        // nothing reported yet and no history to estimate from
        assert!(!auto_compact_due(true, 0.8, Some(100_000), None, false));
        // one compaction per user turn
        assert!(!auto_compact_due(true, 0.8, Some(100_000), Some(95_000), true));
    }

    #[test]
    fn estimate_tokens_counts_text_and_tool_output() {
        let history = vec![
            user(&"a".repeat(400)),
            tool_result("c1", &"b".repeat(800), false),
        ];
        assert_eq!(estimate_tokens(&history), 300);
    }

    #[test]
    fn summarizer_messages_shape() {
        let mut history = apply("earlier summary");
        history.push(tool_call(
            "c1",
            "ducky__fs_write",
            serde_json::json!({ "path": "src/store.ts" }),
        ));
        history.push(user("continue the work"));
        history.push(user("what is next?"));
        let w = window(&history).unwrap();
        let msgs = build_summarizer_messages(&w, Some("focus on auth"));
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
        // the deterministic blocks are handed over already preserved
        assert!(text.contains("- modified: src/store.ts"));
        assert!(text.contains("## Next Move"));
        assert!(!text.contains("## Relevant Files"));
    }
}
