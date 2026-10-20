//! Water (SAD-P2 §7.3, sweep): the flow sweeps over the water active set.
//!
//! A visit applies the §7.3 B steps to one cell in order. Material changes
//! go through [`World::eco_set`] (§7.0.1), which runs the displacement
//! rules (moisture stays on the cell, gases leave a cell that turns to
//! water) and writes their ledger terms; the host sends the changed chunks
//! to clients as snapshots after the tick. Steps so far:
//!
//! 1. air with moisture > 0 becomes water;
//! 2. down into air or water, as much as fits;
//! 3. sideways, to each of the 4 horizontal neighbors in sorted order
//!    that is air or water with lower fill: `min(flow_cap, (fill −
//!    fill_n) / 2, 255 − fill_n)`;
//! 5. water at fill 0 becomes air.
//!
//! Every move reads the donor's fill and the receiver's room from the
//! current state just before it is applied, and the receiver joins the
//! active set. Moves inside the water total need no ledger entry: water
//! that changes cells is neither a source nor a sink. Porous solids
//! (steps 2–4, F122), magma (6) and contact boiling (7) take their places
//! in this order; [`reference::flat_sweep_water`] is the flat-array
//! oracle.
//!
//! [`reference::flat_sweep_water`]: crate::reference::flat_sweep_water

use rmc_world::coords::CellPos;
use rmc_world::layers::{LayerId, SWEEP_WATER};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::world::World;

use crate::runner::{SweepCtx, SweepPass};
use crate::tables::EcoTables;

/// The water flow sweep. Without a `water` (or `air`) material in the
/// tables, it changes nothing.
#[derive(Clone, Debug)]
pub struct WaterPass {
    /// Role `air`: what water at fill 0 becomes.
    air: Option<MaterialId>,
    /// Role `water`.
    water: Option<MaterialId>,
    /// `eco.flow_iters`: sweeps per sub-step.
    flow_iters: u8,
    /// `eco.flow_cap`: most water a sideways move carries.
    flow_cap: i32,
}

/// The most water a cell holds.
const FULL: i32 = u8::MAX as i32;
/// The divisor of the sideways move, `(fill − fill_n) / 2` (§7.3 B3).
const SIDE_SPLIT: i32 = 2;
/// The cell below.
const BELOW: [i32; 3] = [0, -1, 0];
/// The 4 horizontal neighbors in sorted (y, z, x) order.
const SIDEWAYS: [[i32; 3]; 4] = [[0, 0, -1], [-1, 0, 0], [1, 0, 0], [0, 0, 1]];

fn offset(cell: CellPos, d: [i32; 3]) -> CellPos {
    CellPos::new(cell.0.x + d[0], cell.0.y + d[1], cell.0.z + d[2])
}

fn moisture(world: &World, cell: CellPos) -> i32 {
    world.layer(cell, LayerId::Moisture)
}

impl WaterPass {
    pub fn new(tables: &EcoTables) -> Self {
        let params = tables.params();
        Self {
            air: tables.role_material("air"),
            water: tables.role_material("water"),
            flow_iters: params.flow_iters,
            flow_cap: i32::from(params.flow_cap),
        }
    }

    /// Flow sweeps per sub-step (`eco.flow_iters`).
    pub fn flow_iters(&self) -> u8 {
        self.flow_iters
    }

    fn is_air(world: &World, id: MaterialId) -> bool {
        world
            .materials()
            .get(id)
            .is_some_and(|d| d.phase == Phase::Air)
    }

    /// `cell` if water can flow into it (air or water); `None` for
    /// anything else, outside the bounds and in a non-resident chunk.
    fn open(&self, world: &World, cell: CellPos) -> Option<Open> {
        let m = world.get_cell(cell)?;
        let air = Self::is_air(world, m);
        (air || Some(m) == self.water).then(|| Open {
            fill: moisture(world, cell),
            air,
        })
    }

