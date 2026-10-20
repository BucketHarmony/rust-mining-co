//! Input injection (SAD §12.2).
//!
//! Keys and mouse are written as the same `KeyboardInput`, `MouseButtonInput`
//! and `MouseMotion` messages winit produces, before the next `update()`.
//! The game's `InputPlugin` turns them into `ButtonInput<KeyCode>`,
//! `ButtonInput<MouseButton>` and `AccumulatedMouseMotion` in `PreUpdate`,
//! so the game sees injected input exactly as it sees real input.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::input::mouse::{MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel};
use bevy::input::touch::TouchPhase;
use bevy::prelude::*;

use crate::app::TestApp;

impl TestApp {
    /// Presses `key` from the next step on. It stays held until
    /// [`TestApp::release`].
    pub fn press(&mut self, key: KeyCode) {
        self.write_key(key, ButtonState::Pressed);
    }

    /// Releases `key` at the next step.
    pub fn release(&mut self, key: KeyCode) {
        self.write_key(key, ButtonState::Released);
    }

    /// Holds `key` for exactly `frames` steps, then queues its release for
    /// the step after. With 0 frames, press and release arrive in the same
    /// step.
    pub fn hold(&mut self, key: KeyCode, frames: u32) {
        self.press(key);
        self.steps(frames);
        self.release(key);
    }

    /// Moves the mouse by (`dx`, `dy`) during the next step. Several calls
    /// before one step add up.
    pub fn look(&mut self, dx: f32, dy: f32) {
        self.app_mut().world_mut().write_message(MouseMotion {
            delta: Vec2::new(dx, dy),
        });
    }

    /// Turns the mouse wheel by `notches` lines during the next step:
    /// positive is up (away from the user), negative down. Several calls
    /// before one step add up.
    pub fn scroll(&mut self, notches: f32) {
        self.app_mut().world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: notches,
            // There is no window; nothing in the game reads this field.
            window: Entity::PLACEHOLDER,
            phase: TouchPhase::Moved,
        });
    }

    /// Presses `button` from the next step on, until [`TestApp::mouse_up`].
    pub fn mouse_down(&mut self, button: MouseButton) {
        self.write_button(button, ButtonState::Pressed);
    }

    /// Releases `button` at the next step.
    pub fn mouse_up(&mut self, button: MouseButton) {
        self.write_button(button, ButtonState::Released);
    }

    /// Holds `button` for one step, then queues its release for the step
    /// after.
    pub fn click(&mut self, button: MouseButton) {
        self.mouse_down(button);
        self.step();
        self.mouse_up(button);
    }

    fn write_key(&mut self, key_code: KeyCode, state: ButtonState) {
        let logical_key = logical_key(key_code);
        let text = match (&logical_key, state) {
            (Key::Character(c), ButtonState::Pressed) => Some(c.clone()),
            (Key::Space, ButtonState::Pressed) => Some(" ".into()),
            _ => None,
        };
        self.app_mut().world_mut().write_message(KeyboardInput {
            key_code,
            logical_key,
            state,
            text,
            repeat: false,
            // There is no window; nothing in the game reads this field.
            window: Entity::PLACEHOLDER,
        });
    }

    fn write_button(&mut self, button: MouseButton, state: ButtonState) {
        self.app_mut().world_mut().write_message(MouseButtonInput {
            button,
            state,
            window: Entity::PLACEHOLDER,
        });
    }
}

/// Keys that type a character on a US layout.
const CHARACTER_KEYS: [(KeyCode, char); 41] = [
    (KeyCode::KeyA, 'a'),
    (KeyCode::KeyB, 'b'),
    (KeyCode::KeyC, 'c'),
    (KeyCode::KeyD, 'd'),
    (KeyCode::KeyE, 'e'),
    (KeyCode::KeyF, 'f'),
    (KeyCode::KeyG, 'g'),
    (KeyCode::KeyH, 'h'),
    (KeyCode::KeyI, 'i'),
    (KeyCode::KeyJ, 'j'),
    (KeyCode::KeyK, 'k'),
    (KeyCode::KeyL, 'l'),
    (KeyCode::KeyM, 'm'),
    (KeyCode::KeyN, 'n'),
    (KeyCode::KeyO, 'o'),
    (KeyCode::KeyP, 'p'),
    (KeyCode::KeyQ, 'q'),
    (KeyCode::KeyR, 'r'),
    (KeyCode::KeyS, 's'),
    (KeyCode::KeyT, 't'),
    (KeyCode::KeyU, 'u'),
    (KeyCode::KeyV, 'v'),
    (KeyCode::KeyW, 'w'),
    (KeyCode::KeyX, 'x'),
    (KeyCode::KeyY, 'y'),
    (KeyCode::KeyZ, 'z'),
    (KeyCode::Digit0, '0'),
    (KeyCode::Digit1, '1'),
    (KeyCode::Digit2, '2'),
    (KeyCode::Digit3, '3'),
    (KeyCode::Digit4, '4'),
    (KeyCode::Digit5, '5'),
    (KeyCode::Digit6, '6'),
    (KeyCode::Digit7, '7'),
    (KeyCode::Digit8, '8'),
    (KeyCode::Digit9, '9'),
    (KeyCode::BracketLeft, '['),
    (KeyCode::BracketRight, ']'),
    (KeyCode::Minus, '-'),
    (KeyCode::Equal, '='),
    (KeyCode::Period, '.'),
];

