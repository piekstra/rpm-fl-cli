//! `rpmfl` — piekstra-family CLI for the Real Property Management (AppFolio)
//! owner portal.
//!
//! Conforms to piekstra-cli/1. Read-only today: every command observes, none
//! mutate. Writes (contributions, approvals) are deliberately out of scope.

mod client;
mod commands;
mod config;
mod dates;
mod diag;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::{AuthStatus, LoginArgs, LogoutArgs, SetCredentialArgs};
use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{output, CliError, CommonArgs};
use pk_cli_secrets::CredentialStore;
use pk_cli_selfupdate::{SelfUpdateArgs, Updater};

use client::{CodeChannel, LoginOutcome, Portal};
use commands::{api, bills, documents, misc, properties, summary, transactions, Ctx};
use config::{
    Config, DEVICE_TOKEN_ACCOUNT, KEYCHAIN_ACCOUNT, PENDING_SESSION_ACCOUNT, SESSION_ACCOUNT,
};
use dates::RangeArgs;

const BIN: &str = "rpmfl";
const REPO: &str = "piekstra/rpm-fl-cli";

/// Real Property Management owner portal from the command line — properties,
/// ledger, documents, and statements. Unofficial.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,

    /// Per-request timeout in seconds (default 45; `config set timeout_secs`
    /// makes a different budget stick).
    #[arg(
        long,
        global = true,
        value_name = "SECS",
        env = "RPMFL_TIMEOUT",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    timeout: Option<u64>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Portal login, session status, and credential management.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Non-secret settings.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Portfolio overview for a period: cash in/out, disbursements, unpaid bills.
    Summary(RangeArgs),
    /// Owned properties: addresses, units, leases, occupancy.
    #[command(subcommand)]
    Properties(properties::Cmd),
    /// Ownership records: who owns what, and in what share.
    Ownerships,
    /// Owner ledger: cash in, cash out, disbursements.
    #[command(subcommand)]
    Transactions(transactions::Cmd),
    /// Rent charged to tenants, with outstanding balances.
    Charges(RangeArgs),
    /// Unpaid bills charged against the portfolio.
    #[command(subcommand)]
    Bills(bills::Cmd),
    /// Shared documents: 1099s, W-9s, management agreements.
    #[command(subcommand)]
    Documents(documents::Cmd),
    /// Published owner statement packets.
    Statements {
        /// Maximum packets to return.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Estimates and other decisions awaiting your approval.
    Approvals {
        /// Page number (1-based).
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// Documents waiting on your signature.
    Forms,
    /// Raw portal API passthrough.
    Api(api::ApiArgs),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Log in to the portal, clearing two-factor if the device isn't trusted.
    Login(PortalLoginArgs),
    /// Report credential and session state (auth-status/v1).
    Status(StatusArgs),
    /// Clear the cached session; --forget also removes the stored password.
    Logout(LogoutArgs),
    /// Raw keychain write for rotation / headless setup.
    SetCredential(SetCredentialArgs),
}

#[derive(clap::Args, Debug)]
struct StatusArgs {
    /// Prove the stored session still works by making one cheap portal read.
    ///
    /// Without this the answer is local-only: it reports whether a session is
    /// *stored*, which stays "yes" long after the portal has expired it.
    #[arg(long)]
    verify: bool,
}

#[derive(clap::Args, Debug)]
struct PortalLoginArgs {
    #[command(flatten)]
    base: LoginArgs,

    /// How to receive the two-factor code.
    #[arg(long, value_enum, default_value = "sms")]
    channel: CodeChannel,

    /// Two-factor code, when you already have one (skips the prompt).
    #[arg(long, value_name = "CODE")]
    code: Option<String>,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the resolved config file path.
    Path,
    /// Show the effective configuration.
    Show,
    /// Set a config key (base_url, username, timeout_secs).
    Set { key: String, value: String },
    /// Remove a config key.
    Unset { key: String },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, cli.common.json));
    }
}

