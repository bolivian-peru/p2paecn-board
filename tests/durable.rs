//! Durable state in one file (`P2PAECN_STATE`), through the real binary: what was accepted before a restart is
//! there after it, byte for byte; nothing signed before the restart is accepted after it; a damaged or open file
//! stops startup; a change that could not be saved is not acknowledged. Every example finding is invented.
//! Each test runs its own board on a port the operating system picks.

mod common;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use p2paecn_board::sig;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const HOST: &str = "board.test";
static NONCE: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// A fresh directory of mode 0700 under the target directory, short enough for a socket path.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("d-{}-{name}", std::process::id()));
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

// ---------------------------------------------------------------- the server process

struct Server {
    child: Child,
    addr: SocketAddr,
    socket: PathBuf,
    started: String,
    log: mpsc::Receiver<String>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command(dir: &Path, env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_p2paecn-board"));
    cmd.current_dir(dir)
        .env("P2PAECN_HOST", HOST)
        .env("P2PAECN_BIND", "127.0.0.1:0")
        .env("P2PAECN_ADMIN_SOCKET", dir.join("op.sock"))
        .env("P2PAECN_STATE", dir.join("state.json"))
        .env_remove("P2PAECN_CONFIG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (name, value) in env {
        if value.is_empty() {
            cmd.env_remove(name);
        } else {
            cmd.env(name, value);
        }
    }
    cmd
}

/// Starts the board and waits for its startup line, then for the next second: a request signed in the second the
/// board started counts as signed before it.
fn start(dir: &Path, env: &[(&str, &str)]) -> Server {
    let mut child = command(dir, env).spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (lines, log) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = lines.send(line);
        }
    });
    let started = log.recv_timeout(Duration::from_secs(20)).expect("a startup line");
    let addr = started
        .split("listening on ")
        .nth(1)
        .and_then(|r| r.split(',').next())
        .and_then(|a| a.parse().ok())
        .unwrap_or_else(|| panic!("no address in: {started}"));
    let seen = now();
    while now() <= seen {
        std::thread::sleep(Duration::from_millis(50));
    }
    Server { child, addr, socket: dir.join("op.sock"), started, log }
}

/// Runs the board expecting it to refuse to start. Returns its exit status and everything it wrote.
fn refuses_to_start(dir: &Path, env: &[(&str, &str)]) -> (bool, String) {
    let mut child = command(dir, env).spawn().unwrap();
    for _ in 0..100 {
        if let Some(status) = child.try_wait().unwrap() {
            let mut err = String::new();
            child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
            return (status.success(), err);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    panic!("the board started instead of refusing: {err}");
}

// ---------------------------------------------------------------- requests

/// Status and body bytes of one HTTP/1.1 exchange on a fresh connection.
fn exchange(stream: &mut dyn ReadWrite, request: &[u8]) -> (u16, Vec<u8>) {
    stream.write_all(request).unwrap();
    let mut got = Vec::new();
    stream.read_to_end(&mut got).unwrap();
    let split = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(&got)));
    let head = String::from_utf8_lossy(&got[..split]).into_owned();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, got[split + 4..].to_vec())
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

struct Agent(String, SigningKey);

fn agent(id: &str, seed: u8) -> Agent {
    Agent(id.to_owned(), SigningKey::from_bytes(&[seed; 32]))
}

fn public_key(key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes())
}

