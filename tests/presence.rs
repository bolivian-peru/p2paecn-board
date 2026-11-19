//! Two additive fields for a web view: a room's `members` (who is in it and, where that gives no handle away, which
//! members have joined with an agent key) and a card's `messages` (how many messages its thread holds). `you`, `others` and every older card field
//! are unchanged. Every example is invented.

mod common;

use axum::http::StatusCode;
use common::*;
use p2paecn_board::sig;
use serde_json::{json, Value};
use uuid::Uuid;

struct Board {
    w: World,
    /// supplier-a's only key: active, text. member-1 in the hub.
    supplier: Agent,
}

/// supplier-a has an active agent key. reseller-x (member-2) and reseller-y (member-3) have no key yet. reseller-z
/// (member-4) has an agent key that expired long ago, and no other.
fn board() -> Board {
    let supplier = agent("supplier-a/agent", 71);
    let stale = agent("reseller-z/agent", 72);
    let w = world(json!({
        "host": HOST,
        "rooms": [
            { "id": "a-x", "orgs": ["supplier-a", "reseller-x"] },
            { "id": "a-y", "orgs": ["supplier-a", "reseller-y"] },
            { "id": "hub", "kind": "shared", "members": [
                { "org": "supplier-a", "handle": "member-1" },
                { "org": "reseller-x", "handle": "member-2" },
                { "org": "reseller-y", "handle": "member-3" },
                { "org": "reseller-z", "handle": "member-4" }
            ] }
        ],
        "keys": [key_entry(&supplier, "text", FAR_FUTURE), key_entry(&stale, "text", 1_000_000)]
    }));
    Board { w, supplier }
}

/// Every org id and key id of the board. None may appear in the shared room's object.
const IDS: [&str; 9] = ["supplier-a", "reseller-x", "reseller-y", "reseller-z", "supplier-a/agent", "reseller-z/agent",
                        "reseller-x/agent", "reseller-x/loop", "reseller-y/office-web"];

fn member(name: &str, you: bool, joined: bool) -> Value {
    json!({ "name": name, "you": you, "joined": joined })
}

/// Another member of a shared room: a handle and nothing about its organisation's keys.
fn handle(name: &str) -> Value {
    json!({ "name": name, "you": false })
}

/// `room`'s `members` as `a` sees it in `GET /v1/rooms`, after checking that `GET /v1/whoami` describes the rooms
/// identically.
async fn members(w: &World, a: &Agent, room: &str) -> Value {
    let (status, rooms) = w.get(a, "/v1/rooms").await;
    assert_eq!(status, StatusCode::OK, "{}", a.0);
    let (_, me) = w.get(a, "/v1/whoami").await;
    assert_eq!(me["rooms"], rooms["rooms"], "whoami and the room list describe rooms alike");
    let room = rooms["rooms"].as_array().unwrap().iter().find(|r| r["id"] == json!(room));
    room.map_or(Value::Null, |r| r["members"].clone())
}

/// Activates a pending key, as the operator does after the person reads the fingerprint.
async fn activate(w: &World, a: &Agent) {
    let fingerprint = sig::fingerprint(&a.1.verifying_key());
    assert_eq!(w.op("POST", "/keys/activate", json!({ "keyId": a.0, "fingerprint": fingerprint })).await.0, StatusCode::OK);
}

