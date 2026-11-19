//! The Rust verifier must accept exactly what the client's signer (partner-pack/p2p.mjs) produces, and refuse
//! every altered copy. Vectors: tests/vectors/sig-1.json, made by tools/gen-vectors.mjs.

use ed25519_dalek::{SigningKey, VerifyingKey};
use p2paecn_board::sig::{self, Failure, Request};
use p2paecn_board::store::Store;
use serde_json::Value;
use std::time::{Duration, Instant};

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/sig-1.json")).unwrap()
}

struct Case {
    method: String,
    host: String,
    path: String,
    body: String,
    key_id: String,
    created: String,
    nonce: String,
    digest: String,
    signature: String,
    idem: Option<String>,
    canonical: String,
}

fn cases(v: &Value) -> Vec<Case> {
    let s = |x: &Value| x.as_str().unwrap().to_owned();
    v["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let h = &c["headers"];
            Case {
                method: s(&c["method"]),
                host: s(&c["host"]),
                path: s(&c["pathAndQuery"]),
                body: s(&c["body"]),
                key_id: s(&h["p2p-key-id"]),
                created: s(&h["p2p-created"]),
                nonce: s(&h["p2p-nonce"]),
                digest: s(&h["content-digest"]),
                signature: s(&h["p2p-signature"]),
                idem: h.get("idempotency-key").map(s),
                canonical: s(&c["canonical"]),
            }
        })
        .collect()
}

fn check(c: &Case, v: &Value, edit: impl FnOnce(&mut Request<'_>), host: &str, now: u64, fresh_nonce: bool) -> Result<(), Failure> {
    let key = sig::public_key_from_b64url(v["publicKeyB64Url"].as_str().unwrap()).unwrap();
    let mut req = Request {
        method: &c.method,
        path_and_query: &c.path,
        key_id: Some(&c.key_id),
        created: Some(&c.created),
        nonce: Some(&c.nonce),
        digest: Some(&c.digest),
        signature: Some(&c.signature),
        idempotency_key: c.idem.as_deref(),
        body: c.body.as_bytes(),
    };
    edit(&mut req);
    sig::verify(&req, host, now, |id| (id == c.key_id).then_some(key), |_, _| fresh_nonce)
}

#[test]
fn accepts_the_reference_signer_and_matches_its_canonical_string() {
    let v = vectors();
    let now = v["nowUnix"].as_u64().unwrap();
    let all = cases(&v);
    assert_eq!(all.len(), 3);
    for c in &all {
        let canonical = sig::canonical_string(&c.method, &c.host, &c.path, &c.created, &c.nonce, &c.key_id, &c.digest, c.idem.as_deref().unwrap_or(""));
        assert_eq!(canonical, c.canonical);
        assert_eq!(sig::content_digest(c.body.as_bytes()), c.digest);
        assert_eq!(check(c, &v, |_| {}, &c.host, now, true), Ok(()));
    }
}

#[test]
fn the_fingerprint_matches_the_one_node_computes() {
    let v = vectors();
    let key = sig::public_key_from_b64url(v["publicKeyB64Url"].as_str().unwrap()).unwrap();
    assert_eq!(sig::fingerprint(&key), v["fingerprint"].as_str().unwrap());
}

#[test]
fn refuses_every_altered_copy() {
    let v = vectors();
    let now = v["nowUnix"].as_u64().unwrap();
    let all = cases(&v);
    let post = &all[2];
    let host = post.host.as_str();

    assert_eq!(check(post, &v, |r| r.body = b"{\"kind\":\"notice\"}", host, now, true), Err(Failure::DigestMismatch));
    assert_eq!(check(post, &v, |r| r.path_and_query = "/v1/rooms/room-b/cards", host, now, true), Err(Failure::BadSignature));
    assert_eq!(check(post, &v, |r| r.method = "PUT", host, now, true), Err(Failure::BadSignature));
    assert_eq!(check(post, &v, |r| r.idempotency_key = None, host, now, true), Err(Failure::BadSignature));
    assert_eq!(check(post, &v, |r| r.nonce = Some("DDDDDDDDDDDDDDDDDDDDDDDD"), host, now, true), Err(Failure::BadSignature));
    assert_eq!(check(post, &v, |_| {}, "other-board.test", now, true), Err(Failure::BadSignature));
    assert_eq!(check(post, &v, |r| r.signature = Some("not-a-signature"), host, now, true), Err(Failure::BadSignature));

    assert_eq!(check(post, &v, |_| {}, host, now + 301, true), Err(Failure::Stale));
    assert_eq!(check(post, &v, |_| {}, host, now - 301, true), Err(Failure::Stale));
    assert_eq!(check(post, &v, |_| {}, host, now + 300, true), Ok(()));

    assert_eq!(check(post, &v, |r| r.key_id = Some("someone/else"), host, now, true), Err(Failure::UnknownKey));
    assert_eq!(check(post, &v, |r| r.signature = None, host, now, true), Err(Failure::MissingHeader));
    assert_eq!(check(post, &v, |r| r.digest = Some(""), host, now, true), Err(Failure::MissingHeader));
    assert_eq!(check(post, &v, |r| r.created = Some("17948500"), host, now, true), Err(Failure::BadFormat));
    assert_eq!(check(post, &v, |r| r.nonce = Some("short"), host, now, true), Err(Failure::BadFormat));

    assert_eq!(check(post, &v, |_| {}, host, now, false), Err(Failure::Replay));
}

fn request(c: &Case) -> Request<'_> {
    Request {
        method: &c.method,
        path_and_query: &c.path,
        key_id: Some(&c.key_id),
        created: Some(&c.created),
        nonce: Some(&c.nonce),
        digest: Some(&c.digest),
        signature: Some(&c.signature),
        idempotency_key: c.idem.as_deref(),
        body: c.body.as_bytes(),
    }
}

