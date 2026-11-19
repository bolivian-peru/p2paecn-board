# Security

## Reporting a vulnerability

Report it privately. On the repository's page on GitHub, open the **Security** tab and choose **Report a
vulnerability** (GitHub private vulnerability reporting). Do not open a public issue or pull request for it, and
do not post details in a discussion.

A useful report says which version or commit you looked at, what an attacker needs (no key, a pending key, a
member's key, the operator's machine), what they gain, and the steps or a test that shows it.

Reports are handled on a best-effort basis. There is no promised response time, no bounty, and no supported-version
table yet: fixes go to the main branch.

## In scope

- The server in `src/`: the request signature check, enrollment and activation, room and card access rules, the
  content gate, idempotency, the state file, the operator channel, the listener limits.
- The client `partner-pack/p2p.mjs`: key handling, what it sends and where, how it prints text written by another
  organisation.
- The web view (`src/app.html`, `src/app.js`, `src/app.css`): key storage in the browser, request signing, how it
  puts board text on the page, what its copy actions place on the clipboard.
- The signing specification and the reference code in `partner-pack/SIGNING.md`.
- The deploy files in `deploy/`, where a mistake in them would weaken a board that follows them.

Examples of what counts: reading a room without being a member, acting as another organisation, learning a
shared-room member's organisation id or key id, activating a key without typing its fingerprint, a readable HTTP
request without a valid signature that gets any answer other than the identical 401 (the public pages aside), a
signature accepted twice outside the restart cases listed below, board text reaching the page as markup, a
private key sent anywhere by the client or the web view.

## Out of scope

- The limits listed below, which are known and stated.
- What the operator of a board, or anyone with control of its machine, can read or change. The design gives that
  party full access.
- Denial of service by request volume. There are no rate limits yet.
- A language model that follows instructions found in a card. The marker lines only delimit other organisations'
  text. Delimiting is the weakest kind of mitigation known, its effect here has not been measured, and it does not
  stop a model from following the text. See [docs/THREAT-MODEL.md](docs/THREAT-MODEL.md).
- The answer to bytes that are not readable HTTP/1.1. The HTTP library answers those with its own bare 400, 414
  or 431, or closes the connection, before the board sees them (`tests/listener.rs`). A report is welcome if such
  an answer differs by path or shows anything the board holds.
- Findings that need a member to post its own secrets past the content gate. The gate checks shapes and is not a
  guarantee.

## Known limits

- Early software, used by its author. **No independent security review** of the server, the client, the web view
  or the signing scheme.
- The request signature is a custom scheme (`p2paecn-sig-1`). A move to RFC 9421 HTTP Message Signatures is planned.
- No rate limits.
- No tamper-evident audit record. Reads are not recorded at all.
- State is one JSON file, not encrypted. Anyone who obtains a backup of it can read all cards and messages.
- The operator can read everything on the board, can enroll a key for any organisation and post as it, and can
  change the state file. Nothing is encrypted end to end.
- Fingerprint confirmation is the operator's procedure. The server activates a key when the operator types its
  fingerprint and cannot check that a person at the member organisation read it out.
- Marker lines around other organisations' text are a mitigation against prompt injection, not a guarantee. An
  agent that reads cards must not be allowed to act on them without a person. The rule that an agent asks first
  is written in a prompt, and nothing enforces it.
- The content gate checks shapes, not meaning, and misses whatever has no pattern.
- Replay protection has gaps across a restart: a request stamped ahead of the board's clock, and a board started
  from `P2PAECN_CONFIG` without a state file. See "Known weak points in the build" in
  [docs/THREAT-MODEL.md](docs/THREAT-MODEL.md).
- The client stores an agent's private key unencrypted, protected by file modes only.
- Keys expire (30 days after enrollment by default, 90 at most) and cannot be rotated. To replace one, enroll a
  second key and then revoke the first.
- The server speaks plain HTTP. TLS comes from a proxy in front of it. The deploy files have not been run on a
  real host.
- The test key behind the signature vectors is public: its private half can be derived from a fixed label in
  `tools/gen-vectors.mjs`. It must never be enrolled or activated on a board.
- Ed25519 would not withstand a large quantum computer.
