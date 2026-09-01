//! Application configuration: providers, MCP servers, settings, conversations.
//!
//! Stored as JSON in the OS app-config directory. API keys live in a separate
//! `secrets.json` written with `0600` permissions so they never travel to the
//! webview.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Which wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiType {
    /// OpenAI-compatible `/chat/completions` (covers OpenAI, Z.ai, Ollama,
    /// OpenRouter, Groq, LM Studio, OpenCode Zen, vLLM, ...).
    OpenAi,
    /// Anthropic `/v1/messages` protocol.
    Anthropic,
}

impl ApiType {
    pub fn label(&self) -> &'static str {
        match self {
            ApiType::OpenAi => "OpenAI-compatible",
            ApiType::Anthropic => "Anthropic",
        }
    }
}

/// A configured AI provider connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    /// Stable preset id (e.g. `openai`) or `custom`.
    pub kind: String,
    /// User-facing name.
    pub name: String,
    pub base_url: String,
    pub api_type: ApiType,
    /// Optional default model used for new conversations.
    pub default_model: Option<String>,
    /// Cached model list from the provider (refreshed on test / manual refresh).
    #[serde(default)]
    pub models: Vec<String>,
    pub created_at: String,
}

/// Transport for an MCP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// A local server launched as a child process (the classic `stdio` transport).
    Stdio { command: String, args: Vec<String>, env: HashMap<String, String> },
    /// A remote server over Streamable HTTP.
    Http { url: String, headers: HashMap<String, String> },
}

/// How the client authenticates against a remote HTTP MCP server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpAuth {
    #[default]
    None,
    /// A static bearer token supplied by the user.
    Bearer { token_ref: String },
    /// OAuth 2.1 (dynamic registration / PKCE) managed by Ducky.
    OAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub transport: McpTransport,
    pub auth: HttpAuth,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When true Ducky starts this server automatically at launch.
    #[serde(default = "default_true")]
    pub auto_start: bool,
    /// Optional OAuth client id if the user pre-registered one.
    #[serde(default)]
    pub oauth_client_id: Option<String>,
    /// Optional fixed redirect port for pre-registered OAuth clients.
    #[serde(default)]
    pub oauth_redirect_port: Option<u16>,
    pub created_at: String,
}

fn default_true() -> bool {
    true
}

/// Reasoning effort level for a model (as offered by models.dev).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffortLevel {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    #[serde(rename = "max")]
    Max,
}

impl EffortLevel {
    /// Wire value sent as the provider's effort parameter.
    pub fn as_str(&self) -> &'static str {
        match self {
            EffortLevel::None => "none",
            EffortLevel::Minimal => "minimal",
            EffortLevel::Low => "low",
            EffortLevel::Medium => "medium",
            EffortLevel::High => "high",
            EffortLevel::XHigh => "xhigh",
            EffortLevel::Max => "max",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            EffortLevel::None => "None",
            EffortLevel::Minimal => "Minimal",
            EffortLevel::Low => "Low",
            EffortLevel::Medium => "Medium",
            EffortLevel::High => "High",
            EffortLevel::XHigh => "XHigh",
            EffortLevel::Max => "Max",
        }
    }

    /// Fallback levels for models that aren't in the models.dev catalog.
    pub fn default_levels() -> Vec<EffortLevel> {
        vec![EffortLevel::Low, EffortLevel::Medium, EffortLevel::High]
    }
}

/// Tool approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Ask before every tool execution (safest, default).
    AlwaysAsk,
    /// Auto-approve tools the server marks read-only, ask for the rest.
    AutoApproveReadOnly,
    /// Auto-approve everything (only recommended for trusted local servers).
    AutoApproveAll,
}

/// Sampling request policy (a server asking Ducky's model for help).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMode {
    /// Ask the user each time (default).
    Ask,
    /// Silently approve sampling requests.
    AutoApprove,
    /// Silently deny sampling requests.
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRule {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub theme: Theme,
    pub tool_approval: ApprovalMode,
    pub sampling: SamplingMode,
    /// Per-tool overrides keyed by `"<server_id>/<tool_name>"`.
    pub tool_rules: HashMap<String, ToolRule>,
    /// Folders exposed to servers that request roots (deprecated MCP feature,
    /// kept for compatibility).
    pub roots: Vec<String>,
    /// Maximum tool-call rounds in a single chat turn.
    pub max_tool_iterations: u32,
    /// Show reasoning tokens when a provider streams them.
    pub show_reasoning: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            tool_approval: ApprovalMode::AlwaysAsk,
            sampling: SamplingMode::Ask,
            tool_rules: HashMap::new(),
            roots: Vec::new(),
            max_tool_iterations: 25,
            show_reasoning: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model: String,
    /// Reasoning effort for this conversation (None = provider default).
    #[serde(default)]
    pub effort: Option<EffortLevel>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    pub version: u32,
    #[serde(default)]
    pub onboarding_complete: bool,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub settings: AppSettings,
    #[serde(default)]
    pub conversations: Vec<ConversationMeta>,
}

