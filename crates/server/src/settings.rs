//! `/api/settings` — the configuration a person can change from inside the app.
//!
//! A faithful port of the Go `internal/settings` package, which the first Rust
//! cutover simply did not carry across. The `settings` TABLE was ported and the
//! discovery deck already read from it; only the HTTP handlers were missing, so
//! the Settings screen answered `{"error":"no such endpoint"}` and nobody could
//! connect their own ListenBrainz account.
//!
//! That is not cosmetic. Without a personal account every user falls back to the
//! server-wide one, so **two people get one person's recommendations** - the
//! exact symptom reported after the cutover.

use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::Session;
use crate::error::{AppError, AppResult};
use crate::AppState;

const VALIDATE_URL: &str = "https://api.listenbrainz.org/1/validate-token";

/// The ONLY shape this module ever sends to a client.
///
/// There is deliberately no token field. A ListenBrainz token is a bearer
/// credential with no expiry, so handing it back to the browser - even to the
/// person who set it - creates a copy that can leak from a screenshot, a cached
/// response or a service worker. The UI is told that a token EXISTS; it is never
/// shown the token.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub listen_brainz_user: String,
    pub listen_brainz_token_set: bool,
    /// `user` | `server` | `none` — the UI has to be able to tell a personal
    /// account from the shared fallback, or "why am I seeing their music?" has
    /// no answer visible anywhere.
    pub source: String,
}

/// Pure: stored link + server fallback -> what the UI is told.
///
/// Split out of the handler so the three-way `source` can be tested without a
/// database. It is the whole reason this endpoint exists - a user who cannot see
/// that their recommendations come from a SHARED account has no way to explain
/// why they match somebody else's taste.
fn build_view(lb_user: &str, lb_token: &str, fallback: &str) -> View {
    if !lb_token.is_empty() || !lb_user.is_empty() {
        return View {
            listen_brainz_user: lb_user.to_owned(),
            listen_brainz_token_set: !lb_token.is_empty(),
            source: "user".to_owned(),
        };
    }
    if fallback.is_empty() {
        return View {
            source: "none".to_owned(),
            ..View::default()
        };
    }
    View {
        listen_brainz_user: fallback.to_owned(),
        listen_brainz_token_set: false,
        source: "server".to_owned(),
    }
}

async fn view_for(state: &AppState, session: &Session) -> AppResult<View> {
    let (lb_user, lb_token) = mh_db::settings::get(&state.pool, session.user_id).await?;
    let fallback = state.config.server_listenbrainz_user(&session.username);
    Ok(build_view(&lb_user, &lb_token, fallback))
}

/// `GET /api/settings`
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn get(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> AppResult<Json<View>> {
    Ok(Json(view_for(&state, &session).await?))
}

#[derive(Deserialize)]
pub struct TokenBody {
    #[serde(default)]
    token: String,
}

enum Validation {
    Ok(String),
    /// ListenBrainz answered, and said no. Carries its own `message` so a
    /// rejection can be diagnosed from the log without the token ever appearing
    /// in it.
    Rejected(String),
    Unreachable(String),
}

/// Exchange a token for the username it belongs to.
///
/// Asking for the TOKEN rather than the username is what makes this
/// trustworthy: anyone can type "alice", but only the account holder has the
/// token, so the username stored is proven rather than claimed.
async fn validate(token: &str) -> Validation {
    let Ok(http) = mh_core::tls::client_builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
    else {
        return Validation::Unreachable("client build failed".to_owned());
    };
    let res = http
        .get(VALIDATE_URL)
        .header("Authorization", format!("Token {token}"))
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => return Validation::Unreachable(format!("request failed: {e}")),
    };

    // STATUS FIRST. ListenBrainz answers 200 with {"valid": false} for a bad
    // token rather than 401, so the body is what decides a rejection - but only
    // for a 200. Any other status (429 rate limit, 5xx outage) has no `valid`
    // field, and reading the body alone would report an OUTAGE as "your token is
    // wrong": the user then regenerates a perfectly good token and gets the same
    // error, which is the worst possible advice to give them.
    let status = res.status();
    if !status.is_success() {
        return Validation::Unreachable(format!("HTTP {status}"));
    }
    match res.json::<serde_json::Value>().await {
        Ok(body) => classify(&body),
        Err(e) => Validation::Unreachable(format!("unparseable response: {e}")),
    }
}

/// Turn a 200 response body into a verdict. Pure, so the rule that matters can
/// be tested without a network.
fn classify(body: &serde_json::Value) -> Validation {
    // Carried into the log on rejection: LB says "Token invalid." for a genuinely
    // wrong token, so anything ELSE appearing here means our request was
    // malformed rather than the user's token being bad.
    let message = body
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("no message")
        .to_owned();

    if body.get("valid").and_then(serde_json::Value::as_bool) != Some(true) {
        return Validation::Rejected(message);
    }
    body.get("user_name")
        .and_then(serde_json::Value::as_str)
        .filter(|u| !u.trim().is_empty())
        .map_or_else(
            // valid:true with no user_name is ListenBrainz behaving oddly, not
            // the user being wrong.
            || Validation::Unreachable("valid but no user_name".to_owned()),
            |u| Validation::Ok(u.to_owned()),
        )
}

