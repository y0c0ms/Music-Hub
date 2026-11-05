//! Reads every knob from the environment once at boot.
//!
//! `OIDC_ISSUER`, `OIDC_CLIENT_SECRET`, and `DATABASE_URL` are required; every
//! other variable has a default.

use std::collections::HashMap;

/// A configuration error surfaced at startup.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// A required environment variable was unset or empty.
    #[error("{0} is required")]
    Missing(&'static str),
}

/// Every runtime knob, resolved once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    pub port: String,
    /// e.g. `https://music.example.com` — must match the OIDC redirect URI.
    pub public_url: String,

    // OIDC confidential client: the secret never reaches the browser.
    pub oidc_issuer: String,
    pub oidc_client_id: String,
    pub oidc_client_secret: String,

    /// Postgres DSN for the self-owned library.
    pub database_url: String,

    /// Container path of the library root (`/music`).
    pub music_root: String,
    /// Where acquired tracks land, under `music_root`.
    pub requests_dir: String,
    /// The ONLY library-relative prefix a discovery dislike may delete.
    pub discovery_prefix: String,
    /// Where audiobooks live under `music_root`; the ONLY thing that makes a
    /// file a book.
    pub books_prefix: String,

    /// Sessions, covers cache, and other server state.
    pub state_dir: String,
    /// Preview clips.
    pub cache_dir: String,

    pub session_key: String,

    /// Shared secret for `/api/internal/*`, the service-to-service surface used
    /// by jobs that have no browser and therefore no session cookie.
    ///
    /// Empty does NOT mean "allow anything" — it disables those routes outright.
    /// An endpoint that silently becomes unauthenticated the moment its secret
    /// fails to mount turns a typo into an open scan trigger, so the direction
    /// a missing value fails in is closed.
    pub service_token: String,

    /// Server-wide ListenBrainz fallback account.
    pub listenbrainz_user: String,
    /// Per-Keycloak-user ListenBrainz overrides, keyed lower-cased.
    pub listenbrainz_users: HashMap<String, String>,
    /// Last.fm API key for discovery suggestions and genre enrichment. Empty
    /// disables both without erroring — they are optional niceties, not core.
    pub lastfm_api_key: String,

    // Operational alerts. Empty `ntfy_url` disables alerting entirely.
    pub ntfy_url: String,
    pub ntfy_topic: String,
    pub ntfy_token: String,
}

impl Config {
    /// Load configuration from the process environment.
    ///
    /// # Errors
    /// Returns [`ConfigError::Missing`] when `OIDC_ISSUER`, `OIDC_CLIENT_SECRET`,
    /// or `DATABASE_URL` is unset or empty.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::load(|k| std::env::var(k).ok())
    }

    /// Pure loader used by [`Config::from_env`] and by tests. `get` returns the
    /// raw value for a key, or `None` when unset.
    ///
    /// # Errors
    /// Returns [`ConfigError::Missing`] when a required variable is unset/empty.
    pub fn load(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        // Mirror Go's env(): treat empty as unset and fall back to the default.
        let env = |k: &str, def: &str| -> String {
            match get(k) {
                Some(v) if !v.is_empty() => v,
                _ => def.to_owned(),
            }
        };
        let require = |k: &'static str| -> Result<String, ConfigError> {
            match get(k) {
                Some(v) if !v.is_empty() => Ok(v),
                _ => Err(ConfigError::Missing(k)),
            }
        };

        let mut discovery_prefix = env("DISCOVERY_PREFIX", "explo/");
        // A trailing slash makes the delete guard a prefix test rather than a
        // substring test; "explo" would also match "explosive-albums/".
        if !discovery_prefix.ends_with('/') {
            discovery_prefix.push('/');
        }
        let mut books_prefix = env("BOOKS_PREFIX", "books/");
        if !books_prefix.ends_with('/') {
            books_prefix.push('/');
        }

        Ok(Self {
            port: env("PORT", "8080"),
            public_url: trim_end_slashes(&env("PUBLIC_URL", "http://localhost:8080")),
            oidc_issuer: require("OIDC_ISSUER")?,
            oidc_client_id: env("OIDC_CLIENT_ID", "music-hub"),
            oidc_client_secret: require("OIDC_CLIENT_SECRET")?,
            database_url: require("DATABASE_URL")?,
            music_root: env("MUSIC_ROOT", "/music"),
            requests_dir: env("REQUESTS_DIR", "requests"),
            discovery_prefix,
            books_prefix,
            state_dir: env("STATE_DIR", "/state"),
            cache_dir: env("CACHE_DIR", "/tmp/previews"),
            session_key: env("SESSION_KEY", ""),
            service_token: env("SERVICE_TOKEN", "").trim().to_owned(),
            listenbrainz_user: env("LISTENBRAINZ_USER", "").trim().to_owned(),
            listenbrainz_users: parse_user_map(&env("LISTENBRAINZ_USERS", "")),
            lastfm_api_key: env("LASTFM_API_KEY", "").trim().to_owned(),
            ntfy_url: trim_end_slashes(&env("NTFY_URL", "")),
            ntfy_topic: env("NTFY_TOPIC", "music-hub").trim().to_owned(),
            // TRIMMED, and it is not cosmetic. These three become HTTP HEADERS,
            // and a header value may not contain a newline - so a secret created
            // with `echo` instead of `printf` carries a trailing \n that makes
            // reqwest fail at BUILDER time with "builder error", before any
            // request is sent. Measured on production 2026-09-10: NTFY_TOKEN was
            // 33 bytes ending in \n, and every operational alert had been
            // silently dropped - including the yt-dlp-blocked warning that exists
            // precisely so the downloader cannot be dead for hours unnoticed.
            //
            // Every other credential here was already trimmed; this one was
            // missed, which is why trimming belongs at the boundary rather than
            // in whichever call site remembers.
            ntfy_token: env("NTFY_TOKEN", "").trim().to_owned(),
        })
    }

    /// The ListenBrainz account for `keycloak_user`: a per-user override if one
    /// exists (matched case-insensitively), otherwise the server-wide fallback.
    #[must_use]
    pub fn server_listenbrainz_user(&self, keycloak_user: &str) -> &str {
        self.listenbrainz_users
            .get(&keycloak_user.to_lowercase())
            .map_or(self.listenbrainz_user.as_str(), String::as_str)
    }
}

