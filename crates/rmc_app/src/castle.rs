//! A castle on a river, with a moat, and a mine full of ore nearby. This is
//! a built level, not a SAD feature: [`build`] generates the game's default
//! world and edits it, `build_castle` saves the result where
//! `rmc_app --save <dir>` loads it, and the `castle_preview` e2e scenario
//! photographs it.
//!
//! The layout (x, z in cells; the player spawns at the middle column,
//! (64, 64), which is in the courtyard):
//! - a river along z on the west, its centre swinging around x = 20;
//! - a canal east from the river into a moat ringing the castle island;
//! - the castle at (64, 72): a curtain wall with four corner towers, a
//!   gate and a stone bridge on the north side, and a keep in the middle;
//! - a mine to the north-east: a headframe, a stair down, a hall and
//!   drifts whose walls are full of ore veins, lit by glowcrystal.
//!
//! The building materials are named here, as a level's palette; game
//! behaviour still comes only from the tables.

use anyhow::{Context, bail};
use rmc_world::coords::{CellAabb, CellPos};
use rmc_world::edit::{
    CommandId, EditCommand, EditOp, EditStatus, MAX_EDIT_CELLS, PlayerId, RejectReason,
};
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;
use rmc_world::world::World as CellWorld;

use crate::session::{GameAssets, generate_world};

/// The castle's centre column.
pub const CASTLE: (i32, i32) = (64, 72);
/// Half-sizes (Chebyshev radius from the centre) of the castle's rings.
const ISLAND: i32 = 16;
const MOAT_OUT: i32 = 20;
const BERM: i32 = 23;
const WALL_IN: i32 = 13;
const WALL_OUT: i32 = 14;
const WALL_H: i32 = 7;
const TOWER: i32 = 3;
const TOWER_H: i32 = 11;
const KEEP: i32 = 5;
const KEEP_H: i32 = 17;
/// Water depth of the river, the canal and the moat.
const DEPTH: i32 = 3;
/// How far out the ground round a levelled site is terraced.
const TERRACE: i32 = 14;
const RIVER_HALF: i32 = 4;
const CANAL_Z: (i32, i32) = (70, 73);
/// The mine: stair top, stair run (one cell down per cell), the hall and
/// the drifts.
const MINE_X: i32 = 96;
const MINE_Z: (i32, i32) = (28, 30);
const STAIR: i32 = 18;

/// What [`build`] made, for the preview's camera.
#[derive(Clone, Debug)]
pub struct Castle {
    pub world: CellWorld,
    /// The island's ground level: the courtyard is at `ground + 1`.
    pub ground: i32,
    /// The water level of the river, canal and moat.
    pub water: i32,
    /// The mine's floor level.
    pub mine_floor: i32,
    /// The top step of the mine's stair, and its hall.
    pub mine_top: CellPos,
    pub mine_hall: CellAabb,
    /// Commands applied, cells changed.
    pub commands: u32,
    pub changed: u64,
}

struct Palette {
    wall: MaterialId,
    trim: MaterialId,
    plinth: MaterialId,
    floor: MaterialId,
    fill: MaterialId,
    top: MaterialId,
    water: MaterialId,
    glow: MaterialId,
    brass: MaterialId,
    grass: MaterialId,
    ores: Vec<MaterialId>,
}

impl Palette {
    fn new(assets: &GameAssets) -> anyhow::Result<Self> {
        let id = |k: &str| {
            assets
                .materials
                .id(k)
                .with_context(|| format!("materials.ron has no {k}"))
        };
        Ok(Self {
            wall: id("granite")?,
            trim: id("limestone")?,
            plinth: id("basalt")?,
            floor: id("stone")?,
            fill: id("dirt")?,
            top: id("topsoil")?,
            water: assets.eco.role_material("water").context("no water role")?,
            glow: id("glowcrystal")?,
            brass: id("brass_plate")?,
            grass: id("grass")?,
            ores: [
                "iron_ore",
                "sulfide_ore",
                "coal_seam",
                "iron_ore",
                "glowcrystal",
            ]
            .iter()
            .map(|k| id(k))
            .collect::<anyhow::Result<_>>()?,
        })
    }
}

/// Applies edits to the world as commands of at most [`MAX_EDIT_CELLS`].
struct Builder {
    world: CellWorld,
    seq: u32,
    changed: u64,
}

