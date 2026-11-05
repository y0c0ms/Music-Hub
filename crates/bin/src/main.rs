//! The `music-hub` binary: load config, build the router, serve with graceful
//! shutdown on SIGTERM/Ctrl-C.
#![forbid(unsafe_code)]

use std::sync::Arc;

use mh_core::Config;
use mh_server::{build_router, AppState, COMMIT, VERSION};
use tokio::net::TcpListener;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Config::from_env()?;
    let addr = format!("0.0.0.0:{}", config.port);
    let public = config.public_url.clone();

    let pool = mh_db::connect(&config.database_url).await?;
    mh_db::migrate(&pool).await?;
    tracing::info!("migrations applied");

    let auth = discover_auth(&config).await?;

    let playback = Arc::new(mh_server::playback::store::Store::load(pool.clone()).await?);
    spawn_reaper(playback.clone());

    let config = Arc::new(config);
    let alerts =
        mh_server::alert::Notifier::new(&config.ntfy_url, &config.ntfy_topic, &config.ntfy_token);
    let acquire = Arc::new(mh_server::acquire::Downloader::new(
        config.clone(),
        pool.clone(),
        alerts,
    ));
    let books = Arc::new(mh_server::books::Fetcher::new(config.clone()));
    let discovery = Arc::new(mh_server::discovery::Discovery::new(config.clone()));
    mh_server::genre::run_enricher(pool.clone(), config.clone());
    mh_server::enrich::run_year_enricher(pool.clone());
    mh_server::discovery::run_precompute(pool.clone(), discovery.clone());

    let state = AppState {
        config,
        pool,
        auth: Some(Arc::new(auth)),
        playback,
        acquire,
        books,
        discovery,
    };
    // Index the library once at startup, in the background. Nothing else scans
    // a library that was copied in by hand: scans otherwise run only after a
    // download or through the internal API, so a fresh install showed an empty
    // app. Files already indexed are skipped by (mtime, size), so on a warm
    // restart this is one directory walk.
    let startup = state.clone();
    tokio::spawn(async move {
        match mh_server::scan::scan(&startup).await {
            Ok(r) => tracing::info!(
                indexed = r.indexed,
                scanned = r.scanned,
                "startup scan done"
            ),
            Err(e) => tracing::warn!(error = %e, "startup scan failed"),
        }
    });
    let app = build_router(state);

    let listener = TcpListener::bind(&addr).await?;
    tracing::info!(%addr, public, version = VERSION, commit = COMMIT, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("stopped");
    Ok(())
}

/// Discover OIDC with backoff instead of crashing on a transient Keycloak 502.
///
/// Keycloak returns 502 for a beat during its own restart/GC. A single failed
/// discovery used to exit the process, and the orchestrator would restart it straight into the
/// same 502 - a restart storm that severs live playback for every already
/// authenticated user, who need only their session cookie, not Keycloak. Ride a
/// transient outage out in one pod; a sustained one still gives up (login is dead
/// then anyway) and lets the restart policy take over.
async fn discover_auth(
    config: &Config,
) -> Result<mh_server::auth::Auth, mh_server::auth::AuthInitError> {
    let mut backoff = std::time::Duration::from_secs(2);
    let mut last = mh_server::auth::AuthInitError::Init("oidc discovery not attempted".to_owned());
    for attempt in 1..=10 {
        match mh_server::auth::Auth::discover(config).await {
            Ok(auth) => {
                tracing::info!(attempt, "oidc discovery complete");
                return Ok(auth);
            }
            Err(e) => {
                tracing::warn!(attempt, error = %e, "oidc discovery failed; retrying");
                last = e;
                if attempt < 10 {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                }
            }
        }
    }
    Err(last)
}

/// Sweep stale playback devices every 15 seconds.
fn spawn_reaper(store: Arc<mh_server::playback::store::Store>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            tick.tick().await;
            store.reap_stale().await;
        }
    });
}

/// Resolve on the first of Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
