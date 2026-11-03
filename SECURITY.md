# Security Policy

## Reporting a vulnerability

Please report security issues privately via GitHub's
[security advisories](https://github.com/piekstra/rpm-fl-cli/security/advisories/new)
rather than a public issue.

## Threat model

`rpmfl` authenticates to a third-party property-management portal on behalf of
one owner and reads their financial data. The things worth protecting:

- **The portal password**, stored in the OS keychain under service
  `piekstra.rpmfl`, account `password`. It is read at point of use, never
  logged, never placed on argv, and never written to disk.
- **The cached session cookie** (account `session`) and **2FA device token**
  (account `device-token`). Both are credentials: anyone holding the session
  cookie can read the account until the portal expires it. They live in the
  keychain and are redacted from all output.
- **The `rpmfl mcp --http` bearer token** (account `mcp-http-token`), which lets
  your own connector reach the tools on this machine.
- **Pre-signed document URLs.** The portal returns S3 links that grant
  unauthenticated access to a document for ~5 minutes. `rpmfl documents get`
  fetches immediately; `--url` prints one only when explicitly asked. Never
  paste one into an issue or commit it.

## The MCP server

`rpmfl mcp` serves read-only tools to an MCP client (Claude Code, Claude Desktop) over
stdio; it opens no port unless run with `--http` (below). **What the tools return goes
to Claude on every path**, as part of your conversation: portfolio totals, property
names and addresses, rent and lease end dates, ledger entries with the names of tenants
and vendors and their descriptions, rent charges and balances, how many approvals and
forms are waiting, and the names and folders of documents your manager shared. Portal
record ids are held back unless the model asks for `detailed`, which adds them along
with a property's minimum cash reserve. The links the portal issues on documents and
ledger attachments need no sign-in, so no tool returns one, in any format. No tool reads
someone else's records: each read is the signed-in owner's own portal view, and a
`property` argument is matched against the properties that view lists.

It reads the cached session from `$RPMFL_SESSION`, then the keychain, on each call. A
tool never signs in, prompts, sends your password, reads the device token, asks for a
two-factor code, or creates or changes a keychain item, so a background server cannot
lock your account by retrying a stale password or send you text messages, however often
it restarts. The portal may rotate the session cookie on a response; the server keeps
the new copy in memory until the stored session changes, and never writes it to the
keychain, since a write from a background server could block on a dialog. Only `rpmfl`
commands advance the stored session, and every new server starts from it, which on stdio
means each launch or session of the client. So if it has gone stale, `rpmfl auth login` is
needed even though the session was in use. A missing or expired session is an error asking the user to run `rpmfl auth login` in a
terminal; while `$RPMFL_SESSION` is set, the keychain is not read, so the error asks the
user to replace or unset the variable instead. The Claude Desktop extension (`.mcpb`)
takes no settings and so holds no credential of its own; on macOS its bundled binary
needs the user's approval, once, to read the `piekstra.rpmfl` keychain item.

The session goes to one host: `base_url` in `~/.config/rpmfl/config.json`, or
`$RPMFL_BASE_URL`. No file in a project or the working directory can redirect it.

Every tool's read goes through one seam that offers GETs to fixed paths alone.
`tests/mcp_readonly.rs` drives every tool through the real binary against a stub portal
and fails on anything but a GET to a listed read.

`rpmfl mcp --http --port <PORT>` serves the same tools over HTTP for the user's own
Cloudflare Worker, which reaches it through the user's own Cloudflare Tunnel
(cli-common's `connector/`). It binds 127.0.0.1 only, with no flag for another address,
and refuses to start without a bearer token of 32 or more characters, from
`RPMFL_MCP_TOKEN` or the keychain item `mcp-http-token` (`piekstra.rpmfl`), which `rpmfl
mcp --new-token` makes from 256 random bits and replaces only with `--overwrite`. Every
request must carry the token (compared in constant time) and a `Host` of `127.0.0.1` or
`localhost`, and must not carry an `Origin`; there are no CORS headers. Only the
read-only tools are served (all five are). The portal session stays on your computer:
the Worker sends tool calls and receives results, and never sees it.

**Tool results also cross Cloudflare on this path**, through your tunnel and your
Worker, on their way to Claude. A tool result names tenants and vendors, and gives
property addresses and amounts, so treat the connector as you would anything that reads
those. Cloudflare is your own account's, and we run no shared service.

## What this tool does not do

- It never mutates the portal — no payments, approvals, or profile changes, through a
  command or a tool.
- It talks only to the configured portal host and the S3 links that host
  returns. No telemetry, no third-party services.
- It hardcodes no secrets. `gitleaks` runs in CI over the full history.

## Handling credentials safely

Prefer piping from a password manager over typing:

```console
$ op read 'op://Private/your-portal-item/password' \
    | rpmfl auth set-credential --stdin --overwrite
```

`rpmfl auth logout --forget` removes the password, session, device token and `mcp
--http` token
from the keychain and clears the config. It cannot reach a running `rpmfl mcp --http`,
which read its token at the start and keeps accepting it until it stops, or a token in
`$RPMFL_MCP_TOKEN`: stop the server (`launchctl bootout` for its launchd agent), and unset
the variable if it is set.
