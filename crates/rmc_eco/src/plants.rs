//! Plants (SAD-P2 §7.8): the growth pass and the season's aging.
//!
//! A plant is a cell of a passable solid plant material with a
//! [`PlantState`] in its chunk's `plants` map; the cell below is its
//! anchor. Species are the rows of `plants.ron`, indexed by
//! `PlantState::species`.
//!
//! **Growth (Jacobi; sorted apply)**, per plant cell, from the previous
//! state:
//!
//! 1. Needs: light and temperature from the plant cell; moisture and
//!    substances (`substance_min` at least, `substance_max` at most) from
//!    the anchor. Unmet: `stress += 1`; met: `stress = 0`.
//! 2. Death when `stress ≥ eco.stress_death` or `age_seasons ≥
//!    lifespan_seasons`: the cell becomes air through `World::eco_set`,
//!    whose §7.0 displacement removes the plant state, returns `held` into
//!    an `anchor` material below (the rest is `saturation_loss`) and takes
//!    a tall plant's upper cell with it. The pass adds `litter` of the
//!    organic substance to the anchor (ledger `litter_in`, a source; what
//!    finds no room is `saturation_loss`) and counts `plants_died`.
//! 3. Else (stressed or not) `growth += growth_per_substep` (saturating
//!    at 255) and, per `(x, amt)` of `uptake`, `take = min(amt, anchor_x)` leaves the anchor:
//!    a `holds` species keeps it in `held` (capped by `held`'s room; still
//!    inside the substance total), any other books `uptake_sink`.
//! 4. A `tall` species whose growth after this pass is above
//!    `eco.tall_growth` makes an air cell above it its own material (no
//!    second plant state).
//!
//! The anchor and the cell above may lie in a neighbor chunk, so the pass
//! writes them as [`Queued`] changes. Only one plant sits on an anchor, so
//! the queued deltas never compete.
//!
//! **Spread (sweep, after growth)**, per mature plant (growth 255, its
//! cell of its species' material) in the global cell order: when the seeded
//! [`spread_roll`] of `(seed, season, sub-step, cell)` is below
//! `spread_chance`, the plant takes the first cell in the global order
//! within `spread_radius` horizontally (a square) and ±1 vertically that is
//! air and whose cell below carries one of `anchor_tags` and passes the
//! species' needs. The new plant is placed at once (`World::eco_set`, then
//! a fresh [`PlantState`]), so no later plant claims the same cell; ledger
//! `plants_born`. The spread active set holds the mature plants: the pass
//! adds them from the chunks' plant maps before the sweep, and a visited
//! cell that is no longer a mature plant leaves it.
//!
//! **Aging.** At the end of each season every plant's `age_seasons` goes up
//! by one ([`age_plants`]); a plant that reached its lifespan dies in the
//! next season's first growth pass.

use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::layers::{LayerId, PlantState, SWEEP_SPREAD};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::MaterialId;
use rmc_world::world::World;
use xxhash_rust::xxh3::Xxh3;

use crate::reference::order_key;
use crate::runner::{
    ChunkView, JacobiOut, JacobiPass, Queued, SweepCtx, SweepPass, SweepReport, run_sweep,
};
use crate::tables::EcoTables;

/// One `plants.ron` row, resolved to ids and widened.
#[derive(Clone, Debug)]
struct Species {
    /// `None` while the material table lacks the species' material.
    material: Option<MaterialId>,
    light: i32,
    moisture: i32,
    /// Inclusive, 0.1 °C.
    temp: (i32, i32),
    substance_min: Vec<(u8, i32)>,
    substance_max: Vec<(u8, i32)>,
    growth_per_substep: i32,
    litter: i32,
    lifespan_seasons: u8,
    uptake: Vec<(u8, i32)>,
    holds: bool,
    tall: bool,
    /// Per material id: whether it carries one of `anchor_tags`.
    anchors: Vec<bool>,
    spread_chance: u8,
    spread_radius: i32,
}

impl Species {
    /// Every `plants.ron` row, in row order.
    fn all(tables: &EcoTables) -> Vec<Self> {
        let sub = |pairs: &[(String, u8)]| -> Vec<(u8, i32)> {
            pairs
                .iter()
                .map(|(key, n)| {
                    let x = tables
                        .substance_index(key)
                        .expect("the tables validate plant substance keys");
                    (x, i32::from(*n))
                })
                .collect()
        };
        let m = tables.materials();
        tables
            .plants()
            .iter()
            .map(|p| Species {
                material: m.id(&p.material),
                light: i32::from(p.needs.light),
                moisture: i32::from(p.needs.moisture),
                temp: (i32::from(p.needs.temp.0), i32::from(p.needs.temp.1)),
                substance_min: sub(&p.needs.substance_min),
                substance_max: sub(&p.needs.substance_max),
                growth_per_substep: i32::from(p.growth_per_substep),
                litter: i32::from(p.litter),
                lifespan_seasons: p.lifespan_seasons,
                uptake: sub(&p.uptake),
                holds: p.holds,
                tall: p.tall,
                anchors: m
                    .iter()
                    .map(|d| p.anchor_tags.iter().any(|tag| d.has_tag(tag)))
                    .collect(),
                spread_chance: p.spread_chance,
                spread_radius: i32::from(p.spread_radius),
            })
            .collect()
    }

    /// Whether material `id` carries one of the species' `anchor_tags`.
    fn anchors_on(&self, id: MaterialId) -> bool {
        self.anchors
            .get(usize::from(id.0))
            .copied()
            .unwrap_or(false)
    }

    /// §7.8 step 1: whether the needs hold at the plant `cell` and its
    /// `anchor`, reading layers through `layer`.
    fn needs_met(
        &self,
        layer: impl Fn(CellPos, LayerId) -> i32,
        cell: CellPos,
        anchor: CellPos,
    ) -> bool {
        let temp = layer(cell, LayerId::Temperature);
        layer(cell, LayerId::Light) >= self.light
            && (self.temp.0..=self.temp.1).contains(&temp)
            && layer(anchor, LayerId::Moisture) >= self.moisture
            && self
                .substance_min
                .iter()
                .all(|&(x, min)| layer(anchor, LayerId::Substance(x)) >= min)
            && self
                .substance_max
                .iter()
                .all(|&(x, max)| layer(anchor, LayerId::Substance(x)) <= max)
    }
}

/// The plant growth pass (SAD-P2 §7.8), built once from the tables.
#[derive(Clone, Debug)]
pub struct GrowthPass {
    species: Vec<Species>,
    stress_death: u8,
    tall_growth: Option<i32>,
    /// Substance index of role `organic`: what litter adds.
    organic: Option<u8>,
    air: MaterialId,
}

impl GrowthPass {
    pub fn new(tables: &EcoTables) -> Self {
        let species = Species::all(tables);
        let params = tables.params();
        Self {
            species,
            stress_death: params.stress_death,
            tall_growth: params.tall_growth.map(i32::from),
            organic: tables.role_substance("organic"),
            air: tables.role_material("air").unwrap_or(MaterialId::AIR),
        }
    }
}

