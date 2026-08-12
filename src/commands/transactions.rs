//! `rpmfl transactions` — the owner ledger: cash in, cash out, disbursements.

use clap::{Args, Subcommand};
use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::{emit, table_view, Ctx};
use crate::dates::RangeArgs;

/// Ledger entry kinds the portal distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TxType {
    /// Rent and other income.
    CashIn,
    /// Bills, repairs, management fees.
    CashOut,
    /// Owner draws.
    Disbursement,
}

impl TxType {
    /// The value the portal's `type` query param accepts — snake case, and
    /// only for the two cash kinds.
    ///
    /// `disbursement` is a real entry type in *responses*, but passing it as a
    /// filter makes the portal answer HTTP 500. `None` means "the server can't
    /// filter this; do it client-side" (see [`matches_response`]).
    fn server_filter(self) -> Option<&'static str> {
        match self {
            TxType::CashIn => Some("cash_in"),
            TxType::CashOut => Some("cash_out"),
            TxType::Disbursement => None,
        }
    }

    /// The value the portal reports in a response row — camel case.
    fn response_value(self) -> &'static str {
        match self {
            TxType::CashIn => "cashIn",
            TxType::CashOut => "cashOut",
            TxType::Disbursement => "disbursement",
        }
    }

    fn matches_response(self, row: &Value) -> bool {
        row.get("type").and_then(Value::as_str) == Some(self.response_value())
    }
}

/// Rows per request when paging to filter client-side.
const PAGE_SIZE: u32 = 100;

/// Safety cap so a portal quirk can't spin this into an unbounded fetch loop.
const MAX_PAGES: u32 = 50;

#[derive(Args, Debug)]
pub struct ListArgs {
    #[command(flatten)]
    pub range: RangeArgs,

    /// Only entries of this kind.
    #[arg(long, value_enum)]
    pub r#type: Option<TxType>,

    /// Restrict to one property (portal ID; see `rpmfl properties list`).
    #[arg(long)]
    pub property_id: Option<String>,

    /// Maximum entries to return.
    #[arg(long, default_value_t = 100)]
    pub limit: u32,

    /// Skip this many entries (pagination).
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List ledger entries, newest first.
    #[command(visible_alias = "ls")]
    List(ListArgs),
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<(), CliError> {
    match cmd {
        Cmd::List(args) => {
            let items = fetch(ctx, args)?;
            emit(
                ctx,
                "transaction-list",
                json!({ "count": items.len(), "transactions": items }),
                |v| {
                    let rows = v
                        .get("transactions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    output::table(&table_view(
                        &rows,
                        &[
                            "postedOn",
                            "type",
                            "amount",
                            "partyName",
                            "propertyDisplayName",
                            "description",
                        ],
                    ));
                },
            );
            Ok(())
        }
    }
}

/// Fetch ledger rows, letting the portal filter when it can and paging +
/// filtering here when it can't.
fn fetch(ctx: &Ctx, args: &ListArgs) -> Result<Vec<Value>, CliError> {
    let (start, end) = args.range.resolve()?;
    let client = ctx.client()?;

    let base = |limit: u32, offset: u32| {
        let mut q = vec![
            ("start_on", start.clone()),
            ("end_on", end.clone()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];
        if let Some(p) = &args.property_id {
            q.push(("property_ids", p.clone()));
        }
        q
    };

    // Server-side filter (or no filter at all): one request, portal paginates.
    let client_side = match args.r#type {
        None => None,
        Some(t) => match t.server_filter() {
            Some(wire) => {
                let mut q = base(args.limit, args.offset);
                q.push(("type", wire.to_string()));
                return Ok(client
                    .get("/oportal/api/owner_transactions", &q)?
                    .as_array()
                    .cloned()
                    .unwrap_or_default());
            }
            None => Some(t),
        },
    };
    let Some(kind) = client_side else {
        let q = base(args.limit, args.offset);
        return Ok(client
            .get("/oportal/api/owner_transactions", &q)?
            .as_array()
            .cloned()
            .unwrap_or_default());
    };

    // The portal 500s on this filter, so walk unfiltered pages and select
    // here. `--offset` therefore counts within the *matching* rows, which is
    // what a caller paging through disbursements actually means.
    let mut matched: Vec<Value> = Vec::new();
    let wanted = args.offset.saturating_add(args.limit) as usize;
    for page in 0..MAX_PAGES {
        let q = base(PAGE_SIZE, page * PAGE_SIZE);
        let rows = client
            .get("/oportal/api/owner_transactions", &q)?
            .as_array()
            .cloned()
            .unwrap_or_default();
        let exhausted = rows.len() < PAGE_SIZE as usize;
        matched.extend(rows.into_iter().filter(|r| kind.matches_response(r)));
        if matched.len() >= wanted || exhausted {
            break;
        }
    }
    Ok(matched
        .into_iter()
        .skip(args.offset as usize)
        .take(args.limit as usize)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cash_kinds_filter_server_side() {
        assert_eq!(TxType::CashIn.server_filter(), Some("cash_in"));
        assert_eq!(TxType::CashOut.server_filter(), Some("cash_out"));
        // The portal answers HTTP 500 when asked to filter on this one.
        assert_eq!(TxType::Disbursement.server_filter(), None);
    }

    #[test]
    fn response_values_are_camel_case() {
        assert_eq!(TxType::CashIn.response_value(), "cashIn");
        assert_eq!(TxType::CashOut.response_value(), "cashOut");
        assert_eq!(TxType::Disbursement.response_value(), "disbursement");
    }

    #[test]
    fn matches_response_selects_by_type_field() {
        let row = json!({ "type": "disbursement", "amount": 1.0 });
        assert!(TxType::Disbursement.matches_response(&row));
        assert!(!TxType::CashIn.matches_response(&row));
        // A row without a `type` matches nothing rather than everything.
        assert!(!TxType::Disbursement.matches_response(&json!({ "amount": 1.0 })));
    }
}
