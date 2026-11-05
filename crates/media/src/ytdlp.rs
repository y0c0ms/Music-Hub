//! The one home for how this codebase talks to yt-dlp.
//!
//! It exists because the same lesson was learned twice. The music downloader
//! discovered on 2026-08-18 that YouTube's default player client 403s media and
//! resolves some videos as "not available", and pinned `player_client=android`.
//! The audiobook downloader, written later, did not carry the pin and on
//! 2026-09-07 failed to download videos the music path fetched fine — the exact
//! same failure, rediscovered from a user report. Anything true of "how we must
//! call yt-dlp" belongs here, so the next downloader is fixed in one place.

use tokio::sync::{Semaphore, SemaphorePermit};

/// The YouTube player client yt-dlp must impersonate for anything that resolves
/// or downloads MEDIA.
///
/// Re-pinned 2026-11-05 with yt-dlp 2026.10.26, by a real download of one track
/// per client inside the pod: `default` wrote the file; `android` (the pin
/// since 2026-08-18) and `web`, `web_safari`, `ios`, `mweb` answered "Requested
/// format is not available"; `tv` "The page needs to be reloaded"; `android_vr`
/// 403. With yt-dlp 2026.07.04 every client failed, so the version pin in
/// `docker/requirements.txt` moves with this one. It is NOT for search — a `--flat-playlist
/// --dump-json` search never touches a media URL and works unpinned.
/// Deliberately ONE client, never a fallback list: yt-dlp merges the formats
/// every listed client offers and may select one whose media URL 403s. When
/// this breaks, re-run the per-client probe and pin whichever single client
/// still serves media.
pub const PLAYER_CLIENT: &str = "youtube:player_client=default";

/// Prepend the pinned player client to a yt-dlp argument list.
///
/// Use it for every call that resolves metadata or downloads audio — and only
/// those. yt-dlp treats `--extractor-args` as a global option, so leading with
/// it does not disturb the rest of the arguments.
#[must_use]
pub fn media<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(args.len() + 2);
    out.push("--extractor-args");
    out.push(PLAYER_CLIENT);
    out.extend_from_slice(args);
    out
}

/// Bounds concurrent media downloads across every source at once.
///
/// They share a single memory budget — the pod's cgroup — and each yt-dlp+ffmpeg
/// download peaks at a couple hundred MB. Unbounded, enqueuing an album ran a
/// dozen in parallel and the container was OOM-killed (768Mi, exit 137, reported
/// 2026-09-07). Two at a time keeps peak memory predictable while still
/// overlapping one job's download with another's transcode. Global, not
/// per-source: a music album and an audiobook must not each assume they have the
/// box to themselves.
static DOWNLOAD_SLOTS: Semaphore = Semaphore::const_new(2);

/// Acquire a download slot, blocking until one is free.
///
/// Drop the returned permit to release it — holding a slot forever starves every
/// other download. A job awaiting here is legitimately still "queued".
///
/// # Panics
/// Never in practice: the backing semaphore is a `static` that is never closed.
pub async fn slot() -> SemaphorePermit<'static> {
    // The only error is a closed semaphore, and this one is never closed.
    #[allow(clippy::expect_used)] // DOWNLOAD_SLOTS is a static that is never closed
    DOWNLOAD_SLOTS
        .acquire()
        .await
        .expect("download semaphore is never closed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_leads_with_the_pinned_client() {
        let args = media(&["--dump-json", "--", "url"]);
        assert_eq!(args[0], "--extractor-args");
        assert_eq!(args[1], PLAYER_CLIENT);
        assert_eq!(&args[2..], &["--dump-json", "--", "url"]);
    }

    #[tokio::test]
    async fn two_slots_are_available_at_once() {
        let a = slot().await;
        let b = slot().await;
        // A third acquire must not resolve while two are held.
        assert!(DOWNLOAD_SLOTS.try_acquire().is_err());
        drop(a);
        assert!(DOWNLOAD_SLOTS.try_acquire().is_ok());
        drop(b);
    }
}
