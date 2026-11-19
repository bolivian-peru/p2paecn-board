//! The per-room record (`GET /v1/rooms/{room}/chain`, src/chain.rs): what is appended and when, who may read it
//! and what they get, paging, and that a replayed or refused write appends nothing. The format itself is checked
//! against the vectors in tests/chain_vectors.rs. Every example finding is invented.

mod common;

use axum::http::StatusCode;
use common::*;
use p2paecn_board::chain::{self, Payload};
use serde_json::{json, Value};
use uuid::Uuid;

struct Board {
    w: World,
    supplier: Agent,
    reseller: Agent,
    /// reseller-x, class fields
    reseller_fields: Agent,
    /// reseller-x, class viewer
    reseller_viewer: Agent,
    /// member-3 of the hub; in no pair room with the others
    third: Agent,
    /// In no room of the others.
    outsider: Agent,
}

const HUB_ORGS: [&str; 3] = ["supplier-a", "reseller-x", "reseller-y"];
const HUB_KEYS: [&str; 5] = ["supplier-a/agent", "reseller-x/agent", "reseller-x/loop", "reseller-x/browser", "reseller-y/agent"];
const TITLE: &str = "List endpoint skips one item when paging";
const TEXT: &str = "Page two starts at item 22.\nItem 21 is never returned. \u{1F600}";

fn board() -> Board {
    let supplier = agent("supplier-a/agent", 71);
    let reseller = agent("reseller-x/agent", 72);
    let reseller_fields = agent("reseller-x/loop", 73);
    let reseller_viewer = agent("reseller-x/browser", 74);
    let third = agent("reseller-y/agent", 75);
    let outsider = agent("reseller-z/agent", 76);
    let w = world(json!({
        "host": HOST,
        "rooms": [
            { "id": "a-x", "orgs": ["supplier-a", "reseller-x"] },
            { "id": "hub", "kind": "shared", "members": [
                { "org": "supplier-a", "handle": "member-1" },
                { "org": "reseller-x", "handle": "member-2" },
                { "org": "reseller-y", "handle": "member-3" }
            ] },
            { "id": "b-z", "orgs": ["supplier-b", "reseller-z"] }
        ],
        "keys": [
            key_entry(&supplier, "text", FAR_FUTURE),
            key_entry(&reseller, "text", FAR_FUTURE),
            key_entry(&reseller_fields, "fields", FAR_FUTURE),
            key_entry(&reseller_viewer, "viewer", FAR_FUTURE),
            key_entry(&third, "text", FAR_FUTURE),
            key_entry(&outsider, "text", FAR_FUTURE),
        ]
    }));
    Board { w, supplier, reseller, reseller_fields, reseller_viewer, third, outsider }
}

async fn finding(w: &World, a: &Agent, room: &str) -> String {
    let body = json!({ "kind": "finding", "severity": "wrong", "title": TITLE, "text": TEXT });
    let (status, body) = w.post(a, &format!("/v1/rooms/{room}/cards"), body).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["card"]["id"].as_str().unwrap().to_owned()
}

async fn chain_of(w: &World, a: &Agent, room: &str) -> Value {
    let (status, body) = w.get(a, &format!("/v1/rooms/{room}/chain?limit=1000")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn head_of(w: &World, a: &Agent, room: &str) -> Value {
    chain_of(w, a, room).await["head"].clone()
}

/// (kind, actor) of each entry.
fn shape(chain: &Value) -> Vec<(String, String)> {
    chain["entries"].as_array().unwrap().iter().map(|e| (e["kind"].as_str().unwrap().to_owned(), e["actor"].as_str().unwrap().to_owned())).collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, a)| (k.to_string(), a.to_string())).collect()
}

/// Every card of a room with its thread, as `a` reads them.
async fn details(w: &World, a: &Agent, room: &str) -> Vec<Value> {
    let (_, list) = w.get(a, &format!("/v1/rooms/{room}/cards?limit=100")).await;
    let mut out = Vec::new();
    for card in list["cards"].as_array().unwrap() {
        out.push(w.get(a, &format!("/v1/cards/{}", card["id"].as_str().unwrap())).await.1);
    }
    out
}

