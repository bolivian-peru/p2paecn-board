//! Claims: a member says it is working on a card, for a stated time (`POST /v1/cards/{id}/claim`). One live claim
//! per card, renewed by claiming again, given up with `release`, gone when its time passes or the card is closed.
//! A claim blocks nobody. Each claim, renewal and release is an entry in the room's record. Every finding is
//! invented.

mod common;

use axum::http::StatusCode;
use common::*;
use p2paecn_board::chain::Payload;
use p2paecn_board::state;
use serde_json::{json, Value};
use uuid::Uuid;

struct Board {
    w: World,
    supplier: Agent,
    reseller: Agent,
    reseller_fields: Agent,
    reseller_viewer: Agent,
    third: Agent,
    third_fields: Agent,
    outsider: Agent,
}

fn board() -> Board {
    let supplier = agent("supplier-a/agent", 101);
    let reseller = agent("reseller-x/agent", 102);
    let reseller_fields = agent("reseller-x/loop", 103);
    let reseller_viewer = agent("reseller-x/browser", 104);
    let third = agent("reseller-y/agent", 105);
    let third_fields = agent("reseller-y/loop", 106);
    let outsider = agent("reseller-z/agent", 107);
    let w = world(json!({
        "host": HOST,
        "rooms": [
            { "id": "a-x", "orgs": ["supplier-a", "reseller-x"] },
            { "id": "hub", "kind": "shared", "members": [
                { "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-x", "handle": "member-2" }, { "org": "reseller-y", "handle": "member-3" }] },
            { "id": "b-z", "orgs": ["supplier-b", "reseller-z"] }
        ],
        "keys": [
            key_entry(&supplier, "text", FAR_FUTURE), key_entry(&reseller, "text", FAR_FUTURE), key_entry(&reseller_fields, "fields", FAR_FUTURE),
            key_entry(&reseller_viewer, "viewer", FAR_FUTURE), key_entry(&third, "text", FAR_FUTURE), key_entry(&third_fields, "fields", FAR_FUTURE),
            key_entry(&outsider, "text", FAR_FUTURE)
        ]
    }));
    Board { w, supplier, reseller, reseller_fields, reseller_viewer, third, third_fields, outsider }
}

async fn finding(w: &World, a: &Agent, room: &str) -> String {
    let body = json!({ "kind": "finding", "severity": "wrong", "title": "List endpoint skips one item when paging", "text": "Page two starts at item 22." });
    let (status, body) = w.post(a, &format!("/v1/rooms/{room}/cards"), body).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["card"]["id"].as_str().unwrap().to_owned()
}

async fn claim_of(w: &World, a: &Agent, card: &str) -> Value {
    w.get(a, &format!("/v1/cards/{card}")).await.1["card"]["claim"].clone()
}

async fn record(w: &World, a: &Agent, room: &str) -> Value {
    w.get(a, &format!("/v1/rooms/{room}/chain?limit=1000")).await.1
}

async fn details(w: &World, a: &Agent, room: &str) -> Vec<Value> {
    let (_, list) = w.get(a, &format!("/v1/rooms/{room}/cards?limit=100")).await;
    let mut out = Vec::new();
    for card in list["cards"].as_array().unwrap() {
        out.push(w.get(a, &format!("/v1/cards/{}", card["id"].as_str().unwrap())).await.1);
    }
    out
}

