# Rust Mining Co — Solution Architecture Document

**Version:** 0.2 (Phase 1: World Voxel Mechanisms, autonomous build)
**Date:** 2026-10-17
**Engine:** Bevy 0.20.x (pinned; fallback 0.19.1, see §3), Rust stable ≥ 1.97
**Audience:** Claude Code (implementer, running unattended), Bucket (owner)
**Companion files:** `CLAUDE.md`, `PROMPT.md`, `features.json`, `PROGRESS.md`, `.claude/settings.json`, `.claude/hooks/*`, `.claude/agents/*`, `scripts/*`, `.github/workflows/ci.yml`

---

## 1. Purpose

Rust Mining Co is a co-op mining game in the vein of Deep Rock Galactic, reworked as fantasy with mild steampunk: pickaxes, steam-powered tools, and static encounters with goblins, spiders, ogres and owlbears. The long-term plan extends into ecology, gardening, base building and defense. The project is a platform for trying different gameplay loops on one shared world simulation.

This document covers **Phase 1: the voxel world and the mechanisms that read and change it**, built by Claude Code in an overnight, unattended run. Phase 1 ends with a single player walking through a generated cave world, digging with a pickaxe, placing and removing cells with a debug brush, and saving and loading. Every world change travels through the same host/replica message path that co-op will use later.

The build is test-driven. Every feature in `features.json` names the tests that prove it, and a feature is marked passing only by a script that runs those tests. End-to-end tests drive the real game through its input and render pipeline, headless, and produce screenshots that a reviewer agent inspects.

## 2. Scope

### In scope (Phase 1)

- Voxel world storage at 0.5 m cells, one material per cell
- Material definitions loaded from data
- Edit command protocol with an authoritative host and a client replica
- In-process transport (single-player), behind a transport trait
- Deterministic procedural worldgen: surface, strata, caves, ore veins, bedrock shell
- Chunk meshing on background tasks, behind a mesher trait
- Rendering with basic lighting. Darkness is not a mechanic.
- Kinematic first-person character controller colliding against the grid
- Voxel raycast, pickaxe digging with per-cell damage, material yields to a simple inventory counter
- In-game debug editor (egui): brush place/remove/paint, fly camera, stats, gizmos
- Save/load of modified chunks
- Headless unit, property, integration and end-to-end tests; benchmarks
- A `rmc_testkit` crate that drives the full game headlessly (input injection, screenshots, scripted bots)
- CI on GitHub Actions (Linux tests with lavapipe, Windows build + E2E with WARP, Windows release artifact)

### Out of scope (Phase 1)

- Creatures, AI, combat
- Gameplay loops, objectives, progression
- Steam tools other than the pickaxe
- Network transport, lobbies, client-side prediction
- Smooth meshing, texture arrays, final art
- Blender/Skein asset pipeline, glTF prefab voxelizer
- Per-cell ecology layers
- Physics objects (Avian)

§18 lists where each attaches later.

## 3. Fixed decisions

| Decision | Value | Reason |
|---|---|---|
| Engine | Bevy **0.20.x**, pinned at the latest patch at M0. Upgrade only between phases | 0.20 released 2026-10-08. `bevy_egui` 0.43 and `bevy-inspector-egui` 0.38 already target 0.20 and have stopped on 0.19. Starting on 0.19 means starting on a dead minor. **Fallback rule:** if at M0 the dependency set in §4 does not resolve on 0.20 within two attempts, pin `bevy = "=0.19.1"`, `bevy_egui = "0.40"`, `bevy-inspector-egui = "0.37"`, record the reason in `PROGRESS.md`, and continue. No other fallback |
| Rust | stable, `rust-toolchain.toml` pinned to the current stable at M0 | Bevy 0.20 MSRV is 1.97.1 |
| Cell size | 0.5 m | Fine enough for digging and building, coarse enough for gardening grids |
| Cell content | One `MaterialId` (u16) per cell. `0` = Air | Simple, compact, palette-compressible |
| Chunk size | 32 × 32 × 32 cells (16 m cube) | Standard size for meshing and compression |
| Authority | Host owns the world. Clients hold read-only replicas | Co-op ready from day one |
| Single-player shape | Listen server: host on its own thread, client in the Bevy app, connected by channels. Tests drive the host synchronously instead | Enforces the message boundary; tests stay deterministic |
| World mutation | Cells change only through `World::apply(EditCommand)`, `World::apply_delta(ChunkDelta)` and `World::insert_chunk` | One path for pickaxe, editor, worldgen fixes and networking. The replica uses only the last two, inside `ServerMsg` handlers |
| World logic numerics | Integers only in `rmc_world` (raycast is the one float entry point) | Determinism across machines |
| Player movement authority | Client-authoritative (trusted co-op) | Simple and responsive. Only world edits are host-authoritative |
| Coordinate system | Y-up, right-handed (Bevy convention) | |
| Lighting | Generous ambient plus sun plus player lantern. Some materials emissive | Caves stay readable |
| Render style (Phase 1) | Blocky greedy mesher, AO in the merge key | Trait allows a smooth mesher later |
| Development method | TDD: failing test first, then implementation, then refactor. Commits are gated on green tests | Unattended run needs mechanical verification |
| Test rendering | Headless Bevy app, no window, camera renders to an image, software rasterizer (lavapipe on Linux, WARP on Windows) | Runs in WSL2 and CI without a GPU; deterministic enough for semantic pixel checks |

## 4. Workspace layout and dependencies

