//! Water (SAD-P2 §7.3, sweep): the flow sweeps over the water active set.
//!
//! A visit applies the §7.3 B steps to one cell in order. Material changes
//! go through [`World::eco_set`] (§7.0.1), which runs the displacement
//! rules (moisture stays on the cell, gases leave a cell that turns to
//! water) and writes their ledger terms; the host sends the changed chunks
//! to clients as snapshots after the tick. Steps so far:
//!
//! 1. air with moisture > 0 becomes water;
//! 2. down into air or water, as much as fits; into a porous solid with
//!    room, `min(fill, perm / 2^infil_shift, cap_left)`;
//! 3. sideways, to each of the 4 horizontal neighbors in sorted order
//!    that is air or water with lower fill: `min(flow_cap, (fill −
//!    fill_n) / 2, 255 − fill_n)`; or a porous solid with room:
//!    `min(perm_n / 2^infil_side_shift, cap_left)`, within the fill;
//! 4. a porous cell above its seepage threshold `field_capacity · seep_num
//!    / 256` sheds `min(moisture − threshold, perm / 2^seep_shift)`, within
//!    the receiver's room, to the first neighbor, below first, then the 4
//!    horizontal ones in sorted order, that is air or water with room or
//!    porous with room;
//! 5. water at fill 0 becomes air;
//! 6. magma, a whole-cell liquid, moves on the iterations where
//!    `iter % (viscosity + 1) == 0` ([`WaterPass::set_iter`]): it swaps
//!    with the air below, else with the first air cell of the 4
//!    horizontal neighbors in sorted order, taking its temperature along
//!    (§7.1). It never enters water, porous solids or another liquid.
//!    Magma is conserved as a cell count; a swap neither sets nor melts
//!    any;
//! 7. contact boiling, on every iteration: magma that is still in place
//!    after step 6 and has a water face neighbor (the first of the 6 in
//!    sorted order, below first) boils that cell's whole fill to steam on
//!    the cell, which becomes air, and sets as obsidian in place; ledger
//!    `boil_to_steam` and `magma_set`. One event per visit.
//!
//! A porous solid is a `Solid` material tagged `porous`; its moisture is
//! the water it holds, up to its `field_capacity` (`cap_left` is the
//! difference). Every move reads the donor's content and the receiver's
//! room from the current state just before it is applied, and the
//! receiver joins the active set. Moves inside the water total need no
//! ledger entry: water that changes cells is neither a source nor a sink.
//! [`reference::flat_sweep_water`] is the flat-array oracle (without magma
//! and contact boiling).
//!
//! Around the flow sweeps, once per sub-step, run the other §7.3 steps:
//! [`Condensation`] (A) before them, then [`Evaporation`] (C) and
//! [`Rain`] (D), whose per-column amount follows [`rain_pattern`].
//!
//! A water or porous cell whose visit moved nothing is marked settled
//! ([`World::eco_settle_water`]), and the flow sweeps skip it until it or
//! a face neighbor changes material or moisture, which the world tracks.
//! Its visit reads only those 7 cells (and, for magma, which never
//! settles, the iteration), so a skipped visit is one that would have
//! moved nothing (F173, SAD-P2 §7.9 activity).
//!
//! [`reference::flat_sweep_water`]: crate::reference::flat_sweep_water

use std::collections::BTreeSet;

use rmc_world::chunk::Chunk;
use rmc_world::columns::NO_SURFACE;
use rmc_world::coords::{CellAabb, CellPos};
use rmc_world::layers::{CellSet, LayerId, SWEEP_WATER};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::world::World;
use xxhash_rust::xxh3::Xxh3;

use crate::chemistry::{Move, MoveLog};
use crate::gases::GasPass;
use crate::reference::global_order;
use crate::runner::{SweepCtx, SweepPass};
use crate::tables::{EcoTables, SEEP_DEN};

#[cfg(test)]
thread_local! {
    /// Tests turn settling off on their thread (the sweeps run on the
    /// caller's) to compare with sweeps that visit every active cell.
    pub(crate) static SETTLING: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

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
    /// `eco.infil_shift`: infiltration down is `perm / 2^infil_shift`.
    infil_shift: u8,
    /// `eco.infil_side_shift`: infiltration sideways.
    infil_side_shift: u8,
    /// `eco.seep_shift`: seepage out of a porous cell.
    seep_shift: u8,
    /// Per material id: the porous solid's constants, `None` for every
    /// other material.
    porous: Vec<Option<Porous>>,
    /// Role `magma`.
    magma: Option<MaterialId>,
    /// Role `obsidian`: what magma sets to by contact boiling.
    obsidian: Option<MaterialId>,
    /// Magma's `viscosity + 1`: it moves on iterations that are a multiple
    /// of this.
    magma_period: u16,
    /// The flow iteration of the current sweep within the sub-step.
    iter: u8,
    /// The moves of steps 2–4 and 7 since the log was last cleared, for
    /// chemistry transport (§7.6).
    log: MoveLog,
    /// A hash of every constant above that a visit reads: settled cells
    /// hold for a pass with the same key.
    settle_key: u64,
}

/// The constants of a porous solid (a `Solid` tagged `porous`).
#[derive(Clone, Copy, Debug, Hash)]
struct Porous {
    permeability: u8,
    /// `field_capacity`: the most moisture it takes in.
    capacity: i32,
    /// `field_capacity · seep_num / 256`: it sheds moisture above this.
    threshold: i32,
}

/// The tag that makes a solid porous (SAD-P2 §5).
const POROUS: &str = "porous";
/// The most water a cell holds.
const FULL: i32 = u8::MAX as i32;
/// The divisor of the sideways move, `(fill − fill_n) / 2` (§7.3 B3).
const SIDE_SPLIT: i32 = 2;
/// The cell below.
const BELOW: [i32; 3] = [0, -1, 0];
/// The 4 horizontal neighbors in sorted (y, z, x) order.
const SIDEWAYS: [[i32; 3]; 4] = [[0, 0, -1], [-1, 0, 0], [1, 0, 0], [0, 0, 1]];
/// Where seepage goes, in the order it is offered: below first, then the
/// 4 horizontal neighbors in sorted order (§7.3 B4).
const SEEP_ORDER: [[i32; 3]; 5] = [BELOW, SIDEWAYS[0], SIDEWAYS[1], SIDEWAYS[2], SIDEWAYS[3]];
/// The 6 face neighbors in sorted (y, z, x) order (§7.3 B7).
const FACES: [[i32; 3]; 6] = [
    BELOW,
    SIDEWAYS[0],
    SIDEWAYS[1],
    SIDEWAYS[2],
    SIDEWAYS[3],
    [0, 1, 0],
];

fn offset(cell: CellPos, d: [i32; 3]) -> CellPos {
    CellPos::new(cell.0.x + d[0], cell.0.y + d[1], cell.0.z + d[2])
}

fn moisture(world: &World, cell: CellPos) -> i32 {
    world.layer(cell, LayerId::Moisture)
}

/// The material and moisture of `cell`, read with one chunk lookup;
/// `None` outside the bounds and in a non-resident chunk.
fn material_and_moisture(world: &World, cell: CellPos) -> Option<(MaterialId, i32)> {
    if !world.bounds().contains(cell) {
        return None;
    }
    let (pos, local) = cell.split();
    let chunk = world.chunk(pos)?;
    let held = chunk.layers.get_u8(LayerId::Moisture, local);
    Some((chunk.storage.get(local), i32::from(held)))
}

/// `perm / 2^shift`.
fn rate(perm: u8, shift: u8) -> i32 {
    (u32::from(perm) >> shift) as i32
}

impl WaterPass {
    pub fn new(tables: &EcoTables) -> Self {
        let params = tables.params();
        let porous = tables
            .materials()
            .iter()
            .map(|d| {
                (d.phase == Phase::Solid && d.has_tag(POROUS)).then(|| {
                    let capacity = i32::from(d.field_capacity);
                    Porous {
                        permeability: d.permeability,
                        capacity,
                        threshold: capacity * i32::from(params.seep_num) / i32::from(SEEP_DEN),
                    }
                })
            })
            .collect();
        let magma = tables.role_material("magma");
        let viscosity = magma
            .and_then(|m| tables.materials().get(m))
            .map_or(0, |d| d.viscosity);
        let mut pass = Self {
            air: tables.role_material("air"),
            water: tables.role_material("water"),
            flow_iters: params.flow_iters,
            flow_cap: i32::from(params.flow_cap),
            infil_shift: params.infil_shift,
            infil_side_shift: params.infil_side_shift,
            seep_shift: params.seep_shift,
            porous,
            magma,
            obsidian: tables.role_material("obsidian"),
            magma_period: u16::from(viscosity) + 1,
            iter: 0,
            log: MoveLog::default(),
            settle_key: 0,
        };
        pass.settle_key = pass.constants_key(tables);
        pass
    }

    /// A hash of the constants a visit reads, including which materials
    /// are air.
    fn constants_key(&self, tables: &EcoTables) -> u64 {
        use std::hash::{Hash, Hasher};

        /// Feeds `Hash` values to xxh3.
        struct Feed(Xxh3);
        impl Hasher for Feed {
            fn finish(&self) -> u64 {
                self.0.digest()
            }
            fn write(&mut self, bytes: &[u8]) {
                self.0.update(bytes);
            }
        }
        let mut h = Feed(Xxh3::new());
        (self.air, self.water, self.magma, self.obsidian).hash(&mut h);
        (self.flow_cap, self.infil_shift, self.infil_side_shift).hash(&mut h);
        (self.seep_shift, self.magma_period).hash(&mut h);
        self.porous.hash(&mut h);
        let air: Vec<bool> = tables
            .materials()
            .iter()
            .map(|d| d.phase == Phase::Air)
            .collect();
        air.hash(&mut h);
        h.finish()
    }

    /// Marks `cell` settled after a visit that moved nothing.
    fn settle(world: &mut World, cell: CellPos) {
        #[cfg(test)]
        if !SETTLING.get() {
            return;
        }
        world.eco_settle_water(cell);
    }

    /// Tells the pass which of the sub-step's `flow_iters` sweeps runs
    /// next (0-based); magma moves only on some of them (§7.3 B6).
    pub fn set_iter(&mut self, iter: u8) {
        self.iter = iter;
    }

    /// Flow sweeps per sub-step (`eco.flow_iters`).
    pub fn flow_iters(&self) -> u8 {
        self.flow_iters
    }

    /// The moves of steps 2–4 and 7 (§7.3) since the last
    /// [`clear_log`](Self::clear_log), in the order they were made: a
    /// move as `(from, to, amount, fill_from)`, a contact boil as a self
    /// entry `(cell, cell, fill, fill)`.
    pub fn log(&self) -> &MoveLog {
        &self.log
    }

    /// Empties the move log; the tick does it after chemistry transport
    /// has replayed a sub-step's moves.
    pub fn clear_log(&mut self) {
        self.log.clear();
    }

    fn is_air(world: &World, id: MaterialId) -> bool {
        world
            .materials()
            .get(id)
            .is_some_and(|d| d.phase == Phase::Air)
    }

    fn porous(&self, id: MaterialId) -> Option<Porous> {
        self.porous.get(usize::from(id.0)).copied().flatten()
    }

    /// What `cell` is to water moving into it: open (air or water) or
    /// porous; `None` for anything else, outside the bounds and in a
    /// non-resident chunk.
    fn receiver(&self, world: &World, cell: CellPos) -> Option<Receiver> {
        let (m, held) = material_and_moisture(world, cell)?;
        if let Some(porous) = self.porous(m) {
            return Some(Receiver::Porous { held, porous });
        }
        let air = Self::is_air(world, m);
        (air || Some(m) == self.water).then_some(Receiver::Open { fill: held, air })
    }

    /// Moves `amount` of water from `from` (a water or porous cell and its
    /// current moisture) to `to` (a receiver whose room the caller
    /// checked), logs the move and returns the donor's new moisture. Air
    /// becomes water first (§7.0.1: gases leave it). Nothing happens for
    /// an amount of 0.
    fn give(
        &mut self,
        ctx: &mut SweepCtx<'_>,
        (from, held): (CellPos, i32),
        (to, receiver): (CellPos, Receiver),
        amount: i32,
    ) -> i32 {
        if amount <= 0 {
            return held;
        }
        debug_assert!(amount <= held && amount <= receiver.room());
        if let Receiver::Open { air: true, .. } = receiver {
            let water = self.water.expect("flow runs only with a water role");
            ctx.world.eco_set(to, water, ctx.ledger);
        }
        ctx.world
            .eco_put_layer(from, LayerId::Moisture, held - amount);
        ctx.world
            .eco_put_layer(to, LayerId::Moisture, receiver.held() + amount);
        ctx.receive(to);
        self.log.push(Move {
            from,
            to,
            amount: u8::try_from(amount).expect("a move fits a cell"),
            fill_from: u8::try_from(held).expect("moisture fits a cell"),
        });
        held - amount
    }

