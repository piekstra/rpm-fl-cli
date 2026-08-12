# AppFolio owner portal — observed API

Reverse-engineered from the portal's own XHR traffic at
`https://<subdomain>.appfolio.com/oportal` on 2026-08-06. Unofficial and
undocumented: AppFolio publishes no owner-portal API, and any of this can
change without notice. `rpmfl api <path>` is the escape hatch for checking.

## Authentication

The portal is a Rails app. Auth is **cookie-only** — there is no bearer token,
API key, or refresh flow.

| Step | Request | Notes |
| --- | --- | --- |
| 1 | `GET /oportal/users/log_in` | Returns the form carrying `authenticity_token` (Rails CSRF) and a fresh `_oportal_session` cookie. |
| 2 | `POST /oportal/users/log_in` | Fields: `authenticity_token`, `user[email]`, `user[password]`, `require_reverification=true`, `commit=Log in`. Needs `Origin` + `Referer`. |
| 3 | → `/oportal/dashboard` | Trusted device. Done. |
| 3′ | → `/oportal/users/two_factor/new` | Untrusted device; continue below. |
| 4 | `POST /oportal/users/two_factor/create_token` | **An XHR, not a submit of the visible form.** Fields are its own: `number`, `two_factor_method` (`sms`/`call`), `email_2fa`, `fingerprint`, `dummy_value=not_used`. |
| 5 | `POST /oportal/users/two_factor` | The verify form's real action — a plain form POST carrying `user[verification_code]` and `user[remember_my_device]`. |

### The send-code trap

The 2FA page renders a form with a "Send verification code" button, but that
button is wired to jQuery (`Devise2fa.prototype.sendCode` in the vendor
bundle), which fires the `create_token` XHR above. Submitting the visible
form's fields to `/oportal/users/two_factor` instead gets them handled as a
login attempt: the response is a 200 carrying "The email and password
combination you entered is invalid", and **no code is sent**. A client that
trusts the status code will cheerfully report a text that never went out.

Equally, the "A verification code was sent to a number ending in NNNN" banner
is painted by that XHR's success handler — it is *not* server-rendered. Do not
re-fetch the 2FA page to confirm a send; it always comes back showing the
pre-send step. Take the destination from the form's `user[phone_number]`
field instead.

After step 3/5 the `_oportal_session` cookie authenticates every
`/oportal/api/*` read on its own.

### Device trust does work — if the CLI mints the token itself

Ticking "remember this device" issues a 30-day `2fa_user_token` cookie, and a
CLI-issued one **does** skip the challenge on later logins. Verified: after a
completed `rpmfl auth login`, deleting the cached session and logging in again
returned "device recognized — no code needed", repeatably.

The earlier conclusion here was wrong, and the reason is worth recording. A
token minted *in a browser* is bound to the fingerprint that browser computed
(AppFolio ships `af_fingerprint.js` alongside ThreatMetrix), so replaying it
from an HTTP client is rejected however faithfully the cookie jar, User-Agent,
CSRF token, `Origin` and `Referer` are reproduced. That experiment was sound;
generalising it to "device trust cannot work for a CLI" was not. Once the CLI
presents its *own* fingerprint to `create_token`, the token it receives is
bound to that fingerprint and works for the CLI.

So the practical model is: a code is needed once per 30 days, not once per
session. `rpmfl` still caches the session cookie, which avoids a login round
trip on every invocation, and stores the device token so a lapsed session
usually renews silently.

### Session rotation

Rails re-issues `_oportal_session` whenever it feels like it, so the cookie a
client holds drifts from the one the portal considers current. `rpmfl` writes
the rotated value back to the keychain after every successful read; a client
that kept replaying its original cookie would eventually be logged out even
though the portal had just handed it a live session.

### Session expiry

Expiry shows up two ways, and both are treated as `CliError::Auth` (exit 3):

- A `302` to the login page instead of a 401 — Rails redirects the XHR, so the
  client checks whether it *landed* on `/users/log_in` or `/users/two_factor`,
  and also catches an HTML body where JSON was expected.