```
rust-mining-co/
├── Cargo.toml                 # workspace; all versions pinned here
├── rust-toolchain.toml
├── CLAUDE.md                  # build rules for Claude Code (§15)
├── PROMPT.md                  # per-iteration prompt for the overnight loop (§16)
├── features.json              # feature list with pass state; edited only by scripts/
├── PROGRESS.md                # append-only handoff log
├── assets/
│   ├── materials.ron
│   ├── tools.ron
│   └── worldgen.ron
├── crates/
│   ├── rmc_world/             # NO Bevy. Storage, materials, shapes, edits, queries, raycast, save format
│   ├── rmc_worldgen/          # NO Bevy. Deterministic generation
│   ├── rmc_protocol/          # NO Bevy. Client/server messages, serialization
│   ├── rmc_host/              # NO Bevy. Authoritative world, command queue, tick, transport trait
│   ├── rmc_client/            # Bevy. Replica, meshing, rendering, input, controller, HUD, debug UI
│   ├── rmc_testkit/           # Bevy. Headless app builder, input injection, screenshots, bots
│   └── rmc_app/               # Bevy binary. Wires host thread + client; CLI args
├── tests/e2e/                 # package `rmc_e2e` (empty src/lib.rs); scenarios are integration tests in tests/e2e/tests/<name>.rs
├── scripts/                   # smoke, e2e, screens, soak, verify-features, pass-feature, skip-feature, check-no-bevy, report, run-overnight
├── docs/                      # SAD.md, perf.md, REPORT.md, screens/
├── .claude/                   # settings.json, hooks/, agents/
└── .github/workflows/ci.yml
```

**Dependency rule:** `rmc_world`, `rmc_worldgen`, `rmc_protocol` and `rmc_host` must not depend on Bevy. They use `glam` directly, at the version Bevy re-exports (0.32 for Bevy 0.19/0.20; confirm with `cargo tree` at M0). CI runs `scripts/check-no-bevy.sh` (`cargo tree -p <crate> -e normal | grep -q '^bevy'` must fail for each).

```
rmc_app ──> rmc_client ──> rmc_protocol ──> rmc_world
   │              ^                              ^
   │              │                              │
   └──> rmc_host ─┼──> rmc_protocol              │
          └───────┼──> rmc_worldgen ─────────────┘
rmc_testkit ──────┘ (also depends on rmc_host, rmc_worldgen)
rmc_e2e (tests/e2e) ──> rmc_testkit
```

**Pinned crates (verify each on crates.io at M0; record exact versions in the workspace `Cargo.toml`):**

| Crate | Expected | Use |
|---|---|---|
| bevy | 0.20.x | engine |
| bevy_egui | 0.43.x (0.20) / 0.40.x (0.19) | debug editor |
| bevy-inspector-egui | 0.38.x (0.20) / 0.37.x (0.19) | debug editor |
| glam | matches Bevy's | core crates |
| fastnoise-lite | 1.1 | worldgen noise. Chosen over `noise` because it is pure arithmetic (no libm transcendentals), so output is stable across machines |
| rayon | 1.x | parallel worldgen |
| serde, postcard | 1.x | protocol + save |
| ron | 0.8+ | data tables |
| zstd | 0.14 | save compression |
| crossbeam-channel | 0.5 | `ChannelTransport`, screenshot readback |
| xxhash-rust (xxh3) | 0.8 | world/material/params hashing |
| proptest | 1.x | property tests |
| criterion | 0.8 | benchmarks |
| image, image-compare | 0.25 / 0.5 | screenshot assertions |
| clap | 4.x | CLI |

Do not add `bevy_flycam`, `bevy_transform_interpolation`, `bevy_voxel_world` or `block-mesh`. The fly camera, interpolation and mesher are small and written in-repo so they survive engine upgrades. `binary-greedy-meshing` (no Bevy dependency) may be evaluated at M8 if the in-repo mesher misses its budget.

## 5. Coordinates

| Space | Type | Definition |
|---|---|---|
| Cell | `CellPos(IVec3)` | Integer cell index in the world |
| Chunk | `ChunkPos(IVec3)` | `cell >> 5` per axis (arithmetic shift, correct for negatives) |
| Local | `LocalPos(UVec3)` / `u16` index | `cell & 31` per axis. Linear index `x + z*32 + y*1024` |
| World | `Vec3` (meters) | `cell * 0.5`. A cell occupies `[c*0.5, (c+1)*0.5)` |

All conversions live in `rmc_world::coords`, with unit tests covering negative coordinates and chunk borders.

**Phase 1 world bounds:** configurable, default 256 × 128 × 256 cells (128 m × 64 m × 128 m) = 8 × 4 × 8 = 256 chunks. Storage is `HashMap<ChunkPos, Chunk>`. Edits outside bounds are rejected.

## 6. Materials

Loaded from `assets/materials.ron` at startup. Host and client load the same table; `Hello` carries its hash and the host rejects a mismatch. The save header stores the same hash.

```rust
pub struct MaterialId(pub u16);   // 0 = AIR, reserved

pub struct MaterialDef {
    pub id: MaterialId,
    pub key: String,          // "granite", stable across versions
    pub phase: Phase,         // Air | Solid  (Liquid reserved)
    pub tags: Vec<String>,    // "stone", "soil", "ore", "crystal", "placeable" — worldgen and tools filter on tags
    pub hardness: u16,        // damage needed to break; 0 = instant
    pub diggable: bool,       // false for bedrock
    pub yield_count: u8,      // items of this material produced on break (0 = none)
    pub color: [u8; 4],       // vertex color
    pub emissive: f32,        // 0.0.. ; glowing crystals
}
```

Yields are `MaterialId`s. There is no separate item type in Phase 1; `Inventory` counts materials.

