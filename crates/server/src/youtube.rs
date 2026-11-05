//! YouTube as a second source of "new releases".
//!
//! MusicBrainz and ListenBrainz only ever learn about music once it is in a
//! distributor's catalogue, so an album an artist puts on their own channel and
//! nowhere else is simply invisible. This module reads the channel's Releases
//! shelf instead, where the artist (or their label) publishes the record.
//!
//! Two rules, both deliberate:
//!
//! * **A grouping is only ever a real release playlist.** The channel's Releases
//!   shelf lists entries with ids beginning `OLAK5uy_`, which is what YouTube
//!   itself uses for an album or single. Same-day uploads are *not* grouped into
//!   an album, because several artists drop unrelated tracks the same day and an
//!   invented album is worse than a few singles.
//! * **The release date is derived, and only when it can be trusted.** YouTube
//!   publishes no date for a release playlist. See [`release_date`].

use std::collections::HashMap;

use serde_json::Value;

use crate::discovery::Release;

/// A YouTube release entry: the playlist id that defines the album, plus the
/// channel it came from.
pub struct YtRelease {
    pub artist: String,
    pub title: String,
    pub playlist_id: String,
    pub tracks: Vec<String>,
    pub cover: String,
    pub date: String,
    /// The playlist could not be dated, so the row is labelled rather than
    /// sorted into a week. Set by [`release_date`].
    pub date_uncertain: bool,
    /// n == 1, so this is a single rather than an album.
    pub is_single: bool,
    /// How many tracks the playlist holds. Equal to `tracks.len()` after a
    /// resolve; carried on its own because a cached row has the count but not
    /// the tracklist, and the count is what album ownership is judged by.
    pub track_count: usize,
}

/// Pull the tracklist, cover and count out of a resolved release playlist.
///
/// A " - prod @handle" credit is stripped from each track, for the same reason
/// it is stripped from the album title: the credit is not part of the track's
/// name, and leaving it on would make an owned track fail to match.
#[must_use]
pub fn parse_playlist(body: &Value) -> (Vec<String>, String, bool) {
    let mut tracks = Vec::new();
    if let Some(entries) = body["entries"].as_array() {
        for e in entries {
            if let Some(t) = e["title"].as_str() {
                let clean = clean_shelf_title(t.trim());
                if !clean.is_empty() && !tracks.contains(&clean) {
                    tracks.push(clean);
                }
            }
        }
    }
    // Not `.last()`. The array is ascending (180 -> 640 -> 1200) and the
    // largest entry is `.../maxresdefault.jpg` with no signature query - and
    // that bare path 404s for every release playlist checked, so taking the
    // last entry stored a dead URL for every row.
    //
    // The signed entries (the `?sqp=...&rs=...` ones) answer 200. So take the
    // widest thumbnail that still carries a query string, which is the largest
    // image YouTube will actually serve.
    let cover = body["thumbnails"]
        .as_array()
        .and_then(|ts| {
            ts.iter()
                .filter(|t| t["url"].as_str().is_some_and(|u| u.contains('?')))
                .max_by_key(|t| t["width"].as_u64().unwrap_or(0))
        })
        .or_else(|| {
            // A playlist with only unsigned entries: fall back to the widest
            // rather than dropping the cover entirely.
            body["thumbnails"]
                .as_array()
                .and_then(|ts| ts.iter().max_by_key(|t| t["width"].as_u64().unwrap_or(0)))
        })
        .and_then(|t| t["url"].as_str())
        .unwrap_or("")
        .to_owned();
    // A one-track release is a single, whatever the shelf called it.
    let is_single = tracks.len() == 1;
    (tracks, cover, is_single)
}

/// The id prefix YouTube uses for a playlist created through Music, which is what
/// the Releases shelf is built from. Anything else on that shelf is a stray
/// playlist and is ignored.
const RELEASE_ID_PREFIX: &str = "OLAK5uy_";

/// How many entries of the Releases shelf to look at.
///
/// The shelf listing itself is flat and cheap - one `yt-dlp --flat-playlist` per
/// artist - and since the playlist cache landed (v114) it is the *only* per-entry
/// cost, because a playlist already recorded is never resolved again. That is
/// what makes a generous limit affordable here.
///
/// 20 was a guess that turned out to be wrong by more than half: it assumed an
/// artist adds "a few a year", but Alec Benjamin's shelf carries 45 release
/// playlists, so 25 of them - including whole albums - were never examined and
/// the album you want simply could not appear. The limit only bounds work for
/// playlists not yet in the cache, and those pay for the expensive resolve, so
/// a shelf far longer than this would still cost one flat listing.
const SHELF_LIMIT: usize = 50;

