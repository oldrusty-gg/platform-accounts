# Repositories

The project is split into separate repositories:

| Repository | Contents | Depends on today |
|---|---|---|
| `platform-sdk` | The shared platform: `platform-sdk` (game and mod manifests, the launcher-game interface, signed platform tokens) and `platform-net` (connections, TLS, the server file and saved session, the account protocol of sign-up, log-in, sessions and email). No game code. | nothing |
| `platform-accounts` | The platform account service's foundation (e-mail verification, password reset, account states) | `platform-sdk` |
| `launcher` | The Windows launcher (crate `roses-launcher`) | `platform-sdk` (`platform-sdk`, `platform-net`) |
| `rust-yugi-dotr` | The Roses game: `crates/roses-game` (the game), the importer (`roses-cli`, also `verify-import`, the import check the launcher runs), the online server (`roses-server`), the rules engine (`roses-core`), the duel and lobby protocol (`roses-net`), the file parsers and the imported library (`roses-disc`, `roses-assets`, `roses-ee`, `roses-movie`), `roses-client`, developer tools (`roses-compare`, `roses-viewer`), release scripts | `platform-sdk` (`platform-sdk`, `platform-net`), `platform-accounts` |
| website | The project website | nothing (not built from these sources) |

The launcher, the account service and every game depend only on the
platform repositories, never on each other:

```text
platform-sdk  <-  platform-accounts
     ^   ^              ^
     |   +--------------+-- rust-yugi-dotr   (game, importer, server)
     +--------------------- launcher         (launcher)
```

The launcher knows a game only through its manifest (`games/roses.json` is
built in): the importer and its arguments, the import check it runs as a
program and whose one-line report it reads
(`platform_sdk::contract::ImportReport`), the game executable, the
development options each takes, the game's art inside the player's import,
and the fields its server needs in an account-only hello. The Profile page
signs in over `platform-net`'s account protocol, which the Roses lobby
carries unchanged.

## Building across repositories

Dependencies between the repositories are Cargo path dependencies on
**sibling checkouts**. Clone them next to each other:

```text
<any folder>/
  platform-sdk/
  platform-accounts/
  launcher/
  rust-yugi-dotr/
```

```powershell
git clone https://github.com/oldrusty-gg/platform-sdk
git clone https://github.com/oldrusty-gg/platform-accounts
git clone https://github.com/oldrusty-gg/rust-yugi-dotr
git clone https://github.com/oldrusty-gg/launcher
cd rust-yugi-dotr; cargo build --workspace
```

Each repository builds with only the ones it depends on beside it
(`launcher` needs `platform-sdk`; `rust-yugi-dotr` needs `platform-sdk` and
`platform-accounts`). Keep the folder names: the path dependencies and CI
use them.

CI does the same: each workflow checks out the sibling repositories it needs
beside the one being tested (the repository variable `ROSES_SIBLING_REF`
picks their branch or tag; default `main`).

Once the repositories are public and tagged, the path dependencies can
become git dependencies by tag, for example

```toml
platform-sdk = { git = "https://github.com/oldrusty-gg/platform-sdk", tag = "v0.2.0" }
```

with a local override for development in `.cargo/config.toml` (not
committed):

```toml
[patch."https://github.com/oldrusty-gg/platform-sdk"]
platform-sdk = { path = "../platform-sdk/crates/platform-sdk" }
```

## Releases

A release package needs executables from two repositories: the launcher
(`launcher`) and the game and importer (`rust-yugi-dotr`).
`scripts/build-release.ps1` in `rust-yugi-dotr` packages them and expects
`launcher` checked out beside it. Tag both repositories with the same
version.

## Licence

Every repository is dual-licensed under the MIT licence or the Apache
License 2.0, at your option (`LICENSE-MIT`, `LICENSE-APACHE`; SPDX
`MIT OR Apache-2.0` in every `Cargo.toml`).
