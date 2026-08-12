# AGENTS.md

Guidance for AI coding agents (and humans) working in this repo. Tool-agnostic;
`CLAUDE.md` points here.

## What this is

`rpmfl` — a Rust CLI for the AppFolio **owner portal** that Real Property
Management franchises hand to rental-property owners. A thin, portal-specific
layer over the shared [`cli-common`](https://github.com/piekstra/cli-common)
`pk-cli-*` crates (auth, http, config, secrets, self-update). This repo owns
only the portal client, the commands, and their DTOs.

There is no official API. Everything targets the undocumented JSON endpoints
the portal's own front end calls, mapped by watching its XHR traffic and
written up in [`docs/api.md`](docs/api.md).

## Build, test, lint

```console
make verify     # fmt-check + clippy -D warnings + tests + smoke — the CI gate
make test       # unit + integration (fully offline; no network, no creds)
make dev        # debug build, re-signed so keychain grants survive rebuilds
cargo run -- summary
```

Run `make verify` before considering a change done — it's exactly what CI runs.

## Layout

- `src/main.rs` — clap command tree, the login flow, exit-code mapping.
- `src/client.rs` — the portal HTTP client: cookie session, CSRF handling, the
  two-factor dance, and the redirect-means-expired detection. Its module doc
  explains the auth model and why the device-trust token isn't usable here.
- `src/commands/*.rs` — one module per command group; `misc.rs` holds the
  small single-endpoint reads. Each renders a human table and a `--json` DTO.
- `src/config.rs` — non-secret config; every secret is keychain-only.
- `src/dates.rs` — ISO ⇄ the portal's `MM/DD/YYYY`, plus `RangeArgs`.
- `tests/` — offline surface + contract tests, and `tests/fixtures/` (read its
  README before touching a fixture).

## Conventions (do not break these)

- **`--json` on every command**, emitting one DTO tagged with a `schema` field
  (e.g. `"schema":"transaction-list/v1"`). Human output → stdout as a table;
  diagnostics → stderr. Keep the two paths in sync; a breaking DTO change bumps
  the `/vN` suffix.
- **Exit codes:** 0 ok · 2 usage · 3 auth · 4 not found · 5 upstream · 6
  confirmation required. Validate args **before** touching the keychain or
  network, so `--help` and bad args never prompt, hang, or hit the portal.
- **Read-only.** Every command observes. The portal supports contributions,
  approvals, and profile edits; none are implemented. If a write is ever added
  it must prompt for confirmation and require `--force` non-interactively
  (exit 6 otherwise) — and this section must stop saying "read-only".
- **Secrets** come from the OS keychain or stdin — never argv, never logs,
  never a file in the repo. Service `piekstra.rpmfl`, accounts `password`,
  `session`, `device-token`.
- **Dates** are ISO `YYYY-MM-DD` at the CLI boundary, converted in `dates.rs`.
  Never surface the portal's `MM/DD/YYYY` in a flag.

## Tests must never touch the real keychain

`cargo test` rebuilds `target/debug/<bin>` ad-hoc signed, which macOS sees as a
new code identity — so any test that runs a command reading credentials puts up
a permission dialog *per keychain item*, on every test run. `auth status` reads
two, so it prompted twice each time. Keep credential-touching assertions behind
an opt-in env var (`RPMFL_TEST_KEYCHAIN=1`); the default suite stays offline in
every sense.

## Diagnosing a "hang"

Suspect the keychain before the portal. Reads answer in well under a second;
the multi-minute waits were macOS parking the process inside `securityd` while
a permission dialog waited for a click, because an ad-hoc-signed rebuild had
revoked the "Always Allow" grant. `src/diag.rs` puts a line on stderr when a
keychain or portal wait turns long — every `CredentialStore` touch should go
through `diag::keychain`, so no wait is ever silent. It never shortens a wait;
`--timeout` bounds it.

## Portal-specific gotchas

- An expired session returns a **302 to the login page, not a 401**. The client
  treats "landed on `/users/…`" or an HTML body where JSON was expected as
  `CliError::Auth`. Don't "fix" that by only checking status codes.
- Document `download_url`s are **pre-signed S3 links that expire in ~5 minutes**
  — fetch them immediately; never persist one, and never commit one.
- The portal's `type` filter takes snake case (`cash_in`) while responses use
  camel case (`cashIn`). Both appear in the code on purpose. Worse, the filter
  accepts *only* the two cash kinds — `type=disbursement` returns HTTP 500 —
  so `transactions.rs` pages and filters that kind client-side. Don't
  "simplify" it back into a query parameter.
- Two-factor cannot be automated away: AppFolio binds device trust to browser
  fingerprinting. Don't add a "skip 2FA" path; it has been tested and fails.

## Safety & privacy (written as if this repo were public)

- Never commit a password, session cookie, pre-signed URL, real name, address,
  balance, or account/property ID.
- `tests/fixtures/` are **scrubbed** captures: structure preserved exactly,
  every identifying and financial value replaced with an obvious dummy. The
  policy is in `tests/fixtures/README.md`, and `fixtures_carry_no_real_
  identifiers` in `tests/fixture_shapes.rs` enforces it. Extend that test's
  banned list when you add a new kind of identifier.
- Don't paste real portal output into an issue, commit message, or doc example.
  The README's examples use scrubbed figures.

## Definition of done

`make verify` green, tests cover the change, `--json` and human output both
updated, `docs/api.md` still matches reality, and no secrets or PII anywhere in
the diff — including fixtures.
