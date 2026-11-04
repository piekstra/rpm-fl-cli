//! `rpmfl mcp`: read-only tools for Claude, over the reads the commands make.
//!
//! Every read goes through [`Reads`], which can only send a GET to a fixed path,
//! and tests put fixtures behind it, so nothing here needs a login to test.
//!
//! The server reads the cached session and nothing else. It never sends the
//! password, never asks for a two-factor code (each one is a text message to a
//! person) and never writes the keychain: a lapsed session is an auth error naming
//! `rpmfl auth login`. The portal may rotate the session cookie on any response;
//! the server keeps the newest copy in memory, for as long as the keychain still
//! holds the session it came from, so a new sign-in takes effect at once. Only
//! `rpmfl` commands advance the stored one, and every new server starts from it,
//! which on stdio means each launch or session of the client. If it has gone stale,
//! `rpmfl auth login` is needed even though the session was in use. A write back
//! from a background server could block on a keychain dialog, so that is the trade
//! made here.
//!
//! The commands' mappers are best-effort, for a person who can see the raw
//! payload. A tool's answer goes to a model as fact, so an answer the portal did
//! not give (a total no field states, a list its answer no longer holds) is an
//! upstream error here, never 0.00 or an empty list.
//!
//! Document rows and ledger attachments carry pre-signed links that need no
//! sign-in. No tool returns one. Approval and form rows are only counted: no
//! non-empty one has been captured, so what they hold is unknown.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use pk_cli_config::ConfigStore;
use pk_cli_core::transport::require_secure_url;
use pk_cli_core::{dates, resolve, CliError};
use pk_cli_mcp::{
    parse_args, ListArgs, Order, Page, PageRequest, PositionKey, PositionRequest, Server, ToolDef,
    ToolError, ToolOutput, Toolset,
};
use pk_cli_secrets::{CredentialStore, KeychainRead, Secret, SecretResolver, SecretSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::Portal;
use crate::commands::documents::GROUPS;
use crate::commands::summary::total_of;
use crate::config::{Config, SESSION_ACCOUNT};
use crate::dates::RangeArgs;

/// The session cookie's environment variable, read before the keychain.
const SESSION_ENV: &str = "RPMFL_SESSION";

/// The paths a tool may read. [`Reads::get`] takes a `&'static str`, so a tool
/// cannot build one from an argument.
const PROPERTIES: &str = "/oportal/api/owner_properties";
const TRANSACTIONS: &str = "/oportal/api/owner_transactions";
const INCOME: &str = "/oportal/api/owner_income_balances";
const EXPENSES: &str = "/oportal/api/owner_expenses_balances";
const UNPAID: &str = "/oportal/api/unpaid_bills_balances";
const APPROVALS: &str = "/oportal/api/owner_decision_requests";
const FORMS: &str = "/oportal/api/pdf_forms/actionable_documents";
const CHARGES: &str = "/oportal/api/tenant_charges";
const DOCUMENTS: &str = "/oportal/api/shared_documents/";

/// Rows per request when walking the ledger.
const PAGE_SIZE: usize = 100;
/// Most ledger pages one call walks.
const MAX_PAGES: usize = 50;
/// How long one call walks the ledger: below `pk_cli_mcp::CALL_LIMIT`, so a slow portal is told to
/// narrow the period instead of being answered at the limit with nobody told why.
const WALK_BUDGET: Duration = Duration::from_secs(90);

/// The tools `rpmfl mcp` serves. Nothing here reads the config or the keychain:
/// a client lists tools before anyone has signed in.
pub fn tools(timeout: Option<u64>) -> RpmTools<LivePortal> {
    RpmTools::new(
        LivePortal {
            timeout,
            rotated: Mutex::default(),
        },
        std::env::var_os(SESSION_ENV).is_some(),
    )
}

/// The one way a tool reaches the portal: a GET to a fixed path. It offers no way
/// to send a form, which is what every portal write takes.
pub trait Reads {
    fn get(&self, path: &'static str, query: &[(&str, String)]) -> Result<Value, CliError>;
}

/// [`Reads`] over the portal, reading the config and the session on each call.
pub struct LivePortal {
    /// `--timeout`, which wins over the config.
    timeout: Option<u64>,
    rotated: Mutex<Rotated>,
}

/// The newest session cookie the portal issued, and the stored one it replaced.
/// It is memory only, since a tool does not write the keychain. A new sign-in
/// changes the stored value, which retires this.
#[derive(Default)]
struct Rotated(Option<(Secret, Secret)>);

impl Rotated {
    /// What to send, given what is stored now.
    fn current(&self, stored: &Secret) -> Secret {
        match &self.0 {
            Some((from, newest)) if from.expose() == stored.expose() => {
                Secret::new(newest.expose())
            }
            _ => Secret::new(stored.expose()),
        }
    }

    /// Remember the cookie a good read left behind. A slower read can finish after
    /// a newer cookie arrived, and must not put its older one back.
    ///
    /// An auth failure clears nothing: a 403 or an HTML page may be the portal's
    /// edge and not a refused session, and a new sign-in retires this copy anyway.
    fn record(&mut self, stored: &Secret, sent: &Secret, newest: Option<Secret>) {
        let Some(newest) = newest else {
            return;
        };
        if self.current(stored).expose() == sent.expose() {
            self.0 = Some((Secret::new(stored.expose()), newest));
        }
    }
}

impl LivePortal {
    fn lock(&self) -> std::sync::MutexGuard<'_, Rotated> {
        self.rotated.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Reads for LivePortal {
    fn get(&self, path: &'static str, query: &[(&str, String)]) -> Result<Value, CliError> {
        let mut cfg: Config = ConfigStore::new("rpmfl").load().map_err(bad_config)?;
        if let Some(secs) = self.timeout {
            cfg.timeout_secs = Some(secs);
        }
        let base = cfg.base_url().map_err(no_host)?;
        // Before the keychain, so a host off https gets no session and raises no dialog.
        require_secure_url(&base).map_err(bad_config)?;
        // The keychain may wait on an approval dialog, and the portal on the
        // network, so the lock is held for neither.
        let creds = CredentialStore::for_binary("rpmfl");
        let stored = crate::diag::keychain(false, || resolve_session(&creds, SESSION_ENV))?;
        let send = self.lock().current(&stored);

        // A fresh jar per read: no cookie outlives the call but the session.
        let portal = Portal::new(base, cfg.timeout())?;
        portal.seed_session(send.expose());
        let read = portal.get(path, query);
        if read.is_ok() {
            self.lock().record(&stored, &send, portal.session_cookie());
        }
        read
    }
}

/// A command's usage error, for a bad config file or host. No tool argument can fix it.
fn bad_config(e: CliError) -> CliError {
    match e {
        CliError::Usage(message) => CliError::Other(message),
        other => other,
    }
}

/// A command's usage error, for a host nobody has configured. No tool argument
/// can fix it, and signing in cannot start without it, so it is an auth error.
fn no_host(_: CliError) -> CliError {
    CliError::Auth(
        "no portal host is configured: ask the user to set one with `rpmfl config set base_url \
         https://<subdomain>.appfolio.com`"
            .into(),
    )
}

/// The family's secret precedence: the variable, then the keychain.
fn resolve_session(keychain: &impl KeychainRead, env: &str) -> Result<Secret, CliError> {
    let spec = SecretSpec::new(SESSION_ACCOUNT)
        .env(env)
        .remedy("`rpmfl auth login`");
    // A variable set but empty is a usage error on a command line. Here no tool
    // argument can fix it, and the model would be told to try another.
    let found = SecretResolver::new(keychain)
        .resolve(&spec)
        .map_err(|e| match e {
            CliError::Usage(message) => CliError::Auth(message),
            other => other,
        })?;
    found
        .map(|r| r.secret)
        .ok_or_else(|| CliError::Auth("no portal session is stored".into()))
}

fn shape_error(what: &str) -> ToolError {
    ToolError::upstream(format!(
        "the portal's {what} did not have the expected shape, so it is not reported here"
    ))
}

/// A decimal string, with the portal's `-0.0` as plain zero.
fn dollars(amount: f64) -> String {
    format!("{:.2}", if amount == 0.0 { 0.0 } else { amount })
}

fn number(row: &Value, key: &str) -> Option<f64> {
    row.get(key).and_then(Value::as_f64)
}

fn text(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The portal's ids arrive as strings or numbers.
fn scalar(row: &Value, key: &str) -> Option<String> {
    match row.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn rows<'a>(payload: &'a Value, what: &str) -> Result<&'a Vec<Value>, ToolError> {
    payload.as_array().ok_or_else(|| shape_error(what))
}

/// A period's bounds, both as the caller reads them and as the portal does.
struct Period {
    since: String,
    until: String,
    start_on: String,
    end_on: String,
}

impl Period {
    fn of(since: &Option<String>, until: &Option<String>) -> Result<Self, ToolError> {
        let range = RangeArgs {
            since: valid_day("since", since)?,
            until: valid_day("until", until)?,
        };
        // The range's own check words the bounds as a command's flags.
        let flagless =
            |e: CliError| ToolError::usage(ToolError::from(e).message().replace("--", ""));
        let (since, until) = range.resolve_iso().map_err(flagless)?;
        let (start_on, end_on) = range.resolve().map_err(flagless)?;
        Ok(Self {
            since,
            until,
            start_on,
            end_on,
        })
    }
}

impl Period {
    /// `tenant_charges` takes no paging arguments and gives no sign of a cap, and
    /// the longest answer observed is under a year's. A wider ask could come back
    /// short and pass for the whole, so it is refused.
    fn at_most_a_year(&self) -> Result<(), ToolError> {
        let (since, until) = (
            dates::parse_iso(&self.since)?,
            dates::parse_iso(&self.until)?,
        );
        if since < (until.0 - 1, until.1, until.2) {
            return Err(ToolError::usage(format!(
                "rent charges are read at most a year at a time, and {} to {} is longer; \
                 narrow since/until and call again for the rest",
                self.since, self.until
            )));
        }
        Ok(())
    }
}

/// A date argument, checked here so its error names the argument.
fn valid_day(arg: &str, value: &Option<String>) -> Result<Option<String>, ToolError> {
    let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    dates::parse_iso(value)
        .map_err(|e| ToolError::usage(format!("{arg}: {}", ToolError::from(e).message())))?;
    Ok(Some(value.to_string()))
}

/// `rpm_summary`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
struct SummaryArgs {
    /// First day, YYYY-MM-DD (default: January 1 of this year).
    #[serde(default)]
    since: Option<String>,
    /// Last day, YYYY-MM-DD (default: today).
    #[serde(default)]
    until: Option<String>,
}

/// `rpm_summary`'s result.
#[derive(Debug, Serialize, JsonSchema)]
struct SummaryOut {
    since: String,
    until: String,
    cash_in: String,
    cash_out: String,
    disbursements: String,
    net: String,
    unpaid_bills: String,
    approvals_waiting: u64,
    forms_waiting: u64,
}

/// `rpm_properties`' arguments.
#[derive(Debug, Deserialize, JsonSchema)]
struct PropertyArgs {
    #[serde(flatten)]
    list: ListArgs,
}

/// One owned property.
#[derive(Debug, Serialize, JsonSchema)]
struct PropertyItem {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<String>,
    units: u64,
    occupied: u64,
    rent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_end: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum_cash_reserve: Option<String>,
}

/// A ledger entry's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Kind {
    CashIn,
    CashOut,
    Disbursement,
}

impl Kind {
    fn from_wire(wire: &str) -> Option<Self> {
        match wire {
            "cashIn" => Some(Self::CashIn),
            "cashOut" => Some(Self::CashOut),
            "disbursement" => Some(Self::Disbursement),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CashIn => "cash_in",
            Self::CashOut => "cash_out",
            Self::Disbursement => "disbursement",
        }
    }
}

/// `rpm_transactions`' arguments.
#[derive(Debug, Deserialize, JsonSchema)]
struct TransactionArgs {
    /// First day, YYYY-MM-DD (default: January 1 of this year).
    #[serde(default)]
    since: Option<String>,
    /// Last day, YYYY-MM-DD (default: today).
    #[serde(default)]
    until: Option<String>,
    /// Only this kind of entry.
    #[serde(default)]
    kind: Option<Kind>,
    /// A property's name or a unique part of its address.
    #[serde(default)]
    property: Option<String>,
    #[serde(flatten)]
    list: ListArgs,
}

/// One ledger entry, without the attachment links its row carries.
#[derive(Debug, Serialize, JsonSchema)]
struct TransactionItem {
    date: String,
    kind: String,
    amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    party: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    property: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

/// `rpm_transactions`' result: a page, and which period it covers.
#[derive(Debug, Serialize, JsonSchema)]
struct TransactionsOut {
    since: String,
    until: String,
    #[serde(flatten)]
    page: Page<TransactionItem>,
}

/// `rpm_rent`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
struct RentArgs {
    /// First day, YYYY-MM-DD (default: January 1 of this year).
    #[serde(default)]
    since: Option<String>,
    /// Last day, YYYY-MM-DD (default: today).
    #[serde(default)]
    until: Option<String>,
    /// A property's name or a unique part of its address.
    #[serde(default)]
    property: Option<String>,
    #[serde(flatten)]
    list: ListArgs,
}

/// One rent charge to a tenant.
#[derive(Debug, Serialize, JsonSchema)]
struct RentItem {
    date: String,
    amount: String,
    balance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    property: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

/// `rpm_rent`'s result: a page, which period it covers, and what is still owed.
#[derive(Debug, Serialize, JsonSchema)]
struct RentOut {
    since: String,
    until: String,
    outstanding: String,
    #[serde(flatten)]
    page: Page<RentItem>,
}

/// `rpm_documents`' arguments.
#[derive(Debug, Deserialize, JsonSchema)]
struct DocumentArgs {
    /// Text to find in document names, in any case.
    #[serde(default)]
    query: Option<String>,
    #[serde(flatten)]
    list: ListArgs,
}

/// One shared document, without the link its row carries.
#[derive(Debug, Serialize, JsonSchema)]
struct DocumentItem {
    name: String,
    category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    shared: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

/// One page's filters, for binding its cursor.
#[derive(Serialize)]
struct Filter<'a> {
    since: &'a str,
    until: &'a str,
    kind: Option<&'a str>,
    property: Option<&'a str>,
}

/// One owned property, read strictly: [`crate::commands::properties`] takes a
/// missing field as zero, which a model would repeat as fact.
struct Property {
    id: String,
    name: String,
    address: Option<String>,
    units: u64,
    occupied: u64,
    rent: f64,
    lease_end: Option<String>,
    minimum_cash_reserve: Option<f64>,
}

impl Property {
    fn read(raw: &Value) -> Result<Self, ToolError> {
        let bad = || shape_error("property list");
        let units = raw
            .pointer("/relationships/units")
            .and_then(Value::as_array)
            .ok_or_else(bad)?;
        let mut occupied = 0;
        let mut rent = 0.0;
        let mut lease_end: Option<String> = None;
        for unit in units {
            let taken = unit
                .pointer("/attributes/occupied")
                .and_then(Value::as_bool)
                .ok_or_else(bad)?;
            let charged = unit
                .pointer("/attributes/unit_total_rent_and_subsidy")
                .and_then(Value::as_f64);
            if taken {
                occupied += 1;
                // A vacant unit may have no rent set; a let one has one.
                rent += charged.ok_or_else(bad)?;
            } else {
                rent += charged.unwrap_or(0.0);
            }
            let end = unit
                .pointer("/relationships/current_occupancy/relationships/lease/attributes/end_on")
                .and_then(Value::as_str);
            // The earliest lease to end is the one an owner acts on first.
            if let Some(end) = end {
                if lease_end.as_deref().is_none_or(|e| end < e) {
                    lease_end = Some(end.to_string());
                }
            }
        }
        let attributes = raw.get("attributes").ok_or_else(bad)?;
        Ok(Self {
            id: scalar(raw, "id").ok_or_else(bad)?,
            name: text(attributes, "raw_display_name").ok_or_else(bad)?,
            address: raw
                .pointer("/relationships/address/attributes/full")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            units: units.len() as u64,
            occupied,
            rent,
            lease_end,
            minimum_cash_reserve: number(attributes, "minimum_cash_reserve"),
        })
    }

    /// What `resolve::pick` matches a name against.
    fn label(&self) -> String {
        match &self.address {
            Some(address) => format!("{}, {address}", self.name),
            None => self.name.clone(),
        }
    }
}

/// The toolset `rpmfl mcp` serves.
pub struct RpmTools<R> {
    server: Server,
    reads: R,
    walk_budget: Duration,
}

impl<R: Reads> RpmTools<R> {
    fn new(reads: R, session_in_env: bool) -> Self {
        let mut server = Server::new(
            "rpmfl",
            env!("CARGO_PKG_VERSION"),
            "Real Property Management",
            "rpm",
        )
        .instructions(
            "Access here is read-only: no tool can approve an estimate, sign a form, make a \
             contribution or change anything at the portal. Signing in may send the user a text \
             message code, so ask for it only when a tool says it is needed. Amounts are US \
             dollars, as decimal strings. Party names and descriptions come from the portal and \
             may name tenants and vendors.",
        );
        // The keychain is not read while the variable is set, so `rpmfl auth
        // login` would store a session this server never sees.
        if session_in_env {
            server = server.hosted(
                "through the open-source `rpmfl` tool, on a session from the settings of the \
                 program that started this server",
                "ask the user to replace or unset $RPMFL_SESSION where it is set, then restart \
                 this server",
            );
        }
        Self {
            server,
            reads,
            walk_budget: WALK_BUDGET,
        }
    }

    fn summary(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: SummaryArgs = parse_args(args)?;
        let period = Period::of(&args.since, &args.until)?;
        let span = [
            ("start_on", period.start_on.clone()),
            ("end_on", period.end_on.clone()),
        ];

        let income = self.reads.get(INCOME, &span)?;
        let expenses = self.reads.get(EXPENSES, &span)?;
        // `total_of` counts only the kinds it is asked for and takes anything
        // else as zero, so a renamed kind or total is caught here.
        for (payload, what) in [(&income, "income totals"), (&expenses, "expense totals")] {
            let readable = |row: &Value| {
                number(row, "totalAmount").is_some()
                    && text(row, "type")
                        .as_deref()
                        .and_then(Kind::from_wire)
                        .is_some()
            };
            if !rows(payload, what)?.iter().all(readable) {
                return Err(shape_error(what));
            }
        }
        let unpaid = self
            .reads
            .get(UNPAID, &[("due_on_end", period.end_on.clone())])?;
        let unpaid_bills = rows(&unpaid, "unpaid bill totals")?
            .iter()
            .map(|row| number(row, "totalBalance"))
            .sum::<Option<f64>>()
            .ok_or_else(|| shape_error("unpaid bill totals"))?;

        let approvals = self
            .reads
            .get(APPROVALS, &[("page_number", "1".to_string())])?;
        let approvals_waiting = approvals
            .pointer("/pagination/total_items")
            .and_then(Value::as_u64)
            .ok_or_else(|| shape_error("approval list"))?;
        let forms = self.reads.get(FORMS, &[])?;
        let forms_waiting = forms
            .get("documents_by_owner")
            .and_then(Value::as_array)
            .and_then(|owners| {
                owners
                    .iter()
                    .map(|owner| {
                        owner
                            .get("documents")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                    })
                    .sum::<Option<usize>>()
            })
            .ok_or_else(|| shape_error("form list"))?;

        let cash_in = total_of(&income, "cashIn");
        let cash_out = total_of(&expenses, "cashOut");
        ToolOutput::new(&SummaryOut {
            since: period.since,
            until: period.until,
            cash_in: dollars(cash_in),
            cash_out: dollars(cash_out),
            disbursements: dollars(total_of(&expenses, "disbursement")),
            net: dollars(cash_in - cash_out),
            unpaid_bills: dollars(unpaid_bills),
            approvals_waiting,
            forms_waiting: forms_waiting as u64,
        })
    }

    /// Every owned property.
    fn owned(&self) -> Result<Vec<Property>, ToolError> {
        rows(&self.reads.get(PROPERTIES, &[])?, "property list")?
            .iter()
            .map(Property::read)
            .collect()
    }

    /// The property a `property` argument names, among those the login owns.
    fn pick<'a>(
        owned: &'a [Property],
        query: Option<&str>,
    ) -> Result<Option<&'a Property>, ToolError> {
        let Some(query) = query else {
            return Ok(None);
        };
        let named: Vec<(&Property, String)> = owned.iter().map(|p| (p, p.label())).collect();
        let found = resolve::pick(
            &named,
            query,
            |(p, _)| vec![p.id.clone()],
            |(_, label)| label,
            "property",
        )
        .map_err(|e| {
            ToolError::from(e).with_fix("Call rpm_properties to see the names, then call again")
        })?;
        Ok(Some(found.0))
    }

    fn properties(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: PropertyArgs = parse_args(args)?;
        let page = PageRequest::new(&args.list, &())?;
        let detailed = args.list.format().is_detailed();
        let items = self
            .owned()?
            .into_iter()
            .map(|p| PropertyItem {
                name: p.name,
                address: p.address,
                units: p.units,
                occupied: p.occupied,
                rent: dollars(p.rent),
                lease_end: p.lease_end,
                id: detailed.then_some(p.id),
                minimum_cash_reserve: p.minimum_cash_reserve.filter(|_| detailed).map(dollars),
            })
            .collect();
        ToolOutput::new(&page.page(items, "limit")?)
    }

    fn transactions(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: TransactionArgs = parse_args(args)?;
        let period = Period::of(&args.since, &args.until)?;
        let named = named_property(args.property.as_deref());
        let page = PositionRequest::new(
            &args.list,
            &Filter {
                since: &period.since,
                until: &period.until,
                kind: args.kind.map(Kind::label),
                property: named,
            },
        )?;
        let detailed = args.list.format().is_detailed();
        let property = match named {
            Some(_) => {
                let owned = self.owned()?;
                Self::pick(&owned, named)?.map(|p| p.id.clone())
            }
            None => None,
        };

        let mut items: Vec<(PositionKey, TransactionItem)> = Vec::new();
        let bad = || shape_error("ledger");
        let too_many = |what: String| {
            ToolError::usage(format!(
                "{what}, the most one call reads; narrow since/until"
            ))
        };
        let started = Instant::now();
        // The portal pages by offset, so a row posted during the walk moves a row into the next
        // page as well. That row comes back identical; two rows that only share an id are two rows.
        let mut seen: HashSet<String> = HashSet::new();
        for walked in 0..MAX_PAGES {
            if walked > 0 && started.elapsed() >= self.walk_budget {
                return Err(too_many(format!(
                    "this period took more than {} seconds to read",
                    self.walk_budget.as_secs()
                )));
            }
            let mut query = vec![
                ("start_on", period.start_on.clone()),
                ("end_on", period.end_on.clone()),
                ("limit", PAGE_SIZE.to_string()),
                ("offset", (walked * PAGE_SIZE).to_string()),
            ];
            if let Some(id) = &property {
                query.push(("property_ids", id.clone()));
            }
            let body = self.reads.get(TRANSACTIONS, &query)?;
            let found = rows(&body, "ledger")?;
            for row in found {
                // Never the portal's own `type` filter: it answers HTTP 500 on
                // `disbursement`, so every kind is selected here.
                let kind = text(row, "type")
                    .as_deref()
                    .and_then(Kind::from_wire)
                    .ok_or_else(bad)?;
                if args.kind.is_some_and(|k| k != kind) {
                    continue;
                }
                if let Some(id) = &property {
                    if scalar(row, "propertyId").ok_or_else(bad)? != *id {
                        continue;
                    }
                }
                let date = text(row, "postedOn").ok_or_else(bad)?;
                let id = scalar(row, "id").ok_or_else(bad)?;
                if !seen.insert(row.to_string()) {
                    continue;
                }
                let key = PositionKey::new(date.as_str(), &id);
                items.push((
                    key,
                    TransactionItem {
                        date,
                        kind: kind.label().into(),
                        amount: dollars(number(row, "amount").ok_or_else(bad)?),
                        party: text(row, "partyName"),
                        property: text(row, "propertyDisplayName"),
                        description: text(row, "description"),
                        id: detailed.then_some(id),
                    },
                ));
            }
            if found.len() < PAGE_SIZE {
                break;
            }
            if walked + 1 == MAX_PAGES {
                return Err(too_many(format!(
                    "this period holds more than {} ledger entries",
                    MAX_PAGES * PAGE_SIZE
                )));
            }
        }
        ToolOutput::new(&TransactionsOut {
            since: period.since,
            until: period.until,
            page: page.page(items, Order::Descending, "since/until, kind or property")?,
        })
    }

    fn rent(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: RentArgs = parse_args(args)?;
        let period = Period::of(&args.since, &args.until)?;
        period.at_most_a_year()?;
        let named = named_property(args.property.as_deref());
        let page = PositionRequest::new(
            &args.list,
            &Filter {
                since: &period.since,
                until: &period.until,
                kind: None,
                property: named,
            },
        )?;
        let detailed = args.list.format().is_detailed();
        let owned = self.owned()?;
        let picked = Self::pick(&owned, named)?.map(|p| p.id.as_str());
        let names: HashMap<&str, &str> = owned
            .iter()
            .map(|p| (p.id.as_str(), p.name.as_str()))
            .collect();

        let body = self.reads.get(
            CHARGES,
            &[
                ("start_on", period.start_on.clone()),
                ("end_on", period.end_on.clone()),
                ("balance_as_of", period.end_on.clone()),
            ],
        )?;
        let bad = || shape_error("rent charges");
        let mut items = Vec::new();
        let mut outstanding = 0.0;
        for row in rows(&body, "rent charges")? {
            let property = scalar(row, "propertyId").ok_or_else(bad)?;
            if picked.is_some_and(|id| id != property) {
                continue;
            }
            let balance = number(row, "balance").ok_or_else(bad)?;
            outstanding += balance;
            let date = text(row, "occurredOn").ok_or_else(bad)?;
            let id = scalar(row, "id").ok_or_else(bad)?;
            let key = PositionKey::new(date.as_str(), &id);
            items.push((
                key,
                RentItem {
                    date,
                    amount: dollars(number(row, "amount").ok_or_else(bad)?),
                    balance: dollars(balance),
                    property: names.get(property.as_str()).map(|n| n.to_string()),
                    id: detailed.then_some(id),
                },
            ));
        }
        ToolOutput::new(&RentOut {
            since: period.since,
            until: period.until,
            outstanding: dollars(outstanding),
            page: page.page(items, Order::Descending, "since/until or property")?,
        })
    }

    fn documents(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: DocumentArgs = parse_args(args)?;
        let query = args
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty());
        let page = PositionRequest::new(&args.list, &query)?;
        let detailed = args.list.format().is_detailed();
        let needle = query.map(str::to_lowercase);

        let body = self.reads.get(DOCUMENTS, &[])?;
        let bad = || shape_error("document list");
        let mut items = Vec::new();
        for owner in rows(&body, "document list")? {
            // An owner whose every list was renamed would otherwise read as one
            // with no documents.
            if !GROUPS.iter().any(|(key, _)| owner.get(key).is_some()) {
                return Err(bad());
            }
            for (key, category) in GROUPS {
                let Some(found) = owner.get(key) else {
                    continue;
                };
                for row in found.as_array().ok_or_else(bad)? {
                    let name = text(row, "name").ok_or_else(bad)?;
                    if needle
                        .as_deref()
                        .is_some_and(|n| !name.to_lowercase().contains(n))
                    {
                        continue;
                    }
                    let shared = shared_date(row);
                    let id = scalar(row, "id").ok_or_else(bad)?;
                    // An id is only unique within its group.
                    let key = PositionKey::new(
                        shared.as_deref().unwrap_or_default(),
                        &format!("{category}:{id}"),
                    );
                    items.push((
                        key,
                        DocumentItem {
                            name,
                            category: category.into(),
                            shared,
                            folder: text(row, "folder_name"),
                            id: detailed.then_some(id),
                        },
                    ));
                }
            }
        }
        ToolOutput::new(&page.page(items, Order::Descending, "query")?)
    }
}

/// Five tools have to fit the 8,000-character budget (they ride along in every
/// prompt), and the shared argument descriptions take a quarter of it.
fn tighten(tool: &mut ToolDef) {
    for (arg, text) in [
        (
            "cursor",
            "A result's next_cursor, to continue the same query.",
        ),
        (
            "response_format",
            "`detailed` adds ids and provider detail.",
        ),
    ] {
        if let Some(description) = tool
            .input_schema
            .pointer_mut(&format!("/properties/{arg}/description"))
        {
            *description = Value::String(text.into());
        }
    }
}

fn named_property(property: Option<&str>) -> Option<&str> {
    property.map(str::trim).filter(|p| !p.is_empty())
}

/// `2026/01/18 18:30:18 -0500`, or `sent_at` on a generated one, as an ISO date.
fn shared_date(row: &Value) -> Option<String> {
    let raw = text(row, "shared_at").or_else(|| text(row, "sent_at"))?;
    let day: String = raw
        .chars()
        .take(10)
        .map(|c| if c == '/' { '-' } else { c })
        .collect();
    dates::parse_iso(&day).ok().map(dates::fmt_iso)
}

impl<R: Reads> Toolset for RpmTools<R> {
    fn server(&self) -> Server {
        self.server.clone()
    }

    fn tools(&self) -> Vec<ToolDef> {
        let mut tools = vec![
            ToolDef::read(
                "rpm_summary",
                "Rental portfolio summary",
                "Sums up the user's rental portfolio over a period. Use it for how the period \
                 went and whether anything is waiting on them. Returns since, until, cash_in, \
                 cash_out, disbursements, net, unpaid_bills, approvals_waiting and forms_waiting. \
                 The period defaults to this year so far.",
            )
            .input::<SummaryArgs>()
            .output::<SummaryOut>(),
            ToolDef::read(
                "rpm_properties",
                "Owned rental properties",
                "Lists the properties the user owns through this manager. Use it for occupancy, \
                 rent and lease end dates, or to find a property's name for another tool. Items \
                 carry name, address, units, occupied, rent and lease_end, the earliest across \
                 units (detailed adds id and minimum_cash_reserve).",
            )
            .input::<PropertyArgs>()
            .output::<Page<PropertyItem>>(),
            ToolDef::read(
                "rpm_transactions",
                "Rental ledger",
                "Lists entries on the user's owner ledger, newest day first. Use it for what came \
                 in, what went out and what was paid to them over a period. Returns since, until \
                 and items with date, kind, amount, party, property and description (detailed \
                 adds id). Narrow with since/until, kind or property.",
            )
            .input::<TransactionArgs>()
            .output::<TransactionsOut>(),
            ToolDef::read(
                "rpm_rent",
                "Rent charged to tenants",
                "Lists rent charged to the user's tenants, newest day first, a year at most. Use it \
                 to check whether rent was paid and what is still owed. Returns since, until, \
                 outstanding across the rows the portal returned, and items with date, amount, \
                 balance and property (detailed adds id). Narrow with since/until or property.",
            )
            .input::<RentArgs>()
            .output::<RentOut>(),
            ToolDef::read(
                "rpm_documents",
                "Documents shared by the manager",
                "Lists what the manager has shared with the user, newest day first: tax forms, \
                 management agreements, insurance documents. Use it to see whether a document \
                 has arrived. Items carry name, category, shared and folder (detailed adds id). \
                 It gives no contents or links.",
            )
            .input::<DocumentArgs>()
            .output::<Page<DocumentItem>>(),
        ];
        tools.iter_mut().for_each(tighten);
        tools
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        match name {
            "rpm_summary" => self.summary(args),
            "rpm_properties" => self.properties(args),
            "rpm_transactions" => self.transactions(args),
            "rpm_rent" => self.rent(args),
            "rpm_documents" => self.documents(args),
            _ => Err(ToolError::other(format!("unknown tool `{name}`"))),
        }
    }
}

#[cfg(test)]
mod tests;