    /// Moves `amount` of water from `from` (a water cell and its current
    /// fill) to `to` (an open cell whose room the caller checked) and
    /// returns the donor's new fill. Air becomes water first (§7.0.1:
    /// gases leave it). Nothing happens for an amount of 0.
    fn give(
        &self,
        ctx: &mut SweepCtx<'_>,
        (from, fill): (CellPos, i32),
        (to, open): (CellPos, Open),
        amount: i32,
    ) -> i32 {
        if amount <= 0 {
            return fill;
        }
        debug_assert!(amount <= fill && open.fill + amount <= FULL);
        if open.air {
            let water = self.water.expect("flow runs only with a water role");
            ctx.world.eco_set(to, water, ctx.ledger);
        }
        ctx.world
            .eco_put_layer(from, LayerId::Moisture, fill - amount);
        ctx.world
            .eco_put_layer(to, LayerId::Moisture, open.fill + amount);
        ctx.receive(to);
        fill - amount
    }
}

/// A cell water can flow into, as read just before the move.
#[derive(Clone, Copy, Debug)]
struct Open {
    fill: i32,
    /// Air (becomes water when it receives); otherwise water.
    air: bool,
}

impl SweepPass for WaterPass {
    fn kind(&self) -> usize {
        SWEEP_WATER
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        let (Some(air), Some(water)) = (self.air, self.water) else {
            return;
        };
        let Some(m) = ctx.world.get_cell(cell) else {
            return;
        };
        // The cell's fill, kept current through its moves: only they
        // change it during the visit.
        let mut fill = moisture(ctx.world, cell);
        // 1. Air with moisture becomes water.
        let is_water = if Self::is_air(ctx.world, m) && fill > 0 {
            ctx.world.eco_set(cell, water, ctx.ledger);
            true
        } else {
            m == water
        };
        if !is_water {
            return;
        }
        // 2. Down: as much as the cell below has room for.
        let below = offset(cell, BELOW);
        if let Some(open) = self.open(ctx.world, below) {
            let amount = fill.min(FULL - open.fill);
            fill = self.give(ctx, (cell, fill), (below, open), amount);
        }
        // 3. Sideways: half the difference to each lower neighbor in sorted
        // order, at most `flow_cap`, within its room.
        for d in SIDEWAYS {
            let n = offset(cell, d);
            let Some(open) = self.open(ctx.world, n) else {
                continue;
            };
            if open.fill < fill {
                let amount = self
                    .flow_cap
                    .min((fill - open.fill) / SIDE_SPLIT)
                    .min(FULL - open.fill);
                fill = self.give(ctx, (cell, fill), (n, open), amount);
            }
        }
        // 5. Water at fill 0 becomes air.
        if fill == 0 {
            ctx.world.eco_set(cell, air, ctx.ledger);
        }
    }

    /// Water cells and moist air. Ice keeps its moisture in place, so it
    /// leaves the set.
    fn holds(&self, world: &World, cell: CellPos) -> bool {
        let Some(m) = world.get_cell(cell) else {
            return false;
        };
        Some(m) == self.water
            || (Self::is_air(world, m) && world.layer(cell, LayerId::Moisture) > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Arc;

    use proptest::prelude::*;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, ChunkPos, LocalPos};
    use rmc_world::ledger::SeasonLedger;

    use crate::invariants::InvariantChecker;
    use crate::reference::{FlatWorld, WaterRef, flat_sweep_water, global_order};
    use crate::runner::{activate_holders, run_sweep};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    /// Top of the stone slab the pockets are cut into.
    const SLAB_Y: i32 = 8;

    struct Fix {
        t: EcoTables,
        air: MaterialId,
        water: MaterialId,
    }

    impl Fix {
        fn new() -> Self {
            let t = EcoTables::load(ASSETS).expect("Phase 2 tables load");
            let water = t.role_material("water").expect("materials.ron has water");
            let air = t.role_material("air").expect("materials.ron has air");
            Self { t, air, water }
        }

        /// One chunk: stone up to `SLAB_Y` inclusive, air above, with a
        /// one-cell pocket in the top stone layer at each of `pockets`
        /// holding the given material.
        fn world(&self, pockets: &[(CellPos, MaterialId)]) -> World {
            let m = self.t.materials();
            let stone = m.id("stone").unwrap();
            let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
            let mut w = World::new(Arc::clone(m), bounds);
            w.set_eco_rules(self.t.displacement_rules());
            let mut c = Chunk::uniform(self.air);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let at = CellPos::from_chunk_local(ChunkPos::new(0, 0, 0), l);
                let id = match pockets.iter().find(|(p, _)| *p == at) {
                    Some(&(_, id)) => id,
                    None if at.0.y <= SLAB_Y => stone,
                    None => self.air,
                };
                c.storage.set(l, id);
            }
            w.insert_chunk(ChunkPos::new(0, 0, 0), c).unwrap();
            w
        }
    }

