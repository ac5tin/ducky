//! Tauri commands — the whole backend API surface the webview talks to.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::agent::ConversationRuntime;
use crate::config::{
    self, AppConfig, ConversationMeta, McpServerConfig, ProviderConfig, Store, SubagentConfig,
    Theme, ToolRule, UndoRecord,
};
use crate::mcp::bridge::{ApprovalDecision, PlanDecision};
use crate::providers::Msg;
use crate::state::AppState;

fn uuid() -> String {
    Store::new_id()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Bootstrap & config
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Bootstrap {
    pub config: AppConfig,
    pub server_summaries: Vec<crate::mcp::manager::ServerSummary>,
    pub presets: &'static [config::ProviderPreset],
    pub suggestions: Vec<SuggestionView>,
    /// The machine's home directory; the default working directory.
    pub home_dir: String,
}

#[derive(Serialize)]
pub struct SuggestionView {
    pub name: String,
    pub description: String,
    pub command: String,
    pub args: Vec<String>,
}

#[tauri::command]
pub fn get_bootstrap(state: State<'_, Arc<AppState>>) -> Bootstrap {
    let cfg = state.store.config.lock().unwrap().clone();
    Bootstrap {
        config: cfg,
        server_summaries: state.manager.summaries(),
        presets: config::PROVIDER_PRESETS,
        suggestions: config::CONNECTOR_SUGGESTIONS
            .iter()
            .map(|s| SuggestionView {
                name: s.name.to_string(),
                description: s.description.to_string(),
                command: s.command.to_string(),
                args: s.args.iter().map(|a| a.to_string()).collect(),
            })
            .collect(),
        home_dir: state.store.home_dir.to_string_lossy().into_owned(),
    }
}

#[tauri::command]
pub fn get_config(state: State<'_, Arc<AppState>>) -> AppConfig {
    state.store.config.lock().unwrap().clone()
}

// ---------------------------------------------------------------------------
// Subagents
// ---------------------------------------------------------------------------

/// Trim user-typed free text and drop blank-but-present overrides so they
/// behave as "inherit".
fn normalize_subagent(def: SubagentConfig) -> SubagentConfig {
    SubagentConfig {
        name: def.name.trim().to_string(),
        description: def.description.trim().to_string(),
        provider_id: def
            .provider_id
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty()),
        model: def
            .model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty()),
        ..def
    }
}

#[tauri::command]
pub fn subagent_add(
    state: State<'_, Arc<AppState>>,
    def: SubagentConfig,
) -> Result<SubagentConfig, String> {
    let def = normalize_subagent(def);
    let created = {
        let mut c = state.store.config.lock().unwrap();
        let def = SubagentConfig {
            id: uuid(),
            created_at: now(),
            ..def
        };
        config::validate_subagent(&def, &c.providers, &c.subagents)?;
        c.subagents.push(def.clone());
        def
    };
    state.store.save_config().map_err(|e| e.to_string())?;
    Ok(created)
}

#[tauri::command]
pub fn subagent_update(
    state: State<'_, Arc<AppState>>,
    def: SubagentConfig,
) -> Result<SubagentConfig, String> {
    let def = normalize_subagent(def);
    let updated = {
        let mut c = state.store.config.lock().unwrap();
        let Some(existing) = c.subagents.iter().find(|s| s.id == def.id) else {
            return Err("Unknown subagent".into());
        };
        let others: Vec<SubagentConfig> = c
            .subagents
            .iter()
            .filter(|s| s.id != def.id)
            .cloned()
            .collect();
        let def = SubagentConfig {
            created_at: existing.created_at.clone(),
            ..def
        };
        config::validate_subagent(&def, &c.providers, &others)?;
        *c.subagents.iter_mut().find(|s| s.id == def.id).unwrap() = def.clone();
        def
    };
    state.store.save_config().map_err(|e| e.to_string())?;
    Ok(updated)
}

#[tauri::command]
pub fn subagent_remove(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        c.subagents.retain(|s| s.id != id);
    }
    state.store.save_config().map_err(|e| e.to_string())
}

/// Re-insert any default subagent (General-Purpose, Explore) whose name is
/// missing. Existing entries — including user edits of the defaults — are
/// never overwritten. Returns how many were added.
#[tauri::command]
pub fn subagent_restore_defaults(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    let added = {
        let mut c = state.store.config.lock().unwrap();
        config::restore_missing_default_subagents(&mut c, &now())
    };
    if added > 0 {
        state.store.save_config().map_err(|e| e.to_string())?;
    }
    Ok(added)
}

// ---------------------------------------------------------------------------
// Onboarding & providers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewProvider {
    pub kind: String,
    pub name: String,
    pub base_url: String,
    pub api_type: config::ApiType,
    pub api_key: Option<String>,
}

