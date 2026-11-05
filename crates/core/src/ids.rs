//! Deterministic, stable ids derived from library paths and tags via uuid v5.
//!
//! A track keeps its id across rescans (so playlists and annotations survive),
//! and album/artist ids are the same whether computed for a browse list or a
//! detail view, because both are a v5 hash of a normalised grouping key.

use uuid::Uuid;

// Fixed namespace so ids are reproducible across processes and rescans.
const NAMESPACE: Uuid = Uuid::from_u128(0x6d75_7369_6368_7562_0000_0000_0000_0001);

/// Stable id for a track, from its absolute path.
#[must_use]
pub fn track_id(abs_path: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE, format!("track:{abs_path}").as_bytes())
}

/// Stable id for an artist, case-insensitive on the name.
#[must_use]
pub fn artist_id(artist: &str) -> Uuid {
    Uuid::new_v5(
        &NAMESPACE,
        format!("artist:{}", artist.to_lowercase()).as_bytes(),
    )
}

/// Stable id for an album, case-insensitive on album-artist and title. The
/// separator is a unit-separator so "a" + "bc" cannot collide with "ab" + "c".
#[must_use]
pub fn album_id(album_artist: &str, album: &str) -> Uuid {
    Uuid::new_v5(
        &NAMESPACE,
        format!(
            "album:{}\u{1f}{}",
            album_artist.to_lowercase(),
            album.to_lowercase()
        )
        .as_bytes(),
    )
}

/// Split a comma-joined credit string into individual artist names.
///
/// Trimmed, non-empty, de-duplicated on the same lowercased key [`artist_id`]
/// uses, order preserved. "Cronicas de um Babaca, Brandu, Cronicas de um Babaca"
/// yields `["Cronicas de um Babaca", "Brandu"]`. The comma is the credit
/// separator; a lone name that contains one ("Earth, Wind & Fire") is split too
/// - a rare, accepted cost of correctly separating the common collaborator case.
#[must_use]
pub fn split_artists(credit: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in credit.split(',') {
        let name = part.trim();
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        if out.iter().any(|e| e.to_lowercase() == key) {
            continue;
        }
        out.push(name.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn ids_are_deterministic() {
        assert_eq!(track_id("/music/a.opus"), track_id("/music/a.opus"));
        assert_eq!(artist_id("Radiohead"), artist_id("radiohead"));
        assert_eq!(
            album_id("Radiohead", "OK Computer"),
            album_id("radiohead", "ok computer")
        );
    }

    #[test]
    fn distinct_inputs_yield_distinct_ids() {
        assert_ne!(track_id("/music/a.opus"), track_id("/music/b.opus"));
        assert_ne!(artist_id("A"), artist_id("B"));
        // The separator prevents "a"+"bc" colliding with "ab"+"c".
        assert_ne!(album_id("a", "bc"), album_id("ab", "c"));
    }

    #[test]
    fn ids_are_v5() {
        assert_eq!(track_id("/x").get_version_num(), 5);
    }

    #[test]
    fn split_artists_separates_dedups_and_trims() {
        assert_eq!(
            split_artists("Cronicas de um Babaca, Brandu, Cronicas de um Babaca"),
            vec!["Cronicas de um Babaca", "Brandu"]
        );
        assert_eq!(split_artists("Froid"), vec!["Froid"]);
        assert_eq!(split_artists("  A ,  , b ,B"), vec!["A", "b"]);
        assert!(split_artists("").is_empty());
        assert!(split_artists("   ").is_empty());
    }
}