impl Builder {
    fn op(&mut self, op: EditOp, min: (i32, i32, i32), max: (i32, i32, i32)) -> anyhow::Result<()> {
        let (min, max) = (
            CellPos::new(min.0, min.1, min.2),
            CellPos::new(max.0, max.1, max.2),
        );
        if min.0.x > max.0.x || min.0.y > max.0.y || min.0.z > max.0.z {
            return Ok(());
        }
        let layer = i64::from(max.0.x - min.0.x + 1) * i64::from(max.0.z - min.0.z + 1);
        let rows = (i64::try_from(MAX_EDIT_CELLS)? / layer).max(1) as i32;
        let mut y = min.0.y;
        while y <= max.0.y {
            let top = (y + rows - 1).min(max.0.y);
            self.apply(
                op,
                Shape::Box {
                    min: CellPos::new(min.0.x, y, min.0.z),
                    max: CellPos::new(max.0.x, top, max.0.z),
                },
            )?;
            y = top + 1;
        }
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
            EditStatus::Applied => {
                self.changed += result.deltas.len() as u64;
                Ok(())
            }
            EditStatus::Rejected(RejectReason::OutsideBounds) => Ok(()),
            other => bail!("edit {op:?} {shape:?} failed: {other:?}"),
        }
    }

    fn cell(&mut self, op: EditOp, x: i32, y: i32, z: i32) -> anyhow::Result<()> {
        self.apply(op, Shape::Cell(CellPos::new(x, y, z)))
    }

    /// The column's surface, or the bottom of the world where it has none
    /// (outside the bounds, or all air).
    fn surface(&self, x: i32, z: i32) -> i32 {
        i32::from(self.world.surface_y(x, z)).max(self.world.bounds().min.0.y)
    }

    /// Makes column (x, z) ground up to `level`: fill below, air above,
    /// topsoil on top.
    fn level(
        &mut self,
        p: &Palette,
        x0: i32,
        z0: i32,
        x1: i32,
        z1: i32,
        level: i32,
    ) -> anyhow::Result<()> {
        let top = self.world.bounds().max.0.y;
        let low = (x0..=x1)
            .flat_map(|x| (z0..=z1).map(move |z| (x, z)))
            .map(|(x, z)| self.surface(x, z))
            .min()
            .unwrap_or(level);
        self.op(EditOp::Remove, (x0, level + 1, z0), (x1, top, z1))?;
        self.op(
            EditOp::Set(p.fill),
            (x0, low.min(level), z0),
            (x1, level - 1, z1),
        )?;
        self.op(EditOp::Set(p.top), (x0, level, z0), (x1, level, z1))
    }

    /// Terraces the ground round the square of Chebyshev radius `r` about
    /// (cx, cz), levelled at `level`: out to `r + TERRACE`, a column `d`
    /// cells beyond the square is cut down or built up to within `d` of
    /// `level`, so the player can walk in and out a cell at a time.
    fn terrace(&mut self, p: &Palette, cx: i32, cz: i32, r: i32, level: i32) -> anyhow::Result<()> {
        let top = self.world.bounds().max.0.y;
        for z in cz - r - TERRACE..=cz + r + TERRACE {
            for x in cx - r - TERRACE..=cx + r + TERRACE {
                let d = (x - cx).abs().max((z - cz).abs()) - r;
                if d <= 0 || d > TERRACE || !self.world.bounds().contains(CellPos::new(x, level, z))
                {
                    continue;
                }
                let surface = self.surface(x, z);
                if surface > level + d {
                    self.op(EditOp::Remove, (x, level + d + 1, z), (x, top, z))?;
                    self.cell(EditOp::Set(p.top), x, level + d, z)?;
                } else if surface < level - d {
                    self.op(EditOp::Set(p.fill), (x, surface, z), (x, level - d - 1, z))?;
                    self.cell(EditOp::Set(p.top), x, level - d, z)?;
                }
            }
        }
        Ok(())
    }

    /// A water channel over the box's columns: `DEPTH` deep below `water`,
    /// open to the sky, with its banks raised to `water + 1` where the
    /// ground is lower.
    fn channel(
        &mut self,
        p: &Palette,
        x0: i32,
        z0: i32,
        x1: i32,
        z1: i32,
        water: i32,
    ) -> anyhow::Result<()> {
        let top = self.world.bounds().max.0.y;
        for z in z0 - 2..=z1 + 2 {
            for x in x0 - 2..=x1 + 2 {
                // Columns already holding water are another stretch of
                // the same channel: leave them open.
                let surface = self.surface(x, z);
                let wet = self.world.get_cell(CellPos::new(x, surface, z)) == Some(p.water);
                if surface <= water && !wet {
                    self.op(
                        EditOp::Set(p.fill),
                        (x, self.surface(x, z), z),
                        (x, water, z),
                    )?;
                    self.cell(EditOp::Set(p.top), x, water + 1, z)?;
                }
            }
        }
        self.op(EditOp::Remove, (x0, water - DEPTH + 1, z0), (x1, top, z1))?;
        self.op(
            EditOp::Set(p.floor),
            (x0, water - DEPTH, z0),
            (x1, water - DEPTH, z1),
        )?;
        self.op(
            EditOp::Set(p.water),
            (x0, water - DEPTH + 1, z0),
            (x1, water, z1),
        )?;
        // A liquid cell's fill is its moisture: full, so the water stands
        // to the top of the cell.
        self.op(
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: i32::from(u8::MAX),
            },
            (x0, water - DEPTH + 1, z0),
            (x1, water, z1),
        )
    }
}

