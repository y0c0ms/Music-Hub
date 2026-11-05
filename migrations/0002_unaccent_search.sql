-- Accent-insensitive library search.
--
-- Search matched with `col ILIKE '%q%'`, which is case-insensitive but NOT
-- accent-insensitive: "motorhead" found nothing, only "motörhead" did, and this
-- library is half lusophone/Turkish/Brazilian where every other title carries a
-- diacritic. Fold accents on BOTH sides with unaccent so either spelling matches.
--
-- unaccent/pg_trgm are trusted extensions; the app role may create them (verified
-- against the live pg18 instance before shipping this).
CREATE EXTENSION IF NOT EXISTS unaccent;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- The one-argument unaccent() is only STABLE (its dictionary can be reloaded), so
-- it cannot back an index. The two-argument form pinned to a fixed dictionary is
-- immutable; wrap it so expression indexes can build on it.
CREATE OR REPLACE FUNCTION f_unaccent(text) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT
    AS $$ SELECT public.unaccent('public.unaccent'::regdictionary, $1) $$;

-- Trigram GIN indexes on the folded columns so an accent-insensitive ILIKE '%q%'
-- (leading wildcard, so a btree cannot help) is still index-backed.
CREATE INDEX IF NOT EXISTS tracks_title_unaccent_trgm
    ON tracks USING gin (f_unaccent(title) gin_trgm_ops);
CREATE INDEX IF NOT EXISTS tracks_artist_unaccent_trgm
    ON tracks USING gin (f_unaccent(artist) gin_trgm_ops);
CREATE INDEX IF NOT EXISTS tracks_album_unaccent_trgm
    ON tracks USING gin (f_unaccent(album) gin_trgm_ops);
