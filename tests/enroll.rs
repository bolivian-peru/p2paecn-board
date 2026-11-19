//! Joining: enrollment with a one-time code and proof of the new key, the pending state, activation by
//! fingerprint, key listing and revocation. Every failure the API reference calls "the identical 401" is compared
//! byte for byte with the 401 an unsigned request gets.

mod common;

use axum::http::StatusCode;
use common::*;
use p2paecn_board::sig;
use serde_json::{json, Value};
use uuid::Uuid;

struct Board {
    w: World,
    supplier: Agent,
    reseller: Agent,
    expired: Agent,
}

fn board() -> Board {
    let supplier = agent("supplier-a/agent", 1);
    let reseller = agent("reseller-x/agent", 2);
    let expired = agent("reseller-x/old", 5);
    let w = world(json!({
        "host": HOST,
        "rooms": [{ "id": "a-x", "orgs": ["supplier-a", "reseller-x"] }],
        "keys": [
            key_entry(&supplier, "text", FAR_FUTURE),
            key_entry(&reseller, "text", FAR_FUTURE),
            key_entry(&expired, "text", 1_000_000),
        ]
    }));
    Board { w, supplier, reseller, expired }
}

fn body_json(r: &Raw) -> Value {
    serde_json::from_slice(&r.body).unwrap()
}

#[tokio::test]
async fn enrollment_needs_a_valid_code_and_a_proof_of_the_key() {
    let b = board();
    let w = &b.w;
    let reference = w.unauthorized().await;
    let new = agent("reseller-x/new-agent", 21);
    let stranger = agent("reseller-x/stranger", 22);
    let code = w.enroll_code("reseller-x", "text").await;
    let enroll_raw = |key: &Agent, body: &str, idem: Option<&str>| signed_with("enroll", &key.1, "POST", "/v1/enroll", body, idem);
    let body_for = |code: &str, name: &str, key: &Agent| json!({ "code": code, "name": name, "publicKey": public_key(&key.1) }).to_string();

    // Before the code is proven valid, every failure is the identical 401.
    let failures = [
        ("wrong code", enroll_raw(&new, &body_for("p2p-aaaa-aaaa-aaaa-aaaa-aaaa-aaaa", "new-agent", &new), None)),
        ("bad proof", enroll_raw(&stranger, &body_for(&code, "new-agent", &new), None)),
        ("malformed json", enroll_raw(&new, "{\"code\":", None)),
        ("no body", enroll_raw(&new, "", None)),
        ("missing field", enroll_raw(&new, &json!({ "code": code, "publicKey": public_key(&new.1) }).to_string(), None)),
        ("extra field", enroll_raw(&new, &json!({ "code": code, "name": "new-agent", "publicKey": public_key(&new.1), "org": "supplier-a" }).to_string(), None)),
        ("not a key", enroll_raw(&new, &json!({ "code": code, "name": "new-agent", "publicKey": "AAAA" }).to_string(), None)),
        ("idempotency key", enroll_raw(&new, &body_for(&code, "new-agent", &new), Some(&Uuid::new_v4().to_string()))),
        ("other key id", signed_with("reseller-x/agent", &new.1, "POST", "/v1/enroll", &body_for(&code, "new-agent", &new), None)),
        ("unsigned", plain("POST", "/v1/enroll", &body_for(&code, "new-agent", &new))),
    ];
    for (what, req) in failures {
        assert_eq!(raw(&w.app, req).await, reference, "{what}");
    }

    // A good code creates a pending key.
    let created = w.enroll(&code, "new-agent", &new.1).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let body = body_json(&created);
    let expires = body["expires"].as_u64().unwrap();
    assert!(expires.abs_diff(now() + 30 * 86_400) < 10);
    let fingerprint = sig::fingerprint(&new.1.verifying_key());
    assert_eq!(fingerprint.len(), 24);
    assert_eq!(body, json!({ "keyId": "reseller-x/new-agent", "org": "reseller-x", "class": "text", "status": "pending",
                             "fingerprint": fingerprint, "expires": expires }));

    // The same code, key and name replays the 201 (the code is compared after lower-casing and removing spaces).
    let shouted = code.to_uppercase().replace('-', " - ");
    let again = w.enroll(&shouted, "new-agent", &new.1).await;
    assert_eq!((again.status, &again.body), (StatusCode::CREATED, &created.body));
    assert_eq!(w.shared.store.lock().unwrap().keys.len(), 4);

    // A used code with a different key, or the same key under another name, is the identical 401.
    assert_eq!(w.enroll(&code, "stranger", &stranger.1).await, reference);
    assert_eq!(w.enroll(&code, "other-name", &new.1).await, reference);

    // An expired code is the identical 401, even for the key that would replay it.
    let late = w.enroll_code("reseller-x", "text").await;
    w.shared.store.lock().unwrap().codes.last_mut().unwrap().expires = now() - 1;
    assert_eq!(w.enroll(&late, "late-agent", &stranger.1).await, reference);
    w.shared.store.lock().unwrap().codes[0].expires = now() - 1;
    assert_eq!(w.enroll(&code, "new-agent", &new.1).await, reference);

    // The operator sees codes by id, never a code itself.
    let (_, state) = w.op("GET", "/state", Value::Null).await;
    assert!(!state.to_string().contains(&code) && !state.to_string().contains(&late));
    assert_eq!(state["codes"][0]["used"], json!(true));
}