/// Values that must never reach the webview.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Secrets {
    /// provider id -> api key
    #[serde(default)]
    pub provider_keys: HashMap<String, String>,
    /// server id -> static bearer token
    #[serde(default)]
    pub server_tokens: HashMap<String, String>,
    /// server id -> OAuth token set
    #[serde(default)]
    pub oauth_tokens: HashMap<String, OAuthTokens>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix millis when the access token expires (best effort).
    pub expires_at_ms: Option<i64>,
    pub client_id: String,
    pub issuer: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Provider presets
// ---------------------------------------------------------------------------

/// A built-in provider preset. Any other OpenAI- or Anthropic-compatible
/// service can be added through the `custom` preset.
#[derive(Debug, Serialize)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub tagline: &'static str,
    pub base_url: &'static str,
    pub api_type: ApiType,
    pub needs_key: bool,
    pub key_url: &'static str,
    pub default_model: &'static str,
}

/// Built-in provider presets. Any other OpenAI- or Anthropic-compatible
/// service can be added through the `custom` preset.
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        tagline: "GPT models from platform.openai.com",
        base_url: "https://api.openai.com/v1",
        api_type: ApiType::OpenAi,
        needs_key: true,
        key_url: "https://platform.openai.com/api-keys",
        default_model: "",
    },
    ProviderPreset {
        id: "anthropic",
        label: "Claude (Anthropic)",
        tagline: "Claude models from console.anthropic.com",
        base_url: "https://api.anthropic.com/v1",
        api_type: ApiType::Anthropic,
        needs_key: true,
        key_url: "https://console.anthropic.com/settings/keys",
        default_model: "",
    },
    ProviderPreset {
        id: "zai-coding",
        label: "Z.ai Coding Plan",
        tagline: "GLM models via the coding-plan endpoint",
        base_url: "https://api.z.ai/api/coding/paas/v4",
        api_type: ApiType::OpenAi,
        needs_key: true,
        key_url: "https://z.ai/manage-apikey/apikey-list",
        default_model: "",
    },
    ProviderPreset {
        id: "zai",
        label: "Z.ai",
        tagline: "GLM models via the standard endpoint",
        base_url: "https://api.z.ai/api/paas/v4",
        api_type: ApiType::OpenAi,
        needs_key: true,
        key_url: "https://z.ai/manage-apikey/apikey-list",
        default_model: "",
    },
    ProviderPreset {
        id: "opencode",
        label: "OpenCode Zen",
        tagline: "Models from the OpenCode gateway",
        base_url: "https://opencode.ai/zen/v1",
        api_type: ApiType::OpenAi,
        needs_key: true,
        key_url: "https://opencode.ai/auth",
        default_model: "",
    },
    ProviderPreset {
        id: "ollama",
        label: "Ollama",
        tagline: "Local models on this computer",
        base_url: "http://localhost:11434/v1",
        api_type: ApiType::OpenAi,
        needs_key: false,
        key_url: "",
        default_model: "",
    },
    ProviderPreset {
        id: "lmstudio",
        label: "LM Studio",
        tagline: "Local models via LM Studio's server",
        base_url: "http://localhost:1234/v1",
        api_type: ApiType::OpenAi,
        needs_key: false,
        key_url: "",
        default_model: "",
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        tagline: "Hundreds of models behind one key",
        base_url: "https://openrouter.ai/api/v1",
        api_type: ApiType::OpenAi,
        needs_key: true,
        key_url: "https://openrouter.ai/keys",
        default_model: "",
    },
    ProviderPreset {
        id: "groq",
        label: "Groq",
        tagline: "Very fast open models",
        base_url: "https://api.groq.com/openai/v1",
        api_type: ApiType::OpenAi,
        needs_key: true,
        key_url: "https://console.groq.com/keys",
        default_model: "",
    },
    ProviderPreset {
        id: "custom",
        label: "Custom provider",
        tagline: "Any OpenAI- or Anthropic-compatible endpoint",
        base_url: "",
        api_type: ApiType::OpenAi,
        needs_key: false,
        key_url: "",
        default_model: "",
    },
];

pub fn preset_by_id(id: &str) -> Option<&'static ProviderPreset> {
    PROVIDER_PRESETS.iter().find(|p| p.id == id)
}

