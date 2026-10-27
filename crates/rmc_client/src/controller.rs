//! Kinematic first-person character controller (SAD §11.4).
//!
//! The player is an axis-aligned box [`PLAYER_WIDTH_M`] wide and
//! [`PLAYER_HEIGHT_M`] tall, with the eye [`EYE_HEIGHT_M`] above the feet.
//! There is no physics engine: every fixed step [`step`] applies gravity and
//! the walk input to the velocity, then moves the box one axis at a time
//! (Y, then X, then Z) with a swept test against the cells of the replica.
//! A move along an axis stops [`SKIN_M`] short of the first blocking cell
//! layer it would enter, so a box that starts free never comes to overlap a
//! blocking cell, and a blocked axis does not stop the others: walking
//! diagonally into a wall slides along it.
//!
//! A box that overlaps a `Solid` cell at the start of a step (a spawn in
//! rock, a cell set inside the player by another source, a load) is first
//! pushed out: the feet move to the nearest spot where the box is free,
//! preferring up ([`free_feet_near`]), and the velocity is dropped. With no
//! free spot in reach, or overlapping only cells that block without being
//! solid (an unloaded chunk), it stays and can move away. The host already
//! keeps its own `Set` cells out of the box it was last told about (SAD §8.2).
//!
//! Which cells block is [`blocks`]: `Solid` phase cells, every cell outside
//! the world bounds below or beside the world, and cells in chunks the
//! replica does not hold yet. Above the top of the world is open sky.
//!
//! [`ControllerPlugin`] moves every [`Player`] in `FixedUpdate`, reading only
//! the [`Replica`] and `ButtonInput<KeyCode>`. Mouse look reads
//! `AccumulatedMouseMotion` in `Update` (it accumulates per frame, not per
//! fixed step), and the [`GameCamera`] then follows the player's eye,
//! interpolated between the last two fixed steps ([`crate::interp`]). While
//! the fly camera is on, the controller neither moves, turns nor follows.
//!
//! A grounded player that walks into a one-cell rise climbs it when the
//! space above is clear: the feet snap up one cell and the eye follows over
//! [`STEP_SMOOTH_S`]. Two cells need a jump.
//!
//! - W / S walk forward and back along the yaw, D / A strafe, opposite keys
//!   cancel and diagonals are as fast as one axis. Shift sprints. Space
//!   jumps when grounded (held, it jumps again on landing).
//! - Ctrl crouches while held: the box is [`CROUCH_HEIGHT_M`] tall, so it
//!   fits a three-cell gap, the eye eases down to [`CROUCH_EYE_HEIGHT_M`]
//!   over [`CROUCH_SMOOTH_S`], the walk is
//!   [`ControllerConfig::crouch_speed_mps`] with no sprint, and a grounded
//!   crouching player does not walk off a ledge: a horizontal move that
//!   would leave no ground under the box is refused on that axis, so it
//!   slides along the edge. Released, the player stands only when the
//!   standing box is free; under a low ceiling it stays crouched. The host
//!   still keeps `Set` cells out of the standing box, so placing over a
//!   crouching player's head is refused.
//! - In a liquid (the cell at the middle of the box is `Liquid`) the player
//!   swims: gravity is [`ControllerConfig::swim_gravity_mps2`] and the sink
//!   is at most [`ControllerConfig::swim_sink_mps`], Space rises at
//!   [`ControllerConfig::swim_up_mps`], and the walk is
//!   [`ControllerConfig::swim_speed_mps`] with no sprint. Holding Space
//!   against a wall (a bank) while the feet are in the liquid gives a
//!   jump's lift, so the player climbs out.
//! - The mouse turns: yaw right for motion right, pitch clamped to
//!   ±[`MAX_PITCH_DEG`]. With a [`CursorGrab`] (the windowed game), only
//!   while the cursor is grabbed.
//!
//! Once joined (a [`Replica`] exists), every third fixed step from the first
//! ([`PLAYER_STATE_HZ`]) the plugin writes [`ToHost`]`(PlayerState)` with the
//! feet, yaw and pitch after the step. Not while the fly camera is on.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use rmc_protocol::ClientMsg;
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::materials::Phase;
use rmc_world::world::World;

use crate::cursor::{CursorGrab, look_allowed};
use crate::editor::free_feet_near;
use crate::flycam::{FlyCam, MAX_PITCH_DEG};
use crate::interp::InterpolatedEye;
use crate::liquid::settled_fill;
use crate::net::ToHost;
use crate::render::GameCamera;
use crate::replica::Replica;

/// The player's box, shared with the host, which keeps `Set` cells out of
/// it (SAD §8.2).
pub use rmc_protocol::{PLAYER_HEIGHT_M, PLAYER_WIDTH_M};
/// Eye height above the feet, in meters.
pub const EYE_HEIGHT_M: f32 = 1.55;
/// The crouching player's box height, in meters: under three cells.
pub const CROUCH_HEIGHT_M: f32 = 1.4;
/// The crouching player's eye height above the feet, in meters.
pub const CROUCH_EYE_HEIGHT_M: f32 = 1.15;
/// Seconds the eye takes to go down into or up out of a crouch.
pub const CROUCH_SMOOTH_S: f32 = 0.12;
const _: () = assert!(CROUCH_HEIGHT_M < 3.0 * CELL_SIZE_M);
/// Gap left between the box and a blocking cell after a blocked move. Far
/// larger than `f32` rounding at world coordinates, so a stopped box never
/// rounds into the cell it stopped at.
pub const SKIN_M: f32 = 1e-3;

/// Time over which the view catches up after a step-up, in seconds.
pub const STEP_SMOOTH_S: f32 = 0.1;

/// Rate of the fixed step the controller runs in (Bevy's default is 64 Hz).
pub const FIXED_HZ: f64 = 60.0;

/// Rate at which the player's position and view go to the host.
pub const PLAYER_STATE_HZ: f64 = 20.0;

/// Controller tuning.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct ControllerConfig {
    /// Walking speed, m/s.
    pub walk_speed_mps: f32,
    /// Speed while Shift is held, m/s.
    pub sprint_speed_mps: f32,
    /// Speed while crouching, m/s.
    pub crouch_speed_mps: f32,
    /// Speed while swimming, m/s.
    pub swim_speed_mps: f32,
    /// Downward acceleration while swimming, m/s².
    pub swim_gravity_mps2: f32,
    /// Fastest sink while swimming, m/s.
    pub swim_sink_mps: f32,
    /// Upward speed while swimming with Space held, m/s.
    pub swim_up_mps: f32,
    /// Downward acceleration, m/s².
    pub gravity_mps2: f32,
    /// Fastest fall, m/s.
    pub max_fall_mps: f32,
    /// Upward speed a jump starts with, m/s.
    pub jump_speed_mps: f32,
    /// Radians of turn per unit of mouse motion.
    pub sensitivity_rad: f32,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            walk_speed_mps: 4.3,
            sprint_speed_mps: 6.5,
            crouch_speed_mps: 1.9,
            swim_speed_mps: 2.6,
            swim_gravity_mps2: 4.0,
            swim_sink_mps: 1.5,
            swim_up_mps: 3.0,
            gravity_mps2: 20.0,
            max_fall_mps: 40.0,
            jump_speed_mps: 7.0,
            sensitivity_rad: 0.002,
        }
    }
}

/// The local player: where the feet are, how fast it moves and where it
/// looks. `feet` is the middle of the bottom face of the box.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Player {
    pub feet: Vec3,
    /// m/s.
    pub velocity: Vec3,
    /// Radians about world Y; 0 looks along -Z, positive turns left.
    pub yaw: f32,
    /// Radians; positive looks up.
    pub pitch: f32,
    /// Standing on a blocking cell after the last step.
    pub grounded: bool,
    /// Visual eye offset in meters (zero or negative) left by a step-up.
    pub step_offset_m: f32,
    /// Crouching: the box is [`CROUCH_HEIGHT_M`] tall.
    pub crouching: bool,
    /// Visual eye offset in meters (zero or negative) easing toward the
    /// crouch eye height while crouching, and back to zero after.
    pub crouch_offset_m: f32,
    /// In a liquid at the start of the last step (see [`in_liquid`]).
    pub swimming: bool,
}

impl Player {
    /// A player standing still with its feet at `feet`, looking along -Z.
    pub fn at(feet: Vec3) -> Self {
        Self {
            feet,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            grounded: false,
            step_offset_m: 0.0,
            crouching: false,
            crouch_offset_m: 0.0,
            swimming: false,
        }
    }

    /// The box's height: standing or crouching.
    pub fn height(&self) -> f32 {
        if self.crouching {
            CROUCH_HEIGHT_M
        } else {
            PLAYER_HEIGHT_M
        }
    }

    /// The player's box.
    pub fn aabb(&self) -> Aabb {
        Aabb::of_feet_height(self.feet, self.height())
    }

    /// The eye position, lowered by any step-up still being smoothed and by
    /// the crouch.
    pub fn eye(&self) -> Vec3 {
        self.feet + Vec3::Y * (EYE_HEIGHT_M + self.step_offset_m + self.crouch_offset_m)
    }

    /// The view rotation: yaw about world Y, then pitch, no roll.
    pub fn rotation(&self) -> Quat {
        Quat::from_euler(EulerRot::YXZ, self.yaw, self.pitch, 0.0)
    }
}

/// A box in meters, `min <= max` on every axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    /// The player's box for feet at `feet`.
    pub fn of_feet(feet: Vec3) -> Self {
        Self::of_feet_height(feet, PLAYER_HEIGHT_M)
    }

    /// The box of a player `height` meters tall with its feet at `feet`.
    pub fn of_feet_height(feet: Vec3, height: f32) -> Self {
        let half = PLAYER_WIDTH_M / 2.0;
        Self {
            min: feet - Vec3::new(half, 0.0, half),
            max: feet + Vec3::new(half, height, half),
        }
    }

    /// Whether the box shares volume with `cell` (touching faces do not).
    pub fn overlaps_cell(&self, cell: CellPos) -> bool {
        let lo = cell.min_corner_m();
        let hi = lo + Vec3::splat(CELL_SIZE_M);
        self.min.cmplt(hi).all() && self.max.cmpgt(lo).all()
    }
}

