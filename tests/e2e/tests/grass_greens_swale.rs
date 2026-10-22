//! Grass greens the swale (SAD-P2 §12). A contour swale on a slope of loam
//! (a porous soil, so a plant's anchor can hold water), with nitrogen laid
//! evenly over the whole site, the swale full of water and grass planted on
//! every bare loam column. Then six seasons of drought: no rain, so the
//! only water on the site is the swale's.
//!
//! The swale's water seeps sideways into the loam of its banks and berm
//! (§7.3 B3, B4). Grass there meets its moisture need and lives, and once
//! mature it spreads onto the wet loam around it (§7.8). On the dry slope
//! every anchor's moisture stays 0, so the grass there is stressed every
//! sub-step and dies. After the six seasons the band of columns in and
//! around the swale holds more grass than a band of the same width on the
//! slope uphill, though it started with less (the swale's own columns are
//! water, not loam). The playtester judges the shot of the green banks.

use bevy::prelude::*;
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::eco::{FIXTURE_SOIL_DEPTH, SLOPE_RISE};
use rmc_testkit::{Fixture, FixtureSite, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// The swale's contour, in cells above the slope's low end: low on the
/// slope, so the uphill control band is far from the swale's water.
const CONTOUR: i32 = 2;
/// Seasons of drought.
const SEASONS: u32 = 6;
/// Nitrogen in every loam cell: grass needs 2 at its anchor and takes up 1
/// per sub-step, so 96 lasts the six seasons (48 sub-steps).
const NITROGEN: i32 = 96;
/// The band in and around the swale: from this many columns uphill of the
/// swale to this many downhill of it (the berm is the first).
const NEAR_UPHILL: i32 = 2;
const NEAR_DOWNHILL: i32 = 3;
/// The control band starts this many columns from the site's uphill edge.
const CONTROL_DX: i32 = 2;

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
fn grass_greens_swale() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "grass_greens_swale", run);
}

/// The cells of `region`, ascending (y, z, x).
fn cells(region: CellAabb) -> impl Iterator<Item = CellPos> {
    let (min, max) = (region.min.0, region.max.0);
    (min.y..=max.y).flat_map(move |y| {
        (min.z..=max.z).flat_map(move |z| (min.x..=max.x).map(move |x| CellPos::new(x, y, z)))
    })
}

/// How many cells of `region` on the host hold `material`.
fn count(t: &TestApp, material: MaterialId, region: CellAabb) -> usize {
    let world = t.host_world();
    cells(region)
        .filter(|&c| world.get_cell(c) == Some(material))
        .count()
}

/// The sum of the moisture layer over the cells of `region` that hold
/// `material`.
fn moisture_of(t: &TestApp, material: MaterialId, region: CellAabb) -> u64 {
    cells(region)
        .filter(|&c| t.host_world().get_cell(c) == Some(material))
        .map(|c| u64::from(t.layer_u8(c, LayerId::Moisture)))
        .sum()
}

