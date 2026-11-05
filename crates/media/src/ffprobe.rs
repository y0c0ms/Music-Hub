//! Reading tags, duration, and embedded-cover presence with `ffprobe`, and
//! extracting a track's cover to a JPEG with `ffmpeg`.
//!
//! The invocations match the ones the Go acquire tests use. Cover art surfaces
//! as an `attached_pic` video stream for every container the library holds,
//! including Opus (mutagen writes a `METADATA_BLOCK_PICTURE`, which ffmpeg reads
//! back as a png/mjpeg attached picture) — verified against real fixtures.

use std::collections::HashMap;
use std::path::Path;

use tokio::process::Command;

use crate::MediaError;

/// Tags, duration, and cover-presence for one audio file.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub duration_sec: i32,
    /// Tag keys are lower-cased; the first value seen for a key wins.
    pub tags: HashMap<String, String>,
    pub has_cover: bool,
}

impl Probe {
    #[must_use]
    pub fn tag(&self, key: &str) -> &str {
        self.tags.get(key).map_or("", String::as_str)
    }
    #[must_use]
    pub fn title(&self) -> &str {
        self.tag("title")
    }
    #[must_use]
    pub fn artist(&self) -> &str {
        self.tag("artist")
    }
    #[must_use]
    pub fn album(&self) -> &str {
        self.tag("album")
    }
    #[must_use]
    pub fn album_artist(&self) -> &str {
        let a = self.tag("albumartist");
        if a.is_empty() {
            self.tag("album_artist")
        } else {
            a
        }
    }
    #[must_use]
    pub fn genre(&self) -> &str {
        self.tag("genre")
    }
    #[must_use]
    pub fn track_no(&self) -> i32 {
        let t = self.tag("track");
        first_int(if t.is_empty() {
            self.tag("tracknumber")
        } else {
            t
        })
    }
    #[must_use]
    pub fn disc_no(&self) -> i32 {
        let d = self.tag("disc");
        first_int(if d.is_empty() {
            self.tag("discnumber")
        } else {
            d
        })
    }
    #[must_use]
    pub fn year(&self) -> i32 {
        let d = self.tag("date");
        first_int(if d.is_empty() { self.tag("year") } else { d })
    }
}

/// Probe one audio file for duration, tags, and embedded-cover presence.
///
/// # Errors
/// Returns [`MediaError`] if any ffprobe invocation fails to launch or run.
pub async fn probe(path: &Path) -> Result<Probe, MediaError> {
    Ok(Probe {
        duration_sec: probe_duration(path).await?,
        tags: probe_tags(path).await?,
        has_cover: probe_has_cover(path).await?,
    })
}

