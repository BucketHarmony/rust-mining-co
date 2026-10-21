//! Chemistry (SAD-P2 §7.6): transport, then reactions, after the
//! weathering pass (§3).
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
//! **Reactions** ([`ReactionsPass`], a Jacobi pass) try the rows of
//! `reactions.ron` in table order on every cell. The first row whose
//! consumed amounts are all on the cell and whose conditions all hold
//! fires, once per cell per sub-step:
//!
//! - `Exposed`: an air face neighbor;
//! - `Submerged`: the cell is water, or the cell directly above is;
//! - `PhBelow(n)` / `PhAbove(n)`: [`ph`] of the cell `< n` / `> n`;
//! - `Evaporating`: the evaporation step (§7.3 C) took water from the cell
//!   in this sub-step, and its fill is now below `evap_low_fill`;
//! - `Tag(t)`: the cell's material carries tag `t`.
//!
//! A row that fails either way passes the cell on to the next row. Firing
//! subtracts `consume` and adds `produce` on the cell's own layers. The
//! net change per substance is booked as `react_delta`, and the produce
//! that finds no room (above 255) as `saturation_loss`. `WeatherBoost(n)`
//! adds `n` to the cell's regolith, saturating.
//!
//! `Coat(Floor, crust, x, amount)` queues a delta of `amount` of `x` into
//! the floor, the cell directly below ([`Queued`], applied after every
//! chunk's output, so deltas to one target add up whichever chunk computed
//! them). It carries what the row consumed out of the water: the deposit
//! is booked as `react_delta` too, so a row consuming as much as it coats
//! changes the world total by nothing. Only a cell that is not a solid (a
//! water cell, or one that dried to air) has a floor; on a solid, or with
//! no resident cell below, a coating row does not fire. When the floor is
//! air, `porous` or `stone`, and not already a `crust`, and its `x`
//! reaches `eco.crust_threshold`, it becomes the crust material
//! ([`World::eco_set`], so the §7.0 rules run).
//!
//! The `EmitGas` side is not applied yet: decompose fires and consumes,
//! and its chokedamp belongs to F155.
//!
//! [`reference::flat_transport`]: crate::reference::flat_transport

use std::collections::{BTreeMap, BTreeSet};

use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::world::World;

use crate::runner::{ChunkView, JacobiOut, JacobiPass, Queued};
use crate::tables::{CoatTarget, Condition, EcoTables, SideEffect};

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

/// pH lives with the other derived readings in `rmc_world::query`, so the
/// client's HUD and editor read the same formula (§7.6).
pub use rmc_world::query::{PH_NEUTRAL, ph};
/// The divisor of the transported share, `(x · mob · amount) / 256`.
const MOBILITY_DEN: u32 = 256;
/// The most of a substance a cell holds.
const FULL: i32 = u8::MAX as i32;

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

/// The six face neighbors.
const FACES: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];
/// The cell above.
const ABOVE: [i32; 3] = [0, 1, 0];
/// The cell below: a `Coat(Floor, ..)` target.
const BELOW: [i32; 3] = [0, -1, 0];

/// Tags of the materials a coat can turn into a crust (SAD-P2 §7.6), with
/// air, unless the material is already a crust.
const COATABLE_TAGS: [&str; 2] = ["porous", "stone"];
const CRUST: &str = "crust";

fn offset(cell: CellPos, [x, y, z]: [i32; 3]) -> CellPos {
    CellPos::new(cell.0.x + x, cell.0.y + y, cell.0.z + z)
}

/// A `reactions.ron` condition resolved against the tables.
#[derive(Clone, Debug)]
enum Cond {
    Exposed,
    Submerged,
    PhBelow(i32),
    PhAbove(i32),
    Evaporating,
    /// Per material id: whether the material carries the tag.
    Tag(Vec<bool>),
}

/// One `reactions.ron` row resolved against the tables.
#[derive(Clone, Debug)]
struct Rule {
    /// Per substance, summed over the row, sorted by layer index.
    consume: Vec<(u8, i32)>,
    /// Net change `produce − consume` per substance named by either list,
    /// sorted by layer index.
    net: Vec<(u8, i32)>,
    conds: Vec<Cond>,
    /// `WeatherBoost(n)`: regolith added on the cell (saturating).
    weather_boost: i32,
    /// `Coat(Floor, ..)`.
    coat: Option<Coat>,
}

/// A `Coat(Floor, crust, x, amount)` side resolved against the tables.
#[derive(Clone, Copy, Debug)]
struct Coat {
    x: u8,
    amount: i32,
    /// `None` when the material table lacks the crust: the deposit still
    /// lands, nothing converts.
    crust: Option<MaterialId>,
}

/// The reactions pass (SAD-P2 §7.6), a Jacobi pass. Built once per season
/// from the tables; [`ReactionsPass::set_evaporated`] gives it each
/// sub-step's evaporated cells.
#[derive(Clone, Debug)]
pub struct ReactionsPass {
    rules: Vec<Rule>,
    /// Substance roles `acid` and `lime`; an absent one reads 0.
    acid: Option<u8>,
    lime: Option<u8>,
    ph_shift: u8,
    /// Role `water`.
    water: Option<MaterialId>,
    /// Per material id: whether it is `Phase::Air`.
    air: Vec<bool>,
    /// Per material id: whether it is `Phase::Solid` (a solid cell has no
    /// floor to coat).
    solid: Vec<bool>,
    /// Per material id: whether a coat turns it into a crust.
    coatable: Vec<bool>,
    /// `eco.crust_threshold`.
    crust_threshold: i32,
    /// `eco.evap_low_fill`.
    evap_low_fill: i32,
    /// The cells the evaporation step evaporated from this sub-step.
    evaporated: BTreeSet<CellPos>,
}

impl ReactionsPass {
    pub fn new(tables: &EcoTables) -> Self {
        let materials = tables.materials();
        let sub = |key: &str| {
            tables
                .substance_index(key)
                .expect("the tables validate reaction substance keys")
        };
        let rules = tables
            .reactions()
            .iter()
            .map(|r| {
                let mut consume: BTreeMap<u8, i32> = BTreeMap::new();
                let mut net: BTreeMap<u8, i32> = BTreeMap::new();
                for (key, n) in &r.consume {
                    *consume.entry(sub(key)).or_default() += i32::from(*n);
                    *net.entry(sub(key)).or_default() -= i32::from(*n);
                }
                for (key, n) in &r.produce {
                    *net.entry(sub(key)).or_default() += i32::from(*n);
                }
                let conds = r
                    .conditions
                    .iter()
                    .map(|c| match c {
                        Condition::Exposed => Cond::Exposed,
                        Condition::Submerged => Cond::Submerged,
                        Condition::PhBelow(n) => Cond::PhBelow(i32::from(*n)),
                        Condition::PhAbove(n) => Cond::PhAbove(i32::from(*n)),
                        Condition::Evaporating => Cond::Evaporating,
                        Condition::Tag(t) => {
                            Cond::Tag(materials.iter().map(|d| d.has_tag(t)).collect())
                        }
                    })
                    .collect();
                let weather_boost = match &r.side {
                    Some(SideEffect::WeatherBoost(n)) => i32::from(*n),
                    _ => 0,
                };
                let coat = match &r.side {
                    Some(SideEffect::Coat(CoatTarget::Floor, crust, x, amount)) => Some(Coat {
                        x: sub(x),
                        amount: i32::from(*amount),
                        crust: materials.id(crust),
                    }),
                    _ => None,
                };
                Rule {
                    consume: consume.into_iter().collect(),
                    net: net.into_iter().collect(),
                    conds,
                    weather_boost,
                    coat,
                }
            })
            .collect();
        Self {
            rules,
            acid: tables.role_substance("acid"),
            lime: tables.role_substance("lime"),
            ph_shift: tables.params().ph_shift,
            water: tables.role_material("water"),
            air: materials.iter().map(|d| d.phase == Phase::Air).collect(),
            solid: materials.iter().map(|d| d.phase == Phase::Solid).collect(),
            coatable: materials
                .iter()
                .map(|d| {
                    (d.phase == Phase::Air || COATABLE_TAGS.iter().any(|t| d.has_tag(t)))
                        && !d.has_tag(CRUST)
                })
                .collect(),
            crust_threshold: i32::from(tables.params().crust_threshold),
            evap_low_fill: i32::from(tables.params().evap_low_fill),
            evaporated: BTreeSet::new(),
        }
    }