    fn revision(w: &World) -> u64 {
        w.chunk(ChunkPos::new(0, 0, 0)).unwrap().revision
    }

    fn active(w: &World, cell: CellPos) -> bool {
        let (pos, local) = cell.split();
        w.chunk(pos).unwrap().active[SWEEP_WATER].contains(local)
    }

    #[test]
    fn air_with_moisture_becomes_water() {
        let f = Fix::new();
        let steam = f.t.role_gas("steam").expect("steam role");
        let pocket = CellPos::new(10, SLAB_Y, 10);
        let above = CellPos::new(10, SLAB_Y + 1, 10);
        let mut w = f.world(&[(pocket, f.air)]);
        w.eco_put_layer(pocket, LayerId::Moisture, 100);
        w.eco_put_layer(pocket, LayerId::Gas(steam), 7);
        assert!(active(&w, pocket), "moist air joins the water sweep");
        let checker = InvariantChecker::new(&w);
        let rev = revision(&w);
        let mut ledger = SeasonLedger::default();
        let mut pass = WaterPass::new(&f.t);

        let r = run_sweep(&mut w, &mut pass, &mut ledger);

        assert!(r.visits >= 1);
        assert_eq!(w.get_cell(pocket), Some(f.water), "moist air became water");
        assert_eq!(w.layer(pocket, LayerId::Moisture), 100, "fill kept");
        // §7.0: gases leave a cell that turns to water, into the first air
        // neighbor in sorted order (the walls and floor are stone).
        assert_eq!(w.layer(pocket, LayerId::Gas(steam)), 0);
        assert_eq!(w.layer(above, LayerId::Gas(steam)), 7);
        assert!(revision(&w) > rev, "material change bumps the revision");
        assert!(active(&w, pocket), "water stays in the water sweep");
        checker.check(&w, &ledger).expect("water conserved");
        assert_eq!(ledger, SeasonLedger::default(), "nothing gained or lost");

        // Dry air is not water.
        let dry = CellPos::new(20, SLAB_Y + 3, 20);
        w.eco_activate(dry, SWEEP_WATER);
        run_sweep(&mut w, &mut pass, &mut ledger);
        assert_eq!(w.get_cell(dry), Some(f.air));
        assert!(!active(&w, dry), "dry air leaves the water sweep");
    }

    #[test]
    fn water_at_zero_fill_becomes_air() {
        let f = Fix::new();
        let steam = f.t.role_gas("steam").expect("steam role");
        let empty = CellPos::new(10, SLAB_Y, 10);
        let wet = CellPos::new(20, SLAB_Y, 20);
        let mut w = f.world(&[(empty, f.water), (wet, f.water)]);
        w.eco_put_layer(wet, LayerId::Moisture, 40);
        let mut pass = WaterPass::new(&f.t);
        // Inserted chunks keep no active sets: both water cells hold water.
        activate_holders(&mut w, &pass);
        assert!(active(&w, empty) && active(&w, wet));
        let checker = InvariantChecker::new(&w);
        let rev = revision(&w);
        let mut ledger = SeasonLedger::default();

        run_sweep(&mut w, &mut pass, &mut ledger);

        assert_eq!(w.get_cell(empty), Some(f.air), "empty water became air");
        assert_eq!(w.layer(empty, LayerId::Gas(steam)), 0, "no steam made");
        assert_eq!(ledger.boil_to_steam, 0);
        assert!(!active(&w, empty), "the air cell leaves the water sweep");
        assert!(revision(&w) > rev);
        assert_eq!(w.get_cell(wet), Some(f.water), "water with fill stays");
        assert_eq!(w.layer(wet, LayerId::Moisture), 40);
        assert!(active(&w, wet));
        checker.check(&w, &ledger).expect("water conserved");
        assert_eq!(ledger, SeasonLedger::default());
    }

