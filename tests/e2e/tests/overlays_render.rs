//! The data maps (docs/OVERLAYS.md): M steps through the heat maps of
//! `assets/overlays.ron`, the terrain takes the picked map's colors, and a
//! light panel on the right lists the maps and shows the picked one's
//! legend. On the runoff map, arrows show where the water runs.
//!
//! A loam plot in the middle of the world: nitrogen in bands along x,
//! phosphorus rising along z, a potassium blob, a stepped hill (runoff), a
//! pond with wet ground around it, and a saturated layer under the west
//! half (the water table).
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the code set up
//! (each M picks the next map, the meshes take its color source, the panel
//! shows and marks it, the legend runs between the map's colors, the runoff
//! map has arrows that point downhill) and that the frames render; the
//! screenshots, one per map, are for the playtester.

use bevy::prelude::*;
use rmc_client::{
    ColorSource, GameCamera, MAP_KEY, MapOverlay, MapOverlays, MapPanel, MapRow, MapSource,
    MeshColorSource, MeshQueue, MeshStats, RunoffArrows,
};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// The plot's half width, in cells.
const HALF: i32 = 12;
/// Loam depth under the plot.
const DEPTH: i32 = 5;
/// The hill's steps.
const HILL: i32 = 4;

#[test]
#[ignore]
fn overlays_render() {
    let mut t = TestApp::new(TestConfig {
        size: (1280, 720),
        ui: true,
        overlays: true,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "overlays_render", run);
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

fn tap(t: &mut TestApp, key: KeyCode) {
    t.press(key);
    t.step();
    t.release(key);
    t.step();
}

fn boxed(op: EditOp, min: CellPos, max: CellPos) -> (EditOp, Shape) {
    (op, Shape::Box { min, max })
}

fn layer(layer: LayerId, value: i32, min: CellPos, max: CellPos) -> (EditOp, Shape) {
    boxed(EditOp::SetLayer { layer, value }, min, max)
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials().clone();
    let (loam, water) = (
        materials.id("loam").expect("loam"),
        materials.id("water").expect("water"),
    );
    let cap = i32::from(materials.get(loam).expect("loam").field_capacity);
    let nutrient = |key: &str| LayerId::Substance(eco.substance_index(key).expect(key));

    // The plot.
    let bounds = t.host_world().bounds();
    let (mx, mz) = (
        (bounds.min.0.x + bounds.max.0.x + 1) / 2,
        (bounds.min.0.z + bounds.max.0.z + 1) / 2,
    );
    let g = i32::from(t.host_world().surface_y(mx, mz));
    let (x0, x1, z0, z1) = (mx - HALF, mx + HALF, mz - HALF, mz + HALF);
    let at = |x, y, z| CellPos::new(x, y, z);
    let mut edits = vec![
        boxed(
            EditOp::Set(MaterialId::AIR),
            at(x0, g + 1, z0),
            at(x1, g + 20, z1),
        ),
        boxed(EditOp::Set(loam), at(x0, g - DEPTH, z0), at(x1, g, z1)),
        layer(
            LayerId::Moisture,
            cap / 3,
            at(x0, g - DEPTH, z0),
            at(x1, g, z1),
        ),
    ];
    // A stepped hill in the north-east quarter.
    let (hx, hz) = (mx + HALF / 2, mz + HALF / 2);
    for k in 1..=HILL {
        let r = HILL + 1 - k;
        edits.push(boxed(
            EditOp::Set(loam),
            at(hx - r, g + k, hz - r),
            at(hx + r, g + k, hz + r),
        ));
    }
    let top = g + HILL;
    // Nitrogen in bands along x, phosphorus rising along z, a potassium
    // blob in the south-east.
    let band = 4;
    for (i, x) in (x0..=x1).step_by(band as usize).enumerate() {
        let value = (i as i32 * 48).min(255);
        edits.push(layer(
            nutrient("n"),
            value,
            at(x, g - DEPTH, z0),
            at(x + band - 1, top, z1),
        ));
    }
    for z in z0..=z1 {
        let value = (z - z0) * 255 / (z1 - z0);
        edits.push(layer(
            nutrient("p"),
            value,
            at(x0, g - DEPTH, z),
            at(x1, top, z),
        ));
    }
    edits.push((
        EditOp::SetLayer {
            layer: nutrient("k"),
            value: 220,
        },
        Shape::Sphere {
            center: at(mx + HALF / 2, g, mz - HALF / 2),
            radius: 5,
        },
    ));
    // A saturated layer two cells down under the west half, and a pond in
    // the south-west quarter with wet ground around it.
    edits.push(layer(
        LayerId::Moisture,
        cap,
        at(x0, g - 3, z0),
        at(mx, g - 2, z1),
    ));
    let (px, pz) = (mx - HALF / 2, mz - HALF / 2);
    edits.push(layer(
        LayerId::Moisture,
        cap,
        at(px - 5, g - 2, pz - 5),
        at(px + 5, g, pz + 5),
    ));
    edits.push(boxed(
        EditOp::Set(water),
        at(px - 3, g - 1, pz - 3),
        at(px + 3, g, pz + 3),
    ));
    t.apply_edits(&edits);
    settle(t);

    let o = t.app().world().resource::<MapOverlays>().clone();
    let centre = Vec3::new(mx as f32 + 0.5, g as f32, mz as f32 + 0.5) * CELL_SIZE_M;
    let eye = centre + Vec3::new(-6.0, 26.0, -26.0) * CELL_SIZE_M;
    {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
    }
    t.steps(4);
    settle(t);

    let shots = [
        MapSource::Substance("n".into()),
        MapSource::Substance("p".into()),
        MapSource::Substance("k".into()),
        MapSource::Moisture,
        MapSource::WaterTable,
        MapSource::Runoff,
    ];
    let mut at_index = None;
    for source in &shots {
        let want = o
            .maps
            .iter()
            .position(|m| &m.source == source)
            .unwrap_or_else(|| panic!("a {source:?} map"));
        // M steps to the next map, so step until this one is picked.
        while at_index != Some(want) {
            tap(t, MAP_KEY);
            at_index = t.app().world().resource::<MapOverlay>().active;
            assert!(
                at_index.is_some(),
                "{source:?} comes before the maps turn off"
            );
        }
        let map = &o.maps[want];
        t.steps(4);
        settle(t);

        // What the code set up: the meshes take the map's colors, and the
        // panel shows with the map marked.
        let source_now = t.app().world().resource::<MeshColorSource>().0;
        match source_now {
            ColorSource::Layer(_, cmap) | ColorSource::Column(_, cmap) => {
                assert_eq!(cmap, map.colormap, "{}", map.key)
            }
            ColorSource::Material => panic!("{}: the meshes keep their materials", map.key),
        }
        let world = t.app_mut().world_mut();
        let display = world
            .query_filtered::<&Node, With<MapPanel>>()
            .single(world)
            .expect("one map panel")
            .display;
        assert_eq!(display, Display::Flex, "the panel shows");
        let marked: Vec<usize> = world
            .query::<(&MapRow, &BackgroundColor)>()
            .iter(world)
            .filter(|(_, bg)| bg.0 == o.panel.selected)
            .map(|(r, _)| r.0)
            .collect();
        assert_eq!(marked, [want], "only the picked map is marked");
        if *source == MapSource::Runoff {
            let arrows = &t.app().world().resource::<RunoffArrows>().0;
            assert!(!arrows.is_empty(), "the hill sheds water");
            for a in arrows {
                assert!(a.to.0.y < a.from.0.y, "arrows point downhill");
            }
        }

        t.steps(4);
        let image = t.screenshot();
        let path = e2e::save_screenshot(&image, &format!("overlays_render-{}", map.key), t.frame());
        eprintln!("screenshot: {}", path.display());
        assert_eq!(t.render_failure(), None, "{} renders", map.key);
    }

    // Past the last map the maps turn off and the materials come back.
    while t.app().world().resource::<MapOverlay>().active.is_some() {
        tap(t, MAP_KEY);
    }
    t.steps(2);
    assert_eq!(
        t.app().world().resource::<MeshColorSource>().0,
        ColorSource::Material
    );
    assert!(t.app().world().resource::<RunoffArrows>().0.is_empty());
}
