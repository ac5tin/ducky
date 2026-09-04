//! models.dev catalog: per-model reasoning effort levels.
//!
//! Fetches <https://models.dev/api.json> and indexes the
//! `reasoning_options` entries of type `effort` so the UI can offer exactly
//! the levels each model supports. Cached in memory (24h) and on disk so a
//! failed refresh degrades to slightly stale data instead of nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::EffortLevel;

pub const CATALOG_URL: &str = "https://models.dev/api.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// effort levels per provider id -> model id, indexed from models.dev.
/// An empty level list means "model known, no effort control" (hide the UI).
pub type Index = HashMap<String, HashMap<String, Vec<EffortLevel>>>;

pub struct Catalog {
    state: Mutex<CatalogState>,
    cache_path: Option<PathBuf>,
}

#[derive(Default)]
struct CatalogState {
    index: Option<Index>,
    /// When the in-memory index was fetched; `None` = loaded from disk (stale).
    fetched_at: Option<Instant>,
}

impl Catalog {
    pub fn new(cache_path: &std::path::Path) -> Self {
        let index = std::fs::read_to_string(cache_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok());
        Self {
            state: Mutex::new(CatalogState {
                index,
                fetched_at: None,
            }),
            cache_path: Some(cache_path.to_path_buf()),
        }
    }

    /// Effort levels for a Ducky provider kind + model id. Empty = hide the
    /// effort selector; unknown models fall back to the default trio.
    pub async fn effort_levels(&self, kind: &str, model: &str) -> Vec<EffortLevel> {
        {
            let st = self.state.lock().unwrap();
            if let (Some(index), Some(at)) = (&st.index, st.fetched_at) {
                if at.elapsed() < MAX_AGE {
                    return lookup(index, kind, model);
                }
            }
        }
        match fetch_index().await {
            Ok(index) => {
                self.store_cache(&index);
                let mut st = self.state.lock().unwrap();
                st.index = Some(index);
                st.fetched_at = Some(Instant::now());
                lookup(st.index.as_ref().unwrap(), kind, model)
            }
            Err(e) => {
                tracing::warn!("models.dev catalog unavailable: {e}");
                let st = self.state.lock().unwrap();
                match &st.index {
                    Some(index) => lookup(index, kind, model),
                    None => EffortLevel::default_levels(),
                }
            }
        }
    }

    fn store_cache(&self, index: &Index) {
        let Some(path) = &self.cache_path else {
            return;
        };
        let Ok(json) = serde_json::to_string(index) else {
            return;
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, json).ok();
    }
}

async fn fetch_index() -> anyhow::Result<Index> {
    let body = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(CATALOG_URL)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    parse_index(&body)
}

pub fn parse_index(body: &str) -> anyhow::Result<Index> {
    let value: serde_json::Value = serde_json::from_str(body)?;
    let Some(providers) = value.as_object() else {
        anyhow::bail!("unexpected catalog shape");
    };
    let mut index = Index::new();
    for (provider_id, pv) in providers {
        let Some(models) = pv.get("models").and_then(|m| m.as_object()) else {
            continue;
        };
        for (model_id, mv) in models {
            let known = mv.get("reasoning").and_then(|r| r.as_bool()).is_some();
            let levels = effort_values(mv);
            // only index models we know something about; the rest fall back
            // to the default level trio at lookup time
            if known || !levels.is_empty() {
                index
                    .entry(provider_id.clone())
                    .or_default()
                    .insert(model_id.clone(), levels);
            }
        }
    }
    Ok(index)
}

/// Union of all `effort`-type `reasoning_options` values, catalog order.
fn effort_values(model: &serde_json::Value) -> Vec<EffortLevel> {
    let mut out: Vec<EffortLevel> = Vec::new();
    if let Some(opts) = model.get("reasoning_options").and_then(|o| o.as_array()) {
        for opt in opts {
            if opt.get("type").and_then(|t| t.as_str()) != Some("effort") {
                continue;
            }
            if let Some(values) = opt.get("values").and_then(|v| v.as_array()) {
                for v in values {
                    let Some(s) = v.as_str() else { continue };
                    let Ok(level) = serde_json::from_value::<EffortLevel>(serde_json::json!(s))
                    else {
                        continue;
                    };
                    if !out.contains(&level) {
                        out.push(level);
                    }
                }
            }
        }
    }
    out
}

pub fn lookup(index: &Index, kind: &str, model: &str) -> Vec<EffortLevel> {
    let model = model.trim();
    if model.is_empty() {
        return EffortLevel::default_levels();
    }
    let aliases = provider_aliases(kind);
    for pid in &aliases {
        if let Some(levels) = index.get(*pid).and_then(|m| m.get(model)) {
            return levels.clone();
        }
    }
    // normalized fallback: strip vendor prefix / local tag ("openai/gpt-5",
    // "qwen3:8b") and retry the alias providers
    let bare = bare_model_id(model);
    for pid in &aliases {
        if let Some(levels) = index.get(*pid).and_then(|m| m.get(&bare)) {
            return levels.clone();
        }
    }
    // last resort: bare-id scan across the whole catalog (aggregators and
    // custom providers), preferring canonical labs for determinism
    let mut hits: Vec<(&String, &Vec<EffortLevel>)> = index
        .iter()
        .flat_map(|(pid, models)| {
            models
                .iter()
                .filter(|(mid, _)| bare_model_id(mid) == bare)
                .map(move |(_, levels)| (pid, levels))
        })
        .collect();
    hits.sort_by(|(a, _), (b, _)| {
        (provider_rank(a), a.as_str()).cmp(&(provider_rank(b), b.as_str()))
    });
    if let Some((_, levels)) = hits.first() {
        return (*levels).clone();
    }
    EffortLevel::default_levels()
}

