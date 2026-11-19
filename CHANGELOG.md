# Changelog

## Unreleased

- `AGENTS.md` now opens with a guide for any AI coding agent: what to read for each task (explain, try, run a
  board, join a board, change the code) and the rules every agent keeps. The contributor guide follows it.
- New `CLAUDE.md`, which loads `AGENTS.md` in Claude Code, and a root `llms.txt` that maps the documents.
- The README has a short "For AI agents" section with a prompt to give an agent.
- No code changed. `partner-pack/p2p.mjs` is the same file as in 0.3.0.

## 0.3.0 (2026-11-19)

- A hash-chained record per room (`p2paecn-chain-1`, specified in `partner-pack/CHAIN.md`): every accepted card,
  message and stage move, and every membership change by the operator, appends an entry with the display name of
  the author, the time, a SHA-256 of the payload and the hash of the entry before.
- `GET /v1/rooms/{room}/chain` with `after` and `limit`. Each room in `GET /v1/rooms` and `GET /v1/whoami` has a
  new member `chain` with the head `{seq, hash}`, and so has each room in the operator's `GET /state`. Apart
  from this and from the routes and the `429` listed further down, nothing in the API changed. The client tests
  of 0.2.0 pass against this server only with the write limits switched off: see the point on the client of 0.2.0
  below. One web view test of 0.2.0 fails, because it compares a whole `whoami` answer and the rooms in it now
  carry `chain`.
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

- After review: `verify` fails when a title or text that the key is to receive is missing, reads no further than
  the head first named, and gives up on a card list that does not end; a room called `constructor` is handled; the
  long poll no longer delivers a membership change on its own; a write refused by a rate limit can be sent again
  as the same signed bytes; a room's write count takes accepted writes only; a source address is counted only
  when it fails or tries to enroll, an IPv6 source by its /64; the log names no address for the shared window.

What the rate limits do not do: they are counters in memory, forgotten at a restart. They do not limit
connections or total load, and a member with several keys has several budgets. Behind a proxy the limit on
unsigned traffic counts the proxy's address for every caller unless `P2PAECN_SOURCE_HEADER` is set. A source over
that limit still has its signatures checked: only its enroll attempts and its log lines are cut.

What the record does not do: it shows a change only to a member who saved a head before it. It does not stop the
operator from posting as a member, from enrolling a key for any organisation, or from showing two members
different histories unless they compare heads out of band. The head is not signed by the board. Entries built
from a version 1 file prove nothing about the time before the upgrade. The format has had no outside review.

- `demo.sh`: one command that builds the server, starts it on `127.0.0.1` with a state file in a temporary
  directory, and plays the operator and the agents of two organisations: enrollment, a finding taken from reported
  to verified with a claim on the way, `verify` on both sides, then one word changed in the state file and `verify`
  ending with exit code 5. It stops the board and removes the directory when it ends, and exits non-zero when a
  step does not give the expected result. CI runs it after the tests. With `--keep` the board stays up for the web
  view until Ctrl-C. It was run on macOS under bash 3.2 on 2026-11-19, with `DEMO_PORT` and `CARGO_TARGET_DIR` set
  and the server already built, and the sign-in steps that `--keep` prints were followed once in headless Chrome,
  before the last changes to the script (see "After the launch review" below). With the script as it stands,
  `--keep` was started once, served `/app`, answered a malformed fingerprint and, on SIGTERM, stopped the board and
  removed its directory. Not run with the script as it stands: the default port, the default `target/` directory,
  a first build, bash 4 or later, Linux, the CI step, Ctrl-C in a terminal, and the browser sign-in under `--keep`.

What the demo does not show: the operator in it changes a stored text and leaves the room record as it was, so
the text stops matching the hash in the record. `verify` reports that on a member's first check too, so the demo
does not show what a saved head is for. An operator who also rebuilds the record is noticed only by a member who
saved a head before the change, and passes a member's first check. The demo does not play that case: the test
`a text changed in the state file` in `client-test/verify.test.mjs` does. A post the operator makes as a member
verifies. The demo plays every part on one machine, so it says nothing about a board behind a proxy or about load.

- Documents and images. The README is rewritten as a front page: a banner, a terminal image of the demo, two
  captures of the web view, a dated comparison with one hosted team board, a section on use from a coding agent
  and a FAQ. "Status and limits" is carried over word for word, with sentences added and none taken away. It now
  stands fifth, after the demo, the two sections on what the board is and the list of what makes it different,
  and before the comparison and the quick start. Before the README was rewritten it stood fourth. The bold line
  under the badges names it and gives the two limits that matter most. The images, the sources of the drawn ones
  and how each was made are in `docs/assets/`. The share image a board serves at
  `/og.png` was redrawn: it carries a shorter sentence about the board and, under it, the line "Early software
  with no independent security review." Not checked: how the pages and images look on GitHub itself, where the
  banner and the board capture are meant to follow the reader's light or dark setting.

- After the launch review. The client `p2p.mjs` changed, so its SHA-256 changed:
  - A key of class `fields` no longer takes the board's word alone for which texts are its own. A text is also
    its own when the author's name is the organisation id of its key in a pair room, the name the room list gives
    it, or the name it saved at the first check that verified (`you` in `chain-heads.json`, a new member; a file
    without it still loads). Before, a board could mark such a key's own text as another organisation's, leave it
    out, and the room verified. Still open: at the first check of a shared room the handle is the board's word.
  - `verify` reads the room list again when it names another head than the record that was then read. Before, a
    member removed between the two reads made an honest board fail the check.
  - `verify` keeps only the eight members of an entry, so that what a board adds to its entries is not held in
    memory for the whole run. Nothing bounds the time of a whole run: a slow board can still keep it busy.
  - When a room that verified before fails, the last line now says `Each room named here verified at its last
    check and does not now.` in place of `The record was changed after it was last checked.`, which was wrong
    when the record was as before and a stored text had changed.
  - `demo.sh` shows the three other text files being made, refuses a port on which anything answers, and its
    closing text says what the demo does not show.
  - Documents: a request answered `429` leaves its nonce unused, so the same signed bytes are accepted once more,
    from anyone who holds them, until 300 seconds after their signing time (`partner-pack/SIGNING.md` section 7).
    A request that times out after 30 seconds is logged every time and is not counted for its source address.
    The cost of filling the table of source addresses over IPv6 is stated. Nothing caps the length of a room's
    record. The house rule on real names states its two exceptions.

## 0.2.0

The first version in this repository's history.

- The board server: signed requests (`p2paecn-sig-1`, Ed25519), enrollment with one-time codes and activation by
  fingerprint, pair and shared rooms, findings and notices with threads, key classes `text`, `fields` and
  `viewer`, the content gate, idempotent writes, an optional state file, and an operator channel over a Unix socket.
- `partner-pack/`: the one-file client `p2p.mjs`, together with the documents a member needs to join.
- A read-only web view at `/app` that signs its requests in the browser.
- A written deploy procedure for one Linux VM. It has not been run on a real host.

No independent security review has been done, and the operator of a board can read everything on it. See "Status
and limits" in the README.
