//! Keycloak OIDC (authorization-code + PKCE) and server-side sessions.
//!
//! Ported from the Go `internal/auth` package, minus the Navidrome-linking step:
//! the Keycloak identity IS the library user, so `RequireLinked` collapses to
//! [`require`] and `/api/me` always reports `navidrome_linked = true`. Tokens are
//! verified in [`callback`] and then dropped; only the session cookie survives,
//! and only SHA256(cookie) is stored.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration as StdDuration, Instant};

use axum::extract::{Extension, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use mh_core::{model, Config};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AppError, AppState};

const SESSION_COOKIE: &str = "mh_session";
const SESSION_TTL_DAYS: i64 = 30;
const PENDING_TTL: StdDuration = StdDuration::from_secs(600);

/// A resolved session, injected into request extensions by [`require`] and read
/// by handlers via `Extension<Session>`.
#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
}

/// A startup failure building the OIDC relying party.
#[derive(Debug, thiserror::Error)]
pub enum AuthInitError {
    #[error("oidc init: {0}")]
    Init(String),
}

/// One in-flight authorization request. The verifier and nonce never leave the
/// server; only the CSRF state token does, which is what makes the callback
/// verifiable and single-use.
struct Pending {
    verifier: PkceCodeVerifier,
    nonce: Nonce,
    expires: Instant,
}

/// [`CoreClient`] in the typestate `from_provider_metadata` yields: the auth URL
/// is always set; token and userinfo URLs depend on what discovery returned.
type DiscoveredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// The OIDC relying party: discovered client, the HTTP client it talks to the
/// provider through, pending-login store, and the cookie-security flag derived
/// from whether the public URL is HTTPS.
pub struct Auth {
    client: DiscoveredClient,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
    secure_cookies: bool,
}

impl Auth {
    /// Perform OIDC discovery and build the client. One attempt; the caller in
    /// `bin` wraps this in a bounded retry so a transient Keycloak 502 does not
    /// crash-loop the pod and sever live playback.
    ///
    /// # Errors
    /// Returns [`AuthInitError`] if the issuer URL is invalid, the HTTP client
    /// cannot be built, or discovery fails.
    pub async fn discover(config: &Config) -> Result<Self, AuthInitError> {
        let issuer = IssuerUrl::new(config.oidc_issuer.clone())
            .map_err(|e| AuthInitError::Init(e.to_string()))?;
        let http = oidc_http_client().map_err(|e| AuthInitError::Init(e.to_string()))?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|e| AuthInitError::Init(e.to_string()))?;
        let redirect = RedirectUrl::new(format!("{}/auth/callback", config.public_url))
            .map_err(|e| AuthInitError::Init(e.to_string()))?;
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(config.oidc_client_id.clone()),
            Some(ClientSecret::new(config.oidc_client_secret.clone())),
        )
        .set_redirect_uri(redirect);
        Ok(Self {
            client,
            http,
            pending: Mutex::new(HashMap::new()),
            secure_cookies: config.public_url.starts_with("https://"),
        })
    }

    fn put_pending(&self, state: String, verifier: PkceCodeVerifier, nonce: Nonce) {
        let mut map = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        map.insert(
            state,
            Pending {
                verifier,
                nonce,
                expires: Instant::now() + PENDING_TTL,
            },
        );
    }

    /// Single-use: a redeemed (or expired) state cannot be replayed.
    fn take_pending(&self, state: &str) -> Option<Pending> {
        let mut map = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        map.retain(|_, p| p.expires > now);
        map.remove(state)
    }
}

fn hash_id(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn new_session_token() -> String {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut token = String::with_capacity(64);
    for b in bytes {
        let _ = write!(token, "{b:02x}");
    }
    token
}

fn set_cookie_header(token: &str, secure: bool) -> String {
    let max_age = SESSION_TTL_DAYS * 24 * 3600;
    // SameSite=Lax, not Strict: the Keycloak callback is a top-level cross-site
    // GET, and Strict would withhold the cookie on exactly that navigation.
    let mut s =
        format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}");
    if secure {
        s.push_str("; Secure");
    }
    s
}

fn clear_cookie_header(secure: bool) -> String {
    let mut s = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        s.push_str("; Secure");
    }
    s
}

fn session_token_from(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        if let Some(value) = part.trim().strip_prefix("mh_session=") {
            return Some(value.to_owned());
        }
    }
    None
}

fn no_store(status: StatusCode, location: &str, set_cookie: Option<&str>) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(location) {
        headers.insert(header::LOCATION, value);
    }
    if let Some(cookie) = set_cookie {
        if let Ok(value) = HeaderValue::from_str(cookie) {
            headers.insert(header::SET_COOKIE, value);
        }
    }
    (status, headers).into_response()
}

