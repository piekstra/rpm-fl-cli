//! The `rpmfl mcp` tools, offline: `tests/fixtures/` behind [`Reads`], checked by
//! cli-common's conformance helper. `tests/mcp_stdio.rs`, `tests/mcp_http.rs` and
//! `tests/mcp_readonly.rs` drive the real binary.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use pk_cli_mcp::conformance::{assert_conforms, assert_fits, call, definitions_size};
use serde_json::json;

use super::*;

const TOOLS: [&str; 5] = [
    "rpm_summary",
    "rpm_properties",
    "rpm_transactions",
    "rpm_rent",
    "rpm_documents",
];

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// What a read fails with, when a test makes it fail.
type Failure = fn() -> CliError;

/// The query of each read, in order.
type Queries = Rc<RefCell<Vec<Vec<(String, String)>>>>;

/// Each path's answer, by default its fixture.
struct Fixtures {
    answers: HashMap<&'static str, Value>,
    /// Ledger pages after the first, when a test sets them.
    later_pages: Vec<Value>,
    reads: Rc<Cell<u32>>,
    queries: Queries,
    failure: Option<Failure>,
}

impl Fixtures {
    fn new() -> Self {
        Self {
            answers: HashMap::from([
                (PROPERTIES, fixture("owner_properties.json")),
                (TRANSACTIONS, fixture("owner_transactions.json")),
                (INCOME, fixture("owner_income_balances.json")),
                (EXPENSES, fixture("owner_expenses_balances.json")),
                (UNPAID, fixture("unpaid_bills_balances.json")),
                (APPROVALS, fixture("owner_decision_requests.json")),
                (FORMS, fixture("pdf_forms_actionable_documents.json")),
                (CHARGES, fixture("tenant_charges.json")),
                (DOCUMENTS, fixture("shared_documents.json")),
            ]),
            later_pages: Vec::new(),
            reads: Rc::default(),
            queries: Rc::default(),
            failure: None,
        }
    }
}

impl Reads for Fixtures {
    fn get(&self, path: &'static str, query: &[(&str, String)]) -> Result<Value, CliError> {
        self.reads.set(self.reads.get() + 1);
        self.queries.borrow_mut().push(
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        );
        if let Some(failure) = self.failure {
            return Err(failure());
        }
        let offset = query
            .iter()
            .find(|(k, _)| *k == "offset")
            .map_or(0, |(_, v)| v.parse::<usize>().unwrap());
        if offset > 0 {
            let page = offset / PAGE_SIZE - 1;
            return Ok(self.later_pages.get(page).cloned().unwrap_or(json!([])));
        }
        Ok(self.answers[path].clone())
    }
}

fn tools_over(fixtures: Fixtures) -> RpmTools<Fixtures> {
    RpmTools::new(fixtures, false)
}

fn tools() -> RpmTools<Fixtures> {
    tools_over(Fixtures::new())
}

fn ok(tools: &RpmTools<Fixtures>, name: &str, args: Value) -> Value {
    let result = call(tools, name, args);
    assert_fits(tools, name, &result).clone()
}

fn error(tools: &RpmTools<Fixtures>, name: &str, args: Value) -> String {
    let result = call(tools, name, args);
    assert!(result.is_error(), "{name}: {}", result.text());
    result.text().to_string()
}

fn over_answer(path: &'static str, answer: Value) -> RpmTools<Fixtures> {
    let mut fixtures = Fixtures::new();
    fixtures.answers.insert(path, answer);
    tools_over(fixtures)
}

#[test]
fn the_tools_conform_and_fit_the_definition_budget() {
    let tools = tools();
    assert_conforms(&tools);
    let size = definitions_size(&tools);
    assert!(size < 8_000, "{size} characters");
    let names: Vec<String> = tools.tools().into_iter().map(|t| t.name).collect();
    assert_eq!(names, TOOLS);
}

