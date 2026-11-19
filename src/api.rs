//! Public HTTP surface. Closed by default: `GET /` is a static page for people (src/landing.html: what the
//! network is and that sign-in is by key; no forms or scripts, one link, to `/app`), `GET /og.png` is its share
//! image, `GET /llms.txt` a plain-text summary for AI agents, and `GET /app`, `/app.js` and `/app.css` the read-only
//! web view for invited people, which signs its requests in the browser like any agent; every other request
//! without a valid signature gets one identical 401. The reason for a refusal is logged on the server, never
//! returned.
//!
//! Rules enforced here:
//! - an agent acts only as its own organisation, only in rooms that organisation is in;
//! - a pending key may ask "who am I" and nothing else;
//! - a `viewer` key reads what its organisation may read and writes nothing: `403 read_only_key`;
//! - in a pair room only the organisation that reported a finding may close it as verified; in a shared room
//!   only the reporter closes its own card, and nobody else moves it;
//! - in a shared room members see each other by handle only, never by org id or key id;
//! - members of a room see which of its members have an agent key that can act now (`joined`), nothing more
//!   about each other's keys;
//! - a `fields` key never receives text written by another organisation;
//! - every write passes the content gate and carries an idempotency key;
//! - every accepted card, message and stage move appends to its room's chain (`src/chain.rs`) in the same change,
//!   after the last check that can refuse it. A replayed write returns before it and appends nothing.

use crate::store::{AgentKey, Card, Enrolled, KeyStatus, Message, Refusal, Replay, RoomKind, Store, NOT_FOUND};
use crate::{chain, gate, operator, sig, store, KeyClass, Shared};
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{DefaultBodyLimit, Extension, Path, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, HeaderName, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::serve::Listener;
use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use uuid::Uuid;

pub const MAX_BODY: usize = 64 * 1024;
/// A thread takes no more replies, proof, corrections or open-stage moves once it holds this many messages, the
/// report included. The closing move and each member's first ack still go in, so it can grow past this by those.
pub const MAX_THREAD: usize = 500;
/// A client sends a whole request head within this time, between requests too.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// A request, body included, is answered within this time.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const X_ROBOTS: HeaderName = HeaderName::from_static("x-robots-tag");
const OPENER_POLICY: HeaderName = HeaderName::from_static("cross-origin-opener-policy");
const NOINDEX: &str = "noindex, nofollow, noarchive";
const STAGES: [&str; 6] = ["reported", "acknowledged", "need-info", "shipped", "closed", "posted"];

type App = State<Arc<Shared>>;
type Body = Result<Bytes, BytesRejection>;
type Param = Result<Path<String>, PathRejection>;

/// Every route starts from `any(closed)`: a wrong method gets the same 401 as everything else. (A plain
/// `get(..)` with `method_not_allowed_fallback` would add an `Allow` header naming the methods that exist.)
/// `get(..)` also answers HEAD unless `head(..)` is set, so every GET route closes HEAD explicitly.
pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/", any(closed).get(sign_in).head(closed))
        .route("/og.png", any(closed).get(share_image).head(closed))
        .route("/llms.txt", any(closed).get(llms_txt).head(closed))
        .route("/app", any(closed).get(app_page).head(closed))
        .route("/app.js", any(closed).get(app_script).head(closed))
        .route("/app.css", any(closed).get(app_styles).head(closed))
        .route("/v1/enroll", any(closed).post(enroll))
        .route("/v1/whoami", any(closed).get(whoami).head(closed))
        .route("/v1/rooms", any(closed).get(rooms).head(closed))
        .route("/v1/inbox", any(closed).get(inbox).head(closed))
        .route("/v1/inbox/wait", any(closed).get(inbox_wait).head(closed))
        .route("/v1/rooms/{room}/cards", any(closed).get(list_cards).head(closed).post(create_card))
        .route("/v1/rooms/{room}/chain", any(closed).get(room_chain).head(closed))
        .route("/v1/cards/{id}", any(closed).get(get_card).head(closed))
        .route("/v1/cards/{id}/messages", any(closed).post(post_message))
        .route("/v1/cards/{id}/stage", any(closed).post(set_stage))
        .route("/v1/keys", any(closed).get(list_keys).head(closed))
        .route("/v1/keys/revoke", any(closed).post(revoke_key))
        .fallback(closed)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(shared.clone(), unsigned_guard))
        .with_state(shared)
}

/// The address of the TCP peer of a connection, put on each of its requests by `serve`.
#[derive(Clone, Copy)]
pub struct Peer(pub std::net::IpAddr);

tokio::task_local! {
    /// Set while a request from a source address that is over its failure limit is handled: its refusal is not
    /// written to the log line by line.
    static QUIET: bool;
}

/// The limit on unsigned traffic, per source address: enroll attempts and requests that end in the 401. A caller
/// over it is told nothing. It gets the same 401 as before, byte for byte. What changes is on the server's side:
///
/// - an enroll attempt from that address is answered with the 401 at once, before its code or signature is
///   looked at, until the window ends. That bounds how many codes one address can try;
/// - a refusal from that address is no longer logged line by line. One line says that the limit was reached.
///   That bounds what one address can write into the log.
///
/// Signatures of other requests are still checked: a member behind the same address, or behind the same proxy,
/// keeps working. So the limit does not bound the cost of checking signatures, and it does not close or slow
/// connections. The 401 of a failed check costs the same as before.
async fn unsigned_guard(State(s): App, peer: Option<Extension<Peer>>, req: Request, next: Next) -> Response {
    let source = s.limits.source(req.headers(), peer.map(|Extension(Peer(ip))| ip));
    let now = now_unix();
    let over = s.limits.unsigned_over(&source, now);
    if over && req.uri().path() == "/v1/enroll" {
        return unauthorized();
    }
    let response = QUIET.scope(over, next.run(req)).await;
    if response.status() == StatusCode::UNAUTHORIZED && s.limits.unsigned_failed(&source, now) == crate::rate::Failed::ReachedLimit {
        eprintln!(
            "refused: {} requests from the source address {source} failed within {} s; until that time has passed its enroll attempts are not looked at and its refusals are not logged",
            s.limits.config.unsigned, s.limits.config.window
        );
    }
    response
}

