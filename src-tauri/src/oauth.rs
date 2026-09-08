//! OAuth 2.1 authorization for remote (HTTP) MCP servers.
//!
//! Implements the MCP 2026-07-28 authorization core:
//! - Protected Resource Metadata discovery (RFC 9728)
//! - Authorization Server metadata discovery (RFC 8414 + OIDC)
//! - Dynamic Client Registration (RFC 7591) and pre-registered client ids
//! - PKCE (S256) authorization-code flow with a loopback redirect
//! - `resource` indicator (RFC 8707), `iss` validation (RFC 9207)
//! - Refresh-token handling

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::config::{HttpAuth, McpServerConfig, McpTransport, OAuthTokens, Store};

// ---------------------------------------------------------------------------
// Metadata documents
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Clone)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    resource: Option<String>,
    #[serde(default, rename = "authorization_servers")]
    authorization_servers: Vec<String>,
    #[serde(default, rename = "scopes_supported")]
    scopes_supported: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone)]
struct AuthServerMetadata {
    issuer: String,
    #[serde(default)]
    authorization_endpoint: Option<String>,
    #[serde(default)]
    token_endpoint: Option<String>,
    #[serde(default, rename = "registration_endpoint")]
    registration_endpoint: Option<String>,
    #[serde(default, rename = "scopes_supported")]
    scopes_supported: Option<Vec<String>>,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("oauth http client")
}

/// Canonical resource URI per RFC 8707 (scheme://host[:port]/path, no
/// trailing slash, no fragment).
fn canonical_resource(url: &str) -> anyhow::Result<String> {
    let parsed: url::Url = url.parse()?;
    let mut out = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default()
    );
    if let Some(port) = parsed.port() {
        out.push_str(&format!(":{port}"));
    }
    let path = parsed.path().trim_end_matches('/');
    if !path.is_empty() && path != "/" {
        out.push_str(path);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Why an OAuth session cannot be used right now.
#[derive(Debug, Clone)]
pub enum AuthFailure {
    /// The user must (re-)authorize in the browser. Per the MCP authorization
    /// spec, an invalid or expired refresh token requires restarting the full
    /// authorization flow. Stored tokens, if any, have been cleared.
    ReauthRequired(String),
    /// A temporary failure (network, server 5xx, throttling). Tokens were
    /// kept; the caller may retry later.
    Transient(String),
}

impl std::fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReauthRequired(m) | Self::Transient(m) => write!(f, "{m}"),
        }
    }
}

/// Per-server refresh mutex so concurrent 401s share one refresh round-trip
/// instead of racing refresh-token rotation against the token endpoint.
fn refresh_guard(server_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static GUARDS: std::sync::OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        std::sync::OnceLock::new();
    let map = GUARDS.get_or_init(|| Mutex::new(HashMap::new()));
    map.lock()
        .unwrap()
        .entry(server_id.to_string())
        .or_default()
        .clone()
}

fn token_is_fresh(tokens: &OAuthTokens) -> bool {
    tokens
        .expires_at_ms
        .map(|exp| exp > chrono::Utc::now().timestamp_millis() + 60_000)
        .unwrap_or(true)
}

/// True when the token expires within `lead_ms` (or has unknown expiry).
fn token_expires_within(tokens: &OAuthTokens, lead_ms: i64) -> bool {
    tokens
        .expires_at_ms
        .map(|exp| exp <= chrono::Utc::now().timestamp_millis() + lead_ms)
        .unwrap_or(true)
}

