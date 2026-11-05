-- Self-owned music-hub library. One database, the pgvector extension, and the
-- tables that replace Navidrome's SQLite plus the state the Go server kept in
-- JSON files on the PVC.
--
-- Albums and artists are NOT stored: they are pure aggregations of tracks, so a
-- filesystem scan derives them with GROUP BY over an indexed `tracks` table
-- (instant at this scale) instead of maintaining denormalised rows that drift on
-- every rescan. Each track carries a deterministic `album_id`/`artist_id` (uuid
-- v5 of the grouping key) so list and detail endpoints agree on ids.

CREATE EXTENSION IF NOT EXISTS vector;

-- Identity from Keycloak; username == preferred_username. pg18 uuidv7 default.
CREATE TABLE users (
    id           uuid PRIMARY KEY DEFAULT uuidv7(),
    kc_sub       text UNIQUE NOT NULL,
    username     text UNIQUE NOT NULL,
    display_name text NOT NULL DEFAULT '',
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- Path roots under /music: shared roots and per-user download folders. Reproduces
-- today's multi-library layout so a track's owner is derivable from its path.
CREATE TABLE libraries (
    id    serial PRIMARY KEY,
    path  text UNIQUE NOT NULL,
    name  text NOT NULL,
    kind  text NOT NULL,          -- 'shared' | 'user'
    owner text NOT NULL DEFAULT '' -- username for kind='user', else ''
);

CREATE TABLE tracks (
    id           uuid PRIMARY KEY,        -- uuid v5 of abs_path (stable across rescans)
    library_id   int NOT NULL REFERENCES libraries (id) ON DELETE CASCADE,
    rel_path     text NOT NULL,
    abs_path     text UNIQUE NOT NULL,
    title        text NOT NULL DEFAULT '',
    artist       text NOT NULL DEFAULT '',
    artist_id    uuid NOT NULL,           -- uuid v5 of lower(artist)
    album_artist text NOT NULL DEFAULT '',
    album        text NOT NULL DEFAULT '',
    album_id     uuid,                     -- uuid v5 of lower(albumartist)|lower(album); null when no album
    track_no     int NOT NULL DEFAULT 0,
    disc_no      int NOT NULL DEFAULT 0,
    year         int NOT NULL DEFAULT 0,
    genre        text NOT NULL DEFAULT '',
    duration_sec int NOT NULL DEFAULT 0,
    size_bytes   bigint NOT NULL DEFAULT 0,
    has_cover    bool NOT NULL DEFAULT false,
    is_book      bool NOT NULL DEFAULT false, -- path under the books prefix; excluded from music lists
    mtime        timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- Full-text search over title+artist+album. The 'simple'::regconfig cast makes
    -- to_tsvector IMMUTABLE, which a STORED generated column requires.
    search       tsvector GENERATED ALWAYS AS (
        to_tsvector(
            'simple'::regconfig,
            coalesce(title, '') || ' ' || coalesce(artist, '') || ' ' || coalesce(album, '')
        )
    ) STORED
);
CREATE INDEX tracks_search_idx ON tracks USING gin (search);
CREATE INDEX tracks_album_id_idx ON tracks (album_id);
CREATE INDEX tracks_artist_id_idx ON tracks (artist_id);
CREATE INDEX tracks_library_id_idx ON tracks (library_id);
CREATE INDEX tracks_is_book_idx ON tracks (is_book);

CREATE TABLE genres (
    id   serial PRIMARY KEY,
    name text UNIQUE NOT NULL
);
CREATE TABLE track_genres (
    track_id uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    genre_id int NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
    PRIMARY KEY (track_id, genre_id)
);

CREATE TABLE playlists (
    id             uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_username text NOT NULL,
    name           text NOT NULL,
    comment        text NOT NULL DEFAULT '',
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE playlist_tracks (
    playlist_id uuid NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    track_id    uuid NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    position    int NOT NULL,
    PRIMARY KEY (playlist_id, position)
);

-- Starred + play counts, replacing Navidrome's annotation table. item_id is a
-- track/album/artist id; albums and artists are derived (no FK), so this is a
-- plain uuid keyed by (user, item). item_type disambiguates the starred lists;
-- play_count/last_played_at are meaningful only for tracks.
CREATE TABLE annotations (
    user_id        uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    item_id        uuid NOT NULL,
    item_type      text NOT NULL,
    starred        bool NOT NULL DEFAULT false,
    starred_at     timestamptz,
    play_count     int NOT NULL DEFAULT 0,
    last_played_at timestamptz,
    PRIMARY KEY (user_id, item_id)
);
CREATE INDEX annotations_user_type_idx ON annotations (user_id, item_type);

-- Essentia feature vectors for pgvector similarity, plus the per-dimension
-- z-score parameters so a single-track re-featurize applies the same transform.
CREATE TABLE track_features (
    track_id        uuid PRIMARY KEY REFERENCES tracks (id) ON DELETE CASCADE,
    bpm             float8 NOT NULL DEFAULT 0,
    danceability    float8 NOT NULL DEFAULT 0,
    feature_version int NOT NULL DEFAULT 0,
    vec             vector(25)
);
CREATE INDEX track_features_vec_idx ON track_features USING ivfflat (vec vector_l2_ops) WITH (lists = 100);

CREATE TABLE feature_norm (
    dim  int PRIMARY KEY,
    mean float8 NOT NULL,
    std  float8 NOT NULL
);

-- 30-day sessions, SHA256(cookie id) as the key, matching the Go server.
CREATE TABLE sessions (
    id_hash    bytea PRIMARY KEY,
    username   text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

-- Per-chapter book position + the per-book speed/sleep settings.
CREATE TABLE book_progress (
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    book_id       text NOT NULL,
    track_id      text NOT NULL,
    position_sec  int NOT NULL DEFAULT 0,
    speed         float8 NOT NULL DEFAULT 1.0,
    sleep_minutes int NOT NULL DEFAULT 0,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, book_id)
);

CREATE TABLE discovery_verdicts (
    user_id   uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source_id text NOT NULL,
    liked     bool NOT NULL,
    PRIMARY KEY (user_id, source_id)
);

CREATE TABLE settings (
    user_id            uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    listenbrainz_user  text NOT NULL DEFAULT '',
    listenbrainz_token text NOT NULL DEFAULT ''
);

-- The playback state machine's per-user snapshot, replacing playback.json.
CREATE TABLE playback_state (
    username text PRIMARY KEY,
    state    jsonb NOT NULL,
    version  bigint NOT NULL DEFAULT 0
);

-- Postgres-as-cache (pg18): external API responses (Last.fm, ListenBrainz) keyed
-- by request, with a TTL, so the discovery deck and genre enrichment do not
-- re-hit rate-limited services. Swept lazily on read and by an occasional DELETE.
CREATE TABLE api_cache (
    key        text PRIMARY KEY,
    value      jsonb NOT NULL,
    expires_at timestamptz NOT NULL
);
CREATE INDEX api_cache_expiry_idx ON api_cache (expires_at);
