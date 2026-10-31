//! A garden: the game's default world with a meadow of plants by the spawn,
//! where every species of `plants.ron` has what it needs. This is a built
//! level, not a SAD feature, like [`crate::castle`]: [`build`] generates the
//! world and edits it, `build_garden` saves it where `rmc_app --save <dir>`
//! loads it, and [`survey`] says how many plants a world holds and, for the
//! open surface, which of each species' needs fail.
//!
//! A fresh world grows only worldgen's pioneers (moss and grass) on its
//! loam horizon (`docs/VEGETATION.md` V1). The garden lays loam, damp and
//! fed, and plants every species, clearing the wild plants of the meadow.
//!
//! The layout (x, z in cells): a square meadow of loam around [`CENTRE`],
//! cut into four beds by two paths crossing at the spawn, with a lawn
//! around the crossing. The beds hold the species in `plants.ron` row
//! order: north-west moss, north-east grass, south-west fireweed,
//! south-east shrubs, and the lawn the fifth (clover), each with gaps to
//! spread into. Species that grow parts take no bed: trees
//! (`docs/VEGETATION.md` V4, V5) stand near the corners of the meadow, at
//! [`GROVE`] from the centre, far enough apart for their crowns; vines (V6)
//! stand at the foot of a stone trellis wall along the meadow's south edge,
//! every [`VINE_SPACING`] cells, with a gap where the path crosses it.

use std::collections::BTreeMap;

use anyhow::{Context, bail};
use rmc_eco::EcoTables;
use rmc_eco::tables::FormDef;
use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId, RejectReason};
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;
use rmc_world::world::World as CellWorld;

use crate::session::{GameAssets, generate_world};

/// The garden's centre column: the player's spawn (the world's middle
/// column).
pub const CENTRE: (i32, i32) = (64, 64);
/// Half-size of the meadow (Chebyshev radius from the centre).
const MEADOW: i32 = 22;
/// Half-width of the paths crossing at the centre, which stay unplanted.
const PATH: i32 = 1;
/// Half-size of the lawn around the crossing (Chebyshev radius), which
/// holds the fifth species.
const LAWN: i32 = 6;
/// The lawn's species: the row after the four quadrants'.
const LAWN_BED: usize = 4;
/// Offset (along x and along z) from the centre of the trees near the
/// meadow's corners.
const GROVE: i32 = 17;
/// The trellis wall: its z, its half-length along x, and its height above
/// the ground, in cells. The vines stand on its north side.
const TRELLIS_Z: i32 = CENTRE.1 + MEADOW - 1;
const TRELLIS_HALF: i32 = 14;
const TRELLIS_HEIGHT: i32 = 6;
/// Cells between the vines along the trellis.
const VINE_SPACING: i32 = 4;
/// Loam laid at the top of each meadow column, in cells.
const LOAM_DEPTH: i32 = 4;
/// The loam's moisture: its field capacity, the most it holds.
const MOISTURE: i32 = 48;
/// Nutrients in every loam cell: grass takes up one nitrogen a sub-step and
/// shrubs one of each, so the beds keep feeding for many seasons; litter
/// puts some back.
const NUTRIENTS: [(&str, i32); 3] = [("n", 220), ("p", 160), ("k", 160)];
/// The chance in 256 that a bed column starts planted; the gaps are where
/// the beds spread.
const PLANTED: u8 = 160;

/// Plants in a world, and why the surface lacks them.
#[derive(Clone, Debug, Default)]
pub struct Survey {
    /// Surface sites: the air cell over each column's topmost cell.
    pub sites: usize,
    /// Plants per species key.
    pub plants: BTreeMap<String, usize>,
    /// Part cells of multi-cell plants (`docs/VEGETATION.md` V4) per kind.
    pub parts: BTreeMap<String, usize>,
    /// Per species key, per need: surface sites where that need fails.
    pub fails: BTreeMap<String, BTreeMap<String, usize>>,
}

