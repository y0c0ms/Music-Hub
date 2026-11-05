//! The pure playback state machine: `State` plus `apply(state, command)` as a
//! function of its inputs with no I/O, clock, or global randomness.
//!
//! A faithful port of the Go `internal/playback/state.go`. The transitions
//! mirror `web/src/lib/player.js` exactly — the same `order`-as-indices model,
//! repeat rules, and "restart if more than three seconds in" prev behaviour — so
//! the UI never jumps when a broadcast lands. `now` and the shuffle function are
//! parameters, never ambient, so a test asserts an exact result.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Repeat modes, matching the client's `repeat` field exactly.
pub const REPEAT_OFF: &str = "off";
pub const REPEAT_ALL: &str = "all";
pub const REPEAT_ONE: &str = "one";

/// How far into a track "previous" restarts it rather than stepping back.
pub const PREV_RESTART_SECONDS: f64 = 3.0;

/// A shuffle function over queue indices, injected for determinism.
pub type Shuffle<'a> = dyn Fn(Vec<i32>) -> Vec<i32> + 'a;

/// A playback command that could not be applied.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlaybackError {
    /// Another device changed the state first; resend against the returned state.
    #[error("playback: stale version")]
    StaleVersion,
    /// A command this server does not implement.
    #[error("playback: unknown action")]
    UnknownAction,
}

fn epoch() -> DateTime<Utc> {
    DateTime::from_timestamp(0, 0).unwrap_or_else(Utc::now)
}

/// The whole of what a user is playing, broadcast verbatim to every device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// Track ids, not track objects: full metadata would bloat every frame.
    pub queue: Vec<String>,
    /// Playback sequence as indices into `queue`; this is what makes shuffle
    /// non-destructive.
    pub order: Vec<i32>,
    pub cursor: i32,
    pub playing: bool,
    pub shuffle: bool,
    pub repeat: String,
    /// A remote renders a moving bar from `position_ms + (now - position_at)`.
    pub position_ms: i32,
    pub position_at: DateTime<Utc>,
    /// The device currently making sound; empty means nobody is.
    pub active_id: String,
    /// Monotonic per user; a command carrying a stale version is rejected.
    pub version: u64,
    /// Bumps only when a COMMAND moved the position (seek/jump/track change), so
    /// the active device can tell a scrub from an unrelated state change.
    pub position_seq: u64,
    pub updated_at: DateTime<Utc>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            order: Vec::new(),
            cursor: 0,
            playing: false,
            shuffle: false,
            repeat: String::new(),
            position_ms: 0,
            position_at: epoch(),
            active_id: String::new(),
            version: 0,
            position_seq: 0,
            updated_at: epoch(),
        }
    }
}

impl State {
    /// The track the cursor points at, or `""` when there is none.
    #[must_use]
    pub fn track_id(&self) -> &str {
        at(&self.order, self.cursor)
            .and_then(|&i| at(&self.queue, i))
            .map_or("", String::as_str)
    }
}

/// One intent from one device.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub action: String,
    #[serde(default)]
    pub device_id: String,
    /// 0 means "don't care", used by first-write clients.
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub tracks: Vec<String>,
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub cursor: i32,
    #[serde(default)]
    pub seconds: f64,
    #[serde(default)]
    pub track_id: String,
    #[serde(default)]
    pub repeat: String,
    /// next: a user tap versus a track ending.
    #[serde(default)]
    pub manual: bool,
    /// `play` only. `None` follows the persisted shuffle flag; `Some(false)` plays the
    /// list in order (album Play) without touching the flag the shuffle button owns.
    #[serde(default)]
    pub shuffle: Option<bool>,
}

/// Index into a slice by an `i32`, returning `None` for out-of-range or negative.
fn at<T>(xs: &[T], i: i32) -> Option<&T> {
    usize::try_from(i).ok().and_then(|i| xs.get(i))
}

fn index_of_int(xs: &[i32], want: i32) -> i32 {
    let pos = xs.iter().position(|&x| x == want).unwrap_or(0);
    i32::try_from(pos).unwrap_or(0)
}

