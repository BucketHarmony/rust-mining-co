# Rust Mining Co — build rules

Read first, every session: `PROGRESS.md` (last 80 lines), `features.json`, `git log --oneline -15`. The design is `docs/SAD.md` (Phase 1, F0xx) and `docs/SAD-P2.md` (Phase 2, F1xx); read the section the feature's `sad` field names, not the whole document.

## Commands

- Build: `cargo build --workspace`
- Fast check after edits: `cargo clippy -p <crate> --all-targets -- -D warnings`
- Unit/integration tests: `cargo test --workspace`
- End-to-end tests (headless render, lavapipe): `scripts/e2e.sh [name]` (sets `RMC_E2E=1 WGPU_ADAPTER_NAME=llvmpipe`)
- Soak: `scripts/soak.sh`
- Screenshots for review: `scripts/screens.sh` → `docs/screens/`
- Feature state: `scripts/verify-features.sh [--all | --quick | <id>]` (read-only), `scripts/pass-feature.sh <id>` (flips passes), `scripts/skip-feature.sh <id> "<reason>"` (optional features only)
- Smoke: `scripts/smoke.sh` (build + unit tests + one fast E2E)
- Benches: `cargo bench -p rmc_world -p rmc_worldgen -p rmc_host -p rmc_client -p rmc_eco`

## Hard rules

1. No Bevy dependency in `rmc_world`, `rmc_worldgen`, `rmc_protocol`, `rmc_host`, `rmc_eco`. `scripts/check-no-bevy.sh` enforces it.
2. Cells change only via `World::apply`, `World::apply_delta`, `World::insert_chunk`, and (Phase 2, host only, inside `season_tick` only) `World::eco_set`. The client calls only `apply_delta` and `insert_chunk`, only inside `ServerMsg` handlers.
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
13. Do not run `cargo run -p rmc_app` in a session; it needs a display. Use `scripts/e2e.sh` and screenshots to see the game. `cargo run -p rmc_app -- --headless --seasons N` is allowed once F108 exists.

## Phase 2 rules (ecology, `docs/SAD-P2.md`)

14. Layers change only inside `season_tick` passes, `EditOp::SetLayer`, the SAD-P2 §7.0 displacement rules (run by `World::apply` and `World::eco_set`), worldgen, and delta/snapshot application.
15. Two kinds of pass, per SAD-P2 §3. Jacobi passes (temperature, transitions, weathering, reactions, plant growth) compute from the previous state over a chunk plus a 1-cell border, in parallel, and apply in sorted chunk order; their cross-chunk fluxes are pure functions of the cell pair. Sweep passes (water, gases, chemistry transport, plant spread, light) run single-threaded in global sorted cell order and apply each move immediately after checking donor content and receiver room. Do not parallelize a sweep.
16. Every move of water, a substance, or a gas writes a `SeasonLedger` entry. The conservation property tests are the proof; if one fails, the pass is wrong, not the test.
17. Pass order is fixed by SAD-P2 §3. Do not reorder passes to make a test pass.
18. Tables drive behavior: transitions, reactions, gases, plants, seasons are data rows. Pass code matches on tags, phases, table fields and the `roles` section of `eco.ron`; it never names a material, gas or substance key outside tests and `eco.ron`.
19. Every numeric rate, threshold, shift and constant used by a pass lives in `assets/eco.ron` under the name SAD-P2 §7 gives it (`heat_shift`, `flow_cap`, `seep_num`, `stress_death`, ...). Pass code contains no numeric literals other than 0, 1, array sizes, type limits (`u8::MAX`, `i16::MIN`, ...) and the fixed divisors written inside SAD-P2 formulas (`/ 2`, `/ 256`), which go in named `const`s.
20. Cross-chunk fluxes are pure functions of the ordered pair's state, computed identically from both sides. The two-chunk-equals-one-chunk tests enforce this.

## Look and feel

21. Look-and-feel features (lighting, sky, fog, shading, glow, effects, HUD styling) ship with a semantic assertion and a screenshot. The playtester judges the look.
    - The test asserts what the code set up: components and resources present, light, fog, material and camera parameters, vertex attributes, `HudState` fields. A capture check is at most coarse: the frame is not blank, or two frames differ.
    - Do not write luminance or color thresholds, and do not iterate on pixel numbers. That means no tuning constants, re-deriving ceilings or re-shooting to make a pixel check pass.
    - When a feature's notes or tests ask for a pixel threshold, assert the semantic equivalent instead. Save the screenshot with `scripts/screens.sh <scenario>`, commit it, and record the substitution in `PROGRESS.md` under `## Decisions`.
    - Pixel tests that already exist stay (rule 6). If one blocks a feature, fix it under the `test-fix:` procedure; don't tune the look to satisfy it.

## External systems

22. Work you push and then wait on (GitHub Actions CI, anything remote) is capped per session.
    - At most two push-and-wait cycles and 60 minutes of waiting in total. Poll at most every 2 minutes.
    - Before pushing, reproduce what you can locally (the same commands CI runs) so each push carries a real fix.
    - When the cap is reached and it is still red:
      - keep the fixes that helped, and keep the local gate green;
      - record each failing job and its last error under `Blockers` in `PROGRESS.md`;
      - if the feature's listed tests pass, run `scripts/pass-feature.sh`, title the block's status `passed (CI red, see Blockers)`, and stop.
    - Later features must not wait on that CI. A follow-up fix is owner work.
    - Narrowing what CI runs (dropping a job or an E2E scenario, raising a timeout) is allowed only with a reason under `## Decisions`.

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
