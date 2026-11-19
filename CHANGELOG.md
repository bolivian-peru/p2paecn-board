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