#[tokio::test]
async fn members_is_you_first_then_the_others_by_display_name() {
    let b = board();
    let w = &b.w;
    let (_, rooms) = w.get(&b.supplier, "/v1/rooms").await;
    assert_eq!(without_heads(rooms.clone()), json!({ "rooms": [
        { "id": "a-x", "kind": "pair", "you": "supplier-a", "others": ["reseller-x"],
          "members": [member("supplier-a", true, true), member("reseller-x", false, false)] },
        { "id": "a-y", "kind": "pair", "you": "supplier-a", "others": ["reseller-y"],
          "members": [member("supplier-a", true, true), member("reseller-y", false, false)] },
        { "id": "hub", "kind": "shared", "you": "member-1", "others": ["member-2", "member-3", "member-4"],
          "members": [member("member-1", true, true), handle("member-2"), handle("member-3"), handle("member-4")] }
    ] }));
    for room in rooms["rooms"].as_array().unwrap() {
        let names: Vec<Value> = room["members"].as_array().unwrap().iter().map(|m| m["name"].clone()).collect();
        assert_eq!(names[0], room["you"], "the caller first, under its `you`");
        assert_eq!(Value::from(names[1..].to_vec()), room["others"], "then exactly `others`, in its order");
        let yours: Vec<bool> = room["members"].as_array().unwrap().iter().map(|m| m["you"].as_bool().unwrap()).collect();
        assert_eq!(yours.iter().filter(|y| **y).count(), 1);
    }
    // In the shared room: handles only. No org id, the caller's own included, and no key id.
    let hub = rooms["rooms"][2].to_string();
    for id in IDS {
        assert!(!hub.contains(id), "the shared room names {id}: {hub}");
    }

    // From another member's side, once it has a key: itself first. The pair room names org ids, as it always did.
    let reseller = w.active_agent("reseller-x", "text", "agent", 73).await;
    assert_eq!(members(w, &reseller, "a-x").await, json!([member("reseller-x", true, true), member("supplier-a", false, true)]));
    assert_eq!(members(w, &reseller, "hub").await,
               json!([member("member-2", true, true), handle("member-1"), handle("member-3"), handle("member-4")]));
    assert_eq!(members(w, &reseller, "a-y").await, Value::Null, "not its room");
    let (_, theirs) = w.get(&reseller, "/v1/rooms").await;
    let hub = theirs["rooms"][1].to_string();
    for id in IDS {
        assert!(!hub.contains(id), "the shared room names {id} to member-2: {hub}");
    }

    // A removed member is not listed, and is gone from `others` too; the rest keep their order.
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "reseller-y" })).await.0, StatusCode::OK);
    assert_eq!(members(w, &b.supplier, "hub").await, json!([member("member-1", true, true), handle("member-2"), handle("member-4")]));
    assert_eq!(w.get(&b.supplier, "/v1/rooms").await.1["rooms"][2]["others"], json!(["member-2", "member-4"]));
    // Back under its old handle, it is listed again, where the room has it.
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "hub", "org": "reseller-y", "handle": "member-3" })).await.0, StatusCode::CREATED);
    assert_eq!(members(w, &b.supplier, "hub").await,
               json!([member("member-1", true, true), handle("member-2"), handle("member-3"), handle("member-4")]));
}