/// What the player asks for this step: walk axes in -1..=1 relative to the
/// yaw, and sprint.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MoveInput {
    /// +1 forward, -1 back.
    pub forward: f32,
    /// +1 right, -1 left.
    pub right: f32,
    pub sprint: bool,
    pub jump: bool,
    pub crouch: bool,
}

impl MoveInput {
    /// The input the held keys give.
    pub fn from_keys(keys: &ButtonInput<KeyCode>) -> Self {
        let axis = |plus: KeyCode, minus: KeyCode| {
            (keys.pressed(plus) as i32 - keys.pressed(minus) as i32) as f32
        };
        Self {
            forward: axis(KeyCode::KeyW, KeyCode::KeyS),
            right: axis(KeyCode::KeyD, KeyCode::KeyA),
            sprint: keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]),
            jump: keys.pressed(KeyCode::Space),
            crouch: keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]),
        }
    }
}

/// Whether `cell` stops the player: a `Solid` cell that is not `passable`
/// (SAD-P2 §5: plants are walked through), a cell below or beside the world
/// bounds, or a cell in a chunk the world does not hold. Cells above the
/// top of the world are open.
pub fn blocks(world: &World, cell: CellPos) -> bool {
    let bounds = world.bounds();
    if cell.0.y > bounds.max.0.y {
        return false;
    }
    world.get_cell(cell).is_none() || collides(world, cell)
}

/// Whether the middle of a box `height` meters tall with its feet at `feet`
/// is under a liquid's surface: the player swims. With a `height` of 0,
/// whether the feet are. The surface is the cell's poured fill
/// ([`crate::liquid`]), so a rain film over a shallow cell is not one; a
/// liquid run that holds no fill (placed without the ecology) is liquid
/// all through.
pub fn in_liquid(world: &World, feet: Vec3, height: f32) -> bool {
    let point = feet + Vec3::Y * (height / 2.0);
    let waist = CellPos::from_meters(point);
    let liquid = world
        .get_cell(waist)
        .and_then(|id| world.materials().get(id))
        .is_some_and(|d| d.phase == Phase::Liquid);
    if !liquid {
        return false;
    }
    let Some((fill, total)) = settled_fill(world, waist) else {
        return false;
    };
    if total == 0 {
        return true;
    }
    let depth = (point.y / CELL_SIZE_M).fract();
    depth * f32::from(u8::MAX) < f32::from(fill)
}

/// Whether `cell` holds a `Solid`, non-`passable` material.
fn collides(world: &World, cell: CellPos) -> bool {
    world
        .get_cell(cell)
        .and_then(|id| world.materials().get(id))
        .is_some_and(|d| d.is_solid() && !d.passable)
}

/// One fixed step of `dt` seconds: push-out, jump, gravity, walk input, then
/// the swept move.
///
/// A box overlapping a `Solid` cell is first moved to [`free_feet_near`]'s
/// spot, if there is one, standing still and not grounded (see
/// [`push_out`]).
///
/// The horizontal velocity is the walk input along the yaw at walk or
/// sprint speed (no acceleration). A grounded player holding jump starts
/// rising at [`ControllerConfig::jump_speed_mps`]; gravity adds to the
/// vertical velocity up to [`ControllerConfig::max_fall_mps`]. The box then
/// moves along Y, X and Z in turn. An axis that hits a blocking cell stops
/// [`SKIN_M`] short of it and loses its velocity; a blocked fall leaves the
/// player grounded. A player grounded before and after the vertical move
/// that is blocked horizontally climbs one cell if that gets it further (see
/// [`step_up`]), at most once per step.
///
/// A player [`in_liquid`] swims instead: swim speed, swim gravity and sink
/// cap, Space rises at the swim-up speed, and Space held while blocked
/// horizontally with the feet still in the liquid starts the next step
/// rising at the jump speed.
pub fn step(
    world: &World,
    player: &mut Player,
    input: MoveInput,
    config: &ControllerConfig,
    dt: f32,
) {
    push_out(world, player);
    crouch(world, player, input.crouch, dt);
    let yaw = Quat::from_rotation_y(player.yaw);
    let wish = (yaw * Vec3::NEG_Z * input.forward + yaw * Vec3::X * input.right)
        .with_y(0.0)
        .clamp_length_max(1.0);
    player.swimming = in_liquid(world, player.feet, player.height());
    let wading = player.swimming || in_liquid(world, player.feet, 0.0);
    let speed = if player.swimming {
        config.swim_speed_mps
    } else if player.crouching {
        config.crouch_speed_mps
    } else if input.sprint {
        config.sprint_speed_mps
    } else {
        config.walk_speed_mps
    };
    player.velocity.x = wish.x * speed;
    player.velocity.z = wish.z * speed;
    if player.swimming {
        if input.jump {
            player.velocity.y = player.velocity.y.max(config.swim_up_mps);
        }
        player.velocity.y =
            (player.velocity.y - config.swim_gravity_mps2 * dt).max(-config.swim_sink_mps);
    } else {
        if input.jump && player.grounded {
            player.velocity.y = config.jump_speed_mps;
        }
        player.velocity.y =
            (player.velocity.y - config.gravity_mps2 * dt).max(-config.max_fall_mps);
    }
    player.step_offset_m = (player.step_offset_m + CELL_SIZE_M / STEP_SMOOTH_S * dt).min(0.0);

    let delta = player.velocity * dt;
    // Step-up needs the player standing before and after the vertical move:
    // never from the air, nor on the step it lands.
    let was_grounded = player.grounded;
    player.grounded = false;
    let mut stepped = false;
    let mut walled = false;
    for axis in [1, 0, 2] {
        let d = delta[axis];
        if d == 0.0 {
            continue;
        }
        // A crouching player on the ground keeps to it.
        if axis != 1 && player.crouching && was_grounded && player.grounded {
            let mut moved = player.feet;
            moved[axis] += d;
            if !has_ground(world, Aabb::of_feet_height(moved, player.height())) {
                player.velocity[axis] = 0.0;
                continue;
            }
        }
        match sweep(world, player.aabb(), axis, d) {
            Some(stop) => {
                let can_step = axis != 1 && was_grounded && player.grounded && !stepped;
                if can_step && step_up(world, player, axis, d, stop) {
                    stepped = true;
                    continue;
                }
                player.feet[axis] = stop;
                player.velocity[axis] = 0.0;
                if axis == 1 && d < 0.0 {
                    player.grounded = true;
                }
                walled |= axis != 1;
            }
            None => player.feet[axis] += d,
        }
    }
    // Climbing out of a liquid onto a bank.
    if wading && walled && input.jump {
        player.velocity.y = player.velocity.y.max(config.jump_speed_mps);
    }
}

/// Crouches while `want`; otherwise stands up when the standing box is
/// free. Eases the eye toward the height it goes to.
fn crouch(world: &World, player: &mut Player, want: bool, dt: f32) {
    if want {
        player.crouching = true;
    } else if player.crouching && !overlaps_blocking(world, Aabb::of_feet(player.feet)) {
        player.crouching = false;
    }
    let target = if player.crouching {
        CROUCH_EYE_HEIGHT_M - EYE_HEIGHT_M
    } else {
        0.0
    };
    let rate = (EYE_HEIGHT_M - CROUCH_EYE_HEIGHT_M) / CROUCH_SMOOTH_S * dt;
    let gap = target - player.crouch_offset_m;
    player.crouch_offset_m += gap.clamp(-rate, rate);
}

/// Whether a blocking cell lies right under `aabb` (within twice
/// [`SKIN_M`]).
fn has_ground(world: &World, aabb: Aabb) -> bool {
    sweep(world, aabb, 1, -2.0 * SKIN_M).is_some()
}

/// Whether `aabb` shares volume with a cell that [`blocks`].
fn overlaps_blocking(world: &World, aabb: Aabb) -> bool {
    let lo = (aabb.min / CELL_SIZE_M).floor().as_ivec3();
    let hi = (aabb.max / CELL_SIZE_M).ceil().as_ivec3() - IVec3::ONE;
    (lo.y..=hi.y).any(|y| {
        (lo.z..=hi.z).any(|z| {
            (lo.x..=hi.x).any(|x| {
                let cell = CellPos::new(x, y, z);
                blocks(world, cell) && aabb.overlaps_cell(cell)
            })
        })
    })
}

/// Moves a player whose box overlaps a `Solid` cell to the nearest spot
/// where it is free ([`free_feet_near`]: nearest first, then higher), drops
/// its velocity and any step-up smoothing, and returns whether it moved. A
/// box overlapping no solid cell, or with no free spot in reach, stays.
pub fn push_out(world: &World, player: &mut Player) -> bool {
    if !overlaps_solid(world, player.aabb()) {
        return false;
    }
    let Some(feet) = free_feet_near(world, player.feet) else {
        return false;
    };
    player.feet = feet;
    player.velocity = Vec3::ZERO;
    player.grounded = false;
    player.step_offset_m = 0.0;
    true
}

/// Whether `aabb` shares volume with a `Solid`, non-`passable` cell of
/// `world`.
fn overlaps_solid(world: &World, aabb: Aabb) -> bool {
    let lo = (aabb.min / CELL_SIZE_M).floor().as_ivec3();
    let hi = (aabb.max / CELL_SIZE_M).floor().as_ivec3();
    (lo.y..=hi.y).any(|y| {
        (lo.z..=hi.z).any(|z| {
            (lo.x..=hi.x).any(|x| {
                let cell = CellPos::new(x, y, z);
                aabb.overlaps_cell(cell) && collides(world, cell)
            })
        })
    })
}

