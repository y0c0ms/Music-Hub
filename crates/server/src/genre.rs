//! Genre normalisation and the Last.fm enricher that fills `track_genres`.
//!
//! A library of YouTube rips carries no genre tags (497 of 508 tracks here), so
//! grouping by genre was impossible. Last.fm knows what these artists are; this
//! module asks, cleans the free-text answer down to real genres, and stores the
//! result in music-hub's own tables. Nothing is written to the audio files. A
//! faithful port of the Go `internal/genre` package.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;

use mh_core::Config;
use regex::Regex;
use serde_json::Value;

/// Compile a constant pattern; a bad one is a build-time error caught by tests.
#[allow(clippy::expect_used)] // constant patterns
fn compiled(pattern: &str) -> Regex {
    Regex::new(pattern).expect("constant regex pattern is valid")
}

static DECADE: LazyLock<Regex> = LazyLock::new(|| compiled(r"^(19|20)?\d0s$"));
static NUMERIC: LazyLock<Regex> = LazyLock::new(|| compiled(r"^[\d\s]+$"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| compiled(r"\s+"));
static SPLIT_ARTISTS: LazyLock<Regex> = LazyLock::new(|| {
    compiled(r"(?i)\s*(?:,|;|/|\bfeat\.?\b|\bft\.?\b|\bfeaturing\b|\bwith\b|\bvs\.?\b|&| x )\s*")
});

/// Shelving, moods and opinions — what people tag with, not what music is.
static NOT_GENRES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "seen live",
        "favourites",
        "favorites",
        "favourite",
        "favorite",
        "favourite songs",
        "favorite songs",
        "albums i own",
        "vinyl",
        "spotify",
        "youtube",
        "awesome",
        "beautiful",
        "love",
        "love it",
        "amazing",
        "cool",
        "good",
        "great",
        "best",
        "masterpiece",
        "epic",
        "catchy",
        "chill",
        "male vocalists",
        "female vocalists",
        "male vocalist",
        "female vocalist",
        "male",
        "female",
        "vocal",
        "singer",
        "band",
        "music",
        "musica",
        "song",
        "songs",
        "gaming",
        "radio",
        "playlist",
        "mix",
        "my music",
        "my favorites",
        "my favourites",
        "to check out",
        "want to see",
        "heard on pandora",
        "guilty pleasure",
        "sexy",
        "party",
        "summer",
        "driving",
        "workout",
        "study",
        "sleep",
    ]
    .into_iter()
    .collect()
});

/// Canonical names for the same thing said several ways.
static CANONICAL: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    [
        ("hiphop", "hip-hop"),
        ("hip hop", "hip-hop"),
        ("hip-hop", "hip-hop"),
        ("rap", "hip-hop"),
        ("rap music", "hip-hop"),
        ("hip hop rap", "hip-hop"),
        ("hip hop tuga", "hip-hop tuga"),
        ("rap tuga", "hip-hop tuga"),
        ("tuga", "hip-hop tuga"),
        ("rap portugues", "hip-hop tuga"),
        ("portuguese hip hop", "hip-hop tuga"),
        ("rap nacional", "hip-hop tuga"),
        ("portugal", "portuguese"),
        ("portuguese", "portuguese"),
        ("português", "portuguese"),
        ("turkey", "turkish"),
        ("turkish", "turkish"),
        ("türkçe", "turkish"),
        ("turkce", "turkish"),
        ("brazil", "brazilian"),
        ("brasil", "brazilian"),
        ("brazilian", "brazilian"),
        ("alt rock", "alternative rock"),
        ("alternative rock", "alternative rock"),
        ("alt-rock", "alternative rock"),
        ("alternative", "alternative"),
        ("prog rock", "progressive rock"),
        ("progressive rock", "progressive rock"),
        ("prog metal", "progressive metal"),
        ("progressive metal", "progressive metal"),
        ("rnb", "r&b"),
        ("r n b", "r&b"),
        ("r&b", "r&b"),
        ("rhythm and blues", "r&b"),
        ("electronica", "electronic"),
        ("electronic", "electronic"),
        ("edm", "electronic"),
        ("drum and bass", "drum & bass"),
        ("dnb", "drum & bass"),
        ("drum n bass", "drum & bass"),
        ("boombap", "boom bap"),
        ("boom bap", "boom bap"),
        ("lo fi", "lo-fi"),
        ("lofi", "lo-fi"),
        ("lo-fi", "lo-fi"),
        ("indie rock", "indie rock"),
        ("indie", "indie"),
        ("punk rock", "punk"),
        ("punk", "punk"),
        ("heavy metal", "metal"),
        ("metal", "metal"),
        ("pop rock", "pop rock"),
        ("pop", "pop"),
        ("rock", "rock"),
        ("jazz", "jazz"),
        ("blues", "blues"),
        ("soul", "soul"),
        ("funk", "funk"),
        ("reggae", "reggae"),
        ("reggaeton", "reggaeton"),
        ("trap", "trap"),
        ("house", "house"),
        ("techno", "techno"),
        ("ambient", "ambient"),
        ("classical", "classical"),
        ("folk", "folk"),
        ("country", "country"),
        ("grunge", "grunge"),
        ("emo", "emo"),
        ("ska", "ska"),
        ("disco", "disco"),
        ("afrobeat", "afrobeat"),
        ("afrobeats", "afrobeat"),
        ("kizomba", "kizomba"),
    ]
    .into_iter()
    .collect()
});

