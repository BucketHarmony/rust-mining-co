# Phase 2 performance (SAD-P2 §13)

Criterion benches of the season tick, in `crates/rmc_eco/benches/`. Run them with:

```
cargo bench -p rmc_eco
```

The bench bodies are plain functions in `rmc_eco::bench_support`. `tests/benches_run_once.rs` runs each once on a tiny generated world (1 × 2 × 1 chunks) as part of `cargo test`. Its `benches_declared` test checks that every bench is declared, times its `bench_support` function and has a row below.

The world is the default Phase 2 world: 4 × 2 × 4 chunks (128 × 64 × 128 cells, 1 M cells), seed 1 (the game's default), generated from the shipped `assets/` with every Phase 2 worldgen step. `bench_support::install` prepares it for seasons the way the host does: displacement rules, every water and gas holder in its sweep's active set, and a fresh `Activity`.

## Bench results

### After F173 (performance pass)

Measured 2026-10-22 (F173), on the final F173 code, on the same machine and toolchain as below. An earlier run that night was starved by the OS and stopped: every process got about 4% of a core while the machine sat idle. This run came after the throttling ended.

| Bench | Time | §13 target | Status |
|---|---|---|---|
| `tick_first` | 3.03 s [3.01, 3.05] | < 5 s | met |
| `tick_quiet` | 751 ms [739, 784] (criterion slope); mean 859 ms [755, 985] | < 1 s | met once settled (see notes) |
| `water_sweep_world` | 35.7 ms [35.4, 36.2] | none | recorded |
| `chem_transport_world` | 552 ms [545, 563] | none | recorded |

What changed (F173):
- **Chunk map hashing.** The world's chunk map and the surface-column map used SipHash. Every cell read hashes a chunk position, so they now use a fixed Fx-style multiply-rotate hasher (`rmc_world::fasthash::PosMap`). Map iteration order never reaches output (rule 4: output iterates `iter_chunks_sorted`). This alone took the quiet tick from 6.5 s to about 3.5 s.
- **Settled water cells (§7.9 activity for the flow sweeps).** In the generated world about 135 k saturated limestone aquifer cells and 20 k full water cells sit in the water sweep's active set and are visited by all 64 flow sweeps a season, mostly moving nothing. A water or porous cell whose visit moved nothing is now marked settled (`Chunk::water_settled`). The sweep skips it until it or a face neighbor changes material or moisture, which `World` tracks on every write path (`apply`, `apply_delta`, `insert_chunk`, eco snapshots, `eco_set`, `eco_put_layer`). A visit reads only those 7 cells, so a skipped visit is exactly one that would have moved nothing. Results are bit-identical: `water::tests::settled_cells_skipped_without_changing_results` compares world hash, move logs and ledgers with and without skipping after every sweep, and `tick::tests::settling_changes_no_season_result` compares whole seasons with rain, boiling, magma and edits between them. Magma never settles (its visit depends on the iteration count). The settled sets are tied to a hash of the water pass's constants and are cleared when a pass with other constants runs.
- **Fewer chunk lookups** in the water visit (material and moisture in one lookup) and in chemistry transport (a donor with no substances is skipped).

Notes:
- `tick_quiet` ticks the same world for every iteration, and the per-iteration time falls as it settles further: samples went 1.22, 1.22, 0.91, 0.79, 0.75, 0.75, 0.74, 0.74, 0.73, 0.73 s per season. The third quiet season after `install` takes about 1.2 s; from about the fifth on, under 1 s. The criterion slope weights the later, longer samples.
- The generated aquifer never goes fully quiet. Limestone cells at moisture 21–24 (threshold 20, capacity 24) trade seepage sideways, about 7.8 k moves per flow sweep. Every chunk therefore changes every pass, and the Jacobi passes' quiet counters (§7.9) never skip a chunk. All 32 chunks are computed by every Jacobi pass of every sub-step.

### Before F173 (F172 baseline)

Measured 2026-10-22 (F172): release (`bench` profile), Windows 11, Intel Core i9-12900KF (16 cores, 24 threads), rustc 1.99.1, criterion 0.8.2. The time is criterion's estimate with its 95% interval.

| Bench | What one iteration does | Time | §13 target | Status |
|---|---|---|---|---|
| `tick_first` | `season_tick` of season 0 (shipped spring rain 128) on a fresh copy of the generated world, after `install` | 10.9 s [10.88, 10.94] | < 5 s | missed |
| `tick_quiet` | one more `season_tick` of the quiet tables (rain 0, every row's `air_temp` = row 0's 10.0 °C) on the same world, after 2 quiet settling seasons; the activity carries over | 6.48 s [6.44, 6.51] | < 1 s | missed |
| `water_sweep_world` | one flow sweep (§7.3 B, iteration 0) over a fresh copy of the installed world | 86.9 ms [85.8, 89.0] | none | recorded |
| `chem_transport_world` | §7.6 transport replaying one sub-step's flow-sweep moves after season 0 rain, on a copy of the world those sweeps left, every cell holding 64 of every substance | 861 ms [857, 865] | none | recorded |

Notes:
- Copying the world and dropping it are outside the timed region (`iter_batched`, the routine returns the world).
- `tick_first` and `tick_quiet` use 10 samples. One iteration of `tick_first` takes about 11 s, so a run takes a few minutes.
- `chem_transport_world` is a worst case on purpose: worldgen lays no substances, so the bench sets all of them everywhere (`bench_support::add_substances`). Then every water move carries every substance.
- Both tick targets are missed by a wide margin. The quiet tick, about 6.5 s, costs more than half of the first season's tick, so most of the time goes to work that does not shrink when nothing changes. Activity tuning and the performance pass are F173. It also records the dev-profile budget (`quiet_world_tick_under_dev_budget`, < `eco_test.quiet_tick_dev_ms`).

## Other §13 targets

Measured 2026-10-22 (F173) with a throwaway release probe, not a bench.

| Metric | Target | Where it stands |
|---|---|---|
| Season tick, steady rain, 50 edits in one chunk | < 3 s | **Missed**: about 3.6–4.4 s. Shipped tables with rain overridden to 128 in every row; after 2 rainy seasons, 50 single-cell digs in chunk (1, 1, 1), then one timed season. Flow is about 2 s of it, mostly air↔water conversions through `World::eco_set` (about 394 k material changes a season); transport about 0.6 s. |
| Snapshots + remesh after a season | < 1.5 s to no dirty chunks | Not measured (host and client side). |
| Layer memory after 20 seasons | < 48 MB | Met: 5.8 MB of layers (6 127 616 bytes; `World::memory_bytes` 7 170 752) after 20 shipped seasons on the default world. |
| Dev-profile budget, one quiet season | < 60000 ms | Met: 337 ms for `tick::tests::quiet_world_tick_under_dev_budget` (a 4 × 2 × 4-chunk layered world built inside `rmc_eco`, since unit tests cannot use worldgen). It took 5015 ms before F173. |
