//! The local player as scenarios see it (SAD §12.2).
//!
//! The game spawns no [`Player`] on its own yet; a scenario places one with
//! [`TestApp::spawn_player`] and reads it back with [`TestApp::player`].
//! From then on the controller moves it from injected keys and mouse, and
//! the game camera follows its eye.

use bevy::prelude::*;
use rmc_client::controller::{Player, blocks};
use rmc_client::{InterpolatedEye, Replica};
use rmc_world::coords::{CELL_SIZE_M, CellPos};

use crate::app::TestApp;

/// The player's state after the last step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerSnapshot {
    /// The middle of the bottom face of the player's box, in meters.
    pub pos: Vec3,
    /// m/s.
    pub velocity: Vec3,
    /// Radians about world Y; 0 looks along -Z, positive turns left.
    pub yaw: f32,
    /// Radians; positive looks up.
    pub pitch: f32,
    pub grounded: bool,
    /// The eye, including any step-up offset still being smoothed.
    pub eye: Vec3,
}

impl TestApp {
    /// Spawns the local player standing still with its feet at `feet`,
    /// looking along -Z. It moves from the next fixed step on.
    ///
    /// Panics if a player already exists.
    pub fn spawn_player(&mut self, feet: Vec3) -> Entity {
        let world = self.app_mut().world_mut();
        assert_eq!(
            world.query::<&Player>().iter(world).count(),
            0,
            "one local player"
        );
        world.spawn(Player::at(feet)).id()
    }

    /// The local player after the last step.
    ///
    /// Panics if no player was spawned.
    pub fn player(&self) -> PlayerSnapshot {
        let p = self.player_component();
        PlayerSnapshot {
            pos: p.feet,
            velocity: p.velocity,
            yaw: p.yaw,
            pitch: p.pitch,
            grounded: p.grounded,
            eye: p.eye(),
        }
    }

    /// The eye the camera is drawn from this frame: interpolated between
    /// the last two fixed steps once the player has moved (SAD §11.4).
    pub fn player_render_eye(&self) -> Option<InterpolatedEye> {
        let world = self.app().world();
        let entity = self.player_entity();
        world.get::<InterpolatedEye>(entity).copied()
    }

    /// Every cell of the replica that blocks the player and shares volume
    /// with its box, in x, y, z order. Empty while the player is free.
    pub fn player_penetration(&self) -> Vec<CellPos> {
        let aabb = self.player_component().aabb();
        let replica = self.app().world().resource::<Replica>().world();
        let lo = (aabb.min / CELL_SIZE_M).floor().as_ivec3();
        let hi = (aabb.max / CELL_SIZE_M).ceil().as_ivec3() - IVec3::ONE;
        let mut cells = Vec::new();
        for x in lo.x..=hi.x {
            for y in lo.y..=hi.y {
                for z in lo.z..=hi.z {
                    let cell = CellPos::new(x, y, z);
                    if aabb.overlaps_cell(cell) && blocks(replica, cell) {
                        cells.push(cell);
                    }
                }
            }
        }
        cells
    }

    fn player_entity(&self) -> Entity {
        let world = self.app().world();
        world
            .try_query::<(Entity, &Player)>()
            .and_then(|mut q| q.iter(world).next().map(|(e, _)| e))
            .expect("a player was spawned (TestApp::spawn_player)")
    }

    fn player_component(&self) -> &Player {
        let entity = self.player_entity();
        self.app()
            .world()
            .get::<Player>(entity)
            .expect("the player entity has a Player")
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{TestApp, TestConfig};
    use bevy::prelude::*;
    use rmc_client::controller::EYE_HEIGHT_M;
    use rmc_protocol::ClientMsg;
    use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp};
    use rmc_world::materials::Phase;
    use rmc_world::shape::Shape;

    /// A logic-only player on a stone floor built at y = 100 in a 2 × 4 × 2
    /// chunk world, cleared above.
    #[test]
    fn spawned_player_falls_onto_floor_and_walks() {
        let mut t = TestApp::new(TestConfig {
            seed: 3,
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
            render: false,
            ..TestConfig::default()
        });
        let stone = t
            .host_world()
            .materials()
            .iter()
            .find(|d| d.phase == Phase::Solid && d.diggable)
            .expect("a diggable solid")
            .id;
        let edits = [
            (
                EditOp::Set(stone),
                CellPos::new(24, 100, 24),
                CellPos::new(39, 100, 39),
            ),
            (
                EditOp::Remove,
                CellPos::new(24, 101, 24),
                CellPos::new(39, 120, 39),
            ),
        ];
        for (seq, (op, min, max)) in (1..).zip(edits) {
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

        let floor = 101.0 * CELL_SIZE_M;
        t.spawn_player(Vec3::new(16.0, floor + 1.0, 16.0));
        assert!(t.player_render_eye().is_none(), "no fixed step yet");
        t.steps(40);
        let p = t.player();
        assert!(p.grounded, "landed: {p:?}");
        assert!((p.pos.y - floor).abs() < 0.01, "on the floor: {p:?}");
        assert_eq!(p.eye, p.pos + Vec3::Y * EYE_HEIGHT_M);
        assert_eq!(t.player_render_eye().map(|e| e.curr), Some(p.eye));
        assert!(t.player_penetration().is_empty());

        t.press(KeyCode::KeyW);
        t.steps(30);
        t.release(KeyCode::KeyW);
        t.step();
        let moved = t.player().pos - p.pos;
        assert!(
            moved.z < -1.5 && moved.x.abs() < 1e-4,
            "walked -Z: {moved:?}"
        );

        // Teleported into the floor, the box overlaps the floor cells.
        let world = t.app_mut().world_mut();
        let mut q = world.query::<&mut rmc_client::controller::Player>();
        q.single_mut(world).unwrap().feet.y = floor - 0.2;
        let cells = t.player_penetration();
        assert!(!cells.is_empty());
        assert!(cells.iter().all(|c| c.0.y == 100), "{cells:?}");
    }
}
