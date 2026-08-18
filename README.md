# rpmfl — Real Property Management owner portal CLI

[![CI](https://github.com/piekstra/rpm-fl-cli/actions/workflows/ci.yml/badge.svg)](https://github.com/piekstra/rpm-fl-cli/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/piekstra/rpm-fl-cli?sort=semver)](https://github.com/piekstra/rpm-fl-cli/releases)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

Read your rental portfolio from the command line: properties and leases, the
owner ledger, rent charges, unpaid bills, 1099s and management agreements —
all from the AppFolio owner portal your property manager gives you. Fast,
keychain-secured, agent-friendly.

Every command is a **read**. Nothing in this CLI moves money, approves an
estimate, or changes a setting.

> Unofficial and not affiliated with AppFolio or Real Property Management.
> It drives the portal's own undocumented JSON endpoints, which can change
> without notice. See [`docs/api.md`](docs/api.md).

## Install

```console
# from source (recent stable Rust; MSRV 1.88)
$ cargo install --git https://github.com/piekstra/rpm-fl-cli --locked
$ make install        # cargo install --path . --force
```

Prebuilt tarballs ship with each release; `rpmfl self-update` pulls the latest.

## Quick start

```console
$ rpmfl config set base_url https://<subdomain>.appfolio.com   # your portal's URL
$ rpmfl config set username you@example.com
$ rpmfl auth login          # prompts for password, then the 2FA code
$ rpmfl summary
```

`auth login` stores your password in the OS keychain and caches the portal
session there too. Later commands reuse the session; when the portal expires
it, any command exits 3 and tells you to run `auth login` again.

`rpmfl auth status` answers from local state — it reports whether a session is
*stored*, which stays "yes" after the portal has quietly expired it. Add
`--verify` to spend one cheap read and get a truthful answer (exit 3 when the
session is dead), which is what a script should gate on.

To load the password from a password manager instead of typing it:

```console
$ op read 'op://Private/<your-portal-item>/password' \
    | rpmfl auth set-credential --stdin --overwrite
```

### Two-factor

AppFolio challenges unrecognized devices. On a terminal, `rpmfl auth login`
sends a code and prompts for it. Without a TTY — a script, CI, or an editor's
shell — it sends the code, parks the half-finished login, and exits 3 telling
you to finish with:

```console
$ rpmfl auth login --code 123456
```

That second run resumes the parked session rather than starting a new one,
because the portal only honors a code against the session that requested it.
`--channel call` switches from SMS to a voice call when texts are slow.

After one successful login, `rpmfl` stores the portal's 30-day "remember this
device" token, so a lapsed session usually renews with no code at all — you
should only be asked roughly once a month. [`docs/api.md`](docs/api.md)
covers the device-trust behaviour and the send-code endpoint, which is an XHR
rather than the form the page appears to submit.

## Commands

```console
rpmfl summary [--since 2026-01-01] [--until 2026-08-06] # cash in/out, disbursements, unpaid bills
rpmfl properties list                                   # addresses, units, occupancy, lease end
rpmfl properties get <ID>                               # one property in detail
rpmfl ownerships                                        # who owns what, and what share
rpmfl transactions list [--type cash-in] [--limit N]    # the owner ledger
rpmfl charges [--since …] [--until …]                   # rent charged to tenants
rpmfl bills list                                        # unpaid bills
rpmfl bills balances                                    # outstanding balance per property
rpmfl documents list                                    # 1099s, W-9s, agreements, insurance
rpmfl documents get <ID> [-o FILE] [--url]              # download one
rpmfl statements [--limit N]                            # published owner packets
rpmfl approvals [--page N]                              # estimates awaiting your decision
rpmfl forms                                             # documents needing a signature
rpmfl api <PATH> [--query k=v]                          # raw portal passthrough
rpmfl auth login|status [--verify]|logout|set-credential # credentials and session
rpmfl config path|show|set|unset                        # non-secret settings
rpmfl completions <shell> | rpmfl info | rpmfl self-update
```

Global flags: `--json`, `-v/--verbose`, `-q/--quiet`, `--no-color`,
`--timeout <SECS>`.

### When a command seems to hang

It is usually the OS keychain, not the portal. macOS scopes an "Always Allow"
grant to the binary's code signature, so an unsigned rebuild silently revokes
it and the next run parks inside `securityd` waiting for a permission dialog —
with nothing printed. `rpmfl` now names that wait on stderr after a few
seconds, and `-v` times every request:

```console
$ rpmfl -v properties list
rpmfl: waiting on the OS keychain — if macOS is showing a permission dialog, choose "Always Allow" so future runs skip it (4s elapsed)
rpmfl: GET /oportal/api/owner_properties -> HTTP 200 in 0.31s
```

Choose "Always Allow" once and it stops. Build with `make release` / `make
install` rather than bare `cargo`, so the binary keeps the stable signing
identity. `--timeout <SECS>` (default 45) bounds a stalled request;
`rpmfl config set timeout_secs 20` makes a smaller budget stick, which matters
for tool wrappers with a 60s limit.

## Examples

A period overview:

```console
$ rpmfl summary --since 2026-01-01
cash_in: 24000.0
cash_out: 3000.0
disbursements: 18000.0
net: 21000.0
period:
  end: 08/06/2026
  start: 01/01/2026
unpaid_bills: 0.0
```

The ledger, filtered and machine-readable:

```console
$ rpmfl transactions list --type cash-out --limit 3
POSTEDON | TYPE | AMOUNT | PARTYNAME | DESCRIPTION
2026-07-09 | cashOut | 350.0 | Sample Property Mgmt | Replace CO detectors
2026-07-09 | cashOut | 300.0 | Sample Property Mgmt | Management fees

$ rpmfl --json transactions list --type cash-out --limit 3 \
    | jq '[.transactions[] | {postedOn, amount, description}]'
```

Pull this year's 1099:

```console
$ rpmfl documents list
ID   | NAME                 | CATEGORY             | DATE       | SIZE
1001 | Owner_1099_2025.pdf  | shared               | 2026-01-18 | 34099
1002 | Management Agreement | management-agreement | 2025-04-08 |

$ rpmfl documents get 1001 -o ~/Documents/taxes/1099-2025.pdf
```

`documents` follows the family's shared **`documents/v1`** profile: `list`
emits a `document-list/v1` envelope (`.items[]` of `{id, name, category, date}`,
with rpmfl's `shared_at`/`size`/`content_type`/`folder_name` kept as provider
extras) and `get` emits `document-download/v1`. Every read takes `--json`, which emits the
portal payload plus a `schema` tag so downstream tools can version against it.

## Configuration

Non-secret settings live in `~/.config/rpmfl/config.json` (`rpmfl config
path`): `base_url` and `username`. Secrets never go there — the password,
session cookie, and device token live in the OS keychain under service
`piekstra.rpmfl`. `RPMFL_BASE_URL` and `RPMFL_USERNAME` override the
configured values.

`base_url` has no default. AppFolio issues a subdomain per property-management
company, so there is no host that would be right for everyone — and hardcoding
one would disclose whose portal a given install talks to. Set it once; it
stays in your local config, which is outside this repo.

## Exit codes

`0` ok · `2` usage · `3` auth (log in again) · `4` not found · `5` portal or
network failure · `1` anything else.

## Development

```console
$ make verify     # fmt-check + clippy -D warnings + tests + offline smoke
$ make dev        # debug build, re-signed so keychain grants survive rebuilds
```

Tests are offline: a black-box surface suite plus contract tests over captured
portal responses in `tests/fixtures/`, which are scrubbed of real names,
addresses, balances, and IDs (see
[`tests/fixtures/README.md`](tests/fixtures/README.md)).

## License

MIT OR Apache-2.0.