**Phase 1 table:** air, topsoil, dirt, clay, stone, granite, basalt, coal_seam, copper_ore, iron_ore, glowcrystal (emissive), bedrock (not diggable), brass_plate (placeable, for testing built structures).

Rendering, collision and raycast read `phase` and treat `Solid` as blocking. Worldgen and tools filter on `tags`. Nothing in code branches on specific material IDs or keys; tests may reference keys through `MaterialTable::id("granite")`.

## 7. Chunk storage

```rust
pub struct Chunk {
    pub storage: ChunkStorage,
    pub revision: u64,            // increments on every applied change
    pub modified: bool,           // differs from worldgen output -> saved
}

pub enum ChunkStorage {
    Uniform(MaterialId),
    Paletted { palette: Vec<MaterialId>, bits: u8 /*1,2,4,8,16*/, data: Vec<u64> /*32768 entries*/ },
}
```

- `get(local)` / `set(local, id)` handle palette growth and bit-width promotion. A Uniform chunk promotes on its first differing write.
- `compact()` rebuilds the palette, dropping unused entries and demoting bit width. Runs after `Full` deltas and before save.
- `hash() -> u64` (xxh3 over the canonical paletted encoding, so Uniform and an equivalent Paletted hash the same).

**Cell damage** lives on the host `World` only: `BTreeMap<CellPos, u16>`. A cell breaks when damage ≥ hardness. Damage uses saturating add. `Set`, `Paint` and `Remove` on a cell clear its damage. Damage is not saved and resets on load. The replica never receives damage in Phase 1.

```rust
pub struct World {
    chunks: HashMap<ChunkPos, Chunk>,
    damage: BTreeMap<CellPos, u16>,
    materials: Arc<MaterialTable>,
    bounds: CellAabb,
}
// Queries: get_cell, is_solid, chunk(pos), iter_chunks_sorted(), hash()
// Mutation: apply(&EditCommand) -> EditResult
//           apply_delta(&ChunkDelta) -> Result<(), DeltaError>   (revision must equal local+1 for Cells; Full replaces)
//           insert_chunk(ChunkPos, Chunk)
```

`World::hash()` folds `(pos, chunk.hash())` in sorted order. Any output that depends on iteration order (hashing, save, snapshots) uses `iter_chunks_sorted()`.

## 8. Edit command protocol

### 8.1 Commands

```rust
pub struct EditCommand { pub id: CommandId /*(issuer, seq u32)*/, pub issuer: PlayerId, pub op: EditOp, pub shape: Shape }

pub enum EditOp {
    Set(MaterialId),   // place / fill
    Remove,            // set Air, no yield (editor)
    Damage(u16),       // pickaxe: accumulate; break -> Air + yield
    Paint(MaterialId), // replace only Solid cells
}

pub enum Shape {
    Cell(CellPos),
    Box { min: CellPos, max: CellPos },        // inclusive
    Sphere { center: CellPos, radius: u16 },   // integer: dx²+dy²+dz² <= r²
}
```

Shape iteration order is fixed (y, then z, then x, ascending).

### 8.2 Validation (host)

Reject the whole command, applying none of it, if the shape exceeds `MAX_EDIT_CELLS` (32,768) or lies entirely outside bounds. Inside a valid shape, cells are skipped individually when out of bounds, or when `diggable == false` for `Damage`/`Remove`. `Set` on a non-air cell is allowed (overwrite). `Set` also skips, cell by cell, every cell that a joined player's box (0.6 × 1.7 m, §11.4) overlaps at that player's last `PlayerState`; a player that has sent none is not protected (owner extension, F086). Reach validation is a hook returning `Ok` in Phase 1.

### 8.3 Results

```rust
pub struct EditResult { pub command: CommandId, pub status: EditStatus /*Applied | Rejected(reason)*/, pub deltas: Vec<ChunkDelta>, pub yields: Vec<(MaterialId, u32)> }

pub enum ChunkDelta {
    Cells { pos: ChunkPos, revision: u64, changes: Vec<(u16, MaterialId)> },
    Full  { pos: ChunkPos, revision: u64, storage: ChunkStorage },
}
```

`Full` when a chunk's change count exceeds `FULL_DELTA_THRESHOLD` (4,096). Damage without a break produces no delta.

### 8.4 Messages (`rmc_protocol`)

```rust
pub enum ClientMsg {
    Hello { protocol: u32, material_hash: u64 },
    Edit(EditCommand),
    PlayerState { pos: Vec3, yaw: f32, pitch: f32 },
    RequestChunk(ChunkPos),                        // replica asks for a snapshot after a revision gap
    SaveRequest,                                   // editor "Save" button
}

pub enum ServerMsg {
    Welcome { player: PlayerId, seed: u64, bounds: CellAabb, gen_params_hash: u64 },
    Rejected { reason: String },                   // Hello mismatch
    ChunkSnapshot { pos: ChunkPos, revision: u64, storage: ChunkStorage },
    Applied(EditResult),
    Inventory { items: Vec<(MaterialId, u32)> },
    PlayerStates(Vec<(PlayerId, Vec3, f32, f32)>), // unused in Phase 1; shape reserved for co-op
    Saved { ok: bool, path: String },
}
```

Serialization: serde + postcard. `ChunkSnapshot` is the join encoding and the save encoding.

### 8.5 Host (`rmc_host`)

