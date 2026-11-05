//! One authoritative playback state per user, fanned out to every subscribed device.
//!
//! A port of the Go `internal/playback` store, with two changes: state persists
//! to Postgres (`playback_state`) rather than a file, and subscribers use a
//! `tokio::broadcast` channel.
//!
//! The in-memory map is a `std::sync::Mutex`: every mutation is fast and sync,
//! and a broadcast `send` is non-blocking, so nothing ever `.await`s while the
//! guard is held — the guard is dropped before the Postgres persist runs.
// The mutex guard must live across the borrow of the entry it protects; these are
// short, synchronous critical sections, so tightening its scope is noise here.
#![allow(clippy::significant_drop_tightening)]

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use mh_db::Pool;
use serde::Serialize;
use tokio::sync::broadcast;

use super::state::{apply, Command, PlaybackError, State, REPEAT_OFF};

/// The device making sound may be unreachable this long before we declare nobody
/// is playing — long enough to survive a lock screen, short enough that a closed
/// laptop does not leave the UI claiming it plays.
const ACTIVE_GRACE_SECS: i64 = 30;
const DEVICE_FORGET_HOURS: i64 = 24;
const BROADCAST_CAPACITY: usize = 16;

/// One browser signed into the account.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub last_seen: DateTime<Utc>,
    pub active: bool,
}

/// The result of applying a command, so the handler can pick a status code.
pub enum CommandOutcome {
    /// Applied (possibly a no-op); the resulting state.
    Applied(State),
    /// Rejected as stale; the CURRENT state so the caller can rebase and resend.
    Stale(State),
    /// Unknown action.
    Unknown,
}

struct UserState {
    state: State,
    devices: HashMap<String, Device>,
    tx: broadcast::Sender<State>,
    // Scrobble bookkeeping, server-side so several devices sharing one state
    // cannot each submit a play.
    scrobble_track: String,
    scrobble_done: bool,
    last_pos: i32,
}

impl UserState {
    fn fresh(state: State) -> Self {
        Self {
            state,
            devices: HashMap::new(),
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            scrobble_track: String::new(),
            scrobble_done: false,
            last_pos: 0,
        }
    }
}

/// Per-user playback state plus a fan-out to its devices.
pub struct Store {
    pool: Pool,
    users: Mutex<HashMap<String, UserState>>,
}

/// The rand-backed shuffle a real transition uses; tests inject their own.
fn shuffle(mut order: Vec<i32>) -> Vec<i32> {
    use rand::seq::SliceRandom;
    order.shuffle(&mut rand::thread_rng());
    order
}

impl Store {
    /// Restore every persisted queue from Postgres. Nothing is restored as
    /// "playing": no device is connected yet, so a running bar with silence
    /// behind it would be a lie.
    ///
    /// # Errors
    /// Returns [`mh_db::DbError`] if the load query fails.
    pub async fn load(pool: Pool) -> Result<Self, mh_db::DbError> {
        let rows = sqlx::query_as::<_, (String, sqlx::types::Json<State>)>(
            "SELECT username, state FROM playback_state",
        )
        .fetch_all(&pool)
        .await?;
        let mut users = HashMap::new();
        for (username, state) in rows {
            let mut state = state.0;
            state.playing = false;
            state.active_id.clear();
            users.insert(username, UserState::fresh(state));
        }
        Ok(Self {
            pool,
            users: Mutex::new(users),
        })
    }

