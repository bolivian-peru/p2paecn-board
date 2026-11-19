//! Record of organisations, rooms, keys, enrollment codes, cards, messages and each room's hash-chained record
//! (`src/chain.rs`), held in process memory.
//! Without `P2PAECN_STATE` a restart forgets all of it. With it, every accepted change is written to one file
//! before it is acknowledged (`src/state.rs`), and the file is loaded at start. Nonces are never written.
//! There is no database. What a view derives from the record
//! (a thread's length, which organisations have an agent key) is computed when asked and never stored.

use crate::chain::{self, Entry};
use crate::KeyClass;
use axum::http::StatusCode;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;

/// A refusal: status and the error code returned as `{"error": code}`.
pub type Refusal = (StatusCode, &'static str);

pub const NOT_FOUND: Refusal = (StatusCode::NOT_FOUND, "not_found");

/// Lower-case letters, digits and dashes, of a length in `min..=max`.
pub fn is_token(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn is_org_id(s: &str) -> bool {
    is_token(s, 3, 32)
}

pub fn is_key_name(s: &str) -> bool {
    is_token(s, 2, 32)
}

/// What a `viewer` key is for, set by the operator on its enrollment code.
pub const VIEWER_ROLES: [&str; 2] = ["admin", "manager"];

/// A viewer key's label, set by the operator: 3 to 40 of ASCII letters, digits, space and dash, starting and ending
/// with a letter or digit. Shown to the person and the operator; never to another organisation.
pub fn is_label(s: &str) -> bool {
    let ends = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric());
    (3..=40).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'-')
        && ends(s.as_bytes().first())
        && ends(s.as_bytes().last())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomKind {
    /// Exactly two organisations, who see each other's org ids.
    Pair,
    /// One or more organisations, who see each other only by handle.
    Shared,
}

impl RoomKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RoomKind::Pair => "pair",
            RoomKind::Shared => "shared",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub org: String,
    /// Set in a shared room only.
    pub handle: Option<String>,
    /// A removed member keeps its entry so its past cards and messages still show under its handle.
    pub removed: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Room {
    pub id: String,
    pub kind: RoomKind,
    pub members: Vec<Member>,
}

impl Room {
    pub fn is_member(&self, org: &str) -> bool {
        self.members.iter().any(|m| m.org == org && !m.removed)
    }

    /// What other members see for `org` in this room: the org id in a pair room, the handle in a shared room.
    /// Never an org id in a shared room, even for an org that was never a member.
    pub fn display_name<'a>(&'a self, org: &'a str) -> &'a str {
        match self.kind {
            RoomKind::Pair => org,
            RoomKind::Shared => self.members.iter().find(|m| m.org == org).and_then(|m| m.handle.as_deref()).unwrap_or("unknown"),
        }
    }

    /// The current members other than `org`, in the room's order. A removed member is not one of them.
    pub fn other_members<'a, 'o>(&'a self, org: &'o str) -> impl Iterator<Item = &'a Member> + use<'a, 'o> {
        self.members.iter().filter(move |m| !m.removed && m.org != org)
    }

    /// Display names of the current members other than `org`.
    pub fn others(&self, org: &str) -> Vec<&str> {
        self.other_members(org).map(|m| self.display_name(&m.org)).collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyStatus {
    /// Enrolled, waiting for the operator to confirm its fingerprint. May call `GET /v1/whoami` only.
    Pending,
    Active,
    /// Final. Cannot be undone.
    Revoked,
}

impl KeyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyStatus::Pending => "pending",
            KeyStatus::Active => "active",
            KeyStatus::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentKey {
    pub org: String,
    pub class: KeyClass,
    #[serde(with = "raw_key")]
    pub key: VerifyingKey,
    pub status: KeyStatus,
    pub created: u64,
    pub expires: u64,
    /// `viewer` keys only, from the code: `admin` or `manager`. Absent from older state files and from other keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `viewer` keys only, from the code. Absent from older state files and from other keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// What a used enrollment code remembers, so a client that lost the response can retry.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enrolled {
    #[serde(with = "bytes32")]
    pub public_key: [u8; 32],
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollCode {
    pub id: String,
    /// SHA-256 of the normalised code. The code itself is never stored, in memory or in the state file.
    #[serde(with = "bytes32")]
    pub hash: [u8; 32],
    pub org: String,
    pub class: KeyClass,
    pub expires: u64,
    pub key_ttl_days: u64,
    pub used: Option<Enrolled>,
    /// Given to the key enrolled with this code. `viewer` codes only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A member's statement that it is working on a card, for a stated time. A signal to the other members, not a
/// lock: it stops nobody from replying or moving the card. It counts only while `until` is in the future
/// (`Card::live_claim`); nothing removes it when that time passes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub org: String,
    /// Unix seconds. The claim is live while this is later than the board's clock.
    pub until: u64,
    /// A short note by the claiming organisation, through the content gate like any text.
    pub note: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub id: Uuid,
    pub room: String,
    /// "finding" or "notice"
    pub kind: String,
    pub severity: Option<String>,
    pub title: String,
    pub reporter_org: String,
    pub stage: String,
    pub outcome: Option<String>,
    pub created: u64,
    pub updated: u64,
    /// Absent from state files written before claims existed, and from cards nobody claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<Claim>,
}

impl Card {
    /// The claim, while it counts: its time has not passed and the card is not closed.
    pub fn live_claim(&self, now: u64) -> Option<&Claim> {
        self.claim.as_ref().filter(|c| c.until > now && self.stage != "closed")
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: Uuid,
    pub card: Uuid,
    pub org: String,
    pub key_id: String,
    /// "report", "reply", "proof", "correction", "ack" or "stage"
    pub kind: String,
    pub text: String,
    pub replaces: Option<Uuid>,
    pub created: u64,
}

/// A stored write result, replayed when the same key repeats the same request.
#[derive(Clone)]
pub struct Replay {
    pub fingerprint: String,
    pub status: StatusCode,
    pub body: Value,
}

#[derive(Default)]
pub struct Store {
    pub orgs: BTreeSet<String>,
    pub rooms: BTreeMap<String, Room>,
    pub keys: BTreeMap<String, AgentKey>,
    pub codes: Vec<EnrollCode>,
    pub cards: Vec<Card>,
    pub messages: Vec<Message>,
    /// Room id -> that room's chain (`src/chain.rs`): one entry per accepted event, appended in the same change
    /// as the event itself and saved with it.
    pub chains: BTreeMap<String, Vec<Entry>>,
    /// (key id, idempotency key) -> first result
    pub replays: HashMap<(String, String), Replay>,
    /// (key id, nonce) -> expiry, unix seconds
    nonces: HashMap<(String, String), u64>,
    /// The second expired nonces were last dropped in.
    nonces_purged: u64,
}

const UNPROCESSABLE: StatusCode = StatusCode::UNPROCESSABLE_ENTITY;

impl Store {
    /// A copy of everything durable: the whole store except the nonces. Taken before a change, so that a change
    /// whose save failed can be undone.
    pub fn durable_clone(&self) -> Store {
        Store {
            orgs: self.orgs.clone(),
            rooms: self.rooms.clone(),
            keys: self.keys.clone(),
            codes: self.codes.clone(),
            cards: self.cards.clone(),
            messages: self.messages.clone(),
            chains: self.chains.clone(),
            replays: self.replays.clone(),
            nonces: HashMap::new(),
            nonces_purged: 0,
        }
    }

    /// Puts back what `durable_clone` copied. The nonces seen since stay: forgetting one would let it be replayed.
    pub fn restore(&mut self, before: Store) {
        let (nonces, purged) = (std::mem::take(&mut self.nonces), self.nonces_purged);
        *self = before;
        self.nonces = nonces;
        self.nonces_purged = purged;
    }

    /// Records a nonce. False if this key already used it inside the window. Expired nonces are dropped once a
    /// second, not on every call: what has expired only changes when the second does.
    pub fn first_use(&mut self, key_id: &str, nonce: &str, now: u64, ttl: u64) -> bool {
        if now != self.nonces_purged {
            self.nonces.retain(|_, expiry| *expiry > now);
            self.nonces_purged = now;
        }
        self.nonces.insert((key_id.to_owned(), nonce.to_owned()), now + ttl).is_none()
    }

    /// Whether this key already used this nonce inside the window. Records nothing.
    pub fn nonce_seen(&self, key_id: &str, nonce: &str, now: u64) -> bool {
        self.nonces.get(&(key_id.to_owned(), nonce.to_owned())).is_some_and(|expiry| *expiry > now)
    }

    /// How many nonces are remembered.
    pub fn nonce_count(&self) -> usize {
        self.nonces.len()
    }

    pub fn card(&self, id: Uuid) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }

    pub fn card_mut(&mut self, id: Uuid) -> Option<&mut Card> {
        self.cards.iter_mut().find(|c| c.id == id)
    }

    /// Seq and hash of the last entry of a room's chain.
    pub fn chain_head(&self, room: &str) -> (u64, &str) {
        chain::head(self.chains.get(room).map_or(&[], Vec::as_slice))
    }

    /// Appends the `card` entry for a card that was just stored together with its report message.
    pub fn chain_card(&mut self, card: Uuid) {
        let Some(card) = self.cards.iter().rev().find(|c| c.id == card) else { return };
        let Some(room) = self.rooms.get(&card.room) else { return };
        let report = self.messages.iter().rev().find(|m| m.card == card.id && m.kind == "report");
        chain::card_created(self.chains.entry(room.id.clone()).or_default(), room, card, report);
    }

    /// Appends the `message` entry for a message that was just stored.
    pub fn chain_message(&mut self, message: Uuid) {
        let Some(message) = self.messages.iter().rev().find(|m| m.id == message) else { return };
        let Some(card) = self.cards.iter().find(|c| c.id == message.card) else { return };
        let Some(room) = self.rooms.get(&card.room) else { return };
        chain::message_posted(self.chains.entry(room.id.clone()).or_default(), room, message);
    }

    /// Appends the `stage` entry for a card whose stage was just set. `org` is the organisation that moved it,
    /// entered under the name the room shows for it.
    pub fn chain_stage(&mut self, card: Uuid, org: &str, now: u64) {
        let Some(card) = self.cards.iter().find(|c| c.id == card) else { return };
        let Some(room) = self.rooms.get(&card.room) else { return };
        chain::stage_set(self.chains.entry(room.id.clone()).or_default(), room, card, room.display_name(org), now);
    }

    /// Appends the `claim` entry for a claim that was just set on a card, new or renewed.
    pub fn chain_claim(&mut self, card: Uuid, now: u64) {
        let Some(card) = self.cards.iter().find(|c| c.id == card) else { return };
        let (Some(room), Some(claim)) = (self.rooms.get(&card.room), &card.claim) else { return };
        chain::claimed(self.chains.entry(room.id.clone()).or_default(), room, card.id, claim, now);
    }

    /// Appends the `release` entry for a claim that `org` just gave up.
    pub fn chain_release(&mut self, card: Uuid, org: &str, now: u64) {
        let Some(card) = self.cards.iter().find(|c| c.id == card) else { return };
        let Some(room) = self.rooms.get(&card.room) else { return };
        chain::released(self.chains.entry(room.id.clone()).or_default(), room, card.id, room.display_name(org), now);
    }

    /// How many messages a card's thread holds: the report, every reply, proof, correction and ack, and one
    /// message per stage move.
    pub fn thread_length(&self, card: Uuid) -> usize {
        self.messages.iter().filter(|m| m.card == card).count()
    }

    /// The organisations that have joined as agents: those with at least one key that is active, not expired and
    /// not of class `viewer` (a person's browser). Pending, revoked and expired keys do not count.
    pub fn orgs_with_agent_key(&self, now: u64) -> BTreeSet<&str> {
        self.keys
            .values()
            .filter(|k| k.status == KeyStatus::Active && k.expires > now && k.class != KeyClass::Viewer)
            .map(|k| k.org.as_str())
            .collect()
    }

    pub fn add_org(&mut self, id: &str) -> Result<(), Refusal> {
        if !is_org_id(id) {
            return Err((UNPROCESSABLE, "bad_id"));
        }
        if !self.orgs.insert(id.to_owned()) {
            return Err((StatusCode::CONFLICT, "org_exists"));
        }
        Ok(())
    }

    /// A handle is a valid token and never an org id, so a display name cannot give an org away.
    fn check_handle(&self, handle: Option<&str>) -> Result<String, Refusal> {
        handle
            .filter(|h| is_token(h, 3, 32) && !self.orgs.contains(*h))
            .map(str::to_owned)
            .ok_or((UNPROCESSABLE, "bad_handle"))
    }

    /// `members` is (org, handle). A pair room has exactly two different orgs and no handles; a shared room has
    /// one or more different orgs, each with a handle unique in the room.
    pub fn add_room(&mut self, id: &str, kind: RoomKind, members: &[(String, Option<String>)]) -> Result<(), Refusal> {
        if !is_token(id, 3, 64) {
            return Err((UNPROCESSABLE, "bad_id"));
        }
        if self.rooms.contains_key(id) {
            return Err((StatusCode::CONFLICT, "room_exists"));
        }
        if members.iter().any(|(org, _)| !self.orgs.contains(org)) {
            return Err((UNPROCESSABLE, "unknown_org"));
        }
        let distinct: BTreeSet<&str> = members.iter().map(|(org, _)| org.as_str()).collect();
        let size_ok = match kind {
            RoomKind::Pair => members.len() == 2,
            RoomKind::Shared => !members.is_empty(),
        };
        if !size_ok || distinct.len() != members.len() {
            return Err((UNPROCESSABLE, "bad_members"));
        }
        let mut room = Room { id: id.to_owned(), kind, members: Vec::new() };
        for (org, handle) in members {
            let handle = match kind {
                RoomKind::Pair if handle.is_some() => return Err((UNPROCESSABLE, "bad_handle")),
                RoomKind::Pair => None,
                RoomKind::Shared => Some(self.check_handle(handle.as_deref())?),
            };
            if handle.is_some() && room.members.iter().any(|m| m.handle == handle) {
                return Err((StatusCode::CONFLICT, "handle_taken"));
            }
            room.members.push(Member { org: org.clone(), handle, removed: false });
        }
        // The room's chain starts with its members. Nothing above can refuse any more.
        let mut entries = Vec::new();
        for m in &room.members {
            chain::member_added(&mut entries, &room, &m.org, chain::OPERATOR);
        }
        self.chains.insert(id.to_owned(), entries);
        self.rooms.insert(id.to_owned(), room);
        Ok(())
    }

    /// Adds an org to a shared room. A removed member comes back only under its old handle, which its past cards
    /// still show: another handle would rename its history and free the old one for another org.
    pub fn add_member(&mut self, room_id: &str, org: &str, handle: &str) -> Result<(), Refusal> {
        let handle = self.check_handle(Some(handle))?;
        if !self.orgs.contains(org) {
            return Err((UNPROCESSABLE, "unknown_org"));
        }
        let room = self.rooms.get_mut(room_id).ok_or(NOT_FOUND)?;
        if room.kind != RoomKind::Shared {
            return Err((UNPROCESSABLE, "not_a_shared_room"));
        }
        if room.is_member(org) {
            return Err((StatusCode::CONFLICT, "already_member"));
        }
        // A handle stays taken by a removed member, whose past cards still show under it.
        if room.members.iter().any(|m| m.org != org && m.handle.as_deref() == Some(handle.as_str())) {
            return Err((StatusCode::CONFLICT, "handle_taken"));
        }
        match room.members.iter_mut().find(|m| m.org == org) {
            Some(former) if former.handle.as_deref() != Some(handle.as_str()) => return Err((StatusCode::CONFLICT, "handle_mismatch")),
            Some(former) => former.removed = false,
            None => room.members.push(Member { org: org.to_owned(), handle: Some(handle), removed: false }),
        }
        chain::member_added(self.chains.entry(room_id.to_owned()).or_default(), room, org, chain::OPERATOR);
        Ok(())
    }

    /// Removes an org from a shared room. Its past cards and messages stay, under its handle. Its open findings
    /// there are closed as withdrawn: it can no longer close them, and in a shared room nobody else may. The chain
    /// gets one `member-removed` entry and then one `stage` entry for each finding closed this way. A live claim
    /// the member holds on another member's card is taken off, with a `release` entry by the operator.
    pub fn remove_member(&mut self, room_id: &str, org: &str, now: u64) -> Result<(), Refusal> {
        let room = self.rooms.get_mut(room_id).ok_or(NOT_FOUND)?;
        if room.kind != RoomKind::Shared {
            return Err((UNPROCESSABLE, "not_a_shared_room"));
        }
        let member = room.members.iter_mut().find(|m| m.org == org && !m.removed).ok_or(NOT_FOUND)?;
        member.removed = true;
        let room = &*room;
        let entries = self.chains.entry(room_id.to_owned()).or_default();
        chain::member_removed(entries, room, org, chain::OPERATOR);
        let open = |c: &&mut Card| c.room == room_id && c.reporter_org == org && c.kind == "finding" && c.stage != "closed";
        for card in self.cards.iter_mut().filter(open) {
            card.stage = "closed".to_owned();
            card.outcome = Some("withdrawn".to_owned());
            card.updated = now;
            card.claim = None;
            chain::stage_set(entries, room, card, chain::OPERATOR, now);
        }
        for card in self.cards.iter_mut().filter(|c| c.room == room_id && c.claim.as_ref().is_some_and(|claim| claim.org == org)) {
            if card.live_claim(now).is_some() {
                chain::released(entries, room, card.id, chain::OPERATOR, now);
            }
            card.claim = None;
        }
        Ok(())
    }
}

/// A raw 32-byte value as unpadded base64url, as keys are written everywhere else.
mod bytes32 {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        URL_SAFE_NO_PAD.decode(&text).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| D::Error::custom("not 32 bytes of base64url"))
    }
}

/// An Ed25519 public key as its raw 32 bytes, refused when they are not a valid key.
mod raw_key {
    use ed25519_dalek::VerifyingKey;
    use serde::{de::Error, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(key: &VerifyingKey, s: S) -> Result<S::Ok, S::Error> {
        super::bytes32::serialize(key.as_bytes(), s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<VerifyingKey, D::Error> {
        VerifyingKey::from_bytes(&super::bytes32::deserialize(d)?).map_err(|_| D::Error::custom("not an Ed25519 public key"))
    }
}
