//! Cursor grab for the windowed game (owner's extension, F091; SAD §11.4,
//! §11.6).
//!
//! While playing, the primary window's cursor is grabbed: hidden and
//! [`GRABBED_MODE`] (`Locked`; bevy_winit falls back to `Confined` where the
//! platform cannot lock). [`CursorGrab`] holds the state; [`CursorPlugin`]
//! changes it from input and writes it to the window's `CursorOptions`.
//!
//! - [`RELEASE_KEY`] (Escape) releases the cursor, and so does the window
//!   losing focus.
//! - A press of [`GRAB_BUTTON`] (left click) in the window grabs it again.
//!   That press does not swing the pickaxe: swings stay off until the
//!   button is let go.
//! - Opening the editor (F1, or anything that sets `EditorState::open`)
//!   releases the cursor for the egui panel, and clicks then belong to the
//!   brush; closing it grabs again.
//!
//! Mouse look (the player's and the fly camera's) applies only while the
//! cursor is grabbed. An app without [`CursorPlugin`] (tests, the headless
//! test kit) has no [`CursorGrab`], and looks and swings as before.

use bevy::input::InputSystems;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::editor::EditorState;

/// Releases the cursor.
pub const RELEASE_KEY: KeyCode = KeyCode::Escape;

/// Grabs the cursor again, in the window, while it is released and the
/// editor is closed.
pub const GRAB_BUTTON: MouseButton = MouseButton::Left;

/// The grab mode asked of the window while grabbed.
pub const GRABBED_MODE: CursorGrabMode = CursorGrabMode::Locked;

/// Whether the cursor is grabbed. See the module docs.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorGrab {
    grabbed: bool,
    /// The press of [`GRAB_BUTTON`] that grabbed is still held.
    swallow_click: bool,
    /// The editor's `open` when last seen, to act on its changes only.
    editor_open: bool,
}

impl Default for CursorGrab {
    /// Grabbed: the game starts in play.
    fn default() -> Self {
        Self {
            grabbed: true,
            swallow_click: false,
            editor_open: false,
        }
    }
}

impl CursorGrab {
    /// Whether the cursor is grabbed: hidden, locked, and turning the view.
    pub fn grabbed(&self) -> bool {
        self.grabbed
    }

    /// Whether a held [`GRAB_BUTTON`] may swing: grabbed, and not the press
    /// that grabbed.
    pub fn swing_allowed(&self) -> bool {
        self.grabbed && !self.swallow_click
    }
}

/// Whether mouse motion turns the view: no [`CursorGrab`] (no
/// [`CursorPlugin`]), or grabbed.
pub fn look_allowed(grab: Option<&CursorGrab>) -> bool {
    grab.is_none_or(CursorGrab::grabbed)
}

/// Whether a held swing button may swing: no [`CursorGrab`], or
/// [`CursorGrab::swing_allowed`].
pub fn swing_allowed(grab: Option<&CursorGrab>) -> bool {
    grab.is_none_or(CursorGrab::swing_allowed)
}

/// The cursor systems, in `PreUpdate` after input.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct CursorSystems;

/// Grabs and releases the primary window's cursor. Added by the windowed
/// game only.
pub struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CursorGrab>().add_systems(
            PreUpdate,
            (update_grab, show_grab)
                .chain()
                .in_set(CursorSystems)
                .after(InputSystems),
        );
    }
}

/// Changes [`CursorGrab`] from this frame's input, the editor and the
/// primary window's focus. Runs before the fixed steps and `Update`, so a
/// frame's swing and look see the grab its input made.
fn update_grab(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    editor: Option<Res<EditorState>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut grab: ResMut<CursorGrab>,
) {
    let held = buttons.as_ref().is_some_and(|b| b.pressed(GRAB_BUTTON));
    let clicked = buttons.is_some_and(|b| b.just_pressed(GRAB_BUTTON));
    let escape = keys.is_some_and(|k| k.just_pressed(RELEASE_KEY));
    let editor_open = editor.is_some_and(|e| e.open);
    let focused = windows.iter().next().is_none_or(|w| w.focused);
    let mut next = *grab;
    if !held {
        next.swallow_click = false;
    }
    if editor_open != next.editor_open {
        next.editor_open = editor_open;
        next.grabbed = !editor_open;
        next.swallow_click = held;
    }
    if next.grabbed && (escape || !focused) {
        next.grabbed = false;
    } else if !next.grabbed && !editor_open && focused && clicked {
        next.grabbed = true;
        next.swallow_click = true;
    }
    if next.grabbed != grab.grabbed {
        info!(grabbed = next.grabbed, "cursor");
    }
    grab.set_if_neq(next);
}

