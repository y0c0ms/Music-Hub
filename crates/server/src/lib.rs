//! The axum application: routes, auth, handlers, and SSE.
//!
//! Fleshed out per phase. The router currently serves the two unauthenticated
//! endpoints (`/healthz`, `/api/version`) and the embedded PWA as a fallback; the
//! rest of the `/api/*` contract is added in the core-backend and stateful phases.
#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use mh_core::Config;
use serde_json::json;

pub mod acquire;
pub mod alert;
pub mod auth;
pub mod books;
pub mod discovery;
pub mod enrich;
pub mod error;
pub mod genre;
pub mod internal;
pub mod library;
pub mod mutate;
pub mod playback;
pub mod scan;
pub mod settings;
pub mod stream;
pub mod youtube;
pub use error::{AppError, AppResult};

/// Build stamp, injected by the Dockerfile via `MH_VERSION`/`MH_COMMIT`/`MH_BUILT_AT`
/// during compilation, mirroring the Go server's `-ldflags`.
pub const VERSION: &str = match option_env!("MH_VERSION") {
    Some(v) => v,
    None => "dev",
};
pub const COMMIT: &str = match option_env!("MH_COMMIT") {
    Some(v) => v,
    None => "unknown",
};
pub const BUILT_AT: &str = match option_env!("MH_BUILT_AT") {
    Some(v) => v,
    None => "dev",
};

/// Shared, cheaply-cloneable handler state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: mh_db::Pool,
    pub auth: Option<Arc<auth::Auth>>,
    pub playback: Arc<playback::store::Store>,
    pub acquire: Arc<acquire::Downloader>,
    pub books: Arc<books::Fetcher>,
    pub discovery: Arc<discovery::Discovery>,
}

#[cfg(test)]
impl AppState {
    /// A test state around a config and pool: no auth, empty playback, and a
    /// downloader with alerting disabled.
    pub(crate) fn for_test(config: Config, pool: mh_db::Pool) -> Self {
        let config = Arc::new(config);
        Self {
            playback: Arc::new(playback::store::Store::empty(pool.clone())),
            acquire: Arc::new(acquire::Downloader::new(
                config.clone(),
                pool.clone(),
                alert::Notifier::disabled(),
            )),
            books: Arc::new(books::Fetcher::new(config.clone())),
            discovery: Arc::new(discovery::Discovery::new(config.clone())),
            config,
            pool,
            auth: None,
        }
    }
}

