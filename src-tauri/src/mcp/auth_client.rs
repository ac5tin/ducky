//! The transport-level half of OAuth token lifecycle: an HTTP client for
//! rmcp's Streamable HTTP transport that resolves a fresh `Authorization`
//! token for every request and reacts to 401/403 challenges mid-session
//! (refresh + retry once, then surface re-auth requirements).

use std::{borrow::Cow, collections::HashMap, sync::Arc};

use futures::stream::BoxStream;
use futures::StreamExt;
use http::{HeaderName, HeaderValue};
use reqwest::StatusCode;
use rmcp::model::{ClientJsonRpcMessage, JsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, InsufficientScopeError, StreamableHttpClient, StreamableHttpError,
    StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse, SseStream};

/// Python/TS SDKs list JSON first. Ruby servers that pick the first Accept
/// type then return JSON instead of SSE-framing unicode tool results.
const POST_ACCEPT: &str = "application/json, text/event-stream";

use crate::config::{HttpAuth, McpServerConfig, Store};
use crate::oauth::AuthFailure;

use super::manager::AuthReason;

/// Called when a mid-session request determines that the user must sign in
/// (or sign in again). The manager turns this into a `NeedsAuth` status.
pub type AuthNotifier = Arc<dyn Fn(AuthReason, Option<String>) + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum AuthClientError {
    #[error("{0}")]
    ReauthRequired(String),
    #[error("{0}")]
    Transient(String),
}

/// A `StreamableHttpClient` that injects per-request credentials and handles
/// auth challenges from the server.
#[derive(Clone)]
pub struct AuthHttpClient {
    cfg: McpServerConfig,
    store: Arc<Store>,
    notify: AuthNotifier,
    inner: reqwest::Client,
}

fn map_delegate_error(
    err: StreamableHttpError<reqwest::Error>,
) -> StreamableHttpError<AuthClientError> {
    use StreamableHttpError as E;
    match err {
        E::Sse(e) => E::Sse(e),
        E::Io(e) => E::Io(e),
        E::UnexpectedEndOfStream => E::UnexpectedEndOfStream,
        E::UnexpectedServerResponse(m) => E::UnexpectedServerResponse(m),
        E::UnexpectedContentType(ct) => E::UnexpectedContentType(ct),
        E::ServerDoesNotSupportSse => E::ServerDoesNotSupportSse,
        E::ServerDoesNotSupportDeleteSession => E::ServerDoesNotSupportDeleteSession,
        E::TokioJoinError(e) => E::TokioJoinError(e),
        E::Deserialize(e) => E::Deserialize(e),
        E::TransportChannelClosed => E::TransportChannelClosed,
        E::MissingSessionIdInResponse => E::MissingSessionIdInResponse,
        E::SessionExpired => E::SessionExpired,
        E::ReservedHeaderConflict(name) => E::ReservedHeaderConflict(name),
        E::AuthRequired(e) => E::AuthRequired(e),
        E::InsufficientScope(e) => E::InsufficientScope(e),
        E::Client(inner) => E::Client(AuthClientError::Transient(inner.to_string())),
        _ => E::Client(AuthClientError::Transient(
            "unexpected transport error".to_string(),
        )),
    }
}

fn insufficient_scope_detail(err: &InsufficientScopeError) -> String {
    match err.get_required_scope() {
        Some(scope) => format!(
            "The server needs additional permissions ({scope}). Sign in again to grant them."
        ),
        None => "The server needs additional permissions. Sign in again to grant them.".to_string(),
    }
}

fn challenge_detail(err: &AuthRequiredError) -> String {
    let lower = err.www_authenticate_header.to_lowercase();
    if lower.contains("insufficient_scope") {
        insufficient_scope_detail(&InsufficientScopeError::new(
            err.www_authenticate_header.clone(),
            None,
        ))
    } else {
        "The server rejected the access token. Sign in again.".to_string()
    }
}