    /// Step 6, magma, on every `viscosity + 1`-th iteration: swap with the
    /// air below, else with the first air neighbor of the 4 horizontal
    /// ones in sorted order. Anything else (water, porous solids, other
    /// liquids, solids) blocks it.
    fn flow_magma(&self, ctx: &mut SweepCtx<'_>, cell: CellPos, air: MaterialId) {
        if u16::from(self.iter) % self.magma_period != 0 {
            return;
        }
        let to = std::iter::once(BELOW)
            .chain(SIDEWAYS)
            .map(|d| offset(cell, d))
            .find(|&n| {
                ctx.world
                    .get_cell(n)
                    .is_some_and(|id| Self::is_air(ctx.world, id))
            });
        if let Some(to) = to {
            self.swap_magma(ctx, cell, to, air);
        }
    }

    /// Swaps the magma at `cell` with the air at `to`, materials and
    /// temperatures (§7.1 advection). Both changes go through `eco_set`,
    /// so the air's gases and moisture are displaced as for any cell that
    /// stops being air (§7.0), into the cell the magma left when it is the
    /// first with room. `eco_set` books the two halves as a set and a
    /// melt; a swap is neither, so the pass takes both back and the magma
    /// count stays balanced.
    fn swap_magma(&self, ctx: &mut SweepCtx<'_>, cell: CellPos, to: CellPos, air: MaterialId) {
        let magma = self.magma.expect("only magma cells swap");
        let (t_magma, t_air) = (
            ctx.world.layer(cell, LayerId::Temperature),
            ctx.world.layer(to, LayerId::Temperature),
        );
        ctx.world.eco_set(cell, air, ctx.ledger);
        ctx.world.eco_set(to, magma, ctx.ledger);
        ctx.ledger.magma_set -= 1;
        ctx.ledger.magma_melt_in -= 1;
        ctx.world.eco_put_layer(cell, LayerId::Temperature, t_air);
        ctx.world.eco_put_layer(to, LayerId::Temperature, t_magma);
        ctx.receive(to);
    }

    /// Step 7, contact boiling: magma at `cell` with a water face neighbor
    /// boils the first one's whole fill to steam on that cell, which
    /// becomes air, and sets as obsidian. `eco_set` runs the §7.0 rules
    /// for both changes: water → air by a pass converts the fill (ledger
    /// `boil_to_steam`), magma → obsidian leaves the magma count (ledger
    /// `magma_set`); both keep their temperatures.
    fn contact_boil(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos, water: MaterialId) {
        let Some(obsidian) = self.obsidian else {
            return;
        };
        let Some(wet) = FACES
            .iter()
            .map(|&d| offset(cell, d))
            .find(|&n| ctx.world.get_cell(n) == Some(water))
        else {
            return;
        };
        let air = self.air.expect("flow runs only with an air role");
        let fill = u8::try_from(moisture(ctx.world, wet)).expect("moisture fits a cell");
        if fill > 0 {
            self.log.push(Move {
                from: wet,
                to: wet,
                amount: fill,
                fill_from: fill,
            });
        }
        ctx.world.eco_set(wet, air, ctx.ledger);
        ctx.world.eco_set(cell, obsidian, ctx.ledger);
    }

    /// Step 4, seepage: a porous cell above its threshold sheds the
    /// excess, at most `perm / 2^seep_shift`, to the first neighbor with
    /// room.
    fn seep(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos, held: i32, p: Porous) {
        let most = (held - p.threshold).min(rate(p.permeability, self.seep_shift));
        if most <= 0 {
            return;
        }
        for d in SEEP_ORDER {
            let n = offset(cell, d);
            let Some(receiver) = self.receiver(ctx.world, n) else {
                continue;
            };
            let room = receiver.room();
            if room > 0 {
                self.give(ctx, (cell, held), (n, receiver), most.min(room));
                return;
            }
        }
    }
}

/// A cell water can move into, as read just before the move.
#[derive(Clone, Copy, Debug)]
enum Receiver {
    /// Air (becomes water when it receives) or water, and its fill.
    Open { fill: i32, air: bool },
    /// A porous solid and the moisture it holds.
    Porous { held: i32, porous: Porous },
}

impl Receiver {
    /// The water the cell holds now.
    fn held(self) -> i32 {
        match self {
            Receiver::Open { fill, .. } => fill,
            Receiver::Porous { held, .. } => held,
        }
    }

    /// How much more it takes: up to 255, or up to the field capacity.
    fn room(self) -> i32 {
        match self {
            Receiver::Open { fill, .. } => FULL - fill,
            Receiver::Porous { held, porous } => (porous.capacity - held).max(0),
        }
    }
}

impl SweepPass for WaterPass {
    fn kind(&self) -> usize {
        SWEEP_WATER
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        let (Some(air), Some(water)) = (self.air, self.water) else {
            return;
        };
        // The cell's moisture, kept current through its moves: only they
        // change it during the visit.
        let Some((m, mut fill)) = material_and_moisture(ctx.world, cell) else {
            return;
        };
        // Every move is logged, so an unchanged log length means no move.
        let logged = self.log.len();
        if let Some(p) = self.porous(m) {
            self.seep(ctx, cell, fill, p);
            if self.log.len() == logged {
                Self::settle(ctx.world, cell);
            }
            return;
        }
        if Some(m) == self.magma {
            self.flow_magma(ctx, cell, air);
            if ctx.world.get_cell(cell) == Some(m) {
                self.contact_boil(ctx, cell, water);
            }
            return;
        }
        // 1. Air with moisture becomes water.
        let became_water = Self::is_air(ctx.world, m) && fill > 0;
        if became_water {
            ctx.world.eco_set(cell, water, ctx.ledger);
        } else if m != water {
            return;
        }
        // 2. Down: as much as the cell below has room for; into a porous
        // cell, at most `perm / 2^infil_shift`.
        let below = offset(cell, BELOW);
        if let Some(receiver) = self.receiver(ctx.world, below) {
            let amount = match receiver {
                Receiver::Open { .. } => fill,
                Receiver::Porous { porous, .. } => {
                    fill.min(rate(porous.permeability, self.infil_shift))
                }
            };
            let amount = amount.min(receiver.room());
            fill = self.give(ctx, (cell, fill), (below, receiver), amount);
        }
        // 3. Sideways: half the difference to each lower open neighbor in
        // sorted order, at most `flow_cap`; into a porous neighbor, at most
        // `perm / 2^infil_side_shift`; always within the room.
        for d in SIDEWAYS {
            let n = offset(cell, d);
            let Some(receiver) = self.receiver(ctx.world, n) else {
                continue;
            };
            let amount = match receiver {
                Receiver::Open { fill: fill_n, .. } if fill_n < fill => {
                    self.flow_cap.min((fill - fill_n) / SIDE_SPLIT)
                }
                Receiver::Open { .. } => 0,
                Receiver::Porous { porous, .. } => {
                    fill.min(rate(porous.permeability, self.infil_side_shift))
                }
            };
            let amount = amount.min(receiver.room());
            fill = self.give(ctx, (cell, fill), (n, receiver), amount);
        }
        // 5. Water at fill 0 becomes air.
        if fill == 0 {
            ctx.world.eco_set(cell, air, ctx.ledger);
        } else if !became_water && self.log.len() == logged {
            Self::settle(ctx.world, cell);
        }
    }

    /// Settled cells hold for this pass's constants only: a pass with
    /// other constants starts with none.
    fn begin(&mut self, world: &mut World) {
        if world.water_settle_key() != self.settle_key {
            world.eco_clear_water_settled(self.settle_key);
        }
    }

    fn settled<'c>(&self, chunk: &'c Chunk) -> Option<&'c CellSet> {
        Some(&chunk.water_settled)
    }

    /// Water cells, moist air, magma, and porous cells above their seepage
    /// threshold (at or below it, their visit moves nothing; a porous cell
    /// that receives water is visited and leaves again if it is still at
    /// or below). Ice keeps its moisture in place, so it leaves the set.
    fn holds(&self, world: &World, cell: CellPos) -> bool {
        let Some(m) = world.get_cell(cell) else {
            return false;
        };
        if let Some(p) = self.porous(m) {
            return moisture(world, cell) > p.threshold;
        }
        Some(m) == self.water
            || Some(m) == self.magma
            || (Self::is_air(world, m) && moisture(world, cell) > 0)
    }
}

/// The divisor of the rain amount, `rain · pattern / 256` (§7.3 D).
const PATTERN_DEN: i32 = 256;
/// The cell above.
const ABOVE: [i32; 3] = [0, 1, 0];

/// Step A, condensation (SAD-P2 §7.3), once per sub-step before the flow
/// sweeps: one sweep over the steam active set. An air cell holding steam
/// that is colder than `eco.condense_temp` and not sky-open turns
/// `min(steam, 255 − moisture)` of it into moisture on itself (the first
/// flow sweep makes it water); ledger `condense_from_steam`. Steam that
/// finds no room stays steam. Sky-open steam (`y > surface_y`) is left to
/// the steam sweep's vent step (§7.4): above an open pond it vents
/// instead of condensing back.
#[derive(Clone, Debug)]
pub struct Condensation {
    /// The steam sweep: its active set and which cells hold steam.
    steam: GasPass,
    /// `eco.condense_temp`, 0.1 °C.
    condense_temp: i32,
}

impl Condensation {
    /// `None` without a `steam` gas role.
    pub fn new(tables: &EcoTables) -> Option<Self> {
        let steam = tables.role_gas("steam")?;
        Some(Self {
            steam: GasPass::new(tables, steam),
            condense_temp: i32::from(tables.params().condense_temp),
        })
    }
}

impl SweepPass for Condensation {
    fn kind(&self) -> usize {
        self.steam.kind()
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        if !self.steam.holds(ctx.world, cell) {
            return;
        }
        let sky_open = cell.0.y > i32::from(ctx.world.surface_y(cell.0.x, cell.0.z));
        if sky_open || ctx.world.layer(cell, LayerId::Temperature) >= self.condense_temp {
            return;
        }
        let layer = LayerId::Gas(self.steam.gas());
        let steam = ctx.world.layer(cell, layer);
        let held = moisture(ctx.world, cell);
        let amount = steam.min(FULL - held);
        if amount <= 0 {
            return;
        }
        ctx.world.eco_put_layer(cell, layer, steam - amount);
        // Moist air joins the water sweep (`eco_put_layer` adds it).
        ctx.world
            .eco_put_layer(cell, LayerId::Moisture, held + amount);
        ctx.ledger.condense_from_steam += i64::from(amount);
    }

    /// Air cells holding steam, as for the steam sweep.
    fn holds(&self, world: &World, cell: CellPos) -> bool {
        self.steam.holds(world, cell)
    }
}

/// Step C, evaporation (SAD-P2 §7.3), once per sub-step after the flow
/// sweeps: one sweep over the water active set. A water cell whose cell
/// above is air moves `e = min(evap_table[bucket(T)], fill, 255 −
/// steam_above)` of its fill into the steam above; ledger `evap_to_steam`
/// records `e`. `T` is the water cell's temperature; its bucket is `T /
/// evap_bucket`, 0 below 0 °C and the table's last entry past its end. A
/// cell left at fill 0 becomes air at once (§7.3 B5). The cells it
/// evaporated from are kept for the reactions' `Evaporating` condition
/// (§7.6) until [`Evaporation::take_evaporated`].
#[derive(Clone, Debug)]
pub struct Evaporation {
    /// The flow sweep, for which cells the water active set holds.
    flow: WaterPass,
    water: MaterialId,
    air: MaterialId,
    /// The steam gas's layer index.
    steam: u8,
    /// `eco.evap_table`.
    table: Vec<i32>,
    /// `eco.evap_bucket`, 0.1 °C.
    bucket: i32,
    /// Cells evaporated from since the last `take_evaporated`.
    evaporated: BTreeSet<CellPos>,
}

impl Evaporation {
    /// `None` without the `steam` gas role or the `water` and `air`
    /// material roles.
    pub fn new(tables: &EcoTables) -> Option<Self> {
        let params = tables.params();
        Some(Self {
            flow: WaterPass::new(tables),
            water: tables.role_material("water")?,
            air: tables.role_material("air")?,
            steam: tables.role_gas("steam")?,
            table: params.evap_table.iter().map(|&e| i32::from(e)).collect(),
            bucket: i32::from(params.evap_bucket),
            evaporated: BTreeSet::new(),
        })
    }

    /// The cells evaporated from since the last call, which empties the
    /// record: once per sub-step, for the reactions (§7.6 `Evaporating`).
    pub fn take_evaporated(&mut self) -> BTreeSet<CellPos> {
        std::mem::take(&mut self.evaporated)
    }

    /// The table's rate at temperature `t`.
    fn rate(&self, t: i32) -> i32 {
        let i = usize::try_from(t / self.bucket).unwrap_or(0);
        self.table
            .get(i)
            .or(self.table.last())
            .copied()
            .unwrap_or(0)
    }
}