```rust
pub struct Host { world: World, inventories: HashMap<PlayerId, Inventory>, transport: Box<dyn Transport>, ... }
impl Host {
    pub fn tick(&mut self);                      // drain inbound, validate+apply in order, send outbound
    pub fn world(&self) -> &World;               // tests read this; the client never has access to it
}
pub fn spawn_host_thread(host: Host, hz: u32) -> HostHandle;   // app use; wakes on message arrival OR tick, whichever first

pub trait Transport: Send {
    fn recv(&mut self) -> Vec<(PlayerId, ClientMsg)>;
    fn send(&mut self, to: Recipient, msg: ServerMsg);
}
```

- Default thread rate 60 Hz for single player. The thread also wakes immediately when a message arrives, so edit latency is bounded by processing time, not tick interval.
- Tests call `host.tick()` directly; no thread, fully deterministic.
- `ChannelTransport` (crossbeam). A network transport implements the same trait later.

**Replica rule:** the client never edits its replica except by `apply_delta`/`insert_chunk` inside `ServerMsg` handlers. If `apply_delta` returns a revision gap, the client sends `RequestChunk` and ignores deltas for that chunk until the snapshot arrives.

## 9. Worldgen (`rmc_worldgen`)

Pure function: `generate_chunk(seed, pos, &GenParams, &MaterialTable) -> Chunk`. Chunks generate in parallel (rayon). Unmodified chunks regenerate on load instead of being saved.

Generation evaluates steps 1–4 over a **34³ volume** (the chunk plus a 1-cell border) so step 5 can read neighbors; only the inner 32³ is written to the chunk.

1. **Surface:** 2D noise heightmap. Above: air. Topsoil 1–2 cells, then dirt (tag `soil`).
2. **Strata:** base material by depth band (stone → granite → basalt), noise-perturbed boundaries. All tagged `stone`.
3. **Caves:** 3D noise thresholded to air; plus a worm noise (two thresholded noises intersected). Suppressed within 4 cells of the surface.
4. **Ores:** per-ore 3D noise, thresholded, depth-ranged, replacing only cells tagged `stone`.
5. **Glowcrystal:** sparse placement on cells tagged `stone` that are face-adjacent to air, chosen by a cell hash.
6. **Bedrock shell:** bottom 2 layers and world-edge columns.

Noise: `fastnoise-lite`, seeded from `(seed, layer_id)`. Sampled in f32 world meters, thresholded to a material immediately. The noise crate version is recorded in the save header; a mismatch fails load.

`GenParams` lives in `assets/worldgen.ron`. Its hash is part of the save header and of `Welcome`.

## 10. Persistence

```
save/
├── header.ron     # format version, seed, bounds, material hash, gen params hash, noise crate version
└── chunks.bin     # zstd( postcard( Vec<(ChunkPos, revision, ChunkStorage)> ) ), modified chunks only, sorted
```

- Load: generate from seed, then overwrite with saved chunks. Fail loudly on any hash mismatch.
- Autosave every 5 minutes and on quit; `SaveRequest` from the editor. Save runs on the host from a snapshot of modified chunks.

## 11. Client (`rmc_client`)

### 11.1 Replica

Bevy resource `Replica(World)`, mutated only in `ServerMsg` handlers. Applied deltas mark chunks dirty, plus face-adjacent neighbors when a changed cell lies on a chunk border.

### 11.2 Meshing

```rust
pub trait ChunkMesher: Send + Sync { fn mesh(&self, input: &MeshInput) -> MeshData; }
pub struct MeshInput { pub pos: ChunkPos, pub revision: u64, pub cells: Box<[MaterialId; 34*34*34]>, pub materials: Arc<MaterialTable> }
pub struct MeshData { pub positions: Vec<[f32;3]>, pub normals: Vec<[f32;3]>, pub colors: Vec<[f32;4]>, pub indices: Vec<u32>, pub quad_count: u32 }
```

- `GreedyMesher`: faces between Solid and non-Solid, merged into rectangles. Merge key is `(material, normal, ao0, ao1, ao2, ao3)`: quads merge only when their four corner AO values match (the standard 0fps scheme). AO is 3-sample voxel AO, four levels. Vertex color = material color × AO factor; emissive strength is stored in color alpha.
- Jobs run on `AsyncComputeTaskPool`, polled with `check_ready`. A result older than the chunk's current revision is discarded and the chunk re-queued.
- Budgets: 8 jobs in flight, 4 uploads per frame, both configurable. During initial load (`Replica` has never been fully meshed) the upload budget is 32. Chunks nearest the camera first.
- Material: `StandardMaterial` with `base_color` white, vertex colors multiply in. Emissive uses an `ExtendedMaterial<StandardMaterial, EmissiveFromAlpha>` whose fragment adds `color.a * emissive_scale` to the emissive term. If the extension costs more than one milestone's time, ship without emissive and record it.
- Mesh assets use `RenderAssetUsages::RENDER_WORLD` only.

### 11.3 Lighting

