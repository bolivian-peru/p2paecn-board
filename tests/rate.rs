//! Rate limits (src/rate.rs) through the real router and the real listener: each limit, what a signed caller over
//! it gets (`429 rate_limited` with `Retry-After`), that the refused request can be sent again unchanged, that
//! nothing is appended to a room's record, and that a caller without a valid signature gets the identical 401
//! however much it sends. Every example finding is invented.

mod common;

use axum::http::StatusCode;
use common::*;
use p2paecn_board::api::{self, Peer};
use p2paecn_board::rate::Config;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;
use uuid::Uuid;

struct Board {
    w: World,
    supplier: Agent,
    reseller: Agent,
    second: Agent,
}

fn board(limits: Config) -> Board {
    let supplier = agent("supplier-a/agent", 81);
    let reseller = agent("reseller-x/agent", 82);
    let second = agent("reseller-x/second", 83);
    let w = world_with_limits(
        json!({
            "host": HOST,
            "rooms": [
                { "id": "a-x", "orgs": ["supplier-a", "reseller-x"] },
                { "id": "hub", "kind": "shared", "members": [{ "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-x", "handle": "member-2" }] }
            ],
            "keys": [key_entry(&supplier, "text", FAR_FUTURE), key_entry(&reseller, "text", FAR_FUTURE), key_entry(&second, "text", FAR_FUTURE)]
        }),
        limits,
    );
    Board { w, supplier, reseller, second }
}

fn notice(n: usize) -> Value {
    json!({ "kind": "notice", "title": format!("Maintenance window number {n} is planned"), "text": "Plan for it." })
}

/// The 429 a signed caller over a limit gets: the error code, and the same whole number of seconds in the body
/// and in `Retry-After`.
fn assert_limited(r: &Raw, at_most: u64) -> u64 {
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", String::from_utf8_lossy(&r.body));
    let body: Value = serde_json::from_slice(&r.body).unwrap();
    let header = r.headers.iter().find(|(name, _)| name == "retry-after").map(|(_, v)| v.parse::<u64>().unwrap());
    assert_eq!(body["error"], json!("rate_limited"));
    assert_eq!(Some(body["retryAfter"].as_u64().unwrap()), header);
    let seconds = header.unwrap();
    assert!((1..=at_most).contains(&seconds), "{seconds}");
    seconds
}

async fn head(w: &World, a: &Agent, room: &str) -> Value {
    w.get(a, "/v1/rooms").await.1["rooms"].as_array().unwrap().iter().find(|r| r["id"] == json!(room)).unwrap()["chain"].clone()
}

async fn until_the_window_ends() {
    tokio::time::sleep(Duration::from_millis(5100)).await;
}