fn trim_end_slashes(s: &str) -> String {
    s.trim_end_matches('/').to_owned()
}

/// Parse `"kcUser:lbUser, other:acct"` into a lower-cased-key map, dropping any
/// pair missing a key or value.
fn parse_user_map(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in s.split(',') {
        if let Some((k, v)) = pair.trim().split_once(':') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() && !v.is_empty() {
                out.insert(k.to_lowercase(), v.to_owned());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn getter(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("OIDC_ISSUER", "https://kc/realms/example"),
            ("OIDC_CLIENT_SECRET", "sekret"),
            ("DATABASE_URL", "postgres://localhost/musichub"),
        ]
    }

    #[test]
    fn defaults_apply_when_unset() {
        let c = Config::load(getter(&minimal())).unwrap();
        assert_eq!(c.port, "8080");
        assert_eq!(c.public_url, "http://localhost:8080");
        assert_eq!(c.oidc_client_id, "music-hub");
        assert_eq!(c.music_root, "/music");
        assert_eq!(c.discovery_prefix, "explo/");
        assert_eq!(c.books_prefix, "books/");
        assert_eq!(c.ntfy_topic, "music-hub");
    }

    #[test]
    fn missing_required_vars_error() {
        assert!(matches!(
            Config::load(getter(&[
                ("OIDC_CLIENT_SECRET", "s"),
                ("DATABASE_URL", "d")
            ])),
            Err(ConfigError::Missing("OIDC_ISSUER"))
        ));
        assert!(matches!(
            Config::load(getter(&[("OIDC_ISSUER", "i"), ("DATABASE_URL", "d")])),
            Err(ConfigError::Missing("OIDC_CLIENT_SECRET"))
        ));
        assert!(matches!(
            Config::load(getter(&[("OIDC_ISSUER", "i"), ("OIDC_CLIENT_SECRET", "s")])),
            Err(ConfigError::Missing("DATABASE_URL"))
        ));
    }

    #[test]
    fn prefixes_get_a_trailing_slash() {
        let mut pairs = minimal();
        pairs.push(("DISCOVERY_PREFIX", "explo"));
        pairs.push(("BOOKS_PREFIX", "books"));
        let c = Config::load(getter(&pairs)).unwrap();
        assert_eq!(c.discovery_prefix, "explo/");
        assert_eq!(c.books_prefix, "books/");
    }

    #[test]
    fn urls_lose_trailing_slashes() {
        let mut pairs = minimal();
        pairs.push(("PUBLIC_URL", "https://music.example/"));
        pairs.push(("NTFY_URL", "https://ntfy.example//"));
        let c = Config::load(getter(&pairs)).unwrap();
        assert_eq!(c.public_url, "https://music.example");
        assert_eq!(c.ntfy_url, "https://ntfy.example");
    }

    #[test]
    fn a_secret_with_a_trailing_newline_does_not_become_an_invalid_header() {
        // Production had exactly this: a 33-byte NTFY_TOKEN ending in \n, which
        // makes the Authorization header invalid and drops EVERY alert with
        // reqwest's opaque "builder error" - no request, no status, no clue.
        use std::collections::HashMap;
        let env: HashMap<String, String> = [
            ("OIDC_ISSUER", "https://kc/realms/example"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", "postgres://localhost/musichub"),
            ("NTFY_TOKEN", "tk_abc123\n"),
            ("NTFY_TOPIC", " alerts \n"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let c = Config::load(|k| env.get(k).cloned()).unwrap();
        assert_eq!(c.ntfy_token, "tk_abc123");
        assert_eq!(c.ntfy_topic, "alerts");
        assert!(
            !c.ntfy_token.contains('\n'),
            "a newline here silently kills alerting"
        );
    }

    #[test]
    fn listenbrainz_user_map_is_parsed_and_matched_case_insensitively() {
        let mut pairs = minimal();
        pairs.push((
            "LISTENBRAINZ_USERS",
            " Sam:sam_lb , bob:bob_lb ,bad, :x, y: ",
        ));
        pairs.push(("LISTENBRAINZ_USER", "fallback"));
        let c = Config::load(getter(&pairs)).unwrap();
        assert_eq!(c.listenbrainz_users.len(), 2);
        assert_eq!(c.server_listenbrainz_user("SAM"), "sam_lb");
        assert_eq!(c.server_listenbrainz_user("bob"), "bob_lb");
        assert_eq!(c.server_listenbrainz_user("nobody"), "fallback");
    }
}
