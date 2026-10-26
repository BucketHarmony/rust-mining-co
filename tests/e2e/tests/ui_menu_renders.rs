//! The UI kit (`rmc_client::ui`) on the rendered game: Escape opens the
//! game menu over a scrim with its first button focused, Settings replaces
//! it, and a toast shows at the top.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the kit set up
//! (screens, buttons, focus, layers, scale, theme fonts and colors) and
//! that the frames render and differ; the screenshots are for the
//! playtester.

use bevy::input_focus::InputFocus;
use bevy::prelude::*;
use bevy::ui_widgets::Activate;
use rmc_client::ui::{ScaleReadout, ScreenRoot, ToastEntry, UiFonts, effective_scale};
use rmc_client::{MeshQueue, MeshStats, Screen, Toast, UiAction, UiLayer, UiSettings, UiTheme};
use rmc_testkit::{TestApp, TestConfig, e2e};

#[test]
#[ignore]
fn ui_menu_renders() {
    let mut t = TestApp::new(TestConfig {
        ui: true,
        tiles: true,
        size: (1280, 720),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "ui_menu_renders", run);
}

fn settle(t: &mut TestApp) {
    for _ in 0..480 {
        t.step();
        let world = t.app().world();
        let stats = world.resource::<MeshStats>();
        if stats.initial_load_done
            && world.resource::<MeshQueue>().is_idle()
            && stats.awaiting_upload == 0
        {
            return;
        }
    }
    panic!("meshing did not settle");
}

fn shoot(t: &mut TestApp, label: &str) -> image::RgbaImage {
    t.steps(4);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("ui_menu_renders-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
    image
}

fn drawn(t: &mut TestApp) -> Option<Screen> {
    let world = t.app_mut().world_mut();
    let mut q = world.query::<&ScreenRoot>();
    q.iter(world).next().map(|s| s.0)
}

fn button(t: &mut TestApp, action: UiAction) -> Entity {
    let world = t.app_mut().world_mut();
    let mut q = world.query::<(Entity, &UiAction)>();
    q.iter(world)
        .find(|(_, a)| **a == action)
        .map(|(e, _)| e)
        .unwrap_or_else(|| panic!("no {action:?} button"))
}

fn run(t: &mut TestApp) {
    settle(t);
    let world = t.app_mut().world_mut();
    let mut q = world.query::<&UiLayer>();
    assert_eq!(
        q.iter(world).count(),
        UiLayer::ALL.len(),
        "the layers spawn"
    );
    let theme = world.resource::<UiTheme>().clone();
    let fonts = world.resource::<UiFonts>().clone();
    assert_ne!(fonts.regular, Handle::default(), "the theme font loads");
    let want = effective_scale(&theme, 720.0, world.resource::<UiSettings>());
    let scale = world.resource::<UiScale>().0;
    assert!(
        (scale - want).abs() < 1e-4,
        "UiScale {scale} follows 720p: {want}"
    );
    let play = shoot(t, "play");

    t.hold(KeyCode::Escape, 1);
    t.steps(2);
    assert_eq!(drawn(t), Some(Screen::Menu), "Escape opens the menu");
    let resume = button(t, UiAction::Resume);
    let focus = t.app().world().resource::<InputFocus>().get();
    assert_eq!(focus, Some(resume), "Resume has the focus");
    let fill = t.app().world().get::<BackgroundColor>(resume).unwrap().0;
    assert_eq!(
        fill, theme.colors.accent,
        "the primary button is the accent"
    );
    let ring = t.app().world().get::<Outline>(resume).unwrap().color;
    assert_eq!(ring, theme.colors.focus, "the focus ring shows");
    let menu = shoot(t, "menu");
    assert_ne!(play, menu, "the menu draws over the game");

    let settings = button(t, UiAction::OpenSettings);
    t.app_mut()
        .world_mut()
        .trigger(Activate { entity: settings });
    t.steps(2);
    assert_eq!(drawn(t), Some(Screen::Settings));
    let world = t.app_mut().world_mut();
    let mut q = world.query_filtered::<&Text, With<ScaleReadout>>();
    assert_eq!(q.single(world).unwrap().0, "100%");
    shoot(t, "settings");

    let back = button(t, UiAction::Back);
    t.app_mut().world_mut().trigger(Activate { entity: back });
    t.steps(2);
    assert_eq!(drawn(t), Some(Screen::Menu), "Back returns to the menu");
    let resume = button(t, UiAction::Resume);
    t.app_mut().world_mut().trigger(Activate { entity: resume });
    t.steps(2);
    assert_eq!(drawn(t), None, "Resume closes the menu");

    t.app_mut()
        .world_mut()
        .write_message(Toast::info("Seasons paused (P)"));
    t.steps(2);
    let world = t.app_mut().world_mut();
    let mut q = world.query::<&ToastEntry>();
    assert_eq!(q.iter(world).count(), 1, "the toast shows");
    shoot(t, "toast");
}
