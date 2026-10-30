# Contributing to Roses

Thank you for helping. Bug reports, testing on different hardware, fixes and
new features are all welcome.

## The one hard rule: no game data

Never commit, attach or link game data: ISO images, the game's executable or
files extracted from the disc, BIOS files, emulator save states, memory-card
saves, screenshots or captures of the original game, card texts, dialogue or
audio. This includes test fixtures. Pull requests or issues that contain any
of it are closed and the content deleted.

Tests that need data from a disc read it at run time from the developer's own
import and are marked `#[ignore = "Requires ... (ENV_VAR)"]`, naming the
environment variable that points at the input. CI runs only the tests that
need no game data.

## Local folders

Keep everything private in `work/` at the top of the repository (it is
git-ignored): your ISO, imports (`work/imports/`), builds, release packages
(`work/release/`), captures and logs. Some source comments cite research
notes as `work/research/...`; those notes are the maintainers' own and are
not part of the repository.

## Building and testing

The project is split into repositories checked out side by side
([docs/REPOSITORIES.md](docs/REPOSITORIES.md)); a change to crates another
repository uses should be tested with that repository too. Before
opening a pull request (the `crates/roses-game` step only in `roses`):

```powershell
$env:ROSES_MUTE = '1'          # no game sound from test runs
cargo build --workspace
cargo test --workspace
cd crates/roses-game
cargo test --features renderer
cd ../..
python scripts/publish/content_guard.py .
```

`ROSES_MUTE=1` keeps automated runs silent. `scripts/verify.ps1` also runs
the synthetic comparison checks of the developer tools.

Keep changes focused, explain in the pull request what you changed and how
you checked it, and add a test where the change can be tested without game
data. Behaviour that follows the original game should say where that
behaviour comes from (for example the function's address in the original
executable), so others can check it.

## AI-assisted contributions

You may use AI tools, but you are responsible for every line you submit:
read it, test it, and make sure it contains no game data and nothing you
are not allowed to contribute. Do not submit AI-generated images.

## Licence of contributions

The project is dual-licensed under the MIT licence or the Apache License
2.0, at your option (see LICENSE-MIT and LICENSE-APACHE). Unless you
explicitly state otherwise, any contribution you intentionally submit for
inclusion in the work, as defined in the Apache-2.0 licence, is dual-licensed
as above, without any additional terms or conditions.

## Conduct

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
Security problems go to the private channel in [SECURITY.md](SECURITY.md),
not public issues.