/// Extract the embedded cover to `dest` as JPEG. Transcodes (`-c:v mjpeg`) rather
/// than copies so png covers (the Opus case) become a valid JPEG too.
///
/// # Errors
/// Returns [`MediaError`] if ffmpeg fails (e.g. the file has no cover).
pub async fn extract_cover(src: &Path, dest: &Path) -> Result<(), MediaError> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-an", "-c:v", "mjpeg", "-frames:v", "1"])
        .arg(dest)
        .output()
        .await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(MediaError::Tool {
            tool: "ffmpeg",
            message: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

async fn ffprobe(args: &[&str], path: &Path) -> Result<String, MediaError> {
    let out = Command::new("ffprobe")
        .args(args)
        .arg(path)
        .output()
        .await?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(MediaError::Tool {
            tool: "ffprobe",
            message: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
async fn probe_duration(path: &Path) -> Result<i32, MediaError> {
    let out = ffprobe(
        &[
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ],
        path,
    )
    .await?;
    // Durations comfortably fit i32; rounding a small positive float is safe.
    Ok(out.trim().parse::<f64>().unwrap_or(0.0).round().max(0.0) as i32)
}

async fn probe_tags(path: &Path) -> Result<HashMap<String, String>, MediaError> {
    let out = ffprobe(
        &[
            "-v",
            "error",
            // AUDIO STREAM ONLY. Without this, a file with embedded artwork has a
            // second (mjpeg) stream whose own tags ffprobe emits FIRST:
            //
            //   TAG:title=Album cover      <- the picture's title
            //   TAG:title=Papel e tinta    <- the real one
            //
            // and the first-wins merge below then stored "Album cover" as the
            // track title for every MP3 that carries a cover. Measured on this
            // library: 375 of 419 albums were single-track wrecks because of it.
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream_tags:format_tags",
            "-of",
            "default=noprint_wrappers=1",
        ],
        path,
    )
    .await?;
    let mut tags = HashMap::new();
    for line in out.lines() {
        // ffprobe emits `TAG:KEY=value`; the prefix varies in case.
        let line = line.strip_prefix("TAG:").unwrap_or(line);
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_lowercase();
            if !key.is_empty() {
                tags.entry(key).or_insert_with(|| value.trim().to_owned());
            }
        }
    }
    Ok(tags)
}

async fn probe_has_cover(path: &Path) -> Result<bool, MediaError> {
    let out = ffprobe(
        &[
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "stream_disposition=attached_pic",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ],
        path,
    )
    .await?;
    Ok(out.lines().any(|l| l.trim() == "1"))
}

fn first_int(s: &str) -> i32 {
    let digits: String = s.trim().chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::process::Command as StdCommand;

    fn have(tool: &str) -> bool {
        StdCommand::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn tmpdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mh-probe-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn first_int_takes_leading_digits() {
        assert_eq!(first_int("3/12"), 3);
        assert_eq!(first_int("1995-06-01"), 1995);
        assert_eq!(first_int(" 07 "), 7);
        assert_eq!(first_int("none"), 0);
        assert_eq!(first_int(""), 0);
    }

    #[tokio::test]
    async fn probes_opus_tags_and_duration() {
        if !have("ffmpeg") || !have("ffprobe") {
            return;
        }
        let dir = tmpdir();
        let opus = dir.join("t.opus");
        assert!(StdCommand::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("anullsrc=r=48000:cl=mono")
            .args(["-t", "1", "-c:a", "libopus"])
            .args(["-metadata", "TITLE=Lemon Tree"])
            .args(["-metadata", "ARTIST=Fools Garden"])
            .args(["-metadata", "ALBUM=Dish"])
            .args(["-metadata", "DATE=1995"])
            .args(["-metadata", "track=3"])
            .arg(&opus)
            .status()
            .unwrap()
            .success());

        let p = probe(&opus).await.unwrap();
        assert_eq!(p.title(), "Lemon Tree");
        assert_eq!(p.artist(), "Fools Garden");
        assert_eq!(p.album(), "Dish");
        assert_eq!(p.year(), 1995);
        assert_eq!(p.track_no(), 3);
        assert_eq!(p.duration_sec, 1);
        assert!(!p.has_cover);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detects_and_extracts_flac_cover() {
        if !have("ffmpeg") || !have("ffprobe") {
            return;
        }
        let dir = tmpdir();
        let png = dir.join("c.png");
        let flac = dir.join("c.flac");
        assert!(StdCommand::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=red:s=32x32")
            .args(["-frames:v", "1"])
            .arg(&png)
            .status()
            .unwrap()
            .success());
        assert!(StdCommand::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("anullsrc=r=44100:cl=stereo")
            .arg("-i")
            .arg(&png)
            .args(["-map", "0:a", "-map", "1:v", "-t", "1", "-shortest"])
            .args([
                "-c:a",
                "flac",
                "-c:v",
                "mjpeg",
                "-disposition:v:0",
                "attached_pic"
            ])
            .arg(&flac)
            .status()
            .unwrap()
            .success());

        let p = probe(&flac).await.unwrap();
        assert!(p.has_cover);

        let jpg = dir.join("out.jpg");
        extract_cover(&flac, &jpg).await.unwrap();
        assert!(std::fs::metadata(&jpg).unwrap().len() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
