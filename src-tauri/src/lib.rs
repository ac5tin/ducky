//! Ducky — a friendly cross-platform MCP client.

pub mod agent;
pub mod builtin;
pub mod catalog;
pub mod commands;
pub mod config;
pub mod events;
pub mod mcp;
pub mod oauth;
pub mod providers;
pub mod state;
pub mod terminal;
pub mod title;

use std::sync::Arc;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // reqwest is built without a default TLS provider; install the ring one.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring provider");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            use tauri::Manager;
            let handle = app.handle().clone();
            let data_dir = handle.path().app_data_dir().expect("app data dir");
            std::fs::create_dir_all(&data_dir).ok();
            let home_dir = handle.path().home_dir().unwrap_or_else(|_| {
                std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| data_dir.clone())
            });
            let store = Arc::new(config::Store::new(&data_dir, home_dir).expect("config store"));
            let sink: Arc<dyn events::EventSink> = Arc::new(state::TauriSink {
                app: handle.clone(),
            });
            let app_state = state::AppState::build(store, sink, &data_dir);
            app.manage(app_state.clone());

            // Auto-connect enabled MCP servers in the background.
            let manager = app_state.manager.clone();
            tauri::async_runtime::spawn(async move {
                manager.connect_enabled().await;
            });
            // Keep OAuth access tokens fresh so long-lived sessions never
            // send a stale token.
            let manager = app_state.manager.clone();
            tauri::async_runtime::spawn(async move {
                crate::oauth::refresh_loop(manager).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_bootstrap,
            commands::get_config,
            commands::provider_add,
            commands::provider_update,
            commands::provider_delete,
            commands::provider_test,
            commands::provider_has_key,
            commands::settings_set,
            commands::tool_rule_set,
            commands::conversation_create,
            commands::conversation_delete,
            commands::conversation_rename,
            commands::conversation_generate_title,
            commands::conversation_cancel_title,
            commands::conversation_set_model,
            commands::conversation_set_effort,
            commands::conversation_set_mcp_ids,
            commands::conversation_get,
            commands::effort_levels,
            commands::context_limit,
            commands::chat_send,
            commands::chat_cancel,
            commands::terminal_create,
            commands::terminal_write,
            commands::terminal_resize,
            commands::terminal_close,
            commands::approval_respond,
            commands::elicitation_respond,
            commands::sampling_respond,
            commands::mcp_add,
            commands::mcp_update,
            commands::mcp_remove,
            commands::mcp_set_enabled,
            commands::mcp_connect,
            commands::mcp_disconnect,
            commands::mcp_summaries,
            commands::mcp_summary,
            commands::mcp_refresh,
            commands::mcp_read_resource,
            commands::mcp_get_prompt,
            commands::mcp_subscribe_resource,
            commands::mcp_unsubscribe_resource,
            commands::mcp_complete,
            commands::mcp_import_preview,
            commands::mcp_import_add,
            commands::mcp_oauth_login,
            commands::mcp_oauth_logout,
            commands::mcp_set_bearer_token,
            commands::mcp_has_auth,
            commands::mcp_set_oauth_config,
            commands::app_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ducky");
}
