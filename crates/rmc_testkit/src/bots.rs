//! Scripted players (SAD §12.3), composed from the [`TestApp`] API.
//!
//! Bots only inject keys and mouse motion and read what the client shows:
//! the player, the pickaxe's target and the replica. They never edit a
//! world directly. After every step a bot takes it checks that the
//! player's box is clear of blocking cells.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use bevy::prelude::*;
use rmc_client::controller::SKIN_M;
use rmc_client::{
    BRUSH_BUTTON, BRUSH_REACH_M, BrushOp, BrushShape, ControllerConfig, PLAYER_HEIGHT_M,
    PLAYER_WIDTH_M, PickTarget, Pickaxe, Replica, SWING_BUTTON, SwingState, TOGGLE_EDITOR_KEY,
    brush_target,
};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::materials::MaterialId;
use rmc_world::raycast;

use crate::app::{FRAME_HZ, TestApp};

/// Frames a swing may take beyond the profile's interval: the press, the
/// host's tick and the replica's update.
const SWING_SLACK_FRAMES: u32 = 4;
/// Steps without progress after which a walking bot is stuck.
const STUCK_FRAMES: u32 = 10;
/// Movement below this per step, in meters, is no progress.
const PROGRESS_EPS_M: f32 = 1e-4;
/// Gap, in meters, a [`TunnelBot`] leaves between the front of the
/// player's box and the face of the slice it digs next.
const STANDOFF_M: f32 = 0.5;
/// Gap, in meters, at which a [`TunnelBot`] stops at the end of its tunnel.
const END_GAP_M: f32 = 0.05;
/// How many cells ahead a [`TunnelBot`] looks for the wall to dig into.
const SCAN_CELLS: i32 = 32;
/// Cells a [`BuilderBot`] leaves between the front of the player's box
/// and the near edge of its platform.
const BUILD_GAP_CELLS: i32 = 1;
/// Frames a brush click may take to show in the replica: the click, the
/// host's tick and the replica's update.
const PLACE_SLACK_FRAMES: u32 = 4;
/// The editor keys a [`BuilderBot`] presses: brush op Set, shape Cell
/// (`docs/CONTROLS.md`).
const SET_KEY: KeyCode = KeyCode::Digit1;
const CELL_SHAPE_KEY: KeyCode = KeyCode::Digit4;

/// Why a bot gave up.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BotError {
    #[error("{cell:?} is not a diggable solid cell in the replica")]
    NotDiggable { cell: CellPos },
    #[error("no face of {cell:?} is in sight within reach of the eye at {eye}")]
    OutOfSight { cell: CellPos, eye: Vec3 },
    #[error("aimed at {cell:?}, but the pickaxe targets {target:?}")]
    Misaimed {
        cell: CellPos,
        target: Option<CellPos>,
    },
    #[error("{cell:?} did not break within {frames} frames")]
    Unbroken { cell: CellPos, frames: u32 },
    #[error("stuck with the feet at {feet}")]
    Stuck { feet: Vec3 },
    #[error("the player's box overlaps blocking cells {cells:?}")]
    Penetrating { cells: Vec<CellPos> },
    #[error("no solid cell within {cells} cells ahead to tunnel into")]
    NoWall { cells: i32 },
    #[error("{cell:?} is not an air cell on a solid one in the replica")]
    NoRoom { cell: CellPos },
    #[error("the editor keys left op {op:?}, shape {shape:?}, open {open}")]
    EditorKeys {
        op: BrushOp,
        shape: BrushShape,
        open: bool,
    },
    #[error("the brush has no material")]
    NoBrushMaterial,
    #[error("aimed to place {cell:?}, but the brush targets {target:?}")]
    BrushMisaimed {
        cell: CellPos,
        target: Option<CellPos>,
    },
    #[error("{cell:?} was not placed within {frames} frames")]
    Unplaced { cell: CellPos, frames: u32 },
}

/// One step, then the player must be clear of blocking cells.
fn step(t: &mut TestApp) -> Result<(), BotError> {
    t.step();
    let cells = t.player_penetration();
    if cells.is_empty() {
        Ok(())
    } else {
        Err(BotError::Penetrating { cells })
    }
}

fn swings(t: &TestApp) -> u64 {
    t.app().world().resource::<SwingState>().swings()
}