fn index_of(xs: &[String], want: &str) -> Option<usize> {
    xs.iter().position(|x| x == want)
}

/// Mirror player.js buildOrder: the tapped track stays first and only what comes
/// after is randomised, so tapping a song plays that song.
fn build_order(count: usize, first: i32, shuffle: bool, sh: &Shuffle) -> Vec<i32> {
    let all: Vec<i32> = (0..count)
        .map(|i| i32::try_from(i).unwrap_or(i32::MAX))
        .collect();
    if !shuffle {
        return all;
    }
    let rest: Vec<i32> = all.iter().copied().filter(|&i| i != first).collect();
    let mut out = vec![first];
    out.extend(sh(rest));
    out
}

/// Apply `cmd` to `s`, returning the resulting state. Never mutates `s`.
///
/// A no-op command returns `s` unchanged (no version bump); an applied one bumps
/// the version, stamps the clock, and enforces the "sound requires a device"
/// invariant.
///
/// # Errors
/// [`PlaybackError::StaleVersion`] if `cmd.version` disagrees with `s.version`;
/// [`PlaybackError::UnknownAction`] for an unrecognised action.
pub fn apply(
    s: &State,
    cmd: &Command,
    now: DateTime<Utc>,
    sh: &Shuffle,
) -> Result<State, PlaybackError> {
    if cmd.version != 0 && cmd.version != s.version {
        return Err(PlaybackError::StaleVersion);
    }
    let mut next = s.clone();
    let applied = match cmd.action.as_str() {
        "play" => play(&mut next, s, cmd, sh),
        "enqueue" => enqueue(&mut next, s, cmd, sh),
        "next" => advance(&mut next, s, cmd, sh),
        "prev" => prev(&mut next, s),
        "jump" => jump(&mut next, s, cmd),
        "seek" => seek(&mut next, cmd),
        "toggle" => toggle(&mut next, s, cmd),
        "pause" => pause(&mut next),
        "stop" => stop(&mut next),
        "shuffle" => shuffle_toggle(&mut next, s, sh),
        "repeat" => repeat_set(&mut next, s, cmd),
        "drop" => drop_track(&mut next, s, cmd),
        "removeAt" => remove_at(&mut next, s, cmd),
        _ => return Err(PlaybackError::UnknownAction),
    };
    if !applied {
        return Ok(s.clone());
    }
    enforce_active_invariant(&mut next, cmd);
    next.version = s.version + 1;
    next.updated_at = now;
    next.position_at = now;
    if next.position_ms != s.position_ms || next.cursor != s.cursor {
        next.position_seq = s.position_seq + 1;
    }
    Ok(next)
}

// Each transition returns true when it changed the state and false for a no-op
// (which makes `apply` return `s` unchanged, exactly like the Go `return s, nil`).

fn play(next: &mut State, s: &State, cmd: &Command, sh: &Shuffle) -> bool {
    if cmd.tracks.is_empty() {
        return false;
    }
    let idx = match usize::try_from(cmd.index) {
        Ok(i) if i < cmd.tracks.len() => cmd.index,
        _ => 0,
    };
    next.queue.clone_from(&cmd.tracks);
    // An explicit `shuffle` on the command wins; the persisted flag is left alone.
    let shuffle = cmd.shuffle.unwrap_or(s.shuffle);
    next.order = build_order(cmd.tracks.len(), idx, shuffle, sh);
    // Shuffled orders put `idx` at position 0; sequential ones leave it, so the
    // cursor must be looked up rather than assumed.
    next.cursor = index_of_int(&next.order, idx);
    next.playing = true;
    next.position_ms = 0;
    claim_if_free(next, cmd);
    true
}

