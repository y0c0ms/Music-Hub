-- Record whether `is_single` was actually derived from a resolve, or is just the
-- column default.
--
-- `is_single` landed in v116 with `DEFAULT false`, and the one-off backfill in
-- 0009 filled in `cover` but not `is_single`. So every playlist recorded before
-- v116 carries `is_single = false` whether it holds one track or twelve, and
-- `cache_hit_is_complete` - which only looks at the cover - treats such a row as
-- finished. The result is a one-track single that no sweep will ever relabel,
-- because the cache-hit path never resolves it and the default says "album".
--
-- A row whose kind is not known is a row that cannot be rebuilt correctly, so it
-- belongs in the same bucket as a coverless one: incomplete, and therefore
-- re-resolved once by the next sweep. After that resolve the flag is set and the
-- row is trusted like any other.
ALTER TABLE checked_release_playlists
    ADD COLUMN IF NOT EXISTS is_single_known boolean NOT NULL DEFAULT false;