/// Assemble the router. Kept separate from `bin` so tests can exercise handlers
/// without binding a socket.
pub fn build_router(state: AppState) -> Router {
    // Everything the PWA calls behind a session lives here.
    let protected = Router::new()
        .route("/api/me", get(auth::me))
        // Ported late: the settings TABLE came across at the cutover but these
        // handlers did not, so the Settings screen 404ed and nobody could link
        // their own ListenBrainz account - which is why two people were getting
        // one person's recommendations.
        .route("/api/settings", get(settings::get))
        .route(
            "/api/settings/listenbrainz",
            put(settings::set_listenbrainz).delete(settings::clear_listenbrainz),
        )
        .route("/api/library/scan", post(scan::scan_handler))
        .route("/api/library/home", get(library::home))
        .route("/api/library/songs", get(library::songs))
        .route(
            "/api/library/song/:id",
            get(library::song).delete(discovery::delete_song),
        )
        .route("/api/library/similar/:id", get(library::similar))
        .route("/api/library/albums", get(library::albums))
        .route("/api/library/album/:id", get(library::album))
        .route("/api/library/artists", get(library::artists))
        .route("/api/library/artist/:id", get(library::artist))
        .route("/api/library/genres", get(library::genres))
        .route("/api/library/storage", get(library::storage))
        .route("/api/library/starred", get(library::starred))
        .route("/api/search", get(library::search))
        .route(
            "/api/library/playlists",
            get(library::playlists).post(mutate::create_playlist),
        )
        .route(
            "/api/library/playlist/:id",
            get(library::playlist)
                .patch(mutate::rename_playlist)
                .delete(mutate::delete_playlist),
        )
        .route(
            "/api/library/playlist/:id/tracks",
            post(mutate::add_to_playlist),
        )
        .route(
            "/api/library/star/:id",
            post(mutate::star).delete(mutate::unstar),
        )
        .route("/api/library/scrobble/:id", post(mutate::scrobble))
        .route("/api/stream/:id", get(stream::stream))
        .route("/api/cover/:id", get(stream::cover))
        .route("/api/playback/stream", get(playback::stream))
        .route("/api/playback/state", get(playback::state_handler))
        .route("/api/playback/devices", get(playback::devices))
        .route("/api/playback/command", post(playback::command))
        .route("/api/playback/progress", post(playback::progress))
        .route("/api/playback/transfer", post(playback::transfer))
        .route("/api/playback/report", post(playback::report))
        .route("/api/acquire/search", get(acquire::search))
        .route("/api/acquire/albums", get(acquire::albums))
        .route("/api/acquire/album", get(acquire::album))
        .route("/api/acquire", post(acquire::download))
        .route("/api/acquire/jobs", get(acquire::jobs))
        .route("/api/acquire/album-missing", get(acquire::album_missing))
        .route("/api/books", get(books::list))
        .route("/api/books/search", get(books::search_http))
        .route("/api/books/sources", get(books::sources_http))
        .route("/api/books/jobs", get(books::jobs_http))
        .route("/api/books/get", post(books::get_http))
        .route("/api/books/chapters", post(books::chapters_http))
        .route(
            "/api/books/:id",
            get(books::get)
                .put(books::save_settings)
                .delete(books::delete),
        )
        .route("/api/books/:id/progress", put(books::save_progress))
        .route("/api/discovery/deck", get(discovery::deck))
        .route("/api/discovery/preview/:kind/:id", get(discovery::preview))
        .route("/api/discovery/meta/:id", get(discovery::meta))
        .route("/api/discovery/like", post(discovery::like))
        .route("/api/discovery/dislike", post(discovery::dislike))
        .route("/api/discovery/new-releases", get(discovery::new_releases))
        .route(
            "/api/discovery/cover/:playlist",
            get(discovery::playlist_cover),
        )
        .route(
            "/api/discovery/acquire-release",
            post(discovery::acquire_release),
        )
        .route(
            "/api/discovery/release-source",
            get(discovery::release_source),
        )
        .route(
            "/api/discovery/dismiss-release",
            post(discovery::dismiss_release),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require));

    // Service-to-service. `music-request` downloads a file straight into the
    // library and then has to say "it is there now", which is what Navidrome's
    // startScan did for it. It has no browser and so no session cookie, and
    // giving a background job a real user session to hold forever is a worse
    // answer than a scoped shared secret.
    //
    // Its own prefix rather than a second door onto /api/library/scan: the path
    // is then the audit surface, and `grep /api/internal` finds every route
    // reachable with that token.
    let internal = Router::new()
        .route("/api/internal/scan", post(scan::scan_handler))
        .route("/api/internal/retag-album", post(internal::retag_album))
        .route("/api/internal/owned", get(internal::owned))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_service,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/version", get(version))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .merge(protected)
        .merge(internal)
        .fallback(spa)
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "ok": true }))
}

// Deliberately unauthenticated: asking a running service what it is, without a
// session, is the first thing worth checking when a report and the code disagree.
async fn version() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "version": VERSION, "commit": COMMIT, "builtAt": BUILT_AT })),
    )
}

/// The built PWA, embedded at compile time. In debug builds rust-embed reads from
/// disk at runtime, so a `web/` rebuild is reflected without recompiling; release
/// builds bake the bytes into the binary.
mod web_assets {
    use rust_embed::RustEmbed;
    use std::borrow::Cow;

    #[derive(RustEmbed)]
    #[folder = "../../web/dist"]
    struct WebAssets;

    /// The bytes of an embedded asset, or `None` when it does not exist.
    pub fn get(path: &str) -> Option<Cow<'static, [u8]>> {
        WebAssets::get(path).map(|file| file.data)
    }
}