/// Keep stored OAuth tokens fresh: refresh anything close to expiry so
/// long-lived sessions never send a stale token. Runs for the process
/// lifetime.
pub async fn refresh_loop(manager: Arc<crate::mcp::manager::McpManager>) {
    let store = manager.store().clone();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let candidates: Vec<McpServerConfig> = {
            let cfg = store.config.lock().unwrap();
            cfg.mcp_servers
                .iter()
                .filter(|s| {
                    s.enabled
                        && matches!(s.transport, McpTransport::Http { .. })
                        && matches!(s.auth, HttpAuth::OAuth)
                })
                .cloned()
                .collect()
        };
        for cfg in candidates {
            use crate::mcp::manager::ServerStatus;
            if matches!(manager.status(&cfg.id), ServerStatus::NeedsAuth { .. }) {
                // Waiting on the user; stop hammering the token endpoint.
                continue;
            }
            let Some(tokens) = store.oauth_tokens(&cfg.id) else {
                continue;
            };
            if !token_expires_within(&tokens, 5 * 60_000) {
                continue;
            }
            match refresh_now(&store, &cfg, &tokens).await {
                Ok(()) => {}
                Err(AuthFailure::ReauthRequired(detail)) => {
                    manager.mark_needs_auth(
                        &cfg.id,
                        crate::mcp::manager::AuthReason::Expired,
                        Some(detail),
                    );
                }
                Err(AuthFailure::Transient(e)) => {
                    tracing::warn!(
                        server = %cfg.name,
                        "background token refresh failed: {e}"
                    );
                }
            }
        }
    }
}

/// Refresh the stored access token if it is (nearly) expired. Returns
/// `AuthFailure::ReauthRequired` when the user must sign in again and
/// `AuthFailure::Transient` for retryable failures.
pub async fn ensure_fresh_token(
    store: &Arc<Store>,
    cfg: &McpServerConfig,
) -> Result<(), AuthFailure> {
    let Some(tokens) = store.oauth_tokens(&cfg.id) else {
        return Err(AuthFailure::ReauthRequired("Not signed in".to_string()));
    };
    if token_is_fresh(&tokens) {
        return Ok(());
    }
    refresh_now(store, cfg, &tokens).await
}

