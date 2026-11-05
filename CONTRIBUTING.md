# Contributing

Thanks for helping. This guide covers how to set up, what the checks expect,
and how to send a change.

## Setup

In short:

- Rust 1.94.0 is pinned in `rust-toolchain.toml`. `rustup` installs it and the
  `clippy`, `rustfmt`, and `llvm-tools-preview` components automatically on the
  first `cargo` command.
- Node.js 22 for `web/`.
- Postgres 18 with `pgvector` for database tests (`docker run -p 5432:5432 -e POSTGRES_PASSWORD=dev pgvector/pgvector:pg18`).

## Before you open a pull request

Run the same gates CI runs (`.forgejo/workflows/ci.yml`):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cd web && npm ci && npm run build && cd ..   # the server embeds web/dist
DATABASE_URL=postgres://postgres:dev@localhost:5432/postgres \
  cargo test --workspace -- --test-threads=1
```

Coverage is also checked: lines changed relative to `origin/main` must be at
least 95% covered by tests (`diff-cover --fail-under=95`), and total Rust line
coverage may not fall below `.github/coverage-floor`. CI also runs
`cargo audit`, `npm audit` on shipped dependencies, gitleaks, semgrep and a
trivy image scan; a release is published only when all of them pass.

## Code standards

Enforced by the lint configuration:

- `Cargo.toml` `[workspace.lints]` denies `clippy::pedantic`, `clippy::nursery`,
  `unwrap_used`, `expect_used`, and `panic`, and forbids `unsafe_code`.
- Test modules opt out with `#![allow(clippy::unwrap_used, clippy::expect_used,
  clippy::panic)]`, as in `crates/server/src/lib.rs`.
- `clippy.toml` caps cognitive complexity at 20.
- `unsafe_code` is forbidden in every crate.

Practical guidance:

- Keep handlers thin. Put SQL in `crates/db`, subprocess calls in
  `crates/media`, and HTTP plumbing in `crates/server`.
- Use the existing helpers for outbound HTTP. `mh_core::tls` builds the client
  so that `SSL_CERT_FILE` is honoured.
- Pass external-tool arguments as argv arrays, never through a shell.
- Use existing patterns before adding a new one. Each module's `//!` header
  describes its role.

## Tests

- Unit tests live next to the code. Pure logic (parsing, ranking, path guards,
  JSON shapes) should be covered.
- Database tests run only when `DATABASE_URL` points at a live Postgres (CI
  always provides one). Locally they are skipped otherwise.
- Web tests are plain Node scripts in `web/mock/` run through npm scripts
  (`test:saved`, `test:album-state`, `test:bookmark`).
- Do not add tests that only restate the implementation or check that a value
  is non-empty.

## Commits and pull requests

- Use short, imperative commit subjects. The history uses the
  `type(scope): summary` form, for example `fix(books): stop playback when the
  playing book is deleted`.
- Keep one logical change per pull request. Describe the behaviour change and
  how you verified it.
- Update docs for any change to configuration, routes, or behaviour.
- A pull request is ready when every CI job passes.

## Security issues

Do not open a public issue for a vulnerability. Follow [SECURITY.md](SECURITY.md).

## Releases

Maintainers tag `vX.Y.Z` on a commit whose CI run is green. The tag starts
`.forgejo/workflows/release.yml` on the maintainer's Forgejo CI, which runs every gate again, then publishes the container image to ghcr.io and the APK to GitHub Releases. Pull requests on GitHub are tested by the maintainer before merge; GitHub Actions is not used.
