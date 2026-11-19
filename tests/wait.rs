//! The long poll `GET /v1/inbox/wait`: an answer at once when there is news, a held request woken by another
//! member's post, the cap, at most two held requests per key, the immediate identical 401 without a signature,
//! and that a cursor and a held request tell nothing about rooms the caller is not in. Every finding is invented.

mod common;

use axum::http::StatusCode;
use common::*;
use p2paecn_board::rate::Config;
use p2paecn_board::store::KeyStatus;
use p2paecn_board::{api, operator, Shared};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Board {
    w: World,
    supplier: Agent,
    reseller: Agent,
    reseller_fields: Agent,
    reseller_viewer: Agent,
    /// In `b-z` only.
    outsider: Agent,
    other_supplier: Agent,
}

/// A board whose long poll holds for at most `cap` seconds.
fn board_with(cap: u64, limits: Config) -> Board {
    let supplier = agent("supplier-a/agent", 91);
    let reseller = agent("reseller-x/agent", 92);
    let reseller_fields = agent("reseller-x/loop", 93);
    let reseller_viewer = agent("reseller-x/browser", 94);
    let outsider = agent("reseller-z/agent", 95);
    let other_supplier = agent("supplier-b/agent", 96);
    let config = json!({
        "host": HOST,
        "rooms": [
            { "id": "a-x", "orgs": ["supplier-a", "reseller-x"] },
            { "id": "hub", "kind": "shared", "members": [{ "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-x", "handle": "member-2" }] },
            { "id": "b-z", "orgs": ["supplier-b", "reseller-z"] }
        ],
        "keys": [
            key_entry(&supplier, "text", FAR_FUTURE), key_entry(&reseller, "text", FAR_FUTURE), key_entry(&reseller_fields, "fields", FAR_FUTURE),
            key_entry(&reseller_viewer, "viewer", FAR_FUTURE), key_entry(&outsider, "text", FAR_FUTURE), key_entry(&other_supplier, "text", FAR_FUTURE)
        ]
    });
    let mut shared = Shared::from_config_json(&config.to_string(), None).unwrap();
    shared.wait_cap = cap;
    shared.limits = p2paecn_board::rate::Limiter::new(limits);
    let shared = Arc::new(shared);
    let w = World { app: api::router(shared.clone()), ops: operator::router(shared.clone()), shared };
    Board { w, supplier, reseller, reseller_fields, reseller_viewer, outsider, other_supplier }
}

fn board(cap: u64) -> Board {
    board_with(cap, Config::off())
}

fn notice(title: &str) -> Value {
    json!({ "kind": "notice", "title": title, "text": "Plan for it. The word marigold is in this text." })
}

/// The cursor a caller starts from.
async fn cursor(w: &World, a: &Agent) -> String {
    let (status, body) = w.get(a, "/v1/inbox/wait").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["events"].clone(), body["timedOut"].clone()), (json!([]), json!(false)));
    body["cursor"].as_str().unwrap().to_owned()
}

/// Starts a wait in the background. Resolves with its answer and how long it took.
fn waiting(w: &World, a: &Agent, path: String) -> tokio::task::JoinHandle<(StatusCode, Value, Duration)> {
    let (app, request) = (w.app.clone(), signed(a, "GET", &path, "", None));
    tokio::spawn(async move {
        let started = Instant::now();
        let (status, body) = send(&app, request).await;
        (status, serde_json::from_str(&body).unwrap_or(Value::String(body)), started.elapsed())
    })
}

