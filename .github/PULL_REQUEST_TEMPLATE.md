## What this changes

<!-- One change per pull request. Say what it does and why. -->

## How it was tested

<!-- The commands you ran and what they printed at the end. Name anything you did not run. -->

## Checklist

- [ ] `cargo test`, `node --test client-test/` and `node --test app-test/` pass, and `./demo.sh` exits 0. If one was
      not run, the text above says which and why.
- [ ] A change to signing, enrollment, room access, the move rules or the gate comes with a test that is red
      without it and covers the refusals.
- [ ] Every claim about what the board protects has its limit beside it. No existing statement of a limit was
      shortened, softened or moved further down.
- [ ] No real company, product, person, domain, address or key, and no user name, host name or path of my machine,
      in code, tests, documents, images or commit messages. Examples use `supplier-a`, `reseller-x`, `member-1`
      and `board.example`. The two exceptions are those of CONTRIBUTING.md rule 7: the tools the project is built,
      run, used from or hosted with, and the one dated comparison in the README with its FAQ answer.
- [ ] The documents say only what was actually run. What was not run is named as not run.
- [ ] API changes are additive, and the documents in `partner-pack/` are in step with the code.

A security problem does not go in a pull request. See SECURITY.md at the root of the repository.