#[tokio::test]
async fn joined_needs_an_active_unexpired_agent_key() {
    let b = board();
    let w = &b.w;
    // reseller-x as supplier-a sees it in their pair room. The shared room never says it: checked at every step.
    let seen = async || {
        let (pair, hub) = (members(w, &b.supplier, "a-x").await, members(w, &b.supplier, "hub").await);
        assert_eq!(pair[1]["name"], json!("reseller-x"));
        assert_eq!(hub, json!([member("member-1", true, true), handle("member-2"), handle("member-3"), handle("member-4")]),
                   "the shared room says `joined` of the caller only");
        pair[1]["joined"].as_bool().unwrap()
    };
    assert!(!seen().await, "no key at all");

    // Enrolled, the key is pending: not joined, and the key itself is told no rooms.
    let x = agent("reseller-x/agent", 74);
    let code = w.enroll_code("reseller-x", "text").await;
    assert_eq!(w.enroll(&code, "agent", &x.1).await.status, StatusCode::CREATED);
    assert!(!seen().await, "a pending key");
    assert_eq!(w.get(&x, "/v1/whoami").await.1["rooms"], json!([]));

    // Joined from the moment the operator activates it.
    activate(w, &x).await;
    assert!(seen().await, "an active key");
    assert_eq!(members(w, &x, "a-x").await, json!([member("reseller-x", true, true), member("supplier-a", false, true)]));

    // Its only agent key revoked: not joined.
    assert_eq!(w.post(&x, "/v1/keys/revoke", json!({ "keyId": x.0 })).await.0, StatusCode::OK);
    assert!(!seen().await, "its only agent key revoked");

    // A `fields` key is an agent key too. Once it has expired, its organisation has not joined.
    let lp = w.active_agent("reseller-x", "fields", "loop", 75).await;
    assert!(seen().await, "an active fields key");
    w.shared.store.lock().unwrap().keys.get_mut(&lp.0).unwrap().expires = now() - 1;
    assert!(!seen().await, "its only active agent key expired");
    // And a key that expired long ago, still marked active (reseller-z's), never counted: reseller-z itself is told
    // so in the shared room, the only room it is in.
    assert_eq!(w.shared.store.lock().unwrap().keys["reseller-z/agent"].status, p2paecn_board::store::KeyStatus::Active);
    let z = w.active_agent("reseller-z", "viewer", "desk-web", 80).await;
    assert_eq!(members(w, &z, "hub").await, json!([member("member-4", true, false), handle("member-1"), handle("member-2"), handle("member-3")]));

    // A viewer key is a person's browser, not an agent: an organisation with only a viewer key has not joined, as
    // the other side of its pair room sees it and as it sees itself, in both rooms.
    let viewer = w.active_agent("reseller-y", "viewer", "office-web", 76).await;
    assert_eq!(members(w, &b.supplier, "a-y").await, json!([member("supplier-a", true, true), member("reseller-y", false, false)]));
    assert_eq!(members(w, &viewer, "a-y").await, json!([member("reseller-y", true, false), member("supplier-a", false, true)]));
    assert_eq!(members(w, &viewer, "hub").await[0], member("member-3", true, false));
    // Its agent joins: the viewer sees its own organisation joined, in both rooms.
    w.active_agent("reseller-y", "text", "agent", 77).await;
    assert_eq!(members(w, &viewer, "a-y").await[0], member("reseller-y", true, true));
    assert_eq!(members(w, &viewer, "hub").await[0], member("member-3", true, true));
}

/// In a shared room members know each other by handle only. `joined` is a fact about an organisation, so if the
/// shared room said it of every member, a member that also shares a pair room with an organisation could match the
/// two: both would change in the same read when that organisation's agent key is activated, revoked or expires.
#[tokio::test]
async fn joined_never_links_a_shared_room_handle_to_an_organisation() {
    let b = board();
    let w = &b.w;
    // What supplier-a is told about the others in the hub, and about its two pair rooms.
    let hub_others = async || members(w, &b.supplier, "hub").await.as_array().unwrap()[1..].to_vec();
    let pairs = async || (members(w, &b.supplier, "a-x").await[1]["joined"].clone(), members(w, &b.supplier, "a-y").await[1]["joined"].clone());
    let before = hub_others().await;
    assert_eq!(before, [handle("member-2"), handle("member-3"), handle("member-4")]);
    assert_eq!(pairs().await, (json!(false), json!(false)));

    // reseller-x's agent key becomes active, then reseller-y's, then reseller-x's is revoked. The pair rooms follow
    // each change; what the hub says of the others is the same bytes throughout.
    let x = w.active_agent("reseller-x", "text", "agent", 81).await;
    assert_eq!(pairs().await, (json!(true), json!(false)));
    assert_eq!(hub_others().await, before);
    let y = w.active_agent("reseller-y", "text", "agent", 82).await;
    assert_eq!(pairs().await, (json!(true), json!(true)));
    assert_eq!(hub_others().await, before);
    assert_eq!(w.post(&x, "/v1/keys/revoke", json!({ "keyId": x.0 })).await.0, StatusCode::OK);
    assert_eq!(pairs().await, (json!(false), json!(true)));
    assert_eq!(hub_others().await, before);

    // The same from a reseller's side, and no other member's entry holds the word at all.
    let hub = members(w, &y, "hub").await;
    assert_eq!(hub, json!([member("member-3", true, true), handle("member-1"), handle("member-2"), handle("member-4")]));
    for other in &hub.as_array().unwrap()[1..] {
        assert!(other.get("joined").is_none(), "{other}");
    }
}