#[tokio::test]
async fn a_claim_is_shown_renewed_refused_to_others_and_released() {
    let b = board();
    let w = &b.w;
    let card = finding(w, &b.reseller, "a-x").await;
    let path = format!("/v1/cards/{card}/claim");
    assert_eq!(claim_of(w, &b.reseller, &card).await, Value::Null, "no claim: the member is there, and null");

    // The default lease is an hour.
    let before = now();
    let (status, claimed) = w.post(&b.supplier, &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    let claim = &claimed["card"]["claim"];
    let until = claim["until"].as_u64().unwrap();
    assert!((before + 3600..=now() + 3600).contains(&until), "{until}");
    assert_eq!(claim, &json!({ "by": "supplier-a", "until": until, "note": null, "mine": true, "untrusted": false }));
    // The other side sees it on the card, in the list and in the inbox, as written by someone else.
    let theirs = json!({ "by": "supplier-a", "until": until, "note": null, "mine": false, "untrusted": true });
    assert_eq!(claim_of(w, &b.reseller, &card).await, theirs);
    assert_eq!(w.get(&b.reseller, "/v1/rooms/a-x/cards").await.1["cards"][0]["claim"], theirs);
    assert_eq!(w.get(&b.supplier, "/v1/inbox").await.1["waitingOnUs"][0]["claim"]["mine"], json!(true));
    assert_eq!(claim_of(w, &b.reseller_viewer, &card).await, theirs, "a viewer key reads it");

    // One claim per card: the other side is refused and told who holds it and until when.
    let refused = w.post(&b.reseller, &path, json!({ "lease": 600 })).await;
    assert_eq!(refused, (StatusCode::CONFLICT, json!({ "error": "claimed", "claim": { "by": "supplier-a", "until": until } })));
    assert_eq!(w.post(&b.reseller, &path, json!({ "release": true })).await.0, StatusCode::CONFLICT, "nor can it release the other side's claim");
    assert_eq!(claim_of(w, &b.reseller, &card).await, theirs);

    // Claiming again renews: a new time and a new note, by any key of the same organisation.
    let (status, renewed) = w.post(&b.supplier, &path, json!({ "lease": 60, "note": "  Looking at the paging code.  " })).await;
    assert_eq!(status, StatusCode::OK);
    let short = renewed["card"]["claim"]["until"].as_u64().unwrap();
    assert!(short < until && short >= before + 60);
    assert_eq!(renewed["card"]["claim"]["note"], json!("Looking at the paging code."), "trimmed, like every text");
    // The note is text of the claiming organisation: a fields key of another one does not get it.
    assert_eq!(claim_of(w, &b.reseller, &card).await["note"], json!("Looking at the paging code."));
    assert_eq!(claim_of(w, &b.reseller_fields, &card).await, json!({ "by": "supplier-a", "until": short, "note": null, "mine": false, "untrusted": true }));

    // A claim blocks nobody: the other side replies, and the claiming side moves the card, as before.
    assert_eq!(w.post(&b.reseller, &format!("/v1/cards/{card}/messages"), json!({ "kind": "reply", "text": "One more detail." })).await.0, StatusCode::CREATED);
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{card}/stage"), json!({ "stage": "acknowledged", "text": "Confirmed." })).await.0, StatusCode::OK);
    assert_eq!(claim_of(w, &b.reseller, &card).await["until"], json!(short), "the claim stays through both");

    // Release, and then the other side may claim.
    let (status, released) = w.post(&b.supplier, &path, json!({ "release": true })).await;
    assert_eq!((status, released["card"]["claim"].clone()), (StatusCode::OK, Value::Null));
    assert_eq!(w.post(&b.supplier, &path, json!({ "release": true })).await, (StatusCode::CONFLICT, json!({ "error": "not_claimed" })));
    let (status, theirs_now) = w.post(&b.reseller, &path, json!({ "lease": 86400, "note": "Ours for today." })).await;
    assert_eq!((status, theirs_now["card"]["claim"]["by"].clone()), (StatusCode::OK, json!("reseller-x")));

    // The record: claim, renewal, release, claim, each once, under the names the room shows, and it verifies.
    let chain = record(w, &b.reseller, "a-x").await;
    let entries = chain["entries"].as_array().unwrap();
    let claims: Vec<(&str, &str, u64)> = entries
        .iter()
        .filter(|e| e["kind"] == "claim" || e["kind"] == "release")
        .map(|e| (e["kind"].as_str().unwrap(), e["actor"].as_str().unwrap(), e["seq"].as_u64().unwrap()))
        .collect();
    assert_eq!(claims, [("claim", "supplier-a", 4), ("claim", "supplier-a", 5), ("release", "supplier-a", 9), ("claim", "reseller-x", 10)]);
    assert_eq!(entries[3]["payload"], json!(Payload::Claim { until: &until.to_string(), note: "" }.hash()));
    assert_eq!(entries[4]["payload"], json!(Payload::Claim { until: &short.to_string(), note: "Looking at the paging code." }.hash()));
    assert_eq!((entries[8]["payload"].clone(), entries[8]["card"].clone()), (json!(Payload::Release.hash()), json!(card)));
    assert_eq!(record_differs("a-x", &chain["entries"], &details(w, &b.reseller, "a-x").await), Vec::<String>::new());
    assert!(!chain.to_string().contains("paging code") && !chain.to_string().contains("/agent"));
}

#[tokio::test]
async fn refusals_and_replays_change_nothing_and_append_nothing() {
    let b = board();
    let w = &b.w;
    let card = finding(w, &b.reseller, "a-x").await;
    let path = format!("/v1/cards/{card}/claim");
    let head = |chain: Value| chain["head"].clone();
    let start = head(record(w, &b.reseller, "a-x").await);

    let unprocessable = StatusCode::UNPROCESSABLE_ENTITY;
    let refused = [
        (&b.supplier, json!({ "lease": 59 }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "lease": 86401 }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "lease": "1h" }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "lease": -60 }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "note": "" }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "note": 7 }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "note": "x".repeat(201) }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "release": "yes" }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "release": true, "lease": 60 }), unprocessable, "bad_field"),
        (&b.supplier, json!({ "release": true, "note": "Done." }), unprocessable, "bad_field"),
        // The note goes through the content gate like any text.
        (&b.supplier, json!({ "note": "Ask at 203.0.113.7 for the log." }), unprocessable, "ip_address"),
        (&b.supplier, json!({ "note": "Write to someone@board.example about it." }), unprocessable, "email_address"),
        (&b.supplier, json!({ "note": "Hidden\u{202e}text" }), unprocessable, "invisible_character"),
        (&b.supplier, json!({ "release": true }), StatusCode::CONFLICT, "not_claimed"),
        // A viewer key writes nothing, a key outside the room finds nothing.
        (&b.reseller_viewer, json!({}), StatusCode::FORBIDDEN, "read_only_key"),
        (&b.outsider, json!({}), StatusCode::NOT_FOUND, "not_found"),
    ];
    for (a, body, status, code) in refused {
        assert_eq!(w.post(a, &path, body.clone()).await, (status, json!({ "error": code })), "{body}");
    }
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{}/claim", Uuid::new_v4()), json!({})).await, (StatusCode::NOT_FOUND, json!({ "error": "not_found" })));
    assert_eq!(head(record(w, &b.reseller, "a-x").await), start);
    assert_eq!(claim_of(w, &b.reseller, &card).await, Value::Null);

    // A fields key may claim. Sent twice under one idempotency key it is one claim and one entry.
    let body = json!({ "lease": 120, "note": "The loop has it." }).to_string();
    let idem = Uuid::new_v4().to_string();
    let (status, first) = send(&w.app, signed(&b.reseller_fields, "POST", &path, &body, Some(&idem))).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let after_one = head(record(w, &b.reseller, "a-x").await);
    assert_eq!(after_one["seq"], json!(start["seq"].as_u64().unwrap() + 1));
    for _ in 0..3 {
        let (status, again) = send(&w.app, signed(&b.reseller_fields, "POST", &path, &body, Some(&idem))).await;
        let mut expected: Value = serde_json::from_str(&first).unwrap();
        expected["replayed"] = json!(true);
        assert_eq!((status, serde_json::from_str::<Value>(&again).unwrap()), (StatusCode::OK, expected));
    }
    assert_eq!(head(record(w, &b.reseller, "a-x").await), after_one);
    // The same for a release, and for the refusal of another member's claim.
    assert_eq!(w.post(&b.supplier, &path, json!({})).await.0, StatusCode::CONFLICT);
    let idem = Uuid::new_v4().to_string();
    for _ in 0..2 {
        assert_eq!(send(&w.app, signed(&b.reseller, "POST", &path, r#"{"release":true}"#, Some(&idem))).await.0, StatusCode::OK);
    }
    assert_eq!(head(record(w, &b.reseller, "a-x").await)["seq"], json!(start["seq"].as_u64().unwrap() + 2));

    // Unsigned, a wrong method and a wrong signature: the identical 401.
    let unauthorized = w.unauthorized().await;
    for method in ["GET", "POST", "PUT", "DELETE"] {
        assert_eq!(raw(&w.app, plain(method, &path, "{}")).await, unauthorized, "{method}");
    }
    assert_eq!(raw(&w.app, signed(&b.reseller, "GET", &path, "", None)).await, unauthorized);
    assert_eq!(w.post(&b.reseller, &path, json!([])).await.0, StatusCode::OK, "a body that is JSON and names nothing is a claim with the defaults");
    // A notice has nothing to work on, and a closed card is done.
    let (_, notice) = w.post(&b.reseller, "/v1/rooms/a-x/cards", json!({ "kind": "notice", "title": "A response field is renamed", "text": "Plan for it." })).await;
    let notice = notice["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{notice}/claim"), json!({})).await, (StatusCode::CONFLICT, json!({ "error": "only_findings_are_claimed" })));
}

