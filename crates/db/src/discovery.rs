//! Discovery swipe-deck reads and writes.
//!
//! Verdicts are the swiped set: every card the user has judged, so it is never
//! dealt again. Owned cards come straight from the indexed library filtered to
//! the discovery prefix — there is no Navidrome to ask, the path is the truth.

use std::collections::{HashMap, HashSet};

use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

/// One library track eligible for the "owned" side of the deck.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OwnedTrack {
    pub id: Uuid,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_sec: i32,
    pub has_cover: bool,
}

/// Every source id this user has already swiped, so a judged card never returns.
///
/// Presence is the signal, not the value: a disliked card is in the set exactly
/// like a liked one, because both have been decided.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn judged(pool: &PgPool, user_id: Uuid) -> Result<HashSet<String>, DbError> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT source_id FROM discovery_verdicts WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Record a swipe verdict, overwriting any earlier one for the same card.
///
/// # Errors
/// Returns [`DbError`] if the upsert fails.
pub async fn judge(
    pool: &PgPool,
    user_id: Uuid,
    source_id: &str,
    liked: bool,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO discovery_verdicts (user_id, source_id, liked)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_id, source_id) DO UPDATE SET liked = EXCLUDED.liked",
    )
    .bind(user_id)
    .bind(source_id)
    .bind(liked)
    .execute(pool)
    .await?;
    Ok(())
}

/// Library tracks whose absolute path is under the discovery prefix, capped.
///
/// `abs_prefix` is `music_root/<discovery_prefix>` and is matched as a literal
/// path prefix; it is operator config (no `%`/`_`), never user input, so a plain
/// `LIKE` needs no escaping.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn owned_under_prefix(
    pool: &PgPool,
    abs_prefix: &str,
    cap: i64,
) -> Result<Vec<OwnedTrack>, DbError> {
    let pattern = format!("{abs_prefix}%");
    let rows = sqlx::query_as::<_, OwnedTrack>(
        "SELECT id, title, artist, album, duration_sec, has_cover
         FROM tracks WHERE abs_path LIKE $1 AND NOT is_book
         LIMIT $2",
    )
    .bind(pattern)
    .bind(cap)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The absolute path of a track by id, whatever library it lives in.
///
/// The delete guard re-derives and re-checks the path itself; this is only the
/// lookup that replaces Navidrome's native `RealPath`.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn track_abs_path(pool: &PgPool, id: Uuid) -> Result<Option<String>, DbError> {
    let path = sqlx::query_scalar::<_, String>("SELECT abs_path FROM tracks WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(path)
}

/// The artists behind this user's most-played tracks, most-played first, as raw
/// credit strings (the caller reduces each to its lead artist and de-duplicates).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn seed_artists(pool: &PgPool, user_id: Uuid, cap: i64) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT t.artist FROM annotations a
         JOIN tracks t ON t.id = a.item_id
         WHERE a.user_id = $1 AND a.item_type = 'track' AND a.play_count > 0
           AND NOT t.is_book AND t.artist <> ''
         ORDER BY a.play_count DESC
         LIMIT $2",
    )
    .bind(user_id)
    .bind(cap)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The per-user ListenBrainz override, if the user set one (empty maps to `None`).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn listenbrainz_user(pool: &PgPool, user_id: Uuid) -> Result<Option<String>, DbError> {
    let u = sqlx::query_scalar::<_, String>(
        "SELECT listenbrainz_user FROM settings WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(u.filter(|s| !s.trim().is_empty()))
}

/// Every distinct library artist, and every (title, artist) pair, over non-book
/// tracks — loaded once per deck build for the candidate "already owned?" filter.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn library_names(pool: &PgPool) -> Result<(Vec<String>, Vec<(String, String)>), DbError> {
    let artists = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT artist FROM tracks WHERE NOT is_book AND artist <> ''",
    )
    .fetch_all(pool)
    .await?;
    let pairs = sqlx::query_as::<_, (String, String)>(
        "SELECT title, artist FROM tracks WHERE NOT is_book AND title <> ''",
    )
    .fetch_all(pool)
    .await?;
    Ok((artists, pairs))
}

/// Distinct (album-artist, album) pairs in the library, for the new-releases
/// "already owned?" filter.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn library_albums(pool: &PgPool) -> Result<Vec<(String, String)>, DbError> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT coalesce(nullif(album_artist, ''), artist) AS artist, album \
         FROM tracks WHERE NOT is_book AND album_id IS NOT NULL AND album <> ''",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
