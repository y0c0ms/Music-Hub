//! Filesystem scanner: walk `MUSIC_ROOT`, read tags/duration/cover with ffprobe,
//! and upsert the `tracks` index. Replaces Navidrome's `AllSongs`/`RealPath`/
//! `SongIDByPath` — the absolute path is the key.

use std::path::Path;

use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use mh_db::library::NewTrack;
use serde::Serialize;

use crate::{AppError, AppState};

const AUDIO_EXTS: &[&str] = &[
    "opus", "mp3", "flac", "m4a", "ogg", "oga", "aac", "wav", "aiff", "alac", "wma",
];

/// What a scan did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub scanned: usize,
    pub indexed: u64,
    pub skipped: u64,
    pub deleted: usize,
}

/// Which library a path belongs to.
struct LibraryInfo {
    path: String,
    name: String,
    kind: &'static str,
    owner: String,
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str()))
}

fn file_stem(abs_path: &str) -> String {
    Path::new(abs_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned()
}

/// The album tag, or empty when it only repeats the track title.
///
/// yt-dlp writes the video title into the album tag as well as the title tag, so
/// a downloaded single arrives claiming to be an album containing exactly itself.
/// 129 of 600 tracks were in that state, and each one became its own card in the
/// albums grid - "almost all my singles are being considered albums".
///
/// The comparison is case- and whitespace-insensitive because the two tags are
/// written by different code paths and disagree on both. It is NOT fuzzy beyond
/// that: an album whose name genuinely differs from the track is a real record we
/// happen to own one song from, and dropping that would lose true information.
fn collapse_single_album(album: &str, title: &str) -> String {
    if album.trim().to_lowercase() == title.trim().to_lowercase() {
        return String::new();
    }
    album.to_owned()
}

/// Derive the owning library and the library-relative path for an absolute path.
/// `/music/users/<name>/downloads/...` is that user's library; everything else
/// is shared. Pure, so it is unit-tested without a filesystem.
fn derive_library(music_root: &str, abs_path: &str) -> (LibraryInfo, String) {
    let rel = abs_path
        .strip_prefix(music_root)
        .unwrap_or(abs_path)
        .trim_start_matches('/')
        .to_owned();
    if let Some(rest) = rel.strip_prefix("users/") {
        if let Some((name, tail)) = rest.split_once('/') {
            if !name.is_empty() && (tail == "downloads" || tail.starts_with("downloads/")) {
                return (
                    LibraryInfo {
                        path: format!("{music_root}/users/{name}/downloads"),
                        name: name.to_owned(),
                        kind: "user",
                        // LOWERCASED, and this is load-bearing. The owner comes
                        // from a DIRECTORY NAME (`/music/users/Sinem/downloads`)
                        // while the session username comes from Keycloak
                        // (`sinem`), and visibility is `l.owner = $username` - a
                        // case-sensitive comparison. Mismatched, a user sees only
                        // the shared library: measured after the cutover, Sinem
                        // could see 36 tracks of her 280. The display name keeps
                        // the original case; only the identity is normalised.
                        owner: name.to_lowercase(),
                    },
                    rel,
                );
            }
        }
    }
    (
        LibraryInfo {
            path: music_root.to_owned(),
            name: "music".to_owned(),
            kind: "shared",
            owner: String::new(),
        },
        rel,
    )
}

struct FileEntry {
    abs_path: String,
    mtime: Option<DateTime<Utc>>,
    size: i64,
}

/// Recursively collect audio files with their second-precision mtime and size.
/// Second precision keeps the DB round-trip exact so the incremental skip check
/// does not re-probe unchanged files.
fn walk_audio(root: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip hidden entries, files and directories alike. The downloader
            // writes in-progress files as `.mh-<hex>.opus` and renames them into
            // place when complete, and bookfetch stages whole books under
            // `.incoming-<hex>/`. Indexing those gives half-written audio a row
            // in the library - measured here as 8 `.mh-*` tracks showing up as
            // "Unknown artist". Navidrome skipped dotfiles; this did not.
            if entry
                .file_name()
                .to_str()
                .is_none_or(|n| n.starts_with('.'))
            {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() && is_audio(&path) {
                let meta = entry.metadata().ok();
                let size = meta
                    .as_ref()
                    .and_then(|m| i64::try_from(m.len()).ok())
                    .unwrap_or(0);
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|d| i64::try_from(d.as_secs()).ok())
                    .and_then(|secs| DateTime::from_timestamp(secs, 0));
                if let Some(p) = path.to_str() {
                    out.push(FileEntry {
                        abs_path: p.to_owned(),
                        mtime,
                        size,
                    });
                }
            }
        }
    }
    out
}