/// The columns `x = x0..=x1` of the site, from the bottom of the soil to
/// the top of the slope.
fn band(site: &FixtureSite, x0: i32, x1: i32) -> CellAabb {
    CellAabb::new(
        CellPos::new(x0, site.ground_y - FIXTURE_SOIL_DEPTH, site.site.min.0.z),
        CellPos::new(x1, site.ground_y + SLOPE_RISE + 2, site.site.max.0.z),
    )
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials();
    let key = |k: &str| materials.id(k).unwrap_or_else(|| panic!("no {k}"));
    let (loam, grass) = (key("loam"), key("grass"));
    let water = eco.role_material("water").expect("a water material");
    let n = eco.substance_index("n").expect("a nitrogen substance");

    let site = t.fixture(Fixture::Swale(CONTOUR));
    let swale = site.feature;
    let (x0, z0, z1) = (site.site.min.0.x, site.site.min.0.z, site.site.max.0.z);
    let (sx0, sx1) = (site.site.min.0.x, site.site.max.0.x);

    // The soil becomes loam: in every column the soil cells under the
    // surface, and the berm on top of it. Paint leaves the swale's dug
    // cells air and its stone floor stone.
    let paint: Vec<(EditOp, Shape)> = (sx0..=sx1)
        .map(|x| {
            let top = site.surface_y(x);
            (
                EditOp::Paint(loam),
                Shape::Box {
                    min: CellPos::new(x, top - FIXTURE_SOIL_DEPTH + 1, z0),
                    max: CellPos::new(x, top + 1, z1),
                },
            )
        })
        .collect();
    t.apply_edits(&paint);

    // Nitrogen in every loam cell, as runs along z.
    let mut fertilize = Vec::new();
    for x in sx0..=sx1 {
        let top = site.surface_y(x);
        for y in top - FIXTURE_SOIL_DEPTH + 1..=top + 1 {
            let mut run: Option<i32> = None;
            for z in z0..=z1 + 1 {
                let is_loam =
                    z <= z1 && t.host_world().get_cell(CellPos::new(x, y, z)) == Some(loam);
                match (is_loam, run) {
                    (true, None) => run = Some(z),
                    (false, Some(from)) => {
                        fertilize.push((
                            EditOp::SetLayer {
                                layer: LayerId::Substance(n),
                                value: NITROGEN,
                            },
                            Shape::Box {
                                min: CellPos::new(x, y, from),
                                max: CellPos::new(x, y, z - 1),
                            },
                        ));
                        run = None;
                    }
                    _ => {}
                }
            }
        }
    }
    t.apply_edits(&fertilize);

    // The swale full of water (the brush fills each cell to 255).
    t.apply_edits(&[(
        EditOp::Set(water),
        Shape::Box {
            min: swale.min,
            max: swale.max,
        },
    )]);
    assert!(t.count("water", swale) > 0, "the swale holds water");

    // Grass on every column whose top is loam.
    let mut plant = Vec::new();
    for x in sx0..=sx1 {
        for z in z0..=z1 {
            let top = (site.ground_y - FIXTURE_SOIL_DEPTH..=site.surface_y(x) + 1)
                .rev()
                .find(|&y| t.host_world().get_cell(CellPos::new(x, y, z)) != Some(MaterialId::AIR))
                .expect("every column has ground");
            if t.host_world().get_cell(CellPos::new(x, top, z)) == Some(loam) {
                plant.push((EditOp::Set(grass), Shape::Cell(CellPos::new(x, top + 1, z))));
            }
        }
    }
    t.apply_edits(&plant);

    // The band in and around the swale, and an equally wide band uphill.
    let near = band(
        &site,
        swale.min.0.x - NEAR_UPHILL,
        swale.max.0.x + NEAR_DOWNHILL,
    );
    let width = near.max.0.x - near.min.0.x;
    let control = band(&site, x0 + CONTROL_DX, x0 + CONTROL_DX + width);
    assert!(
        control.max.0.x < near.min.0.x,
        "the control band {control:?} lies uphill of the swale's band {near:?}"
    );
    let (near_0, control_0) = (count(t, grass, near), count(t, grass, control));
    eprintln!(
        "swale {swale:?}; planted {} grass: {near_0} in and around the swale, {control_0} uphill",
        plant.len()
    );
    assert!(control_0 > near_0, "the uphill band starts greener");
    assert!(
        cells(near).all(|c| t.host_world().get_cell(c) != Some(grass)
            || t.host_world().plant(c).is_some()),
        "every grass cell is a plant"
    );

    // Six seasons of drought.
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });
    let mut born = 0;
    for season in 0..SEASONS {
        t.advance_season();
        let ledger = t.ledger();
        assert_eq!(ledger.rain_in, 0, "season {season}: no rain");
        born += ledger.plants_born;
        eprintln!(
            "season {season}: grass {} in and around the swale, {} uphill; born {}, died {}; \
             loam moisture {} near, {} uphill; swale water cells {}",
            count(t, grass, near),
            count(t, grass, control),
            ledger.plants_born,
            ledger.plants_died,
            moisture_of(t, loam, near),
            moisture_of(t, loam, control),
            t.count("water", swale),
        );
    }
    assert_eq!(t.season_log().len(), SEASONS as usize);

    // In and around the swale against the uphill band, and against the
    // whole rest of the site (uphill and downhill of the band).
    let (near_n, control_n) = (count(t, grass, near), count(t, grass, control));
    let whole = band(&site, sx0, sx1);
    let elsewhere = count(t, grass, whole) - near_n;
    assert!(
        near_n > control_n,
        "after {SEASONS} seasons: {near_n} grass in and around the swale, {control_n} uphill"
    );
    assert!(
        near_n > elsewhere,
        "after {SEASONS} seasons: {near_n} grass in and around the swale, \
         {elsewhere} on the rest of the site"
    );
    assert!(born > 0, "the grass by the swale spread");
    assert!(
        moisture_of(t, loam, near) > moisture_of(t, loam, control),
        "the loam by the swale is wetter than the loam uphill"
    );
    // Green where wet: every living grass grows on loam the swale wetted.
    // Nothing else wets the site in a drought, and a porous cell sheds water
    // only above its seepage threshold, so a wetted anchor never dries to 0.
    for c in cells(whole) {
        if t.host_world().get_cell(c) == Some(grass) {
            assert!(t.host_world().plant(c).is_some(), "{c:?}: grass is a plant");
            let anchor = CellPos::new(c.0.x, c.0.y - 1, c.0.z);
            assert_eq!(
                t.host_world().get_cell(anchor),
                Some(loam),
                "{c:?}: on loam"
            );
            assert!(
                t.layer_u8(anchor, LayerId::Moisture) > 0,
                "{c:?}: grass lives on a dry anchor"
            );
        }
    }
    // Winter freezes the pool (the swale's water count drops while it is
    // ice); after six seasons it is water again and still there.
    assert!(t.count("water", swale) > 0, "the swale still holds water");

    shoot(t, swale, "grass_greens_swale");
}

/// Saves a screenshot of the swale from above the slope downhill of the
/// berm, looking up the slope: the swale, its green berm and banks and the
/// bare slope uphill are in frame.
fn shoot(t: &mut TestApp, swale: CellAabb, name: &str) {
    t.steps(3);
    settle(t);
    {
        let (min, max) = (swale.min.0, swale.max.0);
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            (max.y + 1) as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let eye = centre + Vec3::new(7.0, 6.0, 0.0);
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