fn run(cli: &Cli) -> Result<(), CliError> {
    let store = ConfigStore::new(BIN);
    let creds = CredentialStore::for_binary(BIN);
    let mut cfg: Config = store.load()?;
    if let Some(secs) = cli.timeout {
        cfg.timeout_secs = Some(secs);
    }
    let ctx = Ctx {
        common: &cli.common,
        cfg: &cfg,
        creds: &creds,
    };

    match &cli.command {
        Command::Auth(cmd) => auth(cli, cmd, &store, &creds, &cfg),
        Command::Config(cmd) => config_cmd(cli, cmd, &store),
        Command::Summary(range) => summary::run(&ctx, range),
        Command::Properties(cmd) => properties::run(&ctx, cmd),
        Command::Ownerships => misc::ownerships(&ctx),
        Command::Transactions(cmd) => transactions::run(&ctx, cmd),
        Command::Charges(range) => misc::charges(&ctx, range),
        Command::Bills(cmd) => bills::run(&ctx, cmd),
        Command::Documents(cmd) => documents::run(&ctx, cmd),
        Command::Statements { limit } => misc::statements(&ctx, *limit),
        Command::Approvals { page } => misc::approvals(&ctx, *page),
        Command::Forms => misc::forms(&ctx),
        Command::Api(args) => api::run(&ctx, args),
        Command::SelfUpdate(args) => Updater {
            repo: REPO.into(),
            binary: BIN.into(),
            target: env!("BUILD_TARGET").into(),
            current: env!("CARGO_PKG_VERSION").into(),
        }
        .run(args, cli.common.json, cli.common.quiet),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            Ok(())
        }
        Command::Info => {
            let info = CliInfo::new(
                BIN,
                env!("CARGO_PKG_VERSION"),
                &format!("https://github.com/{REPO}"),
                AuthInfo {
                    required: true,
                    method: "password".into(),
                    login_hint: Some(format!("{BIN} auth login")),
                },
                &[
                    "summary",
                    "properties",
                    "ownerships",
                    "transactions",
                    "charges",
                    "bills",
                    "documents",
                    "statements",
                    "approvals",
                    "forms",
                    "api",
                ],
            )
            .with_profiles(&[pk_cli_documents::PROFILE]);
            output::json(&serde_json::to_value(&info).unwrap());
            Ok(())
        }
    }
}

fn auth(
    cli: &Cli,
    cmd: &AuthCmd,
    store: &ConfigStore,
    creds: &CredentialStore,
    cfg: &Config,
) -> Result<(), CliError> {
    match cmd {
        AuthCmd::Login(args) => login(cli, args, creds, cfg),
        AuthCmd::Status(args) => {
            // Even these local reads can block for minutes: macOS parks the
            // process inside `securityd` while a keychain permission dialog
            // waits for a click. Name that wait instead of sitting silent.
            let (has_password, has_session) =
                diag::keychain(cli.common.quiet, || -> Result<_, CliError> {
                    Ok((
                        creds.get(KEYCHAIN_ACCOUNT)?.is_some(),
                        creds.get(SESSION_ACCOUNT)?.is_some(),
                    ))
                })?;

            // A stored session is necessary but not sufficient: the portal
            // expires sessions server-side, and nothing local reflects that.
            // Reporting "authenticated" from mere presence means this command
            // answers "yes" while every read returns 401 — so --verify spends
            // one cheap read to give a truthful answer.
            // `session_valid` stays `None` unless we actually checked, so the
            // DTO omits it rather than asserting something unverified (SPEC
            // §1.4: omit, don't null).
            let (authenticated, session_valid, note) = if !args.verify || !has_session {
                (has_session, None, None)
            } else {
                match diag::keychain(cli.common.quiet, || Portal::from_cached_session(cfg, creds))
                    .map(|p| p.with_diagnostics(cli.common.verbose, cli.common.quiet))
                    .and_then(|p| p.get("/oportal/api/owner_ownerships", &[]))
                {
                    Ok(_) => (
                        true,
                        Some(true),
                        Some("session verified against the portal"),
                    ),
                    Err(CliError::Auth(_)) => (
                        false,
                        Some(false),
                        Some("stored session rejected — run `rpmfl auth login`"),
                    ),
                    // A network failure says nothing about the credential;
                    // don't claim the session is dead on its account.
                    Err(e) => return Err(e),
                }
            };

            let mut status =
                AuthStatus::new(true, authenticated, pk_cli_auth::AuthMethod::Password);
            status.username = cfg.username();
            status.credential_in_keychain = Some(has_password);
            status.session_valid = session_valid;
            status.authenticated = authenticated;
            status.emit(cli.common.json);
            if let Some(note) = note {
                if !cli.common.quiet {
                    eprintln!("{note}");
                }
            }
            // Exit 3 when asked to verify and the session is dead, so scripts
            // can gate on `rpmfl auth status --verify` succeeding.
            //
            // Exiting here rather than returning an error is deliberate: the
            // status DTO has already gone to stdout, and returning would make
            // `output::fail` print a second JSON document after it, leaving
            // `--json` output unparseable.
            if args.verify && !authenticated {
                std::process::exit(CliError::Auth(String::new()).exit_code());
            }
            Ok(())
        }
        AuthCmd::Logout(args) => {
            creds.delete(SESSION_ACCOUNT)?;
            // Drop any half-finished login too, so `logout` really does leave
            // nothing session-shaped behind.
            creds.delete(PENDING_SESSION_ACCOUNT)?;
            if args.forget {
                creds.delete(KEYCHAIN_ACCOUNT)?;
                creds.delete(DEVICE_TOKEN_ACCOUNT)?;
                store.clear()?;
                if !cli.common.quiet {
                    eprintln!("session cleared; password and device token removed");
                }
            } else if !cli.common.quiet {
                eprintln!("session cleared (password kept; use --forget to remove it)");
            }
            Ok(())
        }
        AuthCmd::SetCredential(args) => {
            if creds.get(KEYCHAIN_ACCOUNT)?.is_some() && !args.overwrite {
                return Err(CliError::Usage(
                    "a password is already stored; pass --overwrite to replace it".into(),
                ));
            }
            let secret = args.source.read(None)?;
            creds.set(KEYCHAIN_ACCOUNT, &secret)?;
            if !cli.common.quiet {
                eprintln!("password stored in the OS keychain ({})", creds.service());
            }
            Ok(())
        }
    }
}

