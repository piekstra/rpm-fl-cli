# Changelog

All notable changes to `rpmfl` are documented here. Newest first. The project follows
[SemVer](https://semver.org/); until 1.0, minors carry the breaking-change signal.

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