fn enqueue(next: &mut State, s: &State, cmd: &Command, sh: &Shuffle) -> bool {
    // Matches player.js: enqueue appends and never starts playback, and bails on
    // an empty queue.
    if cmd.tracks.is_empty() || s.queue.is_empty() {
        return false;
    }
    let start = i32::try_from(next.queue.len()).unwrap_or(i32::MAX);
    next.queue.extend(cmd.tracks.iter().cloned());
    let mut add: Vec<i32> = (0..cmd.tracks.len())
        .map(|i| start + i32::try_from(i).unwrap_or(0))
        .collect();
    if s.shuffle {
        add = sh(add);
    }
    next.order.extend(add);
    true
}

fn advance(next: &mut State, s: &State, cmd: &Command, sh: &Shuffle) -> bool {
    if s.order.is_empty() {
        return false;
    }
    // repeat=one only repeats when the track ENDED, never on a user tap.
    if !cmd.manual && s.repeat == REPEAT_ONE {
        next.position_ms = 0;
        next.playing = true;
        return true;
    }
    if s.cursor + 1 < i32::try_from(s.order.len()).unwrap_or(i32::MAX) {
        next.cursor = s.cursor + 1;
        next.position_ms = 0;
        next.playing = true;
        return true;
    }
    if s.repeat == REPEAT_ALL {
        if s.shuffle {
            next.order = sh(s.order.clone());
        }
        next.cursor = 0;
        next.position_ms = 0;
        next.playing = true;
        return true;
    }
    // Past the end with no repeat: stop, keep the queue for the UI.
    next.playing = false;
    next.position_ms = 0;
    true
}

fn prev(next: &mut State, s: &State) -> bool {
    if s.order.is_empty() {
        return false;
    }
    // Restart first, like every other music player.
    if f64::from(s.position_ms) / 1000.0 > PREV_RESTART_SECONDS || s.cursor <= 0 {
        next.position_ms = 0;
        return true;
    }
    next.cursor = s.cursor - 1;
    next.position_ms = 0;
    next.playing = true;
    true
}

fn jump(next: &mut State, s: &State, cmd: &Command) -> bool {
    if cmd.cursor < 0 || cmd.cursor >= i32::try_from(s.order.len()).unwrap_or(i32::MAX) {
        return false;
    }
    next.cursor = cmd.cursor;
    next.position_ms = 0;
    next.playing = true;
    true
}

#[allow(clippy::cast_possible_truncation)]
fn seek(next: &mut State, cmd: &Command) -> bool {
    let seconds = cmd.seconds.max(0.0);
    next.position_ms = (seconds * 1000.0) as i32;
    true
}

fn toggle(next: &mut State, s: &State, cmd: &Command) -> bool {
    if s.track_id().is_empty() {
        return false;
    }
    next.playing = !s.playing;
    if next.playing {
        claim_if_free(next, cmd);
    }
    true
}

const fn pause(next: &mut State) -> bool {
    next.playing = false;
    true
}

fn stop(next: &mut State) -> bool {
    next.queue.clear();
    next.order.clear();
    next.cursor = -1;
    next.playing = false;
    next.position_ms = 0;
    next.active_id.clear();
    true
}

fn shuffle_toggle(next: &mut State, s: &State, sh: &Shuffle) -> bool {
    next.shuffle = !s.shuffle;
    if let Some(&cur) = at(&s.order, s.cursor) {
        next.order = build_order(s.queue.len(), cur, next.shuffle, sh);
        // Shuffle keeps the current track first; un-shuffle follows the track.
        next.cursor = if next.shuffle { 0 } else { cur };
    }
    true
}

fn repeat_set(next: &mut State, s: &State, cmd: &Command) -> bool {
    next.repeat = match cmd.repeat.as_str() {
        REPEAT_OFF | REPEAT_ALL | REPEAT_ONE => cmd.repeat.clone(),
        // Cycle off -> all -> one -> off, matching the client's button.
        _ => match s.repeat.as_str() {
            REPEAT_OFF => REPEAT_ALL,
            REPEAT_ALL => REPEAT_ONE,
            _ => REPEAT_OFF,
        }
        .to_owned(),
    };
    true
}

