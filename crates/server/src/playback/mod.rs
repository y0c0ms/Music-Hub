//! Shared playback: one authoritative state per user, so every device signed
//! into an account sees the same queue, track, and position, and any of them can
//! drive it — while only one device makes sound.
//!
//! [`state`] is the pure transition core (ported 1:1 from Go); [`store`] holds
//! and persists it and fans changes out; the handlers here serve the HTTP + SSE
//! surface.

pub mod state;
pub mod store;

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderName, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};
use tokio_stream::StreamExt as _;

use crate::auth::Session;
use crate::{AppError, AppState};
use state::{Command, State as PlaybackState};
use store::CommandOutcome;

const HEARTBEAT_SECS: u64 = 10;

/// `?device=<id>&name=<label>` on the stream endpoint.
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    device: String,
    #[serde(default)]
    name: String,
}

fn state_event(s: &PlaybackState) -> Event {
    Event::default()
        .event("state")
        .json_data(s)
        .unwrap_or_else(|_| Event::default().event("state"))
}

fn ping_event() -> Event {
    Event::default()
        .event("ping")
        .data(Utc::now().timestamp_millis().to_string())
}

/// `GET /api/playback/stream` — the SSE endpoint every device holds open.
///
/// Sends the current state immediately, then `state` events on change and a
/// named `ping` event every 10s (a comment is invisible to Firefox's liveness
/// timer, so the keepalive must be a real event). `X-Accel-Buffering: no` stops
/// nginx from buffering the stream into a single delayed blob.
pub async fn stream(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Query(q): Query<StreamQuery>,
) -> Response {
    let (rx, current) = app
        .playback
        .subscribe(&session.username, &q.device, &q.name);
    let store = app.playback;
    let user = session.username;
    let device = q.device;

    let initial = tokio_stream::once(Ok::<Event, Infallible>(state_event(&current)));
    let updates = BroadcastStream::new(rx)
        .filter_map(|r| r.ok().map(|s| Ok::<Event, Infallible>(state_event(&s))));
    let pings = IntervalStream::new(tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS)))
        .map(move |_| {
            store.touch(&user, &device);
            Ok::<Event, Infallible>(ping_event())
        });
    let body = initial.chain(updates.merge(pings));

    (
        [(HeaderName::from_static("x-accel-buffering"), "no")],
        Sse::new(body),
    )
        .into_response()
}

/// `POST /api/playback/command` — apply one intent. A stale version is 409 WITH
/// the current state, so the caller can rebase and resend.
pub async fn command(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(cmd): Json<Command>,
) -> Response {
    match app.playback.do_command(&session.username, &cmd).await {
        CommandOutcome::Applied(s) => (StatusCode::OK, Json(s)).into_response(),
        CommandOutcome::Stale(s) => (StatusCode::CONFLICT, Json(s)).into_response(),
        CommandOutcome::Unknown => AppError::bad_request("unknown action").into_response(),
    }
}

/// `POST /api/playback/progress` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressBody {
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    position_ms: i32,
    #[serde(default)]
    playing: bool,
    #[serde(default)]
    duration_ms: i32,
}

/// `POST /api/playback/progress` — the active device reporting its position.
/// Only its report counts; a half-track listen records exactly one play.
pub async fn progress(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(body): Json<ProgressBody>,
) -> Json<PlaybackState> {
    let st = app.playback.progress(
        &session.username,
        &body.device_id,
        body.position_ms,
        body.playing,
    );
    maybe_scrobble(&app, &session, &st, &body);
    Json(st)
}

fn maybe_scrobble(app: &AppState, session: &Session, st: &PlaybackState, body: &ProgressBody) {
    if !body.playing || body.device_id.is_empty() || body.device_id != st.active_id {
        return;
    }
    let track = st.track_id();
    if track.is_empty()
        || !app
            .playback
            .scrobble_due(&session.username, track, body.position_ms, body.duration_ms)
    {
        return;
    }
    if let Ok(track_id) = uuid::Uuid::parse_str(track) {
        // Detached: Navidrome-free now, but the device is waiting on this
        // response to keep its progress bar honest, so the DB write does not
        // block it.
        let (pool, user_id) = (app.pool.clone(), session.user_id);
        tokio::spawn(async move {
            if let Err(e) = mh_db::mutate::scrobble(&pool, user_id, track_id).await {
                tracing::warn!(error = %e, "playback: scrobble failed");
            }
        });
    }
}

/// `GET /api/playback/devices` — what is signed in.
pub async fn devices(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
) -> Json<Vec<store::Device>> {
    Json(app.playback.devices(&session.username))
}

/// `POST /api/playback/transfer` body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferBody {
    device_id: String,
}

/// `POST /api/playback/transfer` — move sound to another device (404 if unseen).
pub async fn transfer(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
    Json(body): Json<TransferBody>,
) -> Response {
    app.playback
        .transfer(&session.username, &body.device_id)
        .await
        .map_or_else(
            || AppError::NotFound.into_response(),
            |s| Json(s).into_response(),
        )
}

/// `GET /api/playback/state` — a plain read for a client that will not subscribe.
pub async fn state_handler(
    State(app): State<AppState>,
    Extension(session): Extension<Session>,
) -> Json<PlaybackState> {
    Json(app.playback.get(&session.username))
}

/// `POST /api/playback/report` — a client failure to log, so a player that only
/// breaks on someone else's device is still diagnosable.
pub async fn report(
    Extension(session): Extension<Session>,
    Json(body): Json<Value>,
) -> Json<Value> {
    tracing::warn!(user = session.username, report = %body, "playback client report");
    Json(json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    fn session() -> Session {
        Session {
            user_id: Uuid::nil(),
            username: "sam".to_owned(),
            display_name: "Sam".to_owned(),
        }
    }

    fn state() -> AppState {
        use std::collections::HashMap;
        let env: HashMap<String, String> = [
            ("OIDC_ISSUER", "https://kc"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", "postgres://x/y"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let cfg = mh_core::Config::load(|k| env.get(k).cloned()).unwrap();
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://mh:mh@localhost/musichub")
            .unwrap();
        AppState::for_test(cfg, pool)
    }

    #[tokio::test]
    async fn state_handler_returns_default_for_unseen_user() {
        let Json(s) = state_handler(State(state()), Extension(session())).await;
        assert_eq!(s.version, 0);
        assert!(s.track_id().is_empty());
    }

    #[tokio::test]
    async fn stream_emits_initial_state_event() {
        let resp = stream(
            State(state()),
            Extension(session()),
            Query(StreamQuery {
                device: "d1".to_owned(),
                name: "Phone".to_owned(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "text/event-stream"
        );
        assert_eq!(resp.headers().get("x-accel-buffering").unwrap(), "no");

        let mut body = resp.into_body().into_data_stream();
        let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
            .await
            .expect("first SSE frame within 2s")
            .expect("stream yields a frame")
            .unwrap();
        let text = String::from_utf8_lossy(&chunk);
        assert!(text.contains("event: state"), "got: {text}");
        assert!(text.contains("\"playing\":false"), "got: {text}");
    }
}
