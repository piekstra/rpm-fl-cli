//! Contract tests against captured (scrubbed) portal responses.
//!
//! These guard the assumption every command makes: that the portal's JSON
//! still carries the fields the table views and flatteners read. If AppFolio
//! renames `postedOn` or moves `raw_display_name`, this fails loudly instead
//! of quietly rendering empty columns.

use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {path}: {e}"))
}

#[test]
fn owner_properties_shape() {
    let v = fixture("owner_properties.json");
    let p = &v.as_array().expect("array of properties")[0];
    assert!(p["id"].is_string());
    assert!(p["attributes"]["raw_display_name"].is_string());
    assert!(p["attributes"]["minimum_cash_reserve"].is_number());
    assert!(p["relationships"]["address"]["attributes"]["full"].is_string());

    let unit = &p["relationships"]["units"].as_array().expect("units")[0];
    assert!(unit["attributes"]["occupied"].is_boolean());
    assert!(unit["attributes"]["unit_total_rent_and_subsidy"].is_number());
    // The lease end the `properties list` table shows.
    assert!(
        unit["relationships"]["current_occupancy"]["relationships"]["lease"]["attributes"]
            ["end_on"]
            .is_string()
    );
}

#[test]
fn owner_ownerships_shape() {
    let v = fixture("owner_ownerships.json");
    let o = &v.as_array().expect("array of ownerships")[0];
    assert!(o["attributes"]["percent_owned"].is_number());
    assert!(o["relationships"]["owner"]["attributes"]["name"].is_string());
    let group = &o["relationships"]["ownership_group"];
    assert!(group["relationships"]["property"]["attributes"]["raw_display_name"].is_string());
    assert!(group["relationships"]["property"]["id"].is_string());
    // `end_on` is null for an active ownership — the key must still be present.
    assert!(group["attributes"].get("end_on").is_some());
}

#[test]
fn owner_transactions_shape() {
    let v = fixture("owner_transactions.json");
    let rows = v.as_array().expect("array of transactions");
    assert!(!rows.is_empty());
    for t in rows {
        for key in [
            "id",
            "postedOn",
            "amount",
            "type",
            "partyName",
            "propertyDisplayName",
            "description",
        ] {
            assert!(t.get(key).is_some(), "transaction missing `{key}`");
        }
        assert!(t["amount"].is_number());
        assert!(t["attachments"].is_array());
    }
    // The three ledger kinds the `--type` flag maps onto.
    let kinds: Vec<&str> = rows.iter().filter_map(|t| t["type"].as_str()).collect();
    assert!(kinds.contains(&"cashIn"));
    assert!(kinds.contains(&"cashOut"));
    assert!(kinds.contains(&"disbursement"));
}

#[test]
fn balance_endpoints_shape() {
    for (name, kind) in [
        ("owner_income_balances.json", "cashIn"),
        ("owner_expenses_balances.json", "cashOut"),
    ] {
        let v = fixture(name);
        let rows = v.as_array().expect("array of balances");
        assert!(
            rows.iter().any(|r| r["type"] == kind),
            "{name} missing {kind}"
        );
        for r in rows {
            assert!(r["totalAmount"].is_number());
            assert!(r["propertyId"].is_string());
        }
    }
}

#[test]
fn tenant_charges_shape() {
    let v = fixture("tenant_charges.json");
    for c in v.as_array().expect("array of charges") {
        assert!(c["occurredOn"].is_string());
        assert!(c["amount"].is_number());
        assert!(c["balance"].is_number());
        assert!(c["propertyId"].is_string());
    }
}

#[test]
fn unpaid_bills_balances_shape() {
    let v = fixture("unpaid_bills_balances.json");
    for b in v.as_array().expect("array of balances") {
        assert!(b["propertyId"].is_string());
        assert!(b["totalBalance"].is_number());
    }
}

#[test]
fn shared_documents_shape() {
    let v = fixture("shared_documents.json");
    let owner = &v.as_array().expect("array of owners")[0];
    // Every group `documents::collect` walks must still be present.
    for group in [
        "documents",
        "management_agreement_documents",
        "uploaded_pdf_management_agreements",
        "pdf_template_management_agreements",
        "insurance_policy_documents",
    ] {
        assert!(owner[group].is_array(), "missing document group `{group}`");
    }

    let doc = &owner["documents"].as_array().expect("documents")[0];
    assert!(doc["id"].is_number());
    assert!(doc["name"].is_string());
    assert!(doc["shared_at"].is_string());
    assert!(doc["download_url"].is_string());

    // Generated agreements date themselves with `sent_at`, not `shared_at`.
    let agreement = &owner["pdf_template_management_agreements"]
        .as_array()
        .expect("agreements")[0];
    assert!(agreement["sent_at"].is_string());
    assert!(agreement["shared_at"].is_null());
}

#[test]
fn owner_documents_shape() {
    let v = fixture("owner_documents.json");
    let entry = &v.as_array().expect("array of owners")[0];
    // Statement packets nest under each owner, even when empty.
    assert!(entry["owner_documents"].is_array());
}

#[test]
fn decision_requests_and_forms_shape() {
    let d = fixture("owner_decision_requests.json");
    assert!(d["owner_decision_requests"].is_array());
    assert!(d["pagination"]["total_items"].is_number());

    let f = fixture("pdf_forms_actionable_documents.json");
    let owners = f["documents_by_owner"]
        .as_array()
        .expect("documents_by_owner");
    assert!(owners[0]["documents"].is_array());
}

/// The scrubbing contract from `tests/fixtures/README.md`.
///
/// Stated positively — "identity-bearing fields must hold a known dummy" —
/// rather than as a denylist of real names, because a denylist would itself
/// publish the values it exists to keep out of the repo.
#[test]
fn fixtures_carry_only_dummy_identifiers() {
    for (path, value) in all_fixtures() {
        check_scrubbed(&value, None, &path);
    }
}