/// Unconditionally refresh the stored tokens. Used by the transport wrapper
/// when the server has just rejected an access token (401) that may still
/// look fresh locally.
pub async fn refresh_now(
    store: &Arc<Store>,
    cfg: &McpServerConfig,
    tokens: &OAuthTokens,
) -> Result<(), AuthFailure> {
    let guard = refresh_guard(&cfg.id);
    let _held = guard.lock().await;

    // Another refresh may have completed while waiting for the guard.
    if let Some(current) = store.oauth_tokens(&cfg.id) {
        if current.access_token != tokens.access_token {
            return Ok(());
        }
    } else {
        return Err(AuthFailure::ReauthRequired("Not signed in".to_string()));
    }

    let Some(refresh_token) = tokens.refresh_token.clone() else {
        store.set_oauth_tokens(&cfg.id, None).ok();
        return Err(AuthFailure::ReauthRequired(
            "Session expired — sign in again".to_string(),
        ));
    };
    let mcp_url = match &cfg.transport {
        McpTransport::Http { url, .. } => url.clone(),
        _ => return Ok(()),
    };
    let resource =
        canonical_resource(&mcp_url).map_err(|e| AuthFailure::Transient(e.to_string()))?;
    let meta = discover_auth_server(&mcp_url, &tokens.issuer)
        .await
        .map_err(|e| {
            AuthFailure::Transient(format!(
                "Reconnecting to the authorization server failed: {e}"
            ))
        })?;

    let Some(token_endpoint) = meta.token_endpoint.clone() else {
        return Err(AuthFailure::Transient(
            "Authorization server has no token endpoint".to_string(),
        ));
    };

    let secret = store.oauth_client_secret(&cfg.id);
    let mut params: HashMap<&str, &str> = HashMap::from([
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", tokens.client_id.as_str()),
        ("resource", resource.as_str()),
    ]);
    if let Some(secret) = &secret {
        params.insert("client_secret", secret.as_str());
    }
    let response = client()
        .post(&token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| AuthFailure::Transient(format!("Token refresh failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let code = token_error_code(&body);
        if is_definitive_rejection(status, code.as_deref()) {
            store.set_oauth_tokens(&cfg.id, None).ok();
            return Err(AuthFailure::ReauthRequired(reauth_message(code.as_deref())));
        }
        return Err(AuthFailure::Transient(format!(
            "Token refresh failed ({status}); Ducky will retry — {}",
            truncate(&body, 160)
        )));
    }
    let granted: TokenResponse = response
        .json()
        .await
        .map_err(|e| AuthFailure::Transient(e.to_string()))?;
    let updated = OAuthTokens {
        access_token: granted.access_token,
        refresh_token: granted.refresh_token.or(Some(refresh_token)),
        expires_at_ms: granted
            .expires_in
            .map(|s| chrono::Utc::now().timestamp_millis() + (s - 30) * 1000),
        client_id: tokens.client_id.clone(),
        issuer: tokens.issuer.clone(),
        scopes: granted
            .scope
            .map(|s| s.split(' ').map(String::from).collect())
            .unwrap_or_default(),
    };
    store
        .set_oauth_tokens(&cfg.id, Some(updated))
        .map_err(|e| AuthFailure::Transient(e.to_string()))
}

/// Run the full browser-based authorization flow for a server.
pub async fn login(
    app: tauri::AppHandle,
    store: Arc<Store>,
    sink: Arc<dyn crate::events::EventSink>,
    server_id: String,
) -> Result<(), String> {
    let cfg = {
        let cfg_guard = store.config.lock().unwrap();
        cfg_guard
            .mcp_servers
            .iter()
            .find(|s| s.id == server_id)
            .cloned()
            .ok_or("Unknown server")?
    };
    let mcp_url = match &cfg.transport {
        McpTransport::Http { url, .. } => url.clone(),
        McpTransport::Stdio { .. } => return Err("OAuth only applies to remote servers".into()),
    };

    // 1. Discover the protected-resource metadata (and required scopes).
    let (prm, scope) = discover_resource(&mcp_url).await?;

    // 2. Discover an authorization server.
    let as_url = prm
        .authorization_servers
        .first()
        .cloned()
        .ok_or("The server did not advertise an authorization server")?;
    let meta = discover_auth_server(&mcp_url, &as_url).await?;
    let authorization_endpoint = meta
        .authorization_endpoint
        .clone()
        .ok_or("Authorization server has no authorization endpoint")?;
    let token_endpoint = meta
        .token_endpoint
        .clone()
        .ok_or("Authorization server has no token endpoint")?;

    // 3. Bind first so dynamic registration sees the real loopback port
    // (0 = OS-assigned).
    let port = redirect_port(&cfg);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| {
            format!("Could not listen on 127.0.0.1:{port} for the OAuth callback: {e}.")
        })?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    // 4. Obtain a client id (pre-registered or dynamic registration).
    let client_id = match &cfg.oauth_client_id {
        Some(id) => id.clone(),
        None => {
            let endpoint = meta.registration_endpoint.clone().ok_or_else(|| {
                "This server requires a pre-registered client id (paste it in the \
                     connector's advanced settings)"
                    .to_string()
            })?;
            dynamic_register(&endpoint, port).await?
        }
    };

    // 5. PKCE + state.
    let mut verifier_bytes = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let state = crate::config::Store::new_id();
    let resource = canonical_resource(&mcp_url).map_err(|e| e.to_string())?;

    // scope selection: challenge scope first, else scopes_supported; add
    // `offline_access` when the authorization server offers it so refresh
    // tokens are issued where possible.
    let mut scope = scope
        .or_else(|| prm.scopes_supported.clone().map(|s| s.join(" ")))
        .or_else(|| meta.scopes_supported.clone().map(|s| s.join(" ")));
    if meta
        .scopes_supported
        .as_ref()
        .is_some_and(|s| s.iter().any(|sc| sc == "offline_access"))
    {
        let has_it = scope
            .as_deref()
            .map(|s| s.split(' ').any(|sc| sc == "offline_access"))
            .unwrap_or(false);
        if !has_it {
            scope = Some(match scope {
                Some(s) => format!("{s} offline_access"),
                None => "offline_access".to_string(),
            });
        }
    }
    let scope = scope;

    // 6. Open the browser.
    let mut auth_url = url::Url::parse(&authorization_endpoint).map_err(|e| e.to_string())?;
    {
        let mut q = auth_url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", &client_id);
        q.append_pair("redirect_uri", &redirect_uri);
        q.append_pair("state", &state);
        q.append_pair("code_challenge", &challenge);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("resource", &resource);
        if let Some(s) = &scope {
            q.append_pair("scope", s);
        }
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(auth_url.as_str(), None::<&str>)
        .map_err(|e| format!("Could not open the browser: {e}"))?;

    // 7. Wait for the callback.
    let (code, iss) = wait_for_callback(listener, &state)
        .await
        .map_err(|e| format!("Authorization failed: {e}"))?;

    // 8. Exchange the code for tokens.
    let client_secret = store.oauth_client_secret(&server_id);
    let mut params: HashMap<&str, &str> = HashMap::from([
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
        ("resource", resource.as_str()),
    ]);
    if let Some(secret) = &client_secret {
        params.insert("client_secret", secret.as_str());
    }
    let response = client()
        .post(&token_endpoint)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Token exchange failed: {e}"))?;
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Token exchange failed: {}", truncate(&body, 300)));
    }
    let granted: TokenResponse = response.json().await.map_err(|e| e.to_string())?;

    let tokens = OAuthTokens {
        access_token: granted.access_token,
        refresh_token: granted.refresh_token,
        expires_at_ms: granted
            .expires_in
            .map(|s| chrono::Utc::now().timestamp_millis() + (s - 30) * 1000),
        client_id,
        issuer: iss.unwrap_or(meta.issuer.clone()),
        scopes: granted
            .scope
            .map(|s| s.split(' ').map(String::from).collect())
            .unwrap_or_default(),
    };
    store
        .set_oauth_tokens(&server_id, Some(tokens))
        .map_err(|e| e.to_string())?;

    sink.emit(crate::events::BackendEvent::ServerStatus {
        server_id,
        status: "connecting".into(),
        detail: None,
        reason: None,
    });
    Ok(())
}

pub fn logout(store: &Arc<Store>, server_id: &str) -> Result<(), String> {
    store
        .set_oauth_tokens(server_id, None)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Discovery helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

fn redirect_port(cfg: &McpServerConfig) -> u16 {
    cfg.oauth_redirect_port.unwrap_or(0)
}

fn callback_success_html() -> &'static str {
    concat!(
        "<!DOCTYPE html>\n",
        "<html lang=\"en\">\n",
        "<head>\n",
        "<meta charset=\"utf-8\">\n",
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
        "<title>Signed in \u{2014} Ducky</title>\n",
        "<style>\n",
        ":root{--bg:#f8fafc;--card:#fff;--border:#e2e8f0;--text:#0f172a;--muted:#64748b;--accent:#0ea5e9;--ok:#22c55e;--ok-ring:#bbf7d0}\n",
        "@media(prefers-color-scheme:dark){:root{--bg:#0f172a;--card:#1e293b;--border:#334155;--text:#f8fafc;--muted:#94a3b8;--ok-ring:#14532d}}\n",
        "*{box-sizing:border-box}html,body{height:100%;margin:0}\n",
        "body{font-family:ui-sans-serif,system-ui,-apple-system,sans-serif;background:var(--bg);color:var(--text);display:grid;place-items:center;padding:24px}\n",
        "main{width:min(100%,28rem);background:var(--card);border:1px solid var(--border);border-radius:1.25rem;padding:2.5rem 2rem;text-align:center;box-shadow:0 10px 15px -3px rgb(15 23 42 / 0.08)}\n",
        ".mark{width:3.5rem;height:3.5rem;margin:0 auto 1.25rem;border-radius:999px;background:var(--ok-ring);display:grid;place-items:center}\n",
        ".mark svg{width:1.75rem;height:1.75rem}h1{margin:0 0 .5rem;font-size:1.375rem;letter-spacing:-.02em}p{margin:0;color:var(--muted);line-height:1.5}\n",
        ".brand{margin-top:1.75rem;font-size:.75rem;font-weight:600;letter-spacing:.08em;text-transform:uppercase;color:var(--accent)}\n",
        "</style>\n",
        "</head>\n",
        "<body>\n",
        "<main>\n",
        "<div class=\"mark\" aria-hidden=\"true\">\n",
        "<svg viewBox=\"0 0 24 24\" fill=\"none\">\n",
        "<circle cx=\"12\" cy=\"12\" r=\"12\" fill=\"#22c55e\"/>\n",
        "<path d=\"M7 12.5l3.2 3.2L17 8.8\" stroke=\"#fff\" stroke-width=\"2.2\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>\n",
        "</svg>\n",
        "</div>\n",
        "<h1>You're signed in</h1>\n",
        "<p>You can close this tab and return to Ducky.</p>\n",
        "<div class=\"brand\">Ducky</div>\n",
        "</main>\n",
        "</body>\n",
        "</html>\n",
    )
}

/// Fetch Protected Resource Metadata for the MCP server, following the
/// `WWW-Authenticate` challenge first and well-known paths as fallback.
async fn discover_resource(
    mcp_url: &str,
) -> Result<(ProtectedResourceMetadata, Option<String>), String> {
    let http = client();

    // 1. challenge the server to get the authoritative metadata URL
    let mut scope = None;
    let mut prm_url: Option<String> = None;
    let response = http
        .post(mcp_url)
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", "2026-07-28")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "ping",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        }))
        .send()
        .await;
    if let Ok(response) = response {
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            if let Some(www) = response.headers().get("www-authenticate") {
                let header = www.to_str().unwrap_or_default().to_string();
                scope = parse_www_authenticate_scope(&header);
                prm_url = parse_www_authenticate_resource(&header);
            }
        } else if response.status().is_success() {
            return Err("This server does not seem to require authorization.".into());
        }
    }

    // 2. well-known fallbacks (origin and path variants, RFC 9728 §3.1)
    let parsed: url::Url = mcp_url.parse().map_err(|e| format!("Bad URL: {e}"))?;
    let origin = format!(
        "{}://{}{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default(),
        parsed.port().map(|p| format!(":{p}")).unwrap_or_default()
    );
    let path = parsed.path().trim_end_matches('/');
    let candidates: Vec<String> = match &prm_url {
        Some(u) => vec![u.clone()],
        None => {
            let mut v = vec![
                format!("{origin}/.well-known/oauth-protected-resource{path}"),
                format!("{origin}/.well-known/oauth-protected-resource"),
            ];
            // insert path after well-known prefix per RFC 9728
            v.insert(
                1,
                format!("{origin}/.well-known/oauth-protected-resource/{path}"),
            );
            v
        }
    };

    for candidate in &candidates {
        if let Ok(response) = http.get(candidate).send().await {
            if response.status().is_success() {
                if let Ok(meta) = response.json::<ProtectedResourceMetadata>().await {
                    return Ok((meta, scope));
                }
            }
        }
    }
    Err(
        "Could not find OAuth metadata for this server (no Protected Resource Metadata \
         was returned)."
            .to_string(),
    )
}