/// The card's `messages` in every read that returns it, as `a` sees it: the thread, the room's list and, when it
/// is listed there, the inbox. The thread's count is the length of its `messages` array.
async fn counts(w: &World, a: &Agent, room: &str, id: &str) -> (Value, Value, Value) {
    let (_, thread) = w.get(a, &format!("/v1/cards/{id}")).await;
    assert_eq!(thread["card"]["messages"], json!(thread["messages"].as_array().unwrap().len()), "{}", a.0);
    let (_, list) = w.get(a, &format!("/v1/rooms/{room}/cards")).await;
    let listed = list["cards"].as_array().unwrap().iter().find(|c| c["id"] == json!(id)).unwrap()["messages"].clone();
    let (_, inbox) = w.get(a, "/v1/inbox").await;
    let inboxed = ["waitingOnUs", "noticesToAck", "sharedOpen"]
        .iter()
        .flat_map(|l| inbox[l].as_array().unwrap().clone())
        .find(|c| c["id"] == json!(id))
        .map_or(Value::Null, |c| c["messages"].clone());
    (thread["card"]["messages"].clone(), listed, inboxed)
}

#[tokio::test]
async fn messages_counts_the_thread_and_grows_with_it() {
    let b = board();
    let w = &b.w;
    let s = &b.supplier;
    let reseller = w.active_agent("reseller-x", "text", "agent", 78).await;
    let fields = w.active_agent("reseller-x", "fields", "loop", 79).await;

    let body = json!({ "kind": "finding", "severity": "wrong", "title": "List endpoint skips one item when paging",
                       "text": "Page two starts one item late." }).to_string();
    let idem = Uuid::new_v4().to_string();
    let (status, text) = send(&w.app, signed(&reseller, "POST", "/v1/rooms/a-x/cards", &body, Some(&idem))).await;
    assert_eq!(status, StatusCode::CREATED);
    let created: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(created["card"]["messages"], json!(1), "a new card's thread holds its report");
    let id = created["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(counts(w, s, "a-x", &id).await, (json!(1), json!(1), json!(1)));

    let messages = format!("/v1/cards/{id}/messages");
    let stage = format!("/v1/cards/{id}/stage");
    assert_eq!(w.post(s, &messages, json!({ "kind": "reply", "text": "Which page size?" })).await.0, StatusCode::CREATED);
    assert_eq!(counts(w, s, "a-x", &id).await, (json!(2), json!(2), json!(2)));
    let (_, proof) = w.post(&reseller, &messages, json!({ "kind": "proof", "text": "Page size ten, sorted by creation time." })).await;
    assert_eq!(counts(w, s, "a-x", &id).await, (json!(3), json!(3), json!(3)));
    // A move adds its stage message, and the move's answer counts it.
    let (status, moved) = w.post(s, &stage, json!({ "stage": "acknowledged", "text": "Looking at it." })).await;
    assert_eq!((status, moved["card"]["messages"].clone()), (StatusCode::OK, json!(4)));
    let correction = json!({ "kind": "correction", "text": "Page size ten only.", "replaces": proof["messageId"] });
    assert_eq!(w.post(&reseller, &messages, correction).await.0, StatusCode::CREATED);
    assert_eq!(counts(w, s, "a-x", &id).await, (json!(5), json!(5), json!(5)));
    // A fields key counts the messages whose text it does not get.
    assert_eq!(counts(w, &fields, "a-x", &id).await.0, json!(5));
    let (_, seen) = w.get(&fields, &format!("/v1/cards/{id}")).await;
    assert_eq!(seen["messages"][1]["text"], Value::Null);

    // Shipped (6), then verified by the reporter (7); a reply after closing still counts (8). Closed cards are in
    // no inbox list.
    assert_eq!(w.post(s, &stage, json!({ "stage": "shipped", "text": "Fixed the page offset." })).await.1["card"]["messages"], json!(6));
    assert_eq!(counts(w, &reseller, "a-x", &id).await, (json!(6), json!(6), json!(6)), "waiting on the reporter");
    let verified = json!({ "stage": "closed", "outcome": "verified", "text": "Page two starts with the next item." });
    assert_eq!(w.post(&reseller, &stage, verified).await.1["card"]["messages"], json!(7));
    assert_eq!(w.post(s, &messages, json!({ "kind": "reply", "text": "Thanks for checking." })).await.0, StatusCode::CREATED);
    assert_eq!(counts(w, s, "a-x", &id).await, (json!(8), json!(8), Value::Null));

    // A notice: its report, then each acknowledgement.
    let (_, notice) = w.post(s, "/v1/rooms/hub/cards", json!({ "kind": "notice",
        "title": "Breaking: a response field is renamed in the next kit release", "text": "The old name keeps working for one release." })).await;
    assert_eq!(notice["card"]["messages"], json!(1));
    let notice = notice["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(counts(w, &reseller, "hub", &notice).await, (json!(1), json!(1), json!(1)));
    assert_eq!(w.post(&reseller, &format!("/v1/cards/{notice}/messages"), json!({ "kind": "ack", "text": "ready" })).await.0, StatusCode::CREATED);
    assert_eq!(counts(w, s, "hub", &notice).await, (json!(2), json!(2), Value::Null));

    // A replay is the first answer as it was given: the count at that time.
    let (status, text) = send(&w.app, signed(&reseller, "POST", "/v1/rooms/a-x/cards", &body, Some(&idem))).await;
    let again: Value = serde_json::from_str(&text).unwrap();
    assert_eq!((status, again["replayed"].clone(), again["card"]["messages"].clone()), (StatusCode::CREATED, json!(true), json!(1)));
}

#[tokio::test]
async fn a_viewer_sees_the_same_members_and_counts_as_its_organisations_agent() {
    let b = board();
    let w = &b.w;
    let reseller = w.active_agent("reseller-x", "text", "agent", 80).await;
    let viewer = w.active_agent("reseller-x", "viewer", "office-web", 81).await;
    let (_, pair) = w.post(&reseller, "/v1/rooms/a-x/cards", json!({ "kind": "finding", "severity": "question",
        "title": "Is a sandbox mode planned for testing orders?", "text": "Asking for our test suite." })).await;
    let pair = pair["card"]["id"].as_str().unwrap().to_owned();
    assert_eq!(w.post(&b.supplier, &format!("/v1/cards/{pair}/messages"), json!({ "kind": "reply", "text": "Not yet." })).await.0, StatusCode::CREATED);
    let (_, shared) = w.post(&b.supplier, "/v1/rooms/hub/cards", json!({ "kind": "finding", "severity": "friction",
        "title": "Sample app fails to start on a clean install", "text": "Seen on a clean install." })).await;
    let shared = shared["card"]["id"].as_str().unwrap().to_owned();

    for path in ["/v1/rooms".to_owned(), "/v1/inbox".into(), "/v1/rooms/a-x/cards".into(), "/v1/rooms/hub/cards".into(),
                 format!("/v1/cards/{pair}"), format!("/v1/cards/{shared}")] {
        let (status, seen) = w.get(&viewer, &path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(seen, w.get(&reseller, &path).await.1, "{path}");
    }
    let (_, me) = w.get(&viewer, "/v1/whoami").await;
    assert_eq!(me["rooms"], w.get(&reseller, "/v1/whoami").await.1["rooms"]);
    assert_eq!(me["rooms"][1]["members"],
               json!([member("member-2", true, true), handle("member-1"), handle("member-3"), handle("member-4")]));
    let (_, thread) = w.get(&viewer, &format!("/v1/cards/{pair}")).await;
    assert_eq!(thread["card"]["messages"], json!(2));
    let (_, inbox) = w.get(&viewer, "/v1/inbox").await;
    assert_eq!((inbox["sharedOpen"][0]["id"].clone(), inbox["sharedOpen"][0]["messages"].clone()), (json!(shared), json!(1)));
    assert!(!me["rooms"][1].to_string().contains("reseller-x"), "no org id in the shared room for a viewer either");
}

/// The card fields every client already reads. `messages` is the only one added.
const CARD_FIELDS: [&str; 12] = ["created", "id", "kind", "mine", "outcome", "reporter", "room", "severity", "stage", "title", "untrusted", "updated"];

fn card_fields_without_messages(card: &Value) -> Vec<String> {
    assert!(card["messages"].as_u64().is_some_and(|n| n >= 1), "{card}");
    let mut names: Vec<String> = card.as_object().unwrap().keys().filter(|k| *k != "messages").cloned().collect();
    names.sort();
    names
}

#[tokio::test]
async fn without_the_new_fields_every_answer_is_what_it_was() {
    let b = board();
    let w = &b.w;
    let reseller = w.active_agent("reseller-x", "text", "agent", 82).await;

    // A room minus `members` and `chain` (the head of its record, added later) is exactly the old object: id, kind,
    // you, others.
    let old_rooms = json!([
        { "id": "a-x", "kind": "pair", "you": "reseller-x", "others": ["supplier-a"] },
        { "id": "hub", "kind": "shared", "you": "member-2", "others": ["member-1", "member-3", "member-4"] }
    ]);
    let strip = |mut rooms: Value| {
        for room in rooms.as_array_mut().unwrap() {
            assert!(room.as_object_mut().unwrap().remove("members").is_some_and(|m| m.is_array()));
            assert!(room.as_object_mut().unwrap().remove("chain").is_some_and(|c| c.is_object()));
        }
        rooms
    };
    assert_eq!(strip(w.get(&reseller, "/v1/rooms").await.1["rooms"].clone()), old_rooms);
    assert_eq!(strip(w.get(&reseller, "/v1/whoami").await.1["rooms"].clone()), old_rooms);

    // A card minus `messages` has exactly the old fields, from every route that returns one.
    let (_, created) = w.post(&reseller, "/v1/rooms/a-x/cards", json!({ "kind": "finding", "severity": "wrong",
        "title": "Docs example uses a field name the API rejects", "text": "The example sends a field the API does not know." })).await;
    let id = created["card"]["id"].as_str().unwrap().to_owned();
    let (_, moved) = w.post(&b.supplier, &format!("/v1/cards/{id}/stage"), json!({ "stage": "acknowledged", "text": "Looking at it." })).await;
    let (_, thread) = w.get(&b.supplier, &format!("/v1/cards/{id}")).await;
    let (_, list) = w.get(&b.supplier, "/v1/rooms/a-x/cards").await;
    let (_, inbox) = w.get(&b.supplier, "/v1/inbox").await;
    for (route, card, stage) in [("create", &created["card"], "reported"), ("stage", &moved["card"], "acknowledged"),
                                 ("card", &thread["card"], "acknowledged"), ("list", &list["cards"][0], "acknowledged"),
                                 ("inbox", &inbox["waitingOnUs"][0], "acknowledged")] {
        assert_eq!(card_fields_without_messages(card), CARD_FIELDS, "{route}");
        assert_eq!((card["id"].clone(), card["reporter"].clone(), card["stage"].clone()), (json!(id), json!("reseller-x"), json!(stage)), "{route}");
    }
    // Messages keep their fields: the new count is on the card only.
    let names: Vec<&String> = thread["messages"][0].as_object().unwrap().keys().collect();
    assert_eq!(names, ["author", "card", "created", "id", "kind", "mine", "replaces", "text", "untrusted"]);
}
