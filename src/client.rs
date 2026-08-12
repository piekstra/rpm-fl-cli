//! HTTP client for the AppFolio owner portal (`/oportal/*`).
//!
//! AppFolio publishes no owner-portal API. Everything here targets the same
//! JSON endpoints the portal's own front end calls (`/oportal/api/*`), mapped
//! by watching the site's XHR traffic. See `docs/api.md`.
//!
//! # Auth model
//!
//! The portal is a Rails app: a form login sets an `_oportal_session` cookie,
//! and every `/oportal/api/*` read authenticates with that cookie alone —
//! no bearer token, no header, no refresh flow.
//!
//! Two-factor is the wrinkle. An unrecognized device is bounced to
//! `/users/two_factor/new` and must clear an SMS or voice-call code. Sending
//! that code is an XHR to `two_factor/create_token` with its own field names —
//! *not* a submit of the form the page displays, which lands on the verify
//! endpoint and is read as a login attempt.
//!
//! Ticking "remember this device" issues a 30-day `2fa_user_token` cookie, and
//! one the CLI minted for itself **does** skip later challenges: it is bound to
//! the fingerprint presented at `create_token`, which we supply. (A token
//! minted in a *browser* is bound to that browser's computed fingerprint and is
//! rejected when replayed from here, however faithfully the cookie jar and
//! headers are reproduced — that is a different thing, and conflating the two
//! is what made this look impossible at first.)
//!
//! So the CLI caches the session cookie in the OS keychain for fast reads, and
//! keeps the device token so a lapsed session usually renews without a code.

use std::sync::Arc;
use std::time::Duration;

use pk_cli_core::CliError;
use pk_cli_secrets::{CredentialStore, Secret};
use reqwest::cookie::CookieStore;
use serde_json::Value;

use crate::config::{Config, DEVICE_TOKEN_ACCOUNT, SESSION_ACCOUNT};

/// A recent desktop Chrome UA. The portal's edge rejects obviously-bot clients.
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

pub const SESSION_COOKIE: &str = "_oportal_session";
pub const DEVICE_COOKIE: &str = "2fa_user_token";

const LOGIN_PATH: &str = "/oportal/users/log_in";
const TWO_FACTOR_NEW_PATH: &str = "/oportal/users/two_factor/new";
/// Dispatching a code is an XHR to its own endpoint with its own field names —
/// *not* a submit of the visible form. Captured from the portal's own traffic;
/// posting the form's fields at the verify endpoint instead gets them handled
/// as a login attempt and silently sends nothing.
const TWO_FACTOR_CREATE_TOKEN_PATH: &str = "/oportal/users/two_factor/create_token";
/// The verify form's own action.
const TWO_FACTOR_VERIFY_PATH: &str = "/oportal/users/two_factor";

/// How the portal should deliver a 2FA code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CodeChannel {
    /// Text message (the portal's default).
    Sms,
    /// Automated voice call — useful when SMS is delayed or rate-limited.
    Call,
}

impl CodeChannel {
    /// The value the 2FA form posts for this channel.
    pub fn form_value(self) -> &'static str {
        match self {
            CodeChannel::Sms => "sms",
            CodeChannel::Call => "call",
        }
    }
}

/// Where a password login ended up.
pub enum LoginOutcome {
    /// Fully authenticated; the session cookie is live.
    Authenticated,
    /// The portal demanded a 2FA code before finishing.
    TwoFactorRequired,
}

/// An owner-portal session.
pub struct Portal {
    http: reqwest::blocking::Client,
    jar: Arc<reqwest::cookie::Jar>,
    base: String,
    /// Keychain write-back for a rotated session cookie. Rails re-issues
    /// `_oportal_session` as it pleases; if we kept replaying the value we
    /// were seeded with, the cached session would go stale even though the
    /// portal handed us a live one on the previous call.
    sync: Option<SessionSync>,
}

