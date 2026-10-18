# Rust Mining Co — Phase 2: Ecology, mineral upward

**Version:** 0.5
**Date:** 2026-10-18
**Depends on:** Phase 1 complete (`docs/SAD.md`, all F0xx features passing)
**Audience:** Claude Code (implementer, unattended), Bucket (owner)
**Feature list:** `features.json` entries F100–F175 (appended by `scripts/merge-features.sh features-p2.json`)

---

## 1. Purpose

Phase 1 produced a voxel sandbox where the player digs and places anything through the host/replica command path. Phase 2 makes the land respond. It adds a season tick on the host that moves heat, water, substances and gases through the chunks, weathers rock into soil, melts and freezes, and grows pioneer plants where conditions allow. Every process reads the material table and a small set of per-chunk layers, and every process is a pure, deterministic, conservation-checked function.

Phase 2 ends with a player who can dig a swale, advance a season, and find it full of water; leave crushed sulfide ore above it and find the water acid and the moss dead three seasons later; run that water through a limestone channel and watch it clear; break into a magma chamber and see water flash to steam and rock to obsidian; vent a firedamp pocket by driving a shaft to the surface. Nothing in Phase 2 is a creature, a member, a council, or a charter. Those are Phase 3 and they read what Phase 2 produces.

Order of construction follows the Rumi ascent: mineral first. Temperature, then water and ice, then weathering, then magma, then substances and reactions, then gases, then light, then plants.

## 2. Scope

### In scope

- Season tick on the host: editor button, shift end, headless CLI, testkit. Deterministic, fixed sub-steps.
- Per-chunk sparse layers: temperature, moisture, regolith, substances (10), gases (4), light, growth.
- Material table extensions: thermal, hydraulic, chemical properties; composition vectors; emission.
- Phase transition table (data), reaction table (data), both walked by the tick.
- Water as a visible liquid material with partial fill, flow, infiltration, seepage, evaporation, condensation, pooling.
- Magma as a whole-cell liquid that flows slowly, carries its heat, and sets to basalt or obsidian. Ice.
- Steam as a gas that is conserved water.
- Gases as concentration layers on air cells: steam, firedamp, chokedamp, smoke. Drift, diffusion, venting, ignition by magma.
- Substances in soil and water: weathering releases them gradually, water transports the mobile fraction, reactions change form, precipitation creates crust materials. "Spoil" is a query, not a stored flag.
- Light: global column pass with lateral falloff, attenuated by water.
- Plants: four pioneer species with needs, growth, spread, death to litter; held substances (hyperaccumulation) and harvest; organic carbon decomposes to nitrogen.
- Worldgen additions: geothermal field, magma chambers, limestone band, water table, surface plants, initial light.
- Rendering: liquid pass with fill height, translucency; new material colors; gas tint near camera; layer overlay mode in the mesher.
- Editor: season button, layer overlay picker, place water/ice/magma/crushed_sulfide with the brush, readout at the targeted cell.
- Testkit: `advance_season()`, layer readers, fixture builders, season overrides.
- Save format v2 with layers and plant state. Phase 1 saves are rejected (format version bump).
- Default Phase 2 world: 128 × 64 × 128 cells (4 × 2 × 4 = 32 chunks, 1 M cells). The 256-chunk world remains available by CLI flag and is a Phase 3 scaling target.
- Tests: conservation, determinism, oracle-equivalence for sweeps, table-driven behavior, E2E scenarios per rung, a seasons soak.

### Out of scope (Phase 3+)

- Decomposer organisms, animals, pollinators, creatures of any kind.
- Members, proxies, standing orders, the hall, polities, charters.
- Player health, damage from gas, lantern ignition (HUD warnings only).
- Wall-clock season ticking during play.
- Free-surface fluid dynamics (pressure, waves). Water is a cellular fill model; magma is a cellular whole-cell model.
- Erosion and deposition (`erodibility` is loaded, unused).
- Fire as a spreading process.
- Steam power, boilers, pipes, base building.
- Parallel execution of the sweep passes and the 256-chunk performance pass.

## 3. Fixed decisions

