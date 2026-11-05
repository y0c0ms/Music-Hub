//! Release-year enrichment from MusicBrainz.
//!
//! A library of YouTube rips rarely carries a release year, so albums show no
//! date. MusicBrainz knows release dates for real records; this sweeps albums
//! with no year, asks MusicBrainz by artist + title, and stores the result in
//! `album_meta` - never touching the audio files, so a re-probe cannot wipe it.
//! Best-effort: a miss records "checked, none found" so it is not re-queried.

use std::time::Duration;

use reqwest::header;
use serde_json::Value;

/// MusicBrainz requires a descriptive User-Agent naming a contact URL.
const USER_AGENT: &str = "music-hub/1.0 ( https://github.com/y0c0ms/Music-Hub )";
/// MusicBrainz asks for at most one request per second.
const RATE: Duration = Duration::from_secs(1);
/// Albums dated per sweep, so a first run does not hammer MusicBrainz for hours.
const BATCH: i64 = 30;

/// Spawn the background release-year enricher.
///
/// Sweeps un-dated albums, asks MusicBrainz, and caches the answer in
/// `album_meta`. Each album is checked at most once because a checked album gets
/// a row (including a "none found" 0).
pub fn run_year_enricher(pool: mh_db::Pool) {
    tokio::spawn(async move {
        let http = mh_core::tls::client_builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        let mut tick = tokio::time::interval(Duration::from_secs(120));
        loop {
            tick.tick().await;
            match enrich_once(&pool, &http).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(dated = n, "release-year enrichment batch"),
                Err(e) => tracing::warn!(error = %e, "release-year enrichment"),
            }
        }
    });
}

/// One sweep: date up to [`BATCH`] albums, spaced to respect the rate limit.
/// Returns how many got a real year; a "none found" still records a row.
async fn enrich_once(pool: &mh_db::Pool, http: &reqwest::Client) -> Result<usize, mh_db::DbError> {
    let albums = mh_db::browse::albums_missing_year(pool, BATCH).await?;
    let mut dated = 0;
    for (album_id, artist, title) in albums {
        tokio::time::sleep(RATE).await;
        match musicbrainz_year(http, &artist, &title).await {
            YearLookup::Found(year) => {
                mh_db::browse::set_album_year(pool, album_id, year).await?;
                dated += 1;
            }
            // Answered, but no date exists - record 0 so it is not re-asked.
            YearLookup::NoneFound => mh_db::browse::set_album_year(pool, album_id, 0).await?,
            // Transient failure (503, timeout) - leave unchecked, retry next sweep.
            YearLookup::Failed => {}
        }
    }
    Ok(dated)
}

/// One MusicBrainz outcome, separating "no date exists" (cache it) from "the
/// request failed" (retry later) so a blip never permanently blanks an album.
enum YearLookup {
    Found(i32),
    NoneFound,
    Failed,
}

/// Ask MusicBrainz for an album's release year via the release-group index.
async fn musicbrainz_year(http: &reqwest::Client, artist: &str, album: &str) -> YearLookup {
    let query = format!(
        "artist:\"{}\" AND releasegroup:\"{}\"",
        lucene_escape(artist),
        lucene_escape(album)
    );
    let Ok(resp) = http
        .get("https://musicbrainz.org/ws/2/release-group")
        .query(&[("query", query.as_str()), ("fmt", "json"), ("limit", "5")])
        .header(header::USER_AGENT, USER_AGENT)
        .send()
        .await
    else {
        return YearLookup::Failed;
    };
    if !resp.status().is_success() {
        return YearLookup::Failed;
    }
    let Ok(body) = resp.json::<Value>().await else {
        return YearLookup::Failed;
    };
    first_year(&body).map_or(YearLookup::NoneFound, YearLookup::Found)
}

/// The earliest four-digit year from the top release-group that carries a date.
fn first_year(body: &Value) -> Option<i32> {
    let groups = body.get("release-groups")?.as_array()?;
    for g in groups {
        let date = g
            .get("first-release-date")
            .and_then(Value::as_str)
            .unwrap_or("");
        if let Some(year) = date.get(0..4).and_then(|y| y.parse::<i32>().ok()) {
            if (1900..=2100).contains(&year) {
                return Some(year);
            }
        }
    }
    None
}

/// Neutralise Lucene metacharacters in a value going into a quoted query term.
fn lucene_escape(s: &str) -> String {
    s.replace(['"', '\\'], " ").trim().to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn first_year_takes_the_top_dated_group() {
        let body = serde_json::json!({
            "release-groups": [
                { "title": "DAMN.", "first-release-date": "2017-04-14" },
                { "title": "DAMN. COLLECTORS EDITION", "first-release-date": "2017-12-08" }
            ]
        });
        assert_eq!(first_year(&body), Some(2017));
    }

    #[test]
    fn first_year_skips_undated_and_out_of_range() {
        let none = serde_json::json!({
            "release-groups": [{ "title": "x" }, { "first-release-date": "" }]
        });
        assert_eq!(first_year(&none), None);
        assert_eq!(
            first_year(&serde_json::json!({ "release-groups": [] })),
            None
        );
        let ancient =
            serde_json::json!({ "release-groups": [{ "first-release-date": "1500-01-01" }] });
        assert_eq!(first_year(&ancient), None);
    }

    #[test]
    fn lucene_escape_removes_quotes_and_backslashes() {
        assert_eq!(lucene_escape("AM \"Deluxe\""), "AM  Deluxe");
        assert_eq!(lucene_escape("a\\b"), "a b");
    }
}