async fn discover_auth_server(mcp_url: &str, issuer: &str) -> Result<AuthServerMetadata, String> {
    let _ = mcp_url;
    let http = client();
    let parsed: url::Url = issuer.parse().map_err(|e| format!("Bad issuer URL: {e}"))?;
    let path = parsed.path().trim_end_matches('/').to_string();
    let mut candidates = vec![
        format!("{issuer}/.well-known/oauth-authorization-server"),
        format!("{issuer}/.well-known/openid-configuration"),
    ];
    if !path.is_empty() {
        candidates.insert(
            1,
            format!(
                "{}://{}{}/.well-known/oauth-authorization-server{}",
                parsed.scheme(),
                parsed.host_str().unwrap_or_default(),
                path,
                path
            ),
        );
    }
    for candidate in &candidates {
        if let Ok(response) = http.get(candidate).send().await {
            if response.status().is_success() {
                if let Ok(meta) = response.json::<AuthServerMetadata>().await {
                    return Ok(meta);
                }
            }
        }
    }
    Err("Could not fetch authorization server metadata".into())
}

async fn dynamic_register(registration_endpoint: &str, port: u16) -> Result<String, String> {
    #[derive(serde::Serialize)]
    struct RegistrationRequest {
        client_name: String,
        redirect_uris: Vec<String>,
        grant_types: Vec<String>,
        response_types: Vec<String>,
        token_endpoint_auth_method: String,
        application_type: String,
    }
    let body = RegistrationRequest {
        client_name: "Ducky".into(),
        redirect_uris: vec![format!("http://127.0.0.1:{port}/callback")],
        grant_types: vec!["authorization_code".into(), "refresh_token".into()],
        response_types: vec!["code".into()],
        token_endpoint_auth_method: "none".into(),
        application_type: "native".into(),
    };
    let response = client()
        .post(registration_endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Dynamic client registration failed: {e}"))?;
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(format!(
            "Dynamic client registration failed: {} — you can register Ducky manually and \
             paste the client id in the connector's advanced settings.",
            truncate(&text, 250)
        ));
    }
    #[derive(Deserialize)]
    struct RegistrationResponse {
        client_id: String,
    }
    let registered: RegistrationResponse = response.json().await.map_err(|e| e.to_string())?;
    Ok(registered.client_id)
}

