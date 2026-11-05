# Music Hub

A self-hosted music server with a mobile-first web app (PWA) and an Android
app. It owns its library in PostgreSQL, streams from your own files, signs users
in through any OpenID Connect provider, and fetches new music with yt-dlp.

## Features

- **Library** — scans a music folder (tags, duration and artwork via ffprobe),
  with albums, artists, genres, starred tracks, playlists, play counts and
  accent-insensitive full-text search.
- **Per-user libraries** — files under `users/<name>/downloads/` belong to that
  user; everything else is shared.
- **Session playback** — every device signed into the same account shares one
  queue; pick which device plays and use the others as remotes
  ([design](docs/design/session-playback.md)).
- **Offline** — save songs, albums and playlists to the device
  ([design](docs/design/offline.md)).
- **Acquire** — search YouTube and download tracks or whole albums with yt-dlp,
  tagged and with cover art.
- **Discovery** — a swipe deck built from ListenBrainz and Last.fm suggestions
  with 30-second previews; likes are downloaded, dislikes deleted.
- **Audiobooks** — a books shelf with chapters and per-user progress, plus a
  book downloader.
- **Scrobbling** to ListenBrainz, per user or through a server-wide account.
- **Alerts** to [ntfy](https://ntfy.sh) when the downloader breaks.
- **Android app** — a thin Capacitor wrapper with background playback,
  lockscreen controls and in-app updates ([android-shell/](android-shell/)).

## Requirements

- **PostgreSQL 18** with the **pgvector** extension available
  (e.g. the `pgvector/pgvector:pg18` image). Migrations run automatically at
  startup and create the `vector`, `unaccent` and `pg_trgm` extensions, so the
  database role needs permission to create them (or create them beforehand).
- An **OpenID Connect provider** (Keycloak, Authentik, Authelia, Zitadel, ...)
  with a confidential client.
- A music folder, mounted read-write (downloads are written into it).

The container image already contains ffmpeg, yt-dlp and mutagen.

## Quick start (Docker)

```bash
docker run -d --name music-hub -p 8080:8080 \
  -e PUBLIC_URL=https://music.example.com \
  -e DATABASE_URL=postgres://musichub:secret@db:5432/musichub \
  -e OIDC_ISSUER=https://auth.example.com/realms/example \
  -e OIDC_CLIENT_ID=music-hub \
  -e OIDC_CLIENT_SECRET=change-me \
  -v /srv/music:/music \
  -v music-hub-state:/state \
  ghcr.io/y0c0ms/music-hub:latest
```

Put it behind an HTTPS reverse proxy that serves `PUBLIC_URL`. If that proxy is
nginx, keep response buffering off for `/api/playback/stream` (the server sends
`X-Accel-Buffering: no`, which nginx honours).

The container runs as UID 1000; the music folder and `/state` must be writable
by it. Health check: `GET /healthz`.

## Configuration

All configuration is read from environment variables at startup
(`crates/core/src/config.rs`). Empty counts as unset.

| Variable | Default | Purpose |
|---|---|---|
| `OIDC_ISSUER` | **required** | OIDC issuer URL; discovery is fetched from `<issuer>/.well-known/openid-configuration`. |
| `OIDC_CLIENT_SECRET` | **required** | Secret of the confidential OIDC client. Never sent to the browser. |
| `DATABASE_URL` | **required** | PostgreSQL connection string. |
| `PUBLIC_URL` | `http://localhost:8080` | External URL of the app. The OIDC redirect URI is `<PUBLIC_URL>/auth/callback`; an `https://` URL also marks cookies `Secure`. |
| `OIDC_CLIENT_ID` | `music-hub` | OIDC client ID. |
| `PORT` | `8080` | Listen port. |
| `MUSIC_ROOT` | `/music` | Library root inside the container. |
| `REQUESTS_DIR` | `requests` | Where acquired tracks land, relative to `MUSIC_ROOT`. |
| `DISCOVERY_PREFIX` | `explo/` | The only library-relative prefix a discovery dislike may delete from. |
| `BOOKS_PREFIX` | `books/` | Folder (relative to `MUSIC_ROOT`) whose files are audiobooks. |
| `STATE_DIR` | `/state` | Server state (cover cache, job state). Keep it persistent. |
| `CACHE_DIR` | `/tmp/previews` | Disposable preview-clip cache. |
| `SERVICE_TOKEN` | *(empty)* | Bearer token for `/api/internal/*` (scan, retag, owned). Empty disables those routes. |
| `LISTENBRAINZ_USER` | *(empty)* | Server-wide ListenBrainz account used when a user has not linked their own. |
| `LISTENBRAINZ_USERS` | *(empty)* | Per-user overrides: `oidcUser:lbUser,other:lbUser2`. |
| `LASTFM_API_KEY` | *(empty)* | Enables Last.fm discovery suggestions and genre enrichment. |
| `NTFY_URL` | *(empty)* | ntfy server for operational alerts; empty disables alerting. |
| `NTFY_TOPIC` | `music-hub` | ntfy topic. |
| `NTFY_TOKEN` | *(empty)* | ntfy access token. |
| `SSL_CERT_FILE` | *(unset)* | PEM file or bundle of **extra** CAs to trust for outbound HTTPS (OIDC provider, ntfy, proxies). Added to the built-in public roots, not a replacement. |
| `RUST_LOG` | `info` | Log filter. |

### OIDC

Create a confidential client in your provider:

- Redirect URI: `<PUBLIC_URL>/auth/callback`
- Scopes: `openid profile email`
- The ID token must carry `preferred_username`; it becomes the Music Hub user
  name (lower-cased for library ownership). `name` is used as the display name
  when present.

If the provider's certificate is signed by a private CA, mount the CA and set
`SSL_CERT_FILE` to it — the server does not read the system trust store.

## Building from source

Rust 1.94 (pinned in `rust-toolchain.toml`) and Node.js 22. The web app is
embedded into the binary at compile time, so build it first:

```bash
cd web && npm ci && npm run build && cd ..
cargo build --release -p music-hub
# -> target/release/music-hub
```

Or build the image:

```bash
docker build -t music-hub .
```

Tests: `cargo test --workspace` (database tests run only when `DATABASE_URL`
points at a scratch Postgres) and `cd web && node mock/saved.test.mjs`.

For UI work without a backend, `node web/mock/server.mjs` serves a mock API on
port 8080 for `npm run dev`.

### Importing from Navidrome

`mh-migrate <navidrome.db> [--commit]` imports playlists, stars and play counts
from a Navidrome SQLite database into `DATABASE_URL`. It is a dry run unless
`--commit` is given; users must have signed in once first.

## Android app

Download the APK from
[GitHub Releases](https://github.com/y0c0ms/Music-Hub/releases). On first launch
it asks for your server's `https://` URL. See
[android-shell/README.md](android-shell/README.md) for building it yourself and
for servers behind a private CA.

## License

[MIT](LICENSE)