/// Popular MCP servers offered as one-click installs.
pub const CONNECTOR_SUGGESTIONS: &[ConnectorSuggestion] = &[
    ConnectorSuggestion {
        name: "Everything (demo)",
        description: "A friendly demo server that exercises every MCP feature — great first connection.",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-everything"],
    },
    ConnectorSuggestion {
        name: "Filesystem",
        description: "Let the AI read and search files in a folder you choose.",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-filesystem", "~"],
    },
    ConnectorSuggestion {
        name: "Fetch",
        description: "Let the AI fetch pages from the web and read them for you.",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-fetch"],
    },
    ConnectorSuggestion {
        name: "Memory",
        description: "A simple knowledge graph the AI can remember things in.",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-memory"],
    },
];

pub struct ConnectorSuggestion {
    pub name: &'static str,
    pub description: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

pub struct Store {
    config_path: PathBuf,
    secrets_path: PathBuf,
    conversations_dir: PathBuf,
    pub config: Mutex<AppConfig>,
    pub secrets: Mutex<Secrets>,
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

impl Store {
    pub fn new(base: &Path) -> anyhow::Result<Self> {
        let config_path = base.join("config.json");
        let secrets_path = base.join("secrets.json");
        let conversations_dir = base.join("conversations");
        std::fs::create_dir_all(&conversations_dir)?;

        let config = if config_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&config_path)?).unwrap_or_default()
        } else {
            AppConfig { version: 1, ..Default::default() }
        };
        let secrets: Secrets = if secrets_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&secrets_path)?).unwrap_or_default()
        } else {
            Secrets::default()
        };

        Ok(Self {
            config_path,
            secrets_path,
            conversations_dir,
            config: Mutex::new(config),
            secrets: Mutex::new(secrets),
        })
    }

    pub fn save_config(&self) -> anyhow::Result<()> {
        let cfg = { self.config.lock().unwrap().clone() };
        write_private(
            &self.config_path,
            &serde_json::to_string_pretty(&cfg)?,
        )?;
        Ok(())
    }

    pub fn save_secrets(&self) -> anyhow::Result<()> {
        let s = { self.secrets.lock().unwrap().clone() };
        write_private(&self.secrets_path, &serde_json::to_string_pretty(&s)?)?;
        Ok(())
    }

    // -- secrets ---------------------------------------------------------

    pub fn provider_key(&self, id: &str) -> Option<String> {
        self.secrets.lock().unwrap().provider_keys.get(id).cloned()
    }

    pub fn set_provider_key(&self, id: &str, key: Option<&str>) -> anyhow::Result<()> {
        let mut s = self.secrets.lock().unwrap();
        match key {
            Some(k) if !k.is_empty() => {
                s.provider_keys.insert(id.to_string(), k.to_string());
            }
            _ => {
                s.provider_keys.remove(id);
            }
        }
        drop(s);
        self.save_secrets()
    }

    pub fn server_token(&self, id: &str) -> Option<String> {
        self.secrets.lock().unwrap().server_tokens.get(id).cloned()
    }

    pub fn oauth_tokens(&self, id: &str) -> Option<OAuthTokens> {
        self.secrets.lock().unwrap().oauth_tokens.get(id).cloned()
    }

    pub fn set_oauth_tokens(&self, id: &str, tokens: Option<OAuthTokens>) -> anyhow::Result<()> {
        let mut s = self.secrets.lock().unwrap();
        match tokens {
            Some(t) => {
                s.oauth_tokens.insert(id.to_string(), t);
            }
            None => {
                s.oauth_tokens.remove(id);
            }
        }
        drop(s);
        self.save_secrets()
    }

    // -- conversations ----------------------------------------------------

    pub fn conversation_path(&self, id: &str) -> PathBuf {
        // ids are uuids; guard against path traversal anyway
        let safe: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        self.conversations_dir.join(format!("{safe}.json"))
    }

    pub fn save_conversation(&self, meta: &ConversationMeta, messages: &[serde_json::Value]) -> anyhow::Result<()> {
        let payload = serde_json::json!({ "meta": meta, "messages": messages });
        write_private(&self.conversation_path(&meta.id), &serde_json::to_string_pretty(&payload)?)?;
        Ok(())
    }

    pub fn load_conversation(&self, id: &str) -> Option<(ConversationMeta, Vec<serde_json::Value>)> {
        let raw = std::fs::read_to_string(self.conversation_path(id)).ok()?;
        let payload: serde_json::Value = serde_json::from_str(&raw).ok()?;
        let meta = serde_json::from_value(payload.get("meta")?.clone()).ok()?;
        let messages = payload
            .get("messages")?
            .as_array()?
            .clone();
        Some((meta, messages))
    }

    pub fn delete_conversation(&self, id: &str) -> anyhow::Result<()> {
        let path = self.conversation_path(id);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let mut cfg = self.config.lock().unwrap();
        cfg.conversations.retain(|c| c.id != id);
        drop(cfg);
        self.save_config()
    }

    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }
}