/// Canonical models.dev provider ids for a Ducky provider kind, best first.
fn provider_aliases(kind: &str) -> Vec<&'static str> {
    match kind {
        "zai" | "zai-coding" => vec!["zai-coding-plan", "zai", "zhipuai"],
        "openai" => vec!["openai"],
        "anthropic" => vec!["anthropic"],
        "openrouter" => vec!["openrouter"],
        "groq" => vec!["groq"],
        "ollama" => vec!["ollama", "ollama-cloud"],
        "lmstudio" => vec!["lmstudio"],
        "opencode" => vec!["opencode", "opencode-go"],
        _ => vec![],
    }
}

fn provider_rank(pid: &str) -> usize {
    [
        "zai",
        "zai-coding-plan",
        "zhipuai",
        "openai",
        "anthropic",
        "openrouter",
        "groq",
    ]
    .iter()
    .position(|p| *p == pid)
    .unwrap_or(usize::MAX)
}

/// Lowercase id without vendor prefix ("openai/gpt-5") or local tag ("qwen3:8b").
fn bare_model_id(model: &str) -> String {
    let lowered = model.trim().to_ascii_lowercase();
    let last = lowered.rsplit('/').next().unwrap_or(&lowered);
    last.split(':').next().unwrap_or(last).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Index {
        let body = json!({
            "openai": {
                "id": "openai",
                "models": {
                    "gpt-5.5": {
                        "reasoning": true,
                        "reasoning_options": [
                            { "type": "effort", "values": ["none", "low", "medium", "high", "xhigh"] }
                        ]
                    },
                    "gpt-4o-mini": { "reasoning": false }
                }
            },
            "zai": {
                "id": "zai",
                "models": {
                    "glm-5.3": {
                        "reasoning": true,
                        "reasoning_options": [{ "type": "effort", "values": ["low", "high", "max"] }]
                    },
                    "glm-5.1": {
                        "reasoning": true,
                        "reasoning_options": [{ "type": "toggle" }]
                    },
                    "glm-4.8": { "reasoning": false }
                }
            },
            "anthropic": {
                "id": "anthropic",
                "models": {
                    "claude-opus-4.7": {
                        "reasoning": true,
                        "reasoning_options": [{ "type": "effort", "values": ["low", "medium", "high"] }]
                    }
                }
            },
            "hpc-ai": {
                "id": "hpc-ai",
                "models": {
                    "zai-org/glm-5.2": {
                        "reasoning": true,
                        "reasoning_options": [{ "type": "effort", "values": ["high", "max"] }]
                    }
                }
            }
        });
        parse_index(&serde_json::to_string(&body).unwrap()).unwrap()
    }

    #[test]
    fn extracts_effort_values_in_catalog_order() {
        let index = fixture();
        assert_eq!(
            lookup(&index, "openai", "gpt-5.5"),
            vec![
                EffortLevel::None,
                EffortLevel::Low,
                EffortLevel::Medium,
                EffortLevel::High,
                EffortLevel::XHigh
            ]
        );
        assert_eq!(
            lookup(&index, "zai", "glm-5.3"),
            vec![EffortLevel::Low, EffortLevel::High, EffortLevel::Max]
        );
        assert_eq!(
            lookup(&index, "anthropic", "claude-opus-4.7"),
            vec![EffortLevel::Low, EffortLevel::Medium, EffortLevel::High]
        );
    }

    #[test]
    fn known_models_without_effort_hide_the_ui() {
        let index = fixture();
        assert!(lookup(&index, "zai", "glm-4.8").is_empty());
        // toggle-only reasoning has no effort levels either
        assert!(lookup(&index, "zai", "glm-5.1").is_empty());
    }

    #[test]
    fn unknown_models_fall_back_to_defaults() {
        let index = fixture();
        assert_eq!(
            lookup(&index, "ollama", "llama4:latest"),
            EffortLevel::default_levels()
        );
        assert_eq!(
            lookup(&index, "custom", "mystery-model"),
            EffortLevel::default_levels()
        );
        assert_eq!(lookup(&index, "openai", ""), EffortLevel::default_levels());
    }

    #[test]
    fn custom_providers_match_by_bare_model_id() {
        let index = fixture();
        // vendor-prefixed id, unknown kind -> bare scan finds it
        assert_eq!(
            lookup(&index, "custom", "zai-org/glm-5.2"),
            vec![EffortLevel::High, EffortLevel::Max]
        );
        // canonical labs win over aggregator duplicates
        assert_eq!(lookup(&index, "custom", "gpt-5.5")[0], EffortLevel::None);
    }

    #[test]
    fn effort_level_serde_uses_models_dev_values() {
        let level: EffortLevel = serde_json::from_value(json!("xhigh")).unwrap();
        assert_eq!(level, EffortLevel::XHigh);
        assert_eq!(
            serde_json::to_value(EffortLevel::Max).unwrap(),
            json!("max")
        );
        assert!(serde_json::from_value::<EffortLevel>(json!("bogus")).is_err());
    }

    #[test]
    fn index_disk_roundtrip() {
        let index = fixture();
        let json = serde_json::to_string(&index).unwrap();
        let back: Index = serde_json::from_str(&json).unwrap();
        assert_eq!(
            lookup(&back, "zai", "glm-5.3"),
            lookup(&index, "zai", "glm-5.3")
        );
    }
}
