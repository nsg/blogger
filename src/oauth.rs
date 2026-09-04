use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex as AsyncMutex;

use axum::{
    Form, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{StatusCode, header},
    middleware,
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    auth::{PasswordGate, token_eq},
    mcp,
};

const CLAUDE_CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";
const CHATGPT_CALLBACK: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
static OAUTH_CLIENTS: &[OAuthClient] = &[
    OAuthClient {
        slug: "claude",
        name: "Claude",
        callback: CLAUDE_CALLBACK,
    },
    OAuthClient {
        slug: "chatgpt",
        name: "ChatGPT",
        callback: CHATGPT_CALLBACK,
    },
];
const READ_SCOPE: &str = "posts:read";
const WRITE_SCOPE: &str = "posts:write";
const FULL_SCOPE: &str = "posts:read posts:write";
const CODE_LIFETIME_SECS: u64 = 5 * 60;
const ACCESS_LIFETIME_SECS: u64 = 60 * 60;
const REFRESH_LIFETIME_SECS: u64 = 30 * 24 * 60 * 60;
const MAX_ACTIVE_GRANTS: usize = 256;

struct OAuthClient {
    slug: &'static str,
    name: &'static str,
    callback: &'static str,
}

impl OAuthClient {
    fn allows_callback(&self, callback: &str) -> bool {
        callback == self.callback
            || (self.slug == "chatgpt" && is_chatgpt_callback_id_uri(callback))
    }
}

fn is_chatgpt_callback_id_uri(callback: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(callback) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("chatgpt.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(segments) = url.path_segments() else {
        return false;
    };
    let segments = segments.collect::<Vec<_>>();
    matches!(segments.as_slice(), ["connector", "oauth", callback_id] if
    !callback_id.is_empty()
        && callback_id.len() <= 128
        && callback_id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
        }))
}

#[derive(Clone)]
pub struct OAuthState {
    inner: Arc<OAuthInner>,
}

struct OAuthInner {
    password: String,
    password_gate: Arc<PasswordGate>,
    session_secret: [u8; 32],
    public_url: String,
    issuer: String,
    grants: Mutex<Grants>,
}

#[derive(Default)]
struct Grants {
    codes: HashMap<String, AuthorizationCode>,
    access_tokens: HashMap<String, TokenGrant>,
    refresh_tokens: HashMap<String, TokenGrant>,
    spent_refresh_tokens: HashMap<String, SpentRefreshToken>,
    revoked_grants: HashMap<String, u64>,
}

#[derive(Clone)]
struct AuthorizationCode {
    grant_id: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    resource: String,
    scope: String,
    expires_at: u64,
}

#[derive(Clone)]
struct TokenGrant {
    grant_id: String,
    client_id: String,
    resource: String,
    scope: String,
    expires_at: u64,
}

#[derive(Clone)]
struct SpentRefreshToken {
    grant_id: String,
    expires_at: u64,
}

impl OAuthState {
    pub fn new(
        password: String,
        password_gate: Arc<PasswordGate>,
        session_secret: [u8; 32],
        public_url: String,
        issuer: String,
    ) -> Self {
        Self {
            inner: Arc::new(OAuthInner {
                password,
                password_gate,
                session_secret,
                public_url,
                issuer,
                grants: Mutex::new(Grants::default()),
            }),
        }
    }

    fn client_id(&self, client: &OAuthClient) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.inner.session_secret)
            .expect("HMAC accepts a 32-byte key");
        mac.update(b"blogger-mcp-oauth-client-v1\0");
        mac.update(client.callback.as_bytes());
        format!(
            "blogger-{}-{}",
            client.slug,
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        )
    }

    fn valid_client(&self, client_id: &str) -> bool {
        self.client_for_id(client_id).is_some()
    }

    fn client_for_id(&self, client_id: &str) -> Option<&'static OAuthClient> {
        OAUTH_CLIENTS
            .iter()
            .find(|client| token_eq(client_id, &self.client_id(client)))
    }

    fn client_for_callback(callback: &str) -> Option<&'static OAuthClient> {
        OAUTH_CLIENTS
            .iter()
            .find(|client| client.allows_callback(callback))
    }

    fn resource_metadata_url(&self) -> String {
        format!(
            "{}/.well-known/oauth-protected-resource/mcp",
            self.inner.issuer
        )
    }

    fn validate_access_token(&self, token: &str, now: u64) -> Option<mcp::AccessScopes> {
        let mut grants = self.inner.grants.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut grants, now);
        grants
            .access_tokens
            .get(token)
            .filter(|grant| {
                grant.expires_at > now
                    && grant.resource == self.inner.public_url
                    && has_scope(&grant.scope, READ_SCOPE)
            })
            .map(|grant| mcp::AccessScopes::new(has_scope(&grant.scope, WRITE_SCOPE)))
    }
}

pub fn routes() -> Router<OAuthState> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/register", post(register))
        .route("/authorize", get(authorize_page).post(authorize))
        .route("/token", post(token))
        .route("/revoke", post(revoke))
}

pub fn public_router(
    state: OAuthState,
    zola_root: PathBuf,
    coordinator: Arc<AsyncMutex<()>>,
    allowed_host: String,
) -> (Router, mcp::McpCancellation) {
    let (mcp_service, cancel_mcp) = mcp::http_service(
        zola_root,
        coordinator,
        allowed_host.clone(),
        Default::default(),
    );
    let mcp_routes = Router::new().nest_service("/mcp", mcp_service).route_layer(
        middleware::from_fn_with_state(state.clone(), require_access_token),
    );
    let router = routes()
        .merge(mcp_routes)
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(
            allowed_host,
            require_public_host,
        ))
        .with_state(state);
    (router, cancel_mcp)
}

async fn require_public_host(
    State(allowed_host): State<String>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let allowed = host.is_some_and(|host| {
        host.eq_ignore_ascii_case(&allowed_host)
            || (!allowed_host.contains(':')
                && host.eq_ignore_ascii_case(&format!("{allowed_host}:443")))
    });
    if !allowed {
        return StatusCode::MISDIRECTED_REQUEST.into_response();
    }
    next.run(request).await
}

async fn protected_resource_metadata(State(state): State<OAuthState>) -> Response {
    no_store(Json(json!({
        "resource": state.inner.public_url,
        "authorization_servers": [state.inner.issuer],
        "scopes_supported": [READ_SCOPE, WRITE_SCOPE],
        "bearer_methods_supported": ["header"],
    })))
}

