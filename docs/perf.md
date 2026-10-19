# Performance (SAD §13)

Criterion benches from SAD §14, one per crate whose code they measure. Run them all with:

```
cargo bench -p rmc_world -p rmc_worldgen -p rmc_host -p rmc_client
```

`cargo test --all-targets` and `cargo clippy --all-targets` build them too, and criterion runs each once in its test mode, so a broken bench fails the commit gate. `rmc_world`'s `bench_smoke::tests::benches_compile` checks that each one is declared and has a row here.

## Bench results

Measured 2026-10-19 (F081): release (`bench` profile), Windows 11, Intel Core i9-12900KF (16 cores, 24 threads), rustc 1.99.1, criterion 0.8.2. The time is criterion's estimate with its 95% interval.

| Bench | Crate | What one iteration does | Time | §13 target | Status |
|---|---|---|---|---|---|
| `mesh_chunk` | rmc_client | `GreedyMesher` on the default world's surface chunk at the center column (seed 45), with its border | 1.68 ms [1.62, 1.76] | < 2 ms | met |
| `gen_world` | rmc_worldgen | `Generator::new` + `generate_world` for the default 256-chunk world (seed 45), rayon parallel | 241 ms [241, 242] | < 3 s | met |
| `host_tick_100` | rmc_host | one `Host::tick` handling 100 single-cell edits from a joined client on the default world, alternating Set and Remove so every edit changes its cell | 61.0 µs [59.5, 63.2] | < 2 ms | met |
| `palette_set_get` | rmc_world | 4096 scattered writes of 20 materials into a uniform chunk (promoting through the 1-, 2-, 4- and 8-bit palettes), then 32768 reads | 542 µs [540, 544] | none | recorded |

Notes:
- `mesh_chunk` meets its target with about 15% margin on this CPU. A slower mid-range desktop may be close to 2 ms; F082 is where the mesher gets optimized, if needed.
- `host_tick_100` times only `Host::tick`. Sending the edits and draining the replies happen outside the timed region (`iter_custom`).
- `gen_world` uses 10 samples over 30 s (one iteration is about a quarter of a second).

## Other §13 targets

| Metric | Target | Where it stands |
|---|---|---|
| Frame rate, 256-chunk world, GPU | ≥ 60 fps | Not yet measured on a GPU; F082. The E2E runs use software rendering (WARP/lavapipe) and do not measure it. |
| Dig to visible change | < 50 ms | `latency_dig` asserts at most 2 fixed steps (33 ms at 60 Hz) from the breaking swing to an uploaded mesh. |
| Mesh memory for 256 chunks | < 256 MB | Counted as `MeshStats::mesh_bytes` (vertex and index bytes of the uploaded meshes); not yet shown in the stats panel or recorded. F082. |
