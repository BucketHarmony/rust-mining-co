//! The character screen (owner extension, `docs/ITEMS.md` I3): with items
//! and the UI kit, `I` opens the character screen over the game. It shows
//! a slot for each hotbar, pack and equipment slot, the worn clothes in the
//! equipment slots. A drag of the pickaxe onto a pack slot (a
//! `BagGesture`, as the pointer observers write it) is predicted, sent as
//! `MoveItem`, and the host's bag agrees; a drop the bag refuses changes
//! neither bag. `I` closes the screen.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the screen set
//! up (screen, slots, texts, bags) and that the frames differ; the
//! screenshots are for the playtester.

use bevy::prelude::*;
use rmc_client::{
    BagGesture, BagSlotUi, ClientBag, ClientItems, MeshQueue, MeshStats, Screen, UiScreens,
};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::items::{EquipSlot, SlotRef, Thing};
use rmc_world::materials::Phase;

const NAME: &str = "character_screen_renders";

#[test]
#[ignore]
fn character_screen_renders() {
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ui: true,
        items: true,
        tiles: true,
        size: (1280, 720),
        ..TestConfig::default()
    });
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

fn top(t: &TestApp) -> Option<Screen> {
    t.app().world().resource::<UiScreens>().top()
}

fn client_bag(t: &TestApp) -> rmc_world::items::Bag {
    t.app()
        .world()
        .resource::<ClientBag>()
        .0
        .clone()
        .expect("the host sent a bag")
}

fn host_bag(t: &TestApp) -> rmc_world::items::Bag {
    t.host()
        .bag(t.player_id())
        .cloned()
        .expect("the host keeps a bag")
}

fn play(t: &mut TestApp) {
    settle(t);
    let bounds = t.config().world_bounds;
    let (x, z) = (
        (bounds.min.0.x + bounds.max.0.x) / 2,
        (bounds.min.0.z + bounds.max.0.z) / 2,
    );
    let world = t.host_world();
    let top_y = (bounds.min.0.y..=bounds.max.0.y)
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
        (top_y + 1) as f32 * CELL_SIZE_M,
        (z as f32 + 0.5) * CELL_SIZE_M,
    ));
    t.steps(30);
    settle(t);
    let before = t.screenshot();
    e2e::save_screenshot(&before, NAME, t.frame());

    // I opens the screen: a slot per bag slot, the clothes worn.
    t.hold(KeyCode::KeyI, 1);
    t.steps(3);
    assert_eq!(top(t), Some(Screen::Character));
    let bag = client_bag(t);
    let slots = {
        let world = t.app_mut().world_mut();
        world.query::<&BagSlotUi>().iter(world).count()
    };
    assert_eq!(
        slots,
        bag.hotbar().len() + bag.pack().len() + EquipSlot::ALL.len()
    );
    assert!(
        EquipSlot::ALL.iter().any(|&e| bag.worn(e).is_some()),
        "the starting kit is worn"
    );
    let open = t.screenshot();
    e2e::save_screenshot(&open, NAME, t.frame());
    assert!(e2e::rms(&before, &open) > 0.0, "the frames differ");

    // Drag the pickaxe to the pack's third slot.
    let items = t.app().world().resource::<ClientItems>().items.clone();
    let pick = Thing::Item(items.id("pickaxe").expect("a pickaxe row"));
    let from = SlotRef::Hotbar(
        bag.hotbar()
            .iter()
            .position(|s| s.is_some_and(|s| s.thing == pick))
            .expect("the pickaxe is on the hotbar") as u8,
    );
    let to = SlotRef::Pack(2);
    t.app_mut().world_mut().write_message(BagGesture::Drop {
        from,
        to,
        half: false,
    });
    t.steps(10);
    let mut want = bag.clone();
    want.move_stack(&items, from, to, None).unwrap();
    assert_eq!(client_bag(t), want, "predicted and confirmed");
    assert_eq!(host_bag(t), want, "the host moved it too");
    let moved = t.screenshot();
    e2e::save_screenshot(&moved, NAME, t.frame());
    assert!(e2e::rms(&open, &moved) > 0.0, "the slot changed");

    // A pickaxe is not a hat: nothing moves.
    t.app_mut().world_mut().write_message(BagGesture::Drop {
        from: to,
        to: SlotRef::Equip(EquipSlot::Head),
        half: false,
    });
    t.steps(10);
    assert_eq!(client_bag(t), want);
    assert_eq!(host_bag(t), want);

    t.hold(KeyCode::KeyI, 1);
    t.steps(3);
    assert_eq!(top(t), None, "I closes it");
}
