//! Chemistry (SAD-P2 §7.6). So far: transport.
//!
//! **Transport** replays the sub-step's [`MoveLog`] in order, after the
//! weathering pass (§3). The water sweep appends every move of its steps
//! B2–B4 and B7 (§7.3) as `(A, B, amount, fill_A)`, where `fill_A` is what
//! the donor held just before the move. For each entry and each substance
//! `x`, in row order:
//!
//! - `mob = mobility_acid` when `pH(A) < acid_ph`, else `mobility`;
//! - `m = ((x_A · mob · amount) / 256) / fill_A`, in `u32`, truncating;
//! - `min(m, x_A, 255 − x_B)` moves from A to B. Remainders stay on A.
//!
//! `pH(A)` is read once per entry, from A's state before the entry moves
//! anything. Every move stays inside the substance's total, so transport
//! books no ledger term and is exactly conservative. A contact boil (B7)
//! is logged with `A == B`: the boiled water leaves as steam and its
//! substances stay on the cell (§7.0), so such an entry moves nothing.
//! [`reference::flat_transport`] is the flat-array oracle.
//!
//! [`reference::flat_transport`]: crate::reference::flat_transport

use rmc_world::coords::CellPos;
use rmc_world::layers::LayerId;
use rmc_world::world::World;

use crate::tables::{EcoTables, PH_MAX};

/// One water move of the flow sweeps (SAD-P2 §7.3): `amount` units went
/// from `from` to `to`, out of the `fill_from` the donor held just before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub from: CellPos,
    pub to: CellPos,
    pub amount: u8,
    pub fill_from: u8,
}

/// The water moves of one sub-step, in the order the flow sweeps made
/// them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoveLog {
    moves: Vec<Move>,
}

impl MoveLog {
    pub fn push(&mut self, m: Move) {
        self.moves.push(m);
    }

    pub fn moves(&self) -> &[Move] {
        &self.moves
    }

    pub fn len(&self) -> usize {
        self.moves.len()
    }

    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// Empties the log, keeping its capacity for the next sub-step.
    pub fn clear(&mut self) {
        self.moves.clear();
    }
}

/// The neutral pH, the `7` of `pH = clamp(7 + (lime − acid) / 2^ph_shift,
/// 0, 14)` (§7.6).
pub const PH_NEUTRAL: i32 = 7;
/// The divisor of the transported share, `(x · mob · amount) / 256`.
const MOBILITY_DEN: u32 = 256;
/// The most of a substance a cell holds.
const FULL: i32 = u8::MAX as i32;

/// The derived pH of a cell holding `lime` and `acid` (SAD-P2 §7.6):
/// `clamp(7 + (lime − acid) / 2^ph_shift, 0, 14)`, dividing with
/// truncation toward zero.
pub fn ph(lime: i32, acid: i32, ph_shift: u8) -> i32 {
    (PH_NEUTRAL + (lime - acid) / (1 << ph_shift)).clamp(0, i32::from(PH_MAX))
}

/// The transport replay. Built once per season from the tables.
#[derive(Clone, Debug)]
pub struct Transport {
    /// Per substance row: `(mobility, mobility_acid)`.
    mobility: Vec<(u8, u8)>,
    /// Substance roles `acid` and `lime`; an absent one reads 0.
    acid: Option<u8>,
    lime: Option<u8>,
    /// `eco.ph_shift`.
    ph_shift: u8,
    /// `eco.acid_ph`: below it, `mobility_acid` applies.
    acid_ph: i32,
}

/// What a transport replay did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportReport {
    /// Log entries replayed.
    pub entries: u64,
    /// Substance units moved, all substances together.
    pub moved: u64,
}

impl Transport {
    pub fn new(tables: &EcoTables) -> Self {
        let params = tables.params();
        Self {
            mobility: tables
                .substances()
                .iter()
                .map(|s| (s.mobility, s.mobility_acid))
                .collect(),
            acid: tables.role_substance("acid"),
            lime: tables.role_substance("lime"),
            ph_shift: params.ph_shift,
            acid_ph: i32::from(params.acid_ph),
        }
    }

    /// The derived pH of `cell` in `world`.
    pub fn ph(&self, world: &World, cell: CellPos) -> i32 {
        let read = |x: Option<u8>| x.map_or(0, |x| world.layer(cell, LayerId::Substance(x)));
        ph(read(self.lime), read(self.acid), self.ph_shift)
    }