fn replica(t: &TestApp) -> &rmc_world::world::World {
    t.app().world().resource::<Replica>().world()
}

/// `a` wrapped into `[-π, π)`.
fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}

/// The view (yaw, pitch) that looks along `dir` (SAD §11.4: yaw 0 looks
/// along -Z and positive turns left; positive pitch looks up).
fn view_towards(dir: Vec3) -> (f32, f32) {
    ((-dir.x).atan2(-dir.z), dir.y.atan2(dir.xz().length()))
}

/// Turns the view to (`yaw`, `pitch`) with mouse motion, in one step.
fn aim(t: &mut TestApp, yaw: f32, pitch: f32) -> Result<(), BotError> {
    let p = t.player();
    let sensitivity = t
        .app()
        .world()
        .resource::<ControllerConfig>()
        .sensitivity_rad;
    t.look(
        wrap(p.yaw - yaw) / sensitivity,
        (p.pitch - pitch) / sensitivity,
    );
    step(t)
}

/// Turns the view, in one step, to look from the eye at `point`.
pub fn look_at(t: &mut TestApp, point: Vec3) -> Result<(), BotError> {
    let (yaw, pitch) = view_towards(point - t.player().eye);
    aim(t, yaw, pitch)
}

/// Presses and releases `key`, one step each.
fn tap(t: &mut TestApp, key: KeyCode) -> Result<(), BotError> {
    t.press(key);
    step(t)?;
    t.release(key);
    step(t)
}

/// Looks at a cell and swings the pickaxe until the replica shows it
/// broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigBot {
    pub cell: CellPos,
}

/// What [`DigBot::run`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DigReport {
    pub swings: u64,
    pub frames: u32,
}

impl DigBot {
    pub fn new(cell: CellPos) -> Self {
        Self { cell }
    }

    /// Aims at the middle of the nearest face of the cell the eye sees
    /// first along the ray (a face toward the eye with an open neighbor),
    /// checks that the pickaxe targets the cell, then holds the swing
    /// button until the replica cell is Air. The button is released for
    /// the next step.
    pub fn run(&self, t: &mut TestApp) -> Result<DigReport, BotError> {
        let (frame0, swings0) = (t.frame(), swings(t));
        let cell = self.cell;
        let hardness = replica(t)
            .get_cell(cell)
            .filter(|_| replica(t).is_solid(cell))
            .and_then(|id| replica(t).materials().get(id))
            .filter(|d| d.diggable)
            .map(|d| d.hardness)
            .ok_or(BotError::NotDiggable { cell })?;
        let (yaw, pitch) = self.sight(t)?;
        aim(t, yaw, pitch)?;
        let target = t
            .app()
            .world()
            .resource::<PickTarget>()
            .0
            .map(|hit| hit.cell);
        if target != Some(cell) {
            return Err(BotError::Misaimed { cell, target });
        }

        let profile = t.app().world().resource::<Pickaxe>().0;
        let interval = (f64::from(profile.interval_s) * FRAME_HZ).ceil() as u32;
        let limit = u32::from(hardness.div_ceil(profile.damage)) * interval + SWING_SLACK_FRAMES;
        t.mouse_down(SWING_BUTTON);
        let mut swing = || {
            for _ in 0..limit {
                step(t)?;
                if replica(t).get_cell(cell) == Some(MaterialId::AIR) {
                    return Ok(());
                }
            }
            Err(BotError::Unbroken {
                cell,
                frames: limit,
            })
        };
        let result = swing();
        t.mouse_up(SWING_BUTTON);
        result?;
        Ok(DigReport {
            swings: swings(t) - swings0,
            frames: t.frame() - frame0,
        })
    }