    /// The cells the evaporation step (§7.3 C) evaporated from in this
    /// sub-step, for the `Evaporating` condition.
    pub fn set_evaporated(&mut self, cells: BTreeSet<CellPos>) {
        self.evaporated = cells;
    }
}

impl ReactionsPass {
    /// Whether `cond` holds for `cell` (material `m`, local `local`).
    fn holds(
        &self,
        view: &ChunkView<'_>,
        cell: CellPos,
        local: LocalPos,
        m: MaterialId,
        cond: &Cond,
    ) -> bool {
        let layers = &view.chunk().layers;
        let ph = || {
            let read = |x: Option<u8>| {
                x.map_or(0, |x| {
                    i32::from(layers.get_u8(LayerId::Substance(x), local))
                })
            };
            ph(read(self.lime), read(self.acid), self.ph_shift)
        };
        match cond {
            Cond::Exposed => FACES.iter().any(|&f| {
                view.material(offset(cell, f))
                    .is_some_and(|n| self.air.get(usize::from(n.0)).copied().unwrap_or(false))
            }),
            Cond::Submerged => {
                self.water.is_some()
                    && (Some(m) == self.water || view.material(offset(cell, ABOVE)) == self.water)
            }
            Cond::PhBelow(n) => ph() < *n,
            Cond::PhAbove(n) => ph() > *n,
            Cond::Evaporating => {
                self.evaporated.contains(&cell)
                    && i32::from(layers.get_u8(LayerId::Moisture, local)) < self.evap_low_fill
            }
            Cond::Tag(has) => has.get(usize::from(m.0)).copied().unwrap_or(false),
        }
    }
}

impl ReactionsPass {
    /// Per material id lookup; `false` past the table.
    fn is(flags: &[bool], m: MaterialId) -> bool {
        flags.get(usize::from(m.0)).copied().unwrap_or(false)
    }

    /// The floor a coating rule on `cell` (material `m`) deposits into, with
    /// its material: the resident cell below a cell that is not a solid.
    fn floor(
        &self,
        view: &ChunkView<'_>,
        cell: CellPos,
        m: MaterialId,
    ) -> Option<(CellPos, MaterialId)> {
        if Self::is(&self.solid, m) {
            return None;
        }
        let below = offset(cell, BELOW);
        view.material(below).map(|f| (below, f))
    }
}

