//! Helpers shared by the end-to-end tests: a board built from a bootstrap config, signed requests, the
//! operator router driven directly (no socket), and full responses for byte-for-byte comparison.
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use p2paecn_board::{api, operator, sig, Shared};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;
use uuid::Uuid;

pub const HOST: &str = "board.test";
static NONCE: AtomicU64 = AtomicU64::new(0);

/// A test agent: (key id, signing key). Seeds are fixed TEST values and sign nothing outside these tests.
pub struct Agent(pub String, pub SigningKey);

pub fn agent(id: &str, seed: u8) -> Agent {
    Agent(id.to_owned(), SigningKey::from_bytes(&[seed; 32]))
}

pub fn public_key(key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(key.verifying_key().as_bytes())
}

/// A bootstrap config entry for an active key. The org is the part of the id before the slash.
pub fn key_entry(a: &Agent, class: &str, expires: u64) -> Value {
    let org = a.0.split('/').next().unwrap();
    json!({ "id": a.0, "org": org, "class": class, "expires": expires, "public_key": public_key(&a.1) })
}

pub const FAR_FUTURE: u64 = 4102444800;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

pub struct World {
    pub app: Router,
    pub ops: Router,
    pub shared: Arc<Shared>,
}

pub fn world(config: Value) -> World {
    let shared = Arc::new(Shared::from_config_json(&config.to_string(), None).unwrap());
    World { app: api::router(shared.clone()), ops: operator::router(shared.clone()), shared }
}

pub fn signed_with(key_id: &str, key: &SigningKey, method: &str, path: &str, body: &str, idem: Option<&str>) -> Request<Body> {
    let created = now().to_string();
    let nonce = format!("test-nonce-{:013}", NONCE.fetch_add(1, Ordering::SeqCst));
    let digest = sig::content_digest(body.as_bytes());
    let canonical = sig::canonical_string(method, HOST, path, &created, &nonce, key_id, &digest, idem.unwrap_or(""));
    let signature = URL_SAFE_NO_PAD.encode(key.sign(canonical.as_bytes()).to_bytes());
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("p2p-key-id", key_id)
        .header("p2p-created", created)
        .header("p2p-nonce", nonce)
        .header("content-digest", digest)
        .header("p2p-signature", signature);
    if let Some(idem) = idem {
        req = req.header("idempotency-key", idem);
    }
    req.body(Body::from(body.to_owned())).unwrap()
}

pub fn signed(a: &Agent, method: &str, path: &str, body: &str, idem: Option<&str>) -> Request<Body> {
    signed_with(&a.0, &a.1, method, path, body, idem)
}

/// The same request again: method, path, every header and `body`, which must be the body it was signed over.
pub fn copy(req: &Request<Body>, body: &str) -> Request<Body> {
    let mut again = Request::builder().method(req.method()).uri(req.uri());
    for (name, value) in req.headers() {
        again = again.header(name, value);
    }
    again.body(Body::from(body.to_owned())).unwrap()
}

pub fn plain(method: &str, path: &str, body: &str) -> Request<Body> {
    Request::builder().method(method).uri(path).body(Body::from(body.to_owned())).unwrap()
}

/// A whole response: status, every header, and the body bytes.
#[derive(Debug, PartialEq)]
pub struct Raw {
    pub status: StatusCode,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub async fn raw(app: &Router, req: Request<Body>) -> Raw {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned())).collect();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    Raw { status, headers, body }
}

pub async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let r = raw(app, req).await;
    (r.status, String::from_utf8(r.body).unwrap())
}

fn json_of(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

impl World {
    /// What an unsigned request gets. Every "identical 401" is compared against this, byte for byte.
    pub async fn unauthorized(&self) -> Raw {
        let r = raw(&self.app, plain("GET", "/v1/inbox", "")).await;
        assert_eq!((r.status, r.body.as_slice()), (StatusCode::UNAUTHORIZED, b"unauthorized\n".as_slice()));
        r
    }

    pub async fn get(&self, a: &Agent, path: &str) -> (StatusCode, Value) {
        let (status, body) = send(&self.app, signed(a, "GET", path, "", None)).await;
        (status, json_of(&body))
    }

    pub async fn post(&self, a: &Agent, path: &str, body: Value) -> (StatusCode, Value) {
        let idem = Uuid::new_v4().to_string();
        let (status, text) = send(&self.app, signed(a, "POST", path, &body.to_string(), Some(&idem))).await;
        (status, json_of(&text))
    }

    /// A call on the operator channel, through its router directly.
    pub async fn op(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let body = if body.is_null() { String::new() } else { body.to_string() };
        let (status, text) = send(&self.ops, plain(method, path, &body)).await;
        (status, json_of(&text))
    }

    pub async fn enroll_code(&self, org: &str, class: &str) -> String {
        let (status, body) = self.op("POST", "/enroll-codes", json!({ "org": org, "class": class })).await;
        assert_eq!(status, StatusCode::CREATED);
        body["code"].as_str().unwrap().to_owned()
    }

    /// Enrolls `key` as `<org of the code>/<name>`, signed with the new key itself under the id `enroll`.
    pub async fn enroll(&self, code: &str, name: &str, key: &SigningKey) -> Raw {
        let body = json!({ "code": code, "name": name, "publicKey": public_key(key) }).to_string();
        raw(&self.app, signed_with("enroll", key, "POST", "/v1/enroll", &body, None)).await
    }

    /// Enrolls and activates a key, returning it as an agent.
    pub async fn active_agent(&self, org: &str, class: &str, name: &str, seed: u8) -> Agent {
        let a = agent(&format!("{org}/{name}"), seed);
        let code = self.enroll_code(org, class).await;
        let enrolled = self.enroll(&code, name, &a.1).await;
        assert_eq!(enrolled.status, StatusCode::CREATED);
        let fingerprint = sig::fingerprint(&a.1.verifying_key());
        let (status, _) = self.op("POST", "/keys/activate", json!({ "keyId": a.0, "fingerprint": fingerprint })).await;
        assert_eq!(status, StatusCode::OK);
        a
    }
}