fn river_x(z: i32) -> i32 {
    20 + (4.0 * (z as f32 / 14.0).sin()).round() as i32
}

/// The game's default world from `seed`, with the castle, river, moat and
/// mine built into it.
pub fn build(assets: &GameAssets, seed: u64) -> anyhow::Result<Castle> {
    let p = Palette::new(assets)?;
    let world = generate_world(assets, seed)?;
    let mut b = Builder {
        world,
        seq: 0,
        changed: 0,
    };
    let (cx, cz) = CASTLE;

    // Levels: the water a little under the typical ground along the river
    // and around the castle, the island two cells over the water.
    let mut along: Vec<i32> = (4..124).map(|z| b.surface(river_x(z), z)).collect();
    let mut around: Vec<i32> = (cz - BERM..=cz + BERM)
        .flat_map(|z| (cx - BERM..=cx + BERM).map(move |x| (x, z)))
        .map(|(x, z)| b.surface(x, z))
        .collect();
    along.sort_unstable();
    around.sort_unstable();
    let water = along[along.len() / 4].min(around[around.len() / 2] - 2) - 1;
    let ground = (around[around.len() / 2]).max(water + 2);
    tracing::info!(water, ground, "castle levels");

    // The castle's ground, out to the berm round the moat.
    b.level(&p, cx - BERM, cz - BERM, cx + BERM, cz + BERM, ground)?;
    b.terrace(&p, cx, cz, BERM, ground)?;

    // The river, the canal and the moat.
    for z in 3..=124 {
        let x = river_x(z);
        b.channel(&p, x - RIVER_HALF, z, x + RIVER_HALF, z, water)?;
    }
    let canal_x0 = river_x(CANAL_Z.0).max(river_x(CANAL_Z.1)) + RIVER_HALF;
    b.channel(&p, canal_x0, CANAL_Z.0, cx - ISLAND - 1, CANAL_Z.1, water)?;
    for (x0, z0, x1, z1) in ring(cx, cz, ISLAND + 1, MOAT_OUT) {
        b.channel(&p, x0, z0, x1, z1, water)?;
    }
    // Re-level the island, which the moat's banks may have touched.
    b.level(
        &p,
        cx - ISLAND,
        cz - ISLAND,
        cx + ISLAND,
        cz + ISLAND,
        ground,
    )?;

    // Courtyard floor: flagstones round the keep, grass beyond.
    b.op(
        EditOp::Set(p.floor),
        (cx - KEEP - 2, ground, cz - KEEP - 2),
        (cx + KEEP + 2, ground, cz + KEEP + 2),
    )?;

    // Curtain wall, with a limestone course and a basalt plinth.
    let g = ground;
    for (x0, z0, x1, z1) in ring(cx, cz, WALL_IN, WALL_OUT) {
        b.op(EditOp::Set(p.plinth), (x0, g - 2, z0), (x1, g + 1, z1))?;
        b.op(EditOp::Set(p.wall), (x0, g + 2, z0), (x1, g + WALL_H, z1))?;
        b.op(
            EditOp::Set(p.trim),
            (x0, g + WALL_H - 1, z0),
            (x1, g + WALL_H - 1, z1),
        )?;
    }
    crenellate(&mut b, &p, cx, cz, WALL_OUT, g + WALL_H + 1)?;

    // Corner towers: hollow, with a door to the courtyard and a lamp.
    for (sx, sz) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
        let (tx, tz) = (cx + sx * WALL_IN, cz + sz * WALL_IN);
        b.op(
            EditOp::Set(p.plinth),
            (tx - TOWER, g - 2, tz - TOWER),
            (tx + TOWER, g + 1, tz + TOWER),
        )?;
        b.op(
            EditOp::Set(p.wall),
            (tx - TOWER, g + 2, tz - TOWER),
            (tx + TOWER, g + TOWER_H, tz + TOWER),
        )?;
        b.op(
            EditOp::Set(p.trim),
            (tx - TOWER, g + TOWER_H, tz - TOWER),
            (tx + TOWER, g + TOWER_H, tz + TOWER),
        )?;
        b.op(
            EditOp::Remove,
            (tx - TOWER + 1, g + 1, tz - TOWER + 1),
            (tx + TOWER - 1, g + TOWER_H - 1, tz + TOWER - 1),
        )?;
        b.op(
            EditOp::Set(p.floor),
            (tx - TOWER + 1, g, tz - TOWER + 1),
            (tx + TOWER - 1, g, tz + TOWER - 1),
        )?;
        // The door faces the castle's centre along x.
        let door_x = tx - sx * TOWER;
        b.op(
            EditOp::Remove,
            (door_x, g + 1, tz - sz),
            (door_x, g + 3, tz - sz),
        )?;
        b.cell(
            EditOp::Set(p.glow),
            tx + sx * (TOWER - 1),
            g + 1,
            tz + sz * (TOWER - 1),
        )?;
        b.cell(EditOp::Set(p.glow), tx, g + TOWER_H - 1, tz)?;
        crenellate(&mut b, &p, tx, tz, TOWER, g + TOWER_H + 1)?;
    }

    // The gate in the north wall, and the bridge over the moat.
    b.op(
        EditOp::Remove,
        (cx - 1, g + 1, cz - WALL_OUT),
        (cx + 1, g + 4, cz - WALL_IN),
    )?;
    b.op(
        EditOp::Set(p.floor),
        (cx - 1, g, cz - WALL_OUT),
        (cx + 1, g, cz - WALL_IN),
    )?;
    b.op(
        EditOp::Set(p.trim),
        (cx - 2, g + 5, cz - WALL_OUT),
        (cx + 2, g + 5, cz - WALL_IN),
    )?;
    b.op(
        EditOp::Set(p.plinth),
        (cx - 1, g, cz - MOAT_OUT - 1),
        (cx + 1, g, cz - ISLAND),
    )?;
    for z in [cz - MOAT_OUT - 1, cz - ISLAND] {
        for x in [cx - 2, cx + 2] {
            b.op(EditOp::Set(p.wall), (x, g, z), (x, g + 1, z))?;
        }
    }

    // The keep: hollow, two upper floors, windows, a door, lamps.
    b.op(
        EditOp::Set(p.plinth),
        (cx - KEEP, g - 2, cz - KEEP),
        (cx + KEEP, g + 1, cz + KEEP),
    )?;
    b.op(
        EditOp::Set(p.wall),
        (cx - KEEP, g + 2, cz - KEEP),
        (cx + KEEP, g + KEEP_H, cz + KEEP),
    )?;
    b.op(
        EditOp::Remove,
        (cx - KEEP + 1, g + 1, cz - KEEP + 1),
        (cx + KEEP - 1, g + KEEP_H - 1, cz + KEEP - 1),
    )?;
    for y in [g, g + 6, g + 11] {
        b.op(
            EditOp::Set(p.floor),
            (cx - KEEP + 1, y, cz - KEEP + 1),
            (cx + KEEP - 1, y, cz + KEEP - 1),
        )?;
        // A stair hole in each upper floor, with steps up to it.
        if y > g {
            b.op(
                EditOp::Remove,
                (cx + KEEP - 3, y, cz - 1),
                (cx + KEEP - 1, y, cz + 1),
            )?;
        }
        for (dx, dz) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
            b.cell(
                EditOp::Set(p.glow),
                cx + dx * (KEEP - 1),
                y + 1,
                cz + dz * (KEEP - 1),
            )?;
        }
    }
    // Steps against the east wall from each floor to the next.
    for (base, to) in [(g, g + 6), (g + 6, g + 11)] {
        for k in 1..to - base {
            let z = cz + 1 + (to - base - 1) - k - 2;
            b.op(
                EditOp::Set(p.floor),
                (cx + KEEP - 3, base + 1, z),
                (cx + KEEP - 1, base + k, z),
            )?;
        }
    }
    for y in [g + 3, g + 8, g + 13] {
        for k in [-3, 0, 3] {
            b.op(
                EditOp::Remove,
                (cx + k, y, cz - KEEP),
                (cx + k, y + 1, cz - KEEP),
            )?;
            b.op(
                EditOp::Remove,
                (cx + k, y, cz + KEEP),
                (cx + k, y + 1, cz + KEEP),
            )?;
            b.op(
                EditOp::Remove,
                (cx - KEEP, y, cz + k),
                (cx - KEEP, y + 1, cz + k),
            )?;
            b.op(
                EditOp::Remove,
                (cx + KEEP, y, cz + k),
                (cx + KEEP, y + 1, cz + k),
            )?;
        }
    }
    b.op(
        EditOp::Remove,
        (cx - 1, g + 1, cz - KEEP),
        (cx, g + 3, cz - KEEP),
    )?;
    b.op(
        EditOp::Set(p.trim),
        (cx - KEEP, g + KEEP_H, cz - KEEP),
        (cx + KEEP, g + KEEP_H, cz + KEEP),
    )?;
    crenellate(&mut b, &p, cx, cz, KEEP, g + KEEP_H + 1)?;

    // Grass in the courtyard, on the topsoil left between the buildings.
    for z in cz - WALL_IN + 1..cz + WALL_IN {
        for x in cx - WALL_IN + 1..cx + WALL_IN {
            if (x * 7 + z * 3) % 5 == 0 && b.world.get_cell(CellPos::new(x, g, z)) == Some(p.top) {
                b.cell(EditOp::Set(p.grass), x, g + 1, z)?;
            }
        }
    }

    // The mine.
    let (mine_floor, mine_top, mine_hall) = mine(&mut b, &p)?;

    Ok(Castle {
        commands: b.seq,
        changed: b.changed,
        world: b.world,
        ground,
        water,
        mine_floor,
        mine_top,
        mine_hall,
    })
}