/// Serves the public listener. `axum::serve` sets no timeouts, so anyone could hold connections open by sending
/// nothing. Here a request head must arrive within `head` (also while a kept-alive connection waits for the next
/// request), and a request, body included, is answered within `whole`, else it gets the 401.
pub async fn serve(mut listener: TcpListener, app: Router, head: Duration, whole: Duration) -> std::io::Result<()> {
    let app = app.layer(middleware::from_fn(move |req: Request, next: Next| async move {
        let method = req.method().clone();
        tokio::time::timeout(whole, next.run(req)).await.unwrap_or_else(|_| refused("request_timeout", &method))
    }));
    loop {
        let (io, peer) = Listener::accept(&mut listener).await;
        // Each request of the connection carries the peer's address, for the limit on unsigned traffic.
        let service = TowerToHyperService::new(app.clone().layer(Extension(Peer(peer.ip()))));
        tokio::spawn(async move {
            let connection = http1::Builder::new().timer(TokioTimer::new()).header_read_timeout(head).serve_connection(TokioIo::new(io), service);
            let _ = connection.await;
        });
    }
}

// ---------------------------------------------------------------- responses

/// No route, or a method the route does not have.
async fn closed(method: Method) -> Response {
    refused("no_route", &method)
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, [(X_ROBOTS, NOINDEX), (CACHE_CONTROL, "no-store")], "unauthorized\n").into_response()
}

/// The page at `GET /`, compiled in. It names no member; its one link goes to the web view at `/app`.
const LANDING: &str = include_str!("landing.html");
/// The page needs inline styles and its data: icon, nothing else.
const LANDING_CSP: &str =
    "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

async fn sign_in() -> Response {
    (
        StatusCode::OK,
        [
            (X_ROBOTS, NOINDEX),
            (CACHE_CONTROL, "no-store"),
            (CONTENT_TYPE, "text/html; charset=utf-8"),
            (CONTENT_SECURITY_POLICY, LANDING_CSP),
            (REFERRER_POLICY, "no-referrer"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        LANDING,
    )
        .into_response()
}

/// The share image the page's `og:image` and `twitter:image` tags name by its path (1200x630), compiled in. The
/// page does not know its own public address, so the tags carry `/og.png` and no host: a preview service that wants
/// an absolute address needs the tags changed for that board (see deploy/README.md).
const OG_PNG: &[u8] = include_bytes!("og.png");
/// What the network is and how access works, for AI agents; compiled in. Names no member and no address.
const LLMS_TXT: &str = include_str!("llms.txt");

/// Public and cacheable, unlike the page: neither changes without a new build, and neither carries `x-robots-tag`.
async fn share_image() -> Response {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "image/png"), (CACHE_CONTROL, "public, max-age=86400"), (X_CONTENT_TYPE_OPTIONS, "nosniff")],
        OG_PNG,
    )
        .into_response()
}

