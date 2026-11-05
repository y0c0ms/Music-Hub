//! Read queries for the library browse/search endpoints.
//!
//! Albums and artists are not stored; they are derived with `GROUP BY` over the
//! indexed `tracks` table, so a list and a detail view agree on ids and counts
//! without maintaining denormalised rows. Every music query joins `libraries`
//! for the per-user visibility rule (a user sees shared libraries plus their
//! own) and left-joins `annotations` for the per-user `starred` flag and play
//! counts. Results are returned as `mh_core::model` wire types directly, with
//! `coverUrl`/`streamUrl` filled in.
//!
//! Runtime `sqlx::query_as` (never the compile-time macros) so a build needs no
//! live database; SQL correctness is covered by the `DATABASE_URL`-gated tests
//! in the server crate. User input is always bound, never interpolated: the sort
//! kind only selects a static SQL suffix.

use std::collections::HashSet;

use mh_core::model::{Album, Artist, Playlist, SearchResults, Track};
use sqlx::PgPool;
use uuid::Uuid;

use crate::DbError;

const TRACK_COLS: &str = "t.id, t.title, t.artist, t.album, t.album_id, \
    t.duration_sec, t.year, coalesce(ann.starred, false) AS starred";

const ALBUM_COLS: &str = "t.album_id AS id, max(t.album) AS name, \
    coalesce(nullif(max(t.album_artist), ''), max(t.artist)) AS artist, \
    max(t.artist_id::text) AS artist_id, count(*) AS song_count, \
    coalesce(nullif(max(t.year), 0), \
        nullif((SELECT am.release_year FROM album_meta am WHERE am.album_id = t.album_id), 0), \
        0) AS year";

const ARTIST_COLS: &str = "a.id AS id, a.name AS name, count(DISTINCT t.album_id) AS album_count";

// ------------------------------------------------------------------ row types

#[derive(Debug, Clone, sqlx::FromRow)]
struct TrackRow {
    id: Uuid,
    title: String,
    artist: String,
    album: String,
    album_id: Option<Uuid>,
    duration_sec: i32,
    year: i32,
    starred: bool,
}

impl TrackRow {
    fn into_model(self) -> Track {
        let id = self.id.to_string();
        let cover_url = format!("/api/cover/{id}");
        let stream_url = format!("/api/stream/{id}");
        Track {
            id,
            title: self.title,
            artist: self.artist,
            album: self.album,
            album_id: self.album_id.map(|a| a.to_string()).unwrap_or_default(),
            duration: self.duration_sec,
            year: self.year,
            starred: self.starred,
            cover_url,
            stream_url,
            book_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AlbumRow {
    id: Uuid,
    name: String,
    artist: String,
    artist_id: Option<String>,
    song_count: i64,
    year: i32,
}

impl AlbumRow {
    fn into_model(self) -> Album {
        let id = self.id.to_string();
        let cover_url = format!("/api/cover/{id}");
        Album {
            id,
            name: self.name,
            artist: self.artist,
            artist_id: self.artist_id.unwrap_or_default(),
            song_count: i32::try_from(self.song_count).unwrap_or(i32::MAX),
            year: self.year,
            cover_url,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ArtistRow {
    id: Uuid,
    name: String,
    album_count: i64,
}

impl ArtistRow {
    fn into_model(self) -> Artist {
        let id = self.id.to_string();
        let cover_url = format!("/api/cover/{id}");
        Artist {
            id,
            name: self.name,
            album_count: i32::try_from(self.album_count).unwrap_or(i32::MAX),
            cover_url,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct PlaylistRow {
    id: Uuid,
    name: String,
    owner: String,
    song_count: i64,
}

impl PlaylistRow {
    fn into_model(self) -> Playlist {
        let id = self.id.to_string();
        let cover_url = format!("/api/cover/{id}");
        Playlist {
            id,
            name: self.name,
            owner: self.owner,
            song_count: i32::try_from(self.song_count).unwrap_or(i32::MAX),
            cover_url,
        }
    }
}

/// One genre and how many tracks carry it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GenreCount {
    pub name: String,
    pub tracks: i64,
}

/// Bytes and track count for a storage slice.
#[derive(Debug, Clone, Copy, sqlx::FromRow)]
pub struct Usage {
    pub bytes: i64,
    pub tracks: i64,
}

/// The three song lists that fill the home screen.
#[derive(Debug, Clone)]
pub struct Home {
    pub recent: Vec<Track>,
    pub frequent: Vec<Track>,
    pub random: Vec<Track>,
}

/// The starred tracks, albums, and artists for one user.
#[derive(Debug, Clone)]
pub struct Starred {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
}

// ------------------------------------------------------------------ sorts

/// How the songs list is ordered. `Genre` additionally requires a genre name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongSort {
    Newest,
    Frequent,
    Recent,
    Random,
    Starred,
    Genre,
}

impl SongSort {
    /// Parse the `sort` query value, or `None` for an unknown kind.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "newest" => Some(Self::Newest),
            "frequent" => Some(Self::Frequent),
            "recent" => Some(Self::Recent),
            "random" => Some(Self::Random),
            "starred" => Some(Self::Starred),
            "genre" => Some(Self::Genre),
            _ => None,
        }
    }
}

/// The `WHERE`/`ORDER BY` suffix for a non-genre song sort, ending in `LIMIT $3`.
const fn song_tail(sort: SongSort) -> &'static str {
    match sort {
        // "Recently added" is the earlier of created_at and file mtime. Neither
        // alone is right: the PG cutover stamped 593 rows' created_at with one
        // evening (their mtime holds the real download date), and any later
        // write to a file - an album re-tag on download reuse, the monthly explo
        // refresh - bumps mtime, which floated 22 owned songs to the top on
        // 2026-11-05. The earlier of the two survives both. LEAST skips NULLs.
        SongSort::Newest => {
            " AND NOT t.is_book ORDER BY LEAST(t.created_at, t.mtime) DESC NULLS LAST LIMIT $3"
        }
        SongSort::Frequent => {
            " AND NOT t.is_book AND coalesce(ann.play_count, 0) > 0 \
             ORDER BY ann.play_count DESC LIMIT $3"
        }
        SongSort::Recent => {
            " AND NOT t.is_book AND ann.last_played_at IS NOT NULL \
             ORDER BY ann.last_played_at DESC LIMIT $3"
        }
        SongSort::Random => " AND NOT t.is_book ORDER BY random() LIMIT $3",
        SongSort::Starred => {
            " AND NOT t.is_book AND coalesce(ann.starred, false) \
             ORDER BY ann.starred_at DESC LIMIT $3"
        }
        // Genre is dispatched to its own query before this is reached.
        SongSort::Genre => "",
    }
}

/// How the albums list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumSort {
    AlphabeticalByName,
    Newest,
    Frequent,
    Recent,
    Random,
    Starred,
}

impl AlbumSort {
    /// Parse the `type` query value, or `None` for an unknown kind.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "alphabeticalByName" => Some(Self::AlphabeticalByName),
            "newest" => Some(Self::Newest),
            "frequent" => Some(Self::Frequent),
            "recent" => Some(Self::Recent),
            "random" => Some(Self::Random),
            "starred" => Some(Self::Starred),
            _ => None,
        }
    }
}

/// The `HAVING` clause (empty unless `Starred`) and `ORDER BY` expression for an
/// album sort. Appended after `GROUP BY t.album_id`, before `LIMIT $3 OFFSET $4`.
const fn album_sort_sql(kind: AlbumSort) -> (&'static str, &'static str) {
    match kind {
        AlbumSort::AlphabeticalByName => ("", "lower(max(t.album))"),
        // Same rule as the songs list: a re-tag must not make an album "new".
        AlbumSort::Newest => ("", "max(LEAST(t.created_at, t.mtime)) DESC NULLS LAST"),
        AlbumSort::Frequent => ("", "sum(coalesce(ann.play_count, 0)) DESC"),
        AlbumSort::Recent => ("", "max(ann.last_played_at) DESC NULLS LAST"),
        AlbumSort::Random => ("", "random()"),
        AlbumSort::Starred => (
            " HAVING bool_or(coalesce(ann.starred, false))",
            "lower(max(t.album))",
        ),
    }
}

// ------------------------------------------------------------------ songs

/// The home screen: newest, most-played, and random songs (all excluding books).
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn home(pool: &PgPool, user_id: Uuid, username: &str) -> Result<Home, DbError> {
    let recent = songs(pool, user_id, username, SongSort::Newest, None, 20).await?;
    let frequent = songs(pool, user_id, username, SongSort::Frequent, None, 20).await?;
    let random = songs(pool, user_id, username, SongSort::Random, None, 30).await?;
    Ok(Home {
        recent,
        frequent,
        random,
    })
}

/// The songs list for a sort kind, at most `limit` tracks, always excluding
/// books. `Genre` reads `genre`; the other sorts ignore it.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn songs(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    sort: SongSort,
    genre: Option<&str>,
    limit: i64,
) -> Result<Vec<Track>, DbError> {
    if sort == SongSort::Genre {
        return songs_by_genre(pool, user_id, username, genre.unwrap_or_default(), limit).await;
    }
    let sql = format!(
        "SELECT {TRACK_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         WHERE (l.kind = 'shared' OR l.owner = $2){}",
        song_tail(sort)
    );
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(user_id)
        .bind(username)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TrackRow::into_model).collect())
}

