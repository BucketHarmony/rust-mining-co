//! Scripted players (SAD §12.3), composed from the [`TestApp`] API.
//!
//! Bots only inject keys and mouse motion and read what the client shows:
//! the player, the pickaxe's target and the replica. They never edit a
//! world directly. After every step a bot takes it checks that the
//! player's box is clear of blocking cells.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::time::{Duration, Instant};

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
    /// How many columns wide the tunnel is.
    pub width: i32,
    pub swings: u64,
    pub frames: u32,
    /// The feet when the bot started.
    pub start: Vec3,
}

/// Digs a tunnel the player fits through, [`TunnelBot::length`] cells
/// forward, walking in as it goes.
///
/// The heading is the player's yaw snapped to the nearest world axis. The
/// tunnel is [`TunnelBot::width_cells`] wide if the box, [`SKIN_M`] clear
/// on each side, already fits in that many columns where it stands, and
/// is dug around it. Otherwise it is one column wider, centered as near
/// the player's feet as the grid allows, and the bot steps sideways to its
/// middle: a walk step (about 0.07 m) is wider than the spare width of a
/// one-cell tunnel (0.05 m), so the bot could not step into one. The
/// tunnel is [`TunnelBot::height_cells`] high from the cell layer the feet
/// stand in. It starts at the first cell layer ahead with a solid cell in
/// that cross-section.
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
    /// Columns across.
    width: i32,
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
            .flat_map(move |row| (0..self.width).map(move |c| self.cell(k, c, row)))
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
        (self.first_column as f32 + self.width as f32 / 2.0) * CELL_SIZE_M
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
            width: tunnel.width,
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
        // The columns the box, SKIN_M clear on each side, covers now.
        let half = PLAYER_WIDTH_M / 2.0 + SKIN_M;
        let covered = |edge: f32| ((feet[across] + edge) / CELL_SIZE_M).floor() as i32;
        let (lo, hi) = (covered(-half), covered(half));
        let (first_column, width) = if hi - lo < Self::width_cells() {
            (lo, Self::width_cells())
        } else {
            let width = Self::width_cells() + 1;
            let width_m = width as f32 * CELL_SIZE_M;
            let first = ((feet[across] - width_m / 2.0) / CELL_SIZE_M).round() as i32;
            (first, width)
        };
        let mut tunnel = Tunnel {
            along,
            sign: forward[along],
            across,
            right: right[across],
            first_column,
            width,
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
    /// at least a quarter of the spare width on each side. A tunnel of
    /// [`TunnelBot::width_cells`] is laid out only where the box already
    /// fits, [`SKIN_M`] clear, and needs no step.
    fn center(&self, t: &mut TestApp, tunnel: &Tunnel) -> Result<(), BotError> {
        if tunnel.width == Self::width_cells() {
            return Ok(());
        }
        let spare = tunnel.width as f32 * CELL_SIZE_M - PLAYER_WIDTH_M;
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

/// One kind of thing a [`ChaosBot`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChaosAction {
    /// Holds a random mix of the walk keys, maybe sprinting, while the view
    /// drifts.
    Walk,
    /// Turns the view to a random heading and pitch over a few frames.
    Look,
    /// Looks down a random amount and holds the swing button, maybe
    /// walking forward.
    Dig,
    /// Opens the editor, picks Set, a random shape and maybe the next
    /// material, clicks the brush a few times at random spots, and closes
    /// it.
    Place,
    /// Holds Space for a few frames, maybe walking forward, then waits to
    /// land.
    Jump,
}

impl ChaosAction {
    pub const ALL: [ChaosAction; 5] = [
        ChaosAction::Walk,
        ChaosAction::Look,
        ChaosAction::Dig,
        ChaosAction::Place,
        ChaosAction::Jump,
    ];
}

/// What [`ChaosBot::run`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChaosReport {
    /// The actions, in the order they started. The last one may have been
    /// cut short by the end of the run.
    pub actions: Vec<ChaosAction>,
    /// Steps taken by the actions: exactly [`ChaosBot::frames`].
    pub frames: u32,
    /// Steps taken after them to let go of every input and close the
    /// editor.
    pub cleanup_frames: u32,
    pub swings: u64,
    pub brush_clicks: u32,
    pub jumps: u32,
    /// Steps after which the player's box shared volume with a blocking
    /// cell. Recorded only: the bot checks nothing about the world.
    pub penetrating_frames: u32,
    /// Wall-clock time of each of the [`frames`](Self::frames) steps, the
    /// GPU's included when rendering.
    pub step_times: Vec<Duration>,
    /// The feet when the bot started and when it stopped.
    pub start: Vec3,
    pub end: Vec3,
}