impl SweepPass for Evaporation {
    fn kind(&self) -> usize {
        SWEEP_WATER
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        if ctx.world.get_cell(cell) != Some(self.water) {
            return;
        }
        let above = offset(cell, ABOVE);
        let open = ctx
            .world
            .get_cell(above)
            .is_some_and(|m| WaterPass::is_air(ctx.world, m));
        if !open {
            return;
        }
        let layer = LayerId::Gas(self.steam);
        let fill = moisture(ctx.world, cell);
        let steam = ctx.world.layer(above, layer);
        let rate = self.rate(ctx.world.layer(cell, LayerId::Temperature));
        let amount = rate.min(fill).min(FULL - steam);
        if amount <= 0 {
            return;
        }
        ctx.world
            .eco_put_layer(cell, LayerId::Moisture, fill - amount);
        // The steam joins the steam sweep (`eco_put_layer` adds it).
        ctx.world.eco_put_layer(above, layer, steam + amount);
        ctx.ledger.evap_to_steam += i64::from(amount);
        self.evaporated.insert(cell);
        if fill == amount {
            ctx.world.eco_set(cell, self.air, ctx.ledger);
        }
    }

    /// The water active set's cells, as for the flow sweeps.
    fn holds(&self, world: &World, cell: CellPos) -> bool {
        self.flow.holds(world, cell)
    }
}

/// Step D, rain (SAD-P2 §7.3), once per sub-step after evaporation, per
/// column in the global order of a y-plane. The target is the cell at
/// `surface_y + 1` when it is air (it becomes water, §7.0) or water; when
/// it is a plant (a passable solid above the surface) the target is the
/// porous anchor below it instead. No surface, a target above the world's
/// top or outside the resident chunks, a plant on a non-porous anchor, or
/// anything else at `surface_y + 1` gets no rain. The amount is `min(rain ·
/// pattern / 256, room)` with `room` = `255 − moisture` (air, water) or
/// `field_capacity − moisture` (porous); ledger `rain_in` records it.
#[derive(Clone, Debug)]
pub struct Rain {
    /// The flow sweep, for the roles and the porous solids.
    flow: WaterPass,
    /// Per material id: whether it is a plant (the material of a
    /// `plants.ron` row).
    plant: Vec<bool>,
    /// The season row's `rain`.
    rain: u8,
    season: u32,
    seed: u64,
}

impl Rain {
    /// The rain of season `season` (row `season % len` of `seasons.ron`),
    /// patterned by the world seed `seed`.
    pub fn new(tables: &EcoTables, season: u32, seed: u64) -> Self {
        let rows = &tables.seasons().seasons;
        let m = tables.materials();
        let mut plant = vec![false; m.len()];
        for id in tables.plants().iter().filter_map(|p| m.id(&p.material)) {
            plant[usize::from(id.0)] = true;
        }
        Self {
            flow: WaterPass::new(tables),
            plant,
            rain: rows[season as usize % rows.len()].rain,
            season,
            seed,
        }
    }

    fn is_plant(&self, id: MaterialId) -> bool {
        self.plant.get(usize::from(id.0)).copied().unwrap_or(false)
    }

    /// Where the rain of column `(x, z)` lands: the cell, what it holds,
    /// its room and whether it is air (to become water).
    fn target(&self, world: &World, x: i32, z: i32) -> Option<(CellPos, i32, i32, bool)> {
        let water = self.flow.water?;
        let surface = world.surface_y(x, z);
        if surface == NO_SURFACE {
            return None;
        }
        let surface = i32::from(surface);
        let above = CellPos::new(x, surface + 1, z);
        let m = world.get_cell(above)?;
        let air = WaterPass::is_air(world, m);
        if air || m == water {
            let held = moisture(world, above);
            return Some((above, held, FULL - held, air));
        }
        if !self.is_plant(m) {
            return None;
        }
        let anchor = CellPos::new(x, surface, z);
        let porous = world.get_cell(anchor).and_then(|a| self.flow.porous(a))?;
        let held = moisture(world, anchor);
        Some((anchor, held, (porous.capacity - held).max(0), false))
    }

    /// Rains on every column of `world` and returns how many columns
    /// received water.
    pub fn run(&self, world: &mut World, ledger: &mut SeasonLedger) -> u32 {
        let Some(water) = self.flow.water else {
            return 0;
        };
        if self.rain == 0 {
            return 0;
        }
        let b = world.bounds();
        let plane = CellAabb::new(b.min, CellPos::new(b.max.0.x, b.min.0.y, b.max.0.z));
        let mut columns = 0;
        for col in global_order(plane) {
            let (x, z) = (col.0.x, col.0.z);
            let Some((cell, _, room, air)) = self.target(world, x, z) else {
                continue;
            };
            let pattern = i32::from(rain_pattern(self.seed, self.season, x, z));
            let amount = (i32::from(self.rain) * pattern / PATTERN_DEN).min(room);
            if amount <= 0 {
                continue;
            }
            if air {
                world.eco_set(cell, water, ledger);
            }
            let held = moisture(world, cell);
            world.eco_put_layer(cell, LayerId::Moisture, held + amount);
            ledger.rain_in += i64::from(amount);
            columns += 1;
        }
        columns
    }
}

