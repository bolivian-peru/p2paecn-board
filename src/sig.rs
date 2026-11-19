//! Fail-closed verification of `p2paecn-sig-1` signed requests.
//! The canonical string is the one `partner-pack/SIGNING.md` specifies and `partner-pack/p2p.mjs` signs; the
//! fixed vectors in `tests/vectors/` are made with that signer. Any doubt is a refusal.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

pub const SCHEME: &str = "p2paecn-sig-1";
pub const MAX_SKEW_SECS: u64 = 300;
/// How long a nonce is remembered: a request is fresh from `created - 300` to `created + 300` inclusive, so a
/// nonce first seen at the start of that window must still be known in its last second.
pub const NONCE_TTL_SECS: u64 = MAX_SKEW_SECS * 2 + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    MissingHeader,
    BadFormat,
    Stale,
    /// Signed at or before the second this process started, while a state file is in use (see `verify_after`).
    BeforeStart,
    UnknownKey,
    DigestMismatch,
    BadSignature,
    Replay,
}

impl Failure {
    pub fn reason(self) -> &'static str {
        match self {
            Failure::MissingHeader => "missing_header",
            Failure::BadFormat => "bad_format",
            Failure::Stale => "stale_or_future_timestamp",
            Failure::BeforeStart => "signed_before_start",
            Failure::UnknownKey => "unknown_or_inactive_key",
            Failure::DigestMismatch => "content_digest_mismatch",
            Failure::BadSignature => "bad_signature",
            Failure::Replay => "nonce_replayed",
        }
    }
}

pub fn content_digest(body: &[u8]) -> String {
    format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body)))
}

/// Every field that decides what the request does is signed. `host` is the board's own configured host,
/// never the Host header, so a request signed for one deployment cannot be replayed against another whose
/// configured host differs.
#[allow(clippy::too_many_arguments)]
pub fn canonical_string(
    method: &str,
    host: &str,
    path_and_query: &str,
    created: &str,
    nonce: &str,
    key_id: &str,
    digest: &str,
    idempotency_key: &str,
) -> String {
    let idem = if idempotency_key.is_empty() { "-" } else { idempotency_key };
    [
        SCHEME,
        &method.to_uppercase(),
        &host.to_lowercase(),
        path_and_query,
        created,
        nonce,
        key_id,
        digest,
        idem,
    ]
    .join("\n")
}

/// What the server received. Header values are `None` when absent or not valid text.
pub struct Request<'a> {
    pub method: &'a str,
    pub path_and_query: &'a str,
    pub key_id: Option<&'a str>,
    pub created: Option<&'a str>,
    pub nonce: Option<&'a str>,
    pub digest: Option<&'a str>,
    pub signature: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
    /// The exact bytes received, before any parsing.
    pub body: &'a [u8],
}

fn present(v: Option<&str>) -> Result<&str, Failure> {
    v.filter(|s| !s.is_empty()).ok_or(Failure::MissingHeader)
}

/// Checked in place of a key id that is unknown, revoked or expired, so that refusing one costs the same
/// signature check as refusing a usable key: the time of the 401 must not tell which key ids exist.
/// The Ed25519 base point: a valid key that is not weak, so `verify_strict` runs in full.
static NO_KEY: LazyLock<VerifyingKey> = LazyLock::new(|| {
    let mut base = [0x66u8; 32];
    base[0] = 0x58;
    VerifyingKey::from_bytes(&base).expect("the Ed25519 base point")
});

/// `resolve` returns the public key for a known, active, unexpired key id, else `None`.
/// `first_use` records the nonce and returns false if it was seen before. It runs last, so a caller
/// without the key cannot burn nonces.
pub fn verify(
    req: &Request<'_>,
    host: &str,
    now_unix: u64,
    resolve: impl FnOnce(&str) -> Option<VerifyingKey>,
    first_use: impl FnOnce(&str, &str) -> bool,
) -> Result<(), Failure> {
    verify_after(req, host, now_unix, None, resolve, first_use)
}

/// `verify`, and with `not_before` set, a request whose `created` is at or before that second is stale too. The
/// nonces seen before a restart are not saved; this keeps every request accepted before the restart from being
/// accepted again after it, except one signed for a time ahead of that board's clock (see partner-pack/SIGNING.md, section 7).
pub fn verify_after(
    req: &Request<'_>,
    host: &str,
    now_unix: u64,
    not_before: Option<u64>,
    resolve: impl FnOnce(&str) -> Option<VerifyingKey>,
    first_use: impl FnOnce(&str, &str) -> bool,
) -> Result<(), Failure> {
    let key_id = present(req.key_id)?;
    let created = present(req.created)?;
    let nonce = present(req.nonce)?;
    let digest = present(req.digest)?;
    let signature = present(req.signature)?;

    let created_ok = created.len() == 10 && created.bytes().all(|b| b.is_ascii_digit());
    let nonce_ok = (22..=64).contains(&nonce.len())
        && nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !created_ok || !nonce_ok || key_id.len() > 200 {
        return Err(Failure::BadFormat);
    }
    let created_unix: u64 = created.parse().map_err(|_| Failure::BadFormat)?;
    if now_unix.abs_diff(created_unix) > MAX_SKEW_SECS {
        return Err(Failure::Stale);
    }
    if not_before.is_some_and(|start| created_unix <= start) {
        return Err(Failure::BeforeStart);
    }

    // From here on an unusable key id takes the same steps as a usable one and fails at the end.
    let key = resolve(key_id);
    let digest_ok = content_digest(req.body) == digest;
    let message = canonical_string(
        req.method,
        host,
        req.path_and_query,
        created,
        nonce,
        key_id,
        digest,
        req.idempotency_key.unwrap_or(""),
    );
    let signature_ok = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|b| Signature::from_slice(&b).ok())
        .is_some_and(|sig| key.as_ref().unwrap_or(&NO_KEY).verify_strict(message.as_bytes(), &sig).is_ok());
    if key.is_none() {
        return Err(Failure::UnknownKey);
    }
    if !digest_ok {
        return Err(Failure::DigestMismatch);
    }
    if !signature_ok {
        return Err(Failure::BadSignature);
    }

    if !first_use(key_id, nonce) {
        return Err(Failure::Replay);
    }
    Ok(())
}

/// Decodes a raw 32-byte Ed25519 public key from base64url (the `x` of a JWK).
pub fn public_key_from_b64url(s: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(s).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// First 20 hex characters of SHA-256 over the raw public key, in five groups of four: `ab12-cd34-ef56-7890-1a2b`.
/// What a person reads to the operator before a pending key is activated.
pub fn fingerprint(key: &VerifyingKey) -> String {
    let hex: String = Sha256::digest(key.as_bytes()).iter().take(10).map(|b| format!("{b:02x}")).collect();
    hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect::<Vec<_>>().join("-")
}