impl AuthHttpClient {
    pub fn new(cfg: McpServerConfig, store: Arc<Store>, notify: AuthNotifier) -> Self {
        Self {
            cfg,
            store,
            notify,
            inner: reqwest::Client::new(),
        }
    }

    fn notify_auth(&self, reason: AuthReason, detail: Option<String>) {
        (self.notify)(reason, detail);
    }

    /// Resolve the credential for the next request. With `force_refresh` the
    /// stored token is refreshed even when it does not look expired yet —
    /// used right after the server rejected it.
    async fn token(
        &self,
        force_refresh: bool,
    ) -> Result<Option<String>, StreamableHttpError<AuthClientError>> {
        match &self.cfg.auth {
            HttpAuth::None => Ok(None),
            HttpAuth::Bearer { .. } => Ok(self.store.server_token(&self.cfg.id)),
            HttpAuth::OAuth => {
                let had_tokens = self.store.oauth_tokens(&self.cfg.id).is_some();
                if !force_refresh && !had_tokens {
                    return Ok(None);
                }
                let result = if force_refresh {
                    match self.store.oauth_tokens(&self.cfg.id) {
                        Some(tokens) => {
                            crate::oauth::refresh_now(&self.store, &self.cfg, &tokens).await
                        }
                        None => Err(AuthFailure::ReauthRequired("Not signed in".to_string())),
                    }
                } else {
                    crate::oauth::ensure_fresh_token(&self.store, &self.cfg).await
                };
                match result {
                    Ok(()) => Ok(self
                        .store
                        .oauth_tokens(&self.cfg.id)
                        .map(|t| t.access_token)),
                    Err(AuthFailure::ReauthRequired(m)) => {
                        let reason = if had_tokens {
                            AuthReason::Expired
                        } else {
                            AuthReason::Missing
                        };
                        self.notify_auth(reason, Some(m.clone()));
                        Err(StreamableHttpError::Client(
                            AuthClientError::ReauthRequired(m),
                        ))
                    }
                    Err(AuthFailure::Transient(m)) => {
                        Err(StreamableHttpError::Client(AuthClientError::Transient(m)))
                    }
                }
            }
        }
    }

    /// Run `call` with per-request credentials. A 401 triggers one forced
    /// refresh + retry; a definitive refresh rejection or a 403
    /// `insufficient_scope` challenge is surfaced through the notifier and
    /// returned as an error.
    async fn attempt<T, F, Fut>(
        &self,
        mut call: F,
    ) -> Result<T, StreamableHttpError<AuthClientError>>
    where
        F: FnMut(Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<T, StreamableHttpError<reqwest::Error>>>,
    {
        let mut refreshed = false;
        loop {
            let token = self.token(refreshed).await?;
            match call(token).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let is_401 = matches!(&e, StreamableHttpError::AuthRequired(_))
                        || matches!(&e, StreamableHttpError::Client(inner)
                            if inner.status() == Some(StatusCode::UNAUTHORIZED));
                    if is_401 {
                        if refreshed {
                            // Even the refreshed token was rejected — likely an
                            // audience/scope mismatch the user must re-consent to.
                            let detail = match &e {
                                StreamableHttpError::AuthRequired(c) => challenge_detail(c),
                                _ => "The server rejected the access token. Sign in again."
                                    .to_string(),
                            };
                            self.notify_auth(AuthReason::Expired, Some(detail.clone()));
                            return Err(StreamableHttpError::Client(
                                AuthClientError::ReauthRequired(detail),
                            ));
                        }
                        refreshed = true;
                        continue;
                    }
                    if let StreamableHttpError::InsufficientScope(err) = &e {
                        let detail = insufficient_scope_detail(err);
                        self.notify_auth(AuthReason::Scope, Some(detail.clone()));
                        return Err(StreamableHttpError::Client(
                            AuthClientError::ReauthRequired(detail),
                        ));
                    }
                    return Err(map_delegate_error(e));
                }
            }
        }
    }
}