    /// A store with no persisted state loaded — for tests and callers that will
    /// not touch playback.
    #[must_use]
    pub fn empty(pool: Pool) -> Self {
        Self {
            pool,
            users: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, UserState>> {
        self.users
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn persist(&self, user: &str, state: &State) {
        let version = i64::try_from(state.version).unwrap_or(i64::MAX);
        let result = sqlx::query(
            "INSERT INTO playback_state (username, state, version) VALUES ($1, $2, $3)
             ON CONFLICT (username) DO UPDATE SET state = EXCLUDED.state, version = EXCLUDED.version",
        )
        .bind(user)
        .bind(sqlx::types::Json(state))
        .bind(version)
        .execute(&self.pool)
        .await;
        if let Err(e) = result {
            tracing::warn!(user, error = %e, "playback: cannot persist state");
        }
    }

    /// The current state for a user.
    #[must_use]
    pub fn get(&self, user: &str) -> State {
        let mut users = self.lock();
        entry(&mut users, user).state.clone()
    }

    /// Devices seen for a user, most recently seen first.
    #[must_use]
    pub fn devices(&self, user: &str) -> Vec<Device> {
        let mut users = self.lock();
        let us = entry(&mut users, user);
        let active = us.state.active_id.clone();
        let mut out: Vec<Device> = us
            .devices
            .values()
            .map(|d| Device {
                active: d.id == active,
                ..d.clone()
            })
            .collect();
        out.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        out
    }

    /// Apply a command, broadcasting and persisting when it changed anything.
    pub async fn do_command(&self, user: &str, cmd: &Command) -> CommandOutcome {
        let (outcome, changed) = {
            let mut users = self.lock();
            let us = entry(&mut users, user);
            match apply(&us.state, cmd, Utc::now(), &shuffle) {
                Ok(next) if next.version != us.state.version => {
                    us.state = next.clone();
                    touch(us, &cmd.device_id);
                    let _ = us.tx.send(next.clone());
                    (CommandOutcome::Applied(next.clone()), Some(next))
                }
                // A valid command that changed nothing does not wake every device.
                Ok(_) => (CommandOutcome::Applied(us.state.clone()), None),
                Err(PlaybackError::StaleVersion) => (CommandOutcome::Stale(us.state.clone()), None),
                Err(PlaybackError::UnknownAction) => (CommandOutcome::Unknown, None),
            }
        };
        if let Some(state) = changed {
            self.persist(user, &state).await;
        }
        outcome
    }

    /// Record a position report, but only from the device making sound; a
    /// remote's silent `<audio>` position means nothing. Position is not
    /// persisted (it is a high-frequency fact) but is broadcast so remotes track
    /// it live.
    #[must_use]
    pub fn progress(&self, user: &str, device_id: &str, position_ms: i32, playing: bool) -> State {
        let mut users = self.lock();
        let us = entry(&mut users, user);
        touch(us, device_id);
        if device_id.is_empty() || device_id != us.state.active_id {
            return us.state.clone();
        }
        us.state.position_ms = position_ms;
        us.state.position_at = Utc::now();
        us.state.playing = playing;
        let _ = us.tx.send(us.state.clone());
        us.state.clone()
    }

    /// Whether this report is the moment to record a play, latching once per
    /// listen. Navidrome's rule: at least half the track heard, duration known.
    pub fn scrobble_due(
        &self,
        user: &str,
        track: &str,
        position_ms: i32,
        duration_ms: i32,
    ) -> bool {
        if track.is_empty() {
            return false;
        }
        let mut users = self.lock();
        let us = entry(&mut users, user);
        if track != us.scrobble_track {
            track.clone_into(&mut us.scrobble_track);
            us.scrobble_done = false;
            us.last_pos = position_ms;
        }
        // Position jumping back is a fresh listen (repeat-one, or a seek to the
        // start); one second of slack absorbs a 5s reporter's jitter.
        if position_ms + 1000 < us.last_pos {
            us.scrobble_done = false;
        }
        us.last_pos = position_ms;
        if us.scrobble_done || duration_ms <= 0 || position_ms * 2 < duration_ms {
            return false;
        }
        us.scrobble_done = true;
        true
    }

    /// Move sound to another device. The target must have been seen.
    pub async fn transfer(&self, user: &str, device_id: &str) -> Option<State> {
        let state = {
            let mut users = self.lock();
            let us = entry(&mut users, user);
            if !us.devices.contains_key(device_id) {
                return None;
            }
            us.state.active_id = device_id.to_owned();
            us.state.version += 1;
            us.state.updated_at = Utc::now();
            let _ = us.tx.send(us.state.clone());
            us.state.clone()
        };
        self.persist(user, &state).await;
        Some(state)
    }

    /// Register a device and return a receiver for future states plus the current
    /// one to send immediately, so a fresh tab is never blank.
    #[must_use]
    pub fn subscribe(
        &self,
        user: &str,
        device_id: &str,
        name: &str,
    ) -> (broadcast::Receiver<State>, State) {
        let mut users = self.lock();
        let us = entry(&mut users, user);
        if !device_id.is_empty() {
            let d = us
                .devices
                .entry(device_id.to_owned())
                .or_insert_with(|| Device {
                    id: device_id.to_owned(),
                    name: String::new(),
                    last_seen: Utc::now(),
                    active: false,
                });
            if !name.is_empty() {
                name.clone_into(&mut d.name);
            }
            d.last_seen = Utc::now();
        }
        (us.tx.subscribe(), us.state.clone())
    }

    /// Record that a device is alive; the SSE loop calls it on every heartbeat.
    pub fn touch(&self, user: &str, device_id: &str) {
        let mut users = self.lock();
        touch(entry(&mut users, user), device_id);
    }

    /// Clear playback when the active device has gone quiet, and forget devices
    /// unseen for a day. Deliberately does NOT hand playback to another device.
    pub async fn reap_stale(&self) {
        let changed: Vec<(String, State)> = {
            let mut users = self.lock();
            let now = Utc::now();
            let mut changed = Vec::new();
            for (user, us) in users.iter_mut() {
                if reap_user(us, now) {
                    changed.push((user.clone(), us.state.clone()));
                }
            }
            changed
        };
        for (user, state) in changed {
            self.persist(&user, &state).await;
        }
    }
}

fn entry<'a>(users: &'a mut HashMap<String, UserState>, user: &str) -> &'a mut UserState {
    users
        .entry(user.to_owned())
        .or_insert_with(|| UserState::fresh(fresh_state()))
}

fn fresh_state() -> State {
    State {
        cursor: -1,
        repeat: REPEAT_OFF.to_owned(),
        ..State::default()
    }
}

fn touch(us: &mut UserState, device_id: &str) {
    if device_id.is_empty() {
        return;
    }
    let d = us
        .devices
        .entry(device_id.to_owned())
        .or_insert_with(|| Device {
            id: device_id.to_owned(),
            name: String::new(),
            last_seen: Utc::now(),
            active: false,
        });
    d.last_seen = Utc::now();
}

// Reap one user; returns whether the active device was cleared (so the caller
// persists and the broadcast below reaches every device).
fn reap_user(us: &mut UserState, now: DateTime<Utc>) -> bool {
    let mut cleared = false;
    if !us.state.active_id.is_empty() {
        let stale = us
            .devices
            .get(&us.state.active_id)
            .is_none_or(|d| now - d.last_seen > Duration::seconds(ACTIVE_GRACE_SECS));
        if stale {
            us.state.active_id.clear();
            us.state.playing = false;
            us.state.version += 1;
            us.state.updated_at = now;
            let _ = us.tx.send(us.state.clone());
            cleared = true;
        }
    }
    us.devices
        .retain(|_, d| now - d.last_seen <= Duration::hours(DEVICE_FORGET_HOURS));
    cleared
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn test_store() -> Option<(Store, String)> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let (base, _) = url.rsplit_once('/')?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = format!("mh_pb_{nanos}");
        let admin = PgPoolOptions::new()
            .connect(&format!("{base}/postgres"))
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE {db}"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .connect(&format!("{base}/{db}"))
            .await
            .unwrap();
        mh_db::migrate(&pool).await.unwrap();
        Some((Store::load(pool).await.unwrap(), db))
    }