/// The (album-artist, track title) pairs **this user** owns, for the
/// new-releases "already have it?" check.
///
/// Scoped by `libraries.owner` because ownership is per user, not global: a
/// release one listener owns must still be offered to the other. Rows with a
/// blank album are included deliberately - a single downloaded on its own
/// carries no album, and that is exactly the case a whole-album check missed,
/// so the release has to be told apart by its tracks rather than its album name.
/// The shared library (blank owner) counts for everyone.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn owned_tracks(pool: &PgPool, user_id: Uuid) -> Result<Vec<(String, String)>, DbError> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT coalesce(nullif(t.album_artist, ''), t.artist) AS artist, t.title \
         FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         JOIN users u ON u.id = $1 \
         WHERE NOT t.is_book AND t.title <> '' \
           AND (coalesce(l.owner, '') = '' OR lower(l.owner) = lower(u.username))",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Albums this user owns at least one track from, as `(artist, album)`.
///
/// Track-title matching alone is not enough to recognise a record somebody
/// already has. YouTube writes some titles as `Artist - Song` and appends
/// `(Official Music Video)`, so a track the user owns can carry a title the
/// release's own tracklist never uses - which is how a fully downloaded album
/// ends up filed as unowned. The album name is the stable half of the pair: it
/// is what the user's library calls the record, and what the release is called.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn owned_albums(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<(String, String, usize)>, DbError> {
    let rows = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT coalesce(nullif(t.album_artist, ''), t.artist) AS artist, t.album, \
         count(DISTINCT lower(t.title)) AS n \
         FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         JOIN users u ON u.id = $1 \
         WHERE NOT t.is_book AND t.album <> '' \
           AND (coalesce(l.owner, '') = '' OR lower(l.owner) = lower(u.username)) \
         GROUP BY 1, 2",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(a, al, n)| (a, al, usize::try_from(n).unwrap_or(0)))
        .collect())
}
/// Track titles this user owns by one artist, used to confirm that a YouTube
/// channel really belongs to them.
///
/// A handful per artist is enough: the point is to offer the channel resolver a
/// title it can recognise, not to hand over the whole library. Ordered by how
/// often the track is played, so the best-known song is the one offered first.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn artist_known_songs(
    pool: &PgPool,
    user_id: Uuid,
    artist: &str,
    n: i64,
) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT t.title FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         JOIN users u ON u.id = $1 \
         WHERE NOT t.is_book AND t.title <> '' \
           AND lower(coalesce(nullif(t.album_artist, ''), t.artist)) = lower($2) \
           AND (coalesce(l.owner, '') = '' OR lower(l.owner) = lower(u.username)) \
         GROUP BY t.title \
         ORDER BY count(*) DESC, t.title \
         LIMIT $3",
    )
    .bind(user_id)
    .bind(artist)
    .bind(n)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// One nightly-precomputed "new release" for a user: the display fields plus a
/// resolved preview `source_id` (empty when the resolve found nothing).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CachedRelease {
    pub mbid: String,
    pub artist: String,
    pub title: String,
    pub kind: String,
    pub cover_url: String,
    pub release_date: String,
    pub source_id: String,
    pub position: i32,
    /// Which source produced this row; empty means MusicBrainz/ListenBrainz.
    pub source: String,
    /// The date is the source playlist's last-modified stamp rather than a
    /// release date, so the row is shown as undated instead of sorted into a week.
    pub date_uncertain: bool,
}
/// A YouTube channel accepted as belonging to an artist, and the evidence for it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ArtistChannel {
    pub channel_id: String,
    pub channel_name: String,
    /// `song` when a known track the user owns was found on the channel, `name`
    /// when only the channel name matched. A `name` row is the weaker proof.
    pub confirmed_by: String,
}