async fn songs_by_genre(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    genre: &str,
    limit: i64,
) -> Result<Vec<Track>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         JOIN track_genres tg ON tg.track_id = t.id \
         JOIN genres g ON g.id = tg.genre_id \
         WHERE (l.kind = 'shared' OR l.owner = $2) AND NOT t.is_book AND g.name = $3 \
         ORDER BY random() LIMIT $4"
    );
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(user_id)
        .bind(username)
        .bind(genre)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TrackRow::into_model).collect())
}

/// One track by id, if it exists and is visible to this user (books included).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn song(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    id: Uuid,
) -> Result<Option<Track>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         WHERE (l.kind = 'shared' OR l.owner = $2) AND t.id = $3"
    );
    let row = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(user_id)
        .bind(username)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(TrackRow::into_model))
}

/// Already-indexed copies of a song title the user can see, as
/// `(abs_path, artist)`, for download dedup.
///
/// Matches the title case-insensitively and leaves the artist decision to the
/// caller: the same act is credited "Plutonio" on one upload and "Plutonio 2765"
/// on another, and an exact artist match missed the owned copy and downloaded a
/// duplicate (Multiplica, 2026-11-05). Prefers the user's own copies, then the
/// earliest added.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn track_copies_by_title(
    pool: &PgPool,
    title: &str,
    username: &str,
) -> Result<Vec<(String, String)>, DbError> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT t.abs_path, t.artist FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         WHERE NOT t.is_book AND lower(t.title) = lower($1) \
           AND (l.kind = 'shared' OR l.owner = $2) \
         ORDER BY (l.owner = $2) DESC, LEAST(t.created_at, t.mtime) ASC NULLS LAST \
         LIMIT 20",
    )
    .bind(title)
    .bind(username)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A [`similar`] candidate: a track plus whether it shares a genre with the seed.
#[derive(sqlx::FromRow)]
struct SimilarRow {
    #[sqlx(flatten)]
    track: TrackRow,
    shares_genre: bool,
}

