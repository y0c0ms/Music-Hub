-- Individual artists as first-class rows, split from the comma-joined credit
-- string. A track credits one or more artists via track_artists; the display
-- credit still lives in tracks.artist. This makes "Cronicas de um Babaca" one
-- artist whether it appears alone or inside "Cronicas de um Babaca, Brandu,
-- daniel shadow", so browse and search see the person, not each collaboration
-- as a separate artist.
--
-- Split rule: comma only, the user's credit convention. A legitimate comma in a
-- single name ("Earth, Wind & Fire") is split too; that is a known, rare cost of
-- a rule that correctly separates the overwhelmingly common collaborator case.

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE IF NOT EXISTS artists (
    id   uuid PRIMARY KEY,
    name text NOT NULL
);
-- Accent- and case-insensitive artist search, the same folding as tracks (0002).
CREATE INDEX IF NOT EXISTS artists_name_unaccent_trgm
    ON artists USING gin (f_unaccent(name) gin_trgm_ops);

CREATE TABLE IF NOT EXISTS track_artists (
    track_id  uuid NOT NULL REFERENCES tracks(id)  ON DELETE CASCADE,
    artist_id uuid NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
    pos       int  NOT NULL DEFAULT 0,   -- credit order, 1-based
    PRIMARY KEY (track_id, artist_id)
);
CREATE INDEX IF NOT EXISTS track_artists_artist_idx ON track_artists (artist_id);

-- Backfill from the existing comma-joined tracks.artist. uuid_generate_v5 with
-- this namespace and 'artist:'||lower(name) reproduces mh_core::ids::artist_id
-- EXACTLY (verified against the live table: artist:froid -> the same uuid the
-- scanner wrote), so a row inserted here and one the scanner writes later share
-- an id and never duplicate. Split on comma with surrounding whitespace, drop
-- empty segments, and dedup per track via the primary key.
INSERT INTO artists (id, name)
SELECT DISTINCT
       uuid_generate_v5('6d757369-6368-7562-0000-000000000001', 'artist:' || lower(btrim(part))),
       btrim(part)
FROM tracks t,
     unnest(regexp_split_to_array(t.artist, '\s*,\s*')) AS part
WHERE btrim(part) <> ''
ON CONFLICT (id) DO NOTHING;

INSERT INTO track_artists (track_id, artist_id, pos)
SELECT t.id,
       uuid_generate_v5('6d757369-6368-7562-0000-000000000001', 'artist:' || lower(btrim(x.part))),
       x.ord
FROM tracks t,
     LATERAL unnest(regexp_split_to_array(t.artist, '\s*,\s*')) WITH ORDINALITY AS x(part, ord)
WHERE btrim(x.part) <> ''
ON CONFLICT (track_id, artist_id) DO NOTHING;
