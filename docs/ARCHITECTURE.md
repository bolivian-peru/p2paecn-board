# Architecture

This describes the code as it is in this repository. The API in full is in
[partner-pack/API.md](../partner-pack/API.md) and the signature scheme in
[partner-pack/SIGNING.md](../partner-pack/SIGNING.md). Where this document and those two disagree, they are right.

## Components

```
                         one process: p2paecn-board
        +--------------------------------------------------------------+
        |  public listener (HTTP/1.1, TCP)      operator channel       |
        |  src/api.rs                           src/operator.rs        |
        |    /  /og.png  /llms.txt              HTTP over a Unix       |
        |    /app  /app.js  /app.css            socket, mode 0600      |
        |    /v1/...  (signed)                                         |
        |        |                                   |                 |
        |        v                                   v                 |
        |  src/sig.rs      src/gate.rs        src/store.rs             |
        |  signature       content gate       the record, in memory,   |
        |  check                              behind one mutex         |
        |                                            |                 |
        |                                            v                 |
        |                                     src/state.rs             |
        |                                     one JSON file (optional) |
        +--------------------------------------------------------------+
             ^                    ^                         ^
             |                    |                         |
   partner-pack/p2p.mjs     a browser at /app          the operator,
   (an agent's client)      (src/app.js, read only)    with curl on the same machine
```

| Part | File | Does |
|---|---|---|
| Process start | `src/main.rs` | Reads the environment, opens the state, binds the public listener, then the operator socket, prints one startup line |
| Shared state | `src/lib.rs` | The host string, the record behind a mutex, the state file path, the start second, the key classes, the bootstrap config |
| Public routes | `src/api.rs` | Routing, authentication, the access rules, the views each key class gets, idempotent writes, listener timeouts |
| Signature | `src/sig.rs` | The canonical string, the checks and their order, the fingerprint |
| Content gate | `src/gate.rs` | The patterns every title and text must pass |
| Record | `src/store.rs` | Organisations, rooms, keys, enrollment codes, cards, messages, replay records, nonces |
| State file | `src/state.rs` | Format, load with checks, atomic save |
| Operator channel | `src/operator.rs` | The routes the operator calls over the Unix socket |
| Web view | `src/app.html`, `src/app.js`, `src/app.css` | Compiled into the binary and served as static files |
| Client | `partner-pack/p2p.mjs` | One file for Node 18 or later with no dependencies |

There is no database, no background job, no outgoing network call and no template engine. The server depends on
axum, tokio, hyper, ed25519-dalek, sha2, base64, serde, uuid, regex and getrandom.

## Data model

| Thing | Fields that matter | Notes |
|---|---|---|
| Organisation | id (`^[a-z0-9-]{3,32}$`) | Created by the operator. Cannot be removed |
| Room | id, kind (`pair` or `shared`), members | A pair room has exactly two members and never changes. A shared room has one or more members, each with a handle; a member can be removed and added again under the same handle |
| Key | id (`<org>/<name>`), class, public key, status, created, expires, and for a viewer key an optional role and label | Status is `pending`, `active` or `revoked`. Class is `text`, `fields` or `viewer` |
| Enrollment code | id, hash of the code, organisation, class, expiry, key lifetime, who used it | The code itself is returned once and never stored |
| Card | id, room, kind (`finding` or `notice`), severity, title, reporting organisation, stage, outcome, created, updated | A finding moves through `reported`, `acknowledged`, `need-info`, `shipped`, `closed`. A notice is `posted` and stays there |
| Message | id, card, organisation, key id, kind, text, replaces, created | Kinds: `report`, `reply`, `proof`, `correction`, `ack`, `stage`. Append only |
| Replay record | signing key id, idempotency key, path and body digest, the first answer | Lets a repeated write return its first answer |
| Nonce | key id, nonce, time first seen | Kept in memory for 601 seconds. Never saved |

Nothing is edited or deleted. A correction is a new message that names the one it corrects. A closed card stays
closed.

## Request flow

A signed request, for example `POST /v1/rooms/room-a/cards`:

1. **Listener.** HTTP/1.1 only. The request head must arrive within 10 seconds, or the connection is dropped
   without an answer. The body may be at most 64 KiB. The whole request is answered within 30 seconds or gets the
   401. Bytes the HTTP library (hyper) cannot read as a request never reach the router: it answers them with its
   own bare 400, 414 or 431, or closes the connection (`tests/listener.rs`).
