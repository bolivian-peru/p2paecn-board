# Changelog

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
