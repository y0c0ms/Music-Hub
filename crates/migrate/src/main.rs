//! One-shot Navidrome SQLite -> Postgres importer.
//!
//! Reads Navidrome's `media_file`, `library`, `user`, `playlist`,
//! `playlist_tracks`, and `annotation` tables and imports playlists, starred
//! flags, and play counts into music-hub's Postgres, matching tracks by absolute
//! file path (`library.path || '/' || media_file.path`).
//!
//! Dry-run by default; pass `--commit` to write. Idempotent: an annotation
//! upserts, and a playlist with the same owner and name is skipped. Users must
//! already exist in Postgres (they are created on first Keycloak login); a
//! Navidrome user with no match is reported and skipped.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::Connection;
use sqlx::PgPool;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut path = None;
    let mut commit = false;
    for arg in std::env::args().skip(1) {
        if arg == "--commit" {
            commit = true;
        } else {
            path = Some(arg);
        }
    }
    let Some(path) = path else {
        eprintln!("usage: mh-migrate <navidrome.db> [--commit]   (DATABASE_URL env required)");
        std::process::exit(2);
    };
    let database_url = std::env::var("DATABASE_URL")?;

    let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let nav = read_navidrome(&conn)?;
    drop(conn);

    let pg = mh_db::connect(&database_url).await?;
    let report = apply(&pg, &nav, commit).await?;
    println!("{report}");
    if !commit {
        println!("(dry run — pass --commit to write)");
    }
    Ok(())
}

/// Everything read out of the Navidrome database, already resolved to names and
/// absolute paths so no further SQLite access is needed during the async writes.
struct NavData {
    /// media_file id -> absolute path.
    paths: HashMap<String, String>,
    playlists: Vec<NavPlaylist>,
    annotations: Vec<NavAnnotation>,
}

struct NavPlaylist {
    name: String,
    owner: String,
    /// media_file ids in playlist order.
    track_ids: Vec<String>,
}

struct NavAnnotation {
    media_file_id: String,
    user_name: String,
    starred: bool,
    starred_at: Option<DateTime<Utc>>,
    play_count: i64,
    last_played_at: Option<DateTime<Utc>>,
}

fn read_navidrome(conn: &Connection) -> rusqlite::Result<NavData> {
    let mut paths = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT mf.id, l.path || '/' || mf.path \
             FROM media_file mf JOIN library l ON l.id = mf.library_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, p) = row?;
            paths.insert(id, p);
        }
    }

    let mut users: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, user_name FROM user")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, name) = row?;
            users.insert(id, name);
        }
    }

    let mut playlists = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, name, owner_id FROM playlist")?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (pid, name, owner_id) in rows {
            let owner = users.get(&owner_id).cloned().unwrap_or_default();
            let mut tstmt = conn.prepare(
                "SELECT media_file_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY id",
            )?;
            let track_ids: Vec<String> = tstmt
                .query_map([&pid], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?;
            playlists.push(NavPlaylist {
                name,
                owner,
                track_ids,
            });
        }
    }

    let mut annotations = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT user_id, item_id, starred, starred_at, play_count, play_date \
             FROM annotation WHERE item_type = 'media_file'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<bool>>(2)?.unwrap_or(false),
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (uid, mfid, starred, starred_at, play_count, play_date) = row?;
            let Some(user_name) = users.get(&uid).cloned() else {
                continue;
            };
            if user_name.is_empty() {
                continue;
            }
            annotations.push(NavAnnotation {
                media_file_id: mfid,
                user_name,
                starred,
                starred_at: starred_at.as_deref().and_then(parse_ts),
                play_count,
                last_played_at: play_date.as_deref().and_then(parse_ts),
            });
        }
    }

    Ok(NavData {
        paths,
        playlists,
        annotations,
    })
}

/// Navidrome writes timestamps as RFC3339 or `YYYY-MM-DD HH:MM:SS`; anything else
/// becomes `None` rather than failing the import.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| n.and_utc())
}

#[derive(Default)]
struct Report {
    playlists_created: usize,
    playlists_skipped: usize,
    playlist_tracks: usize,
    annotations: usize,
    skipped_no_track: usize,
    skipped_no_user: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "playlists: {} created, {} already present ({} tracks); annotations: {}; \
             skipped: {} unmatched track, {} unknown user",
            self.playlists_created,
            self.playlists_skipped,
            self.playlist_tracks,
            self.annotations,
            self.skipped_no_track,
            self.skipped_no_user,
        )
    }
}