async fn a_moment() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn news_after_the_cursor_is_answered_at_once_and_own_entries_are_not_news() {
    let b = board(20);
    let w = &b.w;
    let start = cursor(w, &b.reseller).await;
    assert_eq!(start, "a-x.2_hub.2", "the caller's own rooms and where their records stand, nothing else");
    assert_eq!(cursor(w, &b.outsider).await, "b-z.2");

    let (_, card) = w.post(&b.supplier, "/v1/rooms/a-x/cards", notice("Maintenance window on the first weekend")).await;
    let card = card["card"]["id"].clone();
    let started = Instant::now();
    let (status, got) = w.get(&b.reseller, &format!("/v1/inbox/wait?after={start}")).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["events"].as_array().unwrap().len(), 1);
    let event = &got["events"][0];
    assert_eq!((event["room"].clone(), event["seq"].clone(), event["kind"].clone(), event["card"].clone(), event["actor"].clone()),
               (json!("a-x"), json!(3), json!("card"), card, json!("supplier-a")));
    let mut names: Vec<&str> = event.as_object().unwrap().keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["actor", "card", "kind", "room", "seq", "time"], "no hash and no text");
    assert_eq!((got["cursor"].clone(), got["timedOut"].clone(), got["more"].clone()), (json!("a-x.3_hub.2"), json!(false), json!(false)));
    assert!(!got.to_string().contains("marigold") && !got.to_string().contains("Maintenance"));

    // Every key class of the organisation gets the same events: a fields key and a viewer key too.
    for a in [&b.reseller_fields, &b.reseller_viewer] {
        assert_eq!(w.get(a, &format!("/v1/inbox/wait?after={start}")).await.1["events"], got["events"]);
    }
    // For the organisation that posted it, the card is not news: the cursor moves past it with no event.
    let (_, own) = w.get(&b.supplier, &format!("/v1/inbox/wait?after={start}&wait=0")).await;
    assert_eq!((own["events"].clone(), own["cursor"].clone(), own["timedOut"].clone()), (json!([]), json!("a-x.3_hub.2"), json!(false)));
    // In a shared room the actor is a handle.
    w.post(&b.supplier, "/v1/rooms/hub/cards", notice("A notice for the shared room")).await;
    let (_, shared) = w.get(&b.reseller, "/v1/inbox/wait?after=a-x.3_hub.2").await;
    assert_eq!((shared["events"][0]["room"].clone(), shared["events"][0]["actor"].clone()), (json!("hub"), json!("member-1")));
    assert!(!shared.to_string().contains("supplier-a"));
    // One room only: the cursor then names that room alone.
    let (_, one) = w.get(&b.reseller, "/v1/inbox/wait?after=a-x.2_hub.2&room=hub").await;
    assert_eq!((one["events"].as_array().unwrap().len(), one["cursor"].clone()), (1, json!("hub.3")));
    // A room the cursor does not name is new to the caller: its whole record is news.
    let (_, fresh) = w.get(&b.reseller, "/v1/inbox/wait?after=-&room=a-x").await;
    assert_eq!(fresh["events"].as_array().unwrap().len(), 3, "two members added and the card");
    // A cursor ahead of the record starts again at the head.
    let (_, ahead) = w.get(&b.reseller, "/v1/inbox/wait?after=a-x.99_hub.99&wait=0").await;
    assert_eq!((ahead["events"].clone(), ahead["cursor"].clone()), (json!([]), json!("a-x.3_hub.3")));
}