impl ChaosReport {
    /// How many actions of `kind` started.
    pub fn count(&self, kind: ChaosAction) -> usize {
        self.actions.iter().filter(|&&a| a == kind).count()
    }

    /// The `q`-th percentile (0..=100) of the step times, nearest rank
    /// below; zero with no steps.
    pub fn step_time_percentile(&self, q: u32) -> Duration {
        let mut times = self.step_times.clone();
        times.sort_unstable();
        let rank = times.len().saturating_sub(1) * q.min(100) as usize / 100;
        times.get(rank).copied().unwrap_or_default()
    }

    /// One line for logs.
    pub fn summary(&self) -> String {
        let counts: Vec<String> = ChaosAction::ALL
            .iter()
            .map(|&k| format!("{k:?} {}", self.count(k)))
            .collect();
        format!(
            "{} actions ({}), {} swings, {} brush clicks, {} jumps, feet {} -> {}",
            self.actions.len(),
            counts.join(", "),
            self.swings,
            self.brush_clicks,
            self.jumps,
            self.start,
            self.end
        )
    }
}

/// Plays at random for [`ChaosBot::frames`] steps: walks, looks, digs,
/// places and jumps (SAD §12.3), for the soak test.
///
/// The actions come from a shuffled bag of every [`ChaosAction`], refilled
/// when empty, so every kind runs within any ten actions in a row. All
/// randomness comes from [`ChaosBot::seed`]: the same seed gives the same
/// inputs. Like the other bots it only injects input; unlike them it checks
/// nothing about the world, which is the scenario's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChaosBot {
    pub seed: u64,
    pub frames: u32,
}

impl ChaosBot {
    pub fn new(seed: u64, frames: u32) -> Self {
        Self { seed, frames }
    }

    /// Runs actions until [`frames`](Self::frames) steps are taken, then
    /// lets go of every key and button and closes the editor.
    ///
    /// Fails if the editor keys do not set the brush up, or the editor is
    /// still open after the cleanup.
    pub fn run(&self, t: &mut TestApp) -> Result<ChaosReport, BotError> {
        let start = t.player().pos;
        let swings0 = swings(t);
        let mut chaos = Chaos {
            t: &mut *t,
            rng: SplitMix64(self.seed),
            left: self.frames,
            report: ChaosReport {
                start,
                step_times: Vec::with_capacity(self.frames as usize),
                ..ChaosReport::default()
            },
        };
        let mut bag = Vec::new();
        while chaos.left > 0 {
            if bag.is_empty() {
                bag = ChaosAction::ALL.to_vec();
                chaos.shuffle(&mut bag);
            }
            let action = bag.pop().expect("the bag was refilled");
            chaos.report.actions.push(action);
            match action {
                ChaosAction::Walk => chaos.walk(),
                ChaosAction::Look => chaos.look(),
                ChaosAction::Dig => chaos.dig(),
                ChaosAction::Place => chaos.place()?,
                ChaosAction::Jump => chaos.jump(),
            }
        }
        chaos.cleanup()?;
        let mut report = chaos.report;
        report.swings = swings(t) - swings0;
        report.end = t.player().pos;
        Ok(report)
    }
}

