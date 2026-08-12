# Fixtures

Real responses captured from the AppFolio owner portal (`/oportal/api/*`) on
**2026-08-06**, then scrubbed. `tests/fixture_shapes.rs` asserts that the
fields the table views read still exist, so a portal-side rename fails the
build instead of silently emptying a column.

## Scrubbing policy

This repo is written as if it were public. No fixture may carry anything that
identifies a real account, person, property, or balance.

Structure is preserved **exactly** — key names, nesting, types, null-vs-absent
— because that is what the tests assert on. Values are replaced:

| Value | Replaced with |
| --- | --- |
| Owner / tenant / vendor names | `Sample Owner 1`, `Sample Tenant One`, `Sample Property Management` |
| Street addresses | `100 Sample St, Sample City, ST 00000` |
| Property display names | `Sample St 100` |
| Transaction descriptions | `Sample transaction description` |
| Money (`amount`, `balance`, `totalAmount`, rent, reserves) | round dummies (`1000.0`, `5000.0`, `500`) |
| Record IDs (property, unit, lease, owner, party) | small sequential dummies |
| UUIDs | `00000000-0000-4000-8000-…` |
| Pre-signed S3 `download_url` / `display_url` | `https://example.invalid/presigned-url-redacted` |
| File sizes | `1024` |

Dates, booleans, enum-ish strings (`cashIn`, `disbursement`), file names, and
content types are kept verbatim — they carry no personal information and the
parsing logic depends on their exact form.

## Refreshing

Capture with `rpmfl api <path> > raw.json`, then scrub by hand against the
table above before committing. Never commit a raw capture: pre-signed URLs are
live credentials for ~5 minutes, and the account data is real.
