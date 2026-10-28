//! The UI kit: theme, scaling, layers, widgets, screens and toasts.
//!
//! The design rules and the reasoning behind them are in `docs/UI.md`. In
//! short:
//!
//! - Every color, size and duration is a token in `assets/ui.ron`
//!   ([`UiTheme`]); widgets take roles and tokens, never raw values.
//! - Sizes are designed at 1080p and scaled to the viewport and the
//!   player's UI scale setting ([`scale`]).
//! - UI lives on fixed layers (HUD, menus, toasts); the HUD is inset by a
//!   safe area and placed by anchor ([`layers`]).
//! - Buttons are Bevy's accessible headless buttons with the kit's look;
//!   keyboard focus is always visible ([`widgets`]).
//! - Menus are a screen stack that pauses play ([`screens`]); notices are
//!   toasts ([`toast`]).
//! - The settings (scale, temperature unit) are kept on disk ([`prefs`]).

pub mod egui_theme;
pub mod layers;
pub mod prefs;
pub mod scale;
pub mod screens;
pub mod theme;
pub mod toast;
pub mod widgets;

use std::path::PathBuf;

use bevy::input_focus::InputFocus;
use bevy::input_focus::tab_navigation::TabNavigationPlugin;
use bevy::prelude::*;

use crate::hud::TemperatureUnit;

pub use layers::{HudAnchor, HudSlot, UiLayer};
pub use prefs::{SETTINGS_FILE, SavedSettings, SettingsFile, SettingsFilePlugin};
pub use scale::{UiSettings, effective_scale};
pub use screens::{
    MENU_TITLE, ScaleReadout, Screen, ScreenBody, ScreenRoot, UiScreens, UnitReadout, scale_label,
};
pub use theme::{ColorToken, TextRole, UiFonts, UiTheme, UiThemeError, contrast_ratio};
pub use toast::{Toast, ToastEntry, ToastStack, toast_alpha};
pub use widgets::{
    ButtonLook, ButtonStyle, ButtonVisual, UiAction, UiText, button, button_look, panel, text,
};

/// Systems of the UI kit.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct UiSystems;

/// Adds the UI kit with `theme`. Fonts are read from `assets_dir` when
/// given and the app has text assets; otherwise Bevy's default font draws.
pub struct UiKitPlugin {
    pub theme: UiTheme,
    pub assets_dir: Option<PathBuf>,
}

/// Where the kit's font files are read from, if anywhere.
#[derive(Resource)]
pub struct FontDir(pub Option<PathBuf>);

impl Plugin for UiKitPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<TabNavigationPlugin>() {
            app.add_plugins(TabNavigationPlugin);
        }
        app.insert_resource(self.theme.clone())
            .insert_resource(UiSettings::new(&self.theme))
            .init_resource::<TemperatureUnit>()
            .insert_resource(FontDir(self.assets_dir.clone()))
            .init_resource::<UiScreens>()
            .init_resource::<UiFonts>()
            .init_resource::<InputFocus>()
            .init_resource::<UiScale>()
            .add_message::<Toast>()
            .add_systems(Startup, (load_fonts, layers::spawn_layers))
            .add_systems(
                Update,
                (
                    layers::attach_to_camera,
                    layers::place_in_slots,
                    scale::update_scale,
                    screens::screen_keys,
                    screens::sync_screens,
                    screens::arrow_navigation,
                    screens::update_scale_readout,
                    screens::update_unit_readout,
                    toast::spawn_toast_stack,
                    toast::run_toasts,
                    toast::apply_transparency,
                    widgets::restyle_texts,
                    widgets::style_buttons,
                )
                    .chain()
                    .in_set(UiSystems),
            )
            .add_observer(screens::on_activate);
    }
}