/// One scan at a time, process-wide.
///
/// A scan's `delete_absent` removes every row whose path was not in ITS walk.
/// Every finished download triggers a scan, so on an album several overlap: a
/// scan that walked the folder before a file was renamed into place finished
/// after another scan had indexed that file, and deleted the fresh row. Measured
/// 2026-11-05 13:11:22Z: `Thank God.opus` written at 13:10:56, indexed, then
/// `deleted=1`, and the download reported "downloaded but not indexed yet".
/// Serialised, every scan's walk starts after the previous scan's writes.
static SCAN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Walk `MUSIC_ROOT`, index new/changed tracks, extract their covers, and delete
/// rows whose files vanished.
///
/// # Errors
/// Returns [`AppError`] on a database failure or if the walk task panics.
pub async fn scan(state: &AppState) -> Result<ScanReport, AppError> {
    let _one_at_a_time = SCAN_LOCK.lock().await;
    let root = state.config.music_root.clone();
    let books_prefix = state.config.books_prefix.clone();
    let cover_dir = Path::new(&state.config.state_dir).join("covers");
    let _ = tokio::fs::create_dir_all(&cover_dir).await;

    let walk_root = root.clone();
    let files = tokio::task::spawn_blocking(move || walk_audio(Path::new(&walk_root)))
        .await
        .map_err(|e| AppError::internal(format!("scan walk task: {e}")))?;

    let existing = mh_db::library::all_stats(&state.pool).await?;

    let mut present = Vec::with_capacity(files.len());
    let mut indexed = 0u64;
    let mut skipped = 0u64;
    for f in &files {
        present.push(f.abs_path.clone());
        if let Some((mtime, size)) = existing.get(&f.abs_path) {
            if *size == f.size && *mtime == f.mtime {
                skipped += 1;
                continue;
            }
        }
        let probe = match mh_media::ffprobe::probe(Path::new(&f.abs_path)).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(path = %f.abs_path, error = %e, "skipping unreadable file");
                continue;
            }
        };
        let (lib, rel) = derive_library(&root, &f.abs_path);
        let is_book = rel.starts_with(&books_prefix);
        let library_id = mh_db::library::get_or_create_library(
            &state.pool,
            &lib.path,
            &lib.name,
            lib.kind,
            &lib.owner,
        )
        .await?;

        let artist = probe.artist().to_owned();
        let album_artist = if probe.album_artist().is_empty() {
            artist.clone()
        } else {
            probe.album_artist().to_owned()
        };
        let title = if probe.title().is_empty() {
            file_stem(&f.abs_path)
        } else {
            probe.title().to_owned()
        };
        // A self-titled single (yt-dlp writes the video title into BOTH album and
        // title, so 129 tracks here each became a one-track album) collapses to no
        // album - but ONLY when the file carries no explicit album-artist. A
        // deliberate album sets one, and a track there may legitimately share the
        // album's name ("Humor Negro" on the album "Humor Negro"), so it is kept.
        // A track off a real album we own one song from - Keane's "Hopes And
        // Fears" - was never album==title, so it was and stays unaffected.
        let album = if probe.album_artist().is_empty() {
            collapse_single_album(probe.album(), &title)
        } else {
            probe.album().to_owned()
        };
        let id = mh_core::ids::track_id(&f.abs_path);
        let track = NewTrack {
            id,
            library_id,
            rel_path: rel,
            abs_path: f.abs_path.clone(),
            title,
            artist_id: mh_core::ids::artist_id(&artist),
            album_id: if album.is_empty() {
                None
            } else {
                Some(mh_core::ids::album_id(&album_artist, &album))
            },
            artist,
            album_artist,
            album,
            track_no: probe.track_no(),
            disc_no: probe.disc_no(),
            year: probe.year(),
            genre: probe.genre().to_owned(),
            duration_sec: probe.duration_sec,
            size_bytes: f.size,
            has_cover: probe.has_cover,
            is_book,
            mtime: f.mtime,
        };
        mh_db::library::upsert_track(&state.pool, &track).await?;
        mh_db::library::set_track_artists(&state.pool, id, &track.artist).await?;
        if probe.has_cover {
            let dest = cover_dir.join(format!("{id}.jpg"));
            if let Err(e) = mh_media::ffprobe::extract_cover(Path::new(&f.abs_path), &dest).await {
                tracing::warn!(path = %f.abs_path, error = %e, "cover extract failed");
            }
        }
        indexed += 1;
    }

    let deleted = mh_db::library::delete_absent(&state.pool, &present).await?;
    for id in &deleted {
        let _ = tokio::fs::remove_file(cover_dir.join(format!("{id}.jpg"))).await;
    }

    tracing::info!(
        scanned = files.len(),
        indexed,
        skipped,
        deleted = deleted.len(),
        "library scan complete"
    );
    Ok(ScanReport {
        scanned: files.len(),
        indexed,
        skipped,
        deleted: deleted.len(),
    })
}