/// The web view for invited people, compiled in: an HTML shell, one script and one stylesheet, nothing else. The
/// script makes and keeps the browser's own key and signs every request (`p2paecn-sig-1`); the server treats it
/// as any other key. The shell has no inline script or style, so the policy allows only these two files.
const APP_HTML: &str = include_str!("app.html");
const APP_JS: &str = include_str!("app.js");
const APP_CSS: &str = include_str!("app.css");
const APP_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn app_file(content_type: &'static str, csp: Option<&'static str>, body: &'static str) -> Response {
    let mut response = (
        StatusCode::OK,
        [
            (CONTENT_TYPE, content_type),
            (CACHE_CONTROL, "no-store"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(X_ROBOTS, axum::http::HeaderValue::from_static(NOINDEX));
    if let Some(csp) = csp {
        headers.insert(CONTENT_SECURITY_POLICY, axum::http::HeaderValue::from_static(csp));
        // The page is alone in its browsing context group: a page of another origin that opened it, or that it
        // was opened from, holds no handle to its window and so cannot steer its address.
        headers.insert(OPENER_POLICY, axum::http::HeaderValue::from_static("same-origin"));
    }
    response
}

async fn app_page() -> Response {
    app_file("text/html; charset=utf-8", Some(APP_CSP), APP_HTML)
}

async fn app_script() -> Response {
    app_file("text/javascript; charset=utf-8", None, APP_JS)
}

async fn app_styles() -> Response {
    app_file("text/css; charset=utf-8", None, APP_CSS)
}

async fn llms_txt() -> Response {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/plain; charset=utf-8"), (CACHE_CONTROL, "public, max-age=3600"), (X_CONTENT_TYPE_OPTIONS, "nosniff")],
        LLMS_TXT,
    )
        .into_response()
}

pub(crate) fn reply(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(X_ROBOTS, NOINDEX), (CACHE_CONTROL, "no-store"), (CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

pub(crate) fn refuse((status, code): Refusal) -> Response {
    reply(status, json!({ "error": code }))
}

// ---------------------------------------------------------------- who is asking

struct Agent {
    key_id: String,
    org: String,
    class: KeyClass,
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn signed_request<'a>(method: &'a Method, uri: &'a Uri, headers: &'a HeaderMap, body: &'a [u8]) -> sig::Request<'a> {
    sig::Request {
        method: method.as_str(),
        path_and_query: uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"),
        key_id: header(headers, "p2p-key-id"),
        created: header(headers, "p2p-created"),
        nonce: header(headers, "p2p-nonce"),
        digest: header(headers, "content-digest"),
        signature: header(headers, "p2p-signature"),
        idempotency_key: header(headers, "idempotency-key"),
        body,
    }
}

fn refused(reason: &str, method: &Method) -> Response {
    if !QUIET.try_with(|quiet| *quiet).unwrap_or(false) {
        eprintln!("refused: {reason} {method}");
    }
    unauthorized()
}

/// A signed, authenticated caller over a rate limit: `429` with `Retry-After` in seconds, the same number in the
/// body. Never sent to a caller whose signature did not verify.
fn rate_limited(retry_after: u64) -> Response {
    let mut response = reply(StatusCode::TOO_MANY_REQUESTS, json!({ "error": "rate_limited", "retryAfter": retry_after }));
    response.headers_mut().insert(axum::http::header::RETRY_AFTER, axum::http::HeaderValue::from(retry_after));
    response
}

/// A `viewer` key's answer to anything but a read.
pub const READ_ONLY: Refusal = (StatusCode::FORBIDDEN, "read_only_key");

/// Verifies the signature over the exact bytes received. Every failure is the same 401. A key authenticates
/// when it is known, unexpired and pending or active; a pending key only where `allow_pending` is set.
///
/// Every signed route passes through here, so this is where a `viewer` key is kept read-only: every read route is
/// a `GET` and every write a `POST`, and a viewer's request with any method but `GET` is refused here with `403
/// read_only_key`, before the route looks at its target or body. A write route added later is covered without
/// a change here. (A pending viewer gets the 401 first, as any pending key.)
fn authenticate(s: &Shared, method: &Method, uri: &Uri, headers: &HeaderMap, body: &Body, allow_pending: bool) -> Result<(Agent, Bytes), Response> {
    let body = body.as_ref().map_err(|_| refused("body_rejected", method))?.clone();
    let now = now_unix();
    let request = signed_request(method, uri, headers, &body);
    // Copied out under a short lock: the signature is checked without holding it.
    let known = request.key_id.and_then(|id| {
        let store = s.store.lock().unwrap();
        let k = store.keys.get(id).filter(|k| k.expires > now && k.status != KeyStatus::Revoked)?;
        Some((Agent { key_id: id.to_owned(), org: k.org.clone(), class: k.class }, k.key, k.status))
    });
    let mut limited = None;
    let verdict = sig::verify_after(
        &request,
        &s.host,
        now,
        s.not_before,
        |_| known.as_ref().map(|(_, key, _)| *key),
        // Reached only by a request whose signature verified. A key over its limit is stopped here, before its
        // nonce is recorded: the nonce is not used up, so the very same request can be sent again later.
        |id, nonce| match s.limits.key_request(id, now) {
            Ok(()) => s.store.lock().unwrap().first_use(id, nonce, now, sig::NONCE_TTL_SECS),
            Err(retry_after) => {
                limited = Some(retry_after);
                true
            }
        },
    );
    if let Err(failure) = verdict {
        return Err(refused(failure.reason(), method));
    }
    match known {
        Some((_, _, KeyStatus::Pending)) if !allow_pending => Err(refused("pending_key", method)),
        Some(_) if limited.is_some() => Err(rate_limited(limited.unwrap_or(1))),
        Some((agent, _, _)) if agent.class == KeyClass::Viewer && method != Method::GET => {
            eprintln!("refused: read_only_key {method}");
            Err(refuse(READ_ONLY))
        }
        Some((agent, _, _)) => Ok((agent, body)),
        None => Err(refused("unknown_or_inactive_key", method)),
    }
}

macro_rules! authenticated {
    ($s:expr, $method:expr, $uri:expr, $headers:expr, $body:expr) => {
        match authenticate(&$s, &$method, &$uri, &$headers, &$body, false) {
            Ok(ok) => ok,
            Err(r) => return r,
        }
    };
}

fn in_room(store: &Store, agent: &Agent, room: &str) -> bool {
    store.rooms.get(room).is_some_and(|r| r.is_member(&agent.org))
}

// ---------------------------------------------------------------- views

/// A `fields` key gets no free text written by another organisation. A `text` key and a `viewer` key (a person
/// reading in a browser) get it, marked `untrusted`.
fn may_read_text(agent: &Agent, author_org: &str) -> bool {
    matches!(agent.class, KeyClass::Text | KeyClass::Viewer) || agent.org == author_org
}

/// The name other members see for `org` in `room`. Never an org id in a shared room.
fn display<'a>(store: &'a Store, room: &str, org: &'a str) -> &'a str {
    store.rooms.get(room).map_or("unknown", |r| r.display_name(org))
}

/// `messages` is the length of the card's thread (`Store::thread_length`), counted by the caller so that a list
/// of cards counts every thread in one pass over the messages (`card_views`).
fn card_view(store: &Store, card: &Card, agent: &Agent, messages: usize) -> Value {
    let mine = card.reporter_org == agent.org;
    json!({
        "id": card.id,
        "room": card.room,
        "kind": card.kind,
        "severity": card.severity,
        "title": may_read_text(agent, &card.reporter_org).then_some(&card.title),
        "reporter": display(store, &card.room, &card.reporter_org),
        "mine": mine,
        "stage": card.stage,
        "outcome": card.outcome,
        "created": card.created,
        "updated": card.updated,
        "untrusted": !mine,
        "messages": messages,
    })
}

/// The views of `cards`, in their order, each with its thread length.
fn card_views(store: &Store, cards: &[&Card], agent: &Agent) -> Vec<Value> {
    let mut lengths: HashMap<Uuid, usize> = cards.iter().map(|c| (c.id, 0)).collect();
    for m in &store.messages {
        if let Some(n) = lengths.get_mut(&m.card) {
            *n += 1;
        }
    }
    cards.iter().map(|c| card_view(store, c, agent, lengths[&c.id])).collect()
}

fn message_view(store: &Store, room: &str, m: &Message, agent: &Agent) -> Value {
    let mine = m.org == agent.org;
    json!({
        "id": m.id,
        "card": m.card,
        "author": display(store, room, &m.org),
        "mine": mine,
        "kind": m.kind,
        "text": may_read_text(agent, &m.org).then_some(&m.text),
        "replaces": m.replaces,
        "created": m.created,
        "untrusted": !mine,
    })
}

/// Each room of the caller's organisation. `members` is the caller first, then the members in `others`, in its
/// order, each under the same display name. `joined` says that the member's organisation has an agent key that can
/// act now (`Store::orgs_with_agent_key`). It is a fact about the whole organisation, so in a shared room only the
/// caller's own entry carries it: on another member's entry it would change in the same read as that organisation's
/// entry in a pair room, and so tell which handle is which organisation. A removed member is not listed.
fn room_views(store: &Store, agent: &Agent) -> Vec<Value> {
    let joined = store.orgs_with_agent_key(now_unix());
    store
        .rooms
        .values()
        .filter(|r| r.is_member(&agent.org))
        .map(|r| {
            let shared = matches!(r.kind, RoomKind::Shared);
            let caller = std::iter::once((agent.org.as_str(), true));
            let members: Vec<Value> = caller
                .chain(r.other_members(&agent.org).map(|m| (m.org.as_str(), false)))
                .map(|(org, you)| {
                    if shared && !you {
                        json!({ "name": r.display_name(org), "you": you })
                    } else {
                        json!({ "name": r.display_name(org), "you": you, "joined": joined.contains(org) })
                    }
                })
                .collect();
            json!({ "id": r.id, "kind": r.kind.as_str(), "you": r.display_name(&agent.org), "others": r.others(&agent.org), "members": members,
                    "chain": head_view(store, &r.id) })
        })
        .collect()
}

/// The head of a room's chain: the same for every member of the room.
pub(crate) fn head_view(store: &Store, room: &str) -> Value {
    let (seq, hash) = store.chain_head(room);
    json!({ "seq": seq, "hash": hash })
}

fn key_view(key_id: &str, k: &AgentKey) -> Value {
    json!({
        "keyId": key_id,
        "class": k.class.as_str(),
        "status": k.status.as_str(),
        "fingerprint": sig::fingerprint(&k.key),
        "created": k.created,
        "expires": k.expires,
    })
}

// ---------------------------------------------------------------- enrollment

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollBody {
    code: String,
    name: String,
    #[serde(rename = "publicKey")]
    public_key: String,
}

/// The only write not signed by a known key: the request proves possession of the new key's private half.
async fn enroll(State(s): App, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    match enroll_with_code(&s, &method, &uri, &headers, &body) {
        Ok(response) => response,
        Err(reason) => refused(reason, &method),
    }
}

/// What enrollment answers, for the key as it is now.
fn enrolled_view(key_id: &str, k: &AgentKey) -> Value {
    json!({
        "keyId": key_id,
        "org": k.org,
        "class": k.class.as_str(),
        "status": k.status.as_str(),
        "fingerprint": sig::fingerprint(&k.key),
        "expires": k.expires,
    })
}

/// `Err` is a failure before the code is proven valid. Every such failure is the identical 401, so a wrong,
/// used or expired code and a bad proof cannot be told apart. Later refusals do not consume the code.
fn enroll_with_code(s: &Shared, method: &Method, uri: &Uri, headers: &HeaderMap, body: &Body) -> Result<Response, &'static str> {
    let body = body.as_ref().map_err(|_| "body_rejected")?;
    let request = signed_request(method, uri, headers, body);
    if request.idempotency_key.is_some() {
        return Err("idempotency_key_on_enroll");
    }
    let input: EnrollBody = serde_json::from_slice(body).map_err(|_| "enroll_body_shape")?;
    let public_key = sig::public_key_from_b64url(&input.public_key).ok_or("enroll_public_key")?;
    let now = now_unix();
    // Everyone has a key for the id `enroll`, so its nonce is recorded below, and only for a valid code:
    // a caller without one leaves nothing behind.
    sig::verify_after(&request, &s.host, now, s.not_before, |id| (id == "enroll").then_some(public_key), |_, _| true)
        .map_err(sig::Failure::reason)?;

    let mut store = s.store.lock().unwrap();
    let hash = operator::code_hash(&input.code);
    let index = store.codes.iter().position(|c| c.hash == hash).ok_or("unknown_code")?;
    let code = &store.codes[index];
    if code.expires <= now {
        return Err("expired_code");
    }
    let (org, class, key_ttl_days, code_id) = (code.org.clone(), code.class, code.key_ttl_days, code.id.clone());
    let (role, label) = (code.role.clone(), code.label.clone());
    let replay = match &code.used {
        // A client that lost the response retries with the same key and name.
        Some(used) if used.public_key == *public_key.as_bytes() && used.name == input.name => true,
        Some(_) => return Err("used_code"),
        None => false,
    };
    if !store.first_use("enroll", request.nonce.unwrap_or_default(), now, sig::NONCE_TTL_SECS) {
        return Err("nonce_replayed");
    }
    let key_id = format!("{org}/{}", input.name);
    if replay {
        // Told the key's status now. A revoked or expired key gets the 401, as it does everywhere else.
        let key = store.keys.get(&key_id).filter(|k| k.status != KeyStatus::Revoked && k.expires > now).ok_or("enrolled_key_inactive")?;
        return Ok(reply(StatusCode::CREATED, enrolled_view(&key_id, key)));
    }

    // These refusals reached a valid code, so they are logged.
    let refusal = |refusal: Refusal| {
        eprintln!("enroll refused: {} for code {code_id}", refusal.1);
        Ok(refuse(refusal))
    };
    if !store::is_key_name(&input.name) {
        return refusal((StatusCode::UNPROCESSABLE_ENTITY, "bad_name"));
    }
    // Asked before the name: the answer must not depend on which names the code's org already uses.
    if store.keys.values().any(|k| k.key == public_key) {
        return refusal((StatusCode::CONFLICT, "key_already_enrolled"));
    }
    if store.keys.contains_key(&key_id) {
        return refusal((StatusCode::CONFLICT, "key_id_taken"));
    }
    let key = AgentKey { org, class, key: public_key, status: KeyStatus::Pending, created: now, expires: now + key_ttl_days * 86_400, role, label };
    let response = enrolled_view(&key_id, &key);
    let before = s.snapshot(&store);
    store.keys.insert(key_id, key);
    store.codes[index].used = Some(Enrolled { public_key: *public_key.as_bytes(), name: input.name });
    // Past the code, so a failed save is the JSON 500, not the 401; the code is unused again after the undo.
    if let Err(refusal) = s.commit(&mut store, before) {
        return Ok(refuse(refusal));
    }
    Ok(reply(StatusCode::CREATED, response))
}

// ---------------------------------------------------------------- reads

async fn whoami(State(s): App, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = match authenticate(&s, &method, &uri, &headers, &body, true) {
        Ok(ok) => ok,
        Err(r) => return r,
    };
    let store = s.store.lock().unwrap();
    let Some(key) = store.keys.get(&agent.key_id) else {
        return refused("key_gone", &method);
    };
    let rooms = if key.status == KeyStatus::Active { room_views(&store, &agent) } else { Vec::new() };
    reply(
        StatusCode::OK,
        json!({
            "keyId": agent.key_id,
            "org": agent.org,
            "class": key.class.as_str(),
            "status": key.status.as_str(),
            "fingerprint": sig::fingerprint(&key.key),
            "expires": key.expires,
            "role": key.role,
            "label": key.label,
            "serverTime": now_unix(),
            "rooms": rooms,
        }),
    )
}

async fn rooms(State(s): App, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = authenticated!(s, method, uri, headers, body);
    let store = s.store.lock().unwrap();
    reply(StatusCode::OK, json!({ "rooms": room_views(&store, &agent) }))
}

async fn list_keys(State(s): App, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = authenticated!(s, method, uri, headers, body);
    let store = s.store.lock().unwrap();
    let keys: Vec<Value> = store.keys.iter().filter(|(_, k)| k.org == agent.org).map(|(id, k)| key_view(id, k)).collect();
    reply(StatusCode::OK, json!({ "keys": keys }))
}

/// Each list in the inbox holds at most this many cards, most recently updated first. Without a bound, one member
/// of a shared room could grow every other member's inbox past what a client reads. The rest stay in the card list.
pub const INBOX_LIST_MAX: usize = 100;

async fn inbox(State(s): App, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = authenticated!(s, method, uri, headers, body);
    let store = s.store.lock().unwrap();
    let mut waiting_on_us = Vec::new();
    let mut notices_to_ack = Vec::new();
    let mut shared_open = Vec::new();
    let mut waiting_on_them = 0u32;
    // Newest first: among cards updated in the same second, the one created last comes first.
    for card in store.cards.iter().rev() {
        let Some(room) = store.rooms.get(&card.room).filter(|r| r.is_member(&agent.org)) else {
            continue;
        };
        let ours = card.reporter_org == agent.org;
        if card.kind == "notice" {
            let acked = store.messages.iter().any(|m| m.card == card.id && m.kind == "ack" && m.org == agent.org);
            if !ours && !acked {
                notices_to_ack.push(card);
            }
            continue;
        }
        if card.stage == "closed" {
            continue;
        }
        match room.kind {
            // Open findings by other members; nobody else can move them, so they are listed, not owed.
            RoomKind::Shared if !ours => shared_open.push(card),
            RoomKind::Shared => {}
            // The next move belongs to the side that received it until it ships or asks; then to the reporter.
            RoomKind::Pair => {
                let reporter_moves = matches!(card.stage.as_str(), "need-info" | "shipped");
                if ours == reporter_moves {
                    waiting_on_us.push(card);
                } else {
                    waiting_on_them += 1;
                }
            }
        }
    }
    let mut truncated = false;
    let mut list = |mut cards: Vec<&Card>| {
        // A stable sort keeps the newest-created first among equal times.
        cards.sort_by(|a, b| b.updated.cmp(&a.updated));
        truncated |= cards.len() > INBOX_LIST_MAX;
        cards.truncate(INBOX_LIST_MAX);
        card_views(&store, &cards, &agent)
    };
    let (waiting_on_us, notices_to_ack, shared_open) = (list(waiting_on_us), list(notices_to_ack), list(shared_open));
    reply(
        StatusCode::OK,
        json!({ "waitingOnUs": waiting_on_us, "noticesToAck": notices_to_ack, "sharedOpen": shared_open,
                "waitingOnThem": waiting_on_them, "truncated": truncated }),
    )
}

// ---------------------------------------------------------------- waiting for news

/// One key may have this many requests held by `GET /v1/inbox/wait` at a time.
pub const WAITS_PER_KEY: usize = 2;
/// One answer of `GET /v1/inbox/wait` holds at most this many events.
pub const WAIT_EVENTS_MAX: usize = 100;

struct WaitQuery<'a> {
    /// Room id -> the last entry number the caller has seen. `None`: no cursor was sent.
    after: Option<HashMap<&'a str, u64>>,
    /// Seconds to hold the request; `None` for the board's cap.
    wait: Option<u64>,
    room: Option<&'a str>,
}

/// `after` (a cursor from an earlier answer), `wait` (seconds, up to four digits; more than the board's cap is
/// cut to the cap) and `room`. A cursor is `-` for no rooms, or `<room>.<seq>` parts joined by `_`: the rooms are
/// the caller's own and each number is an entry number of that room's record, so it says nothing about any other
/// room. Anything else, a repeated name or a bad value is `400 bad_query`.
fn wait_query(query: Option<&str>) -> Result<WaitQuery<'_>, Refusal> {
    let bad = (StatusCode::BAD_REQUEST, "bad_query");
    let mut q = WaitQuery { after: None, wait: None, room: None };
    let mut seen = BTreeSet::new();
    for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').ok_or(bad)?;
        if !seen.insert(name) {
            return Err(bad);
        }
        match name {
            "wait" if (1..=4).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit()) => q.wait = Some(value.parse().map_err(|_| bad)?),
            "room" if store::is_token(value, 3, 64) => q.room = Some(value),
            "after" => {
                let mut cursor = HashMap::new();
                for part in value.split('_').filter(|_| value != "-") {
                    let (room, seq) = part.rsplit_once('.').ok_or(bad)?;
                    let digits = (1..=15).contains(&seq.len()) && seq.bytes().all(|b| b.is_ascii_digit());
                    if !store::is_token(room, 3, 64) || !digits || cursor.insert(room, seq.parse().map_err(|_| bad)?).is_some() {
                        return Err(bad);
                    }
                }
                q.after = Some(cursor);
            }
            _ => return Err(bad),
        }
    }
    Ok(q)
}

