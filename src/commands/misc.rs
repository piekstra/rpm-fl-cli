//! Smaller read surfaces that don't warrant a module each: ownerships, tenant
//! rent charges, statement packets, estimate approvals, and actionable forms.

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::{emit, table_view, Ctx};
use crate::dates::RangeArgs;

/// `rpmfl ownerships` — who owns what, and in what share.
pub fn ownerships(ctx: &Ctx) -> Result<(), CliError> {
    let payload = ctx.client()?.get("/oportal/api/owner_ownerships", &[])?;
    let rows: Vec<Value> = payload
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|o| {
            json!({
                "id": o.get("id").cloned().unwrap_or(Value::Null),
                "owner": o.pointer("/relationships/owner/attributes/name").cloned().unwrap_or(Value::Null),
                "owner_id": o.pointer("/relationships/owner/id").cloned().unwrap_or(Value::Null),
                "property": o.pointer("/relationships/ownership_group/relationships/property/attributes/raw_display_name").cloned().unwrap_or(Value::Null),
                "property_id": o.pointer("/relationships/ownership_group/relationships/property/id").cloned().unwrap_or(Value::Null),
                "percent_owned": o.pointer("/attributes/percent_owned").cloned().unwrap_or(Value::Null),
                "end_on": o.pointer("/relationships/ownership_group/attributes/end_on").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    emit(ctx, "ownership-list", json!({ "ownerships": rows }), |v| {
        let items = v
            .get("ownerships")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        output::table(&table_view(
            &items,
            &[
                "owner",
                "property",
                "percent_owned",
                "property_id",
                "end_on",
            ],
        ));
    });
    Ok(())
}

/// `rpmfl charges` — rent charged to tenants, with outstanding balances.
pub fn charges(ctx: &Ctx, range: &RangeArgs) -> Result<(), CliError> {
    let (start, end) = range.resolve()?;
    let payload = ctx.client()?.get(
        "/oportal/api/tenant_charges",
        &[
            ("start_on", start),
            ("end_on", end.clone()),
            ("balance_as_of", end),
        ],
    )?;
    let items = payload.as_array().cloned().unwrap_or_default();
    emit(
        ctx,
        "charge-list",
        json!({ "count": items.len(), "charges": items }),
        |v| {
            let rows = v
                .get("charges")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            output::table(&table_view(
                &rows,
                &["occurredOn", "amount", "balance", "propertyId"],
            ));
        },
    );
    Ok(())
}

/// `rpmfl statements` — published owner packets.
pub fn statements(ctx: &Ctx, limit: u32) -> Result<(), CliError> {
    let payload = ctx.client()?.get(
        "/oportal/api/owner_documents",
        &[("limit", limit.to_string())],
    )?;
    // The endpoint answers with one entry per owner, each holding a packet list.
    let packets: Vec<Value> = payload
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|owner| {
            owner
                .get("owner_documents")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect();

    emit(
        ctx,
        "statement-list",
        json!({ "count": packets.len(), "statements": packets }),
        |v| {
            let rows = v
                .get("statements")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if rows.is_empty() {
                eprintln!("no owner packets have been published");
                return;
            }
            output::table(&table_view(&rows, &["id", "name", "shared_at", "size"]));
        },
    );
    Ok(())
}

/// `rpmfl approvals` — estimates and other decisions awaiting the owner.
pub fn approvals(ctx: &Ctx, page: u32) -> Result<(), CliError> {
    let payload = ctx.client()?.get(
        "/oportal/api/owner_decision_requests",
        &[("page_number", page.to_string())],
    )?;
    emit(ctx, "approval-list", payload, |v| {
        let rows = v
            .get("owner_decision_requests")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            eprintln!("no pending approvals");
            return;
        }
        output::table(&table_view(
            &rows,
            &["id", "status", "created_at", "description", "amount"],
        ));
    });
    Ok(())
}

/// `rpmfl forms` — documents the portal is waiting on a signature for.
pub fn forms(ctx: &Ctx) -> Result<(), CliError> {
    let payload = ctx
        .client()?
        .get("/oportal/api/pdf_forms/actionable_documents", &[])?;
    let rows: Vec<Value> = payload
        .get("documents_by_owner")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|o| {
            o.get("documents")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect();

    emit(
        ctx,
        "form-list",
        json!({ "count": rows.len(), "forms": rows }),
        |v| {
            let items = v
                .get("forms")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                eprintln!("no forms need action");
                return;
            }
            output::table(&table_view(&items, &["id", "name", "status", "sent_at"]));
        },
    );
    Ok(())
}