/// Pick the release playlists out of a `yt-dlp --flat-playlist` dump of a
/// channel's Releases shelf.
///
/// Returns `(playlist_id, title)` pairs in shelf order. A title carrying the
/// " - prod @handle" suffix that artists put on uploads is trimmed, because that
/// suffix is a credit, not the record name.
///
/// # Panics
/// Never; a missing or unparseable field is skipped rather than fatal.
#[must_use]
pub fn parse_release_shelf(body: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(entries) = body["entries"].as_array() else {
        return out;
    };
    for e in entries.iter().take(SHELF_LIMIT) {
        let Some(id) = e["id"].as_str() else { continue };
        if !id.starts_with(RELEASE_ID_PREFIX) {
            continue;
        }
        let raw = e["title"].as_str().unwrap_or("").trim();
        if raw.is_empty() {
            continue;
        }
        out.push((id.to_owned(), clean_shelf_title(raw)));
    }
    out
}

/// Strip the production credit artists append to their upload titles.
///
/// "Super Crônico - prod @patriciosid" is one track named `Super Crônico`; the
/// credit belongs on the track, not the record, and leaving it on would stop an
/// owned track from matching. The separators vary in case and spacing in the
/// wild - " - Prod", "  -  PROD", " - Official Video" are all seen - so the
/// credit is matched case-insensitively but on the *original* string, and only
/// after a hyphen. A title whose own text contains a hyphen is untouched, and one
/// that is nothing but a credit is left alone rather than emptied.
fn clean_shelf_title(raw: &str) -> String {
    const CREDITS: &[&str] = &[
        "prod",
        "official video",
        "official audio",
        "m/v",
        "mv",
        "feat",
        "ft",
    ];
    // Split on hyphens, looking only at the last segment for a credit.
    let Some((head, tail)) = raw.rsplit_once('-') else {
        return raw.trim().to_owned();
    };
    let tail = tail.trim().to_lowercase();
    if tail.is_empty() {
        return raw.trim().to_owned();
    }
    if CREDITS
        .iter()
        .any(|c| tail == *c || tail.starts_with(&format!("{c} ")))
    {
        let head = head.trim();
        if !head.is_empty() {
            return head.to_owned();
        }
    }
    raw.trim().to_owned()
}

/// The release date for a playlist, and whether it can be trusted.
///
/// YouTube publishes no date for a release playlist. Two things are observable:
///
/// * `modified_date` on the playlist - when the playlist itself was last touched.
/// * the upload date of each track inside it.
///
/// The track dates are the better source, because artists put pre-release
/// singles out months ahead of the album: the album's tracks share one date and
/// the single is the odd one out. So the **modal** track date is taken, but only
/// when it is recent enough to be a candidate release at all - a playlist whose
/// tracks all cluster on a 2023 date is an older record being re-uploaded, and its
/// cluster is not a release date.
///
/// When the cluster is not trustworthy the playlist's `modified_date` is used
/// instead and the row is marked uncertain, so the list can label it rather than
/// sort a two-year-old album into this week.
#[must_use]
pub fn release_date(body: &Value, since: &str) -> (String, bool) {
    let dates: Vec<String> = body["entries"]
        .as_array()
        .map(|es| {
            es.iter()
                .filter_map(|e| e["upload_date"].as_str())
                .map(ymd_to_iso)
                .filter(|d| !d.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !dates.is_empty() {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for d in &dates {
            *counts.entry(d.as_str()).or_default() += 1;
        }
        // Modal date, but only if a real share of the tracks agree on it. A
        // playlist whose tracks are all different dates has no cluster to trust.
        let (modal, hits) = counts
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .map(|(d, n)| (d.to_owned(), n))
            .unwrap_or_default();
        // Clustered but too old to be a new release, or no cluster at all: fall
        // through to the playlist's own date, marked uncertain.
        if !modal.is_empty() && hits * 2 >= dates.len() && modal.as_str() >= since {
            return (modal, false);
        }
    }
    let modified = ymd_to_iso(body["modified_date"].as_str().unwrap_or(""));
    if modified.is_empty() {
        return (String::new(), true);
    }
    (modified, true)
}

/// yt-dlp reports dates as `YYYYMMDD`; the rest of this module uses ISO dates so
/// they compare correctly as strings.
fn ymd_to_iso(s: &str) -> String {
    let s = s.trim();
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..8])
    } else {
        String::new()
    }
}