/// What is new for the caller after its cursor, and the cursor to send next. Looks only at the rooms the caller's
/// organisation is in now (one of them with `room`). An event is an entry of a room's record without its hashes:
/// the same facts for every key class, and no title or text. Entries the caller's own organisation made are
/// passed over: they move the cursor and are not events. A room the cursor does not name is new to the caller,
/// and all of its record counts. Without a cursor nothing is an event and the cursor is where the rooms stand.
fn news(store: &Store, agent: &Agent, q: &WaitQuery<'_>) -> (Vec<Value>, String, bool) {
    let mut events = Vec::new();
    let mut cursor = Vec::new();
    let mut more = false;
    for room in store.rooms.values().filter(|r| r.is_member(&agent.org) && q.room.is_none_or(|only| only == r.id)) {
        let entries = store.chains.get(&room.id).map_or(&[][..], Vec::as_slice);
        let head = entries.len() as u64;
        // A cursor ahead of the record (a board restored from a backup) starts again at the head.
        let from = q.after.as_ref().map_or(head, |c| c.get(room.id.as_str()).copied().unwrap_or(0)).min(head);
        let own = room.display_name(&agent.org);
        let mut seen = from;
        for e in &entries[from as usize..] {
            if e.actor != own {
                if events.len() == WAIT_EVENTS_MAX {
                    more = true;
                    break;
                }
                events.push(json!({ "room": room.id, "seq": e.seq, "kind": e.kind, "card": e.card, "actor": e.actor, "time": e.time }));
            }
            seen = e.seq;
        }
        cursor.push(format!("{}.{seen}", room.id));
    }
    (events, if cursor.is_empty() { "-".to_owned() } else { cursor.join("_") }, more)
}