- A plain `401` on `/oportal/api/*` when the cookie is recognized but no longer
  valid.

## Endpoints

All are `GET`, return JSON, and live under `/oportal/api/`. Dates are
`MM/DD/YYYY`; the CLI accepts ISO `YYYY-MM-DD` and converts.

| Endpoint | Query params | Returns | CLI command |
| --- | --- | --- | --- |
| `owner_ownerships` | — | Ownership records: owner, property, `percent_owned`, units | `rpmfl ownerships` |
| `owner_properties` | — | Properties with address, units, occupancy, lease end, rent, cash reserve | `rpmfl properties list` |
| `owner_transactions` | `start_on`, `end_on`, `limit`, `offset`, `property_ids`, `type` | Ledger entries | `rpmfl transactions list` |
| `owner_income_balances` | `start_on`, `end_on`, `investor_scoped` | Period totals, `type: cashIn` | `rpmfl summary` |
| `owner_expenses_balances` | `start_on`, `end_on`, `investor_scoped` | Period totals, `type: cashOut` / `disbursement` (one row per owner) | `rpmfl summary` |
| `tenant_charges` | `start_on`, `end_on`, `balance_as_of` | Rent charges with outstanding balance | `rpmfl charges` |
| `unpaid_bills` | `due_on_end`, `limit`, `offset`, `property_ids` | Outstanding bills | `rpmfl bills list` |
| `unpaid_bills_balances` | `due_on_end` | Balance per property | `rpmfl bills balances` |
| `shared_documents/` | — | Documents grouped by owner and kind (note the trailing slash) | `rpmfl documents list` |
| `owner_documents` | `limit` | Published owner statement packets | `rpmfl statements` |
| `owner_decision_requests` | `page_number` | Estimate approvals + `pagination.total_items` | `rpmfl approvals` |
| `pdf_forms/actionable_documents` | — | Forms awaiting signature, grouped by owner | `rpmfl forms` |

### The `type` filter is asymmetric

`owner_transactions` reports three kinds in its responses — `cashIn`,
`cashOut`, `disbursement` (camel case) — but its `type` **query parameter**
only accepts the two cash kinds, in snake case:

| `type=` | Result |
| --- | --- |
| `cash_in` | 200, filtered |
| `cash_out` | 200, filtered |
| `disbursement` | **HTTP 500** |
| `disbursements`, `owner_disbursement` | HTTP 500 |

So `rpmfl transactions list --type disbursement` omits the parameter, pages
the unfiltered endpoint, and filters client-side; `--offset` then counts
within the matching rows. Disbursements also show up in
`owner_expenses_balances` as a `disbursement` row per owner, which is where
`rpmfl summary` gets its total.

Amounts are bare JSON numbers, not strings.

### Shapes worth knowing

`owner_properties` and `owner_ownerships` use a JSON:API-ish nesting —
`{id, type, attributes, relationships}` — while the ledger and balance
endpoints return flat rows. The CLI flattens the former for its table views;
`--json` preserves whatever the portal sent, plus a `schema` tag.

`shared_documents/` splits documents across five sibling arrays
(`documents`, `management_agreement_documents`,
`uploaded_pdf_management_agreements`, `pdf_template_management_agreements`,
`insurance_policy_documents`). Uploaded documents date themselves with
`shared_at`; generated ones use `sent_at`.

`download_url` values are **pre-signed S3 links that expire in ~5 minutes**,
so `rpmfl documents get` fetches immediately rather than printing a link.

## Non-API pages

`/oportal/statements`, `/oportal/transaction_history`, `/oportal/documents`,
`/oportal/properties`, `/oportal/owner_contributions`,
`/oportal/estimate_approvals`, and `/oportal/account_settings` are server-
rendered shells whose data comes from the endpoints above. `account_settings`
has no JSON endpoint — it links to password and bank-account forms, both of
which are writes and therefore out of scope.
