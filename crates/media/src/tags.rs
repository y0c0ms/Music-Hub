//! Cleans the artist and title a downloader writes.
//!
//! Tracks arrive from YouTube, where the "artist" is whoever uploaded the video
//! and the "title" is whatever they typed in the box. That produces rows like
//!
//! - artist `Fools Garden`  title `Fools Garden - Lemon Tree (Official HD Video)`
//! - artist `Pizza Music`   title `Arctic Monkeys - 505`
//!
//! A faithful port of the Go `internal/tags` package.

use std::sync::LazyLock;

use regex::Regex;

/// Compile a constant pattern. Every pattern in this module is a literal fixed
/// at author time, so a failure is a build-time programmer error surfaced by the
/// first test run, never a runtime condition.
#[allow(clippy::expect_used)] // constant patterns; an invalid one fails the tests
fn compiled(pattern: &str) -> Regex {
    Regex::new(pattern).expect("constant regex pattern is valid")
}

/// Junk that describes the upload rather than the song. Anchored to a bracketed
/// group or a trailing dash so it cannot eat part of a real title.
static BRACKET_JUNK: LazyLock<Regex> = LazyLock::new(|| {
    compiled(
        r"(?i)\s*[\(\[\{]\s*(?:official(?:\s+\w+)*|clipe\s*oficial|video\s*oficial|videoclipe|lyrics?(?:\s+video)?|letra|legendado|hd|hq|4k|full\s*hd|audio|áudio|visualizer|prod\.?\s*[^\)\]\}]*|m/?v|mv|explicit|remaster(?:ed)?(?:\s*\d*)?)\s*[\)\]\}]?\s*$",
    )
});

/// `- prod Brandu`, `- prod @patriciosid`: a production credit tacked on.
static TRAILING_PROD: LazyLock<Regex> =
    LazyLock::new(|| compiled(r"(?i)\s*[-–—]\s*prod\.?\s*\S.*$"));

static DASH_SPLIT: LazyLock<Regex> = LazyLock::new(|| compiled(r"\s+[-–—]\s+"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| compiled(r"\s+"));

/// YouTube Music auto-generates one `<Artist> - Topic` channel per artist and
/// stamps it as the uploader on every album track. It is pure noise: it splits
/// an artist's catalogue into a real folder and a `- Topic` twin, and defeats
/// dedup because the same song downloaded twice lands under two artists.
static TOPIC_CHANNEL: LazyLock<Regex> = LazyLock::new(|| compiled(r"(?i)\s*-\s*topic\s*$"));

/// The cleaned pair plus why it is or is not trustworthy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Clean {
    pub artist: String,
    pub title: String,
    /// Set when the title looks like `X - Y` but neither side could be matched
    /// to the artist, so the split was NOT applied. Those rows need a human:
    /// `Pizza Music` / `Arctic Monkeys - 505` is a channel masquerading as an
    /// artist, and guessing would rewrite the wrong field.
    pub ambiguous: bool,
    pub changed: bool,
}

/// Tidy one track's artist and title.
///
/// Junk suffixes always go. The `Artist - Title` split is only applied when one
/// side can be tied back to the artist field, because this library contains both
/// orders — `Piruka - 3 ou 4` and `Sex, Drugs, Etc. - Beach Weather` — and
/// picking wrong swaps the artist and the song.
#[must_use]
pub fn clean(artist: &str, title: &str) -> Clean {
    let orig_artist = artist;
    let orig_title = title;

    let artist = tidy(&TOPIC_CHANNEL.replace(&tidy(artist), ""));
    let title = strip_junk(&tidy(title));

    let mut res = Clean {
        artist: artist.clone(),
        title: title.clone(),
        ambiguous: false,
        changed: false,
    };

    let parts: Vec<&str> = DASH_SPLIT.splitn(&title, 2).collect();
    if parts.len() == 2 {
        let left = tidy(parts[0]);
        let right = tidy(parts[1]);
        if !left.is_empty() && !right.is_empty() {
            if relates(&left, &artist) {
                // "Piruka - 3 ou 4" with artist Piruka, or the Eurythmics row
                // where the left side is the full credit list.
                res.title = strip_junk(&right);
            } else if relates(&right, &artist) {
                // The reverse order: "Sex, Drugs, Etc. - Beach Weather".
                res.title = strip_junk(&left);
            } else {
                res.ambiguous = true;
            }
        }
    }

    if res.title.is_empty() {
        res.title = title; // never clean a title away to nothing
    }
    res.changed = res.artist != orig_artist || res.title != orig_title;
    res
}