impl GrowthPass {
    /// Step 2: the plant at local index `i` dies.
    fn die(
        &self,
        view: &ChunkView<'_>,
        sp: &Species,
        i: u16,
        anchor: CellPos,
        out: &mut JacobiOut,
    ) {
        out.materials.push((i, self.air));
        out.ledger.plants_died += 1;
        if let Some(c) = self.organic
            && sp.litter > 0
            && view.material(anchor).is_some()
        {
            out.ledger.litter_in[usize::from(c)] += i64::from(sp.litter);
            out.queued.push(Queued {
                target: anchor,
                layer: LayerId::Substance(c),
                amount: sp.litter,
                then_set: None,
            });
        }
    }
}

impl JacobiPass for GrowthPass {
    fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
        let mut out = JacobiOut::default();
        let cap = i32::from(u8::MAX);
        let bounds = view.bounds();
        for (&i, plant) in &view.chunk().plants {
            let cell = view.cell(LocalPos::from_index(i));
            let Some(sp) = self.species.get(usize::from(plant.species)) else {
                continue;
            };
            // Only a plant cell of its species' material grows, and only on
            // an anchor that is not itself a plant.
            if !bounds.contains(cell) || sp.material.is_none() || view.material(cell) != sp.material
            {
                continue;
            }
            let anchor = below(cell);
            if view.plant(anchor).is_some() {
                continue;
            }
            let met = sp.needs_met(|c, id| view.layer(c, id), cell, anchor);
            let stress = if met {
                0
            } else {
                plant.stress.saturating_add(1)
            };
            if stress >= self.stress_death || plant.age_seasons >= sp.lifespan_seasons {
                self.die(view, sp, i, anchor, &mut out);
                continue;
            }
            let mut next = PlantState { stress, ..*plant };
            let growth = view.layer(cell, LayerId::Growth);
            let add = sp.growth_per_substep.min(cap - growth);
            if add > 0 {
                out.layers.push((i, LayerId::Growth, add));
            }
            for &(x, amt) in &sp.uptake {
                let layer = LayerId::Substance(x);
                let held = &mut next.held[usize::from(x)];
                let mut take = amt.min(view.layer(anchor, layer));
                if sp.holds {
                    take = take.min(cap - i32::from(*held));
                }
                if take <= 0 {
                    continue;
                }
                out.queued.push(Queued {
                    target: anchor,
                    layer,
                    amount: -take,
                    then_set: None,
                });
                if sp.holds {
                    *held += u8::try_from(take).expect("take is capped by held's room");
                } else {
                    out.ledger.uptake_sink[usize::from(x)] += i64::from(take);
                }
            }
            if next != *plant {
                out.plants.push((i, Some(next)));
            }
            if let (true, Some(tall), Some(m)) = (sp.tall, self.tall_growth, sp.material) {
                let top = above(cell);
                if growth + add > tall && view.material(top) == Some(self.air) {
                    out.queued.push(Queued::set_material(top, m));
                }
            }
        }
        out
    }
}

/// The plant spread sweep (SAD-P2 §7.8), built once per season.
#[derive(Clone, Debug)]
pub struct SpreadPass {
    species: Vec<Species>,
    air: MaterialId,
    season: u32,
    seed: u64,
    /// The sub-step the next run belongs to.
    substep: u8,
}

impl SpreadPass {
    /// The spread of season `season`, seeded by the world seed `seed`.
    pub fn new(tables: &EcoTables, season: u32, seed: u64) -> Self {
        Self {
            species: Species::all(tables),
            air: tables.role_material("air").unwrap_or(MaterialId::AIR),
            season,
            seed,
            substep: 0,
        }
    }

    /// Sets the sub-step the next [`SpreadPass::run`] belongs to (part of
    /// the roll).
    pub fn set_substep(&mut self, substep: u8) {
        self.substep = substep;
    }

    /// Adds every mature plant to the spread active set, then runs the
    /// sweep.
    pub fn run(&mut self, world: &mut World, ledger: &mut SeasonLedger) -> SweepReport {
        let mature: Vec<CellPos> = world
            .iter_chunks_sorted()
            .flat_map(|(pos, chunk)| {
                chunk
                    .plants
                    .keys()
                    .map(move |&i| CellPos::from_chunk_local(pos, LocalPos::from_index(i)))
            })
            .filter(|&cell| self.mature(world, cell).is_some())
            .collect();
        for cell in mature {
            world.eco_activate(cell, SWEEP_SPREAD);
        }
        run_sweep(world, self, ledger)
    }

    /// The species index and row of a mature plant at `cell`: a plant
    /// state, its cell of its species' material, growth 255.
    fn mature(&self, world: &World, cell: CellPos) -> Option<(u8, &Species)> {
        let plant = world.plant(cell)?;
        let sp = self.species.get(usize::from(plant.species))?;
        let mature = sp.material.is_some()
            && world.get_cell(cell) == sp.material
            && world.layer(cell, LayerId::Growth) == i32::from(u8::MAX);
        mature.then_some((plant.species, sp))
    }

    /// The first cell in the global order within `sp`'s reach of `cell`
    /// that is air on an anchor with one of `sp`'s tags and passes `sp`'s
    /// needs.
    fn target(&self, world: &World, sp: &Species, cell: CellPos) -> Option<CellPos> {
        let (c, r) = (cell.0, sp.spread_radius);
        let mut reach: Vec<CellPos> = (c.y - 1..=c.y + 1)
            .flat_map(|y| {
                (c.z - r..=c.z + r)
                    .flat_map(move |z| (c.x - r..=c.x + r).map(move |x| CellPos::new(x, y, z)))
            })
            .collect();
        reach.sort_by_key(|&t| order_key(t));
        reach.into_iter().find(|&t| self.takes(world, sp, t))
    }

    /// Whether a plant of `sp` can start at `cell`: an air cell in the
    /// bounds whose cell below carries one of `sp`'s anchor tags, with
    /// `sp`'s needs met there.
    fn takes(&self, world: &World, sp: &Species, cell: CellPos) -> bool {
        let anchor = CellPos::new(cell.0.x, cell.0.y - 1, cell.0.z);
        world.bounds().contains(cell)
            && world.get_cell(cell) == Some(self.air)
            && world.get_cell(anchor).is_some_and(|a| sp.anchors_on(a))
            && sp.needs_met(|c, id| world.layer(c, id), cell, anchor)
    }
}

impl SweepPass for SpreadPass {
    fn kind(&self) -> usize {
        SWEEP_SPREAD
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        let Some((species, sp)) = self.mature(ctx.world, cell) else {
            return;
        };
        let Some(material) = sp.material else {
            return;
        };
        if spread_roll(self.seed, self.season, self.substep, cell) >= sp.spread_chance {
            return;
        }
        let Some(target) = self.target(ctx.world, sp, cell) else {
            return;
        };
        ctx.world.eco_set(target, material, ctx.ledger);
        ctx.world.eco_put_plant(
            target,
            Some(PlantState {
                species,
                ..PlantState::default()
            }),
        );
        ctx.ledger.plants_born += 1;
    }