/// Tracks similar to a seed, all on disk.
///
/// Feature-vector nearest-neighbours over the Essentia vectors, but genre-aware:
/// among the nearest-sounding tracks, the ones sharing a genre with the seed come
/// first (audio distance still orders within each tier), so a rap seed does not
/// queue an audio-alike reggae track ahead of the library's actual rap. Falls
/// back to a random genre floor when the seed is not featurised.
///
/// Never returns books or the seed itself.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn similar(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    seed: Uuid,
    limit: usize,
) -> Result<Vec<Track>, DbError> {
    let cap = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut out: Vec<Track> = Vec::new();
    let mut picked: HashSet<Uuid> = HashSet::from([seed]);

    // 1. Vector nearest-neighbours, only when the seed has a feature vector.
    let has_vec = sqlx::query_scalar::<_, bool>(
        "SELECT vec IS NOT NULL FROM track_features WHERE track_id = $1",
    )
    .bind(seed)
    .fetch_optional(pool)
    .await?
    .unwrap_or(false);
    if has_vec {
        // A pure vector-NN pulls in tracks that merely *sound* close - a reggae
        // track beside a rap seed - because the audio features capture tempo and
        // timbre, not genre. So take a wider pool of the nearest-sounding tracks
        // and bring genre-sharing ones to the front; audio distance still orders
        // within each tier, and the rest backfill.
        let pool_size = cap.saturating_mul(3);
        let sql = format!(
            "SELECT {TRACK_COLS}, \
                EXISTS ( \
                  SELECT 1 FROM track_genres tg WHERE tg.track_id = t.id \
                    AND tg.genre_id IN (SELECT genre_id FROM track_genres WHERE track_id = $3) \
                ) AS shares_genre \
             FROM tracks t \
             JOIN libraries l ON l.id = t.library_id \
             LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
             JOIN track_features tf ON tf.track_id = t.id \
             WHERE (l.kind = 'shared' OR l.owner = $2) AND NOT t.is_book \
               AND t.id <> $3 AND tf.vec IS NOT NULL \
             ORDER BY tf.vec <-> (SELECT vec FROM track_features WHERE track_id = $3) \
             LIMIT $4"
        );
        let rows = sqlx::query_as::<_, SimilarRow>(&sql)
            .bind(user_id)
            .bind(username)
            .bind(seed)
            .bind(pool_size)
            .fetch_all(pool)
            .await?;
        // Stable two-tier merge: genre-sharing first, then the rest, each kept in
        // the ascending audio-distance order the query returned.
        let mut with_genre: Vec<Track> = Vec::new();
        let mut without_genre: Vec<Track> = Vec::new();
        for row in rows {
            picked.insert(row.track.id);
            if row.shares_genre {
                with_genre.push(row.track.into_model());
            } else {
                without_genre.push(row.track.into_model());
            }
        }
        out.extend(with_genre);
        out.extend(without_genre);
        out.truncate(limit);
    }

    // 2. Genre floor: shuffle in tracks that share a genre with the seed.
    if out.len() < limit {
        let sql = format!(
            "SELECT {TRACK_COLS} FROM tracks t \
             JOIN libraries l ON l.id = t.library_id \
             LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
             WHERE (l.kind = 'shared' OR l.owner = $2) AND NOT t.is_book AND t.id <> $3 \
               AND EXISTS ( \
                 SELECT 1 FROM track_genres tg WHERE tg.track_id = t.id \
                   AND tg.genre_id IN (SELECT genre_id FROM track_genres WHERE track_id = $3)) \
             ORDER BY random() LIMIT $4"
        );
        let rows = sqlx::query_as::<_, TrackRow>(&sql)
            .bind(user_id)
            .bind(username)
            .bind(seed)
            .bind(cap.saturating_mul(2))
            .fetch_all(pool)
            .await?;
        for r in rows {
            if picked.insert(r.id) {
                out.push(r.into_model());
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ albums

/// The paged album grid, derived by grouping tracks (books excluded).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn albums(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    kind: AlbumSort,
    limit: i64,
    offset: i64,
) -> Result<Vec<Album>, DbError> {
    let (having, order) = album_sort_sql(kind);
    let sql = format!(
        "SELECT {ALBUM_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         WHERE (l.kind = 'shared' OR l.owner = $2) AND t.album_id IS NOT NULL AND NOT t.is_book \
         GROUP BY t.album_id{having} ORDER BY {order} LIMIT $3 OFFSET $4"
    );
    let rows = sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(user_id)
        .bind(username)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(AlbumRow::into_model).collect())
}

/// One derived album by id, if any visible track carries it.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn album_single(
    pool: &PgPool,
    username: &str,
    album_id: Uuid,
) -> Result<Option<Album>, DbError> {
    let sql = format!(
        "SELECT {ALBUM_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         WHERE (l.kind = 'shared' OR l.owner = $1) AND t.album_id = $2 \
         GROUP BY t.album_id"
    );
    let row = sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(username)
        .bind(album_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(AlbumRow::into_model))
}

/// Every visible track on an album, ordered by disc then track (books included).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn album_tracks(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    album_id: Uuid,
) -> Result<Vec<Track>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         WHERE (l.kind = 'shared' OR l.owner = $2) AND t.album_id = $3 \
         ORDER BY t.disc_no, t.track_no"
    );
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(user_id)
        .bind(username)
        .bind(album_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TrackRow::into_model).collect())
}

/// Albums with no year on any track and not yet checked externally, for the
/// release-year enricher. Returns (album_id, album-artist, title).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn albums_missing_year(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<(Uuid, String, String)>, DbError> {
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT t.album_id, \
                coalesce(nullif(max(t.album_artist), ''), max(t.artist)) AS artist, \
                max(t.album) AS title \
         FROM tracks t \
         LEFT JOIN album_meta am ON am.album_id = t.album_id \
         WHERE t.album_id IS NOT NULL AND NOT t.is_book AND am.album_id IS NULL \
         GROUP BY t.album_id \
         HAVING max(t.year) = 0 \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Record an album's release year (0 = checked, none found) so it is not
/// re-queried on the next sweep.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn set_album_year(pool: &PgPool, album_id: Uuid, year: i32) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO album_meta (album_id, release_year, source, checked_at) \
         VALUES ($1, $2, 'musicbrainz', now()) \
         ON CONFLICT (album_id) DO UPDATE SET release_year = EXCLUDED.release_year, \
             source = EXCLUDED.source, checked_at = EXCLUDED.checked_at",
    )
    .bind(album_id)
    .bind(year)
    .execute(pool)
    .await?;
    Ok(())
}

