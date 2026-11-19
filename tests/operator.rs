//! The operator channel: its routes and their refusals through the router directly, and one real socket
//! to check that the file is owner-only and answers.

mod common;

use axum::http::StatusCode;
use common::*;
use p2paecn_board::store::{KeyStatus, RoomKind};
use p2paecn_board::{operator, Shared};
use serde_json::{json, Value};
use std::future::IntoFuture;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;

fn empty_board() -> World {
    world(json!({ "host": HOST }))
}

fn err(status: StatusCode, code: &str) -> (StatusCode, Value) {
    (status, json!({ "error": code }))
}

const CONFLICT: StatusCode = StatusCode::CONFLICT;
const UNPROCESSABLE: StatusCode = StatusCode::UNPROCESSABLE_ENTITY;

#[tokio::test]
async fn organisations_and_rooms_are_checked() {
    let w = empty_board();
    for org in ["supplier-a", "reseller-x", "reseller-y", "reseller-z"] {
        assert_eq!(w.op("POST", "/orgs", json!({ "id": org })).await, (StatusCode::CREATED, json!({ "id": org })));
    }
    assert_eq!(w.op("POST", "/orgs", json!({ "id": "supplier-a" })).await, err(CONFLICT, "org_exists"));
    for bad in [json!({ "id": "ab" }), json!({ "id": "Supplier-A" }), json!({ "id": "supplier a" }), json!({}), json!({ "id": 7 })] {
        assert_eq!(w.op("POST", "/orgs", bad.clone()).await, err(UNPROCESSABLE, "bad_id"), "{bad}");
    }
    assert_eq!(w.op("POST", "/orgs", Value::Null).await, err(StatusCode::BAD_REQUEST, "bad_json"));

    let room = |id: &str, kind: &str, members: Value| json!({ "id": id, "kind": kind, "members": members });
    let pair = |a: &str, b: &str| json!([{ "org": a }, { "org": b }]);
    assert_eq!(w.op("POST", "/rooms", room("a-x", "pair", pair("supplier-a", "reseller-x"))).await,
               (StatusCode::CREATED, json!({ "id": "a-x", "kind": "pair" })));
    for (body, expected) in [
        (room("a-x", "pair", pair("supplier-a", "reseller-y")), err(CONFLICT, "room_exists")),
        (room("a-y", "pair", json!([{ "org": "supplier-a" }])), err(UNPROCESSABLE, "bad_members")),
        (room("a-y", "pair", pair("supplier-a", "supplier-a")), err(UNPROCESSABLE, "bad_members")),
        (room("a-y", "pair", json!([{ "org": "supplier-a" }, { "org": "reseller-y" }, { "org": "reseller-z" }])), err(UNPROCESSABLE, "bad_members")),
        (room("a-y", "pair", json!([{ "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-y" }])), err(UNPROCESSABLE, "bad_handle")),
        (room("a-y", "pair", pair("supplier-a", "supplier-b")), err(UNPROCESSABLE, "unknown_org")),
        (room("a-y", "pair", json!("supplier-a")), err(UNPROCESSABLE, "bad_members")),
        (room("a-y", "triple", pair("supplier-a", "reseller-y")), err(UNPROCESSABLE, "bad_kind")),
        (room("A_Y", "pair", pair("supplier-a", "reseller-y")), err(UNPROCESSABLE, "bad_id")),
        (room("hub", "shared", json!([])), err(UNPROCESSABLE, "bad_members")),
        (room("hub", "shared", json!([{ "org": "supplier-a" }])), err(UNPROCESSABLE, "bad_handle")),
        (room("hub", "shared", json!([{ "org": "supplier-a", "handle": "M1" }])), err(UNPROCESSABLE, "bad_handle")),
        // A handle that is an org id would give that org away.
        (room("hub", "shared", json!([{ "org": "supplier-a", "handle": "reseller-x" }])), err(UNPROCESSABLE, "bad_handle")),
        (room("hub", "shared", json!([{ "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-x", "handle": "member-1" }])),
         err(CONFLICT, "handle_taken")),
        (room("hub", "shared", json!([{ "org": "supplier-a", "handle": "member-1" }, { "org": "supplier-a", "handle": "member-2" }])),
         err(UNPROCESSABLE, "bad_members")),
        (room("hub", "shared", json!([{ "org": "supplier-b", "handle": "member-1" }])), err(UNPROCESSABLE, "unknown_org")),
    ] {
        assert_eq!(w.op("POST", "/rooms", body.clone()).await, expected, "{body}");
    }
    assert_eq!(w.op("POST", "/rooms", room("hub", "shared", json!([{ "org": "supplier-a", "handle": "member-1" }]))).await.0,
               StatusCode::CREATED);

    // Members of a shared room.
    let member = |org: &str, handle: &str| json!({ "room": "hub", "org": org, "handle": handle });
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-x", "member-2")).await.0, StatusCode::CREATED);
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-y", "member-2")).await, err(CONFLICT, "handle_taken"));
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-x", "member-5")).await, err(CONFLICT, "already_member"));
    assert_eq!(w.op("POST", "/rooms/members", member("supplier-b", "member-5")).await, err(UNPROCESSABLE, "unknown_org"));
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-y", "x")).await, err(UNPROCESSABLE, "bad_handle"));
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "a-x", "org": "reseller-y", "handle": "member-3" })).await,
               err(UNPROCESSABLE, "not_a_shared_room"));
    assert_eq!(w.op("POST", "/rooms/members", json!({ "room": "nowhere", "org": "reseller-y", "handle": "member-3" })).await,
               err(StatusCode::NOT_FOUND, "not_found"));

    let remove = |org: &str| json!({ "room": "hub", "org": org });
    assert_eq!(w.op("POST", "/rooms/members/remove", remove("reseller-x")).await.0, StatusCode::OK);
    assert_eq!(w.op("POST", "/rooms/members/remove", remove("reseller-x")).await, err(StatusCode::NOT_FOUND, "not_found"));
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "a-x", "org": "reseller-x" })).await, err(UNPROCESSABLE, "not_a_shared_room"));
    // A removed member's handle stays with it, for its past cards.
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-y", "member-2")).await, err(CONFLICT, "handle_taken"));
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-x", "member-2")).await.0, StatusCode::CREATED);

    let (status, state) = w.op("GET", "/state", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["orgs"], json!(["reseller-x", "reseller-y", "reseller-z", "supplier-a"]));
    assert_eq!(state["rooms"][1], json!({ "id": "hub", "kind": "shared", "members": [
        { "org": "supplier-a", "handle": "member-1", "removed": false },
        { "org": "reseller-x", "handle": "member-2", "removed": false }] }));

    // Unknown paths and methods answer with the error shape, not the public 401.
    assert_eq!(w.op("GET", "/v1/inbox", Value::Null).await, err(StatusCode::NOT_FOUND, "not_found"));
    assert_eq!(w.op("DELETE", "/state", Value::Null).await, err(StatusCode::NOT_FOUND, "not_found"));
    assert_eq!(w.op("POST", "/rooms", Value::Null).await, err(StatusCode::BAD_REQUEST, "bad_json"));
}

