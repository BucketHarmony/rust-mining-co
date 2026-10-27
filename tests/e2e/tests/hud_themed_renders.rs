//! The HUD in the UI kit's look: with the kit (`TestConfig::ui`), the
//! inventory, the readout and the target sit on `surface` chips inside the
//! safe area, in the kit's type ramp; a chip with nothing to show is hidden.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the HUD set up
//! (chip fills and visibility, text roles and sizes) and that the frames
//! render and differ; the screenshots are for the playtester.

use bevy::prelude::*;
use rmc_client::ui::{TextRole, UiText, UiTheme};
use rmc_client::{ClientInventory, HudChip, MeshQueue, MeshStats, Replica};
use rmc_testkit::bots::look_at;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::inventory::Inventory;
use rmc_world::materials::Phase;

#[test]
#[ignore]
fn hud_themed_renders() {
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ui: true,
        tiles: true,
        size: (1280, 720),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "hud_themed_renders", run);
}

fn settle(t: &mut TestApp) {
    for _ in 0..240 {
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

/// Each chip's fill and whether it shows.
fn chips(t: &mut TestApp) -> Vec<(HudChip, Color, bool)> {
    let world = t.app_mut().world_mut();
    let mut v: Vec<(HudChip, Color, bool)> = world
        .query::<(&HudChip, &BackgroundColor, &InheritedVisibility)>()
        .iter(world)
        .map(|(c, bg, v)| (*c, bg.0, v.get()))
        .collect();
    v.sort_by_key(|(c, ..)| *c as u8);
    v
}

fn shoot(t: &mut TestApp, label: &str) -> image::RgbaImage {
    t.steps(3);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("hud_themed_renders-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
    image
}

fn run(t: &mut TestApp) {
    settle(t);
    let theme = t.app().world().resource::<UiTheme>().clone();
    // A player on the ground, aiming at the ground a few cells ahead, so
    // the pickaxe targets a cell.
    let (x, z) = (32, 32);
    let surface = |t: &TestApp, x: i32, z: i32| {
        i32::from(
            t.app()
                .world()
                .resource::<Replica>()
                .world()
                .surface_y(x, z),
        )
    };
    let feet = Vec3::new(
        x as f32 + 0.5,
        (surface(t, x, z) + 1) as f32,
        z as f32 + 0.5,
    ) * CELL_SIZE_M;
    t.spawn_player(feet);
    t.steps(30);
    let (ax, az) = (x + 3, z + 2);
    let aim = Vec3::new(
        ax as f32 + 0.5,
        (surface(t, ax, az) + 1) as f32,
        az as f32 + 0.5,
    ) * CELL_SIZE_M;
    look_at(t, aim).unwrap_or_else(|e| panic!("aiming: {e}"));
    settle(t);
    assert!(t.hud().target.is_some(), "the crosshair is on a cell");

    // Empty-handed: no inventory chip.
    let before = chips(t);
    for (chip, fill, _) in &before {
        assert_eq!(*fill, theme.colors.surface, "{chip:?} is a surface");
    }
    assert!(!before[HudChip::Inventory as usize].2, "{before:?}");
    assert!(before[HudChip::Target as usize].2, "{before:?}");
    let empty = shoot(t, "empty");

    // Three solids in hand.
    let items: Vec<_> = t
        .host_world()
        .materials()
        .iter()
        .filter(|d| d.phase == Phase::Solid)
        .take(3)
        .zip([12, 5, 1])
        .map(|(d, n)| (d.id, n))
        .collect();
    t.app_mut().world_mut().resource_mut::<ClientInventory>().0 = Inventory::from_items(&items);
    t.steps(2);
    let after = chips(t);
    assert!(after[HudChip::Inventory as usize].2, "{after:?}");
    assert_eq!(t.hud().inventory.len(), 3);
    {
        let world = t.app_mut().world_mut();
        let roles: Vec<TextRole> = world
            .query::<&UiText>()
            .iter(world)
            .map(|u| u.role)
            .collect();
        for role in [TextRole::Caption, TextRole::Body, TextRole::Label] {
            assert!(roles.contains(&role), "{role:?} in {roles:?}");
        }
    }
    let held = shoot(t, "held");
    assert_ne!(empty, held, "the inventory chip shows");
}