#[tokio::test]
async fn a_pair_room_records_each_event_once_under_organisation_ids() {
    let b = board();
    let w = &b.w;
    let fresh = chain_of(w, &b.reseller, "a-x").await;
    assert_eq!(shape(&fresh), pairs(&[("member-added", "*operator"), ("member-added", "*operator")]));
    assert_eq!((fresh["room"].clone(), fresh["format"].clone()), (json!("a-x"), json!("p2paecn-chain-1")));
    for e in fresh["entries"].as_array().unwrap() {
        assert_eq!((e["time"].clone(), e["card"].clone()), (json!(0), Value::Null), "a membership entry has no time and no card");
    }
    assert_eq!(fresh["entries"][0]["payload"], json!(Payload::MemberAdded { name: "supplier-a" }.hash()));
    assert_eq!(fresh["entries"][0]["prev"], json!(chain::GENESIS));

    let card = finding(w, &b.reseller, "a-x").await;
    let (_, reply) = w.post(&b.supplier, &format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "Looking at it." })).await;
    let reply = reply["messageId"].as_str().unwrap().to_owned();
    let correction = json!({ "kind": "correction", "replaces": reply, "text": "Looking at it: reproduced." });
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/messages"), correction).await.0, StatusCode::CREATED);
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/stage"), json!({ "stage": "shipped", "text": "Fixed." })).await.0, StatusCode::OK);
    let close = json!({ "stage": "closed", "outcome": "verified", "text": "Works." });
    assert_eq!(w.post(&b.reseller, &format!("/v1/cards/{card}/stage"), close).await.0, StatusCode::OK);
    let notice = json!({ "kind": "notice", "title": "A response field is renamed in the next kit release", "text": "Plan for it." });
    let (_, notice) = w.post(&b.supplier, "/v1/rooms/a-x/cards", notice).await;
    let notice = notice["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(w.post(&b.reseller, &format!("/v1/cards/{notice}/messages"), json!({ "kind": "ack", "text": "Noted." })).await.0, StatusCode::CREATED);

    let record = chain_of(w, &b.reseller, "a-x").await;
    assert_eq!(shape(&record), pairs(&[
        ("member-added", "*operator"), ("member-added", "*operator"),
        ("card", "reseller-x"),
        ("message", "supplier-a"), ("message", "supplier-a"),
        ("message", "supplier-a"), ("stage", "supplier-a"),
        ("message", "reseller-x"), ("stage", "reseller-x"),
        ("card", "supplier-a"), ("message", "reseller-x"),
    ]));
    let entries = record["entries"].as_array().unwrap();
    assert_eq!(record["head"], json!({ "seq": 11, "hash": entries[10]["hash"] }));
    assert_eq!(entries[6]["payload"], json!(Payload::Stage { stage: "shipped", outcome: "" }.hash()));
    assert_eq!(entries[8]["payload"], json!(Payload::Stage { stage: "closed", outcome: "verified" }.hash()));
    assert_eq!(entries[2]["card"], json!(card));
    // The whole record against what the board shows, by the steps of CHAIN.md section 7.
    assert_eq!(record_differs("a-x", &record["entries"], &details(w, &b.reseller, "a-x").await), Vec::<String>::new());
    // The check is not empty: one changed text, as a member would be shown it, no longer matches.
    let mut shown = details(w, &b.reseller, "a-x").await;
    shown[0]["messages"][1]["text"] = json!("Noted, and more.");
    assert!(!record_differs("a-x", &record["entries"], &shown).is_empty());

    // The room list, whoami and the operator's state name the same head.
    let head = record["head"].clone();
    assert_eq!(w.get(&b.reseller, "/v1/rooms").await.1["rooms"][0]["chain"], head);
    assert_eq!(w.get(&b.supplier, "/v1/whoami").await.1["rooms"][0]["chain"], head);
    assert_eq!(w.op("GET", "/state", Value::Null).await.1["rooms"][0]["chain"], head);
    // No key id anywhere in it.
    let body = record.to_string();
    for key in HUB_KEYS {
        assert!(!body.contains(key), "{key}");
    }
}