// ------------------------------------------------------------------ artists

/// Every artist, derived by grouping visible non-book tracks.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn artists(pool: &PgPool, username: &str) -> Result<Vec<Artist>, DbError> {
    let sql = format!(
        "SELECT {ARTIST_COLS} FROM artists a \
         JOIN track_artists ta ON ta.artist_id = a.id \
         JOIN tracks t ON t.id = ta.track_id \
         JOIN libraries l ON l.id = t.library_id \
         WHERE (l.kind = 'shared' OR l.owner = $1) AND NOT t.is_book \
         GROUP BY a.id ORDER BY lower(a.name)"
    );
    let rows = sqlx::query_as::<_, ArtistRow>(&sql)
        .bind(username)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(ArtistRow::into_model).collect())
}

/// One derived artist by id, if any visible non-book track carries it.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn artist_single(
    pool: &PgPool,
    username: &str,
    artist_id: Uuid,
) -> Result<Option<Artist>, DbError> {
    let sql = format!(
        "SELECT {ARTIST_COLS} FROM artists a \
         JOIN track_artists ta ON ta.artist_id = a.id \
         JOIN tracks t ON t.id = ta.track_id \
         JOIN libraries l ON l.id = t.library_id \
         WHERE (l.kind = 'shared' OR l.owner = $1) AND a.id = $2 AND NOT t.is_book \
         GROUP BY a.id"
    );
    let row = sqlx::query_as::<_, ArtistRow>(&sql)
        .bind(username)
        .bind(artist_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(ArtistRow::into_model))
}

/// Every derived album for one artist, alphabetical (books excluded).
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn albums_for_artist(
    pool: &PgPool,
    username: &str,
    artist_id: Uuid,
) -> Result<Vec<Album>, DbError> {
    let sql = format!(
        "SELECT {ALBUM_COLS} FROM tracks t \
         JOIN track_artists ta ON ta.track_id = t.id \
         JOIN libraries l ON l.id = t.library_id \
         WHERE (l.kind = 'shared' OR l.owner = $1) AND ta.artist_id = $2 \
           AND t.album_id IS NOT NULL AND NOT t.is_book \
         GROUP BY t.album_id ORDER BY lower(max(t.album))"
    );
    let rows = sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(username)
        .bind(artist_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(AlbumRow::into_model).collect())
}

/// Every track crediting one artist, for the artist page's play-all queue.
/// Ordered by album then disc/track so a discography plays in a sensible order.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn tracks_for_artist(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    artist_id: Uuid,
) -> Result<Vec<Track>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLS} FROM tracks t \
         JOIN track_artists ta ON ta.track_id = t.id \
         JOIN libraries l ON l.id = t.library_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         WHERE (l.kind = 'shared' OR l.owner = $2) AND ta.artist_id = $3 AND NOT t.is_book \
         ORDER BY lower(t.album) NULLS FIRST, t.disc_no, t.track_no, lower(t.title)"
    );
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(user_id)
        .bind(username)
        .bind(artist_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TrackRow::into_model).collect())
}

// ------------------------------------------------------------------ genres

/// Genres carried by at least eight tracks, most common first. Library-wide, so
/// no per-user visibility filter.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn genres(pool: &PgPool) -> Result<Vec<GenreCount>, DbError> {
    let rows = sqlx::query_as::<_, GenreCount>(
        "SELECT g.name AS name, count(*) AS tracks FROM genres g \
         JOIN track_genres tg ON tg.genre_id = g.id \
         GROUP BY g.name HAVING count(*) >= 8 ORDER BY count(*) DESC, g.name ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ------------------------------------------------------------------ storage

/// This user's own downloads and the whole library's usage, as `(you, everyone)`.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn storage_usage(pool: &PgPool, username: &str) -> Result<(Usage, Usage), DbError> {
    let you = sqlx::query_as::<_, Usage>(
        "SELECT coalesce(sum(t.size_bytes), 0)::bigint AS bytes, count(*) AS tracks \
         FROM tracks t JOIN libraries l ON l.id = t.library_id \
         WHERE l.owner = $1 AND l.kind = 'user'",
    )
    .bind(username)
    .fetch_one(pool)
    .await?;
    let everyone = sqlx::query_as::<_, Usage>(
        "SELECT coalesce(sum(size_bytes), 0)::bigint AS bytes, count(*) AS tracks FROM tracks",
    )
    .fetch_one(pool)
    .await?;
    Ok((you, everyone))
}

// ------------------------------------------------------------------ starred

/// The user's starred tracks, albums, and artists. Tracks include books; albums
/// and artists resolve only when a visible track still carries the id.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn starred(pool: &PgPool, user_id: Uuid, username: &str) -> Result<Starred, DbError> {
    let tracks = starred_tracks(pool, user_id, username).await?;
    let albums = starred_albums(pool, user_id, username).await?;
    let artists = starred_artists(pool, user_id, username).await?;
    Ok(Starred {
        tracks,
        albums,
        artists,
    })
}

async fn starred_tracks(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
) -> Result<Vec<Track>, DbError> {
    let sql = "SELECT t.id, t.title, t.artist, t.album, t.album_id, t.duration_sec, t.year, \
               ann.starred AS starred \
               FROM annotations ann JOIN tracks t ON t.id = ann.item_id \
               JOIN libraries l ON l.id = t.library_id \
               WHERE ann.user_id = $1 AND ann.item_type = 'track' AND ann.starred \
                 AND (l.kind = 'shared' OR l.owner = $2) \
               ORDER BY ann.starred_at DESC";
    let rows = sqlx::query_as::<_, TrackRow>(sql)
        .bind(user_id)
        .bind(username)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TrackRow::into_model).collect())
}