/// The four boxes of the square ring of Chebyshev radius `r0..=r1` round
/// (cx, cz), as (x0, z0, x1, z1).
fn ring(cx: i32, cz: i32, r0: i32, r1: i32) -> [(i32, i32, i32, i32); 4] {
    [
        (cx - r1, cz - r1, cx + r1, cz - r0),
        (cx - r1, cz + r0, cx + r1, cz + r1),
        (cx - r1, cz - r0 + 1, cx - r0, cz + r0 - 1),
        (cx + r0, cz - r0 + 1, cx + r1, cz + r0 - 1),
    ]
}

/// Merlons on every other cell of the square ring of radius `r` at `y`.
fn crenellate(
    b: &mut Builder,
    p: &Palette,
    cx: i32,
    cz: i32,
    r: i32,
    y: i32,
) -> anyhow::Result<()> {
    for k in -r..=r {
        if k.rem_euclid(2) != r.rem_euclid(2) {
            continue;
        }
        for (x, z) in [
            (cx + k, cz - r),
            (cx + k, cz + r),
            (cx - r, cz + k),
            (cx + r, cz + k),
        ] {
            b.cell(EditOp::Set(p.wall), x, y, z)?;
        }
    }
    Ok(())
}

/// The mine: a yard and a brass headframe at the surface, a stair down
/// along +x, a hall at the bottom, and drifts whose walls are veined with
/// ore. Returns the floor level, the stair's top step and the hall.
fn mine(b: &mut Builder, p: &Palette) -> anyhow::Result<(i32, CellPos, CellAabb)> {
    let (z0, z1) = MINE_Z;
    let x0 = MINE_X;
    let yard = b.surface(x0, (z0 + z1) / 2);
    b.level(p, x0 - 6, z0 - 4, x0 + 3, z1 + 4, yard)?;
    b.terrace(p, x0 - 1, (z0 + z1) / 2, 4, yard)?;
    let floor_min = b.world.bounds().min.0.y + 8;
    let run = STAIR.min(yard - floor_min);
    let floor = yard - run;
    // Ore first, so the tunnels cut through it and show it in their walls.
    let drifts = [
        // The main drift along x, south of the stair.
        (x0 - 8, z1 + 4, x0 + run + 4, z1 + 6),
        // Cross drifts along z.
        (x0 - 8, z0 - 12, x0 - 6, z1 + 16),
        (x0 + 6, z1 + 4, x0 + 8, z1 + 16),
        (x0 + run - 2, z0 - 12, x0 + run, z1 + 6),
    ];
    let mut k = 0;
    for &(dx0, dz0, dx1, dz1) in &drifts {
        let along_x = dx1 - dx0 > dz1 - dz0;
        let n = if along_x { dx1 - dx0 } else { dz1 - dz0 };
        for i in (2..n).step_by(4) {
            for side in [-1, 1] {
                let (x, z) = if along_x {
                    let z = if side < 0 { dz0 - 2 } else { dz1 + 2 };
                    (dx0 + i, z)
                } else {
                    let x = if side < 0 { dx0 - 2 } else { dx1 + 2 };
                    (x, dz0 + i)
                };
                let ore = p.ores[k % p.ores.len()];
                k += 1;
                let y = floor + 1 + (k as i32 % 3);
                b.apply(
                    EditOp::Paint(ore),
                    Shape::Sphere {
                        center: CellPos::new(x, y, z),
                        radius: 2,
                    },
                )?;
            }
        }
    }
    // The hall at the stair's foot, with a glowcrystal geode in its roof.
    let hall = CellAabb::new(
        CellPos::new(x0 + run - 2, floor + 1, z0 - 4),
        CellPos::new(x0 + run + 5, floor + 5, z1 + 6),
    );
    for (cx, cz) in [
        (hall.min.0.x + 2, hall.min.0.z + 3),
        (hall.max.0.x - 2, hall.max.0.z - 3),
    ] {
        b.apply(
            EditOp::Paint(p.glow),
            Shape::Sphere {
                center: CellPos::new(cx, floor + 7, cz),
                radius: 3,
            },
        )?;
    }
    b.apply(
        EditOp::Paint(p.ores[1]),
        Shape::Sphere {
            center: CellPos::new(hall.max.0.x + 2, floor + 2, (z0 + z1) / 2),
            radius: 3,
        },
    )?;
    b.op(
        EditOp::Remove,
        (hall.min.0.x, hall.min.0.y, hall.min.0.z),
        (hall.max.0.x, hall.max.0.y, hall.max.0.z),
    )?;
    // The drifts, three high, with a glowcrystal lamp in the roof every
    // six cells.
    for &(dx0, dz0, dx1, dz1) in &drifts {
        b.op(EditOp::Remove, (dx0, floor + 1, dz0), (dx1, floor + 3, dz1))?;
        for x in (dx0..=dx1).step_by(6) {
            for z in (dz0..=dz1).step_by(6) {
                b.cell(
                    EditOp::Set(p.glow),
                    x + (dx1 - dx0).min(1),
                    floor + 4,
                    z + (dz1 - dz0).min(1),
                )?;
            }
        }
    }
    // The stair: one cell down per cell along +x, four cells of headroom.
    for k in 0..=run {
        let x = x0 + k;
        let step = yard - k;
        b.op(EditOp::Remove, (x, step, z0), (x, step + 4, z1))?;
        b.op(EditOp::Set(p.floor), (x, step - 1, z0), (x, step - 1, z1))?;
        if k >= 6 && k % 4 == 2 {
            b.cell(EditOp::Set(p.glow), x, step + 4, z0 - 1)?;
        }
    }
    // A short tunnel from the stair's foot into the hall.
    b.op(
        EditOp::Remove,
        (x0 + run - 2, floor + 1, z0),
        (x0 + run + 2, floor + 4, z1),
    )?;
    // The headframe over the stair's mouth.
    for z in [z0 - 1, z1 + 1] {
        for x in [x0 - 1, x0 + 3] {
            b.op(EditOp::Set(p.brass), (x, yard + 1, z), (x, yard + 5, z))?;
        }
        b.op(
            EditOp::Set(p.brass),
            (x0 - 1, yard + 6, z),
            (x0 + 3, yard + 6, z),
        )?;
    }
    b.op(
        EditOp::Set(p.brass),
        (x0 + 1, yard + 6, z0 - 1),
        (x0 + 1, yard + 6, z1 + 1),
    )?;
    // A path of flagstones from the bridge to the mine.
    let (cx, cz) = CASTLE;
    let start = (cx, cz - MOAT_OUT - 2);
    let end = (x0 - 6, (z0 + z1) / 2);
    let steps = (end.0 - start.0).abs().max((end.1 - start.1).abs());
    for i in 0..=steps {
        let x = start.0 + (end.0 - start.0) * i / steps;
        let z = start.1 + (end.1 - start.1) * i / steps;
        let y = b.surface(x, z);
        if b.world.get_cell(CellPos::new(x, y, z)) == Some(p.water) {
            continue;
        }
        b.op(EditOp::Set(p.floor), (x - 1, y, z), (x + 1, y, z))?;
    }
    Ok((floor, CellPos::new(x0, yard, (z0 + z1) / 2), hall))
}