    /// The view that looks at the middle of the nearest face of the cell
    /// a ray from the eye reaches first, within the pickaxe's reach.
    fn sight(&self, t: &TestApp) -> Result<(f32, f32), BotError> {
        let world = replica(t);
        let reach = t.app().world().resource::<Pickaxe>().0.reach_m;
        let eye = t.player().eye;
        let cell = self.cell;
        let normals = [
            IVec3::X,
            IVec3::NEG_X,
            IVec3::Y,
            IVec3::NEG_Y,
            IVec3::Z,
            IVec3::NEG_Z,
        ];
        normals
            .into_iter()
            .filter(|&n| !world.is_solid(CellPos(cell.0 + n)))
            .map(|n| cell.center_m() + n.as_vec3() * (CELL_SIZE_M / 2.0))
            .filter(|&face| (eye - face).dot(face - cell.center_m()) > 0.0)
            .map(|face| face - eye)
            .filter(|&dir| raycast(world, eye, dir, reach).is_some_and(|hit| hit.cell == cell))
            .min_by(|a, b| a.length().total_cmp(&b.length()))
            .map(view_towards)
            .ok_or(BotError::OutOfSight { cell, eye })
    }
}

/// What [`TunnelBot::run`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TunnelReport {
    /// The cells broken, in the order they broke.
    pub cells: Vec<CellPos>,
    /// The lowest cell coordinate of the tunnel's columns on the axis
    /// across the heading.
    pub first_column: i32,
    pub swings: u64,
    pub frames: u32,
    /// The feet when the bot started.
    pub start: Vec3,
}

/// Digs a tunnel the player fits through, [`TunnelBot::length`] cells
/// forward, walking in as it goes.
///
/// The heading is the player's yaw snapped to the nearest world axis. The
/// tunnel is [`TunnelBot::width_cells`] wide, centered as near the
/// player's feet as the grid allows, and [`TunnelBot::height_cells`] high
/// from the cell layer the feet stand in. It starts at the first cell
/// layer ahead with a solid cell in that cross-section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TunnelBot {
    pub length: u32,
}

/// Where a tunnel lies: `start + sign·k` along axis `along` is slice `k`.
#[derive(Clone, Copy, Debug)]
struct Tunnel {
    along: usize,
    sign: i32,
    across: usize,
    /// Sign of the player's right on the `across` axis.
    right: i32,
    first_column: i32,
    floor_row: i32,
    start: i32,
}

impl Tunnel {
    fn cell(&self, k: i32, column: i32, row: i32) -> CellPos {
        let mut c = IVec3::ZERO;
        c[self.along] = self.start + self.sign * k;
        c[self.across] = self.first_column + column;
        c.y = self.floor_row + row;
        CellPos(c)
    }

    fn slice(&self, k: i32) -> impl Iterator<Item = CellPos> + '_ {
        (0..TunnelBot::height_cells())
            .rev()
            .flat_map(move |row| (0..TunnelBot::width_cells()).map(move |c| self.cell(k, c, row)))
    }

    /// How far along the heading slice `k`'s near face lies, in meters.
    fn face_depth(&self, k: i32) -> f32 {
        let layer = self.start + self.sign * k;
        let near = if self.sign > 0 { layer } else { layer + 1 };
        near as f32 * CELL_SIZE_M * self.sign as f32
    }

    /// How far along the heading the front of the box at `feet` lies.
    fn front_depth(&self, feet: Vec3) -> f32 {
        feet[self.along] * self.sign as f32 + PLAYER_WIDTH_M / 2.0
    }

    /// The middle of the tunnel on the `across` axis, in meters.
    fn middle(&self) -> f32 {
        (self.first_column as f32 + TunnelBot::width_cells() as f32 / 2.0) * CELL_SIZE_M
    }
}

impl TunnelBot {
    pub fn new(length: u32) -> Self {
        Self { length }
    }

    /// Cells across: the fewest that hold the player's width.
    pub fn width_cells() -> i32 {
        ((PLAYER_WIDTH_M + 2.0 * SKIN_M) / CELL_SIZE_M).ceil() as i32
    }

    /// Cells up: the fewest that hold the player's height.
    pub fn height_cells() -> i32 {
        ((PLAYER_HEIGHT_M + SKIN_M) / CELL_SIZE_M).ceil() as i32
    }