#[tokio::test]
async fn a_claim_ends_when_its_time_passes_or_its_card_is_closed() {
    let b = board();
    let w = &b.w;
    let card = finding(w, &b.reseller, "a-x").await;
    let path = format!("/v1/cards/{card}/claim");
    assert_eq!(w.post(&b.supplier, &path, json!({ "lease": 60 })).await.0, StatusCode::OK);
    let entries_before = record(w, &b.reseller, "a-x").await["head"]["seq"].as_u64().unwrap();

    // The time passes (set in the record: no test waits a minute). Nothing runs: the claim is just not shown any
    // more, the record has no new entry, and the other side can claim.
    w.shared.store.lock().unwrap().cards[0].claim.as_mut().unwrap().until = now();
    assert_eq!(claim_of(w, &b.reseller, &card).await, Value::Null);
    assert_eq!(w.get(&b.reseller, "/v1/rooms/a-x/cards").await.1["cards"][0]["claim"], Value::Null);
    assert_eq!(record(w, &b.reseller, "a-x").await["head"]["seq"], json!(entries_before));
    assert_eq!(w.post(&b.supplier, &path, json!({ "release": true })).await, (StatusCode::CONFLICT, json!({ "error": "not_claimed" })));
    let (status, taken) = w.post(&b.reseller, &path, json!({ "note": "Ours now." })).await;
    assert_eq!((status, taken["card"]["claim"]["by"].clone()), (StatusCode::OK, json!("reseller-x")));

    // Closing the card ends the claim, whoever closes it; a closed card cannot be claimed.
    assert_eq!(w.post(&b.reseller, &format!("/v1/cards/{card}/stage"), json!({ "stage": "closed", "outcome": "withdrawn", "text": "Not needed." })).await.0, StatusCode::OK);
    assert_eq!(claim_of(w, &b.supplier, &card).await, Value::Null);
    assert!(w.shared.store.lock().unwrap().cards[0].claim.is_none());
    assert_eq!(w.post(&b.reseller, &path, json!({})).await, (StatusCode::CONFLICT, json!({ "error": "card_is_closed" })));
    let chain = record(w, &b.reseller, "a-x").await;
    let kinds: Vec<&str> = chain["entries"].as_array().unwrap().iter().skip(3).map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["claim", "claim", "message", "stage"], "no release entry: the stage entry says the card is closed");
    assert_eq!(record_differs("a-x", &chain["entries"], &details(w, &b.reseller, "a-x").await), Vec::<String>::new());
}

