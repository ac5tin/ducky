//! The transport-level half of OAuth token lifecycle: an HTTP client for
//! rmcp's Streamable HTTP transport that resolves a fresh `Authorization`
//! token for every request and reacts to 401/403 challenges mid-session
//! (refresh + retry once, then surface re-auth requirements).

use std::{collections::HashMap, sync::Arc};

use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use reqwest::StatusCode;
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, InsufficientScopeError, StreamableHttpClient, StreamableHttpError,
    StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse};

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
                inner
                    .post_message(uri, message, session_id, token, custom_headers)
                    .await
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