/// Keys a [`ChaosBot`] may hold.
const CHAOS_KEYS: [KeyCode; 6] = [
    KeyCode::KeyW,
    KeyCode::KeyA,
    KeyCode::KeyS,
    KeyCode::KeyD,
    KeyCode::ShiftLeft,
    KeyCode::Space,
];
/// What a walk holds on each axis, picked evenly: nothing or one key.
/// With nothing on both, it holds W.
const CHAOS_ALONG_KEYS: [Option<KeyCode>; 3] = [None, Some(KeyCode::KeyW), Some(KeyCode::KeyS)];
const CHAOS_ACROSS_KEYS: [Option<KeyCode>; 3] = [None, Some(KeyCode::KeyA), Some(KeyCode::KeyD)];
/// Frames a walk lasts.
const CHAOS_WALK_FRAMES: (u32, u32) = (20, 90);
/// Fastest view drift while walking, in radians per frame.
const CHAOS_DRIFT_RAD: f32 = 0.03;
/// Frames a look takes.
const CHAOS_LOOK_FRAMES: (u32, u32) = (5, 30);
/// Pitch a look or a brush click aims at, in radians.
const CHAOS_LOOK_PITCH: (f32, f32) = (-1.2, 0.5);
/// Pitch a dig aims at, in radians: mostly down, where the ground is.
const CHAOS_DIG_PITCH: (f32, f32) = (-1.3, 0.2);
/// Frames the swing button is held.
const CHAOS_DIG_FRAMES: (u32, u32) = (20, 150);
/// Brush clicks per placing.
const CHAOS_CLICKS: (u32, u32) = (1, 3);
/// Largest turn between brush clicks, in radians.
const CHAOS_CLICK_TURN_RAD: f32 = 0.5;
/// Frames Space is held.
const CHAOS_JUMP_FRAMES: (u32, u32) = (1, 12);
/// Frames a jump waits to land.
const CHAOS_LAND_FRAMES: u32 = 60;
/// The brush shapes a placing picks from, by key: Cell as often as Box and
/// Sphere together.
const CHAOS_SHAPE_KEYS: [KeyCode; 4] = [
    KeyCode::Digit4,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
];
const NEXT_MATERIAL_KEY: KeyCode = KeyCode::Period;

/// A seeded generator (SplitMix64), so a seed gives the same chaos on
/// every platform.
#[derive(Clone, Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    fn range(&mut self, (lo, hi): (u32, u32)) -> u32 {
        lo + (self.next_u64() % u64::from(hi - lo + 1)) as u32
    }

    /// Uniform in `lo..hi`.
    fn between(&mut self, (lo, hi): (f32, f32)) -> f32 {
        let unit = (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32;
        lo + (hi - lo) * unit
    }

    /// True one time in `n`.
    fn one_in(&mut self, n: u32) -> bool {
        self.range((1, n)) == 1
    }

    /// One of `items`.
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.range((0, items.len() as u32 - 1)) as usize]
    }
}

/// A [`ChaosBot`] run in progress. Every action stops early once the run
/// has no steps left, leaving the keys it held to [`Chaos::cleanup`].
struct Chaos<'a> {
    t: &'a mut TestApp,
    rng: SplitMix64,
    /// Steps left in the run.
    left: u32,
    report: ChaosReport,
}