/// Counts the plants of `w` and, per species, the surface sites failing
/// each of its needs.
pub fn survey(w: &CellWorld, t: &EcoTables) -> Survey {
    let mut s = Survey::default();
    for (pos, chunk) in w.iter_chunks_sorted() {
        for (&i, p) in &chunk.plants {
            if let Some(def) = t.plants().get(usize::from(p.species)) {
                *s.plants.entry(def.key.clone()).or_default() += 1;
            }
            let root = CellPos(pos.origin_cell().0 + LocalPos::from_index(i).0.as_ivec3());
            for (_, kind) in rmc_eco::parts::parts_of(w, root) {
                *s.parts.entry(format!("{kind:?}")).or_default() += 1;
            }
        }
    }
    let m = t.materials();
    let air = t.role_material("air");
    let b = w.bounds();
    let (lo, hi) = (b.min.0, b.max.0);
    for z in lo.z..=hi.z {
        for x in lo.x..=hi.x {
            let Some(top) = (lo.y..hi.y).rev().find(|&y| {
                let c = CellPos::new(x, y, z);
                w.get_cell(c) != air && w.plant(c).is_none()
            }) else {
                continue;
            };
            s.sites += 1;
            let (cell, below) = (CellPos::new(x, top + 1, z), CellPos::new(x, top, z));
            let anchor = w.get_cell(below).and_then(|id| m.get(id));
            for p in t.plants() {
                let fails = s.fails.entry(p.key.clone()).or_default();
                let mut fail = |need: &str, bad: bool| {
                    if bad {
                        *fails.entry(need.to_string()).or_default() += 1;
                    }
                };
                fail(
                    "anchor",
                    !anchor.is_some_and(|a| p.anchor_tags.iter().any(|tag| a.has_tag(tag))),
                );
                fail(
                    "light",
                    w.layer(cell, LayerId::Light) < i32::from(p.needs.light),
                );
                let temp = w.layer(cell, LayerId::Temperature);
                fail(
                    "temp",
                    !(i32::from(p.needs.temp.0)..=i32::from(p.needs.temp.1)).contains(&temp),
                );
                fail(
                    "moisture",
                    w.layer(below, LayerId::Moisture) < i32::from(p.needs.moisture),
                );
                let sub = |key: &str| {
                    t.substance_index(key)
                        .map_or(0, |i| w.layer(below, LayerId::Substance(i)))
                };
                for (k, n) in &p.needs.substance_min {
                    fail(&format!("{k} >= {n}"), sub(k) < i32::from(*n));
                }
                for (k, n) in &p.needs.substance_max {
                    fail(&format!("{k} <= {n}"), sub(k) > i32::from(*n));
                }
            }
        }
    }
    s
}

/// What [`build`] made.
#[derive(Clone, Debug)]
pub struct Garden {
    pub world: CellWorld,
    /// Meadow columns laid with loam.
    pub columns: u32,
    /// Plants started, per species key.
    pub planted: BTreeMap<String, u32>,
}

/// Which bed column (x, z) is in: the quadrants around the paths, in
/// `plants.ron` row order (north-west, north-east, south-west, south-east),
/// then the lawn around the crossing. `None` on a path, outside the
/// meadow, or past the last species (a lawn without a fifth species joins
/// its quadrant).
fn bed(x: i32, z: i32, species: usize) -> Option<usize> {
    let (dx, dz) = (x - CENTRE.0, z - CENTRE.1);
    if dx.abs() > MEADOW || dz.abs() > MEADOW || dx.abs() <= PATH || dz.abs() <= PATH {
        return None;
    }
    let quadrant = usize::from(dx > 0) + 2 * usize::from(dz > 0);
    let i = if dx.abs().max(dz.abs()) <= LAWN && LAWN_BED < species {
        LAWN_BED
    } else {
        quadrant
    };
    (i < species).then_some(i)
}

/// A fixed per-column roll in 0..=255.
fn roll(x: i32, z: i32) -> u8 {
    let h = (x as u32)
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add((z as u32).wrapping_mul(0x85EB_CA77));
    ((h ^ (h >> 15)).wrapping_mul(0xC2B2_AE3D) >> 24) as u8
}