/// Hides and locks the primary window's cursor while grabbed; shows and
/// frees it otherwise. Writes only on a difference, so the window backend
/// sees a change only when there is one.
fn show_grab(grab: Res<CursorGrab>, mut cursors: Query<&mut CursorOptions, With<PrimaryWindow>>) {
    let (mode, visible) = if grab.grabbed {
        (GRABBED_MODE, false)
    } else {
        (CursorGrabMode::None, true)
    };
    for mut cursor in &mut cursors {
        if cursor.grab_mode != mode || cursor.visible != visible {
            cursor.grab_mode = mode;
            cursor.visible = visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    use crate::controller::{ControllerPlugin, Player};
    use crate::editor::{EditorPlugin, TOGGLE_EDITOR_KEY};
    use crate::flycam::{FlyCam, FlyCamPlugin};
    use crate::net::ClientMaterials;
    use crate::pickaxe::{Pickaxe, PickaxePlugin, SwingState};
    use crate::render::GameCamera;
    use crate::replica::Replica;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const TOOLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tools.ron");

    /// A 64³-cell world with a stone floor in cell rows y = 0..=3 (top at
    /// 2.0 m) and a stone wall in cell row z = 20 (+Z face at 10.5 m).
    fn world() -> World {
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
        for (min, max) in [
            (CellPos::new(0, 0, 0), CellPos::new(63, 3, 63)),
            (CellPos::new(0, 4, 20), CellPos::new(63, 15, 20)),
        ] {
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

    /// The game's input-driven plugins plus [`CursorPlugin`], under
    /// `MinimalPlugins` at 60 frames per second (one fixed step per frame),
    /// with a primary `Window` entity (no OS window: nothing creates one
    /// without `WinitPlugin`), a game camera, and a player 1 m in front of
    /// the wall, looking at it.
    struct Rig {
        app: App,
        window: Entity,
        camera: Entity,
    }

    impl Rig {
        fn new() -> Self {
            let world = world();
            let profile = rmc_world::ToolTable::load(TOOLS_PATH)
                .expect("assets/tools.ron loads")
                .pickaxe;
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                    1.0 / 60.0,
                )))
                .init_resource::<ButtonInput<KeyCode>>()
                .init_resource::<ButtonInput<MouseButton>>()
                .init_resource::<AccumulatedMouseMotion>()
                .insert_resource(ClientMaterials(world.materials().clone()))
                .insert_resource(Replica::from_world(world))
                .insert_resource(Pickaxe(profile))
                .add_plugins((
                    FlyCamPlugin,
                    ControllerPlugin,
                    PickaxePlugin,
                    EditorPlugin,
                    CursorPlugin,
                ));
            let window = app
                .world_mut()
                .spawn((Window::default(), PrimaryWindow))
                .id();
            let camera = app
                .world_mut()
                .spawn((GameCamera, Transform::IDENTITY))
                .id();
            app.world_mut()
                .spawn(Player::at(Vec3::new(16.25, 2.0, 11.5)));
            let mut rig = Self {
                app,
                window,
                camera,
            };
            rig.frame();
            rig
        }

        /// One frame; then the frame's presses stop being "just" pressed
        /// and the mouse motion is spent, as `InputPlugin` would do.
        fn frame(&mut self) {
            self.app.update();
            let world = self.app.world_mut();
            world.resource_mut::<ButtonInput<KeyCode>>().clear();
            world.resource_mut::<ButtonInput<MouseButton>>().clear();
            world.resource_mut::<AccumulatedMouseMotion>().delta = Vec2::ZERO;
        }

        fn frames(&mut self, n: u32) {
            for _ in 0..n {
                self.frame();
            }
        }

        fn tap(&mut self, key: KeyCode) {
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(key);
            self.frame();
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .release(key);
            self.frame();
        }

        fn button(&mut self, held: bool) {
            let world = self.app.world_mut();
            let mut buttons = world.resource_mut::<ButtonInput<MouseButton>>();
            if held {
                buttons.press(GRAB_BUTTON);
            } else {
                buttons.release(GRAB_BUTTON);
            }
        }

        fn grab(&self) -> CursorGrab {
            *self.app.world().resource::<CursorGrab>()
        }

        fn cursor(&self) -> CursorOptions {
            self.app
                .world()
                .get::<CursorOptions>(self.window)
                .expect("Window requires CursorOptions")
                .clone()
        }

        fn editor_open(&self) -> bool {
            self.app.world().resource::<EditorState>().open
        }

        fn set_editor_open(&mut self, open: bool) {
            self.app.world_mut().resource_mut::<EditorState>().open = open;
        }

        fn set_focused(&mut self, focused: bool) {
            self.app
                .world_mut()
                .get_mut::<Window>(self.window)
                .unwrap()
                .focused = focused;
        }

        fn swings(&self) -> u64 {
            self.app.world().resource::<SwingState>().swings()
        }

        fn yaw(&mut self) -> f32 {
            let mut players = self.app.world_mut().query::<&Player>();
            players.single(self.app.world()).unwrap().yaw
        }

        fn camera_rotation(&self) -> Quat {
            self.app
                .world()
                .get::<Transform>(self.camera)
                .unwrap()
                .rotation
        }

        /// Moves the mouse for one frame; returns how far the player's yaw
        /// turned.
        fn turn(&mut self) -> f32 {
            let before = self.yaw();
            self.app
                .world_mut()
                .resource_mut::<AccumulatedMouseMotion>()
                .delta = Vec2::new(40.0, 0.0);
            self.frame();
            self.yaw() - before
        }

        /// Asserts the grab state and that the window shows it.
        #[track_caller]
        fn assert_grabbed(&self, grabbed: bool) {
            assert_eq!(self.grab().grabbed(), grabbed, "CursorGrab::grabbed");
            let cursor = self.cursor();
            if grabbed {
                assert_eq!(cursor.grab_mode, GRABBED_MODE, "grab mode while grabbed");
                assert!(!cursor.visible, "the cursor is hidden while grabbed");
            } else {
                assert_eq!(
                    cursor.grab_mode,
                    CursorGrabMode::None,
                    "grab mode while released"
                );
                assert!(cursor.visible, "the cursor shows while released");
            }
        }
    }

    #[test]
    fn grab_follows_escape_click_and_editor() {
        assert_eq!(GRABBED_MODE, CursorGrabMode::Locked);
        assert_eq!(RELEASE_KEY, KeyCode::Escape);
        assert_eq!(GRAB_BUTTON, MouseButton::Left);
        let mut rig = Rig::new();

        // Playing: grabbed from the first frame; the mouse turns the view
        // and a held click swings.
        rig.assert_grabbed(true);
        assert!(rig.grab().swing_allowed());
        assert!(rig.turn() < 0.0, "mouse right turns right while grabbed");
        rig.button(true);
        rig.frames(30);
        assert!(rig.swings() >= 1, "holding the button swings while grabbed");
        rig.button(false);
        rig.frames(30);

        // Escape releases; the mouse no longer turns the view.
        rig.tap(RELEASE_KEY);
        rig.assert_grabbed(false);
        assert_eq!(rig.turn(), 0.0, "no mouse look while released");
        // Escape again keeps it released.
        rig.tap(RELEASE_KEY);
        rig.assert_grabbed(false);

        // A click grabs again on its own frame, without swinging, however
        // long it is held.
        let swings = rig.swings();
        rig.button(true);
        rig.frame();
        rig.assert_grabbed(true);
        assert!(
            !rig.grab().swing_allowed(),
            "the grabbing press does not swing"
        );
        rig.frames(60);
        assert_eq!(rig.swings(), swings, "the grabbing click never swings");
        assert!(rig.turn() < 0.0, "mouse look is back with the grab");
        // Let go and press again: that press swings.
        rig.button(false);
        rig.frame();
        assert!(rig.grab().swing_allowed());
        rig.button(true);
        rig.frames(30);
        assert!(rig.swings() > swings, "a later press swings");
        rig.button(false);
        rig.frames(30);

        // F1 opens the editor and releases; clicks then do not grab, and
        // the mouse does not turn the view.
        rig.tap(TOGGLE_EDITOR_KEY);
        assert!(rig.editor_open());
        rig.assert_grabbed(false);
        rig.button(true);
        rig.frame();
        rig.button(false);
        rig.frame();
        rig.assert_grabbed(false);
        assert_eq!(rig.turn(), 0.0, "no mouse look with the editor open");
        // F1 closes it and grabs.
        rig.tap(TOGGLE_EDITOR_KEY);
        assert!(!rig.editor_open());
        rig.assert_grabbed(true);
        assert!(rig.turn() < 0.0);

        // The same through EditorState (the egui panel's close button).
        rig.set_editor_open(true);
        rig.frames(2);
        rig.assert_grabbed(false);
        rig.set_editor_open(false);
        rig.frames(2);
        rig.assert_grabbed(true);

        // Released by Escape, then the editor opened and closed: grabbed.
        rig.tap(RELEASE_KEY);
        rig.assert_grabbed(false);
        rig.tap(TOGGLE_EDITOR_KEY);
        rig.assert_grabbed(false);
        rig.tap(TOGGLE_EDITOR_KEY);
        rig.assert_grabbed(true);

        // Closing the editor with the button held does not swing until it
        // is let go.
        rig.tap(TOGGLE_EDITOR_KEY);
        let swings = rig.swings();
        rig.button(true);
        rig.frame();
        rig.tap(TOGGLE_EDITOR_KEY);
        rig.assert_grabbed(true);
        rig.frames(30);
        assert_eq!(
            rig.swings(),
            swings,
            "a press held through closing does not swing"
        );
        rig.button(false);
        rig.frame();
        assert!(rig.grab().swing_allowed());

        // Losing focus releases; regaining it does not grab, a click does.
        rig.set_focused(false);
        rig.frame();
        rig.assert_grabbed(false);
        rig.set_focused(true);
        rig.frames(2);
        rig.assert_grabbed(false);
        rig.button(true);
        rig.frame();
        rig.assert_grabbed(true);
        rig.button(false);
        rig.frame();

        // The fly camera turns only while grabbed too.
        rig.app.world_mut().resource_mut::<FlyCam>().enabled = true;
        rig.frame();
        let before = rig.camera_rotation();
        rig.turn();
        let turned = rig.camera_rotation();
        assert!(
            turned.angle_between(before) > 1e-3,
            "fly camera turns while grabbed"
        );
        rig.tap(RELEASE_KEY);
        rig.assert_grabbed(false);
        rig.turn();
        assert_eq!(
            rig.camera_rotation(),
            turned,
            "fly camera does not turn while released"
        );
    }
}
