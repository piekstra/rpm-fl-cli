//! Non-secret settings (`~/.config/rpmfl/config.json`).
//!
//! The portal password and the 2FA device-trust token live in the OS keychain
//! (service `piekstra.rpmfl`), never here.

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

/// Environment fallback for the portal host.
pub const ENV_BASE_URL: &str = "RPMFL_BASE_URL";

/// Keychain account the portal password is stored under.
pub const KEYCHAIN_ACCOUNT: &str = "password";

/// Keychain account for the cached `_oportal_session` cookie. Portal reads
/// authenticate with this cookie alone, so caching it is what lets ordinary
/// commands run without re-doing the two-factor dance every time.
pub const SESSION_ACCOUNT: &str = "session";

/// Keychain account for a half-finished login: the session that requested a
/// two-factor code, parked so a follow-up `auth login --code <CODE>` can
/// resume it. AppFolio ties a pending code to the session that asked for it,
/// so a second invocation starting a fresh login would present the code
/// against a session that never requested one.
pub const PENDING_SESSION_ACCOUNT: &str = "pending-session";

/// Keychain account for the 2FA "remember this device" token. AppFolio binds
/// it to browser-side fingerprinting, so it rarely spares a plain HTTP client
/// the challenge — stored and replayed anyway since it costs one cookie.
pub const DEVICE_TOKEN_ACCOUNT: &str = "device-token";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// The portal host, e.g. `https://<subdomain>.appfolio.com`.
    ///
    /// Deliberately has no default: the subdomain is issued per property-
    /// management company, so baking one in would both be wrong for everyone
    /// else and disclose which company this install belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Portal login email (identity label only; secrets stay in the keychain).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

impl Config {
    /// Resolve the portal host: config, then `$RPMFL_BASE_URL`.
    pub fn base_url(&self) -> Result<String, CliError> {
        self.base_url
            .clone()
            .or_else(|| std::env::var(ENV_BASE_URL).ok().filter(|s| !s.is_empty()))
            .ok_or_else(|| {
                CliError::Usage(format!(
                    "no portal host configured — run `rpmfl config set base_url \
                     https://<subdomain>.appfolio.com` (your property manager's portal URL), \
                     or set ${ENV_BASE_URL}"
                ))
            })
    }

    /// Resolve the login email: config, then `$RPMFL_USERNAME`.
    pub fn username(&self) -> Option<String> {
        self.username.clone().or_else(|| {
            std::env::var("RPMFL_USERNAME")
                .ok()
                .filter(|s| !s.is_empty())
        })
    }
}

/// Config keys settable via `rpmfl config set <key> <value>`.
pub const KNOWN_KEYS: &[&str] = &["base_url", "username"];
