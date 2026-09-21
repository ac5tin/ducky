//! End-to-end tests for the agent chat loop, driven by a scripted
//! `MockProvider` (see `provider_override` in `agent.rs`). Scripts are keyed
//! by the last user message text — the subagent task or the main prompt —
//! with unique keys per test since the script store is process-global.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::config::{ApprovalMode, ConversationMeta, Store};
use crate::events::{BackendEvent, CollectingSink};
use crate::mcp::bridge::InteractiveBridge;
use crate::mcp::manager::McpManager;
use crate::providers::{ChatOptions, LlmProvider, Msg, ProviderEvent, StopReason, ToolDef};

const SUBAGENT: &str = "ducky__subagent";

// ---------------------------------------------------------------------------
// Scripted provider
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum MockRound {
    /// End the turn with this text.
    Text(String),
    /// Emit tool calls, then the caller scripts the next round.
    Tools(Vec<(String, serde_json::Value)>),
    /// Wait until every party reached the barrier, then end with this text
    /// (used to prove concurrent execution: sequential runs would deadlock).
    Gated(Arc<tokio::sync::Barrier>, String),
    /// Never resolve (cancellation probe).
    Hang,
}

type ScriptMap = Arc<Mutex<HashMap<String, VecDeque<MockRound>>>>;

static SCRIPTS: LazyLock<ScriptMap> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

/// The provider `Agent::provider_for` returns in test builds.
pub(crate) fn provider_override() -> Option<Arc<dyn LlmProvider>> {
    Some(Arc::new(MockProvider))
}

struct MockProvider;

fn script(key: &str, rounds: Vec<MockRound>) {
    SCRIPTS
        .lock()
        .unwrap()
        .insert(key.to_string(), rounds.into());
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn stream_chat(
        &self,
        messages: &[Msg],
        _tools: &[ToolDef],
        _opts: &ChatOptions,
        tx: mpsc::Sender<ProviderEvent>,
    ) -> anyhow::Result<StopReason> {
        let key = messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::User { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let round = SCRIPTS
            .lock()
            .unwrap()
            .get_mut(&key)
            .and_then(|q| q.pop_front());
        match round {
            Some(MockRound::Text(t)) => {
                tx.send(ProviderEvent::TextDelta(t)).await.ok();
                Ok(StopReason::EndTurn)
            }
            Some(MockRound::Gated(barrier, t)) => {
                barrier.wait().await;
                tx.send(ProviderEvent::TextDelta(t)).await.ok();
                Ok(StopReason::EndTurn)
            }
            Some(MockRound::Tools(calls)) => {
                for (i, (name, args)) in calls.iter().enumerate() {
                    tx.send(ProviderEvent::ToolCallBegin {
                        index: i,
                        id: format!("call_{i}_{name}"),
                        name: name.clone(),
                    })
                    .await
                    .ok();
                    tx.send(ProviderEvent::ToolCallArgsDelta {
                        index: i,
                        fragment: args.to_string(),
                    })
                    .await
                    .ok();
                }
                Ok(StopReason::ToolUse)
            }
            Some(MockRound::Hang) => {
                std::future::pending::<()>().await;
                unreachable!("pending never resolves")
            }
            None => {
                tx.send(ProviderEvent::TextDelta(format!("no script for {key:?}")))
                    .await
                    .ok();
                Ok(StopReason::EndTurn)
            }
        }
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["mock-model".into()])
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn test_agent(conversation_id: &str) -> (Arc<Agent>, Arc<CollectingSink>, Arc<Store>) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(tmp.path(), tmp.path().to_path_buf()).unwrap());
    // keep the tempdir alive by leaking in test scope
    std::mem::forget(tmp);
    {
        let mut cfg = store.config.lock().unwrap();
        cfg.settings.tool_approval = ApprovalMode::AutoApproveAll;
        let now = chrono::Utc::now().to_rfc3339();
        cfg.conversations.push(ConversationMeta {
            id: conversation_id.to_string(),
            title: "test".into(),
            provider_id: "mock".into(),
            model: "mock-model".into(),
            effort: None,
            mcp_ids: None,
            created_at: now.clone(),
            updated_at: now,
        });
    }
    let sink = Arc::new(CollectingSink::default());
    let bridge = Arc::new(InteractiveBridge::new(sink.clone(), store.clone()));
    let manager = Arc::new(McpManager::new(store.clone(), bridge.clone(), sink.clone()));
    let agent = Arc::new(Agent {
        store: store.clone(),
        manager,
        bridge,
        sink: sink.clone(),
    });
    (agent, sink, store)
}