/// Tries to climb onto what stopped a grounded horizontal move of `d`
/// meters along `axis` at `stop`: lifts the box one cell (if the space above
/// is clear), repeats the move there, then lowers it back onto whatever is
/// below, at most one cell. Keeps the result and returns `true` only if the
/// lifted move gets further than `stop`. The feet snap up; the eye follows
/// over [`STEP_SMOOTH_S`] through [`Player::step_offset_m`].
fn step_up(world: &World, player: &mut Player, axis: usize, d: f32, stop: f32) -> bool {
    let s = CELL_SIZE_M;
    if sweep(world, player.aabb(), 1, s).is_some() {
        return false;
    }
    let mut lifted = player.feet + Vec3::Y * s;
    let reach = sweep(
        world,
        Aabb::of_feet_height(lifted, player.height()),
        axis,
        d,
    );
    let to = reach.unwrap_or(lifted[axis] + d);
    if (to - stop) * d.signum() <= 0.0 {
        return false;
    }
    lifted[axis] = to;
    let landed = sweep(world, Aabb::of_feet_height(lifted, player.height()), 1, -s);
    lifted.y = landed.unwrap_or(lifted.y - s);
    player.step_offset_m -= lifted.y - player.feet.y;
    player.feet = lifted;
    if reach.is_some() {
        player.velocity[axis] = 0.0;
    }
    player.grounded = landed.is_some();
    true
}

/// Moves `aabb` by `d` meters along `axis` (0 = X, 1 = Y, 2 = Z) through the
/// cell grid. Returns `None` if the whole move is clear, or the feet
/// coordinate on `axis` that leaves the box [`SKIN_M`] short of the first
/// layer of cells it would enter that holds a blocking cell.
fn sweep(world: &World, aabb: Aabb, axis: usize, d: f32) -> Option<f32> {
    let s = CELL_SIZE_M;
    // Cells the box covers on the other two axes (touching faces excluded).
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let first = |a: usize| (aabb.min[a] / s).floor() as i32;
    let last = |a: usize| (aabb.max[a] / s).ceil() as i32 - 1;
    let layer_blocks = |c: i32| {
        (first(u)..=last(u)).any(|i| {
            (first(v)..=last(v)).any(|j| {
                let mut cell = IVec3::ZERO;
                cell[axis] = c;
                cell[u] = i;
                cell[v] = j;
                blocks(world, CellPos(cell))
            })
        })
    };
    // Feet offset from the box's min and max on this axis.
    let feet_above_min = if axis == 1 { 0.0 } else { PLAYER_WIDTH_M / 2.0 };
    let feet_below_max = if axis == 1 {
        PLAYER_HEIGHT_M
    } else {
        PLAYER_WIDTH_M / 2.0
    };
    if d > 0.0 {
        let from = (aabb.max[axis] / s).ceil() as i32;
        let to = ((aabb.max[axis] + d) / s).ceil() as i32 - 1;
        (from..=to)
            .find(|&c| layer_blocks(c))
            .map(|c| c as f32 * s - SKIN_M - feet_below_max)
    } else {
        let from = (aabb.min[axis] / s).floor() as i32 - 1;
        let to = ((aabb.min[axis] + d) / s).floor() as i32;
        (to..=from)
            .rev()
            .find(|&c| layer_blocks(c))
            .map(|c| (c + 1) as f32 * s + SKIN_M + feet_above_min)
    }
}

/// Systems of the controller.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ControllerSystems {
    /// `FixedUpdate`: moves the players, then records their eyes for
    /// interpolation.
    Move,
    /// `Update`: mouse look, then the camera follows the eye.
    Look,
}

/// The character controller.
pub struct ControllerPlugin;

impl Plugin for ControllerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ControllerConfig>()
            .init_resource::<StateClock>()
            .add_message::<ToHost>()
            .insert_resource(Time::<Fixed>::from_hz(FIXED_HZ))
            .add_systems(
                FixedUpdate,
                (move_players, record_eyes, report_state)
                    .chain()
                    .in_set(ControllerSystems::Move),
            )
            .add_systems(
                Update,
                (look_players, follow_player)
                    .chain()
                    .in_set(ControllerSystems::Look),
            );
    }
}

fn fly_cam_on(fly: Option<Res<FlyCam>>) -> bool {
    fly.is_some_and(|f| f.enabled)
}

fn move_players(
    replica: Option<Res<Replica>>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    fly: Option<Res<FlyCam>>,
    config: Res<ControllerConfig>,
    time: Res<Time>,
    mut players: Query<&mut Player>,
) {
    let Some(replica) = replica else { return };
    if fly_cam_on(fly) {
        return;
    }
    let input = keys.map_or(MoveInput::default(), |k| MoveInput::from_keys(&k));
    let dt = time.delta_secs();
    for mut player in &mut players {
        step(replica.world(), &mut player, input, &config, dt);
    }
}

/// Pushes every player's eye after the fixed step into its
/// [`InterpolatedEye`], adding one on the first step.
fn record_eyes(
    mut commands: Commands,
    mut players: Query<(Entity, &Player, Option<&mut InterpolatedEye>)>,
) {
    for (entity, player, history) in &mut players {
        match history {
            Some(mut history) => history.push(player.eye()),
            None => {
                commands
                    .entity(entity)
                    .insert(InterpolatedEye::at(player.eye()));
            }
        }
    }
}

/// Fixed steps the player has moved since joining, for [`report_state`].
#[derive(Resource, Debug, Default)]
struct StateClock {
    steps: u64,
}

/// Every `FIXED_HZ / PLAYER_STATE_HZ` fixed steps the player moves (from
/// the first), sends its feet, yaw and pitch to the host.
fn report_state(
    replica: Option<Res<Replica>>,
    fly: Option<Res<FlyCam>>,
    mut clock: ResMut<StateClock>,
    players: Query<&Player>,
    mut out: MessageWriter<ToHost>,
) {
    if replica.is_none() || fly_cam_on(fly) {
        return;
    }
    let Some(player) = players.iter().next() else {
        return;
    };
    let every = (FIXED_HZ / PLAYER_STATE_HZ).round().max(1.0) as u64;
    if clock.steps.is_multiple_of(every) {
        out.write(ToHost(ClientMsg::PlayerState {
            pos: player.feet,
            yaw: player.yaw,
            pitch: player.pitch,
        }));
    }
    clock.steps += 1;
}

fn look_players(
    mouse: Option<Res<AccumulatedMouseMotion>>,
    fly: Option<Res<FlyCam>>,
    grab: Option<Res<CursorGrab>>,
    config: Res<ControllerConfig>,
    mut players: Query<&mut Player>,
) {
    let motion = mouse.map_or(Vec2::ZERO, |m| m.delta);
    if motion == Vec2::ZERO || fly_cam_on(fly) || !look_allowed(grab.as_deref()) {
        return;
    }
    let max = MAX_PITCH_DEG.to_radians();
    for mut player in &mut players {
        player.yaw -= motion.x * config.sensitivity_rad;
        player.pitch = (player.pitch - motion.y * config.sensitivity_rad).clamp(-max, max);
    }
}