#[tokio::test]
async fn enrollment_codes_are_random_and_bounded() {
    let w = empty_board();
    assert_eq!(w.op("POST", "/orgs", json!({ "id": "reseller-x" })).await.0, StatusCode::CREATED);
    let code_shape = regex::Regex::new(r"^p2p(-[a-z2-7]{4}){6}$").unwrap();

    let (status, body) = w.op("POST", "/enroll-codes", json!({ "org": "reseller-x", "class": "text" })).await;
    assert_eq!(status, StatusCode::CREATED);
    let code = body["code"].as_str().unwrap().to_owned();
    assert!(code_shape.is_match(&code), "{code}");
    assert!(body["expires"].as_u64().unwrap().abs_diff(now() + 24 * 3600) < 10);
    let other = w.op("POST", "/enroll-codes", json!({ "org": "reseller-x", "class": "fields", "ttlHours": 72, "keyTtlDays": 90 })).await;
    assert_eq!(other.0, StatusCode::CREATED);
    assert_ne!(other.1["code"].as_str().unwrap(), code);

    for (body, expected) in [
        (json!({ "org": "reseller-y", "class": "text" }), err(UNPROCESSABLE, "unknown_org")),
        (json!({ "org": "reseller-x", "class": "admin" }), err(UNPROCESSABLE, "bad_class")),
        (json!({ "org": "reseller-x" }), err(UNPROCESSABLE, "bad_class")),
        (json!({ "org": "reseller-x", "class": "text", "ttlHours": 0 }), err(UNPROCESSABLE, "bad_ttl")),
        (json!({ "org": "reseller-x", "class": "text", "ttlHours": 73 }), err(UNPROCESSABLE, "bad_ttl")),
        (json!({ "org": "reseller-x", "class": "text", "ttlHours": "24" }), err(UNPROCESSABLE, "bad_ttl")),
        (json!({ "org": "reseller-x", "class": "text", "ttlHours": 1.5 }), err(UNPROCESSABLE, "bad_ttl")),
        (json!({ "org": "reseller-x", "class": "text", "keyTtlDays": 0 }), err(UNPROCESSABLE, "bad_ttl")),
        (json!({ "org": "reseller-x", "class": "text", "keyTtlDays": 91 }), err(UNPROCESSABLE, "bad_ttl")),
    ] {
        assert_eq!(w.op("POST", "/enroll-codes", body.clone()).await, expected, "{body}");
    }

    let (_, state) = w.op("GET", "/state", Value::Null).await;
    let codes = state["codes"].as_array().unwrap();
    assert_eq!(codes.len(), 2);
    assert_eq!((codes[0]["codeId"].clone(), codes[0]["used"].clone(), codes[1]["class"].clone()), (body["codeId"].clone(), json!(false), json!("fields")));
    assert!(!state.to_string().contains(&code));
}