#[tokio::test]
async fn a_key_over_its_request_limit_gets_429_and_the_same_request_passes_later() {
    let b = board(Config { key_requests: 5, window: 5, ..Config::off() });
    let w = &b.w;
    for _ in 0..5 {
        assert_eq!(w.get(&b.reseller, "/v1/inbox").await.0, StatusCode::OK);
    }
    let request = signed(&b.reseller, "GET", "/v1/inbox", "", None);
    let again = copy(&request, "");
    let once_more = copy(&request, "");
    assert_limited(&raw(&w.app, request).await, 5);
    // Reads and writes count alike, and so does a request that would be refused for another reason.
    let idem = Uuid::new_v4().to_string();
    assert_limited(&raw(&w.app, signed(&b.reseller, "POST", "/v1/rooms/a-x/cards", &notice(1).to_string(), Some(&idem))).await, 5);
    assert_limited(&raw(&w.app, signed(&b.reseller, "GET", "/v1/rooms/no-such-room/cards", "", None)).await, 5);
    // Another key, of the same organisation too, has its own count.
    assert_eq!(w.get(&b.second, "/v1/inbox").await.0, StatusCode::OK);
    assert_eq!(w.get(&b.supplier, "/v1/inbox").await.0, StatusCode::OK);

    // A request that does not verify never gets the 429, also under the id of a key that is over its limit: it
    // gets the 401, and that tells nothing about the key.
    let unauthorized = w.unauthorized().await;
    let stranger = agent("reseller-x/agent", 99);
    assert_eq!(raw(&w.app, signed(&stranger, "GET", "/v1/inbox", "", None)).await, unauthorized);

    // A request that was accepted, sent again: a replay is the 401 and is not counted, so whoever captured a
    // request cannot use up the key's requests with it, and learns nothing from a 429.
    let accepted = signed(&b.supplier, "GET", "/v1/rooms", "", None);
    let replays: Vec<_> = (0..20).map(|_| copy(&accepted, "")).collect();
    assert_eq!(raw(&w.app, accepted).await.status, StatusCode::OK); // the supplier's second request of five
    for replay in replays {
        assert_eq!(raw(&w.app, replay).await, unauthorized);
    }
    for _ in 0..3 {
        assert_eq!(w.get(&b.supplier, "/v1/inbox").await.0, StatusCode::OK, "twenty replays used up nothing");
    }
    let last = signed(&b.supplier, "GET", "/v1/inbox", "", None);
    assert_limited(&raw(&w.app, last).await, 5);
    // And a replay of an accepted request stays the 401 while the key is over its limit.
    let seen = signed(&b.second, "GET", "/v1/rooms", "", None);
    let seen_again = copy(&seen, "");
    assert_eq!(raw(&w.app, seen).await.status, StatusCode::OK);
    for _ in 0..3 {
        w.get(&b.second, "/v1/inbox").await;
    }
    assert_limited(&raw(&w.app, signed(&b.second, "GET", "/v1/inbox", "", None)).await, 5);
    assert_eq!(raw(&w.app, seen_again).await, unauthorized);

    // The refused request did not use up its nonce: the very same bytes pass once the window has ended, and only
    // then is the nonce taken.
    until_the_window_ends().await;
    assert_eq!(raw(&w.app, again).await.status, StatusCode::OK);
    assert_eq!(raw(&w.app, once_more).await, unauthorized, "now it is a replay");
}