async fn authorization_server_metadata(State(state): State<OAuthState>) -> Response {
    no_store(Json(json!({
        "issuer": state.inner.issuer,
        "authorization_response_iss_parameter_supported": true,
        "authorization_endpoint": format!("{}/authorize", state.inner.issuer),
        "token_endpoint": format!("{}/token", state.inner.issuer),
        "registration_endpoint": format!("{}/register", state.inner.issuer),
        "revocation_endpoint": format!("{}/revoke", state.inner.issuer),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": [READ_SCOPE, WRITE_SCOPE],
    })))
}

#[derive(Deserialize)]
struct RegistrationRequest {
    redirect_uris: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    grant_types: Vec<String>,
    #[serde(default)]
    response_types: Vec<String>,
}

async fn register(
    State(state): State<OAuthState>,
    Json(request): Json<RegistrationRequest>,
) -> Response {
    let auth_method = request
        .token_endpoint_auth_method
        .as_deref()
        .unwrap_or("none");
    let grants_ok = request.grant_types.is_empty()
        || request
            .grant_types
            .iter()
            .all(|grant| matches!(grant.as_str(), "authorization_code" | "refresh_token"));
    let responses_ok = request.response_types.is_empty()
        || request.response_types.iter().all(|kind| kind == "code");
    let client = request
        .redirect_uris
        .first()
        .and_then(|callback| OAuthState::client_for_callback(callback))
        .filter(|client| {
            request
                .redirect_uris
                .iter()
                .all(|callback| client.allows_callback(callback))
        });
    let Some(client) = client else {
        return oauth_json_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "all redirect URIs must use one supported public client",
        );
    };
    if auth_method != "none" || !grants_ok || !responses_ok {
        return oauth_json_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "only public clients using authorization code with PKCE are supported",
        );
    }

    no_store((
        StatusCode::CREATED,
        Json(json!({
            "client_id": state.client_id(client),
            "client_id_issued_at": unix_now(),
            "redirect_uris": request.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })),
    ))
}

#[derive(Clone, Deserialize)]
struct AuthorizeRequest {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    #[serde(default)]
    resource: String,
}

#[derive(Deserialize)]
struct AuthorizeSubmission {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    #[serde(default)]
    resource: String,
    #[serde(default)]
    password: String,
}

impl AuthorizeSubmission {
    fn request(&self) -> AuthorizeRequest {
        AuthorizeRequest {
            client_id: self.client_id.clone(),
            redirect_uri: self.redirect_uri.clone(),
            response_type: self.response_type.clone(),
            scope: self.scope.clone(),
            state: self.state.clone(),
            code_challenge: self.code_challenge.clone(),
            code_challenge_method: self.code_challenge_method.clone(),
            resource: self.resource.clone(),
        }
    }
}

fn validate_authorize(state: &OAuthState, request: &AuthorizeRequest) -> Result<(), &'static str> {
    let Some(client) = state.client_for_id(&request.client_id) else {
        return Err("unknown client_id");
    };
    if request
        .redirect_uri
        .as_deref()
        .is_some_and(|redirect| !client.allows_callback(redirect))
    {
        return Err("invalid redirect_uri");
    }
    if request.response_type != "code" {
        return Err("unsupported response_type");
    }
    if !scope_eq(&request.scope, FULL_SCOPE) {
        return Err("invalid scope");
    }
    if request.resource != state.inner.public_url {
        return Err("invalid resource");
    }
    if request.state.is_empty() {
        return Err("state and PKCE challenge are required");
    }
    if request.code_challenge_method != "S256" || !valid_pkce_value(&request.code_challenge) {
        return Err("PKCE S256 is required");
    }
    Ok(())
}

async fn authorize_page(
    State(state): State<OAuthState>,
    Query(request): Query<AuthorizeRequest>,
) -> Response {
    if let Err(message) = validate_authorize(&state, &request) {
        return authorization_error(
            &state,
            &request,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            message,
        );
    }
    let client = state
        .client_for_id(&request.client_id)
        .expect("validated client ID has a callback");
    let fields = [
        ("client_id", request.client_id),
        (
            "redirect_uri",
            request
                .redirect_uri
                .unwrap_or_else(|| client.callback.to_owned()),
        ),
        ("response_type", request.response_type),
        ("scope", request.scope),
        ("state", request.state),
        ("code_challenge", request.code_challenge),
        ("code_challenge_method", request.code_challenge_method),
        ("resource", request.resource),
    ]
    .into_iter()
    .map(|(name, value)| {
        format!(
            "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
            html_escape(name),
            html_escape(&value)
        )
    })
    .collect::<String>();
    let page = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Authorize Blogger</title><style>:root{{color-scheme:dark;font-family:system-ui,sans-serif}}body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#111827;color:#f9fafb}}main{{width:min(24rem,calc(100vw - 2rem))}}label{{display:block;margin-bottom:.5rem;font-weight:650}}input[type=password]{{box-sizing:border-box;width:100%;padding:.75rem;font:inherit;background:#1f2937;color:inherit;border:1px solid #4b5563;border-radius:.35rem}}button{{margin-top:.75rem;width:100%;padding:.75rem;font:inherit;font-weight:650;cursor:pointer}}</style></head><body><main><h1>Authorize {client_name}</h1><p>Allow {client_name} to read blog posts and the writing-style guide, and to create or edit drafts or replace that guide. {client_name} cannot publish, delete, commit, or push.</p><form method="post" action="/authorize">{fields}<label for="password">Blogger password</label><input id="password" name="password" type="password" autocomplete="current-password" autofocus required><button type="submit">Authorize</button></form></main></body></html>"#,
        client_name = client.name,
    );
    secure_html(Html(page))
}

async fn authorize(
    State(state): State<OAuthState>,
    Form(submission): Form<AuthorizeSubmission>,
) -> Response {
    let request = submission.request();
    if let Err(message) = validate_authorize(&state, &request) {
        return authorization_error(
            &state,
            &request,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            message,
        );
    }
    if !state
        .inner
        .password_gate
        .verify(Some(&submission.password), &state.inner.password)
    {
        return authorization_error(
            &state,
            &request,
            StatusCode::UNAUTHORIZED,
            "access_denied",
            "incorrect password",
        );
    }

    let now = unix_now();
    let client = state
        .client_for_id(&request.client_id)
        .expect("validated client ID has a callback");
    let code = random_token();
    let grant = AuthorizationCode {
        grant_id: random_token(),
        client_id: request.client_id,
        redirect_uri: request
            .redirect_uri
            .clone()
            .unwrap_or_else(|| client.callback.to_owned()),
        code_challenge: request.code_challenge,
        resource: request.resource,
        scope: FULL_SCOPE.to_owned(),
        expires_at: now + CODE_LIFETIME_SECS,
    };
    let mut grants = state.inner.grants.lock().unwrap_or_else(|e| e.into_inner());
    prune(&mut grants, now);
    bound_map(&mut grants.codes);
    grants.codes.insert(code.clone(), grant);
    drop(grants);

    let mut redirect =
        reqwest::Url::parse(request.redirect_uri.as_deref().unwrap_or(client.callback))
            .expect("the fixed client callback URL is valid");
    redirect
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &request.state)
        .append_pair("iss", &state.inner.issuer);
    no_store(Redirect::to(redirect.as_str()))
}

