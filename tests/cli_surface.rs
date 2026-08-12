//! Offline black-box tests: the command surface, the error contract, and the
//! exit codes. Nothing here touches the network or the keychain.

use assert_cmd::Command;
use predicates::prelude::*;

fn rpmfl() -> Command {
    Command::cargo_bin("rpmfl").expect("binary builds")
}

/// Every top-level command, for the help-tree walk below.
const COMMANDS: &[&str] = &[
    "auth",
    "config",
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
    "self-update",
    "completions",
    "info",
];

#[test]
fn top_level_help_lists_the_surface() {
    let out = rpmfl().arg("--help").assert().success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    for cmd in COMMANDS {
        assert!(stdout.contains(cmd), "`{cmd}` missing from --help");
    }
}

/// Rendering a subcommand's help forces clap's debug assertions to run over
/// that subtree, which is what catches conflicting short flags (e.g. an
/// `api -q` colliding with the global `--quiet`).
#[test]
fn every_subcommand_help_renders() {
    for cmd in COMMANDS {
        rpmfl()
            .args([cmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::is_empty().not());
    }
}

#[test]
fn nested_subcommand_help_renders() {
    for (group, sub) in [
        ("auth", "login"),
        ("auth", "status"),
        ("config", "set"),
        ("properties", "list"),
        ("transactions", "list"),
        ("bills", "list"),
        ("documents", "get"),
    ] {
        rpmfl().args([group, sub, "--help"]).assert().success();
    }
}

/// `auth status --verify` must exist and be documented, since the plain form
/// answers from local state alone.
#[test]
fn auth_status_offers_a_verify_flag() {
    rpmfl()
        .args(["auth", "status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--verify"));
}

/// `session_valid` asserts a checked fact, so the DTO must omit it unless
/// `--verify` actually made the call (SPEC §1.4: omit, don't null).
#[test]
fn auth_status_omits_session_valid_when_unverified() {
    let out = rpmfl().args(["--json", "auth", "status"]).output().unwrap();
    // Skip where no keychain backend is available to answer at all.
    if !out.status.success() {
        return;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status emits JSON");
    assert_eq!(v["schema"], "auth-status/v1");
    assert!(
        v.get("session_valid").is_none(),
        "session_valid must be absent without --verify, got {:?}",
        v.get("session_valid")
    );
}

#[test]
fn version_prints_the_crate_version() {
    rpmfl()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn info_emits_cli_info_dto() {
    let out = rpmfl().arg("info").assert().success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("info emits JSON");
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["spec"], "piekstra-cli/1");
    assert_eq!(v["name"], "rpmfl");
    assert_eq!(v["auth"]["required"], true);
    assert!(v["capabilities"]
        .as_array()
        .unwrap()
        .contains(&"summary".into()));
}

#[test]
fn unknown_command_is_a_usage_error() {
    rpmfl().arg("nonsense").assert().code(2);
}

#[test]
fn bad_date_is_usage_error_exit_2() {
    rpmfl()
        .args(["summary", "--since", "01/31/2026"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("ISO date"));
}

/// `--since`/`--until` are the family spelling; `--start`/`--end` read better
/// for a reporting period and are kept as aliases. Both must reach the same
/// validation.
#[test]
fn range_flags_accept_both_spellings() {
    for (from, to) in [("--since", "--until"), ("--start", "--end")] {
        rpmfl()
            .args(["summary", from, "2026-06-01", to, "2026-01-01"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("is after"));
    }
}

#[test]
fn json_errors_use_the_family_error_dto() {
    let out = rpmfl()
        .args(["--json", "summary", "--since", "bogus"])
        .assert()
        .code(2);
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("errors emit JSON in --json mode");
    assert_eq!(v["error"]["code"], "usage");
    assert!(v["error"]["message"].as_str().unwrap().contains("ISO date"));
}

#[test]
fn malformed_api_query_is_a_usage_error() {
    rpmfl()
        .args(["api", "/oportal/api/x", "--query", "no-equals-sign"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("KEY=VALUE"));
}

#[test]
fn completions_generate_for_each_shell() {
    for shell in ["bash", "zsh", "fish"] {
        rpmfl()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("rpmfl"));
    }
}