/// Listing reads nothing, so it needs no credential.
#[test]
fn listing_the_tools_reads_nothing() {
    let fixtures = Fixtures::new();
    let reads = fixtures.reads.clone();
    let tools = tools_over(fixtures);
    let _ = tools.tools();
    let _ = pk_cli_mcp::instructions(&tools);
    assert_eq!(reads.get(), 0);
}

/// Every tool is a read: annotated so, and with no approval prompt, which only a
/// write carries. Each verb the portal could act on is refused by name, and
/// `tests/mcp_readonly.rs` shows that what the tools send is GETs alone.
#[test]
fn no_tool_definition_reaches_a_mutating_command() {
    const MUTATING: [&str; 13] = [
        "approve",
        "approvals",
        "sign",
        "forms",
        "contribute",
        "contributions",
        "pay",
        "edit",
        "update",
        "delete",
        "upload",
        "login",
        "api",
    ];
    for tool in tools().tools() {
        assert!(tool.annotations.read_only, "{}", tool.name);
        assert!(!tool.annotations.destructive, "{}", tool.name);
        assert!(!tool.requires_user_interaction(), "{}", tool.name);
        let verb = tool.name.strip_prefix("rpm_").unwrap();
        assert!(
            !MUTATING.contains(&verb),
            "{} is named for a mutating command",
            tool.name
        );
    }
}

/// Bad arguments cost no request, and no keychain read before it.
#[test]
fn bad_arguments_fail_before_any_read() {
    for (name, args) in [
        ("rpm_summary", json!({"since": "2026-13-01"})),
        (
            "rpm_summary",
            json!({"since": "2026-02-01", "until": "2026-01-01"}),
        ),
        ("rpm_transactions", json!({"until": "soon"})),
        ("rpm_transactions", json!({"kind": "refund"})),
        ("rpm_rent", json!({"since": "yesterday"})),
        ("rpm_properties", json!({"limit": 0})),
        ("rpm_documents", json!({"cursor": "not-a-cursor"})),
    ] {
        let fixtures = Fixtures::new();
        let reads = fixtures.reads.clone();
        let text = error(&tools_over(fixtures), name, args.clone());
        assert!(text.starts_with("usage:"), "{name} {args}: {text}");
        assert_eq!(reads.get(), 0, "{name} {args}");
    }
}

/// A bad range names the tool's arguments, not a command's flags.
#[test]
fn an_inverted_period_names_the_arguments() {
    let text = error(
        &tools(),
        "rpm_summary",
        json!({"since": "2026-02-01", "until": "2026-01-01"}),
    );
    assert!(text.contains("since 2026-02-01"), "{text}");
    assert!(!text.contains("--"), "{text}");
}

#[test]
fn summary_totals_the_period() {
    let got = ok(
        &tools(),
        "rpm_summary",
        json!({"since": "2026-01-01", "until": "2026-08-06"}),
    );
    assert_eq!(got["since"], "2026-01-01");
    assert_eq!(got["until"], "2026-08-06");
    assert_eq!(got["cash_in"], "5000.00");
    assert_eq!(got["cash_out"], "5000.00");
    assert_eq!(got["disbursements"], "10000.00");
    assert_eq!(got["net"], "0.00");
    assert_eq!(got["unpaid_bills"], "0.00");
    assert_eq!(got["approvals_waiting"], 0);
    assert_eq!(got["forms_waiting"], 0);
}

#[test]
fn summary_sends_the_portals_date_form() {
    let fixtures = Fixtures::new();
    let queries = fixtures.queries.clone();
    ok(
        &tools_over(fixtures),
        "rpm_summary",
        json!({"since": "2026-01-02", "until": "2026-03-04"}),
    );
    let first = queries.borrow()[0].clone();
    assert!(
        first.contains(&("start_on".into(), "01/02/2026".into())),
        "{first:?}"
    );
    assert!(
        first.contains(&("end_on".into(), "03/04/2026".into())),
        "{first:?}"
    );
}