/// The cached channel for an artist, if one has been resolved before.
///
/// Channel resolution costs a search plus a listing of candidate uploads, so it
/// is cached permanently. Re-resolving only makes sense if the artist moved
/// channel, which `DELETE`-ing the row expresses.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn artist_channel(
    pool: &PgPool,
    artist_key: &str,
) -> Result<Option<ArtistChannel>, DbError> {
    let row = sqlx::query_as::<_, ArtistChannel>(
        "SELECT channel_id, channel_name, confirmed_by FROM artist_channels WHERE artist_key = $1",
    )
    .bind(artist_key)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Record the channel resolved for an artist, replacing any previous answer.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn set_artist_channel(
    pool: &PgPool,
    artist_key: &str,
    channel_id: &str,
    channel_name: &str,
    confirmed_by: &str,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO artist_channels (artist_key, channel_id, channel_name, confirmed_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (artist_key) DO UPDATE SET channel_id = EXCLUDED.channel_id, \
           channel_name = EXCLUDED.channel_name, confirmed_by = EXCLUDED.confirmed_by, \
           resolved_at = now()",
    )
    .bind(artist_key)
    .bind(channel_id)
    .bind(channel_name)
    .bind(confirmed_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record that a release playlist has been examined, and with what outcome.
///
/// `is_single_known` says whether `is_single` was actually derived: a row
/// written before v116 carries the column default, which `cache_hit_is_complete`
/// reads as "the kind is not known", sending it back through the resolve.
/// `track_count` is the number of tracks the resolve found, 0 for a rejection;
/// it is what lets a warm sweep judge album ownership without the tracklist.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
#[allow(clippy::too_many_arguments)] // a row's recorded facts, read in one statement
pub async fn mark_playlist_checked(
    pool: &PgPool,
    playlist_id: &str,
    artist_key: &str,
    release_date: &str,
    dated: bool,
    cover: &str,
    is_single: bool,
    is_single_known: bool,
    track_count: usize,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO checked_release_playlists \
         (playlist_id, artist_key, release_date, dated, cover, is_single, is_single_known, \
          track_count) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (playlist_id) DO UPDATE SET artist_key = EXCLUDED.artist_key, \
           release_date = EXCLUDED.release_date, dated = EXCLUDED.dated, \
           cover = EXCLUDED.cover, is_single = EXCLUDED.is_single, \
           is_single_known = EXCLUDED.is_single_known, \
           track_count = EXCLUDED.track_count, last_seen = now()",
    )
    .bind(playlist_id)
    .bind(artist_key)
    .bind(release_date)
    .bind(dated)
    .bind(cover)
    .bind(is_single)
    .bind(is_single_known)
    .bind(i32::try_from(track_count).unwrap_or(i32::MAX))
    .execute(pool)
    .await?;
    Ok(())
}

/// What a previously-examined release playlist yielded, so a warm sweep can
/// rebuild the row - or skip the playlist - without paying for the resolve again.
///
/// The date, cover, kind and track count all come out of the same resolve and
/// are stable for a given playlist: the date is derived from the playlist's own
/// track uploads, the cover is its artwork, and the count is how many tracks it
/// holds. Storing only the date (as v114 did) meant a warm sweep rebuilt every
/// row with an empty cover, a hardcoded `is_single: false` and no tracks, so it
/// overwrote the facts it had thrown away instead of carrying them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one row's independent columns, not a state machine
pub struct CheckedPlaylist {
    /// Empty for a rejection; the playlist's modified stamp when `!dated`.
    pub release_date: String,
    /// The date came from the tracks themselves, so it does not drift.
    pub dated: bool,
    pub cover: String,
    pub is_single: bool,
    /// Whether `is_single` was derived from a resolve rather than left at the
    /// column default. See migration 0010.
    pub is_single_known: bool,
    /// How many tracks the resolve found; 0 when not recorded. See migration 0011.
    pub track_count: usize,
    /// Examined within [`REJECTION_RECHECK_DAYS`], so a rejection still stands.
    pub recently_checked: bool,
}

/// How long a rejected playlist is trusted before the sweep looks at it again.
///
/// A rejection is a playlist untouched since before the new-release window, or
/// one with no date at all; neither turns into a new release by waiting, so
/// re-checking every night only repeats the answer - 783 of 933 rejections were
/// re-checked by one night's sweep before this. A week still lets a playlist
/// that was edited back into the window be noticed.
pub const REJECTION_RECHECK_DAYS: i32 = 7;

/// Every release playlist already examined: dated, undated and rejected alike.
///
/// Only loading the dated ones (as before v120) made the rejections write-only:
/// the sweep recorded them "so every sweep does not pay for it again" and then
/// never read them back, so every sweep paid for them again.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn checked_playlists(pool: &PgPool) -> Result<HashMap<String, CheckedPlaylist>, DbError> {
    let rows = sqlx::query_as::<_, (String, String, bool, String, bool, bool, i32, bool)>(
        "SELECT playlist_id, release_date, dated, cover, is_single, is_single_known, \
                track_count, last_seen > now() - make_interval(days => $1) \
         FROM checked_release_playlists",
    )
    .bind(REJECTION_RECHECK_DAYS)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, release_date, dated, cover, is_single, is_single_known, n, recently_checked)| {
                (
                    id,
                    CheckedPlaylist {
                        release_date,
                        dated,
                        cover,
                        is_single,
                        is_single_known,
                        track_count: usize::try_from(n).unwrap_or(0),
                        recently_checked,
                    },
                )
            },
        )
        .collect())
}

