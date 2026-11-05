//! The wire contract shared by every handler and the Svelte PWA.
//!
//! This is a faithful port of the Go `internal/model` package: the frontend in
//! `web/src/lib/api.js` is written against exactly these shapes. Field names use
//! `camelCase` and Go's `omitempty` is reproduced with `skip_serializing_if` so
//! the JSON emitted here is byte-identical to what the Go server produced.

use serde::{Deserialize, Serialize};

/// serde `skip_serializing_if` predicates reproducing Go's `omitempty`, which
/// omits zero-valued scalars. They must take `&T` to match serde's signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
mod skip {
    pub const fn i32_zero(n: &i32) -> bool {
        *n == 0
    }
    pub const fn i64_zero(n: &i64) -> bool {
        *n == 0
    }
    pub const fn is_false(b: &bool) -> bool {
        !*b
    }
}

/// The identity the PWA renders in the header.
///
/// `navidrome_linked` is retained for wire compatibility with the existing PWA
/// (which gates its link screen on it); the Rust backend is the library itself,
/// so `/api/me` always reports it `true` and the link screen never shows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub username: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub navidrome_user: String,
    pub navidrome_linked: bool,
}

/// One playable item. IDs are library track ids; discovery candidates not yet
/// downloaded carry a source id and are marked `kind == "candidate"`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub album: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub album_id: String,
    /// seconds
    pub duration: i32,
    #[serde(default, skip_serializing_if = "skip::i32_zero")]
    pub year: i32,
    #[serde(default, skip_serializing_if = "skip::is_false")]
    pub starred: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stream_url: String,
    /// Set when this track is a chapter of an audiobook; empty for music.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub book_id: String,
}

/// Groups tracks for the library browser.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artist_id: String,
    pub song_count: i32,
    #[serde(default, skip_serializing_if = "skip::i32_zero")]
    pub year: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover_url: String,
}

/// A library browse entry for one artist.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub album_count: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover_url: String,
}

/// A saved playlist.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner: String,
    pub song_count: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover_url: String,
}

/// The unified answer to one query: what the library has, plus what could be
/// fetched from the internet. All four arrays are always present (never null).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub acquirable: Vec<Acquire>,
}

/// A downloadable result that is NOT in the library yet.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Acquire {
    /// yt-dlp video id
    pub source_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artist: String,
    pub duration: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thumb: String,
}

/// A whole album found online that is NOT in the library yet. `playlist_id` is
/// YouTube Music's stable `OLAK5uy_...` album-playlist id.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireAlbum {
    pub playlist_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artist: String,
    pub track_count: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thumb: String,
    #[serde(default, skip_serializing_if = "skip::i64_zero")]
    pub views: i64,
    #[serde(default, skip_serializing_if = "skip::i32_zero")]
    pub duration_sec: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Acquire>,
}

/// Progress of a download so the PWA can show it live.
/// `state`: `queued | downloading | done | failed`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub title: String,
    pub state: String,
    /// The finished track's library id, once indexed. Present => playable now.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub song_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// One card in the swipe deck. `kind` is load-bearing: `owned` (on disk, a left
/// swipe deletes) vs `candidate` (streamed preview, a right swipe downloads).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCard {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub album: String,
    pub duration: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover_url: String,
    pub preview_url: String,
    /// why it was suggested
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
}

/// Extra detail for the top discovery card; each field needs a full yt-dlp
/// extraction so it is fetched one card at a time.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMeta {
    /// `YYYY-MM-DD`
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub posted_at: String,
    #[serde(default, skip_serializing_if = "skip::i64_zero")]
    pub views: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
}

/// What actually happened on a swipe. `action`: `deleted | queued | skipped | kept`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwipeResult {
    pub ok: bool,
    pub action: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// One named stretch of a book. Carries a track id AND an offset range so the
/// client need not care whether the book is one long file or a file per chapter.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chapter {
    pub title: String,
    pub track_id: String,
    pub start_sec: i32,
    pub end_sec: i32,
}

/// Where one person got to in one book. `updated_at` is ms since epoch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookProgress {
    pub track_id: String,
    pub position_sec: i32,
    pub updated_at: i64,
}

