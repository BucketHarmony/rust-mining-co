//! Gases (SAD-P2 §7.4, sweep per kind): concentration layers on air cells.
//!
//! Each row of `gases.ron` is one layer, `LayerId::Gas(i)`, with its own
//! active set (`SWEEP_GAS + i`) and its own sweep per sub-step
//! ([`GasPass::all`]). A visit to an air cell holding the gas applies, in
//! order:
//!
//! 1. **drift**: a gas lighter than air (negative `density`) moves
//!    `min(c, |density| · 2^drift_shift, room_above)` into the air cell
//!    above; a heavier one (positive) into the air cell below; a gas of
//!    density 0 does not drift;
//! 2. **diffusion**: to each of the 6 face neighbors in sorted (y, z, x)
//!    order, below first, that is air and holds less (`c_n < c`):
//!    `min((c − c_n) · diffusion / 2^diff_shift, room_n, c)`.
//!
//! `c` is the cell's concentration as read just before each move, so a
//! move sees the earlier ones; `room_n` is `255 − c_n` of the receiver,
//! read just before the move. Receivers join the active set. Gas only
//! moves between air cells (the one `Phase::Air` material): solids,
//! liquids, the bounds and non-resident chunks block it. Moves stay inside
//! the gas's total, so they book no ledger term; the invariant checker is
//! the proof.
//!
//! 3. **vent**: a sky-open cell (`y > surface_y` of its column) loses
//!    what it still holds; ledger `vent_out`, and for the steam role also
//!    the water total's `steam_vent_out` (SAD-P2 §7: steam's `vent_out`
//!    equals `steam_vent_out`).
//!
//! 5. **ignition** (flammable gases only): a cell still holding
//!    `c ≥ ignite_min` with a face neighbor that is a `heat_source`
//!    material or the `magma` role burns. The gas goes (`react_out`), the
//!    `smoke` role on the cell gains `min(c, room)` (`react_in` books the
//!    actual), and every in-bounds, resident, `diggable` cell of
//!    `Sphere { center: cell, radius: (c / 64) · ignite_radius_per_64 }`
//!    becomes air through `World::eco_set` in the shape's (y, z, x) order,
//!    so the §7.0 rules handle the displaced layers. No yields. Ledger
//!    `ignitions`.
//!
//! [`Emission`] (step 4) runs once after every gas's sweep.
//! [`reference::flat_sweep_gas`] is the flat-array oracle of steps 1 to 3.
//!
//! [`reference::flat_sweep_gas`]: crate::reference::flat_sweep_gas

use rmc_world::chunk::ChunkStorage;
use rmc_world::coords::{CHUNK_VOLUME, CellPos, LocalPos};
use rmc_world::layers::{LayerId, SWEEP_GAS};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;
use rmc_world::world::World;

use crate::reference::order_key;

use crate::runner::{SweepCtx, SweepPass};
use crate::tables::EcoTables;

/// The sweep of one gas kind.
#[derive(Clone, Debug)]
pub struct GasPass {
    /// The gas's layer index (its row in `gases.ron`).
    gas: u8,
    /// Where drift goes and how much it carries per visit
    /// (`|density| · 2^drift_shift`); `None` for density 0.
    drift: Option<([i32; 3], i32)>,
    /// `diffusion` of the gas's row.
    diffusion: u32,
    /// `eco.diff_shift`.
    diff_shift: u8,
    /// Whether the gas is the `steam` role, whose vent is also the water
    /// total's `steam_vent_out`.
    steam: bool,
    /// Per material id: whether it is `Phase::Air`.
    air: Vec<bool>,
    /// Step 5, for a flammable gas whose tables name an `air` role.
    ignite: Option<Ignite>,
}

/// How a flammable gas ignites (SAD-P2 §7.4 step 5).
#[derive(Clone, Debug)]
struct Ignite {
    /// `ignite_min` of the gas's row.
    min: i32,
    /// `ignite_radius_per_64` of the gas's row.
    radius_per_64: i32,
    /// The `smoke` role's layer index, if the tables name one.
    smoke: Option<u8>,
    /// The `air` role: what the sphere's cells become.
    air: MaterialId,
    /// Per material id: a `heat_source` material or the `magma` role.
    hot: Vec<bool>,
    /// Per material id: `diggable`.
    diggable: Vec<bool>,
}

/// The concentration per `ignite_radius_per_64` step of the blast radius
/// (`(c / 64) · ignite_radius_per_64`, SAD-P2 §7.4).
const IGNITE_UNIT: i32 = 64;

/// The most gas an air cell holds.
const FULL: i32 = u8::MAX as i32;
const ABOVE: [i32; 3] = [0, 1, 0];
const BELOW: [i32; 3] = [0, -1, 0];
/// The 6 face neighbors in sorted (y, z, x) order: below first.
const SIX: [[i32; 3]; 6] = [BELOW, [0, 0, -1], [-1, 0, 0], [1, 0, 0], [0, 0, 1], ABOVE];

fn offset(cell: CellPos, d: [i32; 3]) -> CellPos {
    CellPos::new(cell.0.x + d[0], cell.0.y + d[1], cell.0.z + d[2])
}

impl GasPass {
    /// The sweep of gas row `gas`. Panics if the tables have no such row.
    pub fn new(tables: &EcoTables, gas: u8) -> Self {
        let def = &tables.gases()[usize::from(gas)];
        let params = tables.params();
        let rate = (u32::from(def.density.unsigned_abs()) << params.drift_shift) as i32;
        let magma = tables.role_material("magma");
        let ignite = match (def.flammable, tables.role_material("air")) {
            (true, Some(air)) => Some(Ignite {
                min: i32::from(def.ignite_min),
                radius_per_64: i32::from(def.ignite_radius_per_64),
                smoke: tables.role_gas("smoke"),
                air,
                hot: tables
                    .materials()
                    .iter()
                    .map(|d| d.heat_source.is_some() || Some(d.id) == magma)
                    .collect(),
                diggable: tables.materials().iter().map(|d| d.diggable).collect(),
            }),
            _ => None,
        };
        let drift = match def.density.signum() {
            0 => None,
            s if s < 0 => Some((ABOVE, rate)),
            _ => Some((BELOW, rate)),
        };
        Self {
            gas,
            drift,
            diffusion: u32::from(def.diffusion),
            diff_shift: params.diff_shift,
            steam: tables.role_gas("steam") == Some(gas),
            air: tables
                .materials()
                .iter()
                .map(|d| d.phase == Phase::Air)
                .collect(),
            ignite,
        }
    }

    /// One sweep per gas row, in row order (the order they run in a
    /// sub-step).
    pub fn all(tables: &EcoTables) -> Vec<Self> {
        (0..tables.gases().len())
            .map(|g| Self::new(tables, g as u8))
            .collect()
    }

    /// The gas's layer index.
    pub fn gas(&self) -> u8 {
        self.gas
    }

    fn layer(&self) -> LayerId {
        LayerId::Gas(self.gas)
    }

    /// Whether `cell` is an air cell (in bounds and resident).
    fn is_air(&self, world: &World, cell: CellPos) -> bool {
        world
            .get_cell(cell)
            .is_some_and(|m| self.air.get(usize::from(m.0)).copied().unwrap_or(false))
    }

    fn conc(&self, world: &World, cell: CellPos) -> i32 {
        world.layer(cell, self.layer())
    }

    /// Moves `amount` from `from` (holding `c`) to the air cell `to`
    /// (holding `c_to`; the caller checked its room) and returns the
    /// donor's new concentration. Nothing happens for an amount of 0.
    fn give(
        &self,
        ctx: &mut SweepCtx<'_>,
        (from, c): (CellPos, i32),
        (to, c_to): (CellPos, i32),
        amount: i32,
    ) -> i32 {
        if amount <= 0 {
            return c;
        }
        debug_assert!(amount <= c && amount <= FULL - c_to);
        ctx.world.eco_put_layer(from, self.layer(), c - amount);
        ctx.world.eco_put_layer(to, self.layer(), c_to + amount);
        ctx.receive(to);
        c - amount
    }