/// The bytes of a request signed by `key_id` at `created`.
fn signed_at(key_id: &str, key: &SigningKey, method: &str, path: &str, body: &str, idem: Option<&str>, created: u64) -> Vec<u8> {
    let created = created.to_string();
    let nonce = format!("durable-nonce-{:013}-{}", NONCE.fetch_add(1, Ordering::SeqCst), std::process::id());
    let digest = sig::content_digest(body.as_bytes());
    let canonical = sig::canonical_string(method, HOST, path, &created, &nonce, key_id, &digest, idem.unwrap_or(""));
    let signature = URL_SAFE_NO_PAD.encode(key.sign(canonical.as_bytes()).to_bytes());
    let idem = idem.map(|k| format!("idempotency-key: {k}\r\n")).unwrap_or_default();
    format!(
        "{method} {path} HTTP/1.1\r\nhost: {HOST}\r\nconnection: close\r\ncontent-type: application/json\r\n\
         content-length: {}\r\np2p-key-id: {key_id}\r\np2p-created: {created}\r\np2p-nonce: {nonce}\r\n\
         content-digest: {digest}\r\np2p-signature: {signature}\r\n{idem}\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn json_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

impl Server {
    fn send(&self, request: &[u8]) -> (u16, Vec<u8>) {
        exchange(&mut TcpStream::connect(self.addr).unwrap(), request)
    }

    fn get(&self, a: &Agent, path: &str) -> (u16, Vec<u8>) {
        self.send(&signed_at(&a.0, &a.1, "GET", path, "", None, now()))
    }

    fn post_with(&self, a: &Agent, path: &str, body: &Value, idem: &str) -> (u16, Value) {
        let (status, body) = self.send(&signed_at(&a.0, &a.1, "POST", path, &body.to_string(), Some(idem), now()));
        (status, json_of(&body))
    }

    fn post(&self, a: &Agent, path: &str, body: Value) -> (u16, Value) {
        self.post_with(a, path, &body, &Uuid::new_v4().to_string())
    }

    fn op(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let body = if body.is_null() { String::new() } else { body.to_string() };
        let request = format!(
            "{method} {path} HTTP/1.1\r\nhost: op\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let (status, body) = exchange(&mut std::os::unix::net::UnixStream::connect(&self.socket).unwrap(), request.as_bytes());
        (status, json_of(&body))
    }

    fn enroll(&self, org: &str, class: &str, a: &Agent) -> (u16, Value) {
        let (status, issued) = self.op("POST", "/enroll-codes", json!({ "org": org, "class": class }));
        assert_eq!(status, 201, "{issued}");
        self.enroll_with(issued["code"].as_str().unwrap(), a)
    }

    fn enroll_with(&self, code: &str, a: &Agent) -> (u16, Value) {
        let name = a.0.split('/').nth(1).unwrap();
        let body = json!({ "code": code, "name": name, "publicKey": public_key(&a.1) }).to_string();
        let (status, body) = self.send(&signed_at("enroll", &a.1, "POST", "/v1/enroll", &body, None, now()));
        (status, json_of(&body))
    }

    fn activate(&self, a: &Agent) {
        let fingerprint = sig::fingerprint(&a.1.verifying_key());
        assert_eq!(self.op("POST", "/keys/activate", json!({ "keyId": a.0, "fingerprint": fingerprint })).0, 200);
    }

    /// Lines the board logged since the last call, waiting briefly for more.
    fn logged(&self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(line) = self.log.recv_timeout(Duration::from_millis(300)) {
            lines.push(line);
        }
        lines
    }
}

// ---------------------------------------------------------------- a board with something on it

struct Board {
    supplier: Agent,
    reseller: Agent,
    pair_card: String,
    shared_card: String,
    notice: String,
}


/// Orgs, a pair room and a shared room, two enrolled and activated keys, a finding in each room with a reply, a
/// stage move, and a notice with its ack.
fn populate(s: &Server) -> Board {
    for org in ["supplier-a", "reseller-x", "reseller-y"] {
        assert_eq!(s.op("POST", "/orgs", json!({ "id": org })).0, 201);
    }
    let pair = json!({ "id": "a-x", "kind": "pair", "members": [{ "org": "supplier-a" }, { "org": "reseller-x" }] });
    assert_eq!(s.op("POST", "/rooms", pair).0, 201);
    let hub = json!({ "id": "hub", "kind": "shared", "members": [
        { "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-x", "handle": "member-2" }] });
    assert_eq!(s.op("POST", "/rooms", hub).0, 201);
    assert_eq!(s.op("POST", "/rooms/members", json!({ "room": "hub", "org": "reseller-y", "handle": "member-3" })).0, 201);
    assert_eq!(s.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "reseller-y" })).0, 200);

    let (supplier, reseller) = (agent("supplier-a/agent", 31), agent("reseller-x/agent", 32));
    for (org, a) in [("supplier-a", &supplier), ("reseller-x", &reseller)] {
        let (status, enrolled) = s.enroll(org, "text", a);
        assert_eq!((status, enrolled["status"].as_str()), (201, Some("pending")), "{enrolled}");
        s.activate(a);
    }

    let finding = |title: &str| json!({ "kind": "finding", "severity": "wrong", "title": title, "text": "Seen on a clean install." });
    let (status, card) = s.post(&reseller, "/v1/rooms/a-x/cards", finding("List endpoint skips one item when paging"));
    assert_eq!(status, 201, "{card}");
    let pair_card = card["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(s.post(&supplier, &format!("/v1/cards/{pair_card}/messages"), json!({ "kind": "reply", "text": "Looking at it." })).0, 201);
    let moved = s.post(&supplier, &format!("/v1/cards/{pair_card}/stage"), json!({ "stage": "acknowledged", "text": "Confirmed here too." }));
    assert_eq!(moved.0, 200, "{}", moved.1);

    let (status, card) = s.post(&reseller, "/v1/rooms/hub/cards", finding("Docs example uses a field name the API rejects"));
    assert_eq!(status, 201, "{card}");
    let shared_card = card["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(s.post(&supplier, &format!("/v1/cards/{shared_card}/messages"), json!({ "kind": "reply", "text": "The docs are being fixed." })).0, 201);

    let notice = json!({ "kind": "notice", "title": "Breaking: a response field is renamed in the next kit release", "text": "Plan for it." });
    let (status, card) = s.post(&supplier, "/v1/rooms/hub/cards", notice);
    assert_eq!(status, 201, "{card}");
    let notice = card["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(s.post(&reseller, &format!("/v1/cards/{notice}/messages"), json!({ "kind": "ack", "text": "Noted." })).0, 201);
    Board { supplier, reseller, pair_card, shared_card, notice }
}

/// Every read both members can make, and the operator's view.
fn reads(s: &Server, b: &Board) -> Vec<(String, u16, Vec<u8>)> {
    let mut out = Vec::new();
    for a in [&b.supplier, &b.reseller] {
        let cards = [&b.pair_card, &b.shared_card, &b.notice].map(|c| format!("/v1/cards/{c}"));
        let paths = ["/v1/rooms", "/v1/inbox", "/v1/keys", "/v1/rooms/a-x/cards", "/v1/rooms/hub/cards", "/v1/rooms/hub/cards?limit=1"];
        for path in paths.iter().map(|p| p.to_string()).chain(cards) {
            let (status, body) = s.get(a, &path);
            assert_eq!(status, 200, "{} {path}: {}", a.0, String::from_utf8_lossy(&body));
            out.push((format!("{} {path}", a.0), status, body));
        }
    }
    let (status, state) = s.op("GET", "/state", Value::Null);
    out.push(("operator /state".into(), status, state.to_string().into_bytes()));
    out
}

// ---------------------------------------------------------------- tests

#[test]
fn a_restart_keeps_every_read_byte_for_byte() {
    let dir = scratch("roundtrip");
    let s = start(&dir, &[]);
    let b = populate(&s);
    let before = reads(&s, &b);
    drop(s); // killed, not shut down: every acknowledged change must already be on disk

    let s = start(&dir, &[]);
    let after = reads(&s, &b);
    for ((what, status, body), (_, status_after, body_after)) in before.iter().zip(&after) {
        assert_eq!((status, String::from_utf8_lossy(body)), (status_after, String::from_utf8_lossy(body_after)), "{what}");
    }
    assert_eq!(before.len(), after.len());

    // The state file is owner-only.
    let mode = std::fs::metadata(dir.join("state.json")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
    // The board still works after the restart: a new card goes in, and the shared room still hides org ids.
    let (status, card) = s.post(&b.supplier, "/v1/rooms/hub/cards", json!({ "kind": "notice", "title": "Sample app fails to start on a clean install", "text": "Workaround in the thread." }));
    assert_eq!(status, 201, "{card}");
    assert_eq!(card["card"]["reporter"], "member-1");
}

#[test]
fn an_idempotent_write_replayed_after_restart_returns_the_first_answer() {
    let dir = scratch("replay");
    let s = start(&dir, &[]);
    let b = populate(&s);
    let idem = Uuid::new_v4().to_string();
    let body = json!({ "kind": "finding", "severity": "question", "title": "Is a sandbox mode planned for testing orders?", "text": "Asking for the next release." });
    let (status, first) = s.post_with(&b.reseller, "/v1/rooms/a-x/cards", &body, &idem);
    assert_eq!(status, 201, "{first}");
    let (_, listed) = s.get(&b.reseller, "/v1/rooms/a-x/cards");
    drop(s);

    let s = start(&dir, &[]);
    // Signed again (the old request is now stale), with the same idempotency key and the same body.
    let (status, again) = s.post_with(&b.reseller, "/v1/rooms/a-x/cards", &body, &idem);
    let mut expected = first.clone();
    expected["replayed"] = json!(true);
    assert_eq!((status, again), (201, expected));
    // Nothing new was created.
    assert_eq!(s.get(&b.reseller, "/v1/rooms/a-x/cards").1, listed);
    // The same key with a changed request is still a conflict.
    let changed = json!({ "kind": "finding", "severity": "question", "title": "Is a sandbox mode planned for testing orders?", "text": "Changed." });
    assert_eq!(s.post_with(&b.reseller, "/v1/rooms/a-x/cards", &changed, &idem), (409, json!({ "error": "idempotency_key_conflict" })));
}

#[test]
fn a_request_signed_before_the_restart_is_refused() {
    let dir = scratch("stale");
    let s = start(&dir, &[]);
    let b = populate(&s);
    let (status, unauthorized) = s.send(b"GET /v1/inbox HTTP/1.1\r\nhost: board.test\r\nconnection: close\r\n\r\n");
    assert_eq!((status, unauthorized.as_slice()), (401, b"unauthorized\n".as_slice()));
    // Signed now, sent only after the restart: fresh by the clock, but from before the new process began.
    let old_read = signed_at(&b.supplier.0, &b.supplier.1, "GET", "/v1/inbox", "", None, now());
    let note = json!({ "kind": "reply", "text": "Signed before the restart." }).to_string();
    let old_write = signed_at(&b.supplier.0, &b.supplier.1, "POST", &format!("/v1/cards/{}/messages", b.pair_card), &note, Some(&Uuid::new_v4().to_string()), now());
    let old_enroll = {
        let (_, issued) = s.op("POST", "/enroll-codes", json!({ "org": "reseller-y", "class": "text" }));
        let a = agent("reseller-y/agent", 33);
        let body = json!({ "code": issued["code"], "name": "agent", "publicKey": public_key(&a.1) }).to_string();
        signed_at("enroll", &a.1, "POST", "/v1/enroll", &body, None, now())
    };
    drop(s);

    let s = start(&dir, &[]);
    assert!(s.started.contains("state file"), "{}", s.started);
    for request in [&old_read, &old_write, &old_enroll] {
        assert_eq!(s.send(request), (401, unauthorized.clone()));
    }
    let refused: Vec<String> = s.logged().into_iter().filter(|l| l.starts_with("refused: ")).collect();
    assert_eq!(refused, ["refused: signed_before_start GET", "refused: signed_before_start POST", "refused: signed_before_start POST"]);
    // The same key, signing now, is accepted, and the refused write left nothing behind.
    let (status, card) = s.get(&b.supplier, &format!("/v1/cards/{}", b.pair_card));
    assert_eq!(status, 200);
    assert!(!String::from_utf8_lossy(&card).contains("Signed before the restart."));
}

#[test]
fn a_key_revoked_before_the_restart_stays_revoked() {
    let dir = scratch("revoked");
    let s = start(&dir, &[]);
    let b = populate(&s);
    let second = agent("reseller-x/second", 34);
    assert_eq!(s.enroll("reseller-x", "fields", &second).0, 201);
    s.activate(&second);
    // One revoked by the operator, one by its own organisation.
    assert_eq!(s.op("POST", "/keys/revoke", json!({ "keyId": b.supplier.0 })).0, 200);
    assert_eq!(s.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": second.0 })).0, 200);
    drop(s);

    let s = start(&dir, &[]);
    for a in [&b.supplier, &second] {
        assert_eq!(s.get(a, "/v1/whoami").0, 401, "{}", a.0);
    }
    assert_eq!(s.get(&b.reseller, "/v1/whoami").0, 200);
    let (_, state) = s.op("GET", "/state", Value::Null);
    let statuses: Vec<(String, String)> =
        state["keys"].as_array().unwrap().iter().map(|k| (k["id"].as_str().unwrap().into(), k["status"].as_str().unwrap().into())).collect();
    assert_eq!(statuses, [
        ("reseller-x/agent".to_owned(), "active".to_owned()),
        ("reseller-x/second".into(), "revoked".into()),
        ("supplier-a/agent".into(), "revoked".into())
    ]);
    // Revoked is final, after a restart too.
    let fingerprint = sig::fingerprint(&b.supplier.1.verifying_key());
    assert_eq!(s.op("POST", "/keys/activate", json!({ "keyId": b.supplier.0, "fingerprint": fingerprint })), (409, json!({ "error": "not_pending" })));
}

#[test]
fn a_pending_key_stays_pending() {
    let dir = scratch("pending");
    let s = start(&dir, &[]);
    populate(&s);
    let newcomer = agent("reseller-y/agent", 35);
    let (_, issued) = s.op("POST", "/enroll-codes", json!({ "org": "reseller-y", "class": "text" }));
    let code = issued["code"].as_str().unwrap().to_owned();
    let (status, enrolled) = s.enroll_with(&code, &newcomer);
    assert_eq!((status, enrolled["status"].as_str()), (201, Some("pending")));
    drop(s);

    let s = start(&dir, &[]);
    let (status, me) = s.get(&newcomer, "/v1/whoami");
    let me = json_of(&me);
    assert_eq!((status, me["status"].as_str(), me["rooms"].clone()), (200, Some("pending"), json!([])));
    assert_eq!(s.get(&newcomer, "/v1/inbox").0, 401);
    // The used code still replays for the same key and name, and for nothing else.
    assert_eq!(s.enroll_with(&code, &newcomer), (201, enrolled));
    assert_eq!(s.enroll_with(&code, &agent("reseller-y/other", 36)).0, 401);
    // Activation by fingerprint still works after the restart.
    s.activate(&newcomer);
    assert_eq!(s.get(&newcomer, "/v1/inbox").0, 200);
}

#[test]
fn a_damaged_or_open_state_file_stops_startup() {
    let dir = scratch("damaged");
    let state = dir.join("state.json");
    let s = start(&dir, &[]);
    populate(&s);
    drop(s);
    let good = std::fs::read(&state).unwrap();
    assert!(!good.is_empty());

    for mode in [0o640, 0o604, 0o644] {
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(mode)).unwrap();
        let (ok, err) = refuses_to_start(&dir, &[]);
        assert!(!ok && err.contains("readable by group or others") && err.contains("chmod 600"), "mode {mode:o}: {err}");
    }
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();

    let broken: [(&str, &[u8]); 4] = [
        ("cut short", &good[..good.len() / 2]),
        ("empty", b""),
        ("not json", b"not json"),
        ("another format", br#"{"format":"p2paecn-board-state","version":999}"#),
    ];
    for (what, bytes) in broken {
        std::fs::write(&state, bytes).unwrap();
        let (ok, err) = refuses_to_start(&dir, &[]);
        assert!(!ok && err.contains("cannot load state file") && err.contains("not started"), "{what}: {err}");
        // Never started empty over it: the file is left exactly as it was.
        assert_eq!(std::fs::read(&state).unwrap(), bytes, "{what}");
    }

    // A path that is a directory, or under a directory that does not exist, stops startup too.
    std::fs::write(&state, &good).unwrap();
    let (ok, err) = refuses_to_start(&dir, &[("P2PAECN_STATE", dir.to_str().unwrap())]);
    assert!(!ok && err.contains("state file"), "{err}");
    let (ok, err) = refuses_to_start(&dir, &[("P2PAECN_STATE", dir.join("missing/state.json").to_str().unwrap())]);
    assert!(!ok && err.contains("state file"), "{err}");

    // The good file still loads.
    let s = start(&dir, &[]);
    assert!(s.started.contains("3 orgs, 2 rooms, 2 keys"), "{}", s.started);
}

#[test]
fn a_failed_save_is_not_acknowledged() {
    let dir = scratch("readonly");
    let s = start(&dir, &[]);
    let b = populate(&s);
    let (_, state_before) = s.op("GET", "/state", Value::Null);
    let (_, cards_before) = s.get(&b.reseller, "/v1/rooms/a-x/cards");
    let (_, issued) = s.op("POST", "/enroll-codes", json!({ "org": "reseller-y", "class": "text" }));
    let code = issued["code"].as_str().unwrap().to_owned();
    let waiting = agent("supplier-a/waiting", 39);
    assert_eq!(s.enroll("supplier-a", "text", &waiting).0, 201);
    let (_, state_with_code) = s.op("GET", "/state", Value::Null);

    // The directory can no longer take the new file.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let not_saved = json!({ "error": "state_not_saved" });
    for (path, body) in [
        ("/orgs", json!({ "id": "reseller-z" })),
        ("/rooms", json!({ "id": "a-y", "kind": "pair", "members": [{ "org": "supplier-a" }, { "org": "reseller-y" }] })),
        ("/rooms/members", json!({ "room": "hub", "org": "reseller-y", "handle": "member-3" })),
        ("/rooms/members/remove", json!({ "room": "hub", "org": "reseller-x" })),
        ("/enroll-codes", json!({ "org": "reseller-y", "class": "text" })),
        ("/keys/revoke", json!({ "keyId": b.supplier.0 })),
        ("/keys/activate", json!({ "keyId": waiting.0, "fingerprint": sig::fingerprint(&waiting.1.verifying_key()) })),
    ] {
        assert_eq!(s.op("POST", path, body), (500, not_saved.clone()), "{path}");
    }
    let idem = Uuid::new_v4().to_string();
    let finding = json!({ "kind": "finding", "severity": "friction", "title": "Sample app fails to start on a clean install", "text": "Every time." });
    assert_eq!(s.post_with(&b.reseller, "/v1/rooms/a-x/cards", &finding, &idem), (500, not_saved.clone()));
    assert_eq!(s.post(&b.supplier, &format!("/v1/cards/{}/stage", b.pair_card), json!({ "stage": "shipped", "text": "Fixed." })), (500, not_saved.clone()));
    assert_eq!(s.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": b.reseller.0 })), (500, not_saved.clone()));
    assert_eq!(s.post(&b.supplier, &format!("/v1/cards/{}/messages", b.shared_card), json!({ "kind": "proof", "text": "Fixed in the docs." })), (500, not_saved.clone()));
    let newcomer = agent("reseller-y/agent", 37);
    assert_eq!(s.enroll_with(&code, &newcomer), (500, not_saved.clone()));
    // Refusals and reads are unaffected.
    assert_eq!(s.op("POST", "/orgs", json!({ "id": "supplier-a" })), (409, json!({ "error": "org_exists" })));
    assert!(s.logged().iter().any(|l| l.contains("state not saved")), "the reason is logged");

    // Nothing that was refused this way shows, in memory or on disk.
    assert_eq!(s.op("GET", "/state", Value::Null), (200, state_with_code.clone()));
    assert_eq!(s.get(&b.reseller, "/v1/rooms/a-x/cards").1, cards_before);
    assert_eq!(s.get(&b.reseller, "/v1/whoami").0, 200);
    assert_eq!(json_of(&s.get(&waiting, "/v1/whoami").1)["status"], "pending");

    // Once the directory is writable again, the same write goes in as new, not as a replay.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (status, card) = s.post_with(&b.reseller, "/v1/rooms/a-x/cards", &finding, &idem);
    assert_eq!((status, card.get("replayed")), (201, None), "{card}");
    // The code was not used up by the enrollment that could not be saved.
    assert_eq!(s.enroll_with(&code, &newcomer).0, 201);
    drop(s);

    let s = start(&dir, &[]);
    let (_, listed) = s.get(&b.reseller, "/v1/rooms/a-x/cards");
    let titles: Vec<Value> = json_of(&listed)["cards"].as_array().unwrap().iter().map(|c| c["title"].clone()).collect();
    assert_eq!(titles, [json!("Sample app fails to start on a clean install"), json!("List endpoint skips one item when paging")]);
    let (_, state) = s.op("GET", "/state", Value::Null);
    assert_eq!(state["orgs"], state_before["orgs"]);
    assert_eq!(state["keys"].as_array().unwrap().len(), 4);
}

#[test]
fn the_config_applies_only_when_no_state_file_exists() {
    let dir = scratch("config");
    let key = agent("reseller-x/boot", 38);
    let config = |room: &str| {
        json!({ "host": HOST, "rooms": [{ "id": room, "orgs": ["supplier-a", "reseller-x"] }],
                "keys": [{ "id": key.0, "org": "reseller-x", "class": "text", "expires": 4102444800u64, "public_key": public_key(&key.1) }] })
        .to_string()
    };
    std::fs::write(dir.join("first.json"), config("first-room")).unwrap();
    std::fs::write(dir.join("second.json"), config("second-room")).unwrap();
    let first = dir.join("first.json");
    let second = dir.join("second.json");

    // No state file yet: the config is applied, and the state file is written at once. The host comes from the
    // config here.
    let s = start(&dir, &[("P2PAECN_CONFIG", first.to_str().unwrap()), ("P2PAECN_HOST", "")]);
    assert!(s.started.contains("new state file") && s.started.contains("from P2PAECN_CONFIG"), "{}", s.started);
    assert!(dir.join("state.json").exists());
    assert_eq!(s.op("POST", "/orgs", json!({ "id": "reseller-y" })).0, 201);
    drop(s);

    // The state file exists: it wins, the config is not applied, and the host saved in it is used.
    let s = start(&dir, &[("P2PAECN_CONFIG", second.to_str().unwrap()), ("P2PAECN_HOST", "")]);
    assert!(s.started.contains("loaded from state file") && s.started.contains("P2PAECN_CONFIG ignored"), "{}", s.started);
    assert!(s.started.contains(&format!("host {HOST},")), "{}", s.started);
    let (_, state) = s.op("GET", "/state", Value::Null);
    assert_eq!(state["orgs"], json!(["reseller-x", "reseller-y", "supplier-a"]));
    let rooms: Vec<&str> = state["rooms"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(rooms, ["first-room"]);
    let (status, me) = s.get(&key, "/v1/whoami");
    assert_eq!((status, json_of(&me)["status"].clone()), (200, json!("active")));
}

#[test]
fn without_a_state_file_the_board_says_it_keeps_nothing() {
    let dir = scratch("memory");
    let s = start(&dir, &[("P2PAECN_STATE", "")]);
    assert!(s.started.ends_with("(in-memory store: every organisation, room, key and card is lost on restart)"), "{}", s.started);
    assert_eq!(s.op("POST", "/orgs", json!({ "id": "supplier-a" })).0, 201);
    assert!(!dir.join("state.json").exists());
    drop(s);
    let s = start(&dir, &[("P2PAECN_STATE", "")]);
    assert_eq!(s.op("GET", "/state", Value::Null).1["orgs"], json!([]));
}

// ---------------------------------------------------------------- the room records across versions and restarts

/// A state file as version 0.2.0 wrote it (format version 1, no room records), made by running that version: a
/// pair room `a-x` and a shared room `hub` with a removed member, findings in several stages, a notice with its
/// ack, a correction. Its keys are test keys from the fixed seeds 41 to 45 and sign nothing outside these tests.
const OLD_STATE: &[u8] = include_bytes!("fixtures/board-state-v1.json");

fn put_state(dir: &Path, bytes: &[u8]) -> PathBuf {
    let state = dir.join("state.json");
    std::fs::write(&state, bytes).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
    state
}

/// The whole record of a room, as bytes, and parsed.
fn record(s: &Server, a: &Agent, room: &str) -> (Vec<u8>, Value) {
    let (status, body) = s.get(a, &format!("/v1/rooms/{room}/chain?limit=1000"));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let parsed = json_of(&body);
    (body, parsed)
}

/// Every card of a room with its thread.
fn details(s: &Server, a: &Agent, room: &str) -> Vec<Value> {
    let list = json_of(&s.get(a, &format!("/v1/rooms/{room}/cards?limit=100")).1);
    list["cards"].as_array().unwrap().iter().map(|c| json_of(&s.get(a, &format!("/v1/cards/{}", c["id"].as_str().unwrap())).1)).collect()
}

#[test]
fn a_state_file_from_before_the_records_loads_and_gives_the_same_records_at_every_start() {
    let dir = scratch("upgrade");
    let state = put_state(&dir, OLD_STATE);
    let old: Value = serde_json::from_slice(OLD_STATE).unwrap();
    assert_eq!((old["version"].clone(), old.get("chains")), (json!(1), None), "the fixture is of the old format");
    let (supplier, reseller, fields) = (agent("supplier-a/agent", 41), agent("reseller-x/agent", 42), agent("reseller-x/loop", 44));

    let s = start(&dir, &[]);
    assert!(s.started.contains("loaded from a version 1 state file, room records built from it"), "{}", s.started);
    let mut first = Vec::new();
    for room in ["a-x", "hub"] {
        let (bytes, parsed) = record(&s, &supplier, room);
        let entries = parsed["entries"].as_array().unwrap();
        // The record verifies against every card, message and stage the board shows.
        assert_eq!(common::record_differs(room, &parsed["entries"], &details(&s, &supplier, room)), Vec::<String>::new(), "{room}");
        assert_eq!(parsed["head"], json!({ "seq": entries.len(), "hash": entries.last().unwrap()["hash"] }));
        // It ends with the import entry, which counts what stands before it.
        let last = entries.last().unwrap();
        let count = (entries.len() - 1).to_string();
        assert_eq!((last["kind"].clone(), last["actor"].clone(), last["time"].clone()), (json!("import"), json!("*import"), json!(0)));
        assert_eq!(last["payload"], json!(p2paecn_board::chain::Payload::Import { entries: &count }.hash()));
        // Every key class of a member gets the same bytes.
        assert_eq!(record(&s, &reseller, room).0, bytes);
        assert_eq!(record(&s, &fields, room).0, bytes);
        first.push(bytes);
    }
    let kinds = |parsed: &Value| -> Vec<(String, String)> {
        parsed["entries"].as_array().unwrap().iter().map(|e| (e["kind"].as_str().unwrap().to_owned(), e["actor"].as_str().unwrap().to_owned())).collect()
    };
    let pairs = |list: &[(&str, &str)]| -> Vec<(String, String)> { list.iter().map(|(k, a)| (k.to_string(), a.to_string())).collect() };
    assert_eq!(kinds(&record(&s, &supplier, "hub").1), pairs(&[
        ("member-added", "*import"), ("member-added", "*import"), ("member-added", "*import"),
        ("card", "member-2"), ("message", "member-1"), ("message", "member-2"),
        ("card", "member-1"), ("card", "member-3"), ("message", "member-1"),
        ("stage", "*import"), ("stage", "*import"),
        ("member-removed", "*import"), ("import", "*import"),
    ]));
    assert_eq!(kinds(&record(&s, &supplier, "a-x").1).len(), 2 + 7 + 2 + 2 + 2 + 1, "members, the first thread, two more cards with a message each, two stages, the import entry");
    let hub = String::from_utf8(first[1].clone()).unwrap();
    for org in ["supplier-a", "reseller-x", "reseller-y", "/agent", "/loop"] {
        assert!(!hub.contains(org), "{org} in the record of a shared room");
    }
    // Loading and reading wrote nothing: the file is the old one, byte for byte.
    drop(s);
    assert_eq!(std::fs::read(&state).unwrap(), OLD_STATE);

    // A second start builds the same records: the same heads, the same bytes.
    let s = start(&dir, &[]);
    assert!(s.started.contains("version 1 state file"), "{}", s.started);
    for (room, bytes) in ["a-x", "hub"].iter().zip(&first) {
        assert_eq!(&record(&s, &supplier, room).0, bytes, "{room}");
    }

    // The first accepted change writes the file as version 2, records included, and appends after the import entry.
    let before = record(&s, &supplier, "hub").1;
    let (status, card) = s.post(&supplier, "/v1/rooms/hub/cards", json!({ "kind": "notice", "title": "Sample app fails to start on a clean install", "text": "Workaround in the thread." }));
    assert_eq!(status, 201, "{card}");
    let (after_bytes, after) = record(&s, &supplier, "hub");
    let (old_entries, new_entries) = (before["entries"].as_array().unwrap(), after["entries"].as_array().unwrap());
    assert_eq!(new_entries.len(), old_entries.len() + 1);
    assert_eq!(&new_entries[..old_entries.len()], &old_entries[..]);
    let added = new_entries.last().unwrap();
    assert_eq!((added["kind"].clone(), added["actor"].clone(), added["prev"].clone()), (json!("card"), json!("member-1"), before["head"]["hash"].clone()));
    drop(s);
    let saved: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(saved["version"], json!(2));
    assert_eq!(saved["chains"]["hub"], after["entries"]);
    assert_eq!(saved["chains"]["a-x"].as_array().map(Vec::len), Some(16));

    // From now on the records come from the file.
    let s = start(&dir, &[]);
    assert!(s.started.contains("loaded from state file") && !s.started.contains("version 1"), "{}", s.started);
    assert_eq!(record(&s, &supplier, "hub").0, after_bytes);
    assert_eq!(record(&s, &supplier, "a-x").0, first[0]);
    assert_eq!(common::record_differs("hub", &after["entries"], &details(&s, &supplier, "hub")), Vec::<String>::new());
}

#[test]
fn a_state_file_whose_records_do_not_link_stops_startup() {
    let dir = scratch("chains");
    let state = dir.join("state.json");
    let s = start(&dir, &[]);
    populate(&s);
    drop(s);
    let good: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(good["version"], json!(2));
    assert!(good["chains"]["a-x"].as_array().unwrap().len() >= 6 && good["chains"]["hub"].is_array());

    let edit = |change: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        change(&mut v);
        v
    };
    let broken = [
        ("an entry removed", edit(&|v| drop(v["chains"]["a-x"].as_array_mut().unwrap().remove(2))), "the chain of room a-x does not link at entry 3"),
        ("an entry's time changed", edit(&|v| v["chains"]["a-x"][3]["time"] = json!(1)), "the chain of room a-x does not link at entry 4"),
        ("an entry's actor changed", edit(&|v| v["chains"]["hub"][3]["actor"] = json!("member-1")), "the chain of room hub does not link at entry 4"),
        ("the first entry dropped", edit(&|v| drop(v["chains"]["hub"].as_array_mut().unwrap().remove(0))), "the chain of room hub does not link at entry 1"),
        ("no chain for a room", edit(&|v| drop(v["chains"].as_object_mut().unwrap().remove("hub"))), "the chains are not for exactly the rooms"),
        ("no chains at all", edit(&|v| drop(v.as_object_mut().unwrap().remove("chains"))), "a version 2 file without chains"),
        ("chains in a version 1 file", edit(&|v| v["version"] = json!(1)), "a version 1 file with chains"),
        ("a later version", edit(&|v| v["version"] = json!(3)), "this server reads versions 1 and 2 only"),
    ];
    for (what, value, reason) in broken {
        let bytes = value.to_string().into_bytes();
        put_state(&dir, &bytes);
        let (ok, err) = refuses_to_start(&dir, &[]);
        assert!(!ok && err.contains("cannot load state file") && err.contains(reason) && err.contains("not started"), "{what}: {err}");
        assert_eq!(std::fs::read(&state).unwrap(), bytes, "{what}: the file is left as it was");
    }
    // What the start does not catch, and a member's saved head does: the end cut off, with the cards left alone.
    let cut = edit(&|v| drop(v["chains"]["a-x"].as_array_mut().unwrap().pop()));
    put_state(&dir, cut.to_string().as_bytes());
    let s = start(&dir, &[]);
    assert!(s.started.contains("loaded from state file"), "{}", s.started);
}