    async fn drop_db(pool: &Pool, db: &str) {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if let Some((base, _)) = url.rsplit_once('/') {
                pool.close().await;
                if let Ok(admin) = PgPoolOptions::new()
                    .connect(&format!("{base}/postgres"))
                    .await
                {
                    let _ = sqlx::query(&format!("DROP DATABASE {db} WITH (FORCE)"))
                        .execute(&admin)
                        .await;
                }
            }
        }
    }

    fn play_cmd() -> Command {
        Command {
            action: "play".to_owned(),
            device_id: "dev1".to_owned(),
            tracks: vec!["a".to_owned(), "b".to_owned()],
            ..Command::default()
        }
    }

    #[tokio::test]
    async fn command_persists_broadcasts_and_rejects_stale() {
        let Some((store, db)) = test_store().await else {
            return;
        };

        // play -> version 1, track set, persisted.
        match store.do_command("sam", &play_cmd()).await {
            CommandOutcome::Applied(s) => {
                assert_eq!(s.version, 1);
                assert_eq!(s.track_id(), "a");
            }
            _ => panic!("expected Applied"),
        }
        let pool = store_pool(&store);
        let reloaded = Store::load(pool.clone()).await.unwrap();
        assert_eq!(reloaded.get("sam").queue, ["a", "b"]);
        assert!(!reloaded.get("sam").playing); // restored paused

        // a following command reaches a subscriber
        let mut sub = store.subscribe("sam", "dev1", "Phone").0;
        store
            .do_command(
                "sam",
                &Command {
                    action: "pause".to_owned(),
                    device_id: "dev1".to_owned(),
                    ..Command::default()
                },
            )
            .await;
        let got = sub.try_recv();
        assert!(got.is_ok(), "subscriber should receive the pause broadcast");

        // stale version is rejected with the current state
        let stale = Command {
            action: "pause".to_owned(),
            device_id: "dev1".to_owned(),
            version: 999,
            ..Command::default()
        };
        assert!(matches!(
            store.do_command("sam", &stale).await,
            CommandOutcome::Stale(_)
        ));

        drop_db(&pool, &db).await;
    }

    fn store_pool(store: &Store) -> Pool {
        store.pool.clone()
    }
}