/// The seeded rain pattern of column `(x, z)` in season `season`, 0..=255
/// (SAD-P2 §7.3 D; §16: a seeded hash per column).
pub fn rain_pattern(seed: u64, season: u32, x: i32, z: i32) -> u8 {
    let mut h = Xxh3::with_seed(seed);
    h.update(&season.to_le_bytes());
    h.update(&x.to_le_bytes());
    h.update(&z.to_le_bytes());
    h.digest().to_le_bytes()[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Arc;

    use proptest::prelude::*;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, ChunkPos, LocalPos};
    use rmc_world::layers::SWEEP_GAS;
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::materials::MaterialTable;

    use crate::invariants::InvariantChecker;
    use crate::reference::{FlatWorld, WaterRef, flat_sweep_water, global_order};
    use crate::runner::{activate_holders, run_sweep};
    use crate::tables::SeasonOverride;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    /// Top of the stone slab the pockets are cut into.
    const SLAB_Y: i32 = 8;

    /// Permeability of the test loam: infiltration 64 / 2^2 = 16 down,
    /// 64 / 2^3 = 8 sideways, seepage 64 / 2^3 = 8 (shipped `eco.ron`).
    const LOAM_PERM: i32 = 64;
    /// Field capacity of the test loam and hardpan; the seepage threshold
    /// is 48 · 192 / 256 = 36.
    const LOAM_CAP: i32 = 48;

    struct Fix {
        t: EcoTables,
        air: MaterialId,
        water: MaterialId,
        /// Porous, permeability `LOAM_PERM`, field capacity `LOAM_CAP`.
        loam: MaterialId,
        /// Porous with permeability 0.
        hardpan: MaterialId,
        /// Role `magma`: the shipped row, or the SAD-P2 §5 row (Liquid,
        /// viscosity 3) with F131's heat capacity 64.
        magma: MaterialId,
        /// A liquid of another kind than water or magma.
        brine: MaterialId,
        /// A passable plant material (plants.ron's `moss` row).
        moss: MaterialId,
        /// Role `obsidian`: what magma sets to by contact boiling.
        obsidian: MaterialId,
    }

    impl Fix {
        /// The shipped tables, with two porous test materials, a test
        /// liquid, a passable moss and, if the table lacks them, magma (the
        /// SAD-P2 §5 row) and obsidian appended to the shipped material
        /// table.
        fn new() -> Self {
            let base = std::fs::read_to_string(format!("{ASSETS}/materials.ron")).unwrap();
            let n = MaterialTable::from_ron_str(&base).unwrap().len();
            let row = |id: usize, key: &str, perm: i32| {
                format!(
                    "(id: {id}, key: \"{key}\", phase: Solid, tags: [\"soil\", \"porous\"], \
                     hardness: 10, diggable: true, yield_count: 1, color: (110, 80, 50, 255), \
                     emissive: 0.0, permeability: {perm}, field_capacity: {LOAM_CAP}),\n"
                )
            };
            let liquid = |id: usize, key: &str, extra: &str| {
                format!(
                    "(id: {id}, key: \"{key}\", phase: Liquid, tags: [], hardness: 0,                      diggable: false, yield_count: 0, color: (255, 90, 0, 255),                      emissive: 2.0{extra}),
"
                )
            };
            // A passable plant: the material of plants.ron's `moss` row.
            let plant = |id: usize, key: &str| {
                format!(
                    "(id: {id}, key: \"{key}\", phase: Solid, tags: [\"plant\"], hardness: 1, \
                     diggable: true, yield_count: 1, color: (60, 140, 60, 255), \
                     emissive: 0.0, passable: true),\n"
                )
            };
            // Rows in id order from n; magma, moss and obsidian only if
            // the shipped table lacks them.
            let has = |key: &str| base.contains(&format!("key: \"{key}\""));
            let mut rows: Vec<Box<dyn Fn(usize) -> String>> = vec![
                Box::new(|id| row(id, "test_loam", LOAM_PERM)),
                Box::new(|id| row(id, "test_hardpan", 0)),
            ];
            if !has("magma") {
                rows.push(Box::new(|id| {
                    liquid(
                        id,
                        "magma",
                        ", viscosity: 3, conductivity: 64, heat_capacity: 64",
                    )
                }));
            }
            rows.push(Box::new(|id| liquid(id, "test_brine", "")));
            if !has("moss") {
                rows.push(Box::new(|id| plant(id, "moss")));
            }
            if !has("obsidian") {
                rows.push(Box::new(|id| {
                    format!(
                        "(id: {id}, key: \"obsidian\", phase: Solid, tags: [], hardness: 70,                          diggable: true, yield_count: 1, color: (30, 20, 40, 255),                          emissive: 0.0),
"
                    )
                }));
            }
            let end = base.rfind(']').expect("material list closes");
            let appended: String = rows.iter().enumerate().map(|(i, r)| r(n + i)).collect();
            let text = format!("{}{appended}]", &base[..end]);
            let m = Arc::new(MaterialTable::from_ron_str(&text).expect("test table loads"));
            let t = EcoTables::load_with_materials(m, ASSETS).expect("Phase 2 tables load");
            let water = t.role_material("water").expect("materials.ron has water");
            let air = t.role_material("air").expect("materials.ron has air");
            let loam = t.materials().id("test_loam").unwrap();
            let hardpan = t.materials().id("test_hardpan").unwrap();
            let magma = t.role_material("magma").expect("the magma row resolves");
            let brine = t.materials().id("test_brine").unwrap();
            let moss = t.materials().id("moss").unwrap();
            let obsidian = t
                .role_material("obsidian")
                .expect("the obsidian row resolves");
            Self {
                t,
                air,
                water,
                loam,
                hardpan,
                magma,
                brine,
                moss,
                obsidian,
            }
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

    /// The seepage threshold of the test loam: `field_capacity · seep_num /
    /// 256`.
    fn loam_threshold(f: &Fix) -> i32 {
        LOAM_CAP * i32::from(f.t.params().seep_num) / 256
    }

    #[test]
    fn porous_column_fills_to_capacity_and_stops() {
        let f = Fix::new();
        // A column of 4 loam cells under a one-cell pocket of water in the
        // top slab layer, stone all around. The column holds 4 · 48 = 192,
        // so 255 − 192 = 63 stays standing in the pocket.
        let pocket = CellPos::new(10, SLAB_Y, 10);
        let column: Vec<CellPos> = (SLAB_Y - 4..SLAB_Y)
            .map(|y| CellPos::new(10, y, 10))
            .collect();
        let mut cells: Vec<(CellPos, MaterialId, i32)> =
            column.iter().map(|&c| (c, f.loam, 0)).collect();
        cells.push((pocket, f.water, 255));
        let (mut w, mut pass) = pocket_world(&f, &cells);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();

        // One sweep: the top cell takes perm / 2^infil_shift = 16.
        run_sweep(&mut w, &mut pass, &mut ledger);
        assert_eq!(fill(&w, column[3]), LOAM_PERM / 4, "infiltration rate");
        assert_eq!(fill(&w, pocket), 255 - LOAM_PERM / 4);
        checker.check(&w, &ledger).expect("water conserved");

        let mut sweeps = 1;
        while column.iter().any(|&c| fill(&w, c) < LOAM_CAP) {
            assert!(sweeps < 200, "column not full after {sweeps} sweeps");
            run_sweep(&mut w, &mut pass, &mut ledger);
            checker.check(&w, &ledger).expect("water conserved");
            for &c in &column {
                assert!(fill(&w, c) <= LOAM_CAP, "{c:?} above field capacity");
            }
            sweeps += 1;
        }
        assert_eq!(fill(&w, pocket), 255 - 4 * LOAM_CAP, "the rest stands");
        assert_eq!(w.get_cell(pocket), Some(f.water));

        // Full: nothing moves any more.
        let hash = w.hash();
        for _ in 0..16 {
            run_sweep(&mut w, &mut pass, &mut ledger);
        }
        assert_eq!(w.hash(), hash, "a full column stops taking water");
        checker.check(&w, &ledger).expect("water conserved");
        assert_eq!(ledger, SeasonLedger::default());
    }

    #[test]
    fn seepage_into_pit_below_table() {
        let f = Fix::new();
        let threshold = loam_threshold(&f);
        // Two 9 × 9 × 6 loam bodies in the top of the slab: one below the
        // water table (every cell at field capacity), one dry. A 3 × 3 pit,
        // 3 deep, is dug into the middle of each.
        let body = |x0: i32| {
            (SLAB_Y - 5..=SLAB_Y).flat_map(move |y| {
                (6..=14).flat_map(move |z| (x0..x0 + 9).map(move |x| CellPos::new(x, y, z)))
            })
        };
        let in_pit = |x0: i32, c: CellPos| {
            c.0.y >= SLAB_Y - 2 && (x0 + 3..=x0 + 5).contains(&c.0.x) && (9..=11).contains(&c.0.z)
        };
        let (wet_x, dry_x) = (2, 18);
        let mut cells = Vec::new();
        for (x0, v) in [(wet_x, LOAM_CAP), (dry_x, 0)] {
            for c in body(x0) {
                if in_pit(x0, c) {
                    cells.push((c, f.air, 0));
                } else {
                    cells.push((c, f.loam, v));
                }
            }
        }
        let (mut w, mut pass) = pocket_world(&f, &cells);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let pit = |x0: i32| body(x0).filter(move |&c| in_pit(x0, c));
        let pit_water = |w: &World, x0: i32| -> i32 {
            pit(x0)
                .filter(|&c| w.get_cell(c) == Some(f.water))
                .map(|c| fill(w, c))
                .sum()
        };

        for _ in 0..pass.flow_iters() {
            run_sweep(&mut w, &mut pass, &mut ledger);
            checker.check(&w, &ledger).expect("water conserved");
        }

        // Water seeps out of the saturated walls and floor into the pit and
        // collects on its floor.
        assert!(pit_water(&w, wet_x) > 0, "no seepage into the pit");
        let floor: i32 = pit(wet_x)
            .filter(|c| c.0.y == SLAB_Y - 2)
            .map(|c| fill(&w, c))
            .sum();
        assert!(floor > 0, "seeped water reaches the pit floor");
        // Seepage sheds only the excess over the threshold.
        for c in body(wet_x).filter(|&c| w.get_cell(c) == Some(f.loam)) {
            assert!(fill(&w, c) >= threshold, "{c:?} seeped below the threshold");
        }
        // The dry body's pit stays dry and its loam holds nothing.
        assert_eq!(pit_water(&w, dry_x), 0);
        assert!(body(dry_x).all(|c| fill(&w, c) == 0));
        assert_eq!(ledger, SeasonLedger::default());
    }

    #[test]
    fn impermeable_blocks_infiltration() {
        let f = Fix::new();
        let threshold = loam_threshold(&f);
        let p = |x, y| CellPos::new(x, y, 10);
        let (mut w, mut pass) = pocket_world(
            &f,
            &[
                // Water on stone.
                (p(4, SLAB_Y), f.water, 200),
                // Water on porous hardpan (permeability 0).
                (p(8, SLAB_Y), f.water, 200),
                (p(8, SLAB_Y - 1), f.hardpan, 0),
                // Water on stone on dry loam: nothing passes the stone.
                (p(12, SLAB_Y), f.water, 200),
                (p(12, SLAB_Y - 2), f.loam, 0),
                // Saturated loam, above its seepage threshold, with one
                // stone cell between it and dry loam.
                (p(16, SLAB_Y - 2), f.loam, LOAM_CAP),
                (p(18, SLAB_Y - 2), f.loam, 0),
                // Saturated hardpan under an air pocket: it sheds nothing.
                (p(22, SLAB_Y), f.air, 0),
                (p(22, SLAB_Y - 1), f.hardpan, LOAM_CAP),
            ],
        );
        assert!(LOAM_CAP > threshold);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let hash = w.hash();

        for _ in 0..2 * pass.flow_iters() {
            run_sweep(&mut w, &mut pass, &mut ledger);
            checker.check(&w, &ledger).expect("water conserved");
        }

        assert_eq!(fill(&w, p(4, SLAB_Y)), 200);
        assert_eq!(fill(&w, p(8, SLAB_Y)), 200);
        assert_eq!(fill(&w, p(8, SLAB_Y - 1)), 0);
        assert_eq!(fill(&w, p(12, SLAB_Y - 2)), 0);
        assert_eq!(fill(&w, p(16, SLAB_Y - 2)), LOAM_CAP);
        assert_eq!(fill(&w, p(18, SLAB_Y - 2)), 0);
        assert_eq!(w.get_cell(p(22, SLAB_Y)), Some(f.air));
        assert_eq!(w.hash(), hash, "nothing moved");
        assert_eq!(ledger, SeasonLedger::default());
    }

    fn temp(w: &World, cell: CellPos) -> i32 {
        w.layer(cell, LayerId::Temperature)
    }

    /// The ledger's magma terms: a swap is neither a set nor a melt.
    fn magma_terms(l: &SeasonLedger) -> (i64, i64, i64) {
        (l.magma_set, l.magma_melt_in, l.magma_edit)
    }

    /// One sub-step's flow sweeps, each told its iteration.
    fn substep(w: &mut World, pass: &mut WaterPass, ledger: &mut SeasonLedger) {
        for i in 0..pass.flow_iters() {
            pass.set_iter(i);
            run_sweep(w, pass, ledger);
        }
    }

    #[test]
    fn magma_swaps_with_air_and_carries_temperature() {
        let f = Fix::new();
        let steam = f.t.role_gas("steam").expect("steam role");
        let hot = 12_000;
        // Falling: magma 4 cells above the slab, the air under it at 150 and
        // holding steam. In the world's −x −z corner, so that once on the
        // slab its only open neighbors come later in order: it steps to +x
        // and, visited again, straight back.
        let fall = |y| CellPos::new(0, SLAB_Y + y, 0);
        // Sliding: a pocket in the slab top whose only open neighbor is +x,
        // which has air under it.
        let slide = CellPos::new(10, SLAB_Y, 10);
        let ledge = CellPos::new(11, SLAB_Y, 10);
        let drop = CellPos::new(11, SLAB_Y - 1, 10);
        // Sorted order: magma on the slab with air on all four sides moves
        // to the first, −z.
        let flat = CellPos::new(20, SLAB_Y + 1, 20);
        let flat_to = CellPos::new(20, SLAB_Y + 1, 19);
        let mut w = f.world(&[
            (fall(4), f.magma),
            (slide, f.magma),
            (ledge, f.air),
            (drop, f.air),
            (flat, f.magma),
        ]);
        for c in [fall(4), slide, flat] {
            w.eco_put_layer(c, LayerId::Temperature, hot);
        }
        w.eco_put_layer(fall(3), LayerId::Temperature, 150);
        w.eco_put_layer(fall(3), LayerId::Gas(steam), 7);
        w.eco_put_layer(drop, LayerId::Temperature, 80);
        let mut pass = WaterPass::new(&f.t);
        activate_holders(&mut w, &pass);
        for c in [fall(4), slide, flat] {
            assert!(active(&w, c), "magma joins the water sweep");
        }
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let rev = revision(&w);

        pass.set_iter(0);
        run_sweep(&mut w, &mut pass, &mut ledger);

        // One cell down per sweep: the cell below comes earlier in the
        // global order, so the magma is not visited again this sweep.
        assert_eq!(w.get_cell(fall(3)), Some(f.magma));
        assert_eq!(w.get_cell(fall(4)), Some(f.air));
        assert_eq!(temp(&w, fall(3)), hot, "magma takes its temperature");
        assert_eq!(temp(&w, fall(4)), 150, "the air takes its own");
        assert_eq!(w.layer(fall(3), LayerId::Gas(steam)), 0, "gas left");
        assert!(active(&w, fall(3)) && !active(&w, fall(4)));
        // Sideways onto the ledge (later in order, so visited again this
        // sweep), then down off it.
        assert_eq!(w.get_cell(slide), Some(f.air));
        assert_eq!(w.get_cell(ledge), Some(f.air));
        assert_eq!(w.get_cell(drop), Some(f.magma));
        assert_eq!(temp(&w, drop), hot);
        assert_eq!(temp(&w, ledge), 80, "the drop's air went up to the ledge");
        assert_eq!(temp(&w, slide), 0, "the ledge's air went into the pocket");
        // The first air neighbor in sorted order.
        assert_eq!(w.get_cell(flat_to), Some(f.magma));
        assert_eq!(w.get_cell(flat), Some(f.air));
        assert_eq!(temp(&w, flat_to), hot);
        assert!(revision(&w) > rev, "swaps bump the revision");
        checker
            .check(&w, &ledger)
            .expect("magma and steam conserved");
        assert_eq!(
            magma_terms(&ledger),
            (0, 0, 0),
            "a swap is not a set or a melt"
        );

        // It keeps falling until it rests on the slab.
        for _ in 0..8 {
            run_sweep(&mut w, &mut pass, &mut ledger);
        }
        assert_eq!(w.get_cell(fall(1)), Some(f.magma));
        assert_eq!(temp(&w, fall(1)), hot);
        checker.check(&w, &ledger).expect("magma conserved");
        assert_eq!(magma_terms(&ledger), (0, 0, 0));
    }

    #[test]
    fn magma_viscosity_gates_moves() {
        let f = Fix::new();
        let viscosity = f.t.materials().get(f.magma).unwrap().viscosity;
        assert_eq!(viscosity, 3, "the SAD-P2 §5 row");
        assert_eq!(f.t.params().flow_iters, 8, "the counts below assume 8");
        // Magma high in open air falls one cell on each iteration where
        // `iter % (viscosity + 1) == 0`: iterations 0 and 4.
        let top = CellPos::new(10, 28, 10);
        let mut w = f.world(&[(top, f.magma)]);
        let mut pass = WaterPass::new(&f.t);
        activate_holders(&mut w, &pass);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let at = |w: &World| {
            (SLAB_Y + 1..=28).find(|&y| w.get_cell(CellPos::new(10, y, 10)) == Some(f.magma))
        };

        for i in 0..pass.flow_iters() {
            pass.set_iter(i);
            let before = at(&w);
            let r = run_sweep(&mut w, &mut pass, &mut ledger);
            assert!(r.visits >= 1, "iteration {i}: gated magma is still visited");
            let moved = at(&w) != before;
            assert_eq!(moved, i % (viscosity + 1) == 0, "iteration {i}");
        }
        assert_eq!(at(&w), Some(26), "two moves per sub-step");
        substep(&mut w, &mut pass, &mut ledger);
        assert_eq!(at(&w), Some(24));
        checker.check(&w, &ledger).expect("magma conserved");

        // Viscosity 0 moves on every iteration.
        let mut w = f.world(&[(top, f.magma)]);
        let mut runny = WaterPass::new(&f.t);
        runny.magma_period = 1;
        activate_holders(&mut w, &runny);
        substep(&mut w, &mut runny, &mut ledger);
        assert_eq!(at(&w), Some(20));
    }

    #[test]
    fn magma_never_enters_water_or_porous() {
        let f = Fix::new();
        // Shafts in the slab: magma at the top of each, with air above it
        // and stone around it except for the cell named.
        let top = |x| CellPos::new(x, SLAB_Y, 10);
        let under = |x| CellPos::new(x, SLAB_Y - 1, 10);
        let beside = |x| CellPos::new(x + 1, SLAB_Y, 10);
        let mut cells = Vec::new();
        for (x, m, v) in [
            (1, f.water, 255),
            (4, f.water, 100),
            (7, f.loam, 0),
            (10, f.hardpan, 0),
            (13, f.brine, 0),
        ] {
            // One shaft with the cell under the magma, one with it beside.
            cells.push((top(x), f.magma, 0));
            cells.push((under(x), m, v));
            cells.push((top(x + 15), f.magma, 0));
            cells.push((beside(x + 15), m, v));
        }
        let (mut w, mut pass) = pocket_world(&f, &cells);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let magma_cells = |w: &World| -> Vec<CellPos> {
            global_order(w.bounds())
                .filter(|&c| w.get_cell(c) == Some(f.magma))
                .collect()
        };
        let start = magma_cells(&w);
        assert_eq!(start.len(), 10);

        for _ in 0..4 {
            for i in 0..pass.flow_iters() {
                let before: Vec<(CellPos, MaterialId)> = global_order(w.bounds())
                    .map(|c| (c, w.get_cell(c).unwrap()))
                    .collect();
                pass.set_iter(i);
                run_sweep(&mut w, &mut pass, &mut ledger);
                for (c, m) in before {
                    if w.get_cell(c) == Some(f.magma) {
                        assert!(m == f.magma || m == f.air, "magma entered {m:?} at {c:?}");
                    }
                }
                checker.check(&w, &ledger).expect("conserved");
            }
        }
        // Nothing open around them: no magma moved, and none put anything
        // into the porous cells. (Contact boiling, §7.3 B7, may turn magma
        // beside water into obsidian; it never moves magma.)
        let end = magma_cells(&w);
        assert!(
            end.iter().all(|c| start.contains(c)),
            "magma moved: {end:?}"
        );
        for c in [under(7), under(10), beside(22), beside(25)] {
            assert_eq!(fill(&w, c), 0, "magma put nothing into {c:?}");
        }
        assert_eq!((ledger.magma_melt_in, ledger.magma_edit), (0, 0));
    }

    #[test]
    fn magma_count_invariant() {
        let f = Fix::new();
        let mut s = 0x006d_6167_6d61_u64;
        for case in 0..cases() {
            let seed = next(&mut s);
            let (mut w, mut pass) = random_world_with(&f, seed, true);
            let count = |w: &World| {
                global_order(w.bounds())
                    .filter(|&c| w.get_cell(c) == Some(f.magma))
                    .count()
            };
            // Magma cells carry their temperature, and a swap moves both
            // temperatures: all temperatures stay the same multiset through
            // any number of sweeps, and so do the magma temperatures while no
            // magma has set (contact boiling, §7.3 B7, sets it in place).
            let temps = |w: &World, only_magma: bool| {
                let mut v: Vec<i32> = global_order(w.bounds())
                    .filter(|&c| !only_magma || w.get_cell(c) == Some(f.magma))
                    .map(|c| temp(w, c))
                    .collect();
                v.sort_unstable();
                v
            };
            let (n, magma_t, all_t) = (count(&w), temps(&w, true), temps(&w, false));
            let checker = InvariantChecker::new(&w);
            let mut ledger = SeasonLedger::default();
            for step in 0..2 {
                for i in 0..pass.flow_iters() {
                    pass.set_iter(i);
                    run_sweep(&mut w, &mut pass, &mut ledger);
                    let at = format!("case {case} seed {seed:#x} sub-step {step} iter {i}");
                    // The full checker (every quantity) once per sub-step,
                    // which keeps the test fast; magma's own equation below
                    // after every sweep.
                    if i + 1 == pass.flow_iters()
                        && let Err(v) = checker.check(&w, &ledger)
                    {
                        panic!("{at}: {v}");
                    }
                    // The flow sweep never melts or places magma.
                    assert_eq!((ledger.magma_melt_in, ledger.magma_edit), (0, 0), "{at}");
                    let want = n as i64 - ledger.magma_set;
                    assert_eq!(count(&w) as i64, want, "{at}: magma count");
                    if ledger.magma_set == 0 {
                        assert_eq!(temps(&w, true), magma_t, "{at}: magma temperatures");
                    }
                    assert_eq!(temps(&w, false), all_t, "{at}: temperatures");
                    for c in global_order(w.bounds()) {
                        if w.get_cell(c) == Some(f.magma) {
                            assert!(active(&w, c), "{at}: magma at {c:?} not active");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn contact_boiling_whole_fill_obsidian_one_event() {
        let f = Fix::new();
        let st = steam_gas(&f);
        let steam = LayerId::Gas(st);
        let hot = 12_000;
        // Pockets in the slab top: magma with water under it (fill 200) and
        // full water beside it (+x). Below comes first in sorted order.
        let magma = CellPos::new(10, SLAB_Y, 10);
        let under = CellPos::new(10, SLAB_Y - 1, 10);
        let beside = CellPos::new(11, SLAB_Y, 10);
        // Two magma cells with one water cell between them: the first in
        // global order boils it; the second then has no water neighbor.
        let first = CellPos::new(20, SLAB_Y, 10);
        let between = CellPos::new(21, SLAB_Y, 10);
        let second = CellPos::new(22, SLAB_Y, 10);
        // Magma whose only water neighbor is full: the whole 255 boils.
        let full = CellPos::new(10, SLAB_Y, 20);
        let full_under = CellPos::new(10, SLAB_Y - 1, 20);
        let (mut w, mut pass) = pocket_world(
            &f,
            &[
                (magma, f.magma, 0),
                (under, f.water, 200),
                (beside, f.water, 255),
                (first, f.magma, 0),
                (between, f.water, 120),
                (second, f.magma, 0),
                (full, f.magma, 0),
                (full_under, f.water, 255),
            ],
        );
        for c in [magma, first, second, full] {
            w.eco_put_layer(c, LayerId::Temperature, hot);
        }
        w.eco_put_layer(under, LayerId::Temperature, 300);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let rev = revision(&w);

        // An iteration on which magma does not move: boiling is not gated
        // by viscosity.
        pass.set_iter(1);
        assert_ne!(1 % pass.magma_period, 0);
        run_sweep(&mut w, &mut pass, &mut ledger);

        // The water under the magma became air holding its whole fill as
        // steam; the magma set as obsidian in place, keeping its
        // temperature.
        assert_eq!(w.get_cell(under), Some(f.air));
        assert_eq!(fill(&w, under), 0);
        assert_eq!(w.layer(under, steam), 200, "the whole fill boils");
        assert_eq!(temp(&w, under), 300);
        assert!(
            in_set(&w, under, SWEEP_GAS + usize::from(st)),
            "steam joins its sweep"
        );
        assert_eq!(w.get_cell(magma), Some(f.obsidian));
        assert_eq!(temp(&w, magma), hot);
        assert!(!active(&w, magma), "obsidian leaves the water sweep");
        // One event per visit: the full water beside it is untouched.
        assert_eq!(w.get_cell(beside), Some(f.water));
        assert_eq!(fill(&w, beside), 255);
        assert_eq!(w.layer(beside, steam), 0);
        // The shared water cell boiled once, by the first magma cell.
        assert_eq!(w.get_cell(first), Some(f.obsidian));
        assert_eq!(w.get_cell(between), Some(f.air));
        assert_eq!(w.layer(between, steam), 120);
        assert_eq!(w.get_cell(second), Some(f.magma), "nothing left to boil");
        assert_eq!(w.get_cell(full), Some(f.obsidian));
        assert_eq!(w.layer(full_under, steam), 255);
        assert!(revision(&w) > rev);

        assert_eq!(ledger.boil_to_steam, 200 + 120 + 255, "actual amounts");
        assert_eq!(ledger.magma_set, 3, "one cell per event");
        assert_eq!((ledger.magma_melt_in, ledger.magma_edit), (0, 0));
        assert_eq!(ledger.water_saturation_loss, 0);
        checker
            .check(&w, &ledger)
            .expect("water, steam and magma conserved");

        // Obsidian boils nothing: further sweeps leave the water beside it.
        for i in 0..pass.flow_iters() {
            pass.set_iter(i);
            run_sweep(&mut w, &mut pass, &mut ledger);
        }
        assert_eq!(fill(&w, beside), 255);
        assert_eq!(ledger.magma_set, 3);
        assert!(
            !active(&w, under),
            "visited dry, the air left the water sweep"
        );
        checker.check(&w, &ledger).expect("conserved");

        // Face order after below: −z, −x, +x, +z, then above. Each magma
        // pocket has water in two of its faces; the earlier one boils.
        let p = |x, z| CellPos::new(x, SLAB_Y, z);
        let (mz, mx, mp) = (p(5, 5), p(15, 5), p(25, 5));
        // Moving magma does not boil on that visit: on iteration 0 it drops
        // into the air pocket under it, away from the water beside it.
        let (drop_from, drop_to, drop_side) =
            (p(10, 20), CellPos::new(10, SLAB_Y - 1, 20), p(11, 20));
        let (mut w, mut pass) = pocket_world(
            &f,
            &[
                (mz, f.magma, 0),
                (p(5, 4), f.water, 40),
                (p(4, 5), f.water, 41),
                (mx, f.magma, 0),
                (p(14, 5), f.water, 42),
                (p(16, 5), f.water, 43),
                (mp, f.magma, 0),
                (p(26, 5), f.water, 44),
                (p(25, 6), f.water, 45),
                (drop_from, f.magma, 0),
                (drop_to, f.air, 0),
                (drop_side, f.water, 200),
            ],
        );
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        pass.set_iter(0);
        assert_eq!(0 % pass.magma_period, 0, "magma moves on iteration 0");
        run_sweep(&mut w, &mut pass, &mut ledger);
        for (m, boiled, kept) in [
            (mz, p(5, 4), p(4, 5)),
            (mx, p(14, 5), p(16, 5)),
            (mp, p(26, 5), p(25, 6)),
        ] {
            assert_eq!(w.get_cell(m), Some(f.obsidian), "{m:?}");
            assert_eq!(w.get_cell(boiled), Some(f.air), "{boiled:?} boils first");
            assert_eq!(w.get_cell(kept), Some(f.water), "{kept:?} comes later");
        }
        assert_eq!(w.get_cell(drop_to), Some(f.magma), "the magma dropped");
        assert_eq!(w.get_cell(drop_side), Some(f.water), "and boiled nothing");
        assert_eq!(ledger.boil_to_steam, 40 + 42 + 44);
        assert_eq!(ledger.magma_set, 3);
        // The water beside flowed into the pocket the magma left, over the
        // magma: next visit, the magma's only water face is above it.
        let over = drop_from;
        assert_eq!(w.get_cell(over), Some(f.water));
        let poured = fill(&w, over);
        assert!(poured > 0);
        pass.set_iter(1);
        run_sweep(&mut w, &mut pass, &mut ledger);
        // (The boiled cell is air again only until the water beside it,
        // later in order, flows back in.)
        assert_eq!(w.get_cell(drop_to), Some(f.obsidian));
        assert_eq!(ledger.boil_to_steam, 40 + 42 + 44 + i64::from(poured));
        assert_eq!(ledger.magma_set, 4);
        checker.check(&w, &ledger).expect("conserved");
    }

    // §7.3 A, C and D: condensation, evaporation and rain.

    fn steam_gas(f: &Fix) -> u8 {
        f.t.role_gas("steam").expect("steam role")
    }

    fn up(c: CellPos) -> CellPos {
        CellPos::new(c.0.x, c.0.y + 1, c.0.z)
    }

    fn in_set(w: &World, cell: CellPos, kind: usize) -> bool {
        let (pos, local) = cell.split();
        w.chunk(pos).unwrap().active[kind].contains(local)
    }

    #[test]
    fn evaporation_limited_by_steam_room_ledger_actual() {
        let f = Fix::new();
        let st = steam_gas(&f);
        let steam = LayerId::Gas(st);
        let p = f.t.params();
        let table: Vec<i32> = p.evap_table.iter().map(|&e| i32::from(e)).collect();
        let bucket = i32::from(p.evap_bucket);
        let hottest = *table.last().unwrap();
        assert!(hottest > 15, "the room-limited case needs a rate above 15");
        let hot = bucket * (table.len() as i32 + 3);
        // Ponds in the top slab layer under open air, and one sealed under
        // the slab's top (stone above it).
        let at = |x| CellPos::new(x, SLAB_Y, 10);
        let warm = at(4);
        let roomy = at(8);
        let choked = at(12);
        let shallow = at(16);
        let frozen = at(20);
        let sealed = CellPos::new(24, SLAB_Y - 1, 10);
        let (mut w, _) = pocket_world(
            &f,
            &[
                (warm, f.water, 200),
                (roomy, f.water, 200),
                (choked, f.water, 200),
                (shallow, f.water, 3),
                (frozen, f.water, 200),
                (sealed, f.water, 200),
            ],
        );
        for (c, t) in [
            (warm, 2 * bucket + bucket / 2),
            (roomy, hot),
            (choked, hot),
            (shallow, hot),
            (frozen, -bucket / 2),
            (sealed, hot),
        ] {
            w.eco_put_layer(c, LayerId::Temperature, t);
        }
        w.eco_put_layer(up(roomy), steam, 240);
        w.eco_put_layer(up(choked), steam, 255);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut evap = Evaporation::new(&f.t).expect("steam, water and air roles");

        run_sweep(&mut w, &mut evap, &mut ledger);

        // (cell, fill, steam above): the table's rate for the cell's
        // temperature, within the steam room above and the fill.
        let want = [
            (warm, 200 - table[2], table[2]),
            (roomy, 200 - 15, 255),
            (choked, 200, 255),
            (frozen, 200 - table[0], table[0]),
        ];
        for (c, fill_c, steam_above) in want {
            assert_eq!(fill(&w, c), fill_c, "{c:?} fill");
            assert_eq!(w.layer(up(c), steam), steam_above, "{c:?} steam above");
            assert_eq!(w.get_cell(c), Some(f.water));
        }
        // The shallow pond dries out: its whole fill, 3, is steam, and the
        // empty cell is air.
        assert_eq!(fill(&w, shallow), 0);
        assert_eq!(w.layer(up(shallow), steam), 3);
        assert_eq!(w.get_cell(shallow), Some(f.air));
        assert!(!active(&w, shallow), "the dry cell leaves the water sweep");
        // Stone above: nothing evaporates.
        assert_eq!(fill(&w, sealed), 200);
        assert_eq!(w.layer(up(sealed), steam), 0);
        // The steam joins the steam sweep.
        for c in [warm, roomy, shallow, frozen] {
            assert!(in_set(&w, up(c), SWEEP_GAS + usize::from(st)), "{c:?}");
        }
        // The ledger books what moved, not the table's rates.
        let expect = SeasonLedger {
            evap_to_steam: i64::from(table[2] + 15 + 3 + table[0]),
            ..SeasonLedger::default()
        };
        assert_eq!(ledger, expect);
        checker
            .check(&w, &ledger)
            .expect("water and steam conserved");

        // Again: the room above the roomy pond is gone.
        run_sweep(&mut w, &mut evap, &mut ledger);
        assert_eq!(fill(&w, roomy), 200 - 15);
        assert_eq!(fill(&w, warm), 200 - 2 * table[2]);
        assert_eq!(
            ledger.evap_to_steam,
            expect.evap_to_steam + i64::from(table[2] + table[0])
        );
        checker
            .check(&w, &ledger)
            .expect("water and steam conserved");
    }

    #[test]
    fn condensation_below_temp_returns_steam() {
        let f = Fix::new();
        let st = steam_gas(&f);
        let steam = LayerId::Gas(st);
        let ct = i32::from(f.t.params().condense_temp);
        // Sealed one-cell air pockets just under the slab's top.
        let at = |x| CellPos::new(x, SLAB_Y - 1, 10);
        let cold = at(4);
        let warm = at(8);
        let damp = at(12);
        let soaked = at(16);
        let (mut w, _) = pocket_world(
            &f,
            &[
                (cold, f.air, 0),
                (warm, f.air, 0),
                (damp, f.air, 150),
                (soaked, f.air, 255),
            ],
        );
        for (c, s, t) in [
            (cold, 100, ct - 1),
            (warm, 100, ct),
            (damp, 200, 0),
            (soaked, 50, 0),
        ] {
            w.eco_put_layer(c, steam, s);
            w.eco_put_layer(c, LayerId::Temperature, t);
        }
        for c in [cold, warm, damp, soaked] {
            assert!(i32::from(w.surface_y(c.0.x, c.0.z)) > c.0.y, "covered");
        }
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut cond = Condensation::new(&f.t).expect("steam role");
        let gas_set = SWEEP_GAS + usize::from(st);
        assert_eq!(cond.kind(), gas_set);

        run_sweep(&mut w, &mut cond, &mut ledger);

        // Below condense_temp all the steam that fits becomes moisture on
        // the cell itself; at condense_temp nothing does.
        assert_eq!((fill(&w, cold), w.layer(cold, steam)), (100, 0));
        assert_eq!((fill(&w, warm), w.layer(warm, steam)), (0, 100));
        assert_eq!((fill(&w, damp), w.layer(damp, steam)), (255, 95));
        assert_eq!((fill(&w, soaked), w.layer(soaked, steam)), (255, 50));
        assert!(!in_set(&w, cold, gas_set), "no steam left");
        assert!(in_set(&w, damp, gas_set) && in_set(&w, warm, gas_set));
        assert!(active(&w, cold), "moist air joins the water sweep");
        let expect = SeasonLedger {
            condense_from_steam: 100 + 105,
            ..SeasonLedger::default()
        };
        assert_eq!(ledger, expect);
        checker
            .check(&w, &ledger)
            .expect("water and steam conserved");
        // The cell is still air; the first flow sweep makes it water.
        assert_eq!(w.get_cell(cold), Some(f.air));
        run_sweep(&mut w, &mut WaterPass::new(&f.t), &mut ledger);
        assert_eq!(w.get_cell(cold), Some(f.water));
        assert_eq!(fill(&w, cold), 100);
        checker
            .check(&w, &ledger)
            .expect("water and steam conserved");
    }

    #[test]
    fn steam_above_sky_open_pond_vents_not_condenses() {
        let f = Fix::new();
        let st = steam_gas(&f);
        let steam = LayerId::Gas(st);
        let stone = f.t.materials().id("stone").unwrap();
        let p = f.t.params();
        let table: Vec<i32> = p.evap_table.iter().map(|&e| i32::from(e)).collect();
        // A pond open to the sky, and one under a stone overhang two cells
        // above the air over it.
        let pond = CellPos::new(10, SLAB_Y, 10);
        let covered = CellPos::new(20, SLAB_Y, 20);
        let roof = CellPos::new(20, SLAB_Y + 3, 20);
        let (mut w, _) = pocket_world(
            &f,
            &[
                (pond, f.water, 255),
                (covered, f.water, 255),
                (roof, stone, 0),
            ],
        );
        assert_eq!(
            i32::from(w.surface_y(10, 10)),
            SLAB_Y,
            "open above the pond"
        );
        assert_eq!(i32::from(w.surface_y(20, 20)), SLAB_Y + 3, "the overhang");
        // Both air cells are far below condense_temp.
        let ct = i32::from(p.condense_temp);
        for c in [up(pond), up(covered)] {
            assert!(w.layer(c, LayerId::Temperature) < ct);
            w.eco_put_layer(c, steam, 80);
        }
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let mut cond = Condensation::new(&f.t).expect("steam role");
        let gas_set = SWEEP_GAS + usize::from(st);

        run_sweep(&mut w, &mut cond, &mut ledger);

        // Sky-open steam stays steam, in the steam sweep, for its vent step
        // (§7.4); under the overhang it condenses.
        assert_eq!((w.layer(up(pond), steam), fill(&w, up(pond))), (80, 0));
        assert!(in_set(&w, up(pond), gas_set));
        assert_eq!(
            (w.layer(up(covered), steam), fill(&w, up(covered))),
            (0, 80)
        );
        assert_eq!(ledger.condense_from_steam, 80);
        checker.check(&w, &ledger).expect("conserved");

        // The open pond's own evaporation does not come back either. The
        // covered pond (at 0 °C, bucket 0) evaporates into the moist air
        // under the overhang, and that steam condenses again.
        let warm = i32::from(p.evap_bucket) * 5;
        w.eco_put_layer(pond, LayerId::Temperature, warm);
        let e = table[5];
        run_sweep(&mut w, &mut Evaporation::new(&f.t).unwrap(), &mut ledger);
        assert_eq!(w.layer(up(pond), steam), 80 + e);
        assert_eq!(fill(&w, pond), 255 - e);
        run_sweep(&mut w, &mut cond, &mut ledger);
        assert_eq!(w.layer(up(pond), steam), 80 + e);
        assert_eq!(fill(&w, up(pond)), 0);
        assert_eq!(w.layer(up(covered), steam), 0);
        assert_eq!(ledger.condense_from_steam, 80 + i64::from(table[0]));
        checker.check(&w, &ledger).expect("conserved");
    }

    /// Sum of layer `id` over every cell of `w`.
    fn layer_total(w: &World, id: LayerId) -> i64 {
        global_order(w.bounds())
            .map(|c| i64::from(w.layer(c, id)))
            .sum()
    }

    /// The passes of one sub-step that move water or steam, in SAD-P2 §3
    /// order: transitions (`BoilAll`), condensation, the flow sweeps,
    /// evaporation, rain, and the steam sweep (drift, diffusion, vent).
    /// `check` runs after each, with the pass's name.
    fn steam_substep(
        t: &EcoTables,
        w: &mut World,
        rain: &Rain,
        ledger: &mut SeasonLedger,
        check: &mut dyn FnMut(&World, &SeasonLedger, &str),
    ) {
        use crate::gases::GasPass;
        use crate::runner::{Exec, run_jacobi};
        use crate::transitions::TransitionsPass;

        let st = t.role_gas("steam").expect("steam role");
        run_jacobi(w, &TransitionsPass::new(t), Exec::Serial, ledger);
        check(w, ledger, "transitions");
        run_sweep(w, &mut Condensation::new(t).unwrap(), ledger);
        check(w, ledger, "condensation");
        let mut flow = WaterPass::new(t);
        for i in 0..flow.flow_iters() {
            flow.set_iter(i);
            run_sweep(w, &mut flow, ledger);
            check(w, ledger, "flow");
        }
        run_sweep(w, &mut Evaporation::new(t).unwrap(), ledger);
        check(w, ledger, "evaporation");
        rain.run(w, ledger);
        check(w, ledger, "rain");
        run_sweep(w, &mut GasPass::new(t, st), ledger);
        check(w, ledger, "steam sweep");
    }

    /// Both invariants of SAD-P2 §7 that steam enters, written out:
    /// - water: `Σ moisture + Σ steam = before + rain_in + water_edit_in −
    ///   water_edit_out − steam_vent_out − water_saturation_loss`;
    /// - steam: `Σ steam = before + evap_to_steam + boil_to_steam −
    ///   condense_from_steam − steam_vent_out − gas_saturation_loss`,
    ///   with the steam row's `vent_out` equal to `steam_vent_out`;
    ///
    /// and the [`InvariantChecker`] agrees.
    struct SteamBooks {
        checker: InvariantChecker,
        st: usize,
        moisture: i64,
        steam: i64,
    }

    impl SteamBooks {
        fn new(w: &World, st: u8) -> Self {
            Self {
                checker: InvariantChecker::new(w),
                st: usize::from(st),
                moisture: layer_total(w, LayerId::Moisture),
                steam: layer_total(w, LayerId::Gas(st)),
            }
        }

        fn check(&self, w: &World, l: &SeasonLedger, at: &str) {
            let m = layer_total(w, LayerId::Moisture);
            let s = layer_total(w, LayerId::Gas(self.st as u8));
            assert_eq!(
                m + s,
                self.moisture + self.steam + l.rain_in + l.water_edit_in
                    - l.water_edit_out
                    - l.steam_vent_out
                    - l.water_saturation_loss,
                "{at}: water total (moisture {m}, steam {s})"
            );
            assert_eq!(
                s,
                self.steam + l.evap_to_steam + l.boil_to_steam
                    - l.condense_from_steam
                    - l.steam_vent_out
                    - l.gas_saturation_loss[self.st],
                "{at}: steam"
            );
            assert_eq!(l.vent_out[self.st], l.steam_vent_out, "{at}: vent");
            if let Err(v) = self.checker.check(w, l) {
                panic!("{at}: {v}");
            }
        }
    }

    /// Steam is part of the water total and its vent the water total's one
    /// gas exit (SAD-P2 §7.2, §7.3, §7.4). A dry fixture runs the water
    /// and steam passes of four sub-steps:
    /// - a hot pond open to the sky evaporates, and its steam vents
    ///   (it never condenses: the cell is sky-open);
    /// - a pond under a stone overhang evaporates into the covered cell,
    ///   and that steam condenses back;
    /// - water above 100 °C open to the sky boils (`BoilAll`) and vents;
    /// - a sealed water pocket boils, holds its steam, and once cooled
    ///   below `condense_temp` condenses back to the same fill.
    ///
    /// Then random worlds with magma and rain run the same passes. Both
    /// invariants hold after every pass.
    #[test]
    fn water_invariant_includes_steam_and_vent() {
        let f = Fix::new();
        let st = steam_gas(&f);
        let steam = LayerId::Gas(st);
        let stone = f.t.materials().id("stone").unwrap();
        let p = f.t.params();
        let warm = i32::from(p.evap_bucket) * 5;
        let hot = 1500;
        let ct = i32::from(p.condense_temp);
        assert!(warm < ct && hot > ct);
        let dry = f.t.with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: None,
        });
        let open = CellPos::new(6, SLAB_Y, 6);
        let covered = CellPos::new(16, SLAB_Y, 16);
        let roof = CellPos::new(16, SLAB_Y + 2, 16);
        let boiler = CellPos::new(24, SLAB_Y, 8);
        let sealed = CellPos::new(24, SLAB_Y - 2, 24);
        let (mut w, _) = pocket_world(
            &f,
            &[
                (open, f.water, 255),
                (covered, f.water, 255),
                (roof, stone, 0),
                (boiler, f.water, 200),
                (sealed, f.water, 180),
            ],
        );
        for (c, t) in [(open, warm), (covered, warm), (boiler, hot), (sealed, hot)] {
            w.eco_put_layer(c, LayerId::Temperature, t);
        }
        let books = SteamBooks::new(&w, st);
        let mut ledger = SeasonLedger::default();
        let rain = Rain::new(&dry, 0, 1);
        let mut check = |w: &World, l: &SeasonLedger, at: &str| books.check(w, l, at);
        // Sub-step 0 boils both hot cells.
        steam_substep(&dry, &mut w, &rain, &mut ledger, &mut check);
        assert_eq!(ledger.boil_to_steam, 200 + 180);
        assert_eq!(w.get_cell(boiler), Some(f.air));
        assert_eq!(w.get_cell(sealed), Some(f.air));
        assert_eq!(w.layer(sealed, steam), 180, "sealed: fill to steam 1:1");
        // The open boil vents in the steam sweep. Steam that diffuses back
        // into the cell after its visit vents in a later sweep.
        assert!(ledger.steam_vent_out > 0);
        assert!(w.layer(boiler, steam) < 200, "the open boil vents");
        assert_eq!(ledger.condense_from_steam, 0, "nothing was cool yet");
        // Cool the sealed pocket: the next sub-step's condensation returns
        // its steam, and the first flow sweep makes it water again.
        w.eco_put_layer(sealed, LayerId::Temperature, 0);
        for _ in 1..4 {
            steam_substep(&dry, &mut w, &rain, &mut ledger, &mut check);
        }
        assert_eq!(w.get_cell(sealed), Some(f.water));
        assert_eq!((fill(&w, sealed), w.layer(sealed, steam)), (180, 0));
        assert_eq!(w.get_cell(boiler), Some(f.air), "the open boil is gone");
        assert_eq!(fill(&w, boiler), 0);
        // The covered pond's steam came back too; the open pond's did not.
        assert!(
            ledger.condense_from_steam > 180,
            "condensed {}",
            ledger.condense_from_steam
        );
        assert!(ledger.evap_to_steam > 0);
        assert!(ledger.steam_vent_out > 0);
        assert_eq!(ledger.rain_in, 0);
        // Without steam and its vent, moisture alone would not balance.
        assert_ne!(layer_total(&w, LayerId::Moisture), books.moisture);

        // Random worlds with magma (contact boiling), hot water (BoilAll),
        // cool covered steam and the shipped rain.
        let wet = (0..f.t.seasons().seasons.len() as u32)
            .find(|&s| f.t.seasons().seasons[s as usize].rain > 0)
            .expect("a season with rain");
        let mut sums = SeasonLedger::default();
        for seed in 0..24 {
            let (mut w, _) = random_world_with(&f, seed, true);
            let books = SteamBooks::new(&w, st);
            let mut ledger = SeasonLedger::default();
            let rain = Rain::new(&f.t, wet, seed);
            let mut check = |w: &World, l: &SeasonLedger, at: &str| {
                books.check(w, l, &format!("seed {seed}, {at}"));
            };
            for _ in 0..3 {
                steam_substep(&f.t, &mut w, &rain, &mut ledger, &mut check);
            }
            crate::ledger::merge(&mut sums, &ledger);
        }
        for (name, v) in [
            ("rain_in", sums.rain_in),
            ("evap_to_steam", sums.evap_to_steam),
            ("boil_to_steam", sums.boil_to_steam),
            ("condense_from_steam", sums.condense_from_steam),
            ("steam_vent_out", sums.steam_vent_out),
        ] {
            assert!(v > 0, "no random world exercised {name}");
        }
    }

    /// A seed whose rain pattern is at least half strength on every column
    /// of `cols` in `season`.
    fn strong_seed(season: u32, cols: &[(i32, i32)]) -> u64 {
        (0..1 << 16)
            .find(|&s| {
                cols.iter()
                    .all(|&(x, z)| rain_pattern(s, season, x, z) >= 128)
            })
            .expect("a seed with strong rain on the test columns")
    }

    /// The rain a column gets before the room cap: `rain · pattern / 256`.
    fn rain_of(rain: u8, seed: u64, season: u32, x: i32, z: i32) -> i32 {
        i32::from(rain) * i32::from(rain_pattern(seed, season, x, z)) / 256
    }

    /// Every cell's moisture, in global order.
    fn moistures(w: &World) -> Vec<(CellPos, i32)> {
        global_order(w.bounds()).map(|c| (c, fill(w, c))).collect()
    }

    #[test]
    fn rain_targets_surface_plus_one_or_anchor_never_overhang() {
        let f = Fix::new();
        let stone = f.t.materials().id("stone").unwrap();
        let t = f.t.with_season_override(SeasonOverride {
            rain: Some(u8::MAX),
            air_temp: None,
        });
        let season = 2;
        let open = (4, 4);
        let overhang = (8, 8);
        let plant = (12, 12);
        let bare_plant = (16, 16);
        let top = (20, 20);
        let pond = (24, 24);
        let seed = strong_seed(season, &[open, overhang, plant, pond]);
        let c = |(x, z): (i32, i32), y: i32| CellPos::new(x, y, z);
        let (mut w, _) = pocket_world(
            &f,
            &[
                (c(overhang, SLAB_Y + 4), stone, 0),
                (c(plant, SLAB_Y), f.loam, 0),
                (c(plant, SLAB_Y + 1), f.moss, 0),
                (c(bare_plant, SLAB_Y + 1), f.moss, 0),
                (c(top, 31), stone, 0),
                (c(pond, SLAB_Y), f.water, 100),
            ],
        );
        assert_eq!(
            i32::from(w.surface_y(plant.0, plant.1)),
            SLAB_Y,
            "the plant is passable"
        );
        let before = moistures(&w);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let rain = Rain::new(&t, season, seed);

        let columns = rain.run(&mut w, &mut ledger);

        // Where each column's rain lands and how much: the cell above the
        // surface, the anchor under a plant (within its field capacity),
        // nowhere when the plant stands on stone or the surface is the
        // world's top.
        let target = |xz: (i32, i32)| -> Option<(CellPos, i32)> {
            let r = rain_of(u8::MAX, seed, season, xz.0, xz.1);
            if xz == overhang {
                Some((c(xz, SLAB_Y + 5), r))
            } else if xz == plant {
                Some((c(xz, SLAB_Y), r.min(LOAM_CAP)))
            } else if xz == bare_plant || xz == top {
                None
            } else {
                Some((c(xz, SLAB_Y + 1), r))
            }
        };
        let mut rained = 0;
        let mut total = 0;
        for (cell, was) in before {
            let gained = fill(&w, cell) - was;
            let want = target((cell.0.x, cell.0.z))
                .filter(|&(at, _)| at == cell)
                .map_or(0, |(_, r)| r);
            assert_eq!(gained, want, "{cell:?}");
            if want > 0 {
                rained += 1;
                total += want;
            }
        }
        assert_eq!(columns, rained);
        assert_eq!(ledger.rain_in, i64::from(total));
        // Rain on air makes water; the anchor and the plant keep their
        // materials; under the overhang it stays dry air.
        assert_eq!(w.get_cell(c(open, SLAB_Y + 1)), Some(f.water));
        assert_eq!(w.get_cell(c(overhang, SLAB_Y + 5)), Some(f.water));
        for y in SLAB_Y + 1..SLAB_Y + 4 {
            assert_eq!(w.get_cell(c(overhang, y)), Some(f.air), "y {y}");
        }
        assert_eq!(fill(&w, c(plant, SLAB_Y)), LOAM_CAP);
        assert_eq!(w.get_cell(c(plant, SLAB_Y)), Some(f.loam));
        assert_eq!(w.get_cell(c(plant, SLAB_Y + 1)), Some(f.moss));
        assert_eq!(fill(&w, c(pond, SLAB_Y)), 100, "rain lands above the pond");
        assert_eq!(w.get_cell(c(pond, SLAB_Y + 1)), Some(f.water));
        checker
            .check(&w, &ledger)
            .expect("rain_in explains the water");
    }

    #[test]
    fn rain_ledger_records_actual() {
        let f = Fix::new();
        let rain_amount = 200;
        let t = f.t.with_season_override(SeasonOverride {
            rain: Some(rain_amount),
            air_temp: None,
        });
        let season = 0;
        let (damp, soaked, plant) = ((4, 4), (8, 8), (12, 12));
        let seed = strong_seed(season, &[damp, soaked, plant]);
        let c = |(x, z): (i32, i32), y: i32| CellPos::new(x, y, z);
        let (mut w, _) = pocket_world(
            &f,
            &[
                (c(damp, SLAB_Y + 1), f.air, 250),
                (c(soaked, SLAB_Y + 1), f.air, 255),
                (c(plant, SLAB_Y), f.loam, LOAM_CAP - 2),
                (c(plant, SLAB_Y + 1), f.moss, 0),
            ],
        );
        let total = |w: &World| -> i64 { moistures(w).iter().map(|&(_, m)| i64::from(m)).sum() };
        let before = total(&w);
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        let rain = Rain::new(&t, season, seed);

        rain.run(&mut w, &mut ledger);

        // Room left 5, 0 and 2: those columns take that much, every other
        // column its whole share.
        let mut requested = 0;
        let mut actual = 0;
        for z in 0..32 {
            for x in 0..32 {
                let r = rain_of(rain_amount, seed, season, x, z);
                requested += r;
                actual += match (x, z) {
                    xz if xz == damp => 5,
                    xz if xz == soaked => 0,
                    xz if xz == plant => 2,
                    _ => r,
                };
            }
        }
        assert_eq!(fill(&w, c(damp, SLAB_Y + 1)), 255);
        assert_eq!(w.get_cell(c(damp, SLAB_Y + 1)), Some(f.water));
        assert_eq!(fill(&w, c(soaked, SLAB_Y + 1)), 255);
        assert_eq!(fill(&w, c(plant, SLAB_Y)), LOAM_CAP);
        assert!(actual < requested);
        let want = SeasonLedger {
            rain_in: i64::from(actual),
            ..SeasonLedger::default()
        };
        assert_eq!(ledger, want, "the ledger books the actual amounts");
        assert_eq!(total(&w) - before, ledger.rain_in);
        checker
            .check(&w, &ledger)
            .expect("rain_in explains the water");

        // A second sub-step's rain lands on top of the new puddles and adds
        // to the same term.
        let first = ledger.rain_in;
        rain.run(&mut w, &mut ledger);
        assert!(ledger.rain_in > first);
        assert_eq!(total(&w) - before, ledger.rain_in);
        assert_eq!(
            fill(&w, c(damp, SLAB_Y + 2)),
            rain_of(rain_amount, seed, season, 4, 4)
        );
        checker
            .check(&w, &ledger)
            .expect("rain_in explains the water");

        // A dry season rains nothing.
        let dry = f.t.with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: None,
        });
        let hash = w.hash();
        let mut l = SeasonLedger::default();
        assert_eq!(Rain::new(&dry, season, seed).run(&mut w, &mut l), 0);
        assert_eq!(w.hash(), hash);
        assert_eq!(l, SeasonLedger::default());
    }

    #[test]
    fn rain_pattern_is_seeded() {
        let cols: Vec<(i32, i32)> = (0..32).flat_map(|z| (0..32).map(move |x| (x, z))).collect();
        let pattern = |seed, season| -> Vec<u8> {
            cols.iter()
                .map(|&(x, z)| rain_pattern(seed, season, x, z))
                .collect()
        };
        let base = pattern(1, 0);
        assert_eq!(base, pattern(1, 0), "a pure function");
        // Another seed, another season (also the same row a year later):
        // different patterns.
        for (seed, season) in [(2, 0), (1, 1), (1, 4)] {
            let other = pattern(seed, season);
            let same = base.iter().zip(&other).filter(|(a, b)| a == b).count();
            assert!(
                same < cols.len() / 16,
                "seed {seed} season {season}: {same} equal"
            );
        }
        // Spread over the whole range, not a constant or a narrow band.
        let distinct: std::collections::BTreeSet<u8> = base.iter().copied().collect();
        assert!(distinct.len() > 200, "{} distinct values", distinct.len());
        assert!(*distinct.first().unwrap() < 16 && *distinct.last().unwrap() > 240);
        let mean = base.iter().map(|&v| u32::from(v)).sum::<u32>() / cols.len() as u32;
        assert!((96..160).contains(&mean), "mean {mean}");

        // Rain follows it: on the open slab, every column's cell above the
        // surface gets exactly `rain · pattern / 256`; another seed gives
        // another world, the same seed the same one.
        let f = Fix::new();
        let season = 0;
        let rain = f.t.seasons().seasons[0].rain;
        let run = |seed| {
            let (mut w, _) = pocket_world(&f, &[]);
            Rain::new(&f.t, season, seed).run(&mut w, &mut SeasonLedger::default());
            w
        };
        let a = run(1);
        for &(x, z) in &cols {
            let cell = CellPos::new(x, SLAB_Y + 1, z);
            assert_eq!(fill(&a, cell), rain_of(rain, 1, season, x, z), "{x} {z}");
        }
        assert_eq!(a.hash(), run(1).hash());
        assert_ne!(a.hash(), run(2).hash());
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
    /// included): a stone floor, then stone, dry air, moist air, water of
    /// random fill, loam up to its field capacity, and loam or hardpan
    /// with any moisture (over capacity too, as `SetLayer` can leave it).
    /// Water holders are active.
    fn random_world(f: &Fix, seed: u64) -> (World, WaterPass) {
        random_world_with(f, seed, false)
    }

    /// [`random_world`], and with `magma` also magma cells (about one in
    /// six) and a random temperature on every cell.
    fn random_world_with(f: &Fix, seed: u64, magma: bool) -> (World, WaterPass) {
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
        let mut temps = Vec::new();
        let kinds = if magma { 16 } else { 13 };
        for cell in global_order(bounds) {
            let r = next(&mut s);
            let v = ((r >> 8) % 256) as i32;
            if magma {
                temps.push((cell, ((r >> 16) % 13_000) as i32));
            }
            let (m, v) = match r % kinds {
                _ if cell.0.y == bounds.min.0.y => (stone, 0),
                13.. => (f.magma, 0),
                0..=2 => (stone, 0),
                3..=5 => (f.air, 0),
                6 => (f.air, v),
                7..=9 => (f.water, v),
                10 => (f.loam, v % (LOAM_CAP + 1)),
                11 => (f.loam, v),
                _ => (f.hardpan, v),
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
        for (cell, t) in temps {
            w.eco_put_layer(cell, LayerId::Temperature, t);
        }
        let pass = WaterPass::new(&f.t);
        activate_holders(&mut w, &pass);
        (w, pass)
    }

    /// The reference's view of the tables: the roles, the §7.3 constants
    /// and every solid tagged `porous`.
    fn water_ref(f: &Fix) -> WaterRef {
        let p = f.t.params();
        let m = f.t.materials();
        WaterRef {
            air: f.air,
            water: f.water,
            flow_cap: i32::from(p.flow_cap),
            infil_shift: p.infil_shift,
            infil_side_shift: p.infil_side_shift,
            seep_num: i32::from(p.seep_num),
            seep_shift: p.seep_shift,
            porous: m
                .iter()
                .filter(|d| d.phase == Phase::Solid && d.has_tag("porous"))
                .map(|d| {
                    (
                        m.id(&d.key).unwrap(),
                        i32::from(d.permeability),
                        i32::from(d.field_capacity),
                    )
                })
                .collect(),
        }
    }

    /// Seeds of the multi-chunk oracle test.
    const MULTI_CHUNK_SEEDS: u64 = 3;
    /// Rounds of `flow_iters` sweeps in the multi-chunk oracle test.
    const MULTI_CHUNK_ROUNDS: u8 = 3;

    /// A terrain over 4 × 2 × 4 = 32 chunks, 80 × 16 × 80 cells around the
    /// origin, with every water source in chunk (0, 0, −1):
    /// - a stone floor, then a bowl whose lowest point is at the origin,
    ///   built of random stone, test loam and hardpan and the shipped
    ///   porous limestone and crushed sulfide;
    /// - a reservoir of random fill in x 1..=12, z −12..=−1, y 0..=6,
    ///   over the empty part of the bowl in the chunk below;
    /// - in the same chunk, moist air at the top, scattered stone
    ///   overhangs, and porous ground with any moisture (over capacity
    ///   too).
    ///
    /// Water reaches the chunks across the x, y and z faces of the source
    /// chunk only by flowing. Water holders are active.
    fn terrain_world(f: &Fix, seed: u64) -> (World, WaterPass) {
        let m = f.t.materials();
        let stone = m.id("stone").unwrap();
        let ground = [
            stone,
            f.loam,
            f.hardpan,
            m.id("limestone").unwrap(),
            m.id("crushed_sulfide").unwrap(),
        ];
        let source = ChunkPos::new(0, 0, -1);
        let bounds = CellAabb::new(CellPos::new(-40, -6, -40), CellPos::new(39, 9, 39));
        assert_eq!(bounds.chunks().count(), 32);
        let mut w = World::new(Arc::clone(m), bounds);
        w.set_eco_rules(f.t.displacement_rules());
        let mut s = seed;
        for pos in bounds.chunks() {
            let mut c = Chunk::uniform(f.air);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let at = CellPos::from_chunk_local(pos, l);
                if !bounds.contains(at) {
                    continue;
                }
                let (x, y, z) = (at.0.x, at.0.y, at.0.z);
                let r = next(&mut s);
                let surface = -4 + ((x.abs() + z.abs()) / 6).min(6) + (r % 2) as i32;
                let reservoir = (1..=12).contains(&x) && (-12..=-1).contains(&z);
                let v = ((r >> 8) % 256) as u8;
                let (mat, moisture) = if y == bounds.min.0.y {
                    (stone, 0)
                } else if y <= surface {
                    let mat = ground[((r >> 16) % ground.len() as u64) as usize];
                    let porous = mat != stone && mat != f.hardpan;
                    (mat, if pos == source && porous { v } else { 0 })
                } else if reservoir && (0..=6).contains(&y) {
                    (f.water, v.max(1))
                } else if pos == source && y == 7 && (r >> 24).is_multiple_of(5) {
                    (stone, 0)
                } else if pos == source && y >= 8 && (r >> 24).is_multiple_of(3) {
                    (f.air, v)
                } else {
                    (f.air, 0)
                };
                c.storage.set(l, mat);
                c.layers.set_u8(LayerId::Moisture, l, moisture);
            }
            w.insert_chunk(pos, c).unwrap();
        }
        let pass = WaterPass::new(&f.t);
        activate_holders(&mut w, &pass);
        (w, pass)
    }

    /// Total moisture of the chunk at `pos`.
    fn chunk_moisture(w: &World, pos: ChunkPos) -> i32 {
        global_order(w.bounds())
            .filter(|c| c.split().0 == pos)
            .map(|c| w.layer(c, LayerId::Moisture))
            .sum()
    }

    /// The active-set sweep equals the flat reference after every sweep on
    /// a 32-chunk terrain, where a reservoir spills over a bowl across the
    /// x, y and z faces of its chunk, falls into the chunk below, soaks
    /// the porous ground and seeps on.
    #[test]
    fn oracle_equivalence_multi_chunk_world() {
        let f = Fix::new();
        let ids = water_ref(&f);
        let neighbors = [
            ("x", ChunkPos::new(-1, 0, -1)),
            ("y", ChunkPos::new(0, -1, -1)),
            ("z", ChunkPos::new(0, 0, 0)),
        ];
        for seed in 0..MULTI_CHUNK_SEEDS {
            let (mut w, mut pass) = terrain_world(&f, seed);
            for (axis, pos) in neighbors {
                assert_eq!(
                    chunk_moisture(&w, pos),
                    0,
                    "seed {seed}: {axis} neighbor starts dry"
                );
            }
            let before = InvariantChecker::new(&w);
            let mut flat = FlatWorld::from_world(&w, &[LayerId::Moisture]);
            let mut ledger = SeasonLedger::default();
            let mut visits = 0;
            for round in 0..MULTI_CHUNK_ROUNDS {
                for i in 0..pass.flow_iters() {
                    pass.set_iter(i);
                    visits += run_sweep(&mut w, &mut pass, &mut ledger).visits;
                    flat_sweep_water(&mut flat, &ids);
                    let diff = flat.first_difference(&w);
                    assert!(
                        diff.is_none(),
                        "seed {seed}, round {round}, sweep {i}: {diff:?}"
                    );
                }
            }
            assert_eq!(before.check(&w, &ledger), Ok(()), "seed {seed}: conserved");
            // The active sets visit far fewer cells than the reference's
            // every-cell sweeps.
            let cells = global_order(w.bounds()).count() as u64;
            let sweeps = u64::from(MULTI_CHUNK_ROUNDS) * u64::from(pass.flow_iters());
            assert!(visits < cells * sweeps / 4, "seed {seed}: {visits} visits");
            for (axis, pos) in neighbors {
                assert!(
                    chunk_moisture(&w, pos) > 0,
                    "seed {seed}: no water crossed the {axis} face"
                );
            }
        }
    }

    /// Settled cells (F173): a water or porous cell whose visit moved
    /// nothing is skipped until it or a face neighbor changes. On the
    /// 32-chunk terrain, sweeps that skip settled cells give the same
    /// world, move log and ledger after every sweep as sweeps that visit
    /// every active cell (the settled sets cleared before each), with
    /// fewer visits.
    #[test]
    fn settled_cells_skipped_without_changing_results() {
        let f = Fix::new();
        for seed in 0..MULTI_CHUNK_SEEDS {
            let (mut a, mut pa) = terrain_world(&f, seed);
            let (mut b, mut pb) = terrain_world(&f, seed);
            let (mut la, mut lb) = (SeasonLedger::default(), SeasonLedger::default());
            let (mut va, mut vb) = (0, 0);
            for round in 0..MULTI_CHUNK_ROUNDS {
                for i in 0..pa.flow_iters() {
                    pa.set_iter(i);
                    pb.set_iter(i);
                    va += run_sweep(&mut a, &mut pa, &mut la).visits;
                    b.eco_clear_water_settled(b.water_settle_key());
                    vb += run_sweep(&mut b, &mut pb, &mut lb).visits;
                    let at = format!("seed {seed}, round {round}, sweep {i}");
                    assert_eq!(a.hash(), b.hash(), "{at}: worlds");
                    assert_eq!(pa.log(), pb.log(), "{at}: move logs");
                    assert_eq!(la, lb, "{at}: ledgers");
                }
            }
            assert!(
                va < vb,
                "seed {seed}: {va} visits with settling, {vb} without"
            );
        }
    }

    /// A settled cell wakes when it or a face neighbor changes moisture or
    /// material, by a pass or an edit, and not for a change further away.
    #[test]
    fn settled_cell_wakes_on_neighbor_change() {
        let f = Fix::new();
        let pocket = CellPos::new(10, SLAB_Y, 10);
        let mut w = f.world(&[(pocket, f.water)]);
        w.eco_put_layer(pocket, LayerId::Moisture, i32::from(u8::MAX));
        let mut pass = WaterPass::new(&f.t);
        let mut ledger = SeasonLedger::default();
        let settled = |w: &World| {
            let (pos, local) = pocket.split();
            w.chunk(pos).unwrap().water_settled.contains(local)
        };
        // A full pocket in stone has nowhere to go (air above takes none).
        assert_eq!(run_sweep(&mut w, &mut pass, &mut ledger).visits, 1);
        assert!(settled(&w), "a visit that moved nothing settles the cell");
        assert_eq!(run_sweep(&mut w, &mut pass, &mut ledger).visits, 0);
        // Two cells away: still settled.
        w.eco_put_layer(CellPos::new(12, SLAB_Y + 1, 10), LayerId::Moisture, 5);
        assert!(settled(&w));
        // The stone beside the pocket turns to air: the pocket can spill.
        let side = CellPos::new(11, SLAB_Y, 10);
        w.eco_set(side, f.air, &mut ledger);
        assert!(!settled(&w), "a neighbor's material change wakes it");
        run_sweep(&mut w, &mut pass, &mut ledger);
        assert!(
            w.layer(side, LayerId::Moisture) > 0,
            "the woken pocket spilled"
        );
    }

    /// Settled sets hold for one set of water constants: a pass with the
    /// same constants keeps them, and one with other constants clears them
    /// before it sweeps.
    #[test]
    fn settled_sets_clear_for_other_constants() {
        let f = Fix::new();
        let pocket = CellPos::new(10, SLAB_Y, 10);
        let mut w = f.world(&[(pocket, f.water)]);
        w.eco_put_layer(pocket, LayerId::Moisture, i32::from(u8::MAX));
        let mut ledger = SeasonLedger::default();
        let mut pass = WaterPass::new(&f.t);
        assert_eq!(run_sweep(&mut w, &mut pass, &mut ledger).visits, 1);
        assert_eq!(w.water_settle_key(), pass.settle_key);
        let mut same = WaterPass::new(&f.t);
        assert_eq!(
            run_sweep(&mut w, &mut same, &mut ledger).visits,
            0,
            "the same constants keep the pocket settled"
        );
        let mut other = WaterPass::new(&f.t);
        other.flow_cap -= 1;
        other.settle_key = other.constants_key(&f.t);
        assert_ne!(other.settle_key, pass.settle_key);
        assert_eq!(
            run_sweep(&mut w, &mut other, &mut ledger).visits,
            1,
            "other constants clear the settled sets"
        );
        assert_eq!(w.water_settle_key(), other.settle_key);
        assert_eq!(
            run_sweep(&mut w, &mut pass, &mut ledger).visits,
            1,
            "and so does switching back"
        );
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
            let ids = water_ref(&f);
            // The oracle knows every porous row; the random worlds use the
            // test loam and hardpan (the shipped porous rows may be absent).
            let porous: Vec<MaterialId> = ids.porous.iter().map(|p| p.0).collect();
            prop_assert!(
                porous.contains(&f.loam) && porous.contains(&f.hardpan),
                "the test loam and hardpan"
            );
            let mut ledger = SeasonLedger::default();
            for sweep in 0..pass.flow_iters() {
                run_sweep(&mut w, &mut pass, &mut ledger);
                flat_sweep_water(&mut flat, &ids);
                let diff = flat.first_difference(&w);
                prop_assert!(diff.is_none(), "sweep {}: {:?}", sweep, diff);
            }
        }
    }
}
