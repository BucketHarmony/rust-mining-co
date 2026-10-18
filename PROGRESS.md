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

## 2026-10-18 05:20 — F004 MaterialTable loading from materials.ron with tags and hash — passed
Done: rmc_world::materials has MaterialId(u16) (serde transparent, AIR = 0), Phase {Air, Solid}, MaterialDef (the SAD §6 fields, `deny_unknown_fields`), and MaterialTable with load(path), from_ron_str, from_defs, hash, id(key), get(id), with_tag(tag) (id order), has_tag(id, tag), iter, len. Validation rejects: an empty table; more than 65536 entries; id != position; id 0 not phase Air; empty or duplicate keys; empty or duplicate tags within a material; emissive that is non-finite or negative. assets/materials.ron has the 13 Phase 1 materials. rmc_world now depends on ron (already pinned in the workspace, =0.12.2).
Tests added: materials::tests::{loads_phase1_table, hash_is_stable, lookup_by_key_and_tag}.
Decisions:
- materials.ron is a bare RON list of MaterialDef with explicit `id`s. Each id must equal its index, so lookup is a Vec index and ids cannot drift silently.
- The hash is xxh3_64 over a hand-written canonical encoding: a "rmc.materials.v1" prefix, LE integers, length-prefixed strings, tags in file order, emissive as f32 bits. It does not depend on RON whitespace or comments. hash_is_stable pins a golden value (0x839f6f476eb45041) for an inline 3-material fixture, not for the shipped asset, so tuning materials.ron does not break the test. Changing the encoding on purpose means bumping the v1 prefix and the golden value in a `test-fix:` commit.
- Tags use only the SAD vocabulary: soil = topsoil/dirt/clay; stone = stone/granite/basalt; ore = coal_seam/copper_ore/iron_ore; crystal = glowcrystal; placeable = brass_plate. Bedrock and air have no tags. Ores are NOT tagged stone, so glowcrystal and ore placement (which filter on stone) never overwrite ores. Worldgen will pick specific strata/ore materials through keys in worldgen.ron (data, not code).
- Hardness scale is designed for a pickaxe of roughly 10 damage per swing: topsoil 10, dirt 15, clay 20, stone 30, granite 45, basalt 60, coal 30, copper 40, iron 50, glowcrystal 20, brass 40. Bedrock is 65535 and diggable=false. Every diggable solid yields 1; air and bedrock yield 0. Only glowcrystal is emissive (2.0). Air color is (0,0,0,0); all solids are opaque.
- Phase::Liquid ("reserved" in the SAD) is not a variant yet. Adding it later changes no existing encoding byte, because Air=0 and Solid=1 stay as they are.
- The emissive validation compares f32 at load time. That is data validation, not world logic, so rule 3 does not apply.
Next: F005 (scripts wired), then M1 F010.
Blockers: none.