    fn holds(&self, world: &World, cell: CellPos) -> bool {
        self.mature(world, cell).is_some()
    }
}

/// The species as worldgen's surface plants see them (SAD-P2 §8): which
/// species it seeds, and whether one may start on a given anchor.
#[derive(Clone, Debug)]
pub struct PlantStarts {
    species: Vec<Species>,
    chances: Vec<u8>,
}

impl PlantStarts {
    /// From the tables, whose material table must be the one the world
    /// uses.
    pub fn new(tables: &EcoTables) -> Self {
        Self {
            species: Species::all(tables),
            chances: tables.plants().iter().map(|p| p.worldgen_chance).collect(),
        }
    }

    /// `(species index, material, worldgen_chance)` of every species
    /// worldgen seeds (a chance above 0 and a material in the table), in
    /// row order.
    pub fn seeded(&self) -> impl Iterator<Item = (u8, MaterialId, u8)> + '_ {
        self.species
            .iter()
            .zip(&self.chances)
            .enumerate()
            .filter_map(|(i, (sp, &chance))| {
                let i = u8::try_from(i).ok()?;
                (chance > 0).then_some((i, sp.material?, chance))
            })
    }

    /// Whether a plant of species `species` may start at `cell` over an
    /// anchor of material `anchor`: the anchor carries one of its
    /// `anchor_tags` and its needs hold (§7.8 step 1), reading layers
    /// through `layer`. The caller checks that `cell` is air.
    pub fn passes(
        &self,
        species: u8,
        cell: CellPos,
        anchor: MaterialId,
        layer: impl Fn(CellPos, LayerId) -> i32,
    ) -> bool {
        let Some(sp) = self.species.get(usize::from(species)) else {
            return false;
        };
        let below = CellPos::new(cell.0.x, cell.0.y - 1, cell.0.z);
        sp.anchors_on(anchor) && sp.needs_met(layer, cell, below)
    }
}

/// The seeded spread roll of the plant at `cell` in sub-step `substep` of
/// season `season`, 0..=255 (SAD-P2 §7.8: the hash's low byte).
pub fn spread_roll(seed: u64, season: u32, substep: u8, cell: CellPos) -> u8 {
    let mut h = Xxh3::with_seed(seed);
    h.update(&season.to_le_bytes());
    h.update(&[substep]);
    h.update(&cell.0.x.to_le_bytes());
    h.update(&cell.0.y.to_le_bytes());
    h.update(&cell.0.z.to_le_bytes());
    h.digest().to_le_bytes()[0]
}

/// Adds one season to every plant's `age_seasons` (saturating), in sorted
/// chunk and cell order. Returns how many plants aged.
pub fn age_plants(world: &mut World) -> u32 {
    let aged: Vec<(CellPos, PlantState)> = world
        .iter_chunks_sorted()
        .flat_map(|(pos, chunk)| {
            chunk.plants.iter().map(move |(&i, p)| {
                let cell = CellPos::from_chunk_local(pos, LocalPos::from_index(i));
                let older = PlantState {
                    age_seasons: p.age_seasons.saturating_add(1),
                    ..*p
                };
                (cell, older)
            })
        })
        .collect();
    let mut n = 0;
    for (cell, plant) in aged {
        n += u32::from(world.eco_put_plant(cell, Some(plant)));
    }
    n
}

fn below(c: CellPos) -> CellPos {
    CellPos::new(c.0.x, c.0.y - 1, c.0.z)
}