#[tokio::test]
async fn refusals_after_a_valid_code_do_not_use_it_up() {
    let b = board();
    let w = &b.w;
    let new = agent("supplier-a/build-bot", 31);
    let (status, body) = w.op("POST", "/enroll-codes", json!({ "org": "supplier-a", "class": "fields", "ttlHours": 1, "keyTtlDays": 2 })).await;
    assert_eq!(status, StatusCode::CREATED);
    let code = body["code"].as_str().unwrap().to_owned();
    assert!(body["expires"].as_u64().unwrap().abs_diff(now() + 3600) < 10);

    let refusal = |r: Raw| (r.status, body_json(&r));
    for name in ["X", "no spaces", "Upper", "a", "this-name-is-far-too-long-for-a-key"] {
        assert_eq!(refusal(w.enroll(&code, name, &new.1).await), (StatusCode::UNPROCESSABLE_ENTITY, json!({ "error": "bad_name" })), "{name}");
    }
    assert_eq!(refusal(w.enroll(&code, "agent", &new.1).await), (StatusCode::CONFLICT, json!({ "error": "key_id_taken" })));
    // The reseller's key, enrolled already under its own id, cannot be enrolled again for the supplier.
    assert_eq!(refusal(w.enroll(&code, "fresh", &b.reseller.1).await), (StatusCode::CONFLICT, json!({ "error": "key_already_enrolled" })));

    let created = w.enroll(&code, "build-bot", &new.1).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let body = body_json(&created);
    assert_eq!((body["class"].clone(), body["org"].clone()), (json!("fields"), json!("supplier-a")));
    assert!(body["expires"].as_u64().unwrap().abs_diff(now() + 2 * 86_400) < 10);
}

/// Anyone can sign an enrollment with a key of their own. Without a valid code that must leave no state behind.
#[tokio::test]
async fn an_enrollment_without_a_valid_code_leaves_nothing_behind() {
    let b = board();
    let w = &b.w;
    let reference = w.unauthorized().await;
    let nonces = || w.shared.store.lock().unwrap().nonce_count();
    let used = w.enroll_code("reseller-x", "text").await;
    assert_eq!(w.enroll(&used, "first", &agent("reseller-x/first", 24).1).await.status, StatusCode::CREATED);
    let expired = w.enroll_code("reseller-x", "text").await;
    w.shared.store.lock().unwrap().codes.last_mut().unwrap().expires = now() - 1;

    let before = nonces();
    let throwaway = agent("reseller-x/agent-1", 23);
    for code in ["p2p-aaaa-aaaa-aaaa-aaaa-aaaa-aaaa", used.as_str(), expired.as_str()] {
        for _ in 0..3 {
            assert_eq!(w.enroll(code, "agent-1", &throwaway.1).await, reference, "{code}");
        }
    }
    assert_eq!(nonces(), before);

    // With a valid code the nonce is kept, so the same bytes cannot be sent twice.
    let code = w.enroll_code("reseller-x", "text").await;
    let body = json!({ "code": code, "name": "agent-1", "publicKey": public_key(&throwaway.1) }).to_string();
    let first = signed_with("enroll", &throwaway.1, "POST", "/v1/enroll", &body, None);
    let again = copy(&first, &body);
    assert_eq!(raw(&w.app, first).await.status, StatusCode::CREATED);
    assert_eq!(raw(&w.app, again).await, reference);
    assert_eq!(nonces(), before + 1);
}