Directional sun with shadows (surface), high ambient, a point light on the player (lantern), bloom on the camera (`Bloom::NATURAL`, `Hdr`). Caves stay comfortable to read. The camera has linear distance fog in the sky's horizon color, and a skybox with a gradient from that horizon color to a deeper zenith and a sun disc where the directional sun shines from, so distant terrain and the world's bedrock shell fade into the sky (owner's extension, F088).

### 11.4 Character controller

- Kinematic. AABB 0.6 m wide × 1.7 m tall. Eye height 1.55 m.
- Runs in `FixedUpdate` at 60 Hz. Rendering interpolates between the previous and current fixed-step positions using `Time<Fixed>::overstep_fraction()` (in-repo, following Bevy's `physics_in_fixed_timestep` example).
- Collision: per-axis swept AABB against solid cells in the replica (Y, X, Z). No physics engine.
- Gravity, jump, walk/sprint, auto step-up of one cell when grounded and the space above is clear, smoothed over ~0.1 s.
- Reads only `Replica`. Sends `PlayerState` at 20 Hz.
- Input is read from `ButtonInput<KeyCode>`, `ButtonInput<MouseButton>` and `AccumulatedMouseMotion`. Nothing reads winit directly. This is what makes headless injection equivalent to real play.

### 11.5 Raycast and pickaxe

- `rmc_world::raycast(&World, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit { cell, face_normal, distance }>` (Amanatides–Woo). Lives in `rmc_world` so the host can reuse it.
- Pickaxe: reach 3.0 m, swing interval 0.35 s, profile in `assets/tools.ron` (damage, interval, shape). Each swing sends `Edit { Damage(dmg), shape }`.
- HUD (Bevy UI, not egui): crosshair, targeted cell outline (gizmo), material name, damage progress, inventory counts, and a `HudState` resource mirroring the displayed values so tests can assert without reading pixels.

### 11.6 Debug editor

F1 toggles `bevy_egui` + `bevy-inspector-egui`:

- Brush: op (Set/Remove/Paint), shape (Cell/Box/Sphere), size, material picker. Left click applies at the raycast hit; Set targets the cell on the face-normal side.
- Fly camera (noclip), in-repo, separate from the controller.
- Gizmos: chunk bounds, targeted cell, mesh-queue state per chunk.
- Stats: FPS, chunks, mesh jobs, last remesh ms, host tick ms, edits/s.
- Worldgen: seed + regenerate.
- Save / Load buttons.

Every editor action also has a keyboard binding (listed in `docs/CONTROLS.md`) and sets the same `EditorState` resource the egui panel edits. Tests drive the editor through keys and `EditorState`, not by clicking egui widgets. The editor issues normal `EditCommand`s.

## 12. Test kit (`rmc_testkit`) and end-to-end testing

### 12.1 Headless app

`TestApp::new(TestConfig { seed, world_bounds, render: bool, size: (640, 360) })` builds the real game:

- `DefaultPlugins` with `WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit }`, `.disable::<WinitPlugin>()`, `RenderPlugin { synchronous_pipeline_compilation: true, .. }`, `ImagePlugin::default_nearest()`. `render: false` swaps in `MinimalPlugins` + `InputPlugin` for logic-only tests.
- `TimeUpdateStrategy::ManualDuration(1/60 s)`: every `step()` advances exactly one frame.
- The game's camera renders to an `Image` render target (`Rgba8UnormSrgb`, `COPY_SRC`).
- Host runs **in-process, synchronously**: `TestApp::step()` runs `host.tick()` then `app.update()`, so every edit is applied and replicated within one step. Same `ChannelTransport`, same messages.
- Screenshot: spawns `Screenshot::image(handle)` with an observer on `ScreenshotCaptured` that stores the `Image`; `step()` is called until it arrives.
- Software adapter pinned via env in the test runner: `WGPU_ADAPTER_NAME=llvmpipe` (Linux) or `WGPU_BACKEND=dx12 WGPU_FORCE_FALLBACK_ADAPTER=1` (Windows WARP). `RenderErrorPolicy` set to not exit the app on device errors.

### 12.2 Input injection

```rust
impl TestApp {
    pub fn step(&mut self) ; pub fn steps(&mut self, n: u32);
    pub fn press(&mut self, k: KeyCode); pub fn release(&mut self, k: KeyCode); pub fn hold(&mut self, k: KeyCode, frames: u32);
    pub fn look(&mut self, dx: f32, dy: f32);                // writes MouseMotion
    pub fn click(&mut self, b: MouseButton); pub fn mouse_down/mouse_up(..);
    pub fn screenshot(&mut self) -> image::RgbaImage;
    pub fn replica(&self) -> &World; pub fn host_world(&self) -> &World;
    pub fn player(&self) -> PlayerSnapshot;                  // pos, yaw, pitch, grounded
    pub fn hud(&self) -> HudState; pub fn inventory(&self) -> Inventory;
    pub fn editor(&mut self) -> &mut EditorState;
    pub fn save(&mut self, path) ; pub fn load(path) -> TestApp;
}
```

Keys and mouse are written as `KeyboardInput`, `MouseButtonInput` and `MouseMotion` messages before each `update()`; the game's `InputPlugin` turns them into `ButtonInput`/`AccumulatedMouseMotion` exactly as it does for winit. `press` holds the key until `release`.

### 12.3 Bots

`rmc_testkit::bots`: scripted players composed from the API above.

- `WalkBot`: walk to a target cell using yaw steering; reports stuck.
- `DigBot`: look at a cell, swing until it breaks.
- `TunnelBot`: dig a 2-high tunnel N cells forward, walking in as it goes.
- `BuilderBot`: place a 3×3 brass platform with the editor brush, climb it.
- `ChaosBot(seed)`: random walk, look, dig, place, jump for N frames; used by the soak test.

### 12.4 Scenario tests (`tests/e2e/`)

Each is a `#[test] #[ignore]` fn `<name>` in `tests/e2e/tests/<name>.rs` (package `rmc_e2e`), run with `--include-ignored` by `scripts/e2e.sh`, which also pins the software adapter. Each scenario saves a screenshot to `target/e2e/<name>-<step>.png` on completion and on failure.

| Scenario | Asserts |
|---|---|
| `boot_renders` | After 120 steps: no dirty chunks; screenshot has > 40% non-sky pixels; the `MeshStats` resource (F055) reports ≥ 200 chunks meshed |
| `walk_and_jump` | Holding W for 60 frames moves the player ≥ 2 m forward; jump leaves ground and lands; never intersects a solid cell |
| `step_up` | Player walks onto a 1-cell ledge without jumping; a 2-cell wall stops them |
| `dig_cell_visible` | Pickaxe swings until the targeted cell breaks; replica cell is Air; inventory count incremented; the pixel block under the crosshair changed color |
| `dig_tunnel` | TunnelBot digs 10 cells; player ends ≥ 4 m deeper into the wall; host and replica hashes equal |
| `place_and_climb` | BuilderBot places platform via brush (keys + EditorState); player stands on it |
| `editor_brush_sphere` | Sphere remove radius 4 produces the expected cell count change and ≥ 1 remesh |
| `save_load_roundtrip` | Dig + place, save, new TestApp from save, world hash identical, screenshot similar (image-compare RMS < threshold) |
| `fly_camera` | Noclip moves through solid cells and the camera position is not clamped by the controller (re-enable snap-back is a unit test in F070) |
| `soak_chaos` | ChaosBot 3,600 frames: no panic, host hash == replica hash at end, mesh queue drains, frame step time p95 < 33 ms on lavapipe (recorded, not asserted on CI) |
| `latency_dig` | Steps from swing to visible delta ≤ 2 (synchronous host) |

Pixel assertions are semantic (region changed, fraction non-background, RMS between two runs of the same scenario), never golden images.

### 12.5 Screenshot review

`scripts/screens.sh` runs the scenarios and copies screenshots into `docs/screens/`. The `reviewer` subagent (§16) reads them with the `Read` tool and reports; the main session records the findings in `PROGRESS.md`. Claude Code treats "the world is a flat gray plane" or "the camera is inside a wall" found in a screenshot as a failing feature even when tests pass.

## 13. Performance targets (release build, mid-range desktop)

| Metric | Target | Measured by |
|---|---|---|
| Frame rate, 256-chunk world, GPU | ≥ 60 fps | manual + `docs/perf.md` |
| Single-chunk greedy mesh | < 2 ms | criterion `mesh_chunk` |
| Dig to visible change | < 50 ms | `latency_dig` (steps) + manual |
| Full worldgen, 256 chunks, parallel | < 3 s | criterion `gen_world` |
| Host tick with 100 single-cell edits | < 2 ms | criterion `host_tick_100` |
| Mesh memory for 256 chunks | < 256 MB | stats panel; recorded |

## 14. Testing strategy (TDD)

Order for every feature: write the test(s) named in `features.json` so they fail for the right reason; implement until green; refactor with tests green; `scripts/pass-feature.sh <id>`; commit.

Conventions: unit tests live in `#[cfg(test)] mod tests` inside the module under test, so `features.json` names them as `<module>::tests::<name>`. Proptests read `PROPTEST_CASES` (default 256; the commit gate uses 64, CI uses 1000). Every test runs in under 10 s in the dev profile, which sets `opt-level = 1` for workspace crates and `opt-level = 3` for dependencies. `rmc_client` unit tests cannot use `rmc_testkit` (circular); they use `MinimalPlugins` apps or plain functions.

### `rmc_world`
- Coordinates: round trips, negatives, borders (unit).
- Palette: random set/get with promotion/demotion; `compact()` preserves content; Uniform↔Paletted hash equality (proptest).
- Shapes: sphere symmetry and counts r=0..8; box inclusivity; fixed order.
- Determinism: same command sequence on two worlds → identical hash (proptest 1,000 cases).
- Damage: ⌈H/dmg⌉ hits to break; saturating; bedrock never breaks; yields match table; `Set` clears damage.
- Raycast fixtures: axis, diagonal, chunk-border crossing, max distance, start inside solid.
- `apply_delta`: revision gap returns error, no partial application.
- Save/load round trip hash-identical; header mismatch fails.

### `rmc_host` / `rmc_protocol`
- Replica equivalence: 10,000 random edits via messages only, including `Full`, hashes equal.
- Rejection of oversized/out-of-bounds commands with no partial application.
- Serialization round trip for every message variant.
- `RequestChunk` after an induced gap repairs the replica.
- Host wakes on message arrival (thread test, 50 ms bound).

### `rmc_worldgen`
- Same seed + chunk → identical output; different seed → different.
- Border continuity across all six faces for 20 random chunk pairs.
- Bedrock shell complete; no caves within 4 cells of surface; ores only in `stone`-tagged cells; glowcrystal only adjacent to air.
- 256-chunk generation under 10 s in the dev profile (test); the 3 s release target is a criterion bench recorded at M8.

### `rmc_client`
- Greedy mesher fixtures: single cell (6 quads), 2×2 slab, checkerboard, AO-split case (a quad that must not merge because AO differs).
- Interpolation: rendered transform lies between previous and current fixed positions.
- Controller unit tests against hand-built worlds: floor, wall, ledge, ceiling, corner slide.
- Dirty-neighbor marking on border edits.

### End-to-end
§12.4.

### Benchmarks
criterion: `mesh_chunk`, `gen_world`, `host_tick_100`, `palette_set_get`. Results recorded in `docs/perf.md` at M8.

## 15. Build rules for Claude Code

The authoritative copy is `CLAUDE.md`. Summary:

1. No Bevy dependency in `rmc_world`, `rmc_worldgen`, `rmc_protocol`, `rmc_host`.
2. Cells change only through `World::apply`, `apply_delta`, `insert_chunk`; the client calls only the last two, only in `ServerMsg` handlers.
3. Integers in `rmc_world` logic; floats only at the raycast boundary and in worldgen noise sampling.
4. Output-affecting iteration is sorted or fixed-order.
5. Behavior from data tables; filter on `tags`/`phase`, never on specific IDs.
6. TDD: test first. Never delete or weaken an existing test. If a test is wrong, fix it in a commit whose message starts with `test-fix:` and explains why, and record it in `PROGRESS.md`.
7. `features.json` is edited only by `scripts/pass-feature.sh` and `scripts/skip-feature.sh` (optional features only).
8. Every commit passes `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `scripts/check-no-bevy.sh`. The commit gate hook enforces this; `verify-features.sh --quick` runs in CI.
9. One feature per loop iteration. Finish it, commit, update `PROGRESS.md`, stop.
10. Do not ask questions. When the SAD is ambiguous, take the conservative reading, implement it, and record the decision under "Decisions" in `PROGRESS.md`.
11. Pin versions. Before adding a crate, confirm on crates.io that a release targets the pinned Bevy minor.

## 16. Agentic workflow (overnight, unattended)

### 16.1 Environment

- **Where:** WSL2 (Ubuntu) on the 4090 machine, repo under `/home/<user>/rust-mining-co` (Linux filesystem, not `/mnt/c`). Native Windows is the fallback if WSL2 setup fails; on native Windows the Bash sandbox is unavailable and bash hooks run under Git Bash, which needs `jq` installed (`winget install jqlang.jq`) and `CLAUDE_CODE_GIT_BASH_PATH` set if Claude Code cannot find bash.
- **Packages (WSL2):** `sudo apt install build-essential pkg-config libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev mesa-vulkan-drivers vulkan-tools jq ripgrep`; `rustup` stable; `rust-analyzer` component.
- **GPU in WSL2** is not required. E2E uses lavapipe (`WGPU_ADAPTER_NAME=llvmpipe`). Confirm with `vulkaninfo --summary | grep -i llvmpipe`.
- **Claude Code:** current release; plugin `rust-analyzer-lsp@claude-plugins-official` for per-edit diagnostics; `claude` run once interactively in the repo to trust the folder.
- **Dedicated Linux user** for the run. The run has unrestricted shell within WSL2 (owner's decision). Blast radius is the WSL2 distro and the GitHub repo. Deny rules and `PreToolUse` hooks still apply in bypass mode and block force-push, hard reset, secret reads, and direct edits to `features.json`.

### 16.2 Loop shape

The official long-running-agent pattern: **fresh context per feature**, continuity through git and files.

```
scripts/run-overnight.sh
  for i in 1..MAX_ITER:
    claude -p "$(cat PROMPT.md)" \
      --permission-mode bypassPermissions --permission-prompts none \
      --model $MODEL --effort high --fallback-model sonnet \
      --max-turns 200 --output-format json --name "night-$i" > logs/run-$i.json
    stop if output contains ALL_FEATURES_PASS
    on rate-limit error: sleep 20 min, retry
    on 3 consecutive iterations with no new commit: run the unstick prompt once, then stop and write docs/REPORT.md
```

Each iteration (`PROMPT.md`): read `PROGRESS.md`, `features.json`, `git log -15`; run `scripts/smoke.sh` (a no-op before the workspace exists); pick the first feature with `passes: false` and not skipped whose `blockedBy` are all passed or skipped; TDD it; review if tagged; `scripts/pass-feature.sh <id>`; gate commit; append to `PROGRESS.md`; print `ALL_FEATURES_PASS` only if `scripts/verify-features.sh --all` says so; exit. A feature marked `optional` may be skipped with `scripts/skip-feature.sh` after one serious failed attempt; nothing else can be skipped.

Why not one long session with `/goal`: eight hours of compaction degrades the context; a fresh session that re-reads 200 lines of handoff is more reliable, and a failed iteration is retried rather than inherited.

### 16.3 Hooks (`.claude/settings.json`)

| Event | Hook | Effect |
|---|---|---|
| `PostToolUse` Edit/Write `*.rs` | `cargo-check.sh` | `cargo clippy -p <crate> -- -D warnings` on the touched crate; failure fed back as `decision: block` |
| `PreToolUse` Bash containing `git commit` | `gate-commit.sh` | fmt, clippy, `cargo test --workspace` (PROPTEST_CASES=64), no-bevy check; `exit 2` blocks the commit with the failure tail; `--no-verify` blocked |
| `PreToolUse` Edit/Write/MultiEdit | `protect-files.sh` | deny edits to `features.json`, `SAD.md`, `CLAUDE.md`, `PROMPT.md`, `.claude/**`, the feature scripts; deny any edit that removes a `#[test]` or adds `#[ignore]` outside `tests/e2e` (`.github/**` stays editable so CI can be fixed) |
| `Stop` | `stop-guard.sh` | block stopping with a dirty working tree (commit or stash first); includes the test-failure tail when Rust files are dirty; respects `stop_hook_active` and the 8-block cap |
| `SessionStart` compact | inline | re-inject `PROGRESS.md` tail and `git log -10` |

`permissions.deny`: `Bash(git push --force *)`, `Bash(git push -f *)`, `Bash(git reset --hard *)`, `Bash(git checkout -- *)`, `Bash(git clean *)`, `Bash(rm -rf *)`, `Read(~/.ssh/**)`, `Read(./.env)`, `Edit(./features.json)`, `Write(./features.json)`.

### 16.4 Subagents (`.claude/agents/`)

- `reviewer`: fresh context, read-only. Given a feature id, reads the diff, the SAD section, the tests, and the screenshots in `docs/screens/`; reports gaps against the SAD, not style. `PROMPT.md` invokes it before `pass-feature.sh` on every feature tagged `review: true`.
- `playtester`: runs `scripts/screens.sh` and `scripts/soak.sh`, reads the screenshots, writes one scenario of its own under `tests/e2e/tests/playtest_<date>.rs`, and appends a play report (what works, what looks wrong, what felt broken as a player would describe it) to `docs/PLAYTEST.md`. Invoked at the end of M6, M7, M8.

Agent teams are not used (not available in `-p`, and the docs advise against unattended team runs).

### 16.5 Model and budget

- `MODEL=opus` for implementation iterations; `--fallback-model sonnet`. Subagents: `reviewer` on `sonnet`, `playtester` on `sonnet`.
- Max 20x plan: a full night on opus can consume a large share of the weekly allowance. The driver script logs `total_cost_usd` and `num_turns` per iteration and backs off on rate-limit errors. If the first two iterations exceed 150 turns each, the driver switches `MODEL` to `sonnet` for the rest of the night.
- `--max-turns 200` per iteration. An iteration that hits the cap is not a failure; the next one resumes from the commit and `PROGRESS.md`.

### 16.6 Morning handoff

`scripts/report.sh` (run by the driver at the end) writes `docs/REPORT.md`: features passed/failed with test names, `PROGRESS.md` decisions, perf numbers, screenshots inline, and the exact commands to play:

```
git clone <repo> C:\src\rust-mining-co   (or download the CI "windows-release" artifact)
cargo run --release -p rmc_app -- --seed 42
```

Controls in `docs/CONTROLS.md`. CI's Windows job uploads `rust-mining-co.exe` + `assets/` as an artifact on every push to `main`.

## 17. Milestones and features

`features.json` is the source of truth. Milestones group features; order is by `blockedBy`.

| # | Milestone | Features (ids) | Acceptance |
|---|---|---|---|
| M0 | Workspace (7 crates + `rmc_e2e`), toolchain, dev profile, CI skeleton, no-bevy check, materials loader, scripts wired | F001–F005 | `cargo test` green; CI runs; `check-no-bevy.sh` passes; 0.20 resolves (or fallback recorded) |
| M1 | Chunk storage, palette, World queries, hashing | F010–F014 | palette proptests; hash equality |
| M2 | Shapes, edits, damage, yields, raycast, apply_delta, insert_chunk | F020–F027 | determinism and damage tests |
| M3 | Protocol, host, ChannelTransport, replica, RequestChunk repair | F030–F035 | replica equivalence at 10,000 edits |
| M4 | Worldgen | F040–F045 | determinism, border continuity, bedrock, tags, < 3 s |
| M5 | Testkit headless app + screenshot; mesher; async pipeline; rendering; lighting; fly camera | F050–F058 | `boot_renders` passes on lavapipe; mesher fixtures |
| M6 | Controller, interpolation, pickaxe, HUD, inventory | F060–F067 | `walk_and_jump`, `step_up`, `dig_cell_visible`, `dig_tunnel` |
| M7 | Debug editor, save/load, controls doc | F070–F076 | `place_and_climb`, `editor_brush_sphere`, `save_load_roundtrip` |
| M8 | Soak, perf, Windows CI artifact, report | F080–F085 | `soak_chaos`; benches recorded; artifact built; `REPORT.md` |

## 18. Extension points (not built in Phase 1)

| Future feature | Where it attaches |
|---|---|
| Co-op networking | New `Transport`; client-side prediction for edits; `PlayerStates` relay already in the protocol. Controller collision against the replica means a player can briefly stand in a cell another player just filled; prediction work must handle this |
| Dedicated server | New binary over `rmc_host`. No Bevy |
| Ecology and gardening | Sparse per-chunk layers next to the damage map; new `EditOp` variants |
| Base building | `Set` with `placeable`-tagged materials plus multi-cell structure definitions |
| Creatures | Host-side entities reading `World` queries and issuing `EditCommand`s |
| Hand-built ruins | Blender → glTF → voxelizer → prefab stamped by worldgen |
| Steam tools | `tools.ron` entries with other ops/shapes |
| Liquids | `Phase::Liquid` and a host-side flow step |
| Smooth terrain | Alternative `ChunkMesher` |
| Crack visuals | `CellDamage` message from host |

## 19. Open questions (defaults in force)

| Question | Default |
|---|---|
| Pickaxe shape: one cell or cluster | One cell, configurable in `tools.ron`; decide after playing |
| World size and streaming | Fixed bounds, all chunks resident |
| Persist cell damage | No |
| Co-op host tick rate | 60 Hz single-player; revisit for network |
| Bevy 0.20 vs 0.19.1 | 0.20; fallback rule in §3 |

## 20. Sources consulted

- Bevy 0.20 release and migration guide: https://bevy.org/news/bevy-0-20/ , https://bevy.org/learn/migration-guides/0-19-to-0-20/
- Bevy headless rendering and screenshots (v0.19.1 examples, unchanged in 0.20): `examples/app/externally_driven_headless_renderer.rs`, `examples/app/headless_renderer.rs`, `examples/window/screenshot.rs`, `tests/how_to_test_apps.rs`
- Bevy fixed timestep interpolation: `examples/movement/physics_in_fixed_timestep.rs`
- Claude Code: https://code.claude.com/docs/en/headless , /permission-modes , /permissions , /hooks , /sub-agents , /worktrees , /sandbox-environments , /setup
- Anthropic, "Effective harnesses for long-running agents": https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents
- Anthropic, "Getting started with loops": https://claude.com/blog/getting-started-with-loops
- Software rasterizers in CI: wgpu `.github/actions/install-mesa`, `install-warp`; Bevy `.github/workflows/example-run.yml`