/// Puts the camera at the player's eye, interpolated at the fixed clock's
/// overstep fraction. Until the first fixed step has run, at the eye. The
/// rotation is not interpolated: mouse look applies every frame.
fn follow_player(
    fly: Option<Res<FlyCam>>,
    fixed: Res<Time<Fixed>>,
    players: Query<(&Player, Option<&InterpolatedEye>)>,
    mut cameras: Query<&mut Transform, With<GameCamera>>,
) {
    if fly_cam_on(fly) {
        return;
    }
    let Some((player, history)) = players.iter().next() else {
        return;
    };
    let eye = history.map_or(player.eye(), |h| h.render_pos(fixed.overstep_fraction()));
    for mut transform in &mut cameras {
        transform.translation = eye;
        transform.rotation = player.rotation();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;
    use proptest::prelude::*;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::CellAabb;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::layers::LayerId;
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::shape::Shape;
    use std::sync::Arc;
    use std::time::Duration;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const DT: f32 = 1.0 / 60.0;
    const S: f32 = CELL_SIZE_M;

    fn materials() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"))
    }

    /// A world of `size` cells per axis from the origin, all air and every
    /// chunk resident.
    fn air_world(size: i32) -> World {
        let bounds = CellAabb::new(
            CellPos::new(0, 0, 0),
            CellPos::new(size - 1, size - 1, size - 1),
        );
        let mut world = World::new(materials(), bounds);
        for pos in bounds.chunks() {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        world
    }

    fn stone(world: &World) -> MaterialId {
        world.materials().id("stone").unwrap()
    }

    /// Fills the inclusive box of cells with `id`.
    fn fill(world: &mut World, min: (i32, i32, i32), max: (i32, i32, i32), id: MaterialId) {
        let cmd = EditCommand {
            id: CommandId::default(),
            issuer: PlayerId(0),
            op: EditOp::Set(id),
            shape: Shape::Box {
                min: CellPos::new(min.0, min.1, min.2),
                max: CellPos::new(max.0, max.1, max.2),
            },
        };
        assert_eq!(
            world.apply(&cmd).status,
            EditStatus::Applied,
            "{min:?}..{max:?}"
        );
    }

    /// A 64³-cell world with a stone floor in cell rows y = 0..=3, so the
    /// floor top is at 2.0 m.
    fn floor_world() -> World {
        let mut world = air_world(64);
        let id = stone(&world);
        fill(&mut world, (0, 0, 0), (63, 3, 63), id);
        world
    }

    /// Every blocking cell the player's box shares volume with.
    fn penetrated(world: &World, player: &Player) -> Vec<CellPos> {
        let b = player.aabb();
        let lo = (b.min / S).floor().as_ivec3() - IVec3::ONE;
        let hi = (b.max / S).floor().as_ivec3() + IVec3::ONE;
        let mut hits = Vec::new();
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let cell = CellPos::new(x, y, z);
                    if blocks(world, cell) && b.overlaps_cell(cell) {
                        hits.push(cell);
                    }
                }
            }
        }
        hits
    }

    fn run(world: &World, player: &mut Player, input: MoveInput, steps: u32) {
        let config = ControllerConfig::default();
        for i in 0..steps {
            step(world, player, input, &config, DT);
            let hits = penetrated(world, player);
            assert!(
                hits.is_empty(),
                "step {i}: player at {:?} overlaps {hits:?}",
                player.feet
            );
        }
    }

    const FORWARD: MoveInput = MoveInput {
        forward: 1.0,
        right: 0.0,
        sprint: false,
        jump: false,
        crouch: false,
    };

    #[test]
    fn stands_on_floor() {
        let world = floor_world();
        let mut player = Player::at(Vec3::new(16.0, 5.0, 16.0));
        run(&world, &mut player, MoveInput::default(), 10);
        assert!(!player.grounded, "in the air after 10 steps");
        assert!(
            player.feet.y < 5.0,
            "gravity pulls it down: {}",
            player.feet.y
        );
        assert!(player.velocity.y < 0.0);

        run(&world, &mut player, MoveInput::default(), 110);
        assert!(player.grounded, "lands on the floor");
        assert!(
            (player.feet.y - 2.0).abs() <= 2.0 * SKIN_M,
            "feet rest on the floor top at 2.0 m: {}",
            player.feet.y
        );
        assert_eq!(player.velocity.y, 0.0, "landing stops the fall");
        assert_eq!(player.feet.x, 16.0, "no input: no drift");
        assert_eq!(player.feet.z, 16.0);

        // Standing keeps it exactly where it is, grounded, frame after frame.
        let rest = player.feet;
        run(&world, &mut player, MoveInput::default(), 120);
        assert_eq!(player.feet, rest);
        assert!(player.grounded);

        // Walking on the floor stays on it and at walk speed.
        let config = ControllerConfig::default();
        player.yaw = 0.0;
        run(&world, &mut player, FORWARD, 60);
        assert!(player.grounded);
        assert!((player.feet.y - rest.y).abs() < 1e-6);
        let walked = rest.z - player.feet.z;
        assert!(
            (walked - config.walk_speed_mps).abs() < 1e-3,
            "1 s forward along -Z walks {walked} m"
        );

        // Sprint is faster.
        let start = player.feet;
        run(
            &world,
            &mut player,
            MoveInput {
                sprint: true,
                ..FORWARD
            },
            60,
        );
        let sprinted = start.z - player.feet.z;
        assert!(
            (sprinted - config.sprint_speed_mps).abs() < 1e-3,
            "sprinted {sprinted} m"
        );

        // Stepping off an edge falls.
        let mut world = world;
        fill(&mut world, (0, 3, 0), (63, 3, 63), MaterialId::AIR);
        run(&world, &mut player, MoveInput::default(), 120);
        assert!(player.grounded);
        assert!(
            (player.feet.y - 1.5).abs() <= 2.0 * SKIN_M,
            "falls one cell: {}",
            player.feet.y
        );
    }

    #[test]
    fn wall_stops_movement() {
        let mut world = floor_world();
        let id = stone(&world);
        // A wall whose -X face is at x = 10.0 m, 6 cells (3 m) tall.
        fill(&mut world, (20, 4, 0), (21, 9, 63), id);
        let mut player = Player::at(Vec3::new(8.0, 2.0, 16.0));
        // Yaw -90° looks along +X.
        player.yaw = -std::f32::consts::FRAC_PI_2;
        run(&world, &mut player, MoveInput::default(), 5);
        assert!(player.grounded);

        run(&world, &mut player, FORWARD, 120);
        let face = player.aabb().max.x;
        assert!(face <= 10.0, "never past the wall face: {face}");
        assert!(
            face >= 10.0 - 2.0 * SKIN_M,
            "stops at the wall face: {face}"
        );
        assert!(
            (player.feet.z - 16.0).abs() < 1e-4,
            "no sideways drift: {}",
            player.feet.z
        );
        assert!(player.grounded, "still on the floor");
        let stopped = player.feet;
        run(&world, &mut player, FORWARD, 30);
        assert!(
            (player.feet - stopped).length() < 1e-5,
            "pressing on stays put"
        );

        // A wall in -X, -Z and +Z directions too: every direction stops.
        fill(&mut world, (4, 4, 0), (5, 9, 63), id);
        fill(&mut world, (0, 4, 20), (63, 9, 21), id);
        fill(&mut world, (0, 4, 44), (63, 9, 45), id);
        for (yaw_deg, check) in [(90.0f32, 0usize), (0.0, 1), (180.0, 2)] {
            player.yaw = yaw_deg.to_radians();
            run(&world, &mut player, FORWARD, 240);
            let b = player.aabb();
            match check {
                0 => assert!(
                    (b.min.x - 3.0).abs() <= 2.0 * SKIN_M,
                    "-X wall: {}",
                    b.min.x
                ),
                1 => assert!(
                    (b.min.z - 11.0).abs() <= 2.0 * SKIN_M,
                    "-Z wall: {}",
                    b.min.z
                ),
                _ => assert!(
                    (b.max.z - 22.0).abs() <= 2.0 * SKIN_M,
                    "+Z wall: {}",
                    b.max.z
                ),
            }
        }

        // A ceiling stops a rise: launched upward under a low roof, the head
        // stops at the roof and the player falls back.
        let mut world = floor_world();
        fill(&mut world, (0, 8, 0), (63, 8, 63), id);
        let mut player = Player::at(Vec3::new(16.0, 2.0, 16.0));
        player.velocity.y = 30.0;
        run(&world, &mut player, MoveInput::default(), 1);
        run(&world, &mut player, MoveInput::default(), 10);
        assert!(player.aabb().max.y <= 4.0, "head below the roof at 4.0 m");
        run(&world, &mut player, MoveInput::default(), 60);
        assert!(player.grounded);
    }

    #[test]
    fn corner_slides() {
        let mut world = floor_world();
        let id = stone(&world);
        // A wall along Z at x = 10.0 m (its -X face).
        fill(&mut world, (20, 4, 0), (21, 9, 63), id);
        let mut player = Player::at(Vec3::new(9.0, 2.0, 30.0));
        // Yaw -45°: forward is (+X, -Z) at 45°, into the wall and along it.
        player.yaw = -std::f32::consts::FRAC_PI_4;
        run(&world, &mut player, MoveInput::default(), 5);
        let config = ControllerConfig::default();
        let along = config.walk_speed_mps * std::f32::consts::FRAC_1_SQRT_2;

        run(&world, &mut player, FORWARD, 30);
        let face = player.aabb().max.x;
        assert!(
            (10.0 - 2.0 * SKIN_M..=10.0).contains(&face),
            "against the wall: {face}"
        );
        // Slides along -Z at the along-wall component of the walk speed.
        let z0 = player.feet.z;
        run(&world, &mut player, FORWARD, 60);
        let slid = z0 - player.feet.z;
        assert!(
            (slid - along).abs() < 1e-3,
            "slid {slid} m along the wall in 1 s, expected {along}"
        );
        assert!(player.aabb().max.x <= 10.0);

        // Into an inside corner (a second wall across -Z at z = 20.0 m, its
        // +Z face at 22.0 m): both axes stop, nothing jitters through.
        fill(&mut world, (0, 4, 40), (63, 9, 43), id);
        run(&world, &mut player, FORWARD, 240);
        let b = player.aabb();
        assert!(
            (10.0 - 2.0 * SKIN_M..=10.0).contains(&b.max.x),
            "x at the wall: {}",
            b.max.x
        );
        assert!(
            (22.0..=22.0 + 2.0 * SKIN_M).contains(&b.min.z),
            "z at the wall: {}",
            b.min.z
        );
        let cornered = player.feet;
        run(&world, &mut player, FORWARD, 30);
        assert!(
            (player.feet - cornered).length() < 1e-5,
            "stays in the corner"
        );

        // An outside corner: a pillar's edge clipped diagonally only stops
        // the axis that hits it, and the player slides past the pillar.
        let mut world = floor_world();
        fill(&mut world, (20, 4, 20), (23, 9, 23), id); // x, z in [10, 12) m
        let mut player = Player::at(Vec3::new(9.5, 2.0, 12.2));
        player.yaw = -std::f32::consts::FRAC_PI_4; // +X and -Z
        run(&world, &mut player, MoveInput::default(), 5);
        run(&world, &mut player, FORWARD, 120);
        assert!(
            player.feet.x > 12.5,
            "slid past the pillar to x {}",
            player.feet.x
        );
        assert!(
            player.feet.z < 10.0,
            "and on along -Z to z {}",
            player.feet.z
        );
    }

    const JUMP: MoveInput = MoveInput {
        forward: 0.0,
        right: 0.0,
        sprint: false,
        jump: true,
        crouch: false,
    };

    #[test]
    fn jump_leaves_ground_and_lands() {
        // Space is the jump key.
        let mut keys = ButtonInput::<KeyCode>::default();
        assert!(!MoveInput::from_keys(&keys).jump);
        keys.press(KeyCode::Space);
        assert!(MoveInput::from_keys(&keys).jump);

        let world = floor_world();
        let config = ControllerConfig::default();
        let mut player = Player::at(Vec3::new(16.0, 2.0, 16.0));
        run(&world, &mut player, MoveInput::default(), 5);
        assert!(player.grounded);
        let rest = player.feet;

        // One step with jump held leaves the ground going up.
        run(&world, &mut player, JUMP, 1);
        assert!(!player.grounded, "airborne after the jump");
        assert!(player.feet.y > rest.y, "rises: {}", player.feet.y);
        assert!(player.velocity.y > 0.0);

        // Holding jump in the air adds nothing: gravity alone slows the rise.
        let mut apex = player.feet.y;
        for i in 0..10 {
            let v = player.velocity.y;
            run(&world, &mut player, JUMP, 1);
            assert!(!player.grounded, "air step {i}");
            assert!(
                (player.velocity.y - (v - config.gravity_mps2 * DT)).abs() < 1e-4,
                "air step {i}: no second jump, {} after {v}",
                player.velocity.y
            );
            apex = apex.max(player.feet.y);
        }

        // Released, it rises to v² / 2g and lands where it started.
        let mut airborne = 11;
        while !player.grounded {
            run(&world, &mut player, MoveInput::default(), 1);
            apex = apex.max(player.feet.y);
            airborne += 1;
            assert!(airborne < 200, "never landed: {:?}", player.feet);
        }
        let height = apex - rest.y;
        let expected = config.jump_speed_mps.powi(2) / (2.0 * config.gravity_mps2);
        assert!(
            (height - expected).abs() < config.jump_speed_mps * DT,
            "jump height {height} m, expected about {expected}"
        );
        assert!(height > 1.0, "a jump clears two cells: {height}");
        let flight = 2.0 * config.jump_speed_mps / config.gravity_mps2 / DT;
        assert!(
            (airborne as f32 - flight).abs() <= 2.0,
            "{airborne} steps in the air, expected about {flight}"
        );
        assert!(
            (player.feet.y - rest.y).abs() <= 2.0 * SKIN_M,
            "lands on the floor: {}",
            player.feet.y
        );
        assert_eq!(player.velocity.y, 0.0);
        assert_eq!((player.feet.x, player.feet.z), (rest.x, rest.z));

        // Not holding jump, it stays down.
        run(&world, &mut player, MoveInput::default(), 30);
        assert!(player.grounded && (player.feet.y - rest.y).abs() <= 2.0 * SKIN_M);

        // A two-cell (1 m) ledge is out of step-up's reach but a running
        // jump gets on top of it.
        let mut world = floor_world();
        let id = stone(&world);
        fill(&mut world, (20, 4, 0), (63, 5, 63), id); // top 3.0 m, face x = 10 m
        let mut player = Player::at(Vec3::new(8.0, 2.0, 16.0));
        player.yaw = -std::f32::consts::FRAC_PI_2; // +X
        run(&world, &mut player, MoveInput::default(), 5);
        run(
            &world,
            &mut player,
            MoveInput {
                jump: true,
                ..FORWARD
            },
            1,
        );
        run(&world, &mut player, FORWARD, 120);
        assert!(player.grounded);
        assert!(
            (player.feet.y - 3.0).abs() <= 2.0 * SKIN_M && player.feet.x > 10.3,
            "on the ledge: {:?}",
            player.feet
        );
    }

    /// Walks `player` with `input` for `steps` steps, checking that it never
    /// penetrates, never moves upward on its own (no jump), and that its eye
    /// never rises faster than a cell per [`STEP_SMOOTH_S`]. Returns the steps
    /// at which the feet rose.
    fn walk_up(world: &World, player: &mut Player, input: MoveInput, steps: u32) -> Vec<u32> {
        let max_eye_rise = CELL_SIZE_M / STEP_SMOOTH_S * DT + 1e-4;
        let mut rose = Vec::new();
        for i in 0..steps {
            let (feet, eye) = (player.feet, player.eye());
            run(world, player, input, 1);
            assert!(
                player.velocity.y <= 0.0,
                "step {i}: no upward velocity without jumping"
            );
            if player.feet.y > feet.y + 1e-4 {
                rose.push(i);
                assert!(player.grounded, "step {i}: lands on the step");
                assert!(
                    (player.feet.y - feet.y - CELL_SIZE_M).abs() <= 2.0 * SKIN_M,
                    "step {i}: rises one cell, {} -> {}",
                    feet.y,
                    player.feet.y
                );
            }
            let eye_rise = player.eye().y - eye.y;
            assert!(
                eye_rise <= max_eye_rise,
                "step {i}: the eye jumps {eye_rise} m"
            );
        }
        rose
    }

    #[test]
    fn steps_up_one_cell() {
        let mut world = floor_world();
        let id = stone(&world);
        // A one-cell ledge from x = 10 m on: top 2.5 m.
        fill(&mut world, (20, 4, 0), (63, 4, 63), id);
        let mut player = Player::at(Vec3::new(8.0, 2.0, 16.0));
        player.yaw = -std::f32::consts::FRAC_PI_2; // +X
        run(&world, &mut player, MoveInput::default(), 5);
        assert!(player.grounded);

        let rose = walk_up(&world, &mut player, FORWARD, 60);
        assert_eq!(rose.len(), 1, "one step-up: {rose:?}");
        assert!(player.grounded);
        assert!(
            (player.feet.y - 2.5).abs() <= 2.0 * SKIN_M,
            "on the ledge: {}",
            player.feet.y
        );
        let config = ControllerConfig::default();
        assert!(
            player.feet.x > 8.0 + 0.9 * config.walk_speed_mps,
            "the step barely slows the walk: x {}",
            player.feet.x
        );
        assert!((player.feet.z - 16.0).abs() < 1e-4, "no drift");

        // The view catches up with the body within STEP_SMOOTH_S.
        let mut player = Player::at(Vec3::new(8.0, 2.0, 16.0));
        player.yaw = -std::f32::consts::FRAC_PI_2;
        run(&world, &mut player, MoveInput::default(), 5);
        let mut i = 0;
        while player.feet.y < 2.25 {
            run(&world, &mut player, FORWARD, 1);
            i += 1;
            assert!(i < 60, "never stepped up");
        }
        assert!(
            player.eye().y < player.feet.y + EYE_HEIGHT_M - 0.4,
            "the eye starts low: {} vs feet {}",
            player.eye().y,
            player.feet.y
        );
        let smooth_steps = (STEP_SMOOTH_S / DT).round() as u32;
        run(&world, &mut player, FORWARD, smooth_steps - 1);
        assert!(
            player.eye().y < player.feet.y + EYE_HEIGHT_M,
            "still smoothing"
        );
        run(&world, &mut player, FORWARD, 2);
        assert_eq!(player.step_offset_m, 0.0, "smoothed in about 0.1 s");
        assert_eq!(player.eye(), player.feet + Vec3::Y * EYE_HEIGHT_M);

        // A staircase of one-cell steps, each 1 m deep, along -Z.
        let mut world = floor_world();
        for k in 0..4 {
            let z_max = 23 - 2 * k; // faces at z = 12, 11, 10, 9 m
            fill(&mut world, (0, 4 + k, 0), (63, 4 + k, z_max), id);
        }
        let mut player = Player::at(Vec3::new(16.0, 2.0, 14.0));
        run(&world, &mut player, MoveInput::default(), 5);
        let rose = walk_up(&world, &mut player, FORWARD, 120);
        assert_eq!(rose.len(), 4, "four step-ups: {rose:?}");
        assert!(
            (player.feet.y - 4.0).abs() <= 2.0 * SKIN_M,
            "at the top: {}",
            player.feet.y
        );

        // Sprinting diagonally into a step corner climbs it once.
        let mut world = floor_world();
        fill(&mut world, (0, 4, 0), (23, 4, 23), id); // x, z < 12 m
        let mut player = Player::at(Vec3::new(14.0, 2.0, 14.0));
        player.yaw = std::f32::consts::FRAC_PI_4; // -X and -Z
        run(&world, &mut player, MoveInput::default(), 5);
        let rose = walk_up(
            &world,
            &mut player,
            MoveInput {
                sprint: true,
                ..FORWARD
            },
            60,
        );
        assert_eq!(rose.len(), 1, "{rose:?}");
        assert!((player.feet.y - 2.5).abs() <= 2.0 * SKIN_M);
        assert!(
            player.feet.x < 11.0 && player.feet.z < 11.0,
            "{:?}",
            player.feet
        );
    }

    #[test]
    fn two_cell_wall_blocks() {
        let id = stone(&floor_world());
        // A two-cell wall (top 3.0 m), a one-cell ledge under a ceiling that
        // leaves no headroom over the player, and one with a ceiling only
        // over the ledge. None of them can be climbed by walking.
        let ledge: CellBox = ((20, 4, 0), (63, 4, 63));
        let cases: [(&str, Vec<CellBox>); 3] = [
            ("two-cell wall", vec![((20, 4, 0), (63, 5, 63))]),
            // 4.0 m over x < 10 m only: the head is at 3.7.
            ("no headroom", vec![ledge, ((0, 8, 0), (19, 8, 63))]),
            (
                "ceiling over the ledge",
                vec![ledge, ((20, 8, 0), (63, 8, 63))],
            ),
        ];
        for (name, boxes) in cases {
            let mut world = floor_world();
            for (min, max) in boxes {
                fill(&mut world, min, max, id);
            }
            let mut player = Player::at(Vec3::new(8.0, 2.0, 16.0));
            player.yaw = -std::f32::consts::FRAC_PI_2; // +X
            run(&world, &mut player, MoveInput::default(), 5);
            for sprint in [false, true] {
                let input = MoveInput { sprint, ..FORWARD };
                let rose = walk_up(&world, &mut player, input, 90);
                assert!(rose.is_empty(), "{name}: climbed at steps {rose:?}");
                let face = player.aabb().max.x;
                assert!(
                    (10.0 - 2.0 * SKIN_M..=10.0).contains(&face),
                    "{name}: stopped at the face, {face}"
                );
                assert!(player.grounded, "{name}");
                assert!((player.feet.y - 2.0).abs() <= 2.0 * SKIN_M, "{name}");
                assert_eq!(player.step_offset_m, 0.0, "{name}");
            }
        }

        // In the air there is no step-up: falling beside a one-cell ledge
        // while walking into it lands below the ledge, not on it.
        let mut world = floor_world();
        fill(&mut world, (20, 4, 0), (63, 4, 63), id);
        let mut player = Player::at(Vec3::new(9.69, 2.4, 16.0));
        player.yaw = -std::f32::consts::FRAC_PI_2;
        let mut i = 0;
        while !player.grounded {
            run(&world, &mut player, FORWARD, 1);
            i += 1;
            assert!(i < 60, "never landed");
        }
        assert!(
            (player.feet.y - 2.0).abs() <= 2.0 * SKIN_M,
            "lands below the ledge: {:?}",
            player.feet
        );
    }

    #[test]
    fn walks_through_passable_plant() {
        // SAD-P2 §5: the controller treats passable solids (plants) as air.
        let t = materials();
        let plants: Vec<MaterialId> = t
            .iter()
            .filter(|d| d.is_solid() && d.passable)
            .map(|d| d.id)
            .collect();
        assert!(!plants.is_empty(), "the shipped table has passable plants");
        for &plant in &plants {
            let name = &t.get(plant).unwrap().key;
            // A two-cell plant wall (top 3.0 m) where a stone one would stop
            // the player at x = 10.0 m (see `two_cell_wall_blocks`).
            let mut world = floor_world();
            fill(&mut world, (20, 4, 0), (21, 5, 63), plant);
            assert!(!blocks(&world, CellPos::new(20, 4, 16)), "{name}");
            assert!(world.is_solid(CellPos::new(20, 4, 16)), "{name} is solid");
            let mut player = Player::at(Vec3::new(8.0, 2.0, 16.0));
            player.yaw = -std::f32::consts::FRAC_PI_2; // +X
            run(&world, &mut player, MoveInput::default(), 5);
            let rose = walk_up(&world, &mut player, FORWARD, 120);
            assert!(rose.is_empty(), "{name}: climbed at steps {rose:?}");
            assert!(
                player.aabb().min.x > 11.0,
                "{name}: walked through the plants, {}",
                player.aabb().min.x
            );
            assert!(player.grounded, "{name}");
            assert!((player.feet.y - 2.0).abs() <= 2.0 * SKIN_M, "{name}");

            // Standing inside a plant is not embedded: no push-out.
            let mut inside = Player::at(Vec3::new(10.25, 2.0, 16.25));
            assert!(!push_out(&world, &mut inside), "{name}");
            assert_eq!(inside.feet, Vec3::new(10.25, 2.0, 16.25));

            // A plant layer over a pit is no floor: the player falls through
            // it onto the stone below.
            let mut world = floor_world();
            fill(&mut world, (0, 3, 0), (63, 3, 63), plant);
            let mut player = Player::at(Vec3::new(16.0, 3.0, 16.0));
            run(&world, &mut player, MoveInput::default(), 120);
            assert!(player.grounded, "{name}");
            assert!(
                (player.feet.y - 1.5).abs() <= 2.0 * SKIN_M,
                "{name}: fell through the plant layer, {}",
                player.feet.y
            );
        }
    }

    #[test]
    fn walls_at_the_world_edge_and_unloaded_chunks_block() {
        // A floor-only world: the bounds stop the player at x = 0.
        let world = floor_world();
        let mut player = Player::at(Vec3::new(2.0, 2.0, 16.0));
        player.yaw = std::f32::consts::FRAC_PI_2; // -X
        run(&world, &mut player, FORWARD, 120);
        assert!(
            (player.aabb().min.x - 0.0).abs() <= 2.0 * SKIN_M,
            "edge: {}",
            player.aabb().min.x
        );

        // Missing chunk under the player: it does not fall into it.
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 63));
        let mut world = World::new(materials(), bounds);
        for pos in bounds.chunks().filter(|p| p.0.y == 1) {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let mut player = Player::at(Vec3::new(8.0, 20.0, 8.0));
        run(&world, &mut player, MoveInput::default(), 120);
        assert!(player.grounded);
        assert!(
            (player.feet.y - 16.0).abs() <= 2.0 * SKIN_M,
            "on the unloaded chunk: {}",
            player.feet.y
        );

        // Above the top of the world is open: a player there falls into it.
        let world = floor_world();
        let mut player = Player::at(Vec3::new(16.0, 33.0, 16.0));
        assert!(!blocks(&world, CellPos::new(32, 64, 32)));
        run(&world, &mut player, MoveInput::default(), 240);
        assert!(player.grounded && (player.feet.y - 2.0).abs() <= 2.0 * SKIN_M);
    }

    /// Whether the box's footprint lies inside cell column (`x`, `z`).
    fn inside_column(player: &Player, x: i32, z: i32) -> bool {
        let b = player.aabb();
        let (lo_x, lo_z) = (x as f32 * S, z as f32 * S);
        b.min.x >= lo_x && b.max.x <= lo_x + S && b.min.z >= lo_z && b.max.z <= lo_z + S
    }

    #[test]
    fn player_drops_into_one_cell_hole() {
        // The box is narrower than a cell (owner change F094), so a player
        // over a one-cell hole falls into it instead of resting on the rims.
        // Digging straight down: the player stands in the middle of cell
        // column (32, 32); the floor cell under it goes, then the next one.
        let mut world = floor_world();
        let mut player = Player::at(Vec3::new(16.25, 2.0, 16.25));
        run(&world, &mut player, MoveInput::default(), 5);
        assert!(player.grounded);
        for (cell_y, floor_m) in [(3, 1.5), (2, 1.0)] {
            fill(
                &mut world,
                (32, cell_y, 32),
                (32, cell_y, 32),
                MaterialId::AIR,
            );
            run(&world, &mut player, MoveInput::default(), 60);
            assert!(player.grounded, "lands in the hole: {:?}", player.feet);
            assert!(
                (player.feet.y - floor_m).abs() <= 2.0 * SKIN_M,
                "feet on the hole floor at {floor_m} m: {}",
                player.feet.y
            );
            assert!(inside_column(&player, 32, 32), "{:?}", player.feet);
        }

        // Dropped from above, a player over an open one-cell hole falls to
        // its bottom.
        let mut world = floor_world();
        fill(&mut world, (32, 2, 32), (32, 3, 32), MaterialId::AIR);
        let mut player = Player::at(Vec3::new(16.25, 3.0, 16.25));
        run(&world, &mut player, MoveInput::default(), 120);
        assert!(player.grounded);
        assert!(
            (player.feet.y - 1.0).abs() <= 2.0 * SKIN_M,
            "at the bottom of the hole: {}",
            player.feet.y
        );
    }

    #[test]
    fn player_fits_one_cell_shaft() {
        // A stone block with its top at 12.0 m, a one-cell shaft down its
        // cell column (32, 32) to a floor at 2.0 m, and a one-cell-wide,
        // four-cell-tall corridor from the shaft's bottom along -Z to a
        // face at z = 10.0 m.
        let mut world = air_world(64);
        let id = stone(&world);
        fill(&mut world, (24, 0, 16), (40, 23, 40), id);
        fill(&mut world, (32, 4, 32), (32, 23, 32), MaterialId::AIR);
        fill(&mut world, (32, 4, 20), (32, 7, 31), MaterialId::AIR);

        // Dropped at the shaft's mouth, the player falls all the way down,
        // never touching the walls.
        let mut player = Player::at(Vec3::new(16.25, 12.5, 16.25));
        run(&world, &mut player, MoveInput::default(), 240);
        assert!(player.grounded);
        assert!(
            (player.feet.y - 2.0).abs() <= 2.0 * SKIN_M,
            "at the shaft's bottom: {:?}",
            player.feet
        );
        assert!(inside_column(&player, 32, 32), "{:?}", player.feet);

        // Walking back and strafing into the shaft's walls stops at them (forward
        // is the corridor); a jump rises inside the shaft and lands back on
        // its floor.
        for (forward, right) in [(-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            let input = MoveInput {
                forward,
                right,
                ..MoveInput::default()
            };
            run(&world, &mut player, input, 30);
            assert!(
                inside_column(&player, 32, 32),
                "{input:?}: {:?}",
                player.feet
            );
            assert!(player.grounded);
        }
        let floor = player.feet.y;
        run(&world, &mut player, JUMP, 1);
        assert!(!player.grounded && player.feet.y > floor);
        run(&world, &mut player, MoveInput::default(), 120);
        assert!(player.grounded && (player.feet.y - floor).abs() <= 2.0 * SKIN_M);
        assert!(inside_column(&player, 32, 32), "{:?}", player.feet);

        // Forward (-Z) the corridor leads out of the shaft to its end face.
        run(&world, &mut player, FORWARD, 180);
        let b = player.aabb();
        assert!(
            (b.min.z - 10.0).abs() <= 2.0 * SKIN_M,
            "walked to the corridor's end: {:?}",
            player.feet
        );
        assert!(
            b.min.x >= 16.0 && b.max.x <= 16.5,
            "stays in the corridor: {:?}",
            player.feet
        );
        assert!(player.grounded && (player.feet.y - 2.0).abs() <= 2.0 * SKIN_M);
    }

    const CROUCH: MoveInput = MoveInput {
        crouch: true,
        ..FORWARD
    };

    /// [`floor_world`] with a stone ceiling over cell rows z <= 24 (z < 12.5
    /// m) whose underside is three cells (1.5 m) above the floor.
    fn tunnel_world() -> World {
        let mut world = floor_world();
        let id = stone(&world);
        fill(&mut world, (0, 7, 0), (63, 9, 24), id);
        world
    }

    #[test]
    fn crouching_fits_a_three_cell_tunnel_and_stands_only_with_headroom() {
        let world = tunnel_world();
        let mut player = Player::at(Vec3::new(16.0, 2.0, 16.0));
        // Standing, the ceiling stops the box at the mouth.
        run(&world, &mut player, FORWARD, 120);
        assert!(player.feet.z > 12.5, "standing went in: {:?}", player.feet);
        assert!(!player.crouching);

        // Crouched, it goes in, slower, with the eye lowered.
        let start = player.feet.z;
        run(&world, &mut player, CROUCH, 60);
        let config = ControllerConfig::default();
        let walked = start - player.feet.z;
        assert!(
            (walked - config.crouch_speed_mps).abs() < 0.05,
            "crouched {walked} m in 1 s"
        );
        run(&world, &mut player, CROUCH, 60);
        assert!(player.feet.z < 12.0, "crouched in: {:?}", player.feet);
        assert!(player.crouching);
        assert!((player.crouch_offset_m - (CROUCH_EYE_HEIGHT_M - EYE_HEIGHT_M)).abs() < 1e-5);
        assert!((player.eye().y - (2.0 + CROUCH_EYE_HEIGHT_M)).abs() < 1e-3);

        // Released under the ceiling, it stays crouched.
        run(&world, &mut player, MoveInput::default(), 30);
        assert!(player.crouching, "stood up under the ceiling");

        // Out in the open it stands, and the eye comes back up.
        let back = MoveInput {
            forward: -1.0,
            ..FORWARD
        };
        run(&world, &mut player, back, 240);
        assert!(player.feet.z > 12.5 + PLAYER_WIDTH_M / 2.0);
        assert!(!player.crouching);
        assert_eq!(player.crouch_offset_m, 0.0);
        assert!(player.grounded);
    }

    #[test]
    fn crouching_does_not_walk_off_a_ledge() {
        // Floor only over cell rows z >= 20: its edge is at z = 10 m.
        let mut world = air_world(64);
        let id = stone(&world);
        fill(&mut world, (0, 0, 20), (63, 3, 63), id);
        let mut player = Player::at(Vec3::new(16.0, 2.0, 12.0));
        run(&world, &mut player, CROUCH, 240);
        assert!(player.grounded);
        assert!(
            (player.feet.y - 2.0).abs() < 0.01,
            "fell: {:?}",
            player.feet
        );
        assert!(
            player.feet.z > 10.0 - PLAYER_WIDTH_M / 2.0,
            "{:?}",
            player.feet
        );
        // Sliding along the edge still works.
        let along = MoveInput {
            forward: 1.0,
            right: 1.0,
            crouch: true,
            ..MoveInput::default()
        };
        let x = player.feet.x;
        run(&world, &mut player, along, 30);
        assert!(player.feet.x > x + 0.5, "slid {:?}", player.feet);
        assert!((player.feet.y - 2.0).abs() < 0.01);

        // Standing, it walks off.
        run(&world, &mut player, FORWARD, 60);
        assert!(player.feet.y < 1.0, "did not fall: {:?}", player.feet);
    }

    /// [`floor_world`] with water over cell rows y 4..=11 (2 m to 6 m) and a
    /// stone bank over cell rows z <= 10 (z < 5.5 m) up to y 12 (6.5 m).
    fn pool_world() -> World {
        let mut world = floor_world();
        let water = world.materials().id("water").unwrap();
        let id = stone(&world);
        fill(&mut world, (0, 4, 11), (63, 11, 63), water);
        fill(&mut world, (0, 4, 0), (63, 12, 10), id);
        world
    }

    #[test]
    fn swimming_sinks_slowly_rises_on_space_and_climbs_out() {
        let world = pool_world();
        let config = ControllerConfig::default();

        // Idle in deep water: sinks no faster than the swim cap.
        let mut player = Player::at(Vec3::new(16.0, 4.0, 16.0));
        run(&world, &mut player, MoveInput::default(), 30);
        assert!(player.swimming);
        assert!(player.velocity.y >= -config.swim_sink_mps - 1e-4);
        assert!(player.feet.y > 4.0 - config.swim_sink_mps * 0.5 - 0.01);

        // Space rises from the bottom to the surface.
        let up = MoveInput {
            jump: true,
            ..MoveInput::default()
        };
        let mut player = Player::at(Vec3::new(16.0, 2.0, 16.0));
        run(&world, &mut player, up, 120);
        assert!(player.feet.y > 4.0, "did not rise: {:?}", player.feet);

        // Space and forward (-Z) toward the bank: out of the water onto it.
        let out = MoveInput {
            jump: true,
            ..FORWARD
        };
        let mut player = Player::at(Vec3::new(16.0, 2.0, 8.0));
        run(&world, &mut player, out, 300);
        assert!(
            player.feet.z < 5.5 && player.feet.y > 6.49,
            "not on the bank: {:?}",
            player.feet
        );
        assert!(!player.swimming);
    }

    /// Sets the fill of every cell in the inclusive box.
    fn set_fill(world: &mut World, min: (i32, i32, i32), max: (i32, i32, i32), fill: u8) {
        for x in min.0..=max.0 {
            for y in min.1..=max.1 {
                for z in min.2..=max.2 {
                    let (pos, local) = CellPos::new(x, y, z).split();
                    let mut chunk = world.chunk(pos).unwrap().clone();
                    chunk.layers.set_u8(LayerId::Moisture, local, fill);
                    world.insert_chunk(pos, chunk).unwrap();
                }
            }
        }
    }

    /// A shallow pool (cell row 4, fill 40) with the host's rain film over
    /// it (row 5, fill 10) at the player's waist: the film pours into the
    /// pool, so the player wades, not swims; a full row 5 is swum in, and a
    /// half-full one only below its surface.
    #[test]
    fn rain_film_over_a_shallow_pool_is_not_swum_in() {
        let mut world = floor_world();
        let water = world.materials().id("water").unwrap();
        fill(&mut world, (24, 4, 24), (40, 5, 40), water);
        set_fill(&mut world, (24, 4, 24), (40, 4, 40), 40);
        set_fill(&mut world, (24, 5, 24), (40, 5, 40), 10);
        let feet = Vec3::new(16.0, 2.0, 16.0);
        let waist = CellPos::from_meters(feet + Vec3::Y * PLAYER_HEIGHT_M / 2.0);
        assert_eq!(waist.0.y, 5, "the waist is in the film's row");
        assert!(!in_liquid(&world, feet, PLAYER_HEIGHT_M));
        assert!(in_liquid(&world, feet, 0.0), "the feet are in the pool");
        let mut player = Player::at(feet);
        run(&world, &mut player, MoveInput::default(), 30);
        assert!(!player.swimming);
        assert!(player.grounded, "it stands on the floor");

        set_fill(&mut world, (24, 4, 24), (40, 5, 40), 255);
        assert!(in_liquid(&world, feet, PLAYER_HEIGHT_M));
        // Row 5 half full: its surface is at 2.75 m.
        set_fill(&mut world, (24, 5, 24), (40, 5, 40), 128);
        let at = |waist_m: f32| feet.with_y(waist_m - PLAYER_HEIGHT_M / 2.0);
        assert!(in_liquid(&world, at(2.6), PLAYER_HEIGHT_M));
        assert!(!in_liquid(&world, at(2.9), PLAYER_HEIGHT_M));
    }

    #[test]
    fn embedded_player_is_pushed_out() {
        let config = ControllerConfig::default();
        let still = MoveInput::default();

        // A Set over the head of a standing player (the M6 playtest's white
        // screen). The box spans cells x and z 31..=32, y 4..=7; stone goes
        // into rows 6..=7, around the eye.
        let mut world = floor_world();
        let id = stone(&world);
        let mut player = Player::at(Vec3::new(16.0, 2.0, 16.0));
        run(&world, &mut player, still, 5);
        assert!(player.grounded);
        fill(&mut world, (31, 6, 31), (32, 7, 32), id);
        assert!(!penetrated(&world, &player).is_empty());
        let expected = free_feet_near(&world, player.feet).expect("a free spot nearby");
        step(&world, &mut player, still, &config, DT);
        assert!(
            penetrated(&world, &player).is_empty(),
            "pushed out to {:?}",
            player.feet
        );
        assert_eq!(player.feet.xz(), expected.xz(), "the nearest free spot");
        assert!((player.feet.y - expected.y).abs() <= 2.0 * SKIN_M);
        assert!((player.feet.y - 2.0).abs() <= 2.0 * SKIN_M, "on the floor");
        assert!(player.grounded);
        // It walks on from there as usual.
        run(&world, &mut player, FORWARD, 30);

        // Sunk into the floor (a Set under the feet, or a spawn too low):
        // up is preferred over sideways at the same distance, so it ends
        // standing on the floor above the same cell.
        let world = floor_world();
        let mut player = Player::at(Vec3::new(16.25, 1.8, 16.25));
        player.velocity = Vec3::new(0.0, -10.0, 0.0);
        assert!(!penetrated(&world, &player).is_empty());
        step(&world, &mut player, still, &config, DT);
        assert!(penetrated(&world, &player).is_empty());
        assert_eq!(player.feet.xz(), Vec2::new(16.25, 16.25));
        assert!(
            (player.feet.y - 2.0).abs() <= 2.0 * SKIN_M,
            "{:?}",
            player.feet
        );
        assert!(player.grounded);
        assert_eq!(player.velocity, Vec3::ZERO, "the fall does not carry over");
        run(&world, &mut player, still, 10);

        // Only solid cells count: a box reaching into a chunk the replica
        // does not hold yet is not moved sideways or up to somewhere else.
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 63));
        let mut world = World::new(materials(), bounds);
        for pos in bounds.chunks().filter(|p| p.0.y == 1) {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let mut player = Player::at(Vec3::new(8.0, 15.9, 8.0));
        assert!(!penetrated(&world, &player).is_empty());
        step(&world, &mut player, still, &config, DT);
        assert_eq!(player.feet.xz(), Vec2::new(8.0, 8.0));
        assert!(player.feet.y <= 15.9, "{:?}", player.feet);

        // Nowhere free in reach (solid rock all around): it stays put.
        let mut world = air_world(32);
        let id = stone(&world);
        fill(&mut world, (0, 0, 0), (31, 31, 31), id);
        let mut player = Player::at(Vec3::new(8.0, 8.0, 8.0));
        assert_eq!(free_feet_near(&world, player.feet), None);
        step(&world, &mut player, still, &config, DT);
        assert_eq!(player.feet.xz(), Vec2::new(8.0, 8.0));
        assert!(
            (player.feet.y - 8.0).abs() <= 2.0 * SKIN_M,
            "{:?}",
            player.feet
        );

        // Through the plugin: a player spawned in rock is out once fixed
        // steps run (the first frame advances no time).
        let mut world = floor_world();
        let id = stone(&world);
        fill(&mut world, (28, 4, 28), (35, 7, 35), id);
        let (mut app, p, _) = controller_app(world, Player::at(Vec3::new(16.0, 2.0, 16.0)));
        app.update();
        app.update();
        let moved = app.world().get::<Player>(p).unwrap().clone();
        let world = app.world().resource::<Replica>().world();
        assert!(
            penetrated(world, &moved).is_empty(),
            "still in rock at {:?}",
            moved.feet
        );
    }

    /// Inclusive cell box (min, max).
    type CellBox = ((i32, i32, i32), (i32, i32, i32));
    /// Yaw, forward, right, sprint, launch upward and hold jump, frames held.
    type ScriptLeg = (f32, i8, i8, bool, bool, u8);

    /// Random walls and boxes in a one-chunk world, a free start, and a
    /// random input script.
    fn scenario() -> impl Strategy<Value = (Vec<CellBox>, (f32, f32, f32), Vec<ScriptLeg>)> {
        let block = ((0..32, 0..32, 0..32), (1..7, 1..7, 1..7))
            .prop_map(|((x, y, z), (w, h, d))| ((x, y, z), (x + w - 1, y + h - 1, z + d - 1)));
        let start = (0.3f32..15.7, 0.0f32..14.0, 0.3f32..15.7);
        let script = (
            -7.0f32..7.0,
            -1i8..=1,
            -1i8..=1,
            any::<bool>(),
            any::<bool>(),
            1u8..40,
        );
        (
            proptest::collection::vec(block, 0..24),
            start,
            proptest::collection::vec(script, 1..12),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(256)
        ))]

        #[test]
        fn never_penetrates_solid_proptest((blocks_, start, script) in scenario()) {
            let mut world = air_world(32);
            let id = stone(&world);
            fill(&mut world, (0, 0, 0), (31, 0, 31), id);
            for (min, max) in blocks_ {
                fill(&mut world, min, (max.0.min(31), max.1.min(31), max.2.min(31)), id);
            }
            let mut player = Player::at(Vec3::new(start.0, start.1, start.2));
            // Clear the start box so the player begins free.
            let b = player.aabb();
            let lo = (b.min / S).floor().as_ivec3().max(IVec3::ZERO);
            let hi = (b.max / S).floor().as_ivec3().min(IVec3::splat(31));
            fill(&mut world, (lo.x, lo.y, lo.z), (hi.x, hi.y, hi.z), MaterialId::AIR);
            prop_assert!(penetrated(&world, &player).is_empty());

            let config = ControllerConfig::default();
            for (yaw, forward, right, sprint, launch, frames) in script {
                player.yaw = yaw;
                if launch {
                    // A kick in any direction, faster than walking.
                    player.velocity.y = 12.0;
                }
                let input = MoveInput { forward: forward as f32, right: right as f32, sprint, jump: launch, crouch: false };
                for _ in 0..frames {
                    step(&world, &mut player, input, &config, DT);
                    prop_assert!(player.feet.is_finite());
                    let hits = penetrated(&world, &player);
                    prop_assert!(hits.is_empty(), "player at {:?} overlaps {:?}", player.feet, hits);
                }
            }
        }
    }

    // ---- Plugin ----

    fn controller_app(world: World, player: Player) -> (App, Entity, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 60.0,
            )))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<FlyCam>()
            .insert_resource(Replica::from_world(world))
            .add_plugins(ControllerPlugin);
        assert_eq!(
            app.world().resource::<Time<Fixed>>().timestep(),
            Duration::from_secs_f64(1.0 / 60.0),
            "the controller's fixed step is 60 Hz, one per 1/60 s frame"
        );
        let player = app.world_mut().spawn(player).id();
        let camera = app
            .world_mut()
            .spawn((GameCamera, Transform::default()))
            .id();
        app.update();
        (app, player, camera)
    }

    fn player(app: &App, e: Entity) -> Player {
        app.world().get::<Player>(e).unwrap().clone()
    }

    /// The `PlayerState`s written each frame, with the player's state after
    /// that frame's fixed step.
    #[derive(Resource, Default)]
    struct Reports(Vec<(u32, ClientMsg, Player)>);

    fn record_reports(
        mut frame: Local<u32>,
        mut out: MessageReader<ToHost>,
        players: Query<&Player>,
        mut reports: ResMut<Reports>,
    ) {
        *frame += 1;
        let player = players.single().unwrap().clone();
        for ToHost(msg) in out.read() {
            if matches!(msg, ClientMsg::PlayerState { .. }) {
                reports.0.push((*frame, msg.clone(), player.clone()));
            }
        }
    }

    #[test]
    fn player_state_sent_at_20_hz() {
        let (mut app, _, _) = controller_app(floor_world(), Player::at(Vec3::new(16.0, 4.0, 16.0)));
        app.init_resource::<Reports>()
            .add_systems(Last, record_reports);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        app.world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = Vec2::new(30.0, -20.0);
        // The turn applies in Update, after this frame's fixed step and its
        // report; from the next frame on the report carries it.
        app.update();
        app.world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = Vec2::ZERO;
        app.world_mut().resource_mut::<Reports>().0.clear();
        // 2 s at one fixed step per frame: 40 states, one every 3 steps.
        for _ in 0..120 {
            app.update();
        }
        let reports = &app.world().resource::<Reports>().0;
        assert_eq!(reports.len(), 40, "20 Hz for 2 s");
        assert!(
            reports.windows(2).all(|w| w[1].0 - w[0].0 == 3),
            "evenly spaced: {:?}",
            reports.iter().map(|r| r.0).collect::<Vec<_>>()
        );
        for (frame, msg, player) in reports {
            assert_eq!(
                *msg,
                ClientMsg::PlayerState {
                    pos: player.feet,
                    yaw: player.yaw,
                    pitch: player.pitch,
                },
                "frame {frame}: the feet and view after the step"
            );
        }
        let last = &reports.last().unwrap().2;
        assert!(last.yaw != 0.0 && last.pitch != 0.0);
        assert!(last.feet.z < 15.0, "walked: {:?}", last.feet);

        // Fly mode: the player is parked and nothing more is reported.
        app.world_mut().resource_mut::<FlyCam>().enabled = true;
        app.world_mut().resource_mut::<Reports>().0.clear();
        for _ in 0..30 {
            app.update();
        }
        assert!(app.world().resource::<Reports>().0.is_empty());
    }

    #[test]
    fn plugin_moves_player_in_fixed_update_and_camera_follows_eye() {
        let (mut app, p, camera) =
            controller_app(floor_world(), Player::at(Vec3::new(16.0, 4.0, 16.0)));
        for _ in 0..90 {
            app.update();
        }
        let standing = player(&app, p);
        assert!(
            standing.grounded,
            "falls onto the floor: {:?}",
            standing.feet
        );
        let t = *app.world().get::<Transform>(camera).unwrap();
        assert!(
            (t.translation - standing.eye()).length() < 1e-6,
            "camera at the eye"
        );
        assert!((t.translation.y - (2.0 + EYE_HEIGHT_M)).abs() < 0.01);

        // Mouse right turns right; W then walks along the new yaw.
        app.world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = Vec2::new(100.0, 40.0);
        app.update();
        app.world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = Vec2::ZERO;
        let turned = player(&app, p);
        let sens = ControllerConfig::default().sensitivity_rad;
        assert!((turned.yaw + 100.0 * sens).abs() < 1e-6);
        assert!((turned.pitch + 40.0 * sens).abs() < 1e-6);
        let t = *app.world().get::<Transform>(camera).unwrap();
        assert!(
            t.rotation.angle_between(turned.rotation()) < 1e-5,
            "camera looks where the player looks"
        );

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        let before = player(&app, p).feet;
        for _ in 0..30 {
            app.update();
        }
        let moved = player(&app, p).feet - before;
        let dir = (turned.rotation() * Vec3::NEG_Z).with_y(0.0).normalize();
        assert!(moved.length() > 1.0, "W walks: {moved:?}");
        assert!(
            moved.normalize().dot(dir) > 0.999,
            "along the yaw: {moved:?} vs {dir:?}"
        );

        // Fly mode: the controller stops moving the player and the camera.
        app.world_mut().resource_mut::<FlyCam>().enabled = true;
        let parked = player(&app, p);
        app.world_mut()
            .get_mut::<Transform>(camera)
            .unwrap()
            .translation = Vec3::new(1.0, 99.0, 1.0);
        for _ in 0..10 {
            app.update();
        }
        assert_eq!(
            player(&app, p).feet,
            parked.feet,
            "player frozen in fly mode"
        );
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().translation,
            Vec3::new(1.0, 99.0, 1.0)
        );
    }
}