/// A key id that is unknown, revoked or expired must cost the same signature check as one that is usable,
/// or the time of the 401 tells a caller with no key which `<org>/<name>` exists.
#[test]
fn an_unknown_key_takes_as_long_to_refuse_as_a_known_one() {
    let v = vectors();
    let all = cases(&v);
    let c = &all[2];
    let now = v["nowUnix"].as_u64().unwrap();
    // The request is signed by the vectors' key; checked against another key it fails like a forgery.
    // The seed is a fixed TEST value and signs nothing outside this test.
    let known: VerifyingKey = SigningKey::from_bytes(&[7; 32]).verifying_key();
    let time = |key: Option<VerifyingKey>| {
        let started = Instant::now();
        let verdict = sig::verify(&request(c), &c.host, now, |_| key, |_, _| true);
        (started.elapsed(), verdict)
    };
    let (mut known_times, mut unknown_times) = (Vec::new(), Vec::new());
    for _ in 0..201 {
        let (t, verdict) = time(Some(known));
        assert_eq!(verdict, Err(Failure::BadSignature));
        known_times.push(t);
        let (t, verdict) = time(None);
        assert_eq!(verdict, Err(Failure::UnknownKey));
        unknown_times.push(t);
    }
    let median = |mut t: Vec<Duration>| {
        t.sort();
        t[t.len() / 2]
    };
    let (known_median, unknown_median) = (median(known_times), median(unknown_times));
    eprintln!("median refusal: unknown key {unknown_median:?}, known key {known_median:?}");
    assert!(unknown_median * 2 >= known_median && known_median * 2 >= unknown_median, "unknown key {unknown_median:?}, known key {known_median:?}");
}

/// A request is fresh from `created - 300` to `created + 300`. Its nonce must be remembered for all of that.
#[test]
fn a_nonce_is_remembered_for_as_long_as_its_request_is_fresh() {
    let v = vectors();
    let all = cases(&v);
    let c = &all[0];
    let key = sig::public_key_from_b64url(v["publicKeyB64Url"].as_str().unwrap()).unwrap();
    let created: u64 = c.created.parse().unwrap();
    let mut store = Store::default();
    let mut at = |now: u64| sig::verify(&request(c), &c.host, now, |_| Some(key), |id, nonce| store.first_use(id, nonce, now, sig::NONCE_TTL_SECS));
    // First seen when the server's clock is as far behind the client's as allowed.
    assert_eq!(at(created - 300), Ok(()));
    assert_eq!(at(created - 300), Err(Failure::Replay));
    // The last second it is still fresh, the same bytes are still a replay.
    assert_eq!(at(created + 300), Err(Failure::Replay));
    assert_eq!(at(created + 301), Err(Failure::Stale));
}

/// Expired nonces are forgotten, and an expired one is fresh again (it can only come back on a stale request).
#[test]
fn the_nonce_table_forgets_expired_nonces() {
    let mut store = Store::default();
    assert!(store.first_use("k", "n1", 1000, 10));
    assert!(store.first_use("k", "n2", 1005, 10));
    assert!(!store.first_use("k", "n2", 1014, 10));
    assert!(store.first_use("other", "n1", 1009, 10));
    assert!(store.first_use("k", "n1", 1010, 10));
    assert!(!store.first_use("k", "n1", 1010, 10));
    assert!(store.first_use("k", "n3", 1100, 10));
    assert_eq!(store.nonce_count(), 1);
}

/// The partner pack ships its own copy of the vectors. Both are made by tools/gen-vectors.mjs and must not drift.
#[test]
fn the_partner_pack_vectors_are_the_test_vectors() {
    let ours = include_bytes!("vectors/sig-1.json");
    let theirs = include_bytes!("../partner-pack/vectors/sig-1.json");
    assert!(ours.as_slice() == theirs.as_slice(), "partner-pack/vectors/sig-1.json differs from tests/vectors/sig-1.json: copy it over");
}