#[tokio::test]
async fn a_shared_room_records_handles_only_and_every_member_and_key_class_gets_the_same_bytes() {
    let b = board();
    let w = &b.w;
    let card = finding(w, &b.reseller, "hub").await;
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "The docs are being fixed." })).await.0, StatusCode::CREATED);
    let close = json!({ "stage": "closed", "outcome": "verified", "text": "Answered." });
    assert_eq!(w.post(&b.reseller, &format!("/v1/cards/{card}/stage"), close).await.0, StatusCode::OK);

    let record = chain_of(w, &b.supplier, "hub").await;
    assert_eq!(shape(&record), pairs(&[
        ("member-added", "*operator"), ("member-added", "*operator"), ("member-added", "*operator"),
        ("card", "member-2"), ("message", "member-1"), ("message", "member-2"), ("stage", "member-2"),
    ]));
    assert_eq!(record["entries"][1]["payload"], json!(Payload::MemberAdded { name: "member-2" }.hash()));
    assert_eq!(record_differs("hub", &record["entries"], &details(w, &b.supplier, "hub").await), Vec::<String>::new());

    // Caller independence: two other members, a fields key and a viewer key get the same body, byte for byte.
    let path = "/v1/rooms/hub/chain?limit=1000";
    let first = raw(&w.app, signed(&b.supplier, "GET", path, "", None)).await;
    assert_eq!(first.status, StatusCode::OK);
    for a in [&b.reseller, &b.third, &b.reseller_fields, &b.reseller_viewer] {
        let other = raw(&w.app, signed(a, "GET", path, "", None)).await;
        assert_eq!((other.status, &other.body), (StatusCode::OK, &first.body), "{}", a.0);
    }
    // No organisation id, no key id, and no title or text, in the record or in the room list.
    let body = String::from_utf8(first.body).unwrap();
    for a in [&b.supplier, &b.reseller, &b.third, &b.reseller_fields, &b.reseller_viewer] {
        let rooms = w.get(a, "/v1/rooms").await.1;
        let hub = rooms["rooms"].as_array().unwrap().iter().find(|r| r["id"] == json!("hub")).unwrap().to_string();
        for secret in HUB_ORGS.iter().chain(&HUB_KEYS) {
            assert!(!body.contains(secret) && !hub.contains(secret), "{secret} for {}", a.0);
        }
    }
    for text in ["List endpoint", "skips", "Page two", "Item 21", "docs are being", "Answered"] {
        assert!(!body.contains(text), "{text}");
    }
    // What a fields key gets is hashes, names the room shows, ids, kinds and times: nothing else.
    let fields = chain_of(w, &b.reseller_fields, "hub").await;
    for e in fields["entries"].as_array().unwrap() {
        let mut names: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["actor", "card", "hash", "kind", "payload", "prev", "seq", "time"]);
        for hash in ["payload", "prev", "hash"] {
            let h = e[hash].as_str().unwrap();
            assert!(h.len() == 64 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)), "{hash}: {h}");
        }
    }
    // And the same key still gets no text on the card itself.
    let (_, shown) = w.get(&b.reseller_fields, &format!("/v1/cards/{card}")).await;
    assert_eq!(shown["messages"][1]["text"], Value::Null);
}