/// One audiobook: an ordered thing you are partway through.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Book {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover_url: String,
    pub chapter_count: i32,
    pub duration_sec: i32,
    /// derived from progress, for the shelf's progress bar
    pub listened_sec: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<BookProgress>,
    pub speed: f64,
    pub sleep_minutes: i32,
    /// omitted from the shelf listing, present on the detail
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<Chapter>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use serde_json::{json, Value};

    fn value_of<T: Serialize>(v: &T) -> Value {
        serde_json::to_value(v).unwrap()
    }

    #[test]
    fn track_omits_zero_and_empty_fields() {
        let t = Track {
            id: "t1".into(),
            title: "Song".into(),
            artist: "Band".into(),
            duration: 210,
            ..Default::default()
        };
        // Only non-omitempty and set fields appear; camelCase keys.
        assert_eq!(
            value_of(&t),
            json!({"id":"t1","title":"Song","artist":"Band","duration":210})
        );
    }

    #[test]
    fn track_full_uses_camel_case_keys() {
        let t = Track {
            id: "t1".into(),
            title: "Song".into(),
            artist: "Band".into(),
            album: "Alb".into(),
            album_id: "a1".into(),
            duration: 210,
            year: 1999,
            starred: true,
            cover_url: "/api/cover/t1".into(),
            stream_url: "/api/stream/t1".into(),
            book_id: "b1".into(),
        };
        assert_eq!(
            value_of(&t),
            json!({
                "id":"t1","title":"Song","artist":"Band","album":"Alb","albumId":"a1",
                "duration":210,"year":1999,"starred":true,
                "coverUrl":"/api/cover/t1","streamUrl":"/api/stream/t1","bookId":"b1"
            })
        );
    }

    #[test]
    fn user_always_reports_linked_flag() {
        let u = User {
            username: "sam".into(),
            display_name: "Sam".into(),
            navidrome_user: String::new(),
            navidrome_linked: false,
        };
        assert_eq!(
            value_of(&u),
            json!({"username":"sam","displayName":"Sam","navidromeLinked":false})
        );
    }

    #[test]
    fn search_results_arrays_are_always_present() {
        let r = SearchResults::default();
        assert_eq!(
            value_of(&r),
            json!({"tracks":[],"albums":[],"artists":[],"acquirable":[]})
        );
    }

    #[test]
    fn album_and_artist_counts_are_always_present() {
        assert_eq!(
            value_of(&Album {
                id: "a1".into(),
                name: "Alb".into(),
                artist: "Band".into(),
                song_count: 0,
                ..Default::default()
            }),
            json!({"id":"a1","name":"Alb","artist":"Band","songCount":0})
        );
        assert_eq!(
            value_of(&Artist {
                id: "ar1".into(),
                name: "Band".into(),
                album_count: 0,
                ..Default::default()
            }),
            json!({"id":"ar1","name":"Band","albumCount":0})
        );
    }

    #[test]
    fn book_serializes_scalar_fields_and_optional_progress() {
        let b = Book {
            id: "bk1".into(),
            title: "Book".into(),
            chapter_count: 3,
            duration_sec: 3600,
            listened_sec: 60,
            speed: 1.0,
            sleep_minutes: 0,
            ..Default::default()
        };
        assert_eq!(
            value_of(&b),
            json!({
                "id":"bk1","title":"Book","chapterCount":3,"durationSec":3600,
                "listenedSec":60,"speed":1.0,"sleepMinutes":0
            })
        );
        let with_progress = Book {
            progress: Some(BookProgress {
                track_id: "t9".into(),
                position_sec: 500,
                updated_at: 1_700_000_000_000,
            }),
            ..b
        };
        let v = value_of(&with_progress);
        assert_eq!(v["progress"]["trackId"], json!("t9"));
        assert_eq!(v["progress"]["positionSec"], json!(500));
        assert_eq!(v["progress"]["updatedAt"], json!(1_700_000_000_000_i64));
    }

    #[test]
    fn track_round_trips_through_json() {
        let t = Track {
            id: "t1".into(),
            title: "Song".into(),
            artist: "Band".into(),
            duration: 210,
            year: 1999,
            ..Default::default()
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: Track = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }
}
