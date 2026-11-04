# Changelog

All notable changes to `rpmfl` are documented here. Newest first. The project follows
[SemVer](https://semver.org/); until 1.0, minors carry the breaking-change signal.

## 0.3.4 — 2026-11-04

- The read budget, the offset walk's dedupe and the note for a server on `$RPMFL_MCP_TOKEN` come from cli-common, which took them from this crate.
- cli-common v0.29.0, up from v0.27.0.

## 0.3.3 — 2026-11-03

- `rpm_transactions`, `rpm_rent` and `rpm_documents` page by position, so a row that arrives between
  pages repeats nothing and skips nothing. Rows of the same day now run in a fixed order of their
  own. A cursor from an earlier version still works, as the count of rows it holds.
- `rpm_transactions` reads the whole period on each page, and asks for a narrower one past 90 seconds
  or 5,000 entries. A row posted during that read comes back once. One removed during it can still be
  missed, since the portal pages by offset.
- A row with no `id` is now an upstream error, since the id decides a row's position.
- `auth logout --forget` attempts every step and reports every failure, where the first failure used
  to stop the rest. When it removed a connector token, it says a running `rpmfl mcp --http` keeps
  accepting that token until it stops, whatever else failed.
  When the keychain held none, it says so and names `$RPMFL_MCP_TOKEN`.
- On Windows, a session too long for one credential is split and read back as one.
- cli-common v0.27.0, up from v0.24.0.

## 0.3.2 — 2026-11-03

- The portal host must be `https`, or `http` to this machine, before a password or a
  session goes to it. This covers `base_url`, `$RPMFL_BASE_URL`, a full URL given to
  `rpmfl api` and a document link. A tool refuses a host off `https` before it reads the
  keychain.
- A redirect that leaves `https` is an error, and the sign-in forms, which carry the
  password or a code, follow a redirect only on the portal's host. `rpmfl auth login`
  refuses a host off `https` before it reads or writes the stored password.
- A network failure names the portal's origin, not the whole URL, and says why a redirect
  was refused.
- CI: the workflow's token is read-only at the top level.
- cli-common v0.24.0, up from v0.23.0.

## 0.3.1 — 2026-11-03

- README and SECURITY.md say plainly what the memory-only session means: Claude starts a
  new server for each launch or session, and each one starts from the session last stored,
  which only `rpmfl` commands advance. `rpmfl auth status --verify` advances it without
  signing in, as long as it still works.
- `auth logout --forget` removes the `mcp --http` token through the name cli-common writes
  it under, not a copy of that name.
- Release workflow: the release action is pinned to a commit.
- cli-common v0.23.0, up from v0.21.0. A failed keychain call from a tool now reads "the OS
  keychain could not be used".

## 0.3.0 — 2026-11-03

- `rpmfl mcp`: five read-only tools for Claude: `rpm_summary`, `rpm_properties`,
  `rpm_transactions`, `rpm_rent` and `rpm_documents`. They run over stdio for Claude Code
  and Claude Desktop, and with `--http` on 127.0.0.1 behind a bearer token for Claude on
  the web and mobile through your own Cloudflare Worker and tunnel. No tool can approve
  an estimate, sign a form, make a contribution or change anything at the portal.
- `rpmfl mcp` reads `$RPMFL_SESSION` before the keychain. It never signs in, sends the
  password, asks for a two-factor code or writes the keychain, and reads the config and
  the session on each call. A changed portal answer is an error rather than a zero total
  or an empty list. Every other command is unchanged.
- Releases attach `rpmfl-<version>.mcpb`, a Claude Desktop extension, and a Windows
  x86_64 tarball.
- `self-update` says to restart a running `rpmfl mcp --http` and to install the new
  `.mcpb`.
- cli-common v0.21.0, up from v0.15.1. Rust 1.89 or later.
- Fixtures: lease and move-in dates are now dummies.
