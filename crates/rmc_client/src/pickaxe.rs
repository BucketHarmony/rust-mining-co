//! Raycast targeting and the pickaxe (SAD §11.5).
//!
//! The player looks at the first solid cell along the view ray from the
//! eye, within the pickaxe's reach ([`target`]). [`PickaxePlugin`] keeps
//! that hit in [`PickTarget`] every frame for the HUD, and swings while the
//! left mouse button is held: each swing writes one
//! [`ToHost`]`(Edit { Damage(damage), shape })` over the profile's
//! [`ToolShape`](rmc_world::ToolShape) at the hit cell, then waits the
//! profile's interval before the next one.
//!
//! The profile comes from `assets/tools.ron` in the [`Pickaxe`] resource;
//! without it, or before `Welcome` (no [`Replica`]), the pickaxe does
//! nothing. The client only asks: the host decides what breaks, and the
//! replica changes when its `Applied` result comes back.
//!
//! - Swings run in `FixedUpdate`, after the controller has moved the
//!   player, reading the held button (not presses), so a swing happens on
//!   the first fixed step the button is down and the interval is counted
//!   in fixed-step time.
//! - A swing needs a target. Holding the button at the sky or at a cell
//!   beyond reach sends nothing and does not restart the interval.
//! - Releasing the button does not reset the interval.
//! - While the fly camera is on, the pickaxe does not swing.

use std::time::Duration;

use bevy::prelude::*;
use rmc_protocol::ClientMsg;
use rmc_world::edit::{EditCommand, EditOp};
use rmc_world::world::World;
use rmc_world::{Hit, ToolProfile, raycast};

use crate::controller::{ControllerSystems, Player};
use crate::flycam::FlyCam;
use crate::net::{CommandSeq, ToHost};
use crate::replica::Replica;

/// The button that swings.
pub const SWING_BUTTON: MouseButton = MouseButton::Left;

/// Clock rounding allowed when checking the interval: a whole number of
/// fixed steps that adds up to the interval may fall a few nanoseconds short.
pub const SWING_SLACK: Duration = Duration::from_millis(1);

/// The pickaxe profile in use (`tools.ron`). Without it the pickaxe is inert.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Pickaxe(pub ToolProfile);

/// The cell the player is looking at within the pickaxe's reach, updated
/// every frame after mouse look.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct PickTarget(pub Option<Hit>);

/// Swing timing.
#[derive(Resource, Debug, Default)]
pub struct SwingState {
    /// Fixed-step time since the last swing; `None` before the first.
    since_swing: Option<Duration>,
    swings: u64,
}

impl SwingState {
    /// Swings sent so far.
    pub fn swings(&self) -> u64 {
        self.swings
    }
}

/// [`PickaxePlugin`]'s system sets.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PickaxeSystems {
    /// `FixedUpdate`, after [`ControllerSystems::Move`]: swings.
    Swing,
    /// `Update`, after [`ControllerSystems::Look`]: updates [`PickTarget`].
    Target,
}

/// The first solid cell along `player`'s view ray from the eye, at most
/// `reach_m` meters away.
pub fn target(world: &World, player: &Player, reach_m: f32) -> Option<Hit> {
    raycast(
        world,
        player.eye(),
        player.rotation() * Vec3::NEG_Z,
        reach_m,
    )
}

/// Targeting and swinging. Needs the [`Pickaxe`] resource to do anything.
pub struct PickaxePlugin;

impl Plugin for PickaxePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ToHost>()
            .init_resource::<PickTarget>()
            .init_resource::<SwingState>()
            .init_resource::<CommandSeq>()
            .add_systems(
                FixedUpdate,
                swing
                    .in_set(PickaxeSystems::Swing)
                    .after(ControllerSystems::Move),
            )
            .add_systems(
                Update,
                update_target
                    .in_set(PickaxeSystems::Target)
                    .after(ControllerSystems::Look),
            );
    }
}

fn update_target(
    pickaxe: Option<Res<Pickaxe>>,
    replica: Option<Res<Replica>>,
    players: Query<&Player>,
    mut target_res: ResMut<PickTarget>,
) {
    let hit = match (pickaxe, replica, players.iter().next()) {
        (Some(p), Some(r), Some(player)) => target(r.world(), player, p.0.reach_m),
        _ => None,
    };
    target_res.set_if_neq(PickTarget(hit));
}