    /// Step 5 at `cell`, which holds `c` of the gas: burns it if it is at
    /// least `ignite_min` and a face neighbor is hot. Returns whether it
    /// ignited.
    fn ignite(&self, ctx: &mut SweepCtx<'_>, cell: CellPos, c: i32) -> bool {
        let Some(ig) = &self.ignite else {
            return false;
        };
        if c == 0 || c < ig.min {
            return false;
        }
        let flag = |v: &[bool], m: MaterialId| v.get(usize::from(m.0)).copied().unwrap_or(false);
        let hot = SIX.iter().any(|&d| {
            ctx.world
                .get_cell(offset(cell, d))
                .is_some_and(|m| flag(&ig.hot, m))
        });
        if !hot {
            return false;
        }
        ctx.world.eco_put_layer(cell, self.layer(), 0);
        ctx.ledger.react_out[usize::from(self.gas)] += i64::from(c);
        if let Some(smoke) = ig.smoke {
            let layer = LayerId::Gas(smoke);
            let have = ctx.world.layer(cell, layer);
            let add = c.min(FULL - have);
            if add > 0 {
                // The cell joins the smoke sweep (`eco_put_layer`).
                ctx.world.eco_put_layer(cell, layer, have + add);
                ctx.ledger.react_in[usize::from(smoke)] += i64::from(add);
            }
        }
        let radius = (c / IGNITE_UNIT) * ig.radius_per_64;
        let sphere = Shape::Sphere {
            center: cell,
            radius: u16::try_from(radius).unwrap_or(u16::MAX),
        };
        let bounds = ctx.world.bounds();
        for n in sphere.cells().filter(|&n| bounds.contains(n)) {
            // `get_cell` is `None` for a non-resident chunk.
            if ctx.world.get_cell(n).is_some_and(|m| flag(&ig.diggable, m)) {
                ctx.world.eco_set(n, ig.air, ctx.ledger);
            }
        }
        ctx.ledger.ignitions += 1;
        true
    }
}

impl SweepPass for GasPass {
    fn kind(&self) -> usize {
        SWEEP_GAS + usize::from(self.gas)
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        if !self.is_air(ctx.world, cell) {
            return;
        }
        // The cell's concentration, kept current through its moves: only
        // they change it during the visit.
        let mut c = self.conc(ctx.world, cell);
        // 1. Drift into the air cell above (light) or below (heavy).
        if let Some((d, rate)) = self.drift {
            let n = offset(cell, d);
            if c > 0 && self.is_air(ctx.world, n) {
                let c_n = self.conc(ctx.world, n);
                let amount = c.min(rate).min(FULL - c_n);
                c = self.give(ctx, (cell, c), (n, c_n), amount);
            }
        }
        // 2. Diffusion into each air neighbor that holds less, in sorted
        // order, within its room and the cell's content.
        for d in SIX {
            let n = offset(cell, d);
            if c == 0 || !self.is_air(ctx.world, n) {
                continue;
            }
            let c_n = self.conc(ctx.world, n);
            if c_n >= c {
                continue;
            }
            let share = ((c - c_n) as u32 * self.diffusion) >> self.diff_shift;
            let amount = (share as i32).min(FULL - c_n).min(c);
            c = self.give(ctx, (cell, c), (n, c_n), amount);
        }
        // 3. Vent what is left of a sky-open cell's gas.
        if c > 0 && cell.0.y > i32::from(ctx.world.surface_y(cell.0.x, cell.0.z)) {
            ctx.world.eco_put_layer(cell, self.layer(), 0);
            let vented = i64::from(c);
            ctx.ledger.vent_out[usize::from(self.gas)] += vented;
            if self.steam {
                ctx.ledger.steam_vent_out += vented;
            }
            c = 0;
        }
        // 5. Ignition of what is left next to heat.
        self.ignite(ctx, cell, c);
    }

    /// Air cells holding the gas. Other cells never hold gas: the
    /// displacement rules (§7.0) push it out of a cell that stops being
    /// air.
    fn holds(&self, world: &World, cell: CellPos) -> bool {
        self.is_air(world, cell) && self.conc(world, cell) > 0
    }
}

/// Step 4, emission (SAD-P2 §7.4), once per sub-step after every gas's
/// sweep: each cell of a material with `emits` that has an air face
/// neighbor adds, per `(gas, amount)` row, `min(amount, room)` to its first
/// air neighbor in sorted (y, z, x) order, below first; ledger `emit_in`
/// records the actual amount. Emitters are visited in the global order,
/// so two that share an air cell fill it in that order. The receiver joins
/// the gas's sweep.
#[derive(Clone, Debug)]
pub struct Emission {
    /// Per material id: its `emits` rows as `(gas layer index, amount)`.
    emits: Vec<Vec<(u8, i32)>>,
    /// Per material id: whether it is `Phase::Air`.
    air: Vec<bool>,
}

impl Emission {
    pub fn new(tables: &EcoTables) -> Self {
        let materials = tables.materials();
        Self {
            emits: materials
                .iter()
                .map(|d| {
                    d.emits
                        .iter()
                        .filter_map(|(key, amount)| {
                            Some((tables.gas_index(key)?, i32::from(*amount)))
                        })
                        .collect()
                })
                .collect(),
            air: materials.iter().map(|d| d.phase == Phase::Air).collect(),
        }
    }

    fn emits(&self, m: MaterialId) -> &[(u8, i32)] {
        self.emits.get(usize::from(m.0)).map_or(&[], Vec::as_slice)
    }

    fn is_air(&self, world: &World, cell: CellPos) -> bool {
        world
            .get_cell(cell)
            .is_some_and(|m| self.air.get(usize::from(m.0)).copied().unwrap_or(false))
    }

    /// Emits from every exposed emitter of `world`; returns how many
    /// emitters added gas.
    pub fn run(&self, world: &mut World, ledger: &mut SeasonLedger) -> u32 {
        let bounds = world.bounds();
        let mut cells = Vec::new();
        for (pos, chunk) in world.iter_chunks_sorted() {
            // The palette lists every material the chunk may hold.
            let any = match &chunk.storage {
                ChunkStorage::Uniform(m) => !self.emits(*m).is_empty(),
                ChunkStorage::Paletted { palette, .. } => {
                    palette.iter().any(|&m| !self.emits(m).is_empty())
                }
            };
            if !any {
                continue;
            }
            cells.extend(
                (0..CHUNK_VOLUME as u16)
                    .map(LocalPos::from_index)
                    .filter(|&l| !self.emits(chunk.storage.get(l)).is_empty())
                    .map(|l| CellPos::from_chunk_local(pos, l))
                    .filter(|&c| bounds.contains(c)),
            );
        }
        cells.sort_by_key(|&c| order_key(c));
        let mut emitted = 0;
        for cell in cells {
            // Emission changes no material, so the cell still emits.
            let Some(m) = world.get_cell(cell) else {
                continue;
            };
            let Some(n) = SIX
                .iter()
                .map(|&d| offset(cell, d))
                .find(|&n| self.is_air(world, n))
            else {
                continue;
            };
            let mut any = false;
            for &(g, amount) in self.emits(m) {
                let layer = LayerId::Gas(g);
                let c_n = world.layer(n, layer);
                let add = amount.min(FULL - c_n);
                if add > 0 {
                    // The receiver joins the gas's sweep (`eco_put_layer`).
                    world.eco_put_layer(n, layer, c_n + add);
                    ledger.emit_in[usize::from(g)] += i64::from(add);
                    any = true;
                }
            }
            emitted += u32::from(any);
        }
        emitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Arc;

    use proptest::prelude::*;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos};
    use rmc_world::materials::MaterialId;

