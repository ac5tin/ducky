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
use crate::config::{
    AgentMode, ApprovalMode, ConversationMeta, EffortLevel, ProviderConfig, Store, SubagentConfig,
};
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

/// What the mock saw in one request: the tools offered and the options used,
/// so tests can assert on the system prompt, tool allowlist and overrides.
#[derive(Clone, Debug)]
struct CapturedRound {
    tool_names: Vec<String>,
    system: String,
    model: String,
    effort: Option<String>,
}

static CAPTURES: LazyLock<Mutex<HashMap<String, Vec<CapturedRound>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn captures_for(key: &str) -> Vec<CapturedRound> {
    CAPTURES
        .lock()
        .unwrap()
        .get(key)
        .cloned()
        .unwrap_or_default()
}

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
        tools: &[ToolDef],
        opts: &ChatOptions,
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
        CAPTURES
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default()
            .push(CapturedRound {
                tool_names: tools.iter().map(|t| t.name.clone()).collect(),
                system: messages
                    .iter()
                    .find_map(|m| match m {
                        Msg::System { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
                model: opts.model.clone(),
                effort: opts.effort.map(|e| e.as_str().to_string()),
            });
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
    let (agent, sink, store, _dir) = test_agent_full(conversation_id, AgentMode::Default, false);
    (agent, sink, store)
}

/// An agent whose conversation starts in the given mode.
fn test_agent_in_mode(
    conversation_id: &str,
    mode: AgentMode,
) -> (Arc<Agent>, Arc<CollectingSink>, Arc<Store>) {
    let (agent, sink, store, _dir) = test_agent_full(conversation_id, mode, false);
    (agent, sink, store)
}

/// An agent whose working directory is its own tempdir and that returns that
/// directory, so tests can prove whether a file was actually written.
fn test_agent_in_dir(
    conversation_id: &str,
    mode: AgentMode,
) -> (Arc<Agent>, Arc<CollectingSink>, Arc<Store>, std::path::PathBuf) {
    test_agent_full(conversation_id, mode, true)
}

fn test_agent_full(
    conversation_id: &str,
    mode: AgentMode,
    own_working_dir: bool,
) -> (
    Arc<Agent>,
    Arc<CollectingSink>,
    Arc<Store>,
    std::path::PathBuf,
) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let store = Arc::new(Store::new(tmp.path(), dir.clone()).unwrap());
    // keep the tempdir alive by leaking in test scope
    std::mem::forget(tmp);
    {
        let mut cfg = store.config.lock().unwrap();
        cfg.settings.tool_approval = ApprovalMode::AutoApproveAll;
        if own_working_dir {
            cfg.settings.working_dir = Some(dir.to_string_lossy().into_owned());
        }
        let now = chrono::Utc::now().to_rfc3339();
        cfg.conversations.push(ConversationMeta {
            id: conversation_id.to_string(),
            title: "test".into(),
            provider_id: "mock".into(),
            model: "mock-model".into(),
            effort: None,
            mcp_ids: None,
            mode,
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
    (agent, sink, store, dir)
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

/// The running card's subagent meta (the first `running` card with meta).
fn sink_meta(sink: &CollectingSink) -> crate::events::SubagentMeta {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            BackendEvent::ToolCallUpdate {
                status,
                subagent: Some(m),
                ..
            } if status == "running" => Some(m.clone()),
            _ => None,
        })
        .expect("running card carries subagent meta")
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
    // the running card tells the user what the subagent inherits
    let meta = sink_meta(&sink);
    assert_eq!(meta.provider_id, "mock");
    assert_eq!(meta.model, "mock-model");
    // an untyped spawn resolves to the seeded General-Purpose definition
    assert_eq!(meta.agent.as_deref(), Some("General-Purpose"));
    let sub = captures_for("t1-sub");
    assert_eq!(sub.len(), 1);
    assert!(sub[0].system.contains("You are \"General-Purpose\""));
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

#[tokio::test]
async fn agent_type_applies_persona_and_tool_allowlist() {
    script(
        "t6-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t6-sub", "agent": "Explore"}),
            )]),
            MockRound::Text("main done".into()),
        ],
    );
    script("t6-sub", vec![MockRound::Text("explore answer".into())]);

    let (agent, sink, store) = test_agent("t6-conv");
    run(&agent, "t6-conv", "t6-main", &CancellationToken::new()).await;

    assert_eq!(
        tool_results(&store, "t6-conv"),
        vec!["explore answer".to_string()]
    );
    // the Explore run's request carried its persona and only its tools
    let sub = captures_for("t6-sub");
    assert_eq!(sub.len(), 1);
    assert!(sub[0].system.contains("# Your role"));
    assert!(sub[0].system.contains("You are \"Explore\""));
    assert!(sub[0].system.contains("read-only search agent"));
    assert!(sub[0].tool_names.contains(&"ducky__fs_read".to_string()));
    assert!(sub[0].tool_names.contains(&"ducky__web_search".to_string()));
    assert!(!sub[0].tool_names.contains(&"ducky__fs_write".to_string()));
    assert!(!sub[0].tool_names.contains(&"ducky__fs_mkdir".to_string()));
    assert!(!sub[0].tool_names.contains(&SUBAGENT.to_string()));
    // the running card names the resolved type
    let meta = sink_meta(&sink);
    assert_eq!(meta.agent.as_deref(), Some("Explore"));
    // the main run stays unfiltered
    let main = captures_for("t6-main");
    assert_eq!(main.len(), 2);
    assert!(main[0].tool_names.contains(&"ducky__fs_write".to_string()));
    assert!(main[0].tool_names.contains(&SUBAGENT.to_string()));
    assert!(!main[0].system.contains("# Your role"));
}