/// The default world with the garden laid around [`CENTRE`].
pub fn build(assets: &GameAssets, seed: u64) -> anyhow::Result<Garden> {
    let t = &assets.eco;
    let mut world = generate_world(assets, seed)?;
    // Placing a plant material starts a plant only under the eco rules.
    world.set_eco_rules(t.displacement_rules());
    world.set_tree_rules(t.tree_rules());
    let m = &assets.materials;
    let id = |k: &str| m.id(k).with_context(|| format!("materials.ron has no {k}"));
    let loam = id("loam")?;
    let air = t.role_material("air");
    // Bed plants (0), trees (1) and vines (2).
    let resolve = |form: u8| {
        t.plants()
            .iter()
            .filter(|p| {
                let f = match p.parts.as_ref().map(|d| &d.form) {
                    None => 0,
                    Some(FormDef::Tree { .. } | FormDef::Graph { .. }) => 1,
                    Some(FormDef::Vine { .. }) => 2,
                };
                f == form
            })
            .map(|p| Ok((p.key.clone(), id(&p.material)?)))
            .collect::<anyhow::Result<Vec<(String, MaterialId)>>>()
    };
    let (plants, trees, vines) = (resolve(0)?, resolve(1)?, resolve(2)?);
    let stone = id("stone")?;
    let nutrients: Vec<(LayerId, i32)> = NUTRIENTS
        .iter()
        .map(|&(k, v)| {
            let i = t
                .substance_index(k)
                .with_context(|| format!("substances.ron has no {k}"))?;
            Ok((LayerId::Substance(i), v))
        })
        .collect::<anyhow::Result<_>>()?;
    let mut g = Applier { world, seq: 0 };
    let mut columns = 0;
    let mut planted = BTreeMap::new();
    for z in CENTRE.1 - MEADOW..=CENTRE.1 + MEADOW {
        for x in CENTRE.0 - MEADOW..=CENTRE.0 + MEADOW {
            let top = i32::from(g.world.surface_y(x, z));
            // A wild plant worldgen seeded (over the surface, which skips
            // plants) gives way to the bed.
            let above = CellPos::new(x, top + 1, z);
            if g.world.plant(above).is_some() {
                g.apply(
                    EditOp::Set(air.unwrap_or(MaterialId::AIR)),
                    Shape::Cell(above),
                )?;
            }
            let ground = g
                .world
                .get_cell(CellPos::new(x, top, z))
                .and_then(|c| m.get(c))
                .is_some_and(|d| d.is_solid() && !d.has_tag("plant"));
            if !ground {
                continue;
            }
            let soil = Shape::Box {
                min: CellPos::new(x, top - LOAM_DEPTH + 1, z),
                max: CellPos::new(x, top, z),
            };
            g.apply(EditOp::Set(loam), soil)?;
            g.apply(
                EditOp::SetLayer {
                    layer: LayerId::Moisture,
                    value: MOISTURE,
                },
                soil,
            )?;
            for &(layer, value) in &nutrients {
                g.apply(EditOp::SetLayer { layer, value }, soil)?;
            }
            columns += 1;
            let Some(i) = bed(x, z, plants.len()) else {
                continue;
            };
            let cell = CellPos::new(x, top + 1, z);
            if roll(x, z) >= PLANTED || g.world.get_cell(cell) != air {
                continue;
            }
            let (key, material) = &plants[i];
            g.plant(key, *material, cell, &mut planted)?;
        }
    }
    // The trees, in place of whatever bed plant stood there.
    let corners = [(-1, -1), (1, -1), (-1, 1), (1, 1)];
    for (&(sx, sz), (key, material)) in corners.iter().zip(trees.iter().cycle()) {
        let (x, z) = (CENTRE.0 + sx * GROVE, CENTRE.1 + sz * GROVE);
        let cell = CellPos::new(x, i32::from(g.world.surface_y(x, z)) + 1, z);
        g.clear(t, cell, &mut planted)?;
        g.plant(key, *material, cell, &mut planted)?;
    }
    // The trellis, then the vines at its foot.
    let open = air.unwrap_or(MaterialId::AIR);
    let mut foot = Vec::new();
    for x in CENTRE.0 - TRELLIS_HALF..=CENTRE.0 + TRELLIS_HALF {
        if (x - CENTRE.0).abs() <= PATH {
            continue;
        }
        let top = i32::from(g.world.surface_y(x, TRELLIS_Z));
        for y in top + 1..=top + TRELLIS_HEIGHT {
            g.clear(t, CellPos::new(x, y, TRELLIS_Z), &mut planted)?;
        }
        g.apply(
            EditOp::Set(stone),
            Shape::Box {
                min: CellPos::new(x, top + 1, TRELLIS_Z),
                max: CellPos::new(x, top + TRELLIS_HEIGHT, TRELLIS_Z),
            },
        )?;
        if (x - CENTRE.0).rem_euclid(VINE_SPACING) == 0 {
            foot.push(x);
        }
    }
    for (x, (key, material)) in foot.into_iter().zip(vines.iter().cycle()) {
        let z = TRELLIS_Z - 1;
        let cell = CellPos::new(x, i32::from(g.world.surface_y(x, z)) + 1, z);
        g.clear(t, cell, &mut planted)?;
        if g.world.get_cell(cell) == Some(open) {
            g.plant(key, *material, cell, &mut planted)?;
        }
    }
    planted.retain(|_, n| *n > 0);
    Ok(Garden {
        world: g.world,
        columns,
        planted,
    })
}

/// Applies edits as player 0's commands.
struct Applier {
    world: CellWorld,
    seq: u32,
}