| Decision | Value | Reason |
|---|---|---|
| Season tick trigger | Editor button; shift end (quit/save); `advance_season()` in tests; `--seasons N` headless | Deterministic; no mid-play remeshing |
| Sub-steps per season | `seasons.substeps` = 8 | Enough for water to settle and heat to move a few meters |
| Flow iterations per sub-step | `eco.flow_iters` = 8 | Water moves faster than seasons; bounded cost |
| Pass order per sub-step | temperature → transitions → water (incl. magma and contact boiling) → gases → weathering → chemistry → light → plants | Each pass reads the previous pass's output. Order is part of the spec |
| Two kinds of pass | **Jacobi passes** (temperature, transitions, weathering, reactions, plant growth): compute from state S over a chunk with a 1-cell border, apply to S' in sorted chunk order; run per chunk in parallel. **Sweep passes** (water, gases, chemistry transport, plant spread, light): single-threaded, over the whole world in a fixed global cell order, each move applied immediately with donor-content and receiver-room checks against current state. Light is a sweep: its column pass is sequential and its lateral iterations are double-buffered over the whole world, single-threaded (§7.7) | Jacobi is only safe where there is no capacity limit and no dependency across more than one cell. Everything else is a sweep |
| Global cell order for sweeps | `(y, cz, cx, lz, lx)`: y ascending over the world; within a y-plane, chunks in sorted (cz, cx) order, then local (z, x). Light's column pass is y descending | Deterministic; matches a flat-array reference implementation, which the oracle tests use |
| Active sets for sweeps | Per chunk, per sweep kind, a 32768-bit set of cells to visit; a cell is set when it holds the moving quantity or received any of it; cleared when it holds none at the end of a visit. Sweeps visit only set cells | Cost proportional to wet or gassy cells, not world size |
| Cross-chunk rule for Jacobi passes | A flux between two cells is a pure function of the ordered pair's state and is computed identically by both chunks | Border continuity without a global pass |
| Numerics | Integers only. `i16` temperature in 0.1 °C; `i32` intermediates; `u8` for everything else with saturating arithmetic where a cap exists; integer division truncates toward zero (Rust `/`), never `>>` on signed values | Determinism and antisymmetry. The temperature pass quantizes each edge's transfer so that `Σ T·C` is exactly conserved (§7.1) |
| Conservation | Water (fill + porous moisture + ice fill + steam), each substance, each gas, and magma cell count: conserved exactly with every source and sink in a typed `SeasonLedger`; ledger entries record actual amounts moved, never requested amounts | Test invariant for every pass |
| Named constants | Every rate, threshold, shift and constant lives in `assets/eco.ron` under the names used here. Pass code contains no numeric literals other than 0, 1 and array sizes | Rule 19 |
| Role materials | `eco.ron` `roles`: `water`, `ice`, `magma`, `obsidian`, `basalt`, `air`, `steam`, `smoke`, `firedamp`, `chokedamp`, `organic` (= substance `c`), `copper_dust` and the other per-substance yield materials. Code refers to roles | Rule 18 |
| Layer storage | `BTreeMap<LayerId, LayerData>` per chunk; absent means 0; `Temperature` and `Light` are materialized for every chunk at worldgen | Sparse except the two dense ones; no float on read |
| Chunk revisions | `revision` bumps on material change. `eco_revision` bumps on any layer or plant-state change. Replication: `ChunkSnapshot` (v2) when `revision` changed, `LayerSnapshot` when only `eco_revision` changed, `ChunkDelta::Layers` for edit-time layer changes between seasons | Remesh only when materials change |
| Layer mutation | Host: inside `season_tick`, `EditOp::SetLayer`, worldgen, and the displacement rules in §7.0 (which run inside `World::apply`, emitting layer deltas, and inside `World::eco_set`). Client: only via deltas and snapshots in `ServerMsg` handlers | Same discipline as cells |
| Cell writes during the tick | Passes change materials only through `World::eco_set(cell, to, &mut SeasonLedger)` (§7.0.1): host-only, called only from `rmc_eco` inside `season_tick`. It runs the §7.0 displacement rules, bumps `revision`, maintains `surface_y`, sets `modified`, and emits no `ChunkDelta` (the post-season snapshots replicate). Edits between seasons keep using `World::apply` | One cell path for passes, without per-cell edit validation and delta building inside a sweep |
| Ledger home | `SeasonLedger` lives in `rmc_world::ledger` (plain data, no logic) so `World::apply` and `World::eco_set` can write displacement terms into it. `rmc_eco` re-exports it and owns the invariant checker | `rmc_eco` depends on `rmc_world`, not the reverse |
| Water units | `moisture` is an absolute amount 0..255: fill of a `water` or `ice` cell, or water held by a porous solid up to its `field_capacity` (absolute). Magma does not use `moisture` | One unit, one conservation sum |
| Magma representation | Whole-cell liquid. A magma cell is full by definition; it moves by swapping with an air cell, taking its temperature with it. Conserved as a cell count | No partial magma; no fill accounting |
| Gas representation | Concentration `u8` per gas kind on `air` cells (role `air` is the only `Phase::Air` material) | Mixtures; no mesher impact |
| Substance representation | Ten `u8` layers; amount adsorbed in a solid or dissolved in water; `c` doubles as organic carbon | Nutrients and metals are the same kind of thing |
| "Spoil" | `query::spoil(region, profile)`; never stored | The HUD and charters use the word |
| Save format | v2 only; header gains `tables_hash`; v1 saves fail with a version message | |
| Columns | Per column `surface_y: i16` = highest cell that is `Phase::Liquid`, or `Phase::Solid` and not `passable`. "Sky-open" = `y > surface_y`. "Surface" = `y == surface_y`. Maintained on every material change in that column | One definition for rain, vent, clamps, light |
| Temperature scale | `conductivity` ≤ 64 (rejected at load if higher), `heat_capacity` a power of two in 8..=128 (rejected otherwise), `heat_shift` = 7 | Keeps the explicit scheme stable and makes 2 °C differences move heat (see §7.1) |

## 4. Crate changes

```
crates/
├── rmc_world/      + layers.rs, + columns.rs (surface_y), + plants state on Chunk, + eco_revision, + ChunkDelta::Layers, + ledger.rs (SeasonLedger), + displacement shared by apply and eco_set, + query.rs, + Phase::Liquid
├── rmc_worldgen/   + geothermal profile, magma chambers, limestone band, water table, surface plants, initial temperature and light layers
├── rmc_eco/        NEW. NO Bevy. The season tick: passes, tables, active sets, invariant checker (re-exports rmc_world::ledger). Depends on rmc_world
├── rmc_protocol/   + ChunkSnapshot v2, + LayerSnapshot, + ClientMsg::AdvanceSeason, + EditOp::SetLayer, + ServerMsg::SeasonStarted/SeasonDone
├── rmc_host/       + season tick orchestration, + between-season edit ledger, + headless --seasons, + --world-size
├── rmc_client/     + liquid mesh pass, + layer overlay, + gas tint, + editor eco panel, + HUD eco readout
├── rmc_testkit/    + advance_season, layer readers, fixtures, season overrides
└── tests/e2e/      + Phase 2 scenarios
```

Data files: `assets/substances.ron`, `assets/gases.ron`, `assets/transitions.ron`, `assets/reactions.ron`, `assets/plants.ron`, `assets/seasons.ron`, `assets/eco.ron`. `materials.ron` grows new fields with serde defaults; Phase 2 ships new entries and changes `copper_ore` to `sulfide_ore` (F140). Phase 1 tests and `worldgen.ron` that name `copper_ore` are updated in one `test-fix:` commit whose only change is the key.

## 5. Material table extensions

```rust
pub struct MaterialDef {
    // Phase 1 fields unchanged; `phase` gains `Liquid` (Phase 1 has only Air | Solid; F100 adds it)
    pub passable: bool,               // default false; plants true; the controller treats passable solids as air; light treats them as air
    pub conductivity: u8,             // default 16; max 64
    pub heat_capacity: u8,            // default 16; a power of two in 8..=128
    pub permeability: u8,             // default 0
    pub field_capacity: u8,           // default 0; absolute moisture a porous solid holds
    pub viscosity: u8,                // default 0; magma: a cell attempts a move only on iterations where iter % (viscosity+1) == 0
    pub weathering_rate: u8,          // default 0
    pub weathers_to: Option<String>,
    pub erodibility: u8,              // loaded, unused
    pub composition: Vec<(String, u8)>, // substance key, total released over a full 0..255 regolith cycle
    pub emits: Vec<(String, u8)>,     // gas key, amount per sub-step while exposed to air
    pub heat_source: Option<i16>,
    pub light_attenuation: u8,        // default: 0 for Air and passable solids, 255 otherwise; the table sets water 32 and ice 0
    pub anchor: bool,
    pub yield_material: Option<String>,
}
```