/// The reqwest-0.12 client openidconnect drives (oauth2 implements
/// `AsyncHttpClient` for it) for discovery, JWKS, and token exchange. Built from
/// the shared TLS builder so the optional extra CA applies.
fn oidc_http_client() -> Result<reqwest::Client, reqwest::Error> {
    mh_core::tls::client_builder()
        // Following redirects here would open an SSRF hole.
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

fn auth_or_500(state: &AppState) -> Result<&Auth, AppError> {
    state
        .auth
        .as_deref()
        .ok_or_else(|| AppError::internal("oidc not configured"))
}

/// `GET /auth/login` — start the authorization-code flow with PKCE. A full-page
/// navigation, never a fetch: the redirect chain ends on Keycloak's own origin.
///
/// # Errors
/// Returns [`AppError::Internal`] when OIDC is not configured (unreachable in a
/// discovered deployment).
pub async fn login(State(state): State<AppState>) -> Result<Response, AppError> {
    let auth = auth_or_500(&state)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf, nonce) = auth
        .client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("profile".to_owned()))
        .add_scope(Scope::new("email".to_owned()))
        .set_pkce_challenge(challenge)
        .url();
    auth.put_pending(csrf.secret().clone(), verifier, nonce);
    Ok(no_store(StatusCode::FOUND, url.as_str(), None))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// `GET /auth/callback` — complete the flow. Every branch that is not a fully
/// verified token ends in 401; there is no path that trusts an unverified token.
///
/// # Errors
/// Returns [`AppError::Unauthorized`] on any verification failure, or
/// [`AppError::BadRequest`] on an incomplete callback.
pub async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let auth = auth_or_500(&state)?;
    if let Some(err) = query.error {
        // Logged, not reflected: the query string is attacker-controlled.
        tracing::warn!(error = %err, desc = ?query.error_description, "identity provider refused the login");
        return Err(AppError::Unauthorized);
    }
    let (Some(code), Some(csrf_state)) = (query.code, query.state) else {
        return Err(AppError::bad_request("incomplete callback"));
    };
    let pending = auth
        .take_pending(&csrf_state)
        .ok_or(AppError::Unauthorized)?;

    let token = auth
        .client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|e| {
            tracing::warn!(error = %e, "provider advertised no token endpoint");
            AppError::Unauthorized
        })?
        .set_pkce_verifier(pending.verifier)
        .request_async(&auth.http)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "code exchange failed");
            AppError::Unauthorized
        })?;
    let id_token = token.id_token().ok_or_else(|| {
        tracing::warn!("token response carried no id_token");
        AppError::Unauthorized
    })?;
    let claims = id_token
        .claims(&auth.client.id_token_verifier(), &pending.nonce)
        .map_err(|e| {
            tracing::warn!(error = %e, "id token did not verify");
            AppError::Unauthorized
        })?;

    let username = claims
        .preferred_username()
        .map(|u| u.as_str())
        .unwrap_or_default();
    if username.is_empty() {
        return Err(AppError::Unauthorized);
    }
    let display = claims
        .name()
        .and_then(|n| n.get(None))
        .map_or(username, |s| s.as_str());
    let subject = claims.subject().as_str();

    // The tokens go out of scope here; only these claims survive the request.
    mh_db::users::upsert(&state.pool, subject, username, display).await?;
    let token_str = new_session_token();
    let expires = Utc::now() + Duration::days(SESSION_TTL_DAYS);
    mh_db::sessions::create(&state.pool, &hash_id(&token_str), username, expires).await?;

    tracing::info!(user = username, "signed in");
    Ok(no_store(
        StatusCode::SEE_OTHER,
        "/",
        Some(&set_cookie_header(&token_str, auth.secure_cookies)),
    ))
}

/// `POST /auth/logout` — drop the server-side session, which is what actually
/// ends it; clearing the cookie is only tidiness.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let secure = state.auth.as_ref().is_some_and(|a| a.secure_cookies);
    if let Some(token) = session_token_from(&headers) {
        // Best-effort: a failed delete still clears the cookie.
        let _ = mh_db::sessions::delete(&state.pool, &hash_id(&token)).await;
    }
    let mut resp = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&clear_cookie_header(secure)) {
        resp.insert(header::SET_COOKIE, value);
    }
    (StatusCode::NO_CONTENT, resp).into_response()
}

/// `GET /api/me` — the signed-in identity. `navidrome_linked` is always true now
/// linking is gone, so the PWA's link screen never shows.
pub async fn me(Extension(session): Extension<Session>) -> impl IntoResponse {
    let user = model::User {
        username: session.username,
        display_name: session.display_name,
        navidrome_user: String::new(),
        navidrome_linked: true,
    };
    ([(header::CACHE_CONTROL, "no-store")], Json(user))
}

/// Middleware: reject anyone without a valid session, otherwise inject the
/// resolved [`Session`] into request extensions for downstream handlers.
///
/// # Errors
/// Returns [`AppError::Unauthorized`] when the cookie is missing or the session
/// is unknown/expired.
pub async fn require(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = session_token_from(req.headers()).ok_or(AppError::Unauthorized)?;
    let row = mh_db::sessions::resolve(&state.pool, &hash_id(&token))
        .await?
        .ok_or(AppError::Unauthorized)?;
    req.extensions_mut().insert(Session {
        user_id: row.user_id,
        username: row.username,
        display_name: row.display_name,
    });
    Ok(next.run(req).await)
}