#[tokio::test]
async fn a_key_over_its_write_limit_writes_nothing_and_a_replay_is_not_a_write() {
    let b = board(Config { key_writes: 3, window: 5, ..Config::off() });
    let w = &b.w;
    let first_body = notice(0).to_string();
    let first_idem = Uuid::new_v4().to_string();
    let (status, first) = send(&w.app, signed(&b.reseller, "POST", "/v1/rooms/a-x/cards", &first_body, Some(&first_idem))).await;
    assert_eq!(status, StatusCode::CREATED);
    for n in 1..3 {
        assert_eq!(w.post(&b.reseller, "/v1/rooms/a-x/cards", notice(n)).await.0, StatusCode::CREATED);
    }
    let before = head(w, &b.reseller, "a-x").await;
    assert_eq!(before["seq"], json!(5));

    let body = notice(3).to_string();
    let idem = Uuid::new_v4().to_string();
    let refused_write = signed(&b.reseller, "POST", "/v1/rooms/a-x/cards", &body, Some(&idem));
    let (same_bytes, same_bytes_again) = (copy(&refused_write, &body), copy(&refused_write, &body));
    assert_limited(&raw(&w.app, refused_write).await, 5);
    assert_limited(&raw(&w.app, signed(&b.reseller, "POST", "/v1/keys/revoke", r#"{"keyId":"reseller-x/second"}"#, Some(&Uuid::new_v4().to_string()))).await, 5);
    // Nothing was stored and nothing appended to the record. Reads are not writes.
    assert_eq!(head(w, &b.reseller, "a-x").await, before);
    assert_eq!(w.get(&b.reseller, "/v1/rooms/a-x/cards").await.1["cards"].as_array().unwrap().len(), 3);
    assert_eq!(w.get(&b.second, "/v1/keys").await.1["keys"][1]["status"], json!("active"));
    // The key's own count takes attempts, the ones the rules would refuse included: over its limit, a write
    // with an empty title gets the 429 before the rules are looked at.
    let empty_title = r#"{"kind":"notice","title":"","text":"x"}"#;
    assert_limited(&raw(&w.app, signed(&b.reseller, "POST", "/v1/rooms/a-x/cards", empty_title, Some(&Uuid::new_v4().to_string()))).await, 5);
    // A replay of a write that was done returns its first answer: it is not a new write and is not refused.
    let (status, replayed) = send(&w.app, signed(&b.reseller, "POST", "/v1/rooms/a-x/cards", &first_body, Some(&first_idem))).await;
    let mut expected: Value = serde_json::from_str(&first).unwrap();
    expected["replayed"] = json!(true);
    assert_eq!((status, serde_json::from_str::<Value>(&replayed).unwrap()), (StatusCode::CREATED, expected));
    // The other side's key is not held back by this one.
    assert_eq!(w.post(&b.supplier, "/v1/rooms/a-x/cards", notice(9)).await.0, StatusCode::CREATED);

    // After the window the refused write is done once, sent again as the very same signed bytes: the 429 took
    // back the nonce that the signature check had recorded. Once it is accepted, those bytes are a replay.
    until_the_window_ends().await;
    let (status, done) = send(&w.app, same_bytes).await;
    assert_eq!(status, StatusCode::CREATED, "{done}");
    assert_eq!(raw(&w.app, same_bytes_again).await, w.unauthorized().await);
    let (status, replay) = send(&w.app, signed(&b.reseller, "POST", "/v1/rooms/a-x/cards", &body, Some(&idem))).await;
    assert_eq!((status, serde_json::from_str::<Value>(&replay).unwrap()["replayed"].clone()), (StatusCode::CREATED, json!(true)));
    assert_eq!(head(w, &b.reseller, "a-x").await["seq"], json!(7), "the other side's card and this one, each once");
}

#[tokio::test]
async fn a_room_over_its_write_limit_refuses_every_member_and_other_rooms_go_on() {
    let b = board(Config { room_writes: 4, ..Config::off() });
    let w = &b.w;
    let (_, card) = w.post(&b.reseller, "/v1/rooms/a-x/cards", json!({ "kind": "finding", "severity": "wrong", "title": "List endpoint skips one item when paging", "text": "Page two starts at item 22." })).await;
    let card = card["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "Looking at it." })).await.0, StatusCode::CREATED);
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/stage"), json!({ "stage": "acknowledged", "text": "Confirmed." })).await.0, StatusCode::OK);
    // Writes the rules refuse do not count against the room: a member cannot use up the room's writes with
    // requests that leave nothing in it. Twenty-one of them, of three kinds, and the room still takes its fourth.
    for _ in 0..7 {
        assert_eq!(w.post(&b.reseller, &format!("/v1/cards/{card}/stage"), json!({ "stage": "shipped", "text": "Our own card." })).await.0, StatusCode::FORBIDDEN);
        assert_eq!(w.post(&b.reseller, &format!("/v1/cards/{card}/stage"), json!({ "stage": "closed", "outcome": "verified", "text": "Too early." })).await.0, StatusCode::CONFLICT);
        assert_eq!(w.post(&b.second, "/v1/rooms/a-x/cards", json!({ "kind": "notice", "title": "", "text": "No title." })).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert_eq!(w.post(&b.second, &format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "The fourth accepted write." })).await.0, StatusCode::CREATED);
    let before = head(w, &b.reseller, "a-x").await;
    assert_eq!(before["seq"], json!(7), "two members, the card, a reply, the two entries of the move, a reply");

    // Every kind of write into the room, from every member, is now refused, and the record stands still.
    let idem = || Uuid::new_v4().to_string();
    for (a, path, body) in [
        (&b.reseller, "/v1/rooms/a-x/cards".to_owned(), notice(1)),
        (&b.second, "/v1/rooms/a-x/cards".to_owned(), notice(2)),
        (&b.supplier, format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "More." })),
        (&b.supplier, format!("/v1/cards/{card}/stage"), json!({ "stage": "shipped", "text": "Fixed." })),
    ] {
        assert_limited(&raw(&w.app, signed(a, "POST", &path, &body.to_string(), Some(&idem()))).await, 60);
    }
    assert_eq!(head(w, &b.reseller, "a-x").await, before);
    // Reads of the room, writes into another room and writes outside rooms go on.
    assert_eq!(w.get(&b.reseller, &format!("/v1/cards/{card}")).await.0, StatusCode::OK);
    assert_eq!(w.post(&b.reseller, "/v1/rooms/hub/cards", notice(3)).await.0, StatusCode::CREATED);
    assert_eq!(w.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": "reseller-x/second" })).await.0, StatusCode::OK);
    // A room the caller is not in is the 404 as before, and uses up nothing of that room.
    let outsider_room = w.post(&b.reseller, "/v1/rooms/no-such-room/cards", notice(4)).await;
    assert_eq!(outsider_room, (StatusCode::NOT_FOUND, json!({ "error": "not_found" })));
}

/// A request as it arrives through the listener from `ip`.
fn from(ip: &str, mut req: axum::http::Request<axum::body::Body>) -> axum::http::Request<axum::body::Body> {
    req.extensions_mut().insert(Peer(ip.parse::<IpAddr>().unwrap()));
    req
}

#[tokio::test]
async fn a_source_over_its_failure_limit_keeps_getting_the_identical_401_and_its_enroll_attempts_are_not_looked_at() {
    let b = board(Config { unsigned: 6, window: 5, ..Config::off() });
    let w = &b.w;
    let unauthorized = raw(&w.app, from("203.0.113.7", plain("GET", "/v1/inbox", ""))).await;
    assert_eq!((unauthorized.status, unauthorized.body.as_slice()), (StatusCode::UNAUTHORIZED, b"unauthorized\n".as_slice()));
    let stranger = agent("reseller-x/agent", 99);
    let (code, new_key) = (w.enroll_code("reseller-x", "text").await, agent("reseller-x/third", 84));
    let enroll = |code: &str| {
        let body = json!({ "code": code, "name": "third", "publicKey": public_key(&new_key.1) }).to_string();
        signed_with("enroll", &new_key.1, "POST", "/v1/enroll", &body, None)
    };

    // Far more failures than the limit, of every kind: each answer is the same bytes, before and after the limit.
    for n in 0..40 {
        let request = match n % 5 {
            0 => plain("GET", "/v1/inbox", ""),
            1 => plain("POST", "/v1/nothing-here", "{}"),
            2 => signed(&stranger, "GET", "/v1/rooms", "", None),
            3 => enroll("p2p-aaaa-bbbb-cccc-dddd-eeee-ffff"),
            _ => plain("GET", "/v1/rooms/a-x/chain", ""),
        };
        assert_eq!(raw(&w.app, from("203.0.113.7", request)).await, unauthorized, "request {n}");
    }
    // Over the limit an enroll attempt is not looked at, a right code included: the same 401, and the code is
    // not used up.
    assert_eq!(raw(&w.app, from("203.0.113.7", enroll(&code))).await, unauthorized);
    let state = w.op("GET", "/state", Value::Null).await.1;
    assert_eq!(state["codes"][0]["used"], json!(false));
    assert!(state["keys"].as_array().unwrap().iter().all(|k| k["id"] != json!("reseller-x/third")));
    // A member with a valid signature behind the same address is still served, and never sees a 429 for it.
    let (status, _) = send(&w.app, from("203.0.113.7", signed(&b.reseller, "GET", "/v1/inbox", "", None))).await;
    assert_eq!(status, StatusCode::OK);
    // The public pages are still served.
    assert_eq!(raw(&w.app, from("203.0.113.7", plain("GET", "/", ""))).await.status, StatusCode::OK);
    // An IPv6 caller is counted by its /64: failures from many addresses of one /64 are one count.
    for n in 0..6 {
        assert_eq!(raw(&w.app, from(&format!("2001:db8:5:5::{n:x}"), plain("GET", "/v1/inbox", ""))).await, unauthorized);
    }
    assert!(w.shared.limits.unsigned_over("2001:db8:5:5::/64", now()));
    assert_eq!(raw(&w.app, from("2001:db8:5:5:ffff::1", enroll(&code))).await, unauthorized, "another address of that /64 cannot enroll now");
    assert_eq!(w.op("GET", "/state", Value::Null).await.1["codes"][0]["used"], json!(false));
    // Another address has its own count: its enroll attempt is looked at. A forwarding header is not believed.
    let mut spoofed = from("203.0.113.7", enroll(&code));
    spoofed.headers_mut().insert("x-forwarded-for", "203.0.113.99".parse().unwrap());
    assert_eq!(raw(&w.app, spoofed).await, unauthorized);
    assert_eq!(w.op("GET", "/state", Value::Null).await.1["codes"][0]["used"], json!(false));

    // When the window has ended the same address enrolls with the same code.
    until_the_window_ends().await;
    assert_eq!(raw(&w.app, from("203.0.113.7", enroll(&code))).await.status, StatusCode::CREATED);
}

/// A request that is served counts nothing and leaves no window: a thousand visitors from a thousand addresses
/// take no room in the table. This is its own test because it takes longer than a short window lasts on a slow
/// machine.
#[tokio::test]
async fn served_requests_from_many_addresses_leave_no_window() {
    let b = board(Config { unsigned: 6, window: 600, ..Config::off() });
    let w = &b.w;
    assert_eq!(raw(&w.app, from("203.0.113.7", plain("GET", "/v1/inbox", ""))).await.status, StatusCode::UNAUTHORIZED);
    let windows = w.shared.limits.window_count();
    for n in 0..1000 {
        let visitor = format!("198.51.{}.{}", 100 + n / 250, n % 250);
        assert_eq!(raw(&w.app, from(&visitor, plain("GET", "/", ""))).await.status, StatusCode::OK);
        let (status, _) = send(&w.app, from(&visitor, signed(&b.supplier, "GET", "/v1/rooms", "", None))).await;
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(w.shared.limits.window_count(), windows);
}

#[tokio::test]
async fn with_a_source_header_configured_the_last_value_of_that_header_is_the_source() {
    let header = axum::http::HeaderName::from_static("x-forwarded-for");
    let b = board(Config { unsigned: 3, source_header: Some(header), ..Config::off() });
    let w = &b.w;
    let via = |client: &str, req: axum::http::Request<axum::body::Body>| {
        let mut req = from("198.51.100.4", req); // the proxy
        req.headers_mut().insert("x-forwarded-for", format!("192.0.2.1, {client}").parse().unwrap());
        req
    };
    let code = w.enroll_code("reseller-x", "text").await;
    let new_key = agent("reseller-x/third", 84);
    let enroll = || {
        let body = json!({ "code": code, "name": "third", "publicKey": public_key(&new_key.1) }).to_string();
        signed_with("enroll", &new_key.1, "POST", "/v1/enroll", &body, None)
    };
    for _ in 0..5 {
        assert_eq!(raw(&w.app, via("203.0.113.7", plain("GET", "/v1/inbox", ""))).await.status, StatusCode::UNAUTHORIZED);
    }
    // That client is over its limit. Another client behind the same proxy is not.
    assert_eq!(raw(&w.app, via("203.0.113.7", enroll())).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(raw(&w.app, via("203.0.113.8", enroll())).await.status, StatusCode::CREATED);
}

#[tokio::test(flavor = "multi_thread")]
async fn through_the_listener_every_401_is_the_same_bytes_under_load() {
    let b = board(Config { unsigned: 10, ..Config::off() });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(api::serve(listener, b.w.app.clone(), Duration::from_secs(5), Duration::from_secs(10)));
    let answers = tokio::task::spawn_blocking(move || {
        let ask = |request: &str| {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut got = String::new();
            stream.read_to_string(&mut got).unwrap();
            // The Date header changes with the clock; everything else must not.
            got.lines().filter(|l| !l.to_ascii_lowercase().starts_with("date:")).collect::<Vec<_>>().join("\n")
        };
        let mut answers = Vec::new();
        for n in 0..60 {
            let (method, path) = [("GET", "/v1/inbox"), ("POST", "/v1/enroll"), ("GET", "/v1/rooms/a-x/chain"), ("DELETE", "/v1/whoami")][n % 4];
            answers.push(ask(&format!("{method} {path} HTTP/1.1\r\nHost: board.test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")));
        }
        answers
    })
    .await
    .unwrap();
    server.abort();
    assert!(answers[0].starts_with("HTTP/1.1 401") && answers[0].ends_with("unauthorized"), "{}", answers[0]);
    assert!(!answers[0].to_ascii_lowercase().contains("retry-after"));
    for (n, answer) in answers.iter().enumerate() {
        assert_eq!(answer, &answers[0], "answer {n}");
    }
    // The listener gave the limiter the peer's address: the address is over its limit now.
    assert!(b.w.shared.limits.unsigned_over("127.0.0.1", now()));
}

struct Server(std::process::Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_binary_reads_its_limits_from_the_environment_and_stops_logging_a_source_that_is_over() {
    let run = |env: &[(&str, &str)]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_p2paecn-board"));
        cmd.env("P2PAECN_HOST", HOST).env("P2PAECN_BIND", "127.0.0.1:0").env_remove("P2PAECN_ADMIN_SOCKET").env_remove("P2PAECN_CONFIG").env_remove("P2PAECN_STATE");
        for name in ["P2PAECN_LIMIT_KEY_REQUESTS", "P2PAECN_LIMIT_KEY_WRITES", "P2PAECN_LIMIT_ROOM_WRITES", "P2PAECN_LIMIT_UNSIGNED", "P2PAECN_LIMIT_WINDOW_SECONDS", "P2PAECN_SOURCE_HEADER"] {
            cmd.env_remove(name);
        }
        cmd.envs(env.iter().copied()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap()
    };
    // A limit that is not understood stops startup.
    let mut bad = run(&[("P2PAECN_LIMIT_KEY_WRITES", "plenty")]);
    let mut said = String::new();
    bad.stderr.take().unwrap().read_to_string(&mut said).unwrap();
    assert!(!bad.wait().unwrap().success() && said.contains("P2PAECN_LIMIT_KEY_WRITES must be a whole number"), "{said}");

    // The defaults, as the startup line states them.
    let mut defaults = Server(run(&[]));
    let mut line = String::new();
    let mut out = BufReader::new(defaults.0.stderr.take().unwrap());
    out.read_line(&mut line).unwrap();
    assert!(line.contains("listening on") && !line.contains("limits"), "{line}");
    line.clear();
    out.read_line(&mut line).unwrap();
    assert!(line.contains("limits per 60 s: 1200 requests and 60 writes per key, 240 writes per room, 120 failed or enroll requests per source address (source address is the TCP peer)"), "{line}");
    drop(defaults);

    let mut server = Server(run(&[("P2PAECN_LIMIT_UNSIGNED", "4"), ("P2PAECN_LIMIT_KEY_REQUESTS", "0")]));
    let mut log = BufReader::new(server.0.stderr.take().unwrap());
    let mut started = String::new();
    log.read_line(&mut started).unwrap();
    let mut limits = String::new();
    log.read_line(&mut limits).unwrap();
    assert!(limits.contains("none requests and 60 writes per key") && limits.contains("4 failed or enroll requests per source address"), "{limits}");
    let addr: std::net::SocketAddr = started.split("listening on ").nth(1).and_then(|r| r.split(',').next()).unwrap().parse().unwrap();
    let mut answers = Vec::new();
    for _ in 0..25 {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(b"GET /v1/inbox HTTP/1.1\r\nHost: board.test\r\nConnection: close\r\n\r\n").unwrap();
        let mut got = String::new();
        stream.read_to_string(&mut got).unwrap();
        answers.push(got.lines().filter(|l| !l.to_ascii_lowercase().starts_with("date:")).collect::<Vec<_>>().join("\n"));
    }
    assert!(answers.iter().all(|a| a == &answers[0] && a.starts_with("HTTP/1.1 401")));
    drop(server);
    let lines: Vec<String> = log.lines().map_while(Result::ok).collect();
    assert_eq!(lines.iter().filter(|l| l.starts_with("refused: missing_header GET")).count(), 4, "{lines:?}");
    let said: Vec<&String> = lines.iter().filter(|l| l.contains("failed or were enroll attempts within")).collect();
    assert_eq!(said.len(), 1, "{lines:?}");
    assert!(said[0].starts_with("refused: 4 requests from the source address 127.0.0.1 failed or were enroll attempts within 60 s"), "{}", said[0]);
    assert_eq!(lines.len(), 5, "twenty-one refusals were not logged: {lines:?}");
}

/// With the table of source addresses full, the count that fills is the shared window's. The log then says so and
/// names no address: the address whose request happened to fill it has a count of one.
#[test]
fn a_full_table_of_sources_logs_the_shared_window_and_names_no_address() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_p2paecn-board"));
    child.env("P2PAECN_HOST", HOST).env("P2PAECN_BIND", "127.0.0.1:0").env("P2PAECN_LIMIT_UNSIGNED", "3").env("P2PAECN_LIMIT_WINDOW_SECONDS", "3600")
        .env("P2PAECN_SOURCE_HEADER", "x-forwarded-for");
    for name in ["P2PAECN_ADMIN_SOCKET", "P2PAECN_CONFIG", "P2PAECN_STATE", "P2PAECN_LIMIT_KEY_REQUESTS"] {
        child.env_remove(name);
    }
    let mut server = Server(child.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap());
    let mut log = BufReader::new(server.0.stderr.take().unwrap());
    let mut started = String::new();
    log.read_line(&mut started).unwrap();
    let addr: std::net::SocketAddr = started.split("listening on ").nth(1).and_then(|r| r.split(',').next()).unwrap().parse().unwrap();
    // The log is read while the requests go out, so that the board never waits on a full pipe.
    let said = std::thread::spawn(move || log.lines().map_while(Result::ok).filter(|l| l.contains("failed or were enroll attempts")).collect::<Vec<String>>());

    // One connection, one failed request per made-up source address, until the table is full, then four more.
    let stream = TcpStream::connect(addr).unwrap();
    let mut answers = BufReader::new(stream.try_clone().unwrap());
    let mut ask = |source: &str| {
        (&stream).write_all(format!("GET /v1/inbox HTTP/1.1\r\nHost: board.test\r\nX-Forwarded-For: {source}\r\n\r\n").as_bytes()).unwrap();
        let mut head = String::new();
        while !head.ends_with("\r\n\r\n") {
            answers.read_line(&mut head).unwrap();
        }
        let mut body = [0u8; 13];
        answers.read_exact(&mut body).unwrap();
        assert!(head.starts_with("HTTP/1.1 401") && &body == b"unauthorized\n", "{head}");
    };
    for n in 0..p2paecn_board::rate::MAX_WINDOWS {
        ask(&format!("198.18.{}.{}", n / 256, n % 256));
    }
    for source in ["192.0.2.1", "192.0.2.2", "192.0.2.3", "192.0.2.4"] {
        ask(source);
    }
    drop(answers);
    drop(server);
    let lines = said.join().unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("refused: 3 requests counted in the shared window failed or were enroll attempts within 3600 s (the table of source addresses is full)"), "{}", lines[0]);
    assert!(!lines[0].contains("192.0.2.") && !lines[0].contains("198.18."), "an address is named: {}", lines[0]);
}