/// Every user and their ListenBrainz name, empty when unset.
///
/// For the nightly new-releases precompute; users with no ListenBrainz are
/// included too - their list is built from play history via MusicBrainz.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn all_users_with_lb(pool: &PgPool) -> Result<Vec<(Uuid, String)>, DbError> {
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT u.id, coalesce(s.listenbrainz_user, '') \
         FROM users u LEFT JOIN settings s ON s.user_id = u.id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The nightly-precomputed new releases for a user, newest first (by position).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn cached_new_releases(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<CachedRelease>, DbError> {
    let rows = sqlx::query_as::<_, CachedRelease>(
        "SELECT mbid, artist, title, kind, cover_url, release_date, source_id, position, \
                source, date_uncertain \
         FROM new_release_cache WHERE user_id = $1 ORDER BY position",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Rebuild a user's cached new releases: delete the old set and insert the new
/// one in a single transaction, so a reader never sees a half-updated list.
///
/// # Errors
/// Returns [`DbError`] if the transaction fails.
pub async fn replace_new_releases(
    pool: &PgPool,
    user_id: Uuid,
    rows: &[CachedRelease],
) -> Result<(), DbError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM new_release_cache WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO new_release_cache \
             (user_id, mbid, artist, title, kind, cover_url, release_date, source_id, position, \
              source, date_uncertain) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) \
             ON CONFLICT (user_id, artist, title) DO NOTHING",
        )
        .bind(user_id)
        .bind(&r.mbid)
        .bind(&r.artist)
        .bind(&r.title)
        .bind(&r.kind)
        .bind(&r.cover_url)
        .bind(&r.release_date)
        .bind(&r.source_id)
        .bind(r.position)
        .bind(&r.source)
        .bind(r.date_uncertain)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::users;
    use sqlx::postgres::PgPoolOptions;

    // Integration tests: skipped unless DATABASE_URL points at a live Postgres.
    async fn test_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        PgPoolOptions::new().connect(&url).await.ok()
    }

    #[tokio::test]
    async fn judge_then_judged_contains_the_source() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let kc = format!("kc-{}", Uuid::new_v4());
        let username = format!("user-{}", Uuid::new_v4());
        let user = users::upsert(&pool, &kc, &username, "Disco").await.unwrap();

        let src = format!("yt{}", &Uuid::new_v4().simple().to_string()[..8]);
        assert!(!judged(&pool, user).await.unwrap().contains(&src));

        judge(&pool, user, &src, true).await.unwrap();
        assert!(judged(&pool, user).await.unwrap().contains(&src));

        // A later opposite verdict is still "judged" — presence, not value.
        judge(&pool, user, &src, false).await.unwrap();
        assert!(judged(&pool, user).await.unwrap().contains(&src));

        sqlx::query("DELETE FROM discovery_verdicts WHERE user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn replace_then_cached_returns_rows_in_order() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let kc = format!("kc-{}", Uuid::new_v4());
        let username = format!("user-{}", Uuid::new_v4());
        let user = users::upsert(&pool, &kc, &username, "Disco").await.unwrap();

        let row = |title: &str, pos: i32, src: &str| CachedRelease {
            mbid: format!("m-{title}"),
            artist: "A".to_owned(),
            title: title.to_owned(),
            kind: "Album".to_owned(),
            cover_url: String::new(),
            release_date: "2026-09-01".to_owned(),
            source_id: src.to_owned(),
            position: pos,
            source: String::new(),
            date_uncertain: false,
        };
        // Inserted out of order; read must come back by position.
        let rows = vec![row("T2", 1, "s2"), row("T1", 0, "")];
        replace_new_releases(&pool, user, &rows).await.unwrap();

        let got = cached_new_releases(&pool, user).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].title, "T1");
        assert_eq!(got[1].source_id, "s2");

        // A second run with fewer rows drops the stale ones.
        replace_new_releases(&pool, user, &rows[..1]).await.unwrap();
        assert_eq!(cached_new_releases(&pool, user).await.unwrap().len(), 1);

        sqlx::query("DELETE FROM new_release_cache WHERE user_id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
    }
}