    use crate::invariants::InvariantChecker;
    use crate::reference::{FlatWorld, GasRef, flat_sweep_gas, global_order};
    use crate::runner::{activate_holders, run_sweep};
    use crate::tables::EcoSources;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    /// The shipped tables with `gases.ron` replaced by `gases`.
    fn tables_with_gases(gases: &str) -> EcoTables {
        let shipped = tables();
        let mut src = EcoSources::read(ASSETS).unwrap();
        src.gases = gases.to_string();
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src).expect("test gases load")
    }

    /// Test gases with one behavior each (the role keys stay valid):
    /// `steam` only drifts up (1 · 2^drift_shift = 4), `chokedamp` only
    /// drifts down (2 · 4 = 8), `firedamp` only diffuses, with a
    /// diffusion of 255 > 2^diff_shift, so that the room and content caps
    /// bind, and `smoke` does nothing.
    const MOVES: &str = r#"[
        (key: "steam", density: -1, diffusion: 0, flammable: false,
         ignite_min: 0, ignite_radius_per_64: 0, visible: true),
        (key: "firedamp", density: 0, diffusion: 255, flammable: true,
         ignite_min: 64, ignite_radius_per_64: 2, visible: false),
        (key: "chokedamp", density: 2, diffusion: 0, flammable: false,
         ignite_min: 0, ignite_radius_per_64: 0, visible: false),
        (key: "smoke", density: 0, diffusion: 0, flammable: false,
         ignite_min: 0, ignite_radius_per_64: 0, visible: true),
    ]"#;

    fn id(t: &EcoTables, key: &str) -> MaterialId {
        t.materials().id(key).unwrap()
    }

    /// Stone over `bounds` with `air` cells carved out, chunks inserted in
    /// reverse sorted order.
    fn carved(t: &EcoTables, bounds: CellAabb, air: impl Fn(CellPos) -> bool) -> World {
        let air_id = t.role_material("air").unwrap();
        built(t, bounds, |c| air(c).then_some(air_id))
    }

    /// Stone over `bounds` with the cells `mat` names set to its material,
    /// chunks inserted in reverse sorted order.
    fn built(
        t: &EcoTables,
        bounds: CellAabb,
        mat: impl Fn(CellPos) -> Option<MaterialId>,
    ) -> World {
        let stone = id(t, "stone");
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut chunks: BTreeMap<ChunkPos, Chunk> = bounds
            .chunks()
            .map(|p| (p, Chunk::uniform(stone)))
            .collect();
        for cell in global_order(bounds) {
            if let Some(m) = mat(cell) {
                let (pos, local) = cell.split();
                chunks.get_mut(&pos).unwrap().storage.set(local, m);
            }
        }
        for (pos, chunk) in chunks.into_iter().rev() {
            w.insert_chunk(pos, chunk).unwrap();
        }
        w
    }

    /// Two chunks along x.
    fn two_chunks() -> CellAabb {
        CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31))
    }

    /// A sealed air chamber, 8 × 8 × 6 cells (x × y × z), across the
    /// chunk face at x = 32.
    const CHAMBER_X: (i32, i32) = (28, 35);
    const CHAMBER_Y: (i32, i32) = (8, 15);
    const CHAMBER_Z: (i32, i32) = (12, 17);

    fn in_chamber(c: CellPos) -> bool {
        let c = c.0;
        (CHAMBER_X.0..=CHAMBER_X.1).contains(&c.x)
            && (CHAMBER_Y.0..=CHAMBER_Y.1).contains(&c.y)
            && (CHAMBER_Z.0..=CHAMBER_Z.1).contains(&c.z)
    }

    fn chamber_cells() -> Vec<CellPos> {
        global_order(two_chunks())
            .filter(|&c| in_chamber(c))
            .collect()
    }

    fn active(w: &World, cell: CellPos, pass: &GasPass) -> bool {
        let (pos, local) = cell.split();
        w.chunk(pos).unwrap().active[pass.kind()].contains(local)
    }

    /// Gas totals per chamber y-layer, bottom first.
    fn layer_sums(w: &World, gas: u8) -> Vec<i64> {
        (CHAMBER_Y.0..=CHAMBER_Y.1)
            .map(|y| {
                chamber_cells()
                    .into_iter()
                    .filter(|c| c.0.y == y)
                    .map(|c| i64::from(w.layer(c, LayerId::Gas(gas))))
                    .sum()
            })
            .collect()
    }

    /// Runs `sweeps` sweeps of the gas `key` from a uniform concentration
    /// of `start` in the chamber, checking conservation and that no gas
    /// leaves the chamber after every sweep; returns the layer sums.
    fn settle(t: &EcoTables, key: &str, start: i32, sweeps: usize) -> Vec<i64> {
        let g = t.gas_index(key).unwrap();
        let mut w = carved(t, two_chunks(), in_chamber);
        for c in chamber_cells() {
            w.eco_put_layer(c, LayerId::Gas(g), start);
        }
        let mut pass = GasPass::new(t, g);
        for c in chamber_cells() {
            assert!(active(&w, c, &pass), "gas joins its sweep");
        }
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let total = i64::from(start) * chamber_cells().len() as i64;
        for sweep in 0..sweeps {
            run_sweep(&mut w, &mut pass, &mut ledger);
            checker
                .check(&w, &ledger)
                .unwrap_or_else(|v| panic!("sweep {sweep}: {v}"));
            assert_eq!(layer_sums(&w, g).iter().sum::<i64>(), total);
        }
        assert_eq!(ledger, SeasonLedger::default(), "moves book nothing");
        for c in chamber_cells() {
            assert_eq!(
                active(&w, c, &pass),
                pass.holds(&w, c),
                "active set at {c:?}"
            );
        }
        layer_sums(&w, g)
    }

    #[test]
    fn light_gas_collects_at_ceiling() {
        let t = tables();
        for key in ["firedamp", "steam", "smoke"] {
            let g = &t.gases()[usize::from(t.gas_index(key).unwrap())];
            assert!(g.density < 0, "{key} is lighter than air");
            let sums = settle(&t, key, 40, 64);
            let mean = sums.iter().sum::<i64>() / sums.len() as i64;
            let (bottom, top) = (sums[0], *sums.last().unwrap());
            // Rising beats diffusing back down: the concentration grows
            // with height, the ceiling holds the most and the floor the
            // least. The slowest shipped light gas (firedamp: drift 4,
            // diffusion 24 / 64) settles at about 7 more per cell per
            // layer, so the 8 layers span 16 to 61 around a mean of 40.
            assert!(
                sums.windows(2).all(|p| p[0] <= p[1]),
                "{key}: layer sums {sums:?} not non-decreasing upward"
            );
            assert!(top >= 3 * bottom, "{key}: {sums:?}");
            assert!(top > mean * 5 / 4, "{key}: ceiling {top}, mean {mean}");
            assert!(bottom < mean * 3 / 4, "{key}: floor {bottom}, mean {mean}");
        }
    }

    #[test]
    fn heavy_gas_collects_at_floor() {
        let t = tables();
        let key = "chokedamp";
        let g = &t.gases()[usize::from(t.gas_index(key).unwrap())];
        assert!(g.density > 0, "{key} is heavier than air");
        let sums = settle(&t, key, 40, 64);
        let mean = sums.iter().sum::<i64>() / sums.len() as i64;
        let (bottom, top) = (sums[0], *sums.last().unwrap());
        assert!(
            sums.windows(2).all(|p| p[0] >= p[1]),
            "layer sums {sums:?} not non-increasing upward"
        );
        assert!(bottom > 2 * mean, "floor {bottom}, mean {mean}");
        assert!(top < mean / 2, "ceiling {top}, mean {mean}");
    }

    /// One-chunk world for the exact move checks: stone with the given
    /// air cells, each holding `(gas key, concentration)`.
    fn pockets(t: &EcoTables, cells: &[(CellPos, &str, i32)]) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut w = carved(t, bounds, |c| cells.iter().any(|&(p, _, _)| p == c));
        for &(c, key, v) in cells {
            w.eco_put_layer(c, LayerId::Gas(t.gas_index(key).unwrap()), v);
        }
        w
    }

    fn sweep_all(w: &mut World, passes: &mut [GasPass], ledger: &mut SeasonLedger) {
        for p in passes.iter_mut() {
            run_sweep(w, p, ledger);
        }
    }

    #[test]
    fn drift_and_diffusion_conserve_with_room_checks() {
        let t = tables_with_gases(MOVES);
        let p = |x, y, z| CellPos::new(x, y, z);
        let conc =
            |w: &World, key: &str, c: CellPos| w.layer(c, LayerId::Gas(t.gas_index(key).unwrap()));
        // Drift up (4 per visit) in two-cell shafts, stone above: the top
        // cell is visited after the bottom one and can go no further.
        // Drift down (8) in two-cell shafts: the bottom cell comes first in
        // order, so it is visited in the next sweep. Diffusion (255 / 64 of
        // the difference) in two-cell trenches along x, into the cell at
        // x − 1, which comes first in order.
        let cells = [
            // Up: plain, room 2, no room, less than the rate.
            (p(2, 4, 2), "steam", 100),
            (p(2, 5, 2), "steam", 0),
            (p(4, 4, 2), "steam", 100),
            (p(4, 5, 2), "steam", 253),
            (p(6, 4, 2), "steam", 100),
            (p(6, 5, 2), "steam", 255),
            (p(8, 4, 2), "steam", 3),
            (p(8, 5, 2), "steam", 0),
            // Down: room 5; a full cell under a sealed ceiling.
            (p(2, 4, 8), "chokedamp", 0),
            (p(2, 5, 8), "chokedamp", 100),
            (p(4, 4, 8), "chokedamp", 250),
            (p(4, 5, 8), "chokedamp", 100),
            // Diffusion: room binds (20 · 255 / 64 = 79 > 75); content
            // binds (100 · 255 / 64 = 398 > 100); equal: nothing.
            (p(2, 4, 14), "firedamp", 180),
            (p(3, 4, 14), "firedamp", 200),
            (p(2, 4, 18), "firedamp", 0),
            (p(3, 4, 18), "firedamp", 100),
            (p(2, 4, 22), "firedamp", 50),
            (p(3, 4, 22), "firedamp", 50),
            // Smoke neither drifts nor diffuses here.
            (p(10, 4, 2), "smoke", 90),
            (p(10, 5, 2), "smoke", 0),
        ];
        let mut w = pockets(&t, &cells);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut passes = GasPass::all(&t);
        assert_eq!(passes.len(), t.gases().len());

        sweep_all(&mut w, &mut passes, &mut ledger);

        let pair =
            |w: &World, key: &str, a: CellPos, b: CellPos| (conc(w, key, a), conc(w, key, b));
        assert_eq!(pair(&w, "steam", p(2, 4, 2), p(2, 5, 2)), (96, 4));
        assert_eq!(pair(&w, "steam", p(4, 4, 2), p(4, 5, 2)), (98, 255));
        assert_eq!(pair(&w, "steam", p(6, 4, 2), p(6, 5, 2)), (100, 255));
        assert_eq!(pair(&w, "steam", p(8, 4, 2), p(8, 5, 2)), (0, 3));
        assert_eq!(pair(&w, "chokedamp", p(2, 4, 8), p(2, 5, 8)), (8, 92));
        assert_eq!(pair(&w, "chokedamp", p(4, 4, 8), p(4, 5, 8)), (255, 95));
        assert_eq!(pair(&w, "firedamp", p(2, 4, 14), p(3, 4, 14)), (255, 125));
        assert_eq!(pair(&w, "firedamp", p(2, 4, 18), p(3, 4, 18)), (100, 0));
        assert_eq!(pair(&w, "firedamp", p(2, 4, 22), p(3, 4, 22)), (50, 50));
        assert_eq!(pair(&w, "smoke", p(10, 4, 2), p(10, 5, 2)), (90, 0));
        // Gas never enters stone.
        for (c, key, _) in cells {
            for d in SIX {
                let n = offset(c, d);
                if w.get_cell(n) == Some(id(&t, "stone")) {
                    assert_eq!(conc(&w, key, n), 0, "{key} in stone at {n:?}");
                }
            }
        }
        let steam = GasPass::new(&t, t.gas_index("steam").unwrap());
        assert!(!active(&w, p(8, 4, 2), &steam), "an emptied cell leaves");
        assert!(active(&w, p(8, 5, 2), &steam), "a receiver joins");
        checker.check(&w, &ledger).expect("gases conserved");

        // Many sweeps: every shaft ends with all of its gas at its end,
        // within the room; nothing is created or lost.
        for _ in 0..64 {
            sweep_all(&mut w, &mut passes, &mut ledger);
            checker.check(&w, &ledger).expect("gases conserved");
        }
        assert_eq!(pair(&w, "steam", p(2, 4, 2), p(2, 5, 2)), (0, 100));
        assert_eq!(pair(&w, "steam", p(4, 4, 2), p(4, 5, 2)), (98, 255));
        assert_eq!(pair(&w, "chokedamp", p(2, 4, 8), p(2, 5, 8)), (100, 0));
        assert_eq!(pair(&w, "chokedamp", p(4, 4, 8), p(4, 5, 8)), (255, 95));
        assert_eq!(ledger, SeasonLedger::default(), "moves book nothing");

        // The shipped gases on random worlds: conserved after every sweep
        // of every gas, only on air cells, every holder active, and equal
        // to the flat reference that visits every cell.
        let t = tables();
        for seed in 0..32 {
            check_random(&t, seed, 4).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        }
        let t = tables_with_gases(MOVES);
        for seed in 32..48 {
            check_random(&t, seed, 4).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        }
    }

    fn cases() -> u32 {
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(256)
    }

    /// SplitMix64.
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A random 8 × 8 × 8 world around a chunk corner (eight chunks,
    /// negative coordinates included): a stone floor, then stone, water
    /// and air (with `coal`, some of the stone is that material instead);
    /// air cells hold each gas at a random concentration (often 0,
    /// sometimes 255). Gas holders are active. Columns whose cells above
    /// some air are all air are sky-open there.
    fn random_world(t: &EcoTables, seed: u64, coal: Option<MaterialId>) -> World {
        let (stone, water, air) = (
            id(t, "stone"),
            t.role_material("water").unwrap(),
            t.role_material("air").unwrap(),
        );
        let bounds = CellAabb::new(CellPos::new(-4, -4, -4), CellPos::new(3, 3, 3));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut chunks: BTreeMap<ChunkPos, Chunk> =
            bounds.chunks().map(|p| (p, Chunk::uniform(air))).collect();
        let mut s = seed;
        let mut gas = Vec::new();
        for cell in global_order(bounds) {
            let r = next(&mut s);
            let m = match r % 8 {
                _ if cell.0.y == bounds.min.0.y => stone,
                0 => stone,
                1 => coal.unwrap_or(stone),
                2 => water,
                _ => air,
            };
            let (pos, local) = cell.split();
            chunks.get_mut(&pos).unwrap().storage.set(local, m);
            if m == air {
                for g in 0..t.gases().len() as u8 {
                    let r = next(&mut s);
                    let v = match r % 4 {
                        0 => 0,
                        1 => 255,
                        _ => ((r >> 8) % 256) as i32,
                    };
                    gas.push((cell, g, v));
                }
            }
        }
        for (pos, chunk) in chunks {
            w.insert_chunk(pos, chunk).unwrap();
        }
        for (cell, g, v) in gas {
            w.eco_put_layer(cell, LayerId::Gas(g), v);
        }
        for p in GasPass::all(t) {
            activate_holders(&mut w, &p);
        }
        w
    }

    /// The reference's view of gas `g`.
    fn gas_ref(t: &EcoTables, g: u8) -> GasRef {
        let def = &t.gases()[usize::from(g)];
        GasRef {
            air: t.role_material("air").unwrap(),
            gas: g,
            density: i32::from(def.density),
            diffusion: i32::from(def.diffusion),
            drift_shift: t.params().drift_shift,
            diff_shift: t.params().diff_shift,
            surface: t.materials().iter().map(|d| d.forms_surface()).collect(),
        }
    }

    /// Books `vented` units of gas `g` as the vent step does: steam's vent
    /// is also the water total's `steam_vent_out` (SAD-P2 §7).
    fn book_vent(t: &EcoTables, ledger: &mut SeasonLedger, g: u8, vented: i64) {
        ledger.vent_out[usize::from(g)] += vented;
        if t.role_gas("steam") == Some(g) {
            ledger.steam_vent_out += vented;
        }
    }

    /// `rounds` rounds of every gas sweep on a random world: after each
    /// sweep, conservation, gas only on air, holders active, the flat
    /// reference equal, and the ledger holding only the reference's vents.
    fn check_random(t: &EcoTables, seed: u64, rounds: usize) -> Result<(), String> {
        let mut w = random_world(t, seed, None);
        let ids: Vec<LayerId> = (0..t.gases().len() as u8).map(LayerId::Gas).collect();
        let mut flat = FlatWorld::from_world(&w, &ids);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut want = SeasonLedger::default();
        let mut passes = GasPass::all(t);
        for round in 0..rounds {
            for pass in &mut passes {
                let g = pass.gas();
                run_sweep(&mut w, pass, &mut ledger);
                let vented = flat_sweep_gas(&mut flat, &gas_ref(t, g));
                book_vent(t, &mut want, g, vented);
                let at = format!("round {round}, gas {g}");
                checker
                    .check(&w, &ledger)
                    .map_err(|v| format!("{at}: {v}"))?;
                if ledger != want {
                    return Err(format!("{at}: ledger {ledger:?}, want {want:?}"));
                }
                if let Some(d) = flat.first_difference(&w) {
                    return Err(format!("{at}: reference differs at {d:?}"));
                }
                for cell in global_order(w.bounds()) {
                    let v = w.layer(cell, LayerId::Gas(g));
                    if v > 0 && !pass.is_air(&w, cell) {
                        return Err(format!("{at}: gas {v} on non-air {cell:?}"));
                    }
                    if pass.holds(&w, cell) && !active(&w, cell, pass) {
                        return Err(format!("{at}: {cell:?} holds gas but is not active"));
                    }
                }
            }
        }
        Ok(())
    }

    /// Test gases that neither drift nor diffuse, so a visit only vents.
    const STILL: &str = r#"[
        (key: "steam", density: 0, diffusion: 0, flammable: false,
         ignite_min: 0, ignite_radius_per_64: 0, visible: true),
        (key: "firedamp", density: 0, diffusion: 0, flammable: true,
         ignite_min: 64, ignite_radius_per_64: 2, visible: false),
        (key: "chokedamp", density: 0, diffusion: 0, flammable: false,
         ignite_min: 0, ignite_radius_per_64: 0, visible: false),
        (key: "smoke", density: 0, diffusion: 0, flammable: false,
         ignite_min: 0, ignite_radius_per_64: 0, visible: true),
    ]"#;

    #[test]
    fn vent_only_sky_open() {
        let t = tables_with_gases(STILL);
        let p = |x, y, z| CellPos::new(x, y, z);
        let bounds = CellAabb::new(p(0, 0, 0), p(31, 31, 31));
        // A shaft open to the top of the world; a sealed pocket; a column
        // with a stone roof at y = 21 and air above and below it.
        let shaft = |c: CellPos| c.0.x == 4 && c.0.z == 4 && c.0.y >= 10;
        let pocket = p(10, 10, 10);
        let roofed = |c: CellPos| c.0.x == 16 && c.0.z == 4 && c.0.y >= 10 && c.0.y != 21;
        let mut w = carved(&t, bounds, |c| shaft(c) || c == pocket || roofed(c));
        assert_eq!(w.surface_y(4, 4), 9);
        assert_eq!(w.surface_y(16, 4), 21);
        let open = [p(4, 10, 4), p(4, 31, 4), p(16, 22, 4)];
        let covered = [pocket, p(16, 20, 4), p(16, 10, 4)];
        let v = |g: u8| 50 + 10 * i32::from(g);
        let n_gas = t.gases().len() as u8;
        for c in open.into_iter().chain(covered) {
            for g in 0..n_gas {
                w.eco_put_layer(c, LayerId::Gas(g), v(g));
            }
        }
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut passes = GasPass::all(&t);
        sweep_all(&mut w, &mut passes, &mut ledger);

        let mut want = SeasonLedger::default();
        for g in 0..n_gas {
            book_vent(&t, &mut want, g, open.len() as i64 * i64::from(v(g)));
            for c in open {
                assert_eq!(w.layer(c, LayerId::Gas(g)), 0, "gas {g} vents at {c:?}");
                assert!(!active(&w, c, &passes[usize::from(g)]), "{c:?} leaves");
            }
            for c in covered {
                assert_eq!(w.layer(c, LayerId::Gas(g)), v(g), "gas {g} stays at {c:?}");
                assert!(active(&w, c, &passes[usize::from(g)]), "{c:?} stays");
            }
        }
        assert_eq!(ledger, want, "vent_out per gas; steam also steam_vent_out");
        let steam = usize::from(t.role_gas("steam").unwrap());
        assert_eq!(ledger.steam_vent_out, ledger.vent_out[steam]);
        assert!(ledger.steam_vent_out > 0);
        checker.check(&w, &ledger).expect("vents balance");

        // Nothing more vents: the covered cells keep their gas.
        sweep_all(&mut w, &mut passes, &mut ledger);
        assert_eq!(ledger, want);
        checker.check(&w, &ledger).expect("vents balance");

        // The random worlds of the invariant proptest vent steam and the
        // other gases and emit firedamp, so its checks are not vacuous.
        let t = tables();
        let mut sum = SeasonLedger::default();
        for seed in 0..8 {
            let l = check_vent_emit(&t, seed, 2).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
            crate::ledger::merge(&mut sum, &l);
        }
        let fd = usize::from(t.gas_index("firedamp").unwrap());
        assert!(sum.steam_vent_out > 0, "{sum:?}");
        assert!(sum.vent_out[fd] > 0, "{sum:?}");
        assert!(sum.emit_in[fd] > 0, "{sum:?}");
    }

    #[test]
    fn emission_only_when_exposed_actual_amounts() {
        let t = tables();
        let coal = id(&t, "coal_seam");
        let water = t.role_material("water").unwrap();
        let air = t.role_material("air").unwrap();
        let fd = t.gas_index("firedamp").unwrap();
        // SAD-P2 §8: coal seams emit firedamp 2 per sub-step.
        assert_eq!(
            t.materials().get(coal).unwrap().emits,
            [("firedamp".to_string(), 2)]
        );
        let p = |x, y, z| CellPos::new(x, y, z);
        let mats: Vec<(CellPos, MaterialId)> = vec![
            // Buried: no air face.
            (p(4, 4, 4), coal),
            // Air below and above: the first in sorted order (below) gets it.
            (p(10, 10, 10), coal),
            (p(10, 9, 10), air),
            (p(10, 11, 10), air),
            // Room 1 in the only air neighbor.
            (p(16, 10, 10), coal),
            (p(17, 10, 10), air),
            // The first air neighbor (+x) is full; the one above is empty
            // but is not the first.
            (p(22, 10, 10), coal),
            (p(23, 10, 10), air),
            (p(22, 11, 10), air),
            // Only water beside it.
            (p(4, 20, 4), coal),
            (p(4, 19, 4), water),
            // Two seams share one air cell holding 252: the first in order
            // adds 2, the second the 1 left.
            (p(10, 20, 10), coal),
            (p(11, 20, 10), air),
            (p(12, 20, 10), coal),
            // Across the chunk face at x = 32.
            (p(31, 10, 20), coal),
            (p(32, 10, 20), air),
        ];
        let mut w = built(&t, two_chunks(), |c| {
            mats.iter().find(|&&(q, _)| q == c).map(|&(_, m)| m)
        });
        let g = LayerId::Gas(fd);
        w.eco_put_layer(p(4, 19, 4), LayerId::Moisture, 200);
        w.eco_put_layer(p(17, 10, 10), g, 254);
        w.eco_put_layer(p(23, 10, 10), g, 255);
        w.eco_put_layer(p(11, 20, 10), g, 252);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let emission = Emission::new(&t);

        assert_eq!(emission.run(&mut w, &mut ledger), 5, "emitting cells");
        let at = |w: &World, c: CellPos| w.layer(c, g);
        assert_eq!((at(&w, p(10, 9, 10)), at(&w, p(10, 11, 10))), (2, 0));
        assert_eq!(at(&w, p(17, 10, 10)), 255);
        assert_eq!((at(&w, p(23, 10, 10)), at(&w, p(22, 11, 10))), (255, 0));
        assert_eq!(at(&w, p(11, 20, 10)), 255);
        assert_eq!(at(&w, p(32, 10, 20)), 2);
        assert_eq!(at(&w, p(4, 19, 4)), 0, "no gas on water");
        let mut want = SeasonLedger::default();
        want.emit_in[usize::from(fd)] = 2 + 1 + 3 + 2;
        assert_eq!(ledger, want, "emit_in records the actual amounts");
        checker.check(&w, &ledger).expect("emission balances");
        let pass = GasPass::new(&t, fd);
        for c in [p(10, 9, 10), p(17, 10, 10), p(11, 20, 10), p(32, 10, 20)] {
            assert!(active(&w, c, &pass), "{c:?} joins the firedamp sweep");
        }

        // Every sub-step emits again, into what room is left.
        assert_eq!(emission.run(&mut w, &mut ledger), 2);
        assert_eq!((at(&w, p(10, 9, 10)), at(&w, p(32, 10, 20))), (4, 4));
        want.emit_in[usize::from(fd)] += 4;
        assert_eq!(ledger, want);
        checker.check(&w, &ledger).expect("emission balances");
    }

    /// `rounds` rounds of every gas sweep (vent step included), then
    /// emission, on a random world with coal seams and sky-open air: after
    /// each, every gas kind balances (steam through the water total's
    /// terms), steam's two vent terms agree, nothing emits steam, and gas
    /// sits only on air. Returns the ledger.
    fn check_vent_emit(t: &EcoTables, seed: u64, rounds: usize) -> Result<SeasonLedger, String> {
        let mut w = random_world(t, seed, Some(id(t, "coal_seam")));
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut passes = GasPass::all(t);
        let emission = Emission::new(t);
        let steam = t.role_gas("steam").map(usize::from);
        for round in 0..rounds {
            for pass in &mut passes {
                let g = pass.gas();
                run_sweep(&mut w, pass, &mut ledger);
                checker
                    .check(&w, &ledger)
                    .map_err(|v| format!("round {round}, gas {g}: {v}"))?;
            }
            emission.run(&mut w, &mut ledger);
            checker
                .check(&w, &ledger)
                .map_err(|v| format!("round {round}, emission: {v}"))?;
            if let Some(st) = steam {
                if ledger.steam_vent_out != ledger.vent_out[st] {
                    return Err(format!("round {round}: steam vent terms differ"));
                }
                if ledger.emit_in[st] != 0 {
                    return Err(format!("round {round}: steam emitted"));
                }
            }
            for cell in global_order(w.bounds()) {
                for g in 0..t.gases().len() as u8 {
                    let v = w.layer(cell, LayerId::Gas(g));
                    if v > 0 && !passes[usize::from(g)].is_air(&w, cell) {
                        return Err(format!("round {round}: gas {g} {v} on non-air {cell:?}"));
                    }
                }
            }
        }
        Ok(ledger)
    }

    #[test]
    fn sealed_pocket_drains_through_shaft() {
        let t = tables();
        let fd = t.gas_index("firedamp").unwrap();
        let g = LayerId::Gas(fd);
        // A 4 × 4 × 4 pocket across the chunk face at x = 32.
        let (xs, ys, zs) = (30..=33, 8..=11, 12..=15);
        let in_pocket =
            |c: CellPos| xs.contains(&c.0.x) && ys.contains(&c.0.y) && zs.contains(&c.0.z);
        let mut w = carved(&t, two_chunks(), in_pocket);
        let pocket: Vec<CellPos> = global_order(two_chunks())
            .filter(|&c| in_pocket(c))
            .collect();
        for &c in &pocket {
            w.eco_put_layer(c, g, 120);
        }
        let total = |w: &World| -> i64 {
            global_order(w.bounds())
                .map(|c| i64::from(w.layer(c, g)))
                .sum()
        };
        let top = |w: &World| -> i64 {
            pocket
                .iter()
                .filter(|c| c.0.y == *ys.end())
                .map(|&c| i64::from(w.layer(c, g)))
                .sum()
        };
        let before = total(&w);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut pass = GasPass::new(&t, fd);
        let substeps = usize::from(t.seasons().substeps);
        // Sealed, for two seasons: it settles under the ceiling and
        // nothing leaves.
        for _ in 0..2 * substeps {
            run_sweep(&mut w, &mut pass, &mut ledger);
        }
        assert_eq!(total(&w), before);
        assert_eq!(
            ledger,
            SeasonLedger::default(),
            "a sealed pocket vents nothing"
        );
        let ceiling = top(&w);
        assert!(ceiling * 4 > before, "light gas collects under the roof");

        // A one-cell shaft from the pocket's roof to the top of the world.
        let air = t.role_material("air").unwrap();
        let (sx, sz) = (31, 13);
        for y in ys.end() + 1..=two_chunks().max.0.y {
            w.eco_set(CellPos::new(sx, y, sz), air, &mut ledger);
        }
        assert_eq!(i32::from(w.surface_y(sx, sz)), ys.start() - 1);
        checker.check(&w, &ledger).expect("the shaft moves no gas");
        let mut last = before;
        for sweep in 0..SHAFT_SEASONS * substeps {
            run_sweep(&mut w, &mut pass, &mut ledger);
            checker
                .check(&w, &ledger)
                .unwrap_or_else(|v| panic!("sweep {sweep}: {v}"));
            let now = total(&w);
            assert!(now <= last, "sweep {sweep}: gas grew {last} -> {now}");
            assert_eq!(ledger.vent_out[usize::from(fd)], before - now);
            last = now;
        }
        let (after, ceiling_after) = (total(&w), top(&w));
        assert!(
            after * 10 < before,
            "pocket kept {after} of {before} after {SHAFT_SEASONS} seasons"
        );
        assert!(
            ceiling_after * 10 < ceiling,
            "roof layer {ceiling_after}, was {ceiling}"
        );
    }

    /// Seasons of sweeps [`sealed_pocket_drains_through_shaft`] allows.
    const SHAFT_SEASONS: usize = 12;

    /// Sum of layer `id` over every cell of `w`.
    fn total(w: &World, id: LayerId) -> i64 {
        global_order(w.bounds())
            .map(|c| i64::from(w.layer(c, id)))
            .sum()
    }

    /// SAD-P2 §7.2 `BoilAll` through the transitions pass: a water cell
    /// above 100 °C becomes air and its whole fill becomes steam on the
    /// cell, 1:1 (ledger `boil_to_steam`). The water total (moisture plus
    /// steam) does not change; the steam gas gains exactly the boil. The
    /// boiled steam then follows the steam sweep: sealed, it stays; open to
    /// the sky, it vents, and the vent is the water total's
    /// `steam_vent_out`, equal to the steam row's `vent_out`.
    #[test]
    fn boil_all_converts_fill_to_steam_1to1() {
        use crate::runner::{Exec, run_jacobi};
        use crate::transitions::TransitionsPass;

        let t = tables();
        let st = t.role_gas("steam").expect("steam role");
        let steam = LayerId::Gas(st);
        let water = t.role_material("water").expect("water role");
        let air = t.role_material("air").expect("air role");
        // The shipped BoilAll row: water above 100.0 °C.
        let boil_at = t
            .transitions()
            .iter()
            .find(|r| r.effect == Some(crate::tables::TransitionEffect::BoilAll))
            .map(|r| match r.when {
                crate::tables::TransitionWhen::TempAbove(x) => i32::from(x),
                other => panic!("BoilAll row on {other:?}"),
            })
            .expect("transitions.ron boils water");
        // A sealed row of water cells across the chunk face at x = 32, a
        // control cell at exactly the boiling row's temperature, and a
        // pond at the bottom of a shaft open to the sky.
        let row: Vec<(CellPos, i32, i32)> = [1, 37, 90, 128, 200, 254, 255, 77]
            .into_iter()
            .enumerate()
            .map(|(i, fill)| {
                let x = 28 + i as i32;
                (CellPos::new(x, 10, 14), fill, boil_at + 1 + 400 * i as i32)
            })
            .collect();
        let control = CellPos::new(20, 10, 14);
        let pond = CellPos::new(40, 20, 20);
        let in_shaft = |c: CellPos| c.0.x == 40 && c.0.z == 20 && c.0.y > 20;
        let mut w = built(&t, two_chunks(), |c| {
            if row.iter().any(|&(r, _, _)| r == c) || c == control || c == pond {
                Some(water)
            } else {
                in_shaft(c).then_some(air)
            }
        });
        let mut wet = row.clone();
        wet.extend([(control, 150, boil_at), (pond, 222, boil_at + 1000)]);
        for &(c, fill, temp) in &wet {
            w.eco_put_layer(c, LayerId::Moisture, fill);
            w.eco_put_layer(c, LayerId::Temperature, temp);
        }
        assert_eq!(
            i32::from(w.surface_y(40, 20)),
            20,
            "the pond tops its column"
        );
        let row_fill: i64 = row.iter().map(|&(_, f, _)| i64::from(f)).sum();
        let boiled = row_fill + 222;
        let (moist0, steam0) = (total(&w, LayerId::Moisture), total(&w, steam));
        assert_eq!(steam0, 0);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();

        run_jacobi(
            &mut w,
            &TransitionsPass::new(&t),
            Exec::Parallel,
            &mut ledger,
        );

        let gas = GasPass::new(&t, st);
        for &(c, fill, _) in row.iter().chain([&(pond, 222, 0)]) {
            assert_eq!(w.get_cell(c), Some(air), "{c:?} boiled");
            assert_eq!(w.layer(c, LayerId::Moisture), 0, "{c:?} keeps no fill");
            assert_eq!(w.layer(c, steam), fill, "{c:?}: fill to steam 1:1");
            assert!(active(&w, c, &gas), "{c:?} joins the steam sweep");
        }
        assert_eq!(w.get_cell(control), Some(water), "at the threshold");
        assert_eq!(w.layer(control, LayerId::Moisture), 150);
        let mut want = SeasonLedger {
            boil_to_steam: boiled,
            transitions: vec![(water, air, row.len() as u32 + 1)],
            ..SeasonLedger::default()
        };
        assert_eq!(ledger, want, "only the boil and its transitions");
        // Both invariants: the water total is unchanged, moisture went to
        // steam, and the steam gas gained exactly the boil.
        checker
            .check(&w, &ledger)
            .expect("conserved after the boil");
        assert_eq!(total(&w, LayerId::Moisture), moist0 - boiled);
        assert_eq!(total(&w, steam), steam0 + boiled);
        // The pond's column is open again down to its stone floor.
        assert_eq!(i32::from(w.surface_y(40, 20)), 19);

        // The steam sweep: the sealed row keeps its steam; the shaft vents
        // everything the pond boiled, and only that.
        let mut pass = gas.clone();
        let shaft_steam = |w: &World| -> i64 {
            global_order(w.bounds())
                .filter(|&c| c == pond || in_shaft(c))
                .map(|c| i64::from(w.layer(c, steam)))
                .sum()
        };
        let mut sweeps = 0;
        while shaft_steam(&w) > 0 {
            assert!(sweeps < 32, "the shaft still holds {}", shaft_steam(&w));
            run_sweep(&mut w, &mut pass, &mut ledger);
            sweeps += 1;
            checker
                .check(&w, &ledger)
                .unwrap_or_else(|v| panic!("sweep {sweeps}: {v}"));
            assert_eq!(ledger.steam_vent_out, ledger.vent_out[usize::from(st)]);
            assert_eq!(shaft_steam(&w) + ledger.steam_vent_out, 222);
            let row_steam: i64 = row
                .iter()
                .map(|&(c, _, _)| i64::from(w.layer(c, steam)))
                .sum();
            assert_eq!(row_steam, row_fill, "sweep {sweeps}: the sealed row");
            assert_eq!(
                total(&w, LayerId::Moisture) + total(&w, steam),
                moist0 + steam0 - ledger.steam_vent_out,
                "sweep {sweeps}: water total less the vent"
            );
        }
        want.steam_vent_out = 222;
        want.vent_out[usize::from(st)] = 222;
        assert_eq!(ledger, want, "the vent is the pond's whole boil");
        assert_eq!(total(&w, steam), row_fill);
    }

    /// Every cell's material over the resident chunks of `w`.
    fn materials_of(w: &World) -> BTreeMap<CellPos, MaterialId> {
        global_order(w.bounds())
            .filter_map(|c| Some((c, w.get_cell(c)?)))
            .collect()
    }

    /// SAD-P2 §7.4 step 5 on two blasts in a world of two chunks of which
    /// only the first is resident:
    ///
    /// - A: firedamp 200 over magma, with smoke 100 already on the cell, so
    ///   smoke gains its room (155), not 200. Radius `(200 / 64) · 2 = 6`.
    ///   The sphere holds stone, bedrock (not diggable), the magma (not
    ///   diggable), water (fill 150: through `eco_set` it boils to steam),
    ///   ice (fill 200, kept as moisture), and limestone wet with 20 and
    ///   holding acid 250 under a plant that holds acid 20. The limestone
    ///   keeps its moisture and acid as air, the plant dies (its species'
    ///   litter goes into the cell), and the 15 of its acid that finds no
    ///   room is `saturation_loss`.
    /// - B: firedamp 128 beside `magma_source` (a `heat_source`), radius 4,
    ///   at z = 2 next to the non-resident chunk: cells out of bounds and in
    ///   the missing chunk are skipped.
    ///
    /// Exactly the diggable cells of the two spheres become air, the ledger
    /// is exactly the reactions, the boil and the displacement loss, and
    /// every invariant holds.
    #[test]
    fn ignition_clears_diggable_sphere_smoke_by_room_displacement_ledgered() {
        let t = tables_with_gases(STILL);
        let fd = t.role_gas("firedamp").unwrap();
        let smoke = t.role_gas("smoke").unwrap();
        let steam = t.role_gas("steam").unwrap();
        let acid = t.role_substance("acid").unwrap();
        let air = t.role_material("air").unwrap();
        let (stone, bedrock, limestone) = (id(&t, "stone"), id(&t, "bedrock"), id(&t, "limestone"));
        let (water, ice) = (
            t.role_material("water").unwrap(),
            t.role_material("ice").unwrap(),
        );
        let (magma, source) = (
            t.role_material("magma").unwrap(),
            t.role_material("magma_source").unwrap(),
        );
        assert!(t.gases()[usize::from(fd)].flammable);

        let a = CellPos::new(10, 10, 10);
        let b = CellPos::new(30, 20, 2);
        let wet = CellPos::new(12, 10, 10);
        let frozen = CellPos::new(10, 10, 12);
        let porous = CellPos::new(10, 11, 10);
        let plant_cell = CellPos::new(10, 12, 10);
        let mats = [
            (a, air),
            (CellPos::new(10, 9, 10), magma),
            (CellPos::new(8, 10, 10), bedrock),
            (wet, water),
            (frozen, ice),
            (porous, limestone),
            (plant_cell, air),
            (b, air),
            (CellPos::new(31, 20, 2), source),
        ];
        let mut w = World::new(Arc::clone(t.materials()), two_chunks());
        w.set_eco_rules(t.displacement_rules());
        let mut chunk = Chunk::uniform(stone);
        for &(c, m) in &mats {
            chunk.storage.set(c.split().1, m);
        }
        w.insert_chunk(ChunkPos::new(0, 0, 0), chunk).unwrap();
        assert!(w.chunk(ChunkPos::new(1, 0, 0)).is_none());
        w.eco_put_layer(a, LayerId::Gas(fd), 200);
        w.eco_put_layer(a, LayerId::Gas(smoke), 100);
        w.eco_put_layer(b, LayerId::Gas(fd), 128);
        w.eco_put_layer(wet, LayerId::Moisture, 150);
        w.eco_put_layer(frozen, LayerId::Moisture, 200);
        w.eco_put_layer(porous, LayerId::Moisture, 20);
        w.eco_put_layer(porous, LayerId::Substance(acid), 250);
        let mut held = [0; rmc_world::layers::N_SUBSTANCES];
        held[usize::from(acid)] = 20;
        w.eco_put_plant(
            plant_cell,
            Some(rmc_world::layers::PlantState {
                species: 0,
                stress: 0,
                age_seasons: 0,
                held,
            }),
        );

        let before = materials_of(&w);
        let spheres = [(a, 6), (b, 4)].map(|(center, radius)| Shape::Sphere { center, radius });
        let in_sphere = |c: CellPos| spheres.iter().any(|s| s.cells().any(|n| n == c));
        let want_mats: BTreeMap<CellPos, MaterialId> = before
            .iter()
            .map(|(&c, &m)| {
                let diggable = t.materials().get(m).unwrap().diggable;
                (c, if diggable && in_sphere(c) { air } else { m })
            })
            .collect();
        // The fixture puts every kind of cell inside a sphere.
        for &(c, _) in &mats {
            assert!(in_sphere(c), "{c:?}");
        }

        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut passes = GasPass::all(&t);
        run_sweep(&mut w, &mut passes[usize::from(fd)], &mut ledger);
        checker.check(&w, &ledger).unwrap_or_else(|v| panic!("{v}"));

        assert_eq!(
            materials_of(&w),
            want_mats,
            "exactly the diggable sphere cells clear"
        );
        for (c, m) in [
            (CellPos::new(8, 10, 10), bedrock),
            (CellPos::new(10, 9, 10), magma),
            (CellPos::new(31, 20, 2), source),
            (CellPos::new(16, 10, 10), air),
            (CellPos::new(17, 10, 10), stone),
            (CellPos::new(30, 24, 2), air),
            (CellPos::new(30, 25, 2), stone),
            (CellPos::new(10, 16, 10), air),
            (CellPos::new(10, 17, 10), stone),
        ] {
            assert_eq!(w.get_cell(c), Some(m), "{c:?}");
        }

        let mut want = SeasonLedger::default();
        want.react_out[usize::from(fd)] = 200 + 128;
        want.react_in[usize::from(smoke)] = 155 + 128;
        want.ignitions = 2;
        want.boil_to_steam = 150;
        want.saturation_loss[usize::from(acid)] = 15;
        want.plants_died = 1;
        let organic = t.role_substance("organic").unwrap();
        let litter = i32::from(t.plants()[0].litter);
        want.litter_in[usize::from(organic)] = i64::from(litter);
        assert_eq!(ledger, want);

        for (c, layer, v) in [
            (a, LayerId::Gas(fd), 0),
            (a, LayerId::Gas(smoke), 255),
            (b, LayerId::Gas(fd), 0),
            (b, LayerId::Gas(smoke), 128),
            (wet, LayerId::Moisture, 0),
            (wet, LayerId::Gas(steam), 150),
            (frozen, LayerId::Moisture, 200),
            (porous, LayerId::Moisture, 20),
            (porous, LayerId::Substance(acid), 255),
            (porous, LayerId::Substance(organic), litter),
        ] {
            assert_eq!(w.layer(c, layer), v, "{layer:?} at {c:?}");
        }
        assert!(w.plant(plant_cell).is_none(), "the plant died");
        assert!(
            active(&w, a, &passes[usize::from(smoke)]),
            "smoke joins its sweep"
        );
        assert!(
            active(&w, wet, &passes[usize::from(steam)]),
            "steam joins its sweep"
        );
        for c in [frozen, porous] {
            let (pos, local) = c.split();
            let water_set = &w.chunk(pos).unwrap().active[rmc_world::layers::SWEEP_WATER];
            assert!(water_set.contains(local), "{c:?} joins the water sweep");
        }

        // The gas is spent: the next sweeps of every gas burn nothing.
        let after = materials_of(&w);
        for _ in 0..4 {
            sweep_all(&mut w, &mut passes, &mut ledger);
            checker.check(&w, &ledger).unwrap_or_else(|v| panic!("{v}"));
        }
        assert_eq!(materials_of(&w), after);
        assert_eq!(ledger, want, "later sweeps book nothing");
    }

    /// No ignition without a flammable gas at `ignite_min` face-adjacent
    /// to a `heat_source` material or magma: firedamp 63 over magma,
    /// firedamp 64 with magma only on an edge diagonal, firedamp 64 beside
    /// stone at 1300 °C (hot, but not a heat source), and steam, chokedamp
    /// and smoke at 255 over magma. A control at exactly 64 beside
    /// `magma_source` ignites (radius 2) once. Repeated sweeps change
    /// nothing else.
    #[test]
    fn no_ignition_below_min() {
        let shipped = tables();
        let fd = shipped.role_gas("firedamp").unwrap();
        for (g, def) in shipped.gases().iter().enumerate() {
            assert_eq!(def.flammable, g == usize::from(fd), "{}", def.key);
        }
        assert_eq!(shipped.gases()[usize::from(fd)].ignite_min, 64);

        let t = tables_with_gases(STILL);
        let fd = t.role_gas("firedamp").unwrap();
        let smoke = t.role_gas("smoke").unwrap();
        let air = t.role_material("air").unwrap();
        let magma = t.role_material("magma").unwrap();
        let source = t.role_material("magma_source").unwrap();
        let stone = id(&t, "stone");
        let p1 = CellPos::new(4, 4, 4);
        let p2 = CellPos::new(10, 4, 4);
        let p3 = CellPos::new(16, 4, 4);
        let p4 = CellPos::new(4, 10, 4);
        let control = CellPos::new(24, 20, 20);
        let hot_stone = CellPos::new(5, 10, 4);
        let mats = [
            (p1, air),
            (CellPos::new(4, 3, 4), magma),
            (p2, air),
            (CellPos::new(11, 3, 4), magma),
            (p3, air),
            (CellPos::new(16, 3, 4), magma),
            (p4, air),
            (control, air),
            (CellPos::new(24, 20, 21), source),
        ];
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut w = built(&t, bounds, |c| {
            mats.iter().find(|&&(p, _)| p == c).map(|&(_, m)| m)
        });
        let cases = [(p1, 63), (p2, 64), (p4, 64), (control, 64)];
        for &(c, v) in &cases {
            w.eco_put_layer(c, LayerId::Gas(fd), v);
        }
        let others: Vec<u8> = (0..t.gases().len() as u8).filter(|&g| g != fd).collect();
        for &g in &others {
            w.eco_put_layer(p3, LayerId::Gas(g), 255);
        }
        w.eco_put_layer(hot_stone, LayerId::Temperature, 13000);
        assert_eq!(w.get_cell(hot_stone), Some(stone));
        assert!(t.materials().get(stone).unwrap().heat_source.is_none());

        let before = materials_of(&w);
        let sphere = Shape::Sphere {
            center: control,
            radius: 2,
        };
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut passes = GasPass::all(&t);
        let mut want = SeasonLedger::default();
        want.react_out[usize::from(fd)] = 64;
        want.react_in[usize::from(smoke)] = 64;
        want.ignitions = 1;
        for sweep in 0..8 {
            sweep_all(&mut w, &mut passes, &mut ledger);
            checker
                .check(&w, &ledger)
                .unwrap_or_else(|v| panic!("sweep {sweep}: {v}"));
            assert_eq!(ledger, want, "sweep {sweep}");
            for (&c, &m) in &before {
                let blown = m == stone && sphere.cells().any(|n| n == c);
                let m = if blown { air } else { m };
                assert_eq!(w.get_cell(c), Some(m), "sweep {sweep}: {c:?}");
            }
            for &(c, v) in &cases[..3] {
                assert_eq!(w.layer(c, LayerId::Gas(fd)), v, "{c:?}");
            }
            for &g in &others {
                assert_eq!(w.layer(p3, LayerId::Gas(g)), 255, "gas {g}");
            }
        }
        assert_eq!(w.layer(control, LayerId::Gas(fd)), 0);
        assert_eq!(w.layer(control, LayerId::Gas(smoke)), 64);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// [`check_random`] on random seeds with the shipped gases, two
        /// rounds (for time).
        #[test]
        fn oracle_equivalence_flat_sweep_gas_proptest(seed in any::<u64>()) {
            let t = tables();
            let r = check_random(&t, seed, 2);
            prop_assert!(r.is_ok(), "{:?}", r);
        }

        /// [`check_vent_emit`] on random seeds with the shipped gases, two
        /// rounds (for time).
        #[test]
        fn invariant_per_kind_including_steam_terms_proptest(seed in any::<u64>()) {
            let t = tables();
            let r = check_vent_emit(&t, seed, 2);
            prop_assert!(r.is_ok(), "{:?}", r);
        }
    }
}
