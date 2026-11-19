//! The room record `p2paecn-chain-1`: the server's code (src/chain.rs) must build, from the same values, the
//! same bytes and hashes as the client's code (partner-pack/p2p.mjs), which made tests/vectors/chain-1.json through
//! tools/gen-chain-vectors.mjs. The two were written separately against partner-pack/CHAIN.md.

use p2paecn_board::chain::{self, Entry, Payload};
use serde_json::Value;
use uuid::Uuid;

const VECTORS: &str = include_str!("vectors/chain-1.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS).unwrap()
}

/// A payload value: a string as it is, a number in decimal, `null` as the empty value.
fn value(values: &Value, name: &str) -> String {
    match &values[name] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => panic!("{name}: {other}"),
    }
}

fn payload_bytes(kind: &str, values: &Value) -> Vec<u8> {
    let v = |name: &str| value(values, name);
    match kind {
        "member-added" => Payload::MemberAdded { name: &v("name") }.bytes(),
        "member-removed" => Payload::MemberRemoved { name: &v("name") }.bytes(),
        "card" => Payload::Card { kind: &v("kind"), severity: &v("severity"), title: &v("title"), message: &v("message"), text: &v("text") }.bytes(),
        "message" => Payload::Message { id: &v("id"), kind: &v("kind"), replaces: &v("replaces"), text: &v("text") }.bytes(),
        "stage" => Payload::Stage { stage: &v("stage"), outcome: &v("outcome") }.bytes(),
        "import" => Payload::Import { entries: &v("entries") }.bytes(),
        other => panic!("unknown kind {other}"),
    }
}

#[test]
fn the_copy_in_the_partner_pack_is_the_same_file() {
    assert_eq!(VECTORS, include_str!("../partner-pack/vectors/chain-1.json"));
}

#[test]
fn the_server_builds_the_same_bytes_and_hashes_as_the_client() {
    let v = vectors();
    assert_eq!((v["format"].as_str(), v["genesis"].as_str()), (Some(chain::FORMAT), Some(chain::GENESIS)));
    let rooms = v["rooms"].as_array().unwrap();
    assert_eq!(rooms.len(), 2);
    let mut kinds = std::collections::BTreeSet::new();
    for room in rooms {
        let id = room["room"].as_str().unwrap();
        let mut prev = chain::GENESIS.to_owned();
        let mut built: Vec<Entry> = Vec::new();
        for (index, case) in room["entries"].as_array().unwrap().iter().enumerate() {
            let e: Entry = serde_json::from_value(case["entry"].clone()).unwrap();
            kinds.insert(e.kind.clone());
            let what = format!("{id} entry {}", e.seq);
            assert_eq!(e.seq, index as u64 + 1, "{what}");

            let payload = payload_bytes(&e.kind, &case["values"]);
            assert_eq!(payload, case["payloadRecord"].as_str().unwrap().as_bytes(), "{what}: payload record");
            assert_eq!(chain::sha256_hex(&payload), e.payload, "{what}: payload hash");

            let entry = chain::entry_bytes(id, e.seq, &e.kind, e.card, &e.actor, e.time, &e.payload, &prev);
            assert_eq!(entry, case["entryRecord"].as_str().unwrap().as_bytes(), "{what}: entry record");
            assert_eq!(chain::sha256_hex(&entry), e.hash, "{what}: entry hash");
            assert_eq!(e.prev, prev, "{what}: prev");
            prev = e.hash.clone();
            built.push(e);
        }
        assert_eq!(room["head"], serde_json::json!({ "seq": built.len(), "hash": prev }), "{id}: head");
        assert_eq!(chain::head(&built), (built.len() as u64, prev.as_str()));
        assert_eq!(chain::check(id, &built), Ok(()));
    }
    let all: Vec<&str> = kinds.iter().map(String::as_str).collect();
    assert_eq!(all, ["card", "import", "member-added", "member-removed", "message", "stage"], "every kind has a vector");
    assert!(rooms[0]["entries"].as_array().unwrap().len() >= 10, "a seq of two digits is covered");
}

#[test]
fn a_length_is_counted_in_bytes_and_a_value_cannot_pose_as_a_field() {
    // U+00FC is two bytes and U+1F600 four: the length is neither characters nor UTF-16 units.
    assert_eq!(chain::record("x", &[("a", "\u{fc}\u{1F600}")]), "p2paecn-chain-1 x\na:6:\u{fc}\u{1F600}\n".as_bytes());
    assert_eq!(chain::record("x", &[("a", "")]), b"p2paecn-chain-1 x\na:0:\n");
    // The same bytes split differently between two fields give different records.
    let one = Payload::Message { id: "i", kind: "reply", replaces: "", text: "a\ntext:1:b" }.bytes();
    let other = Payload::Message { id: "i", kind: "reply", replaces: "a", text: "b" }.bytes();
    assert_ne!(one, other);
    assert_ne!(Payload::MemberAdded { name: "member-1" }.hash(), Payload::MemberRemoved { name: "member-1" }.hash());
}

#[test]
fn appending_reproduces_the_vectors_and_any_change_breaks_the_check() {
    let v = vectors();
    let room = &v["rooms"][0];
    let id = room["room"].as_str().unwrap();
    let expected: Vec<Entry> = room["entries"].as_array().unwrap().iter().map(|c| serde_json::from_value(c["entry"].clone()).unwrap()).collect();
    let mut built = Vec::new();
    for (case, e) in room["entries"].as_array().unwrap().iter().zip(&expected) {
        let values = &case["values"];
        let s = |name: &str| value(values, name);
        let (a, b, c, d, f) = (s("kind"), s("severity"), s("title"), s("message"), s("text"));
        let (g, h, i, j) = (s("id"), s("replaces"), s("stage"), s("outcome"));
        let name = s("name");
        let payload = match e.kind.as_str() {
            "member-added" => Payload::MemberAdded { name: &name },
            "card" => Payload::Card { kind: &a, severity: &b, title: &c, message: &d, text: &f },
            "message" => Payload::Message { id: &g, kind: &a, replaces: &h, text: &f },
            "stage" => Payload::Stage { stage: &i, outcome: &j },
            other => panic!("{other}"),
        };
        chain::append(&mut built, id, &payload, e.card, &e.actor, e.time);
    }
    assert_eq!(built, expected);

    // Every single change to an entry is found at that entry, and so is a removed or swapped entry.
    let edits: [(&str, fn(&mut Entry)); 7] = [
        ("seq", |e| e.seq += 1),
        ("kind", |e| e.kind = "message".into()),
        ("card", |e| e.card = Some(Uuid::nil())),
        ("actor", |e| e.actor = "supplier-b".into()),
        ("time", |e| e.time += 1),
        ("payload", |e| e.payload = chain::GENESIS.into()),
        ("hash", |e| e.hash = chain::GENESIS.into()),
    ];
    for (what, edit) in edits {
        let mut changed = expected.clone();
        edit(&mut changed[6]);
        assert_eq!(chain::check(id, &changed), Err(7), "{what}");
    }
    let mut removed = expected.clone();
    removed.remove(4);
    assert_eq!(chain::check(id, &removed), Err(5));
    let mut swapped = expected.clone();
    swapped.swap(3, 4);
    assert_eq!(chain::check(id, &swapped), Err(4));
    // The same entries under another room's id do not verify: the room id is hashed into each entry.
    assert_eq!(chain::check("a-y", &expected), Err(1));
    // Cutting the end off leaves a chain that links. Only a saved head shows that.
    assert_eq!(chain::check(id, &expected[..5]), Ok(()));
}