async fn starred_albums(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
) -> Result<Vec<Album>, DbError> {
    let sql = format!(
        "SELECT {ALBUM_COLS} FROM annotations ann \
         JOIN tracks t ON t.album_id = ann.item_id \
         JOIN libraries l ON l.id = t.library_id \
         WHERE ann.user_id = $1 AND ann.item_type = 'album' AND ann.starred \
           AND (l.kind = 'shared' OR l.owner = $2) \
         GROUP BY t.album_id ORDER BY lower(max(t.album))"
    );
    let rows = sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(user_id)
        .bind(username)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(AlbumRow::into_model).collect())
}

async fn starred_artists(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
) -> Result<Vec<Artist>, DbError> {
    let sql = format!(
        "SELECT {ARTIST_COLS} FROM annotations ann \
         JOIN artists a ON a.id = ann.item_id \
         JOIN track_artists ta ON ta.artist_id = a.id \
         JOIN tracks t ON t.id = ta.track_id \
         JOIN libraries l ON l.id = t.library_id \
         WHERE ann.user_id = $1 AND ann.item_type = 'artist' AND ann.starred \
           AND (l.kind = 'shared' OR l.owner = $2) \
         GROUP BY a.id ORDER BY lower(a.name)"
    );
    let rows = sqlx::query_as::<_, ArtistRow>(&sql)
        .bind(user_id)
        .bind(username)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(ArtistRow::into_model).collect())
}

// ------------------------------------------------------------------ search

/// Library-only search over tracks, derived albums, and derived artists.
///
/// An empty query returns empty lists; `acquirable` is always empty (that route
/// is owned by the acquire package). Matches are case-insensitive substrings.
///
/// # Errors
/// Returns [`DbError`] if a query fails.
pub async fn search(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    query: &str,
    n: i64,
) -> Result<SearchResults, DbError> {
    if query.is_empty() {
        return Ok(SearchResults::default());
    }
    let pat = format!("%{query}%");
    let tracks = search_tracks(pool, user_id, username, &pat, n).await?;
    let albums = search_albums(pool, username, &pat, n).await?;
    let artists = search_artists(pool, username, &pat, n).await?;
    Ok(SearchResults {
        tracks,
        albums,
        artists,
        acquirable: Vec::new(),
    })
}

async fn search_tracks(
    pool: &PgPool,
    user_id: Uuid,
    username: &str,
    pat: &str,
    n: i64,
) -> Result<Vec<Track>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         WHERE (l.kind = 'shared' OR l.owner = $2) AND NOT t.is_book \
           AND (f_unaccent(t.title) ILIKE f_unaccent($3) OR f_unaccent(t.artist) ILIKE f_unaccent($3) OR f_unaccent(t.album) ILIKE f_unaccent($3)) LIMIT $4"
    );
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(user_id)
        .bind(username)
        .bind(pat)
        .bind(n)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TrackRow::into_model).collect())
}

/// `(artist, title)` for every track matching `query`, across the WHOLE library.
///
/// Deliberately UNSCOPED, unlike [`search`]. Its one caller is the duplicate
/// check in `music-request`, and the music tree is shared with Syncify, Explo and
/// LidaTube: a track sitting in another user's downloads is still a track you
/// already have, and re-downloading it is exactly the rot this check exists to
/// prevent. Scoping it to one user would report "not owned" for a file that is
/// on disk a directory away.
///
/// Books are excluded on the same grounds as everywhere else - `music-request`
/// downloads songs, and an audiobook chapter whose title happens to match would
/// only ever be a false positive.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn owned_matching(
    pool: &PgPool,
    query: &str,
    n: i64,
) -> Result<Vec<(String, String)>, DbError> {
    // TOKENISED, not one substring match on the whole query, and this is the
    // difference between working and silently never matching. Callers type
    // "radiohead creep" - an artist and a title. That exact string appears in NO
    // single column, so a `%radiohead creep%` LIKE returns nothing while the
    // track sits right there in the library. Measured: the first real search
    // after the port flagged 0 of 6 YouTube results as owned, with Radiohead's
    // "Creep" already on disk. Navidrome's search3 tokenised, so a naive port
    // reintroduces the duplicate downloads this check exists to prevent.
    //
    // Every term must match SOMEWHERE (title, artist or album) - AND across
    // terms, OR across columns. That is what makes artist+title work while
    // keeping "creep" from matching every track by an artist called Creep.
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("%{t}%"))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let sql = owned_sql(terms.len());
    let mut q = sqlx::query_as::<_, (String, String)>(&sql);
    for t in &terms {
        q = q.bind(t);
    }
    Ok(q.bind(n).fetch_all(pool).await?)
}

/// The SQL for [`owned_matching`] with `n_terms` term placeholders.
///
/// Split out so the placeholder numbering can be tested without a database. It
/// is built from the term COUNT and never from the term TEXT, so no user input
/// reaches the statement - the terms are bound.
fn owned_sql(n_terms: usize) -> String {
    use std::fmt::Write as _;
    let mut sql = String::from("SELECT t.artist, t.title FROM tracks t WHERE NOT t.is_book");
    for i in 1..=n_terms {
        let _ = write!(
            sql,
            " AND (f_unaccent(t.title) ILIKE f_unaccent(${i}) OR f_unaccent(t.artist) ILIKE f_unaccent(${i}) OR f_unaccent(t.album) ILIKE f_unaccent(${i}))"
        );
    }
    let _ = write!(sql, " ORDER BY t.artist, t.title LIMIT ${}", n_terms + 1);
    sql
}

#[cfg(test)]
mod owned_tests {
    use super::owned_sql;

    #[test]
    fn every_term_must_match_somewhere() {
        // "radiohead creep" is an artist AND a title: the terms have to be ANDed
        // across the row while ORing across columns. A single `%radiohead creep%`
        // matches no column and silently reports "not owned" for a track that is
        // right there - the regression this replaced.
        let sql = owned_sql(2);
        assert_eq!(sql.matches(" AND (").count(), 2);
        assert!(sql.contains("f_unaccent(t.title) ILIKE f_unaccent($1) OR f_unaccent(t.artist) ILIKE f_unaccent($1) OR f_unaccent(t.album) ILIKE f_unaccent($1)"));
        assert!(sql.contains("f_unaccent(t.title) ILIKE f_unaccent($2) OR f_unaccent(t.artist) ILIKE f_unaccent($2) OR f_unaccent(t.album) ILIKE f_unaccent($2)"));
    }