/// Listen for exactly one GET on the loopback listener; returns (code, iss).
async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<(String, Option<String>), String> {
    let timeout = tokio::time::Duration::from_secs(300);
    let (mut stream, _) = tokio::time::timeout(timeout, listener.accept())
        .await
        .map_err(|_| "Timed out waiting for the browser to redirect back".to_string())?
        .map_err(|e| e.to_string())?;

    let mut buf = vec![0u8; 8192];
    let read = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
    let request = String::from_utf8_lossy(&buf[..read]).to_string();
    let request_line = request.lines().next().unwrap_or_default().to_string();
    // e.g. GET /callback?code=...&state=...&iss=... HTTP/1.1
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let parsed: url::Url = format!("http://127.0.0.1{target}")
        .parse()
        .map_err(|_| "Malformed redirect".to_string())?;

    let mut code = None;
    let mut state = None;
    let mut iss = None;
    let mut error: Option<String> = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.to_string()),
            "state" => state = Some(v.to_string()),
            "iss" => iss = Some(v.to_string()),
            "error" => error = Some(v.to_string()),
            _ => {}
        }
    }
    if let Some(err) = error {
        return Err(format!("The authorization server returned an error: {err}"));
    }
    if state.as_deref() != Some(expected_state) {
        return Err("State mismatch in the authorization response (possible tampering)".into());
    }
    let Some(code) = code else {
        return Err("No authorization code in the redirect".into());
    };

    let body = callback_success_html();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
    Ok((code, iss))
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Extract the `error` code from an RFC 6749 §5.2 token endpoint error body.
fn token_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .as_str()
        .map(String::from)
}

