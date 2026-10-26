//! Screens: the game menu and settings, as a stack.
//!
//! [`UiScreens`] is a stack of [`Screen`]s; the top one is drawn on the
//! menu layer over a scrim, the rest wait under it. While any screen is
//! open, play is blocked: the cursor stays free and a click does not grab
//! it ([`crate::cursor`] treats an open screen like the open editor).
//!
//! - Releasing the cursor while playing (Escape, or the window losing
//!   focus) opens the game menu, unless the editor is open.
//! - Escape closes the top screen; closing the last one grabs the cursor
//!   again. Without a [`CursorGrab`] (tests, the test kit), Escape opens
//!   the menu instead.
//! - Tab / Shift+Tab and the Up / Down arrows move the keyboard focus
//!   within the screen; Enter or Space activates; the first button is
//!   focused when a screen opens.
//!
//! Buttons say what they do with a [`UiAction`]; [`on_activate`] is the
//! one handler.

use bevy::app::AppExit;
use bevy::input_focus::tab_navigation::{NavAction, TabGroup, TabNavigation};
use bevy::input_focus::{AutoFocus, FocusCause, InputFocus};
use bevy::prelude::*;
use bevy::ui_widgets::Activate;

use super::layers::{UiLayer, layer_root};
use super::scale::UiSettings;
use super::theme::{ColorToken, TextRole, UiFonts, UiTheme};
use super::widgets::{ButtonStyle, UiAction, button, panel, text};
use crate::cursor::{CursorGrab, RELEASE_KEY};
use crate::editor::EditorState;

/// The game's title on the menu.
pub const MENU_TITLE: &str = "Rust Mining Co";

/// A screen of the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Screen {
    /// Resume, Settings, Quit.
    Menu,
    /// The UI scale.
    Settings,
}

/// The open screens, bottom first.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct UiScreens {
    stack: Vec<Screen>,
}

impl UiScreens {
    pub fn top(&self) -> Option<Screen> {
        self.stack.last().copied()
    }

    pub fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
    }

    pub fn pop(&mut self) -> Option<Screen> {
        self.stack.pop()
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }

    /// Whether a screen is open, so play (look, swing, grab) waits.
    pub fn blocks_play(&self) -> bool {
        !self.stack.is_empty()
    }
}

/// The root node of the drawn screen.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRoot(pub Screen);

/// The settings screen's UI scale readout.
#[derive(Component, Clone, Copy, Debug)]
pub struct ScaleReadout;

/// Opens and closes screens from Escape and the cursor grab.
pub(crate) fn screen_keys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    grab: Option<Res<CursorGrab>>,
    editor: Option<Res<EditorState>>,
    mut screens: ResMut<UiScreens>,
    mut was_grabbed: Local<Option<bool>>,
) {
    let escape = keys.is_some_and(|k| k.just_pressed(RELEASE_KEY));
    let editor_open = editor.is_some_and(|e| e.open);
    let Some(grab) = grab else {
        if escape && screens.pop().is_none() {
            screens.push(Screen::Menu);
        }
        return;
    };
    let grabbed = grab.grabbed();
    let released = *was_grabbed == Some(true) && !grabbed;
    *was_grabbed = Some(grabbed);
    if escape && screens.blocks_play() {
        screens.pop();
    } else if released && !editor_open && !screens.blocks_play() {
        screens.push(Screen::Menu);
    }
}

/// Up and Down move the focus like Shift+Tab and Tab.
pub(crate) fn arrow_navigation(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    nav: TabNavigation,
    focus: Option<ResMut<InputFocus>>,
) {
    let (Some(keys), Some(mut focus)) = (keys, focus) else {
        return;
    };
    let action = if keys.just_pressed(KeyCode::ArrowDown) {
        NavAction::Next
    } else if keys.just_pressed(KeyCode::ArrowUp) {
        NavAction::Previous
    } else {
        return;
    };
    if let Ok(next) = nav.navigate(&focus, action) {
        focus.set(next, FocusCause::Navigated);
    }
}

