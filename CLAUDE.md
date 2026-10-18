# Rust Mining Co — build rules

Read first, every session: `PROGRESS.md` (last 80 lines), `features.json`, `git log --oneline -15`. The design is `docs/SAD.md`; read the section for the feature you are working on, not the whole document.

## Commands

- Build: `cargo build --workspace`
- Fast check after edits: `cargo clippy -p <crate> --all-targets -- -D warnings`
- Unit/integration tests: `cargo test --workspace`
- End-to-end tests (headless render, lavapipe): `scripts/e2e.sh [name]` (sets `RMC_E2E=1 WGPU_ADAPTER_NAME=llvmpipe`)
- Soak: `scripts/soak.sh`
- Screenshots for review: `scripts/screens.sh` → `docs/screens/`
- Feature state: `scripts/verify-features.sh [--all | --quick | <id>]` (read-only), `scripts/pass-feature.sh <id>` (flips passes), `scripts/skip-feature.sh <id> "<reason>"` (optional features only)
- Smoke: `scripts/smoke.sh` (build + unit tests + one fast E2E)
- Benches: `cargo bench -p rmc_world -p rmc_worldgen -p rmc_host -p rmc_client`

## Hard rules

1. No Bevy dependency in `rmc_world`, `rmc_worldgen`, `rmc_protocol`, `rmc_host`. `scripts/check-no-bevy.sh` enforces it.
2. Cells change only via `World::apply`, `World::apply_delta`, `World::insert_chunk`. The client calls only the last two, only inside `ServerMsg` handlers.
3. Integers in `rmc_world` logic. Floats only at the raycast boundary and in worldgen noise sampling.
4. Any iteration that affects output (hash, save, snapshot, delta) is sorted or fixed-order.
5. Behavior comes from `assets/*.ron`. Filter on `tags` and `phase`; never branch on a specific material id or key in non-test code.
6. TDD. Write the failing test named in `features.json` first. Never delete, `#[ignore]`, or weaken an existing test. If a test is wrong, fix it in its own commit with message prefix `test-fix:` and a reason, and note it in `PROGRESS.md`.
7. `features.json` is modified only by `scripts/pass-feature.sh` and `scripts/skip-feature.sh`. Do not edit it by hand or with sed.
8. Every commit passes: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `scripts/check-no-bevy.sh`. A hook blocks commits that fail. Do not bypass it (`--no-verify` is denied).
9. One feature per session. Finish it, commit, append to `PROGRESS.md`, stop. Do not start a second feature.
10. Never ask questions. Nobody is watching. Take the conservative reading of the SAD, implement, and record the decision in `PROGRESS.md` under `## Decisions`.
11. Pin every crate version in the workspace `Cargo.toml`. Before adding a crate, confirm on crates.io (WebFetch `https://crates.io/api/v1/crates/<name>`) that a release targets the pinned Bevy minor.
12. Do not create a window in tests. End-to-end tests (in `tests/e2e/tests/`) and `rmc_testkit`'s own tests use `rmc_testkit::TestApp`. Unit tests inside `rmc_client` cannot depend on `rmc_testkit` (it depends on `rmc_client`); they build a small `App` with `MinimalPlugins` plus the plugin under test, or test plain functions.
13. Do not run `cargo run -p rmc_app` in a session; it needs a display. Use `scripts/e2e.sh` and screenshots to see the game.

## Conventions

- Rust 2024 edition, `#![deny(unsafe_code)]` in every crate except where wgpu readback needs it (document the reason).
- Error handling: `thiserror` in library crates, `anyhow` in `rmc_app` and scripts.
- Logging: `tracing`; `info!` for milestones, `debug!` for per-chunk work.
- Unit tests live in `#[cfg(test)] mod tests` inside the module they test, so their libtest path is `<module>::tests::<name>`; `features.json` lists that exact path. E2E scenarios are integration tests of the `rmc_e2e` package: `tests/e2e/tests/<name>.rs` with a `#[test] #[ignore]` fn named `<name>`.
- Proptests read `PROPTEST_CASES` from the environment with a default of 256; CI sets 1000. Keep tests under 10 s each in the dev profile.
- Commit messages: `feat(F0xx): <summary>`, `test(F0xx): ...`, `fix: ...`, `chore: ...`, `test-fix: ...`.

## PROGRESS.md contract

Append, never rewrite. Each session adds one block:

```
## <date time> — F0xx <title> — <passed|partial|blocked>
Done: ...
Tests added: ...
Decisions: ...
Next: ...
Blockers: ...
```

## When compacting

Preserve: the feature id in progress, the list of modified files, the test commands above, and any failing test names with their last error.
