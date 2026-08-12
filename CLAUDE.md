# CLAUDE.md

The canonical agent guide for this repo is **[AGENTS.md](AGENTS.md)** — read it
first. It covers build/test/lint, layout, conventions, and the safety rules.

Claude Code specifics:

- **Gate on `make verify`.** Don't report a change as done until it's green
  (fmt + clippy `-D warnings` + tests + smoke). Tests are fully offline.
- **This CLI is read-only.** Don't add a command that writes to the portal
  without being asked; contributions, approvals, and profile edits are
  deliberately out of scope.
- **Secrets:** the portal password, session cookie, and 2FA device token live
  in the OS keychain (`piekstra.rpmfl`). Never print one, put it on argv, or
  write it to a file — including while debugging.
- **Testing against the live portal costs a text message.** When the cached
  session expires, `auth login` needs a 2FA code from the user's phone, and
  AppFolio rate-limits code requests. Prefer the offline fixtures; ask before
  burning a code.
- **"Deployed" means released + installed.** A change isn't live until a
  release is cut (tag `v*` → the release workflow) and the binary is installed
  or `self-update`d on the target machine.
- **Written as if public.** No secrets, real names, addresses, balances, or
  property IDs in any diff — including test fixtures (dummies only).