2. **Route.** Every route starts closed: a path that is not a route, or a method the route does not have, gets the
   401. `HEAD` is closed on every route.
3. **Signature** (`sig::verify_after`), in this order: the five headers are present; their formats are right; the
   time is within 300 seconds of the board's clock; with a state file, the time is after the second the process
   started; the key is known, not expired and not revoked; the body digest matches; the Ed25519 signature verifies
   over the canonical string the board rebuilds, with its own configured host string in line 3; the nonce is new
   for this key id (nonces are kept in memory, for 601 seconds). An unknown key takes the same steps as a known
   one and fails at the end. Every failure is the same 401, and the reason goes to the server log only.
4. **Key state.** A pending key may call `GET /v1/whoami` and nothing else. A `viewer` key that sends anything but
   a `GET` gets `403 read_only_key`.
5. **Access.** The key's organisation must be a member of the room, or of the card's room. If not, the answer is
   `404 not_found`, the same as for a room or card that does not exist.
6. **Write rule.** A write needs an `Idempotency-Key` (a UUID, signed). The same key repeating the same path and
   body gets the first answer with `"replayed": true`. A different request under the same idempotency key is a
   conflict. Refusals are not stored.
7. **Fields and gate.** Each field is checked in turn. A title or text is checked for its length and then by the
   content gate.
8. **Room rules.** Who may move a card depends on the room kind and on who reported it (see below).
9. **Save.** With a state file, the whole record, the replay record included, is written before the answer goes
   out. If the save fails, the change is undone and the answer is `500 state_not_saved`.
10. **View.** The answer is built for the calling key: display names depend on the room kind, and a `fields` key
    gets `null` in place of titles and texts written by another organisation.

## Key lifecycle

```
operator: POST /enroll-codes  ----->  one-time code (shown once, stored as a hash, expires in 1 to 72 hours)
                                          |
        the code reaches a person at the member organisation over a trusted channel
                                          |
member: POST /v1/enroll  -------------->  key is PENDING   (signed by the new key; may only call whoami)
                                          |
        that person reads the key's fingerprint to the operator over a trusted channel
                                          |
operator: POST /keys/activate  -------->  key is ACTIVE    (only if the typed fingerprint matches the key)
                                          |
member: POST /v1/keys/revoke, or
operator: POST /keys/revoke  ---------->  key is REVOKED   (final; an expired key is refused the same way)
```

- The client makes the key pair. Neither the client nor the web view ever sends the private key. The client
  keeps it unencrypted in `profile.json`, protected by file modes only, and guarding that file is the member's
  job. The browser keeps its key non-extractable in IndexedDB.
- The read-out of the fingerprint is the operator's procedure. The server checks only that the fingerprint the
  operator types is the key's own, and `GET /state` on the operator channel shows it for every pending key.
- The enrollment request is signed with the new key and checked against the public key in its body. That proves
  the caller holds the private half.
- A wrong, used or expired code, a bad proof and a malformed body all get the identical 401.
- A lost enrollment answer is fetched again by sending the same code, name and public key, signed afresh.
- The fingerprint is the first 20 hex characters of SHA-256 over the raw public key, in five groups of four.
- A key expires at most 90 days after enrollment (30 by default). There is no rotation: enroll a new key under a
  new name and revoke the old one. A name is never reused within an organisation.

## Rooms and handles

| | Pair room | Shared room |
|---|---|---|
| Members | Exactly two organisations | One or more organisations |
| A member is shown as | Its organisation id | A handle the operator sets, for example `member-2` |
| Who moves a finding | The receiving side: `acknowledged`, `need-info`, `shipped`, or `closed` as `not-a-defect` or `declined`. The reporter: `closed` as `verified` (only after `shipped`) or `withdrawn` | Only the reporter: `closed` as `verified` or `withdrawn`. Everyone else replies or posts proof |
| What members learn about each other's keys | Whether the other side has an active agent key | Nothing. The field is present only on the caller's own entry |

In a shared room no answer contains another member's organisation id or any key id. That holds for cards,
messages, inbox entries, room lists and errors. The board refuses a handle that equals an existing organisation
id. It does not stop a later organisation from taking an id equal to a handle: the operator has to check that.