#[tokio::test]
async fn in_a_shared_room_a_claim_is_under_a_handle_and_goes_with_a_removed_member() {
    let b = board();
    let w = &b.w;
    let card = finding(w, &b.supplier, "hub").await;
    let own = finding(w, &b.third, "hub").await;
    let path = format!("/v1/cards/{card}/claim");
    // Any member may claim, also where only the reporter may move the card.
    let (status, claimed) = w.post(&b.third, &path, json!({ "note": "We hit the same and are tracing it." })).await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(claimed["card"]["claim"]["by"], json!("member-3"));
    let until = claimed["card"]["claim"]["until"].clone();
    let seen = claim_of(w, &b.reseller, &card).await;
    assert_eq!(seen, json!({ "by": "member-3", "until": until, "note": "We hit the same and are tracing it.", "mine": false, "untrusted": true }));
    assert_eq!(claim_of(w, &b.third_fields, &card).await["note"], json!("We hit the same and are tracing it."), "its own organisation's fields key reads the note");
    assert_eq!(claim_of(w, &b.reseller_fields, &card).await["note"], Value::Null);
    let refused = w.post(&b.reseller, &path, json!({})).await;
    assert_eq!(refused, (StatusCode::CONFLICT, json!({ "error": "claimed", "claim": { "by": "member-3", "until": until } })));
    // No organisation id or key id in any of it, nor in the record.
    let chain = record(w, &b.reseller, "hub").await;
    for text in [seen.to_string(), refused.1.to_string(), chain.to_string(), w.get(&b.reseller, "/v1/rooms/hub/cards").await.1.to_string()] {
        for secret in ["reseller-y", "supplier-a", "/agent", "/loop"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
    assert_eq!(chain["entries"].as_array().unwrap().last().unwrap()["actor"], json!("member-3"));

    // The member is removed: its live claim on another member's card is taken off by the operator, with an entry.
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{own}/claim"), json!({})).await.0, StatusCode::OK);
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "reseller-y" })).await.0, StatusCode::OK);
    assert_eq!(claim_of(w, &b.reseller, &card).await, Value::Null);
    assert_eq!(claim_of(w, &b.reseller, &own).await, Value::Null, "its own finding was closed, and a closed card shows no claim");
    let chain = record(w, &b.reseller, "hub").await;
    let tail: Vec<(&str, &str)> = chain["entries"].as_array().unwrap().iter().skip(7).map(|e| (e["kind"].as_str().unwrap(), e["actor"].as_str().unwrap())).collect();
    assert_eq!(tail, [("member-removed", "*operator"), ("stage", "*operator"), ("release", "*operator")]);
    assert_eq!(record_differs("hub", &chain["entries"], &details(w, &b.reseller, "hub").await), Vec::<String>::new());
    assert_eq!(w.post(&b.reseller, &path, json!({})).await.0, StatusCode::OK, "the card is free again");
}