impl Chaos<'_> {
    /// One timed step, if the run has any left. Returns whether it stepped.
    fn step(&mut self) -> bool {
        if self.left == 0 {
            return false;
        }
        let clock = Instant::now();
        self.t.step();
        self.report.step_times.push(clock.elapsed());
        self.left -= 1;
        self.report.frames += 1;
        if !self.t.player_penetration().is_empty() {
            self.report.penetrating_frames += 1;
        }
        true
    }

    /// `n` steps; false if the run ended first.
    fn steps(&mut self, n: u32) -> bool {
        (0..n).all(|_| self.step())
    }

    /// Presses and releases `key`, one step each.
    fn tap(&mut self, key: KeyCode) -> bool {
        self.t.press(key);
        let pressed = self.step();
        self.t.release(key);
        pressed && self.step()
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.rng.range((0, i as u32)) as usize;
            items.swap(i, j);
        }
    }

    fn sensitivity(&self) -> f32 {
        self.t
            .app()
            .world()
            .resource::<ControllerConfig>()
            .sensitivity_rad
    }

    /// The mouse motion per step that turns the view by `yaw` radians
    /// (positive turns left) and to `pitch` in `frames` equal steps.
    fn turn_per_frame(&self, yaw: f32, pitch: f32, frames: u32) -> Vec2 {
        let p = self.t.player();
        Vec2::new(-yaw, p.pitch - pitch) / (self.sensitivity() * frames as f32)
    }

    /// Turns by `yaw` and to `pitch` in one step.
    fn aim(&mut self, yaw: f32, pitch: f32) -> bool {
        let m = self.turn_per_frame(yaw, pitch, 1);
        self.t.look(m.x, m.y);
        self.step()
    }

    fn walk(&mut self) {
        let mut keys = Vec::new();
        keys.extend(self.rng.pick(&CHAOS_ALONG_KEYS));
        keys.extend(self.rng.pick(&CHAOS_ACROSS_KEYS));
        if keys.is_empty() {
            keys.push(KeyCode::KeyW);
        }
        if self.rng.one_in(3) {
            keys.push(KeyCode::ShiftLeft);
        }
        let frames = self.rng.range(CHAOS_WALK_FRAMES);
        let drift = self.rng.between((-CHAOS_DRIFT_RAD, CHAOS_DRIFT_RAD)) / self.sensitivity();
        for &key in &keys {
            self.t.press(key);
        }
        for _ in 0..frames {
            self.t.look(drift, 0.0);
            if !self.step() {
                break;
            }
        }
        for &key in &keys {
            self.t.release(key);
        }
    }

    fn look(&mut self) {
        let yaw = self.rng.between((-PI, PI));
        let pitch = self.rng.between(CHAOS_LOOK_PITCH);
        let frames = self.rng.range(CHAOS_LOOK_FRAMES);
        let m = self.turn_per_frame(yaw, pitch, frames);
        for _ in 0..frames {
            self.t.look(m.x, m.y);
            if !self.step() {
                break;
            }
        }
    }

    fn dig(&mut self) {
        let pitch = self.rng.between(CHAOS_DIG_PITCH);
        let forward = self.rng.one_in(3);
        let frames = self.rng.range(CHAOS_DIG_FRAMES);
        if !self.aim(0.0, pitch) {
            return;
        }
        if forward {
            self.t.press(KeyCode::KeyW);
        }
        self.t.mouse_down(SWING_BUTTON);
        self.steps(frames);
        self.t.mouse_up(SWING_BUTTON);
        if forward {
            self.t.release(KeyCode::KeyW);
        }
    }

    fn place(&mut self) -> Result<(), BotError> {
        let shape = self.rng.pick(&CHAOS_SHAPE_KEYS);
        let next_material = self.rng.one_in(2);
        let clicks = self.rng.range(CHAOS_CLICKS);
        let opened = self.t.editor().open || self.tap(TOGGLE_EDITOR_KEY);
        if !(opened && self.tap(SET_KEY) && self.tap(shape)) {
            return Ok(());
        }
        if next_material && !self.tap(NEXT_MATERIAL_KEY) {
            return Ok(());
        }
        let editor = self.t.editor().clone();
        if !(editor.open && editor.op == BrushOp::Set) {
            return Err(BotError::EditorKeys {
                op: editor.op,
                shape: editor.shape,
                open: editor.open,
            });
        }
        for _ in 0..clicks {
            let yaw = self
                .rng
                .between((-CHAOS_CLICK_TURN_RAD, CHAOS_CLICK_TURN_RAD));
            let pitch = self.rng.between(CHAOS_LOOK_PITCH);
            if !self.aim(yaw, pitch) {
                return Ok(());
            }
            self.t.mouse_down(BRUSH_BUTTON);
            let pressed = self.step();
            self.t.mouse_up(BRUSH_BUTTON);
            if !pressed {
                return Ok(());
            }
            self.report.brush_clicks += 1;
            if !self.steps(PLACE_SLACK_FRAMES) {
                return Ok(());
            }
        }
        self.tap(TOGGLE_EDITOR_KEY);
        Ok(())
    }

    fn jump(&mut self) {
        let frames = self.rng.range(CHAOS_JUMP_FRAMES);
        let forward = self.rng.one_in(2);
        if forward {
            self.t.press(KeyCode::KeyW);
        }
        self.t.press(KeyCode::Space);
        self.report.jumps += 1;
        let held = self.steps(frames);
        self.t.release(KeyCode::Space);
        if held {
            for _ in 0..CHAOS_LAND_FRAMES {
                if !self.step() || self.t.player().grounded {
                    break;
                }
            }
        }
        if forward {
            self.t.release(KeyCode::KeyW);
        }
    }

    /// Lets go of every key and button and closes the editor, in steps
    /// that do not count toward the run.
    fn cleanup(&mut self) -> Result<(), BotError> {
        for key in CHAOS_KEYS {
            self.t.release(key);
        }
        self.t.mouse_up(SWING_BUTTON);
        self.t.mouse_up(BRUSH_BUTTON);
        self.t.step();
        self.report.cleanup_frames += 1;
        if self.t.editor().open {
            self.t.press(TOGGLE_EDITOR_KEY);
            self.t.step();
            self.t.release(TOGGLE_EDITOR_KEY);
            self.t.step();
            self.report.cleanup_frames += 2;
        }
        let editor = self.t.editor().clone();
        if editor.open {
            return Err(BotError::EditorKeys {
                op: editor.op,
                shape: editor.shape,
                open: editor.open,
            });
        }
        Ok(())
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
            .filter(|d| d.phase == Phase::Solid && !d.passable && d.diggable)
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
        // The width follows the player's box (owner change F094): the two
        // inequalities below pin it to the one cell count that fits.
        assert_eq!(TunnelBot::height_cells(), 4);
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
        // At x = 17.3 m the box straddles columns 34 and 35: one wider
        // than the fewest cells it fits.
        assert_eq!(report.width, TunnelBot::width_cells() + 1);
        assert_eq!(report.width, 2);
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

        // Feet 1.5 m (3 slices) minus half a box into the wall, centered
        // to a quarter of the two-cell tunnel's spare width, standing,
        // facing -Z, still.
        let p = t.player();
        let half = PLAYER_WIDTH_M / 2.0;
        assert!((p.pos.z - (16.5 + half + END_GAP_M)).abs() < 0.08, "{p:?}");
        let spare = 2.0 * CELL_SIZE_M - PLAYER_WIDTH_M;
        assert!((p.pos.x - 17.5).abs() <= spare / 4.0, "{p:?}");
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

    /// Plays a seeded chaos run of `frames` from the course's floor, 2 m
    /// out from the wall, and returns the report and the app.
    fn chaos(seed: u64, frames: u32) -> (ChaosReport, TestApp) {
        let (mut t, _) = course();
        t.spawn_player(Vec3::new(17.5, FLOOR_M, 20.0));
        t.steps(20);
        let before = t.host_world().hash();
        let report = ChaosBot::new(seed, frames)
            .run(&mut t)
            .expect("the chaos runs");
        assert_ne!(t.host_world().hash(), before, "the chaos changed the world");
        (report, t)
    }

    /// Through the real host: the bot takes exactly its frames, does every
    /// kind of action, leaves no input held and the editor closed, and the
    /// same seed plays the same game.
    #[test]
    fn chaos_bot_plays_every_action_reproducibly() {
        let frames = 900;
        let (report, mut t) = chaos(7, frames);
        assert_eq!(report.frames, frames);
        assert_eq!(report.step_times.len(), frames as usize);
        assert!(report.cleanup_frames >= 1);
        for kind in ChaosAction::ALL {
            assert!(report.count(kind) > 0, "no {kind:?}: {}", report.summary());
        }
        // The shuffled bag: every five actions from the start hold each
        // kind once.
        let (bags, _) = report.actions.as_chunks::<{ ChaosAction::ALL.len() }>();
        for bag in bags {
            let mut kinds = *bag;
            kinds.sort();
            assert_eq!(kinds, ChaosAction::ALL);
        }
        assert!(report.swings > 0 && report.brush_clicks > 0 && report.jumps > 0);
        assert!(report.start.distance(report.end) > 0.0, "the player moved");
        assert!(report.step_time_percentile(95) >= report.step_time_percentile(50));
        assert_eq!(
            report.step_time_percentile(100),
            *report.step_times.iter().max().unwrap()
        );
        assert!(!t.editor().open);
        let input = t.app().world().resource::<ButtonInput<KeyCode>>();
        assert_eq!(input.get_pressed().count(), 0, "keys let go");
        let mouse = t.app().world().resource::<ButtonInput<MouseButton>>();
        assert_eq!(mouse.get_pressed().count(), 0, "buttons let go");
        t.step();
        assert_eq!(replica(&t).hash(), t.host_world().hash());

        let (again, t2) = chaos(7, frames);
        assert_eq!(again.actions, report.actions);
        assert_eq!(
            (again.swings, again.brush_clicks, again.jumps, again.end),
            (report.swings, report.brush_clicks, report.jumps, report.end)
        );
        assert_eq!(t2.host_world().hash(), t.host_world().hash());
        let (other, _) = chaos(8, frames);
        assert_ne!(other.actions, report.actions, "another seed, another game");
    }
}