    #[test]
    fn the_limit_placeholder_follows_the_terms() {
        // Off-by-one here binds the LIMIT over a search term, which sqlx reports
        // as a type error at runtime and never at compile time.
        assert!(owned_sql(1).ends_with("LIMIT $2"));
        assert!(owned_sql(3).ends_with("LIMIT $4"));
        assert!(owned_sql(0).ends_with("LIMIT $1"));
    }

    #[test]
    fn books_are_excluded_whatever_the_term_count() {
        for n in 0..4 {
            assert!(owned_sql(n).contains("NOT t.is_book"), "n={n}");
        }
    }
}

async fn search_albums(
    pool: &PgPool,
    username: &str,
    pat: &str,
    n: i64,
) -> Result<Vec<Album>, DbError> {
    let sql = format!(
        "SELECT {ALBUM_COLS} FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         WHERE (l.kind = 'shared' OR l.owner = $1) AND t.album_id IS NOT NULL AND NOT t.is_book \
         GROUP BY t.album_id \
         HAVING f_unaccent(max(t.album)) ILIKE f_unaccent($2) \
             OR f_unaccent(coalesce(nullif(max(t.album_artist), ''), max(t.artist))) ILIKE f_unaccent($2) \
         ORDER BY lower(max(t.album)) LIMIT $3"
    );
    let rows = sqlx::query_as::<_, AlbumRow>(&sql)
        .bind(username)
        .bind(pat)
        .bind(n)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(AlbumRow::into_model).collect())
}

async fn search_artists(
    pool: &PgPool,
    username: &str,
    pat: &str,
    n: i64,
) -> Result<Vec<Artist>, DbError> {
    let sql = format!(
        "SELECT {ARTIST_COLS} FROM artists a \
         JOIN track_artists ta ON ta.artist_id = a.id \
         JOIN tracks t ON t.id = ta.track_id \
         JOIN libraries l ON l.id = t.library_id \
         WHERE (l.kind = 'shared' OR l.owner = $1) AND NOT t.is_book \
           AND f_unaccent(a.name) ILIKE f_unaccent($2) \
         GROUP BY a.id ORDER BY lower(a.name) LIMIT $3"
    );
    let rows = sqlx::query_as::<_, ArtistRow>(&sql)
        .bind(username)
        .bind(pat)
        .bind(n)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(ArtistRow::into_model).collect())
}

// ------------------------------------------------------------------ playlists

/// This user's playlists, alphabetical, with their track counts.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn playlists(pool: &PgPool, username: &str) -> Result<Vec<Playlist>, DbError> {
    let rows = sqlx::query_as::<_, PlaylistRow>(
        "SELECT p.id AS id, p.name AS name, p.owner_username AS owner, \
         count(pt.track_id) AS song_count \
         FROM playlists p LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id \
         WHERE p.owner_username = $1 \
         GROUP BY p.id, p.name, p.owner_username ORDER BY p.name",
    )
    .bind(username)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(PlaylistRow::into_model).collect())
}

/// One playlist by id, only if this user owns it.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn playlist_single(
    pool: &PgPool,
    username: &str,
    id: Uuid,
) -> Result<Option<Playlist>, DbError> {
    let row = sqlx::query_as::<_, PlaylistRow>(
        "SELECT p.id AS id, p.name AS name, p.owner_username AS owner, \
         (SELECT count(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id) AS song_count \
         FROM playlists p WHERE p.id = $1 AND p.owner_username = $2",
    )
    .bind(id)
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(PlaylistRow::into_model))
}

/// The tracks of a playlist in position order (books included). Ownership is the
/// caller's responsibility via [`playlist_single`].
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn playlist_tracks(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<Vec<Track>, DbError> {
    let sql = format!(
        "SELECT {TRACK_COLS} FROM playlist_tracks pt \
         JOIN tracks t ON t.id = pt.track_id \
         LEFT JOIN annotations ann ON ann.item_id = t.id AND ann.user_id = $1 \
         WHERE pt.playlist_id = $2 ORDER BY pt.position"
    );
    let rows = sqlx::query_as::<_, TrackRow>(&sql)
        .bind(user_id)
        .bind(id)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(TrackRow::into_model).collect())
}

// ------------------------------------------------------------------ bytes

/// The absolute path of a track, if it exists and is visible to this user.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn track_path(
    pool: &PgPool,
    username: &str,
    id: Uuid,
) -> Result<Option<String>, DbError> {
    let path = sqlx::query_scalar::<_, String>(
        "SELECT t.abs_path FROM tracks t \
         JOIN libraries l ON l.id = t.library_id \
         WHERE t.id = $1 AND (l.kind = 'shared' OR l.owner = $2)",
    )
    .bind(id)
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(path)
}