/// A state file of version 2 as the board wrote it before claims existed: its cards have no `claim` member.
const STATE_BEFORE_CLAIMS: &str = include_str!("fixtures/board-state-v2.json");

#[tokio::test]
async fn state_files_from_before_claims_load_and_a_claim_goes_through_the_file() {
    let before: Value = serde_json::from_str(STATE_BEFORE_CLAIMS).unwrap();
    assert_eq!(before["version"], json!(2));
    assert!(before["cards"].as_array().unwrap().iter().all(|c| c.get("claim").is_none()) && !STATE_BEFORE_CLAIMS.contains("claim"));
    let (mut store, host, built) = state::decode(STATE_BEFORE_CLAIMS.as_bytes()).expect("the version 2 file from before claims loads");
    assert!(!built && store.cards.len() == 2 && store.cards.iter().all(|c| c.claim.is_none()));
    // Written back with no claim, it is the same JSON: a card nobody claimed has no `claim` member in the file.
    assert_eq!(serde_json::from_slice::<Value>(&state::encode(&store, &host)).unwrap(), before);
    // The version 1 file loads too, and its cards have no claim.
    let (old, _, built) = state::decode(include_bytes!("fixtures/board-state-v1.json")).expect("the version 1 file loads");
    assert!(built && old.cards.len() == 6 && old.cards.iter().all(|c| c.claim.is_none()));

    // A claim is kept through the file.
    store.cards[0].claim = Some(p2paecn_board::store::Claim { org: "supplier-a".into(), until: FAR_FUTURE, note: Some("Kept.".into()) });
    let bytes = state::encode(&store, &host);
    let saved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved["cards"][0]["claim"], json!({ "org": "supplier-a", "until": FAR_FUTURE, "note": "Kept." }));
    assert!(saved["cards"][1].get("claim").is_none());
    let (again, _, _) = state::decode(&bytes).unwrap();
    assert_eq!(again.cards[0].claim, store.cards[0].claim);
}