/// A place among the held requests of a key, given back when the request ends, however it ends.
struct WaitSlot(Arc<Shared>, String);

impl WaitSlot {
    fn take(s: &Arc<Shared>, key_id: &str) -> Option<Self> {
        let mut waits = s.waits.lock().unwrap();
        let held = waits.entry(key_id.to_owned()).or_insert(0);
        (*held < WAITS_PER_KEY).then(|| {
            *held += 1;
            WaitSlot(s.clone(), key_id.to_owned())
        })
    }
}

impl Drop for WaitSlot {
    fn drop(&mut self) {
        let mut waits = self.0.waits.lock().unwrap();
        if let Some(held) = waits.get_mut(&self.1) {
            *held -= 1;
            if *held == 0 {
                waits.remove(&self.1);
            }
        }
    }
}

/// The long poll. Answers at once when there is an event after the caller's cursor, or when no cursor was sent
/// (then with the cursor to start from). Otherwise the request is held until an event arrives or the time is up
/// (`wait` seconds, at most the board's cap), and then answered with no events and `timedOut: true`.
///
/// A request without a valid signature is never held: it gets the 401 at once, as everywhere. A held request
/// counts as one request against the key's rate limit, when it arrives. Each time a held request looks again,
/// and before it answers, the key is checked: a key that was revoked or has expired meanwhile gets the 401 and no
/// data. A key holds at most `WAITS_PER_KEY` requests; one more is `409 too_many_waits`.
async fn inbox_wait(State(s): App, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = authenticated!(s, method, uri, headers, body);
    let q = match wait_query(uri.query()) {
        Ok(q) => q,
        Err(r) => return refuse(r),
    };
    let hold = q.wait.unwrap_or(s.wait_cap).min(s.wait_cap);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(hold);
    let mut slot = None;
    loop {
        // Listening starts before the look, so a change between the look and the wait is not missed.
        let changed = s.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        {
            let store = s.store.lock().unwrap();
            let now = now_unix();
            if !store.keys.get(&agent.key_id).is_some_and(|k| k.status == KeyStatus::Active && k.expires > now) {
                return refused("key_ended_while_waiting", &method);
            }
            if q.room.is_some_and(|room| !in_room(&store, &agent, room)) {
                return refuse(NOT_FOUND);
            }
            let (events, cursor, more) = news(&store, &agent, &q);
            let timed_out = tokio::time::Instant::now() >= deadline;
            if q.after.is_none() || !events.is_empty() || timed_out {
                let timed_out = events.is_empty() && q.after.is_some() && hold > 0;
                return reply(StatusCode::OK, json!({ "events": events, "cursor": cursor, "more": more, "timedOut": timed_out, "wait": hold }));
            }
        }
        if slot.is_none() {
            slot = WaitSlot::take(&s, &agent.key_id);
            if slot.is_none() {
                return refuse((StatusCode::CONFLICT, "too_many_waits"));
            }
        }
        tokio::select! {
            _ = &mut changed => {}
            _ = tokio::time::sleep_until(deadline) => {}
        }
    }
}

