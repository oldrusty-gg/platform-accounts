## What this changes

## How it was checked

<!-- Tests added or run, and anything checked by playing. -->

## Checklist

- [ ] No game data: no files from the disc, ISO images, saves, BIOS files, captures of the original game, card texts or dialogue, and no links to them.
- [ ] Tests that need game data are `#[ignore = "Requires ... (ENV_VAR)"]`.
- [ ] `cargo test --workspace` and, for game changes, `cargo test --features renderer` in `crates/roses-game` pass (with `ROSES_MUTE=1`).
- [ ] `python scripts/publish/content_guard.py .` passes.
