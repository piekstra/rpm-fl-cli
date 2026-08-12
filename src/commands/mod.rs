//! Domain command modules. Each read emits the provider payload (plus a
//! `schema` tag) in `--json` mode and a shaped table/kv view in text mode.

pub mod api;
pub mod bills;
pub mod documents;
pub mod misc;
pub mod properties;
pub mod summary;
pub mod transactions;

use pk_cli_core::{output, CliError, CommonArgs};
use pk_cli_secrets::CredentialStore;
use serde_json::Value;

use crate::client::Portal;
use crate::config::Config;

// §1.4's column projection now lives in the shared crate; re-exported so the
// command modules keep their short `use super::{emit, table_view}`.
pub use pk_cli_core::output::table_view;

pub struct Ctx<'a> {
    pub common: &'a CommonArgs,
    pub cfg: &'a Config,
    pub creds: &'a CredentialStore,
}

impl Ctx<'_> {
    /// A portal session replayed from the keychain. Expiry surfaces as a
    /// `CliError::Auth` (exit 3) on the first read, pointing at `auth login`.
    pub fn client(&self) -> Result<Portal, CliError> {
        // The keychain reads in here block indefinitely while macOS waits on
        // a permission dialog — the real cause of the multi-minute "hangs" —
        // so name that wait instead of sitting silent.
        let portal = crate::diag::keychain(self.common.quiet, || {
            Portal::from_cached_session(self.cfg, self.creds)
        })?;
        Ok(portal.with_diagnostics(self.common.verbose, self.common.quiet))
    }
}

/// Emit a DTO, taking the `--json` flag off the context.
///
/// A thin adapter over `pk_cli_core::output::emit`, which owns the contract.
pub fn emit(ctx: &Ctx, schema: &str, payload: Value, text: impl FnOnce(&Value)) {
    output::emit(ctx.common.json, schema, payload, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn table_view_selects_and_skips_missing() {
        let items = vec![json!({ "a": 1, "b": 2, "c": 3 }), json!({ "a": 4 })];
        let rows = table_view(&items, &["a", "c"]);
        assert_eq!(rows[0], json!({ "a": 1, "c": 3 }));
        // Absent columns are omitted rather than nulled (SPEC: omit-don't-null).
        assert_eq!(rows[1], json!({ "a": 4 }));
    }
}