    /// Replays `log` on `world` in order. An entry between cells that are
    /// not resident, or with a donor fill of 0, moves nothing; the water
    /// sweep logs neither.
    pub fn run(&self, world: &mut World, log: &MoveLog) -> TransportReport {
        let mut report = TransportReport::default();
        for m in log.moves() {
            report.entries += 1;
            let resident = |c: CellPos| world.get_cell(c).is_some();
            if m.from == m.to || m.fill_from == 0 || !resident(m.from) || !resident(m.to) {
                continue;
            }
            let acidic = self.ph(world, m.from) < self.acid_ph;
            for (x, &(mobility, mobility_acid)) in self.mobility.iter().enumerate() {
                let id = LayerId::Substance(u8::try_from(x).expect("substance rows fit a u8"));
                let x_a = world.layer(m.from, id);
                if x_a == 0 {
                    continue;
                }
                let mob = if acidic { mobility_acid } else { mobility };
                let share = (x_a as u32 * u32::from(mob) * u32::from(m.amount) / MOBILITY_DEN)
                    / u32::from(m.fill_from);
                let x_b = world.layer(m.to, id);
                let k = (share as i32).min(x_a).min(FULL - x_b);
                if k > 0 {
                    world.eco_put_layer(m.from, id, x_a - k);
                    world.eco_put_layer(m.to, id, x_b + k);
                    report.moved += k as u64;
                }
            }
        }
        tracing::debug!(
            entries = report.entries,
            moved = report.moved,
            "transport replayed"
        );
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, OnceLock};