New materials: `water` (Liquid, diggable, hardness 0, yield_count 0, light_attenuation 32), `ice` (Solid, diggable, tags none, light_attenuation 0), `magma` (Liquid, viscosity 3, conductivity 64, heat_capacity 32), `magma_source` (Solid, not diggable, heat_source 13000), `obsidian`, `basalt` (gains composition ca/fe/p), `limestone` (tags stone, carbonate, porous; permeability 32; field_capacity 24; weathering_rate 2; composition lime 120), `sulfide_ore` (replaces copper_ore; tags stone, sulfide, ore; composition cu 60, fe 40, s 80; yield_material crushed_sulfide), `crushed_sulfide` (Solid, placeable; tags sulfide, porous; permeability 96; field_capacity 48; weathering_rate 48; composition as sulfide_ore), `ochre_crust` (tags crust, porous; composition fe 60), `salt_crust` (tags crust; composition salt 80), `moss`, `grass`, `fireweed`, `shrub` (Solid, passable, tag plant), `copper_dust`, `iron_dust`, `salt_dust` (placeable yield materials with the matching composition; one per substance that has `yield_material` in `substances.ron`).

Tags the code paths read: `stone`, `soil`, `porous`, `sulfide`, `carbonate`, `crust`, `plant`, `ore`. Keys are read only through `eco.ron` roles.

**`Phase::Liquid` in Phase 1 code.** Collision: not solid (§10). Raycast: Liquid cells are hit, so the pickaxe and brush can target water and magma (magma is not diggable, so only the brush changes it). Meshing: opaque pass until F127 adds the liquid pass. Every exhaustive `match` on `Phase` handles the new variant; Phase 1 behavior for Air and Solid materials is unchanged.

## 6. Layers and chunk state

```rust
pub enum LayerId { Temperature, Moisture, Regolith, Substance(u8), Gas(u8), Light, Growth }
pub enum LayerData { U8(Box<[u8; 32768]>), I16(Box<[i16; 32768]>) }

pub struct ChunkLayers { map: BTreeMap<LayerId, LayerData> }
impl ChunkLayers {
    pub fn get_u8(&self, id, local) -> u8;        // 0 if absent
    pub fn get_i16(&self, id, local) -> i16;      // 0 if absent (Temperature is never absent after worldgen)
    pub fn set_u8(&mut self, id, local, v);  pub fn set_i16(&mut self, id, local, v);
    pub fn compact(&mut self);                    // drops all-zero u8 layers; never drops Temperature or Light
    pub fn hash(&self) -> u64;
}

pub struct PlantState { species: u8, stress: u8, age_seasons: u8, held: [u8; N_SUBSTANCES] }
pub struct Chunk {
    pub storage: ChunkStorage, pub layers: ChunkLayers, pub plants: BTreeMap<u16, PlantState>,
    pub freeze_thaw: Box<[u64; 512]> /* bitset, season-scoped */,
    pub active: [Box<[u64; 512]>; N_SWEEPS],      // active sets per sweep kind (water, gas×4, transport, spread)
    pub revision: u64, pub eco_revision: u64, pub modified: bool,
}
```

`Chunk::hash()` folds storage, layers and plants (not `active` or `freeze_thaw`). `World::hash()` therefore covers ecology state. Cell damage stays on `World`, host-only, unsaved.

`World` gains `columns: HashMap<(i32,i32), [i16; 1024]>` (per chunk column, `surface_y`), maintained by `apply`, `apply_delta`, `insert_chunk`, with `i16::MIN` meaning no solid or liquid in the column.

`World::eco_set` (§7.0.1) is the season tick's only way to change a cell's material.

`ChunkDelta` gains `Layers { pos, eco_revision, changes: Vec<(u16, LayerId, i32)>, plants: Vec<(u16, Option<PlantState>)> }`, produced by `World::apply` for `SetLayer` and for the §7.0 effects of material edits, and applied by the replica in `apply_delta`.

## 7. The season tick (`rmc_eco`)

```rust
pub struct EcoTables { materials, substances, gases, transitions, reactions, plants, seasons, params: EcoParams, roles: Roles }

// rmc_world::ledger, re-exported by rmc_eco
pub struct SeasonLedger {
    // water units
    pub rain_in: i64, pub steam_vent_out: i64, pub water_edit_in: i64, pub water_edit_out: i64,
    pub evap_to_steam: i64, pub boil_to_steam: i64, pub condense_from_steam: i64,       // informational: water↔steam within the water total
    // substances
    pub weather_in: [i64; N_SUB], pub litter_in: [i64; N_SUB], pub uptake_sink: [i64; N_SUB], pub harvest_out: [i64; N_SUB],
    pub edit_in: [i64; N_SUB], pub edit_out: [i64; N_SUB], pub react_delta: [i64; N_SUB], pub saturation_loss: [i64; N_SUB],
    // gases (steam's gas entries mirror the water entries above)
    pub emit_in: [i64; N_GAS], pub vent_out: [i64; N_GAS], pub react_out: [i64; N_GAS], pub react_in: [i64; N_GAS],
    pub gas_edit_out: [i64; N_GAS], pub gas_saturation_loss: [i64; N_GAS],
    // magma
    pub magma_set: i64 /* cells turned to basalt/obsidian */, pub magma_melt_in: i64 /* stone melted */, pub magma_edit: i64,
    // events
    pub transitions: Vec<(MaterialId, MaterialId, u32)>, pub weathered_cells: u32, pub plants_born: u32, pub plants_died: u32, pub ignitions: u32,
}
```

Invariants, checked by tests after every pass of every sub-step:

- **Water:** `Σ moisture(water, ice, porous) + Σ steam = before + rain_in + water_edit_in − water_edit_out − steam_vent_out`.
- **Substance x:** `Σ layer_x + Σ held_x = before + weather_in + litter_in + edit_in − edit_out − uptake_sink − harvest_out + react_delta − saturation_loss`.
- **Gas g (non-steam):** `Σ c_g = before + emit_in + react_in − vent_out − react_out − gas_edit_out − gas_saturation_loss`. For steam, `emit_in` is `evap_to_steam + boil_to_steam`, `react_out` includes `condense_from_steam`, and `vent_out` equals `steam_vent_out`.
- **Magma:** `count(magma) = before + magma_melt_in + magma_edit − magma_set`.

Edits are paused during the tick. Edits made between seasons write their ledger terms into `host.between_seasons: SeasonLedger`, which the next season's report includes.

### 7.0 Displacement rules

