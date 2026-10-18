//! Render interpolation of fixed-step motion (SAD §11.4).
//!
//! The controller moves the player in `FixedUpdate` at 60 Hz, but frames
//! come at the display's rate. Showing the latest fixed-step position would
//! make the view stutter whenever the two rates differ. Instead, as in
//! Bevy's `physics_in_fixed_timestep` example, every fixed step records the
//! eye it ended at in an [`InterpolatedEye`] (keeping the one before), and
//! each frame the camera goes to [`render_pos`] of the two, at
//! `Time<Fixed>::overstep_fraction()`: the share of a fixed step that has
//! elapsed since the last one ran. The view is at most one fixed step
//! behind the simulation, and it moves every frame.

use bevy::prelude::*;

/// The eye at the end of the last two fixed steps, in meters.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct InterpolatedEye {
    /// After the step before the last.
    pub prev: Vec3,
    /// After the last step.
    pub curr: Vec3,
}

impl InterpolatedEye {
    /// No motion yet: both positions are `eye`.
    pub fn at(eye: Vec3) -> Self {
        Self {
            prev: eye,
            curr: eye,
        }
    }

    /// Records the eye after a fixed step.
    pub fn push(&mut self, eye: Vec3) {
        self.prev = self.curr;
        self.curr = eye;
    }

    /// Where to draw the eye `overstep_fraction` of a step after the last.
    pub fn render_pos(&self, overstep_fraction: f32) -> Vec3 {
        render_pos(self.prev, self.curr, overstep_fraction)
    }
}

/// The point `overstep_fraction` of the way from `prev` to `curr`. The
/// fraction is clamped to `0..=1`, so the result never leaves the segment.
pub fn render_pos(prev: Vec3, curr: Vec3, overstep_fraction: f32) -> Vec3 {
    prev.lerp(curr, overstep_fraction.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{ControllerConfig, ControllerPlugin, EYE_HEIGHT_M, Player};
    use crate::flycam::FlyCam;
    use crate::render::GameCamera;
    use crate::replica::Replica;
    use bevy::input::mouse::AccumulatedMouseMotion;
    use bevy::time::TimeUpdateStrategy;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, CellPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::shape::Shape;
    use rmc_world::world::World;
    use std::sync::Arc;
    use std::time::Duration;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// A 64³-cell world with a stone floor whose top is at 2.0 m.
    fn floor_world() -> World {
        let materials =
            Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 63));
        let mut world = World::new(materials, bounds);
        for pos in bounds.chunks() {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let stone = world.materials().id("stone").unwrap();
        let cmd = EditCommand {
            id: CommandId::default(),
            issuer: PlayerId(0),
            op: EditOp::Set(stone),
            shape: Shape::Box {
                min: CellPos::new(0, 0, 0),
                max: CellPos::new(63, 3, 63),
            },
        };
        assert_eq!(world.apply(&cmd).status, EditStatus::Applied);
        world
    }

    fn near(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn render_pos_between_prev_and_curr() {
        // The pure lerp: the ends, the middle, every fraction on the segment
        // and clamped outside 0..=1.
        let prev = Vec3::new(1.0, 2.0, 3.0);
        let curr = Vec3::new(3.0, 1.0, 7.0);
        assert!(near(render_pos(prev, curr, 0.0), prev), "0 is prev");
        assert!(near(render_pos(prev, curr, 1.0), curr), "1 is curr");
        assert!(near(render_pos(prev, curr, 0.5), Vec3::new(2.0, 1.5, 5.0)));
        assert!(near(render_pos(prev, curr, -0.5), prev), "clamped below");
        assert!(near(render_pos(prev, curr, 1.5), curr), "clamped above");
        for i in 0..=16 {
            let t = i as f32 / 16.0;
            let p = render_pos(prev, curr, t);
            assert!(
                near(p, prev + (curr - prev) * t),
                "{t}: {p:?} is t of the way"
            );
            assert!(
                (p - prev).length() + (p - curr).length() <= (curr - prev).length() + 1e-5,
                "{t}: {p:?} lies between prev and curr"
            );
        }
        assert_eq!(render_pos(prev, prev, 0.3), prev, "no motion, no drift");

        // push keeps the last two.
        let mut eye = InterpolatedEye::at(prev);
        assert_eq!(eye.render_pos(0.7), prev);
        eye.push(curr);
        assert_eq!((eye.prev, eye.curr), (prev, curr));
        eye.push(Vec3::ZERO);
        assert_eq!((eye.prev, eye.curr), (curr, Vec3::ZERO));
        assert!(near(eye.render_pos(0.25), curr * 0.75));

        // In the controller: frames at 240 Hz, fixed steps at 60 Hz, so a
        // fixed step runs every fourth frame. The player walks along -Z on a
        // floor. Every frame the camera is at the overstep fraction between
        // the last two fixed-step eyes, and it moves every frame by a quarter
        // of a step instead of a whole step every fourth frame.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 240.0,
            )))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<FlyCam>()
            .insert_resource(Replica::from_world(floor_world()))
            .add_plugins(ControllerPlugin);
        let player = app
            .world_mut()
            .spawn(Player::at(Vec3::new(16.0, 2.0, 24.0)))
            .id();
        let camera = app
            .world_mut()
            .spawn((GameCamera, Transform::default()))
            .id();
        // Settle onto the floor.
        for _ in 0..40 {
            app.update();
        }
        let p = app.world().get::<Player>(player).unwrap().clone();
        assert!(p.grounded, "on the floor: {:?}", p.feet);
        let still = app.world().get::<InterpolatedEye>(player).copied();
        assert_eq!(
            still,
            Some(InterpolatedEye::at(p.eye())),
            "the controller records the eye of a player standing still"
        );
        let cam = app.world().get::<Transform>(camera).unwrap().translation;
        assert!(near(cam, p.eye()), "standing still: camera at the eye");

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        let walk = ControllerConfig::default().walk_speed_mps;
        let mut last = cam;
        let mut fixed_steps = 0;
        let mut last_curr = still.unwrap().curr;
        for frame in 0..48 {
            app.update();
            let hist = *app.world().get::<InterpolatedEye>(player).unwrap();
            let p = app.world().get::<Player>(player).unwrap().clone();
            assert!(near(hist.curr, p.eye()), "{frame}: curr is the eye now");
            if hist.curr != last_curr {
                fixed_steps += 1;
                assert_eq!(hist.prev, last_curr, "{frame}: prev is the last curr");
                last_curr = hist.curr;
            }
            let alpha = app.world().resource::<Time<Fixed>>().overstep_fraction();
            let cam = app.world().get::<Transform>(camera).unwrap().translation;
            assert!(
                near(cam, render_pos(hist.prev, hist.curr, alpha)),
                "{frame}: camera {cam:?} at {alpha} between {:?} and {:?}",
                hist.prev,
                hist.curr
            );
            assert!(
                (cam.y - (2.0 + EYE_HEIGHT_M)).abs() < 0.01,
                "{frame}: eye height"
            );
            if frame >= 8 {
                // Past the first fixed step of walking, every frame moves
                // the view by walk / 240 along -Z.
                let moved = last - cam;
                assert!(
                    (moved.z - walk / 240.0).abs() < 1e-3 && moved.x.abs() < 1e-5,
                    "{frame}: the view moves a quarter step each frame: {moved:?}"
                );
            }
            last = cam;
        }
        assert_eq!(fixed_steps, 12, "one fixed step per four frames");
    }
}
