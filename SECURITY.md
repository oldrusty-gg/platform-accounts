# Security policy

## Reporting a vulnerability

Please report security problems **privately**, not in public issues, with
GitHub's private vulnerability reporting: "Report a vulnerability" on this
repository's Security tab. Only you and the maintainers see the report.
The project has no e-mail contact.

Include what is affected, how to reproduce it and what an attacker could do.
Do not test against other players' accounts or degrade the public server for
others; a local server (`roses-server hub --synthetic --db test.db
--local 127.0.0.1:<port>`) reproduces most server behaviour.

We aim to acknowledge reports within a week, agree a fix and a disclosure
date with you, and credit you in the release notes unless you prefer not to
be named.

## Supported versions

Only the latest release and the current `main` branch get security fixes.
The online server refuses clients whose protocol version does not match, so
players are asked to update when a fix changes the protocol.

## Scope

In scope:

- `crates/roses-server`: the online server (lobby, accounts, matches,
  rate limits, TLS), and `crates/roses-net`: the protocol and its decoding
  of untrusted input (a crash on malformed input is a bug worth reporting).
- The launcher's and the game's online code (account page, session file,
  certificate pinning).
- The importer's handling of malformed ISO images (memory safety, path
  traversal when writing the import).

Out of scope: problems that need an attacker already running code on the
player's PC, missing hardening headers on third-party sites, and the
original game itself.