#[test]
fn the_bootstrap_config_is_checked() {
    let key = public_key(&agent("reseller-x/agent", 2).1);
    let load = |config: Value| Shared::from_config_json(&config.to_string(), None).map(|_| ());
    // The example in the repository loads once a real public key is put in.
    let example = include_str!("../config.example.json").replace(
        "<base64url of the raw 32-byte Ed25519 public key; the private key never leaves the agent's host>", &key);
    let shared = Shared::from_config_json(&example, None).unwrap();
    {
        let store = shared.store.lock().unwrap();
        let kinds: Vec<_> = store.rooms.values().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![RoomKind::Shared, RoomKind::Pair]);
        assert_eq!(store.orgs.len(), 3);
        assert_eq!(store.keys["reseller-x/agent-1"].status, KeyStatus::Active);
    }

    // P2PAECN_HOST wins over the config's host; one of them is required.
    assert_eq!(Shared::from_config_json(&example, Some("127.0.0.1:8797".into())).unwrap().host, "127.0.0.1:8797");
    assert_eq!(shared.host, "board.example");
    assert!(Shared::from_config_json("{}", None).is_err());
    assert_eq!(Shared::from_config_json("{}", Some("board.test".into())).unwrap().host, "board.test");

    let key_entry = |id: &str, org: &str| json!({ "id": id, "org": org, "class": "text", "public_key": key, "expires": FAR_FUTURE });
    for bad in [
        json!({ "host": HOST, "rooms": [{ "id": "a-x", "orgs": ["supplier-a"] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "a-x", "orgs": ["supplier-a", "supplier-a"] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "a-x", "orgs": ["supplier-a", "reseller-x"] }, { "id": "a-x", "orgs": ["supplier-a", "reseller-y"] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "hub", "kind": "shared", "orgs": ["supplier-a", "reseller-x"] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "hub", "kind": "shared", "members": [{ "org": "supplier-a" }] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "hub", "kind": "shared", "members": [
            { "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-x", "handle": "member-1" }] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "hub", "kind": "shared", "members": [{ "org": "supplier-a", "handle": "supplier-a" }] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "Hub", "kind": "shared", "members": [{ "org": "supplier-a", "handle": "member-1" }] }] }),
        json!({ "host": HOST, "rooms": [{ "id": "hub", "kind": "triple", "members": [{ "org": "supplier-a", "handle": "member-1" }] }] }),
        json!({ "host": HOST, "keys": [key_entry("supplier-a/agent", "reseller-x")] }),
        json!({ "host": HOST, "keys": [key_entry("reseller-x", "reseller-x")] }),
        json!({ "host": HOST, "keys": [key_entry("reseller-x/agent", "reseller-x"), key_entry("reseller-x/agent", "reseller-x")] }),
        json!({ "host": HOST, "keys": [key_entry("RX/agent", "RX")] }),
    ] {
        assert!(load(bad.clone()).is_err(), "{bad}");
    }
}