struct CardQuery<'a> {
    limit: usize,
    cursor: Option<Uuid>,
    stage: Option<&'a str>,
    kind: Option<&'a str>,
}

/// `limit` (1 to 100, default 50), `cursor` (a card id from `nextCursor`), `stage`, `kind`. Anything else,
/// a repeated name or a bad value is `400 bad_query`.
fn card_query(query: Option<&str>) -> Result<CardQuery<'_>, Refusal> {
    let bad = (StatusCode::BAD_REQUEST, "bad_query");
    let mut q = CardQuery { limit: 50, cursor: None, stage: None, kind: None };
    let mut seen = BTreeSet::new();
    for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').ok_or(bad)?;
        if !seen.insert(name) {
            return Err(bad);
        }
        match name {
            "limit" if value.bytes().all(|b| b.is_ascii_digit()) => {
                q.limit = value.parse().ok().filter(|n| (1..=100).contains(n)).ok_or(bad)?
            }
            "cursor" => q.cursor = Some(Uuid::parse_str(value).map_err(|_| bad)?),
            "stage" if STAGES.contains(&value) => q.stage = Some(value),
            "kind" if ["finding", "notice"].contains(&value) => q.kind = Some(value),
            _ => return Err(bad),
        }
    }
    Ok(q)
}

async fn list_cards(State(s): App, room: Param, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = authenticated!(s, method, uri, headers, body);
    let store = s.store.lock().unwrap();
    let Some(Path(room)) = room.ok().filter(|Path(r)| in_room(&store, &agent, r)) else {
        return refuse(NOT_FOUND);
    };
    let q = match card_query(uri.query()) {
        Ok(q) => q,
        Err(r) => return refuse(r),
    };
    // Newest first, by order of creation (seconds tie). A cursor is a card of this room, else a bad query.
    let in_this_room: Vec<&Card> = store.cards.iter().rev().filter(|c| c.room == room).collect();
    let start = match q.cursor {
        None => 0,
        Some(cursor) => match in_this_room.iter().position(|c| c.id == cursor) {
            Some(at) => at + 1,
            None => return refuse((StatusCode::BAD_REQUEST, "bad_query")),
        },
    };
    let mut matching = in_this_room[start..]
        .iter()
        .filter(|c| q.stage.is_none_or(|st| c.stage == st) && q.kind.is_none_or(|k| c.kind == k));
    let page: Vec<&Card> = matching.by_ref().take(q.limit).copied().collect();
    let mut out = json!({ "cards": card_views(&store, &page, &agent) });
    if let (Some(last), Some(_)) = (page.last(), matching.next()) {
        out["nextCursor"] = json!(last.id);
    }
    reply(StatusCode::OK, out)
}

/// A page of a room's chain holds at most this many entries.
pub const CHAIN_PAGE_MAX: usize = 1000;
const CHAIN_PAGE_DEFAULT: usize = 200;

/// `after` (a seq; entries above it are returned, default 0) and `limit` (1 to `CHAIN_PAGE_MAX`, default 200).
/// Anything else, a repeated name or a bad value is `400 bad_query`.
fn chain_query(query: Option<&str>) -> Result<(u64, usize), Refusal> {
    let bad = (StatusCode::BAD_REQUEST, "bad_query");
    let (mut after, mut limit) = (0, CHAIN_PAGE_DEFAULT);
    let mut seen = BTreeSet::new();
    for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (name, value) = pair.split_once('=').ok_or(bad)?;
        if !seen.insert(name) || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad);
        }
        match name {
            "after" => after = value.parse().map_err(|_| bad)?,
            "limit" => limit = value.parse().ok().filter(|n| (1..=CHAIN_PAGE_MAX).contains(n)).ok_or(bad)?,
            _ => return Err(bad),
        }
    }
    Ok((after, limit))
}

/// The room's chain, oldest first, and its head. The answer does not depend on who asks: every member of the room
/// and every key class gets the same bytes. It holds hashes, display names, card ids, kinds and times, and no
/// title or text, so a `fields` key may read it. A room the caller is not in answers like one that does not exist.
async fn room_chain(State(s): App, room: Param, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = authenticated!(s, method, uri, headers, body);
    let store = s.store.lock().unwrap();
    let Some(Path(room)) = room.ok().filter(|Path(r)| in_room(&store, &agent, r)) else {
        return refuse(NOT_FOUND);
    };
    let (after, limit) = match chain_query(uri.query()) {
        Ok(q) => q,
        Err(r) => return refuse(r),
    };
    let entries = store.chains.get(&room).map_or(&[][..], Vec::as_slice);
    // Entry n is at index n - 1, so `after` is also where the page starts.
    let start = usize::try_from(after).unwrap_or(usize::MAX).min(entries.len());
    let page = &entries[start..entries.len().min(start.saturating_add(limit))];
    reply(StatusCode::OK, json!({ "room": room, "format": chain::FORMAT, "entries": page, "head": head_view(&store, &room) }))
}

