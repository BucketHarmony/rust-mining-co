//! Fly camera (noclip) for the debug editor (SAD §11.6).
//!
//! While [`FlyCam::enabled`], the [`GameCamera`] moves freely: no collision,
//! no gravity, no world bounds. It is separate from the player controller
//! and does not read the replica, so it passes through solid cells.
//!
//! Input comes from `ButtonInput<KeyCode>` and `AccumulatedMouseMotion`
//! only (SAD §11.4), so injected input drives it exactly as real input:
//!
//! - [`FLY_TOGGLE_KEY`] (V) turns it on and off.
//! - W / S along the view direction (pitch included), D / A right and left,
//!   Space up and Left Ctrl down along world Y. Shift multiplies the speed
//!   by [`FlyCam::fast_multiplier`].
//! - The mouse turns it: yaw about world Y, pitch clamped to ±[`MAX_PITCH_DEG`],
//!   no roll.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;

use crate::render::GameCamera;

/// Turns the fly camera on and off.
pub const FLY_TOGGLE_KEY: KeyCode = KeyCode::KeyV;

/// How far the fly camera pitches up or down, in degrees.
pub const MAX_PITCH_DEG: f32 = 89.0;

/// Fly camera settings and state.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct FlyCam {
    pub enabled: bool,
    /// Meters per second.
    pub speed_mps: f32,
    /// Speed factor while Shift is held.
    pub fast_multiplier: f32,
    /// Radians of turn per unit of mouse motion.
    pub sensitivity_rad: f32,
}

impl Default for FlyCam {
    fn default() -> Self {
        Self {
            enabled: false,
            speed_mps: 10.0,
            fast_multiplier: 4.0,
            sensitivity_rad: 0.002,
        }
    }
}

/// The fly camera's systems, in `Update`.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FlyCamSystems;

/// The fly camera.
pub struct FlyCamPlugin;

impl Plugin for FlyCamPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlyCam>().add_systems(
            Update,
            (toggle_fly_cam, fly_camera).chain().in_set(FlyCamSystems),
        );
    }
}

/// `rotation` turned by `mouse` motion: right and down are positive, as
/// winit reports them. Yaw is about world Y, pitch is clamped, roll is
/// dropped.
pub fn turn(rotation: Quat, mouse: Vec2, sensitivity_rad: f32) -> Quat {
    let (yaw, pitch, _roll) = rotation.to_euler(EulerRot::YXZ);
    let max = MAX_PITCH_DEG.to_radians();
    let yaw = yaw - mouse.x * sensitivity_rad;
    let pitch = (pitch - mouse.y * sensitivity_rad).clamp(-max, max);
    Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0)
}

/// The velocity, in m/s, the held keys ask for with the camera at
/// `rotation`. Opposite keys cancel; diagonals are as fast as one axis.
pub fn fly_velocity(rotation: Quat, keys: &ButtonInput<KeyCode>, fly: &FlyCam) -> Vec3 {
    let forward = rotation * Vec3::NEG_Z;
    let right = rotation * Vec3::X;
    let axis = |plus: KeyCode, minus: KeyCode| {
        (keys.pressed(plus) as i32 - keys.pressed(minus) as i32) as f32
    };
    let dir = forward * axis(KeyCode::KeyW, KeyCode::KeyS)
        + right * axis(KeyCode::KeyD, KeyCode::KeyA)
        + Vec3::Y * axis(KeyCode::Space, KeyCode::ControlLeft);
    let fast = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let speed = fly.speed_mps * if fast { fly.fast_multiplier } else { 1.0 };
    dir.normalize_or_zero() * speed
}

fn toggle_fly_cam(keys: Option<Res<ButtonInput<KeyCode>>>, mut fly: ResMut<FlyCam>) {
    if keys.is_some_and(|k| k.just_pressed(FLY_TOGGLE_KEY)) {
        fly.enabled = !fly.enabled;
        info!(enabled = fly.enabled, "fly camera");
    }
}

