//! `rpmfl bills` — unpaid bills charged against the portfolio.

use clap::{Args, Subcommand};
use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::{emit, table_view, Ctx};
use crate::dates::RangeArgs;

#[derive(Args, Debug)]
pub struct ListArgs {
    #[command(flatten)]
    pub range: RangeArgs,

    /// Restrict to one property (portal ID).
    #[arg(long)]
    pub property_id: Option<String>,

    /// Maximum bills to return.
    #[arg(long, default_value_t = 50)]
    pub limit: u32,

    /// Skip this many bills (pagination).
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List unpaid bills due on or before the end date.
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Outstanding balance per property.
    Balances(RangeArgs),
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<(), CliError> {
    let client = ctx.client()?;
    match cmd {
        Cmd::List(args) => {
            let mut q = vec![
                ("due_on_end", args.range.resolve_end()?),
                ("limit", args.limit.to_string()),
                ("offset", args.offset.to_string()),
            ];
            if let Some(p) = &args.property_id {
                q.push(("property_ids", p.clone()));
            }
            let payload = client.get("/oportal/api/unpaid_bills", &q)?;
            let items = payload.as_array().cloned().unwrap_or_default();
            emit(
                ctx,
                "bill-list",
                json!({ "count": items.len(), "bills": items }),
                |v| {
                    let rows = v
                        .get("bills")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if rows.is_empty() {
                        eprintln!("no unpaid bills");
                        return;
                    }
                    output::table(&table_view(
                        &rows,
                        &[
                            "id",
                            "dueOn",
                            "amount",
                            "balance",
                            "payeeName",
                            "description",
                        ],
                    ));
                },
            );
            Ok(())
        }
        Cmd::Balances(range) => {
            let payload = client.get(
                "/oportal/api/unpaid_bills_balances",
                &[("due_on_end", range.resolve_end()?)],
            )?;
            let items = payload.as_array().cloned().unwrap_or_default();
            emit(ctx, "bill-balances", json!({ "balances": items }), |v| {
                let rows = v
                    .get("balances")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                output::table(&table_view(&rows, &["propertyId", "totalBalance"]));
            });
            Ok(())
        }
    }
}
