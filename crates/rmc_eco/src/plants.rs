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
//! **Aging.** At the end of each season every plant's `age_seasons` goes up
//! by one ([`age_plants`]); a plant that reached its lifespan dies in the
//! next season's first growth pass.

use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::layers::{LayerId, PlantState};
use rmc_world::materials::MaterialId;
use rmc_world::world::World;

use crate::runner::{ChunkView, JacobiOut, JacobiPass, Queued};
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
        let species = tables
            .plants()
            .iter()
            .map(|p| Species {
                material: tables.materials().id(&p.material),
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
            })
            .collect();
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
    /// Step 1: whether `sp`'s needs hold at the plant `cell` and its
    /// `anchor`.
    fn needs_met(
        &self,
        view: &ChunkView<'_>,
        sp: &Species,
        cell: CellPos,
        anchor: CellPos,
    ) -> bool {
        let temp = view.layer(cell, LayerId::Temperature);
        view.layer(cell, LayerId::Light) >= sp.light
            && (sp.temp.0..=sp.temp.1).contains(&temp)
            && view.layer(anchor, LayerId::Moisture) >= sp.moisture
            && sp
                .substance_min
                .iter()
                .all(|&(x, min)| view.layer(anchor, LayerId::Substance(x)) >= min)
            && sp
                .substance_max
                .iter()
                .all(|&(x, max)| view.layer(anchor, LayerId::Substance(x)) <= max)
    }

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
            let met = self.needs_met(view, sp, cell, anchor);
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
    use rmc_world::layers::N_SUBSTANCES;
    use rmc_world::ledger::SeasonLedger;

    use crate::activity::Activity;
    use crate::invariants::InvariantChecker;
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
}