fn authorization_error(
    state: &OAuthState,
    request: &AuthorizeRequest,
    local_status: StatusCode,
    error: &str,
    description: &str,
) -> Response {
    let Some(client) = state.client_for_id(&request.client_id) else {
        return oauth_json_error(local_status, error, description);
    };
    if request
        .redirect_uri
        .as_deref()
        .is_some_and(|redirect| !client.allows_callback(redirect))
    {
        return oauth_json_error(local_status, error, description);
    }
    let callback = request.redirect_uri.as_deref().unwrap_or(client.callback);
    let mut redirect = reqwest::Url::parse(callback).expect("the validated callback URL is valid");
    let mut parameters = redirect.query_pairs_mut();
    parameters
        .append_pair("error", error)
        .append_pair("error_description", description);
    if !request.state.is_empty() {
        parameters.append_pair("state", &request.state);
    }
    parameters.append_pair("iss", &state.inner.issuer);
    drop(parameters);
    no_store(Redirect::to(redirect.as_str()))
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    client_id: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

async fn token(State(state): State<OAuthState>, Form(request): Form<TokenRequest>) -> Response {
    if !state.valid_client(&request.client_id) {
        return oauth_json_error(StatusCode::UNAUTHORIZED, "invalid_client", "unknown client");
    }
    match request.grant_type.as_str() {
        "authorization_code" => exchange_code(&state, request),
        "refresh_token" => refresh_access(&state, request),
        _ => oauth_json_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "supported grants are authorization_code and refresh_token",
        ),
    }
}

fn exchange_code(state: &OAuthState, request: TokenRequest) -> Response {
    let Some(code) = request.code else {
        return invalid_grant("authorization code is required");
    };
    let now = unix_now();
    let grant = {
        let mut grants = state.inner.grants.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut grants, now);
        grants.codes.remove(&code)
    };
    let Some(grant) = grant else {
        return invalid_grant("authorization code is invalid or expired");
    };
    let verifier = request.code_verifier.unwrap_or_default();
    if !valid_pkce_value(&verifier) {
        return invalid_grant("PKCE verifier is invalid");
    }
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if grant.expires_at <= now
        || !token_eq(&grant.client_id, &request.client_id)
        || request
            .redirect_uri
            .as_deref()
            .is_some_and(|redirect| redirect != grant.redirect_uri)
        || request.resource.as_deref() != Some(grant.resource.as_str())
        || !token_eq(&challenge, &grant.code_challenge)
    {
        return invalid_grant("authorization code validation failed");
    }
    issue_tokens(
        state,
        grant.grant_id,
        grant.client_id,
        grant.resource,
        grant.scope,
        now,
    )
}

fn refresh_access(state: &OAuthState, request: TokenRequest) -> Response {
    let Some(refresh_token) = request.refresh_token else {
        return invalid_grant("refresh token is required");
    };
    let now = unix_now();
    let mut grants = state.inner.grants.lock().unwrap_or_else(|e| e.into_inner());
    prune(&mut grants, now);
    if let Some(spent) = grants.spent_refresh_tokens.get(&refresh_token).cloned() {
        revoke_grant(&mut grants, &spent.grant_id, spent.expires_at);
        return invalid_grant("refresh token replay detected; grant revoked");
    }
    let grant = grants.refresh_tokens.get(&refresh_token).cloned();
    let Some(grant) = grant else {
        return invalid_grant("refresh token is invalid or expired");
    };
    if grant.expires_at <= now
        || !token_eq(&grant.client_id, &request.client_id)
        || request
            .resource
            .as_deref()
            .is_some_and(|value| value != grant.resource)
        || request
            .scope
            .as_deref()
            .is_some_and(|value| !scope_eq(value, &grant.scope))
    {
        return invalid_grant("refresh token validation failed");
    }
    grants.refresh_tokens.remove(&refresh_token);
    bound_map(&mut grants.spent_refresh_tokens);
    grants.spent_refresh_tokens.insert(
        refresh_token,
        SpentRefreshToken {
            grant_id: grant.grant_id.clone(),
            expires_at: grant.expires_at,
        },
    );
    drop(grants);
    issue_tokens(
        state,
        grant.grant_id,
        grant.client_id,
        grant.resource,
        grant.scope,
        now,
    )
}

fn issue_tokens(
    state: &OAuthState,
    grant_id: String,
    client_id: String,
    resource: String,
    scope: String,
    now: u64,
) -> Response {
    let access_token = random_token();
    let refresh_token = random_token();
    let mut grants = state.inner.grants.lock().unwrap_or_else(|e| e.into_inner());
    if grants.revoked_grants.contains_key(&grant_id) {
        return invalid_grant("authorization grant has been revoked");
    }
    bound_map(&mut grants.access_tokens);
    bound_map(&mut grants.refresh_tokens);
    grants.access_tokens.insert(
        access_token.clone(),
        TokenGrant {
            grant_id: grant_id.clone(),
            client_id: client_id.clone(),
            resource: resource.clone(),
            scope: scope.clone(),
            expires_at: now + ACCESS_LIFETIME_SECS,
        },
    );
    grants.refresh_tokens.insert(
        refresh_token.clone(),
        TokenGrant {
            grant_id,
            client_id,
            resource,
            scope: scope.clone(),
            expires_at: now + REFRESH_LIFETIME_SECS,
        },
    );
    drop(grants);
    no_store(Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_LIFETIME_SECS,
        "refresh_token": refresh_token,
        "scope": scope,
    })))
}

#[derive(Deserialize)]
struct RevokeRequest {
    token: String,
    client_id: String,
}

async fn revoke(State(state): State<OAuthState>, Form(request): Form<RevokeRequest>) -> Response {
    if !state.valid_client(&request.client_id) {
        return oauth_json_error(StatusCode::UNAUTHORIZED, "invalid_client", "unknown client");
    }
    let mut grants = state.inner.grants.lock().unwrap_or_else(|e| e.into_inner());
    let grant = grants
        .access_tokens
        .get(&request.token)
        .or_else(|| grants.refresh_tokens.get(&request.token))
        .filter(|grant| token_eq(&grant.client_id, &request.client_id))
        .map(|grant| (grant.grant_id.clone(), grant.expires_at))
        .or_else(|| {
            grants
                .spent_refresh_tokens
                .get(&request.token)
                .map(|grant| (grant.grant_id.clone(), grant.expires_at))
        });
    if let Some((grant_id, expires_at)) = grant {
        revoke_grant(&mut grants, &grant_id, expires_at);
    }
    no_store(StatusCode::OK)
}