/// Build a [`Release`] from a resolved playlist.
pub(crate) fn to_release(y: &YtRelease) -> Release {
    Release {
        artist: y.artist.clone(),
        title: y.title.clone(),
        date: y.date.clone(),
        kind: if y.is_single { "Single" } else { "Album" }.to_owned(),
        cover: y.cover.clone(),
        // The playlist id is the stable identity here, exactly as a MusicBrainz
        // release id is, so a dismissal survives across sweeps.
        mbid: y.playlist_id.clone(),
        confidence: 100,
        tracks: y.tracks.clone(),
        track_count: y.track_count,
        source: "youtube".to_owned(),
        date_uncertain: y.date_uncertain,
    }
}

/// Rebuild a release row from a playlist the sweep has already resolved.
///
/// Only the date is genuinely worth re-deriving, and that is exactly what v114
/// decided to cache: a playlist's date comes from its own track uploads, so it
/// does not drift, and re-reading it cost ~13s per entry and pushed the sweep
/// past an hour.
///
/// The cover and the kind come free with that same resolve and are just as
/// stable, so they are cached alongside it. Dropping them - which is what this
/// function used to do inline, with `cover: String::new()` and
/// `is_single: false` - meant every warm sweep rewrote all 300-odd rows with no
/// artwork and relabelled every album, overwriting the rows the resolve had
/// just written. That is why fixing the thumbnail URL changed nothing in the
/// list: the fix worked, and the sweep then erased it.
fn cached_release(
    artist: &str,
    title: String,
    playlist_id: &str,
    hit: &mh_db::discovery::CheckedPlaylist,
) -> Release {
    to_release(&YtRelease {
        artist: artist.to_owned(),
        title,
        playlist_id: playlist_id.to_owned(),
        // The tracklist is deliberately NOT cached - that is the cost v114
        // removed. Its length is, because album ownership needs it: without
        // the count a fully downloaded album was only recognised on the one
        // night its playlist was resolved, and came back the night after.
        tracks: Vec::new(),
        track_count: hit.track_count,
        cover: hit.cover.clone(),
        date: hit.release_date.clone(),
        date_uncertain: false,
        is_single: hit.is_single,
    })
}

/// Whether a cached playlist record is complete enough to rebuild the row from.
///
/// Facts recorded before the release that added them are missing, and they stay
/// missing if the cache-hit path is trusted, since it never resolves: covers
/// (v116), whether the playlist is a single rather than defaulted to "album"
/// (migration 0010), and the track count album ownership needs (migration 0011).
/// An undated row is never complete: its date is the playlist's last-modified
/// stamp, which can still change.
///
/// Treating an incomplete record as a miss sends it back through the resolve once,
/// after which it is cached like any other. The cost is paid one time per row
/// rather than every night, and no row is left permanently wrong.
const fn cache_hit_is_complete(hit: &mh_db::discovery::CheckedPlaylist) -> bool {
    hit.dated && !hit.cover.is_empty() && hit.is_single_known && hit.track_count > 0
}

/// What a warm sweep does with one shelf entry.
#[derive(Debug, PartialEq, Eq)]
enum Reuse {
    /// Rebuild the row from the cache; no yt-dlp call.
    Emit,
    /// Leave it out; no yt-dlp call.
    Skip,
    /// Examine it again.
    Resolve,
}

/// Decide whether a shelf entry needs yt-dlp at all.
///
/// - A complete record is rebuilt from the cache, unless its date has fallen out
///   of the window: the window check used to run only for uncached playlists, so
///   a cached release was listed as "new" for as long as it stayed on the shelf.
/// - A rejection (recorded with no date) is trusted for
///   [`mh_db::discovery::REJECTION_RECHECK_DAYS`]. It used to be written and
///   never read back, so every sweep re-checked every rejected playlist.
/// - Anything else - never seen, incomplete, or dated only by its modified stamp -
///   is examined again.
fn reuse(hit: Option<&mh_db::discovery::CheckedPlaylist>, since: &str) -> Reuse {
    let Some(hit) = hit else {
        return Reuse::Resolve;
    };
    if cache_hit_is_complete(hit) {
        return if hit.release_date.as_str() < since {
            Reuse::Skip
        } else {
            Reuse::Emit
        };
    }
    if !hit.dated && hit.release_date.is_empty() && hit.recently_checked {
        return Reuse::Skip;
    }
    Reuse::Resolve
}