async fn get_card(State(s): App, id: Param, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, _) = authenticated!(s, method, uri, headers, body);
    let store = s.store.lock().unwrap();
    // A card in a room the caller is not in looks exactly like a card that does not exist.
    let Ok(card_id) = id.map_err(|_| NOT_FOUND).and_then(|Path(id)| visible_card(&store, &agent, &id)) else {
        return refuse(NOT_FOUND);
    };
    let Some(card) = store.card(card_id) else {
        return refuse(NOT_FOUND);
    };
    let messages: Vec<Value> =
        store.messages.iter().filter(|m| m.card == card.id).map(|m| message_view(&store, &card.room, m, &agent)).collect();
    // `card.messages` is the thread's length; the top-level `messages` is the thread itself.
    reply(StatusCode::OK, json!({ "card": card_view(&store, card, &agent, messages.len()), "messages": messages }))
}

// ---------------------------------------------------------------- writes

fn text_field<'a>(input: &'a Value, name: &str, max: usize) -> Result<&'a str, Refusal> {
    let text = input.get(name).and_then(Value::as_str).map(str::trim).unwrap_or("");
    if text.is_empty() || text.chars().count() > max {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "bad_field"));
    }
    gate::check(text).map_err(|rule| (StatusCode::UNPROCESSABLE_ENTITY, rule))?;
    Ok(text)
}

fn one_of<'a>(input: &'a Value, name: &str, allowed: &[&str]) -> Result<&'a str, Refusal> {
    input
        .get(name)
        .and_then(Value::as_str)
        .filter(|v| allowed.contains(v))
        .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "bad_field"))
}

/// `room` is the room the write goes into, for the room's write limit; `None` for a write outside rooms.
/// Runs one write under the idempotency rule: the same key repeating the same request gets the first
/// result back; the same idempotency key with a different request is a conflict. Refusals are not stored. With a
/// state file, the change and its replay record are saved before the answer; a failed save is `500
/// state_not_saved` and leaves nothing behind, so the same request can be sent again.
fn write(
    s: &Shared,
    agent: &Agent,
    room: Option<&str>,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
    action: impl FnOnce(&mut Store, &Value, u64) -> Result<(StatusCode, Value), Refusal>,
) -> Response {
    let Some(idem) = header(headers, "idempotency-key").filter(|k| Uuid::parse_str(k).is_ok()) else {
        return refuse((StatusCode::BAD_REQUEST, "idempotency_key_required"));
    };
    let Ok(input) = serde_json::from_slice::<Value>(body) else {
        return refuse((StatusCode::BAD_REQUEST, "bad_json"));
    };
    let fingerprint = format!("{} {}", uri.path(), sig::content_digest(body));
    let slot = (agent.key_id.clone(), idem.to_owned());
    let mut store = s.store.lock().unwrap();
    if let Some(first) = store.replays.get(&slot) {
        if first.fingerprint != fingerprint {
            return refuse((StatusCode::CONFLICT, "idempotency_key_conflict"));
        }
        let mut again = first.body.clone();
        again["replayed"] = json!(true);
        return reply(first.status, again);
    }
    // A new write, not a replay: counted against the key's writes and the room's. Over a limit, nothing is done
    // and nothing is stored, so the same request with the same idempotency key can be sent again.
    let now = now_unix();
    if let Err(retry_after) = s.limits.write(&agent.key_id, room, now) {
        return rate_limited(retry_after);
    }
    let before = s.snapshot(&store);
    match action(&mut store, &input, now) {
        Ok((status, value)) => {
            store.replays.insert(slot, Replay { fingerprint, status, body: value.clone() });
            // Saved, replay record included, before the answer goes out; if that fails, undone and not answered.
            match s.commit(&mut store, before) {
                Ok(()) => reply(status, value),
                Err(refusal) => refuse(refusal),
            }
        }
        Err(refusal) => {
            s.undo(&mut store, before);
            refuse(refusal)
        }
    }
}

fn append(store: &mut Store, card: Uuid, agent: &Agent, kind: &str, text: &str, replaces: Option<Uuid>, now: u64) -> Uuid {
    let id = Uuid::new_v4();
    store.messages.push(Message {
        id,
        card,
        org: agent.org.clone(),
        key_id: agent.key_id.clone(),
        kind: kind.to_owned(),
        text: text.to_owned(),
        replaces,
        created: now,
    });
    id
}

/// The card, if it exists in a room the caller's organisation is in.
fn visible_card(store: &Store, agent: &Agent, id: &str) -> Result<Uuid, Refusal> {
    Uuid::parse_str(id)
        .ok()
        .and_then(|id| store.card(id))
        .filter(|c| in_room(store, agent, &c.room))
        .map(|c| c.id)
        .ok_or(NOT_FOUND)
}

/// The room of the card, if the card exists in a room the caller's organisation is in.
fn visible_card_room(store: &Store, agent: &Agent, id: &str) -> Option<String> {
    let id = visible_card(store, agent, id).ok()?;
    store.card(id).map(|c| c.room.clone())
}

fn thread_has_room(store: &Store, card: Uuid) -> Result<(), Refusal> {
    match store.thread_length(card) < MAX_THREAD {
        true => Ok(()),
        false => Err((StatusCode::CONFLICT, "thread_full")),
    }
}

async fn create_card(State(s): App, room: Param, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, body) = authenticated!(s, method, uri, headers, body);
    // A room the caller is not in is 404 before anything else, as in v0.1.
    let Some(Path(room)) = room.ok().filter(|Path(r)| in_room(&s.store.lock().unwrap(), &agent, r)) else {
        return refuse(NOT_FOUND);
    };
    write(&s, &agent, Some(&room), &uri, &headers, &body, |store, input, now| {
        if !in_room(store, &agent, &room) {
            return Err(NOT_FOUND);
        }
        let kind = one_of(input, "kind", &["finding", "notice"])?;
        let severity = match kind {
            "finding" => Some(one_of(input, "severity", &["blocker", "wrong", "friction", "question"])?.to_owned()),
            _ => None,
        };
        let title = text_field(input, "title", gate::MAX_TITLE)?;
        let text = text_field(input, "text", gate::MAX_TEXT)?;
        let card = Card {
            id: Uuid::new_v4(),
            room: room.clone(),
            kind: kind.to_owned(),
            severity,
            title: title.to_owned(),
            reporter_org: agent.org.clone(),
            stage: if kind == "finding" { "reported" } else { "posted" }.to_owned(),
            outcome: None,
            created: now,
            updated: now,
        };
        let id = card.id;
        append(store, id, &agent, "report", text, None, now);
        store.cards.push(card);
        store.chain_card(id);
        // Viewed as stored, so its thread already holds the report.
        let card = store.card(id).ok_or(NOT_FOUND)?;
        Ok((StatusCode::CREATED, json!({ "card": card_view(store, card, &agent, store.thread_length(id)) })))
    })
}

