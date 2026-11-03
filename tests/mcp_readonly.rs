//! Every tool through the real binary against a stub portal on 127.0.0.1, so what
//! goes over the wire is what is checked: GETs to the read endpoints, and never a
//! password, a two-factor request or a sign-in. Offline and keychain-free: the
//! session comes from `$RPMFL_SESSION`, the host from `$RPMFL_BASE_URL`.
//!
//! The session cookie is marked `Secure`, so it is not sent to this stub's plain
//! HTTP: nothing here sees it, and the rotation logic is tested in `src/mcp/tests.rs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

/// Everything a tool may read, by method and path. A new read must be added here
/// on purpose, and nothing but a GET can be.
const READS: [&str; 9] = [
    "GET /oportal/api/owner_properties",
    "GET /oportal/api/owner_transactions",
    "GET /oportal/api/owner_income_balances",
    "GET /oportal/api/owner_expenses_balances",
    "GET /oportal/api/unpaid_bills_balances",
    "GET /oportal/api/owner_decision_requests",
    "GET /oportal/api/pdf_forms/actionable_documents",
    "GET /oportal/api/tenant_charges",
    "GET /oportal/api/shared_documents/",
];

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// What the portal answers: each path's fixture, with no ledger page after the
/// first.
fn answer(path: &str, query: &str) -> (u16, String) {
    if query
        .split('&')
        .any(|q| q.starts_with("offset=") && q != "offset=0")
    {
        return (200, "[]".into());
    }
    match path {
        "/oportal/api/owner_properties" => (200, fixture("owner_properties.json")),
        "/oportal/api/owner_transactions" => (200, fixture("owner_transactions.json")),
        "/oportal/api/owner_income_balances" => (200, fixture("owner_income_balances.json")),
        "/oportal/api/owner_expenses_balances" => (200, fixture("owner_expenses_balances.json")),
        "/oportal/api/unpaid_bills_balances" => (200, fixture("unpaid_bills_balances.json")),
        "/oportal/api/owner_decision_requests" => (200, fixture("owner_decision_requests.json")),
        "/oportal/api/pdf_forms/actionable_documents" => {
            (200, fixture("pdf_forms_actionable_documents.json"))
        }
        "/oportal/api/tenant_charges" => (200, fixture("tenant_charges.json")),
        "/oportal/api/shared_documents/" => (200, fixture("shared_documents.json")),
        _ => (404, "{}".into()),
    }
}

#[derive(Clone, Default)]
struct Stub {
    /// `METHOD /path` and the body length of every request seen.
    seen: Arc<Mutex<Vec<(String, usize)>>>,
}

impl Stub {
    /// Serve on a free port, answering each request with `answer`, until the
    /// process ends.
    fn start(answer: fn(&str, &str) -> (u16, String)) -> (Self, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stub = Stub::default();
        let seen = stub.seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                handle(stream, &seen, answer);
            }
        });
        (stub, port)
    }

    fn seen(&self) -> Vec<(String, usize)> {
        self.seen.lock().unwrap().clone()
    }
}

fn handle(
    mut stream: TcpStream,
    seen: &Mutex<Vec<(String, usize)>>,
    answer: fn(&str, &str) -> (u16, String),
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
            break;
        }
        let lower = line.to_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    seen.lock()
        .unwrap()
        .push((format!("{method} {path}"), length));
    let (status, text) = answer(path, query);
    write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{text}",
        text.len()
    )
    .unwrap();
}

static DIRS: AtomicU32 = AtomicU32::new(0);

/// An empty, per-test config home: the server must not need a config to start.
fn empty_config_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rpmfl-mcp-readonly-{}-{}",
        std::process::id(),
        DIRS.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn start(port: u16) -> Self {
        Self::start_at(format!("http://127.0.0.1:{port}"))
    }

    fn start_at(base: String) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rpmfl"))
            .arg("mcp")
            .env("XDG_CONFIG_HOME", empty_config_home())
            .env("RPMFL_BASE_URL", base)
            .env("RPMFL_SESSION", "dummy-session")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("rpmfl starts");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            stdin,
            lines,
            next_id: 1,
        };
        server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }),
        );
        server
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(30))
                .expect("a response within 30 s");
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == json!(id) {
                return msg;
            }
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn every_tool_call() -> Vec<(&'static str, Value)> {
    vec![
        ("rpm_summary", json!({})),
        ("rpm_properties", json!({})),
        ("rpm_transactions", json!({"since": "2026-01-01"})),
        ("rpm_transactions", json!({"property": "Sample St 100"})),
        ("rpm_rent", json!({})),
        ("rpm_documents", json!({})),
    ]
}

#[test]
fn every_tool_answers_with_gets_to_the_read_endpoints_alone() {
    let (stub, port) = Stub::start(answer);
    let mut server = Server::start(port);

    let listed = server.request("tools/list", json!({}));
    let mut called: Vec<String> = Vec::new();
    for tool in listed["result"]["tools"].as_array().unwrap() {
        called.push(tool["name"].as_str().unwrap().to_string());
    }
    for (name, _) in every_tool_call() {
        called.retain(|c| c != name);
    }
    assert!(called.is_empty(), "no call below reaches {called:?}");

    for (name, args) in every_tool_call() {
        let result = server.call(name, args.clone());
        assert_eq!(result["isError"], false, "{name} {args}: {result}");
    }

    let seen = stub.seen();
    assert!(!seen.is_empty());
    for (request, body) in &seen {
        assert!(
            READS.contains(&request.as_str()),
            "a tool sent {request}, which is not one of the reads"
        );
        assert_eq!(*body, 0, "{request} carried a body");
    }
    for read in READS {
        assert!(
            seen.iter().any(|(request, _)| request == read),
            "nothing reached {read}"
        );
    }
}

/// A lapsed session is reported, and the server does not sign in again: no POST,
/// so no password and no text message, however many tools are called.
#[test]
fn a_rejected_session_is_an_auth_error_and_nothing_is_sent_but_the_read() {
    let (stub, port) = Stub::start(|_, _| (401, "{}".into()));
    let mut server = Server::start(port);
    for (name, args) in every_tool_call() {
        let result = server.call(name, args);
        assert_eq!(result["isError"], true, "{name}: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("auth:"), "{name}: {text}");
    }
    for (request, _) in stub.seen() {
        assert!(request.starts_with("GET /oportal/api/"), "{request}");
    }
}

/// The host comes from the environment or the config, and the session goes to it.
#[test]
fn a_tool_refuses_a_portal_host_off_https() {
    let mut server = Server::start_at("http://portal.example".into());
    let result = server.call("rpm_summary", json!({}));
    assert_eq!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("only https"), "{text}");
}

/// Expiry is rarely a 401: Rails answers an XHR with HTML, the login page.
#[test]
fn a_login_page_where_json_was_expected_is_an_auth_error() {
    let (_stub, port) = Stub::start(|_, _| (200, "<html>Sign in</html>".into()));
    let mut server = Server::start(port);
    let result = server.call("rpm_properties", json!({}));
    assert_eq!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("auth:"), "{text}");
}

/// A failure of the portal's own is the portal's, not a sign-in.
#[test]
fn a_server_error_is_an_upstream_error() {
    let (_stub, port) = Stub::start(|_, _| (503, "{}".into()));
    let mut server = Server::start(port);
    let result = server.call("rpm_properties", json!({}));
    assert_eq!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("upstream:"), "{text}");
}