/// Full portal login: password, then the two-factor dance when the portal
/// doesn't recognize this client. The resulting session cookie is what every
/// later command rides on.
fn login(
    cli: &Cli,
    args: &PortalLoginArgs,
    creds: &CredentialStore,
    cfg: &Config,
) -> Result<(), CliError> {
    let email = cfg.username().ok_or_else(|| {
        CliError::Usage(
            "no portal email configured — run `rpmfl config set username <you@example.com>`".into(),
        )
    })?;

    // A code supplied on the command line belongs to the login that requested
    // it, so resume that parked session rather than starting a fresh one.
    if let (Some(code), Some(pending)) = (&args.code, creds.get(PENDING_SESSION_ACCOUNT)?) {
        let portal = Portal::new(cfg.base_url()?, cfg.timeout())?;
        portal.seed_session(pending.expose());
        match portal.verify_code(code.trim()) {
            Ok(()) => {
                creds.delete(PENDING_SESSION_ACCOUNT)?;
                if !cli.common.quiet {
                    eprintln!("two-factor verified");
                }
                return finish_login(cli, &portal, creds);
            }
            Err(e) => {
                // The parked session is spent either way; drop it so the next
                // attempt starts clean instead of replaying a dead one.
                creds.delete(PENDING_SESSION_ACCOUNT)?;
                return Err(e);
            }
        }
    }

    // Take the password from the keychain, falling back to the standard
    // ingestion flags so a first run can supply it inline.
    let password = match creds.get(KEYCHAIN_ACCOUNT)? {
        Some(p) if !args.base.overwrite => p,
        _ => {
            let prompt = if args.base.non_interactive {
                None
            } else {
                Some("Portal password")
            };
            let secret = args.base.source.read(prompt)?;
            creds.set(KEYCHAIN_ACCOUNT, &secret)?;
            secret
        }
    };

    let portal = Portal::new(cfg.base_url()?, cfg.timeout())?;
    if let Some(token) = creds.get(DEVICE_TOKEN_ACCOUNT)? {
        // Replayed on the chance this account skips fingerprint binding.
        portal.seed_device_token(token.expose());
    }

    match portal.login(&email, &password)? {
        LoginOutcome::Authenticated => {
            if !cli.common.quiet {
                eprintln!("logged in (device recognized — no code needed)");
            }
        }
        LoginOutcome::TwoFactorRequired => {
            let code = match &args.code {
                Some(c) => c.clone(),
                None => {
                    let dest = portal.send_code(args.channel)?;
                    let where_to = dest
                        .map(|d| format!(" to the number ending in {d}"))
                        .unwrap_or_default();
                    if !cli.common.interactive() {
                        // No TTY to prompt on (a script, CI, or an editor's
                        // shell). Park the session that owns this code so the
                        // follow-up `--code` run resumes it instead of asking
                        // the portal to honor a code it never issued.
                        if let Some(s) = portal.session_cookie() {
                            creds.set(PENDING_SESSION_ACCOUNT, &s)?;
                        }
                        return Err(CliError::Auth(format!(
                            "a verification code was sent{where_to} — \
                             re-run with `rpmfl auth login --code <CODE>`"
                        )));
                    }
                    eprintln!("A verification code was sent{where_to}.");
                    prompt_line("Verification code: ")?
                }
            };
            portal.verify_code(code.trim())?;
            if !cli.common.quiet {
                eprintln!("two-factor verified");
            }
        }
    }

    finish_login(cli, &portal, creds)
}