/// Resolve a cover id (a track, album, artist, or playlist id) to a track whose
/// cover art is cached, preferring the track itself. Artwork is not access-
/// controlled, matching the Go server's cover proxy.
///
/// # Errors
/// Returns [`DbError`] if the query fails.
pub async fn cover_track_id(pool: &PgPool, id: Uuid) -> Result<Option<(Uuid, String)>, DbError> {
    // Returns the PATH as well as the id, so the handler can re-extract a cover
    // whose cached file has gone missing. Without the path the cache could only
    // be filled by a full rescan, which made a wiped cache permanent.
    let track = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT t.id, t.abs_path FROM tracks t \
         WHERE t.has_cover AND ( \
             t.id = $1 OR t.album_id = $1 OR t.artist_id = $1 \
             OR t.id IN (SELECT track_id FROM playlist_tracks WHERE playlist_id = $1)) \
         ORDER BY (t.id = $1) DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(track)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::library::{get_or_create_library, set_track_artists, upsert_track, NewTrack};
    use sqlx::postgres::PgPoolOptions;

    fn vec25(first: f64) -> String {
        let mut parts = vec![first.to_string()];
        parts.extend(std::iter::repeat_n("0".to_owned(), 24));
        format!("[{}]", parts.join(","))
    }

    async fn seed_track(pool: &PgPool, lib: i32, id: Uuid, title: &str, vec_first: f64) {
        let track = NewTrack {
            id,
            library_id: lib,
            rel_path: format!("{title}.opus"),
            abs_path: format!("/music/{title}.opus"),
            title: title.to_owned(),
            artist: "Art".to_owned(),
            artist_id: Uuid::new_v4(),
            album_artist: String::new(),
            album: "Alb".to_owned(),
            album_id: None,
            track_no: 0,
            disc_no: 0,
            year: 0,
            genre: String::new(),
            duration_sec: 100,
            size_bytes: 1,
            has_cover: false,
            is_book: false,
            mtime: None,
        };
        upsert_track(pool, &track).await.unwrap();
        sqlx::query(
            "INSERT INTO track_features (track_id, vec) VALUES ($1, $2::vector)
             ON CONFLICT (track_id) DO UPDATE SET vec = EXCLUDED.vec",
        )
        .bind(id)
        .bind(vec25(vec_first))
        .execute(pool)
        .await
        .unwrap();
    }

    /// 2026-11-05: re-tagging 22 owned files on a download moved them to the top
    /// of "Recently added". A later write to a file must not make it new.
    #[tokio::test]
    async fn recently_added_ignores_a_later_rewrite_of_an_old_file() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = PgPoolOptions::new().connect(&url).await else {
            return;
        };
        let owner = format!("recent-{}", Uuid::new_v4());
        let lib = get_or_create_library(&pool, &format!("/{owner}"), "r", "user", &owner)
            .await
            .unwrap();
        let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
        seed_track(&pool, lib, old, &format!("Old {owner}"), 0.0).await;
        seed_track(&pool, lib, new, &format!("New {owner}"), 0.0).await;
        // `old` was added months ago and its file re-tagged today; `new` was
        // added yesterday and never touched.
        for (id, created, mtime) in [
            (old, "2026-09-15", "2026-11-05"),
            (new, "2026-11-04", "2026-11-04"),
        ] {
            sqlx::query("UPDATE tracks SET created_at = $2::date, mtime = $3::date WHERE id = $1")
                .bind(id)
                .bind(created)
                .bind(mtime)
                .execute(&pool)
                .await
                .unwrap();
        }

        let got = songs(&pool, Uuid::new_v4(), &owner, SongSort::Newest, None, 500)
            .await
            .unwrap();
        let order: Vec<String> = got
            .iter()
            .filter(|t| t.title.ends_with(&owner))
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(order, [new.to_string(), old.to_string()]);

        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn similar_orders_by_vector_distance_and_excludes_the_seed() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = PgPoolOptions::new().connect(&url).await else {
            return;
        };
        let lib = get_or_create_library(
            &pool,
            &format!("/sim-{}", Uuid::new_v4()),
            "sim",
            "shared",
            "",
        )
        .await
        .unwrap();
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        seed_track(&pool, lib, a, "seed", 0.0).await;
        seed_track(&pool, lib, b, "near", 0.1).await;
        seed_track(&pool, lib, c, "far", 9.0).await;

        let out = similar(&pool, Uuid::new_v4(), "anyone", a, 40)
            .await
            .unwrap();
        let ids: Vec<String> = out.iter().map(|t| t.id.clone()).collect();
        let bi = ids.iter().position(|x| *x == b.to_string());
        let ci = ids.iter().position(|x| *x == c.to_string());
        assert!(bi.is_some() && ci.is_some(), "both neighbours returned");
        assert!(bi < ci, "the near neighbour ranks before the far one");
        assert!(!ids.contains(&a.to_string()), "the seed is never returned");

        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn similar_tiers_genre_matches_ahead_of_closer_audio() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = PgPoolOptions::new().connect(&url).await else {
            return;
        };
        let lib = get_or_create_library(
            &pool,
            &format!("/simg-{}", Uuid::new_v4()),
            "simg",
            "shared",
            "",
        )
        .await
        .unwrap();
        let (seed, near_nogenre, far_genre) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        seed_track(&pool, lib, seed, "seed", 0.0).await;
        seed_track(&pool, lib, near_nogenre, "near", 0.1).await; // closest audio, no shared genre
        seed_track(&pool, lib, far_genre, "far", 5.0).await; // far audio, shares the seed's genre
        let g = format!("tier-{}", Uuid::new_v4());
        crate::genre::set_track_genres(&pool, seed, std::slice::from_ref(&g))
            .await
            .unwrap();
        crate::genre::set_track_genres(&pool, far_genre, std::slice::from_ref(&g))
            .await
            .unwrap();

        let out = similar(&pool, Uuid::new_v4(), "anyone", seed, 40)
            .await
            .unwrap();
        let ids: Vec<String> = out.iter().map(|t| t.id.clone()).collect();
        let far_i = ids.iter().position(|x| *x == far_genre.to_string());
        let near_i = ids.iter().position(|x| *x == near_nogenre.to_string());
        assert!(
            far_i.is_some() && near_i.is_some(),
            "both neighbours returned"
        );
        assert!(
            far_i < near_i,
            "genre-sharing track ranks ahead of the closer no-genre track"
        );

        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn track_copies_by_title_finds_a_copy_case_insensitively() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = PgPoolOptions::new().connect(&url).await else {
            return;
        };
        let lib = get_or_create_library(
            &pool,
            &format!("/id-{}", Uuid::new_v4()),
            "id",
            "shared",
            "",
        )
        .await
        .unwrap();
        let id = Uuid::new_v4();
        let title = format!("Dedup Song {}", Uuid::new_v4());
        seed_track(&pool, lib, id, &title, 0.0).await; // artist "Art", /music/{title}.opus

        // case-insensitive title match returns the path with its credited artist
        let hit = track_copies_by_title(&pool, &title.to_lowercase(), "nobody")
            .await
            .unwrap();
        assert_eq!(
            hit,
            vec![(format!("/music/{title}.opus"), "Art".to_owned())]
        );

        // an unknown song is not a match, so the caller downloads
        let miss = track_copies_by_title(&pool, "no such song at all", "nobody")
            .await
            .unwrap();
        assert!(miss.is_empty());

        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
    }

    async fn seed_named(pool: &PgPool, lib: i32, id: Uuid, title: &str, artist: &str, album: &str) {
        let track = NewTrack {
            id,
            library_id: lib,
            rel_path: format!("{id}.opus"),
            abs_path: format!("/music/{id}.opus"),
            title: title.to_owned(),
            artist: artist.to_owned(),
            artist_id: Uuid::new_v5(&Uuid::NAMESPACE_OID, artist.to_lowercase().as_bytes()),
            album_artist: String::new(),
            album: album.to_owned(),
            album_id: Some(Uuid::new_v5(
                &Uuid::NAMESPACE_OID,
                album.to_lowercase().as_bytes(),
            )),
            track_no: 0,
            disc_no: 0,
            year: 0,
            genre: String::new(),
            duration_sec: 100,
            size_bytes: 1,
            has_cover: false,
            is_book: false,
            mtime: None,
        };
        upsert_track(pool, &track).await.unwrap();
        set_track_artists(pool, id, artist).await.unwrap();
    }

    #[tokio::test]
    async fn search_folds_accents_case_and_partials() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = PgPoolOptions::new().connect(&url).await else {
            return;
        };
        let lib = get_or_create_library(
            &pool,
            &format!("/acc-{}", Uuid::new_v4()),
            "acc",
            "shared",
            "",
        )
        .await
        .unwrap();
        let user = Uuid::new_v4();
        seed_named(
            &pool,
            lib,
            Uuid::new_v4(),
            "Ace of Spades",
            "Motörhead",
            "Ace of Spades",
        )
        .await;
        seed_named(
            &pool,
            lib,
            Uuid::new_v4(),
            "Não Sei",
            "João Gilberto",
            "Coração",
        )
        .await;

        // Unaccented query finds the accented row (the reported bug).
        let r = search(&pool, user, "anyone", "motorhead", 50)
            .await
            .unwrap();
        assert!(
            r.artists.iter().any(|a| a.name == "Motörhead"),
            "motorhead -> Motörhead artist"
        );
        assert!(r.tracks.iter().any(|t| t.title == "Ace of Spades"));

        // Accent + uppercase together still match (case-insensitive too).
        let r = search(&pool, user, "anyone", "MOTÖRHEAD", 50)
            .await
            .unwrap();
        assert!(r.artists.iter().any(|a| a.name == "Motörhead"));

        // Album: cedilla + tilde dropped by the user.
        let r = search(&pool, user, "anyone", "coracao", 50).await.unwrap();
        assert!(
            r.albums.iter().any(|a| a.name == "Coração"),
            "coracao -> Coração album"
        );

        // Artist and a bare partial substring.
        let r = search(&pool, user, "anyone", "joao", 50).await.unwrap();
        assert!(r.artists.iter().any(|a| a.name == "João Gilberto"));
        let r = search(&pool, user, "anyone", "sei", 50).await.unwrap();
        assert!(
            r.tracks.iter().any(|t| t.title == "Não Sei"),
            "partial substring matches"
        );

        // The tokenised owned-track dedup folds accents too: an unaccented
        // "artist + title" query must recognise the accented track as owned.
        let owned = owned_matching(&pool, "joao gilberto nao sei", 10)
            .await
            .unwrap();
        assert!(
            owned
                .iter()
                .any(|(a, t)| a == "João Gilberto" && t == "Não Sei"),
            "unaccented multi-term query must match the owned accented track"
        );

        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn artist_view_splits_comma_credits_and_plays_all() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = PgPoolOptions::new().connect(&url).await else {
            return;
        };
        let lib = get_or_create_library(
            &pool,
            &format!("/art-{}", Uuid::new_v4()),
            "art",
            "shared",
            "",
        )
        .await
        .unwrap();
        let user = Uuid::new_v4();
        seed_named(
            &pool,
            lib,
            Uuid::new_v4(),
            "Humor Negro",
            "Cronicas de um Babaca",
            "",
        )
        .await;
        seed_named(
            &pool,
            lib,
            Uuid::new_v4(),
            "Mo-Zaico",
            "Cronicas de um Babaca, Brandu",
            "",
        )
        .await;

        let cronicas = mh_core::ids::artist_id("Cronicas de um Babaca");
        let brandu = mh_core::ids::artist_id("Brandu");

        // The list carries the person ONCE, not one row per collaboration credit.
        let all = artists(&pool, "anyone").await.unwrap();
        assert_eq!(
            all.iter().filter(|a| a.id == cronicas.to_string()).count(),
            1,
            "one Cronicas artist row, not one per collab"
        );
        assert!(all.iter().any(|a| a.id == brandu.to_string()));

        // The artist page plays every track they are credited on: solo and collab.
        let tracks = tracks_for_artist(&pool, user, "anyone", cronicas)
            .await
            .unwrap();
        let titles: Vec<&str> = tracks.iter().map(|t| t.title.as_str()).collect();
        assert!(titles.contains(&"Humor Negro") && titles.contains(&"Mo-Zaico"));
        // A collaborator's page shows only the tracks they are on.
        let bt = tracks_for_artist(&pool, user, "anyone", brandu)
            .await
            .unwrap();
        assert_eq!(bt.len(), 1);
        assert_eq!(bt[0].title, "Mo-Zaico");

        // Search folds accents and returns the individual artist, never the raw
        // comma-joined credit string.
        let r = search(&pool, user, "anyone", "cronicas", 50).await.unwrap();
        assert!(r
            .artists
            .iter()
            .any(|a| a.id == cronicas.to_string() && a.name == "Cronicas de um Babaca"));
        assert!(!r.artists.iter().any(|a| a.name.contains(',')));

        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pool)
            .await
            .unwrap();
    }
}