struct SessionSync {
    creds: CredentialStore,
    last: std::cell::RefCell<String>,
}

impl Portal {
    /// A client with an empty cookie jar and no keychain write-back.
    pub fn new(base: impl Into<String>) -> Result<Self, CliError> {
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let http = reqwest::blocking::Client::builder()
            .user_agent(UA)
            // Total request budget plus an explicit connect budget, so a
            // stalled TLS handshake fails fast instead of hanging the CLI.
            .timeout(Duration::from_secs(45))
            .connect_timeout(Duration::from_secs(15))
            .cookie_provider(jar.clone())
            .build()
            .map_err(|e| CliError::Other(format!("failed to build HTTP client: {e}")))?;
        Ok(Portal {
            http,
            jar,
            base: base.into(),
            sync: None,
        })
    }

    /// Replay a cached session (and device token) from the keychain. The
    /// session is *not* verified here; the first API call surfaces expiry.
    ///
    /// The returned client writes a rotated session cookie back to the
    /// keychain, so a long-lived login survives the portal re-issuing it.
    pub fn from_cached_session(cfg: &Config, creds: &CredentialStore) -> Result<Self, CliError> {
        // Resolve the host before touching the keychain (SPEC §1.5): on a
        // fresh install both are missing, and "configure a portal host" is the
        // step the user has to take first. Reading the keychain first reports
        // "no session stored — run auth login", which sends them down a path
        // that cannot succeed yet.
        let mut portal = Portal::new(cfg.base_url()?)?;
        let session = creds.get(SESSION_ACCOUNT)?.ok_or_else(|| {
            CliError::Auth("no portal session stored — run `rpmfl auth login`".into())
        })?;
        portal.seed_cookie(SESSION_COOKIE, session.expose());
        if let Some(token) = creds.get(DEVICE_TOKEN_ACCOUNT)? {
            portal.seed_cookie(DEVICE_COOKIE, token.expose());
        }
        portal.sync = Some(SessionSync {
            creds: CredentialStore::new(creds.service()),
            last: std::cell::RefCell::new(session.expose().to_string()),
        });
        Ok(portal)
    }

    /// Persist the session cookie if the portal rotated it. Best-effort: a
    /// keychain hiccup must not fail a read that already succeeded.
    fn sync_session(&self) {
        let Some(sync) = &self.sync else { return };
        let Some(current) = self.cookie(SESSION_COOKIE) else {
            return;
        };
        if *sync.last.borrow() == current {
            return;
        }
        if sync
            .creds
            .set(SESSION_ACCOUNT, &Secret::new(current.clone()))
            .is_ok()
        {
            *sync.last.borrow_mut() = current;
        }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else if path.starts_with('/') {
            format!("{}{}", self.base.trim_end_matches('/'), path)
        } else {
            format!("{}/{}", self.base.trim_end_matches('/'), path)
        }
    }

    fn base_url(&self) -> reqwest::Url {
        self.base
            .parse()
            .expect("base URL validated when the client was built")
    }

    /// Inject a cookie into the jar for the portal's host.
    fn seed_cookie(&self, name: &str, value: &str) {
        self.jar
            .add_cookie_str(&format!("{name}={value}; Path=/; Secure"), &self.base_url());
    }

    /// Read a cookie value back out of the jar.
    fn cookie(&self, name: &str) -> Option<String> {
        let header = self.jar.cookies(&self.base_url())?;
        let raw = header.to_str().ok()?;
        raw.split("; ")
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
            .map(str::to_string)
    }

    /// The live session cookie, for persisting after a successful login.
    pub fn session_cookie(&self) -> Option<Secret> {
        self.cookie(SESSION_COOKIE).map(Secret::new)
    }

    /// The 30-day device-trust token, if the portal issued one.
    pub fn device_token(&self) -> Option<Secret> {
        self.cookie(DEVICE_COOKIE).map(Secret::new)
    }

