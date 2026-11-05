-- Record how many tracks a checked release playlist holds.
--
-- The tracklist itself is deliberately not cached (v114 removed that cost), so a
-- warm sweep rebuilt every row with no tracks at all. Ownership then fell back to
-- comparing the release title with the user's track titles, which an album only
-- passes when it happens to contain a title track. v119's album-name rule - "the
-- user owns at least as many tracks filed under this album as the release has" -
-- needs the release's track count, so it only ran on the night a playlist was
-- first resolved, and a fully downloaded album came back the next night.
--
-- 0 means "not recorded", and `cache_hit_is_complete` treats it like a missing
-- cover: the row is re-resolved once by the next sweep, which fills it in.
ALTER TABLE checked_release_playlists
    ADD COLUMN IF NOT EXISTS track_count integer NOT NULL DEFAULT 0;
