//! Shared application state passed to every Tauri command.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, ConversationRuntime};
use crate::config::Store;
use crate::events::{BackendEvent, EventSink};
use crate::mcp::bridge::InteractiveBridge;
use crate::mcp::manager::McpManager;

/// Emits backend events to the webview over the `backend://event` channel.
pub struct TauriSink {
    pub app: tauri::AppHandle,
}

impl EventSink for TauriSink {
    fn emit(&self, event: BackendEvent) {
        use tauri::Emitter;
        let _ = self.app.emit("backend://event", &event);
    }
}

pub struct AppState {
    pub store: Arc<Store>,
    pub sink: Arc<dyn EventSink>,
    pub bridge: Arc<InteractiveBridge>,
    pub manager: Arc<McpManager>,
    pub agent: Arc<Agent>,
    /// models.dev catalog for per-model effort levels and wire protocols.
    pub catalog: Arc<crate::catalog::Catalog>,
    /// Cancellation tokens for conversations that are generating a reply.
    pub runtimes: Mutex<HashMap<String, Arc<ConversationRuntime>>>,
    /// Cancellation tokens for in-flight chat-title generation.
    pub title_runtimes: Mutex<HashMap<String, CancellationToken>>,
    /// Conversations currently being compacted (`/compact`); `chat_send` and
    /// `/undo` refuse to run for these until the summary lands.
    pub compacting: Mutex<HashSet<String>>,
    /// Per-conversation PTY terminals (one shell per chat session).
    pub terminals: crate::terminal::TerminalMap,
}

impl AppState {
    pub fn build(
        store: Arc<Store>,
        sink: Arc<dyn EventSink>,
        data_dir: &std::path::Path,
    ) -> Arc<Self> {
        let bridge = Arc::new(InteractiveBridge::new(sink.clone(), store.clone()));
        let manager = Arc::new(McpManager::new(store.clone(), bridge.clone(), sink.clone()));
        let catalog = Arc::new(crate::catalog::Catalog::new(
            &data_dir.join("models-dev.json"),
        ));
        let agent = Arc::new(Agent {
            store: store.clone(),
            manager: manager.clone(),
            bridge: bridge.clone(),
            sink: sink.clone(),
            catalog: catalog.clone(),
        });
        Arc::new(Self {
            store,
            sink,
            bridge,
            manager,
            agent,
            catalog,
            runtimes: Mutex::new(HashMap::new()),
            title_runtimes: Mutex::new(HashMap::new()),
            compacting: Mutex::new(HashSet::new()),
            terminals: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}
