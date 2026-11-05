# syntax=docker/dockerfile:1
# Three stages: build the PWA, compile the Rust binary (static musl) with the PWA
# embedded, then assemble an Alpine runtime that also carries yt-dlp, ffmpeg and
# mutagen. The runtime is Alpine (musl), so the binary is built on rust:alpine —
# a glibc binary from a Debian stage would not run here, the same reason the Go
# build used CGO_ENABLED=0.
#
# Version stamping. Passed by the deploy pipeline as:
#   docker build --build-arg MH_VERSION=v75 --build-arg MH_COMMIT=$(git rev-parse HEAD)
# Unset means "dev", which is honest: a hand build is not a release.
ARG MH_VERSION=dev
ARG MH_COMMIT=unknown

# --- 1. PWA -------------------------------------------------------------------
FROM node:22-alpine AS web
WORKDIR /web
COPY web/package.json web/package-lock.json* ./
RUN if [ -f package-lock.json ]; then npm ci; else npm install; fi
COPY web/ ./
# The bundle carries its own version so a browser can be asked what it is running
# rather than inferred from an asset hash.
ARG MH_VERSION
ARG MH_COMMIT
ENV MH_VERSION=${MH_VERSION} MH_COMMIT=${MH_COMMIT}
RUN npm run build

# --- 2. Rust binary (musl, PWA embedded) --------------------------------------
FROM rust:1.94-alpine AS build
# build-base gives gcc + musl-dev, which ring (via reqwest/sqlx rustls) needs.
RUN apk add --no-cache build-base
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ ./crates/
COPY migrations/ ./migrations/
# rust-embed reads web/dist relative to crates/server at compile time.
COPY --from=web /web/dist ./web/dist
ARG MH_VERSION
ARG MH_COMMIT
ENV MH_VERSION=${MH_VERSION} MH_COMMIT=${MH_COMMIT}
# Cache mounts: the cargo registry, git db and target/ dir persist across builds
# on the same BuildKit host, so only changed crates recompile. A clean build of
# this workspace on a 2-core machine takes 30+ min; a warm build is ~1-2 min.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    MH_BUILT_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    cargo build --release --locked -p music-hub \
    && cp target/release/music-hub /music-hub

# --- 3. Runtime ---------------------------------------------------------------
# 3.21 reached end of life and carried 1 critical + 23 high CVEs (trivy,
# 2026-11-05); 3.24 with `apk upgrade` scans clean. The release workflow scans
# the built image and refuses to publish on any fixable HIGH/CRITICAL.
FROM alpine:3.24
# yt-dlp comes from pip, NOT apk: Alpine's package lags and YouTube rejects it.
# Bump this pin when YouTube breaks it again. mutagen is REQUIRED — yt-dlp's
# --embed-thumbnail refuses to run on Opus without it, and the tag rewrite shells
# to python3 + mutagen because ffmpeg's Ogg muxer drops embedded cover art.
ARG YTDLP_VERSION=2026.10.26
RUN apk upgrade --no-cache \
    && apk add --no-cache ca-certificates tzdata ffmpeg python3 py3-pip \
    && pip install --no-cache-dir --break-system-packages "yt-dlp==${YTDLP_VERSION}" mutagen \
    && yt-dlp --version \
    && python3 -c "import mutagen; print('mutagen', mutagen.version_string)" \
    && adduser -D -u 1000 -h /home/app app
COPY --from=build /music-hub /usr/local/bin/music-hub
# /music is the library (mounted read-write). /state holds sessions + the cover
# cache and should be persistent. /tmp/previews is a disposable clip cache.
RUN mkdir -p /state /tmp/previews && chown -R 1000:1000 /state /tmp/previews
USER 1000
ENV PORT=8080 STATE_DIR=/state CACHE_DIR=/tmp/previews
EXPOSE 8080

# The same endpoint suits Kubernetes liveness/readiness probes, so `docker run`
# and an orchestrator agree on what "healthy" means. wget is in busybox, which
# alpine already provides. start-period covers a cold start while rust-embed
# unpacks the embedded PWA and yt-dlp is first invoked.
HEALTHCHECK --interval=30s --timeout=3s --start-period=20s --retries=3 \
    CMD wget -qO- "http://127.0.0.1:${PORT}/healthz" >/dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/music-hub"]