async fn post_message(State(s): App, id: Param, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, body) = authenticated!(s, method, uri, headers, body);
    // A card the caller cannot see is 404 before anything else, a replay of its own earlier write included.
    let Some((id, room)) = id.ok().and_then(|Path(id)| visible_card_room(&s.store.lock().unwrap(), &agent, &id).map(|room| (id, room))) else {
        return refuse(NOT_FOUND);
    };
    write(&s, &agent, Some(&room), &uri, &headers, &body, |store, input, now| {
        let card_id = visible_card(store, &agent, &id)?;
        let kind = one_of(input, "kind", &["reply", "proof", "correction", "ack"])?;
        let text = text_field(input, "text", gate::MAX_TEXT)?;
        // A full thread still takes each member's first ack, so filling it cannot stop a notice being acknowledged.
        let first_ack = kind == "ack" && !store.messages.iter().any(|m| m.card == card_id && m.kind == "ack" && m.org == agent.org);
        if !first_ack {
            thread_has_room(store, card_id)?;
        }
        let card = store.card(card_id).ok_or(NOT_FOUND)?;
        if kind == "ack" && (card.kind != "notice" || card.reporter_org == agent.org) {
            // Any member other than the one that posted a notice can acknowledge it.
            return Err((StatusCode::FORBIDDEN, "ack_is_for_the_receiver_of_a_notice"));
        }
        let replaces = match kind {
            // A correction is a new message tied to the one it replaces. Nothing is edited.
            "correction" => {
                let target = input.get("replaces").and_then(Value::as_str).and_then(|v| Uuid::parse_str(v).ok());
                let own = target.filter(|t| store.messages.iter().any(|m| m.id == *t && m.card == card_id && m.org == agent.org));
                Some(own.ok_or((StatusCode::UNPROCESSABLE_ENTITY, "correction_must_replace_your_own_message_on_this_card"))?)
            }
            _ => None,
        };
        let message_id = append(store, card_id, &agent, kind, text, replaces, now);
        store.chain_message(message_id);
        if let Some(card) = store.card_mut(card_id) {
            card.updated = now;
        }
        Ok((StatusCode::CREATED, json!({ "messageId": message_id, "card": card_id })))
    })
}

async fn set_stage(State(s): App, id: Param, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, body) = authenticated!(s, method, uri, headers, body);
    // A card the caller cannot see is 404 before anything else, a replay of its own earlier write included.
    let Some((id, room)) = id.ok().and_then(|Path(id)| visible_card_room(&s.store.lock().unwrap(), &agent, &id).map(|room| (id, room))) else {
        return refuse(NOT_FOUND);
    };
    write(&s, &agent, Some(&room), &uri, &headers, &body, |store, input, now| {
        let card_id = visible_card(store, &agent, &id)?;
        let stage = one_of(input, "stage", &["acknowledged", "need-info", "shipped", "closed"])?;
        let text = text_field(input, "text", gate::MAX_TEXT)?;
        // A card closes once, so a full thread never stops it being closed.
        if stage != "closed" {
            thread_has_room(store, card_id)?;
        }
        let card = store.card(card_id).ok_or(NOT_FOUND)?;
        let room = store.rooms.get(&card.room).ok_or(NOT_FOUND)?;
        if card.kind != "finding" {
            return Err((StatusCode::CONFLICT, "only_findings_have_stages"));
        }
        if card.stage == "closed" {
            return Err((StatusCode::CONFLICT, "card_is_closed"));
        }
        let reporter = card.reporter_org == agent.org;
        let outcome = match stage {
            "closed" => Some(one_of(input, "outcome", &["verified", "not-a-defect", "declined", "withdrawn"])?),
            _ => None,
        };
        let forbidden = |code| Err((StatusCode::FORBIDDEN, code));
        match (room.kind, stage, outcome) {
            // Withdrawing is the reporter's word, from any open stage, in any room.
            (_, "closed", Some("withdrawn")) if reporter => {}
            // In a shared room the reporter closes its own card ("answered to my satisfaction"), and nobody
            // else moves it: no member can acknowledge, ship, decline or close another member's card.
            (RoomKind::Shared, "closed", Some("verified")) if reporter => {}
            (RoomKind::Shared, ..) => return forbidden("shared_cards_are_closed_by_their_reporter"),
            (RoomKind::Pair, "closed", Some("withdrawn")) => return forbidden("this_move_belongs_to_the_other_side"),
            // "Fixed" is the reporter's word, and only after the other side says it shipped.
            (RoomKind::Pair, "closed", Some("verified")) if !reporter => return forbidden("only_the_reporter_may_verify"),
            (RoomKind::Pair, "closed", Some("verified")) if card.stage != "shipped" => {
                return Err((StatusCode::CONFLICT, "verify_follows_shipped"))
            }
            (RoomKind::Pair, "closed", Some("verified")) => {}
            // Every other move belongs to the side that received the finding.
            _ if reporter => return forbidden("this_move_belongs_to_the_other_side"),
            _ => {}
        }
        // "closed: unverified" is for a person, after a reminder went unanswered. People sign in read-only, so not yet.
        // Two chain entries: the move's text as a message, then the stage it set.
        let message_id = append(store, card_id, &agent, "stage", text, None, now);
        store.chain_message(message_id);
        let card = store.card_mut(card_id).ok_or(NOT_FOUND)?;
        card.stage = stage.to_owned();
        card.outcome = outcome.map(str::to_owned);
        card.updated = now;
        store.chain_stage(card_id, &agent.org, now);
        let card = store.card(card_id).ok_or(NOT_FOUND)?;
        Ok((StatusCode::OK, json!({ "card": card_view(store, card, &agent, store.thread_length(card_id)) })))
    })
}

/// Any active key may revoke any key of its own organisation, itself included. A key of another organisation
/// answers exactly like a key that does not exist. Revocation is immediate and final.
async fn revoke_key(State(s): App, method: Method, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let (agent, body) = authenticated!(s, method, uri, headers, body);
    write(&s, &agent, None, &uri, &headers, &body, |store, input, _now| {
        let key_id = input.get("keyId").and_then(Value::as_str).ok_or((StatusCode::UNPROCESSABLE_ENTITY, "bad_field"))?;
        let key = store.keys.get_mut(key_id).filter(|k| k.org == agent.org).ok_or(NOT_FOUND)?;
        key.status = KeyStatus::Revoked;
        Ok((StatusCode::OK, json!({ "keyId": key_id, "status": "revoked" })))
    })
}