/// Prove a freshly authenticated session works, then cache it.
///
/// The read happens *before* the write so a login that authenticated but
/// can't fetch anything leaves no broken session behind — and so the cookie
/// we store is the one that survived that read, since the portal may rotate
/// it mid-flight.
fn finish_login(cli: &Cli, portal: &Portal, creds: &CredentialStore) -> Result<(), CliError> {
    portal.get("/oportal/api/owner_ownerships", &[])?;

    let session = portal.session_cookie().ok_or_else(|| {
        CliError::Upstream("login succeeded but the portal issued no session cookie".into())
    })?;
    creds.set(SESSION_ACCOUNT, &session)?;
    if let Some(token) = portal.device_token() {
        creds.set(DEVICE_TOKEN_ACCOUNT, &token)?;
    }
    if !cli.common.quiet {
        eprintln!("session cached in the OS keychain ({})", creds.service());
    }
    Ok(())
}

/// Read one line from stdin (for the 2FA code, which isn't a secret worth
/// hiding — it's single-use and expires in minutes).
fn prompt_line(label: &str) -> Result<String, CliError> {
    use std::io::{BufRead, Write};
    eprint!("{label}");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| CliError::Other(format!("reading input: {e}")))?;
    Ok(line.trim().to_string())
}

fn config_cmd(cli: &Cli, cmd: &ConfigCmd, store: &ConfigStore) -> Result<(), CliError> {
    match cmd {
        ConfigCmd::Path => {
            println!("{}", store.path()?.display());
            Ok(())
        }
        ConfigCmd::Show => {
            let cfg: Config = store.load()?;
            let v = serde_json::to_value(&cfg).unwrap_or_default();
            if cli.common.json {
                output::json(&v);
            } else {
                output::render(&v);
            }
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg: Config = store.load()?;
            match key.as_str() {
                "base_url" => cfg.base_url = Some(value.clone()),
                "username" => cfg.username = Some(value.clone()),
                "timeout_secs" => {
                    let secs: u64 = value.parse().map_err(|_| {
                        CliError::Usage(format!(
                            "timeout_secs must be a positive integer, got {value:?}"
                        ))
                    })?;
                    if secs == 0 {
                        return Err(CliError::Usage("timeout_secs must be at least 1".into()));
                    }
                    cfg.timeout_secs = Some(secs);
                }
                other => return Err(unknown_key(other)),
            }
            store.save(&cfg)
        }
        ConfigCmd::Unset { key } => {
            let mut cfg: Config = store.load()?;
            match key.as_str() {
                "base_url" => cfg.base_url = None,
                "username" => cfg.username = None,
                "timeout_secs" => cfg.timeout_secs = None,
                other => return Err(unknown_key(other)),
            }
            store.save(&cfg)
        }
    }
}

fn unknown_key(key: &str) -> CliError {
    CliError::Usage(format!(
        "unknown config key `{key}` (known: {})",
        config::KNOWN_KEYS.join(", ")
    ))
}