/// A client that lost the response is told the key's status as it is now, and a revoked key gets the 401.
#[tokio::test]
async fn an_enrollment_replay_tells_the_status_of_the_key_now() {
    let b = board();
    let w = &b.w;
    let reference = w.unauthorized().await;
    let new = agent("reseller-x/new-agent", 21);
    let code = w.enroll_code("reseller-x", "text").await;
    let created = w.enroll(&code, "new-agent", &new.1).await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(w.enroll(&code, "new-agent", &new.1).await, created);

    let fingerprint = sig::fingerprint(&new.1.verifying_key());
    assert_eq!(w.op("POST", "/keys/activate", json!({ "keyId": new.0, "fingerprint": fingerprint })).await.0, StatusCode::OK);
    let again = w.enroll(&code, "new-agent", &new.1).await;
    let mut active = body_json(&created);
    active["status"] = json!("active");
    assert_eq!((again.status, body_json(&again)), (StatusCode::CREATED, active));

    assert_eq!(w.op("POST", "/keys/revoke", json!({ "keyId": new.0 })).await.0, StatusCode::OK);
    assert_eq!(w.enroll(&code, "new-agent", &new.1).await, reference);
}

/// A key that is already enrolled gets one answer whatever the name, so it cannot list the code's org's key names.
#[tokio::test]
async fn an_enrolled_key_cannot_learn_which_key_names_exist() {
    let b = board();
    let w = &b.w;
    let code = w.enroll_code("reseller-x", "text").await;
    let refusal = |r: Raw| (r.status, body_json(&r));
    let existing = refusal(w.enroll(&code, "agent", &b.supplier.1).await);
    let missing = refusal(w.enroll(&code, "nobody-here", &b.supplier.1).await);
    assert_eq!(existing, (StatusCode::CONFLICT, json!({ "error": "key_already_enrolled" })));
    assert_eq!(existing, missing);
    // The code was not used up.
    assert_eq!(w.enroll(&code, "fresh", &agent("reseller-x/fresh", 25).1).await.status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_pending_key_may_only_ask_who_it_is() {
    let b = board();
    let w = &b.w;
    let reference = w.unauthorized().await;
    let new = agent("reseller-x/new-agent", 21);
    let code = w.enroll_code("reseller-x", "text").await;
    assert_eq!(w.enroll(&code, "new-agent", &new.1).await.status, StatusCode::CREATED);
    let card = Uuid::new_v4();

    let (status, me) = w.get(&new, "/v1/whoami").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((me["status"].clone(), me["rooms"].clone(), me["keyId"].clone()), (json!("pending"), json!([]), json!("reseller-x/new-agent")));
    assert!(me["serverTime"].as_u64().unwrap().abs_diff(now()) < 10);

    let every_other_route = || {
        [("GET", "/v1/inbox".to_owned()), ("GET", "/v1/rooms".into()), ("GET", "/v1/keys".into()), ("GET", "/v1/rooms/a-x/cards".into()),
         ("GET", format!("/v1/cards/{card}")), ("POST", "/v1/rooms/a-x/cards".into()), ("POST", format!("/v1/cards/{card}/messages")),
         ("POST", format!("/v1/cards/{card}/stage")), ("POST", "/v1/keys/revoke".into())]
    };
    let body = json!({ "kind": "notice", "title": "Hello", "text": "x", "keyId": "reseller-x/new-agent" }).to_string();
    for (method, path) in every_other_route() {
        let idem = Uuid::new_v4().to_string();
        assert_eq!(raw(&w.app, signed(&new, method, &path, &body, Some(&idem))).await, reference, "{method} {path}");
    }

    // A wrong fingerprint is refused and the key stays pending.
    let wrong = sig::fingerprint(&b.supplier.1.verifying_key());
    let (status, body) = w.op("POST", "/keys/activate", json!({ "keyId": new.0, "fingerprint": wrong })).await;
    assert_eq!((status, body), (StatusCode::CONFLICT, json!({ "error": "fingerprint_mismatch" })));
    assert_eq!(w.get(&new, "/v1/whoami").await.1["status"], json!("pending"));
    assert_eq!(raw(&w.app, signed(&new, "GET", "/v1/inbox", "", None)).await, reference);
    let (status, _) = w.op("POST", "/keys/activate", json!({ "keyId": "reseller-x/nobody", "fingerprint": wrong })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The fingerprint as a person reads it: case and dashes do not matter.
    let spoken = sig::fingerprint(&new.1.verifying_key()).replace('-', "").to_uppercase();
    let (status, body) = w.op("POST", "/keys/activate", json!({ "keyId": new.0, "fingerprint": spoken })).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "keyId": "reseller-x/new-agent", "status": "active" })));
    let (status, _) = w.op("POST", "/keys/activate", json!({ "keyId": new.0, "fingerprint": spoken })).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (_, me) = w.get(&new, "/v1/whoami").await;
    assert_eq!((me["status"].clone(), me["rooms"].clone()),
               (json!("active"), json!([{ "id": "a-x", "kind": "pair", "you": "reseller-x", "others": ["supplier-a"],
                                          "members": [{ "name": "reseller-x", "you": true, "joined": true }, { "name": "supplier-a", "you": false, "joined": true }] }])));
    assert_eq!(w.get(&new, "/v1/inbox").await.0, StatusCode::OK);
    let (status, _) = w.post(&new, "/v1/rooms/a-x/cards", json!({ "kind": "finding", "severity": "question",
                                                                  "title": "Is a sandbox mode planned for testing orders?", "text": "Asking for our test suite." })).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn revoked_and_expired_keys_get_the_identical_401() {
    let b = board();
    let w = &b.w;
    let reference = w.unauthorized().await;
    assert_eq!(raw(&w.app, signed(&b.expired, "GET", "/v1/whoami", "", None)).await, reference);
    assert_eq!(raw(&w.app, signed(&b.expired, "GET", "/v1/inbox", "", None)).await, reference);

    // The key list is the caller's own organisation only.
    let (_, keys) = w.get(&b.reseller, "/v1/keys").await;
    let ids: Vec<_> = keys["keys"].as_array().unwrap().iter().map(|k| k["keyId"].clone()).collect();
    assert_eq!(ids, vec![json!("reseller-x/agent"), json!("reseller-x/old")]);
    assert_eq!(keys["keys"][0]["fingerprint"], json!(sig::fingerprint(&b.reseller.1.verifying_key())));
    assert!(!keys.to_string().contains("supplier-a"));

    // A key cannot revoke another organisation's key, and cannot tell it exists.
    let theirs = w.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": "supplier-a/agent" })).await;
    let nobody = w.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": "supplier-a/nobody" })).await;
    assert_eq!(theirs, (StatusCode::NOT_FOUND, json!({ "error": "not_found" })));
    assert_eq!(theirs, nobody);
    assert_eq!(w.get(&b.supplier, "/v1/inbox").await.0, StatusCode::OK);

    // A sibling key is revoked at once, for good.
    let second = w.active_agent("reseller-x", "text", "second", 41).await;
    assert_eq!(w.get(&second, "/v1/inbox").await.0, StatusCode::OK);
    let (status, body) = w.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": second.0 })).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "keyId": "reseller-x/second", "status": "revoked" })));
    assert_eq!(raw(&w.app, signed(&second, "GET", "/v1/inbox", "", None)).await, reference);
    assert_eq!(raw(&w.app, signed(&second, "GET", "/v1/whoami", "", None)).await, reference);
    let (status, _) = w.op("POST", "/keys/activate", json!({ "keyId": second.0, "fingerprint": sig::fingerprint(&second.1.verifying_key()) })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, keys) = w.get(&b.reseller, "/v1/keys").await;
    assert!(keys["keys"].as_array().unwrap().iter().any(|k| k["keyId"] == json!("reseller-x/second") && k["status"] == json!("revoked")));

    // A key may revoke itself.
    let (status, _) = w.post(&b.reseller, "/v1/keys/revoke", json!({ "keyId": "reseller-x/agent" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw(&w.app, signed(&b.reseller, "GET", "/v1/keys", "", None)).await, reference);

    // The operator revokes any key, pending ones included.
    let (status, _) = w.op("POST", "/keys/revoke", json!({ "keyId": "supplier-a/agent" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw(&w.app, signed(&b.supplier, "GET", "/v1/inbox", "", None)).await, reference);
    let pending = agent("supplier-a/pending", 42);
    let code = w.enroll_code("supplier-a", "text").await;
    assert_eq!(w.enroll(&code, "pending", &pending.1).await.status, StatusCode::CREATED);
    let (status, _) = w.op("POST", "/keys/revoke", json!({ "keyId": pending.0 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(raw(&w.app, signed(&pending, "GET", "/v1/whoami", "", None)).await, reference);
    let (status, _) = w.op("POST", "/keys/revoke", json!({ "keyId": "supplier-a/nobody" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