impl Applier {
    /// Clears the plant at `cell`, if any, and uncounts it.
    fn clear(
        &mut self,
        t: &EcoTables,
        cell: CellPos,
        planted: &mut BTreeMap<String, u32>,
    ) -> anyhow::Result<()> {
        let Some(p) = self.world.plant(cell) else {
            return Ok(());
        };
        let key = &t.plants()[usize::from(p.species)].key;
        if let Some(n) = planted.get_mut(key) {
            *n -= 1;
        }
        let air = t.role_material("air").unwrap_or(MaterialId::AIR);
        self.apply(EditOp::Set(air), Shape::Cell(cell))
    }

    /// Places `material` at `cell` and counts the plant it starts.
    fn plant(
        &mut self,
        key: &str,
        material: MaterialId,
        cell: CellPos,
        planted: &mut BTreeMap<String, u32>,
    ) -> anyhow::Result<()> {
        self.apply(EditOp::Set(material), Shape::Cell(cell))?;
        if self.world.plant(cell).is_none() {
            bail!("{key} at {cell:?} did not start a plant");
        }
        *planted.entry(key.to_owned()).or_default() += 1;
        Ok(())
    }

    fn apply(&mut self, op: EditOp, shape: Shape) -> anyhow::Result<()> {
        self.seq += 1;
        let issuer = PlayerId(0);
        let result = self.world.apply(&EditCommand {
            id: CommandId {
                issuer,
                seq: self.seq,
            },
            issuer,
            op,
            shape,
        });
        match result.status {
            EditStatus::Applied | EditStatus::Rejected(RejectReason::OutsideBounds) => Ok(()),
            other => bail!("edit {op:?} {shape:?} failed: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::DEFAULT_WORLD_SIZE;
    use crate::session::{DEFAULT_SEED, assets_dir};

    /// `docs/VEGETATION.md` V1: the default world's loam horizon is damp
    /// and fed, so worldgen seeds its pioneers there.
    #[test]
    fn a_fresh_world_seeds_moss_and_grass() {
        let assets = GameAssets::load(&assets_dir())
            .unwrap()
            .with_world_bounds(DEFAULT_WORLD_SIZE.bounds())
            .unwrap();
        let world = generate_world(&assets, DEFAULT_SEED).unwrap();
        let s = survey(&world, &assets.eco);
        for key in ["moss", "grass"] {
            let n = s.plants.get(key).copied().unwrap_or(0);
            assert!(n > 0, "{key}: {s:?}");
        }
    }

    #[test]
    fn the_garden_plants_every_species_where_its_needs_hold() {
        let assets = GameAssets::load(&assets_dir())
            .unwrap()
            .with_world_bounds(DEFAULT_WORLD_SIZE.bounds())
            .unwrap();
        let g = build(&assets, DEFAULT_SEED).unwrap();
        let t = &assets.eco;
        assert!(g.columns > 0);
        // The meadow holds only what the garden planted; worldgen's wild
        // plants there gave way to the beds.
        let mut meadow: BTreeMap<String, u32> = BTreeMap::new();
        let b = g.world.bounds();
        for z in CENTRE.1 - MEADOW..=CENTRE.1 + MEADOW {
            for x in CENTRE.0 - MEADOW..=CENTRE.0 + MEADOW {
                for y in b.min.0.y..=b.max.0.y {
                    if let Some(p) = g.world.plant(CellPos::new(x, y, z)) {
                        let key = &t.plants()[usize::from(p.species)].key;
                        *meadow.entry(key.clone()).or_default() += 1;
                    }
                }
            }
        }
        assert_eq!(meadow, g.planted);
        for p in t.plants() {
            let n = g.planted.get(&p.key).copied().unwrap_or(0);
            assert!(n > 0, "{} planted", p.key);
        }
        // Each plant's anchor is damp and fed enough for its species.
        let b = g.world.bounds();
        for z in b.min.0.z..=b.max.0.z {
            for x in b.min.0.x..=b.max.0.x {
                for y in b.min.0.y + 1..=b.max.0.y {
                    let cell = CellPos::new(x, y, z);
                    let Some(p) = g.world.plant(cell) else {
                        continue;
                    };
                    let def = &t.plants()[usize::from(p.species)];
                    let below = CellPos::new(x, y - 1, z);
                    assert!(
                        g.world.layer(below, LayerId::Moisture) >= i32::from(def.needs.moisture),
                        "{} at {cell:?} is dry",
                        def.key
                    );
                    for (k, n) in &def.needs.substance_min {
                        let i = t.substance_index(k).unwrap();
                        assert!(
                            g.world.layer(below, LayerId::Substance(i)) >= i32::from(*n),
                            "{} at {cell:?} lacks {k}",
                            def.key
                        );
                    }
                }
            }
        }
    }
}