    #[test]
    fn without_water_role_nothing_changes() {
        // A table set whose `water` role resolves to no material.
        let f = Fix::new();
        let mut pass = WaterPass::new(&f.t);
        pass.water = None;
        let pocket = CellPos::new(10, SLAB_Y, 10);
        let mut w = f.world(&[(pocket, f.air)]);
        w.eco_put_layer(pocket, LayerId::Moisture, 100);
        let hash = w.hash();
        run_sweep(&mut w, &mut pass, &mut SeasonLedger::default());
        assert_eq!(w.hash(), hash);
    }

    fn fill(w: &World, cell: CellPos) -> i32 {
        w.layer(cell, LayerId::Moisture)
    }

    /// The slab world with `pockets` of (material, fill), water holders
    /// active.
    fn pocket_world(f: &Fix, pockets: &[(CellPos, MaterialId, i32)]) -> (World, WaterPass) {
        let ids: Vec<(CellPos, MaterialId)> = pockets.iter().map(|&(p, m, _)| (p, m)).collect();
        let mut w = f.world(&ids);
        for &(p, _, v) in pockets {
            w.eco_put_layer(p, LayerId::Moisture, v);
        }
        let pass = WaterPass::new(&f.t);
        activate_holders(&mut w, &pass);
        (w, pass)
    }

    #[test]
    fn sideways_halves_difference_with_cap() {
        let f = Fix::new();
        assert_eq!(f.t.params().flow_cap, 64, "the numbers below assume 64");
        // Two-cell trenches along x in the top slab layer: the donor at
        // x = 10, the receiver at x = 11 (later in global order, so it is
        // visited after the donor in the same sweep).
        let d = |z| CellPos::new(10, SLAB_Y, z);
        let r = |z| CellPos::new(11, SLAB_Y, z);
        let (mut w, mut pass) = pocket_world(
            &f,
            &[
                // Halving: (200 − 100) / 2 = 50, under the cap.
                (d(10), f.water, 200),
                (r(10), f.water, 100),
                // Cap: (250 − 0) / 2 = 125, capped at flow_cap 64.
                (d(14), f.water, 250),
                (r(14), f.air, 0),
                // A difference of 1 halves to 0: no move.
                (d(18), f.water, 101),
                (r(18), f.water, 100),
            ],
        );
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();

        run_sweep(&mut w, &mut pass, &mut ledger);

        assert_eq!((fill(&w, d(10)), fill(&w, r(10))), (150, 150));
        assert_eq!((fill(&w, d(14)), fill(&w, r(14))), (186, 64));
        assert_eq!(w.get_cell(r(14)), Some(f.water), "dry air received water");
        assert_eq!((fill(&w, d(18)), fill(&w, r(18))), (101, 100));
        checker.check(&w, &ledger).expect("water conserved");

        // The next sweep halves what is left: (186 − 64) / 2 = 61.
        run_sweep(&mut w, &mut pass, &mut ledger);
        assert_eq!((fill(&w, d(14)), fill(&w, r(14))), (125, 125));
        assert_eq!((fill(&w, d(10)), fill(&w, r(10))), (150, 150));
        checker.check(&w, &ledger).expect("water conserved");
        assert_eq!(ledger, SeasonLedger::default());
    }

