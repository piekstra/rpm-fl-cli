//! `rpmfl properties` — the owned portfolio: addresses, units, leases.

use clap::Subcommand;
use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::{emit, table_view, Ctx};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List owned properties with unit, lease, and occupancy detail.
    #[command(visible_alias = "ls")]
    List,
    /// Show one property by its portal ID.
    Get { property_id: String },
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<(), CliError> {
    let client = ctx.client()?;
    let payload = client.get("/oportal/api/owner_properties", &[])?;
    let properties = payload.as_array().cloned().unwrap_or_default();

    match cmd {
        Cmd::List => {
            let rows: Vec<Value> = properties.iter().map(flatten).collect();
            emit(ctx, "property-list", json!({ "properties": rows }), |v| {
                let items = v
                    .get("properties")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                output::table(&table_view(
                    &items,
                    &[
                        "id",
                        "name",
                        "address",
                        "units",
                        "occupied",
                        "rent",
                        "lease_end",
                    ],
                ));
            });
            Ok(())
        }
        Cmd::Get { property_id } => {
            let found = properties
                .iter()
                .find(|p| p.get("id").and_then(Value::as_str) == Some(property_id.as_str()))
                .ok_or_else(|| CliError::NotFound(format!("no property with id {property_id}")))?;
            emit(ctx, "property", flatten(found), output::render);
            Ok(())
        }
    }
}

/// Flatten the portal's nested JSON:API-ish shape into one flat row.
fn flatten(p: &Value) -> Value {
    let attrs = p.get("attributes").cloned().unwrap_or(Value::Null);
    let units = p
        .pointer("/relationships/units")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let occupied = units
        .iter()
        .filter(|u| {
            u.pointer("/attributes/occupied")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    let rent: f64 = units
        .iter()
        .filter_map(|u| {
            u.pointer("/attributes/unit_total_rent_and_subsidy")
                .and_then(Value::as_f64)
        })
        .sum();
    // Earliest lease end across units is the one the owner cares about.
    let lease_end = units
        .iter()
        .filter_map(|u| {
            u.pointer("/relationships/current_occupancy/relationships/lease/attributes/end_on")
                .and_then(Value::as_str)
        })
        .min()
        .map(String::from);

    json!({
        "id": p.get("id").cloned().unwrap_or(Value::Null),
        "name": attrs.get("raw_display_name").cloned().unwrap_or(Value::Null),
        "address": p.pointer("/relationships/address/attributes/full").cloned().unwrap_or(Value::Null),
        "units": units.len(),
        "occupied": occupied,
        "rent": rent,
        "lease_end": lease_end,
        "minimum_cash_reserve": attrs.get("minimum_cash_reserve").cloned().unwrap_or(Value::Null),
        "hidden": attrs.get("hidden").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "id": "1",
            "attributes": { "raw_display_name": "Sample St 100", "minimum_cash_reserve": 500, "hidden": false },
            "relationships": {
                "address": { "attributes": { "full": "100 Sample St, Sample City, ST 00000" } },
                "units": [
                    { "attributes": { "occupied": true, "unit_total_rent_and_subsidy": 1000 },
                      "relationships": { "current_occupancy": { "relationships": { "lease": { "attributes": { "end_on": "2027-05-31" } } } } } },
                    { "attributes": { "occupied": false, "unit_total_rent_and_subsidy": 900 },
                      "relationships": { "current_occupancy": { "relationships": { "lease": { "attributes": { "end_on": "2026-11-30" } } } } } }
                ]
            }
        })
    }

    #[test]
    fn flatten_summarizes_units() {
        let f = flatten(&sample());
        assert_eq!(f["name"], "Sample St 100");
        assert_eq!(f["units"], 2);
        assert_eq!(f["occupied"], 1);
        assert_eq!(f["rent"], 1900.0);
        // Earliest lease end wins.
        assert_eq!(f["lease_end"], "2026-11-30");
    }

    #[test]
    fn flatten_tolerates_missing_relationships() {
        let f = flatten(&json!({ "id": "9" }));
        assert_eq!(f["id"], "9");
        assert_eq!(f["units"], 0);
        assert_eq!(f["occupied"], 0);
        assert_eq!(f["rent"], 0.0);
        assert_eq!(f["lease_end"], Value::Null);
    }
}
