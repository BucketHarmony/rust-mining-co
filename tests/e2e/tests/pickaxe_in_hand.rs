//! The pickaxe in hand (owner extension, `rmc_client::viewmodel`): a player
//! on the surface sees the pickaxe at the lower right, and a swing at the
//! ground chops it forward. Asserts what the view model set up (rule 21)
//! and saves a shot at rest and one mid-chop for the playtester.

use bevy::prelude::*;
use rmc_client::{
    ControllerConfig, GameCamera, MeshQueue, MeshStats, SWING_BUTTON, ViewModel, ViewModelPlugin,
};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::materials::Phase;

const NAME: &str = "pickaxe_in_hand";

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

/// The view model's transform and visibility.
fn model(t: &mut TestApp) -> (Transform, Visibility, Entity) {
    let world = t.app_mut().world_mut();
    world
        .query_filtered::<(&Transform, &Visibility, &ChildOf), With<ViewModel>>()
        .single(world)
        .map(|(t, v, c)| (*t, *v, c.parent()))
        .expect("one view model")
}

#[test]
#[ignore]
fn pickaxe_in_hand() {
    let mut t = TestApp::new(TestConfig::default());
    // Not in the testkit's default plugins, so other scenarios' shots stay
    // as they were; built in place, the app having finished its plugins.
    ViewModelPlugin.build(t.app_mut());
    e2e::run_scenario(&mut t, NAME, play);
}

fn play(t: &mut TestApp) {
    settle(t);
    // Stand on the highest solid cell of the middle column.
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
    let feet = Vec3::new(
        (x as f32 + 0.5) * CELL_SIZE_M,
        (top + 1) as f32 * CELL_SIZE_M,
        (z as f32 + 0.5) * CELL_SIZE_M,
    );
    t.spawn_player(feet);
    // Look down at the ground, inside the pickaxe's reach.
    let config = t.app().world().resource::<ControllerConfig>().clone();
    t.look(0.0, 0.9 / config.sensitivity_rad);
    t.steps(30);
    settle(t);

    let (rest, visibility, parent) = model(t);
    assert_eq!(visibility, Visibility::Inherited, "shown for a player");
    assert!(
        t.app().world().get::<GameCamera>(parent).is_some(),
        "rides the game camera"
    );
    assert_eq!(rest, rmc_client::viewmodel::rest_transform());
    let shot = t.screenshot();
    e2e::save_screenshot(&shot, NAME, t.frame());

    t.mouse_down(SWING_BUTTON);
    let mut chopped = None;
    for _ in 0..30 {
        t.step();
        let (now, _, _) = model(t);
        let angle = now.rotation.angle_between(rest.rotation);
        if angle > rmc_client::viewmodel::SWING_ANGLE * 0.6 {
            chopped = Some(angle);
            break;
        }
    }
    t.mouse_up(SWING_BUTTON);
    let angle = chopped.expect("the pickaxe chops on a swing");
    eprintln!("chopped {angle} rad");
    let mid = t.screenshot();
    e2e::save_screenshot(&mid, NAME, t.frame());
    assert!(e2e::rms(&shot, &mid) > 0.0, "the chop changes the frame");
}
