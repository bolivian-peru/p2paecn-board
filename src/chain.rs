//! The per-room record `p2paecn-chain-1`: one hash-chained list of entries for each room, as
//! `partner-pack/CHAIN.md` specifies it. This file is written against that document, and the client's verifier
//! in `partner-pack/p2p.mjs` is written against it separately; `tests/vectors/chain-1.json` holds what both must
//! produce.
//!
//! What goes into an entry is the same for every caller and is something every member of the room can already
//! see: the name the room shows for the author (an organisation id in a pair room, a handle in a shared room),
//! never a key id, and never an organisation id in a shared room. Titles and texts enter only as a SHA-256.
//!
//! The chain is per room. One chain for the whole board would let a member count the writes in rooms it is not in.
//!
//! What it is not: a defence against the operator. Whoever can write the state file can rebuild a whole chain. A
//! change shows only to a member that kept an earlier head (see CHAIN.md, "What this proves and what it does not").

use crate::store::{Card, Message, Room};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

/// The label every hashed record starts with.
pub const FORMAT: &str = "p2paecn-chain-1";
/// `prev` of the first entry of a room.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// The actor of an entry the operator caused. The `*` keeps it apart from every organisation id and handle.
pub const OPERATOR: &str = "*operator";
/// The actor of an entry that was built from an older state file and has no author of its own.
pub const IMPORT: &str = "*import";

/// One entry of a room's chain, as it is stored and as `GET /v1/rooms/{room}/chain` returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// From 1, without gaps.
    pub seq: u64,
    /// `member-added`, `member-removed`, `card`, `message`, `stage` or `import`.
    pub kind: String,
    /// The card the entry is about. `None` for membership and import entries.
    pub card: Option<Uuid>,
    /// A display name of the room, `*operator` or `*import`.
    pub actor: String,
    /// Unix seconds on the board's clock. 0 where no time is recorded (membership and import entries).
    pub time: u64,
    /// SHA-256 of the payload record, 64 lower-case hex characters.
    pub payload: String,
    /// `hash` of the entry before, or `GENESIS` for the first.
    pub prev: String,
    /// SHA-256 of the entry record, 64 lower-case hex characters.
    pub hash: String,
}

/// A record: the header line `p2paecn-chain-1 <what>` and one line `name:<length>:<value>` per field, each ended
/// by one LF. The length is the number of bytes of the value in UTF-8, in decimal. A value is copied as it is, so
/// it may hold LF and colons: the length says where it ends.
pub fn record(what: &str, fields: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(FORMAT.as_bytes());
    out.push(b' ');
    out.extend_from_slice(what.as_bytes());
    out.push(b'\n');
    for (name, value) in fields {
        out.extend_from_slice(name.as_bytes());
        out.push(b':');
        out.extend_from_slice(value.len().to_string().as_bytes());
        out.push(b':');
        out.extend_from_slice(value.as_bytes());
        out.push(b'\n');
    }
    out
}

/// SHA-256 as 64 lower-case hex characters.
pub fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 15)] as char);
    }
    out
}

