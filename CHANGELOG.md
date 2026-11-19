# Changelog

## Unreleased

- A hash-chained record per room (`p2paecn-chain-1`, specified in `partner-pack/CHAIN.md`): every accepted card,
  message and stage move, and every membership change by the operator, appends an entry with the display name of
  the author, the time, a SHA-256 of the payload and the hash of the entry before.
- `GET /v1/rooms/{room}/chain` with `after` and `limit`. Each room in `GET /v1/rooms` and `GET /v1/whoami` has a
  new member `chain` with the head `{seq, hash}`, and so has each room in the operator's `GET /state`. Nothing
  else in the API changed. The client and the client tests of 0.2.0 were run against this server: all 19 pass.
  One web view test of 0.2.0 fails, because it compares a whole `whoami` answer and the rooms in it now carry `chain`.
- `p2p.mjs verify [room]`: recomputes every hash, compares the cards, messages, stages and members it can see
  with the record, saves the head that verified in `chain-heads.json` in the profile, and on the next run checks
  that the saved head is still in place. Exit code 5 when a room does not verify. The file `p2p.mjs` changed, so
  its SHA-256 changed.
- The state file is now version 2 and holds the records. A version 1 file still loads: its records are built
  from its cards and messages at start, and the file is written as version 2 with the first accepted change. The
  server of 0.2.0 does not read a version 2 file, so copy the state file before upgrading.
- Test vectors for the record in `tests/vectors/chain-1.json` and `partner-pack/vectors/chain-1.json`.

- Rate limits (`src/rate.rs`): per key on signed requests (1200 per minute by default) and on writes (60), per
  room on writes (240), per source address on enroll attempts and requests answered with the 401 (120). A signed
  caller over a limit gets `429 rate_limited` with `Retry-After`; its nonce is not used up, nothing is stored and
  nothing is appended to a room's record. A caller without a valid signature keeps getting the identical 401.
  The limits are set with `P2PAECN_LIMIT_KEY_REQUESTS`, `P2PAECN_LIMIT_KEY_WRITES`, `P2PAECN_LIMIT_ROOM_WRITES`,
  `P2PAECN_LIMIT_UNSIGNED` and `P2PAECN_LIMIT_WINDOW_SECONDS`; `0` turns one off. `P2PAECN_SOURCE_HEADER` names a
  header to take the source address from; none is trusted by default.
- The client waits and tries again on `429` with `Retry-After`, at most three more times and at most 120 seconds
  each, for reads and for writes, and says so. A 429 that remains is exit code 3.
- A client of 0.2.0 does not know the 429. By its code (this was read, not run) it reports `refused by the board:
  rate_limited (HTTP 429)` with exit code 2 and, for a write, forgets the idempotency key. The client tests of
  0.2.0 were run against this server with the default limits: 17 of 19 pass, and the two that post more than
  sixty cards within seconds get `429 rate_limited`. With the write limits switched off all 19 pass.

- `GET /v1/inbox/wait`, a long poll: answers at once when something is new in the caller's rooms after the cursor
  it sends, else holds the request for up to 25 seconds (`P2PAECN_WAIT_SECONDS`, 28 at most). At most two held
  requests per key (`409 too_many_waits`). `GET /v1/inbox` is unchanged.
- `p2p.mjs wait [--timeout <seconds>] [room]`: loops over the long poll until another member does something,
  prints it with the usual marker lines and exits 0, or exits with the new code 6 when nothing happened.

- Claims: `POST /v1/cards/{id}/claim` with `lease` (60 to 86400 seconds, default 3600), an optional `note` and
  `release`. A card carries `claim: {by, until, note, mine, untrusted}` or `null`. One live claim per card
  (`409 claimed` with the holder), renewed by claiming again, ended by time, release or closing. New kinds
  `claim` and `release` in the room record, with vectors (a third room in `chain-1.json`; the two rooms that
  were there are unchanged). `p2p.mjs claim` and `release`; `show`, `cards`, `inbox` and the web view show a live
  claim. A state file from before claims loads unchanged; a card nobody claimed has no `claim` member in the file.

What a claim is not: a lock, or evidence that anyone is working. It blocks no reply and no move. The operator can
remove or forge one like anything else; `verify` finds a forged or changed claim and cannot find a removed one.

What the long poll does not do: the board still pushes nothing, and `wait` reports only what happens while it
runs. The total number of held requests is bounded only by two per key, and it was not run through a proxy.

What the rate limits do not do: they are counters in memory, forgotten at a restart. They do not limit
connections or total load, and a member with several keys has several budgets. Behind a proxy the limit on
unsigned traffic counts the proxy's address for every caller unless `P2PAECN_SOURCE_HEADER` is set. A source over
that limit still has its signatures checked: only its enroll attempts and its log lines are cut.

What the record does not do: it shows a change only to a member who saved a head before it. It does not stop the
operator from posting as a member, from enrolling a key for any organisation, or from showing two members
different histories unless they compare heads out of band. The head is not signed by the board. Entries built
from a version 1 file prove nothing about the time before the upgrade. The format has had no outside review.

## 0.2.0

First public release.

- The board server: signed requests (`p2paecn-sig-1`, Ed25519), enrollment with one-time codes and activation by
  fingerprint, pair and shared rooms, findings and notices with threads, key classes `text`, `fields` and
  `viewer`, the content gate, idempotent writes, an optional state file, and an operator channel over a Unix socket.
- `partner-pack/`: the one-file client `p2p.mjs`, together with the documents a member needs to join.
- A read-only web view at `/app` that signs its requests in the browser.
- A written deploy procedure for one Linux VM. It has not been run on a real host.

No independent security review has been done, and the operator of a board can read everything on it. See "Status
and limits" in the README.
