# Rust Mining Co

A co-op fantasy voxel mining game, with a little steampunk, in Rust and [Bevy](https://bevy.org) 0.20. It is also a platform for trying gameplay loops on one shared world simulation.

![Generated terrain](docs/screens/horizon_readable-north-81.png)

**Phase 1 — the voxel world** (complete). Deterministic cave worldgen at 0.5 m cells, a first-person controller, pickaxe digging with per-cell damage and yields, placing from the inventory, a debug editor (egui), and save/load. Every world change goes through an authoritative host and a client replica, the same message path co-op will use.

**Phase 2 — ecology** (in progress). A deterministic, conservation-checked season tick on the host moves heat, water, substances and gases through the world: swales fill, ice freezes and thaws, rock weathers to soil, acid mine drainage kills moss downslope, limestone clears it, and magma flashes water to steam.

Progress is in [docs/REPORT.md](docs/REPORT.md); the session-by-session log is [PROGRESS.md](PROGRESS.md).

## Play

Requires stable Rust (1.97.1 or newer; `rust-toolchain.toml` selects it).

```sh
git clone https://github.com/BucketHarmony/rust-mining-co.git
cd rust-mining-co
cargo run --release -p rmc_app -- --seed 42
```

Prebuilt Windows builds are attached to CI runs on `main` as the `windows-release` artifact. Unzip it and run `rust-mining-co.exe` from that folder.

On Linux, Bevy needs `pkg-config libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev`.

Options:

| Flag | Meaning |
|---|---|
| `--seed N` | World seed |
| `--save DIR` | Save directory; loaded on start if it holds a save |
| `--world-size WxHxD` | World size in cells, each a multiple of 32 |
| `--headless --seasons N` | Advance N seasons with no window, then write the save and a report |

### Controls

| Input | Action |
|---|---|
| Mouse | Look |
| `W` `A` `S` `D`, `Shift`, `Space` | Move, sprint, jump |
| Left mouse | Swing the pickaxe |
| Right mouse | Place the selected material |
| Wheel, `Q` / `E` | Select material |
| `V` | Fly camera |
| `F1` | Debug editor (brush, gizmos, save/load, advance a season with `F6`) |
| `Escape` | Release the cursor |

Full list: [docs/CONTROLS.md](docs/CONTROLS.md).

## Layout

| Path | Contents |
|---|---|
| `crates/rmc_world` | Cell storage, materials, edits, raycast, save format |
| `crates/rmc_worldgen` | Deterministic terrain, strata, caves and ore veins |
| `crates/rmc_eco` | The season tick: temperature, water, weathering, reactions, gases, light, plants |
| `crates/rmc_protocol` | Client/server messages |
| `crates/rmc_host` | Authoritative world, command queue, transport |
| `crates/rmc_client` | Bevy client: replica, meshing, rendering, controller, HUD, editor |
| `crates/rmc_testkit` | Headless app, input injection, screenshots, bots |
| `crates/rmc_app` | The game binary |
| `tests/e2e` | End-to-end scenarios that drive the real game headlessly |
| `assets/*.ron` | Materials, tools, worldgen, seasons and ecology tables; behavior is data |

The world crates (`rmc_world`, `rmc_worldgen`, `rmc_eco`, `rmc_protocol`, `rmc_host`) do not depend on Bevy.

## Development

```sh
cargo test --workspace --exclude rmc_e2e    # unit, property and integration tests
cargo clippy --workspace --all-targets -- -D warnings
scripts/e2e.sh <scenario>                    # one end-to-end scenario, headless on a software rasterizer
scripts/screens.sh <scenario>                # screenshots into docs/screens/
```

Design documents: [docs/SAD.md](docs/SAD.md) (Phase 1) and [docs/SAD-P2.md](docs/SAD-P2.md) (Phase 2).

### How it is built

The code is written by Claude Code running unattended, one feature per session (`scripts/run-overnight.sh`, `PROMPT.md`). [CLAUDE.md](CLAUDE.md) holds the build rules. Each feature in `features.json` names the tests that prove it, and only `scripts/pass-feature.sh` marks it passing after running them. A commit hook blocks any commit that fails fmt, clippy or the tests. End-to-end scenarios save screenshots, and a playtester agent reviews them ([docs/PLAYTEST.md](docs/PLAYTEST.md)).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