/// The logical key a US layout gives for `code`, so `ButtonInput<Key>` and
/// text input (egui) see what winit would send. Other keys are
/// `Unidentified`; the game reads physical `KeyCode`s (SAD §11).
fn logical_key(code: KeyCode) -> Key {
    if let Some(&(_, c)) = CHARACTER_KEYS.iter().find(|(k, _)| *k == code) {
        return Key::Character(c.to_string().into());
    }
    match code {
        KeyCode::Space => Key::Space,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Escape => Key::Escape,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Key::Shift,
        KeyCode::ControlLeft | KeyCode::ControlRight => Key::Control,
        KeyCode::AltLeft | KeyCode::AltRight => Key::Alt,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        _ => Key::Unidentified(NativeKey::Unidentified),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TestConfig;
    use bevy::input::mouse::AccumulatedMouseMotion;
    use rmc_world::coords::{CellAabb, CellPos};

    /// One chunk column, logic only: input needs no renderer.
    fn logic_app() -> TestApp {
        TestApp::new(TestConfig {
            seed: 5,
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 63, 31)),
            render: false,
            ..TestConfig::default()
        })
    }

    /// (pressed, just_pressed, just_released) as the game saw them in the
    /// last step. `ButtonInput` is cleared at the start of the next step's
    /// `PreUpdate`, so it still holds the last frame's state here.
    fn key(t: &TestApp, k: KeyCode) -> (bool, bool, bool) {
        let input = t.app().world().resource::<ButtonInput<KeyCode>>();
        (
            input.pressed(k),
            input.just_pressed(k),
            input.just_released(k),
        )
    }

    fn logical(t: &TestApp, k: &Key) -> bool {
        t.app()
            .world()
            .resource::<ButtonInput<Key>>()
            .pressed(k.clone())
    }

    fn mouse(t: &TestApp, b: MouseButton) -> (bool, bool, bool) {
        let input = t.app().world().resource::<ButtonInput<MouseButton>>();
        (
            input.pressed(b),
            input.just_pressed(b),
            input.just_released(b),
        )
    }

    fn motion(t: &TestApp) -> Vec2 {
        t.app().world().resource::<AccumulatedMouseMotion>().delta
    }

    /// Pressed state of `k` as seen by an `Update` system, one entry per
    /// frame, so the test checks what game systems read.
    #[derive(Resource, Default)]
    struct Seen(Vec<bool>);

    fn watch_space(input: Res<ButtonInput<KeyCode>>, mut seen: ResMut<Seen>) {
        seen.0.push(input.pressed(KeyCode::Space));
    }

    const UP: (bool, bool, bool) = (false, false, false);
    const DOWN: (bool, bool, bool) = (true, true, false);
    const HELD: (bool, bool, bool) = (true, false, false);
    const RELEASED: (bool, bool, bool) = (false, false, true);

    #[test]
    fn press_sets_button_input() {
        let mut t = logic_app();
        let w = KeyCode::KeyW;
        t.step();
        assert_eq!(key(&t, w), UP, "nothing pressed yet");

        // press: nothing changes until the next step, then held until release.
        t.press(w);
        assert_eq!(key(&t, w), UP, "press takes effect at the next step");
        t.step();
        assert_eq!(key(&t, w), DOWN, "first frame: just pressed");
        assert!(logical(&t, &Key::Character("w".into())), "logical key too");
        for i in 0..10 {
            t.step();
            assert_eq!(key(&t, w), HELD, "held frame {i}");
        }
        t.release(w);
        t.step();
        assert_eq!(key(&t, w), RELEASED);
        assert!(!logical(&t, &Key::Character("w".into())));
        t.step();
        assert_eq!(key(&t, w), UP);

        // Two keys at once are independent.
        t.press(KeyCode::KeyA);
        t.press(KeyCode::ShiftLeft);
        t.step();
        assert_eq!(key(&t, KeyCode::KeyA), DOWN);
        assert_eq!(key(&t, KeyCode::ShiftLeft), DOWN);
        assert!(logical(&t, &Key::Shift));
        t.release(KeyCode::KeyA);
        t.step();
        assert_eq!(key(&t, KeyCode::KeyA), RELEASED);
        assert_eq!(key(&t, KeyCode::ShiftLeft), HELD);
        t.release(KeyCode::ShiftLeft);
        t.step();
        assert_eq!(key(&t, KeyCode::ShiftLeft), RELEASED);

        // hold: an Update system sees the key on exactly `frames` frames.
        t.app_mut()
            .init_resource::<Seen>()
            .add_systems(Update, watch_space);
        t.hold(KeyCode::Space, 4);
        assert_eq!(key(&t, KeyCode::Space), HELD, "last of the 4 frames");
        assert!(logical(&t, &Key::Space));
        t.step();
        assert_eq!(key(&t, KeyCode::Space), RELEASED, "released after hold");
        t.step();
        let seen = &t.app().world().resource::<Seen>().0;
        assert_eq!(seen, &[true, true, true, true, false, false]);

        // hold for 0 frames presses and releases on the same step: the game
        // sees both edges, but never a held key.
        t.hold(KeyCode::KeyE, 0);
        t.step();
        assert_eq!(key(&t, KeyCode::KeyE), (false, true, true));

        // Mouse buttons work the same way.
        let left = MouseButton::Left;
        t.mouse_down(left);
        assert_eq!(mouse(&t, left), UP);
        t.step();
        assert_eq!(mouse(&t, left), DOWN);
        t.steps(3);
        assert_eq!(mouse(&t, left), HELD);
        t.mouse_up(left);
        t.step();
        assert_eq!(mouse(&t, left), RELEASED);
        t.step();
        assert_eq!(mouse(&t, left), UP);

        // click: held for exactly one frame, released on the next.
        let right = MouseButton::Right;
        t.click(right);
        assert_eq!(mouse(&t, right), DOWN, "click runs its pressed frame");
        t.step();
        assert_eq!(mouse(&t, right), RELEASED);
        t.step();
        assert_eq!(mouse(&t, right), UP);
        assert_eq!(mouse(&t, left), UP, "other buttons untouched");
    }

    #[test]
    fn look_accumulates_mouse_motion() {
        let mut t = logic_app();
        t.step();
        assert_eq!(motion(&t), Vec2::ZERO);

        t.look(3.0, -2.0);
        assert_eq!(motion(&t), Vec2::ZERO, "look takes effect at the next step");
        t.step();
        assert_eq!(motion(&t), Vec2::new(3.0, -2.0));

        // No look: the accumulated motion resets for the frame.
        t.step();
        assert_eq!(motion(&t), Vec2::ZERO);

        // Several looks before one step add up into that one frame.
        t.look(1.5, 0.25);
        t.look(-4.0, 10.0);
        t.look(0.5, 0.75);
        t.step();
        assert_eq!(motion(&t), Vec2::new(-2.0, 11.0));
        t.step();
        assert_eq!(motion(&t), Vec2::ZERO);

        // One look per frame is one frame's motion each, never carried over.
        for i in 1..=5 {
            t.look(i as f32, -(i as f32));
            t.step();
            assert_eq!(motion(&t), Vec2::new(i as f32, -(i as f32)), "frame {i}");
        }
        t.step();
        assert_eq!(motion(&t), Vec2::ZERO);

        // Looking does not press anything.
        let keys = t.app().world().resource::<ButtonInput<KeyCode>>();
        assert_eq!(keys.get_pressed().count(), 0);
        let buttons = t.app().world().resource::<ButtonInput<MouseButton>>();
        assert_eq!(buttons.get_pressed().count(), 0);
    }

    #[test]
    fn scroll_accumulates_mouse_wheel() {
        use bevy::input::mouse::AccumulatedMouseScroll;
        let scroll = |t: &TestApp| t.app().world().resource::<AccumulatedMouseScroll>().delta;
        let mut t = logic_app();
        t.step();
        assert_eq!(scroll(&t), Vec2::ZERO);

        t.scroll(-1.0);
        assert_eq!(
            scroll(&t),
            Vec2::ZERO,
            "scroll takes effect at the next step"
        );
        t.step();
        assert_eq!(scroll(&t), Vec2::new(0.0, -1.0));
        t.step();
        assert_eq!(scroll(&t), Vec2::ZERO, "one frame only");

        t.scroll(2.0);
        t.scroll(1.0);
        t.step();
        assert_eq!(scroll(&t), Vec2::new(0.0, 3.0));
        t.step();
        assert_eq!(scroll(&t), Vec2::ZERO);
    }
}