#[tokio::test]
async fn membership_changes_are_recorded_without_a_time_and_a_removal_records_the_findings_it_closes() {
    let b = board();
    let w = &b.w;
    let open = finding(w, &b.third, "hub").await;
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "reseller-y" })).await.0, StatusCode::OK);
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "hub", "org": "reseller-z", "handle": "member-4" })).await.0, StatusCode::CREATED);
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "hub", "org": "reseller-y", "handle": "member-3" })).await.0, StatusCode::CREATED);
    // Refused operator calls append nothing.
    let before = head_of(w, &b.supplier, "hub").await;
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "hub", "org": "reseller-z", "handle": "member-4" })).await.0, StatusCode::CONFLICT);
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "hub", "org": "supplier-b", "handle": "member-1" })).await.0, StatusCode::CONFLICT);
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "supplier-b" })).await.0, StatusCode::NOT_FOUND);
    assert_eq!(head_of(w, &b.supplier, "hub").await, before);

    let record = chain_of(w, &b.supplier, "hub").await;
    assert_eq!(shape(&record)[3..], pairs(&[
        ("card", "member-3"), ("member-removed", "*operator"), ("stage", "*operator"), ("member-added", "*operator"), ("member-added", "*operator"),
    ]));
    let entries = record["entries"].as_array().unwrap();
    assert_eq!(entries[4]["payload"], json!(Payload::MemberRemoved { name: "member-3" }.hash()));
    assert_eq!((entries[5]["card"].clone(), entries[5]["payload"].clone()), (json!(open), json!(Payload::Stage { stage: "closed", outcome: "withdrawn" }.hash())));
    assert_eq!(entries[6]["payload"], json!(Payload::MemberAdded { name: "member-4" }.hash()));
    for e in entries.iter().filter(|e| e["kind"].as_str().unwrap().starts_with("member-")) {
        assert_eq!(e["time"], json!(0));
    }
    assert_ne!(entries[5]["time"], json!(0), "the closing of a finding keeps its time");
    assert_eq!(record_differs("hub", &record["entries"], &details(w, &b.supplier, "hub").await), Vec::<String>::new());
    let body = record.to_string();
    for org in ["reseller-y", "reseller-z", "supplier-b"] {
        assert!(!body.contains(org), "{org}");
    }
    // The new member reads the whole record, the entries from before it joined included.
    assert_eq!(chain_of(w, &b.outsider, "hub").await, record);
    // A room made by the operator at run time starts with its members too.
    assert_eq!(w.op("POST", "/rooms", json!({ "id": "x-z", "kind": "pair", "members": [{ "org": "reseller-x" }, { "org": "reseller-z" }] })).await.0, StatusCode::CREATED);
    assert_eq!(shape(&chain_of(w, &b.outsider, "x-z").await), pairs(&[("member-added", "*operator"), ("member-added", "*operator")]));
}

#[tokio::test]
async fn a_non_member_gets_the_answer_of_a_room_that_does_not_exist_and_no_signature_gets_the_401() {
    let b = board();
    let w = &b.w;
    finding(w, &b.reseller, "a-x").await;
    let unauthorized = w.unauthorized().await;
    let missing = raw(&w.app, signed(&b.outsider, "GET", "/v1/rooms/no-such-room/chain", "", None)).await;
    assert_eq!((missing.status, missing.body.as_slice()), (StatusCode::NOT_FOUND, br#"{"error":"not_found"}"#.as_slice()));
    // Membership is asked before the query is read: a bad query on a room the caller is not in is still the 404.
    for path in ["/v1/rooms/a-x/chain", "/v1/rooms/a-x/chain?after=1", "/v1/rooms/a-x/chain?limit=0", "/v1/rooms/a-x/chain?nonsense", "/v1/rooms/hub/chain"] {
        assert_eq!(raw(&w.app, signed(&b.outsider, "GET", path, "", None)).await, missing, "{path}");
    }
    // A removed member is a non-member.
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "reseller-y" })).await.0, StatusCode::OK);
    assert_eq!(raw(&w.app, signed(&b.third, "GET", "/v1/rooms/hub/chain", "", None)).await, missing);

    // Unsigned, badly signed, another method, HEAD: the identical 401, for a room that exists and one that does not.
    for room in ["a-x", "no-such-room"] {
        let path = format!("/v1/rooms/{room}/chain");
        for method in ["GET", "HEAD", "POST", "PUT", "DELETE"] {
            let got = raw(&w.app, plain(method, &path, "")).await;
            // An answer to HEAD has no body by the rules of HTTP: its status and headers are the 401's.
            let body = if method == "HEAD" { &b""[..] } else { &unauthorized.body[..] };
            assert_eq!((got.status, &got.headers, &got.body[..]), (unauthorized.status, &unauthorized.headers, body), "{method} {path}");
        }
        let stranger = agent("reseller-x/agent", 99); // the right key id, another key
        assert_eq!(raw(&w.app, signed(&stranger, "GET", &path, "", None)).await, unauthorized);
        let head = raw(&w.app, signed(&b.reseller, "HEAD", &path, "", None)).await;
        assert_eq!((head.status, &head.headers, head.body.len()), (unauthorized.status, &unauthorized.headers, 0));
        let idem = Uuid::new_v4().to_string();
        assert_eq!(raw(&w.app, signed(&b.reseller, "POST", &path, "{}", Some(&idem))).await, unauthorized);
    }
    // A pending key reads no record.
    let code = w.enroll_code("reseller-x", "text").await;
    let pending = agent("reseller-x/second", 77);
    assert_eq!(w.enroll(&code, "second", &pending.1).await.status, StatusCode::CREATED);
    assert_eq!(raw(&w.app, signed(&pending, "GET", "/v1/rooms/a-x/chain", "", None)).await, unauthorized);
}

