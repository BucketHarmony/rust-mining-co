# Phase 2 performance (SAD-P2 §13)

Criterion benches of the season tick, in `crates/rmc_eco/benches/`. Run them with:

```
cargo bench -p rmc_eco
```

The bench bodies are plain functions in `rmc_eco::bench_support`. `tests/benches_run_once.rs` runs each once on a tiny generated world (1 × 2 × 1 chunks) as part of `cargo test`. Its `benches_declared` test checks that every bench is declared, times its `bench_support` function and has a row below.

The world is the default Phase 2 world: 4 × 2 × 4 chunks (128 × 64 × 128 cells, 1 M cells), seed 1 (the game's default), generated from the shipped `assets/` with every Phase 2 worldgen step. `bench_support::install` prepares it for seasons the way the host does: displacement rules, every water and gas holder in its sweep's active set, and a fresh `Activity`.

## Bench results

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

| Metric | Target | Where it stands |
|---|---|---|
| Season tick, steady rain, 50 edits in one chunk | < 3 s | Not measured yet (F173). |
| Snapshots + remesh after a season | < 1.5 s to no dirty chunks | Not measured yet (F173). |
| Layer memory after 20 seasons | < 48 MB | Not measured yet (F173/F174). |
| Dev-profile budget, one quiet season | < 60000 ms | `tick::tests::quiet_world_tick_under_dev_budget` (F173). |
