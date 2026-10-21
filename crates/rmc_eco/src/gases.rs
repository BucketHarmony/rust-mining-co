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
//! the proof. Venting, emission and ignition (§7.4 steps 3 to 5) take
//! their places after diffusion. [`reference::flat_sweep_gas`] is the
//! flat-array oracle.
//!
//! [`reference::flat_sweep_gas`]: crate::reference::flat_sweep_gas

use rmc_world::coords::CellPos;
use rmc_world::layers::{LayerId, SWEEP_GAS};
use rmc_world::materials::Phase;
use rmc_world::world::World;

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
    /// Per material id: whether it is `Phase::Air`.
    air: Vec<bool>,
}

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
            air: tables
                .materials()
                .iter()
                .map(|d| d.phase == Phase::Air)
                .collect(),
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
    }

    /// Air cells holding the gas. Other cells never hold gas: the
    /// displacement rules (§7.0) push it out of a cell that stops being
    /// air.
    fn holds(&self, world: &World, cell: CellPos) -> bool {
        self.is_air(world, cell) && self.conc(world, cell) > 0
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
    use rmc_world::ledger::SeasonLedger;
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
        let (stone, air_id) = (id(t, "stone"), t.role_material("air").unwrap());
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut chunks: BTreeMap<ChunkPos, Chunk> = bounds
            .chunks()
            .map(|p| (p, Chunk::uniform(stone)))
            .collect();
        for cell in global_order(bounds) {
            if air(cell) {
                let (pos, local) = cell.split();
                chunks.get_mut(&pos).unwrap().storage.set(local, air_id);
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
    /// and air; air cells hold each gas at a random concentration (often
    /// 0, sometimes 255). Gas holders are active.
    fn random_world(t: &EcoTables, seed: u64) -> World {
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
                0 | 1 => stone,
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
        }
    }

    /// `rounds` rounds of every gas sweep on a random world: after each
    /// sweep, conservation, gas only on air, holders active and the flat
    /// reference equal.
    fn check_random(t: &EcoTables, seed: u64, rounds: usize) -> Result<(), String> {
        let mut w = random_world(t, seed);
        let ids: Vec<LayerId> = (0..t.gases().len() as u8).map(LayerId::Gas).collect();
        let mut flat = FlatWorld::from_world(&w, &ids);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut passes = GasPass::all(t);
        for round in 0..rounds {
            for pass in &mut passes {
                let g = pass.gas();
                run_sweep(&mut w, pass, &mut ledger);
                flat_sweep_gas(&mut flat, &gas_ref(t, g));
                let at = format!("round {round}, gas {g}");
                checker
                    .check(&w, &ledger)
                    .map_err(|v| format!("{at}: {v}"))?;
                if ledger != SeasonLedger::default() {
                    return Err(format!("{at}: ledger {ledger:?}"));
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
    }
}