    use proptest::prelude::*;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos};
    use rmc_world::layers::N_SUBSTANCES;
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::materials::{MaterialId, Phase};

    use crate::invariants::InvariantChecker;
    use crate::reference::{FlatWorld, TransportRef, flat_transport, global_order};
    use crate::runner::{activate_holders, run_sweep};
    use crate::tables::{EcoSources, SeasonOverride, SubstanceDef};
    use crate::water::WaterPass;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

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

    fn tables() -> EcoTables {
        shipped().clone()
    }

    /// The shipped tables, loaded once for every proptest case.
    fn shipped() -> &'static EcoTables {
        static TABLES: OnceLock<EcoTables> = OnceLock::new();
        TABLES.get_or_init(|| EcoTables::load(ASSETS).expect("Phase 2 tables load"))
    }

    /// The shipped tables with the substance rows changed by `edit`.
    fn tables_with(edit: impl FnOnce(&mut Vec<SubstanceDef>)) -> EcoTables {
        let shipped = tables();
        let mut rows = shipped.substances().to_vec();
        edit(&mut rows);
        let mut src = EcoSources::read(ASSETS).unwrap();
        src.substances = ron::to_string(&rows).unwrap();
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src).unwrap()
    }

    fn layer(x: u8) -> LayerId {
        LayerId::Substance(x)
    }

    /// One chunk of air over a stone floor at y 0, with the tables'
    /// displacement rules.
    fn small_world(t: &EcoTables) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(7, 7, 7));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let stone = t.materials().id("stone").unwrap();
        let mut c = Chunk::uniform(MaterialId::AIR);
        for cell in global_order(bounds) {
            if cell.0.y == 0 {
                c.storage.set(cell.local(), stone);
            }
        }
        w.insert_chunk(ChunkPos::new(0, 0, 0), c).unwrap();
        w
    }

    fn mv(from: CellPos, to: CellPos, amount: u8, fill_from: u8) -> Move {
        Move {
            from,
            to,
            amount,
            fill_from,
        }
    }

    fn log_of(moves: &[Move]) -> MoveLog {
        let mut log = MoveLog::default();
        moves.iter().for_each(|&m| log.push(m));
        log
    }

    /// `m = ((x_A · mob · amount) / 256) / fill_A`: scale by 256 first,
    /// then by the donor's fill, each truncating; then the receiver's room
    /// caps the move and the remainder stays. Entries replay in order, so
    /// a receiver passes on what it got.
    #[test]
    fn transport_scaling_by_256_then_fill() {
        // A substance that is neither acid nor lime, with fixed mobilities.
        let t = tables_with(|rows| {
            rows[0].mobility = 37;
            rows[0].mobility_acid = 200;
        });
        let x = 0u8;
        assert_ne!(Some(x), t.role_substance("acid"));
        assert_ne!(Some(x), t.role_substance("lime"));
        let tr = Transport::new(&t);
        let (a, b, c) = (
            CellPos::new(1, 1, 1),
            CellPos::new(2, 1, 1),
            CellPos::new(3, 1, 1),
        );
        let mut w = small_world(&t);
        assert_eq!(tr.ph(&w, a), PH_NEUTRAL, "no acid: mobility applies");
        w.eco_put_layer(a, layer(x), 201);
        // 201 · 37 · 50 = 371850; / 256 = 1452; / 77 = 18.
        let report = tr.run(&mut w, &log_of(&[mv(a, b, 50, 77)]));
        assert_eq!(w.layer(a, layer(x)), 201 - 18);
        assert_eq!(w.layer(b, layer(x)), 18);
        assert_eq!(report.entries, 1);
        assert_eq!(report.moved, 18);

        // Truncation at each division: x 255, mob 37, amount 1, fill 1:
        // 9435 / 256 = 36 (36.86), / 1 = 36.
        let mut w = small_world(&t);
        w.eco_put_layer(a, layer(x), 255);
        tr.run(&mut w, &log_of(&[mv(a, b, 1, 1)]));
        assert_eq!(w.layer(b, layer(x)), 36);

        // The amount scales before any division: x 7, mob 37, amount 254,
        // fill 255 gives 65786 / 256 = 256, / 255 = 1, where dividing the
        // amount by the fill first would give 7 · 37 · (254 / 255 = 0) = 0.
        let mut w = small_world(&t);
        w.eco_put_layer(a, layer(x), 7);
        tr.run(&mut w, &log_of(&[mv(a, b, 254, 255)]));
        assert_eq!(w.layer(b, layer(x)), 1);
        assert_eq!(w.layer(a, layer(x)), 6);

        // Room: B holds 250, m = 18, so 5 move and 13 stay on A.
        let mut w = small_world(&t);
        w.eco_put_layer(a, layer(x), 201);
        w.eco_put_layer(b, layer(x), 250);
        let report = tr.run(&mut w, &log_of(&[mv(a, b, 50, 77)]));
        assert_eq!(w.layer(a, layer(x)), 201 - 5);
        assert_eq!(w.layer(b, layer(x)), 255);
        assert_eq!(report.moved, 5);

        // In order: A → B, then B → C reads B after the first entry.
        // First: 18 to B. Second: 18 · 37 · 200 / 256 = 520, / 200 = 2.
        let mut w = small_world(&t);
        w.eco_put_layer(a, layer(x), 201);
        tr.run(&mut w, &log_of(&[mv(a, b, 50, 77), mv(b, c, 200, 200)]));
        assert_eq!(w.layer(a, layer(x)), 183);
        assert_eq!(w.layer(b, layer(x)), 16);
        assert_eq!(w.layer(c, layer(x)), 2);
        // The reverse order moves nothing on to C: B is empty then.
        let mut w = small_world(&t);
        w.eco_put_layer(a, layer(x), 201);
        tr.run(&mut w, &log_of(&[mv(b, c, 200, 200), mv(a, b, 50, 77)]));
        assert_eq!(w.layer(b, layer(x)), 18);
        assert_eq!(w.layer(c, layer(x)), 0);
        // A self entry (a contact boil) moves nothing.
        let mut w = small_world(&t);
        w.eco_put_layer(a, layer(x), 201);
        let report = tr.run(&mut w, &log_of(&[mv(a, a, 255, 255)]));
        assert_eq!(w.layer(a, layer(x)), 201);
        assert_eq!(report.moved, 0);
    }

    /// A substance with mobility 0 and mobility_acid 0 never leaves its
    /// cell through a whole sub-step of flow, acid or not, while a mobile
    /// one in the same water does.
    #[test]
    fn immobile_substance_stays() {
        let t = tables_with(|rows| {
            rows[0].mobility = 0;
            rows[0].mobility_acid = 0;
            rows[1].mobility = 255;
            rows[1].mobility_acid = 255;
        });
        let (still, mobile) = (0u8, 1u8);
        let acid = t.role_substance("acid").unwrap();
        let water = t.role_material("water").unwrap();
        let tr = Transport::new(&t);
        let mut pass = WaterPass::new(&t);
        let mut w = small_world(&t);
        // A full column of water at x 1, z 1, y 3..=5 over air, spilling
        // down and sideways.
        let column: Vec<CellPos> = (3..=5).map(|y| CellPos::new(1, y, 1)).collect();
        for (i, &c) in column.iter().enumerate() {
            w.eco_set(c, water, &mut SeasonLedger::default());
            w.eco_put_layer(c, LayerId::Moisture, 255);
            w.eco_put_layer(c, layer(still), 100 + i as i32);
            w.eco_put_layer(c, layer(mobile), 100);
            if i == 0 {
                w.eco_put_layer(c, layer(acid), 255);
            }
        }
        activate_holders(&mut w, &pass);
        let before = FlatWorld::from_world(&w, &[layer(still)]);
        let mut ledger = SeasonLedger::default();
        for i in 0..pass.flow_iters() {
            pass.set_iter(i);
            run_sweep(&mut w, &mut pass, &mut ledger);
        }
        let log = pass.log().clone();
        assert!(log.len() > 4, "the water moved: {}", log.len());
        let report = tr.run(&mut w, &log);
        assert!(report.moved > 0, "the mobile substance moved");
        for cell in global_order(w.bounds()) {
            assert_eq!(
                w.layer(cell, layer(still)),
                before.layer(cell, layer(still)),
                "{cell:?}"
            );
        }
        let spread = global_order(w.bounds())
            .filter(|&c| !column.contains(&c) && w.layer(c, layer(mobile)) > 0)
            .count();
        assert!(spread > 0, "the mobile substance left the column");
    }

    /// Below `acid_ph` the donor's `mobility_acid` applies, at and above it
    /// `mobility`. pH is read once per entry, before the entry moves the
    /// donor's acid, and its division truncates toward zero.
    #[test]
    fn mobility_acid_used_below_acid_ph() {
        let t0 = tables();
        let (acid, lime) = (
            t0.role_substance("acid").unwrap(),
            t0.role_substance("lime").unwrap(),
        );
        // The probe: a substance after both acid and lime in row order,
        // so the entry moves acid before it.
        let probe = (acid.max(lime) + 1) as usize;
        assert!(probe < t0.substances().len());
        let t = tables_with(|rows| {
            rows[probe].mobility = 16;
            rows[probe].mobility_acid = 128;
            rows[usize::from(acid)].mobility = 255;
            rows[usize::from(acid)].mobility_acid = 255;
        });
        let probe = probe as u8;
        let p = t.params();
        let step = 1 << p.ph_shift;
        let acid_ph = i32::from(p.acid_ph);
        let tr = Transport::new(&t);
        let (a, b) = (CellPos::new(1, 1, 1), CellPos::new(2, 1, 1));
        // (acid, lime) on A and the pH they give.
        let below = (PH_NEUTRAL - acid_ph + 1) * step; // pH acid_ph − 1
        let at = (PH_NEUTRAL - acid_ph) * step; // pH acid_ph
        let at_trunc = (PH_NEUTRAL - acid_ph + 1) * step - 1; // truncates to acid_ph
        let cases = [
            (below, 0, acid_ph - 1, true),
            (at, 0, acid_ph, false),
            (at_trunc, 0, acid_ph, false),
            (below + 2 * step, 2 * step, acid_ph - 1, true),
            (0, 0, PH_NEUTRAL, false),
        ];
        for (acid_v, lime_v, want_ph, acidic) in cases {
            let mut w = small_world(&t);
            w.eco_put_layer(a, layer(acid), acid_v);
            w.eco_put_layer(a, layer(lime), lime_v);
            w.eco_put_layer(a, layer(probe), 200);
            assert_eq!(tr.ph(&w, a), want_ph, "acid {acid_v} lime {lime_v}");
            // A whole fill moves: acid at 255 mobility leaves almost all,
            // which would lift A's pH above acid_ph if it were re-read.
            tr.run(&mut w, &log_of(&[mv(a, b, 200, 200)]));
            let mob: u32 = if acidic { 128 } else { 16 };
            let want = (200 * mob * 200 / 256 / 200) as i32;
            assert_eq!(
                w.layer(b, layer(probe)),
                want,
                "acid {acid_v} lime {lime_v} (pH {want_ph})"
            );
            if acidic && acid_v > 0 {
                assert!(tr.ph(&w, a) >= acid_ph, "the acid left A first");
            }
        }
        // The pH formula's ends clamp to 0..=14.
        assert_eq!(ph(0, 255, 0), 0);
        assert_eq!(ph(255, 0, 0), i32::from(PH_MAX));
    }

    /// A contact boil (§7.3 B7) is logged as a self entry with the boiled
    /// fill, and its substances stay on the cell.
    #[test]
    fn contact_boil_logged_as_self_entry() {
        let t = tables();
        let (Some(magma), Some(water), Some(_)) = (
            t.role_material("magma"),
            t.role_material("water"),
            t.role_material("obsidian"),
        ) else {
            panic!("shipped magma, water and obsidian");
        };
        let mut w = small_world(&t);
        let (hot, wet) = (CellPos::new(2, 1, 2), CellPos::new(3, 1, 2));
        let mut ledger = SeasonLedger::default();
        w.eco_set(hot, magma, &mut ledger);
        w.eco_set(wet, water, &mut ledger);
        w.eco_put_layer(wet, LayerId::Moisture, 90);
        w.eco_put_layer(wet, layer(0), 40);
        let mut pass = WaterPass::new(&t);
        activate_holders(&mut w, &pass);
        // An iteration on which magma does not move (viscosity > 0), so
        // the wet cell stays its neighbour.
        pass.set_iter(1);
        run_sweep(&mut w, &mut pass, &mut ledger);
        assert!(ledger.boil_to_steam > 0, "it boiled");
        let boils: Vec<Move> = pass
            .log()
            .moves()
            .iter()
            .copied()
            .filter(|m| m.from == m.to)
            .collect();
        assert_eq!(boils, vec![mv(wet, wet, 90, 90)]);
        Transport::new(&t).run(&mut w, pass.log());
        assert_eq!(w.layer(wet, layer(0)), 40);
    }

    /// Through a whole season tick, a mobile substance follows the water
    /// out of a spilling column, and every invariant holds: transport runs
    /// after weathering on each sub-step's log.
    #[test]
    fn season_tick_carries_substances_with_water() {
        let t = tables_with(|rows| {
            rows[1].mobility = 255;
            rows[1].mobility_acid = 255;
        })
        .with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: Some(100),
        });
        let mobile = 1u8;
        let water = t.role_material("water").unwrap();
        let mut w = small_world(&t);
        let column: Vec<CellPos> = (3..=5).map(|y| CellPos::new(1, y, 1)).collect();
        for &c in &column {
            w.eco_set(c, water, &mut SeasonLedger::default());
            w.eco_put_layer(c, LayerId::Moisture, 255);
            w.eco_put_layer(c, layer(mobile), 120);
        }
        activate_holders(&mut w, &WaterPass::new(&t));
        let checker = InvariantChecker::new(&w);
        let mut transports = 0;
        let report = crate::tick::season_tick_observed(
            &mut w,
            &t,
            0,
            1,
            &mut crate::activity::Activity::new(t.params().quiet_skip),
            crate::runner::Exec::Serial,
            &mut |i, pass, w, ledger| {
                if let Err(v) = checker.check(w, ledger) {
                    panic!("sub-step {i}, after {pass:?}: {v}");
                }
                transports += usize::from(pass == crate::tick::PassId::Transport);
            },
        );
        assert_eq!(transports, usize::from(t.seasons().substeps));
        checker.check(&w, &report.ledger).unwrap();
        let spread = global_order(w.bounds())
            .filter(|&c| !column.contains(&c) && w.layer(c, layer(mobile)) > 0)
            .count();
        assert!(spread > 0, "the substance left the column with the water");
    }

    /// The random worlds' cell kinds: a stone floor, then stone, dry air,
    /// moist air, water of random fill and the shipped porous solids with
    /// any moisture; substances on any cell, acid and lime often enough to
    /// cross `acid_ph`.
    fn random_world(t: &EcoTables, seed: u64) -> (World, WaterPass) {
        let m = t.materials();
        let stone = m.id("stone").unwrap();
        let water = t.role_material("water").unwrap();
        let porous: Vec<MaterialId> = m
            .iter()
            .filter(|d| d.phase == Phase::Solid && d.has_tag("porous"))
            .map(|d| m.id(&d.key).unwrap())
            .collect();
        assert!(!porous.is_empty(), "shipped porous solids");
        let (acid, lime) = (
            t.role_substance("acid").unwrap(),
            t.role_substance("lime").unwrap(),
        );
        let bounds = CellAabb::new(CellPos::new(-4, -4, -4), CellPos::new(3, 3, 3));
        let mut w = World::new(Arc::clone(m), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut chunks: BTreeMap<ChunkPos, Chunk> = bounds
            .chunks()
            .map(|p| (p, Chunk::uniform(MaterialId::AIR)))
            .collect();
        let mut s = seed;
        let mut layers = Vec::new();
        for cell in global_order(bounds) {
            let r = next(&mut s);
            let v = ((r >> 8) % 256) as i32;
            let (mat, v) = match r % 12 {
                _ if cell.0.y == bounds.min.0.y => (stone, 0),
                0..=1 => (stone, 0),
                2..=3 => (MaterialId::AIR, 0),
                4 => (MaterialId::AIR, v),
                5..=8 => (water, v.max(1)),
                k => (porous[k as usize % porous.len()], v),
            };
            let (pos, local) = cell.split();
            chunks.get_mut(&pos).unwrap().storage.set(local, mat);
            layers.push((cell, LayerId::Moisture, v));
            let r = next(&mut s);
            for x in 0..t.substances().len() as u8 {
                let bits = r >> (6 * u32::from(x));
                if bits.is_multiple_of(3) {
                    layers.push((cell, layer(x), ((bits >> 2) % 256) as i32));
                }
            }
            let r = next(&mut s);
            if r.is_multiple_of(3) {
                layers.push((cell, layer(acid), ((r >> 2) % 256) as i32));
            }
            if r.is_multiple_of(5) {
                layers.push((cell, layer(lime), ((r >> 10) % 256) as i32));
            }
        }
        for (pos, chunk) in chunks {
            w.insert_chunk(pos, chunk).unwrap();
        }
        for (cell, id, v) in layers {
            w.eco_put_layer(cell, id, v);
        }
        let pass = WaterPass::new(t);
        activate_holders(&mut w, &pass);
        (w, pass)
    }

    fn transport_ref(t: &EcoTables) -> TransportRef {
        let p = t.params();
        TransportRef {
            mobility: t
                .substances()
                .iter()
                .map(|s| (i32::from(s.mobility), i32::from(s.mobility_acid)))
                .collect(),
            acid: t.role_substance("acid"),
            lime: t.role_substance("lime"),
            ph_shift: p.ph_shift,
            acid_ph: i32::from(p.acid_ph),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// Over a random 8-chunk world, one sub-step's flow sweeps log every
        /// move: replaying the log's amounts on the starting moisture gives
        /// the moisture after the sweeps, and each entry's `fill_from` is
        /// what the donor held at that point. Transport over that log then
        /// keeps every invariant exact (no ledger term), and it equals the
        /// flat reference replay.
        #[test]
        fn transport_replays_movelog_invariant_proptest(seed in any::<u64>()) {
            let t = shipped();
            let tr = Transport::new(t);
            let (mut w, mut pass) = random_world(t, seed);
            let checker = InvariantChecker::new(&w);
            let mut moist = FlatWorld::from_world(&w, &[LayerId::Moisture]);
            let mut ledger = SeasonLedger::default();
            for i in 0..pass.flow_iters() {
                pass.set_iter(i);
                run_sweep(&mut w, &mut pass, &mut ledger);
            }
            prop_assert_eq!(&ledger, &SeasonLedger::default());
            let log = pass.log().clone();
            for (k, m) in log.moves().iter().enumerate() {
                prop_assert!(m.from != m.to, "entry {}: no boils here", k);
                let held = moist.layer(m.from, LayerId::Moisture);
                prop_assert_eq!(held, i32::from(m.fill_from), "entry {} fill_from", k);
                prop_assert!(m.amount > 0 && m.amount <= m.fill_from, "entry {}", k);
                let got = moist.layer(m.to, LayerId::Moisture);
                moist.set_layer(m.from, LayerId::Moisture, held - i32::from(m.amount));
                moist.set_layer(m.to, LayerId::Moisture, got + i32::from(m.amount));
            }
            for cell in global_order(w.bounds()) {
                prop_assert_eq!(
                    moist.layer(cell, LayerId::Moisture),
                    w.layer(cell, LayerId::Moisture),
                    "moisture at {:?}", cell
                );
            }
            let ids: Vec<LayerId> = (0..N_SUBSTANCES as u8).map(layer).collect();
            let mut flat = FlatWorld::from_world(&w, &ids);
            let report = tr.run(&mut w, &log);
            prop_assert_eq!(report.entries, log.len() as u64);
            if let Err(v) = checker.check(&w, &ledger) {
                prop_assert!(false, "after transport: {}", v);
            }
            prop_assert_eq!(&ledger, &SeasonLedger::default());
            let moved = flat_transport(&mut flat, &log, &transport_ref(t));
            prop_assert_eq!(report.moved, moved);
            for cell in global_order(w.bounds()) {
                for &id in &ids {
                    prop_assert_eq!(
                        flat.layer(cell, id),
                        w.layer(cell, id),
                        "{:?} at {:?}", id, cell
                    );
                }
            }
        }
    }
}
