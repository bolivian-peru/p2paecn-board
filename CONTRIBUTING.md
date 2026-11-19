# Contributing

Thank you for looking. This is a small project, so reviews happen on a best-effort basis.

Security problems do not go in issues or pull requests. See [SECURITY.md](SECURITY.md).

## Build

You need macOS or Linux, a Rust toolchain (rustup installs the version named in `rust-toolchain.toml`), and Node
for the client and web view tests.

```bash
cargo build
```

## Test

```bash
cargo test
node --test client-test/      # starts target/debug/p2paecn-board on port $P2P_TEST_PORT (8821 if unset)
node --test app-test/         # the same binary on port $APP_TEST_PORT (8841 if unset)
```

All three must pass before a pull request is opened. The Node tests start the debug binary on a local port, so run
`cargo build` first and set the port variables if the defaults are taken. One client test needs `expect` for a
terminal and is skipped without it.

Some Rust tests start the real binary with its operator socket under `target/tmp/`. A Unix socket path has to stay
under about 104 bytes on macOS and 108 on Linux, and the tests add nearly 40 characters to the path of the clone.
So when the clone's own path is longer than about 65 characters on macOS, those tests fail with `path must be
shorter than SUN_LEN`. Clone somewhere shorter, or point `CARGO_TARGET_DIR` at a short path for `cargo test`. Set
that variable for `cargo test` only: the Node tests start `./target/debug/p2paecn-board` and do not read it.

## The rules

1. **API changes are additive.** Members write their own clients against `partner-pack/API.md`. Add a field or a
   route; do not rename, remove or change the meaning of one. The signature scheme, its headers, the environment
   variables, the binary name and the package name are part of the protocol and stay as they are. A change that
   cannot be additive needs an issue first.
2. **No board text as markup.** Text that came from the board is never parsed as HTML, Markdown or anything else
   that can carry behaviour. In the web view it goes on the page through text nodes only. In the client it is
   printed between the marker lines with every line prefixed. Do not add a rendering shortcut.
3. **Tests with every change.** Anything that touches signing, enrollment, room access, the move rules or the gate
   must come with a test that is red without the change and green with it, and that covers the refusals as well as
   the accepted cases. A change without a test is not merged.
4. **The gate lives in two places.** A change to `src/gate.rs` is made in the client's mirror in
   `partner-pack/p2p.mjs` in the same pull request. The parity test in `client-test/` checks that they agree.
5. **Closed by default stays.** A new public path, a new unsigned answer, or a refusal that says why, needs a
   reason in the pull request and a test.
6. **The client has no dependencies** and stays one file. The server adds a dependency only when the alternative
   is clearly worse.
7. **Examples are invented.** Use the organisations already in the tests and documents (`supplier-a`,
   `reseller-x`, handles such as `member-1`) and the host `board.example`. No real company, product, person,
   address or key in code, tests, documents or commit messages.
8. **Documents say what was run.** If you write that something works, say how you know. If something was not run,
   say so.
9. **Limits stand beside claims.** A document or page that says what the board protects also says what it does
   not. Do not shorten "Status and limits" in the README to make a feature read better.

## Signature vectors

```bash
node tools/gen-vectors.mjs > tests/vectors/sig-1.json
cp tests/vectors/sig-1.json partner-pack/vectors/sig-1.json
```

The script is deterministic. `tests/sig_vectors.rs` compares the two copies byte for byte.

## Pull requests

- One change per pull request, with a description of what it does and how it was tested.
- Keep the documents in `partner-pack/` in step with the code: a member reads those, not the source.
- By contributing you agree that your contribution is licensed under the Apache License 2.0, as section 5 of the
  licence states.
