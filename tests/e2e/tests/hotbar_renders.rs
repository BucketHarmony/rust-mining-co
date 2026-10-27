//! The hotbar (owner extension, `docs/ITEMS.md` I2): with items
//! (`TestConfig::items`) the player joins with the starting kit, the HUD
//! shows a row of hotbar chips at the bottom with slot 1 outlined, and the
//! pickaxe is in hand. Pressing 2 selects the shovel: the outline moves,
//! the view model becomes the shovel and swings with the shovel's profile.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the hotbar set
//! up (bag, selection, chips, held model, profile) and that the frames
//! differ; the screenshots are for the playtester.

use bevy::prelude::*;
use rmc_client::ui::UiTheme;
use rmc_client::viewmodel::InHand;
use rmc_client::{
    ClientBag, ClientItems, ControllerConfig, Held, HotbarChip, HotbarHud, HotbarSlot, MeshQueue,
    MeshStats, Pickaxe, ViewModel, ViewModelPlugin,
};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::items::HeldModel;
use rmc_world::materials::Phase;

const NAME: &str = "hotbar_renders";

#[test]
#[ignore]
fn hotbar_renders() {
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ui: true,
        items: true,
        tiles: true,
        size: (1280, 720),
        ..TestConfig::default()
    });
    // Not in the testkit's default plugins (see `pickaxe_in_hand`).
    ViewModelPlugin.build(t.app_mut());
    e2e::run_scenario(&mut t, NAME, play);
}

fn settle(t: &mut TestApp) {
    for _ in 0..400 {
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

/// The shown chips and the outlined ones, by slot.
fn chips(t: &mut TestApp) -> (Vec<u8>, Vec<u8>) {
    let accent = t.app().world().resource::<UiTheme>().colors.accent;
    let world = t.app_mut().world_mut();
    let mut q = world.query::<(&HotbarChip, &Node, &BorderColor)>();
    let mut shown = Vec::new();
    let mut outlined = Vec::new();
    for (chip, node, border) in q.iter(world) {
        if node.display != Display::None {
            shown.push(chip.0);
        }
        if *border == BorderColor::all(accent) {
            outlined.push(chip.0);
        }
    }
    shown.sort();
    outlined.sort();
    (shown, outlined)
}

fn in_hand(t: &mut TestApp) -> (InHand, Visibility) {
    let world = t.app_mut().world_mut();
    let mut q = world.query::<(&ViewModel, &Visibility)>();
    let (m, v) = q.single(world).expect("one view model");
    (m.in_hand, *v)
}

fn play(t: &mut TestApp) {
    settle(t);
    let bounds = t.config().world_bounds;
    let (x, z) = (
        (bounds.min.0.x + bounds.max.0.x) / 2,
        (bounds.min.0.z + bounds.max.0.z) / 2,
    );
    let world = t.host_world();
    let top = (bounds.min.0.y..=bounds.max.0.y)
        .rev()
        .find(|&y| {
            world
                .get_cell(CellPos::new(x, y, z))
                .and_then(|id| world.materials().get(id))
                .is_some_and(|d| d.phase == Phase::Solid)
        })
        .expect("a surface");
    t.spawn_player(Vec3::new(
        (x as f32 + 0.5) * CELL_SIZE_M,
        (top + 1) as f32 * CELL_SIZE_M,
        (z as f32 + 0.5) * CELL_SIZE_M,
    ));
    let config = t.app().world().resource::<ControllerConfig>().clone();
    t.look(0.0, 0.5 / config.sensitivity_rad);
    t.steps(30);
    settle(t);

    // The host sent the starting kit; the row shows every hotbar slot with
    // the first outlined, and the pickaxe is in hand.
    let bag = t
        .app()
        .world()
        .resource::<ClientBag>()
        .0
        .clone()
        .expect("the host sent a bag");
    let slots = bag.hotbar().len() as u8;
    let hud_shown = {
        let world = t.app_mut().world_mut();
        let mut q = world.query_filtered::<&Node, With<HotbarHud>>();
        q.single(world).unwrap().display != Display::None
    };
    assert!(hud_shown, "the hotbar row shows");
    assert_eq!(chips(t), ((0..slots).collect(), vec![0]));
    assert_eq!(
        in_hand(t),
        (InHand::Tool(HeldModel::Pickaxe), Visibility::Inherited)
    );
    let first = t.screenshot();
    e2e::save_screenshot(&first, NAME, t.frame());

    // 2 selects the shovel.
    t.hold(KeyCode::Digit2, 1);
    t.steps(3);
    assert_eq!(t.app().world().resource::<HotbarSlot>().0, 1);
    assert_eq!(chips(t).1, vec![1], "the outline moved");
    let held = t.app().world().resource::<Held>().clone();
    assert_eq!(held.model, Some(HeldModel::Shovel));
    assert_eq!(in_hand(t).0, InHand::Tool(HeldModel::Shovel));
    let shovel = *t
        .app()
        .world()
        .resource::<ClientItems>()
        .tools
        .profile("shovel")
        .unwrap();
    assert_eq!(t.app().world().resource::<Pickaxe>().0, shovel);
    let second = t.screenshot();
    e2e::save_screenshot(&second, NAME, t.frame());
    assert!(e2e::rms(&first, &second) > 0.0, "the frames differ");
}