fn parse_json_rpc_error(body: &str) -> Option<ServerJsonRpcMessage> {
    match serde_json::from_str::<ServerJsonRpcMessage>(body) {
        Ok(message @ JsonRpcMessage::Error(_)) => Some(message),
        _ => None,
    }
}

/// Same as rmcp's reqwest `post_message`, except Accept lists JSON first.
async fn post_prefer_json(
    client: &reqwest::Client,
    uri: Arc<str>,
    message: ClientJsonRpcMessage,
    session_id: Option<Arc<str>>,
    token: Option<String>,
    custom_headers: HashMap<HeaderName, HeaderValue>,
) -> Result<StreamableHttpPostResponse, StreamableHttpError<reqwest::Error>> {
    let mut request = client
        .post(uri.as_ref())
        .header(reqwest::header::ACCEPT, POST_ACCEPT);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    for (name, value) in custom_headers {
        request = request.header(name, value);
    }
    let session_was_attached = session_id.is_some();
    if let Some(session_id) = session_id {
        request = request.header("mcp-session-id", session_id.as_ref());
    }
    let response = request.json(&message).send().await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        if let Some(header) = response.headers().get(reqwest::header::WWW_AUTHENTICATE) {
            let header = header
                .to_str()
                .map_err(|_| {
                    StreamableHttpError::UnexpectedServerResponse(Cow::from(
                        "invalid www-authenticate header value",
                    ))
                })?
                .to_string();
            return Err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
                header,
            )));
        }
    }
    if response.status() == StatusCode::FORBIDDEN {
        if let Some(header) = response.headers().get(reqwest::header::WWW_AUTHENTICATE) {
            let header_str = header.to_str().map_err(|_| {
                StreamableHttpError::UnexpectedServerResponse(Cow::from(
                    "invalid www-authenticate header value",
                ))
            })?;
            // ponytail: skip WWW-Authenticate scope parse; generic re-auth if upgrade needed
            return Err(StreamableHttpError::InsufficientScope(
                InsufficientScopeError::new(header_str.to_string(), None),
            ));
        }
    }
    let status = response.status();
    if matches!(status, StatusCode::ACCEPTED | StatusCode::NO_CONTENT) {
        return Ok(StreamableHttpPostResponse::Accepted);
    }
    if status == StatusCode::NOT_FOUND && session_was_attached {
        return Err(StreamableHttpError::SessionExpired);
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|ct| String::from_utf8_lossy(ct.as_bytes()).into_owned());
    let content_length = response.content_length();
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if status.is_success()
        && content_length == Some(0)
        && matches!(
            message,
            ClientJsonRpcMessage::Notification(_)
                | ClientJsonRpcMessage::Response(_)
                | ClientJsonRpcMessage::Error(_)
        )
    {
        return Ok(StreamableHttpPostResponse::Accepted);
    }
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<failed to read response body>".to_owned());
        if content_type
            .as_deref()
            .is_some_and(|ct| ct.as_bytes().starts_with(b"application/json"))
        {
            if let Some(message) = parse_json_rpc_error(&body) {
                return Ok(StreamableHttpPostResponse::Json(message, session_id));
            }
        }
        return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
            format!("HTTP {status}: {body}"),
        )));
    }
    match content_type.as_deref() {
        Some(ct) if ct.as_bytes().starts_with(b"text/event-stream") => {
            // ponytail: no 16MB SSE size cap (rmcp limiter is crate-private); add if a server floods
            Ok(StreamableHttpPostResponse::Sse(
                SseStream::from_bytes_stream(response.bytes_stream()).boxed(),
                session_id,
            ))
        }
        Some(ct) if ct.as_bytes().starts_with(b"application/json") => {
            match response.json::<ServerJsonRpcMessage>().await {
                Ok(message) => Ok(StreamableHttpPostResponse::Json(message, session_id)),
                Err(_) => Ok(StreamableHttpPostResponse::Accepted),
            }
        }
        _ => Err(StreamableHttpError::UnexpectedContentType(content_type)),
    }
}