#[tokio::test]
async fn agent_type_model_and_effort_overrides_apply() {
    script(
        "t7-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t7-sub", "agent": "Fast"}),
            )]),
            MockRound::Text("main done".into()),
        ],
    );
    script("t7-sub", vec![MockRound::Text("fast answer".into())]);

    let (agent, sink, _store) = test_agent("t7-conv");
    {
        let mut cfg = agent.store.config.lock().unwrap();
        cfg.providers.push(ProviderConfig {
            id: "mock".into(),
            kind: "custom".into(),
            name: "Mock".into(),
            base_url: "http://localhost".into(),
            api_type: crate::config::ApiType::OpenAi,
            default_model: None,
            models: vec!["mock-model".into(), "mock-fast".into()],
            created_at: "t".into(),
        });
        cfg.subagents.push(SubagentConfig {
            id: "fast".into(),
            name: "Fast".into(),
            description: "quick helper".into(),
            system_prompt: "Be quick.".into(),
            provider_id: Some("mock".into()),
            model: Some("mock-fast".into()),
            effort: Some(EffortLevel::High),
            tools: None,
            created_at: "t".into(),
        });
    }
    run(&agent, "t7-conv", "t7-main", &CancellationToken::new()).await;

    // the running card reflects the resolved overrides
    let meta = sink_meta(&sink);
    assert_eq!(meta.provider_id, "mock");
    assert_eq!(meta.model, "mock-fast");
    assert_eq!(meta.effort.as_deref(), Some("high"));
    assert_eq!(meta.agent.as_deref(), Some("Fast"));
    // the subagent's request actually used them
    let sub = captures_for("t7-sub");
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0].model, "mock-fast");
    assert_eq!(sub[0].effort.as_deref(), Some("high"));
    assert!(sub[0].system.contains("Be quick."));
}

#[tokio::test]
async fn unknown_agent_type_fails_fast() {
    script(
        "t8-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t8-sub", "agent": "Nope"}),
            )]),
            MockRound::Text("main done".into()),
        ],
    );
    script("t8-sub", vec![MockRound::Text("must never run".into())]);

    let (agent, sink, store) = test_agent("t8-conv");
    run(&agent, "t8-conv", "t8-main", &CancellationToken::new()).await;

    assert!(tool_cards(&sink).iter().any(|(s, _t, r, _parent)| {
        s == "error" && r.as_deref().is_some_and(|t| t.contains("unknown subagent type"))
    }));
    assert_eq!(
        tool_results(&store, "t8-conv"),
        vec![
            "Error: unknown subagent type \"Nope\". Available types: General-Purpose, Explore."
                .to_string()
        ]
    );
    assert!(!subagent_text(&sink).contains("must never run"));
    assert!(chat_text(&sink).contains("main done"));
}