/// Index the library once at startup and log the result; the binary spawns it.
///
/// Nothing else scans a library copied in by hand: scans otherwise run only
/// after a download or through `/api/internal/scan`, so a fresh install showed
/// an empty app. Files already indexed are skipped by (mtime, size), so on a
/// warm restart this is one directory walk. A failure is logged, not fatal:
/// the server is still useful and the next download scans again.
pub async fn startup_scan(state: AppState) {
    match scan(&state).await {
        Ok(r) => tracing::info!(
            indexed = r.indexed,
            scanned = r.scanned,
            "startup scan done"
        ),
        Err(e) => tracing::warn!(error = %e, "startup scan failed"),
    }
}

/// `POST /api/library/scan` — rescan the library. Behind `require`; the session
/// is not used because the index is global.
///
/// # Errors
/// Returns [`AppError`] if the scan fails.
pub async fn scan_handler(State(state): State<AppState>) -> Result<Json<ScanReport>, AppError> {
    Ok(Json(scan(&state).await?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn an_album_that_only_repeats_the_title_is_dropped() {
        // The yt-dlp single: album and title are the same string, so it is not an
        // album at all and must not become a one-track card in the grid.
        assert_eq!(collapse_single_album("Creep", "Creep"), "");
        // Case and whitespace differ because the two tags are written by
        // different code paths.
        assert_eq!(collapse_single_album("  creep ", "Creep"), "");
        assert_eq!(collapse_single_album("A PIOR MUSICA", "a pior musica"), "");
    }

    #[test]
    fn a_real_album_survives_even_when_we_own_one_track_of_it() {
        // THE case this must not break. We own one Keane song; "Hopes And Fears"
        // is a real record, correctly named, and collapsing it would destroy true
        // information to tidy a grid.
        assert_eq!(
            collapse_single_album("Hopes And Fears", "Somewhere Only We Know"),
            "Hopes And Fears"
        );
        // Substrings are not matches - narrow on purpose.
        assert_eq!(collapse_single_album("Creep EP", "Creep"), "Creep EP");
        assert_eq!(collapse_single_album("", "Creep"), "");
    }

    #[test]
    fn derive_library_splits_user_downloads_from_shared() {
        let (lib, rel) = derive_library("/music", "/music/users/sam/downloads/a/b.opus");
        assert_eq!(lib.kind, "user");
        assert_eq!(lib.owner, "sam");
        assert_eq!(lib.path, "/music/users/sam/downloads");
        assert_eq!(rel, "users/sam/downloads/a/b.opus");

        let (lib, rel) = derive_library("/music", "/music/albums/x.flac");
        assert_eq!(lib.kind, "shared");
        assert_eq!(lib.owner, "");
        assert_eq!(rel, "albums/x.flac");

        // "users/sam/other" is not a downloads folder -> shared.
        let (lib, _) = derive_library("/music", "/music/users/sam/other/x.mp3");
        assert_eq!(lib.kind, "shared");
    }

    #[test]
    fn is_audio_matches_known_extensions() {
        assert!(is_audio(Path::new("/x/a.opus")));
        assert!(is_audio(Path::new("/x/a.MP3")));
        assert!(is_audio(Path::new("/x/a.flac")));
        assert!(!is_audio(Path::new("/x/a.txt")));
        assert!(!is_audio(Path::new("/x/cover.jpg")));
        assert!(!is_audio(Path::new("/x/noext")));
    }

    #[test]
    fn file_stem_extracts_name() {
        assert_eq!(file_stem("/music/users/sam/downloads/Song.opus"), "Song");
        assert_eq!(file_stem("bare.mp3"), "bare");
    }

    #[tokio::test]
    async fn scans_a_fixture_directory() {
        use mh_core::Config;
        use sqlx::postgres::PgPoolOptions;
        use std::collections::HashMap;
        use std::process::Command as Cmd;

        let ffmpeg = Cmd::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success());
        let Some(admin_url) = std::env::var("DATABASE_URL").ok() else {
            return;
        };
        if !ffmpeg {
            return;
        }

        // Throwaway database: scan mutates the whole tracks table, so isolate it.
        let (base, _) = admin_url.rsplit_once('/').unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let test_db = format!("mh_scan_{nanos}");
        let admin = PgPoolOptions::new()
            .connect(&format!("{base}/postgres"))
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE {test_db}"))
            .execute(&admin)
            .await
            .unwrap();
        let url = format!("{base}/{test_db}");
        let pool = PgPoolOptions::new().connect(&url).await.unwrap();
        mh_db::migrate(&pool).await.unwrap();

        // Fixture: a tagged opus under a per-user downloads folder.
        let root = std::env::temp_dir().join(format!("mh-scanfix-{nanos}"));
        let downloads = root.join("users/sam/downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let song = downloads.join("song.opus");
        assert!(Cmd::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("anullsrc=r=48000:cl=mono")
            .args(["-t", "1", "-c:a", "libopus"])
            .args(["-metadata", "TITLE=Lemon Tree"])
            .args(["-metadata", "ARTIST=Fools Garden"])
            .args(["-metadata", "ALBUM=Dish"])
            .arg(&song)
            .status()
            .unwrap()
            .success());

        let state_dir = std::env::temp_dir().join(format!("mh-scanstate-{nanos}"));
        std::fs::create_dir_all(&state_dir).unwrap();
        let env: HashMap<String, String> = [
            ("OIDC_ISSUER", "https://kc"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", url.as_str()),
            ("MUSIC_ROOT", root.to_str().unwrap()),
            ("STATE_DIR", state_dir.to_str().unwrap()),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let cfg = Config::load(|k| env.get(k).cloned()).unwrap();
        let app = AppState::for_test(cfg, pool.clone());

        // A fresh install is indexed by the startup path, with no request made.
        startup_scan(app.clone()).await;
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1, "startup scan indexed the library");

        let (title, artist, dur, is_book): (String, String, i32, bool) =
            sqlx::query_as("SELECT title, artist, duration_sec, is_book FROM tracks")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(title, "Lemon Tree");
        assert_eq!(artist, "Fools Garden");
        assert!(dur >= 1);
        assert!(!is_book);

        let (kind, owner): (String, String) = sqlx::query_as("SELECT kind, owner FROM libraries")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind, "user");
        assert_eq!(owner, "sam");

        // A second scan skips the unchanged file.
        let again = scan(&app).await.unwrap();
        assert_eq!(again.skipped, 1);
        assert_eq!(again.indexed, 0);

        // Album downloads finish one after another and each one scans. A scan
        // that walked before a file landed must not delete the row a later scan
        // wrote for it (2026-11-05: "Thank God" indexed, then deleted=1).
        let mut scans = Vec::new();
        for i in 0..12 {
            let extra = downloads.join(format!("extra-{i}.opus"));
            std::fs::copy(&song, &extra).unwrap();
            for _ in 0..3 {
                let app = app.clone();
                scans.push(tokio::spawn(async move { scan(&app).await.unwrap() }));
            }
        }
        let deleted: usize = futures::future::join_all(scans)
            .await
            .into_iter()
            .map(|r| r.unwrap().deleted)
            .sum();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(deleted, 0, "a scan deleted a file that is on disk");
        assert_eq!(rows, 13, "every file on disk has a row");

        drop(app);
        drop(pool);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&state_dir);
        sqlx::query(&format!("DROP DATABASE {test_db} WITH (FORCE)"))
            .execute(&admin)
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod walk_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// The downloader writes in-progress files as `.mh-<hex>.opus` and renames
    /// them when complete; bookfetch stages whole books under `.incoming-<hex>/`.
    /// Indexing either gives half-written audio a row in the library. Measured on
    /// the real library before this guard: 8 `.mh-*` tracks, listed as "Unknown
    /// artist" with no title.
    #[test]
    fn hidden_files_and_directories_are_not_indexed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();

        std::fs::write(root.join("real.opus"), b"x").unwrap();
        std::fs::write(root.join(".mh-deadbeef.opus"), b"x").unwrap();

        std::fs::create_dir(root.join(".incoming-abc")).unwrap();
        std::fs::write(root.join(".incoming-abc").join("part.mp3"), b"x").unwrap();

        std::fs::create_dir(root.join("Album")).unwrap();
        std::fs::write(root.join("Album").join("track.mp3"), b"x").unwrap();

        let found: Vec<String> = walk_audio(root)
            .into_iter()
            .map(|f| {
                std::path::Path::new(&f.abs_path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert!(
            found.contains(&"real.opus".to_owned()),
            "lost a real file: {found:?}"
        );
        assert!(
            found.contains(&"track.mp3".to_owned()),
            "lost a nested file: {found:?}"
        );
        assert!(
            !found.iter().any(|n| n.starts_with(".mh-")),
            "indexed an in-progress download: {found:?}"
        );
        assert!(
            !found.contains(&"part.mp3".to_owned()),
            "descended into a staging directory: {found:?}"
        );
        assert_eq!(found.len(), 2, "{found:?}");
    }
}