    /// Replay a stored device-trust token before logging in.
    pub fn seed_device_token(&self, token: &str) {
        self.seed_cookie(DEVICE_COOKIE, token);
    }

    /// Adopt a parked half-finished login session, so a two-factor code can be
    /// verified against the session that requested it.
    pub fn seed_session(&self, session: &str) {
        self.seed_cookie(SESSION_COOKIE, session);
    }

    /// Fetch a binary document. Attachment URLs are pre-signed S3 links, so
    /// they carry their own auth and must not get the portal's cookies.
    pub fn download(&self, url: &str) -> Result<Vec<u8>, CliError> {
        let resp = self
            .http
            .get(url)
            .send()
            .map_err(|e| CliError::Upstream(format!("download failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CliError::Upstream(format!(
                "download returned HTTP {} (pre-signed link may have expired)",
                status.as_u16()
            )));
        }
        resp.bytes()
            .map(|b| b.to_vec())
            .map_err(|e| CliError::Upstream(format!("reading download body: {e}")))
    }

    // ---- Authentication ----------------------------------------------------

    /// Fetch a page and return its HTML (used to harvest CSRF tokens).
    fn get_html(&self, path: &str) -> Result<(String, String), CliError> {
        let resp = self
            .http
            .get(self.url(path))
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .send()
            .map_err(|e| CliError::Upstream(format!("GET {path} failed: {e}")))?;
        let url = resp.url().to_string();
        let html = resp
            .text()
            .map_err(|e| CliError::Upstream(format!("reading {path}: {e}")))?;
        Ok((url, html))
    }

    /// POST a form, following redirects, returning (final URL, body).
    fn post_form(
        &self,
        path: &str,
        referer: &str,
        fields: &[(&str, String)],
    ) -> Result<(String, String), CliError> {
        let resp = self
            .http
            .post(self.url(path))
            .header("Origin", self.base.trim_end_matches('/'))
            .header("Referer", self.url(referer))
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .form(fields)
            .send()
            .map_err(|e| CliError::Upstream(format!("POST {path} failed: {e}")))?;
        let url = resp.url().to_string();
        let body = resp
            .text()
            .map_err(|e| CliError::Upstream(format!("reading {path}: {e}")))?;
        Ok((url, body))
    }

    /// Submit the password login form.
    pub fn login(&self, email: &str, password: &Secret) -> Result<LoginOutcome, CliError> {
        let (_, html) = self.get_html(LOGIN_PATH)?;
        let token = csrf_token(&html).ok_or_else(|| {
            CliError::Upstream("login page had no CSRF token — portal markup changed?".into())
        })?;
        let (url, body) = self.post_form(
            LOGIN_PATH,
            LOGIN_PATH,
            &[
                ("authenticity_token", token),
                ("require_reverification", "true".into()),
                ("user[email]", email.to_string()),
                ("user[password]", password.expose().to_string()),
                ("commit", "Log in".into()),
            ],
        )?;

        // Check the flash before the URL: the portal can bounce a *rejected*
        // login to the two-factor page anyway, so landing there is not by
        // itself proof the password was accepted. Reading the URL first made
        // a bad password look like a device challenge, and the CLI went on to
        // ask for a code that could never be sent.
        if rejected_credentials(&body) {
            return Err(CliError::Auth(
                "the portal rejected your email or password — verify them by signing in at the \
                 portal in a browser, then re-store with \
                 `rpmfl auth set-credential --stdin --overwrite`"
                    .into(),
            ));
        }
        if url.contains("/users/two_factor") {
            return Ok(LoginOutcome::TwoFactorRequired);
        }
        if url.contains("/users/log_in") {
            return Err(CliError::Auth(format!(
                "login did not complete — the portal said: {:?}",
                visible_text(&body, 160)
            )));
        }
        Ok(LoginOutcome::Authenticated)
    }

    /// Ask the portal to send a verification code. Returns the masked
    /// destination it reports, as the number's last four digits.
    pub fn send_code(&self, channel: CodeChannel) -> Result<Option<String>, CliError> {
        let (_, html) = self.get_html(TWO_FACTOR_NEW_PATH)?;
        // Submit the page's own form rather than a guessed endpoint: the 2FA
        // page hosts two forms, and posting the send-code fields at the wrong
        // one gets them handled as a login attempt ("the email and password
        // combination you entered is invalid") while nothing is dispatched.
        let number = hidden_field(&html, "user[phone_number]").ok_or_else(|| {
            CliError::Upstream(
                "the 2FA page carried no phone number — portal markup changed?".into(),
            )
        })?;

        // The portal fingerprints the device in JS and echoes that value in
        // both a cookie and this field. We can't reproduce its fingerprint, so
        // present a random one and keep the cookie consistent with it. Device
        // trust won't survive regardless (see the module docs) — this only has
        // to satisfy the endpoint's shape.
        let fingerprint = random_hex_64();
        self.seed_cookie("af_fingerprint", &fingerprint);

        let resp = self
            .http
            .post(self.url(TWO_FACTOR_CREATE_TOKEN_PATH))
            .header("Origin", self.base.trim_end_matches('/'))
            .header("Referer", self.url(TWO_FACTOR_NEW_PATH))
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Accept", "*/*")
            .form(&[
                ("number", number.as_str()),
                ("two_factor_method", channel.form_value()),
                ("email_2fa", "false"),
                ("fingerprint", fingerprint.as_str()),
                ("dummy_value", "not_used"),
            ])
            .send()
            .map_err(|e| CliError::Upstream(format!("requesting a verification code: {e}")))?;

        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(CliError::Upstream(format!(
                "the portal refused to send a code (HTTP {}): {:?}",
                status.as_u16(),
                visible_text(&body, 160)
            )));
        }
        // Some Rails endpoints answer 200 with an error payload.
        let lower = body.to_lowercase();
        if lower.contains("\"error\"") || lower.contains("\"success\":false") {
            return Err(CliError::Upstream(format!(
                "the portal reported a problem sending the code: {:?}",
                visible_text(&body, 160)
            )));
        }

        // Don't re-fetch the 2FA page to confirm: the "a code was sent"
        // message is painted by the page's own JavaScript after this XHR, so a
        // fresh GET always returns the pre-send step and would look like
        // failure. The endpoint's status is the signal; the destination is
        // simply the number we just asked it to text.
        Ok(last_four(&number))
    }

    /// Submit a verification code, asking the portal to trust this device.
    pub fn verify_code(&self, code: &str) -> Result<(), CliError> {
        let (_, html) = self.get_html(TWO_FACTOR_NEW_PATH)?;
        // Submit the verify form as rendered, so hidden state travels with it.
        let form = form_containing(&html, "id=\"submit_verification_code\"")
            .or_else(|| form_containing(&html, "id=\"two-factor-verification\""))
            .ok_or_else(|| {
                CliError::Upstream(
                    "could not find the verify form on the 2FA page — portal markup changed?"
                        .into(),
                )
            })?;
        let action = form_action(form).unwrap_or_else(|| TWO_FACTOR_VERIFY_PATH.to_string());

        let mut fields = form_inputs(form);
        fields.retain(|(n, _)| {
            n != "user[verification_code]" && n != "user[remember_my_device]" && n != "commit"
        });
        fields.push(("user[verification_code]".into(), code.to_string()));
        fields.push(("user[remember_my_device]".into(), "1".into()));
        fields.push(("commit".into(), "Log In".into()));
        let borrowed: Vec<(&str, String)> = fields
            .iter()
            .map(|(n, v)| (n.as_str(), v.clone()))
            .collect();

        let (url, body) = self.post_form(&action, TWO_FACTOR_NEW_PATH, &borrowed)?;
        if url.contains("/users/two_factor") || url.contains("/users/log_in") {
            let hint = if rejected_credentials(&body) {
                "the portal rejected the login while verifying".to_string()
            } else if body.to_lowercase().contains("invalid") {
                "the portal rejected that verification code (expired or mistyped)".to_string()
            } else {
                format!(
                    "verification did not complete — the portal said: {:?}",
                    visible_text(&body, 160)
                )
            };
            return Err(CliError::Auth(format!(
                "{hint} — run `rpmfl auth login` again"
            )));
        }
        Ok(())
    }

    // ---- Reads -------------------------------------------------------------

    /// GET a portal JSON endpoint.
    pub fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, CliError> {
        let mut req = self
            .http
            .get(self.url(path))
            .header("Accept", "application/json")
            .header("X-Requested-With", "XMLHttpRequest");
        if !query.is_empty() {
            req = req.query(query);
        }
        let resp = req
            .send()
            .map_err(|e| CliError::Upstream(format!("request to {path} failed: {e}")))?;
        let out = self.handle(resp, path);
        if out.is_ok() {
            self.sync_session();
        }
        out
    }

    /// Map the portal's responses onto the family exit codes.
    ///
    /// A Rails app with an expired session answers an XHR with a 302 to the
    /// login page rather than a 401, so landing on `/users/…` after redirects
    /// is the real "session expired" signal.
    fn handle(&self, resp: reqwest::blocking::Response, path: &str) -> Result<Value, CliError> {
        let status = resp.status();
        let final_url = resp.url().to_string();
        if final_url.contains("/users/log_in") || final_url.contains("/users/two_factor") {
            return Err(CliError::Auth(
                "portal session expired — run `rpmfl auth login`".into(),
            ));
        }
        if matches!(status.as_u16(), 401 | 403) {
            return Err(CliError::Auth(format!(
                "portal returned {} for {path} — run `rpmfl auth login`",
                status.as_u16()
            )));
        }
        if status.as_u16() == 404 {
            return Err(CliError::NotFound(format!("{path} (HTTP 404)")));
        }
        let text = resp
            .text()
            .map_err(|e| CliError::Upstream(format!("reading response body: {e}")))?;
        if !status.is_success() {
            return Err(CliError::Upstream(format!(
                "portal HTTP {} for {path}{}",
                status.as_u16(),
                body_hint(&text)
            )));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|_| {
            if text.trim_start().starts_with('<') {
                // HTML where JSON was expected means we were served a login page.
                CliError::Auth(
                    "portal returned HTML instead of JSON (session expired) — run `rpmfl auth login`"
                        .into(),
                )
            } else {
                CliError::Upstream(format!(
                    "portal returned non-JSON for {path} (first bytes: {:?})",
                    text.chars().take(60).collect::<String>()
                ))
            }
        })
    }
}

/// Pull the Rails CSRF token out of a rendered form.
fn csrf_token(html: &str) -> Option<String> {
    let anchor = html.find("name=\"authenticity_token\"")?;
    let rest = &html[anchor..];
    let value_at = rest.find("value=\"")? + "value=\"".len();
    let rest = &rest[value_at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Read a hidden input's value out of a rendered form.
fn hidden_field(html: &str, name: &str) -> Option<String> {
    let anchor = html.find(&format!("name=\"{name}\""))?;
    // Scan the *whole* enclosing tag, not just what follows the name. Rails'
    // `form.hidden_field` emits `value` before `name`
    // (`<input value="…" type="hidden" name="user[phone_number]" …>`), so
    // reading only forwards silently drops the value — which meant the portal
    // received a send-code request with no destination and quietly sent
    // nothing.
    let tag_start = html[..anchor].rfind('<')?;
    let tag_end = anchor + html[anchor..].find('>')?;
    let tag = &html[tag_start..tag_end];
    let value_at = tag.find("value=\"")? + "value=\"".len();
    let rest = &tag[value_at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The `<form>…</form>` slice containing `marker`, so a page hosting several
/// forms can be submitted faithfully rather than against a guessed endpoint.
fn form_containing<'a>(html: &'a str, marker: &str) -> Option<&'a str> {
    let at = html.find(marker)?;
    let start = html[..at].rfind("<form")?;
    let end = html[at..].find("</form>").map(|e| at + e)?;
    Some(&html[start..end])
}

/// The `action` attribute of a form slice.
fn form_action(form: &str) -> Option<String> {
    let tag_end = form.find('>')?;
    let tag = &form[..tag_end];
    let at = tag.find("action=\"")? + "action=\"".len();
    let rest = &tag[at..];
    let end = rest.find('"')?;
    let action = rest[..end].trim();
    (!action.is_empty()).then(|| action.to_string())
}

/// Every named `<input>` in a form, as `(name, value)`. Submit buttons other
/// than the one we set explicitly are dropped, and unchecked radios are left
/// to the caller.
fn form_inputs(form: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut rest = form;
    while let Some(at) = rest.find("<input") {
        let tag_end = match rest[at..].find('>') {
            Some(e) => at + e,
            None => break,
        };
        let tag = &rest[at..tag_end];
        rest = &rest[tag_end..];

        let attr = |name: &str| -> Option<String> {
            let a = tag.find(&format!("{name}=\""))? + name.len() + 2;
            let r = &tag[a..];
            let e = r.find('"')?;
            Some(r[..e].to_string())
        };
        let Some(name) = attr("name") else { continue };
        let ty = attr("type").unwrap_or_default().to_lowercase();
        // A radio only submits when checked; submit buttons are set by name.
        if ty == "submit" || ((ty == "radio" || ty == "checkbox") && !tag.contains("checked")) {
            continue;
        }
        let value = attr("value").unwrap_or_default();
        if !out.iter().any(|(n, _)| *n == name) {
            out.push((name, value));
        }
    }
    out
}

/// 64 hex characters, matching the shape of the portal's device fingerprint.
/// Uniqueness is all that's needed; this is not a security boundary.
fn random_hex_64() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut seed =
        now.as_nanos() ^ ((std::process::id() as u128) << 64) ^ (&now as *const _ as u128);
    let mut out = String::with_capacity(64);
    while out.len() < 64 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.push_str(&format!("{:016x}", seed as u64));
    }
    out.truncate(64);
    out
}

/// Last four digits of a phone number, for a "sent to …1234" hint. Never log
/// or store the full number — it is the account holder's personal data.
fn last_four(number: &str) -> Option<String> {
    let digits: Vec<char> = number.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits.len() >= 4).then(|| digits[digits.len() - 4..].iter().collect())
}

/// Does this page carry the portal's bad-credentials flash?
fn rejected_credentials(html: &str) -> bool {
    let lower = html.to_lowercase();
    lower.contains("email and password combination you entered is invalid")
        || lower.contains("invalid email or password")
}

/// Best-effort human-readable text from an HTML page, for error messages.
///
/// Skips `<script>` and `<style>` bodies — the portal inlines a large Datadog
/// RUM snippet in `<head>`, which otherwise swamps the message we actually
/// want to show.
fn visible_text(html: &str, limit: usize) -> String {
    let lower = html.to_lowercase();
    let mut out = String::new();
    let mut last_space = true;
    let bytes: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    let mut i = 0;

    let starts_with = |at: usize, pat: &str| -> bool {
        let p: Vec<char> = pat.chars().collect();
        at + p.len() <= lower_chars.len() && lower_chars[at..at + p.len()] == p[..]
    };
    let find_from = |at: usize, pat: &str| -> Option<usize> {
        let p: Vec<char> = pat.chars().collect();
        (at..lower_chars.len().saturating_sub(p.len().saturating_sub(1)))
            .find(|&j| lower_chars[j..j + p.len()] == p[..])
    };

    while i < bytes.len() {
        if starts_with(i, "<script") || starts_with(i, "<style") {
            let close = if starts_with(i, "<script") {
                "</script>"
            } else {
                "</style>"
            };
            i = find_from(i, close).map_or(bytes.len(), |j| j + close.chars().count());
            continue;
        }
        if bytes[i] == '<' {
            i = find_from(i, ">").map_or(bytes.len(), |j| j + 1);
            continue;
        }
        let c = bytes[i];
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
            if out.chars().count() >= limit {
                break;
            }
        }
        i += 1;
    }
    out.trim().to_string()
}

/// Pull a short human hint out of an error body for error messages.
fn body_hint(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        for key in ["message", "error", "errors"] {
            if let Some(m) = v.get(key).and_then(|x| x.as_str()) {
                if !m.is_empty() {
                    return format!(" — {m}");
                }
            }
        }
    }
    format!(" — {}", trimmed.chars().take(120).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_token_extracted_from_form() {
        let html =
            r#"<form><input type="hidden" name="authenticity_token" value="abc123==" /></form>"#;
        assert_eq!(csrf_token(html).as_deref(), Some("abc123=="));
        assert_eq!(csrf_token("<form></form>"), None);
    }

    #[test]
    fn hidden_field_stays_within_its_tag() {
        let html =
            r#"<input name="user[email]" value="a@b.com" /><input name="other" value="zzz" />"#;
        assert_eq!(
            hidden_field(html, "user[email]").as_deref(),
            Some("a@b.com")
        );
        // A field with no value attribute must not borrow the next tag's.
        let html2 = r#"<input name="user[email]" /><input name="other" value="zzz" />"#;
        assert_eq!(hidden_field(html2, "user[email]"), None);
    }

    /// Rails' `form.hidden_field` puts `value` *before* `name`. Reading only
    /// forwards from the name dropped the value, so send-code requests went
    /// out with no destination and the portal silently sent nothing.
    #[test]
    fn hidden_field_finds_value_before_name() {
        let html = r#"<input value="+15550001234" type="hidden" name="user[phone_number]" id="user_phone_number" />"#;
        assert_eq!(
            hidden_field(html, "user[phone_number]").as_deref(),
            Some("+15550001234")
        );
    }

    #[test]
    fn hidden_field_does_not_reach_into_the_previous_tag() {
        let html =
            r#"<input value="earlier" name="other" /><input type="hidden" name="user[email]" />"#;
        assert_eq!(hidden_field(html, "user[email]"), None);
    }

    #[test]
    fn form_containing_picks_the_right_form() {
        let html = r#"<form action="/a"><input name="x" id="one" /></form>
                      <form action="/b"><input name="y" id="two" /></form>"#;
        let f = form_containing(html, r#"id="two""#).unwrap();
        assert_eq!(form_action(f).as_deref(), Some("/b"));
        assert!(f.contains(r#"name="y""#));
        assert!(!f.contains(r#"name="x""#));
        assert!(form_containing(html, r#"id="missing""#).is_none());
    }

    #[test]
    fn form_inputs_replays_named_fields_only() {
        let form = r#"<form action="/go">
            <input value="tok" type="hidden" name="authenticity_token" />
            <input type="hidden" name="user[phone_number]" value="+15550001234" />
            <input type="radio" name="user[two_factor_field]" value="sms" checked />
            <input type="radio" name="user[two_factor_field]" value="call" />
            <input type="submit" name="commit" value="Send verification code" />
            <input type="text" />
        </form>"#;
        let got = form_inputs(form);
        assert_eq!(
            got.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec![
                "authenticity_token",
                "user[phone_number]",
                "user[two_factor_field]"
            ]
        );
        // Value read regardless of attribute order, and the checked radio wins.
        assert_eq!(got[0].1, "tok");
        assert_eq!(got[1].1, "+15550001234");
        assert_eq!(got[2].1, "sms");
    }

    #[test]
    fn last_four_reads_trailing_digits() {
        assert_eq!(last_four("+15550001234").as_deref(), Some("1234"));
        assert_eq!(last_four("(555) 000-9876").as_deref(), Some("9876"));
        assert_eq!(last_four("12").as_deref(), None);
        assert_eq!(last_four("").as_deref(), None);
    }

    #[test]
    fn random_fingerprint_has_the_right_shape() {
        let a = random_hex_64();
        let b = random_hex_64();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn bad_credentials_flash_is_detected() {
        assert!(rejected_credentials(
            "<p>The email and password combination you entered is invalid. Please try again.</p>"
        ));
        assert!(rejected_credentials("<p>Invalid email or password</p>"));
        assert!(!rejected_credentials(
            "<p>The device you are logging in from is not recognized.</p>"
        ));
    }

    #[test]
    fn visible_text_strips_markup_and_collapses_space() {
        let html = "<div>\n  <p>Hello   <b>there</b></p>\n</div>";
        assert_eq!(visible_text(html, 100), "Hello there");
        assert_eq!(visible_text("<p>abcdefghij</p>", 4), "abcd");
    }

    #[test]
    fn visible_text_skips_script_and_style_bodies() {
        // The portal inlines a large Datadog RUM snippet; without skipping it
        // the diagnostic shows JavaScript instead of the portal's message.
        let html = r#"<head><script>window.DD_RUM && window.DD_RUM.init({clientToken:'abc'});</script>
                      <style>.a{color:red}</style></head><body><p>Code sent.</p></body>"#;
        assert_eq!(visible_text(html, 200), "Code sent.");
    }

    #[test]
    fn channel_form_values() {
        assert_eq!(CodeChannel::Sms.form_value(), "sms");
        assert_eq!(CodeChannel::Call.form_value(), "call");
    }

    #[test]
    fn body_hint_prefers_message_then_snippet() {
        assert_eq!(body_hint(""), "");
        assert_eq!(body_hint(r#"{"message":"nope"}"#), " — nope");
        assert_eq!(body_hint("plain text"), " — plain text");
        // " — " plus 120 truncated chars.
        assert_eq!(body_hint(&"x".repeat(200)).chars().count(), 123);
    }

    #[test]
    fn urls_join_base_and_path() {
        let p = Portal::new("https://example.appfolio.com").unwrap();
        assert_eq!(
            p.url("/oportal/api/x"),
            "https://example.appfolio.com/oportal/api/x"
        );
        assert_eq!(
            p.url("oportal/api/x"),
            "https://example.appfolio.com/oportal/api/x"
        );
        assert_eq!(p.url("https://other/y"), "https://other/y");
    }

    #[test]
    fn sync_is_a_noop_without_a_keychain_binding() {
        // `Portal::new` is the login-time client: it has no cached session to
        // write back to, so syncing must do nothing rather than panic.
        let p = Portal::new("https://example.appfolio.com").unwrap();
        assert!(p.sync.is_none());
        p.seed_cookie(SESSION_COOKIE, "fresh");
        p.sync_session();
        assert_eq!(p.cookie(SESSION_COOKIE).as_deref(), Some("fresh"));
    }

    #[test]
    fn seeded_cookies_round_trip() {
        let p = Portal::new("https://example.appfolio.com").unwrap();
        p.seed_cookie(SESSION_COOKIE, "sess-value");
        p.seed_cookie(DEVICE_COOKIE, "device-value");
        assert_eq!(
            p.session_cookie().map(|s| s.expose().to_string()),
            Some("sess-value".into())
        );
        assert_eq!(
            p.device_token().map(|s| s.expose().to_string()),
            Some("device-value".into())
        );
        assert_eq!(p.cookie("nonexistent"), None);
    }
}