/// Every fixture on disk, parsed.
fn all_fixtures() -> Vec<(String, Value)> {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("reading fixture");
        let parsed = serde_json::from_str(&raw).expect("fixture parses");
        out.push((path.display().to_string(), parsed));
    }
    assert!(!out.is_empty(), "no fixtures found");
    out
}

/// Walk a fixture, asserting that any value under an identity-bearing key
/// matches the dummy shape documented in `tests/fixtures/README.md`.
fn check_scrubbed(node: &Value, key: Option<&str>, path: &str) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                check_scrubbed(v, Some(k), path);
            }
        }
        Value::Array(items) => {
            for v in items {
                check_scrubbed(v, key, path);
            }
        }
        Value::String(s) => {
            let ok = match key {
                // People and companies.
                Some("name") | Some("owner_name") | Some("partyName") => {
                    s.is_empty() || s.starts_with("Sample") || is_document_name(s)
                }
                // Street addresses.
                Some("full") => s.starts_with("100 Sample St"),
                // Property display names.
                Some("raw_display_name") | Some("propertyDisplayName") => s.starts_with("Sample"),
                Some("description") => s.starts_with("Sample"),
                // Pre-signed links are live credentials; only the dummy host.
                Some("download_url") | Some("display_url") => {
                    s.starts_with("https://example.invalid/")
                }
                _ => true,
            };
            assert!(
                ok,
                "{path}: key `{}` holds un-scrubbed value {s:?} \
                 — see tests/fixtures/README.md",
                key.unwrap_or("?")
            );
            // No AWS pre-sign material anywhere, under any key.
            for marker in ["X-Amz-Signature", "X-Amz-Credential", "amazonaws.com"] {
                assert!(
                    !s.contains(marker),
                    "{path}: value contains {marker:?} — never commit a pre-signed URL"
                );
            }
        }
        _ => {}
    }
}

/// Document titles legitimately aren't `Sample …` (e.g. `W-9.pdf`), but they
/// must not read like a person's name or carry a street address.
fn is_document_name(s: &str) -> bool {
    s.ends_with(".pdf") || s.starts_with("Management Agreement - 100 Sample St")
}

/// The fixture scrub test only ever guarded `tests/fixtures/`, which is
/// exactly how a real phone number's last four digits reached `docs/api.md`
/// and two doc comments. Personal data leaks into prose and examples at least
/// as easily as into fixtures, so scan the tracked sources too.
///
/// Stated as shapes rather than a list of real values, for the same reason the
/// fixture check is: a denylist of what to hide publishes it.
#[test]
fn tracked_sources_carry_no_personal_data() {
    let root = env!("CARGO_MANIFEST_DIR");
    let listed = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output();
    let Ok(out) = listed else { return }; // not a git checkout (e.g. vendored)
    if !out.status.success() {
        return;
    }

    for rel in String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
    {
        // Fixtures have their own, stricter check. Lockfiles are generated and
        // full of hex checksums that trip digit heuristics.
        if rel.starts_with("tests/fixtures/")
            || rel == "tests/fixture_shapes.rs"
            || rel.ends_with(".lock")
        {
            continue;
        }
        let path = std::path::Path::new(root).join(rel);
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };

        for (n, line) in body.lines().enumerate() {
            if let Some(hit) = phone_like(line) {
                panic!(
                    "{rel}:{}: looks like a real phone number or fragment ({hit:?}).                      Use an obviously-fake placeholder.",
                    n + 1
                );
            }
            if let Some(hit) = street_address_like(line) {
                panic!(
                    "{rel}:{}: looks like a real street address ({hit:?}).                      Use the sample address from tests/fixtures/README.md.",
                    n + 1
                );
            }
        }
    }
}

/// A run of digits long enough to identify a phone, or a masked-tail hint next
/// to digits that aren't the conventional placeholder.
fn phone_like(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    if let Some(at) = lower.find("ending in") {
        let tail: String = line[at..].chars().take(32).collect();
        let digits: String = tail
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        // NNNN / 0000 / 1234 read as placeholders; anything else is suspect.
        if !digits.is_empty() && !matches!(digits.as_str(), "0000" | "1234") {
            return Some(digits);
        }
    }
    // A standalone 10/11-digit run that isn't 555-prefixed. Bounded by
    // non-alphanumerics so digits inside a hex checksum or an identifier
    // (`a1b2...0625062526`) don't read as a phone number.
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let before_ok = start == 0 || !chars[start - 1].is_ascii_alphanumeric();
        let after_ok = i >= chars.len() || !chars[i].is_ascii_alphanumeric();
        if run.len() >= 10 && run.len() <= 11 && before_ok && after_ok && !run.contains("555") {
            return Some(run);
        }
    }
    None
}

/// `<number> <Word> <Street-type>` where the number isn't the sample `100`.
fn street_address_like(line: &str) -> Option<String> {
    const TYPES: [&str; 8] = [
        "street",
        "st,",
        "ave",
        "avenue",
        "road",
        "rd,",
        "way",
        "boulevard",
    ];
    let lower = line.to_lowercase();
    if !TYPES.iter().any(|t| lower.contains(t)) {
        return None;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    for w in words.windows(3) {
        let num_ok = w[0].chars().all(|c| c.is_ascii_digit()) && w[0].len() >= 4;
        let type_ok = TYPES.iter().any(|t| {
            w[2].to_lowercase()
                .trim_matches(|c: char| !c.is_alphanumeric())
                == t.trim_end_matches(',')
        });
        if num_ok && type_ok {
            return Some(w.join(" "));
        }
    }
    None
}