#[tokio::test]
async fn restricted_subagent_cannot_spawn_subagents() {
    script(
        "t9-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t9-sub", "agent": "Explore"}),
            )]),
            MockRound::Text("main done".into()),
        ],
    );
    // the Explore subagent tries to delegate anyway
    script(
        "t9-sub",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t9-inner"}),
            )]),
            MockRound::Text("explored anyway".into()),
        ],
    );
    script("t9-inner", vec![MockRound::Text("must never run".into())]);

    let (agent, sink, store) = test_agent("t9-conv");
    run(&agent, "t9-conv", "t9-main", &CancellationToken::new()).await;

    // the nested spawn is rejected with an error card attributed to the parent
    assert!(tool_cards(&sink).iter().any(|(s, _, r, parent)| {
        s == "error"
            && *parent
            && r.as_deref()
                .is_some_and(|t| t.contains("does not have access to the subagent tool"))
    }));
    assert!(!subagent_text(&sink).contains("must never run"));
    // the Explore run still finishes with its own answer
    assert_eq!(
        tool_results(&store, "t9-conv"),
        vec!["explored anyway".to_string()]
    );
}

#[tokio::test]
async fn untyped_spawn_without_definitions_stays_generic() {
    script(
        "t10-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t10-sub"}),
            )]),
            MockRound::Text("main done".into()),
        ],
    );
    script("t10-sub", vec![MockRound::Text("generic answer".into())]);

    let (agent, sink, store) = test_agent("t10-conv");
    {
        let mut cfg = agent.store.config.lock().unwrap();
        cfg.subagents.clear();
    }
    run(&agent, "t10-conv", "t10-main", &CancellationToken::new()).await;

    assert_eq!(
        tool_results(&store, "t10-conv"),
        vec!["generic answer".to_string()]
    );
    // no definitions at all: no resolved type, no role block — base generic
    let meta = sink_meta(&sink);
    assert_eq!(meta.agent, None);
    let sub = captures_for("t10-sub");
    assert_eq!(sub.len(), 1);
    assert!(!sub[0].system.contains("# Your role"));
}

#[tokio::test]
async fn subagent_internal_builtin_tool_completes() {
    // the path the live QA exercised: a subagent running ordinary built-in
    // tools round after round must keep making progress and finish
    script(
        "t13-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t13-sub"}),
            )]),
            MockRound::Text("main done".into()),
        ],
    );
    script(
        "t13-sub",
        vec![
            MockRound::Tools(vec![
                ("ducky__fs_list".into(), serde_json::json!({})),
                ("ducky__fs_list".into(), serde_json::json!({})),
            ]),
            MockRound::Text("listed everything".into()),
        ],
    );

    let (agent, sink, store) = test_agent("t13-conv");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run(&agent, "t13-conv", "t13-main", &CancellationToken::new()),
    )
    .await
    .expect("subagent with internal tool calls must not hang");

    assert_eq!(
        tool_results(&store, "t13-conv"),
        vec!["listed everything".to_string()]
    );
    // the internal calls surface as activity updates on the parent card
    assert!(tool_cards(&sink)
        .iter()
        .any(|(s, _, _, parent)| s == "running" && *parent));
    assert!(tool_cards(&sink)
        .iter()
        .any(|(s, _, _, parent)| s == "done" && *parent));
}

#[tokio::test]
async fn stalled_subagent_stream_fails_loudly() {
    // a subagent whose provider stream goes silent must error out (idle
    // watchdog) instead of leaving the turn "Working…" forever
    script(
        "t14-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t14-sub"}),
            )]),
            MockRound::Text("main recovered".into()),
        ],
    );
    script("t14-sub", vec![MockRound::Hang]);

    let (agent, sink, store) = test_agent("t14-conv");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run(&agent, "t14-conv", "t14-main", &CancellationToken::new()),
    )
    .await
    .expect("the idle watchdog must fire instead of hanging");

    // the subagent's stall surfaces as an error on its own card (parentless —
    // it IS the subagent card) and an error result, and the main agent
    // still finishes its turn
    assert!(tool_cards(&sink).iter().any(|(s, _, r, parent)| {
        s == "error" && !parent && r.as_deref().is_some_and(|t| t.contains("stream stalled"))
    }));
    assert_eq!(
        tool_results(&store, "t14-conv").len(),
        1,
        "the stalled subagent's error is its single tool result"
    );
    assert!(chat_text(&sink).contains("main recovered"));
    assert!(sink
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, BackendEvent::MessageDone { .. })));
}