#[tauri::command]
pub async fn provider_add(
    state: State<'_, Arc<AppState>>,
    provider: NewProvider,
) -> Result<ProviderConfig, String> {
    let id = uuid();
    let cfg = ProviderConfig {
        id: id.clone(),
        kind: provider.kind,
        name: provider.name,
        base_url: provider.base_url.trim_end_matches('/').to_string(),
        api_type: provider.api_type,
        default_model: None,
        models: Vec::new(),
        created_at: now(),
    };
    {
        let mut c = state.store.config.lock().unwrap();
        c.providers.push(cfg.clone());
        c.onboarding_complete = true;
    }
    state
        .store
        .set_provider_key(&id, provider.api_key.as_deref())
        .map_err(|e| e.to_string())?;
    state.store.save_config().map_err(|e| e.to_string())?;

    // try to populate the model list right away (non-fatal)
    let provider = crate::providers::build_provider(&cfg, state.store.provider_key(&id).as_deref());
    if let Ok(models) = provider.list_models().await {
        if let Some(p) = state
            .store
            .config
            .lock()
            .unwrap()
            .providers
            .iter_mut()
            .find(|p| p.id == id)
        {
            p.models = models;
        }
        let _ = state.store.save_config();
    }
    let c = state.store.config.lock().unwrap();
    Ok(c.providers
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .expect("just inserted"))
}

#[derive(Deserialize)]
pub struct ProviderUpdate {
    pub id: String,
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub api_key: Option<String>, // None = leave unchanged, Some("") = remove
}