/// True when the authorization server definitively rejected the client or the
/// refresh token: the only remedy is a full re-authorization. Everything else
/// (network hiccups, 5xx, throttling) is transient and must not clear tokens.
fn is_definitive_rejection(status: reqwest::StatusCode, error_code: Option<&str>) -> bool {
    match error_code {
        Some("invalid_grant") | Some("invalid_client") | Some("invalid_scope") => true,
        Some("temporarily_unavailable")
        | Some("slow_down")
        | Some("authorization_pending")
        | Some("server_error") => false,
        // Unknown codes and unparseable bodies: only an explicit client
        // rejection status is definitive (token endpoints normally answer 400,
        // and a 5xx is never the user's fault).
        _ => {
            status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
        }
    }
}

fn reauth_message(code: Option<&str>) -> String {
    match code {
        Some("invalid_grant") => "Your session expired — sign in again".to_string(),
        Some("invalid_client") => {
            "This app is no longer registered with the server's sign-in — sign in again".to_string()
        }
        Some("invalid_scope") => {
            "The server needs different permissions — sign in again to grant them".to_string()
        }
        _ => "Sign in again".to_string(),
    }
}

fn parse_www_authenticate_param(header: &str, name: &str) -> Option<String> {
    let idx = header.find(&format!("{name}="))?;
    let rest = header[idx + name.len() + 1..].trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"')?;
        Some(stripped[..end].to_string())
    } else {
        let end = rest.find(',').unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}

fn parse_www_authenticate_resource(header: &str) -> Option<String> {
    parse_www_authenticate_param(header, "resource_metadata")
}

fn parse_www_authenticate_scope(header: &str) -> Option<String> {
    parse_www_authenticate_param(header, "scope")
}