#[tokio::test]
async fn main_system_prompt_applies_to_main_run_only() {
    script(
        "t11-main",
        vec![
            MockRound::Tools(vec![(
                SUBAGENT.into(),
                serde_json::json!({"task": "t11-sub"}),
            )]),
            MockRound::Text("main done".into()),
        ],
    );
    script("t11-sub", vec![MockRound::Text("sub answer".into())]);

    let (agent, _sink, store) = test_agent("t11-conv");
    store
        .config
        .lock()
        .unwrap()
        .settings
        .system_prompt = "Always answer in haiku.".into();
    run(&agent, "t11-conv", "t11-main", &CancellationToken::new()).await;

    // the main run's system message carries the custom prompt after the grounding
    let main = captures_for("t11-main");
    assert_eq!(main.len(), 2);
    assert!(main[0].system.contains("Working directory"));
    assert!(main[0].system.contains("Always answer in haiku."));
    assert!(
        main[0]
            .system
            .find("Working directory")
            .unwrap()
            < main[0].system.find("Always answer in haiku.").unwrap()
    );
    // the subagent run keeps its own persona flow, without the main prompt
    let sub = captures_for("t11-sub");
    assert_eq!(sub.len(), 1);
    assert!(sub[0].system.contains("Working directory"));
    assert!(!sub[0].system.contains("Always answer in haiku."));
}

// ---------------------------------------------------------------------------
// Mode predicate
// ---------------------------------------------------------------------------

#[test]
fn mode_allows_matrix() {
    use crate::agent::mode_allows;

    // Default and Auto keep every tool available
    for name in ["ducky__fs_write", "ducky__fs_read", "srv__search"] {
        assert!(mode_allows(AgentMode::Default, name, false), "{name} in Default");
        assert!(mode_allows(AgentMode::Auto, name, false), "{name} in Auto");
    }

    // ReadOnly and Plan keep read-only tools and drop the rest
    for mode in [AgentMode::ReadOnly, AgentMode::Plan] {
        assert!(mode_allows(mode, "ducky__fs_read", true), "read in {mode:?}");
        assert!(mode_allows(mode, "srv__search", true), "mcp read in {mode:?}");
        assert!(!mode_allows(mode, "ducky__fs_write", false), "write in {mode:?}");
        // an absent readOnlyHint is not read-only (MCP spec default)
        assert!(!mode_allows(mode, "srv__write", false), "unannotated mcp in {mode:?}");
    }

    // the subagent tool stays available: its children read this same mode
    for mode in [AgentMode::Default, AgentMode::ReadOnly, AgentMode::Plan, AgentMode::Auto] {
        assert!(mode_allows(mode, "ducky__subagent", false), "subagent in {mode:?}");
    }

    // control tools only where they mean something
    assert!(mode_allows(AgentMode::Auto, "ducky__set_mode", true));
    assert!(!mode_allows(AgentMode::Default, "ducky__set_mode", true));
    assert!(!mode_allows(AgentMode::Plan, "ducky__set_mode", true));
    assert!(!mode_allows(AgentMode::ReadOnly, "ducky__set_mode", true));
    assert!(mode_allows(AgentMode::Plan, "ducky__present_plan", true));
    assert!(!mode_allows(AgentMode::Default, "ducky__present_plan", true));
    assert!(!mode_allows(AgentMode::Auto, "ducky__present_plan", true));
}

#[test]
fn mode_denial_names_the_mode_and_the_way_out() {
    use crate::agent::mode_denial;
    let read_only = mode_denial(AgentMode::ReadOnly, "ducky__fs_write");
    assert!(read_only.contains("read-only mode is active"), "{read_only}");
    let plan = mode_denial(AgentMode::Plan, "ducky__fs_write");
    assert!(plan.contains("ducky__present_plan"), "{plan}");
    assert!(mode_denial(AgentMode::Plan, "ducky__set_mode").contains("auto mode"));
    assert!(mode_denial(AgentMode::Default, "ducky__present_plan").contains("plan mode"));
}