    /// Faces the heading, steps sideways to the tunnel's middle, then for
    /// each slice walks up to it and digs its cells top row first. At the
    /// end it walks to the tunnel's far end and stands still.
    pub fn run(&self, t: &mut TestApp) -> Result<TunnelReport, BotError> {
        let (frame0, swings0) = (t.frame(), swings(t));
        let start = t.player().pos;
        let quarter = (t.player().yaw / FRAC_PI_2).round();
        let heading = quarter * FRAC_PI_2;
        aim(t, heading, 0.0)?;
        let tunnel = self.lay_out(t, quarter as i32)?;

        self.center(t, &tunnel)?;
        let mut cells = Vec::new();
        for k in 0..self.length as i32 {
            walk_to(t, &tunnel, tunnel.face_depth(k) - STANDOFF_M)?;
            for cell in tunnel.slice(k).collect::<Vec<_>>() {
                if replica(t).is_solid(cell) {
                    DigBot::new(cell).run(t)?;
                    cells.push(cell);
                }
            }
            aim(t, heading, 0.0)?;
        }
        walk_to(
            t,
            &tunnel,
            tunnel.face_depth(self.length as i32) - END_GAP_M,
        )?;
        step(t)?;

        Ok(TunnelReport {
            cells,
            first_column: tunnel.first_column,
            swings: swings(t) - swings0,
            frames: t.frame() - frame0,
            start,
        })
    }

    /// The tunnel for the heading `quarter` quarter turns from -Z.
    fn lay_out(&self, t: &TestApp, quarter: i32) -> Result<Tunnel, BotError> {
        // Forward and right for yaw = quarter · π/2.
        let (forward, right) = [
            (IVec3::NEG_Z, IVec3::X),
            (IVec3::NEG_X, IVec3::NEG_Z),
            (IVec3::Z, IVec3::NEG_X),
            (IVec3::X, IVec3::Z),
        ][quarter.rem_euclid(4) as usize];
        let along = if forward.x != 0 { 0 } else { 2 };
        let across = 2 - along;
        let feet = t.player().pos;
        let width_m = Self::width_cells() as f32 * CELL_SIZE_M;
        let mut tunnel = Tunnel {
            along,
            sign: forward[along],
            across,
            right: right[across],
            first_column: ((feet[across] - width_m / 2.0) / CELL_SIZE_M).round() as i32,
            // Grounded feet rest SKIN_M above the floor; allow as much below.
            floor_row: ((feet.y + SKIN_M) / CELL_SIZE_M).floor() as i32,
            start: 0,
        };
        let front = feet[along] + tunnel.sign as f32 * PLAYER_WIDTH_M / 2.0;
        let front_layer = (front / CELL_SIZE_M).floor() as i32;
        let world = replica(t);
        tunnel.start = (0..SCAN_CELLS)
            .map(|k| front_layer + tunnel.sign * k)
            .find(|&layer| {
                let probe = Tunnel {
                    start: layer,
                    ..tunnel
                };
                probe.slice(0).any(|c| world.is_solid(c))
            })
            .ok_or(BotError::NoWall { cells: SCAN_CELLS })?;
        Ok(tunnel)
    }

    /// Steps sideways until the box is clear of the tunnel's side walls by
    /// at least half the spare width on each side.
    fn center(&self, t: &mut TestApp, tunnel: &Tunnel) -> Result<(), BotError> {
        let spare = Self::width_cells() as f32 * CELL_SIZE_M - PLAYER_WIDTH_M;
        let tolerance = spare / 4.0;
        let offset = |t: &TestApp| tunnel.middle() - t.player().pos[tunnel.across];
        if offset(t).abs() <= tolerance {
            return Ok(());
        }
        let key = if offset(t) * tunnel.right as f32 > 0.0 {
            KeyCode::KeyD
        } else {
            KeyCode::KeyA
        };
        t.press(key);
        let result = walk_until(t, |t| offset(t).abs() <= tolerance);
        t.release(key);
        result
    }
}

/// What [`BuilderBot::run`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildReport {
    /// The platform's cells, in the order they were placed.
    pub cells: Vec<CellPos>,
    /// The brush material the platform is made of.
    pub material: MaterialId,
    pub frames: u32,
    /// The feet when the bot started.
    pub start: Vec3,
}