fn load_fonts(
    theme: Res<UiTheme>,
    dir: Res<FontDir>,
    fonts: Option<ResMut<Assets<Font>>>,
    mut ui_fonts: ResMut<UiFonts>,
) {
    if let Some(dir) = &dir.0 {
        *ui_fonts = theme::load_fonts(&theme, dir, fonts.map(ResMut::into_inner));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::tests::THEME_PATH;
    use bevy::app::AppExit;
    use bevy::ui_widgets::Activate;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_message::<AppExit>()
            .add_plugins(UiKitPlugin {
                theme: UiTheme::load(THEME_PATH).unwrap(),
                assets_dir: None,
            });
        app.update();
        app
    }

    fn tap(app: &mut App, key: KeyCode) {
        // As `InputPlugin` would: pressed for one frame, then released.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        app.update();
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(key);
        keys.clear();
        app.update();
    }

    fn drawn(app: &mut App) -> Option<Screen> {
        let mut q = app.world_mut().query::<&ScreenRoot>();
        let all: Vec<Screen> = q.iter(app.world()).map(|s| s.0).collect();
        assert!(all.len() <= 1, "one screen drawn at a time: {all:?}");
        all.first().copied()
    }

    fn button_for(app: &mut App, action: UiAction) -> Entity {
        let mut q = app.world_mut().query::<(Entity, &UiAction)>();
        q.iter(app.world())
            .find(|(_, a)| **a == action)
            .map(|(e, _)| e)
            .unwrap_or_else(|| panic!("no {action:?} button"))
    }

    fn activate(app: &mut App, action: UiAction) {
        let entity = button_for(app, action);
        app.world_mut().trigger(Activate { entity });
        app.update();
    }

    fn focused(app: &App) -> Option<Entity> {
        app.world().resource::<InputFocus>().get()
    }

    #[test]
    fn layers_and_anchors_spawn_in_order() {
        let mut app = app();
        let mut q = app.world_mut().query::<(&UiLayer, &GlobalZIndex)>();
        let mut layers: Vec<(i32, UiLayer)> = q.iter(app.world()).map(|(l, z)| (z.0, *l)).collect();
        layers.sort_by_key(|(z, _)| *z);
        let order: Vec<UiLayer> = layers.iter().map(|(_, l)| *l).collect();
        assert_eq!(order, UiLayer::ALL, "HUD under menus under toasts");
        let mut q = app.world_mut().query::<&HudAnchor>();
        assert_eq!(q.iter(app.world()).count(), HudAnchor::ALL.len());
    }

    #[test]
    fn hud_slots_move_into_their_anchor() {
        let mut app = app();
        let item = app
            .world_mut()
            .spawn((Node::default(), HudSlot(HudAnchor::TopRight)))
            .id();
        app.update();
        let parent = app.world().get::<ChildOf>(item).expect("placed").parent();
        assert_eq!(
            app.world().get::<HudAnchor>(parent),
            Some(&HudAnchor::TopRight)
        );

        app.world_mut()
            .entity_mut(item)
            .insert(HudSlot(HudAnchor::Bottom));
        app.update();
        let parent = app.world().get::<ChildOf>(item).unwrap().parent();
        assert_eq!(
            app.world().get::<HudAnchor>(parent),
            Some(&HudAnchor::Bottom)
        );
    }

    #[test]
    fn settings_switch_the_temperature_unit_from_fahrenheit() {
        let mut app = app();
        let unit = |app: &App| *app.world().resource::<TemperatureUnit>();
        assert_eq!(unit(&app), TemperatureUnit::Fahrenheit, "the default");
        tap(&mut app, KeyCode::Escape);
        activate(&mut app, UiAction::OpenSettings);
        let readout = |app: &mut App| {
            let mut q = app.world_mut().query_filtered::<&Text, With<UnitReadout>>();
            q.single(app.world()).unwrap().0.clone()
        };
        assert_eq!(readout(&mut app), "Fahrenheit");
        activate(&mut app, UiAction::ToggleTemperatureUnit);
        assert_eq!(unit(&app), TemperatureUnit::Celsius);
        app.update();
        assert_eq!(readout(&mut app), "Celsius");
        activate(&mut app, UiAction::ToggleTemperatureUnit);
        assert_eq!(unit(&app), TemperatureUnit::Fahrenheit);
    }

    #[test]
    fn escape_opens_the_menu_and_buttons_drive_the_stack() {
        let mut app = app();
        assert_eq!(drawn(&mut app), None);

        tap(&mut app, KeyCode::Escape);
        assert_eq!(drawn(&mut app), Some(Screen::Menu));
        assert!(app.world().resource::<UiScreens>().blocks_play());
        let resume = button_for(&mut app, UiAction::Resume);
        assert_eq!(
            focused(&app),
            Some(resume),
            "the first button takes the focus"
        );

        activate(&mut app, UiAction::OpenSettings);
        assert_eq!(drawn(&mut app), Some(Screen::Settings));
        let before = app.world().resource::<UiSettings>().scale;
        activate(&mut app, UiAction::ScaleUp);
        let after = app.world().resource::<UiSettings>().scale;
        assert!(after > before);
        app.update();
        let mut q = app
            .world_mut()
            .query_filtered::<&Text, With<ScaleReadout>>();
        let readout = q.single(app.world()).unwrap().0.clone();
        assert_eq!(readout, scale_label(&UiSettings { scale: after }));
        activate(&mut app, UiAction::ScaleReset);
        assert_eq!(app.world().resource::<UiSettings>().scale, before);

        tap(&mut app, KeyCode::Escape);
        assert_eq!(drawn(&mut app), Some(Screen::Menu), "Escape goes back");
        activate(&mut app, UiAction::Resume);
        assert_eq!(drawn(&mut app), None);
        assert!(!app.world().resource::<UiScreens>().blocks_play());
    }

    #[test]
    fn quit_writes_app_exit() {
        let mut app = app();
        tap(&mut app, KeyCode::Escape);
        activate(&mut app, UiAction::Quit);
        let exits = app.world().resource::<Messages<AppExit>>();
        assert!(exits.iter_current_update_messages().count() + exits.len() > 0);
    }

    #[test]
    fn arrows_move_the_focus_within_the_screen() {
        let mut app = app();
        tap(&mut app, KeyCode::Escape);
        // Tab navigation skips hidden nodes, and without the render
        // plugins nothing propagates visibility: show the screen by hand.
        let mut q = app.world_mut().query::<&mut InheritedVisibility>();
        for mut v in q.iter_mut(app.world_mut()) {
            *v = InheritedVisibility::VISIBLE;
        }
        let resume = button_for(&mut app, UiAction::Resume);
        let settings = button_for(&mut app, UiAction::OpenSettings);
        let quit = button_for(&mut app, UiAction::Quit);
        tap(&mut app, KeyCode::ArrowDown);
        assert_eq!(focused(&app), Some(settings));
        tap(&mut app, KeyCode::ArrowUp);
        tap(&mut app, KeyCode::ArrowUp);
        assert_eq!(focused(&app), Some(quit), "wraps around");
        tap(&mut app, KeyCode::ArrowDown);
        assert_eq!(focused(&app), Some(resume));
    }

    #[test]
    fn toasts_fade_in_hold_fade_out_and_cap() {
        let theme = UiTheme::load(THEME_PATH).unwrap();
        let m = theme.motion;
        assert_eq!(toast_alpha(0.0, &m), Some(0.0));
        assert_eq!(toast_alpha(m.fast + m.toast * 0.5, &m), Some(1.0));
        let late = toast_alpha(m.fast + m.toast + m.normal * 0.5, &m).unwrap();
        assert!(late > 0.0 && late < 1.0);
        assert_eq!(toast_alpha(m.fast + m.toast + m.normal, &m), None);

        let mut app = app();
        for i in 0..theme.toast_max + 2 {
            app.world_mut()
                .write_message(Toast::info(format!("notice {i}")));
        }
        app.update();
        app.update();
        let mut q = app.world_mut().query::<&ToastEntry>();
        assert_eq!(
            q.iter(app.world()).count(),
            theme.toast_max,
            "the oldest go"
        );
        let mut q = app.world_mut().query::<(&ToastEntry, &ChildOf)>();
        let stacks: Vec<Entity> = q.iter(app.world()).map(|(_, p)| p.parent()).collect();
        let mut s = app.world_mut().query_filtered::<Entity, With<ToastStack>>();
        let stack = s.single(app.world()).unwrap();
        assert!(stacks.iter().all(|p| *p == stack));
    }
}
