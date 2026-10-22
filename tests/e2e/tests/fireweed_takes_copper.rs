//! Fireweed takes copper (SAD-P2 §12). A copper-rich patch of loam (a
//! porous soil, so a plant's anchor holds the rain) on flat ground, and a
//! clean loam patch beside it as a control. Both carry the same nitrogen,
//! so copper is the only difference. Fireweed and grass are planted on the
//! copper patch, and grass on the control.
//!
//! In three seasons of the default weather (spring, summer, autumn) the
//! fireweed takes copper up from its anchor and holds it (§7.8 `holds`),
//! so the anchor's `cu` falls. The grass on copper is above its
//! `substance_max`, is stressed every sub-step, and dies; the control grass
//! lives. Harvesting the fireweed yields the plant and `copper_dust`. The
//! playtester judges the shot of the patch before the harvest.

use bevy::prelude::*;
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::{Fixture, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::shape::Shape;

/// Copper in the patch's cells: well above grass's `substance_max` (16)
/// and fireweed's uptake over three seasons (2 per sub-step, 24 sub-steps).
const COPPER: u8 = 120;
/// Nitrogen in both patches: grass needs 2 at its anchor.
const NITROGEN: i32 = 96;
/// Seasons of growth.
const SEASONS: u32 = 3;
/// The control patch, in site offsets: `dx` and `dz` ranges, clear of the
/// copper patch in the middle (`12..=19`).
const CONTROL: (i32, i32) = (2, 7);

/// Steps until meshing is idle and nothing waits to upload.
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

#[test]
#[ignore]
fn fireweed_takes_copper() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "fireweed_takes_copper", run);
}

fn below(c: CellPos) -> CellPos {
    CellPos::new(c.0.x, c.0.y - 1, c.0.z)
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials();
    let key = |k: &str| materials.id(k).unwrap_or_else(|| panic!("no {k}"));
    let (loam, grass, fireweed) = (key("loam"), key("grass"), key("fireweed"));
    let copper_dust = eco.role_material("copper_dust").expect("copper_dust");
    let cu = eco.substance_index("cu").expect("a copper substance");
    let n = eco.substance_index("n").expect("a nitrogen substance");
    let x = usize::from(cu);

    let site = t.fixture(Fixture::Patch(loam, cu, COPPER));
    let patch = site.feature;
    let g = site.ground_y;
    let (x0, z0) = (site.site.min.0.x, site.site.min.0.z);
    let control = CellAabb::new(
        CellPos::new(x0 + CONTROL.0, patch.min.0.y, z0 + CONTROL.0),
        CellPos::new(x0 + CONTROL.1, patch.max.0.y, z0 + CONTROL.1),
    );
    let fertilize = |b: CellAabb| {
        (
            EditOp::SetLayer {
                layer: LayerId::Substance(n),
                value: NITROGEN,
            },
            Shape::Box {
                min: b.min,
                max: b.max,
            },
        )
    };
    t.apply_edits(&[
        (
            EditOp::Set(loam),
            Shape::Box {
                min: control.min,
                max: control.max,
            },
        ),
        fertilize(patch),
        fertilize(control),
    ]);

    // Fireweed and grass on the copper, at opposite corners of the patch's
    // middle; grass on the control.
    let on_copper = CellPos::new(patch.min.0.x + 2, g + 1, patch.min.0.z + 2);
    let grass_on_copper = CellPos::new(patch.max.0.x - 2, g + 1, patch.max.0.z - 2);
    let grass_clean = CellPos::new(control.min.0.x + 2, g + 1, control.min.0.z + 2);
    t.apply_edits(&[
        (EditOp::Set(fireweed), Shape::Cell(on_copper)),
        (EditOp::Set(grass), Shape::Cell(grass_on_copper)),
        (EditOp::Set(grass), Shape::Cell(grass_clean)),
    ]);
    for c in [on_copper, grass_on_copper, grass_clean] {
        assert!(t.host_world().plant(c).is_some(), "{c:?} is planted");
    }
    let anchor = below(on_copper);
    let anchor_cu = t.layer_u8(anchor, LayerId::Substance(cu));
    assert_eq!(anchor_cu, COPPER);
    assert_eq!(
        t.layer_u8(below(grass_on_copper), LayerId::Substance(cu)),
        COPPER
    );
    assert_eq!(t.layer_u8(below(grass_clean), LayerId::Substance(cu)), 0);
    let site_cu = t.sum_layer(LayerId::Substance(cu), site.site);

    let mut died = 0;
    for season in 0..SEASONS {
        t.advance_season();
        died += t.ledger().plants_died;
        let world = t.host_world();
        eprintln!(
            "season {season}: fireweed {:?}, anchor cu {}, moisture {}; grass on copper {:?}; \
             control grass {:?}",
            world.plant(on_copper),
            t.layer_u8(anchor, LayerId::Substance(cu)),
            t.layer_u8(anchor, LayerId::Moisture),
            world.plant(grass_on_copper),
            world.plant(grass_clean),
        );
    }

    let after = t.layer_u8(anchor, LayerId::Substance(cu));
    assert!(
        after < anchor_cu,
        "the anchor's cu fell under the fireweed: {anchor_cu} -> {after}"
    );
    let held = t
        .host_world()
        .plant(on_copper)
        .expect("the fireweed lives")
        .held[x];
    assert!(held > 0, "the fireweed holds copper");
    // The copper the fireweed holds came out of the ground: the site's cu
    // plus the plant's is what the site started with (SAD-P2 §7.8 step 3,
    // held amounts stay inside the substance total).
    assert_eq!(
        t.sum_layer(LayerId::Substance(cu), site.site) + u64::from(held),
        site_cu,
        "copper is conserved between the ground and the fireweed"
    );
    assert_eq!(t.host_world().get_cell(on_copper), Some(fireweed));
    assert_ne!(
        t.host_world().get_cell(grass_on_copper),
        Some(grass),
        "the grass on copper is dead"
    );
    assert!(t.host_world().plant(grass_on_copper).is_none());
    assert!(died > 0, "the ledger counts the dead grass");
    assert_eq!(
        t.host_world().get_cell(grass_clean),
        Some(grass),
        "the control grass on clean loam lives"
    );

    shoot(t, grass_clean, grass_on_copper, "fireweed_takes_copper");

    // The harvest: the plant, and max(1, held >> harvest_shift) copper_dust.
    let got = t.harvest(on_copper);
    eprintln!("harvest: {:?}", got.items());
    let dust = (u32::from(held) >> eco.params().harvest_shift).max(1);
    assert_eq!(got.count(fireweed), 1);
    assert_eq!(
        got.count(copper_dust),
        dust,
        "the harvest yields copper_dust"
    );
    assert!(t.host_world().plant(on_copper).is_none());
}

/// Saves a screenshot from high above the site, looking down at the
/// middle of the cells `a` and `b`: the control grass, the fireweed and the
/// bare loam where the grass on copper died are in frame.
fn shoot(t: &mut TestApp, a: CellPos, b: CellPos, name: &str) {
    t.steps(3);
    settle(t);
    {
        let centre = ((a.0 + b.0).as_vec3() / 2.0 + Vec3::splat(0.5)) * CELL_SIZE_M;
        let eye = centre + Vec3::new(2.0, 8.0, 6.0);
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
    }
    t.steps(2);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, name, t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "{name}: render failure: {:?}",
        t.render_failure()
    );
}