Implemented once in `rmc_world` and called by both `World::apply` (edits: effects emitted as `ChunkDelta::Layers`; ledger terms written into a `SeasonLedger` on the `EditResult`, which the host merges into `between_seasons`) and `World::eco_set` (passes: no deltas; ledger terms written into the season's ledger). Every material change passes through them.

- **Solid or Liquid water/ice → Air** (pickaxe, brush Remove, ignition, drying): `moisture` and substance layers stay on the cell (a wet pit is a puddle next sweep). `regolith` and `growth` zero. A plant on top of the cell dies by the §7.8 death rule.
- **Water → Air by boiling** (§7.2 BoilAll, §7.3 contact): the fill is converted to steam, not retained. This is the one exception, and the ledger records it.
- **Air → Solid** (brush Set, crust formation): `moisture` is pushed to the first air or water neighbor in sorted order with room; remainder to `water_edit_out` (or `saturation_loss` when caused by a pass). Substances follow the water proportionally; remainder stays adsorbed in the new solid. Gases are pushed to air neighbors in sorted order; remainder to `gas_edit_out` / `gas_saturation_loss`.
- **Air → Liquid water** (flow, rain, brush Set(water)): gases pushed as above. Brush `Set(water)` or `Set(ice)` sets fill 255 and ledgers `water_edit_in`; `Set(magma)` sets temperature to `eco.magma_init_temp` and ledgers `magma_edit`.
- **Water ↔ Ice:** moisture retained.
- **Magma → Basalt / Obsidian:** ledger `magma_set`; temperature unchanged.
- **Stone → Magma:** ledger `magma_melt_in`; temperature unchanged.
- **Magma → Air by digging:** not allowed; magma is not diggable (`diggable: false`).
- **Plant removal** (dig, death): `held` substances go into the anchor cell below if it is `anchor`, else `saturation_loss`; on harvest (dig) they go to the inventory instead (§7.8).

### 7.0.1 `World::eco_set`

```rust
impl World {
    /// Host-only. Called only by rmc_eco inside season_tick.
    pub fn eco_set(&mut self, cell: CellPos, to: MaterialId, ledger: &mut SeasonLedger);
}
```

Applies one material change. It runs the full §7.0 displacement set for the (from, to) pair, including the boiling exception (fill to steam) and the Air→Solid push of moisture, substances and gases, with pass-caused remainders charged to `saturation_loss` / `gas_saturation_loss`. It updates `surface_y` and the sweep active sets of every cell it changed or pushed into, bumps the chunk's `revision`, sets `modified`, and clears the cell's damage. No validation, no delta. `from == to` changes nothing.

### 7.1 Temperature (Jacobi, parallel)

- Clamps before diffusion: cells with `y ≥ surface_y` that are air, water or ice → `seasons[season].air_temp`; `heat_source` materials → their value; the bottom cell layer → `profile(bottom_y)`.
- Edge flux for each face pair with i < j in global order: `f = (k · (T_j − T_i)) / 2^heat_shift` using `i32` and truncating division (so `f_ij = −f_ji` exactly), `k = min(cond_i, cond_j)`.
- Energy update, per edge: with `m = max(C_i, C_j)` and `q = (f_ij / m) · m` (truncating), `T_i += q / C_i` and `T_j −= q / C_j`. Because capacities are powers of two, `m` is a multiple of both, so both divisions are exact and the edge moves exactly `q` units of energy out of one cell and into the other. When `C_i == C_j` this is `T_i' = T_i + Σ_j trunc(f_ij / C_i)`. Each cell sums its edge terms (computed identically from both sides, so the pass stays Jacobi and parallel) and the result is clamped to `i16`. With `k ≤ 64`, `C ≥ 8`, `heat_shift = 7`: six neighbors at the maximum difference change a cell by at most `6·64/128/8 = 0.375` of the difference per sub-step, which is stable, and a 2 °C difference (ΔT = 20) between stone cells (k = 32) gives `f = 5` and a change of `5/16 = 0` for C = 16 but `5/8 = 0` for C = 8. Therefore conductivity and capacity are tuned so that surface materials (soil, water, air) have `C = 8` and `k = 64`, giving movement at ΔT ≥ 16 (1.6 °C). Deep rock (`k = 16`, `C = 32`) moves only above ΔT ≈ 64 (6.4 °C), which is the point: deep rock is quiet. The test `surface_cell_follows_seasonal_swing` asserts a pond (1 cell deep) and its floor cross 0 °C between a winter and a summer season.
- Fluid advection: when a magma cell swaps with an air cell (§7.3), their temperatures swap. Water flow does not move temperature.
- Energy `Σ T·C` is exactly conserved by every edge, so with no sources and no clamps it is constant (and therefore non-increasing). Truncating the update per cell instead would let a cell with several small inflows round up and create energy; truncating per side with unequal capacities would too (soil C = 8 gains 1 degree from f = 12 while rock C = 32 loses 0). Dead zone: an edge moves heat only when `|f_ij| ≥ max(C_i, C_j)`, so a cell moves only when one neighbour's flux reaches the edge's capacity: surface materials (k = 64, C = 8) at ΔT ≥ 16 (1.6 °C), deep rock (k = 16, C = 32) at ΔT ≥ 256 (25.6 °C). Also tested: maximum principle (no cell leaves the [min, max] of itself and its six neighbours), global spread non-increasing, and a linear profile under uniform conductivity is a fixed point.

### 7.2 Phase transitions (Jacobi; temperature rules only)

`transitions.ron`, table order, first match per cell:

```
[
  (from: Key("water"), to: "ice",      when: TempBelow(0)),
  (from: Key("ice"),   to: "water",    when: TempAbove(50)),      // 5.0 °C hysteresis
  (from: Key("water"), to: "air",      when: TempAbove(1000), effect: BoilAll),
  (from: Key("magma"), to: "basalt",   when: TempBelow(9000)),
  (from: Tag("stone"), to: "magma",    when: TempAbove(12500)),
]
```

`BoilAll`: the cell becomes air and its fill becomes steam concentration 1:1 (ledger `boil_to_steam`). Contact boiling is in §7.3 because it needs sequential targeting. Worldgen sets magma at `eco.magma_init_temp` (12000) and clamps cores, so chambers stay molten; magma that flows away cools and sets.

### 7.3 Water and magma (sweep)

Per sub-step, in this order:

**A. Condensation** (once, sweep over cells in the steam active set): an air cell with steam > 0 and `T < eco.condense_temp` (default 900, i.e. 90 °C) converts `min(steam, 255 − moisture)` to moisture on itself (it becomes water in step 1 of the first sweep); ledger `condense_from_steam`. Steam that cannot condense for lack of room stays steam.

**B. Flow sweeps** (`eco.flow_iters` of them), each visiting the water active set in global order. For a visited cell:

1. **Air with moisture > 0 → water** via `World::eco_set` (§7.0.1).
2. **Down.** Below is air: move all fill (below becomes water). Below is water with fill < 255: move `min(fill, 255 − fill_below)`. Below is porous with room: move `min(fill, perm_below / 2^infil_shift, cap_left)`.
3. **Sideways.** For the 4 horizontal neighbors in sorted order: air or water with lower fill → move `min(flow_cap, (fill − fill_n) / 2, 255 − fill_n)`; porous with room → move `min(perm_n / 2^infil_side_shift, cap_left)`.
4. **Seepage.** A porous cell with `moisture > field_capacity · seep_num / 256` sheds `min(moisture − threshold, perm / 2^seep_shift)` to the first neighbor in sorted order (below first) that is air (becomes water) or water with room, or porous with room.
5. **Water at fill 0 → air.**
6. **Magma** (every `viscosity+1`-th iteration): if the cell below is air, swap the two cells (material and temperature); else for the 4 horizontal neighbors in sorted order, swap with the first that is air. Magma never enters water, porous solids, or liquids of another kind.
7. **Contact boiling.** When the visited cell is magma and has a water face neighbor (first in sorted order): that water cell's fill becomes steam on itself, `min(fill, 255)`, with any excess fill above 255 staying as water (it cannot exceed 255 anyway, so the whole fill converts); the water cell becomes air; the magma cell becomes obsidian; ledger `boil_to_steam`, `magma_set`. One event per visit.

**C. Evaporation** (once, sweep over water cells whose cell above is air): `e = min(evap_table[temp_bucket(T)], fill, 255 − steam_above)`; `fill −= e; steam_above += e`; ledger `evap_to_steam`.

**D. Rain** (once, per column): target is the cell at `surface_y + 1` if it is air or water; if it is a passable plant, the target is the anchor cell below it (porous) instead; if there is no cell (above world top) there is no rain. Amount `r = min(seasons[season].rain · pattern(seed, season, x, z) / 256, room)`, where room is `255 − moisture` for air/water and `field_capacity − moisture` for porous. Ledger `rain_in` records `r` (actual).

Every move in B2–B4 and B7 is appended to the sub-step's `MoveLog` as `(from, to, amount, fill_from_before)` for chemistry.

### 7.4 Gases (sweep per kind)

`gases.ron`: as before, with `density`, `diffusion`, `flammable`, `ignite_min`, `ignite_radius_per_64`, `visible`.

One sweep per sub-step per gas over that gas's active set, global order, immediate application, room checked on every move:

1. **Drift.** Lighter: move `min(c, |density| · 2^drift_shift, room_above)` to the air cell above. Heavier: to the air cell below.
2. **Diffusion.** For each of 6 air neighbors in sorted order with `c_n < c`: move `min((c − c_n) · diffusion / 2^diff_shift, room_n, c)`.
3. **Vent.** Sky-open air cells: `vent_out += c; c = 0`.
4. **Emission** (not per gas; once after the sweeps): each solid with `emits` and an air face neighbor adds `min(amount, room)` to its first air neighbor in sorted order; ledger `emit_in` records the actual amount.
5. **Ignition** (during the flammable gas's sweep): a cell with `c ≥ ignite_min` face-adjacent to a `heat_source` material or a magma cell: `react_out[g] += c`; `smoke` on the cell gains `min(c, room_smoke)` with ledger `react_in[smoke]` recording the actual; then every cell of `Sphere { center, radius: (c / 64) · ignite_radius_per_64 }` that is in bounds, resident and `diggable` becomes air through `World::eco_set` in the shape's iteration order, with no yields (§7.0 handles the displaced layers, charging `saturation_loss` where nothing can receive). Ledger `ignitions`.

### 7.5 Weathering (Jacobi)

**Freeze-thaw bit.** `Chunk.freeze_thaw` is cleared at the start of each season. In each sub-step the temperature pass sets a cell's bit when `(T_before < 0) != (T_after < 0)`, where `T_before` is the cell's temperature before that pass (before clamps) and `T_after` after it. The bit stays set for the rest of the season; below, `freeze_thaw` is that bit (0 or 1).

For each solid cell with `weathering_rate > 0` that has an air or liquid face neighbor and has `moisture > 0` or a water face neighbor:

- `Δ_req = weathering_rate · (1 + freeze_thaw)`; `Δ = min(Δ_req, 255 − regolith)`; `regolith += Δ`.
- Gradual release: for each `(x, total)` in `composition`: `rel = total · Δ / 256` (truncating); `x += min(rel, 255 − x)`; ledger `weather_in` records the actual, `saturation_loss` the remainder. Over a full 0..255 cycle the sum of releases is at least `total − 255 · n_steps / 256`, which the test `release_complete_within_rounding` checks.
- At `regolith == 255`: material becomes `weathers_to` (delta), `regolith = 0`.
- Pickaxe: damage needed is `hardness / 2` when `regolith > eco.soft_regolith`.

Yields: breaking a material with `yield_material` yields that material. `sulfide_ore` yields `crushed_sulfide`.

### 7.6 Chemistry

`substances.ron`: `cu, fe, s, acid, lime, salt, n, p, k, c` with `mobility: u8`, `mobility_acid: u8`, `plant_nutrient: bool`, `tox_plant: Option<u8>`, `tox_player: Option<u8>`, `yield_material: Option<String>`.

`pH(cell) = clamp(7 + ((lime as i32 − acid as i32) / 2^ph_shift), 0, 14)`; derived.

**Transport (sweep replaying the MoveLog in order):** for each `(A, B, amount, fill_A)` and each substance `x`: `mob = if pH(A) < acid_ph { mobility_acid } else { mobility }`; `m = ((x_A as u32 · mob as u32 · amount as u32) / 256) / fill_A`; move `min(m, x_A, 255 − x_B)`. Remainders stay. Exact conservation.

**Reactions (Jacobi; sorted apply; `Coat` effects queued as deltas):** `reactions.ron`, table order, first match per cell. Conditions: `Exposed` (an air face neighbor), `Submerged` (a water cell directly above, or the cell is water), `PhBelow(n)`, `PhAbove(n)`, `Evaporating` (a water cell that evaporated this sub-step and has `fill < evap_low_fill`), `Tag(t)`.

```
[
  (id: "oxidize",      consume: [("s", 4)],                 where: [Exposed, Tag("sulfide")],      produce: [("acid", 4)]),
  (id: "neutralize",   consume: [("acid", 2), ("lime", 2)], where: [],                            produce: [("salt", 2)]),
  (id: "dissolve_carbonate", consume: [("acid", 1)],        where: [PhBelow(4), Tag("carbonate")], produce: [("lime", 2)], side: WeatherBoost(16)),
  (id: "ochre",        consume: [("fe", 8)],                where: [Submerged, PhAbove(5)],        side: Coat(Floor, "ochre_crust", "fe", 8)),
  (id: "salt_crust",   consume: [("salt", 16)],             where: [Evaporating],                  side: Coat(Floor, "salt_crust", "salt", 16)),
  (id: "decompose",    consume: [("c", 4)],                 where: [Tag("soil")],                  produce: [("n", 1)], side: EmitGas("chokedamp", 2)),
]
```

- `consume`/`produce` act on the cell's own layers; net change per substance → `react_delta`; produce that cannot fit → `saturation_loss`. A reaction fires once per cell per sub-step when every consumed amount is available.
- `WeatherBoost(n)`: `regolith.saturating_add(n)` on the cell (queued).
- `Coat(Floor, crust, x, amount)`: queues a delta moving `amount` of `x` from the reacting cell into the cell below. Deltas to the same target are additive. When the target is `air`, `porous`, or `stone`-tagged and its coated `x` reaches `eco.crust_threshold`, the target's material becomes the crust (delta). Crusts are diggable; digging leaves the substance on the cell (§7.0).
- `EmitGas(kind, amount)`: to the first air neighbor in sorted order, `min(amount, room)`, ledger `react_in`.
- Organic decomposition is this reaction only.

Limestone is porous (§5), so acid water seeps into it and `dissolve_carbonate` fires there; a channel whose floor and walls are limestone neutralizes the water that passes through it.

### 7.7 Light (sweep, global, dirty-triggered)

Recomputed when any chunk's `revision` changed since the last light pass (a world-level dirty flag); otherwise skipped.

- Column pass, y descending per column: `light = 255` above the world; `light = light.saturating_sub(material.light_attenuation)` entering each cell (§5: air, passable solids and ice 0; water 32; other solids and magma 255).
- Lateral: `eco.light_lateral_iters` (6) global, single-threaded, double-buffered iterations over all cells with `light_attenuation < 255`: `light = max(light, max_neighbor.saturating_sub(eco.light_falloff))`, computed from the previous iteration's array.
- Initialized by worldgen with one full pass.

### 7.8 Plants

`plants.ron` per species: `key`, `material`, `anchor_tags`, `needs { light, moisture, temp, substance_min, substance_max }`, `growth_per_substep`, `spread_chance`, `spread_radius`, `litter`, `lifespan_seasons`, `uptake`, `holds`, `tall`.

**Growth (Jacobi; sorted apply)** per plant cell:

1. Needs: light and temperature from the plant cell; moisture and substances from the anchor. Unmet → `stress += 1`; met → `stress = 0`.
2. Death when `stress ≥ eco.stress_death` or `age_seasons ≥ lifespan`: cell → air; anchor `c += min(litter, room)` with ledger `litter_in` (a source) and `saturation_loss` for the remainder; `held` → anchor layers (`min` with room, remainder `saturation_loss`); plant state removed; ledger `plants_died`.
3. Else `growth = growth.saturating_add(growth_per_substep)`; for each `(x, amt)` in `uptake`: `take = min(amt, anchor_x)`; `anchor_x −= take`; if `holds` then `held[x] += take` (still inside the substance total, no ledger entry) else ledger `uptake_sink[x] += take`.
4. `tall` species with `growth > 192` occupy the cell above as well (same material, no second plant state).

**Harvest:** digging a plant cell yields its material plus, for each substance with `held > 0` and a `yield_material`, `max(1, held / 2^harvest_shift)` units of that yield material; ledger `harvest_out[x] += held[x]`.

**Spread (sweep, global order, after growth):** for each plant cell with `growth == 255` in the spread active set: if `hash(seed, season, substep, cell) & 255 < spread_chance`, plant on the first cell in sorted order within `spread_radius` horizontally and ±1 vertically whose cell is air and whose cell below matches `anchor_tags` and passes needs; ledger `plants_born`. Immediate application prevents double claims.

### 7.9 Activity

Sweeps are bounded by their active sets (§3). Jacobi passes use per-chunk `quiet: [u8; N_JACOBI]` counters: a pass that changes nothing in a chunk increments the counter, any change resets it to 0, any change resets the 6 neighbors; a chunk with `quiet ≥ eco.quiet_skip` is skipped; a season boundary (new `air_temp`) resets temperature counters for chunks containing surface or sky-open cells; edits reset the touched chunk and neighbors. `skipping_changes_no_result_on_constant_fixture` runs a fixture with constant `air_temp`, zero rain and no plants and asserts identical hashes with skipping on and off.

## 8. Worldgen additions

- **Temperature layer** for every chunk: `profile(y) = geothermal_base + (bottom_y − y) · gradient` (integer, 0.1 °C), surface and above at `seasons[0].air_temp`, magma cells at `eco.magma_init_temp`, cores clamped. The bottom clamp in §7.1 is `profile(bottom_y)`, so the linear profile is a steady state for uniform conductivity and deep chunks go quiet.
- **Light layer** by one full light pass.
- **Magma chambers:** 1–3 blobs below `chamber_max_y` with a `magma_source` core, never within 8 cells of the bedrock shell.
- **Limestone band** between stone and granite, noise-perturbed.
- **Sulfide seams** from the ore noise. Coal seams keep `emits: [("firedamp", 2)]`.
- **Water table:** porous cells below `water_table_y(seed, x, z)` at `field_capacity`; caves below it filled with water (fill 255).
- **Surface plants:** moss and grass seeded by hash on anchor cells passing needs at season 0 (light is initialized first).
- Save header gains `tables_hash`.

## 9. Protocol and host

```rust
pub enum EditOp { /* Phase 1 */ SetLayer { layer: LayerId, value: i32 } }
pub enum ChunkDelta { Cells{..}, Full{..}, Layers { pos, eco_revision, changes: Vec<(u16, LayerId, i32)>, plants: Vec<(u16, Option<PlantState>)> } }
pub enum ClientMsg { /* Phase 1 */ AdvanceSeason }
pub enum ServerMsg {
    /* Phase 1 */
    ChunkSnapshot { pos, revision, eco_revision, storage, layers, plants },   // v2
    LayerSnapshot { pos, eco_revision, layers, plants },
    SeasonStarted { season: u32 },
    SeasonDone { season: u32, report: SeasonReport, changed_chunks: u32 },
}
pub struct SeasonReport { ledger: SeasonLedger, between_seasons: SeasonLedger, substep_ms: Vec<u32>, active_cells_per_sweep: Vec<u32>, active_chunks_per_jacobi: Vec<u32> }
```

- `AdvanceSeason` runs `season_tick` on the host thread with edit processing paused; afterwards `ChunkSnapshot` for chunks whose `revision` changed, `LayerSnapshot` for chunks whose only `eco_revision` changed, then `SeasonDone`. Snapshots are postcard+zstd.
- Between seasons, every edit's layer effects travel as `ChunkDelta::Layers` inside the normal `Applied` result.
- `rmc_app --headless --seasons N --seed S --save path [--world-size WxHxD]` runs N seasons with no client and writes the save plus `report.json`. Default world size is 128×64×128.

## 10. Client

- **Liquid pass.** Mesher emits `water` faces into a second `MeshData`: top face at `y + fill/255`, side faces clipped to fill, alpha-blended `StandardMaterial` after opaque. Magma is opaque and full-cell, in the normal pass, with emissive via the Phase 1 alpha channel. Water color shifts by dissolved substances (`eco.tint`).
- **Opaque materials** from the table; plant materials `passable`; the controller's solidity check is `phase == Solid && !passable`.
- **Gas tint.** Camera samples its own cell's gas concentrations each frame; full-screen tint; HUD per-gas readout; lantern icon state.
- **Layer overlay.** `MeshInput.color_source: ColorSource::{Material, Layer(LayerId, Colormap)}`; the editor picks a layer; remesh under the burst budget; `LayerSnapshot` and `ChunkDelta::Layers` trigger remesh only while an overlay is active.
- **Editor eco panel.** Season button with report summary; overlay picker; brush adds `water`, `ice`, `magma`, `crushed_sulfide`, `copper_dust`; targeted-cell readout: temperature, moisture, pH, substances, gases, light, plant species/stage/held.
- **HUD.** Temperature at the player's cell; gas warnings; spoil indicator when the targeted cell exceeds the player toxicity profile.

## 11. Testkit additions

```rust
impl TestApp {
    pub fn advance_season(&mut self); pub fn seasons(&mut self, n: u32);
    pub fn layer_u8(&self, cell, id) -> u8; pub fn temp(&self, cell) -> i16; pub fn ph(&self, cell) -> u8;
    pub fn ledger(&self) -> &SeasonLedger; pub fn host_world(&self) -> &World;
    pub fn set_layer(&mut self, cell, id, value);
    pub fn fixture(&mut self, f: Fixture);     // Slope, Pit, Trench, Swale(contour), Pile(material), SealedCave, Channel(material), Pool, Patch(material, substance, amount)
    pub fn count(&self, tag_or_role: &str, region: Box) -> u32;
    pub fn sum_layer(&self, id, region: Box) -> u64;
    pub fn spoil_cells(&self, region: Box, profile: ToxProfile) -> Vec<CellPos>;
    pub fn set_season_params(&mut self, SeasonOverride { rain: Option<u8>, air_temp: Option<i16> });
    pub fn harvest(&mut self, cell) -> Inventory;   // dig a plant cell and return the yields
}
```

`rmc_eco::testing` exposes `run_pass(pass, &mut World, &EcoTables, seed)`, `invariants::check(&World, &SeasonLedger, &Totals)`, and `reference::flat_sweep_water(...)`, a flat-array implementation of §7.3 used as an oracle.

## 12. Tests

### `rmc_world`
- Layers: default on absent; materialize on set; compact keeps Temperature and Light; chunk/world hash cover layers and plants; save v2 round trip; v1 rejected; `surface_y` maintained across Set/Remove/Full/Layers deltas (proptest against a brute-force scan); `ChunkDelta::Layers` round trip through replica.
- Displacement: every §7.0 case on 3×3×3 fixtures with the invariant checker, through both `World::apply` and `World::eco_set`, including brush Set(water) fill 255 and Set(magma) temperature; `eco_set` emits no delta and bumps `revision`.

### `rmc_eco`
- Temperature: energy non-increasing without sources or clamps (exactly conserved per edge, including edges between unequal capacities); maximum principle; spread non-increasing; linear profile is a fixed point; antisymmetry of flux (proptest); source clamp; surface clamp applies to pond tops and open air above the ground, not to ground solids and not under overhangs; `surface_cell_follows_seasonal_swing` (pond and floor cross 0 °C between winter and summer); deep cell at profile stays quiet for 20 sub-steps; i16 extremes safe; two-chunk equals one-chunk.
- Transitions: each row fires on its condition; order precedence; water↔ice preserves moisture; BoilAll 1:1; worldgen magma molten for 20 sub-steps; stone melts beside a core.
- Water: invariant exact after every sweep (proptest, 300 random worlds); oracle equivalence with `reference::flat_sweep_water` (proptest); pit fills before overflow; sideways halving with cap; receiver never exceeds 255; porous column fills to capacity and stops; seepage into a pit below the table; impermeable blocks; evaporation limited by steam room and ledgered as actual; condensation returns steam below `condense_temp` and the steam above a sky-open pond vents instead; rain lands at `surface_y+1`, into the anchor under a plant, never under an overhang, ledger records actual; magma swaps with air carrying temperature; magma never enters water or porous; contact boiling converts the whole fill, sets obsidian, one event per visit; magma count invariant.
- Gases: invariant per kind including steam's water terms; ceiling and floor pockets; vent only sky-open; emission only when exposed, actual amounts; sealed pocket drains through a shaft within `N` sub-steps; ignition removes exactly the sphere, converts gas to smoke by actual room, charges saturation_loss for displaced layers.
- Weathering: only exposed and moist; freeze-thaw doubles; release proportional to actual `Δ`; `release_complete_within_rounding`; conversion at 255; regolith halves hardness.
- Chemistry: transport oracle and invariant (proptest); `mobility_acid` below `acid_ph`; each reaction fires only on its condition; precedence; ochre on a submerged floor; salt crust on the floor under a drying pool; coat across a chunk border queued as a delta and additive; dissolve_carbonate fires inside porous limestone fed by acid seepage; `react_delta` and `saturation_loss` balance.
- Light: open column 255; overhang 0 beneath; lateral falloff; water attenuates; passable plants transparent; recomputed only when dirty (count calls).
- Plants: needs met grows; stress death with litter as ledgered source; spread only onto passing anchors, seeded, no double claims; fireweed holds copper and the anchor's `cu` falls; grass dies on copper soil; harvest yields `copper_dust` and ledgers `harvest_out`; uptake invariant across life, death and harvest.
- Activity: Jacobi skipping and reactivation; sweep active sets include receivers (a cell that received water is visited next sweep); `skipping_changes_no_result_on_constant_fixture`.
- Whole tick: deterministic (100 seeds); every invariant after each pass on 20 random 32-chunk worlds.

### `rmc_worldgen`
- Temperature profile monotone; surface at `air_temp`; magma at init temp; bottom equals `profile(bottom_y)`; chambers with cores clear of the shell; limestone band present and porous; water table fills porous cells and floods caves; initial light non-zero at the surface; initial plants only on passing anchors.

### `rmc_host` / `rmc_protocol`
- `AdvanceSeason` round trip; snapshot v2 and `LayerSnapshot` round trips; `ChunkDelta::Layers` emitted for `SetLayer` and for brush edits; replica hash equals host hash after 3 seasons with edits between seasons; `--headless --seasons` writes `report.json` satisfying the invariants; `between_seasons` ledger merged.

### End-to-end (`tests/e2e/tests/`)
| Scenario | Asserts |
|---|---|
| `season_swale_fills` | Contour swale on a slope; rain high; 2 seasons; `count(water)` in the swale ≥ `eco_test.swale_min` and greater than a control region on the slope; screenshot shows the pool |
| `pit_becomes_pond` | Pit dug below the water table with rain 0; 1 season; `count(water)` in the pit > 0 |
| `freeze_thaw_weathers_cut` | Trench on a slope so it collects rain; three seasons (autumn, winter, spring) with `air_temp` crossing 0; regolith on the trench wall cells adjacent to the trench water > regolith at an unexposed control cell at the same depth; after N further seasons at least one wall cell has converted to its `weathers_to` material |
| `magma_meets_water` | Pool above a magma pocket; dig through; `count(obsidian) > 0`; `ledger.boil_to_steam > 0`; the pool's fill sum decreased by at least `boil_to_steam` |
| `firedamp_vented_by_shaft` | Sealed cave over a coal seam; 3 seasons; ceiling firedamp ≥ `ignite_min`; dig a shaft to the surface; 1 season; ceiling concentration < 10% of before |
| `tailings_acidify_downslope` | Crushed sulfide pile above a swale with water; 4 seasons; swale pH < 4; swale `cu` > control swale |
| `tailings_kill_moss` | Same with moss seeded in both swales; moss count in the polluted swale < control |
| `limestone_channel_clears` | Acidify scenario plus a limestone-floored channel between pile and swale; swale pH ≥ 6; channel salt > 0 |
| `fireweed_takes_copper` | Copper-rich soil patch; fireweed and grass planted; 3 seasons; anchor `cu` under fireweed decreased; grass dead; `harvest(fireweed)` yields `copper_dust` |
| `grass_greens_swale` | The swale after 6 seasons: grass in and around the swale > outside; screenshot green where wet |
| `layer_overlay_renders` | Moisture overlay; screenshot differs from material render; mean blue over the pool region higher |
| `soak_seasons` | ChaosBot 600 frames then 20 seasons, three seeds; no panic; all invariants per season; host hash == replica hash; sub-step time stabilizes by season 4 |

## 13. Performance targets

Release build, default 32-chunk world (1 M cells), the 4090 box. CI checks only the dev-profile budget.

| Metric | Target |
|---|---|
| Season tick, quiescent world (rain 0, constant `air_temp`) | < 1 s |
| Season tick, first season after worldgen with rain | < 5 s |
| Season tick, steady rain, 50 edits in one chunk | < 3 s |
| Snapshots + remesh after a season | < 1.5 s to no dirty chunks |
| Layer memory after 20 seasons | < 48 MB |
| Dev-profile budget, one quiet season (`quiet_world_tick_under_dev_budget`) | < `eco_test.quiet_tick_dev_ms` (60000) |

Estimate for the first season: about 60–100k active water cells × 64 sweeps ≈ 5 M visits; MoveLog in the low millions; gas sweeps over active cells only. The 256-chunk world is 8× larger and is deferred to Phase 3 with parallel sweeps.

Benches: `tick_quiet`, `tick_first`, `water_sweep_world`, `chem_transport_world`. Results in `docs/perf-p2.md`.

## 14. Build rules

`CLAUDE.md` rules 14–20. Rule 2 is extended for Phase 2: during `season_tick`, cells change only via `World::eco_set`.

## 15. Milestones and features

| # | Milestone | Features | Acceptance |
|---|---|---|---|
| E0 | Layers, columns, deltas, tables, roles, ledger, invariant checker, runners, active sets, temperature (incl. worldgen layer), save v2, protocol v2, headless, testkit | F100–F109 | temperature tests; replica equivalence over seasons with edits |
| E2 | Water, ice, rain, evaporation, condensation, seepage, steam layer, worldgen water table, liquid mesh pass | F120–F129, F150 | `season_swale_fills`, `pit_becomes_pond` |
| E1 | Weathering, freeze-thaw, gradual release, regolith hardness, yield_material | F110–F114 | `freeze_thaw_weathers_cut` |
| E2b | Magma, obsidian, basalt, chambers, advection | F130–F134 | `magma_meets_water` |
| E3 | Substances, transport, reactions, crusts, crushed tailings, spoil query, HUD | F140–F148 | `tailings_acidify_downslope`, `limestone_channel_clears` |
| E3b | Gas drift/vent/emission/ignition, gas tint | F151–F156 | `firedamp_vented_by_shaft` |
| E4 | Light, layer overlay | F160–F162 | light tests; `layer_overlay_renders` |
| E5 | Plants, held substances, harvest, worldgen plants, editor eco panel, perf, soak, report | F165–F175 | `grass_greens_swale`, `fireweed_takes_copper`, `tailings_kill_moss`, `soak_seasons` |

## 16. Open questions (defaults in force)

| Question | Default |
|---|---|
| Wet-soil look | Overlay only |
| Rain spatial pattern | Seeded hash per column |
| Consumed (non-held) nutrients | Sink until decomposers (Phase 3) |
| Player harm from gas | HUD only |
| 256-chunk world performance | Phase 3 |

## 17. Extension points

| Later | Attaches at |
|---|---|
| Wall-clock seasons | Host timer calling `season_tick` with edits paused per sub-step |
| Parallel sweeps | 8-color chunk scheduling; accepted only if `whole_tick_deterministic` hashes match the sequential result |
| Parallel light | The lateral light step is double-buffered, so a parallel per-chunk form with a border exchange between global iterations produces identical hashes. This is the Phase 3 scaling path for light |
| Decomposers, animals | New passes after plants; agents issue `EditCommand`s and read layers |
| Erosion and deposition | `erodibility` loaded; a sweep after water moving regolith with flow |
| Fire | A `burning` layer and `Contact(role: Fire)`; smoke exists |
| Steam power | Steam concentration in sealed volumes as pressure; boiler as an emitter |
| Lantern ignition, gas harm | Player state on the host |
| Members and councils (Phase 3) | Stocks are `query::watershed_sums` over layers; proxies are clients |