// ---------------------------------------------------------------------------
// Mode filtering of the offered tool list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_only_mode_hides_mutating_tools() {
    script("m1-main", vec![MockRound::Text("done".into())]);
    let (agent, _sink, _store, _dir) = test_agent_in_dir("m1-conv", AgentMode::ReadOnly);
    run(&agent, "m1-conv", "m1-main", &CancellationToken::new()).await;

    let names = captures_for("m1-main")[0].tool_names.clone();
    assert!(names.contains(&"ducky__fs_read".to_string()), "{names:?}");
    assert!(names.contains(&"ducky__fs_search".to_string()), "{names:?}");
    // the subagent tool stays: its children inherit this mode
    assert!(names.contains(&SUBAGENT.to_string()), "{names:?}");
    assert!(!names.contains(&"ducky__fs_write".to_string()), "{names:?}");
    assert!(!names.contains(&"ducky__fs_mkdir".to_string()), "{names:?}");
    assert!(!names.contains(&"ducky__set_mode".to_string()), "{names:?}");
    assert!(!names.contains(&"ducky__present_plan".to_string()), "{names:?}");
}

#[tokio::test]
async fn plan_mode_offers_the_plan_tool_only() {
    script("m2-main", vec![MockRound::Text("done".into())]);
    let (agent, _sink, _store) = test_agent_in_mode("m2-conv", AgentMode::Plan);
    run(&agent, "m2-conv", "m2-main", &CancellationToken::new()).await;

    let names = captures_for("m2-main")[0].tool_names.clone();
    assert!(names.contains(&"ducky__present_plan".to_string()), "{names:?}");
    assert!(!names.contains(&"ducky__fs_write".to_string()), "{names:?}");
    assert!(!names.contains(&"ducky__set_mode".to_string()), "{names:?}");
}

#[tokio::test]
async fn auto_mode_offers_the_mode_tool_and_writes() {
    script("m3b-main", vec![MockRound::Text("done".into())]);
    let (agent, _sink, _store) = test_agent_in_mode("m3b-conv", AgentMode::Auto);
    run(&agent, "m3b-conv", "m3b-main", &CancellationToken::new()).await;

    let names = captures_for("m3b-main")[0].tool_names.clone();
    assert!(names.contains(&"ducky__set_mode".to_string()), "{names:?}");
    assert!(names.contains(&"ducky__fs_write".to_string()), "{names:?}");
    assert!(!names.contains(&"ducky__present_plan".to_string()), "{names:?}");
}

#[tokio::test]
async fn effective_mode_falls_back_to_the_app_default() {
    let (agent, _sink, store) = test_agent("m4-conv");
    assert_eq!(agent.effective_mode("m4-conv"), AgentMode::Default);
    store.config.lock().unwrap().settings.default_mode = AgentMode::Auto;
    // an unknown conversation uses the app default
    assert_eq!(agent.effective_mode("nope"), AgentMode::Auto);
}

// ---------------------------------------------------------------------------
// Mode gate on execution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_only_mode_refuses_a_forced_write() {
    script(
        "m5-main",
        vec![
            MockRound::Tools(vec![(
                "ducky__fs_write".into(),
                serde_json::json!({ "path": "forced.txt", "content": "nope" }),
            )]),
            MockRound::Text("stopped".into()),
        ],
    );
    let (agent, sink, store, dir) = test_agent_in_dir("m5-conv", AgentMode::ReadOnly);
    run(&agent, "m5-conv", "m5-main", &CancellationToken::new()).await;

    // the file was never created, and the model was told why
    assert!(!dir.join("forced.txt").exists());
    let results = tool_results(&store, "m5-conv");
    assert_eq!(results.len(), 1);
    assert!(
        results[0].contains("read-only mode is active"),
        "{}",
        results[0]
    );
    // it is an error card, and the refusal happened before any consent prompt:
    // no pending_approval card was emitted for it
    assert!(tool_cards(&sink)
        .iter()
        .any(|(s, t, _, _)| s == "error" && t.as_deref() == Some("ducky__fs_write")));
    assert!(!tool_cards(&sink)
        .iter()
        .any(|(s, t, _, _)| s == "pending_approval" && t.as_deref() == Some("ducky__fs_write")));
}

#[tokio::test]
async fn default_mode_allows_the_same_write() {
    // control for the test above: without the mode gate the write goes through,
    // so the read-only test fails for the right reason
    script(
        "m6-main",
        vec![
            MockRound::Tools(vec![(
                "ducky__fs_write".into(),
                serde_json::json!({ "path": "allowed.txt", "content": "yes" }),
            )]),
            MockRound::Text("done".into()),
        ],
    );
    let (agent, _sink, _store, dir) = test_agent_in_dir("m6-conv", AgentMode::Default);
    run(&agent, "m6-conv", "m6-main", &CancellationToken::new()).await;

    assert!(dir.join("allowed.txt").exists());
}