/// Places a square platform, [`BuilderBot::side`] cells on a side and one
/// cell high, with the editor brush, then walks onto it (SAD §12.3).
///
/// The heading is the player's yaw snapped to the nearest world axis. The
/// platform lies on the cell layer the feet stand in, centered across the
/// heading as near the feet as the grid allows, and starts
/// [`BUILD_GAP_CELLS`] cells beyond the cell layer holding the front of the
/// player's box. Every platform cell must be air with a solid cell under
/// it.
///
/// The bot drives the editor with keys only: F1 opens it, Set and the Cell
/// shape are picked, and each cell is one click at the middle of the top
/// face of the cell under it, so Set fills on the face-normal side. The
/// material is the brush's as the editor holds it. F1 closes the editor
/// before the walk, and the bot stops with the feet over the platform's
/// middle on the heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuilderBot {
    pub side: i32,
}

/// Where a platform lies: depth `k` from the near edge along axis
/// `along`, column `c` across it.
#[derive(Clone, Copy, Debug)]
struct Platform {
    along: usize,
    sign: i32,
    across: usize,
    first_column: i32,
    row: i32,
    near_layer: i32,
    side: i32,
}

impl Platform {
    fn cell(&self, k: i32, column: i32) -> CellPos {
        let mut c = IVec3::ZERO;
        c[self.along] = self.near_layer + self.sign * k;
        c[self.across] = self.first_column + column;
        c.y = self.row;
        CellPos(c)
    }

    /// Placing order: far rows first, and in a row the outer columns
    /// first, so no placed cell stands between the eye and a later target.
    fn cells(&self) -> Vec<CellPos> {
        let mut order: Vec<(i32, i32)> = (0..self.side)
            .flat_map(|k| (0..self.side).map(move |c| (k, c)))
            .collect();
        let off_middle = |c: i32| (2 * c - (self.side - 1)).abs();
        order.sort_by_key(|&(k, c)| (-k, -off_middle(c), c));
        order.into_iter().map(|(k, c)| self.cell(k, c)).collect()
    }

    /// The platform's middle along the heading, in meters.
    fn middle(&self) -> f32 {
        let low = self
            .near_layer
            .min(self.near_layer + self.sign * (self.side - 1));
        (low as f32 + self.side as f32 / 2.0) * CELL_SIZE_M
    }
}

impl BuilderBot {
    pub fn new(side: i32) -> Self {
        Self { side }
    }

    /// Lays out the platform, places it with the brush, then walks onto it.
    pub fn run(&self, t: &mut TestApp) -> Result<BuildReport, BotError> {
        let (frame0, start) = (t.frame(), t.player().pos);
        let quarter = (t.player().yaw / FRAC_PI_2).round();
        let heading = quarter * FRAC_PI_2;
        aim(t, heading, 0.0)?;
        let platform = self.lay_out(t, quarter as i32)?;

        if !t.editor().open {
            tap(t, TOGGLE_EDITOR_KEY)?;
        }
        tap(t, SET_KEY)?;
        tap(t, CELL_SHAPE_KEY)?;
        let editor = t.editor().clone();
        if !(editor.open && editor.op == BrushOp::Set && editor.shape == BrushShape::Cell) {
            return Err(BotError::EditorKeys {
                op: editor.op,
                shape: editor.shape,
                open: editor.open,
            });
        }
        let material = editor
            .brush_material(replica(t).materials())
            .ok_or(BotError::NoBrushMaterial)?;

        let cells = platform.cells();
        for &cell in &cells {
            place(t, cell, material)?;
        }
        tap(t, TOGGLE_EDITOR_KEY)?;

        aim(t, heading, 0.0)?;
        let middle = platform.middle();
        let sign = platform.sign as f32;
        let along = platform.along;
        let reached = |t: &TestApp| (t.player().pos[along] - middle) * sign >= 0.0;
        if !reached(t) {
            t.press(KeyCode::KeyW);
            let result = walk_until(t, reached);
            t.release(KeyCode::KeyW);
            result?;
        }
        step(t)?;

        Ok(BuildReport {
            cells,
            material,
            frames: t.frame() - frame0,
            start,
        })
    }

