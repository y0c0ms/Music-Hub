//! Short operational alerts to ntfy.
//!
//! A broken downloader is otherwise invisible: yt-dlp failures land in a pod log
//! nobody reads while the deck keeps handing out cards. Three rules shape this: an
//! alert must never break the thing it reports on (fire-and-forget, tight timeout,
//! errors swallowed); alerting once is useful but per-failure is a pager storm, so
//! sends are deduplicated by key with a cooldown; a recovery re-arms via `clear`.
//! An empty `ntfy_url` disables alerting entirely, which is what makes it optional
//! outside the cluster. A faithful port of the Go `internal/alert` package.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Between two alerts sharing a key: long enough that a bad afternoon is a handful
/// of messages, short enough that a fresh outage after lunch is not swallowed.
const COOLDOWN: Duration = Duration::from_secs(2 * 60 * 60);
/// Deliberately tight — this runs inside a download path; if ntfy is slow, the
/// alert is the thing that gets dropped.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Delivers deduplicated alerts to ntfy, or nothing when no URL is configured.
pub struct Notifier {
    inner: Option<Ntfy>,
}

struct Ntfy {
    url: String,
    token: String,
    client: reqwest::Client,
    last: Mutex<HashMap<String, Instant>>,
}

impl Notifier {
    /// A notifier for the given ntfy base URL and topic. An empty base URL or
    /// topic yields the no-op notifier.
    #[must_use]
    pub fn new(base_url: &str, topic: &str, token: &str) -> Self {
        if base_url.is_empty() || topic.is_empty() {
            return Self { inner: None };
        }
        let client = mh_core::tls::client_builder()
            .timeout(SEND_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            inner: Some(Ntfy {
                url: format!("{base_url}/{topic}"),
                token: token.to_owned(),
                client,
                last: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The disabled notifier, for tests and unconfigured deployments.
    #[must_use]
    pub const fn disabled() -> Self {
        Self { inner: None }
    }

    /// Fire an alert unless one with this key fired within the cooldown.
    ///
    /// Detached: the caller is mid-download and must not wait on a push to be
    /// delivered.
    pub fn fire(&self, key: &str, title: &str, body: &str) {
        let Some(ntfy) = &self.inner else {
            return;
        };
        {
            let mut last = lock(&ntfy.last);
            if last.get(key).is_some_and(|t| t.elapsed() < COOLDOWN) {
                return;
            }
            last.insert(key.to_owned(), Instant::now());
        }
        let client = ntfy.client.clone();
        let url = ntfy.url.clone();
        let token = ntfy.token.clone();
        let title = title.to_owned();
        let body = body.to_owned();
        tokio::spawn(async move {
            post(&client, &url, &token, &title, &body).await;
        });
    }

    /// Re-arm the key so the next failure alerts immediately.
    pub fn clear(&self, key: &str) {
        if let Some(ntfy) = &self.inner {
            lock(&ntfy.last).remove(key);
        }
    }
}

fn lock(
    m: &Mutex<HashMap<String, Instant>>,
) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn post(client: &reqwest::Client, url: &str, token: &str, title: &str, body: &str) {
    let mut req = client
        .post(url)
        .header("Title", title)
        .header("Tags", "warning")
        .header("Priority", "high")
        .body(body.to_owned());
    if !token.is_empty() {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    match req.send().await {
        Ok(resp) if resp.status().as_u16() >= 300 => {
            tracing::warn!(status = %resp.status(), "alert: ntfy answered");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "alert: send"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn disabled_notifier_never_panics() {
        let n = Notifier::disabled();
        n.fire("k", "t", "b"); // no runtime needed: returns before spawning
        n.clear("k");
    }

    #[tokio::test]
    async fn dedup_suppresses_a_second_fire_within_cooldown() {
        let n = Notifier::new("http://ntfy.invalid", "topic", "");
        let Some(inner) = &n.inner else {
            unreachable!("configured notifier has an inner");
        };
        n.fire("dup", "t", "b");
        assert_eq!(lock(&inner.last).len(), 1);
        let first = lock(&inner.last).get("dup").copied();
        n.fire("dup", "t", "b"); // within cooldown: timestamp unchanged
        assert_eq!(lock(&inner.last).get("dup").copied(), first);
        n.clear("dup");
        assert!(lock(&inner.last).is_empty());
    }
}
