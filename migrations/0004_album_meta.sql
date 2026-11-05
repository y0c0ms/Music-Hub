-- Album metadata fetched from external sources (MusicBrainz), kept out of the
-- tracks table so a full re-probe from file tags never wipes it. A row existing
-- means the album has been checked; release_year 0 means "checked, none found"
-- so it is not re-queried.
CREATE TABLE IF NOT EXISTS album_meta (
    album_id     uuid PRIMARY KEY,
    release_year integer NOT NULL DEFAULT 0,
    source       text NOT NULL DEFAULT '',
    checked_at   timestamptz NOT NULL DEFAULT now()
);