    /// The platform for the heading `quarter` quarter turns from -Z.
    fn lay_out(&self, t: &TestApp, quarter: i32) -> Result<Platform, BotError> {
        let forward =
            [IVec3::NEG_Z, IVec3::NEG_X, IVec3::Z, IVec3::X][quarter.rem_euclid(4) as usize];
        let along = if forward.x != 0 { 0 } else { 2 };
        let sign = forward[along];
        let feet = t.player().pos;
        let side_m = self.side as f32 * CELL_SIZE_M;
        let front = feet[along] + sign as f32 * PLAYER_WIDTH_M / 2.0;
        let front_layer = (front / CELL_SIZE_M).floor() as i32;
        let platform = Platform {
            along,
            sign,
            across: 2 - along,
            first_column: ((feet[2 - along] - side_m / 2.0) / CELL_SIZE_M).round() as i32,
            // Grounded feet rest SKIN_M above the floor; allow as much below.
            row: ((feet.y + SKIN_M) / CELL_SIZE_M).floor() as i32,
            near_layer: front_layer + sign * (1 + BUILD_GAP_CELLS),
            side: self.side,
        };
        let world = replica(t);
        if let Some(cell) = platform.cells().into_iter().find(|&c| {
            world.get_cell(c) != Some(MaterialId::AIR) || !world.is_solid(CellPos(c.0 - IVec3::Y))
        }) {
            return Err(BotError::NoRoom { cell });
        }
        Ok(platform)
    }
}

/// Places `cell` with one brush click (Set, Cell) at the middle of the top
/// face of the cell under it, after checking that the brush targets it,
/// then steps until the replica shows it.
fn place(t: &mut TestApp, cell: CellPos, material: MaterialId) -> Result<(), BotError> {
    let below = CellPos(cell.0 - IVec3::Y);
    look_at(t, below.center_m() + Vec3::Y * (CELL_SIZE_M / 2.0))?;
    let p = t.player();
    let dir = Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0) * Vec3::NEG_Z;
    let target = raycast(replica(t), p.eye, dir, BRUSH_REACH_M)
        .and_then(|hit| brush_target(BrushOp::Set, hit));
    if target != Some(cell) {
        return Err(BotError::BrushMisaimed { cell, target });
    }
    t.mouse_down(BRUSH_BUTTON);
    let pressed = step(t);
    t.mouse_up(BRUSH_BUTTON);
    pressed?;
    for _ in 0..PLACE_SLACK_FRAMES {
        step(t)?;
        if replica(t).get_cell(cell) == Some(material) {
            return Ok(());
        }
    }
    Err(BotError::Unplaced {
        cell,
        frames: PLACE_SLACK_FRAMES,
    })
}

/// Walks forward until the front of the box is at least `depth` meters
/// along the heading. The key is released for the next step.
fn walk_to(t: &mut TestApp, tunnel: &Tunnel, depth: f32) -> Result<(), BotError> {
    let reached = |t: &TestApp| tunnel.front_depth(t.player().pos) >= depth;
    if reached(t) {
        return Ok(());
    }
    t.press(KeyCode::KeyW);
    let result = walk_until(t, reached);
    t.release(KeyCode::KeyW);
    result
}