/// Draws the top screen when the stack changes.
pub(crate) fn sync_screens(
    mut commands: Commands,
    screens: Res<UiScreens>,
    theme: Res<UiTheme>,
    fonts: Res<UiFonts>,
    settings: Res<UiSettings>,
    drawn: Query<(Entity, &ScreenRoot)>,
    layers: Query<(Entity, &UiLayer)>,
) {
    let top = screens.top();
    let current = drawn.iter().next().map(|(_, s)| s.0);
    if current == top && !fonts.is_changed() {
        return;
    }
    let Some(menu) = layer_root(&layers, UiLayer::Menu) else {
        return;
    };
    for (e, _) in &drawn {
        commands.entity(e).despawn();
    }
    let Some(screen) = top else {
        return;
    };
    let (t, f) = (&*theme, &*fonts);
    let root = commands
        .spawn((
            ScreenRoot(screen),
            Node {
                position_type: PositionType::Absolute,
                left: Val::ZERO,
                right: Val::ZERO,
                top: Val::ZERO,
                bottom: Val::ZERO,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(t.colors.scrim),
            ChildOf(menu),
        ))
        .id();
    let body = commands
        .spawn((panel(t), TabGroup::modal(), ChildOf(root)))
        .id();
    let title = |s: &'static str| text(t, f, TextRole::Title, ColorToken::Text, s);
    let secondary = |a, s: &'static str| button(t, f, ButtonStyle::Secondary, a, s);
    let primary = |a, s: &'static str| button(t, f, ButtonStyle::Primary, a, s);
    match screen {
        Screen::Menu => {
            commands.spawn((title(MENU_TITLE), ChildOf(body)));
            commands.spawn((
                primary(UiAction::Resume, "Resume"),
                AutoFocus,
                ChildOf(body),
            ));
            commands.spawn((secondary(UiAction::OpenSettings, "Settings"), ChildOf(body)));
            commands.spawn((secondary(UiAction::Quit, "Save and quit"), ChildOf(body)));
        }
        Screen::Settings => {
            let readout = scale_label(&settings);
            commands.spawn((title("Settings"), ChildOf(body)));
            commands.spawn((
                text(t, f, TextRole::Caption, ColorToken::TextDim, "UI scale"),
                ChildOf(body),
            ));
            commands.spawn((
                text(t, f, TextRole::Label, ColorToken::Text, readout),
                ScaleReadout,
                ChildOf(body),
            ));
            commands.spawn((
                secondary(UiAction::ScaleUp, "Larger"),
                AutoFocus,
                ChildOf(body),
            ));
            commands.spawn((secondary(UiAction::ScaleDown, "Smaller"), ChildOf(body)));
            commands.spawn((
                secondary(UiAction::ScaleReset, "Default size"),
                ChildOf(body),
            ));
            commands.spawn((primary(UiAction::Back, "Back"), ChildOf(body)));
        }
    }
}

/// The scale readout's text.
pub fn scale_label(settings: &UiSettings) -> String {
    format!("{:.0}%", settings.scale * 100.0)
}

pub(crate) fn update_scale_readout(
    settings: Res<UiSettings>,
    mut readouts: Query<&mut Text, With<ScaleReadout>>,
) {
    if !settings.is_changed() {
        return;
    }
    for mut t in &mut readouts {
        t.0 = scale_label(&settings);
    }
}

/// Does what an activated button's [`UiAction`] says.
pub(crate) fn on_activate(
    ev: On<Activate>,
    actions: Query<&UiAction>,
    theme: Res<UiTheme>,
    mut screens: ResMut<UiScreens>,
    mut settings: ResMut<UiSettings>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(action) = actions.get(ev.entity) else {
        return;
    };
    info!(?action, "ui");
    match action {
        UiAction::Resume => screens.clear(),
        UiAction::OpenSettings => screens.push(Screen::Settings),
        UiAction::Back => {
            screens.pop();
        }
        UiAction::Quit => {
            exit.write(AppExit::Success);
        }
        UiAction::ScaleUp => settings.step_scale(&theme, 1),
        UiAction::ScaleDown => settings.step_scale(&theme, -1),
        UiAction::ScaleReset => *settings = UiSettings::new(&theme),
    }
}