    #[test]
    fn receiver_never_exceeds_255() {
        let f = Fix::new();
        // Vertical two-cell shafts (y = SLAB_Y − 1 and SLAB_Y) and two-cell
        // trenches along x.
        let lo = |x| CellPos::new(x, SLAB_Y - 1, 10);
        let hi = |x| CellPos::new(x, SLAB_Y, 10);
        let t = |x, z| CellPos::new(x, SLAB_Y, z);
        let (mut w, mut pass) = pocket_world(
            &f,
            &[
                // Down into water with room 5.
                (hi(4), f.water, 255),
                (lo(4), f.water, 250),
                // Down into moist air with room 5.
                (hi(8), f.water, 255),
                (lo(8), f.air, 250),
                // Down into full moist air: no room.
                (hi(12), f.water, 255),
                (lo(12), f.air, 255),
                // Sideways from full into 254: halves to 0.
                (t(16, 20), f.water, 255),
                (t(17, 20), f.water, 254),
                // Sideways from full into 200: 27 moves.
                (t(16, 24), f.water, 255),
                (t(17, 24), f.water, 200),
            ],
        );
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();

        run_sweep(&mut w, &mut pass, &mut ledger);

        assert_eq!((fill(&w, hi(4)), fill(&w, lo(4))), (250, 255));
        assert_eq!((fill(&w, hi(8)), fill(&w, lo(8))), (250, 255));
        assert_eq!(w.get_cell(lo(8)), Some(f.water));
        assert_eq!((fill(&w, hi(12)), fill(&w, lo(12))), (255, 255));
        assert_eq!(w.get_cell(lo(12)), Some(f.water), "moist air became water");
        assert_eq!((fill(&w, t(16, 20)), fill(&w, t(17, 20))), (255, 254));
        assert_eq!((fill(&w, t(16, 24)), fill(&w, t(17, 24))), (228, 227));
        checker.check(&w, &ledger).expect("water conserved");

        // Full cells stay full, and nothing is lost, over many sweeps.
        for _ in 0..16 {
            run_sweep(&mut w, &mut pass, &mut ledger);
            checker.check(&w, &ledger).expect("water conserved");
        }
        assert_eq!(fill(&w, lo(4)), 255);
        assert_eq!(fill(&w, lo(12)), 255);
        assert_eq!(ledger, SeasonLedger::default());
    }

    /// Water poured per sweep in `pit_fills_before_overflow`: slow enough
    /// for the sideways flow inside the pit to keep its surface level. A
    /// faster pour heaps a mound over the inflow that reaches the rim
    /// early (at 64 per sweep it spills with the pit about 97 % full).
    const POUR: i32 = 8;

