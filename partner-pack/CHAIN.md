# The room record: `p2paecn-chain-1`

Every room has a record: a list of entries, one for each thing that happened in the room, in which every entry
holds the hash of the entry before it. This document says exactly which bytes are hashed, how to fetch the record,
how to check it, and what a successful check proves. The client does all of it with `node p2p.mjs verify`.

**Read section 1 first.** The record makes a later change to a room's history visible to a member who kept an
earlier head. It does not make the board trustworthy on its own.

## Contents

1. [What this proves and what it does not](#1-what-this-proves-and-what-it-does-not)
2. [The record of a room](#2-the-record-of-a-room)
3. [The bytes](#3-the-bytes)
4. [The payload of each kind](#4-the-payload-of-each-kind)
5. [The entry and its hash](#5-the-entry-and-its-hash)
6. [Fetching the record](#6-fetching-the-record)
7. [How to verify](#7-how-to-verify)
8. [History from before the record existed](#8-history-from-before-the-record-existed)
9. [Test vectors](#9-test-vectors)

## 1. What this proves and what it does not

What a check that passes tells you:

- The entries the board gave you link: each is numbered in order, names the hash of the one before it, and has the
  hash of its own content.
- Every card, message, current stage, shown claim and current member your key can see has its entry, and the
  record holds no card, message, stage or claim entry for something the board no longer shows you.
- The head you saved the last time is still in the record at the same place. So nothing at or before that head was
  changed, removed or put in between since you saved it.

What it does not tell you, stated plainly:

- **It detects a change only after you have saved a head.** The first check of a room compares the board with
  itself. An operator who changed the history before your first check, and rebuilt the record to match, passes it.
- **The operator can still post as a member.** The operator can enroll a key for any organisation and write with
  it. Such a write is appended to the record like any other and verifies.
- **The operator can show two members different histories.** Each would verify against its own saved head. Two
  members find out only by comparing heads over a channel the operator does not control: both run `verify` when
  the room is quiet and read the entry number and the hash to each other.
- **The head is not signed by the board.** A saved head is your own evidence of what you were shown. It is not a
  proof that a third party has to accept, because anyone can write such a file.
- **It is only as good as the place you keep the head.** The client keeps it in `chain-heads.json` in its profile
  directory, mode 0600. Whoever can write that file can make a changed record pass.
- **Withheld text is not compared.** A key of class `fields` does not receive titles and texts written by other
  organisations, so it cannot recompute their hashes. It checks the links, the saved head, authors, times, stages
  and its own organisation's texts, and says for how many cards and messages it left the title or text out.
- **A hash is not secret text, but it confirms a guess.** The record gives every member, `fields` keys included,
  the SHA-256 of each payload. Nobody can read a text out of it. Someone who guesses a whole payload exactly
  (the text, and for a card or message also its ids) can confirm the guess.
- **A claim that was taken off a card is not detected.** A claim the board shows must have its entry, but a card
  that shows none always passes (section 7).
- **Reads are not recorded**, and neither is anything the operator does outside rooms: creating organisations,
  issuing codes, activating and revoking keys.
- **The time of an entry is the board's own clock.** Nothing outside the board witnesses it.
- Nobody independent has reviewed this format or the code that implements it.

## 2. The record of a room

There is one record per room and none for the board as a whole. A record of the whole board would let a member
count the writes in rooms it is not in.

An entry has these parts:

| Part | What it is |
|---|---|
| `seq` | The entry's number in its room, from 1, without gaps |
| `kind` | `member-added`, `member-removed`, `card`, `message`, `stage`, `claim`, `release` or `import` |
| `card` | The id of the card the entry is about. `null` for `member-added`, `member-removed` and `import` |
| `actor` | Who did it, under the name the room's members see: the organisation id in a pair room, the handle in a shared room. Or `*operator` for something the operator did, or `*import` (section 8). Never a key id, and never an organisation id in a shared room |
| `time` | Unix seconds on the board's clock, or `0` where no time is recorded (see below) |
| `payload` | SHA-256 of the payload record (section 4), as 64 lower-case hex characters |
| `prev` | `hash` of the entry before. For entry 1: 64 zeros |
| `hash` | SHA-256 of the entry record (section 5), as 64 lower-case hex characters |

The **head** of a record is the `seq` and `hash` of its last entry.

Everything in an entry is the same whoever asks, and is something every member of the room can already see. A
title or a text is in the record only inside a hash.

When entries are appended:

| Event | Entries |
|---|---|
| The operator creates a room | One `member-added` for each member, by `*operator` |
| The operator adds a member to a shared room | One `member-added`, by `*operator` |
| The operator removes a member from a shared room | One `member-removed`, by `*operator`, then one `stage` entry by `*operator` for each open finding of that member, which the removal closes as `withdrawn`, then one `release` entry by `*operator` for each live claim the member held on another member's card |
| A member claims a card, or renews its claim | One `claim`, by the claiming member |
| A member gives up its claim | One `release`, by that member |
| A member posts a card | One `card`, by its reporter. It covers the card and its report message |
| A member posts a reply, proof, correction or ack | One `message`, by its author |
| A member moves a card to another stage | Two entries by the member: a `message` for the text of the move (its message kind is `stage`), then a `stage` with the stage and outcome it set |

A stage move is two entries because a card's stage and the text that came with the move are two things the board
shows separately, and because a board upgraded from an older state file knows the texts of past moves but not
which stage each of them set (section 8).

A claim that ends because its time has passed, or because its card was closed, gets no entry: nothing happens on
the board at that moment, and the `stage` entry says that the card is closed.

A write that is refused appends nothing. A write repeated with the same `Idempotency-Key` returns its first answer
and appends nothing.

**Membership entries carry no time.** Their `time` is `0`. An exact time would let a member of a shared room match
the moment a handle arrived with the moment a pair room appeared, and so learn which organisation is behind the
handle. Their place in the order still shows between which two entries the change was made, and the `stage`
entries that follow a removal carry the time of the removal. A member who joins a shared room later can also see
how many membership changes came before, and can test a guessed handle against the payload hash of a
`member-added` entry.

## 3. The bytes

Everything that is hashed is a **record** in this sense: a header line, then one line per field.

```
p2paecn-chain-1 <what> LF
<name>:<length>:<value> LF
<name>:<length>:<value> LF
...
```

- `LF` is the single byte `0x0A`. There is exactly one space between `p2paecn-chain-1` and `<what>`.
- `<name>` is a fixed ASCII word given in sections 4 and 5. The fields come in the order given there, every field
  is present, and no other field is.
- `<value>` is the value's bytes in UTF-8, copied as they are. Nothing is escaped, trimmed or normalised. A value
  may hold line feeds and colons.
- `<length>` is the number of **bytes** of `<value>`, in decimal ASCII digits without a leading zero (`0` for an
  empty value). It is not the number of characters or of UTF-16 units: `ü` counts 2 and an emoji such as U+1F600
  counts 4.
- A value that does not exist (a notice has no severity) is the empty value, written `name:0:` and `LF`.
- A number (a `seq`, a `time`, a count) is written in decimal ASCII digits without a leading zero. An id is
  written as the board prints it: 36 characters, lower case, with dashes.
- After the last field's `LF` there is nothing more.

A hash is SHA-256 over all bytes of the record, written as 64 lower-case hex characters.

Because every value is preceded by its length, a text cannot pose as another field: a text that holds the line
`text:3:abc` is still one value of the length stated for it.

## 4. The payload of each kind

The payload record's header is `p2paecn-chain-1 payload <kind>`. Its fields, in order:

| Kind | Fields | Values |
|---|---|---|
| `member-added`, `member-removed` | `name` | The name the room shows for the member: an organisation id in a pair room, a handle in a shared room |
| `card` | `kind`, `severity`, `title`, `message`, `text` | The card's `kind` (`finding` or `notice`), its `severity` (empty for a notice), its `title`, the `id` of its report message, and the `text` of that message |
| `message` | `id`, `kind`, `replaces`, `text` | The message's `id`, its `kind` (`reply`, `proof`, `correction`, `ack` or `stage`), the id it `replaces` (empty unless it is a correction), and its `text` |
| `stage` | `stage`, `outcome` | The stage the card was set to, and the outcome (empty unless the stage is `closed`) |
| `claim` | `until`, `note` | The time the claim runs to, as a number, and its note (empty when there is none) |
| `release` | none | The record is the header line alone: `p2paecn-chain-1 payload release` and `LF` |
| `import` | `entries` | How many entries stand before this one in the room, as a number |

Titles and texts are the ones the board stores and returns: already trimmed at both ends by the board when they
were posted, and otherwise byte for byte what was sent. The values are exactly the fields of the same names in
[API.md](API.md) section 4, where `null` there is the empty value here.

Example, the payload of a new finding (the third entry of room `a-x` in the test vectors). The text has two lines:

```
p2paecn-chain-1 payload card
kind:7:finding
severity:5:wrong
title:40:List endpoint skips one item when paging
message:36:7d3a9f40-0000-4000-8000-000000000001
text:54:Page two starts at item 22.
Item 21 is never returned.
```

Its SHA-256 is `04fea823155fe0fe3f6a909021518e92931096b9ee2099bdb0878496c7eb488c`. (The block above ends with one
`LF` after `returned.`)

## 5. The entry and its hash

The entry record's header is `p2paecn-chain-1 entry`. Its fields, in order:

| Field | Value |
|---|---|
| `room` | The room's id |
| `seq` | The entry's number |
| `kind` | The entry's kind |
| `card` | The card's id, or empty |
| `actor` | The actor |
| `time` | The time, `0` where none is recorded |
| `payload` | The payload hash, 64 hex characters |
| `prev` | The hash of the entry before, or 64 zeros for entry 1 |

The entry's `hash` is the SHA-256 of this record. The room id is part of it, so an entry of one room cannot stand
in another room's record.

The same example as an entry:

```
p2paecn-chain-1 entry
room:3:a-x
seq:1:3
kind:4:card
card:36:0b6f2c1e-0000-4000-8000-000000000001
actor:10:reseller-x
time:10:1794850000
payload:64:04fea823155fe0fe3f6a909021518e92931096b9ee2099bdb0878496c7eb488c
prev:64:de339a731b617d05162bf972ae6dcf35fde93c822777009d39a1b659cc0a3a85
```

Its SHA-256, the entry's `hash`, is `4ab15b30c25b6f2b3f5dfbd8c8f98085ba239ff166e2599f01cb6f5efa55d387`.

## 6. Fetching the record

`GET /v1/rooms/{room}/chain`, signed like every other request ([API.md](API.md) section 3.13). Query:

| Name | Meaning |
|---|---|
| `after` | A `seq`. Entries with a higher `seq` are returned. Default `0`: from the start |
| `limit` | 1 to 1000 entries. Default 200 |

The answer below is put together from the test vectors (the entry of sections 4 and 5 and the head of that room),
to show the shape. Answers captured from a running board are in API.md section 3.13.

```json
{
  "room": "a-x",
  "format": "p2paecn-chain-1",
  "entries": [
    { "seq": 3, "kind": "card", "card": "0b6f2c1e-0000-4000-8000-000000000001", "actor": "reseller-x",
      "time": 1794850000,
      "payload": "04fea823155fe0fe3f6a909021518e92931096b9ee2099bdb0878496c7eb488c",
      "prev": "de339a731b617d05162bf972ae6dcf35fde93c822777009d39a1b659cc0a3a85",
      "hash": "4ab15b30c25b6f2b3f5dfbd8c8f98085ba239ff166e2599f01cb6f5efa55d387" }
  ],
  "head": { "seq": 12, "hash": "09f0760b84bfb0092426b4103ce7fc9520a8493f9d0e5a4b2c5b2d14aa885b64" }
}
```

Entries come oldest first. There are more when the last entry's `seq` is below `head.seq`: ask again with `after`
set to that `seq`. An `after` at or above the head gives an empty list and the head.

The answer is the same for every member of the room and for every key class. A room you are not in answers
`404 not_found`, exactly like a room that does not exist. `GET /v1/rooms` and `GET /v1/whoami` also give each
room's head, as `chain: { seq, hash }`.

## 7. How to verify

This is what `node p2p.mjs verify [room]` does for each room. A client of your own should do the same.

1. **Fetch** the whole record, then every card of the room with its thread (`GET /v1/rooms/{room}/cards` page by
   page, then `GET /v1/cards/{id}` for each), then the record once more from the last entry you hold. If entries
   were added in between, the room changed while you read it: start again.
2. **Links.** For each entry in order: its `seq` is the previous one plus 1, starting at 1. Its `prev` is the hash
   of the entry before, or 64 zeros. Its `hash` is the SHA-256 of the entry record you build yourself from its
   parts and your room id. The head the board names is the last entry.
3. **The saved head.** If you saved a head `(seq, hash)` for this room before: the record must still have an entry
   with that `seq`, and its `hash` must be the saved one. If the record is now shorter, entries were removed. If
   the hash differs, the record was rewritten.
4. **Cards.** For each card: there is exactly one `card` entry with its id. Its `actor` is the card's `reporter`,
   its `time` the card's `created`, and its `payload` the hash of the payload record you build from the card's
   `kind`, `severity` and `title` and from the `id` and `text` of the first message of the thread (the one of kind
   `report`).
5. **Messages.** For each further message of the thread there is one `message` entry of that card with the
   message's `author` as `actor`, its `created` as `time`, and the payload hash you compute from its `id`, `kind`,
   `replaces` and `text`. Each entry is used for one message. No `message` entry of the card is left over.
6. **Stages.** The last `stage` entry of the card has the payload hash you compute from the card's `stage` and
   `outcome`. A card without a `stage` entry is in its first stage (`reported` for a finding, `posted` for a
   notice) and has no outcome.
7. **Claims.** If a card shows a claim, the last entry of that card whose kind is `claim` or `release` is a
   `claim`, its `actor` is the claim's `by`, and its `payload` is the hash you compute from the claim's `until`
   and `note`. A card that shows no claim needs no entry: see below.
8. **Nothing left over.** Every `card`, `message`, `stage`, `claim` and `release` entry belongs to a card the
   board shows you.
9. **Members.** For each name the room list gives for the room (yours and the others'), the last membership entry
   whose payload hash is that of the name is a `member-added`.
10. **Import.** There is at most one `import` entry, and its payload hash is that of the number of entries before
    it.
11. If everything held, **save the head** together with the time. If anything failed, keep the head you had: it
    is the evidence. Tell a person. Do not act on the board because of it.

Where your key does not receive a title or a text (class `fields`), skip the payload comparison of that entry and
count it. The same holds for the note of another organisation's claim.

What the check cannot tell about claims: a claim that the board shows must be in the record, so one that was put
on a card without its entry, or whose note or time was changed, is found. A claim that is missing from a card is
not found. A claim ends silently when its time passes, and that time is inside the payload hash, so the record
does not let you tell a claim that ended from a live one that was taken off.

The client exits with code 5 when a room does not verify, prints which entry, card or message differs, and leaves
the saved head as it was. `--json` gives the same per room: `ok`, `head`, the counts, the `saved` head and a list
of `problems`.

The cost: one request per 1000 entries, one per 100 cards and one per card, each time. That suits a board with a
modest number of cards. It has not been measured on a large one.

## 8. History from before the record existed

A board that ran an earlier version has a state file without records. When the new version first starts on such a
file, it builds each room's record from what the file holds, in this fixed order:

1. a `member-added` for each member the room lists, removed ones included, in the room's order;
2. every message of the room in the order it was stored: a card's report as a `card` entry, every other message
   as a `message` entry, with its author as `actor` and its own time;
3. a `card` entry for a card that has no report message (its `message` and `text` are empty);
4. a `stage` entry for each card that is not in its first stage, with the stage and outcome it has now, in the
   order the cards were created;
5. a `member-removed` for each member that is marked removed;
6. one `import` entry, whose payload is the number of entries before it.

The entries of steps 1, 4, 5 and 6 have the actor `*import` and the time `0`: the old file does not say who did
them or when. The stage moves a card went through before the upgrade are not in the record, only where it stood at
the upgrade. Until the first accepted change after the upgrade, the record is rebuilt at each start, and the same
file gives the same record.

**Entries up to and including the `import` entry prove nothing about the time before the upgrade.** They are a
statement by the board, made at the upgrade, of what its state file then held. If that file had been changed
before, the record was built from the changed file. What they give you is a fixed point: once you have saved a
head at or after the `import` entry, a later change to that older history shows like any other.

The client prints `imported  entries 1 to N were built when the board was upgraded` for such a room.

## 9. Test vectors

[vectors/chain-1.json](vectors/chain-1.json) holds three rooms, a pair room, a shared room that was built by an
import, and a pair room with claims, with every kind of entry. For each entry it gives the payload values, the payload record and the entry
record as text (each is valid UTF-8; hash its UTF-8 bytes), and the entry as the API returns it. The values include
a title and texts with multi-byte characters, a four-byte character, line feeds, colons, a text that looks like a
field line, empty values, and entry numbers of two digits.

To check your own code: build each payload record from `values` and compare it with `payloadRecord`, hash it and
compare with `entry.payload`; build the entry record from `entry` and the room id and compare with `entryRecord`,
hash it and compare with `entry.hash`; check that each `entry.prev` is the hash before it and that `head` is the
last entry.

The vectors are generated with the client's code (`tools/gen-chain-vectors.mjs` in the repository). The board's
own implementation in Rust is checked against the same file (`tests/chain_vectors.rs`), and the client's `verify`
is run against a real board in `client-test/verify.test.mjs`.
