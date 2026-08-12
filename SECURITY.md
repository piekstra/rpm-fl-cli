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
- **Pre-signed document URLs.** The portal returns S3 links that grant
  unauthenticated access to a document for ~5 minutes. `rpmfl documents get`
  fetches immediately; `--url` prints one only when explicitly asked. Never
  paste one into an issue or commit it.

## What this tool does not do

- It never mutates the portal — no payments, approvals, or profile changes.
- It talks only to the configured portal host and the S3 links that host
  returns. No telemetry, no third-party services.
- It hardcodes no secrets. `gitleaks` runs in CI over the full history.

## Handling credentials safely

Prefer piping from a password manager over typing:

```console
$ op read 'op://Private/your-portal-item/password' \
    | rpmfl auth set-credential --stdin --overwrite
```

`rpmfl auth logout --forget` removes the password, session, and device token
from the keychain and clears the config.
