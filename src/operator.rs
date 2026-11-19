//! Operator channel: HTTP over a Unix socket. There is no token: the socket file's mode (0600, owner only) is
//! the control. None of these routes exist on the public listener. Errors are `{"error": code}` with a 4xx status,
//! and `500 state_not_saved` when a state file is in use and the change could not be saved (it is then undone).
//!
//! The operator creates organisations and rooms, issues one-time enrollment codes, and activates a pending key
//! only after a person on the member's side has read its fingerprint over a channel both already trust.

use crate::api::{now_unix, refuse, reply, MAX_BODY};
use crate::store::{EnrollCode, KeyStatus, Refusal, RoomKind, Store, NOT_FOUND};
use crate::{sig, store, KeyClass, Shared};
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::sync::{Arc, MutexGuard};
use tokio::net::UnixListener;
use uuid::Uuid;

type App = State<Arc<Shared>>;
type Body = Result<Bytes, BytesRejection>;
type Outcome = Result<(StatusCode, Value), Refusal>;

const UNPROCESSABLE: StatusCode = StatusCode::UNPROCESSABLE_ENTITY;
const CODE_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/state", get(state))
        .route("/orgs", post(add_org))
        .route("/rooms", post(add_room))
        .route("/rooms/members", post(add_member))
        .route("/rooms/members/remove", post(remove_member))
        .route("/enroll-codes", post(enroll_code))
        .route("/keys/activate", post(activate_key))
        .route("/keys/revoke", post(revoke_key))
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(shared)
}

/// Removes a stale socket file at `path`, binds, and sets the mode to 0600 before anything is accepted.
/// Refuses to remove anything at `path` that is not a socket, or a socket another process still listens on.
pub fn bind(path: &std::path::Path) -> std::io::Result<UnixListener> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() && std::os::unix::net::UnixStream::connect(path).is_ok() => {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "another process is listening on it"))
        }
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "exists and is not a socket")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Codes are compared after lower-casing and removing spaces, and stored only as this hash.
pub fn code_hash(code: &str) -> [u8; 32] {
    let normal: String = code.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    Sha256::digest(normal.as_bytes()).into()
}

/// `p2p-` and 24 characters of a-z2-7 in groups of four: 120 random bits from the operating system.
fn new_code() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 15];
    getrandom::fill(&mut bytes)?;
    let bits = bytes.iter().fold(0u128, |acc, b| acc << 8 | u128::from(*b));
    let chars: Vec<u8> = (0..24).rev().map(|i| CODE_ALPHABET[(bits >> (i * 5)) as usize & 31]).collect();
    let groups: Vec<&str> = chars.chunks(4).map(|g| std::str::from_utf8(g).unwrap_or_default()).collect();
    Ok(format!("p2p-{}", groups.join("-")))
}

// ---------------------------------------------------------------- plumbing

async fn not_found() -> Response {
    refuse(NOT_FOUND)
}

fn answer(outcome: Outcome) -> Response {
    match outcome {
        Ok((status, body)) => reply(status, body),
        Err(refusal) => refuse(refusal),
    }
}

fn input(body: &Body) -> Result<Value, Refusal> {
    body.as_ref()
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .filter(Value::is_object)
        .ok_or((StatusCode::BAD_REQUEST, "bad_json"))
}

/// A string field, or "" when absent, so the store's own checks name the problem.
fn text<'a>(input: &'a Value, name: &str) -> &'a str {
    input.get(name).and_then(Value::as_str).unwrap_or("")
}

/// An optional whole number in `range`, else `default`.
fn number(input: &Value, name: &str, range: std::ops::RangeInclusive<u64>, default: u64) -> Result<u64, Refusal> {
    match input.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v.as_u64().filter(|n| range.contains(n)).ok_or((UNPROCESSABLE, "bad_ttl")),
    }
}

fn lock(s: &Shared) -> MutexGuard<'_, Store> {
    s.store.lock().unwrap()
}

// ---------------------------------------------------------------- routes

/// Everything the operator needs to see, never an enrollment code itself.
async fn state(State(s): App) -> Response {
    let store = lock(&s);
    let rooms: Vec<Value> = store
        .rooms
        .values()
        .map(|r| {
            let members: Vec<Value> =
                r.members.iter().map(|m| json!({ "org": m.org, "handle": m.handle, "removed": m.removed })).collect();
            json!({ "id": r.id, "kind": r.kind.as_str(), "members": members })
        })
        .collect();
    let keys: Vec<Value> = store
        .keys
        .iter()
        .map(|(id, k)| {
            json!({ "id": id, "org": k.org, "class": k.class.as_str(), "status": k.status.as_str(),
                    "fingerprint": sig::fingerprint(&k.key), "created": k.created, "expires": k.expires,
                    "role": k.role, "label": k.label })
        })
        .collect();
    let codes: Vec<Value> = store
        .codes
        .iter()
        .map(|c| json!({ "codeId": c.id, "org": c.org, "class": c.class.as_str(), "expires": c.expires, "used": c.used.is_some(),
                         "role": c.role, "label": c.label }))
        .collect();
    reply(StatusCode::OK, json!({ "orgs": store.orgs, "rooms": rooms, "keys": keys, "codes": codes }))
}

async fn add_org(State(s): App, body: Body) -> Response {
    answer((|| {
        let input = input(&body)?;
        let id = text(&input, "id");
        let mut store = lock(&s);
        let before = s.snapshot(&store);
        store.add_org(id)?;
        s.commit(&mut store, before)?;
        Ok((StatusCode::CREATED, json!({ "id": id })))
    })())
}