fn strip_junk(s: &str) -> String {
    let mut s = s.to_owned();
    loop {
        let next = tidy(&BRACKET_JUNK.replace(&TRAILING_PROD.replace(&s, ""), ""));
        if next == s || next.is_empty() {
            return s;
        }
        s = next;
    }
}

/// Whether a side of the title refers to the artist: the same name, or a credit
/// list that starts with or contains it.
fn relates(side: &str, artist: &str) -> bool {
    if side.is_empty() || artist.is_empty() {
        return false;
    }
    let s = side.to_lowercase();
    let a = artist.to_lowercase();
    if s == a {
        return true;
    }
    // "Eurythmics" against "Eurythmics, Annie Lennox, Dave Stewart", and
    // "bapcat" against "XTINTO x bapcat".
    if s.contains(&a) || a.contains(&s) {
        // Guard against a one-letter artist matching everything.
        return a.chars().count() >= 3 || s.chars().count() >= 3;
    }
    false
}

fn tidy(s: &str) -> String {
    SPACES.replace_all(s.trim(), " ").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_official_video_suffix() {
        let c = clean(
            "Fools Garden",
            "Fools Garden - Lemon Tree (Official HD Video)",
        );
        assert_eq!(c.artist, "Fools Garden");
        assert_eq!(c.title, "Lemon Tree");
        assert!(!c.ambiguous);
        assert!(c.changed);
    }

    #[test]
    fn ambiguous_channel_is_left_alone() {
        // "Pizza Music" is a channel, not the artist; neither side relates.
        let c = clean("Pizza Music", "Arctic Monkeys - 505");
        assert!(c.ambiguous);
        assert_eq!(c.title, "Arctic Monkeys - 505");
    }

    #[test]
    fn topic_channel_suffix_stripped_from_artist() {
        let c = clean("Daft Punk - Topic", "Get Lucky");
        assert_eq!(c.artist, "Daft Punk");
        assert_eq!(c.title, "Get Lucky");
    }

    #[test]
    fn reverse_order_split_when_right_relates() {
        let c = clean("Beach Weather", "Sex, Drugs, Etc. - Beach Weather");
        assert_eq!(c.title, "Sex, Drugs, Etc.");
    }

    #[test]
    fn credit_list_left_relates_to_first_name() {
        let c = clean("Eurythmics", "Eurythmics, Annie Lennox - Sweet Dreams");
        assert_eq!(c.title, "Sweet Dreams");
    }

    #[test]
    fn strips_trailing_prod_credit() {
        let c = clean(
            "Dfideliz",
            "Dfideliz - Menino Bom ( prod celo) [CLIPE OFICIAL]",
        );
        assert_eq!(c.title, "Menino Bom");
    }

    #[test]
    fn never_empties_a_title() {
        let c = clean("x", "(Official Video)");
        assert!(!c.title.is_empty());
    }

    #[test]
    fn unrelated_sides_leave_the_split_ambiguous() {
        // Neither "x" nor "y" ties back to artist "z", so the split is refused
        // and the row is flagged for a human rather than guessed.
        let c = clean("z", "x - y");
        assert!(c.ambiguous);
        assert_eq!(c.title, "x - y");
    }
}