#[tokio::test]
async fn a_held_request_is_woken_by_another_members_post_and_not_by_anything_else() {
    let b = board(3);
    let w = &b.w;
    let start = cursor(w, &b.reseller).await;
    let held = waiting(w, &b.reseller, format!("/v1/inbox/wait?after={start}"));
    a_moment().await;
    assert!(!held.is_finished(), "nothing new: the request is held");
    // A post in a room the caller is not in, the caller's own post, and an operator action outside rooms do not
    // answer it.
    assert_eq!(w.post(&b.outsider, "/v1/rooms/b-z/cards", notice("Nothing to do with the others")).await.0, StatusCode::CREATED);
    assert_eq!(w.post(&b.reseller, "/v1/rooms/a-x/cards", notice("Our own notice")).await.0, StatusCode::CREATED);
    assert_eq!(w.op("POST", "/orgs", json!({ "id": "reseller-q" })).await.0, StatusCode::CREATED);
    a_moment().await;
    assert!(!held.is_finished());
    let (_, reply_to) = w.get(&b.reseller, "/v1/rooms/a-x/cards").await;
    let card = reply_to["cards"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/messages"), json!({ "kind": "ack", "text": "Noted." })).await.0, StatusCode::CREATED);
    let (status, got, took) = held.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(took < Duration::from_millis(2500), "woken, not timed out: {took:?}");
    assert_eq!(got["events"], json!([{ "room": "a-x", "seq": 4, "kind": "message", "card": card, "actor": "supplier-a", "time": got["events"][0]["time"] }]));
    assert_eq!((got["cursor"].clone(), got["timedOut"].clone()), (json!("a-x.4_hub.2"), json!(false)));

    // A stage move wakes with both of its entries, and a membership change by the operator wakes too.
    let (_, finding) = w.post(&b.reseller, "/v1/rooms/a-x/cards", json!({ "kind": "finding", "severity": "wrong", "title": "List endpoint skips one item", "text": "Page two starts late." })).await;
    let finding = finding["card"]["id"].as_str().unwrap().to_owned();
    let start = cursor(w, &b.reseller).await;
    let held = waiting(w, &b.reseller, format!("/v1/inbox/wait?after={start}"));
    a_moment().await;
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{finding}/stage"), json!({ "stage": "acknowledged", "text": "Confirmed." })).await.0, StatusCode::OK);
    let (_, got, _) = held.await.unwrap();
    let kinds: Vec<&str> = got["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["message", "stage"]);
    let held = waiting(w, &b.reseller, format!("/v1/inbox/wait?after={}&room=hub", got["cursor"].as_str().unwrap()));
    a_moment().await;
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "hub", "org": "reseller-z", "handle": "member-3" })).await.0, StatusCode::CREATED);
    let (_, got, _) = held.await.unwrap();
    assert_eq!(got["events"], json!([{ "room": "hub", "seq": 3, "kind": "member-added", "card": null, "actor": "*operator", "time": 0 }]));
}

#[tokio::test]
async fn at_the_cap_the_answer_is_empty_with_the_cursor_to_send_next() {
    let b = board(1);
    let w = &b.w;
    let start = cursor(w, &b.reseller).await;
    // The caller's own post moves the cursor while the request is held.
    let held = waiting(w, &b.reseller, format!("/v1/inbox/wait?after={start}&wait=3600"));
    a_moment().await;
    assert_eq!(w.post(&b.reseller, "/v1/rooms/a-x/cards", notice("Our own notice")).await.0, StatusCode::CREATED);
    let (status, got, took) = held.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(took >= Duration::from_millis(900) && took < Duration::from_millis(2500), "held for the cap of one second, not for the hour asked: {took:?}");
    assert_eq!(got, json!({ "events": [], "cursor": "a-x.3_hub.2", "more": false, "timedOut": true, "wait": 1 }));
    // `wait=0` is a plain look.
    let started = Instant::now();
    let (_, got) = w.get(&b.reseller, "/v1/inbox/wait?after=a-x.3_hub.2&wait=0").await;
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!((got["events"].clone(), got["timedOut"].clone(), got["wait"].clone()), (json!([]), json!(false), json!(0)));

    let bad = (StatusCode::BAD_REQUEST, json!({ "error": "bad_query" }));
    for query in ["after=a-x", "after=a-x.x", "after=a-x.1_a-x.2", "after=A-X.1", "after=", "after=a-x.1&after=hub.1", "wait=-1", "wait=12345", "wait=x", "room=X", "room=", "limit=5", "wait"] {
        assert_eq!(w.get(&b.reseller, &format!("/v1/inbox/wait?{query}")).await, bad, "{query}");
    }
    // The existing inbox call is what it was.
    let (status, inbox) = w.get(&b.reseller, "/v1/inbox").await;
    assert!(status == StatusCode::OK && inbox.get("events").is_none() && inbox["waitingOnUs"].is_array());
}