/// Steps with the movement keys as they are until `done`, or fails once
/// the feet have not moved for [`STUCK_FRAMES`] steps.
fn walk_until(t: &mut TestApp, done: impl Fn(&TestApp) -> bool) -> Result<(), BotError> {
    let mut last = t.player().pos;
    let mut still = 0;
    while !done(t) {
        step(t)?;
        let feet = t.player().pos;
        if feet.distance(last) < PROGRESS_EPS_M {
            still += 1;
            if still >= STUCK_FRAMES {
                return Err(BotError::Stuck { feet });
            }
        } else {
            still = 0;
        }
        last = feet;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TestConfig;
    use rmc_protocol::ClientMsg;
    use rmc_world::coords::CellAabb;
    use rmc_world::edit::{CommandId, EditCommand, EditOp};
    use rmc_world::materials::Phase;
    use rmc_world::shape::Shape;

    const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;

    /// A logic-only 2 × 4 × 2 chunk world with a floor at y = 100, cleared
    /// above, and a wall of the softest plain diggable solid filling
    /// z = 20..=35 (near face at z = 18 m) from x = 26 to 42, y = 101 to
    /// 112. Returns the app and the wall material.
    fn course() -> (TestApp, MaterialId) {
        let mut t = TestApp::new(TestConfig {
            seed: 5,
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
            render: false,
            ..TestConfig::default()
        });
        let wall = t
            .host_world()
            .materials()
            .iter()
            .filter(|d| d.phase == Phase::Solid && d.diggable)
            .min_by_key(|d| d.hardness)
            .expect("a diggable solid")
            .id;
        let edits = [
            (
                EditOp::Set(wall),
                CellPos::new(20, 100, 4),
                CellPos::new(43, 100, 59),
            ),
            (
                EditOp::Remove,
                CellPos::new(20, 101, 4),
                CellPos::new(43, 124, 59),
            ),
            (
                EditOp::Set(wall),
                CellPos::new(26, 101, 20),
                CellPos::new(42, 112, 35),
            ),
        ];
        for (seq, (op, min, max)) in (1000..).zip(edits) {
            t.send(ClientMsg::Edit(EditCommand {
                id: CommandId {
                    issuer: t.player_id(),
                    seq,
                },
                issuer: t.player_id(),
                op,
                shape: Shape::Box { min, max },
            }));
        }
        t.steps(3);
        (t, wall)
    }

    #[test]
    fn tunnel_is_the_smallest_the_player_fits() {
        assert_eq!(
            (TunnelBot::width_cells(), TunnelBot::height_cells()),
            (2, 4)
        );
        let (w, h) = (TunnelBot::width_cells(), TunnelBot::height_cells());
        assert!(w as f32 * CELL_SIZE_M > PLAYER_WIDTH_M);
        assert!((w - 1) as f32 * CELL_SIZE_M < PLAYER_WIDTH_M);
        assert!(h as f32 * CELL_SIZE_M > PLAYER_HEIGHT_M);
        assert!((h - 1) as f32 * CELL_SIZE_M < PLAYER_HEIGHT_M);
    }

    /// Off the tunnel's middle and 2 m from the wall, the bot centers,
    /// digs 3 slices (24 cells) and walks to the end, through the real
    /// host.
    #[test]
    fn tunnel_bot_digs_and_walks_in() {
        let (mut t, wall) = course();
        t.spawn_player(Vec3::new(17.3, FLOOR_M, 20.0));
        t.steps(20);
        assert!(t.player().grounded);

        let report = TunnelBot::new(3).run(&mut t).expect("the bot digs");
        assert_eq!(report.first_column, 34);
        assert_eq!(report.start.x, 17.3);
        let mut want: Vec<CellPos> = (0..3)
            .flat_map(|k| {
                (34..=35).flat_map(move |x| (101..=104).map(move |y| CellPos::new(x, y, 35 - k)))
            })
            .collect();
        let mut dug = report.cells.clone();
        want.sort_by_key(|c| c.0.to_array());
        dug.sort_by_key(|c| c.0.to_array());
        assert_eq!(dug, want);
        // Top row first within a slice; slices front to back.
        assert_eq!(report.cells[0], CellPos::new(34, 104, 35));
        assert_eq!(report.cells[23], CellPos::new(35, 101, 33));
        let hardness = t.host_world().materials().get(wall).unwrap().hardness;
        let damage = t.app().world().resource::<Pickaxe>().0.damage;
        assert_eq!(report.swings, 24 * u64::from(hardness.div_ceil(damage)));
        for &c in &want {
            assert_eq!(t.host_world().get_cell(c), Some(MaterialId::AIR));
            assert_eq!(replica(&t).get_cell(c), Some(MaterialId::AIR));
        }
        // The cells around the tunnel stand.
        for c in [
            CellPos::new(33, 102, 34),
            CellPos::new(36, 102, 34),
            CellPos::new(34, 105, 34),
            CellPos::new(35, 102, 32),
        ] {
            assert_eq!(t.host_world().get_cell(c), Some(wall), "{c:?}");
        }

        // Feet 1.5 m (3 slices) minus half a box into the wall, centered,
        // standing, facing -Z, still.
        let p = t.player();
        assert!((p.pos.z - (16.5 + 0.3 + END_GAP_M)).abs() < 0.08, "{p:?}");
        assert!((p.pos.x - 17.5).abs() <= 0.1, "{p:?}");
        assert!(p.grounded && p.velocity == Vec3::ZERO, "{p:?}");
        assert!(p.yaw.abs() < 1e-4, "{p:?}");
        assert!(t.player_penetration().is_empty());
        t.step();
        assert_eq!(replica(&t).hash(), t.host_world().hash());
    }

    #[test]
    fn bots_report_what_stops_them() {
        let (mut t, _) = course();
        t.spawn_player(Vec3::new(17.5, FLOOR_M, 20.0));
        t.steps(20);

        // Air is not diggable; a cell inside the wall is out of sight.
        let air = CellPos::new(34, 103, 38);
        assert_eq!(
            DigBot::new(air).run(&mut t),
            Err(BotError::NotDiggable { cell: air })
        );
        let hidden = CellPos::new(34, 103, 31);
        assert!(matches!(
            DigBot::new(hidden).run(&mut t),
            Err(BotError::OutOfSight { cell, .. }) if cell == hidden
        ));

        // Facing +Z, the first thing ahead is the world's undiggable shell
        // at z = 63: the bot walks up to it and reports its top cell.
        let p = t.player();
        let sensitivity = ControllerConfig::default().sensitivity_rad;
        t.look(-PI / sensitivity, 0.0);
        t.step();
        assert!((wrap(t.player().yaw - p.yaw).abs() - PI).abs() < 1e-3);
        let shell = CellPos::new(34, 104, 63);
        let id = t.host_world().get_cell(shell).unwrap();
        assert!(!t.host_world().materials().get(id).unwrap().diggable);
        assert_eq!(
            TunnelBot::new(3).run(&mut t).map(|r| r.cells),
            Err(BotError::NotDiggable { cell: shell })
        );
        let front = t.player().pos.z + PLAYER_WIDTH_M / 2.0;
        assert!(
            front >= 63.0 * CELL_SIZE_M - STANDOFF_M,
            "walked up: {front}"
        );
    }

    /// From the floor 8.5 m out from the wall, facing it, the bot places a
    /// 3 × 3 platform of the brush's default material with the editor
    /// keys and clicks, then stands on it, through the real host.
    #[test]
    fn builder_bot_places_and_climbs() {
        let (mut t, _) = course();
        t.spawn_player(Vec3::new(17.5, FLOOR_M, 27.0));
        t.steps(20);
        assert!(t.player().grounded);

        let report = BuilderBot::new(3).run(&mut t).expect("the bot builds");
        let table = t.host_world().materials().clone();
        assert_eq!(
            Some(report.material),
            rmc_client::editor::default_material(&table)
        );
        // Three cells ahead of the box's front layer (z = 53) is a one-cell
        // gap (z = 52); the columns center on x = 17.5 m.
        let mut want: Vec<CellPos> = (34..=36)
            .flat_map(|x| (49..=51).map(move |z| CellPos::new(x, 101, z)))
            .collect();
        let mut placed = report.cells.clone();
        want.sort_by_key(|c| c.0.to_array());
        placed.sort_by_key(|c| c.0.to_array());
        assert_eq!(placed, want);
        // Far row first, outer columns before the middle.
        assert_eq!(report.cells[0], CellPos::new(34, 101, 49));
        assert_eq!(report.cells[2], CellPos::new(35, 101, 49));
        assert_eq!(report.cells[8], CellPos::new(35, 101, 51));
        for &c in &want {
            assert_eq!(t.host_world().get_cell(c), Some(report.material));
            assert_eq!(replica(&t).get_cell(c), Some(report.material));
        }
        assert!(!t.editor().open, "closed again");

        // On top, just past the middle (z = 25.25 m), facing -Z.
        t.steps(5);
        let p = t.player();
        assert!(
            p.grounded && (p.pos.y - 102.0 * CELL_SIZE_M).abs() < 0.01,
            "{p:?}"
        );
        assert!(p.pos.z <= 25.25 && p.pos.z > 25.25 - 0.1, "{p:?}");
        assert!((p.pos.x - 17.5).abs() < 1e-3, "{p:?}");
        assert!(t.player_penetration().is_empty());
        t.step();
        assert_eq!(replica(&t).hash(), t.host_world().hash());

        // Facing the wall from close up, the platform would go into it.
        let mut t = course().0;
        t.spawn_player(Vec3::new(17.5, FLOOR_M, 19.5));
        t.steps(20);
        assert_eq!(
            BuilderBot::new(3).run(&mut t).map(|r| r.cells),
            Err(BotError::NoRoom {
                cell: CellPos::new(34, 101, 34)
            })
        );
    }
}