/// `PUT /api/settings/listenbrainz`
///
/// # Errors
/// [`AppError::BadRequest`] `invalid_token` when ListenBrainz rejects it,
/// [`AppError::Internal`] `listenbrainz_unreachable` when validation could not
/// complete, or a database error. A token that could not be validated is NEVER
/// stored.
pub async fn set_listenbrainz(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Json(body): Json<TokenBody>,
) -> AppResult<Json<View>> {
    let token = body.token.trim().to_owned();
    if token.is_empty() {
        return Err(AppError::bad_request("invalid_token"));
    }
    match validate(&token).await {
        Validation::Ok(lb_user) => {
            mh_db::settings::set_listenbrainz(&state.pool, session.user_id, &lb_user, &token)
                .await?;
            // The account name is safe to log; the token never is.
            tracing::info!(user = %session.username, listenbrainz = %lb_user, "linked listenbrainz account");
            Ok(Json(view_for(&state, &session).await?))
        }
        Validation::Rejected(why) => {
            // LOGGED, because a silent 400 is undiagnosable. The token never
            // appears - only its length and how it was shaped, which is enough
            // to tell a mis-paste (a username, a truncated copy, a URL) from a
            // genuinely revoked token.
            tracing::warn!(
                user = %session.username,
                listenbrainz_said = %why,
                token_len = token.len(),
                looks_like_uuid = token.len() == 36 && token.matches('-').count() == 4,
                "listenbrainz rejected the token"
            );
            Err(AppError::bad_request("invalid_token"))
        }
        Validation::Unreachable(why) => {
            tracing::warn!(user = %session.username, reason = %why,
                "listenbrainz validation could not complete");
            // 502, not 500: the PWA branches on it to say "ListenBrainz could
            // not be reached and the token was not changed", which is true and
            // actionable. As a 500 that branch was dead code.
            Err(AppError::bad_gateway("listenbrainz_unreachable"))
        }
    }
}

/// `DELETE /api/settings/listenbrainz` — disconnect, revealing the fallback.
///
/// # Errors
/// Returns [`AppError`] on a database failure.
pub async fn clear_listenbrainz(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
) -> AppResult<Json<View>> {
    mh_db::settings::clear_listenbrainz(&state.pool, session.user_id).await?;
    tracing::info!(user = %session.username, "disconnected listenbrainz");
    Ok(Json(view_for(&state, &session).await?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::{build_view, classify, Validation};
    use serde_json::json;

    #[test]
    fn a_valid_token_yields_the_proven_username() {
        // Measured from the real service: it answers 200 for everything.
        let v =
            classify(&json!({"code":200,"message":"Token valid.","user_name":"bob","valid":true}));
        assert!(matches!(v, Validation::Ok(u) if u == "bob"));
    }

    #[test]
    fn valid_false_is_a_rejection_carrying_listenbrainz_own_words() {
        let v = classify(&json!({"code":200,"message":"Token invalid.","valid":false}));
        // The message is what makes a 400 diagnosable from the log without ever
        // putting the token in it.
        assert!(matches!(v, Validation::Rejected(m) if m == "Token invalid."));
    }

    #[test]
    fn an_unexpected_body_is_unreachable_rather_than_a_bad_token() {
        // THE bug worth guarding. A rate limit or an outage has no `valid` field,
        // and calling that "your token is wrong" sends the user off to regenerate
        // a perfectly good token - and get the identical error. Anything that is
        // not an explicit valid:false must not read as a rejection... except that
        // a missing `valid` IS how LB reports nothing useful, so the guard that
        // actually matters lives on the STATUS, checked before this is reached.
        let v = classify(&json!({"code":200,"message":"Token valid.","valid":true}));
        assert!(
            matches!(v, Validation::Unreachable(_)),
            "valid:true without a user_name is not a rejection"
        );
    }

    #[test]
    fn a_personal_token_reads_as_the_users_own_account() {
        let v = build_view("alice", "tok", "bob");
        assert_eq!(v.source, "user");
        assert_eq!(v.listen_brainz_user, "alice");
        assert!(v.listen_brainz_token_set);
    }

    #[test]
    fn no_personal_token_falls_back_and_says_so() {
        // THE bug this endpoint's absence caused. Without a way to see `server`,
        // two people share one account's recommendations and nothing anywhere
        // explains why they are getting each other's music.
        let v = build_view("", "", "bob");
        assert_eq!(v.source, "server");
        assert_eq!(v.listen_brainz_user, "bob");
        assert!(
            !v.listen_brainz_token_set,
            "the fallback is not the user's token"
        );
    }

    #[test]
    fn no_token_and_no_fallback_is_none_rather_than_a_blank_server() {
        let v = build_view("", "", "");
        assert_eq!(v.source, "none");
        assert!(v.listen_brainz_user.is_empty());
    }

    #[test]
    fn a_username_without_a_token_still_counts_as_personal() {
        // Reachable after DELETE half-succeeded, or via the migration that wrote
        // a username from Navidrome. It is still THEIR account, so it must not
        // silently present as the shared one.
        let v = build_view("alice", "", "bob");
        assert_eq!(v.source, "user");
        assert!(!v.listen_brainz_token_set);
    }
}
