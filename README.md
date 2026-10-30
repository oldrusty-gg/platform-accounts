# platform-accounts

The foundation of the platform account service of the Roses project: e-mail
addresses and their verification, password reset by e-mail, account states
and an e-mail sending abstraction. It depends only on
[platform-sdk](https://github.com/oldrusty-gg/platform-sdk), checked out beside
it (see [docs/REPOSITORIES.md](docs/REPOSITORIES.md)), and on no game code.

```powershell
cargo build --workspace
cargo test --workspace
```

Personal data handled here (e-mail addresses, account states) must be
covered by the service's privacy notice before it runs in public.

## Licence

Dual-licensed under either of

- the MIT licence ([LICENSE-MIT](LICENSE-MIT)), or
- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)),

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in the work by you, as defined in the
Apache-2.0 licence, shall be dual-licensed as above, without any additional
terms or conditions.