impl StreamableHttpClient for AuthHttpClient {
    type Error = AuthClientError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        // The transport-provided `auth_header` is deliberately ignored: this
        // client resolves the current credential per request instead.
        let _ = auth_header;
        let inner = self.inner.clone();
        self.attempt(move |token| {
            let inner = inner.clone();
            let message = message.clone();
            let session_id = session_id.clone();
            let custom_headers = custom_headers.clone();
            let uri = uri.clone();
            async move {
                post_prefer_json(&inner, uri, message, session_id, token, custom_headers).await
            }
        })
        .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let _ = auth_header;
        let inner = self.inner.clone();
        self.attempt(move |token| {
            let inner = inner.clone();
            let session_id = session_id.clone();
            let custom_headers = custom_headers.clone();
            let uri = uri.clone();
            async move {
                inner
                    .delete_session(uri, session_id, token, custom_headers)
                    .await
            }
        })
        .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let _ = auth_header;
        let inner = self.inner.clone();
        self.attempt(move |token| {
            let inner = inner.clone();
            let last_event_id = last_event_id.clone();
            let session_id = session_id.clone();
            let custom_headers = custom_headers.clone();
            let uri = uri.clone();
            async move {
                inner
                    .get_stream(uri, session_id, last_event_id, token, custom_headers)
                    .await
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpTransport;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn post_accept_lists_json_before_event_stream() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 32_000 {
                    break;
                }
            }
            let body = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        });

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(dir.path(), dir.path().to_path_buf()).unwrap());
        let url = format!("http://{addr}/mcp");
        let cfg = McpServerConfig {
            id: "t".into(),
            name: "t".into(),
            transport: McpTransport::Http {
                url: url.clone(),
                headers: HashMap::new(),
            },
            auth: HttpAuth::None,
            enabled: true,
            auto_start: true,
            oauth_client_id: None,
            oauth_redirect_port: None,
            created_at: "now".into(),
        };
        let client = AuthHttpClient::new(cfg, store, Arc::new(|_, _| {}));
        let msg: ClientJsonRpcMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        let _ = client
            .post_message(url.into(), msg, None, None, HashMap::new())
            .await;

        let req = rx.await.unwrap();
        let accept = req
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("accept:"))
            .expect("Accept header");
        let value = accept
            .split_once(':')
            .unwrap()
            .1
            .trim()
            .to_ascii_lowercase();
        assert!(
            value.starts_with("application/json"),
            "Accept should list JSON first, got {value:?}"
        );
        assert!(
            value.contains("text/event-stream"),
            "Accept should still allow SSE, got {value:?}"
        );
    }

    #[tokio::test]
    async fn oauth_without_tokens_sends_an_unauthenticated_request() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        });

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(dir.path(), dir.path().to_path_buf()).unwrap());
        let url = format!("http://{addr}/mcp");
        let cfg = McpServerConfig {
            id: "oauth-server".into(),
            name: "OAuth Server".into(),
            transport: McpTransport::Http {
                url: url.clone(),
                headers: HashMap::new(),
            },
            auth: HttpAuth::OAuth,
            enabled: true,
            auto_start: true,
            oauth_client_id: None,
            oauth_redirect_port: None,
            created_at: "now".into(),
        };
        let client = AuthHttpClient::new(cfg, store, Arc::new(|_, _| {}));
        let message: ClientJsonRpcMessage =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();

        assert!(client
            .post_message(url.into(), message, None, None, HashMap::new())
            .await
            .is_ok());
        let request = tokio::time::timeout(std::time::Duration::from_secs(1), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!request
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("authorization:")));
    }
}