/// Serve the built PWA, falling back to `index.html` for client-side routes while
/// still returning a real 404 for a missing asset — and a JSON 404 for an
/// unregistered `/api/` path so a bad endpoint is never answered with HTML. A
/// faithful port of the Go `spa` handler.
async fn spa(uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let mut path = if raw.is_empty() { "index.html" } else { raw };

    let mut asset = web_assets::get(path);
    if asset.is_none() {
        if path.starts_with("api/") {
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":"no such endpoint"}"#,
            )
                .into_response();
        }
        if path.starts_with("assets/") || path.contains('.') {
            return StatusCode::NOT_FOUND.into_response();
        }
        // A client-side route: serve the app shell.
        path = "index.html";
        asset = web_assets::get(path);
    }
    let Some(content) = asset else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut headers = HeaderMap::new();
    // Go's mime table lacks .webmanifest, so it was set explicitly there; do the
    // same so Chrome treats the app as installable.
    let content_type = if path.ends_with(".webmanifest") {
        "application/manifest+json".to_owned()
    } else {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string()
    };
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    // The service worker and shell must never be cached, or a stale one pins the
    // app to an old build forever.
    if path.ends_with("sw.js") || path == "index.html" {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    (headers, content.into_owned()).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use std::collections::HashMap;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        state_with(&[])
    }

    /// A test state with `extra` layered over the required variables, so a test
    /// can set (or deliberately omit) one setting without restating the rest.
    fn state_with(extra: &[(&str, &str)]) -> AppState {
        let mut env: HashMap<String, String> = [
            ("OIDC_ISSUER", "https://kc/realms/example"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", "postgres://localhost/musichub"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        for (k, v) in extra {
            env.insert((*k).to_owned(), (*v).to_owned());
        }
        let cfg = Config::load(|k| env.get(k).cloned()).unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://mh:mh@localhost/musichub")
            .unwrap();
        AppState::for_test(cfg, pool)
    }

    async fn get(uri: &str) -> Response {
        build_router(test_state())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn json_body(res: Response) -> serde_json::Value {
        let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn healthz_returns_ok_json() {
        let res = get("/healthz").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(json_body(res).await, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn me_without_session_is_401() {
        let res = get("/api/me").await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(res).await, json!({ "error": "unauthenticated" }));
    }

    /// POST with an optional bearer token, against a state configured by `extra`.
    async fn post_internal_scan(extra: &[(&str, &str)], bearer: Option<&str>) -> StatusCode {
        let mut req = Request::builder().method("POST").uri("/api/internal/scan");
        if let Some(token) = bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        build_router(state_with(extra))
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn internal_scan_rejects_a_caller_with_no_token() {
        let status = post_internal_scan(&[("SERVICE_TOKEN", "right")], None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn internal_scan_rejects_the_wrong_token() {
        let status = post_internal_scan(&[("SERVICE_TOKEN", "right")], Some("wrong")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // A prefix of the real token must be as rejected as anything else - the
        // comparison hashes both sides precisely so a prefix cannot be walked.
        assert_eq!(
            post_internal_scan(&[("SERVICE_TOKEN", "right")], Some("righ")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn an_unset_service_token_closes_the_route_rather_than_opening_it() {
        // THE failure this guards. If SERVICE_TOKEN is missing - a Secret that did
        // not mount, a typo in the key name - the empty string must not become a
        // password that every caller happens to know. Note the caller here sends
        // an empty bearer AND a non-empty one; both are refused.
        assert_eq!(
            post_internal_scan(&[], Some("anything")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post_internal_scan(&[], None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_right_token_gets_past_the_gate() {
        // Not asserting 200: the handler behind it walks MUSIC_ROOT and talks to
        // Postgres, neither of which exists in a unit test. The property under
        // test is that the middleware ACCEPTED - anything other than 401 means the
        // request reached the handler, which is what distinguishes a working
        // secret from a rejected one.
        let status = post_internal_scan(&[("SERVICE_TOKEN", "right")], Some("right")).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn every_internal_route_is_behind_the_service_gate() {
        // Asserted per-route, not per-middleware: `.route_layer` applies to the
        // routes registered on that Router, so a route added to the wrong builder
        // is unauthenticated with no compile error and no failing middleware test.
        for (method, uri) in [
            ("POST", "/api/internal/scan"),
            ("POST", "/api/internal/retag-album"),
            ("GET", "/api/internal/owned"),
        ] {
            let res = build_router(state_with(&[("SERVICE_TOKEN", "right")]))
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} is open"
            );
        }
    }

    #[tokio::test]
    async fn the_service_token_does_not_open_the_session_routes() {
        // The two gates must stay separate: a service secret is not a login, and
        // /api/me behind it would hand a background job somebody's identity.
        let res = build_router(state_with(&[("SERVICE_TOKEN", "right")]))
            .oneshot(
                Request::builder()
                    .uri("/api/me")
                    .header(header::AUTHORIZATION, "Bearer right")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn version_is_no_store_and_carries_a_build_stamp() {
        let res = get("/api/version").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let v = json_body(res).await;
        assert!(v.get("version").is_some());
        assert!(v.get("commit").is_some());
        assert!(v.get("builtAt").is_some());
    }

    #[tokio::test]
    async fn root_serves_the_app_shell() {
        let res = get("/").await;
        assert_eq!(res.status(), StatusCode::OK);
        let ctype = res.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ctype.to_str().unwrap().starts_with("text/html"));
    }

    #[tokio::test]
    async fn unknown_client_route_falls_back_to_shell() {
        let res = get("/library/albums").await;
        assert_eq!(res.status(), StatusCode::OK);
        let ctype = res.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ctype.to_str().unwrap().starts_with("text/html"));
    }

    #[tokio::test]
    async fn unregistered_api_path_is_json_404() {
        let res = get("/api/does-not-exist").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(res).await, json!({ "error": "no such endpoint" }));
    }

    #[tokio::test]
    async fn missing_asset_is_plain_404() {
        let res = get("/assets/nope-12345.js").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