#[test]
fn a_summary_no_field_states_is_an_error_not_zero() {
    for (path, answer) in [
        (INCOME, json!({"rows": []})),
        (INCOME, json!([{"type": "cashIn", "total": 5.0}])),
        (INCOME, json!([{"type": "cash_in", "totalAmount": 5.0}])),
        (EXPENSES, json!([{"kind": "cashOut", "totalAmount": 5.0}])),
        (UNPAID, json!([{"propertyId": "1", "balance": 1.0}])),
        (UNPAID, json!({})),
        (APPROVALS, json!({"owner_decision_requests": []})),
        (FORMS, json!({"documents": []})),
        (FORMS, json!({"documents_by_owner": [{"owner": {}}]})),
    ] {
        let text = error(&over_answer(path, answer.clone()), "rpm_summary", json!({}));
        assert!(text.starts_with("upstream:"), "{path} {answer}: {text}");
    }
}

/// A count of forms is counted across owners.
#[test]
fn forms_waiting_counts_every_owners_documents() {
    let got = ok(
        &over_answer(
            FORMS,
            json!({"documents_by_owner": [{"documents": [{}, {}]}, {"documents": [{}]}]}),
        ),
        "rpm_summary",
        json!({}),
    );
    assert_eq!(got["forms_waiting"], 3);
}

#[test]
fn properties_list_occupancy_and_the_earliest_lease_end() {
    let got = ok(&tools(), "rpm_properties", json!({}));
    assert_eq!(got["returned"], 1);
    let item = &got["items"][0];
    assert_eq!(item["name"], "Sample St 100");
    assert_eq!(item["address"], "100 Sample St, Sample City, ST 00000");
    assert_eq!(item["units"], 1);
    assert_eq!(item["occupied"], 1);
    assert_eq!(item["rent"], "1000.00");
    assert_eq!(item["lease_end"], "2030-01-31");
    assert!(item.get("id").is_none(), "{item}");
    assert!(item.get("minimum_cash_reserve").is_none(), "{item}");

    let got = ok(
        &tools(),
        "rpm_properties",
        json!({"response_format": "detailed"}),
    );
    assert_eq!(got["items"][0]["id"], "1");
    assert_eq!(got["items"][0]["minimum_cash_reserve"], 500.0);
}

fn property_with_units(units: Value) -> Value {
    let mut properties = fixture("owner_properties.json");
    properties[0]["relationships"]["units"] = units;
    properties
}

#[test]
fn a_vacant_unit_may_have_no_rent_and_the_earliest_lease_wins() {
    let lease = |end: &str| json!({"relationships": {"lease": {"attributes": {"end_on": end}}}});
    let got = ok(
        &over_answer(
            PROPERTIES,
            property_with_units(json!([
                {"attributes": {"occupied": true, "unit_total_rent_and_subsidy": 100.0},
                 "relationships": {"current_occupancy": lease("2031-01-31")}},
                {"attributes": {"occupied": true, "unit_total_rent_and_subsidy": 50.5},
                 "relationships": {"current_occupancy": lease("2030-06-30")}},
                {"attributes": {"occupied": false}},
            ])),
        ),
        "rpm_properties",
        json!({}),
    );
    let item = &got["items"][0];
    assert_eq!(item["units"], 3);
    assert_eq!(item["occupied"], 2);
    assert_eq!(item["rent"], "150.50");
    assert_eq!(item["lease_end"], "2030-06-30");
}

#[test]
fn a_property_that_changed_shape_is_an_error_not_zero_rent() {
    for properties in [
        json!({"properties": []}),
        property_with_units(json!({})),
        // A let unit whose rent field went away.
        property_with_units(json!([{"attributes": {"occupied": true}}])),
        // The occupancy flag went away.
        property_with_units(json!([{"attributes": {"unit_total_rent_and_subsidy": 1.0}}])),
    ] {
        let tools = over_answer(PROPERTIES, properties.clone());
        let text = error(&tools, "rpm_properties", json!({}));
        assert!(text.starts_with("upstream:"), "{properties}: {text}");
    }
    let mut renamed = fixture("owner_properties.json");
    renamed[0]["attributes"]
        .as_object_mut()
        .unwrap()
        .remove("raw_display_name");
    let text = error(
        &over_answer(PROPERTIES, renamed),
        "rpm_properties",
        json!({}),
    );
    assert!(text.starts_with("upstream:"), "{text}");
}

