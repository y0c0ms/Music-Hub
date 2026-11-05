-- Nightly-precomputed "New releases" per user.
--
-- The live path asks ListenBrainz for fresh releases and then runs yt-dlp once
-- per release to resolve a preview source - seconds of wait the first time a
-- user opens Discover in the morning. A nightly job rebuilds this table so the
-- list is instant and the first preview tap plays with no live resolve: the
-- source id is already here.
--
-- Rebuilt whole per user each run (delete + insert in one transaction), so a
-- row is never stale relative to its siblings. `source_id = ''` means the
-- resolve found nothing; the card then falls back to the live resolve on tap.
CREATE TABLE IF NOT EXISTS new_release_cache (
    user_id      uuid NOT NULL,
    mbid         text NOT NULL DEFAULT '',
    artist       text NOT NULL,
    title        text NOT NULL,
    kind         text NOT NULL DEFAULT '',
    cover_url    text NOT NULL DEFAULT '',
    release_date text NOT NULL DEFAULT '',
    source_id    text NOT NULL DEFAULT '',
    position     integer NOT NULL DEFAULT 0,
    computed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, artist, title)
);