fn truncate(s: &str, n: usize) -> &str {
    if s.len() > n {
        &s[..n]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalises_resources() {
        assert_eq!(
            canonical_resource("https://mcp.example.com/mcp").unwrap(),
            "https://mcp.example.com/mcp"
        );
        assert_eq!(
            canonical_resource("https://mcp.example.com/").unwrap(),
            "https://mcp.example.com"
        );
        assert_eq!(
            canonical_resource("https://MCP.Example.COM:8443/mcp/").unwrap(),
            "https://mcp.example.com:8443/mcp"
        );
    }

    #[test]
    fn parses_www_authenticate() {
        let header = r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="files:read""#;
        assert_eq!(
            parse_www_authenticate_resource(header).as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(
            parse_www_authenticate_scope(header).as_deref(),
            Some("files:read")
        );
    }

    #[test]
    fn pkce_shapes() {
        let mut v = [0u8; 48];
        rand::thread_rng().fill_bytes(&mut v);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge.len(), 43);
    }

    #[test]
    fn classifies_token_errors() {
        use reqwest::StatusCode;
        // Definitive rejections: full re-authorization required.
        assert!(is_definitive_rejection(
            StatusCode::BAD_REQUEST,
            Some("invalid_grant")
        ));
        assert!(is_definitive_rejection(
            StatusCode::BAD_REQUEST,
            Some("invalid_client")
        ));
        assert!(is_definitive_rejection(
            StatusCode::BAD_REQUEST,
            Some("invalid_scope")
        ));
        assert!(is_definitive_rejection(StatusCode::UNAUTHORIZED, None));
        // Transient: tokens must survive these.
        assert!(!is_definitive_rejection(
            StatusCode::BAD_REQUEST,
            Some("slow_down")
        ));
        assert!(!is_definitive_rejection(
            StatusCode::BAD_REQUEST,
            Some("temporarily_unavailable")
        ));
        assert!(!is_definitive_rejection(StatusCode::BAD_REQUEST, None));
        assert!(!is_definitive_rejection(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("invalid_request")
        ));
    }

    #[test]
    fn reads_token_error_codes() {
        assert_eq!(
            token_error_code(r#"{"error":"invalid_grant","error_description":"expired"}"#)
                .as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(token_error_code("not json"), None);
        assert_eq!(token_error_code(r#"{"error":42}"#), None);
    }

    #[test]
    fn callback_port_defaults_to_ephemeral() {
        let cfg = McpServerConfig {
            id: "s".into(),
            name: "s".into(),
            transport: McpTransport::Http {
                url: "https://example.com/mcp".into(),
                headers: HashMap::new(),
            },
            auth: HttpAuth::OAuth,
            enabled: true,
            auto_start: true,
            oauth_client_id: None,
            oauth_redirect_port: None,
            created_at: "t".into(),
        };
        assert_eq!(redirect_port(&cfg), 0);
        let mut cfg = cfg;
        cfg.oauth_redirect_port = Some(6274);
        assert_eq!(redirect_port(&cfg), 6274);
    }

    #[tokio::test]
    async fn ensure_fresh_token_expired_does_not_deadlock() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(tmp.path(), tmp.path().to_path_buf()).unwrap());
        let cfg = McpServerConfig {
            id: "s".into(),
            name: "s".into(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:1/mcp".into(),
                headers: HashMap::new(),
            },
            auth: HttpAuth::OAuth,
            enabled: true,
            auto_start: true,
            oauth_client_id: None,
            oauth_redirect_port: None,
            created_at: "t".into(),
        };
        store
            .set_oauth_tokens(
                "s",
                Some(OAuthTokens {
                    access_token: "a".into(),
                    refresh_token: Some("r".into()),
                    expires_at_ms: Some(1),
                    client_id: "c".into(),
                    issuer: "http://127.0.0.1:1".into(),
                    scopes: vec![],
                }),
            )
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            ensure_fresh_token(&store, &cfg),
        )
        .await;
        assert!(result.is_ok(), "deadlocked on refresh_guard");
    }

    #[test]
    fn callback_success_html_is_a_full_page() {
        let html = callback_success_html();
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<title>Signed in \u{2014} Ducky</title>"));
        assert!(html.contains("You're signed in"));
        assert!(html.contains("You can close this tab and return to Ducky."));
    }
}