async fn add_room(State(s): App, body: Body) -> Response {
    answer((|| {
        let input = input(&body)?;
        let id = text(&input, "id");
        let kind = match text(&input, "kind") {
            "pair" => RoomKind::Pair,
            "shared" => RoomKind::Shared,
            _ => return Err((UNPROCESSABLE, "bad_kind")),
        };
        let bad_members = (UNPROCESSABLE, "bad_members");
        let mut members = Vec::new();
        for m in input.get("members").and_then(Value::as_array).ok_or(bad_members)? {
            let org = m.get("org").and_then(Value::as_str).ok_or(bad_members)?;
            let handle = match m.get("handle") {
                None | Some(Value::Null) => None,
                Some(h) => Some(h.as_str().ok_or((UNPROCESSABLE, "bad_handle"))?.to_owned()),
            };
            members.push((org.to_owned(), handle));
        }
        let mut store = lock(&s);
        let before = s.snapshot(&store);
        store.add_room(id, kind, &members)?;
        s.commit(&mut store, before)?;
        Ok((StatusCode::CREATED, json!({ "id": id, "kind": kind.as_str() })))
    })())
}

async fn add_member(State(s): App, body: Body) -> Response {
    answer((|| {
        let input = input(&body)?;
        let (room, org, handle) = (text(&input, "room"), text(&input, "org"), text(&input, "handle"));
        let mut store = lock(&s);
        let before = s.snapshot(&store);
        store.add_member(room, org, handle)?;
        s.commit(&mut store, before)?;
        Ok((StatusCode::CREATED, json!({ "room": room, "org": org, "handle": handle })))
    })())
}

async fn remove_member(State(s): App, body: Body) -> Response {
    answer((|| {
        let input = input(&body)?;
        let (room, org) = (text(&input, "room"), text(&input, "org"));
        let mut store = lock(&s);
        let before = s.snapshot(&store);
        store.remove_member(room, org, now_unix())?;
        s.commit(&mut store, before)?;
        Ok((StatusCode::OK, json!({ "room": room, "org": org, "removed": true })))
    })())
}

/// An optional string field: absent or null is `None`; anything else must pass `valid`, else `refusal`.
fn optional(input: &Value, name: &str, valid: impl Fn(&str) -> bool, refusal: Refusal) -> Result<Option<String>, Refusal> {
    match input.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_str().filter(|s| valid(s)).map(|s| Some(s.to_owned())).ok_or(refusal),
    }
}

/// The code is returned once, here, and never again. Only its hash is kept. A `viewer` code may carry a `role`
/// (`admin` or `manager`) and a `label`, which the enrolled key keeps; any other class with either is refused.
async fn enroll_code(State(s): App, body: Body) -> Response {
    answer((|| {
        let input = input(&body)?;
        let org = text(&input, "org");
        let class = KeyClass::parse(text(&input, "class")).ok_or((UNPROCESSABLE, "bad_class"))?;
        let role = optional(&input, "role", |r| store::VIEWER_ROLES.contains(&r), (UNPROCESSABLE, "bad_role"))?;
        let label = optional(&input, "label", store::is_label, (UNPROCESSABLE, "bad_label"))?;
        if class != KeyClass::Viewer && (role.is_some() || label.is_some()) {
            return Err((UNPROCESSABLE, "role_and_label_are_for_viewer_keys"));
        }
        let ttl_hours = number(&input, "ttlHours", 1..=72, 24)?;
        let key_ttl_days = number(&input, "keyTtlDays", 1..=90, 30)?;
        let mut store = lock(&s);
        if !store.orgs.contains(org) {
            return Err((UNPROCESSABLE, "unknown_org"));
        }
        let code = new_code().map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "no_randomness"))?;
        let id = Uuid::new_v4().to_string();
        let expires = now_unix() + ttl_hours * 3600;
        let before = s.snapshot(&store);
        store.codes.push(EnrollCode { id: id.clone(), hash: code_hash(&code), org: org.to_owned(), class, expires, key_ttl_days, used: None, role, label });
        // A code that could not be saved is never shown: it would not survive a restart.
        s.commit(&mut store, before)?;
        Ok((StatusCode::CREATED, json!({ "codeId": id, "code": code, "expires": expires })))
    })())
}

/// Pending to active, only when the fingerprint the member read out matches (case and dashes ignored).
async fn activate_key(State(s): App, body: Body) -> Response {
    answer((|| {
        let input = input(&body)?;
        let key_id = text(&input, "keyId");
        let normal = |f: &str| f.chars().filter(|c| *c != '-').collect::<String>().to_lowercase();
        let mut store = lock(&s);
        let key = store.keys.get(key_id).ok_or(NOT_FOUND)?;
        if key.status != KeyStatus::Pending {
            return Err((StatusCode::CONFLICT, "not_pending"));
        }
        if normal(text(&input, "fingerprint")) != normal(&sig::fingerprint(&key.key)) {
            return Err((StatusCode::CONFLICT, "fingerprint_mismatch"));
        }
        let before = s.snapshot(&store);
        store.keys.get_mut(key_id).ok_or(NOT_FOUND)?.status = KeyStatus::Active;
        s.commit(&mut store, before)?;
        Ok((StatusCode::OK, json!({ "keyId": key_id, "status": "active" })))
    })())
}

/// Any status to revoked. Final.
async fn revoke_key(State(s): App, body: Body) -> Response {
    answer((|| {
        let input = input(&body)?;
        let key_id = text(&input, "keyId");
        let mut store = lock(&s);
        let before = s.snapshot(&store);
        store.keys.get_mut(key_id).ok_or(NOT_FOUND)?.status = KeyStatus::Revoked;
        s.commit(&mut store, before)?;
        Ok((StatusCode::OK, json!({ "keyId": key_id, "status": "revoked" })))
    })())
}