/// One artist's releases from their channel, newest first.
///
/// The channel is resolved once and then cached in `artist_channels`; only the
/// shelf listing is repeated. A playlist is only resolved further when its
/// `modified_date` could plausibly be a new release, because resolving one costs
/// ~13s and an artist's shelf is mostly old records.
pub(crate) async fn artist_releases(
    pool: &mh_db::Pool,
    user_id: uuid::Uuid,
    artist: &str,
    since: &str,
    already: &HashMap<String, mh_db::discovery::CheckedPlaylist>,
) -> Vec<Release> {
    let key = mh_media::acquire::normalise_name(artist);
    let channel = match mh_db::discovery::artist_channel(pool, &key).await {
        Ok(Some(c)) => Some(c),
        Ok(None) => {
            // Not seen before. Offer the resolver titles this user really owns by
            // this artist, so it can confirm the channel from a known song rather
            // than from the name alone.
            let known = mh_db::discovery::artist_known_songs(pool, user_id, artist, 5)
                .await
                .unwrap_or_default();
            match mh_media::acquire::resolve_artist_channel(artist, &known).await {
                Ok(Some(hit)) => {
                    if let Err(e) = mh_db::discovery::set_artist_channel(
                        pool,
                        &key,
                        &hit.channel_id,
                        &hit.channel_name,
                        &hit.confirmed_by,
                    )
                    .await
                    {
                        tracing::warn!(error = %e, artist = %artist, "new-releases: channel cache");
                    }
                    Some(mh_db::discovery::ArtistChannel {
                        channel_id: hit.channel_id,
                        channel_name: hit.channel_name,
                        confirmed_by: hit.confirmed_by,
                    })
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(error = %e, artist = %artist, "new-releases: channel resolve");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, artist = %artist, "new-releases: channel lookup");
            None
        }
    };
    let Some(channel) = channel else {
        return Vec::new();
    };

    let Ok(shelf) = mh_media::acquire::channel_release_shelf(&channel.channel_id).await else {
        return Vec::new();
    };
    let entries = parse_release_shelf(&shelf);
    let mut out = Vec::new();
    for (playlist_id, title) in entries {
        // A playlist already examined is answered from the cache when the answer
        // cannot have changed - see `reuse`. Re-reading a dated playlist costs
        // ~13s to learn the same thing and a rejection ~1.8s, and across a
        // 50-artist shelf that is what pushed the sweep to two hours.
        let hit = already.get(&playlist_id);
        match reuse(hit, since) {
            Reuse::Skip => continue,
            Reuse::Emit => {
                if let Some(hit) = hit {
                    out.push(cached_release(
                        &channel.channel_name,
                        title,
                        &playlist_id,
                        hit,
                    ));
                }
                continue;
            }
            Reuse::Resolve => {}
        }
        // `modified_date` costs ~1.8s and resolving the playlist costs ~13s, and
        // an artist's shelf is 20 entries of which typically one or two are new.
        // So the cheap read decides first: a playlist untouched for months is not
        // a candidate release and never pays for the expensive resolve. The other
        // order would cost ~3.6 hours across a 50-artist sweep.
        let Ok(modified) = mh_media::acquire::playlist_modified_date(&playlist_id).await else {
            continue;
        };
        if modified.is_empty() || modified.as_str() < since {
            let _ = mh_db::discovery::mark_playlist_checked(
                pool,
                &playlist_id,
                &key,
                "",
                false,
                "",
                false,
                false,
                0,
            )
            .await;
            continue;
        }
        let Ok(body) = mh_media::acquire::release_playlist(&playlist_id).await else {
            continue;
        };
        // The date the row shows comes from the resolved track dates, which are
        // more accurate than the playlist's own stamp. The stamp is only ever a
        // fallback, and marks the row uncertain when it is the one being used.
        let (date, uncertain) = release_date(&body, since);
        if date.is_empty() {
            let _ = mh_db::discovery::mark_playlist_checked(
                pool,
                &playlist_id,
                &key,
                "",
                false,
                "",
                false,
                false,
                0,
            )
            .await;
            continue;
        }
        let (tracks, cover, is_single) = parse_playlist(&body);
        if tracks.is_empty() {
            let _ = mh_db::discovery::mark_playlist_checked(
                pool,
                &playlist_id,
                &key,
                "",
                false,
                "",
                false,
                false,
                0,
            )
            .await;
            continue;
        }
        // Only a date derived from the tracks themselves is safe to keep. An
        // uncertain one is the playlist's last-modified stamp, which does change,
        // so it is re-derived next time and can become dated later.
        if let Err(e) = mh_db::discovery::mark_playlist_checked(
            pool,
            &playlist_id,
            &key,
            &date,
            !uncertain,
            &cover,
            is_single,
            true,
            tracks.len(),
        )
        .await
        {
            tracing::warn!(error = %e, playlist = %playlist_id, "new-releases: playlist cache");
        }
        out.push(to_release(&YtRelease {
            artist: channel.channel_name.clone(),
            title,
            playlist_id,
            track_count: tracks.len(),
            tracks,
            cover,
            date,
            date_uncertain: uncertain,
            is_single,
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shelf_keeps_only_release_playlists() {
        let body = json!({ "entries": [
            { "id": "OLAK5uy_k8AY", "title": "Super Crônico" },
            { "id": "PL1234567890", "title": "Some Random Playlist" },
            { "id": "OLAK5uy_m3gK", "title": "Sem Entender" },
        ]});
        let got = parse_release_shelf(&body);
        assert_eq!(
            got,
            vec![
                ("OLAK5uy_k8AY".to_owned(), "Super Crônico".to_owned()),
                ("OLAK5uy_m3gK".to_owned(), "Sem Entender".to_owned()),
            ]
        );
    }

    /// A real channel's Releases shelf carries far more entries than the limit
    /// this used to impose. Alec Benjamin's returns 45 release playlists, so a
    /// limit of 20 silently dropped 25 of them - whole albums included, and the
    /// album you were looking for was among them. It is not a rendering limit:
    /// anything past it is never examined at all, so it cannot appear.
    #[test]
    fn the_shelf_is_not_truncated_below_what_a_real_channel_returns() {
        let entries: Vec<_> = (0..45)
            .map(|i| json!({ "id": format!("OLAK5uy_rel{i:02}"), "title": format!("Album {i}") }))
            .collect();
        let got = parse_release_shelf(&json!({ "entries": entries }));
        assert_eq!(
            got.len(),
            45,
            "a 45-entry shelf must survive the sweep intact"
        );
        assert_eq!(got[44].0, "OLAK5uy_rel44", "the newest entries come last");
    }

    #[test]
    fn shelf_title_drops_the_production_credit() {
        assert_eq!(
            clean_shelf_title("Super Crônico - prod @patriciosid"),
            "Super Crônico"
        );
        assert_eq!(
            clean_shelf_title("Vencer Na Vida ?  -  Prod @offpadrin"),
            "Vencer Na Vida ?"
        );
        // A hyphen inside a real title must survive.
        assert_eq!(
            clean_shelf_title("Something - Something Else"),
            "Something - Something Else"
        );
        // A title that is nothing but a credit is not emptied into "", which the
        // caller would read as a blank track; it comes back trimmed instead.
        assert_eq!(clean_shelf_title(" - prod"), "- prod");
    }

    #[test]
    fn modal_track_date_wins_over_a_presingle() {
        // The real shape of an album with a single released months earlier.
        let body = json!({ "modified_date": "20260926", "entries": [
            { "upload_date": "20260807" },
            { "upload_date": "20260924" },
            { "upload_date": "20260924" },
            { "upload_date": "20260924" },
        ]});
        let (date, uncertain) = release_date(&body, "2026-05-31");
        assert_eq!(date, "2026-09-24");
        assert!(!uncertain, "a clean recent cluster is a real release date");
    }

    #[test]
    fn an_old_cluster_never_becomes_a_new_release_date() {
        // Tracks all cluster on 2024: an older record re-uploaded, not a release.
        // Taking the mode here would file it in "New this week" forever.
        let body = json!({ "modified_date": "20260822", "entries": [
            { "upload_date": "20240704" },
            { "upload_date": "20240704" },
            { "upload_date": "20240704" },
        ]});
        let (date, uncertain) = release_date(&body, "2026-05-31");
        assert_ne!(date, "2024-07-04");
        assert!(uncertain, "an old cluster must fall back, not be trusted");
    }

    #[test]
    fn scattered_track_dates_fall_back_to_the_playlist_date() {
        let body = json!({ "modified_date": "20260926", "entries": [
            { "upload_date": "20240101" },
            { "upload_date": "20250607" },
            { "upload_date": "20260808" },
        ]});
        let (date, uncertain) = release_date(&body, "2026-05-31");
        assert_eq!(date, "2026-09-26");
        assert!(uncertain);
    }

    #[test]
    fn a_playlist_with_no_dates_at_all_is_still_listed() {
        let body = json!({ "modified_date": null, "entries": [] });
        let (date, uncertain) = release_date(&body, "2026-05-31");
        assert!(date.is_empty());
        assert!(uncertain);
    }

    #[test]
    fn cover_prefers_the_widest_thumbnail_youtube_actually_serves() {
        // Captured verbatim from yt-dlp for release playlist
        // OLAK5uy_nwZ-c95b1hUtXWPObN1srn06K5MpAjj1I ("Màgoa Para Elefantes").
        // The array is ascending and the widest entry is the bare
        // `maxresdefault.jpg`, which 404s; the signed entries answer 200.
        let body = json!({ "entries": [{ "title": "Um Momento" }], "thumbnails": [
            { "width": 180, "url": "https://i9.ytimg.com/s_p/OLAK5uy_x/mqdefault.jpg?sqp=CMyI9NUGir7X7AMICInShMYGEAE=&rs=AOn4CLAE&v=1757489417" },
            { "width": 640, "url": "https://i9.ytimg.com/s_p/OLAK5uy_x/sddefault.jpg?sqp=CMyI9NUGir7X7AMICInShMYGEAE=&rs=AOn4CLAz&v=1757489417" },
            { "width": 1200, "url": "https://i9.ytimg.com/s_p/OLAK5uy_x/maxresdefault.jpg" },
        ]});
        let (_, cover, _) = parse_playlist(&body);
        assert_eq!(
            cover,
            "https://i9.ytimg.com/s_p/OLAK5uy_x/sddefault.jpg?sqp=CMyI9NUGir7X7AMICInShMYGEAE=&rs=AOn4CLAz&v=1757489417",
            "must not take the unsigned 1200px entry: that URL 404s"
        );
        assert_ne!(
            cover, "https://i9.ytimg.com/s_p/OLAK5uy_x/maxresdefault.jpg",
            "the old `.last()` answer, which is the broken one"
        );
    }

    #[test]
    fn a_cover_is_still_found_when_no_thumbnail_is_signed() {
        // Some playlists hand back unsigned entries only. A cover that is merely
        // unverified beats no cover at all, so fall back to the widest.
        let body = json!({ "entries": [{ "title": "Um Momento" }], "thumbnails": [
            { "width": 180, "url": "https://i9.ytimg.com/s_p/OLAK5uy_y/mqdefault.jpg" },
            { "width": 640, "url": "https://i9.ytimg.com/s_p/OLAK5uy_y/sddefault.jpg" },
        ]});
        let (_, cover, _) = parse_playlist(&body);
        assert_eq!(cover, "https://i9.ytimg.com/s_p/OLAK5uy_y/sddefault.jpg");
    }

    #[test]
    fn a_release_with_no_thumbnails_has_an_empty_cover_not_a_panic() {
        let body = json!({ "entries": [{ "title": "Um Momento" }] });
        let (tracks, cover, is_single) = parse_playlist(&body);
        assert_eq!(tracks, vec!["Um Momento".to_owned()]);
        assert!(cover.is_empty());
        assert!(is_single);
    }

    /// A fully recorded, dated, in-window playlist: the shape every test below
    /// starts from and then breaks one fact of.
    fn complete() -> mh_db::discovery::CheckedPlaylist {
        mh_db::discovery::CheckedPlaylist {
            release_date: "2026-09-28".to_owned(),
            dated: true,
            cover: "https://i9.ytimg.com/s_p/OLAK5uy_x/hqdefault.jpg?sqp=abc&rs=def".to_owned(),
            is_single: false,
            is_single_known: true,
            track_count: 11,
            recently_checked: true,
        }
    }

    const SINCE: &str = "2026-06-10";

    /// The bug this pins: the sweep reuses a cached playlist's date, skips the
    /// resolve, and used to rebuild the row with an empty cover, a hardcoded
    /// `is_single: false` and no track count. Every warm sweep therefore erased
    /// the artwork, relabelled every single, and hid nothing the user owned.
    #[test]
    fn a_cached_playlist_carries_its_cover_kind_and_track_count() {
        let hit = complete();
        let rel = cached_release("Keane", "Hopes And Fears".to_owned(), "OLAK5uy_x", &hit);
        assert_eq!(rel.cover, hit.cover, "the warm sweep dropped the cover");
        assert_eq!(rel.kind, "Album");
        assert_eq!(rel.date, "2026-09-28");
        assert_eq!(
            rel.track_count, 11,
            "a cached album lost its track count, so an owned album reappears"
        );
    }

    #[test]
    fn a_cached_single_is_still_labelled_a_single() {
        let hit = mh_db::discovery::CheckedPlaylist {
            is_single: true,
            track_count: 1,
            ..complete()
        };
        let rel = cached_release(
            "Plain White T's",
            "Let's Lay Low".to_owned(),
            "OLAK5uy_y",
            &hit,
        );
        assert_eq!(
            rel.kind, "Single",
            "a cached single was relabelled an album"
        );
    }

    #[test]
    fn a_fully_recorded_playlist_in_the_window_is_rebuilt_from_the_cache() {
        assert_eq!(reuse(Some(&complete()), SINCE), Reuse::Emit);
    }

    #[test]
    fn a_playlist_never_seen_is_resolved() {
        assert_eq!(reuse(None, SINCE), Reuse::Resolve);
    }

    #[test]
    fn a_record_missing_any_fact_is_resolved_once() {
        // Each fact was added by a later release, so rows written before it lack
        // it and the cache-hit path would never fill it in.
        let cases = [
            (
                "no cover (pre-v116)",
                mh_db::discovery::CheckedPlaylist {
                    cover: String::new(),
                    ..complete()
                },
            ),
            (
                "kind only defaulted (0010)",
                mh_db::discovery::CheckedPlaylist {
                    is_single_known: false,
                    ..complete()
                },
            ),
            (
                "no track count (0011)",
                mh_db::discovery::CheckedPlaylist {
                    track_count: 0,
                    ..complete()
                },
            ),
        ];
        for (what, hit) in cases {
            assert_eq!(
                reuse(Some(&hit), SINCE),
                Reuse::Resolve,
                "{what}: trusted as complete"
            );
        }
    }

    #[test]
    fn a_cached_release_older_than_the_window_is_left_out() {
        // The window used to be checked only for uncached playlists, so a cached
        // release stayed "new" for as long as it sat on the channel's shelf.
        let old = mh_db::discovery::CheckedPlaylist {
            release_date: "2026-06-09".to_owned(),
            ..complete()
        };
        assert_eq!(
            reuse(Some(&old), SINCE),
            Reuse::Skip,
            "an aged-out release was still listed"
        );
        let edge = mh_db::discovery::CheckedPlaylist {
            release_date: SINCE.to_owned(),
            ..complete()
        };
        assert_eq!(
            reuse(Some(&edge), SINCE),
            Reuse::Emit,
            "the first day of the window was dropped"
        );
    }

    #[test]
    fn a_recent_rejection_is_not_checked_again() {
        // Rejections were written and never read back, so every sweep re-checked
        // every one: 783 of 933 in a single night.
        let rejected = mh_db::discovery::CheckedPlaylist {
            release_date: String::new(),
            dated: false,
            cover: String::new(),
            is_single_known: false,
            track_count: 0,
            ..complete()
        };
        assert_eq!(reuse(Some(&rejected), SINCE), Reuse::Skip);
        let stale = mh_db::discovery::CheckedPlaylist {
            recently_checked: false,
            ..rejected
        };
        assert_eq!(
            reuse(Some(&stale), SINCE),
            Reuse::Resolve,
            "a rejection was trusted forever, so an edited playlist would never be noticed"
        );
    }

    #[test]
    fn a_playlist_dated_only_by_its_modified_stamp_is_resolved_again() {
        // Its date can still change, so it is never answered from the cache.
        let uncertain = mh_db::discovery::CheckedPlaylist {
            dated: false,
            ..complete()
        };
        assert_eq!(reuse(Some(&uncertain), SINCE), Reuse::Resolve);
    }
}