fn fly_camera(
    fly: Res<FlyCam>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mouse: Option<Res<AccumulatedMouseMotion>>,
    time: Res<Time>,
    mut cameras: Query<&mut Transform, With<GameCamera>>,
) {
    if !fly.enabled {
        return;
    }
    let motion = mouse.map_or(Vec2::ZERO, |m| m.delta);
    let dt = time.delta_secs();
    for mut transform in &mut cameras {
        if motion != Vec2::ZERO {
            transform.rotation = turn(transform.rotation, motion, fly.sensitivity_rad);
        }
        if let Some(keys) = keys.as_deref() {
            let velocity = fly_velocity(transform.rotation, keys, &fly);
            if velocity != Vec3::ZERO {
                transform.translation += velocity * dt;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;
    use std::time::Duration;

    const DT: f32 = 1.0 / 60.0;
    const EPS: f32 = 1e-4;

    /// MinimalPlugins + FlyCamPlugin and one game camera at `start`. Input
    /// resources are set by hand; [`frame`] clears them like InputPlugin.
    fn fly_app(start: Transform) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(
                DT,
            )))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<AccumulatedMouseMotion>()
            .add_plugins(FlyCamPlugin);
        let camera = app.world_mut().spawn((GameCamera, start)).id();
        // The first update starts the clock.
        app.update();
        (app, camera)
    }

    fn frame(app: &mut App) {
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        app.world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = Vec2::ZERO;
    }

    fn keys(app: &mut App) -> Mut<'_, ButtonInput<KeyCode>> {
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>()
    }

    fn mouse(app: &mut App, delta: Vec2) {
        app.world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = delta;
    }

    fn transform(app: &App, camera: Entity) -> Transform {
        *app.world().get::<Transform>(camera).unwrap()
    }

    fn enabled(app: &App) -> bool {
        app.world().resource::<FlyCam>().enabled
    }

    fn tilted() -> Transform {
        Transform::from_xyz(3.0, 40.0, -7.0).with_rotation(Quat::from_euler(
            EulerRot::YXZ,
            30f32.to_radians(),
            -20f32.to_radians(),
            0.0,
        ))
    }

    fn pitch(t: &Transform) -> f32 {
        t.rotation.to_euler(EulerRot::YXZ).1
    }

    #[test]
    fn disabled_by_default_and_ignores_input() {
        let start = tilted();
        let (mut app, camera) = fly_app(start);
        assert!(!enabled(&app), "the fly camera starts off");
        keys(&mut app).press(KeyCode::KeyW);
        keys(&mut app).press(KeyCode::Space);
        for _ in 0..10 {
            mouse(&mut app, Vec2::new(50.0, -30.0));
            frame(&mut app);
        }
        assert_eq!(transform(&app, camera), start, "off: nothing moves");
    }

    #[test]
    fn toggle_key_flips_enabled_once_per_press() {
        let (mut app, _) = fly_app(tilted());
        keys(&mut app).press(FLY_TOGGLE_KEY);
        frame(&mut app);
        assert!(enabled(&app), "press turns it on");
        // Still held: no repeat.
        frame(&mut app);
        frame(&mut app);
        assert!(enabled(&app), "holding the key does not toggle again");
        keys(&mut app).release(FLY_TOGGLE_KEY);
        frame(&mut app);
        assert!(enabled(&app), "release does not toggle");
        keys(&mut app).press(FLY_TOGGLE_KEY);
        frame(&mut app);
        assert!(!enabled(&app), "the next press turns it off");
    }

    #[test]
    fn keys_move_along_view_axes_at_speed() {
        let start = tilted();
        let (mut app, camera) = fly_app(start);
        app.world_mut().resource_mut::<FlyCam>().enabled = true;
        let fly = app.world().resource::<FlyCam>().clone();
        let f = start.forward().as_vec3();
        let r = start.right().as_vec3();
        assert!(f.y < -0.3, "forward is pitched down");
        assert!(r.y.abs() < EPS, "right is horizontal");

        use KeyCode::*;
        let cases: [(&[KeyCode], Vec3, f32); 11] = [
            (&[KeyW], f, 1.0),
            (&[KeyS], -f, 1.0),
            (&[KeyD], r, 1.0),
            (&[KeyA], -r, 1.0),
            (&[Space], Vec3::Y, 1.0),
            (&[ControlLeft], Vec3::NEG_Y, 1.0),
            (&[KeyW, KeyD], (f + r).normalize(), 1.0),
            (&[KeyW, ShiftLeft], f, fly.fast_multiplier),
            (&[ControlLeft, ShiftRight], Vec3::NEG_Y, fly.fast_multiplier),
            (&[KeyW, KeyS], Vec3::ZERO, 1.0),
            (&[ShiftLeft], Vec3::ZERO, 1.0),
        ];
        for (held, dir, factor) in cases {
            let before = transform(&app, camera);
            for &k in held {
                keys(&mut app).press(k);
            }
            frame(&mut app);
            for &k in held {
                keys(&mut app).release(k);
            }
            let after = transform(&app, camera);
            let expected = dir * fly.speed_mps * factor * DT;
            let moved = after.translation - before.translation;
            assert!(
                (moved - expected).length() < EPS,
                "{held:?}: moved {moved:?}, expected {expected:?}"
            );
            assert_eq!(after.rotation, start.rotation, "{held:?}: no turn");
        }

        // Held keys keep moving it every frame, with no clamp of any kind.
        keys(&mut app).press(ControlLeft);
        keys(&mut app).press(ShiftLeft);
        let before = transform(&app, camera).translation;
        for _ in 0..120 {
            frame(&mut app);
        }
        let fell = before.y - transform(&app, camera).translation.y;
        let expected = fly.speed_mps * fly.fast_multiplier * 2.0;
        assert!((fell - expected).abs() < 1e-2, "fell {fell} m in 2 s");
        assert!(transform(&app, camera).translation.y < 0.0, "below y = 0");
    }

    #[test]
    fn mouse_turns_and_pitch_clamps() {
        let start = tilted();
        let (mut app, camera) = fly_app(start);
        app.world_mut().resource_mut::<FlyCam>().enabled = true;
        let sens = app.world().resource::<FlyCam>().sensitivity_rad;

        // Right turns right (clockwise seen from above), about world Y.
        mouse(&mut app, Vec2::new(100.0, 0.0));
        frame(&mut app);
        let t = transform(&app, camera);
        let expected = Quat::from_rotation_y(-100.0 * sens) * start.forward().as_vec3();
        assert!((t.forward().as_vec3() - expected).length() < EPS);
        assert_eq!(t.translation, start.translation, "turning does not move");
        assert!(t.right().y.abs() < EPS, "no roll");

        // Down pitches down.
        let p0 = pitch(&t);
        mouse(&mut app, Vec2::new(0.0, 50.0));
        frame(&mut app);
        let t = transform(&app, camera);
        assert!((pitch(&t) - (p0 - 50.0 * sens)).abs() < EPS);

        // Far down or up stops at ±MAX_PITCH_DEG, never over the top.
        let max = MAX_PITCH_DEG.to_radians();
        mouse(&mut app, Vec2::new(0.0, 1e5));
        frame(&mut app);
        let t = transform(&app, camera);
        assert!((pitch(&t) + max).abs() < EPS, "clamped down: {}", pitch(&t));
        assert!(t.forward().y < -0.99 && t.up().y > 0.0);
        mouse(&mut app, Vec2::new(0.0, -1e5));
        frame(&mut app);
        let t = transform(&app, camera);
        assert!((pitch(&t) - max).abs() < EPS, "clamped up: {}", pitch(&t));
        assert!(t.forward().y > 0.99 && t.up().y > 0.0);
        assert!(t.right().y.abs() < EPS, "no roll");

        // No motion leaves the rotation exactly as it was.
        let before = transform(&app, camera);
        frame(&mut app);
        assert_eq!(transform(&app, camera), before);

        // Off: the mouse does nothing.
        app.world_mut().resource_mut::<FlyCam>().enabled = false;
        mouse(&mut app, Vec2::new(300.0, 300.0));
        frame(&mut app);
        assert_eq!(transform(&app, camera), before);
    }
}
