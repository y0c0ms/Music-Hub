-- Cache the cover and kind alongside the date, so a warm sweep can still render
-- a release.
--
-- v114 stopped re-reading a playlist it had already dated, which is right for the
-- date (a property of the playlist's own uploads, so it does not drift) but threw
-- away the other two facts that same resolve had already produced. The cache-hit
-- path therefore rebuilt every row with an empty cover and `is_single: false`,
-- so a warm sweep actively destroyed the covers it had fixed and relabelled every
-- cached album as an album. That is why the list showed no artwork at all even
-- after the thumbnail fix shipped: the fix was correct, and the cache then
-- overwrote its own rows.
--
-- Storing them costs two columns and keeps a warm sweep at one flat shelf listing
-- per artist, which was the whole point of v114.
ALTER TABLE checked_release_playlists
    ADD COLUMN IF NOT EXISTS cover     text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS is_single boolean NOT NULL DEFAULT false;

-- The one-off backfill of what the v114-era rows threw away. A row whose cover is
-- empty is re-resolved by the next sweep, so this is an optimisation rather than
-- the fix: correctness comes from the sweep, not from this statement.
UPDATE checked_release_playlists c
SET cover = n.cover_url
FROM new_release_cache n
WHERE n.source = 'youtube'
  AND n.mbid = c.playlist_id
  AND c.cover = ''
  AND n.cover_url <> '';