/// A removed member that comes back keeps the handle its past cards show, and no other org ever gets it.
#[tokio::test]
async fn a_returning_member_keeps_its_handle() {
    let w = world(json!({ "host": HOST, "rooms": [{ "id": "hub", "kind": "shared", "members": [
        { "org": "supplier-a", "handle": "member-1" }, { "org": "reseller-x", "handle": "member-2" }] }] }));
    assert_eq!(w.op("POST", "/orgs", json!({ "id": "reseller-z" })).await.0, StatusCode::CREATED);
    let member = |org: &str, handle: &str| json!({ "room": "hub", "org": org, "handle": handle });
    assert_eq!(w.op("POST", "/rooms/members/remove", json!({ "room": "hub", "org": "reseller-x" })).await.0, StatusCode::OK);

    assert_eq!(w.op("POST", "/rooms/members", member("reseller-x", "member-5")).await, err(CONFLICT, "handle_mismatch"));
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-z", "member-2")).await, err(CONFLICT, "handle_taken"));
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-x", "member-2")).await.0, StatusCode::CREATED);
    assert_eq!(w.op("POST", "/rooms/members", member("reseller-z", "member-5")).await.0, StatusCode::CREATED);
    let (_, state) = w.op("GET", "/state", Value::Null).await;
    assert_eq!(state["rooms"][0]["members"], json!([
        { "org": "supplier-a", "handle": "member-1", "removed": false },
        { "org": "reseller-x", "handle": "member-2", "removed": false },
        { "org": "reseller-z", "handle": "member-5", "removed": false }]));
}

/// A second instance started with the same socket path must not take the running instance's socket away.
#[tokio::test]
async fn a_live_operator_socket_is_left_alone() {
    use std::os::unix::fs::MetadataExt;
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("op-live-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let inode = std::fs::metadata(&path).unwrap().ino();

    let refused = operator::bind(&path);
    assert!(refused.is_err());
    assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
    // The running owner still answers on it.
    let _client = std::os::unix::net::UnixStream::connect(&path).unwrap();
    assert!(live.accept().is_ok());
    drop(live);
    std::fs::remove_file(&path).unwrap();
}

#[tokio::test]
async fn the_operator_socket_is_owner_only_and_answers() {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("op-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);

    // A stale socket file from an earlier run is replaced.
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let listener = operator::bind(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");

    let w = world(json!({ "host": HOST, "rooms": [{ "id": "a-x", "orgs": ["supplier-a", "reseller-x"] }] }));
    let server = tokio::spawn(axum::serve(listener, operator::router(w.shared.clone())).into_future());
    let client_path = path.clone();
    let response = tokio::task::spawn_blocking(move || {
        let mut stream = std::os::unix::net::UnixStream::connect(client_path).unwrap();
        stream.write_all(b"GET /state HTTP/1.1\r\nHost: operator\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap();
    server.abort();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"orgs\":[\"reseller-x\",\"supplier-a\"]"), "{response}");
    std::fs::remove_file(&path).unwrap();

    // Anything at the path that is not a socket is left alone and refused.
    std::fs::write(&path, "not a socket").unwrap();
    assert!(operator::bind(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a socket");
    std::fs::remove_file(&path).unwrap();
}