/// Turn one raw Last.fm tag into a canonical genre, or `""` to drop it.
///
/// `context` holds names the tag must not simply repeat — the artist and the
/// track — because an artist's own name is among the most popular tags on their
/// music and is not a genre.
#[must_use]
pub fn clean(tag: &str, context: &[&str]) -> String {
    let mut t = tag.trim().to_lowercase().replace('_', " ");
    t = SPACES.replace_all(&t, " ").into_owned();
    if t.is_empty() || t.len() > 30 {
        return String::new();
    }
    if DECADE.is_match(&t) || NUMERIC.is_match(&t) {
        return String::new();
    }
    if NOT_GENRES.contains(t.as_str()) {
        return String::new();
    }
    for c in context {
        if !c.trim().is_empty() && t == c.trim().to_lowercase() {
            return String::new();
        }
    }
    if let Some(canon) = CANONICAL.get(t.as_str()) {
        return (*canon).to_owned();
    }
    // Shelving tags carry numbers ("top 100", "3 stars"); genres essentially
    // never do.
    if t.bytes().any(|b| b.is_ascii_digit()) {
        return String::new();
    }
    // Unknown but plausible: a one- or two-space tag is usually a real sub-genre.
    if t.matches(' ').count() <= 2 {
        return t;
    }
    String::new()
}

/// Clean, merge and rank raw weighted tags, returning at most `n` genres.
///
/// Merging matters: "hip hop", "Hip-Hop" and "hiphop" are one genre with a
/// combined weight, and ranking them apart would push the real second off.
#[must_use]
pub fn top(raw: &[(String, i32)], n: usize, context: &[&str]) -> Vec<String> {
    let mut merged: HashMap<String, i32> = HashMap::new();
    for (name, count) in raw {
        let c = clean(name, context);
        if c.is_empty() {
            continue;
        }
        let entry = merged.entry(c).or_insert(0);
        if *count > *entry {
            *entry = *count;
        }
    }
    let mut out: Vec<(String, i32)> = merged.into_iter().collect();
    // Weight first, then name, so the result is stable across runs.
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.into_iter().take(n).map(|(name, _)| name).collect()
}

/// The lead artist from a credit string.
///
/// A third of this library's artist fields are the whole credit list in one
/// value, which Last.fm cannot match; asking about the first name gets an
/// answer and is the right one for a collaboration anyway.
#[must_use]
pub fn primary_artist(artist: &str) -> String {
    SPLIT_ARTISTS
        .split(artist.trim())
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// The artist names worth asking Last.fm about, in order, without repeats: the
/// full credit then its lead artist.
fn candidates(artist: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    let full = artist.trim().to_owned();
    if !full.is_empty() {
        out.push(full);
    }
    let primary = primary_artist(artist);
    if !primary.is_empty() && !out.iter().any(|c| c.eq_ignore_ascii_case(&primary)) {
        out.push(primary);
    }
    out
}

// ------------------------------------------------------------------ enricher

const ENRICH_BATCH: i64 = 40;
const LASTFM_BASE: &str = "https://ws.audioscrobbler.com/2.0/";
const USER_AGENT: &str = "music-hub/1.0 (self-hosted)";

/// Classify one batch of not-yet-classified tracks, writing their genres.
/// Best-effort: a Last.fm miss on one track never stops the batch. Returns the
/// number of tracks that gained at least one genre.
///
/// # Errors
/// Returns [`mh_db::DbError`] only if the initial "what is unclassified" query
/// fails; per-track failures are swallowed.
pub async fn enrich_once(
    pool: &mh_db::Pool,
    config: &Config,
    http: &reqwest::Client,
) -> Result<usize, mh_db::DbError> {
    if config.lastfm_api_key.is_empty() {
        return Ok(0);
    }
    let tracks = mh_db::genre::unclassified(pool, ENRICH_BATCH).await?;
    let mut classified = 0;
    for (id, artist, title) in tracks {
        let genres = classify(pool, config, http, &artist, &title).await;
        if !genres.is_empty() {
            if let Err(e) = mh_db::genre::set_track_genres(pool, id, &genres).await {
                tracing::warn!(track = %id, error = %e, "genre: could not store track genres");
            } else {
                classified += 1;
            }
        }
    }
    Ok(classified)
}

/// Spawn a background loop that classifies a batch of tracks every 5 minutes.
/// A no-op when no Last.fm key is configured.
pub fn run_enricher(pool: mh_db::Pool, config: Arc<Config>) {
    if config.lastfm_api_key.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let http = crate::discovery::https_only_client();
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            match enrich_once(&pool, &config, &http).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(classified = n, "genre enrichment batch"),
                Err(e) => tracing::warn!(error = %e, "genre enrichment"),
            }
        }
    });
}