async fn apply(pg: &PgPool, nav: &NavData, commit: bool) -> Result<Report, sqlx::Error> {
    let mut report = Report::default();

    let track_rows: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, abs_path FROM tracks")
        .fetch_all(pg)
        .await?;
    let tracks: HashMap<&str, Uuid> = track_rows.iter().map(|(id, p)| (p.as_str(), *id)).collect();

    let user_rows: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, username FROM users")
        .fetch_all(pg)
        .await?;
    let users: HashMap<String, Uuid> = user_rows
        .iter()
        .map(|(id, u)| (u.to_lowercase(), *id))
        .collect();

    for pl in &nav.playlists {
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM playlists WHERE owner_username = $1 AND name = $2")
                .bind(&pl.owner)
                .bind(&pl.name)
                .fetch_optional(pg)
                .await?;
        if exists.is_some() {
            report.playlists_skipped += 1;
            continue;
        }
        let ids: Vec<Uuid> = pl
            .track_ids
            .iter()
            .filter_map(|mfid| nav.paths.get(mfid))
            .filter_map(|abs| tracks.get(abs.as_str()).copied())
            .collect();
        if commit {
            let pid: Uuid = sqlx::query_scalar(
                "INSERT INTO playlists (owner_username, name) VALUES ($1, $2) RETURNING id",
            )
            .bind(&pl.owner)
            .bind(&pl.name)
            .fetch_one(pg)
            .await?;
            for (pos, tid) in ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES ($1, $2, $3)",
                )
                .bind(pid)
                .bind(tid)
                .bind(i32::try_from(pos).unwrap_or(i32::MAX))
                .execute(pg)
                .await?;
            }
        }
        report.playlists_created += 1;
        report.playlist_tracks += ids.len();
    }

    for a in &nav.annotations {
        let Some(tid) = nav
            .paths
            .get(&a.media_file_id)
            .and_then(|abs| tracks.get(abs.as_str()))
        else {
            report.skipped_no_track += 1;
            continue;
        };
        let Some(uid) = users.get(&a.user_name.to_lowercase()) else {
            report.skipped_no_user += 1;
            continue;
        };
        if commit {
            sqlx::query(
                "INSERT INTO annotations \
                   (user_id, item_id, item_type, starred, starred_at, play_count, last_played_at) \
                 VALUES ($1, $2, 'track', $3, $4, $5, $6) \
                 ON CONFLICT (user_id, item_id) DO UPDATE SET \
                   starred = EXCLUDED.starred, starred_at = EXCLUDED.starred_at, \
                   play_count = EXCLUDED.play_count, last_played_at = EXCLUDED.last_played_at",
            )
            .bind(uid)
            .bind(tid)
            .bind(a.starred)
            .bind(a.starred_at)
            .bind(i32::try_from(a.play_count).unwrap_or(i32::MAX))
            .bind(a.last_played_at)
            .execute(pg)
            .await?;
        }
        report.annotations += 1;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn navidrome_fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE library (id INTEGER PRIMARY KEY, path TEXT, name TEXT);
             CREATE TABLE media_file (id TEXT PRIMARY KEY, path TEXT, library_id INTEGER,
                 title TEXT, artist TEXT);
             CREATE TABLE user (id TEXT PRIMARY KEY, user_name TEXT);
             CREATE TABLE playlist (id TEXT PRIMARY KEY, name TEXT, owner_id TEXT);
             CREATE TABLE playlist_tracks (id INTEGER, playlist_id TEXT, media_file_id TEXT);
             CREATE TABLE annotation (user_id TEXT, item_id TEXT, item_type TEXT,
                 starred INTEGER, starred_at TEXT, play_count INTEGER, play_date TEXT, rating INTEGER);
             INSERT INTO library VALUES (1, '/music', 'Music');
             INSERT INTO media_file VALUES ('mf1', 'song.opus', 1, 'Song', 'Artist');
             INSERT INTO media_file VALUES ('mf2', 'other.opus', 1, 'Other', 'Artist');
             INSERT INTO user VALUES ('u1', 'Sam');
             INSERT INTO playlist VALUES ('p1', 'Faves', 'u1');
             INSERT INTO playlist_tracks VALUES (1, 'p1', 'mf1');
             INSERT INTO playlist_tracks VALUES (2, 'p1', 'mf2');
             INSERT INTO annotation VALUES ('u1', 'mf1', 'media_file', 1,
                 '2025-01-02 03:04:05', 7, '2025-02-03 04:05:06', 0);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn read_navidrome_resolves_paths_owners_and_annotations() {
        let nav = read_navidrome(&navidrome_fixture()).unwrap();
        assert_eq!(nav.paths.get("mf1").unwrap(), "/music/song.opus");
        assert_eq!(nav.playlists.len(), 1);
        assert_eq!(nav.playlists[0].owner, "Sam");
        assert_eq!(nav.playlists[0].track_ids, vec!["mf1", "mf2"]); // in playlist order
        assert_eq!(nav.annotations.len(), 1);
        let a = &nav.annotations[0];
        assert!(a.starred);
        assert_eq!(a.play_count, 7);
        assert_eq!(a.user_name, "Sam");
        assert!(a.starred_at.is_some()); // space-format timestamp parsed
        assert!(a.last_played_at.is_some());
    }

    #[test]
    fn parse_ts_handles_both_shapes_and_rejects_junk() {
        assert!(parse_ts("2025-01-02T03:04:05Z").is_some());
        assert!(parse_ts("2025-01-02 03:04:05").is_some());
        assert!(parse_ts("").is_none());
        assert!(parse_ts("not a date").is_none());
    }

    #[tokio::test]
    async fn apply_imports_playlists_and_annotations_matched_by_path() {
        use sqlx::postgres::PgPoolOptions;
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pg) = PgPoolOptions::new().connect(&url).await else {
            return;
        };
        // Seed our side: one user and the track the fixture's mf1 maps to.
        let user = Uuid::new_v4();
        let lib: i32 = mh_db::library::get_or_create_library(
            &pg,
            &format!("/mig-{user}"),
            "mig",
            "shared",
            "",
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO users (id, kc_sub, username) VALUES ($1, $2, 'Sam')")
            .bind(user)
            .bind(format!("mig-{user}"))
            .execute(&pg)
            .await
            .unwrap();
        let track = Uuid::new_v4();
        let nt = mh_db::library::NewTrack {
            id: track,
            library_id: lib,
            rel_path: "song.opus".to_owned(),
            abs_path: "/music/song.opus".to_owned(),
            title: "Song".to_owned(),
            artist: "Artist".to_owned(),
            artist_id: Uuid::new_v4(),
            album_artist: String::new(),
            album: "Alb".to_owned(),
            album_id: None,
            track_no: 0,
            disc_no: 0,
            year: 0,
            genre: String::new(),
            duration_sec: 1,
            size_bytes: 1,
            has_cover: false,
            is_book: false,
            mtime: None,
        };
        mh_db::library::upsert_track(&pg, &nt).await.unwrap();

        let nav = read_navidrome(&navidrome_fixture()).unwrap();
        let report = apply(&pg, &nav, true).await.unwrap();
        assert_eq!(report.playlists_created, 1);
        assert_eq!(report.playlist_tracks, 1); // only mf1 exists on our side
        assert_eq!(report.annotations, 1);

        let starred: bool = sqlx::query_scalar(
            "SELECT starred FROM annotations WHERE user_id = $1 AND item_id = $2",
        )
        .bind(user)
        .bind(track)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert!(starred);
        let pcount: i32 = sqlx::query_scalar(
            "SELECT play_count FROM annotations WHERE user_id = $1 AND item_id = $2",
        )
        .bind(user)
        .bind(track)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert_eq!(pcount, 7);
        let plname: String = sqlx::query_scalar(
            "SELECT name FROM playlists WHERE owner_username = 'Sam' ORDER BY name LIMIT 1",
        )
        .fetch_one(&pg)
        .await
        .unwrap();
        assert_eq!(plname, "Faves");

        // Idempotent: a second run creates no new playlist.
        let again = apply(&pg, &nav, true).await.unwrap();
        assert_eq!(again.playlists_created, 0);
        assert_eq!(again.playlists_skipped, 1);

        // Cleanup.
        sqlx::query("DELETE FROM playlists WHERE owner_username = 'Sam'")
            .execute(&pg)
            .await
            .unwrap();
        sqlx::query("DELETE FROM tracks WHERE library_id = $1")
            .bind(lib)
            .execute(&pg)
            .await
            .unwrap();
        sqlx::query("DELETE FROM libraries WHERE id = $1")
            .bind(lib)
            .execute(&pg)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user)
            .execute(&pg)
            .await
            .unwrap();
    }
}