#[tokio::test]
async fn a_key_holds_two_requests_and_a_third_is_refused() {
    let b = board(2);
    let w = &b.w;
    let start = cursor(w, &b.reseller).await;
    let path = format!("/v1/inbox/wait?after={start}");
    let (first, second) = (waiting(w, &b.reseller, path.clone()), waiting(w, &b.reseller, path.clone()));
    a_moment().await;
    let started = Instant::now();
    assert_eq!(w.get(&b.reseller, &path).await, (StatusCode::CONFLICT, json!({ "error": "too_many_waits" })));
    assert!(started.elapsed() < Duration::from_millis(500), "refused at once");
    // The limit is per key: another key of the same organisation, and a call that needs no holding, are served.
    let other = waiting(w, &b.reseller_fields, path.clone());
    assert_eq!(w.get(&b.reseller, &format!("{path}&wait=0")).await.0, StatusCode::OK);
    assert_eq!(w.get(&b.reseller, "/v1/inbox/wait").await.0, StatusCode::OK);
    assert_eq!(w.get(&b.reseller, "/v1/inbox").await.0, StatusCode::OK);
    for held in [first, second, other] {
        let (status, got, _) = held.await.unwrap();
        assert_eq!((status, got["timedOut"].clone()), (StatusCode::OK, json!(true)));
    }
    // The places are free again, also after a held request was dropped by its caller.
    assert!(b.w.shared.waits.lock().unwrap().is_empty());
    let dropped = waiting(w, &b.reseller, path.clone());
    a_moment().await;
    dropped.abort();
    let _ = dropped.await;
    assert!(b.w.shared.waits.lock().unwrap().is_empty());
    let (again, also) = (waiting(w, &b.reseller, path.clone()), waiting(w, &b.reseller, path.clone()));
    a_moment().await;
    assert_eq!(b.w.shared.waits.lock().unwrap().get("reseller-x/agent"), Some(&2));
    again.abort();
    also.abort();
}

#[tokio::test]
async fn no_signature_is_the_identical_401_at_once_and_a_room_of_others_is_not_found() {
    let b = board(3);
    let w = &b.w;
    let unauthorized = w.unauthorized().await;
    let started = Instant::now();
    for path in ["/v1/inbox/wait", "/v1/inbox/wait?after=a-x.2_hub.2", "/v1/inbox/wait?after=a-x.2&wait=3&room=a-x", "/v1/inbox/wait?nonsense"] {
        assert_eq!(raw(&w.app, plain("GET", path, "")).await, unauthorized, "{path}");
        let stranger = agent("reseller-x/agent", 99);
        assert_eq!(raw(&w.app, signed(&stranger, "GET", path, "", None)).await, unauthorized, "{path}");
        assert_eq!(raw(&w.app, plain("POST", path, "")).await, unauthorized, "{path}");
    }
    let head = raw(&w.app, signed(&b.reseller, "HEAD", "/v1/inbox/wait", "", None)).await;
    assert_eq!((head.status, &head.headers), (unauthorized.status, &unauthorized.headers));
    assert!(started.elapsed() < Duration::from_secs(1), "none of them was held");
    assert!(b.w.shared.waits.lock().unwrap().is_empty());
    // A pending key is not held either.
    let code = w.enroll_code("reseller-x", "text").await;
    let pending = agent("reseller-x/second", 97);
    assert_eq!(w.enroll(&code, "second", &pending.1).await.status, StatusCode::CREATED);
    assert_eq!(raw(&w.app, signed(&pending, "GET", "/v1/inbox/wait?after=a-x.2", "", None)).await, unauthorized);

    // A room the caller is not in answers like one that does not exist, at once, whatever the cursor says.
    let missing = raw(&w.app, signed(&b.outsider, "GET", "/v1/inbox/wait?room=no-such-room&after=no-such-room.1", "", None)).await;
    assert_eq!((missing.status, missing.body.as_slice()), (StatusCode::NOT_FOUND, br#"{"error":"not_found"}"#.as_slice()));
    let started = Instant::now();
    for path in ["/v1/inbox/wait?room=a-x&after=a-x.2", "/v1/inbox/wait?room=a-x", "/v1/inbox/wait?room=hub&after=hub.1&wait=3"] {
        assert_eq!(raw(&w.app, signed(&b.outsider, "GET", path, "", None)).await, missing, "{path}");
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    // A cursor that names rooms of others gets nothing about them: the names are ignored, and a write in such a
    // room during the wait neither answers the request nor shows in its cursor.
    let held = waiting(w, &b.outsider, "/v1/inbox/wait?after=a-x.1_hub.1_b-z.2&wait=1".to_owned());
    a_moment().await;
    assert_eq!(w.post(&b.supplier, "/v1/rooms/a-x/cards", notice("Not for the outsider")).await.0, StatusCode::CREATED);
    let (status, got, took) = held.await.unwrap();
    assert!(took >= Duration::from_millis(900), "{took:?}");
    assert_eq!((status, got), (StatusCode::OK, json!({ "events": [], "cursor": "b-z.2", "more": false, "timedOut": true, "wait": 1 })));
    // A member removed from the room while it waits on that room gets the 404, not the room's news.
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "hub", "org": "reseller-z", "handle": "member-3" })).await.0, StatusCode::CREATED);
    let held = waiting(w, &b.outsider, "/v1/inbox/wait?room=hub&after=hub.3".to_owned());
    a_moment().await;
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "reseller-z" })).await.0, StatusCode::OK);
    let (status, got, _) = held.await.unwrap();
    assert_eq!((status, got), (StatusCode::NOT_FOUND, json!({ "error": "not_found" })));
    let _ = &b.other_supplier;
}