#[test]
fn transactions_list_the_ledger_without_its_attachments() {
    let got = ok(&tools(), "rpm_transactions", json!({"since": "2026-01-01"}));
    let total = fixture("owner_transactions.json").as_array().unwrap().len();
    assert_eq!(got["returned"].as_u64().unwrap() as usize, total.min(20));
    assert_eq!(got["since"], "2026-01-01");
    let item = &got["items"][0];
    assert_eq!(item["date"], "2026-08-01");
    assert_eq!(item["kind"], "cash_in");
    assert_eq!(item["amount"], "1000.00");
    assert_eq!(item["party"], "Sample Tenant One, Sample Tenant Two");
    assert_eq!(item["property"], "Sample St 100");
    assert!(item.get("attachments").is_none(), "{item}");
    assert!(item.get("id").is_none(), "{item}");

    let got = ok(
        &tools(),
        "rpm_transactions",
        json!({"response_format": "detailed", "limit": 1}),
    );
    assert_eq!(got["items"][0]["id"], "1057");
}

fn ledger_row(id: u64, kind: &str) -> Value {
    json!({
        "id": id.to_string(),
        "postedOn": "2026-07-01",
        "amount": 10.0,
        "type": kind,
        "propertyId": "1",
        "propertyDisplayName": "Sample St 100",
        "partyName": "Sample Party",
        "description": "Sample transaction description",
        "attachments": [],
    })
}