fn drop_track(next: &mut State, s: &State, cmd: &Command) -> bool {
    index_of(&s.queue, &cmd.track_id).is_some_and(|i| {
        *next = drop_index(s, i);
        true
    })
}

fn remove_at(next: &mut State, s: &State, cmd: &Command) -> bool {
    // Position-based, unlike `drop`: the same track can sit in the queue twice.
    at(&s.order, cmd.cursor)
        .and_then(|&queue_idx| usize::try_from(queue_idx).ok())
        .is_some_and(|i| {
            *next = drop_index(s, i);
            true
        })
}

/// Only claim the speakers when nobody already has them and a device asked.
fn claim_if_free(next: &mut State, cmd: &Command) {
    if next.active_id.is_empty() && !cmd.device_id.is_empty() {
        next.active_id.clone_from(&cmd.device_id);
    }
}

/// THE INVARIANT: `playing == true` requires `active_id != ""`. Enforced once for
/// every action so a new one gets it for free.
fn enforce_active_invariant(next: &mut State, cmd: &Command) {
    if next.playing && next.active_id.is_empty() {
        if cmd.device_id.is_empty() {
            next.playing = false;
        } else {
            next.active_id.clone_from(&cmd.device_id);
        }
    }
}

/// Remove queue entry `i` and repair `order`, whose values are indices INTO the
/// queue and so must be renumbered rather than merely filtered.
fn drop_index(s: &State, i: usize) -> State {
    let mut out = s.clone();
    out.queue.remove(i);
    let drop = i32::try_from(i).unwrap_or(i32::MAX);
    let playing = at(&s.order, s.cursor).copied().filter(|_| s.cursor >= 0);

    let mut order = Vec::with_capacity(s.order.len());
    let mut new_cursor: Option<i32> = None;
    for &idx in &s.order {
        if idx == drop {
            continue;
        }
        let renumbered = if idx > drop { idx - 1 } else { idx };
        if let Some(p) = playing {
            if p != drop {
                let want = if p > drop { p - 1 } else { p };
                if renumbered == want {
                    new_cursor = i32::try_from(order.len()).ok();
                }
            }
        }
        order.push(renumbered);
    }
    out.order = order;

    if out.order.is_empty() {
        out.cursor = -1;
        out.playing = false;
        out.position_ms = 0;
    } else if playing == Some(drop) {
        // The playing track was removed: stay at the same position (now the
        // following track) and keep playing.
        let len = i32::try_from(out.order.len()).unwrap_or(i32::MAX);
        out.cursor = if s.cursor >= len { len - 1 } else { s.cursor };
        out.position_ms = 0;
    } else if let Some(c) = new_cursor {
        out.cursor = c;
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn ident(v: Vec<i32>) -> Vec<i32> {
        v
    }
    fn rev(mut v: Vec<i32>) -> Vec<i32> {
        v.reverse();
        v
    }
    fn now() -> DateTime<Utc> {
        Utc::now()
    }
    fn cmd(action: &str) -> Command {
        Command {
            action: action.to_owned(),
            device_id: "dev1".to_owned(),
            ..Command::default()
        }
    }
    fn play3() -> State {
        let mut c = cmd("play");
        c.tracks = vec!["a".into(), "b".into(), "c".into()];
        apply(&State::default(), &c, now(), &ident).unwrap()
    }

    #[test]
    fn play_sets_queue_and_claims_a_free_device() {
        let s = play3();
        assert_eq!(s.queue, ["a", "b", "c"]);
        assert_eq!(s.order, [0, 1, 2]);
        assert_eq!(s.cursor, 0);
        assert!(s.playing);
        assert_eq!(s.active_id, "dev1");
        assert_eq!(s.track_id(), "a");
        assert_eq!(s.version, 1);
    }

    #[test]
    fn play_with_index_starts_there() {
        let mut c = cmd("play");
        c.tracks = vec!["a".into(), "b".into(), "c".into()];
        c.index = 2;
        let s = apply(&State::default(), &c, now(), &ident).unwrap();
        assert_eq!(s.track_id(), "c");
    }

    #[test]
    fn album_play_stays_in_order_while_shuffle_is_on() {
        // Shuffle on (the user tapped the shuffle button earlier). Album Play sends
        // shuffle:false and must play the album in its own order from the tapped track.
        let on = State {
            shuffle: true,
            ..State::default()
        };
        let mut c = cmd("play");
        c.tracks = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        c.index = 0;
        c.shuffle = Some(false);
        let s = apply(&on, &c, now(), &ident).unwrap();
        assert_eq!(s.order, [0, 1, 2, 3]);
        assert_eq!(s.cursor, 0);
        assert!(
            s.shuffle,
            "album play must not clear the persisted shuffle flag"
        );
    }

    #[test]
    fn play_without_override_still_follows_shuffle_flag() {
        let on = State {
            shuffle: true,
            ..State::default()
        };
        let mut c = cmd("play");
        c.tracks = vec!["a".into(), "b".into(), "c".into()];
        c.index = 1;
        let s = apply(&on, &c, now(), &ident).unwrap();
        assert_eq!(s.order[0], 1, "shuffled order starts at the tapped track");
        assert_eq!(s.track_id(), "b");
    }

    #[test]
    fn stale_version_is_rejected() {
        let s = play3();
        let mut c = cmd("pause");
        c.version = s.version + 9;
        assert_eq!(
            apply(&s, &c, now(), &ident),
            Err(PlaybackError::StaleVersion)
        );
    }

    #[test]
    fn unknown_action_errors() {
        assert_eq!(
            apply(&State::default(), &cmd("wat"), now(), &ident),
            Err(PlaybackError::UnknownAction)
        );
    }

    #[test]
    fn next_advances_then_stops_at_end_without_repeat() {
        let mut s = play3();
        s = apply(&s, &cmd("next"), now(), &ident).unwrap();
        assert_eq!(s.cursor, 1);
        s = apply(&s, &cmd("next"), now(), &ident).unwrap();
        assert_eq!(s.cursor, 2);
        s = apply(&s, &cmd("next"), now(), &ident).unwrap();
        assert_eq!(s.cursor, 2);
        assert!(!s.playing);
    }

    #[test]
    fn next_repeat_all_wraps() {
        let mut s = play3();
        s.repeat = REPEAT_ALL.to_owned();
        s.cursor = 2;
        let out = apply(&s, &cmd("next"), now(), &ident).unwrap();
        assert_eq!(out.cursor, 0);
        assert!(out.playing);
    }

    #[test]
    fn next_repeat_one_holds_only_when_not_manual() {
        let mut s = play3();
        s.repeat = REPEAT_ONE.to_owned();
        s.cursor = 1;
        let auto = apply(&s, &cmd("next"), now(), &ident).unwrap();
        assert_eq!(auto.cursor, 1);
        let mut manual = cmd("next");
        manual.manual = true;
        let tapped = apply(&s, &manual, now(), &ident).unwrap();
        assert_eq!(tapped.cursor, 2);
    }

    #[test]
    fn prev_restarts_past_three_seconds_else_steps_back() {
        let mut s = play3();
        s.cursor = 1;
        s.position_ms = 4000;
        let restart = apply(&s, &cmd("prev"), now(), &ident).unwrap();
        assert_eq!(restart.cursor, 1);
        assert_eq!(restart.position_ms, 0);
        s.position_ms = 1000;
        let back = apply(&s, &cmd("prev"), now(), &ident).unwrap();
        assert_eq!(back.cursor, 0);
    }

    #[test]
    fn seek_sets_position_and_bumps_seq() {
        let s = play3();
        let out = apply(
            &s,
            &{
                let mut c = cmd("seek");
                c.seconds = 12.5;
                c
            },
            now(),
            &ident,
        )
        .unwrap();
        assert_eq!(out.position_ms, 12500);
        assert_eq!(out.position_seq, s.position_seq + 1);
    }

    #[test]
    fn shuffle_keeps_current_first_then_restores_by_track() {
        let mut s = play3();
        s.cursor = 1; // playing "b"
        let sh = apply(&s, &cmd("shuffle"), now(), &rev).unwrap();
        assert!(sh.shuffle);
        assert_eq!(sh.order[0], 1); // current track first
        assert_eq!(sh.cursor, 0);
        let off = apply(&sh, &cmd("shuffle"), now(), &rev).unwrap();
        assert!(!off.shuffle);
        assert_eq!(off.order, [0, 1, 2]);
        assert_eq!(off.cursor, 1); // cursor follows the track "b"
    }

    #[test]
    fn repeat_cycles_and_accepts_explicit() {
        let mut s = play3();
        s.repeat = REPEAT_OFF.to_owned();
        let all = apply(&s, &cmd("repeat"), now(), &ident).unwrap();
        assert_eq!(all.repeat, REPEAT_ALL);
        let one = apply(&all, &cmd("repeat"), now(), &ident).unwrap();
        assert_eq!(one.repeat, REPEAT_ONE);
        let off = apply(&one, &cmd("repeat"), now(), &ident).unwrap();
        assert_eq!(off.repeat, REPEAT_OFF);
        // A fresh state's empty repeat cycles to off first, matching Go's default.
        let fresh = apply(&play3(), &cmd("repeat"), now(), &ident).unwrap();
        assert_eq!(fresh.repeat, REPEAT_OFF);
        let mut explicit = cmd("repeat");
        explicit.repeat = REPEAT_ONE.to_owned();
        assert_eq!(
            apply(&s, &explicit, now(), &ident).unwrap().repeat,
            REPEAT_ONE
        );
    }

    #[test]
    fn stop_clears_everything() {
        let out = apply(&play3(), &cmd("stop"), now(), &ident).unwrap();
        assert!(out.queue.is_empty());
        assert_eq!(out.cursor, -1);
        assert!(!out.playing);
        assert_eq!(out.active_id, "");
    }

    #[test]
    fn drop_renumbers_order_and_keeps_the_right_track() {
        let mut s = play3();
        s.cursor = 2; // playing "c"
        let mut c = cmd("drop");
        c.track_id = "a".to_owned();
        let out = apply(&s, &c, now(), &ident).unwrap();
        assert_eq!(out.queue, ["b", "c"]);
        assert_eq!(out.track_id(), "c");
    }

    #[test]
    fn drop_playing_track_advances_to_the_next() {
        let mut s = play3();
        s.cursor = 1; // playing "b"
        let mut c = cmd("drop");
        c.track_id = "b".to_owned();
        let out = apply(&s, &c, now(), &ident).unwrap();
        assert_eq!(out.queue, ["a", "c"]);
        assert_eq!(out.track_id(), "c");
        assert!(out.playing);
    }

    #[test]
    fn no_action_can_play_without_a_device() {
        // A command with no device on a state with no active device must not end
        // up "playing on nobody".
        let mut s = play3();
        s.active_id.clear();
        for action in ["next", "prev", "jump", "toggle"] {
            let mut c = cmd(action);
            c.device_id.clear();
            if action == "jump" {
                c.cursor = 1;
            }
            let out = apply(&s, &c, now(), &ident).unwrap();
            assert!(
                !(out.playing && out.active_id.is_empty()),
                "{action} left playing with no device"
            );
        }
    }

    #[test]
    fn enqueue_appends_without_playing() {
        let s = play3();
        let mut c = cmd("enqueue");
        c.tracks = vec!["d".into()];
        let out = apply(&s, &c, now(), &ident).unwrap();
        assert_eq!(out.queue, ["a", "b", "c", "d"]);
        assert_eq!(out.order, [0, 1, 2, 3]);
    }

    #[test]
    fn empty_play_is_a_noop_without_version_bump() {
        let s = play3();
        let out = apply(&s, &cmd("play"), now(), &ident).unwrap();
        assert_eq!(out.version, s.version);
    }
}
