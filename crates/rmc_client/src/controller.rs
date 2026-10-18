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
//! diagonally into a wall slides along it. A box that already overlaps one
//! (a cell set inside the player) is not pushed out; it can move away.
//!
//! Which cells block is [`blocks`]: `Solid` phase cells, every cell outside
//! the world bounds below or beside the world, and cells in chunks the
//! replica does not hold yet. Above the top of the world is open sky.
//!
//! [`ControllerPlugin`] moves every [`Player`] in `FixedUpdate`, reading only
//! the [`Replica`] and `ButtonInput<KeyCode>`. Mouse look reads
//! `AccumulatedMouseMotion` in `Update` (it accumulates per frame, not per
//! fixed step), and the [`GameCamera`] then follows the player's eye. While
//! the fly camera is on, the controller neither moves, turns nor follows.
//!
//! - W / S walk forward and back along the yaw, D / A strafe, opposite keys
//!   cancel and diagonals are as fast as one axis. Shift sprints.
//! - The mouse turns: yaw right for motion right, pitch clamped to
//!   ±[`MAX_PITCH_DEG`].

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::world::World;

use crate::flycam::{FlyCam, MAX_PITCH_DEG};
use crate::render::GameCamera;
use crate::replica::Replica;

/// Width and depth of the player's box, in meters.
pub const PLAYER_WIDTH_M: f32 = 0.6;
/// Height of the player's box, in meters.
pub const PLAYER_HEIGHT_M: f32 = 1.7;
/// Eye height above the feet, in meters.
pub const EYE_HEIGHT_M: f32 = 1.55;
/// Gap left between the box and a blocking cell after a blocked move. Far
/// larger than `f32` rounding at world coordinates, so a stopped box never
/// rounds into the cell it stopped at.
pub const SKIN_M: f32 = 1e-3;

/// Rate of the fixed step the controller runs in (Bevy's default is 64 Hz).
pub const FIXED_HZ: f64 = 60.0;

/// Controller tuning.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct ControllerConfig {
    /// Walking speed, m/s.
    pub walk_speed_mps: f32,
    /// Speed while Shift is held, m/s.
    pub sprint_speed_mps: f32,
    /// Downward acceleration, m/s².
    pub gravity_mps2: f32,
    /// Fastest fall, m/s.
    pub max_fall_mps: f32,
    /// Radians of turn per unit of mouse motion.
    pub sensitivity_rad: f32,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            walk_speed_mps: 4.3,
            sprint_speed_mps: 6.5,
            gravity_mps2: 20.0,
            max_fall_mps: 40.0,
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
        }
    }

    /// The player's box.
    pub fn aabb(&self) -> Aabb {
        Aabb::of_feet(self.feet)
    }

    /// The eye position.
    pub fn eye(&self) -> Vec3 {
        self.feet + Vec3::Y * EYE_HEIGHT_M
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
        let half = PLAYER_WIDTH_M / 2.0;
        Self {
            min: feet - Vec3::new(half, 0.0, half),
            max: feet + Vec3::new(half, PLAYER_HEIGHT_M, half),
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
        }
    }
}

/// Whether `cell` stops the player: a `Solid` cell, a cell below or beside
/// the world bounds, or a cell in a chunk the world does not hold. Cells
/// above the top of the world are open.
pub fn blocks(world: &World, cell: CellPos) -> bool {
    let bounds = world.bounds();
    if cell.0.y > bounds.max.0.y {
        return false;
    }
    world.get_cell(cell).is_none() || world.is_solid(cell)
}

/// One fixed step of `dt` seconds: gravity, walk input, then the swept move.
///
/// The horizontal velocity is the walk input along the yaw at walk or
/// sprint speed (no acceleration); gravity adds to the vertical velocity up
/// to [`ControllerConfig::max_fall_mps`]. The box then moves along Y, X and
/// Z in turn. An axis that hits a blocking cell stops [`SKIN_M`] short of it
/// and loses its velocity; a blocked fall leaves the player grounded.
pub fn step(
    world: &World,
    player: &mut Player,
    input: MoveInput,
    config: &ControllerConfig,
    dt: f32,
) {
    let yaw = Quat::from_rotation_y(player.yaw);
    let wish = (yaw * Vec3::NEG_Z * input.forward + yaw * Vec3::X * input.right)
        .with_y(0.0)
        .clamp_length_max(1.0);
    let speed = if input.sprint {
        config.sprint_speed_mps
    } else {
        config.walk_speed_mps
    };
    player.velocity.x = wish.x * speed;
    player.velocity.z = wish.z * speed;
    player.velocity.y = (player.velocity.y - config.gravity_mps2 * dt).max(-config.max_fall_mps);

    let delta = player.velocity * dt;
    player.grounded = false;
    for axis in [1, 0, 2] {
        let d = delta[axis];
        if d == 0.0 {
            continue;
        }
        match sweep(world, player.aabb(), axis, d) {
            Some(stop) => {
                player.feet[axis] = stop;
                player.velocity[axis] = 0.0;
                if axis == 1 && d < 0.0 {
                    player.grounded = true;
                }
            }
            None => player.feet[axis] += d,
        }
    }
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
    /// `FixedUpdate`: moves the players.
    Move,
    /// `Update`: mouse look, then the camera follows the eye.
    Look,
}

/// The character controller.
pub struct ControllerPlugin;

impl Plugin for ControllerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ControllerConfig>()
            .insert_resource(Time::<Fixed>::from_hz(FIXED_HZ))
            .add_systems(FixedUpdate, move_players.in_set(ControllerSystems::Move))
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

fn look_players(
    mouse: Option<Res<AccumulatedMouseMotion>>,
    fly: Option<Res<FlyCam>>,
    config: Res<ControllerConfig>,
    mut players: Query<&mut Player>,
) {
    let motion = mouse.map_or(Vec2::ZERO, |m| m.delta);
    if motion == Vec2::ZERO || fly_cam_on(fly) {
        return;
    }
    let max = MAX_PITCH_DEG.to_radians();
    for mut player in &mut players {
        player.yaw -= motion.x * config.sensitivity_rad;
        player.pitch = (player.pitch - motion.y * config.sensitivity_rad).clamp(-max, max);
    }
}

fn follow_player(
    fly: Option<Res<FlyCam>>,
    players: Query<&Player>,
    mut cameras: Query<&mut Transform, With<GameCamera>>,
) {
    if fly_cam_on(fly) {
        return;
    }
    let Some(player) = players.iter().next() else {
        return;
    };
    for mut transform in &mut cameras {
        transform.translation = player.eye();
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

    /// Inclusive cell box (min, max).
    type CellBox = ((i32, i32, i32), (i32, i32, i32));
    /// Yaw, forward, right, sprint, launch upward, frames held.
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
                let input = MoveInput { forward: forward as f32, right: right as f32, sprint };
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
