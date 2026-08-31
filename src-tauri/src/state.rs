//! Shared application state passed to every Tauri command.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    /// Cancellation tokens for conversations that are generating.
    pub runtimes: Mutex<HashMap<String, Arc<ConversationRuntime>>>,
}

impl AppState {
    pub fn build(store: Arc<Store>, sink: Arc<dyn EventSink>) -> Arc<Self> {
        let bridge = Arc::new(InteractiveBridge::new(sink.clone(), store.clone()));
        let manager = Arc::new(McpManager::new(store.clone(), bridge.clone(), sink.clone()));
        let agent = Arc::new(Agent {
            store: store.clone(),
            manager: manager.clone(),
            bridge: bridge.clone(),
            sink: sink.clone(),
        });
        Arc::new(Self {
            store,
            sink,
            bridge,
            manager,
            agent,
            runtimes: Mutex::new(HashMap::new()),
        })
    }
}