#[tauri::command]
pub async fn provider_update(
    state: State<'_, Arc<AppState>>,
    update: ProviderUpdate,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(p) = c.providers.iter_mut().find(|p| p.id == update.id) else {
            return Err("Unknown provider".into());
        };
        if let Some(name) = update.name {
            p.name = name;
        }
        if let Some(url) = update.base_url {
            p.base_url = url.trim_end_matches('/').to_string();
        }
        if let Some(model) = update.default_model {
            p.default_model = if model.is_empty() { None } else { Some(model) };
        }
    }
    if let Some(key) = update.api_key {
        state
            .store
            .set_provider_key(&update.id, Some(&key))
            .map_err(|e| e.to_string())?;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn provider_delete(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        c.providers.retain(|p| p.id != id);
        // drop a dangling default model setting with its provider
        if c.settings.default_provider_id.as_deref() == Some(id.as_str()) {
            c.settings.default_provider_id = None;
            c.settings.default_model = None;
            c.settings.default_effort = None;
        }
        if c.settings.title_provider_id.as_deref() == Some(id.as_str()) {
            c.settings.title_provider_id = None;
            c.settings.title_model = None;
            c.settings.title_effort = None;
        }
    }
    state
        .store
        .set_provider_key(&id, None)
        .map_err(|e| e.to_string())?;
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn provider_test(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<Vec<String>, String> {
    let (cfg, key) = {
        let c = state.store.config.lock().unwrap();
        let p = c
            .providers
            .iter()
            .find(|p| p.id == id.clone())
            .ok_or("Unknown provider")?;
        (p.clone(), state.store.provider_key(&p.id))
    };
    let provider = crate::providers::build_provider(&cfg, key.as_deref());
    let models = provider.list_models().await.map_err(|e| e.to_string())?;
    {
        let mut c = state.store.config.lock().unwrap();
        if let Some(p) = c.providers.iter_mut().find(|p| p.id == id) {
            p.models = models.clone();
        }
    }
    state.store.save_config().map_err(|e| e.to_string())?;
    Ok(models)
}

#[tauri::command]
pub fn provider_has_key(state: State<'_, Arc<AppState>>, id: String) -> bool {
    state.store.provider_key(&id).is_some()
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn settings_set(
    state: State<'_, Arc<AppState>>,
    settings: AppSettingsPatch,
) -> Result<(), String> {
    // ids of stdio servers to relaunch because their spawn cwd changed
    let mut restart_stdio: Vec<String> = Vec::new();
    {
        let mut c = state.store.config.lock().unwrap();
        if let Some(theme) = settings.theme {
            c.settings.theme = theme;
        }
        if let Some(v) = settings.tool_approval {
            c.settings.tool_approval = v;
        }
        if let Some(v) = settings.sampling {
            c.settings.sampling = v;
        }
        if let Some(v) = settings.max_tool_iterations {
            c.settings.max_tool_iterations = v.clamp(1, 100);
        }
        if let Some(v) = settings.show_reasoning {
            c.settings.show_reasoning = v;
        }
        if let Some(v) = settings.tool_details {
            c.settings.tool_details = v;
        }
        if let Some(v) = settings.roots {
            c.settings.roots = v;
        }
        if let Some(v) = settings.working_dir {
            let v = if v.trim().is_empty() { None } else { Some(v) };
            if let Some(dir) = &v {
                let meta = std::fs::metadata(dir).map_err(|e| format!("{dir}: {e}"))?;
                if !meta.is_dir() {
                    return Err(format!("{dir} is not a directory"));
                }
            }
            if c.settings.working_dir != v {
                c.settings.working_dir = v;
                restart_stdio = c
                    .mcp_servers
                    .iter()
                    .filter(|s| matches!(s.transport, config::McpTransport::Stdio { .. }))
                    .map(|s| s.id.clone())
                    .collect();
            }
        }
        if let Some(v) = settings.default_provider_id {
            let v = if v.trim().is_empty() { None } else { Some(v) };
            if let Some(id) = &v {
                if !c.providers.iter().any(|p| &p.id == id) {
                    return Err(format!("Unknown provider {id}"));
                }
            }
            c.settings.default_provider_id = v;
        }
        if let Some(v) = settings.default_model {
            let v = if v.trim().is_empty() { None } else { Some(v) };
            c.settings.default_model = v;
        }
        if let Some(v) = settings.default_effort {
            c.settings.default_effort = v;
        }
        if let Some(v) = settings.title_provider_id {
            let v = if v.trim().is_empty() { None } else { Some(v) };
            if let Some(id) = &v {
                if !c.providers.iter().any(|p| &p.id == id) {
                    return Err(format!("Unknown provider {id}"));
                }
            }
            c.settings.title_provider_id = v;
        }
        if let Some(v) = settings.title_model {
            let v = if v.trim().is_empty() { None } else { Some(v) };
            c.settings.title_model = v;
        }
        if let Some(v) = settings.title_effort {
            c.settings.title_effort = v;
        }
        if let Some(v) = settings.update_mode {
            c.settings.update_mode = v;
        }
        if let Some(v) = settings.update_check_interval_hours {
            c.settings.update_check_interval_hours = v;
        }
        if let Some(v) = settings.system_prompt {
            c.settings.system_prompt = v;
        }
        if let Some(v) = settings.default_mode {
            c.settings.default_mode = v;
        }
    }
    state.store.save_config().map_err(|e| e.to_string())?;

    // stdio servers bake their cwd in at spawn time; relaunch the connected
    // ones so a changed working directory takes effect immediately
    for id in restart_stdio {
        if !matches!(
            state.manager.status(&id),
            crate::mcp::manager::ServerStatus::Connected
        ) {
            continue;
        }
        state.manager.disconnect(&id).await;
        let _ = state.manager.connect(&id).await;
    }
    Ok(())
}

/// Missing field → `None` (keep current), `null` → `Some(None)` (clear),
/// value → `Some(Some(v))` (set). Lets the UI reset the default effort
/// without touching the rest of the default.
fn deserialize_clearable<'de, D>(de: D) -> Result<Option<Option<config::EffortLevel>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

#[derive(Deserialize)]
pub struct AppSettingsPatch {
    pub theme: Option<Theme>,
    pub tool_approval: Option<config::ApprovalMode>,
    pub sampling: Option<config::SamplingMode>,
    pub max_tool_iterations: Option<u32>,
    pub show_reasoning: Option<bool>,
    pub tool_details: Option<config::ToolDetailsMode>,
    pub roots: Option<Vec<String>>,
    pub working_dir: Option<String>,
    pub default_provider_id: Option<String>,
    pub default_model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    pub default_effort: Option<Option<config::EffortLevel>>,
    pub title_provider_id: Option<String>,
    pub title_model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_clearable")]
    pub title_effort: Option<Option<config::EffortLevel>>,
    pub update_mode: Option<config::UpdateMode>,
    pub update_check_interval_hours: Option<u32>,
    pub system_prompt: Option<String>,
    pub default_mode: Option<config::AgentMode>,
}

#[tauri::command]
pub fn tool_rule_set(
    state: State<'_, Arc<AppState>>,
    key: String,
    rule: Option<ToolRule>,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        match rule {
            Some(r) => {
                c.settings.tool_rules.insert(key, r);
            }
            None => {
                c.settings.tool_rules.remove(&key);
            }
        }
    }
    state.store.save_config().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn conversation_create(
    state: State<'_, Arc<AppState>>,
    provider_id: String,
    model: String,
) -> ConversationMeta {
    let (effort, mode) = {
        let cfg = state.store.config.lock().unwrap();
        (cfg.settings.default_effort, cfg.settings.default_mode)
    };
    let meta = ConversationMeta {
        id: uuid(),
        title: String::new(),
        provider_id,
        model,
        effort,
        mcp_ids: None,
        mode,
        created_at: now(),
        updated_at: now(),
    };
    {
        let mut c = state.store.config.lock().unwrap();
        c.conversations.insert(0, meta.clone());
    }
    let _ = state.store.save_config();
    let _ = state.store.save_conversation(&meta, &[], &[]);
    meta
}

#[tauri::command]
pub fn conversation_delete(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    cancel_title_gen(&state, &id);
    // drop any live terminal with the conversation
    if let Some(session) = state.terminals.lock().unwrap().get(&id).cloned() {
        session.kill();
    }
    state
        .store
        .delete_conversation(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn conversation_rename(
    state: State<'_, Arc<AppState>>,
    id: String,
    title: String,
) -> Result<(), String> {
    cancel_title_gen(&state, &id);
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(meta) = c.conversations.iter_mut().find(|c| c.id == id) else {
            return Err("Unknown conversation".into());
        };
        meta.title = title;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn conversation_set_model(
    state: State<'_, Arc<AppState>>,
    id: String,
    provider_id: String,
    model: String,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(meta) = c.conversations.iter_mut().find(|c| c.id == id) else {
            return Err("Unknown conversation".into());
        };
        meta.provider_id = provider_id;
        meta.model = model;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn conversation_set_effort(
    state: State<'_, Arc<AppState>>,
    id: String,
    effort: Option<config::EffortLevel>,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(meta) = c.conversations.iter_mut().find(|c| c.id == id) else {
            return Err("Unknown conversation".into());
        };
        meta.effort = effort;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn conversation_set_mode(
    state: State<'_, Arc<AppState>>,
    id: String,
    mode: config::AgentMode,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(meta) = c.conversations.iter_mut().find(|c| c.id == id) else {
            return Err("Unknown conversation".into());
        };
        meta.mode = mode;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn conversation_set_mcp_ids(
    state: State<'_, Arc<AppState>>,
    id: String,
    mcp_ids: Option<Vec<String>>,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(meta) = c.conversations.iter_mut().find(|c| c.id == id) else {
            return Err("Unknown conversation".into());
        };
        meta.mcp_ids = mcp_ids;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

/// Effort levels the given model supports, from the models.dev catalog.
/// Empty = the model is known to have no effort control; unknown models get
/// the default low/medium/high trio (see `catalog::lookup`).
#[tauri::command]
pub async fn effort_levels(
    state: State<'_, Arc<AppState>>,
    kind: String,
    model: String,
) -> Result<Vec<config::EffortLevel>, String> {
    // async commands with borrowed state must return a Result (Tauri requirement)
    Ok(state.catalog.effort_levels(&kind, &model).await)
}

/// Context window in tokens for the given model, from the models.dev catalog.
#[tauri::command]
pub async fn context_limit(
    state: State<'_, Arc<AppState>>,
    kind: String,
    model: String,
) -> Result<Option<u64>, String> {
    Ok(state.catalog.context_limit(&kind, &model).await)
}

#[tauri::command]
pub fn conversation_get(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(ConversationMeta, Vec<serde_json::Value>), String> {
    state
        .store
        .load_conversation(&id)
        .ok_or_else(|| "Conversation not found".to_string())
}

fn cancel_title_gen(state: &AppState, id: &str) {
    if let Some(ct) = state.title_runtimes.lock().unwrap().remove(id) {
        ct.cancel();
    }
}

fn spawn_title_gen(state: Arc<AppState>, id: String, user_text: String, force: bool) {
    cancel_title_gen(&state, &id);
    let ct = CancellationToken::new();
    state
        .title_runtimes
        .lock()
        .unwrap()
        .insert(id.clone(), ct.clone());
    // Sync Tauri commands have no current Tokio runtime; use Tauri's.
    tauri::async_runtime::spawn(async move {
        state.agent.generate_title(&id, &user_text, force, ct).await;
        state.title_runtimes.lock().unwrap().remove(&id);
    });
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn chat_send(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    text: String,
) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Message is empty".into());
    }

    // conversation meta → provider/model
    let (provider_id, model, title_empty) = {
        let c = state.store.config.lock().unwrap();
        let meta = c
            .conversations
            .iter()
            .find(|c| c.id == conversation_id)
            .ok_or("Unknown conversation")?;
        (
            meta.provider_id.clone(),
            meta.model.clone(),
            meta.title.is_empty(),
        )
    };

    // existing history
    let history: Vec<Msg> = state
        .store
        .load_conversation(&conversation_id)
        .map(|(_, msgs)| msgs.iter().filter_map(Msg::from_json).collect())
        .unwrap_or_default();
    let auto_title = history.is_empty() && title_empty;

    let ct = CancellationToken::new();
    {
        // both locks held together (always in this order) so a concurrent
        // /compact can never slip between the check and the insert
        let compacting = state.compacting.lock().unwrap();
        if compacting.contains(&conversation_id) {
            return Err("This conversation is being compacted — try again in a moment.".into());
        }
        let mut runtimes = state.runtimes.lock().unwrap();
        runtimes.insert(
            conversation_id.clone(),
            Arc::new(ConversationRuntime { ct: ct.clone() }),
        );
    }

    // /undo bookkeeping: capture the project's state before this turn can
    // touch files. Outside git repos (or without git) there is nothing to
    // capture and /undo falls back to removing messages only.
    {
        let mut records = state.store.load_undo_records(&conversation_id);
        let user_index = history.len();
        let snap_root = state.store.snapshots_dir.clone();
        let wd = {
            let cfg = state.store.config.lock().unwrap();
            cfg.settings.effective_working_dir(&state.store.home_dir)
        };
        match tokio::task::spawn_blocking(move || crate::snapshot::capture(&snap_root, &wd)).await
        {
            Ok(Ok((tree, root))) => {
                records.push(UndoRecord {
                    user_index,
                    tree,
                    wd: root.to_string_lossy().into_owned(),
                });
                // bound the record list; older turns are long gone anyway
                if records.len() > 100 {
                    let drop = records.len() - 100;
                    records.drain(0..drop);
                }
                // the record must survive even if this turn is interrupted
                // before its first persist, so write it out right away
                let meta = state
                    .store
                    .config
                    .lock()
                    .unwrap()
                    .conversations
                    .iter()
                    .find(|c| c.id == conversation_id)
                    .cloned()
                    .ok_or("Unknown conversation")?;
                let payload: Vec<serde_json::Value> =
                    history.iter().map(|m| m.as_json()).collect();
                if let Err(e) = state.store.save_conversation(&meta, &payload, &records) {
                    tracing::warn!("failed to persist /undo snapshot record: {e}");
                }
            }
            Ok(Err(e)) => {
                tracing::debug!("no /undo snapshot for this turn: {e}");
            }
            Err(e) => {
                tracing::debug!("/undo snapshot task failed: {e}");
            }
        }
    };

    let app_state = state.inner().clone();
    if auto_title {
        spawn_title_gen(
            app_state.clone(),
            conversation_id.clone(),
            text.clone(),
            false,
        );
    }
    let conversation_id_task = conversation_id.clone();
    tokio::spawn(async move {
        app_state
            .agent
            .run_turn(conversation_id_task, provider_id, model, history, text, ct)
            .await;
        // drop the runtime once the turn finishes
        // (keep a small delay so late cancel calls resolve harmlessly)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        app_state.runtimes.lock().unwrap().remove(&conversation_id);
    });
    Ok(())
}

#[tauri::command]
pub fn chat_cancel(state: State<'_, Arc<AppState>>, conversation_id: String) {
    if let Some(rt) = state.runtimes.lock().unwrap().get(&conversation_id) {
        rt.ct.cancel();
    }
    state.bridge.cancel_for_conversation(&conversation_id);
}

/// `/compact`: replace the conversation's history with a model-generated
/// summary. Refuses while a turn is running (and vice versa).
#[tauri::command]
pub async fn conversation_compact(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    instructions: Option<String>,
) -> Result<(), String> {
    {
        // same nested-lock order as chat_send
        let mut compacting = state.compacting.lock().unwrap();
        if compacting.contains(&conversation_id) {
            return Err("This conversation is already being compacted.".into());
        }
        let runtimes = state.runtimes.lock().unwrap();
        if runtimes.contains_key(&conversation_id) {
            return Err("Wait for the current response to finish before compacting.".into());
        }
        compacting.insert(conversation_id.clone());
    }
    let app_state = state.inner().clone();
    // run in its own task: a panic inside the provider call must surface as
    // an Err here (tokio catches it at the task boundary) instead of
    // unwinding through the command, which would leave the compacting flag
    // set and the invoke forever pending.
    let task_agent = app_state.agent.clone();
    let task_id = conversation_id.clone();
    let result = match tokio::spawn(async move {
        crate::compact::run(&task_agent, &task_id, instructions.as_deref()).await
    })
    .await
    {
        Ok(result) => result,
        Err(e) => Err(format!("Compaction failed unexpectedly: {e}")),
    };
    app_state.compacting.lock().unwrap().remove(&conversation_id);
    result
}

#[derive(Serialize)]
pub struct UndoOutcome {
    /// The user message that was removed, for the composer to restore.
    pub undone_text: String,
    /// Repository-relative paths restored or deleted by the file revert.
    pub reverted_files: Vec<String>,
    /// How many transcript messages were removed.
    pub truncated: usize,
    /// Set when the messages were removed but the files could not be reverted.
    pub file_warning: Option<String>,
}

/// `/undo`: drop the last user turn from the transcript and, when a snapshot
/// exists, restore the project files to their pre-turn state.
#[tauri::command]
pub async fn conversation_undo(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<UndoOutcome, String> {
    {
        let compacting = state.compacting.lock().unwrap();
        if compacting.contains(&conversation_id) {
            return Err("This conversation is being compacted.".into());
        }
        let runtimes = state.runtimes.lock().unwrap();
        if runtimes.contains_key(&conversation_id) {
            return Err("Wait for the current response to finish before undoing.".into());
        }
    }
    let app_state = state.inner().clone();
    let (meta, messages, records) = {
        let Some((meta, messages)) = app_state.store.load_conversation(&conversation_id) else {
            return Err("Unknown conversation".into());
        };
        (meta, messages, app_state.store.load_undo_records(&conversation_id))
    };

    // boundary = the last real user message (compaction summaries are
    // carriers, not turns); everything from it onward is removed
    let boundary = messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, v)| {
            Msg::from_json(v)
                .map(|m| {
                    matches!(&m, Msg::User { text, .. }
                        if !text.starts_with(crate::compact::COMPACT_MARKER))
                })
                .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .ok_or("Nothing to undo")?;
    let undone_text = match Msg::from_json(&messages[boundary]) {
        Some(Msg::User { text, .. }) => text,
        _ => return Err("Nothing to undo".into()),
    };

    let record = records.iter().rev().find(|r| r.user_index == boundary).cloned();
    let (reverted_files, file_warning) = match record {
        Some(rec) => {
            let snap_root = app_state.store.snapshots_dir.clone();
            let wd = std::path::PathBuf::from(&rec.wd);
            let tree = rec.tree.clone();
            match tokio::task::spawn_blocking(move || {
                crate::snapshot::restore(&snap_root, &wd, &tree)
            })
            .await
            {
                Ok(Ok(files)) => (files, None),
                Ok(Err(e)) => (
                    Vec::new(),
                    Some(format!("Files were not reverted: {e}")),
                ),
                Err(e) => (
                    Vec::new(),
                    Some(format!("Files were not reverted: {e}")),
                ),
            }
        }
        None => (
            Vec::new(),
            Some(
                "No file snapshot for this turn — files were left unchanged \
                 (snapshots need the working directory to be a git repository)."
                    .into(),
            ),
        ),
    };

    let truncated = messages.len() - boundary;
    let kept: Vec<serde_json::Value> = messages[..boundary].to_vec();
    let kept_records: Vec<UndoRecord> =
        records.into_iter().filter(|r| r.user_index < boundary).collect();
    let mut meta = meta;
    meta.updated_at = now();
    app_state
        .store
        .save_conversation(&meta, &kept, &kept_records)
        .map_err(|e| e.to_string())?;
    Ok(UndoOutcome {
        undone_text,
        reverted_files,
        truncated,
        file_warning,
    })
}

#[tauri::command]
pub fn conversation_generate_title(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let user_text = state
        .store
        .load_conversation(&id)
        .ok_or("Unknown conversation")?
        .1
        .iter()
        .filter_map(Msg::from_json)
        .find_map(|m| match m {
            Msg::User { text, .. } => Some(text),
            _ => None,
        })
        .ok_or_else(|| "Send a message first".to_string())?;
    spawn_title_gen(state.inner().clone(), id, user_text, true);
    Ok(())
}

#[tauri::command]
pub fn conversation_cancel_title(state: State<'_, Arc<AppState>>, id: String) {
    cancel_title_gen(&state, &id);
}

// ---------------------------------------------------------------------------
// Per-conversation terminal
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct TerminalCreated {
    /// Always true on success; exists so the UI can distinguish a fresh spawn
    /// from a reattach without extra commands.
    pub running: bool,
    /// Base64 of buffered output since the session started ("" when fresh).
    pub scrollback: String,
    /// Live `terminal_output` events with `seq <` this are already contained
    /// in `scrollback` and must not be written to the emulator again.
    pub last_seq: u64,
}

/// Attach to the conversation's terminal, spawning a login shell in the
/// app's working directory if none is running yet.
#[tauri::command]
pub fn terminal_create(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<TerminalCreated, String> {
    let cwd = {
        let c = state.store.config.lock().unwrap();
        c.settings.effective_working_dir(&state.store.home_dir)
    };
    let session = crate::terminal::get_or_spawn(
        &conversation_id,
        &cwd,
        state.sink.clone(),
        state.terminals.clone(),
    )?;
    use base64::Engine as _;
    let (bytes, last_seq) = session.snapshot();
    Ok(TerminalCreated {
        running: true,
        scrollback: base64::engine::general_purpose::STANDARD.encode(bytes),
        last_seq,
    })
}

#[tauri::command]
pub fn terminal_write(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    data: String,
) -> Result<(), String> {
    let session = state
        .terminals
        .lock()
        .unwrap()
        .get(&conversation_id)
        .cloned()
        .ok_or("No terminal running for this conversation")?;
    session.write(&data)
}

#[tauri::command]
pub fn terminal_resize(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let session = state
        .terminals
        .lock()
        .unwrap()
        .get(&conversation_id)
        .cloned()
        .ok_or("No terminal running for this conversation")?;
    session.resize(cols, rows)
}

/// Kill the conversation's terminal. Its exit is reported asynchronously via
/// `terminal_closed`; the entry is removed by the waiter thread.
#[tauri::command]
pub fn terminal_close(
    state: State<'_, Arc<AppState>>,
    conversation_id: String,
) -> Result<(), String> {
    let session = state
        .terminals
        .lock()
        .unwrap()
        .get(&conversation_id)
        .cloned()
        .ok_or("No terminal running for this conversation")?;
    session.kill();
    Ok(())
}

// ---------------------------------------------------------------------------
// Interactive responses
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ApprovalResponse {
    pub request_id: String,
    pub decision: String, // "allow_once" | "always_allow" | "deny"
}

#[tauri::command]
pub fn approval_respond(state: State<'_, Arc<AppState>>, response: ApprovalResponse) -> bool {
    let decision = match response.decision.as_str() {
        "always_allow" => ApprovalDecision::AlwaysAllow,
        "allow_once" | "allow" => ApprovalDecision::AllowOnce,
        _ => ApprovalDecision::Deny,
    };
    state
        .bridge
        .resolve_approval(&response.request_id, decision)
}

#[derive(Deserialize)]
pub struct PlanResponse {
    pub request_id: String,
    pub decision: String, // "approve" | "revise"
    pub feedback: Option<String>,
}

/// Answer the plan review dialog. Returns false when the request is gone
/// (cancelled turn, or the app restarted while a plan was pending).
#[tauri::command]
pub fn plan_respond(state: State<'_, Arc<AppState>>, response: PlanResponse) -> bool {
    let decision = match response.decision.as_str() {
        "approve" => PlanDecision::Approve,
        "revise" => PlanDecision::Revise(match response.feedback {
            Some(text) if !text.trim().is_empty() => text,
            _ => "the user asked for changes without saying what".to_string(),
        }),
        _ => return false,
    };
    state.bridge.resolve_plan(&response.request_id, decision)
}

#[derive(Deserialize)]
pub struct ElicitationResponse {
    pub request_id: String,
    pub action: String, // "accept" | "decline" | "cancel"
    pub content: Option<serde_json::Value>,
}

#[tauri::command]
pub fn elicitation_respond(state: State<'_, Arc<AppState>>, response: ElicitationResponse) -> bool {
    use rmcp::model::ElicitationAction;
    let action = match response.action.as_str() {
        "accept" => ElicitationAction::Accept,
        "decline" => ElicitationAction::Decline,
        _ => ElicitationAction::Cancel,
    };
    let mut result = rmcp::model::ElicitResult::new(action);
    if let Some(content) = response.content {
        result = result.with_content(content);
    }
    state
        .bridge
        .resolve_elicitation(&response.request_id, result)
}

#[tauri::command]
pub fn sampling_respond(
    state: State<'_, Arc<AppState>>,
    request_id: String,
    approve: bool,
) -> bool {
    state.bridge.resolve_sampling(&request_id, approve)
}

// ---------------------------------------------------------------------------
// MCP connectors
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewServer {
    pub name: String,
    pub transport: McpTransportView,
    #[serde(default)]
    pub auth: config::HttpAuth,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportView {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

impl From<McpTransportView> for config::McpTransport {
    fn from(v: McpTransportView) -> Self {
        match v {
            McpTransportView::Stdio { command, args, env } => {
                config::McpTransport::Stdio { command, args, env }
            }
            McpTransportView::Http { url, headers } => config::McpTransport::Http { url, headers },
        }
    }
}

#[tauri::command]
pub fn mcp_add(
    state: State<'_, Arc<AppState>>,
    server: NewServer,
) -> Result<McpServerConfig, String> {
    let cfg = McpServerConfig {
        id: uuid(),
        name: server.name,
        transport: server.transport.into(),
        auth: server.auth,
        enabled: server.enabled,
        auto_start: server.enabled,
        oauth_client_id: None,
        oauth_redirect_port: None,
        created_at: now(),
    };
    {
        let mut c = state.store.config.lock().unwrap();
        c.mcp_servers.push(cfg.clone());
    }
    state.store.save_config().map_err(|e| e.to_string())?;
    Ok(cfg)
}

#[tauri::command]
pub fn mcp_update(state: State<'_, Arc<AppState>>, server: McpServerConfig) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(existing) = c.mcp_servers.iter_mut().find(|s| s.id == server.id) else {
            return Err("Unknown server".into());
        };
        *existing = server;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mcp_remove(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    state.manager.disconnect(&id).await;
    state.manager.forget(&id);
    {
        let mut c = state.store.config.lock().unwrap();
        c.mcp_servers.retain(|s| s.id != id);
    }
    state
        .store
        .set_oauth_tokens(&id, None)
        .map_err(|e| e.to_string())?;
    state
        .store
        .set_oauth_client_secret(&id, None)
        .map_err(|e| e.to_string())?;
    {
        state
            .store
            .secrets
            .lock()
            .unwrap()
            .server_tokens
            .remove(&id);
    }
    state.store.save_secrets().map_err(|e| e.to_string())?;
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn mcp_set_enabled(
    state: State<'_, Arc<AppState>>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(s) = c.mcp_servers.iter_mut().find(|s| s.id == id) else {
            return Err("Unknown server".into());
        };
        s.enabled = enabled;
        s.auto_start = enabled;
    }
    state.store.save_config().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mcp_connect(state: State<'_, Arc<AppState>>, id: String) -> Result<String, String> {
    let status = state.manager.connect(&id).await?;
    Ok(match status {
        crate::mcp::manager::ServerStatus::Connected => "connected".into(),
        crate::mcp::manager::ServerStatus::NeedsAuth { .. } => "needs_auth".into(),
        crate::mcp::manager::ServerStatus::Error { .. } => "error".into(),
        other => format!("{other:?}"),
    })
}

#[tauri::command]
pub async fn mcp_disconnect(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    state.manager.disconnect(&id).await;
    Ok(())
}

#[tauri::command]
pub fn mcp_summaries(state: State<'_, Arc<AppState>>) -> Vec<crate::mcp::manager::ServerSummary> {
    state.manager.summaries()
}

#[tauri::command]
pub fn mcp_summary(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> crate::mcp::manager::ServerSummary {
    state.manager.summary(&id)
}

#[tauri::command]
pub async fn mcp_refresh(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<crate::mcp::manager::ServerSummary, String> {
    state.manager.refresh_server(&id).await
}

#[tauri::command]
pub async fn mcp_read_resource(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    uri: String,
) -> Result<serde_json::Value, String> {
    let result = state.manager.read_resource(&server_id, &uri).await?;
    serde_json::to_value(&result.contents).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mcp_get_prompt(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    name: String,
    arguments: HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let result = state
        .manager
        .get_prompt(&server_id, &name, arguments)
        .await?;
    serde_json::to_value(&result).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mcp_subscribe_resource(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    uri: String,
) -> Result<(), String> {
    state.manager.subscribe_resource(&server_id, &uri).await
}

#[tauri::command]
pub fn mcp_unsubscribe_resource(state: State<'_, Arc<AppState>>, server_id: String, uri: String) {
    state.manager.unsubscribe_resource(&server_id, &uri)
}

#[tauri::command]
pub async fn mcp_complete(
    state: State<'_, Arc<AppState>>,
    server_id: String,
    reference: serde_json::Value,
    argument_name: String,
    argument_value: String,
) -> Result<serde_json::Value, String> {
    let result = state
        .manager
        .complete(&server_id, reference, argument_name, argument_value)
        .await?;
    serde_json::to_value(&result).map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
pub struct ImportedPreview {
    pub name: String,
    pub transport: config::McpTransport,
}

#[tauri::command]
pub fn mcp_import_preview(text: String) -> Result<Vec<ImportedPreview>, String> {
    let imported = config::parse_import_json(&text).map_err(|e| e.to_string())?;
    Ok(imported
        .into_iter()
        .map(|i| ImportedPreview {
            name: i.name,
            transport: i.transport,
        })
        .collect())
}

#[tauri::command]
pub fn mcp_import_add(
    state: State<'_, Arc<AppState>>,
    servers: Vec<ImportedPreview>,
) -> Result<usize, String> {
    let mut added = 0;
    {
        let mut c = state.store.config.lock().unwrap();
        for s in servers {
            if c.mcp_servers.iter().any(|existing| existing.name == s.name) {
                continue;
            }
            c.mcp_servers.push(McpServerConfig {
                id: uuid(),
                name: s.name,
                transport: s.transport,
                auth: config::HttpAuth::None,
                enabled: true,
                auto_start: true,
                oauth_client_id: None,
                oauth_redirect_port: None,
                created_at: now(),
            });
            added += 1;
        }
    }
    state.store.save_config().map_err(|e| e.to_string())?;
    Ok(added)
}

// ---------------------------------------------------------------------------
// OAuth
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn mcp_oauth_login(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    crate::oauth::login(app, state.store.clone(), state.sink.clone(), id.clone()).await?;
    // Return as soon as tokens are stored so the tool card drops "Waiting for
    // sign-in". Reconnect in the background; notify waiters only after that
    // handshake so they do not start a second one.
    let manager = state.manager.clone();
    tauri::async_runtime::spawn(async move {
        let _ = manager.connect(&id).await;
        manager.notify_auth_completed(&id);
    });
    Ok(())
}

#[tauri::command]
pub fn mcp_oauth_logout(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    crate::oauth::logout(&state.store, &id)
}

#[tauri::command]
pub fn mcp_set_bearer_token(
    state: State<'_, Arc<AppState>>,
    id: String,
    token: String,
) -> Result<(), String> {
    {
        let mut secrets = state.store.secrets.lock().unwrap();
        if token.is_empty() {
            secrets.server_tokens.remove(&id);
        } else {
            secrets.server_tokens.insert(id.clone(), token);
        }
    }
    state.store.save_secrets().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn mcp_has_auth(state: State<'_, Arc<AppState>>, id: String) -> String {
    let cfg = state.store.config.lock().unwrap();
    let Some(server) = cfg.mcp_servers.iter().find(|s| s.id == id) else {
        return "none".into();
    };
    match &server.auth {
        config::HttpAuth::None => "none".into(),
        config::HttpAuth::Bearer { .. } => {
            if state.store.server_token(&id).is_some() {
                "bearer".into()
            } else {
                "bearer_missing".into()
            }
        }
        config::HttpAuth::OAuth => {
            if state.store.oauth_tokens(&id).is_some() {
                "oauth".into()
            } else {
                "oauth_missing".into()
            }
        }
    }
}

#[tauri::command]
pub fn mcp_set_oauth_config(
    state: State<'_, Arc<AppState>>,
    id: String,
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_port: Option<u16>,
) -> Result<(), String> {
    {
        let mut c = state.store.config.lock().unwrap();
        let Some(s) = c.mcp_servers.iter_mut().find(|s| s.id == id) else {
            return Err("Unknown server".into());
        };
        s.oauth_client_id = if client_id.as_deref().unwrap_or("").is_empty() {
            None
        } else {
            client_id
        };
        s.oauth_redirect_port = redirect_port;
    }
    state
        .store
        .set_oauth_client_secret(
            &id,
            client_secret
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        )
        .map_err(|e| e.to_string())
}

// App version for the About panel
#[tauri::command]
pub fn app_info() -> serde_json::Value {
    serde_json::json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "mcp_spec": "2026-07-28",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_effort_patch_is_tristate() {
        // missing field keeps the current default
        let keep: AppSettingsPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(keep.default_effort, None);

        // null clears it
        let clear: AppSettingsPatch = serde_json::from_str(r#"{"default_effort": null}"#).unwrap();
        assert_eq!(clear.default_effort, Some(None));

        let set: AppSettingsPatch = serde_json::from_str(r#"{"default_effort": "high"}"#).unwrap();
        assert_eq!(set.default_effort, Some(Some(config::EffortLevel::High)));
    }

    #[test]
    fn title_effort_patch_is_tristate() {
        let keep: AppSettingsPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(keep.title_effort, None);

        let clear: AppSettingsPatch = serde_json::from_str(r#"{"title_effort": null}"#).unwrap();
        assert_eq!(clear.title_effort, Some(None));

        let set: AppSettingsPatch = serde_json::from_str(r#"{"title_effort": "low"}"#).unwrap();
        assert_eq!(set.title_effort, Some(Some(config::EffortLevel::Low)));
    }

    #[test]
    fn tool_details_patch_deserializes() {
        let keep: AppSettingsPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(keep.tool_details, None);

        let set: AppSettingsPatch =
            serde_json::from_str(r#"{"tool_details": "collapsed"}"#).unwrap();
        assert_eq!(set.tool_details, Some(config::ToolDetailsMode::Collapsed));
    }

    #[test]
    fn update_settings_patch_deserializes() {
        let keep: AppSettingsPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(keep.update_mode, None);
        assert_eq!(keep.update_check_interval_hours, None);

        let set: AppSettingsPatch =
            serde_json::from_str(r#"{"update_mode": "auto", "update_check_interval_hours": 24}"#)
                .unwrap();
        assert_eq!(set.update_mode, Some(config::UpdateMode::Auto));
        assert_eq!(set.update_check_interval_hours, Some(24));
    }
}