pub async fn require_access_token(
    State(state): State<OAuthState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .and_then(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer").then_some(token))
        .filter(|token| !token.is_empty() && !token.contains(' '));
    if let Some(scopes) = token.and_then(|token| state.validate_access_token(token, unix_now())) {
        request.extensions_mut().insert(scopes);
        return next.run(request).await;
    }
    let challenge = format!(
        "Bearer resource_metadata=\"{}\", scope=\"{}\"",
        state.resource_metadata_url(),
        FULL_SCOPE
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge)],
        Json(json!({ "error": "invalid_token" })),
    )
        .into_response()
}

fn has_scope(granted: &str, required: &str) -> bool {
    granted
        .split_ascii_whitespace()
        .any(|scope| scope == required)
}

fn scope_eq(left: &str, right: &str) -> bool {
    let left = left.split_ascii_whitespace().collect::<Vec<_>>();
    let right = right.split_ascii_whitespace().collect::<Vec<_>>();
    left.len() == right.len()
        && left.iter().all(|scope| right.contains(scope))
        && right.iter().all(|scope| left.contains(scope))
}

fn invalid_grant(description: &'static str) -> Response {
    oauth_json_error(StatusCode::BAD_REQUEST, "invalid_grant", description)
}

fn oauth_json_error(status: StatusCode, error: &str, description: &str) -> Response {
    no_store((
        status,
        Json(json!({
            "error": error,
            "error_description": description,
        })),
    ))
}

fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, header::HeaderValue::from_static("no-cache"));
    response
}

fn secure_html(response: impl IntoResponse) -> Response {
    let mut response = no_store(response);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    response
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).expect("operating system randomness must be available");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn prune(grants: &mut Grants, now: u64) {
    grants.codes.retain(|_, grant| grant.expires_at > now);
    grants
        .access_tokens
        .retain(|_, grant| grant.expires_at > now);
    grants
        .refresh_tokens
        .retain(|_, grant| grant.expires_at > now);
    grants
        .spent_refresh_tokens
        .retain(|_, grant| grant.expires_at > now);
    grants
        .revoked_grants
        .retain(|_, expires_at| *expires_at > now);
}

fn revoke_grant(grants: &mut Grants, grant_id: &str, expires_at: u64) {
    let expires_at = grants
        .access_tokens
        .values()
        .filter(|grant| grant.grant_id == grant_id)
        .map(|grant| grant.expires_at)
        .chain(
            grants
                .refresh_tokens
                .values()
                .filter(|grant| grant.grant_id == grant_id)
                .map(|grant| grant.expires_at),
        )
        .chain(
            grants
                .spent_refresh_tokens
                .values()
                .filter(|grant| grant.grant_id == grant_id)
                .map(|grant| grant.expires_at),
        )
        .max()
        .unwrap_or(expires_at);
    grants
        .access_tokens
        .retain(|_, grant| grant.grant_id != grant_id);
    grants
        .refresh_tokens
        .retain(|_, grant| grant.grant_id != grant_id);
    grants
        .spent_refresh_tokens
        .retain(|_, grant| grant.grant_id != grant_id);
    bound_map(&mut grants.revoked_grants);
    grants
        .revoked_grants
        .insert(grant_id.to_owned(), expires_at);
}