async fn run(agent: &Agent, conversation_id: &str, prompt: &str, ct: &CancellationToken) {
    agent
        .run_turn(
            conversation_id.to_string(),
            "mock".into(),
            "mock-model".into(),
            Vec::new(),
            prompt.to_string(),
            ct.clone(),
        )
        .await;
}

fn tool_results(store: &Store, conversation_id: &str) -> Vec<String> {
    let (_, messages) = store.load_conversation(conversation_id).unwrap();
    messages
        .iter()
        .filter(|m| m["kind"] == "tool_result")
        .map(|m| m["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn subagent_text(sink: &CollectingSink) -> String {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            BackendEvent::SubagentDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn chat_text(sink: &CollectingSink) -> String {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            BackendEvent::ChatDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// (status, tool, result_text, parent_tool_call_id) for every tool card.
fn tool_cards(sink: &CollectingSink) -> Vec<(String, Option<String>, Option<String>, bool)> {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            BackendEvent::ToolCallUpdate {
                status,
                tool,
                result_text,
                parent_tool_call_id,
                ..
            } => Some((
                status.clone(),
                tool.clone(),
                result_text.clone(),
                parent_tool_call_id.is_some(),
            )),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawns_subagent_and_returns_final_answer() {
    script(
        "t1-main",
        vec![
            MockRound::Tools(vec![(SUBAGENT.into(), serde_json::json!({"task": "t1-sub"}))]),
            MockRound::Text("all done".into()),
        ],
    );
    script("t1-sub", vec![MockRound::Text("sub answer".into())]);

    let (agent, sink, store) = test_agent("t1-conv");
    run(&agent, "t1-conv", "t1-main", &CancellationToken::new()).await;

    // the subagent's final answer is the tool result; the main answer streams
    assert_eq!(tool_results(&store, "t1-conv"), vec!["sub answer".to_string()]);
    assert!(chat_text(&sink).contains("all done"));
    assert_eq!(subagent_text(&sink), "sub answer");
    assert!(tool_cards(&sink).iter().any(|(s, t, r, parent)| {
        s == "done" && r.as_deref() == Some("sub answer") && !parent
    }));
    assert!(tool_cards(&sink).iter().any(|(s, t, _, parent)| {
        s == "pending_approval" && t.as_deref() == Some(SUBAGENT) && !parent
    }));
    assert!(sink
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, BackendEvent::MessageDone { .. })));
}

#[tokio::test]
async fn parallel_subagents_run_concurrently() {
    // both subagents must overlap or the barrier deadlocks past the timeout
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    script(
        "t2-main",
        vec![
            MockRound::Tools(vec![
                (SUBAGENT.into(), serde_json::json!({"task": "t2-a"})),
                (SUBAGENT.into(), serde_json::json!({"task": "t2-b"})),
            ]),
            MockRound::Text("parallel done".into()),
        ],
    );
    script("t2-a", vec![MockRound::Gated(barrier.clone(), "A result".into())]);
    script("t2-b", vec![MockRound::Gated(barrier.clone(), "B result".into())]);

    let (agent, sink, store) = test_agent("t2-conv");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run(&agent, "t2-conv", "t2-main", &CancellationToken::new()),
    )
    .await
    .expect("subagents did not run concurrently (barrier timeout)");

    // results land in call order even though both ran at once
    assert_eq!(
        tool_results(&store, "t2-conv"),
        vec!["A result".to_string(), "B result".to_string()]
    );
    assert!(chat_text(&sink).contains("parallel done"));
}

#[tokio::test]
async fn subagents_nest_and_internal_cards_carry_parent() {
    script(
        "t3-main",
        vec![
            MockRound::Tools(vec![(SUBAGENT.into(), serde_json::json!({"task": "t3-outer"}))]),
            MockRound::Text("main done".into()),
        ],
    );
    script(
        "t3-outer",
        vec![
            MockRound::Tools(vec![(SUBAGENT.into(), serde_json::json!({"task": "t3-inner"}))]),
            MockRound::Text("outer finished".into()),
        ],
    );
    script("t3-inner", vec![MockRound::Text("inner answer".into())]);

    let (agent, sink, store) = test_agent("t3-conv");
    run(&agent, "t3-conv", "t3-main", &CancellationToken::new()).await;

    // nested transcript streams; only the outer result reaches the file
    assert!(subagent_text(&sink).contains("inner answer"));
    assert_eq!(
        tool_results(&store, "t3-conv"),
        vec!["outer finished".to_string()]
    );
    // the inner spawn's card is attributed to the outer subagent's call
    assert!(tool_cards(&sink).iter().any(|(s, t, _, parent)| {
        s == "pending_approval" && t.as_deref() == Some(SUBAGENT) && *parent
    }));
}

#[tokio::test]
async fn nesting_depth_is_capped() {
    script(
        "t4-main",
        vec![
            MockRound::Tools(vec![(SUBAGENT.into(), serde_json::json!({"task": "t4-d1"}))]),
            MockRound::Text("main done".into()),
        ],
    );
    for depth in 1..=3 {
        let next = format!("t4-d{}", depth + 1);
        script(
            &format!("t4-d{depth}"),
            vec![
                MockRound::Tools(vec![(SUBAGENT.into(), serde_json::json!({"task": next}))]),
                MockRound::Text(format!("d{depth} done")),
            ],
        );
    }
    script("t4-d4", vec![MockRound::Text("must never run".into())]);

    let (agent, sink, store) = test_agent("t4-conv");
    run(&agent, "t4-conv", "t4-main", &CancellationToken::new()).await;

    // the depth-3 subagent got an error result instead of spawning d4
    assert!(tool_cards(&sink).iter().any(|(s, _, r, _)| {
        s == "error" && r.as_deref().is_some_and(|t| t.contains("nesting limit"))
    }));
    assert!(subagent_text(&sink).contains("d3 done"));
    assert!(!subagent_text(&sink).contains("must never run"));
    // the chain unwinds back to the main agent
    assert!(chat_text(&sink).contains("main done"));
    assert_eq!(
        tool_results(&store, "t4-conv"),
        vec!["d1 done".to_string()]
    );
}

#[tokio::test]
async fn cancelling_the_turn_cancels_subagents() {
    script(
        "t5-main",
        vec![MockRound::Tools(vec![(
            SUBAGENT.into(),
            serde_json::json!({"task": "t5-hang"}),
        )])],
    );
    script("t5-hang", vec![MockRound::Hang]);

    let (agent, sink, _store) = test_agent("t5-conv");
    let ct = CancellationToken::new();
    let task_ct = ct.clone();
    let task_agent = agent.clone();
    let handle = tokio::spawn(async move {
        task_agent
            .run_turn(
                "t5-conv".into(),
                "mock".into(),
                "mock-model".into(),
                Vec::new(),
                "t5-main".into(),
                task_ct,
            )
            .await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    ct.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("cancel did not propagate to the subagent")
        .unwrap();

    let events = sink.events.lock().unwrap().clone();
    assert!(events
        .iter()
        .any(|e| matches!(e, BackendEvent::MessageDone { .. })));
    assert!(!events
        .iter()
        .any(|e| matches!(e, BackendEvent::ChatError { .. })));
    assert!(tool_cards(&sink)
        .iter()
        .any(|(s, _, r, _)| s == "error" && r.as_deref() == Some("cancelled by user")));
}
