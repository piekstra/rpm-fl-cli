//! `rpmfl summary` — the dashboard view: cash in/out, disbursements, reserves.
//!
//! One command that fans out over the same endpoints the portal's dashboard
//! calls, so a single invocation answers "how did the portfolio do this period".

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::{emit, Ctx};
use crate::dates::RangeArgs;

pub fn run(ctx: &Ctx, range: &RangeArgs) -> Result<(), CliError> {
    let (start, end) = range.resolve()?;
    let client = ctx.client()?;
    let period = [("start_on", start.clone()), ("end_on", end.clone())];

    let income = client.get("/oportal/api/owner_income_balances", &period)?;
    let expenses = client.get("/oportal/api/owner_expenses_balances", &period)?;
    let unpaid = client.get(
        "/oportal/api/unpaid_bills_balances",
        &[("due_on_end", end.clone())],
    )?;

    let cash_in = total_of(&income, "cashIn");
    let cash_out = total_of(&expenses, "cashOut");
    let disbursements = total_of(&expenses, "disbursement");
    let unpaid_total: f64 = unpaid
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("totalBalance").and_then(Value::as_f64))
                .sum()
        })
        .unwrap_or(0.0);

    let payload = json!({
        "period": { "start": start, "end": end },
        "cash_in": cash_in,
        "cash_out": cash_out,
        "disbursements": disbursements,
        "net": cash_in - cash_out,
        "unpaid_bills": unpaid_total,
    });

    emit(ctx, "owner-summary", payload, |v| output::kv(v, 0));
    Ok(())
}

/// Sum `totalAmount` across rows of a given balance `type`.
fn total_of(payload: &Value, kind: &str) -> f64 {
    let sum: f64 = payload
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| r.get("type").and_then(Value::as_str) == Some(kind))
                .filter_map(|r| r.get("totalAmount").and_then(Value::as_f64))
                .sum()
        })
        .unwrap_or(0.0);
    // The portal reports zeroed periods as `-0.0`, which renders as "-0.0".
    if sum == 0.0 {
        0.0
    } else {
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_sum_matching_rows_only() {
        let payload = json!([
            { "type": "cashOut", "totalAmount": 100.5 },
            { "type": "disbursement", "totalAmount": 10.0 },
            { "type": "disbursement", "totalAmount": 15.0 },
        ]);
        assert_eq!(total_of(&payload, "cashOut"), 100.5);
        assert_eq!(total_of(&payload, "disbursement"), 25.0);
        assert_eq!(total_of(&payload, "cashIn"), 0.0);
    }

    #[test]
    fn zero_totals_never_render_as_negative_zero() {
        let payload = json!([{ "type": "cashOut", "totalAmount": -0.0 }]);
        let total = total_of(&payload, "cashOut");
        assert_eq!(total, 0.0);
        assert_eq!(format!("{total}"), "0");
        assert!(!format!("{total:?}").starts_with('-'));
    }

    #[test]
    fn totals_tolerate_unexpected_shapes() {
        assert_eq!(total_of(&json!({}), "cashIn"), 0.0);
        assert_eq!(total_of(&json!([{ "type": "cashIn" }]), "cashIn"), 0.0);
    }
}
