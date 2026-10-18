# PROGRESS — Rust Mining Co, Phase 1

Append-only handoff log. Each session adds one block (format in CLAUDE.md). The newest block is at the bottom; read the last 80 lines first.

## Standing notes

- Engine: Bevy 0.20.x pinned (fallback rule in docs/SAD.md §3).
- Core crates are Bevy-free; the client is Bevy.
- All Bevy tests go through rmc_testkit; E2E runs on lavapipe via scripts/e2e.sh.
- features.json is the source of truth for what is done. Only scripts/pass-feature.sh flips it.

## Decisions

(none yet)

## Blockers

(none yet)

---

## 2026-10-18 04:15 — F001 Workspace skeleton with pinned Bevy 0.20 and all crates compiling — passed
Done: Workspace Cargo.toml (resolver 3, edition 2024, rust-version 1.97.1) with every external crate pinned with `=`; seven crates under crates/ (rmc_world, rmc_worldgen, rmc_protocol, rmc_host, rmc_client, rmc_testkit, rmc_app) plus tests/e2e as package rmc_e2e (empty src/lib.rs). Each crate root has `#![deny(unsafe_code)]`. Dependency edges follow the SAD §4 graph. [profile.dev] opt-level=1, [profile.dev.package."*"] opt-level=3. Cargo.lock committed. `cargo build --workspace` ~6 min cold on this box.
Tests added: rmc_world coords::tests::smoke_compiles.
Decisions:
- Bevy 0.20 resolved on the first attempt: bevy =0.20.0, bevy_egui =0.43.1, bevy-inspector-egui =0.38.0 (both declare bevy_* ^0.20.0 on crates.io). The §3 fallback was not needed.
- glam pinned =0.33.12. bevy_math 0.20.0 requires glam ^0.33.2, so the SAD's "0.32" is out of date. Cargo.lock has one glam, 0.33.12. The core crates use glam directly.
- Other pins (crates.io latest on 2026-10-17): fastnoise-lite 1.1.1, rayon 1.12.0, serde 1.0.229, postcard 1.1.3, ron 0.12.2, zstd 0.14.3, crossbeam-channel 0.5.17, xxhash-rust 0.8.19 (xxh3), thiserror 2.0.21, anyhow 1.0.104, tracing 0.1.44, clap 4.6.7, image 0.25.10, image-compare 0.5.0, proptest 1.12.0, criterion 0.8.2.
- rust-toolchain.toml left as the kit's `channel = "stable"` (feature notes say "from the kit"). The local stable at M0 is rustc 1.99.1, which meets Bevy's MSRV of 1.97.1.
- For now each crate depends only on what its SAD role needs. rmc_client already pulls in bevy_egui and bevy-inspector-egui, so the full engine dependency set is proven to resolve.
- Running scripts/pass-feature.sh makes jq rewrite `§` escapes in features.json as literal `§`. The JSON is semantically the same; this comes from the script, and the file was not edited by hand.
Next: F002 (no-Bevy check script + CI workflow skeleton), then F003/F004/F005.
Blockers: none.

## 2026-10-18 04:40 — F002 No-Bevy dependency check script and CI workflow skeleton — passed
Done: Hardened scripts/check-no-bevy.sh. A missing `crates/<core>` directory or a failing `cargo tree` now fails the check; before, both counted as "Bevy-free" because stderr was discarded and the loop `continue`d. On failure it lists the offending bevy crates (deduplicated, first 10). Checked both ways: it passes on the current tree (rc 0). With `bevy.workspace = true` injected into rmc_world `[dependencies]` it fails for all four core crates (rc 1), since the others reach rmc_world transitively. The same line under `[dev-dependencies]` is ignored, which is correct per `-e normal`. The manifest and Cargo.lock were restored afterwards. .github/workflows/ci.yml parses as YAML (PyYAML); jobs linux and windows.
Tests added: none (feature test is the existing coords::tests::smoke_compiles).
Decisions:
- CI "End-to-end" steps (linux lavapipe, windows WARP) got `if: hashFiles('tests/e2e/tests/*.rs') != ''`. Without it, `scripts/e2e.sh` exits 101 ("no test target matches pattern `*`") until the first scenario lands at F056, and CI is red on every push. The guard lifts itself once a scenario exists. scripts/e2e.sh itself is unchanged.
- The no-Bevy check stays on `-e normal` as the SAD §4 says. Dev-dependencies and build-dependencies are not checked.
- On native Windows, jq in pass-feature.sh writes features.json with CRLF. Git normalizes it to LF on commit (warning only).
Next: F003 (coordinate conversions), then F004, F005.
Blockers: none.

## 2026-10-18 05:00 — F003 Coordinate conversions — passed
Done: rmc_world::coords has constants CHUNK_SHIFT/CHUNK_SIZE/CHUNK_MASK/CHUNK_VOLUME/CELL_SIZE_M and the types CellPos(IVec3), ChunkPos(IVec3), LocalPos(UVec3), all serde. CellPos::{chunk, local, split, from_chunk_local, min_corner_m, center_m, from_meters}, ChunkPos::origin_cell, LocalPos::{index, from_index}. Chunk = arithmetic `>> 5`, local = `& 31`, index = x + z*32 + y*1024. Meters: cell * 0.5, half-open, from_meters floors.
Tests added: coords::tests::{cell_to_chunk_negative, local_index_roundtrip, world_meters_roundtrip, ordering_is_y_z_x}. They cover i32::MIN/MAX, mixed-sign borders, all 32768 indices, and floor vs truncate for negative meters.
Decisions:
- CellPos and ChunkPos implement Ord manually (IVec3 has no Ord) as lexicographic (y, z, x), which matches the local linear index and the §8.1 shape order. Every "sorted" output (iter_chunks_sorted, save, damage BTreeMap) uses this order.
- The meter conversions are the float boundary that rule 3 allows (raycast/client). Integer logic does not call them.
- SAD §5 says 0.5 m cells, so CELL_SIZE_M = 0.5.
Next: F004 (MaterialTable from materials.ron), then F005.
Blockers: none.