/// What an event says, before it is hashed. An absent value (a notice's severity, `replaces` on anything but a
/// correction, the outcome of a stage that is not `closed`) is the empty string.
pub enum Payload<'a> {
    MemberAdded { name: &'a str },
    MemberRemoved { name: &'a str },
    /// A new card and its report message.
    Card { kind: &'a str, severity: &'a str, title: &'a str, message: &'a str, text: &'a str },
    /// Any message after the report, the text of a stage move included (its message kind is `stage`).
    Message { id: &'a str, kind: &'a str, replaces: &'a str, text: &'a str },
    Stage { stage: &'a str, outcome: &'a str },
    /// Closes the entries built from an older state file. `entries` is how many stand before it, in decimal.
    Import { entries: &'a str },
}

impl Payload<'_> {
    pub fn kind(&self) -> &'static str {
        match self {
            Payload::MemberAdded { .. } => "member-added",
            Payload::MemberRemoved { .. } => "member-removed",
            Payload::Card { .. } => "card",
            Payload::Message { .. } => "message",
            Payload::Stage { .. } => "stage",
            Payload::Import { .. } => "import",
        }
    }

    /// The payload record: `p2paecn-chain-1 payload <kind>` and the fields of that kind, in this order.
    pub fn bytes(&self) -> Vec<u8> {
        let what = format!("payload {}", self.kind());
        match self {
            Payload::MemberAdded { name } | Payload::MemberRemoved { name } => record(&what, &[("name", name)]),
            Payload::Card { kind, severity, title, message, text } => {
                record(&what, &[("kind", kind), ("severity", severity), ("title", title), ("message", message), ("text", text)])
            }
            Payload::Message { id, kind, replaces, text } => record(&what, &[("id", id), ("kind", kind), ("replaces", replaces), ("text", text)]),
            Payload::Stage { stage, outcome } => record(&what, &[("stage", stage), ("outcome", outcome)]),
            Payload::Import { entries } => record(&what, &[("entries", entries)]),
        }
    }

    pub fn hash(&self) -> String {
        sha256_hex(&self.bytes())
    }
}

/// The entry record: everything of an entry but its own hash, with the room id, so that an entry of one room
/// cannot stand in another.
#[allow(clippy::too_many_arguments)]
pub fn entry_bytes(room: &str, seq: u64, kind: &str, card: Option<Uuid>, actor: &str, time: u64, payload: &str, prev: &str) -> Vec<u8> {
    let card = card.map(|c| c.to_string()).unwrap_or_default();
    record(
        "entry",
        &[
            ("room", room),
            ("seq", &seq.to_string()),
            ("kind", kind),
            ("card", &card),
            ("actor", actor),
            ("time", &time.to_string()),
            ("payload", payload),
            ("prev", prev),
        ],
    )
}

/// Appends one entry to the chain of `room`.
pub fn append(chain: &mut Vec<Entry>, room: &str, payload: &Payload<'_>, card: Option<Uuid>, actor: &str, time: u64) {
    let seq = chain.len() as u64 + 1;
    let prev = chain.last().map_or(GENESIS, |e| e.hash.as_str()).to_owned();
    let (kind, payload) = (payload.kind(), payload.hash());
    let hash = sha256_hex(&entry_bytes(room, seq, kind, card, actor, time, &payload, &prev));
    chain.push(Entry { seq, kind: kind.to_owned(), card, actor: actor.to_owned(), time, payload, prev, hash });
}

/// Seq and hash of the last entry; `(0, GENESIS)` for an empty chain.
pub fn head(chain: &[Entry]) -> (u64, &str) {
    chain.last().map_or((0, GENESIS), |e| (e.seq, e.hash.as_str()))
}

/// Checks that the entries count from 1, that each names the hash of the one before, and that each hash is the
/// hash of its record. `Err` is the seq at which that first fails. It says nothing about whether the payload
/// hashes match the cards: that is what a member's client checks.
pub fn check(room: &str, chain: &[Entry]) -> Result<(), u64> {
    let mut prev = GENESIS;
    for (index, e) in chain.iter().enumerate() {
        let seq = index as u64 + 1;
        let hash = sha256_hex(&entry_bytes(room, seq, &e.kind, e.card, &e.actor, e.time, &e.payload, prev));
        if e.seq != seq || e.prev != prev || e.hash != hash {
            return Err(seq);
        }
        prev = &e.hash;
    }
    Ok(())
}

// ---------------------------------------------------------------- the events

fn opt(id: Option<Uuid>) -> String {
    id.map(|v| v.to_string()).unwrap_or_default()
}

/// A member was added to the room, or was one when the room was made. No time is recorded: see CHAIN.md.
pub fn member_added(chain: &mut Vec<Entry>, room: &Room, org: &str, actor: &str) {
    append(chain, &room.id, &Payload::MemberAdded { name: room.display_name(org) }, None, actor, 0);
}

pub fn member_removed(chain: &mut Vec<Entry>, room: &Room, org: &str, actor: &str) {
    append(chain, &room.id, &Payload::MemberRemoved { name: room.display_name(org) }, None, actor, 0);
}

/// A card and its report message. `report` is `None` only for a card that an older state file holds without one.
pub fn card_created(chain: &mut Vec<Entry>, room: &Room, card: &Card, report: Option<&Message>) {
    let message = opt(report.map(|m| m.id));
    let payload = Payload::Card {
        kind: &card.kind,
        severity: card.severity.as_deref().unwrap_or(""),
        title: &card.title,
        message: &message,
        text: report.map_or("", |m| m.text.as_str()),
    };
    append(chain, &room.id, &payload, Some(card.id), room.display_name(&card.reporter_org), card.created);
}

pub fn message_posted(chain: &mut Vec<Entry>, room: &Room, m: &Message) {
    let (id, replaces) = (m.id.to_string(), opt(m.replaces));
    let payload = Payload::Message { id: &id, kind: &m.kind, replaces: &replaces, text: &m.text };
    append(chain, &room.id, &payload, Some(m.card), room.display_name(&m.org), m.created);
}

/// The card's stage and outcome as they are now. `actor` is a display name, `OPERATOR` or `IMPORT`.
pub fn stage_set(chain: &mut Vec<Entry>, room: &Room, card: &Card, actor: &str, time: u64) {
    let payload = Payload::Stage { stage: &card.stage, outcome: card.outcome.as_deref().unwrap_or("") };
    append(chain, &room.id, &payload, Some(card.id), actor, time);
}

/// The stage a card of this kind starts in. A card still in it has no `stage` entry.
pub fn first_stage(card_kind: &str) -> &'static str {
    if card_kind == "finding" {
        "reported"
    } else {
        "posted"
    }
}

