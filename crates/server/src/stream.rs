//! Audio streaming and cover art.
//!
//! Both are served straight off disk with `tower_http::services::ServeFile`,
//! which handles HTTP Range/If-Range/ETag natively — seeking and background
//! playback depend on the browser being able to ask for a byte range, and
//! nothing is buffered. Covers are the JPEGs the scanner extracted to
//! `<STATE_DIR>/covers/<track_id>.jpg`; a cover id may be a track, album, artist,
//! or playlist id, resolved to a representative track that has art.
use std::path::Path as FsPath;

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

use crate::auth::Session;
use crate::{AppError, AppResult, AppState};

fn parse_id(raw: &str) -> AppResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| AppError::NotFound)
}

async fn serve(file: impl AsRef<FsPath>, request: Request) -> Response {
    match ServeFile::new(file).oneshot(request).await {
        Ok(response) => response.map(axum::body::Body::new).into_response(),
        // ServeFile is infallible; this arm is unreachable and takes no value.
        Err(never) => match never {},
    }
}

/// `GET /api/stream/{id}` — the audio file, Range-aware, no transcoding.
///
/// # Errors
/// Returns [`AppError::NotFound`] if the id is invalid or the track is unknown
/// or not visible to this user, or [`AppError`] on a database failure.
pub async fn stream(
    State(state): State<AppState>,
    Extension(session): Extension<Session>,
    Path(id): Path<String>,
    request: Request,
) -> AppResult<Response> {
    let id = parse_id(&id)?;
    let path = mh_db::browse::track_path(&state.pool, &session.username, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(serve(path, request).await)
}

/// Extract `src`'s embedded cover to `dest`, atomically and best-effort.
///
/// Writes to a unique temporary file in the same directory and renames it into
/// place: a rename within one filesystem is atomic, so a second request can
/// never observe a half-written JPEG. Two requests racing on the same cover
/// duplicate the ffmpeg work and one rename wins, which is cheap and correct -
/// the alternative is a lock held across a subprocess.
///
/// Failure is deliberately silent. A file whose art cannot be decoded should
/// serve a 404 from the missing cache entry, exactly as before, rather than turn
/// a cover request into a 500.
async fn ensure_cover(dest: &FsPath, src: &FsPath) {
    let Some(dir) = dest.parent() else { return };
    if tokio::fs::create_dir_all(dir).await.is_err() {
        return;
    }
    // THE TEMP NAME MUST STILL END IN .jpg. ffmpeg picks its muxer from the
    // output EXTENSION, so writing to `.<name>.<pid>.tmp` fails with "unable to
    // find a suitable output format" and every cover silently 404s - which is
    // exactly what shipped in the first cut of this function. The failure looked
    // identical to "this file has no artwork", and the unit test passed because
    // it only exercised the failure path.
    //
    // The pid keeps concurrent extractions of the SAME cover off one another's
    // temp file; without it they would interleave writes into one path and the
    // rename could publish a truncated image. The leading dot keeps a
    // half-written file from looking like a finished cover to anything listing
    // the directory.
    let tmp = dir.join(format!(
        ".{}.{}.tmp.jpg",
        dest.file_stem().and_then(|s| s.to_str()).unwrap_or("cover"),
        std::process::id()
    ));
    if mh_media::ffprobe::extract_cover(src, &tmp).await.is_ok() {
        let _ = tokio::fs::rename(&tmp, dest).await;
    }
    // Covers the failure path and a rename that lost the race.
    let _ = tokio::fs::remove_file(&tmp).await;
}

/// `GET /api/cover/{id}` — cover art for a track/album/artist/playlist id,
/// served from the cache and extracted on demand when the cache is cold.
///
/// # Errors
/// Returns [`AppError::NotFound`] if the id is invalid or no art matches, or
/// [`AppError`] on a database failure.
pub async fn cover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: Request,
) -> AppResult<Response> {
    let id = parse_id(&id)?;
    let (track, abs_path) = mh_db::browse::cover_track_id(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let file = FsPath::new(&state.config.state_dir)
        .join("covers")
        .join(format!("{track}.jpg"));
    // Fill the cache if it is cold. Covers were only ever written by the
    // SCANNER, and the scanner skips unchanged files - so once the cache was
    // lost, every cover 404ed permanently and the only cure was clearing every
    // stored mtime to force a full re-probe.
    //
    // That is not hypothetical: the QA overlay mounts /state as an emptyDir, so
    // all 600 covers vanished on each of three pod rolls while `has_cover`
    // stayed true in the database. `has_cover` describes the FILE, which has not
    // changed; the jpg beside it is a derived artifact and belongs on the
    // recompute-on-miss path, not the reindex path.
    if !file.exists() {
        ensure_cover(&file, FsPath::new(&abs_path)).await;
    }
    let mut response = serve(file, request).await;
    // Artwork is immutable for a given id, so a long cache keeps the album grid
    // and the deck from re-fetching on every scroll.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    Ok(response)
}

#[cfg(test)]
mod cover_cache_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::ensure_cover;
    use std::path::Path as FsPath;

    #[tokio::test]
    async fn a_source_with_no_cover_leaves_nothing_behind() {
        // The important half is the temp file. Extraction failing is normal - not
        // every file has art - but a `.jpg.<pid>.tmp` left in the cache directory
        // on every miss accumulates one turd per request forever, and the next
        // reader cannot tell it from a real cover.
        let dir = std::env::temp_dir().join("mh-cover-cache-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("deadbeef.jpg");

        ensure_cover(&dest, FsPath::new("/nonexistent/track.opus")).await;

        assert!(!dest.exists(), "no cover should have been published");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real audio file carrying real artwork, so the SUCCESS path is exercised.
    ///
    /// This is the test that was missing. The first version of `ensure_cover`
    /// wrote its temp file as `.<name>.<pid>.tmp`, and ffmpeg selects its muxer
    /// from the output EXTENSION - so every extraction failed and every cover
    /// 404ed. Only the failure path was tested, so it passed.
    async fn fixture_with_cover(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let png = dir.join("art.png");
        let flac = dir.join("song.flac");
        let ok = tokio::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=64x64:d=1",
                "-frames:v",
                "1",
            ])
            .arg(&png)
            .status()
            .await
            .ok()?
            .success();
        if !ok {
            return None; // no ffmpeg in this environment
        }
        let ok = tokio::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-i",
            ])
            .arg(&png)
            .args([
                "-map",
                "0:a",
                "-map",
                "1:v",
                "-c:a",
                "flac",
                "-c:v",
                "mjpeg",
                "-disposition:v:0",
                "attached_pic",
            ])
            .arg(&flac)
            .status()
            .await
            .ok()?
            .success();
        ok.then_some(flac)
    }

    #[tokio::test]
    async fn a_real_cover_is_published_and_no_temp_file_survives() {
        let dir = std::env::temp_dir().join("mh-cover-success-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Some(src) = fixture_with_cover(&dir).await else {
            return; // ffmpeg unavailable; nothing to assert
        };
        let dest = dir.join("covers").join("abcdef.jpg");

        ensure_cover(&dest, &src).await;

        assert!(dest.exists(), "cover was not published");
        assert!(
            std::fs::metadata(&dest).unwrap().len() > 0,
            "published cover is empty"
        );
        let strays: Vec<_> = std::fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp.jpg"))
            .collect();
        assert!(strays.is_empty(), "temp files left behind: {strays:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_cache_directory_is_created_rather_than_fatal() {
        // The cold-start case this exists for: an emptyDir /state comes up with
        // no covers/ directory at all, and the first request must not fail on
        // that instead of on the extraction.
        let dir = std::env::temp_dir().join("mh-cover-cache-mkdir/covers");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        ensure_cover(&dir.join("x.jpg"), FsPath::new("/nonexistent/track.opus")).await;
        assert!(dir.exists(), "covers/ should have been created");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::StatusCode;
    use mh_db::library::NewTrack;
    use sqlx::postgres::PgPoolOptions;

    fn session() -> Session {
        Session {
            user_id: Uuid::nil(),
            username: "sam".to_owned(),
            display_name: "Sam".to_owned(),
        }
    }

    async fn seeded() -> Option<(mh_db::Pool, String, Uuid, std::path::PathBuf)> {
        let admin_url = std::env::var("DATABASE_URL").ok()?;
        let (base, _) = admin_url.rsplit_once('/')?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let test_db = format!("mh_stream_{nanos}");
        let admin = PgPoolOptions::new()
            .connect(&format!("{base}/postgres"))
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE {test_db}"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .connect(&format!("{base}/{test_db}"))
            .await
            .unwrap();
        mh_db::migrate(&pool).await.unwrap();

        let dir = std::env::temp_dir().join(format!("mh-stream-{nanos}"));
        std::fs::create_dir_all(dir.join("covers")).unwrap();
        let audio = dir.join("song.opus");
        std::fs::write(&audio, vec![7u8; 1000]).unwrap();

        let lib = mh_db::library::get_or_create_library(
            &pool,
            dir.to_str().unwrap(),
            "music",
            "shared",
            "",
        )
        .await
        .unwrap();
        let abs = audio.to_str().unwrap().to_owned();
        let id = mh_core::ids::track_id(&abs);
        let track = NewTrack {
            id,
            library_id: lib,
            rel_path: "song.opus".to_owned(),
            abs_path: abs,
            title: "Song".to_owned(),
            artist: "Band".to_owned(),
            artist_id: mh_core::ids::artist_id("Band"),
            album_artist: "Band".to_owned(),
            album: "Alb".to_owned(),
            album_id: Some(mh_core::ids::album_id("Band", "Alb")),
            track_no: 1,
            disc_no: 0,
            year: 2020,
            genre: String::new(),
            duration_sec: 1,
            size_bytes: 1000,
            has_cover: true,
            is_book: false,
            mtime: None,
        };
        mh_db::library::upsert_track(&pool, &track).await.unwrap();
        std::fs::write(dir.join("covers").join(format!("{id}.jpg")), vec![9u8; 40]).unwrap();
        Some((pool, test_db, id, dir))
    }

    fn state(pool: mh_db::Pool, dir: &std::path::Path) -> AppState {
        use std::collections::HashMap;
        let env: HashMap<String, String> = [
            ("OIDC_ISSUER", "https://kc"),
            ("OIDC_CLIENT_SECRET", "s"),
            ("DATABASE_URL", "postgres://x/y"),
            ("STATE_DIR", dir.to_str().unwrap()),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        let cfg = mh_core::Config::load(|k| env.get(k).cloned()).unwrap();
        AppState::for_test(cfg, pool)
    }

    async fn drop_db(name: &str) {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if let Some((base, _)) = url.rsplit_once('/') {
                if let Ok(admin) = PgPoolOptions::new()
                    .connect(&format!("{base}/postgres"))
                    .await
                {
                    let _ = sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
                        .execute(&admin)
                        .await;
                }
            }
        }
    }

    #[tokio::test]
    async fn stream_serves_a_byte_range() {
        let Some((pool, db, id, dir)) = seeded().await else {
            return;
        };
        let st = state(pool, &dir);
        let request = Request::builder()
            .uri("/")
            .header(header::RANGE, "bytes=0-99")
            .body(Body::empty())
            .unwrap();
        let resp = stream(
            State(st),
            Extension(session()),
            Path(id.to_string()),
            request,
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert!(resp.headers().contains_key(header::CONTENT_RANGE));
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        assert_eq!(bytes.len(), 100);

        let _ = std::fs::remove_dir_all(&dir);
        drop_db(&db).await;
    }

    #[tokio::test]
    async fn cover_serves_cached_jpeg_with_long_cache() {
        let Some((pool, db, id, dir)) = seeded().await else {
            return;
        };
        let st = state(pool, &dir);
        let request = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = cover(State(st), Path(id.to_string()), request)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=86400"
        );
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        assert_eq!(bytes.len(), 40);

        let _ = std::fs::remove_dir_all(&dir);
        drop_db(&db).await;
    }
}