impl JacobiPass for ReactionsPass {
    /// Per cell, the first rule (table order) whose consumed amounts are
    /// on the cell, whose conditions hold and, for a coating rule, which
    /// has a floor, fires once. Reads the cell, its face neighbors and the
    /// evaporated set; writes the cell and queues the coat of its floor.
    fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
        let mut out = JacobiOut::default();
        let layers = &view.chunk().layers;
        // A rule can fire in this chunk only if every substance it consumes
        // has a layer here.
        let live: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| {
                r.consume
                    .iter()
                    .all(|&(x, _)| layers.contains(LayerId::Substance(x)))
            })
            .collect();
        if live.is_empty() {
            return out;
        }
        let amount =
            |x: u8, local: LocalPos| i32::from(layers.get_u8(LayerId::Substance(x), local));
        for (i, cell) in view.cells() {
            let local = LocalPos::from_index(i);
            let m = view.chunk().storage.get(local);
            let Some(rule) = live.iter().find(|r| {
                r.consume.iter().all(|&(x, n)| amount(x, local) >= n)
                    && r.conds.iter().all(|c| self.holds(view, cell, local, m, c))
                    && (r.coat.is_none() || self.floor(view, cell, m).is_some())
            }) else {
                continue;
            };
            for &(x, net) in &rule.net {
                let old = amount(x, local);
                let got = (old + net).min(FULL) - old;
                if got != 0 {
                    out.layers.push((i, LayerId::Substance(x), got));
                }
                out.ledger.react_delta[usize::from(x)] += i64::from(net);
                out.ledger.saturation_loss[usize::from(x)] += i64::from(net - got);
            }
            if rule.weather_boost > 0 {
                // The runner clamps regolith at 255: a saturating add.
                out.layers.push((i, LayerId::Regolith, rule.weather_boost));
            }
            if let Some(coat) = rule.coat
                && let Some((target, floor)) = self.floor(view, cell, m)
            {
                // The runner charges what the floor has no room for to
                // `saturation_loss`.
                out.queued.push(Queued {
                    target,
                    layer: LayerId::Substance(coat.x),
                    amount: coat.amount,
                    then_set: coat
                        .crust
                        .filter(|_| Self::is(&self.coatable, floor))
                        .map(|crust| (crust, self.crust_threshold)),
                });
                out.ledger.react_delta[usize::from(coat.x)] += i64::from(coat.amount);
            }
        }
        out
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
    use crate::runner::{Exec, activate_holders, run_jacobi, run_sweep};
    use crate::tables::{EcoSources, MAX_SHIFT, PH_MAX, ReactionDef, SeasonOverride, SubstanceDef};
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

    // ---- Reactions (F142) ----

    /// The shipped tables with `rows` as the reactions table.
    fn tables_reacting(rows: &[ReactionDef]) -> EcoTables {
        let mut src = EcoSources::read(ASSETS).unwrap();
        src.reactions = ron::to_string(rows).unwrap();
        EcoTables::from_sources(Arc::clone(shipped().materials()), &src).unwrap()
    }

    fn reaction(
        id: &str,
        consume: &[(&str, u8)],
        conditions: Vec<Condition>,
        produce: &[(&str, u8)],
        side: Option<SideEffect>,
    ) -> ReactionDef {
        let pairs = |v: &[(&str, u8)]| v.iter().map(|&(k, n)| (k.to_string(), n)).collect();
        ReactionDef {
            id: id.to_string(),
            consume: pairs(consume),
            conditions,
            produce: pairs(produce),
            side,
        }
    }

    /// The tag conditions of the shipped reactions.
    fn condition_tags(t: &EcoTables) -> Vec<String> {
        t.reactions()
            .iter()
            .flat_map(|r| &r.conditions)
            .filter_map(|c| match c {
                Condition::Tag(tag) => Some(tag.clone()),
                _ => None,
            })
            .collect()
    }

    /// A solid, non-passable material with none of the shipped reactions'
    /// condition tags.
    fn filler(t: &EcoTables) -> MaterialId {
        let tags = condition_tags(shipped());
        let m = t.materials();
        let d = m
            .iter()
            .find(|d| {
                d.phase == Phase::Solid && !d.passable && !tags.iter().any(|tag| d.has_tag(tag))
            })
            .expect("an untagged solid");
        m.id(&d.key).unwrap()
    }

    /// The first solid, non-passable material carrying `tag`.
    fn tagged(t: &EcoTables, tag: &str) -> MaterialId {
        let m = t.materials();
        let d = m
            .iter()
            .find(|d| d.phase == Phase::Solid && !d.passable && d.has_tag(tag))
            .unwrap_or_else(|| panic!("a solid tagged {tag:?}"));
        m.id(&d.key).unwrap()
    }

    /// One chunk of [`filler`].
    fn filled_world(t: &EcoTables) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(7, 7, 7));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        w.insert_chunk(ChunkPos::new(0, 0, 0), Chunk::uniform(filler(t)))
            .unwrap();
        w
    }

    /// The reacting cell of the fixtures, inside the chunk.
    fn at() -> CellPos {
        CellPos::new(3, 3, 3)
    }

    /// Per substance, what `row` consumes, summed.
    fn needs(t: &EcoTables, row: &ReactionDef) -> BTreeMap<u8, i32> {
        let mut need = BTreeMap::new();
        for (k, n) in &row.consume {
            *need.entry(t.substance_index(k).unwrap()).or_default() += i32::from(*n);
        }
        need
    }

    /// Per substance, `produce − consume` of `row`.
    fn nets(t: &EcoTables, row: &ReactionDef) -> BTreeMap<u8, i32> {
        let mut net: BTreeMap<u8, i32> = needs(t, row).into_iter().map(|(x, n)| (x, -n)).collect();
        for (k, n) in &row.produce {
            *net.entry(t.substance_index(k).unwrap()).or_default() += i32::from(*n);
        }
        net
    }

    /// `(acid, lime)` giving pH `p` with at least `need_acid` acid and
    /// `need_lime` lime.
    fn acid_lime_for(t: &EcoTables, p: i32, need_acid: i32, need_lime: i32) -> (i32, i32) {
        let diff = (p - PH_NEUTRAL) * (1 << t.params().ph_shift);
        let (mut acid, mut lime) = if diff >= 0 {
            (need_acid, need_acid + diff)
        } else {
            (need_lime - diff, need_lime)
        };
        let lift = (need_acid - acid).max(need_lime - lime).max(0);
        acid += lift;
        lime += lift;
        assert!(acid <= FULL && lime <= FULL, "pH {p} out of reach");
        assert_eq!(ph(lime, acid, t.params().ph_shift), p);
        (acid, lime)
    }

    /// A world in which `row` (the only reaction of `t`) meets every
    /// condition but condition `fail`, and has every consumed amount but
    /// one unit of substance `short`. `variant` picks between the two
    /// ways `Evaporating` fails: 0 the cell evaporated with a fill at
    /// `evap_low_fill`, 1 it did not evaporate.
    fn reaction_fixture(
        t: &EcoTables,
        fail: Option<usize>,
        short: Option<u8>,
        variant: u8,
    ) -> (World, ReactionsPass) {
        let [row] = t.reactions() else {
            panic!("one reaction");
        };
        let p = t.params();
        let water = t.role_material("water").unwrap();
        let mut w = filled_world(t);
        let c = at();
        let holds = |k: usize| fail != Some(k);
        let mut ledger = SeasonLedger::default();
        let mut mat = filler(t);
        if row.conditions.contains(&Condition::Evaporating) {
            mat = water;
        } else if matches!(row.side, Some(SideEffect::Coat(..))) {
            // A coating row needs a floor: a cell that is not solid.
            mat = MaterialId::AIR;
        }
        for (k, cond) in row.conditions.iter().enumerate() {
            if let Condition::Tag(tag) = cond {
                mat = if holds(k) { tagged(t, tag) } else { filler(t) };
                assert_eq!(t.materials().has_tag(mat, tag), holds(k));
            }
        }
        w.eco_set(c, mat, &mut ledger);
        let mut evaporated = BTreeSet::new();
        let mut ph_target = None;
        for (k, cond) in row.conditions.iter().enumerate() {
            match cond {
                Condition::Exposed if holds(k) => {
                    w.eco_set(offset(c, [1, 0, 0]), MaterialId::AIR, &mut ledger);
                }
                Condition::Submerged if holds(k) => {
                    w.eco_set(offset(c, ABOVE), water, &mut ledger);
                    w.eco_put_layer(offset(c, ABOVE), LayerId::Moisture, FULL);
                }
                Condition::Submerged => assert_ne!(mat, water, "a water cell is submerged"),
                Condition::Evaporating => {
                    let low = i32::from(p.evap_low_fill);
                    let (fill, evaporates) = match (holds(k), variant) {
                        (true, _) => (low - 1, true),
                        (false, 0) => (low, true),
                        (false, _) => (low - 1, false),
                    };
                    w.eco_put_layer(c, LayerId::Moisture, fill);
                    if evaporates {
                        evaporated.insert(c);
                    }
                }
                Condition::PhBelow(n) => {
                    assert!(ph_target.is_none(), "one pH condition per row");
                    let n = i32::from(*n);
                    ph_target = Some(if holds(k) { n - 1 } else { n });
                }
                Condition::PhAbove(n) => {
                    assert!(ph_target.is_none(), "one pH condition per row");
                    let n = i32::from(*n);
                    ph_target = Some(if holds(k) { n + 1 } else { n });
                }
                _ => {}
            }
        }
        let need = needs(t, row);
        let (acid, lime) = (
            t.role_substance("acid").unwrap(),
            t.role_substance("lime").unwrap(),
        );
        let get = |x: u8| need.get(&x).copied().unwrap_or(0);
        let (a, l) = match ph_target {
            Some(target) => acid_lime_for(t, target, get(acid), get(lime)),
            None => (get(acid), get(lime)),
        };
        w.eco_put_layer(c, layer(acid), a);
        w.eco_put_layer(c, layer(lime), l);
        for (&x, &n) in &need {
            if x != acid && x != lime {
                w.eco_put_layer(c, layer(x), n);
            }
        }
        if let Some(x) = short {
            w.eco_put_layer(c, layer(x), need[&x] - 1);
        }
        let mut pass = ReactionsPass::new(t);
        pass.set_evaporated(evaporated);
        (w, pass)
    }

    fn react(w: &mut World, pass: &ReactionsPass) -> SeasonLedger {
        let mut ledger = SeasonLedger::default();
        run_jacobi(w, pass, Exec::Serial, &mut ledger);
        ledger
    }

    fn substances_at(w: &World, c: CellPos) -> Vec<i32> {
        (0..N_SUBSTANCES as u8)
            .map(|x| w.layer(c, layer(x)))
            .collect()
    }

    /// pH is derived in `i32` from the `u8` amounts: `lime − acid` never
    /// wraps, the division by `2^ph_shift` truncates toward zero (never an
    /// arithmetic shift, which rounds down), and the result clamps to
    /// `0..=14`. Checked against an `i64` reference for every amount pair
    /// and every accepted shift.
    #[test]
    fn ph_derivation_i32_no_underflow() {
        for shift in 0..=MAX_SHIFT {
            for lime in 0..=FULL {
                for acid in 0..=FULL {
                    let q = (i64::from(lime) - i64::from(acid)) / (1i64 << shift);
                    let want = (i64::from(PH_NEUTRAL) + q).clamp(0, i64::from(PH_MAX));
                    assert_eq!(
                        i64::from(ph(lime, acid, shift)),
                        want,
                        "lime {lime} acid {acid} shift {shift}"
                    );
                }
            }
        }
        // Toward zero: −17 / 16 is −1 (pH 6); `-17 >> 4` would be −2.
        assert_eq!(ph(0, 17, 4), 6);
        assert_eq!(ph(17, 0, 4), 8);
        assert_eq!(ph(0, 15, 4), PH_NEUTRAL);
        // The ends clamp, at every shift that can reach them.
        assert_eq!(ph(0, FULL, 0), 0);
        assert_eq!(ph(FULL, 0, 0), i32::from(PH_MAX));
        assert_eq!(ph(0, FULL, 4), 0);
        assert_eq!(ph(FULL, 0, 4), i32::from(PH_MAX));
        assert_eq!(ph(FULL, 0, MAX_SHIFT), PH_NEUTRAL);

        // The reactions read the same pH from a cell's u8 layers: a row
        // conditioned on PhBelow(1) fires at acid 255, lime 0 (pH 0, where
        // a u8 `lime - acid` would wrap to 1 and give pH 7) and not at
        // lime 255, acid 0, nor at equal amounts.
        let t0 = shipped();
        let (acid, lime) = (
            t0.role_substance("acid").unwrap(),
            t0.role_substance("lime").unwrap(),
        );
        let key = |x: u8| t0.substances()[usize::from(x)].key.clone();
        let t = tables_reacting(&[reaction(
            "probe",
            &[(&key(acid), 1)],
            vec![Condition::PhBelow(1)],
            &[],
            None,
        )]);
        let pass = ReactionsPass::new(&t);
        for (a, l, fires) in [(FULL, 0, true), (0, FULL, false), (FULL, FULL, false)] {
            let mut w = filled_world(&t);
            w.eco_put_layer(at(), layer(acid), a);
            w.eco_put_layer(at(), layer(lime), l);
            react(&mut w, &pass);
            let left = w.layer(at(), layer(acid));
            assert_eq!(left == a - 1, fires, "acid {a} lime {l}: {left}");
        }
    }

    /// Each shipped reaction, alone in the table, fires on a cell that
    /// meets all its conditions and holds every consumed amount: the
    /// consumed amounts leave, the produced ones arrive, a WeatherBoost
    /// adds regolith, a Coat adds its amount to the cell below, the ledger
    /// books the net change (coat included) as `react_delta`, and nothing
    /// else changes. Failing any one condition, or one unit
    /// short of any consumed amount, it does not fire and changes nothing.
    #[test]
    fn each_reaction_fires_only_on_condition() {
        let shipped = shipped();
        assert!(!shipped.reactions().is_empty());
        let mut kinds = std::collections::HashSet::new();
        for row in shipped.reactions() {
            let t = tables_reacting(std::slice::from_ref(row));
            let net = nets(&t, row);
            let (mut w, pass) = reaction_fixture(&t, None, None, 0);
            let c = at();
            let before = substances_at(&w, c);
            let mut want = w.clone();
            for (&x, &d) in &net {
                want.eco_put_layer(c, layer(x), before[usize::from(x)] + d);
            }
            if let Some(SideEffect::WeatherBoost(n)) = row.side {
                let r = (w.layer(c, LayerId::Regolith) + i32::from(n)).min(FULL);
                want.eco_put_layer(c, LayerId::Regolith, r);
            }
            // A coat lands on the floor, the cell below.
            let mut coated = None;
            if let Some(SideEffect::Coat(_, _, key, n)) = &row.side {
                let (x, floor) = (t.substance_index(key).unwrap(), offset(c, BELOW));
                let n = i32::from(*n);
                assert!(w.layer(floor, layer(x)) + n < i32::from(t.params().crust_threshold));
                want.eco_put_layer(floor, layer(x), w.layer(floor, layer(x)) + n);
                coated = Some((x, n));
            }
            let ledger = react(&mut w, &pass);
            assert_eq!(w.hash(), want.hash(), "{}: fired as the row says", row.id);
            let mut booked = SeasonLedger::default();
            for (&x, &d) in &net {
                booked.react_delta[usize::from(x)] = i64::from(d);
            }
            if let Some((x, n)) = coated {
                booked.react_delta[usize::from(x)] += i64::from(n);
            }
            assert_eq!(ledger, booked, "{}: ledger", row.id);

            for (k, cond) in row.conditions.iter().enumerate() {
                kinds.insert(std::mem::discriminant(cond));
                let variants = if *cond == Condition::Evaporating {
                    2
                } else {
                    1
                };
                for v in 0..variants {
                    let (mut w, pass) = reaction_fixture(&t, Some(k), None, v);
                    let hash = w.hash();
                    let ledger = react(&mut w, &pass);
                    assert_eq!(
                        (w.hash(), ledger),
                        (hash, SeasonLedger::default()),
                        "{}: fired without {cond:?} (variant {v})",
                        row.id
                    );
                }
            }
            for &x in needs(&t, row).keys() {
                let (mut w, pass) = reaction_fixture(&t, None, Some(x), 0);
                let hash = w.hash();
                let ledger = react(&mut w, &pass);
                assert_eq!(
                    (w.hash(), ledger),
                    (hash, SeasonLedger::default()),
                    "{}: fired one unit short of substance {x}",
                    row.id
                );
            }
        }
        // The shipped table exercises every kind of condition.
        assert_eq!(kinds.len(), 6, "condition kinds covered");

        // `Evaporating` is fed by the evaporation step: a shallow water
        // cell under air evaporates, and a row on that condition fires
        // there in the same sub-step, and not on the next without it.
        let key = |x: usize| shipped.substances()[x].key.clone();
        let t = tables_reacting(&[reaction(
            "dry",
            &[(&key(0), 1)],
            vec![Condition::Evaporating],
            &[],
            None,
        )]);
        let water = t.role_material("water").unwrap();
        let mut w = small_world(&t);
        let wet = CellPos::new(2, 1, 2);
        w.eco_set(wet, water, &mut SeasonLedger::default());
        w.eco_put_layer(wet, LayerId::Moisture, i32::from(t.params().evap_low_fill));
        w.eco_put_layer(wet, LayerId::Temperature, 500);
        w.eco_put_layer(wet, layer(0), 10);
        let mut evaporation = crate::water::Evaporation::new(&t).unwrap();
        activate_holders(&mut w, &WaterPass::new(&t));
        let mut ledger = SeasonLedger::default();
        run_sweep(&mut w, &mut evaporation, &mut ledger);
        assert!(ledger.evap_to_steam > 0, "it evaporated");
        let mut pass = ReactionsPass::new(&t);
        pass.set_evaporated(evaporation.take_evaporated());
        react(&mut w, &pass);
        assert_eq!(w.layer(wet, layer(0)), 9, "fired on the evaporated cell");
        pass.set_evaporated(evaporation.take_evaporated());
        react(&mut w, &pass);
        assert_eq!(w.layer(wet, layer(0)), 9, "the record was emptied");
    }

    /// Rows are tried in table order and the first whose conditions hold
    /// and whose consumed amounts are available fires, once per cell per
    /// pass. A row that fails either way passes the cell on to the next.
    #[test]
    fn reaction_order_precedence() {
        let t0 = shipped();
        let (acid, lime) = (
            t0.role_substance("acid").unwrap(),
            t0.role_substance("lime").unwrap(),
        );
        // Four substances that are neither acid nor lime.
        let free: Vec<u8> = (0..t0.substances().len() as u8)
            .filter(|&x| x != acid && x != lime)
            .take(4)
            .collect();
        let [s0, s1, s2, s3] = free[..] else {
            panic!("four free substances");
        };
        let key = |x: u8| t0.substances()[usize::from(x)].key.clone();
        let (k0, k1, k2, k3) = (key(s0), key(s1), key(s2), key(s3));
        let a = reaction("a", &[(&k0, 1)], vec![], &[(&k1, 1)], None);
        let b = reaction("b", &[(&k0, 1)], vec![], &[(&k2, 1)], None);
        let unavailable = reaction("u", &[(&k3, 1)], vec![], &[(&k1, 7)], None);
        let exposed = reaction(
            "e",
            &[(&k0, 2)],
            vec![Condition::Exposed],
            &[(&k3, 3)],
            None,
        );
        let c = at();
        // (table, open the cell to air, (s0, s1, s2, s3) after one pass).
        let cases = [
            (vec![a.clone(), b.clone()], false, [4, 1, 0, 0]),
            (vec![b.clone(), a.clone()], false, [4, 0, 1, 0]),
            (vec![unavailable.clone(), b.clone()], false, [4, 0, 1, 0]),
            (vec![exposed.clone(), b.clone()], false, [4, 0, 1, 0]),
            (vec![exposed.clone(), b.clone()], true, [3, 0, 0, 3]),
            (vec![b.clone(), exposed.clone()], true, [4, 0, 1, 0]),
        ];
        for (i, (rows, open, want)) in cases.into_iter().enumerate() {
            let t = tables_reacting(&rows);
            let pass = ReactionsPass::new(&t);
            let mut w = filled_world(&t);
            if open {
                w.eco_set(
                    offset(c, [0, 0, -1]),
                    MaterialId::AIR,
                    &mut SeasonLedger::default(),
                );
            }
            w.eco_put_layer(c, layer(s0), 5);
            react(&mut w, &pass);
            let got = [s0, s1, s2, s3].map(|x| w.layer(c, layer(x)));
            assert_eq!(got, want, "case {i}");
            // The next pass fires the same row once more: once per cell
            // per pass. (Case 2's unavailable row now has s3 only after
            // case 4's firing, which these tables do not combine.)
            react(&mut w, &pass);
            let twice = [s0, s1, s2, s3].map(|x| w.layer(c, layer(x)));
            let step: Vec<i32> = (0..4).map(|j| want[j] - [5, 0, 0, 0][j]).collect();
            let expect: Vec<i32> = (0..4).map(|j| want[j] + step[j]).collect();
            assert_eq!(twice.to_vec(), expect, "case {i}: second pass");
        }

        // The shipped table: on a carbonate cell acid enough for
        // dissolve_carbonate, lime makes neutralize, the earlier row, fire
        // instead; without lime dissolve_carbonate fires.
        let row = |id: &str| {
            t0.reactions()
                .iter()
                .position(|r| r.id == id)
                .unwrap_or_else(|| panic!("shipped reaction {id}"))
        };
        let (neutralize, dissolve) = (row("neutralize"), row("dissolve_carbonate"));
        assert!(neutralize < dissolve);
        let tag = t0.reactions()[dissolve]
            .conditions
            .iter()
            .find_map(|c| match c {
                Condition::Tag(tag) => Some(tag.clone()),
                _ => None,
            })
            .unwrap();
        let pass = ReactionsPass::new(t0);
        for (l, fired) in [(2, neutralize), (0, dissolve)] {
            let mut w = filled_world(t0);
            w.eco_set(c, tagged(t0, &tag), &mut SeasonLedger::default());
            w.eco_put_layer(c, layer(acid), 112);
            w.eco_put_layer(c, layer(lime), l);
            assert!(ph(l, 112, t0.params().ph_shift) < 4);
            let before = substances_at(&w, c);
            let ledger = react(&mut w, &pass);
            let net = nets(t0, &t0.reactions()[fired]);
            for x in 0..N_SUBSTANCES as u8 {
                let d = net.get(&x).copied().unwrap_or(0);
                assert_eq!(
                    w.layer(c, layer(x)),
                    before[usize::from(x)] + d,
                    "lime {l}: {} fired, substance {x}",
                    t0.reactions()[fired].id
                );
                assert_eq!(ledger.react_delta[usize::from(x)], i64::from(d));
            }
            let boosted = w.layer(c, LayerId::Regolith) > 0;
            assert_eq!(boosted, fired == dissolve, "lime {l}: WeatherBoost");
        }
    }

    /// The ledger books each firing's net change per substance as
    /// `react_delta` and the produce that finds no room as
    /// `saturation_loss`, so the substance invariant holds exactly: by
    /// hand on a cell that overflows, and over random worlds with the
    /// shipped reactions, where parallel equals serial.
    #[test]
    fn react_delta_and_saturation_loss_balance() {
        let t0 = shipped();
        let (acid, lime) = (
            t0.role_substance("acid").unwrap(),
            t0.role_substance("lime").unwrap(),
        );
        let free: Vec<u8> = (0..t0.substances().len() as u8)
            .filter(|&x| x != acid && x != lime)
            .take(2)
            .collect();
        let [a, b] = free[..] else {
            panic!("two free substances");
        };
        let key = |x: u8| t0.substances()[usize::from(x)].key.clone();
        // Consumes 3 of a and gives 1 back, produces 5 of b.
        let t = tables_reacting(&[reaction(
            "r",
            &[(&key(a), 3)],
            vec![],
            &[(&key(a), 1), (&key(b), 5)],
            None,
        )]);
        let pass = ReactionsPass::new(&t);
        let c = at();
        let mut w = filled_world(&t);
        w.eco_put_layer(c, layer(a), 10);
        w.eco_put_layer(c, layer(b), 252);
        let checker = InvariantChecker::new(&w);
        let ledger = react(&mut w, &pass);
        assert_eq!(w.layer(c, layer(a)), 8);
        assert_eq!(w.layer(c, layer(b)), FULL);
        let mut want = SeasonLedger::default();
        want.react_delta[usize::from(a)] = -2;
        want.react_delta[usize::from(b)] = 5;
        want.saturation_loss[usize::from(b)] = 2;
        assert_eq!(ledger, want);
        checker.check(&w, &ledger).unwrap();
        // Full already: all of b is lost, a still falls.
        let ledger = react(&mut w, &pass);
        assert_eq!(w.layer(c, layer(a)), 6);
        assert_eq!(w.layer(c, layer(b)), FULL);
        assert_eq!(ledger.saturation_loss[usize::from(b)], 5);

        // Random worlds, every shipped reaction, random evaporated cells.
        let (mut delta, mut lost, mut fired_worlds) = (0i64, 0i64, 0);
        for seed in 0..32u64 {
            let (mut w, _) = random_world(t0, seed);
            let mut pass = ReactionsPass::new(t0);
            let mut s = seed;
            pass.set_evaporated(
                global_order(w.bounds())
                    .filter(|_| next(&mut s).is_multiple_of(4))
                    .collect(),
            );
            let checker = InvariantChecker::new(&w);
            let mut ledger = SeasonLedger::default();
            for _ in 0..3 {
                run_jacobi(&mut w, &pass, Exec::Parallel, &mut ledger);
                if let Err(v) = checker.check(&w, &ledger) {
                    panic!("seed {seed}: {v}");
                }
            }
            let d: i64 = ledger.react_delta.iter().map(|v| v.abs()).sum();
            delta += d;
            lost += ledger.saturation_loss.iter().sum::<i64>();
            fired_worlds += usize::from(d > 0);
            // Parallel equals serial.
            let (mut serial, _) = random_world(t0, seed);
            let mut l2 = SeasonLedger::default();
            for _ in 0..3 {
                run_jacobi(&mut serial, &pass, Exec::Serial, &mut l2);
            }
            assert_eq!(serial.hash(), w.hash(), "seed {seed}");
            assert_eq!(l2, ledger, "seed {seed}");
        }
        assert!(fired_worlds > 16, "reactions fired in {fired_worlds} of 32");
        assert!(
            delta > 0 && lost > 0,
            "react_delta {delta}, saturation_loss {lost}"
        );
    }

    // ---- Shipped reactions in the season tick (F143) ----

    /// The shipped tables with no rain and a spring air temperature, so
    /// nothing freezes and no water arrives from the sky.
    fn dry_spring() -> EcoTables {
        tables().with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: Some(100),
        })
    }

    /// Runs one whole season on `w`, checking every invariant after every
    /// pass, and hands each pass to `observe`. Returns the season ledger.
    fn season(
        w: &mut World,
        t: &EcoTables,
        observe: &mut dyn FnMut(u8, crate::tick::PassId, &World),
    ) -> SeasonLedger {
        activate_holders(w, &WaterPass::new(t));
        let checker = InvariantChecker::new(w);
        let report = crate::tick::season_tick_observed(
            w,
            t,
            0,
            1,
            &mut crate::activity::Activity::new(t.params().quiet_skip),
            Exec::Serial,
            &mut |i, pass, w, ledger| {
                if let Err(v) = checker.check(w, ledger) {
                    panic!("sub-step {i}, after {pass:?}: {v}");
                }
                observe(i, pass, w);
            },
        );
        checker.check(w, &report.ledger).unwrap();
        report.ledger
    }

    /// The world total of substance `x`.
    fn total(w: &World, x: u8) -> i32 {
        global_order(w.bounds()).map(|c| w.layer(c, layer(x))).sum()
    }

    /// `oxidize`: weathering releases sulfur from an exposed, damp
    /// crushed_sulfide, and in the same sub-step the reaction turns it
    /// into acid. A submerged crushed_sulfide weathers too, but with no
    /// air face it never oxidizes, and the unbroken ore (which does not
    /// weather) releases nothing to oxidize.
    #[test]
    fn exposed_sulfide_releases_s_then_oxidizes() {
        use crate::tick::PassId;
        let t = dry_spring();
        let m = t.materials();
        let (crushed, ore) = (
            m.id("crushed_sulfide").unwrap(),
            m.id("sulfide_ore").unwrap(),
        );
        let s = t.substance_index("s").unwrap();
        let acid = t.role_substance("acid").unwrap();
        let mut ledger = SeasonLedger::default();

        // Exposed: on the floor, in open air, damp below its field capacity.
        let mut w = small_world(&t);
        let (heap, lump) = (CellPos::new(2, 1, 2), CellPos::new(5, 1, 5));
        w.eco_set(heap, crushed, &mut ledger);
        w.eco_set(lump, ore, &mut ledger);
        for c in [heap, lump] {
            w.eco_put_layer(c, LayerId::Moisture, 30);
        }
        let mut seen = Vec::new();
        let mut s_before_react = 0;
        let got = season(&mut w, &t, &mut |i, pass, w| {
            let (s_at, acid_at) = (w.layer(heap, layer(s)), w.layer(heap, layer(acid)));
            match pass {
                PassId::Weathering if i == 0 => {
                    assert!(s_at > 0, "weathering released sulfur");
                    assert_eq!(acid_at, 0, "no acid before the reactions pass");
                    seen.push(pass);
                }
                PassId::Transport if i == 0 => s_before_react = s_at,
                PassId::Reactions if i == 0 => {
                    let [(_, n)] = t.reactions()[0].produce[..] else {
                        panic!("oxidize produces one substance");
                    };
                    assert_eq!(acid_at, i32::from(n), "oxidize fired once");
                    let [(_, k)] = t.reactions()[0].consume[..] else {
                        panic!("oxidize consumes one substance");
                    };
                    assert_eq!(s_at, s_before_react - i32::from(k));
                    seen.push(pass);
                }
                _ => {}
            }
        });
        assert_eq!(seen, [PassId::Weathering, PassId::Reactions]);
        assert!(got.weather_in[usize::from(s)] > 0);
        assert!(got.react_delta[usize::from(s)] < 0);
        assert!(got.react_delta[usize::from(acid)] > 0);
        assert!(w.layer(heap, layer(acid)) > 0, "acid stays on the heap");
        assert_eq!(w.layer(lump, layer(s)), 0, "the ore does not weather");
        assert_eq!(w.layer(lump, layer(acid)), 0);

        // Submerged: the same heap under a full pool, no air face.
        let water = t.role_material("water").unwrap();
        let mut w = small_world(&t);
        for cell in global_order(w.bounds()).filter(|c| c.0.y > 0) {
            w.eco_set(cell, water, &mut ledger);
            w.eco_put_layer(cell, LayerId::Moisture, FULL);
        }
        let heap = CellPos::new(3, 1, 3);
        w.eco_set(heap, crushed, &mut ledger);
        w.eco_put_layer(heap, LayerId::Moisture, 30);
        let got = season(&mut w, &t, &mut |_, _, _| {});
        assert!(
            got.weather_in[usize::from(s)] > 0,
            "it weathers under water"
        );
        assert_eq!(got.react_delta[usize::from(s)], 0, "but never oxidizes");
        assert_eq!(total(&w, acid), 0);
    }

    /// `neutralize`: wherever acid meets lime, two of each become two salt
    /// each sub-step, at any pH and on any material; acid or lime alone
    /// makes nothing.
    #[test]
    fn acid_plus_lime_makes_salt() {
        let t = dry_spring();
        let [acid, lime, salt] = ["acid", "lime", "salt"].map(|k| t.substance_index(k).unwrap());
        let row = t
            .reactions()
            .iter()
            .find(|r| r.produce.iter().any(|(k, _)| k == "salt"))
            .expect("a salt-making reaction");
        assert!(row.conditions.is_empty(), "neutralize needs no condition");
        let need = needs(&t, row);
        let (per_acid, per_lime) = (need[&acid], need[&lime]);
        let per_salt = nets(&t, row)[&salt];
        let substeps = i32::from(t.seasons().substeps);

        let mut w = filled_world(&t);
        // (cell, acid, lime, salt) before the season.
        let cases = [
            (CellPos::new(1, 1, 1), 40, 40, 0),
            (CellPos::new(5, 1, 1), 40, 0, 0),
            (CellPos::new(1, 1, 5), 0, 40, 0),
            // pH 14: fires once, then lacks acid.
            (CellPos::new(5, 1, 5), per_acid + 1, FULL, 0),
            // Room for one fire's salt: the rest is lost.
            (CellPos::new(3, 5, 3), 40, 40, FULL - per_salt),
        ];
        for &(c, a, l, s) in &cases {
            w.eco_put_layer(c, layer(acid), a);
            w.eco_put_layer(c, layer(lime), l);
            w.eco_put_layer(c, layer(salt), s);
        }
        assert_eq!(ph(FULL, per_acid + 1, t.params().ph_shift), PH_MAX.into());
        let got = season(&mut w, &t, &mut |_, _, _| {});
        let fires = |a: i32, l: i32| substeps.min(a / per_acid).min(l / per_lime);
        let mut fired = 0;
        for &(c, a, l, s) in &cases {
            let n = fires(a, l);
            fired += n;
            assert_eq!(w.layer(c, layer(acid)), a - n * per_acid, "acid at {c:?}");
            assert_eq!(w.layer(c, layer(lime)), l - n * per_lime, "lime at {c:?}");
            assert_eq!(
                w.layer(c, layer(salt)),
                (s + n * per_salt).min(FULL),
                "salt at {c:?}"
            );
        }
        assert_eq!(
            fires(40, 40),
            substeps,
            "the first case fires every sub-step"
        );
        assert_eq!(
            got.react_delta[usize::from(salt)],
            i64::from(fired * per_salt)
        );
        assert_eq!(
            got.react_delta[usize::from(acid)],
            -i64::from(fired * per_acid)
        );
        assert_eq!(
            got.react_delta[usize::from(lime)],
            -i64::from(fired * per_lime)
        );
        assert_eq!(
            got.saturation_loss[usize::from(salt)],
            i64::from(substeps * per_salt - per_salt),
            "the full cell keeps one fire's salt"
        );
    }

    /// `dissolve_carbonate`: acid water over a limestone bed seeps into
    /// the porous stone and carries its acid in. There, below pH 4, the
    /// carbonate dissolves: lime appears, the stone softens (WeatherBoost)
    /// and the lime then neutralizes acid into salt. The same pool without
    /// acid only weathers the bed.
    #[test]
    fn dissolve_carbonate_in_porous_limestone_fed_by_seepage() {
        let t = dry_spring();
        let m = t.materials();
        let limestone = m.id("limestone").unwrap();
        assert!(m.has_tag(limestone, "carbonate") && m.has_tag(limestone, "porous"));
        let water = t.role_material("water").unwrap();
        let [acid, lime, salt] = ["acid", "lime", "salt"].map(|k| t.substance_index(k).unwrap());
        let bed_top = 3;
        let pool = |pool_acid: i32| {
            let mut w = small_world(&t);
            let mut ledger = SeasonLedger::default();
            for cell in global_order(w.bounds()).filter(|c| c.0.y > 0) {
                if cell.0.y <= bed_top {
                    w.eco_set(cell, limestone, &mut ledger);
                } else {
                    w.eco_set(cell, water, &mut ledger);
                    w.eco_put_layer(cell, LayerId::Moisture, FULL);
                    w.eco_put_layer(cell, layer(acid), pool_acid);
                }
            }
            w
        };
        // The top of the bed, under the pool, and the layer below it, which
        // touches no water: only porous-to-porous seepage (§7.3 B4) reaches it.
        let bed = |w: &World| -> Vec<CellPos> {
            global_order(w.bounds())
                .filter(|c| c.0.y == bed_top || c.0.y == bed_top - 1)
                .collect()
        };

        let mut sour = pool(FULL);
        let before = bed(&sour);
        assert!(before.iter().all(|&c| sour.layer(c, layer(acid)) == 0));
        let got = season(&mut sour, &t, &mut |_, _, _| {});
        let mut clear = pool(0);
        season(&mut clear, &t, &mut |_, _, _| {});

        let cells = bed(&sour);
        let wet = cells
            .iter()
            .filter(|&&c| sour.layer(c, LayerId::Moisture) > 0)
            .count();
        assert_eq!(wet, cells.len(), "the pool seeped into both layers");
        for &c in &cells {
            assert_eq!(sour.get_cell(c), Some(limestone));
            if c.0.y < bed_top {
                assert!(
                    FACES
                        .iter()
                        .all(|&f| sour.get_cell(offset(c, f)) != Some(water)),
                    "{c:?} is fed only through limestone"
                );
            }
            assert!(
                sour.layer(c, LayerId::Regolith) > clear.layer(c, LayerId::Regolith),
                "acid softened the limestone at {c:?}"
            );
            assert!(
                sour.layer(c, layer(lime)) + sour.layer(c, layer(salt)) > 0,
                "the carbonate dissolved at {c:?}"
            );
            assert_eq!(clear.layer(c, layer(salt)), 0);
        }
        assert!(
            got.react_delta[usize::from(acid)] < 0,
            "the bed consumed acid"
        );
        assert!(
            got.react_delta[usize::from(salt)] > 0,
            "lime neutralized acid"
        );
        // dissolve_carbonate adds lime; neutralize turns lime into as much
        // salt. Their sum is the lime the carbonate gave up.
        let dissolved = got.react_delta[usize::from(lime)] + got.react_delta[usize::from(salt)];
        assert!(dissolved > 0, "carbonate lime: {dissolved}");
    }

    /// `mobility_acid`: the same water moving through the same copper
    /// carries far more of it when the water is acidic (pH below
    /// `acid_ph`). The water moves identically either way.
    #[test]
    fn acid_increases_copper_transport() {
        let t = dry_spring();
        let water = t.role_material("water").unwrap();
        let [cu, acid] = ["cu", "acid"].map(|k| t.substance_index(k).unwrap());
        let row = &t.substances()[usize::from(cu)];
        assert!(row.mobility_acid > row.mobility);
        let column: Vec<CellPos> = (3..=5).map(|y| CellPos::new(1, y, 1)).collect();
        let spill = |column_acid: i32| {
            let mut w = small_world(&t);
            let mut ledger = SeasonLedger::default();
            for &c in &column {
                w.eco_set(c, water, &mut ledger);
                w.eco_put_layer(c, LayerId::Moisture, FULL);
                w.eco_put_layer(c, layer(cu), 120);
                w.eco_put_layer(c, layer(acid), column_acid);
            }
            assert_eq!(
                Transport::new(&t).ph(&w, column[0]) < i32::from(t.params().acid_ph),
                column_acid > 0
            );
            season(&mut w, &t, &mut |_, _, _| {});
            w
        };
        let sour = spill(FULL);
        let plain = spill(0);
        let mut carried = [0; 2];
        for cell in global_order(sour.bounds()) {
            assert_eq!(
                sour.layer(cell, LayerId::Moisture),
                plain.layer(cell, LayerId::Moisture),
                "the water moved the same at {cell:?}"
            );
            if !column.contains(&cell) {
                carried[0] += sour.layer(cell, layer(cu));
                carried[1] += plain.layer(cell, layer(cu));
            }
        }
        assert_eq!(total(&sour, cu), total(&plain, cu), "copper is conserved");
        let [acidic, neutral] = carried;
        assert!(
            acidic > 2 * neutral && acidic > 0,
            "acid water carried {acidic} copper, neutral water {neutral}"
        );
    }

    // ---- Coating and crusts (F144) ----

    /// The shipped coating row whose crust is `crust`: (substance, amount
    /// coated per firing).
    fn coat_of(t: &EcoTables, crust: &str) -> (u8, i32) {
        t.reactions()
            .iter()
            .find_map(|r| match &r.side {
                Some(SideEffect::Coat(CoatTarget::Floor, c, x, n)) if c == crust => {
                    Some((t.substance_index(x).unwrap(), i32::from(*n)))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("a shipped row coats {crust}"))
    }

    /// [`small_world`] with stone up to `floor_y` and, above it, a pool:
    /// one water layer per `(y, fill)`, every cell holding `substances`.
    /// Every cell starts at temperature `temp`.
    fn pool_world(
        t: &EcoTables,
        floor_y: i32,
        pool: &[(i32, i32)],
        substances: &[(u8, i32)],
        temp: i32,
    ) -> World {
        let stone = t.materials().id("stone").unwrap();
        let water = t.role_material("water").unwrap();
        let mut w = small_world(t);
        let mut ledger = SeasonLedger::default();
        for cell in global_order(w.bounds()) {
            w.eco_put_layer(cell, LayerId::Temperature, temp);
            if (1..=floor_y).contains(&cell.0.y) {
                w.eco_set(cell, stone, &mut ledger);
            }
            if let Some(&(_, fill)) = pool.iter().find(|&&(y, _)| y == cell.0.y) {
                w.eco_set(cell, water, &mut ledger);
                w.eco_put_layer(cell, LayerId::Moisture, fill);
                for &(x, n) in substances {
                    w.eco_put_layer(cell, layer(x), n);
                }
            }
        }
        w
    }

    /// The cells of `w` at height `y`.
    fn plane(w: &World, y: i32) -> Vec<CellPos> {
        global_order(w.bounds()).filter(|c| c.0.y == y).collect()
    }

    /// `ochre`: iron in neutral water precipitates onto the floor of the
    /// pool. Each sub-step a water cell holding iron coats the cell below
    /// with it; the pool's upper layer coats the lower one, the lower one
    /// the stone floor. The floor turns to ochre_crust in the sub-step its
    /// iron reaches `crust_threshold`. The submerged floor itself is solid
    /// and passes nothing further down. Acid water (pH ≤ 5) keeps its iron.
    #[test]
    fn ochre_crust_forms_on_submerged_floor() {
        use crate::tick::PassId;
        let t = dry_spring();
        let m = t.materials();
        let (stone, ochre) = (m.id("stone").unwrap(), m.id("ochre_crust").unwrap());
        let water = t.role_material("water").unwrap();
        let (fe, amount) = coat_of(&t, "ochre_crust");
        let acid = t.role_substance("acid").unwrap();
        let threshold = i32::from(t.params().crust_threshold);
        let substeps = i32::from(t.seasons().substeps);
        let fires = (threshold + amount - 1) / amount;
        assert!(fires <= substeps, "a season coats a floor");
        let (floor_y, iron) = (2, 120);
        assert!(iron >= substeps * amount, "the pool never runs out");
        let pool = |pool_acid: i32| {
            pool_world(
                &t,
                floor_y,
                &[(3, FULL), (4, FULL)],
                &[(fe, iron), (acid, pool_acid)],
                0,
            )
        };

        let mut w = pool(0);
        let floor = plane(&w, floor_y);
        let before = total(&w, fe);
        // Per floor cell, the sub-step after whose reactions it was crust.
        let mut crusted: BTreeMap<CellPos, u8> = BTreeMap::new();
        let got = season(&mut w, &t, &mut |i, pass, w| {
            if pass == PassId::Reactions {
                for &c in &floor {
                    if w.get_cell(c) == Some(ochre) {
                        crusted.entry(c).or_insert(i);
                    }
                }
            }
        });
        let when = u8::try_from(fires - 1).unwrap();
        for &c in &floor {
            assert_eq!(w.get_cell(c), Some(ochre), "{c:?} is crust");
            assert_eq!(crusted.get(&c), Some(&when), "{c:?} turned at fire {fires}");
            // The lower pool layer coated it every sub-step.
            assert_eq!(w.layer(c, layer(fe)), (substeps * amount).min(FULL));
        }
        for c in plane(&w, floor_y - 1) {
            assert_eq!(w.get_cell(c), Some(stone), "{c:?} under the floor");
            assert_eq!(
                w.layer(c, layer(fe)),
                0,
                "nothing passes the floor at {c:?}"
            );
        }
        for c in plane(&w, 3).into_iter().chain(plane(&w, 4)) {
            assert_eq!(w.get_cell(c), Some(water));
        }
        // The upper layer gave its iron to the lower one, which passed as
        // much on: the lower layer is where it started.
        for c in plane(&w, 3) {
            assert_eq!(w.layer(c, layer(fe)), iron);
        }
        for c in plane(&w, 4) {
            assert_eq!(w.layer(c, layer(fe)), iron - substeps * amount);
        }
        assert_eq!(total(&w, fe), before, "iron only moved");
        assert_eq!(got.react_delta[usize::from(fe)], 0, "consumed as coated");
        assert_eq!(got.saturation_loss[usize::from(fe)], 0);

        // Acid water: pH 0, no ochre, no crust.
        let mut sour = pool(FULL);
        assert!(Transport::new(&t).ph(&sour, plane(&sour, 3)[0]) <= 5);
        season(&mut sour, &t, &mut |_, _, _| {});
        for &c in &floor {
            assert_eq!(sour.get_cell(c), Some(stone));
            assert_eq!(sour.layer(c, layer(fe)), 0);
        }
    }

    /// `salt_crust`: a shallow salty pool dries out. Each sub-step in which
    /// a pool cell evaporates below `evap_low_fill` (down to dry, when the
    /// cell turns to air) it coats the floor with salt; the floor turns to
    /// salt_crust once its salt reaches `crust_threshold`. A deep pool that
    /// evaporates as fast never gets low, so it leaves no crust.
    #[test]
    fn salt_crust_on_floor_under_drying_pool() {
        let p = shipped().params();
        let (salt, amount) = coat_of(shipped(), "salt_crust");
        let threshold = i32::from(p.crust_threshold);
        let low = i32::from(p.evap_low_fill);
        let need = (threshold + amount - 1) / amount;
        // The warmest bucket that still takes `need` sub-steps to dry a
        // cell at `evap_low_fill`.
        let (bucket, rate) = p
            .evap_table
            .iter()
            .map(|&e| i32::from(e))
            .enumerate()
            .rfind(|&(_, e)| e > 0 && e * need <= low)
            .expect("a slow enough evaporation bucket");
        let temp = i32::try_from(bucket).unwrap() * i32::from(p.evap_bucket);
        assert!(temp < i32::from(p.condense_temp));
        let t = tables().with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: Some(i16::try_from(temp).unwrap()),
        });
        let substeps = i32::from(t.seasons().substeps);
        let dries_in = (low + rate - 1) / rate;
        assert!(need <= dries_in && dries_in <= substeps);
        let floor_y = 2;
        let brine = 100;
        assert!(brine >= dries_in * amount);
        let salt_crust = t.materials().id("salt_crust").unwrap();
        let stone = t.materials().id("stone").unwrap();
        let water = t.role_material("water").unwrap();

        let mut w = pool_world(&t, floor_y, &[(3, low)], &[(salt, brine)], temp);
        let before = total(&w, salt);
        let got = season(&mut w, &t, &mut |_, _, _| {});
        // Below `evap_low_fill` from the first evaporation on, dry after
        // `dries_in`: one coat per evaporating sub-step.
        let coated = dries_in * amount;
        for c in plane(&w, floor_y) {
            assert_eq!(w.get_cell(c), Some(salt_crust), "{c:?} is crust");
            assert_eq!(w.layer(c, layer(salt)), coated.min(FULL));
        }
        for c in plane(&w, 3) {
            assert_eq!(w.get_cell(c), Some(MaterialId::AIR), "{c:?} dried");
            assert_eq!(w.layer(c, layer(salt)), brine - coated);
        }
        assert_eq!(total(&w, salt), before, "salt only moved");
        assert_eq!(got.react_delta[usize::from(salt)], 0);
        assert!(got.evap_to_steam > 0);

        // A deep pool stays above `evap_low_fill` all season: no crust.
        let mut deep = pool_world(&t, floor_y, &[(3, FULL)], &[(salt, brine)], temp);
        season(&mut deep, &t, &mut |_, _, _| {});
        for c in plane(&deep, floor_y) {
            assert_eq!(deep.get_cell(c), Some(stone));
            assert_eq!(deep.layer(c, layer(salt)), 0);
        }
        for c in plane(&deep, 3) {
            assert_eq!(deep.get_cell(c), Some(water));
            assert_eq!(deep.layer(c, layer(salt)), brine);
        }
    }

    /// Queues one delta per chunk into the same cell, plus the target
    /// chunk's own delta there, for the additivity check.
    struct QueueEach {
        target: CellPos,
        layer: LayerId,
        own: i32,
        by_chunk: Vec<(ChunkPos, i32)>,
        then_set: Option<(MaterialId, i32)>,
    }

    impl JacobiPass for QueueEach {
        fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
            let mut out = JacobiOut::default();
            let mut booked = 0;
            if self.target.split().0 == view.pos() {
                let i = self.target.local().index();
                out.layers.push((i, self.layer, self.own));
                booked += self.own;
            }
            for &(pos, amount) in &self.by_chunk {
                if pos == view.pos() {
                    out.queued.push(Queued {
                        target: self.target,
                        layer: self.layer,
                        amount,
                        then_set: self.then_set,
                    });
                    booked += amount;
                }
            }
            if let LayerId::Substance(x) = self.layer {
                out.ledger.react_delta[usize::from(x)] += i64::from(booked);
            }
            out
        }
    }

    /// A coat whose floor lies in the chunk below is queued by the upper
    /// chunk and lands after both chunks' outputs: on top of the floor
    /// cell's own reaction in the same pass, and of what it already held.
    /// Two chunks' deltas to one cell add up, the sum is clamped with the
    /// excess booked as `saturation_loss`, and the crust test reads the
    /// sum. Across the border the result equals the same layout inside one
    /// chunk, serial equals parallel, and the invariants hold.
    #[test]
    fn coat_across_chunk_border_queued_additive() {
        let t0 = shipped();
        let fe = t0.substance_index("fe").unwrap();
        let amount = 8u8;
        let t = tables_reacting(&[
            reaction(
                "coat",
                &[("fe", amount)],
                vec![Condition::Submerged],
                &[],
                Some(SideEffect::Coat(
                    CoatTarget::Floor,
                    "ochre_crust".into(),
                    "fe".into(),
                    amount,
                )),
            ),
            reaction(
                "rust",
                &[("fe", 3)],
                vec![Condition::Tag("stone".into())],
                &[],
                None,
            ),
        ]);
        let m = t.materials();
        let (stone, ochre) = (m.id("stone").unwrap(), m.id("ochre_crust").unwrap());
        let water = t.role_material("water").unwrap();
        let threshold = i32::from(t.params().crust_threshold);
        let pass = ReactionsPass::new(&t);
        // Two chunks stacked: y 0..=31 and 32..=63.
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(7, 63, 7));
        let (low, high) = (ChunkPos::new(0, 0, 0), ChunkPos::new(0, 1, 0));
        // A water cell over a stone floor; the floor holds two short of the
        // threshold after its own rust: it turns only with the coat added.
        let floor_fe = threshold - i32::from(amount) + 3;
        let build = |water_at: CellPos| {
            let mut w = World::new(Arc::clone(t.materials()), bounds);
            w.set_eco_rules(t.displacement_rules());
            for pos in [low, high] {
                w.insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                    .unwrap();
            }
            let floor = offset(water_at, BELOW);
            let mut ledger = SeasonLedger::default();
            w.eco_set(water_at, water, &mut ledger);
            w.eco_put_layer(water_at, LayerId::Moisture, FULL);
            w.eco_put_layer(water_at, layer(fe), 40);
            w.eco_set(floor, stone, &mut ledger);
            w.eco_put_layer(floor, layer(fe), floor_fe);
            (w, floor)
        };
        let run = |w: &mut World, exec: Exec| {
            let checker = InvariantChecker::new(w);
            let mut ledger = SeasonLedger::default();
            let report = run_jacobi(w, &pass, exec, &mut ledger);
            checker.check(w, &ledger).unwrap();
            (report, ledger)
        };

        // Across the border: the water at the bottom of the upper chunk.
        let src = CellPos::new(3, 32, 3);
        let (mut w, floor) = build(src);
        assert_eq!((src.split().0, floor.split().0), (high, low));
        let (report, ledger) = run(&mut w, Exec::Serial);
        assert_eq!(w.layer(src, layer(fe)), 40 - i32::from(amount));
        assert_eq!(
            w.layer(floor, layer(fe)),
            floor_fe - 3 + i32::from(amount),
            "the coat adds to the floor's own change"
        );
        assert_eq!(
            w.get_cell(floor),
            Some(ochre),
            "the sum reached the threshold"
        );
        assert_eq!(ledger.react_delta[usize::from(fe)], -3);
        assert_eq!(report.changed, vec![low, high]);
        // Parallel gives the same world and ledger.
        let (mut par, _) = build(src);
        let (_, par_ledger) = run(&mut par, Exec::Parallel);
        assert_eq!((par.hash(), par_ledger), (w.hash(), ledger.clone()));
        // A second pass coats the crust once more; it rusts no longer (not
        // stone) and stays ochre.
        run(&mut w, Exec::Serial);
        assert_eq!(
            w.layer(floor, layer(fe)),
            floor_fe - 3 + 2 * i32::from(amount)
        );
        assert_eq!(w.get_cell(floor), Some(ochre));

        // The same layout inside the lower chunk ends the same.
        let inside = CellPos::new(3, 20, 3);
        let (mut one, one_floor) = build(inside);
        assert_eq!((inside.split().0, one_floor.split().0), (low, low));
        let (_, one_ledger) = run(&mut one, Exec::Serial);
        assert_eq!(one_ledger, ledger);
        for (a, b) in [(src, inside), (floor, one_floor)] {
            assert_eq!(one.get_cell(b), par.get_cell(a));
            assert_eq!(one.layer(b, layer(fe)), par.layer(a, layer(fe)));
        }

        // Only the queue reaches the lower chunk: no rust there (iron below
        // what it consumes), so the lower chunk's revision moves only by
        // the queued coat, and the report lists it.
        let (mut w, floor) = build(src);
        w.eco_put_layer(floor, layer(fe), 0);
        let (report, _) = run(&mut w, Exec::Serial);
        assert_eq!(w.layer(floor, layer(fe)), i32::from(amount));
        assert_eq!(w.get_cell(floor), Some(stone));
        assert_eq!(report.changed, vec![high, low]);

        // Two chunks' queued deltas to one cell, with the cell's own delta:
        // all three add before the clamp and the crust test.
        let target = CellPos::new(3, 31, 3);
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        for pos in [low, high] {
            w.insert_chunk(pos, Chunk::uniform(stone)).unwrap();
        }
        w.eco_put_layer(target, layer(fe), 50);
        let each = |then: i32| QueueEach {
            target,
            layer: layer(fe),
            own: -3,
            by_chunk: vec![(low, 5), (high, 7)],
            then_set: Some((ochre, then)),
        };
        // 50 − 3 + 5 + 7 = 59: no two of the three reach it.
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        run_jacobi(&mut w, &each(59), Exec::Parallel, &mut ledger);
        assert_eq!(w.layer(target, layer(fe)), 59);
        assert_eq!(w.get_cell(target), Some(ochre), "converted on the sum");
        checker.check(&w, &ledger).unwrap();
        // Near full: the sum is clamped at 255 and the excess is lost; a
        // threshold above the range never converts.
        let mut w2 = w.clone();
        w2.eco_set(target, stone, &mut SeasonLedger::default());
        w2.eco_put_layer(target, layer(fe), 250);
        let checker = InvariantChecker::new(&w2);
        let mut ledger = SeasonLedger::default();
        run_jacobi(&mut w2, &each(FULL + 1), Exec::Serial, &mut ledger);
        assert_eq!(w2.layer(target, layer(fe)), FULL);
        assert_eq!(
            ledger.saturation_loss[usize::from(fe)],
            i64::from(250 - 3 + 12 - FULL)
        );
        assert_eq!(w2.get_cell(target), Some(stone));
        checker.check(&w2, &ledger).unwrap();
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