/// Builds every room's chain from a state file written before chains existed. The result depends on nothing but
/// its arguments, so loading the same file twice gives the same heads. For each room, in this order:
///
/// 1. `member-added` for each member the room lists, removed ones included, in the room's order;
/// 2. every message of the room's cards in the order they were stored: a card's report as a `card` entry, every
///    other message as a `message` entry, each with its author and its time;
/// 3. a `card` entry for any card that has no report message, in card order;
/// 4. a `stage` entry for each card whose stage is not its first one, in card order, with the stage and outcome
///    it has now. The moves in between were never stored, so they cannot be rebuilt;
/// 5. `member-removed` for each member marked removed;
/// 6. one `import` entry that says how many entries stand before it.
///
/// Entries of steps 1, 4, 5 and 6 have the actor `*import` and the time 0.
pub fn import(rooms: &BTreeMap<String, Room>, cards: &[Card], messages: &[Message]) -> BTreeMap<String, Vec<Entry>> {
    let mut chains: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    for room in rooms.values() {
        let chain = chains.entry(room.id.clone()).or_default();
        for m in &room.members {
            member_added(chain, room, &m.org, IMPORT);
        }
    }
    let by_id: HashMap<Uuid, &Card> = cards.iter().map(|c| (c.id, c)).collect();
    let mut reported = HashSet::new();
    for m in messages {
        let Some(card) = by_id.get(&m.card) else { continue };
        let (Some(room), Some(chain)) = (rooms.get(&card.room), chains.get_mut(&card.room)) else { continue };
        if m.kind == "report" && reported.insert(card.id) {
            card_created(chain, room, card, Some(m));
        } else {
            message_posted(chain, room, m);
        }
    }
    for card in cards {
        let (Some(room), Some(chain)) = (rooms.get(&card.room), chains.get_mut(&card.room)) else { continue };
        if !reported.contains(&card.id) {
            card_created(chain, room, card, None);
        }
    }
    for card in cards {
        let (Some(room), Some(chain)) = (rooms.get(&card.room), chains.get_mut(&card.room)) else { continue };
        if card.stage != first_stage(&card.kind) {
            stage_set(chain, room, card, IMPORT, 0);
        }
    }
    for room in rooms.values() {
        let Some(chain) = chains.get_mut(&room.id) else { continue };
        for m in room.members.iter().filter(|m| m.removed) {
            member_removed(chain, room, &m.org, IMPORT);
        }
        let entries = chain.len().to_string();
        append(chain, &room.id, &Payload::Import { entries: &entries }, None, IMPORT, 0);
    }
    chains
}