#[tokio::test]
async fn the_record_is_read_a_page_at_a_time() {
    let b = board();
    let w = &b.w;
    let card = finding(w, &b.reseller, "a-x").await;
    for n in 0..7 {
        let reply = json!({ "kind": "reply", "text": format!("Step {n} checked.") });
        assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/messages"), reply).await.0, StatusCode::CREATED);
    }
    let all = chain_of(w, &b.reseller, "a-x").await;
    let entries = all["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 10);
    let seqs = |v: &Value| -> Vec<u64> { v["entries"].as_array().unwrap().iter().map(|e| e["seq"].as_u64().unwrap()).collect() };

    let (_, default) = w.get(&b.reseller, "/v1/rooms/a-x/chain").await;
    assert_eq!(default, all, "ten entries fit the default page");
    let (_, page) = w.get(&b.reseller, "/v1/rooms/a-x/chain?limit=4").await;
    assert_eq!((seqs(&page), page["head"].clone()), (vec![1, 2, 3, 4], all["head"].clone()));
    let (_, page) = w.get(&b.reseller, "/v1/rooms/a-x/chain?after=4&limit=4").await;
    assert_eq!(seqs(&page), vec![5, 6, 7, 8]);
    assert_eq!(page["entries"][0], entries[4]);
    let (_, page) = w.get(&b.reseller, "/v1/rooms/a-x/chain?limit=4&after=8").await;
    assert_eq!(seqs(&page), vec![9, 10]);
    for after in ["10", "11", "18446744073709551615"] {
        let (status, page) = w.get(&b.reseller, &format!("/v1/rooms/a-x/chain?after={after}")).await;
        assert_eq!((status, seqs(&page), page["head"].clone()), (StatusCode::OK, vec![], all["head"].clone()), "after={after}");
    }
    // Pages put together are the whole record.
    let mut joined = Vec::new();
    while joined.len() < 10 {
        let (_, page) = w.get(&b.reseller, &format!("/v1/rooms/a-x/chain?after={}&limit=3", joined.len())).await;
        joined.extend(page["entries"].as_array().unwrap().iter().cloned());
    }
    assert_eq!(&joined, entries);

    let bad = (StatusCode::BAD_REQUEST, json!({ "error": "bad_query" }));
    for query in ["limit=0", "limit=1001", "limit=x", "limit=", "after=-1", "after=x", "after=", "after=18446744073709551616", "after=1&after=2", "cursor=1", "after", "limit=5&stage=closed"] {
        assert_eq!(w.get(&b.reseller, &format!("/v1/rooms/a-x/chain?{query}")).await, bad, "{query}");
    }
    assert_eq!(w.get(&b.reseller, "/v1/rooms/a-x/chain?limit=1000").await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_replayed_or_refused_write_appends_nothing() {
    let b = board();
    let w = &b.w;
    let path = "/v1/rooms/a-x/cards";
    let body = json!({ "kind": "finding", "severity": "wrong", "title": TITLE, "text": TEXT }).to_string();
    let idem = Uuid::new_v4().to_string();
    let (status, first) = send(&w.app, signed(&b.reseller, "POST", path, &body, Some(&idem))).await;
    assert_eq!(status, StatusCode::CREATED);
    let card = serde_json::from_str::<Value>(&first).unwrap()["card"]["id"].as_str().unwrap().to_owned();
    let head = head_of(w, &b.reseller, "a-x").await;
    assert_eq!(head["seq"], json!(3));

    // The same write again, signed afresh with the same idempotency key: the first answer, and no new entry.
    for _ in 0..3 {
        let (status, again) = send(&w.app, signed(&b.reseller, "POST", path, &body, Some(&idem))).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(serde_json::from_str::<Value>(&again).unwrap()["replayed"], json!(true));
    }
    assert_eq!(head_of(w, &b.reseller, "a-x").await, head);
    // The same idempotency key with another body is a conflict, and appends nothing.
    let other = json!({ "kind": "finding", "severity": "wrong", "title": TITLE, "text": "Changed." }).to_string();
    assert_eq!(send(&w.app, signed(&b.reseller, "POST", path, &other, Some(&idem))).await.0, StatusCode::CONFLICT);
    assert_eq!(head_of(w, &b.reseller, "a-x").await, head);

    // A reply and a stage move, each sent twice under one key: one entry for the reply, two for the move.
    let reply = json!({ "kind": "reply", "text": "Looking at it." }).to_string();
    let idem = Uuid::new_v4().to_string();
    for _ in 0..2 {
        assert_eq!(send(&w.app, signed(&b.supplier, "POST", &format!("/v1/cards/{card}/messages"), &reply, Some(&idem))).await.0, StatusCode::CREATED);
    }
    assert_eq!(head_of(w, &b.reseller, "a-x").await["seq"], json!(4));
    let moved = json!({ "stage": "acknowledged", "text": "Confirmed here too." }).to_string();
    let idem = Uuid::new_v4().to_string();
    for _ in 0..2 {
        assert_eq!(send(&w.app, signed(&b.supplier, "POST", &format!("/v1/cards/{card}/stage"), &moved, Some(&idem))).await.0, StatusCode::OK);
    }
    let head = head_of(w, &b.reseller, "a-x").await;
    assert_eq!(head["seq"], json!(6));

    // Refusals of every kind leave the head where it was.
    let refused = [
        (&b.reseller, path.to_owned(), json!({ "kind": "finding", "severity": "wrong", "title": TITLE, "text": "Call 203.0.113.7 for details." }), StatusCode::UNPROCESSABLE_ENTITY),
        (&b.reseller, path.to_owned(), json!({ "kind": "finding", "severity": "nope", "title": TITLE, "text": TEXT }), StatusCode::UNPROCESSABLE_ENTITY),
        (&b.reseller, format!("/v1/cards/{card}/stage"), json!({ "stage": "shipped", "text": "Our own card." }), StatusCode::FORBIDDEN),
        (&b.reseller, format!("/v1/cards/{card}/stage"), json!({ "stage": "closed", "outcome": "verified", "text": "Too early." }), StatusCode::CONFLICT),
        (&b.reseller, format!("/v1/cards/{card}/messages"), json!({ "kind": "ack", "text": "Not a notice." }), StatusCode::FORBIDDEN),
        (&b.supplier, format!("/v1/cards/{card}/messages"), json!({ "kind": "correction", "replaces": Uuid::new_v4(), "text": "Nothing to replace." }), StatusCode::UNPROCESSABLE_ENTITY),
        (&b.outsider, format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "Not our room." }), StatusCode::NOT_FOUND),
        (&b.reseller_viewer, format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "A viewer cannot write." }), StatusCode::FORBIDDEN),
    ];
    for (a, path, body, expected) in refused {
        let (status, answer) = w.post(a, &path, body).await;
        assert_eq!(status, expected, "{path}: {answer}");
        assert_eq!(head_of(w, &b.reseller, "a-x").await, head, "{path}: {answer}");
    }
    // A key revocation is not a room event.
    assert_eq!(w.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": "reseller-x/loop" })).await.0, StatusCode::OK);
    assert_eq!(head_of(w, &b.reseller, "a-x").await, head);
    // A write in one room does not move another room's head.
    let hub = head_of(w, &b.reseller, "hub").await;
    finding(w, &b.reseller, "a-x").await;
    assert_eq!(head_of(w, &b.reseller, "hub").await, hub);
    assert_eq!(head_of(w, &b.reseller, "a-x").await["seq"], json!(7));
}