/// The portal answers HTTP 500 to `type=disbursement`, so a kind is never sent.
#[test]
fn transactions_filter_by_kind_here_and_never_ask_the_portal_to() {
    let fixtures = Fixtures::new();
    let queries = fixtures.queries.clone();
    let got = ok(
        &tools_over(fixtures),
        "rpm_transactions",
        json!({"kind": "disbursement", "limit": 50}),
    );
    let items = got["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i["kind"] == "disbursement"), "{got}");
    for query in queries.borrow().iter() {
        assert!(!query.iter().any(|(k, _)| k == "type"), "{query:?}");
    }
}

/// The match is past the first page, and the walk reaches it.
#[test]
fn transactions_walk_the_ledger_and_offer_a_cursor() {
    let mut fixtures = Fixtures::new();
    fixtures.answers.insert(
        TRANSACTIONS,
        Value::Array(
            (0..PAGE_SIZE as u64)
                .map(|i| ledger_row(i, "cashIn"))
                .collect(),
        ),
    );
    fixtures.later_pages = vec![
        Value::Array(
            (0..PAGE_SIZE as u64)
                .map(|i| ledger_row(1000 + i, "disbursement"))
                .collect(),
        ),
        Value::Array(vec![ledger_row(2000, "disbursement")]),
    ];
    let tools = tools_over(fixtures);

    let first = ok(
        &tools,
        "rpm_transactions",
        json!({"kind": "disbursement", "limit": 100, "response_format": "detailed"}),
    );
    assert_eq!(first["returned"], 100, "{}", first["hint"]);
    assert_eq!(first["items"][0]["id"], "1000");
    let cursor = first["next_cursor"].as_str().expect("a next cursor");

    let next = ok(
        &tools,
        "rpm_transactions",
        json!({"kind": "disbursement", "limit": 100, "response_format": "detailed", "cursor": cursor}),
    );
    assert_eq!(next["returned"], 1);
    assert_eq!(next["items"][0]["id"], "2000");
    assert!(next.get("next_cursor").is_none(), "{next}");
}

/// A cursor from one query does not continue another.
#[test]
fn a_cursor_is_bound_to_its_query() {
    let tools = tools();
    let first = ok(
        &tools,
        "rpm_transactions",
        json!({"since": "2026-01-01", "limit": 1}),
    );
    let cursor = first["next_cursor"].as_str().expect("a next cursor");
    let text = error(
        &tools,
        "rpm_transactions",
        json!({"since": "2026-01-01", "kind": "cash_out", "limit": 1, "cursor": cursor}),
    );
    assert!(text.starts_with("usage:"), "{text}");
}

#[test]
fn a_ledger_past_the_most_one_call_reads_is_an_error_not_a_short_list() {
    let mut fixtures = Fixtures::new();
    let full: Vec<Value> = (0..PAGE_SIZE as u64)
        .map(|i| ledger_row(i, "cashIn"))
        .collect();
    fixtures
        .answers
        .insert(TRANSACTIONS, Value::Array(full.clone()));
    fixtures.later_pages = vec![Value::Array(full); MAX_PAGES];
    let reads = fixtures.reads.clone();
    let text = error(
        &tools_over(fixtures),
        "rpm_transactions",
        json!({"kind": "disbursement"}),
    );
    assert!(text.starts_with("usage:"), "{text}");
    assert_eq!(reads.get() as usize, MAX_PAGES);
}

#[test]
fn a_ledger_that_changed_shape_is_an_error_not_an_empty_list() {
    let mut no_kind = fixture("owner_transactions.json");
    no_kind[0].as_object_mut().unwrap().remove("type");
    let mut new_kind = fixture("owner_transactions.json");
    new_kind[0]["type"] = json!("cash_in");
    let mut no_amount = fixture("owner_transactions.json");
    no_amount[0].as_object_mut().unwrap().remove("amount");
    for ledger in [json!({"transactions": []}), no_kind, new_kind] {
        let tools = over_answer(TRANSACTIONS, ledger.clone());
        let text = error(&tools, "rpm_transactions", json!({"kind": "disbursement"}));
        assert!(text.starts_with("upstream:"), "{text}");
    }
    let tools = over_answer(TRANSACTIONS, no_amount);
    let text = error(&tools, "rpm_transactions", json!({}));
    assert!(text.starts_with("upstream:"), "{text}");

    // With a property named, the row's own property id is what filters it.
    let mut no_property = fixture("owner_transactions.json");
    no_property[0].as_object_mut().unwrap().remove("propertyId");
    let text = error(
        &over_answer(TRANSACTIONS, no_property),
        "rpm_transactions",
        json!({"property": "Sample St 100"}),
    );
    assert!(text.starts_with("upstream:"), "{text}");
}

#[test]
fn a_named_property_narrows_by_its_id() {
    let fixtures = Fixtures::new();
    let queries = fixtures.queries.clone();
    let got = ok(
        &tools_over(fixtures),
        "rpm_transactions",
        json!({"property": "sample st", "limit": 1}),
    );
    assert_eq!(got["returned"], 1);
    let walked = queries.borrow().last().unwrap().clone();
    assert!(
        walked.contains(&("property_ids".into(), "1".into())),
        "{walked:?}"
    );
}

/// The portal's filter ignored, or its reply mixed: a row of another property
/// does not count as this one's.
#[test]
fn rows_of_another_property_are_left_out() {
    let mut ledger = fixture("owner_transactions.json");
    for row in ledger.as_array_mut().unwrap() {
        row["propertyId"] = json!("2");
    }
    let got = ok(
        &over_answer(TRANSACTIONS, ledger),
        "rpm_transactions",
        json!({"property": "Sample St 100"}),
    );
    assert_eq!(got["returned"], 0);
}

#[test]
fn an_unknown_property_says_how_to_find_one() {
    let text = error(&tools(), "rpm_rent", json!({"property": "nowhere"}));
    assert!(text.starts_with("not_found:"), "{text}");
    assert!(text.contains("rpm_properties"), "{text}");
}

#[test]
fn rent_lists_charges_newest_first_with_what_is_owed() {
    let mut charges = fixture("tenant_charges.json");
    charges[0]["balance"] = json!(12.5);
    charges[1]["balance"] = json!(1.25);
    let got = ok(
        &over_answer(CHARGES, charges),
        "rpm_rent",
        json!({"since": "2026-01-01", "until": "2026-08-06", "limit": 100}),
    );
    assert_eq!(got["outstanding"], "13.75");
    assert_eq!(got["until"], "2026-08-06");
    let items = got["items"].as_array().unwrap();
    assert_eq!(items.len(), 8);
    assert_eq!(items[0]["property"], "Sample St 100");
    let dates: Vec<&str> = items.iter().map(|i| i["date"].as_str().unwrap()).collect();
    let mut sorted = dates.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(dates, sorted);
    assert!(items[0].get("id").is_none(), "{got}");
}

#[test]
fn rent_for_another_property_is_left_out_of_the_rows_and_the_total() {
    let mut charges = fixture("tenant_charges.json");
    charges[0]["balance"] = json!(12.5);
    charges[0]["propertyId"] = json!("2");
    let got = ok(
        &over_answer(CHARGES, charges),
        "rpm_rent",
        json!({"property": "Sample St 100"}),
    );
    assert_eq!(got["outstanding"], "0.00");
    assert_eq!(got["returned"], 7);
}

#[test]
fn rent_that_changed_shape_is_an_error_not_nothing_owed() {
    for field in ["balance", "amount", "occurredOn", "propertyId"] {
        let mut charges = fixture("tenant_charges.json");
        charges[0].as_object_mut().unwrap().remove(field);
        let text = error(&over_answer(CHARGES, charges), "rpm_rent", json!({}));
        assert!(text.starts_with("upstream:"), "{field}: {text}");
    }
    let text = error(&over_answer(CHARGES, json!({})), "rpm_rent", json!({}));
    assert!(text.starts_with("upstream:"), "{text}");
}

/// A pre-signed link works with no sign-in, so no format returns one.
#[test]
fn documents_never_carry_a_link() {
    let tools = tools();
    for args in [json!({}), json!({"response_format": "detailed"})] {
        let result = call(&tools, "rpm_documents", args.clone());
        assert_fits(&tools, "rpm_documents", &result);
        assert!(!result.text().contains("example.invalid"), "{args}");
        assert!(!result.text().contains("url"), "{args}");
    }
}

#[test]
fn documents_list_newest_first_and_filter_by_name() {
    let got = ok(&tools(), "rpm_documents", json!({"limit": 100}));
    let items = got["items"].as_array().unwrap();
    assert!(items.len() >= 2, "{got}");
    assert_eq!(items[0]["name"], "Owner_1099_2025.pdf");
    assert_eq!(items[0]["category"], "shared");
    assert_eq!(items[0]["shared"], "2026-01-18");
    assert!(items[0].get("id").is_none(), "{got}");

    let got = ok(&tools(), "rpm_documents", json!({"query": "w-9"}));
    assert_eq!(got["returned"], 1);
    assert_eq!(got["items"][0]["name"], "W-9.pdf");

    let got = ok(&tools(), "rpm_documents", json!({"query": "no such file"}));
    assert_eq!(got["returned"], 0);
}

#[test]
fn documents_that_changed_shape_are_an_error_not_an_empty_list() {
    for answer in [
        json!({"owners": []}),
        json!([{"id": 100, "files": []}]),
        json!([{"id": 100, "documents": {}}]),
        json!([{"id": 100, "documents": [{"title": "Sample.pdf"}]}]),
    ] {
        let tools = over_answer(DOCUMENTS, answer.clone());
        let text = error(&tools, "rpm_documents", json!({}));
        assert!(text.starts_with("upstream:"), "{answer}: {text}");
    }
    // An owner with nothing shared yet is an empty list.
    let got = ok(
        &over_answer(DOCUMENTS, json!([{"documents": []}])),
        "rpm_documents",
        json!({}),
    );
    assert_eq!(got["returned"], 0);
    let got = ok(
        &over_answer(DOCUMENTS, json!([])),
        "rpm_documents",
        json!({}),
    );
    assert_eq!(got["returned"], 0);
}

#[test]
fn portal_failures_keep_their_kind() {
    let cases: [(Failure, &str); 3] = [
        (
            || CliError::Auth("session expired — run `rpmfl auth login`".into()),
            "auth:",
        ),
        (|| CliError::Upstream("HTTP 503".into()), "upstream:"),
        (|| CliError::NotFound("gone".into()), "not_found:"),
    ];
    for (failure, slug) in cases {
        let mut fixtures = Fixtures::new();
        fixtures.failure = Some(failure);
        let tools = tools_over(fixtures);
        for name in TOOLS {
            let text = error(&tools, name, json!({}));
            assert!(text.starts_with(slug), "{name}: {text}");
        }
    }
}

/// Signing in may text the user a code, and `auth login` is the one command for it.
#[test]
fn a_lapsed_session_names_one_command() {
    let mut fixtures = Fixtures::new();
    fixtures.failure = Some(|| CliError::Auth("session expired — run `rpmfl auth login`".into()));
    let text = error(&tools_over(fixtures), "rpm_summary", json!({}));
    assert_eq!(text.matches("rpmfl auth login").count(), 1, "{text}");
}

#[test]
fn while_the_session_is_in_the_variable_the_remedy_names_the_variable() {
    let mut fixtures = Fixtures::new();
    fixtures.failure = Some(|| CliError::Auth("session expired".into()));
    let tools = RpmTools::new(fixtures, true);
    let text = error(&tools, "rpm_summary", json!({}));
    assert!(text.contains("$RPMFL_SESSION"), "{text}");
    assert!(!text.contains("auth login"), "{text}");
}

#[test]
fn a_missing_host_is_not_a_tool_arguments_problem() {
    let e = no_host(CliError::Usage("no portal host".into()));
    assert_eq!(e.exit_code(), 3, "{e}");
}

struct Keychain(Option<&'static str>);

impl KeychainRead for Keychain {
    fn get(&self, _account: &str) -> Result<Option<Secret>, CliError> {
        Ok(self.0.map(Secret::new))
    }
    fn service(&self) -> &str {
        "piekstra.rpmfl"
    }
}

#[test]
fn the_session_comes_from_the_variable_then_the_keychain() {
    let unset = "RPMFL_TEST_VARIABLE_NOBODY_SETS";
    let got = resolve_session(&Keychain(Some("from-keychain")), unset).unwrap();
    assert_eq!(got.expose(), "from-keychain");

    let e = resolve_session(&Keychain(None), unset).expect_err("none stored");
    assert_eq!(e.exit_code(), 3, "{e}");

    // `CARGO_MANIFEST_DIR` is set for every test run, and stands in for a session.
    let got = resolve_session(&Keychain(Some("from-keychain")), "CARGO_MANIFEST_DIR").unwrap();
    assert_eq!(got.expose(), env!("CARGO_MANIFEST_DIR"));
}

fn secret(value: &str) -> Secret {
    Secret::new(value)
}

#[test]
fn a_rotated_cookie_is_sent_while_the_stored_session_is_the_one_it_replaced() {
    let mut rotated = Rotated::default();
    let stored = secret("stored");
    assert_eq!(rotated.current(&stored).expose(), "stored");

    rotated.record(&stored, &Ok(json!([])), Some(secret("rotated")));
    assert_eq!(rotated.current(&stored).expose(), "rotated");

    // A new sign-in changed what is stored, which retires the rotated copy.
    assert_eq!(
        rotated.current(&secret("signed-in-again")).expose(),
        "signed-in-again"
    );
}

#[test]
fn a_rejected_cookie_is_dropped_and_a_network_failure_keeps_it() {
    let stored = secret("stored");
    let mut rotated = Rotated::default();
    rotated.record(&stored, &Ok(json!([])), Some(secret("rotated")));

    rotated.record(&stored, &Err(CliError::Upstream("HTTP 503".into())), None);
    assert_eq!(rotated.current(&stored).expose(), "rotated");

    rotated.record(&stored, &Err(CliError::Auth("expired".into())), None);
    assert_eq!(rotated.current(&stored).expose(), "stored");
}
