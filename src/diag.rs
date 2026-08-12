//! Stall diagnostics: name *where* the time is going, so a wait never reads
//! as a hang.
//!
//! The "2–6 minute commands" of issue #3 turned out not to be the portal at
//! all — reads answer in well under a second. Commands were blocked inside
//! the OS keychain (`SecKeychainFindGenericPassword` waiting on `securityd`),
//! which happens whenever macOS puts up a permission dialog: the keychain
//! scopes an "Always Allow" grant to the binary's code signature, and every
//! ad-hoc-signed reinstall minted a new identity, silently revoking the
//! grant. Nothing was printed during the wait, so the CLI looked hung and
//! wrapper timeouts fired with no clue why.
//!
//! The helpers here put a line on stderr the moment a blocking call turns
//! long, saying what is being waited on and what would unstick it. They never
//! shorten the wait — that's the per-request timeout's job — they only make
//! it legible.

use std::sync::mpsc;
use std::time::Duration;

/// What to tell the operator when a keychain read stalls: the wait is a
/// permission dialog, and "Always Allow" is the durable fix.
pub const KEYCHAIN_NOTE: &str = "waiting on the OS keychain — if macOS is showing a permission \
     dialog, choose \"Always Allow\" so future runs skip it";

/// Keychain reads answer in milliseconds when the grant is in place; anything
/// longer means `securityd` is waiting on something (usually a dialog).
pub const KEYCHAIN_STALL: Duration = Duration::from_secs(4);

/// Portal reads usually answer in under a second. The per-request timeout
/// bounds the wait; this only makes sure something is said before it trips.
pub const PORTAL_STALL: Duration = Duration::from_secs(10);

/// Follow-up cadence once a wait has been called out.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// Wrap one keychain interaction (or a contiguous run of them) in the stall
/// watchdog. Every `CredentialStore` touch should go through this — or
/// through `Ctx::client`, which applies it around session replay — so no
/// keychain wait is ever silent.
pub fn keychain<T>(quiet: bool, op: impl FnOnce() -> T) -> T {
    with_stall_note(quiet, KEYCHAIN_STALL, KEYCHAIN_NOTE, op)
}

/// Run `op`; if it hasn't returned after `after`, print `note` to stderr with
/// an elapsed count, repeating every [`HEARTBEAT`] until it does. A fast `op`
/// (or `quiet`) prints nothing.
pub fn with_stall_note<T>(quiet: bool, after: Duration, note: &str, op: impl FnOnce() -> T) -> T {
    if quiet {
        return op();
    }
    // The sender's only job is to be dropped: `recv_timeout` then returns
    // `Disconnected`, which is how the watchdog learns the call finished.
    let (tx, rx) = mpsc::channel::<()>();
    let note = note.to_string();
    let watchdog =
        std::thread::spawn(move || watch(&rx, after, HEARTBEAT, &note, &mut |l| eprintln!("{l}")));
    let out = op();
    drop(tx);
    let _ = watchdog.join();
    out
}

/// The timing core, separated from stderr so tests can drive it with
/// millisecond budgets and inspect what it would have printed. Returns once
/// the channel disconnects (the watched call finished).
fn watch(
    rx: &mpsc::Receiver<()>,
    after: Duration,
    every: Duration,
    note: &str,
    emit: &mut dyn FnMut(String),
) {
    if !matches!(rx.recv_timeout(after), Err(mpsc::RecvTimeoutError::Timeout)) {
        return; // finished before the threshold — stay silent
    }
    let mut elapsed = after;
    loop {
        emit(format!("rpmfl: {note} ({}s elapsed)", elapsed.as_secs()));
        if !matches!(rx.recv_timeout(every), Err(mpsc::RecvTimeoutError::Timeout)) {
            return;
        }
        elapsed += every;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fast_call_prints_nothing() {
        let (tx, rx) = mpsc::channel::<()>();
        drop(tx); // the call "finished" immediately
        let mut lines = Vec::new();
        watch(
            &rx,
            Duration::from_millis(20),
            Duration::from_millis(20),
            "n/a",
            &mut |l| lines.push(l),
        );
        assert!(
            lines.is_empty(),
            "silent when under the threshold: {lines:?}"
        );
    }

    #[test]
    fn a_stalled_call_gets_a_note_and_heartbeats() {
        let (tx, rx) = mpsc::channel::<()>();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            drop(tx);
        });
        let mut lines = Vec::new();
        watch(
            &rx,
            Duration::from_millis(20),
            Duration::from_millis(30),
            "waiting on the OS keychain",
            &mut |l| lines.push(l),
        );
        release.join().unwrap();
        assert!(
            lines.len() >= 2,
            "expected the note plus at least one heartbeat, got {lines:?}"
        );
        assert!(lines[0].contains("waiting on the OS keychain"));
        assert!(lines[0].contains("elapsed"));
    }

    #[test]
    fn with_stall_note_returns_the_ops_value() {
        let out = with_stall_note(false, Duration::from_secs(60), "n/a", || 7);
        assert_eq!(out, 7);
        // Quiet mode must not spawn or wait on anything either.
        let out = with_stall_note(true, Duration::from_secs(60), "n/a", || "ok");
        assert_eq!(out, "ok");
    }
}