## 2026-10-18 05:40 — F005 Scripts: smoke.sh, e2e.sh, verify-features.sh, pass-feature.sh wired and executable — passed
Done: Ran and exercised the four scripts on native Windows (Git Bash); no script changes were needed for the features.json contract. All scripts are mode 100755 in git. smoke.sh: SMOKE_OK (build, tests, no-bevy; the boot_renders step is skipped until that scenario exists). `verify-features.sh F003` and `F004`: every test PASS via `--exact` with no substring fallback. A copy of the script against a scratch features.json under target/ (protected originals untouched) gave: missing test → FAIL rc 1; ambiguous filter `coords::tests` → FAIL; mod-path mismatch `tests::cell_to_chunk_negative` → PASS (substring match), as designed; a feature with one good and one missing test → rc 1. e2e specs, using temporary dummy scenarios in tests/e2e/tests (deleted afterwards, not committed): passing scenario → PASS (the other scenario binary reports 0 passed and does not interfere); panicking scenario → FAIL rc 1; unknown scenario → FAIL. pass-feature.sh refuses blocked features (F011 → "blocked by unpassed: F010").
Tests added: none (feature test is coords::tests::smoke_compiles).
Decisions:
- `--all-targets` in verify-features.sh is safe once criterion benches exist (F081): criterion 0.8.2's CLI accepts `--exact` and `--include-ignored`, and with a non-matching filter in test mode it runs nothing and exits 0.
- F005's stated criteria (cargo --exact, e2e entries, tested on F003) are met, so the feature was passed. The defect below is in a hook-protected file and is left for the owner rather than bypassing protect-files.sh with sed.
Next: M1 F010 (ChunkStorage Uniform/Paletted).
Blockers:
- OWNER ACTION (native Windows only; Linux/WSL2/CI unaffected): `scripts/verify-features.sh --quick` and `--all` report "no such feature: F001 / REGRESSION" for every id except the last. jq.exe writes CRLF to pipes, and `for id in $(jq -r ...)` keeps the `\r` (Git Bash's `read` strips it, so the single-id path used by pass-feature.sh works). Fix: add `-b` to the two `for id in $(jq -r ...)` jq calls and the `unpassed=` jq call (`jq -rb`), or pipe them through `tr -d '\r'`. Checked: `jq -rb` emits LF here. Until it is fixed, `--all` cannot print ALL_FEATURES_PASS on this box; per-id `verify-features.sh <id>` is reliable.

## 2026-10-18 06:00 — F010 ChunkStorage Uniform/Paletted with get/set and bit-width promotion — passed
Done: New rmc_world::chunk with Chunk { storage, revision, modified } (Chunk::new, Chunk::uniform; revision 0, not modified) and ChunkStorage::{Uniform(MaterialId), Paletted { palette, bits, data }}. get(LocalPos) and set(LocalPos, id) -> previous MaterialId. A Uniform chunk stays Uniform when written with its own material; the first differing write promotes it to palette [fill, id] at 1 bit. A new palette entry repacks data to the smallest width in PALETTE_BITS = [1, 2, 4, 8, 16] that holds it. Promotion happens at palette sizes 2/3/5/17/257 → 1/2/4/8/16 bits. data always has CHUNK_VOLUME*bits/64 words; entries are packed LSB-first in local linear index order and never span words.
Tests added: chunk::tests::{uniform_promotes_on_first_write, set_get_roundtrip_proptest, bits_promote_through_16}. The proptest checks every set/get against a 32768-cell model, plus invariants: allowed bits, palette fits and is not oversized, no duplicate entries, correct data length. Also passes with PROPTEST_CASES=1000 (about 1 s).
Decisions:
- set returns the previous material, so later edit code (F021) can find changed cells without a second get.
- Palette lookup on set is a linear scan. Phase 1 palettes are small (the table has 13 materials). Revisit in the F082 perf pass if host_tick_100 shows it.
- set never demotes and never drops unused palette entries; compact() does that (F011). Revision and modified are not touched by ChunkStorage; World::apply owns them (F021).
- ChunkStorage PartialEq is structural (Uniform != equivalent Paletted). Content equality is the canonical hash's job (F012).
- No serde derives yet. Save (F027) and protocol (F030) will choose their wire form and validate bits/data length on decode.
- Deleted the proptest-regressions/chunk.txt that the deliberate todo!() failure wrote (seed: fill=0, ops=[]). It recorded only the stub panic.
Next: F011 (compact), F012 (chunk hash), then F013 World.
Blockers: none new (the Windows CRLF issue in verify-features.sh --all from F005 still stands).

## 2026-10-18 06:20 — F011 compact() preserves content and demotes bit width — passed
Done: ChunkStorage::compact(&mut self) in rmc_world::chunk. Uniform is a no-op. Paletted is rebuilt in one pass over local linear index order: each used old slot gets a new slot on first appearance, unused entries are dropped, and data is repacked at bits_for(len) (smallest of 1/2/4/8/16). If only one material is left, the chunk becomes Uniform. Revision and modified are untouched, since content does not change.
Tests added: chunk::tests::{compact_preserves_contents_proptest, compact_demotes_bits}. The proptest sets cells, writes the fill back over a random prefix so some palette entries go unused, then checks: every cell against the model; the palette is exactly the used materials in first-appearance order (or Uniform when only one is used); compact is idempotent; set still works afterwards; well-formedness (shared assert_well_formed, which also rejects a width wider than needed). compact_demotes_bits starts at 16 bits (301 entries) and steps down to 8/4/2/1 by overwriting cells, then checks Uniform when one material is left, including a chunk whose original fill was fully overwritten. Also passes with PROPTEST_CASES=1000 (about 1.5 s).
Decisions:
- Compacted palette order is first appearance in local linear index order, not sorted by id and not the old palette order. The result depends only on content, so equal contents give a structurally equal ChunkStorage after compact. F012 can use that as the canonical encoding if it wants.
- A single remaining material demotes to Uniform. The SAD says "demoting bit width"; Uniform is the narrowest form, and it matches how worldgen emits single-material chunks.
- Deleted the proptest-regressions/chunk.txt seed that the deliberate todo!() failure wrote (fill=0, ops=[], undo=0). It recorded only the stub panic.
Next: F012 (chunk hash canonical across Uniform/Paletted), then F013 World.
Blockers: none new (the Windows CRLF issue in verify-features.sh --all from F005 still stands).

## 2026-10-18 06:40 — F012 Chunk hash canonical across Uniform and equivalent Paletted — passed
Done: ChunkStorage::hash() and Chunk::hash() (which delegates) in rmc_world::chunk. The hash is xxh3_64 over the compacted form, encoded as: "rmc.chunk.v1" prefix, palette length (u32 LE), palette ids (u16 LE), bit width (u8), data words (u64 LE). Uniform encodes as a one-entry palette with 0 bits and no data words, and a Paletted chunk that compacts to one material encodes the same way. Pulled the compaction logic out of compact() into a private `compacted(palette, bits, data) -> ChunkStorage` used by both compact() and hash(). It now does two passes (remap, then write) and no longer keeps a 32768-entry slot Vec. The F010/F011 tests are unchanged and still pass.
Tests added: chunk::tests::{hash_uniform_equals_paletted, hash_changes_on_edit}. Equal hashes for: Uniform vs promoted-then-reverted (1 bit); Uniform vs a chunk grown to 16 bits and then fully overwritten; a fully overwritten paletted chunk vs Uniform of the new material; the same mixed contents with different palette order, unused entries and widths; compacted vs uncompacted. Different hashes for: a different uniform material; one edited cell at index 0, 1 and 32767 (pairwise distinct); a different written material; two cells swapped under the same palette; an edit in a 16-bit chunk. Reverting an edit restores the hash.
Decisions:
- The hash covers cell contents only. Chunk.revision and Chunk.modified are not hashed: SAD §7 defines the hash over the canonical paletted encoding, and World::hash (F013) folds in positions, not revisions. Revision agreement between host and replica is apply_delta's job (F025).
- The canonical encoding reuses compact()'s first-appearance palette order, so it depends only on content. No golden value is pinned: chunk hashes are compared only between worlds in the same build (determinism, replica equivalence) and are never persisted. Changing the encoding on purpose means bumping the v1 prefix.
- hash() on a Paletted chunk allocates a compacted copy (at most 64 KiB data). That is fine for world hashing at test and handshake rates. Revisit in F082 if profiling shows it.
Next: F013 (World with bounds, chunk map, get_cell/is_solid, iter_chunks_sorted, world hash).
Blockers: none new (the Windows CRLF issue in verify-features.sh --all from F005 still stands).

## 2026-10-18 07:00 — F013 World with bounds, chunk map, get_cell/is_solid, iter_chunks_sorted, world hash — passed
Done: New rmc_world::world with World { chunks: HashMap<ChunkPos, Chunk>, materials: Arc<MaterialTable>, bounds: CellAabb } and WorldError::ChunkOutOfBounds. API: new(materials, bounds) (empty), bounds(), materials(), get_cell, is_solid, chunk(pos), chunk_count, iter_chunks_sorted, hash, insert_chunk. New coords::CellAabb { min, max } (inclusive like Shape::Box, serde for Welcome/save header) with PHASE1_DEFAULT = (0,0,0)..=(255,127,255) (8×4×8 = 256 chunks, also its Default), new (asserts min <= max), contains, intersects_chunk, and chunks() (every overlapping chunk in (y, z, x) order).
Tests added: world::tests::{get_cell_out_of_bounds_is_none, iter_sorted_is_deterministic (proptest: random insertion order and subset over a 3×2×3-chunk world with negative coords), world_hash_independent_of_insert_order, iter_sorted_orders_y_then_z_then_x}. Proptest also passes with PROPTEST_CASES=1000 (0.1 s).
Decisions:
- get_cell returns None when the cell is outside the bounds OR its chunk is not resident (for example a replica before the snapshot arrives). It does not fall back to AIR. is_solid is false for None and for ids the table does not know. A chunk that straddles the bounds is resident, but its out-of-bounds cells read as None.
- Default bounds start at the origin with y up: cells 0..=255 × 0..=127 × 0..=255. The SAD gives only the size, so the origin is the conservative choice.
- insert_chunk returns Result<Option<Chunk>, WorldError>: the previous chunk, or ChunkOutOfBounds when no cell of the chunk is inside the bounds (the chunk is then not stored). The SAD lists no return type; rejecting is safer than storing unreachable chunks. It does not touch revision, modified or compaction; the caller's chunk is stored as is.
- World::hash = xxh3_64 over "rmc.world.v1" + for each chunk in iter_chunks_sorted: pos x,y,z (i32 LE) + chunk.hash() (u64 LE). Bounds, material table, revisions and `modified` are not hashed (the SAD says it folds (pos, chunk.hash())). Material agreement is checked separately through the Hello hash. No golden value is pinned; the hash is never persisted.
- iter_chunks_sorted collects and sorts the keys on each call (O(n log n), 256 chunks in Phase 1). Revisit in F082 if it matters.
- The damage map (BTreeMap<CellPos, u16>) is left for F023, where it is first used; adding it now would be dead code under -D warnings.
- Deleted the proptest-regressions/world.txt seed that the deliberate todo!() stub failure wrote; it recorded only the stub panic.
Next: F014 (256 uniform-stone chunks under 64 MB), then M2 F020 shapes.
Blockers: none new (the Windows CRLF issue in verify-features.sh --all from F005 still stands).

## 2026-10-18 07:20 — F014 256 uniform-stone chunks occupy under 64 MB — passed
Done: ChunkStorage::heap_bytes() (palette + data Vec capacity; Uniform = 0) and World::memory_bytes() (size_of World + estimated chunk-map table + sum of chunk heap bytes). Measured: 256 uniform-stone chunks = 45,664 bytes; 256 chunks all at a 16-bit palette (worst case) = 16,954,464 bytes. Both are far under the budget.
Tests added: world::tests::memory_256_chunks_under_budget. It checks the uniform world (< 64,000,000 bytes, < 1 MiB, no chunk heap) and the 16-bit worst case (< 64,000,000 bytes, ≥ 256 × 64 KiB of data, so the measurement is not trivially small). It also checks that an empty world reports exactly size_of::<World>().
Decisions:
- Memory is accounted, not measured with a counting global allocator. That would need `unsafe impl GlobalAlloc` (rule: deny(unsafe_code)) or a new crate. The table estimate follows std/hashbrown's SwissTable layout: buckets from capacity() (cap+1 below 7, else cap/7*8), one (ChunkPos, Chunk) entry plus one control byte per bucket, plus a 16-byte group. Vecs count capacity, not len. The shared Arc<MaterialTable> is not counted (it is not chunk storage).
- "64 MB" is read as 64,000,000 bytes, the stricter of MB and MiB.
- The SAD has no 64 MB row in §13; the budget comes from the feature title. The test also covers the 16-bit worst case, so edits cannot push a full 256-chunk world over the budget either.
Next: M2 F020 (shapes with fixed iteration order and cell counts).
Blockers: none new (the Windows CRLF issue in verify-features.sh --all from F005 still stands).

## 2026-10-18 07:40 — F020 Shapes with fixed iteration order and cell counts — passed
Done: New rmc_world::shape with Shape::{Cell(CellPos), Box { min, max } (inclusive), Sphere { center, radius: u16 } (dx²+dy²+dz² <= r²)}. It derives serde for the protocol. API: cells() -> ShapeCells, cell_count() -> u64, aabb() -> Option<CellAabb>. ShapeCells is a FusedIterator over x-runs per (y, z) row, so cells come out in ascending (y, z, x) order (= CellPos Ord). It never scans empty rows. Sphere row spans use integer square roots (u64::isqrt); the module has no floats.
Tests added: shape::tests::{sphere_counts_r0_to_8, sphere_is_symmetric, box_is_inclusive, iteration_order_is_y_z_x}. Sphere counts match OEIS A000605 (1, 7, 33, 123, 257, 515, 925, 1419, 2109) and a brute-force scan of the bounding cube at three centers (one negative, one on a chunk border). Symmetry: three axis mirrors, five axis permutations, axis tips, and translation invariance. Box: single cell, both corners included, min > max on each axis is empty, i32-edge boxes, and a full-i32 box whose count saturates. Order: an explicit 2×2×2 box and r=1 sphere sequence, and strictly ascending for larger shapes. Also a sphere at the i32 edge.
Decisions:
- An empty Box (min > max on any axis) is allowed and yields nothing: count 0, aabb None. Shape is wire data, so it does not panic like CellAabb::new. F022 can reject it as "entirely outside".
- Coordinates are i64 inside the iterator. Cells that would leave the i32 range are skipped (they are outside every world). cell_count() and aabb() describe the ideal shape (aabb clamped to i32), so cells().count() <= cell_count(). Validation therefore errs toward rejecting.
- cell_count() is computed, not iterated: boxes are a saturating product, spheres are a sum of isqrt row spans (O(r²)). For a radius of up to 65535 that is too slow on a hostile command. F022 should reject big radii before counting (r >= 20 already exceeds MAX_EDIT_CELLS: 33,401 > 32,768) or check the aabb volume first.
- Cell is iterated as a 1×1×1 box.
Next: F021 (EditCommand Set/Remove/Paint via World::apply with deltas).
Blockers: none new (the Windows CRLF issue in verify-features.sh --all from F005 still stands).
