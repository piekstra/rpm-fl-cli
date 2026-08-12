//! `rpmfl api` — raw passthrough to any portal endpoint.
//!
//! The escape hatch for surfaces the typed commands don't cover yet, and the
//! quickest way to check whether the portal's JSON has drifted.

use clap::Args;
use pk_cli_core::{output, CliError};

use super::Ctx;

#[derive(Args, Debug)]
pub struct ApiArgs {
    /// Path under the portal host, e.g. `/oportal/api/owner_properties`.
    pub path: String,

    /// Repeatable query parameter, `key=value`.
    #[arg(long = "query", value_name = "KEY=VALUE")]
    pub query: Vec<String>,
}

pub fn run(ctx: &Ctx, args: &ApiArgs) -> Result<(), CliError> {
    let mut query = Vec::new();
    for pair in &args.query {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| CliError::Usage(format!("--query expects KEY=VALUE, got {pair:?}")))?;
        query.push((k.to_string(), v.to_string()));
    }
    // Borrow as &str keys for the client's signature.
    let borrowed: Vec<(&str, String)> =
        query.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();

    let payload = ctx.client()?.get(&args.path, &borrowed)?;
    output::json(&payload);
    Ok(())
}