// ---------------------------------------------------------------------------
// Import: accept configs from other MCP clients (Claude Desktop, Cursor, ...)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ImportedServer {
    pub name: String,
    pub transport: McpTransport,
}

/// Parse the common `{"mcpServers": {...}}` shape used by most MCP clients.
pub fn parse_import_json(text: &str) -> anyhow::Result<Vec<ImportedServer>> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| anyhow::anyhow!("That doesn't look like valid JSON: {e}"))?;

    // unwrap mcpServers wrapper if present
    let servers = value
        .get("mcpServers")
        .or_else(|| value.get("servers"))
        .unwrap_or(&value);

    let map = servers
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Expected an object mapping server names to settings"))?;

    let mut out = Vec::new();
    for (name, spec) in map {
        let transport = if let Some(url) = spec.get("url").and_then(|u| u.as_str()) {
            McpTransport::Http {
                url: url.to_string(),
                headers: headers_from_json(spec.get("headers")),
            }
        } else if let Some(cmd) = spec.get("command").and_then(|c| c.as_str()) {
            let args = spec
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let env = spec
                .get("env")
                .and_then(|e| e.as_object())
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            McpTransport::Stdio { command: cmd.to_string(), args, env }
        } else {
            continue;
        };
        out.push(ImportedServer { name: name.clone(), transport });
    }
    if out.is_empty() {
        return Err(anyhow::anyhow!("No servers found in that JSON"));
    }
    Ok(out)
}

fn headers_from_json(v: Option<&serde_json::Value>) -> HashMap<String, String> {
    v.and_then(|h| h.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path()).unwrap();
        {
            let mut cfg = store.config.lock().unwrap();
            cfg.onboarding_complete = true;
            cfg.providers.push(ProviderConfig {
                id: "p1".into(),
                kind: "openai".into(),
                name: "OpenAI".into(),
                base_url: "https://api.openai.com/v1".into(),
                api_type: ApiType::OpenAi,
                default_model: None,
                models: vec!["gpt-test".into()],
                created_at: "now".into(),
            });
            cfg.mcp_servers.push(McpServerConfig {
                id: "s1".into(),
                name: "demo".into(),
                transport: McpTransport::Stdio {
                    command: "demo".into(),
                    args: vec![],
                    env: HashMap::new(),
                },
                auth: HttpAuth::None,
                enabled: true,
                auto_start: true,
                oauth_client_id: None,
                oauth_redirect_port: None,
                created_at: "now".into(),
            });
        }
        store.save_config().unwrap();
        store.set_provider_key("p1", Some("sk-test")).unwrap();

        let store2 = Store::new(tmp.path()).unwrap();
        let cfg = store2.config.lock().unwrap();
        assert!(cfg.onboarding_complete);
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.providers[0].models, vec!["gpt-test"]);
        assert_eq!(store2.provider_key("p1").as_deref(), Some("sk-test"));
        assert!(store2.provider_key("nope").is_none());
    }

    #[test]
    fn imports_claude_desktop_style_config() {
        let json = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
                },
                "remote": { "url": "https://example.com/mcp" }
            }
        }"#;
        let imported = parse_import_json(json).unwrap();
        assert_eq!(imported.len(), 2);
        assert!(matches!(&imported[0].transport, McpTransport::Stdio { command, args, .. }
            if command == "npx" && args.len() == 3));
        assert!(matches!(&imported[1].transport, McpTransport::Http { url, .. }
            if url == "https://example.com/mcp"));
    }

    #[test]
    fn conversation_persistence() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path()).unwrap();
        let meta = ConversationMeta {
            id: "abc-123".into(),
            title: "Hi".into(),
            provider_id: "p".into(),
            model: "m".into(),
            effort: Some(EffortLevel::High),
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let messages = vec![serde_json::json!({"role": "user", "text": "hello"})];
        store.save_conversation(&meta, &messages).unwrap();
        let (meta2, msgs2) = store.load_conversation("abc-123").unwrap();
        assert_eq!(meta2.title, "Hi");
        assert_eq!(meta2.effort, Some(EffortLevel::High));
        assert_eq!(msgs2.len(), 1);
        store.delete_conversation("abc-123").unwrap();
        assert!(store.load_conversation("abc-123").is_none());
    }
}