/// Middleware: reject anyone not presenting `SERVICE_TOKEN` as a bearer token.
///
/// This is DELIBERATELY separate from [`require`] rather than an extra branch
/// inside it. `require` fronts every route the PWA calls; teaching it a second
/// way to say yes would widen the surface of a shared secret from one endpoint
/// to all forty, and a mistake there is invisible until someone audits it.
///
/// No [`Session`] is injected, which is the point: a service is not a user, has
/// no library of its own and must never reach a handler that reads
/// `Extension<Session>`. Axum enforces that for us — mounting a session-reading
/// handler behind this middleware fails at request time with a 500 rather than
/// silently acting as somebody.
///
/// # Errors
/// Returns [`AppError::Unauthorized`] when `SERVICE_TOKEN` is unset (the routes
/// are then disabled outright) or the presented token does not match.
pub async fn require_service(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let expected = state.config.service_token.as_str();
    // Closed, not open. An unset secret disables the route; the alternative is
    // an unauthenticated scan trigger the first time a Secret fails to mount.
    if expected.is_empty() {
        return Err(AppError::Unauthorized);
    }
    let presented = bearer_token_from(req.headers()).ok_or(AppError::Unauthorized)?;
    // Compare DIGESTS, not the secrets. Both operands are then a fixed 32 bytes,
    // so the comparison cannot leak the token's length or its matching prefix
    // through how early it bails out. (`Vec<u8>` equality is still not formally
    // constant-time; hashing removes the part an attacker could actually walk.)
    if hash_id(&presented) != hash_id(expected) {
        return Err(AppError::Unauthorized);
    }
    Ok(next.run(req).await)
}

fn bearer_token_from(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    // ASCII-case-insensitive: the scheme is case-insensitive per RFC 7235, and a
    // client sending "bearer" is not an authentication failure worth debugging.
    let (scheme, value) = raw.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn hash_is_stable_and_distinct() {
        assert_eq!(hash_id("abc"), hash_id("abc"));
        assert_ne!(hash_id("a"), hash_id("b"));
        assert_eq!(hash_id("abc").len(), 32);
    }

    #[test]
    fn session_token_is_64_hex_chars() {
        let token = new_session_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_session_token(), new_session_token());
    }

    #[test]
    fn cookie_headers_reflect_secure_flag() {
        let secure = set_cookie_header("tok", true);
        assert!(secure.contains("mh_session=tok"));
        assert!(secure.contains("HttpOnly"));
        assert!(secure.contains("SameSite=Lax"));
        assert!(secure.contains("Max-Age=2592000"));
        assert!(secure.contains("; Secure"));
        assert!(!set_cookie_header("tok", false).contains("; Secure"));
        assert!(clear_cookie_header(false).contains("Max-Age=0"));
    }

    #[test]
    fn parses_a_bearer_token_case_insensitively() {
        let bearer = |v: &'static str| {
            let mut h = HeaderMap::new();
            h.insert(header::AUTHORIZATION, HeaderValue::from_static(v));
            bearer_token_from(&h)
        };
        assert_eq!(bearer("Bearer s3cret").as_deref(), Some("s3cret"));
        // RFC 7235 makes the scheme case-insensitive; a client sending "bearer"
        // must not turn into a 401 someone spends an afternoon on.
        assert_eq!(bearer("bearer s3cret").as_deref(), Some("s3cret"));
        assert_eq!(bearer("BEARER  s3cret ").as_deref(), Some("s3cret"));
    }

    #[test]
    fn a_malformed_authorization_header_yields_no_token() {
        let bearer = |v: &'static str| {
            let mut h = HeaderMap::new();
            h.insert(header::AUTHORIZATION, HeaderValue::from_static(v));
            bearer_token_from(&h)
        };
        // Never confuse another scheme for ours: Basic credentials are base64 and
        // would otherwise be compared against SERVICE_TOKEN as if offered as one.
        assert!(bearer("Basic dXNlcjpwYXNz").is_none());
        assert!(bearer("Bearer").is_none());
        assert!(bearer("Bearer ").is_none());
        assert!(bearer("s3cret").is_none());
        assert!(bearer_token_from(&HeaderMap::new()).is_none());
    }

    #[test]
    fn parses_session_cookie_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; mh_session=deadbeef; b=2"),
        );
        assert_eq!(session_token_from(&headers).as_deref(), Some("deadbeef"));

        let mut none = HeaderMap::new();
        none.insert(header::COOKIE, HeaderValue::from_static("a=1; b=2"));
        assert_eq!(session_token_from(&none), None);
        assert_eq!(session_token_from(&HeaderMap::new()), None);
    }

    #[tokio::test]
    async fn me_reports_always_linked() {
        let session = Session {
            user_id: Uuid::nil(),
            username: "sam".to_owned(),
            display_name: "Sam".to_owned(),
        };
        let res = me(Extension(session)).await.into_response();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["username"], "sam");
        assert_eq!(v["displayName"], "Sam");
        assert_eq!(v["navidromeLinked"], true);
    }
}