fn above(c: CellPos) -> CellPos {
    CellPos::new(c.0.x, c.0.y + 1, c.0.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditResult, EditStatus, PlayerId};
    use rmc_world::inventory::Inventory;
    use rmc_world::layers::N_SUBSTANCES;
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::shape::Shape;

    use crate::activity::Activity;
    use crate::invariants::{InvariantChecker, Totals};
    use crate::runner::{Exec, activate_holders, run_jacobi};
    use crate::tick::{PassId, season_tick_observed};
    use crate::water::WaterPass;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    fn id(t: &EcoTables, key: &str) -> MaterialId {
        t.materials().id(key).unwrap()
    }

    fn sub(t: &EcoTables, key: &str) -> LayerId {
        LayerId::Substance(t.substance_index(key).unwrap())
    }

    fn species(t: &EcoTables, key: &str) -> u8 {
        t.plants().iter().position(|p| p.key == key).unwrap() as u8
    }

    /// Two chunks stacked: topsoil up to y = 30, air above. Plants at
    /// y = 31 anchor in their own chunk; a plant at y = 32 on a soil cell
    /// at y = 31 anchors in the chunk below.
    fn world(t: &EcoTables) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 63, 31));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let soil = id(t, "topsoil");
        let mut low = Chunk::uniform(soil);
        for z in 0..32 {
            for x in 0..32 {
                low.storage.set(
                    CellPos::new(x, 31, z).split().1,
                    t.role_material("air").unwrap(),
                );
            }
        }
        w.insert_chunk(ChunkPos::new(0, 0, 0), low).unwrap();
        w.insert_chunk(ChunkPos::new(0, 1, 0), Chunk::uniform(MaterialId::AIR))
            .unwrap();
        w
    }

    /// A plant of `key` at `cell` with `state`, its cell lit to `light`
    /// at 20.0 °C, and its anchor holding `moisture` and `subs`. Makes the
    /// anchor topsoil when it is air.
    #[allow(clippy::too_many_arguments)]
    fn plant(
        w: &mut World,
        t: &EcoTables,
        cell: CellPos,
        key: &str,
        state: PlantState,
        light: i32,
        moisture: i32,
        subs: &[(&str, i32)],
    ) {
        let mut scratch = SeasonLedger::default();
        let anchor = below(cell);
        if w.get_cell(anchor) == Some(MaterialId::AIR) {
            w.eco_set(anchor, id(t, "topsoil"), &mut scratch);
        }
        let row = &t.plants()[usize::from(species(t, key))];
        w.eco_set(cell, id(t, &row.material), &mut scratch);
        assert_eq!(scratch, SeasonLedger::default());
        w.eco_put_plant(
            cell,
            Some(PlantState {
                species: species(t, key),
                ..state
            }),
        );
        w.eco_put_layer(cell, LayerId::Light, light);
        w.eco_put_layer(cell, LayerId::Temperature, 200);
        w.eco_put_layer(anchor, LayerId::Moisture, moisture);
        for &(x, n) in subs {
            w.eco_put_layer(anchor, sub(t, x), n);
        }
    }

    /// Runs the growth pass once, checking the invariants.
    fn grow(w: &mut World, t: &EcoTables, exec: Exec) -> SeasonLedger {
        let checker = InvariantChecker::new(w);
        let mut ledger = SeasonLedger::default();
        run_jacobi(w, &GrowthPass::new(t), exec, &mut ledger);
        checker.check(w, &ledger).unwrap_or_else(|v| panic!("{v}"));
        ledger
    }

    /// A stressed case: name, light, temperature, anchor moisture, anchor
    /// substances.
    type Unmet<'a> = (&'a str, i32, i32, i32, &'a [(&'a str, i32)]);

    const FED: [(&str, i32); 4] = [("n", 50), ("p", 50), ("k", 50), ("cu", 10)];

    #[test]
    fn needs_met_grows() {
        let t = tables();
        let held = |x: &str| usize::from(t.substance_index(x).unwrap());
        let growth = |w: &World, c: CellPos| w.layer(c, LayerId::Growth);
        let mut w = world(&t);
        let healthy = PlantState::default();

        // Grass anchored in its own chunk; fireweed (holds) anchored in the
        // chunk below; a tall shrub about to pass tall_growth, whose upper
        // cell is in the chunk above.
        let grass = CellPos::new(2, 31, 2);
        let fireweed = CellPos::new(5, 32, 5);
        let shrub = CellPos::new(8, 31, 8);
        plant(&mut w, &t, grass, "grass", healthy, 255, 100, &FED);
        plant(&mut w, &t, fireweed, "fireweed", healthy, 255, 100, &FED);
        plant(&mut w, &t, shrub, "shrub", healthy, 255, 100, &FED);
        let tall = i32::from(t.params().tall_growth.expect("shipped tall_growth"));
        let shrub_row = &t.plants()[usize::from(species(&t, "shrub"))];
        assert!(shrub_row.tall);
        let start = tall - i32::from(shrub_row.growth_per_substep) + 1;
        w.eco_put_layer(shrub, LayerId::Growth, start);

        // Each need broken alone: the plant is stressed but, short of
        // death, still grows and takes up (§7.8 step 3 is the death check's
        // else).
        let unmet: [Unmet; 6] = [
            ("dark", 0, 200, 100, &FED),
            ("cold", 255, -50, 100, &FED),
            ("hot", 255, 900, 100, &FED),
            ("dry", 255, 200, 0, &FED),
            ("hungry", 255, 200, 100, &[("n", 0), ("p", 50)]),
            (
                "copper",
                255,
                200,
                100,
                &[("n", 50), ("p", 50), ("cu", 200)],
            ),
        ];
        let stressed: Vec<CellPos> = (0..unmet.len() as i32)
            .map(|i| CellPos::new(14 + 2 * i, 31, 20))
            .collect();
        for (&c, &(_, light, temp, moisture, subs)) in stressed.iter().zip(&unmet) {
            plant(&mut w, &t, c, "grass", healthy, light, moisture, subs);
            w.eco_put_layer(c, LayerId::Temperature, temp);
        }

        let mut parallel = w.clone();
        let ledger = grow(&mut w, &t, Exec::Serial);
        assert_eq!(grow(&mut parallel, &t, Exec::Parallel), ledger);
        assert_eq!(w.hash(), parallel.hash(), "serial and parallel agree");

        // Growth by growth_per_substep.
        let row = |key: &str| &t.plants()[usize::from(species(&t, key))];
        assert_eq!(
            growth(&w, grass),
            i32::from(row("grass").growth_per_substep)
        );
        assert_eq!(
            growth(&w, fireweed),
            i32::from(row("fireweed").growth_per_substep)
        );
        assert_eq!(growth(&w, shrub), tall + 1);

        // Uptake: grass and shrub sink theirs, fireweed holds it.
        let anchor = |w: &World, c: CellPos, x: &str| w.layer(below(c), sub(&t, x));
        assert_eq!((anchor(&w, grass, "n"), anchor(&w, grass, "p")), (49, 49));
        assert_eq!(anchor(&w, grass, "k"), 50, "grass takes no k");
        assert_eq!(
            (anchor(&w, fireweed, "cu"), anchor(&w, fireweed, "n")),
            (8, 49)
        );
        assert_eq!(
            (
                anchor(&w, shrub, "n"),
                anchor(&w, shrub, "p"),
                anchor(&w, shrub, "k")
            ),
            (49, 49, 49)
        );
        let fw = w.plant(fireweed).unwrap();
        assert_eq!((fw.held[held("cu")], fw.held[held("n")]), (2, 1));
        assert_eq!(fw.stress, 0);
        assert_eq!(w.plant(grass).unwrap().held, [0; N_SUBSTANCES]);
        // Grass and shrub sink one n and one p each, and every stressed
        // grass one p and, unless its anchor has none, one n.
        let fed_n = stressed
            .iter()
            .zip(&unmet)
            .filter(|(_, u)| unmet_n(&unmet, u.0) > 0)
            .count() as i64;
        let mut want = SeasonLedger::default();
        want.uptake_sink[held("n")] = 2 + fed_n;
        want.uptake_sink[held("p")] = 2 + unmet.len() as i64;
        want.uptake_sink[held("k")] = 1;
        assert_eq!(ledger, want, "only sunk uptake is booked");

        // The tall shrub occupies the air cell above, in the chunk above,
        // without a second plant state.
        assert_eq!(w.get_cell(above(shrub)), Some(id(&t, "shrub")));
        assert!(w.plant(above(shrub)).is_none());
        assert_eq!(w.get_cell(above(grass)), Some(MaterialId::AIR));

        for (&c, &(name, ..)) in stressed.iter().zip(&unmet) {
            let p = w.plant(c).unwrap_or_else(|| panic!("{name} lives"));
            assert_eq!(p.stress, 1, "{name} is stressed");
            assert_eq!(
                growth(&w, c),
                i32::from(row("grass").growth_per_substep),
                "{name} still grows"
            );
            assert_eq!(
                anchor(&w, c, "n"),
                (unmet_n(&unmet, name) - 1).max(0),
                "{name} still takes up"
            );
        }

        // Needs met again: stress resets.
        let dark = stressed[0];
        w.eco_put_layer(dark, LayerId::Light, 255);
        grow(&mut w, &t, Exec::Serial);
        assert_eq!(w.plant(dark).unwrap().stress, 0);
        assert!(growth(&w, dark) > 0);

        // Growth saturates at 255, and a plant at 255 keeps taking up.
        w.eco_put_layer(grass, LayerId::Growth, 250);
        grow(&mut w, &t, Exec::Serial);
        assert_eq!(growth(&w, grass), 255);
        let n = anchor(&w, grass, "n");
        grow(&mut w, &t, Exec::Serial);
        assert_eq!(growth(&w, grass), 255);
        assert_eq!(anchor(&w, grass, "n"), n - 1);

        // Uptake takes no more than the anchor holds.
        let n_layer = sub(&t, "n");
        w.eco_put_layer(below(grass), n_layer, 0);
        w.eco_put_layer(below(fireweed), sub(&t, "cu"), 1);
        let ledger = grow(&mut w, &t, Exec::Serial);
        assert_eq!(anchor(&w, fireweed, "cu"), 0);
        // Four runs of 2 before, then the 1 the anchor had left.
        assert_eq!(w.plant(fireweed).unwrap().held[held("cu")], 4 * 2 + 1);
        assert_eq!(ledger.uptake_sink[held("cu")], 0);

        // Held takes no more than its room; the rest stays in the anchor.
        let full = CellPos::new(2, 31, 26);
        let mut nearly = [0; N_SUBSTANCES];
        nearly[held("cu")] = u8::MAX - 1;
        let brimming = PlantState {
            held: nearly,
            ..PlantState::default()
        };
        plant(&mut w, &t, full, "fireweed", brimming, 255, 100, &FED);
        // A shrub reaching exactly tall_growth stays one cell; one passing
        // it under a solid leaves the solid.
        let at = CellPos::new(20, 31, 8);
        let capped = CellPos::new(23, 31, 8);
        plant(&mut w, &t, at, "shrub", healthy, 255, 100, &FED);
        plant(&mut w, &t, capped, "shrub", healthy, 255, 100, &FED);
        w.eco_put_layer(at, LayerId::Growth, start - 1);
        w.eco_put_layer(capped, LayerId::Growth, start);
        let roof = id(&t, "topsoil");
        let mut scratch = SeasonLedger::default();
        w.eco_set(above(capped), roof, &mut scratch);
        grow(&mut w, &t, Exec::Serial);
        assert_eq!(w.plant(full).unwrap().held[held("cu")], u8::MAX);
        assert_eq!(anchor(&w, full, "cu"), 10 - 1);
        assert_eq!(growth(&w, at), tall);
        assert_eq!(w.get_cell(above(at)), Some(MaterialId::AIR));
        assert_eq!(growth(&w, capped), tall + 1);
        assert_eq!(w.get_cell(above(capped)), Some(roof));
    }

    /// The anchor `n` a stressed case started with.
    fn unmet_n(unmet: &[Unmet], name: &str) -> i32 {
        let (.., subs) = unmet.iter().find(|u| u.0 == name).unwrap();
        subs.iter().find(|s| s.0 == "n").map_or(0, |s| s.1)
    }

    #[test]
    fn stress_death_litter_ledgered_source() {
        let t = tables();
        let x = |k: &str| usize::from(t.substance_index(k).unwrap());
        let c = t.role_substance("organic").unwrap();
        let organic = LayerId::Substance(c);
        let litter = |key: &str| i32::from(t.plants()[usize::from(species(&t, key))].litter);
        let death = t.params().stress_death;
        let mut w = world(&t);
        let dying = PlantState {
            stress: death - 1,
            ..PlantState::default()
        };

        // Unmet needs at stress_death − 1: the next unmet sub-step kills.
        let grass = CellPos::new(2, 31, 2);
        plant(&mut w, &t, grass, "grass", dying, 0, 100, &FED);
        // Fireweed holding copper and nitrogen, anchored in the chunk below.
        let fireweed = CellPos::new(5, 32, 5);
        let mut held = [0; N_SUBSTANCES];
        held[x("cu")] = 30;
        held[x("n")] = 5;
        let holding = PlantState { held, ..dying };
        plant(&mut w, &t, fireweed, "fireweed", holding, 0, 100, &FED);
        // An anchor nearly full of organic matter: the litter overflows.
        let full = CellPos::new(8, 31, 2);
        plant(&mut w, &t, full, "grass", dying, 0, 100, &FED);
        w.eco_put_layer(below(full), organic, 250);
        // A tall shrub at its lifespan with every need met, its upper cell
        // grown.
        let shrub = CellPos::new(11, 31, 2);
        let lifespan = t.plants()[usize::from(species(&t, "shrub"))].lifespan_seasons;
        let old = PlantState {
            age_seasons: lifespan,
            ..PlantState::default()
        };
        plant(&mut w, &t, shrub, "shrub", old, 255, 100, &FED);
        let mut scratch = SeasonLedger::default();
        w.eco_set(above(shrub), id(&t, "shrub"), &mut scratch);
        w.eco_put_layer(shrub, LayerId::Growth, 255);
        // Stressed but not yet at the threshold: lives.
        let survivor = CellPos::new(14, 31, 2);
        let close = PlantState {
            stress: death - 2,
            ..PlantState::default()
        };
        plant(&mut w, &t, survivor, "grass", close, 0, 100, &FED);

        let ledger = grow(&mut w, &t, Exec::Serial);

        for cell in [grass, fireweed, full, shrub, above(shrub)] {
            assert_eq!(w.get_cell(cell), Some(MaterialId::AIR), "{cell:?} is air");
            assert!(w.plant(cell).is_none(), "{cell:?} has no plant");
        }
        let s = w.plant(survivor).expect("the survivor lives");
        assert_eq!(s.stress, death - 1);
        let grass_row = &t.plants()[usize::from(species(&t, "grass"))];
        assert_eq!(
            w.layer(survivor, LayerId::Growth),
            i32::from(grass_row.growth_per_substep),
            "a stressed plant short of death still grows"
        );

        // Litter lands on the anchor; held goes back into it.
        assert_eq!(w.layer(below(grass), organic), litter("grass"));
        assert_eq!(w.layer(below(shrub), organic), litter("shrub"));
        assert_eq!(w.layer(below(full), organic), 255);
        let anchor = below(fireweed);
        assert_eq!(anchor.split().0, ChunkPos::new(0, 0, 0));
        assert_eq!(w.layer(anchor, organic), litter("fireweed"));
        assert_eq!(w.layer(anchor, sub(&t, "cu")), 10 + 30);
        assert_eq!(w.layer(anchor, sub(&t, "n")), 50 + 5);

        // Litter is a source; what found no room is saturation_loss.
        let mut want = SeasonLedger {
            plants_died: 4,
            ..SeasonLedger::default()
        };
        want.litter_in[usize::from(c)] =
            i64::from(litter("grass") * 2 + litter("fireweed") + litter("shrub"));
        want.saturation_loss[usize::from(c)] = i64::from(250 + litter("grass") - 255);
        // The survivor's uptake (one n, one p) is sunk.
        want.uptake_sink[x("n")] = 1;
        want.uptake_sink[x("p")] = 1;
        assert_eq!(ledger, want);
    }

    #[test]
    fn plants_age_each_season_and_die_at_lifespan() {
        let t = tables();
        let mut w = world(&t);
        let grass = CellPos::new(2, 31, 2);
        let lifespan = t.plants()[usize::from(species(&t, "grass"))].lifespan_seasons;
        let aging = PlantState {
            age_seasons: lifespan - 1,
            ..PlantState::default()
        };
        plant(&mut w, &t, grass, "grass", aging, 255, 100, &FED);
        let young = CellPos::new(6, 31, 6);
        plant(
            &mut w,
            &t,
            young,
            "grass",
            PlantState::default(),
            255,
            100,
            &FED,
        );
        activate_holders(&mut w, &WaterPass::new(&t));
        let mut activity = Activity::new(t.params().quiet_skip);
        let mut died_at = None;
        for season in 0..2 {
            let checker = InvariantChecker::new(&w);
            let report = season_tick_observed(
                &mut w,
                &t,
                season,
                1,
                &mut activity,
                Exec::Serial,
                &mut |i, pass, w, ledger| {
                    if let Err(v) = checker.check(w, ledger) {
                        panic!("season {season}, sub-step {i}, after {pass:?}: {v}");
                    }
                    if pass == PassId::Growth && died_at.is_none() && w.plant(grass).is_none() {
                        died_at = Some((season, i));
                    }
                },
            );
            checker.check(&w, &report.ledger).unwrap();
            if season == 0 {
                assert_eq!(w.plant(grass).unwrap().age_seasons, lifespan);
                assert_eq!(w.plant(young).unwrap().age_seasons, 1);
                assert!(w.layer(grass, LayerId::Growth) > 0, "it grew");
            }
        }
        assert_eq!(
            died_at,
            Some((1, 0)),
            "dies in the next season's first growth"
        );
        assert_ne!(w.get_cell(grass), Some(id(&t, "grass")), "its cell cleared");
        assert_eq!(w.plant(young).map(|p| p.age_seasons), Some(2));
    }

    /// Breaks `cell` with one pickaxe edit, which harvests a plant there.
    fn dig(w: &mut World, seq: u32, cell: CellPos) -> EditResult {
        let issuer = PlayerId(1);
        let r = w.apply(&EditCommand {
            id: CommandId { issuer, seq },
            issuer,
            op: EditOp::Damage(u16::MAX),
            shape: Shape::Cell(cell),
        });
        assert_eq!(r.status, EditStatus::Applied);
        r
    }

    #[test]
    fn uptake_invariant_over_life_death_harvest() {
        let t = tables();
        let x = |k: &str| usize::from(t.substance_index(k).unwrap());
        let mut w = world(&t);
        let healthy = PlantState::default();
        let copper: [(&str, i32); 5] = [("n", 50), ("p", 50), ("k", 50), ("cu", 60), ("c", 0)];
        // Two fireweeds (holds) on copper soil, one anchored in the chunk
        // below; a grass (sinks) beside them.
        let (kept, doomed, grass) = (
            CellPos::new(2, 31, 2),
            CellPos::new(6, 32, 6),
            CellPos::new(10, 31, 10),
        );
        plant(&mut w, &t, kept, "fireweed", healthy, 255, 100, &copper);
        plant(&mut w, &t, doomed, "fireweed", healthy, 255, 100, &copper);
        plant(&mut w, &t, grass, "grass", healthy, 255, 100, &FED);

        // One checker from the start: every step's ledger folds into one.
        let checker = InvariantChecker::new(&w);
        let mut total = SeasonLedger::default();
        let step = |w: &mut World, total: &mut SeasonLedger, from: &SeasonLedger| {
            crate::ledger::merge(total, from);
            checker.check(w, total).unwrap_or_else(|v| panic!("{v}"));
        };

        // Life: uptake moves copper from the anchor into `held`, inside the
        // substance total; the grass's uptake leaves through uptake_sink.
        let runs = 10;
        for _ in 0..runs {
            let ledger = grow(&mut w, &t, Exec::Serial);
            step(&mut w, &mut total, &ledger);
        }
        let held = |w: &World, c: CellPos, k: &str| w.plant(c).map_or(0, |p| p.held[x(k)]);
        let fw = &t.plants()[usize::from(species(&t, "fireweed"))];
        let cu_rate = fw.uptake.iter().find(|u| u.0 == "cu").unwrap().1;
        let taken = i32::from(cu_rate) * runs;
        for c in [kept, doomed] {
            assert_eq!(i32::from(held(&w, c, "cu")), taken);
            assert_eq!(w.layer(below(c), sub(&t, "cu")), 60 - taken);
        }
        assert_eq!(total.uptake_sink[x("cu")], 0, "held copper is not sunk");
        assert_eq!(total.uptake_sink[x("n")], i64::from(runs), "grass sinks n");

        // Death: the doomed fireweed's `held` goes back into its anchor.
        let dying = PlantState {
            stress: t.params().stress_death - 1,
            ..*w.plant(doomed).unwrap()
        };
        w.eco_put_plant(doomed, Some(dying));
        w.eco_put_layer(doomed, LayerId::Light, 0);
        let ledger = grow(&mut w, &t, Exec::Serial);
        assert_eq!(ledger.plants_died, 1);
        step(&mut w, &mut total, &ledger);
        assert!(w.plant(doomed).is_none());
        assert_eq!(w.layer(below(doomed), sub(&t, "cu")), 60, "held returned");

        // Harvest: what the kept fireweed holds leaves through harvest_out,
        // and its copper comes out as copper_dust.
        let held_cu = held(&w, kept, "cu");
        let held_n = held(&w, kept, "n");
        assert!(held_cu > 0 && held_n > 0);
        let anchor_cu = w.layer(below(kept), sub(&t, "cu"));
        let r = dig(&mut w, 1, kept);
        let mut want = SeasonLedger::default();
        want.harvest_out[x("cu")] = i64::from(held_cu);
        want.harvest_out[x("n")] = i64::from(held_n);
        assert_eq!(*r.ledger, want);
        step(&mut w, &mut total, &r.ledger);
        let dust = (u32::from(held_cu) >> t.params().harvest_shift).max(1);
        assert_eq!(
            Inventory::from_items(&r.yields),
            Inventory::from_items(&[(id(&t, "fireweed"), 1), (id(&t, "copper_dust"), dust)])
        );
        assert!(w.plant(kept).is_none());
        assert_eq!(w.get_cell(kept), Some(MaterialId::AIR));
        assert_eq!(
            w.layer(below(kept), sub(&t, "cu")),
            anchor_cu,
            "a harvest returns nothing to the anchor"
        );

        // Over the whole life the copper total fell by exactly the harvest.
        let cu_now: i64 = Totals::measure(&w).substances[x("cu")];
        let cu_before = 2 * 60 + 10;
        assert_eq!(cu_now, cu_before - i64::from(held_cu));
    }

    #[test]
    fn fireweed_holds_copper_anchor_cu_falls() {
        let t = tables();
        let cu = sub(&t, "cu");
        let x = usize::from(t.substance_index("cu").unwrap());
        let tox = t.substances()[x].tox_plant.expect("cu is toxic to plants");
        let rich = i32::from(tox) * 5;
        let soil: [(&str, i32); 4] = [("n", 50), ("p", 50), ("k", 50), ("cu", rich)];
        let mut w = world(&t);
        // One anchored in its own chunk, one in the chunk below.
        let fireweeds = [CellPos::new(4, 31, 4), CellPos::new(9, 32, 9)];
        for &c in &fireweeds {
            plant(
                &mut w,
                &t,
                c,
                "fireweed",
                PlantState::default(),
                255,
                100,
                &soil,
            );
        }
        let fw = &t.plants()[usize::from(species(&t, "fireweed"))];
        assert!(fw.holds);
        let rate = i32::from(fw.uptake.iter().find(|u| u.0 == "cu").unwrap().1);

        // Each sub-step the anchor's copper falls by the uptake and the
        // plant's held copper rises by the same; nothing is sunk, and the
        // plant is never stressed by it.
        let mut runs = 0;
        while w.layer(below(fireweeds[0]), cu) > 0 {
            let before: Vec<i32> = fireweeds.iter().map(|&c| w.layer(below(c), cu)).collect();
            let ledger = grow(&mut w, &t, Exec::Serial);
            runs += 1;
            assert_eq!(ledger.uptake_sink[x], 0);
            assert_eq!(ledger.plants_died, 0);
            for (&c, &b) in fireweeds.iter().zip(&before) {
                assert_eq!(w.layer(below(c), cu), b - rate.min(b));
                let p = w.plant(c).expect("fireweed lives on copper");
                assert_eq!(i32::from(p.held[x]), rich - w.layer(below(c), cu));
                assert_eq!(p.stress, 0);
            }
        }
        assert_eq!(runs, (rich + rate - 1) / rate);
        for &c in &fireweeds {
            assert_eq!(i32::from(w.plant(c).unwrap().held[x]), rich);
        }
        // A drained anchor gives no more.
        grow(&mut w, &t, Exec::Serial);
        assert_eq!(w.plant(fireweeds[0]).map(|p| p.held[x]), Some(rich as u8));
    }

    #[test]
    fn fireweed_survives_copper_grass_dies() {
        let t = tables();
        let cu = sub(&t, "cu");
        let x = usize::from(t.substance_index("cu").unwrap());
        let organic = LayerId::Substance(t.role_substance("organic").unwrap());
        let row = |key: &str| &t.plants()[usize::from(species(&t, key))];
        // Copper above every cap but fireweed's (it has none).
        let max_cu = |key: &str| {
            row(key)
                .needs
                .substance_max
                .iter()
                .find(|s| s.0 == "cu")
                .map(|s| i32::from(s.1))
        };
        assert_eq!(max_cu("fireweed"), None);
        let rich = max_cu("grass").unwrap().max(max_cu("moss").unwrap()) * 4;
        let soil: [(&str, i32); 4] = [("n", 50), ("p", 50), ("k", 50), ("cu", rich)];
        let mut w = world(&t);
        let (fireweed, grass, moss) = (
            CellPos::new(3, 31, 3),
            CellPos::new(7, 31, 7),
            CellPos::new(11, 32, 11),
        );
        for (c, key) in [(fireweed, "fireweed"), (grass, "grass"), (moss, "moss")] {
            plant(&mut w, &t, c, key, PlantState::default(), 255, 100, &soil);
        }
        // The same plants on clean soil all live.
        let clean: [(&str, i32); 3] = [("n", 50), ("p", 50), ("k", 50)];
        let control = [CellPos::new(20, 31, 20), CellPos::new(24, 31, 24)];
        for (c, key) in control.iter().zip(["grass", "moss"]) {
            plant(&mut w, &t, *c, key, PlantState::default(), 255, 100, &clean);
        }

        let death = i32::from(t.params().stress_death);
        for run in 1..=death {
            let ledger = grow(&mut w, &t, Exec::Serial);
            if run < death {
                assert_eq!(ledger.plants_died, 0, "run {run}");
                for c in [grass, moss] {
                    assert_eq!(i32::from(w.plant(c).unwrap().stress), run);
                }
            } else {
                assert_eq!(ledger.plants_died, 2, "grass and moss die together");
            }
        }
        for c in [grass, moss] {
            assert!(w.plant(c).is_none(), "{c:?} died on copper");
            assert_eq!(w.get_cell(c), Some(MaterialId::AIR));
            assert!(w.layer(below(c), organic) > 0, "its litter");
            assert_eq!(w.layer(below(c), cu), rich, "grass and moss take no cu");
        }
        let fw = w.plant(fireweed).expect("fireweed survives copper");
        assert_eq!(fw.stress, 0);
        let rate = i32::from(
            row("fireweed")
                .uptake
                .iter()
                .find(|u| u.0 == "cu")
                .unwrap()
                .1,
        );
        assert_eq!(i32::from(fw.held[x]), (rate * death).min(rich));
        for c in control {
            assert_eq!(w.plant(c).map(|p| p.stress), Some(0), "{c:?} lives");
        }
    }

    /// Two chunks side by side along x: topsoil up to y = 30, air at
    /// y = 31. The border between them runs between x = 31 and x = 32.
    fn wide_world(t: &EcoTables) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut soil = Chunk::uniform(id(t, "topsoil"));
        for z in 0..32 {
            for x in 0..32 {
                soil.storage
                    .set(CellPos::new(x, 31, z).split().1, MaterialId::AIR);
            }
        }
        w.insert_chunk(ChunkPos::new(0, 0, 0), soil.clone())
            .unwrap();
        w.insert_chunk(ChunkPos::new(1, 0, 0), soil).unwrap();
        w
    }

    /// Makes the air cell `cell` a site where every shipped species'
    /// needs hold: lit, 20.0 °C, its anchor (topsoil) moist and fed.
    fn site(w: &mut World, t: &EcoTables, cell: CellPos) {
        assert_eq!(w.get_cell(cell), Some(MaterialId::AIR));
        assert_eq!(w.get_cell(below(cell)), Some(id(t, "topsoil")));
        w.eco_put_layer(cell, LayerId::Light, 255);
        w.eco_put_layer(cell, LayerId::Temperature, 200);
        w.eco_put_layer(below(cell), LayerId::Moisture, 100);
        for &(x, n) in &FED {
            w.eco_put_layer(below(cell), sub(t, x), n);
        }
    }

    /// A mature plant of `key` at `cell` (growth 255).
    fn mature(w: &mut World, t: &EcoTables, cell: CellPos, key: &str) {
        plant(w, t, cell, key, PlantState::default(), 255, 100, &FED);
        w.eco_put_layer(cell, LayerId::Growth, 255);
    }

    /// Runs the spread sweep once for sub-step `substep` of `season`,
    /// checking the invariants.
    fn spread(w: &mut World, t: &EcoTables, seed: u64, season: u32, substep: u8) -> SeasonLedger {
        let checker = InvariantChecker::new(w);
        let mut ledger = SeasonLedger::default();
        let mut pass = SpreadPass::new(t, season, seed);
        pass.set_substep(substep);
        pass.run(w, &mut ledger);
        checker.check(w, &ledger).unwrap_or_else(|v| panic!("{v}"));
        ledger
    }

    fn chance(t: &EcoTables, key: &str) -> u8 {
        t.plants()[usize::from(species(t, key))].spread_chance
    }

    /// Every plant cell of the world, sorted.
    fn plant_cells(w: &World) -> Vec<CellPos> {
        let mut cells: Vec<CellPos> = w
            .iter_chunks_sorted()
            .flat_map(|(pos, c)| {
                c.plants
                    .keys()
                    .map(move |&i| CellPos::from_chunk_local(pos, LocalPos::from_index(i)))
            })
            .collect();
        cells.sort_by_key(|&c| crate::reference::order_key(c));
        cells
    }

    fn born(n: u32) -> SeasonLedger {
        SeasonLedger {
            plants_born: n,
            ..SeasonLedger::default()
        }
    }

    #[test]
    fn spread_only_onto_passing_anchors() {
        let t = tables();
        let mut w = world(&t);
        let grass = id(&t, "grass");
        let p = CellPos::new(10, 31, 10);
        mature(&mut w, &t, p, "grass");

        // The cells around P in the global order: y = 30 is soil, then the
        // first row of y = 31. Each of the first sites fails one condition.
        let at = |x, z| CellPos::new(x, 31, z);
        let (stone, dark, dry, copper, cold, solid) = (
            at(8, 8),
            at(9, 8),
            at(10, 8),
            at(11, 8),
            at(12, 8),
            at(8, 9),
        );
        let (first, second) = (at(9, 9), at(11, 11));
        let mut scratch = SeasonLedger::default();
        for c in [stone, dark, dry, copper, cold, solid, first, second] {
            site(&mut w, &t, c);
        }
        // Grass anchors on soil only.
        w.eco_set(below(stone), id(&t, "stone"), &mut scratch);
        w.eco_put_layer(dark, LayerId::Light, 0);
        w.eco_put_layer(below(dry), LayerId::Moisture, 0);
        w.eco_put_layer(below(copper), sub(&t, "cu"), 200);
        w.eco_put_layer(cold, LayerId::Temperature, -50);
        // Not air (its own cell above, at y = 32, is outside this world's
        // lit sites: dark).
        w.eco_set(solid, id(&t, "topsoil"), &mut scratch);
        // Out of reach (radius 2), though passing.
        let far = at(13, 10);
        site(&mut w, &t, far);
        // A plant one step short of mature, and a mature one whose roll
        // fails, each beside a passing site.
        let young = CellPos::new(20, 31, 20);
        mature(&mut w, &t, young, "grass");
        w.eco_put_layer(young, LayerId::Growth, 254);
        site(&mut w, &t, at(21, 20));
        let unlucky = CellPos::new(20, 31, 4);
        mature(&mut w, &t, unlucky, "grass");
        site(&mut w, &t, at(21, 4));

        let c = chance(&t, "grass");
        let season = 2;
        let roll = |seed, step, cell| spread_roll(seed, season, step, cell);
        let seed = (0..u64::from(u32::MAX))
            .find(|&s| {
                (0..3)
                    .all(|i| roll(s, i, p) < c && roll(s, i, unlucky) >= c && roll(s, i, young) < c)
            })
            .expect("a seed where P and the young plant roll to spread and the other never");
        let before = plant_cells(&w);

        // The first passing site in the global order is taken.
        assert_eq!(spread(&mut w, &t, seed, season, 0), born(1));
        assert_eq!(w.get_cell(first), Some(grass));
        let fresh = PlantState {
            species: species(&t, "grass"),
            ..PlantState::default()
        };
        assert_eq!(w.plant(first), Some(&fresh));
        assert_eq!(w.layer(first, LayerId::Growth), 0);
        for c in [
            stone,
            dark,
            dry,
            copper,
            cold,
            second,
            far,
            at(21, 20),
            at(21, 4),
        ] {
            assert_eq!(w.get_cell(c), Some(MaterialId::AIR), "{c:?} stays air");
        }
        assert_eq!(w.get_cell(solid), Some(id(&t, "topsoil")));
        let mut want = before.clone();
        want.push(first);
        want.sort_by_key(|&c| crate::reference::order_key(c));
        assert_eq!(plant_cells(&w), want, "one new plant");

        // The next roll takes the next passing site; the newborn is no
        // parent yet.
        assert_eq!(spread(&mut w, &t, seed, season, 1), born(1));
        assert_eq!(w.plant(second), Some(&fresh));
        // Nothing passing is left in reach.
        let hash = w.hash();
        assert_eq!(spread(&mut w, &t, seed, season, 2), born(0));
        assert_eq!(w.hash(), hash);
        assert_eq!(w.get_cell(far), Some(MaterialId::AIR));
    }

    #[test]
    fn spread_is_seeded_deterministic() {
        let t = tables();
        let mut w = world(&t);
        // Every air cell of y = 31 is a passing site; mature grass on a
        // grid whose reaches (radius 2) do not overlap.
        for z in 0..32 {
            for x in 0..32 {
                site(&mut w, &t, CellPos::new(x, 31, z));
            }
        }
        let parents: Vec<CellPos> = (0..4)
            .flat_map(|i| (0..4).map(move |j| CellPos::new(4 + 8 * j, 31, 4 + 8 * i)))
            .collect();
        for &p in &parents {
            mature(&mut w, &t, p, "grass");
        }
        let c = chance(&t, "grass");
        let (season, substep) = (3, 2);
        let mut outcomes = std::collections::BTreeSet::new();
        for seed in 0..16 {
            let mut a = w.clone();
            let mut b = w.clone();
            let la = spread(&mut a, &t, seed, season, substep);
            assert_eq!(spread(&mut b, &t, seed, season, substep), la);
            assert_eq!(
                a.hash(),
                b.hash(),
                "seed {seed}: the same seed spreads the same"
            );
            // Exactly the parents whose roll is below spread_chance spread,
            // each onto its first site in the global order.
            let spreading: Vec<CellPos> = parents
                .iter()
                .copied()
                .filter(|&p| spread_roll(seed, season, substep, p) < c)
                .collect();
            assert_eq!(la, born(spreading.len() as u32), "seed {seed}");
            for &p in &parents {
                let first = CellPos::new(p.0.x - 2, 31, p.0.z - 2);
                assert_eq!(
                    a.plant(first).is_some(),
                    spreading.contains(&p),
                    "seed {seed}: {p:?}"
                );
            }
            outcomes.insert(spreading);
        }
        assert!(outcomes.len() > 1, "the seed changes which plants spread");
        // The season and the sub-step are part of the roll too.
        let roll_of = |season, step| -> Vec<u8> {
            parents
                .iter()
                .map(|&p| spread_roll(0, season, step, p))
                .collect()
        };
        assert_ne!(roll_of(season, substep), roll_of(season + 1, substep));
        assert_ne!(roll_of(season, substep), roll_of(season, substep + 1));
    }

    #[test]
    fn no_double_claim_across_chunk_border() {
        let t = tables();
        let season = 1;
        // P1 in the west chunk and P2 in the east chunk both reach the one
        // passing site X, in the east chunk. P1 comes first in the global
        // order (same y-plane, chunk (0, 0) before (1, 0)) and claims it.
        let x = CellPos::new(32, 31, 10);
        let (west, east) = (CellPos::new(30, 31, 10), CellPos::new(34, 31, 10));
        assert_ne!(west.split().0, x.split().0);
        assert_eq!(east.split().0, x.split().0);
        for (first, second) in [("grass", "moss"), ("moss", "grass")] {
            let mut w = wide_world(&t);
            mature(&mut w, &t, west, first);
            mature(&mut w, &t, east, second);
            site(&mut w, &t, x);
            let seed = (0..u64::from(u32::MAX))
                .find(|&s| {
                    spread_roll(s, season, 0, west) < chance(&t, first)
                        && spread_roll(s, season, 0, east) < chance(&t, second)
                })
                .expect("a seed where both roll to spread");

            assert_eq!(
                spread(&mut w, &t, seed, season, 0),
                born(1),
                "{first} first"
            );
            assert_eq!(w.get_cell(x), Some(id(&t, first)));
            assert_eq!(w.plant(x).map(|p| p.species), Some(species(&t, first)));
            assert_eq!(plant_cells(&w), vec![west, x, east], "no second plant");
        }
    }
}
