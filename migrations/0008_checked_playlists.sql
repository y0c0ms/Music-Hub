-- Release playlists the nightly sweep has already looked at, and what it decided.
--
-- A channel's Releases shelf holds 20 entries and is newest-first, so a nightly
-- sweep that re-reads all of them does ~36s of work per artist to re-derive
-- answers it already has. At 50 artists that dominated the sweep and pushed it
-- past an hour, which is unacceptable for a job that runs before people open the
-- app.
--
-- Recording the decision turns the per-sweep cost into one flat shelf listing
-- per artist: a playlist seen once is never fetched again unless it is new to
-- this table. `last_seen` is when the sweep last confirmed it still on the shelf,
-- which is what keeps a deleted-and-recreated entry from being re-added forever.
CREATE TABLE IF NOT EXISTS checked_release_playlists (
    playlist_id text PRIMARY KEY,
    artist_key   text NOT NULL DEFAULT '',
    release_date text NOT NULL DEFAULT '',
    -- True when the playlist was resolved and its date was trusted; false when it
    -- was rejected or only its modified date was available.
    dated       boolean NOT NULL DEFAULT false,
    last_seen   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS checked_release_playlists_artist
    ON checked_release_playlists (artist_key);