fn bound_map<T>(map: &mut HashMap<String, T>) {
    if map.len() >= MAX_ACTIVE_GRANTS
        && let Some(key) = map.keys().next().cloned()
    {
        map.remove(&key);
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn valid_pkce_value(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

#[cfg(test)]
mod tests {
    use super::{
        ACCESS_LIFETIME_SECS, AsyncMutex, CHATGPT_CALLBACK, CLAUDE_CALLBACK, FULL_SCOPE,
        OAuthState, PasswordGate, READ_SCOPE, TokenGrant, html_escape, public_router, unix_now,
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use reqwest::{Client, StatusCode, Url, header};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn state() -> OAuthState {
        OAuthState::new(
            "password".to_owned(),
            Arc::new(PasswordGate::new()),
            [7; 32],
            "https://mcp.example.com/mcp".to_owned(),
            "https://mcp.example.com".to_owned(),
        )
    }

    #[test]
    fn client_id_is_stable_and_secret_bound() {
        let state = state();
        let claude_client = OAuthState::client_for_callback(CLAUDE_CALLBACK).unwrap();
        let chatgpt_client = OAuthState::client_for_callback(CHATGPT_CALLBACK).unwrap();
        let claude = state.client_id(claude_client);
        let chatgpt = state.client_id(chatgpt_client);
        assert_eq!(
            claude,
            "blogger-claude-XtB_aCZn2ESDl7slCtE-MeYETCjT870qug0eqRmcVks"
        );
        assert_eq!(claude, state.client_id(claude_client));
        assert_eq!(chatgpt, state.client_id(chatgpt_client));
        assert_ne!(claude, chatgpt);
        assert!(state.valid_client(&claude));
        assert!(state.valid_client(&chatgpt));
        assert_eq!(
            state.client_for_id(&claude).map(|client| client.callback),
            Some(CLAUDE_CALLBACK)
        );
        assert_eq!(
            state.client_for_id(&chatgpt).map(|client| client.callback),
            Some(CHATGPT_CALLBACK)
        );
        assert!(!state.valid_client("blogger-claude-wrong"));
    }

    #[test]
    fn escapes_authorization_form_values() {
        assert_eq!(
            html_escape("<a x='&\"'>"),
            "&lt;a x=&#39;&amp;&quot;&#39;&gt;"
        );
    }

    #[test]
    fn validates_pkce_unreserved_syntax_and_bounds() {
        assert!(super::valid_pkce_value(&"a".repeat(43)));
        assert!(super::valid_pkce_value(&format!("{}-._~", "a".repeat(124))));
        assert!(!super::valid_pkce_value(&"a".repeat(42)));
        assert!(!super::valid_pkce_value(&"a".repeat(129)));
        assert!(!super::valid_pkce_value(&format!("{}+", "a".repeat(42))));
    }

    #[test]
    fn treats_oauth_scopes_as_an_unordered_set() {
        assert!(super::scope_eq(
            "posts:write posts:read",
            "posts:read posts:write"
        ));
        assert!(!super::scope_eq("posts:read", "posts:read posts:write"));
        assert!(!super::scope_eq(
            "posts:read posts:read",
            "posts:read posts:write"
        ));
    }

    struct TestSite(PathBuf);

    impl TestSite {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("blogger-oauth-test-{}-{nanos}", std::process::id()));
            fs::create_dir_all(path.join("content/post/2026")).unwrap();
            fs::write(
                path.join("content/post/2026/draft.md"),
                "+++\ntitle = \"Voice Draft\"\ndate = 2026-08-12\ndraft = true\n[taxonomies]\ntags = [\"voice\", \"shared\"]\n+++\nA distinctive narwhal voice note.\n",
            )
            .unwrap();
            fs::write(
                path.join("content/post/2026/published.md"),
                "+++\ntitle = \"Published\"\ndate = 2026-08-12\ndraft = false\n[taxonomies]\ntags = [\"published\", \"shared\"]\n+++\nPublic material.\n",
            )
            .unwrap();
            fs::write(
                path.join(crate::writing_style::WRITING_STYLE_PATH),
                "# Writing style\n\nWarm, direct, and fond of narwhals.\n",
            )
            .unwrap();
            Self(path)
        }
    }

    impl Drop for TestSite {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    async fn start_test_server() -> (TestSite, String, OAuthState, tokio::task::JoinHandle<()>) {
        let site = TestSite::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = state();
        let (router, _cancel_mcp) = public_router(
            state.clone(),
            site.0.clone(),
            Arc::new(AsyncMutex::new(())),
            address.to_string(),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (site, format!("http://{address}"), state, task)
    }

    fn test_client() -> Client {
        Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    async fn authorize_code(
        client: &Client,
        base: &str,
        client_id: &str,
        challenge: &str,
        state_value: &str,
        password: &str,
        callback: &str,
    ) -> reqwest::Response {
        client
            .post(format!("{base}/authorize"))
            .form(&[
                ("client_id", client_id),
                ("redirect_uri", callback),
                ("response_type", "code"),
                ("scope", FULL_SCOPE),
                ("state", state_value),
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
                ("resource", "https://mcp.example.com/mcp"),
                ("password", password),
            ])
            .send()
            .await
            .unwrap()
    }

    fn redirect_code(
        response: &reqwest::Response,
        expected_state: &str,
        expected_callback: &str,
    ) -> String {
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers().get(header::LOCATION).unwrap();
        let url = Url::parse(location.to_str().unwrap()).unwrap();
        let callback = Url::parse(expected_callback).unwrap();
        assert_eq!(url.origin(), callback.origin());
        assert_eq!(url.path(), callback.path());
        let parameters = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(parameters.get("state").unwrap(), expected_state);
        assert_eq!(parameters.get("iss").unwrap(), "https://mcp.example.com");
        parameters.get("code").unwrap().to_string()
    }

    fn sse_json(body: &str) -> Value {
        let data = body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .find(|data| !data.trim().is_empty())
            .expect("MCP response contains an SSE data event");
        serde_json::from_str(data).unwrap()
    }

    fn insert_read_only_token(state: &OAuthState) -> String {
        let token = "test-read-only-access-token".to_owned();
        state.inner.grants.lock().unwrap().access_tokens.insert(
            token.clone(),
            TokenGrant {
                grant_id: "old-read-only-grant".to_owned(),
                client_id: state
                    .client_id(OAuthState::client_for_callback(CLAUDE_CALLBACK).unwrap()),
                resource: state.inner.public_url.clone(),
                scope: READ_SCOPE.to_owned(),
                expires_at: unix_now() + ACCESS_LIFETIME_SECS,
            },
        );
        token
    }

    async fn register_client(client: &Client, base: &str, callback: &str) -> Value {
        let response = client
            .post(format!("{base}/register"))
            .json(&json!({
                "client_name": "Blogger OAuth test",
                "redirect_uris": [callback],
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let registration = response.json::<Value>().await.unwrap();
        assert_eq!(registration["redirect_uris"], json!([callback]));
        registration
    }

    #[tokio::test]
    async fn supports_chatgpt_oauth_and_keeps_clients_bound() {
        let (_site, base, state, server) = start_test_server().await;
        let client = test_client();

        let metadata = client
            .get(format!("{base}/.well-known/oauth-authorization-server"))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(
            metadata["authorization_response_iss_parameter_supported"],
            true
        );

        let chatgpt = register_client(&client, &base, CHATGPT_CALLBACK).await;
        let chatgpt_client = chatgpt["client_id"].as_str().unwrap();
        let claude = register_client(&client, &base, CLAUDE_CALLBACK).await;
        let claude_client = claude["client_id"].as_str().unwrap();
        assert_ne!(chatgpt_client, claude_client);

        let callback_id_uri = "https://chatgpt.com/connector/oauth/callback-id_123";
        let callback_id_only = register_client(&client, &base, callback_id_uri).await;
        assert_eq!(callback_id_only["client_id"], chatgpt["client_id"]);
        let callback_id_registration = client
            .post(format!("{base}/register"))
            .json(&json!({
                "redirect_uris": [CHATGPT_CALLBACK, callback_id_uri],
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(callback_id_registration.status(), StatusCode::CREATED);
        let callback_id_registration = callback_id_registration.json::<Value>().await.unwrap();
        assert_eq!(callback_id_registration["client_id"], chatgpt["client_id"]);
        assert_eq!(
            callback_id_registration["redirect_uris"],
            json!([CHATGPT_CALLBACK, callback_id_uri])
        );

        let mut callback_id_error_url = Url::parse(&format!("{base}/authorize")).unwrap();
        callback_id_error_url
            .query_pairs_mut()
            .append_pair("client_id", chatgpt_client)
            .append_pair("redirect_uri", callback_id_uri)
            .append_pair("state", "callback-id-state");
        let callback_id_error = client.get(callback_id_error_url).send().await.unwrap();
        assert_eq!(callback_id_error.status(), StatusCode::SEE_OTHER);
        let callback_id_error = Url::parse(
            callback_id_error
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            callback_id_error.as_str().split('?').next(),
            Some(callback_id_uri)
        );
        let callback_id_error_parameters = callback_id_error
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            callback_id_error_parameters.get("state").unwrap(),
            "callback-id-state"
        );
        assert_eq!(
            callback_id_error_parameters.get("iss").unwrap(),
            "https://mcp.example.com"
        );

        let mut incomplete_url = Url::parse(&format!("{base}/authorize")).unwrap();
        incomplete_url
            .query_pairs_mut()
            .append_pair("client_id", chatgpt_client)
            .append_pair("redirect_uri", CHATGPT_CALLBACK)
            .append_pair("state", "incomplete-state");
        let incomplete = client.get(incomplete_url).send().await.unwrap();
        assert_eq!(incomplete.status(), StatusCode::SEE_OTHER);
        let incomplete_url = Url::parse(
            incomplete
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            incomplete_url.origin().ascii_serialization(),
            "https://chatgpt.com"
        );
        assert_eq!(incomplete_url.path(), "/connector_platform_oauth_redirect");
        let incomplete_parameters = incomplete_url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            incomplete_parameters.get("error").unwrap(),
            "invalid_request"
        );
        assert_eq!(
            incomplete_parameters.get("state").unwrap(),
            "incomplete-state"
        );
        assert_eq!(
            incomplete_parameters.get("iss").unwrap(),
            "https://mcp.example.com"
        );

        for redirect_uris in [
            json!([]),
            json!([CHATGPT_CALLBACK, CLAUDE_CALLBACK]),
            json!(["http://chatgpt.com/connector/oauth/callback-id"]),
            json!(["https://chatgpt.com:444/connector/oauth/callback-id"]),
            json!(["https://chatgpt.com/connector/oauth/"]),
            json!(["https://chatgpt.com/connector/oauth/callback-id/extra"]),
            json!(["https://chatgpt.com/connector/oauth/callback-id?next=evil"]),
            json!(["https://chatgpt.com/connector/oauth/callback%2Fid"]),
        ] {
            let rejected = client
                .post(format!("{base}/register"))
                .json(&json!({"redirect_uris": redirect_uris}))
                .send()
                .await
                .unwrap();
            assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        }

        let verifier = "chatgpt-pkce-verifier-that-is-long-enough-for-oauth-123456789";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorize_url = Url::parse(&format!("{base}/authorize")).unwrap();
        authorize_url
            .query_pairs_mut()
            .append_pair("client_id", chatgpt_client)
            .append_pair("response_type", "code")
            .append_pair("scope", FULL_SCOPE)
            .append_pair("state", "chatgpt-state")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", "https://mcp.example.com/mcp");
        let page = client.get(authorize_url.clone()).send().await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let page = page.text().await.unwrap();
        assert!(page.contains("Authorize ChatGPT"));
        assert!(page.contains(CHATGPT_CALLBACK));

        authorize_url
            .query_pairs_mut()
            .append_pair("redirect_uri", CLAUDE_CALLBACK);
        let mismatched = client.get(authorize_url).send().await.unwrap();
        assert_eq!(mismatched.status(), StatusCode::BAD_REQUEST);

        let authorized = authorize_code(
            &client,
            &base,
            chatgpt_client,
            &challenge,
            "chatgpt-state",
            "password",
            CHATGPT_CALLBACK,
        )
        .await;
        let code = redirect_code(&authorized, "chatgpt-state", CHATGPT_CALLBACK);

        let wrong_client = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", claude_client),
                ("code", code.as_str()),
                ("redirect_uri", CHATGPT_CALLBACK),
                ("code_verifier", verifier),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_client.status(), StatusCode::BAD_REQUEST);

        let authorized = authorize_code(
            &client,
            &base,
            chatgpt_client,
            &challenge,
            "chatgpt-state-2",
            "password",
            callback_id_uri,
        )
        .await;
        let code = redirect_code(&authorized, "chatgpt-state-2", callback_id_uri);
        let tokens = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", chatgpt_client),
                ("code", code.as_str()),
                ("redirect_uri", callback_id_uri),
                ("code_verifier", verifier),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(tokens.status(), StatusCode::OK);
        let tokens = tokens.json::<Value>().await.unwrap();
        let access_token = tokens["access_token"].as_str().unwrap();
        let refresh_token = tokens["refresh_token"].as_str().unwrap();

        let wrong_revoke = client
            .post(format!("{base}/revoke"))
            .form(&[("token", access_token), ("client_id", claude_client)])
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_revoke.status(), StatusCode::OK);
        assert!(
            state
                .validate_access_token(access_token, unix_now())
                .is_some()
        );

        let wrong_refresh = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", claude_client),
                ("refresh_token", refresh_token),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_refresh.status(), StatusCode::BAD_REQUEST);

        let refreshed = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", chatgpt_client),
                ("refresh_token", refresh_token),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(refreshed.status(), StatusCode::OK);
        let refreshed = refreshed.json::<Value>().await.unwrap();
        let next_refresh = refreshed["refresh_token"].as_str().unwrap();

        let wrong_replay = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", claude_client),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_replay.status(), StatusCode::BAD_REQUEST);

        let still_refreshable = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", chatgpt_client),
                ("refresh_token", next_refresh),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(still_refreshable.status(), StatusCode::BAD_REQUEST);

        server.abort();
    }

    async fn mcp_post(
        client: &Client,
        base: &str,
        token: &str,
        session: Option<&str>,
        body: Value,
    ) -> reqwest::Response {
        let mut request = client
            .post(format!("{base}/mcp"))
            .bearer_auth(token)
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-06-18")
            .json(&body);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        request.send().await.unwrap()
    }

    #[tokio::test]
    async fn completes_oauth_and_draft_writing_mcp_flow() {
        let (site, base, state, server) = start_test_server().await;
        let client = test_client();

        let wrong_host = client
            .get(format!("{base}/.well-known/oauth-authorization-server"))
            .header(header::HOST, "wrong.example.com")
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_host.status(), StatusCode::MISDIRECTED_REQUEST);

        let protected: Value = client
            .get(format!("{base}/.well-known/oauth-protected-resource/mcp"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(protected["resource"], "https://mcp.example.com/mcp");
        assert_eq!(
            protected["scopes_supported"],
            json!(["posts:read", "posts:write"])
        );
        let metadata: Value = client
            .get(format!("{base}/.well-known/oauth-authorization-server"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(metadata["code_challenge_methods_supported"][0], "S256");
        assert_eq!(
            metadata["authorization_response_iss_parameter_supported"],
            true
        );
        assert_eq!(
            metadata["scopes_supported"],
            json!(["posts:read", "posts:write"])
        );

        let registration = client
            .post(format!("{base}/register"))
            .json(&json!({
                "redirect_uris": [CLAUDE_CALLBACK],
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(registration.status(), StatusCode::CREATED);
        let client_id = registration.json::<Value>().await.unwrap()["client_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let verifier = "a-very-long-pkce-verifier-for-the-blogger-mcp-flow-123456789";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorize_url = Url::parse(&format!("{base}/authorize")).unwrap();
        authorize_url
            .query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", CLAUDE_CALLBACK)
            .append_pair("response_type", "code")
            .append_pair("scope", FULL_SCOPE)
            .append_pair("state", "claude-state")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", "https://mcp.example.com/mcp");
        let page = client.get(authorize_url).send().await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let page = page.text().await.unwrap();
        assert!(page.contains("Authorize Claude"));
        assert!(page.contains("writing-style guide"));

        let wrong = authorize_code(
            &client,
            &base,
            &client_id,
            &challenge,
            "claude-state",
            "wrong",
            CLAUDE_CALLBACK,
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::SEE_OTHER);
        let wrong_url = Url::parse(
            wrong
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let wrong_parameters = wrong_url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(wrong_parameters.get("error").unwrap(), "access_denied");
        assert_eq!(wrong_parameters.get("state").unwrap(), "claude-state");
        assert_eq!(
            wrong_parameters.get("iss").unwrap(),
            "https://mcp.example.com"
        );
        assert!(!wrong_url.as_str().contains("wrong"));

        let authorized = authorize_code(
            &client,
            &base,
            &client_id,
            &challenge,
            "claude-state",
            "password",
            CLAUDE_CALLBACK,
        )
        .await;
        let code = redirect_code(&authorized, "claude-state", CLAUDE_CALLBACK);
        let token_response = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", verifier),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(token_response.status(), StatusCode::OK);
        let tokens: Value = token_response.json().await.unwrap();
        assert_eq!(tokens["scope"], FULL_SCOPE);
        let access = tokens["access_token"].as_str().unwrap();
        let refresh = tokens["refresh_token"].as_str().unwrap();

        let replay = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", verifier),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

        let unauthorized = client.post(format!("{base}/mcp")).send().await.unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert!(
            unauthorized
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("oauth-protected-resource/mcp")
        );
        assert!(
            unauthorized
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("posts:read posts:write")
        );

        let initialize = mcp_post(
            &client,
            &base,
            access,
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "blogger-test", "version": "1"}
                }
            }),
        )
        .await;
        assert_eq!(initialize.status(), StatusCode::OK);
        let session = initialize
            .headers()
            .get("Mcp-Session-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(sse_json(&initialize.text().await.unwrap())["id"], 1);

        let initialized = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert!(initialized.status().is_success());

        let listed = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        )
        .await;
        let listed = sse_json(&listed.text().await.unwrap());
        let names = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "append_draft",
                "create_draft",
                "edit_draft",
                "get_post",
                "get_writing_style",
                "list_archive",
                "list_tags",
                "replace_draft",
                "replace_writing_style",
                "search_posts"
            ]
        );

        let style = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":20,"method":"tools/call",
                "params":{"name":"get_writing_style","arguments":{}}
            }),
        )
        .await;
        let style = sse_json(&style.text().await.unwrap());
        assert_eq!(style["result"]["isError"], false);
        let style = &style["result"]["structuredContent"];
        assert_eq!(style.as_object().unwrap().len(), 2);
        assert!(style["content"].as_str().unwrap().contains("narwhals"));
        let style_revision = style["revision"].as_str().unwrap().to_owned();

        let read_only_access = insert_read_only_token(&state);
        let denied_style_write = mcp_post(
            &client,
            &base,
            &read_only_access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":21,"method":"tools/call",
                "params":{"name":"replace_writing_style","arguments":{
                    "revision":style_revision,
                    "content":"must not be written"
                }}
            }),
        )
        .await;
        let denied_style_write = sse_json(&denied_style_write.text().await.unwrap());
        assert_eq!(denied_style_write["result"]["isError"], true);
        assert_eq!(
            denied_style_write["result"]["structuredContent"]["error"],
            "insufficient_scope"
        );

        let replaced_style = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":22,"method":"tools/call",
                "params":{"name":"replace_writing_style","arguments":{
                    "revision":style_revision,
                    "content":"# Writing style\n\nPrecise, warm, and concise.\n"
                }}
            }),
        )
        .await;
        let replaced_style = sse_json(&replaced_style.text().await.unwrap());
        assert_eq!(replaced_style["result"]["isError"], false);
        assert_eq!(
            replaced_style["result"]["structuredContent"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            fs::read_to_string(site.0.join(crate::writing_style::WRITING_STYLE_PATH)).unwrap(),
            "# Writing style\n\nPrecise, warm, and concise.\n"
        );

        let stale_style = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":23,"method":"tools/call",
                "params":{"name":"replace_writing_style","arguments":{
                    "revision":style_revision,
                    "content":"stale"
                }}
            }),
        )
        .await;
        let stale_style = sse_json(&stale_style.text().await.unwrap());
        assert_eq!(stale_style["result"]["isError"], true);
        assert_eq!(
            stale_style["result"]["structuredContent"]["error"],
            "revision_conflict"
        );

        let archive = mcp_post(
            &client,
            &base,
            &read_only_access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":24,"method":"tools/call",
                "params":{"name":"list_archive","arguments":{}}
            }),
        )
        .await;
        let archive = sse_json(&archive.text().await.unwrap());
        assert_eq!(
            archive["result"]["structuredContent"],
            json!(["Published", "Voice Draft"])
        );

        let tags = mcp_post(
            &client,
            &base,
            &read_only_access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":25,"method":"tools/call",
                "params":{"name":"list_tags","arguments":{}}
            }),
        )
        .await;
        let tags = sse_json(&tags.text().await.unwrap());
        assert_eq!(
            tags["result"]["structuredContent"],
            json!(["published", "shared", "voice"])
        );

        let searched = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"search_posts","arguments":{"query":"NARWHAL"}}
            }),
        )
        .await;
        let searched = sse_json(&searched.text().await.unwrap());
        assert_eq!(
            searched["result"]["structuredContent"][0]["path"],
            "post/2026/draft.md"
        );
        assert_eq!(searched["result"]["structuredContent"][0]["draft"], true);

        let retrieved = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":4,"method":"tools/call",
                "params":{"name":"get_post","arguments":{"path":"post/2026/draft.md"}}
            }),
        )
        .await;
        let retrieved = sse_json(&retrieved.text().await.unwrap());
        assert!(
            retrieved["result"]["structuredContent"]["content"]
                .as_str()
                .unwrap()
                .contains("distinctive narwhal")
        );

        let created = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":5,"method":"tools/call",
                "params":{"name":"create_draft","arguments":{
                    "front_matter":"title = \"Voice Written Post\"\ndate = 2026-08-12\ndraft = false\n[taxonomies]\ntags = [\"voice\"]",
                    "body":"Opening paragraph."
                }}
            }),
        )
        .await;
        let created = sse_json(&created.text().await.unwrap());
        assert_eq!(created["result"]["isError"], false);
        let created = &created["result"]["structuredContent"];
        assert_eq!(created["path"], "post/2026/voice-written-post.md");
        assert_eq!(created["draft"], true);
        assert!(
            created["message"]
                .as_str()
                .unwrap()
                .contains("Created draft")
        );
        let created_revision = created["revision"].as_str().unwrap().to_owned();

        let denied_write = mcp_post(
            &client,
            &base,
            &read_only_access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":6,"method":"tools/call",
                "params":{"name":"append_draft","arguments":{
                    "path":"post/2026/voice-written-post.md",
                    "revision":created_revision,
                    "text":"must not be written"
                }}
            }),
        )
        .await;
        let denied_write = sse_json(&denied_write.text().await.unwrap());
        assert_eq!(denied_write["result"]["isError"], true);
        assert_eq!(
            denied_write["result"]["structuredContent"]["error"],
            "insufficient_scope"
        );

        let appended = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name":"append_draft","arguments":{
                    "path":"post/2026/voice-written-post.md",
                    "revision":created_revision,
                    "text":"Second paragraph.",
                    "separator":"blank_line"
                }}
            }),
        )
        .await;
        let appended = sse_json(&appended.text().await.unwrap());
        assert_eq!(appended["result"]["isError"], false);
        let appended_revision = appended["result"]["structuredContent"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();

        let ambiguous = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":8,"method":"tools/call",
                "params":{"name":"edit_draft","arguments":{
                    "path":"post/2026/voice-written-post.md",
                    "revision":appended_revision,
                    "replacements":[
                        {"old_text":"paragraph.","new_text":"must not be written"}
                    ]
                }}
            }),
        )
        .await;
        let ambiguous = sse_json(&ambiguous.text().await.unwrap());
        assert_eq!(ambiguous["result"]["isError"], true);
        assert_eq!(
            ambiguous["result"]["structuredContent"]["error"],
            "replacement_ambiguous"
        );
        assert_eq!(ambiguous["result"]["structuredContent"]["match_count"], 2);

        let edited = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":9,"method":"tools/call",
                "params":{"name":"edit_draft","arguments":{
                    "path":"post/2026/voice-written-post.md",
                    "revision":appended_revision,
                    "replacements":[
                        {"old_text":"Opening paragraph.","new_text":"Revised opening."},
                        {"old_text":"Second paragraph.","new_text":"Revised ending."}
                    ]
                }}
            }),
        )
        .await;
        let edited = sse_json(&edited.text().await.unwrap());
        assert_eq!(edited["result"]["isError"], false);
        let edited_revision = edited["result"]["structuredContent"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();

        let stale = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":10,"method":"tools/call",
                "params":{"name":"append_draft","arguments":{
                    "path":"post/2026/voice-written-post.md",
                    "revision":created_revision,
                    "text":"stale"
                }}
            }),
        )
        .await;
        let stale = sse_json(&stale.text().await.unwrap());
        assert_eq!(stale["result"]["isError"], true);
        assert_eq!(
            stale["result"]["structuredContent"]["error"],
            "revision_conflict"
        );
        assert_eq!(
            stale["result"]["structuredContent"]["current_revision"],
            edited_revision
        );

        let replaced = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":11,"method":"tools/call",
                "params":{"name":"replace_draft","arguments":{
                    "path":"post/2026/voice-written-post.md",
                    "revision":edited_revision,
                    "front_matter":"title = \"Final Voice Draft\"\ndate = 2026-08-12\ndraft = false\n[taxonomies]\ntags = [\"complete\"]",
                    "body":"Final complete body."
                }}
            }),
        )
        .await;
        let replaced = sse_json(&replaced.text().await.unwrap());
        assert_eq!(replaced["result"]["isError"], false);
        assert_eq!(replaced["result"]["structuredContent"]["draft"], true);

        let published = crate::post_store::load_post(&site.0, "post/2026/published.md").unwrap();
        let published_denied = mcp_post(
            &client,
            &base,
            access,
            Some(&session),
            json!({
                "jsonrpc":"2.0","id":12,"method":"tools/call",
                "params":{"name":"append_draft","arguments":{
                    "path":published.path,
                    "revision":published.revision,
                    "text":"must not change"
                }}
            }),
        )
        .await;
        let published_denied = sse_json(&published_denied.text().await.unwrap());
        assert_eq!(published_denied["result"]["isError"], true);
        assert_eq!(
            published_denied["result"]["structuredContent"]["error"],
            "published_post"
        );
        assert_eq!(
            crate::post_store::load_post(&site.0, "post/2026/published.md")
                .unwrap()
                .content,
            "+++\ntitle = \"Published\"\ndate = 2026-08-12\ndraft = false\n[taxonomies]\ntags = [\"published\", \"shared\"]\n+++\nPublic material.\n"
        );

        let final_draft =
            crate::post_store::load_post(&site.0, "post/2026/voice-written-post.md").unwrap();
        assert_eq!(final_draft.title, "Final Voice Draft");
        assert!(final_draft.draft);
        assert!(final_draft.content.ends_with("+++\nFinal complete body."));

        for private_path in ["/", "/api/posts", "/auth/login", "/preview-site"] {
            let response = client
                .get(format!("{base}{private_path}"))
                .bearer_auth(access)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{private_path}");
        }

        let refreshed = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id.as_str()),
                ("refresh_token", refresh),
                ("resource", "https://mcp.example.com/mcp"),
                ("scope", FULL_SCOPE),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(refreshed.status(), StatusCode::OK);
        let refreshed: Value = refreshed.json().await.unwrap();
        let new_access = refreshed["access_token"].as_str().unwrap();
        let refresh_replay = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id.as_str()),
                ("refresh_token", refresh),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(refresh_replay.status(), StatusCode::BAD_REQUEST);

        let replay_revoked = mcp_post(
            &client,
            &base,
            new_access,
            None,
            json!({"jsonrpc":"2.0","id":8,"method":"initialize","params":{}}),
        )
        .await;
        assert_eq!(replay_revoked.status(), StatusCode::UNAUTHORIZED);

        let second_authorized = authorize_code(
            &client,
            &base,
            &client_id,
            &challenge,
            "second-state",
            "password",
            CLAUDE_CALLBACK,
        )
        .await;
        let second_code = redirect_code(&second_authorized, "second-state", CLAUDE_CALLBACK);
        let second_tokens: Value = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", second_code.as_str()),
                ("code_verifier", verifier),
                ("resource", "https://mcp.example.com/mcp"),
            ])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let second_access = second_tokens["access_token"].as_str().unwrap();
        let second_refresh = second_tokens["refresh_token"].as_str().unwrap();

        let revoked = client
            .post(format!("{base}/revoke"))
            .form(&[("token", second_access), ("client_id", client_id.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::OK);
        let rejected = mcp_post(
            &client,
            &base,
            second_access,
            None,
            json!({"jsonrpc":"2.0","id":9,"method":"initialize","params":{}}),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        let revoked_refresh = client
            .post(format!("{base}/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id.as_str()),
                ("refresh_token", second_refresh),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(revoked_refresh.status(), StatusCode::BAD_REQUEST);

        server.abort();
    }
}