#[tokio::test]
async fn a_key_revoked_or_expired_while_its_request_is_held_gets_the_401_and_no_data() {
    let b = board(2);
    let w = &b.w;
    let start = cursor(w, &b.reseller).await;
    // Revoked by the operator, then something is posted: the held request must not carry it.
    let held = waiting(w, &b.reseller, format!("/v1/inbox/wait?after={start}"));
    a_moment().await;
    assert_eq!(w.op("POST", "/keys/revoke", json!({ "keyId": "reseller-x/agent" })).await.0, StatusCode::OK);
    assert_eq!(w.post(&b.supplier, "/v1/rooms/a-x/cards", notice("After the revocation")).await.0, StatusCode::CREATED);
    let (status, got, took) = held.await.unwrap();
    assert_eq!((status, got), (StatusCode::UNAUTHORIZED, Value::String("unauthorized\n".into())));
    assert!(took < Duration::from_millis(1500), "answered when the key was revoked: {took:?}");

    // Revoked with no wake-up of its own (set in the record directly); the news that follows wakes the request.
    let start = cursor(w, &b.reseller_fields).await;
    let held = waiting(w, &b.reseller_fields, format!("/v1/inbox/wait?after={start}"));
    a_moment().await;
    w.shared.store.lock().unwrap().keys.get_mut("reseller-x/loop").unwrap().status = KeyStatus::Revoked;
    assert_eq!(w.post(&b.supplier, "/v1/rooms/a-x/cards", notice("For nobody with a revoked key")).await.0, StatusCode::CREATED);
    let (status, got, _) = held.await.unwrap();
    assert_eq!((status, got), (StatusCode::UNAUTHORIZED, Value::String("unauthorized\n".into())));

    // A key that expires while it waits gets the 401 at the cap, not the empty answer.
    let quiet = cursor(w, &b.supplier).await;
    w.shared.store.lock().unwrap().keys.get_mut("supplier-a/agent").unwrap().expires = now() + 1;
    let held = waiting(w, &b.supplier, format!("/v1/inbox/wait?after={quiet}"));
    let (status, got, _) = held.await.unwrap();
    assert_eq!((status, got), (StatusCode::UNAUTHORIZED, Value::String("unauthorized\n".into())));
    assert!(b.w.shared.waits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_wait_counts_as_one_request_against_the_keys_limit_however_long_it_is_held() {
    let b = board_with(1, Config { key_requests: 4, ..Config::off() });
    let w = &b.w;
    let start = cursor(w, &b.reseller).await; // 1
    for _ in 0..2 {
        let (status, got) = w.get(&b.reseller, &format!("/v1/inbox/wait?after={start}")).await; // 2 and 3, each held a second
        assert_eq!((status, got["timedOut"].clone()), (StatusCode::OK, json!(true)));
    }
    assert_eq!(w.get(&b.reseller, "/v1/inbox").await.0, StatusCode::OK); // 4
    let limited = raw(&w.app, signed(&b.reseller, "GET", &format!("/v1/inbox/wait?after={start}"), "", None)).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS, "the fifth request is refused at once, not held");
    assert!(limited.headers.iter().any(|(name, _)| name == "retry-after"));
    assert!(b.w.shared.waits.lock().unwrap().is_empty());
}

/// Through the real listener with its real timeouts: a request held for longer than the 10 seconds a request
/// head may take is not dropped, and an unsigned one on the same path is answered at once.
#[tokio::test(flavor = "multi_thread")]
async fn the_listener_keeps_a_held_request_open_past_its_head_timeout() {
    use std::io::{Read, Write};
    let b = board(12);
    let start = cursor(&b.w, &b.reseller).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(api::serve(listener, b.w.app.clone(), api::HEAD_TIMEOUT, api::REQUEST_TIMEOUT));
    let request = signed(&b.reseller, "GET", &format!("/v1/inbox/wait?after={start}"), "", None);
    let mut bytes = format!("GET {} HTTP/1.1\r\nhost: {HOST}\r\nconnection: close\r\n", request.uri());
    for (name, value) in request.headers() {
        bytes.push_str(&format!("{name}: {}\r\n", value.to_str().unwrap()));
    }
    bytes.push_str("\r\n");
    let (held, unsigned) = tokio::task::spawn_blocking(move || {
        let ask = |bytes: &[u8]| {
            let started = Instant::now();
            let mut stream = std::net::TcpStream::connect(addr).unwrap();
            stream.write_all(bytes).unwrap();
            let mut got = String::new();
            stream.read_to_string(&mut got).unwrap();
            (got, started.elapsed())
        };
        let unsigned = ask(b"GET /v1/inbox/wait?after=a-x.2&wait=12 HTTP/1.1\r\nhost: board.test\r\nconnection: close\r\n\r\n");
        (ask(bytes.as_bytes()), unsigned)
    })
    .await
    .unwrap();
    server.abort();
    assert!(unsigned.0.starts_with("HTTP/1.1 401") && unsigned.0.ends_with("unauthorized\n") && unsigned.1 < Duration::from_secs(1), "{unsigned:?}");
    assert!(held.0.starts_with("HTTP/1.1 200"), "{}", held.0);
    assert!(held.0.contains(r#""timedOut":true"#) && held.0.contains(r#""wait":12"#), "{}", held.0);
    assert!(held.1 >= Duration::from_secs(11) && held.1 < Duration::from_secs(15), "{:?}", held.1);
}

#[tokio::test]
async fn an_answer_holds_at_most_a_hundred_events_and_the_cursor_goes_on_from_there() {
    let b = board(1);
    let w = &b.w;
    let start = cursor(w, &b.reseller).await;
    for n in 0..130 {
        assert_eq!(w.post(&b.supplier, "/v1/rooms/a-x/cards", notice(&format!("Maintenance window number {n}"))).await.0, StatusCode::CREATED);
    }
    let (_, first) = w.get(&b.reseller, &format!("/v1/inbox/wait?after={start}")).await;
    assert_eq!((first["events"].as_array().unwrap().len(), first["more"].clone(), first["cursor"].clone()), (100, json!(true), json!("a-x.102_hub.2")));
    let (_, rest) = w.get(&b.reseller, &format!("/v1/inbox/wait?after={}", first["cursor"].as_str().unwrap())).await;
    assert_eq!((rest["events"].as_array().unwrap().len(), rest["more"].clone(), rest["cursor"].clone()), (30, json!(false), json!("a-x.132_hub.2")));
    assert_eq!(rest["events"][0]["seq"], json!(103));
}