    #[test]
    fn pit_fills_before_overflow() {
        let f = Fix::new();
        // A 3 × 3 pit, 2 deep, in the top of the slab; water is poured onto
        // the cell above its center, a little every sweep.
        let pit: Vec<CellPos> = (SLAB_Y - 1..=SLAB_Y)
            .flat_map(|y| {
                (10..=12).flat_map(move |z| (10..=12).map(move |x| CellPos::new(x, y, z)))
            })
            .collect();
        let ids: Vec<(CellPos, MaterialId)> = pit.iter().map(|&p| (p, f.air)).collect();
        let mut w = f.world(&ids);
        let mut pass = WaterPass::new(&f.t);
        let pour = CellPos::new(11, SLAB_Y + 1, 11);
        let over_pit = |c: CellPos| (10..=12).contains(&c.0.x) && (10..=12).contains(&c.0.z);
        let capacity = pit.len() as i32 * 255;
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let cells: Vec<CellPos> = global_order(w.bounds()).collect();
        let mut poured = 0;
        let mut spilled = false;
        // Each sweep pours at most POUR, so this is enough sweeps to pour
        // the pit's capacity twice.
        for _ in 0..2 * capacity / POUR {
            if spilled {
                break;
            }
            let add = POUR.min(255 - fill(&w, pour));
            w.eco_put_layer(pour, LayerId::Moisture, fill(&w, pour) + add);
            ledger.water_edit_in += i64::from(add);
            poured += add;
            run_sweep(&mut w, &mut pass, &mut ledger);
            checker.check(&w, &ledger).expect("water conserved");
            let outside: i32 = cells
                .iter()
                .filter(|&&c| !over_pit(c))
                .map(|&c| fill(&w, c))
                .sum();
            spilled = outside > 0;
        }
        assert!(spilled, "no spill after pouring {poured}");
        // Halving leaves differences of 1 standing, so when water first
        // spills the pit may be short of full by a unit per cell; its
        // bottom layer is full.
        let held: i32 = pit.iter().map(|&c| fill(&w, c)).sum();
        assert!(
            held >= capacity - pit.len() as i32,
            "spilled with the pit at {held} of {capacity} after pouring {poured}"
        );
        for &c in pit.iter().filter(|c| c.0.y == SLAB_Y - 1) {
            assert_eq!(fill(&w, c), 255, "bottom layer full at {c:?}");
        }
        // Each step across the top layer may stand a unit lower than the
        // last: within the pit's width (3) of full everywhere.
        for &c in pit.iter().filter(|c| c.0.y == SLAB_Y) {
            assert!(
                fill(&w, c) >= 255 - 3,
                "top layer at {c:?}: {}",
                fill(&w, c)
            );
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

    /// A random 8 × 8 × 8 world around a chunk corner, so that it spans
    /// eight chunks and every kind of chunk face (negative coordinates
    /// included): a stone floor, then stone, dry air, moist air and water
    /// of random fill. Water holders are active.
    fn random_world(f: &Fix, seed: u64) -> (World, WaterPass) {
        let stone = f.t.materials().id("stone").unwrap();
        let bounds = CellAabb::new(CellPos::new(-4, -4, -4), CellPos::new(3, 3, 3));
        let mut w = World::new(Arc::clone(f.t.materials()), bounds);
        w.set_eco_rules(f.t.displacement_rules());
        let mut chunks: BTreeMap<ChunkPos, Chunk> = bounds
            .chunks()
            .map(|p| (p, Chunk::uniform(f.air)))
            .collect();
        let mut s = seed;
        let mut fills = Vec::new();
        for cell in global_order(bounds) {
            let r = next(&mut s);
            let v = ((r >> 8) % 256) as i32;
            let (m, v) = match r % 10 {
                _ if cell.0.y == bounds.min.0.y => (stone, 0),
                0..=2 => (stone, 0),
                3..=5 => (f.air, 0),
                6 => (f.air, v),
                _ => (f.water, v),
            };
            let (pos, local) = cell.split();
            chunks.get_mut(&pos).unwrap().storage.set(local, m);
            fills.push((cell, v));
        }
        for (pos, chunk) in chunks {
            w.insert_chunk(pos, chunk).unwrap();
        }
        for (cell, v) in fills {
            w.eco_put_layer(cell, LayerId::Moisture, v);
        }
        let pass = WaterPass::new(&f.t);
        activate_holders(&mut w, &pass);
        (w, pass)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// The water total is exact after every sweep, the ledger stays
        /// empty (flow neither creates nor destroys water), and every cell
        /// that holds water is in the active set.
        #[test]
        fn invariant_exact_after_every_sweep_proptest(seed in any::<u64>()) {
            let f = Fix::new();
            let (mut w, mut pass) = random_world(&f, seed);
            let checker = InvariantChecker::new(&w);
            let mut ledger = SeasonLedger::default();
            for sweep in 0..pass.flow_iters() {
                run_sweep(&mut w, &mut pass, &mut ledger);
                if let Err(v) = checker.check(&w, &ledger) {
                    prop_assert!(false, "sweep {}: {}", sweep, v);
                }
                prop_assert_eq!(&ledger, &SeasonLedger::default());
                for cell in global_order(w.bounds()) {
                    prop_assert!(
                        !pass.holds(&w, cell) || active(&w, cell),
                        "sweep {}: {:?} holds water but is not active", sweep, cell
                    );
                }
            }
        }

        /// The active-set sweep over the chunked world equals the flat
        /// reference that visits every cell, after every sweep.
        #[test]
        fn oracle_equivalence_flat_sweep_proptest(seed in any::<u64>()) {
            let f = Fix::new();
            let (mut w, mut pass) = random_world(&f, seed);
            let mut flat = FlatWorld::from_world(&w, &[LayerId::Moisture]);
            let ids = WaterRef {
                air: f.air,
                water: f.water,
                flow_cap: i32::from(f.t.params().flow_cap),
            };
            let mut ledger = SeasonLedger::default();
            for sweep in 0..pass.flow_iters() {
                run_sweep(&mut w, &mut pass, &mut ledger);
                flat_sweep_water(&mut flat, ids);
                let diff = flat.first_difference(&w);
                prop_assert!(diff.is_none(), "sweep {}: {:?}", sweep, diff);
            }
        }
    }
}