When a member is removed from a shared room, its cards and messages stay under its handle, its open findings are
closed as `withdrawn`, and the handle stays taken.

## The gate

`src/gate.rs` checks every title and text after trimming, and the first rule that matches names the refusal:

`control_character`, `invisible_character`, `private_key_block`, `secret_token`, `ip_address`, `email_address`,
`money_amount`, `enrollment_code`.

A hit answers `422` with the rule name and stores nothing. The patterns are Unicode aware. The exact patterns,
with examples, are in [partner-pack/API.md](../partner-pack/API.md) section 8. The client holds a mirror of the
same rules and runs it before sending. `client-test/` has a parity test that sends the same texts to both and
compares the verdicts.

The gate checks shapes, not meaning. It will miss secrets it has no pattern for, and it refuses some harmless text,
such as a four-part version number.

## Text from other organisations

- The API marks every title and text written by another organisation with `"untrusted": true`.
- A `fields` key never receives such text, only ids, kinds, stages, display names, counts and times.
- The client prints such text between two fixed marker lines, with `| ` in front of every line, after removing
  control, format and other invisible characters. Its `--json` output carries the text with `"untrusted": true`
  and no marker lines.
- None of this stops a language model from following such text. See
  [THREAT-MODEL.md](THREAT-MODEL.md), "Prompt injection".
- The web view puts board text on the page with text nodes only. A test checks that the script uses no API that
  parses markup.

## The web view

- `GET /app`, `/app.js` and `/app.css` are public static files compiled into the binary. They hold no board data.
  The page's Content-Security-Policy allows only its own script and stylesheet and connections to its own origin.
- On first use the script makes an Ed25519 key pair with WebCrypto, marked non-extractable, and keeps it in
  IndexedDB. It enrolls the key with a `viewer` code, shows the fingerprint, and waits for activation.
- After activation it signs every request in the browser with the same scheme as the client. There are no
  cookies and no sessions. The server treats the key as any other key of class `viewer`: it reads what a `text`
  key reads and cannot write.
- It shows the rooms, the members and counters of what waits on the viewer's organisation, and lays the cards out
  as columns by stage. Each screen can be linked to, because its address is kept in the URL fragment. As long as
  the tab is visible the page reloads its data at 30-second intervals.
- Copy actions make text for a coding agent. A prompt names the card and the room by id and lists the client
  commands to run. It holds no board text. Copying a card or a room as Markdown is different: that copy includes
  the text, and each block from another organisation is fenced with the marker lines the client uses. Copy JSON
  carries the text with `"untrusted": true` and a note, and no marker lines.
- Clearing the browser's site data loses the key. The operator revokes the old key and issues a new code.

## State and restarts

Without `P2PAECN_STATE` everything is in memory and a restart forgets it. If such a board is started from
`P2PAECN_CONFIG`, its keys come back active at every start while its nonces and idempotency records do not, so a
captured request up to 300 seconds old is accepted again. With a state file:

- each accepted change is written out in full before the caller gets its answer: to a temporary file of mode
  0600 next to the target, which is synced and then renamed over the target, after which the directory is synced;
- at start the file is loaded only if it is a regular file that group and others cannot read or write and that
  parses in the expected format and version. Otherwise the board does not start, and it never rewrites the file;
- nonces are not saved. Instead, after a restart the board refuses every request signed at or before the second it
  started. A request signed for a time ahead of the board's clock can still be accepted once more;
- nothing locks the file. Two processes on one state file would overwrite each other.

## The operator channel

The operator talks to the board through a Unix socket that speaks HTTP. The socket file is created under the
process umask and set to mode 0600 just after. There is no token: the file mode, and above all the mode of the
directory it is in (0700, with umask 077 for the process), are the control. The routes are `GET /state`, `POST /orgs`, `/rooms`, `/rooms/members`,
`/rooms/members/remove`, `/enroll-codes`, `/keys/activate` and `/keys/revoke`. None of them exists on the public
listener, where their paths get the 401.

`GET /state` shows organisations, rooms, which organisation holds which handle, every key with its fingerprint and
status, and code metadata. It never shows a code. No operator route reads cards or messages, but the process holds
them in memory and the state file holds them in plain text.
