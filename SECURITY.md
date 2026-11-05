# Security policy

## Supported versions

Only the latest release and the `main` branch receive security fixes.

## Reporting a vulnerability

Do not open a public issue for a vulnerability.

Report it privately through GitHub:
[**Report a vulnerability**](https://github.com/y0c0ms/Music-Hub/security/advisories/new)
(Security tab). Only the maintainers can see the report.

Include the version or commit, the configuration needed to reproduce, and the
impact you observed. Please do not include real credentials, tokens, or
personal data in the report. Use placeholders.

We aim to acknowledge reports promptly and to agree on a disclosure timeline with you before any public write-up.

## Security model

These are the properties the code is written to provide. Each one has a
corresponding source file.

- **Credentials stay on the server.** The OIDC client secret is used only
  server-side (`crates/core/src/config.rs`, `crates/server/src/auth.rs`).
  The browser receives an `HttpOnly`, `SameSite=Lax` session cookie
  (`mh_session`) and no access tokens.
- **Sessions are stored hashed.** Only `SHA256(token)` is persisted
  (`crates/server/src/auth.rs`, `crates/db/src/sessions.rs`).
- **Service routes fail closed.** `/api/internal/*` requires
  `Authorization: Bearer <SERVICE_TOKEN>`. If `SERVICE_TOKEN` is unset, those
  routes reject every request. Tokens are compared as digests
  (`crates/server/src/auth.rs`, `require_service`).
- **No shell for external tools.** `yt-dlp`, `ffmpeg`, and `ffprobe` are invoked
  with argument arrays (`crates/media/src/acquire.rs`, `crates/media/src/ytdlp.rs`).
- **Deletes are scoped.** A discovery dislike may delete only files under
  `DISCOVERY_PREFIX`, and the prefix always ends in `/`
  (`crates/core/src/config.rs`, `guard_delete` in `crates/server/src/discovery.rs`).
- **Outbound trust is explicit.** Extra CA certificates are added only when
  `SSL_CERT_FILE` is set (`crates/core/src/tls.rs`).
- **Container runs unprivileged.** The image runs as UID 1000 and exposes port
  8080 (`Dockerfile`).

## Operator guidance

- Set `PUBLIC_URL` to the HTTPS origin you serve. This makes cookies `Secure`.
- Generate `SERVICE_TOKEN` and `OIDC_CLIENT_SECRET` with a cryptographic random
  generator. Write them with `printf`, not `echo`, so no trailing newline is
  included. The config loader trims some values, but not all of them.
- Keep `.env` files and secrets out of version control. `.gitignore` already
  excludes `.env` and `.env.*`.
- Expose only the HTTP port through your reverse proxy. Do not expose Postgres.
- Keep `yt-dlp` current. Downloads depend on it, and the pinned version in
  `docker/requirements.txt` (hash-pinned) should be bumped when extractors break.

## Automated checks

`.forgejo/workflows/ci.yml` runs gitleaks, Semgrep, `cargo audit`, `npm audit`
and a Trivy config scan; `.forgejo/workflows/release.yml` also scans the built
image with Trivy. Any finding blocks the build and the release.