/// One fixed step of the pickaxe: advances the interval clock, then swings
/// at the target if the button is held and the interval has passed.
#[allow(clippy::too_many_arguments)]
fn swing(
    pickaxe: Option<Res<Pickaxe>>,
    replica: Option<Res<Replica>>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    fly: Option<Res<FlyCam>>,
    time: Res<Time>,
    mut state: ResMut<SwingState>,
    mut seq: ResMut<CommandSeq>,
    players: Query<&Player>,
    mut out: MessageWriter<ToHost>,
) {
    let (Some(pickaxe), Some(replica)) = (pickaxe, replica) else {
        return;
    };
    let profile = pickaxe.0;
    if let Some(since) = state.since_swing.as_mut() {
        *since += time.delta();
    }
    let held = buttons.is_some_and(|b| b.pressed(SWING_BUTTON));
    if !held || fly.is_some_and(|f| f.enabled) {
        return;
    }
    let interval = Duration::from_secs_f32(profile.interval_s);
    if state
        .since_swing
        .is_some_and(|since| since + SWING_SLACK < interval)
    {
        return;
    }
    let Some(player) = players.iter().next() else {
        return;
    };
    let Some(hit) = target(replica.world(), player, profile.reach_m) else {
        return;
    };
    let issuer = replica.player();
    out.write(ToHost(ClientMsg::Edit(EditCommand {
        id: seq.next_id(issuer),
        issuer,
        op: EditOp::Damage(profile.damage),
        shape: profile.shape.at(hit.cell),
    })));
    state.since_swing = Some(Duration::ZERO);
    state.swings += 1;
    debug!(cell = ?hit.cell, "pickaxe swing");
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::mouse::AccumulatedMouseMotion;
    use bevy::time::TimeUpdateStrategy;
    use rmc_world::ToolShape;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, CellPos};
    use rmc_world::edit::{CommandId, EditStatus, PlayerId};
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::shape::Shape;
    use std::sync::Arc;

    use crate::controller::ControllerPlugin;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const TOOLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tools.ron");
    /// The id `Replica::from_world` gives the local player.
    const ME: PlayerId = PlayerId(1);

    fn profile() -> ToolProfile {
        rmc_world::ToolTable::load(TOOLS_PATH)
            .expect("assets/tools.ron loads")
            .pickaxe
    }

    /// A 64³-cell world with a stone floor in cell rows y = 0..=3 (top at
    /// 2.0 m) and, if `wall_z` is given, a stone wall filling cell row
    /// z = `wall_z` from the floor up to y = 15. Its +Z face is at
    /// (wall_z + 1) · 0.5 m.
    fn world(wall_z: Option<i32>) -> World {
        let materials =
            Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
        let stone = materials.id("stone").unwrap();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 63));
        let mut world = World::new(materials, bounds);
        for pos in bounds.chunks() {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let mut boxes = vec![(CellPos::new(0, 0, 0), CellPos::new(63, 3, 63))];
        if let Some(z) = wall_z {
            boxes.push((CellPos::new(0, 4, z), CellPos::new(63, 15, z)));
        }
        for (min, max) in boxes {
            let cmd = EditCommand {
                id: CommandId::default(),
                issuer: PlayerId(0),
                op: EditOp::Set(stone),
                shape: Shape::Box { min, max },
            };
            assert_eq!(world.apply(&cmd).status, EditStatus::Applied);
        }
        world
    }

    /// Every message the client wrote for the host, with the frame it was
    /// written in.
    #[derive(Resource, Default)]
    struct Sent(Vec<(u32, ClientMsg)>);

    #[derive(Resource, Default)]
    struct Frame(u32);

    fn count_frame(mut frame: ResMut<Frame>) {
        frame.0 += 1;
    }

    fn record(frame: Res<Frame>, mut out: MessageReader<ToHost>, mut sent: ResMut<Sent>) {
        sent.0.extend(out.read().map(|m| (frame.0, m.0.clone())));
    }

    struct Rig {
        app: App,
        player: Entity,
    }

    impl Rig {
        /// The pickaxe with `profile` and the controller, at 60 frames per
        /// second (one fixed step per frame), a player standing at `feet`
        /// looking along -Z, and one frame run.
        fn new(world: World, feet: Vec3, profile: ToolProfile) -> Self {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                    1.0 / 60.0,
                )))
                .init_resource::<ButtonInput<KeyCode>>()
                .init_resource::<ButtonInput<MouseButton>>()
                .init_resource::<AccumulatedMouseMotion>()
                .init_resource::<FlyCam>()
                .init_resource::<Sent>()
                .init_resource::<Frame>()
                .insert_resource(Replica::from_world(world))
                .insert_resource(Pickaxe(profile))
                .add_plugins((ControllerPlugin, PickaxePlugin))
                .add_systems(First, count_frame)
                .add_systems(Last, record);
            let player = app.world_mut().spawn(Player::at(feet)).id();
            let mut rig = Self { app, player };
            rig.frames(1);
            rig
        }

        fn frames(&mut self, n: u32) {
            for _ in 0..n {
                self.app.update();
            }
        }

        fn frame(&self) -> u32 {
            self.app.world().resource::<Frame>().0
        }

        fn hold(&mut self, held: bool) {
            let mut buttons = self
                .app
                .world_mut()
                .resource_mut::<ButtonInput<MouseButton>>();
            if held {
                buttons.press(SWING_BUTTON);
            } else {
                buttons.release(SWING_BUTTON);
            }
        }

        fn player_mut(&mut self) -> Mut<'_, Player> {
            self.app.world_mut().get_mut::<Player>(self.player).unwrap()
        }

        fn player(&self) -> Player {
            self.app.world().get::<Player>(self.player).unwrap().clone()
        }

        /// The edits sent so far, with their frames.
        fn edits(&self) -> Vec<(u32, EditCommand)> {
            self.app
                .world()
                .resource::<Sent>()
                .0
                .iter()
                .filter_map(|(f, m)| match m {
                    ClientMsg::Edit(cmd) => Some((*f, *cmd)),
                    _ => None,
                })
                .collect()
        }

        fn target(&self) -> Option<Hit> {
            self.app.world().resource::<PickTarget>().0
        }

        fn expected_hit(&self) -> Option<Hit> {
            let reach = self.app.world().resource::<Pickaxe>().0.reach_m;
            let replica = self.app.world().resource::<Replica>();
            target(replica.world(), &self.player(), reach)
        }
    }

    /// Feet in the middle of cell column x = 32, z = 32 on the floor; the
    /// eye (3.55 m) is in cell row y = 7.
    fn feet_at_z(z: f32) -> Vec3 {
        Vec3::new(16.25, 2.0, z)
    }

    fn gaps(edits: &[(u32, EditCommand)]) -> Vec<u32> {
        edits.windows(2).map(|w| w[1].0 - w[0].0).collect()
    }

    #[test]
    fn swing_respects_interval() {
        let p = profile();
        let steps = (f64::from(p.interval_s) * 60.0).round() as u32;
        assert_eq!(steps, 21, "0.35 s is 21 fixed steps at 60 Hz");

        // A wall 2 m in front of the eye.
        let mut rig = Rig::new(world(Some(27)), feet_at_z(16.0), p);
        rig.frames(5);
        assert!(rig.edits().is_empty(), "nothing before the button");

        // Held for 2 s: a swing on the first step, then one per interval.
        rig.hold(true);
        let start = rig.frame() + 1;
        rig.frames(120);
        let edits = rig.edits();
        assert_eq!(edits.len(), 6, "2 s / 0.35 s, plus the first: {edits:?}");
        assert_eq!(
            edits[0].0, start,
            "the first swing is on the first held step"
        );
        assert_eq!(gaps(&edits), vec![steps; 5]);
        assert_eq!(rig.app.world().resource::<SwingState>().swings(), 6);

        // Releasing does not reset the interval: a re-press right after a
        // swing waits for the rest of it.
        let last = edits[5].0;
        while rig.frame() < last + 3 {
            rig.frames(1);
        }
        rig.hold(false);
        rig.frames(2);
        rig.hold(true);
        rig.frames(30);
        let edits = rig.edits();
        assert_eq!(edits[6].0, last + steps, "{edits:?}");

        // After a long rest, a press swings at once.
        rig.hold(false);
        rig.frames(60);
        let n = rig.edits().len();
        rig.hold(true);
        rig.frames(1);
        let edits = rig.edits();
        assert_eq!(edits.len(), n + 1);
        assert_eq!(edits[n].0, rig.frame());

        // The interval comes from the profile.
        let quick = ToolProfile {
            interval_s: 0.1,
            ..p
        };
        let mut rig = Rig::new(world(Some(27)), feet_at_z(16.0), quick);
        rig.hold(true);
        rig.frames(61);
        let edits = rig.edits();
        assert_eq!(edits.len(), 11, "{edits:?}");
        assert_eq!(gaps(&edits), vec![6; 10]);
    }

    #[test]
    fn swing_sends_damage_for_hit_cell() {
        let p = profile();
        let mut rig = Rig::new(world(Some(27)), feet_at_z(16.0), p);

        // Straight ahead: the wall cell in front of the eye, 2 m away.
        let hit = rig.expected_hit().expect("the wall is in reach");
        assert_eq!(hit.cell, CellPos::new(32, 7, 27));
        assert_eq!(hit.face_normal, IVec3::Z);
        assert!((hit.distance - 2.0).abs() < 1e-3, "{hit:?}");
        rig.frames(1);
        assert_eq!(rig.target(), Some(hit), "PickTarget mirrors the hit");

        rig.hold(true);
        rig.frames(1);
        rig.hold(false);
        let edits = rig.edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].1,
            EditCommand {
                id: CommandId { issuer: ME, seq: 0 },
                issuer: ME,
                op: EditOp::Damage(p.damage),
                shape: Shape::Cell(hit.cell),
            }
        );

        // Looking down: the floor cell the ray hits, through its top face.
        rig.player_mut().pitch = -1.0;
        rig.frames(30);
        let hit = rig.expected_hit().expect("the floor is in reach");
        assert_eq!(hit.cell.0.y, 3);
        assert_eq!(hit.face_normal, IVec3::Y);
        assert_eq!(rig.target(), Some(hit));
        rig.hold(true);
        rig.frames(1);
        let edits = rig.edits();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[1].1.id, CommandId { issuer: ME, seq: 1 });
        assert_eq!(edits[1].1.shape, Shape::Cell(hit.cell));
        assert_eq!(edits[1].1.op, EditOp::Damage(p.damage));

        // Damage and shape come from the profile; the shape centers on the hit.
        let cluster = ToolProfile {
            damage: 25,
            shape: ToolShape::Sphere { radius: 1 },
            ..p
        };
        let mut rig = Rig::new(world(Some(27)), feet_at_z(16.0), cluster);
        rig.hold(true);
        rig.frames(50);
        let edits = rig.edits();
        assert_eq!(edits.len(), 3);
        for (i, (_, cmd)) in edits.iter().enumerate() {
            assert_eq!(cmd.id.seq, i as u32, "each swing has its own id");
            assert_eq!(cmd.op, EditOp::Damage(25));
            assert_eq!(
                cmd.shape,
                Shape::Sphere {
                    center: CellPos::new(32, 7, 27),
                    radius: 1
                }
            );
        }
    }

    #[test]
    fn no_swing_beyond_reach() {
        let p = profile();
        assert_eq!(p.reach_m, 3.0);
        // The wall's face is at z = 13.0 m. From z = 16.02 the eye is
        // 3.02 m away: out of reach.
        let mut rig = Rig::new(world(Some(25)), feet_at_z(16.02), p);
        rig.hold(true);
        rig.frames(60);
        assert_eq!(rig.target(), None);
        assert!(rig.edits().is_empty(), "{:?}", rig.edits());

        // 2.99 m: in reach, and it swings on the next step.
        rig.player_mut().feet.z = 15.99;
        rig.frames(1);
        let hit = rig.expected_hit().expect("in reach");
        assert_eq!(hit.cell, CellPos::new(32, 7, 25));
        assert_eq!(rig.target(), Some(hit));
        let edits = rig.edits();
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(edits[0].1.shape, Shape::Cell(hit.cell));

        // Back out of reach: no more swings, however long it is held.
        rig.player_mut().feet.z = 16.02;
        rig.frames(60);
        assert_eq!(rig.edits().len(), 1);

        // Looking at the sky, nothing is in reach.
        let mut rig = Rig::new(world(None), feet_at_z(16.0), p);
        rig.player_mut().pitch = 1.2;
        rig.hold(true);
        rig.frames(60);
        assert_eq!(rig.target(), None);
        assert!(rig.edits().is_empty());

        // A wall in reach, but the fly camera is on: no swing.
        let mut rig = Rig::new(world(Some(27)), feet_at_z(16.0), p);
        rig.app.world_mut().resource_mut::<FlyCam>().enabled = true;
        rig.hold(true);
        rig.frames(60);
        assert!(rig.edits().is_empty());

        // No Pickaxe profile: nothing targeted, nothing sent.
        let mut rig = Rig::new(world(Some(27)), feet_at_z(16.0), p);
        rig.app.world_mut().remove_resource::<Pickaxe>();
        rig.hold(true);
        rig.frames(30);
        assert_eq!(rig.target(), None);
        assert!(rig.edits().is_empty());
    }
}