async fn classify(
    pool: &mh_db::Pool,
    config: &Config,
    http: &reqwest::Client,
    artist: &str,
    title: &str,
) -> Vec<String> {
    let context = [artist, title];
    for cand in candidates(artist) {
        // The cache is the whole point: 508 tracks come from 412 artists.
        match mh_db::genre::artist_genres(pool, &cand).await {
            Ok(Some(cached)) => {
                if !cached.is_empty() {
                    return cached;
                }
                continue; // known to have no genres; try the next candidate
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(artist = %cand, error = %e, "genre: cache read"),
        }
        let raw = artist_top_tags(http, &config.lastfm_api_key, &cand).await;
        let genres = top(&raw, 5, &context);
        if let Err(e) = mh_db::genre::set_artist_genres(pool, &cand, &genres).await {
            tracing::warn!(artist = %cand, error = %e, "genre: cache write");
        }
        if !genres.is_empty() {
            return genres;
        }
    }
    Vec::new()
}

/// Last.fm `artist.gettoptags`. A "no such artist" body error or an empty tag
/// list both come back as an empty slice — neither stops a 500-track run.
async fn artist_top_tags(http: &reqwest::Client, key: &str, artist: &str) -> Vec<(String, i32)> {
    let url = format!(
        "{LASTFM_BASE}?method=artist.gettoptags&artist={}&api_key={key}&format=json&autocorrect=1",
        urlencode(artist)
    );
    let Ok(resp) = http
        .get(&url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(12))
        .send()
        .await
    else {
        return Vec::new();
    };
    let Ok(body) = resp.json::<Value>().await else {
        return Vec::new();
    };
    parse_top_tags(&body)
}

/// Pull `(name, count)` pairs from a Last.fm `toptags` document. Pure, so the
/// awkward single-vs-array shape is tested without a network.
fn parse_top_tags(body: &Value) -> Vec<(String, i32)> {
    let tags = &body["toptags"]["tag"];
    let items = match tags {
        Value::Array(a) => a.clone(),
        Value::Object(_) => vec![tags.clone()],
        _ => return Vec::new(),
    };
    items
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.trim().to_owned();
            if name.is_empty() {
                return None;
            }
            let count = flex_int(t.get("count"));
            Some((name, count))
        })
        .collect()
}

/// Last.fm sends counts as a number or a numeric string; both become an i32.
fn flex_int(v: Option<&Value>) -> i32 {
    match v {
        Some(Value::Number(n)) => {
            i32::try_from(n.as_i64().unwrap_or(0).clamp(0, i64::from(i32::MAX))).unwrap_or(0)
        }
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Percent-encode a query parameter (unreserved kept, space -> `+`).
fn urlencode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn clean_drops_shelving_decades_and_numbers() {
        assert_eq!(clean("Hip-Hop", &[]), "hip-hop");
        assert_eq!(clean("rap", &[]), "hip-hop"); // canonical
        assert_eq!(clean("seen live", &[]), "");
        assert_eq!(clean("2010s", &[]), "");
        assert_eq!(clean("top 100", &[]), "");
        assert_eq!(clean("doom metal", &[]), "doom metal"); // plausible sub-genre kept
        assert_eq!(clean("soundtrack", &[]), "soundtrack"); // NOT blocked
    }

    #[test]
    fn clean_drops_a_tag_that_is_just_the_artist_name() {
        assert_eq!(clean("Coldplay", &["Coldplay", "Yellow"]), "");
    }

    #[test]
    fn top_merges_variants_and_ranks_by_weight() {
        let raw = vec![
            ("hip hop".to_owned(), 17),
            ("Hip-Hop".to_owned(), 100),
            ("rock".to_owned(), 40),
            ("seen live".to_owned(), 500),
        ];
        let out = top(&raw, 3, &[]);
        assert_eq!(out, vec!["hip-hop", "rock"]); // shelving dropped, variants merged
    }

    #[test]
    fn primary_artist_takes_the_lead_from_a_credit_list() {
        assert_eq!(primary_artist("LEALL, Rock Danger, Babidi"), "LEALL");
        assert_eq!(primary_artist("J. Cole feat. PJ"), "J. Cole");
        assert_eq!(primary_artist("A x B"), "A");
    }

    #[test]
    fn candidates_are_full_then_lead_without_repeats() {
        assert_eq!(candidates("Drake"), vec!["Drake"]);
        assert_eq!(
            candidates("Drake feat. Future"),
            vec!["Drake feat. Future", "Drake"]
        );
    }

    #[test]
    fn parse_top_tags_handles_array_and_single_object() {
        let arr = serde_json::json!({"toptags":{"tag":[
            {"name":"rock","count":100},{"name":"","count":5},{"name":"pop","count":"50"}]}});
        assert_eq!(
            parse_top_tags(&arr),
            vec![("rock".to_owned(), 100), ("pop".to_owned(), 50)]
        );
        let one = serde_json::json!({"toptags":{"tag":{"name":"jazz","count":7}}});
        assert_eq!(parse_top_tags(&one), vec![("jazz".to_owned(), 7)]);
        let empty = serde_json::json!({"toptags":{"tag":[]}});
        assert!(parse_top_tags(&empty).is_empty());
    }
}
