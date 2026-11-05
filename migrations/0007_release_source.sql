-- YouTube release provenance on the cached deck.
--
-- `source` distinguishes where a row came from, so the client can label a YouTube
-- album rather than presenting it as a MusicBrainz release.
--
-- `date_uncertain` marks a row whose date is NOT a release date but the source
-- playlist's last-modified stamp. Such a row is deliberately sorted below every
-- dated row, and the client shows it as undated rather than filing it into a
-- week it does not belong to. Both columns default to empty/false so every
-- existing MusicBrainz- and ListenBrainz-sourced row is unchanged.
ALTER TABLE new_release_cache
    ADD COLUMN IF NOT EXISTS source text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS date_uncertain boolean NOT NULL DEFAULT false;
