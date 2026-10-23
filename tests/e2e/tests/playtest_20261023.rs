//! E5 player's walk (2026-10-23): a loam swale meadow over several seasons
//! of the shipped weather, plants growing, spreading and dying, tailings-free
//! this time; a copper patch with fireweed and grass; the moisture, growth
//! and copper overlays; the editor's eco panel; and a pickaxe harvest by a
//! real player.
//!
//! Screenshots are `playtest_20261023-<what>-<frame>.png` in the e2e output
//! directory; notes go to `<out>/playtest_20261023.log`. Nothing here
//! asserts the game is good, only that it does not crash.

use std::io::Write;

use bevy::prelude::*;
use rmc_client::controller::Player;
use rmc_client::pickaxe::SWING_BUTTON;
use rmc_client::{GameCamera, MeshQueue, MeshStats, TOGGLE_EDITOR_KEY};
use rmc_testkit::eco::FIXTURE_SOIL_DEPTH;
use rmc_testkit::{Fixture, FixtureSite, TestApp, TestConfig, bots, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

const NAME: &str = "playtest_20261023";
/// Seasons of the meadow.
const SEASONS: u32 = 6;
/// Nitrogen and phosphorus in every loam cell.
const FERTILITY: i32 = 96;
const COPPER: i32 = 120;

fn note(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("{msg}");
    let path = e2e::out_dir().join(format!("{NAME}.log"));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the log");
    writeln!(f, "{msg}").expect("write the log");
}

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
    note("meshing did not settle in 240 steps");
}

fn shot(t: &mut TestApp, what: &str) {
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("{NAME}-{what}"), t.frame());
    note(format!("shot {what}: {}", path.display()));
}

fn tap(t: &mut TestApp, key: KeyCode) {
    t.press(key);
    t.step();
    t.release(key);
    t.step();
}

fn cube(min: CellPos, max: CellPos) -> Shape {
    Shape::Box { min, max }
}

fn set_camera(t: &mut TestApp, eye: Vec3, at: Vec3) {
    let world = t.app_mut().world_mut();
    let mut camera = world
        .query_filtered::<&mut Transform, With<GameCamera>>()
        .single_mut(world)
        .expect("one game camera");
    *camera = Transform::from_translation(eye).looking_at(at, Vec3::Y);
}

/// The top ground cell of column (x, z): not air, not water, not a plant.
fn ground_top(t: &TestApp, x: i32, z: i32, from: i32, to: i32) -> i32 {
    let world = t.host_world();
    let water = t.eco_tables().role_material("water");
    (from..=to)
        .rev()
        .find(|&y| {
            let c = CellPos::new(x, y, z);
            let m = world.get_cell(c);
            m != Some(MaterialId::AIR)
                && m != water
                && world.plant(c).is_none()
                && world.get_cell(CellPos::new(x, y + 1, z)).is_some()
        })
        .unwrap_or(from)
}

/// Plants per species on the site.
fn census(t: &TestApp, site: &FixtureSite, n: usize) -> Vec<usize> {
    let world = t.host_world();
    let (lo, hi) = (site.site.min.0, site.site.max.0);
    let mut out = vec![0; n];
    for y in site.ground_y - 4..=site.ground_y + 10 {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                if let Some(p) = world.plant(CellPos::new(x, y, z)) {
                    out[usize::from(p.species)] += 1;
                }
            }
        }
    }
    out
}

/// Columns of the site: how far the topmost non-air cell stands above the
/// slope's own surface, and how many ice and water cells the site holds.
fn column_stats(t: &TestApp, site: &FixtureSite, ice: MaterialId, water: MaterialId) -> String {
    let world = t.host_world();
    let (lo, hi) = (site.site.min.0, site.site.max.0);
    let (mut max_over, mut sum_over, mut cols, mut ice_n, mut water_n) = (0, 0, 0, 0, 0);
    let mut over_hist = [0usize; 8];
    for z in lo.z..=hi.z {
        for x in lo.x..=hi.x {
            let mut top = None;
            for y in (site.ground_y - 4..=hi.y).rev() {
                let m = world.get_cell(CellPos::new(x, y, z));
                if m == Some(ice) {
                    ice_n += 1;
                }
                if m == Some(water) {
                    water_n += 1;
                }
                if top.is_none() && m != Some(MaterialId::AIR) {
                    top = Some(y);
                }
            }
            let over = top.unwrap_or(0) - site.surface_y(x) - 1;
            max_over = max_over.max(over);
            sum_over += over;
            cols += 1;
            over_hist[over.clamp(0, 7) as usize] += 1;
        }
    }
    format!(
        "columns {cols}: top above slope max {max_over}, mean {:.2}, histogram 0..7+ {over_hist:?}; ice cells {ice_n}, water cells {water_n}",
        f64::from(sum_over) / f64::from(cols)
    )
}

fn describe(t: &TestApp, label: &str, c: CellPos) -> String {
    let anchor = CellPos::new(c.0.x, c.0.y - 1, c.0.z);
    match t.host_world().plant(c) {
        Some(p) => format!(
            "{label}: species {} growth {} stress {} age {} held {:?}; anchor moisture {} n {} cu {}",
            p.species,
            t.layer_u8(c, LayerId::Growth),
            p.stress,
            p.age_seasons,
            p.held,
            t.layer_u8(anchor, LayerId::Moisture),
            t.layer_u8(anchor, LayerId::Substance(6)),
            t.layer_u8(anchor, LayerId::Substance(0)),
        ),
        None => format!("{label}: no plant (cell {:?})", t.host_world().get_cell(c)),
    }
}

#[test]
#[ignore]
fn playtest_20261023() {
    if std::env::var("PT_STAGE").as_deref() == Ok("world") {
        world_stage();
        return;
    }
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, NAME, run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials();
    let key = |k: &str| materials.id(k).unwrap_or_else(|| panic!("no {k}"));
    let (loam, grass, moss, fireweed, shrub) = (
        key("loam"),
        key("grass"),
        key("moss"),
        key("fireweed"),
        key("shrub"),
    );
    let water = eco.role_material("water").expect("water");
    let cu = eco.substance_index("cu").expect("cu");
    let n = eco.substance_index("n").expect("n");
    let p = eco.substance_index("p").expect("p");
    let species = eco.plants().len();
    note(format!(
        "substance indexes cu {cu} n {n} p {p}; {species} species: {:?}",
        eco.plants()
            .iter()
            .map(|d| d.key.clone())
            .collect::<Vec<_>>()
    ));

    // Stage 0: the generated world's own plants.
    let generated: usize = t
        .host_world()
        .iter_chunks_sorted()
        .map(|(_, c)| c.plants.len())
        .sum();
    note(format!(
        "STAGE0 plants in the generated 64x128x64 world: {generated}"
    ));

    // Stage 1: the meadow. A swale on a loam slope, N and P everywhere, the
    // swale full, grass on every loam column, moss on the banks, shrubs on
    // the berm side, fireweed and grass on a copper patch uphill.
    let site = t.fixture(Fixture::Swale(2));
    let swale = site.feature;
    let (sx0, sx1) = (site.site.min.0.x, site.site.max.0.x);
    let (z0, z1) = (site.site.min.0.z, site.site.max.0.z);
    note(format!(
        "site {:?} swale {swale:?} ground {}",
        site.site, site.ground_y
    ));
    let paint: Vec<(EditOp, Shape)> = (sx0..=sx1)
        .map(|x| {
            let top = site.surface_y(x);
            (
                EditOp::Paint(loam),
                cube(
                    CellPos::new(x, top - FIXTURE_SOIL_DEPTH + 1, z0),
                    CellPos::new(x, top + 1, z1),
                ),
            )
        })
        .collect();
    t.apply_edits(&paint);
    let mut fert = Vec::new();
    for x in sx0..=sx1 {
        let top = site.surface_y(x);
        for y in top - FIXTURE_SOIL_DEPTH + 1..=top + 1 {
            for sub in [n, p] {
                // Whole z rows; SetLayer on air cells is harmless.
                fert.push((
                    EditOp::SetLayer {
                        layer: LayerId::Substance(sub),
                        value: FERTILITY,
                    },
                    cube(CellPos::new(x, y, z0), CellPos::new(x, y, z1)),
                ));
            }
        }
    }
    t.apply_edits(&fert);
    t.apply_edits(&[(EditOp::Set(water), cube(swale.min, swale.max))]);

    // Copper patch uphill (low x), loam top cells.
    let (cx0, cx1, cz0, cz1) = (sx0 + 3, sx0 + 8, z0 + 10, z0 + 15);
    let mut copper = Vec::new();
    for x in cx0..=cx1 {
        let top = site.surface_y(x);
        copper.push((
            EditOp::SetLayer {
                layer: LayerId::Substance(cu),
                value: COPPER,
            },
            cube(CellPos::new(x, top - 1, cz0), CellPos::new(x, top, cz1)),
        ));
    }
    t.apply_edits(&copper);

    let top_of = |t: &TestApp, x: i32, z: i32| {
        ground_top(
            t,
            x,
            z,
            site.ground_y - FIXTURE_SOIL_DEPTH,
            site.ground_y + 8,
        )
    };
    let mut plant = Vec::new();
    for x in sx0..=sx1 {
        for z in z0..=z1 {
            let top = top_of(t, x, z);
            if t.host_world().get_cell(CellPos::new(x, top, z)) != Some(loam) {
                continue;
            }
            let c = CellPos::new(x, top + 1, z);
            let m = if (cx0..=cx1).contains(&x) && (cz0..=cz1).contains(&z) {
                // Copper patch: a checkerboard of fireweed and grass.
                if (x + z) % 2 == 0 { fireweed } else { grass }
            } else if (x + z) % 11 == 0 {
                moss
            } else if (x + z) % 13 == 0 && x > swale.max.0.x {
                shrub
            } else {
                grass
            };
            plant.push((EditOp::Set(m), Shape::Cell(c)));
        }
    }
    t.apply_edits(&plant);
    settle(t);
    let c0 = census(t, &site, species);
    note(format!(
        "STAGE1 census at season 0 (moss grass fireweed shrub): {c0:?}"
    ));

    // Named plants to follow.
    let fw = CellPos::new(cx0 + 2, top_of(t, cx0 + 2, cz0 + 3) + 1, cz0 + 3);
    let g_on_cu = CellPos::new(cx0 + 2, top_of(t, cx0 + 2, cz0 + 2) + 1, cz0 + 2);
    let bank_x = swale.max.0.x + 2;
    let g_bank = CellPos::new(bank_x, top_of(t, bank_x, z0 + 6) + 1, z0 + 6);
    let g_dry = CellPos::new(sx0 + 1, top_of(t, sx0 + 1, z0 + 20) + 1, z0 + 20);
    note(format!(
        "fireweed {fw:?} grass_on_cu {g_on_cu:?} bank {g_bank:?} dry {g_dry:?}"
    ));
    for (l, c) in [
        ("fw", fw),
        ("grass_cu", g_on_cu),
        ("grass_bank", g_bank),
        ("grass_dry", g_dry),
    ] {
        note(describe(t, &format!("s-1 {l}"), c));
    }

    let centre = Vec3::new(
        (sx0 + sx1 + 1) as f32 / 2.0,
        (site.ground_y + 2) as f32,
        (z0 + z1 + 1) as f32 / 2.0,
    ) * CELL_SIZE_M;
    let overview = |t: &mut TestApp| {
        set_camera(t, centre + Vec3::new(10.0, 7.5, 0.0), centre);
        t.steps(2);
    };
    let bank_m = g_bank.center_m();
    let closeup = |t: &mut TestApp| {
        set_camera(
            t,
            bank_m + Vec3::new(2.6, 1.6, 0.0),
            bank_m + Vec3::new(-2.0, -0.9, 0.8),
        );
        t.steps(2);
    };
    closeup(t);
    shot(t, "closeup-bank-s0");
    note(format!("s0 {}", column_stats(t, &site, key("ice"), water)));
    overview(t);
    shot(t, "meadow-s0");

    // Stage 2: seasons.
    let mut born = 0;
    let mut died = 0;
    for season in 1..=SEASONS {
        t.advance_season();
        let l = t.ledger().clone();
        born += l.plants_born;
        died += l.plants_died;
        let c = census(t, &site, species);
        note(format!(
            "STAGE2 season {season}: census {c:?}; ledger born {} died {} rain {} uptake {:?} litter {:?} harvest {:?}; water cells in swale {}",
            l.plants_born,
            l.plants_died,
            l.rain_in,
            l.uptake_sink,
            l.litter_in,
            l.harvest_out,
            t.count("water", swale)
        ));
        note(format!(
            "s{season} {}",
            column_stats(t, &site, key("ice"), water)
        ));
        for (lb, cell) in [
            ("fw", fw),
            ("grass_cu", g_on_cu),
            ("grass_bank", g_bank),
            ("grass_dry", g_dry),
        ] {
            note(describe(t, &format!("s{season} {lb}"), cell));
        }
        if matches!(season, 1 | 2 | 3 | 4 | SEASONS) {
            settle(t);
            overview(t);
            shot(t, &format!("meadow-s{season}"));
        }
        if season == 2 {
            for (what, layer) in [
                ("moisture", LayerId::Moisture),
                ("growth", LayerId::Growth),
                ("copper", LayerId::Substance(cu)),
            ] {
                t.editor().overlay = Some(layer);
                settle(t);
                t.steps(2);
                overview(t);
                shot(t, &format!("overlay-{what}-s2"));
            }
            t.editor().overlay = None;
            settle(t);
            closeup(t);
            shot(t, "closeup-bank-s2");
        }
    }
    note(format!("STAGE2 totals: born {born} died {died}"));

    // Stage 4: a real player at the copper patch. First what the player
    // meets by walking up unprepared: the cells between the player and the
    // fireweed, then the same with a cleared corridor.
    {
        let top = top_of(t, fw.0.x, fw.0.z);
        let col: Vec<String> = (top - 1..=top + 8)
            .map(|y| {
                let c = CellPos::new(fw.0.x, y, fw.0.z);
                format!(
                    "y{y}={}",
                    t.host_world()
                        .get_cell(c)
                        .and_then(|m| materials.get(m).map(|d| d.key.clone()))
                        .unwrap_or_default()
                )
            })
            .collect();
        note(format!("column at the fireweed: {col:?} (fw y {})", fw.0.y));
    }
    let clear_a = (
        EditOp::Remove,
        cube(
            CellPos::new(fw.0.x + 1, fw.0.y, fw.0.z),
            CellPos::new(fw.0.x + 4, fw.0.y + 12, fw.0.z),
        ),
    );
    let clear_b = (
        EditOp::Remove,
        cube(
            CellPos::new(fw.0.x, fw.0.y + 1, fw.0.z),
            CellPos::new(fw.0.x, fw.0.y + 12, fw.0.z),
        ),
    );
    t.apply_edits(&[clear_a, clear_b]);
    settle(t);
    note(format!(
        "fireweed after clearing a corridor: {}",
        describe(t, "fw", fw)
    ));
    let live = |t: &TestApp, c: CellPos| t.host_world().plant(c).is_some();
    note(format!("fireweed alive at harvest time: {}", live(t, fw)));
    let stand = CellPos::new(fw.0.x + 3, top_of(t, fw.0.x + 3, fw.0.z) + 1, fw.0.z);
    let feet = Vec3::new(
        stand.0.x as f32 * CELL_SIZE_M + CELL_SIZE_M / 2.0,
        stand.0.y as f32 * CELL_SIZE_M,
        stand.0.z as f32 * CELL_SIZE_M + CELL_SIZE_M / 2.0,
    );
    t.spawn_player(feet + Vec3::Y * 1.0);
    t.steps(30);
    {
        let world = t.app_mut().world_mut();
        let mut q = world.query::<&mut Player>();
        let mut pl = q.single_mut(world).expect("player");
        pl.feet = feet;
        pl.velocity = Vec3::ZERO;
    }
    t.steps(10);
    note(format!("player {:?}", t.player()));
    if let Err(e) = bots::look_at(t, fw.center_m()) {
        note(format!("look_at failed: {e:?}"));
    }
    t.steps(3);
    let hud = t.hud();
    note(format!(
        "POV at fireweed: target {:?} material {:?} (fw {fw:?}); crosshair {}",
        hud.target, hud.material, hud.crosshair
    ));
    shot(t, "pov-fireweed");

    // Editor open: the eco panel and cell readout.
    tap(t, TOGGLE_EDITOR_KEY);
    t.steps(3);
    note(format!("editor open: {}", t.editor().open));
    shot(t, "editor-panel-on-plant");
    t.editor().overlay = Some(LayerId::Growth);
    settle(t);
    t.steps(2);
    shot(t, "editor-panel-growth-overlay");
    t.editor().overlay = None;
    tap(t, TOGGLE_EDITOR_KEY);
    settle(t);
    t.steps(3);

    // Pickaxe: hold the swing on the fireweed.
    let before = t.inventory();
    let (fw_held, fw_growth) = t
        .host_world()
        .plant(fw)
        .map(|s| (s.held[usize::from(cu)], t.layer_u8(fw, LayerId::Growth)))
        .unwrap_or((0, 0));
    note(format!(
        "before swing: fireweed holds cu {fw_held}, growth {fw_growth}"
    ));
    t.mouse_down(SWING_BUTTON);
    let mut frames = 0;
    while live(t, fw) && frames < 240 {
        t.step();
        frames += 1;
    }
    t.mouse_up(SWING_BUTTON);
    t.steps(4);
    let after = t.inventory();
    note(format!(
        "swing: plant gone {} after {frames} frames; target {:?} material {:?}",
        !live(t, fw),
        t.hud().target,
        t.hud().material
    ));
    for (id, count) in after.items() {
        let was = before.count(id);
        if count != was {
            note(format!(
                "inventory {}: {} -> {}",
                materials.get(id).map(|d| d.key.clone()).unwrap_or_default(),
                was,
                count
            ));
        }
    }
    note(format!("HUD inventory: {:?}", t.hud().inventory));
    shot(t, "after-harvest");
    let l = t.host_world();
    note(format!(
        "cell after harvest {:?}, plant {:?}",
        l.get_cell(fw),
        l.plant(fw)
    ));
    assert_eq!(t.render_failure(), None);
}

/// The shipped default world (128 x 64 x 128), no fixtures: plants at
/// generation, then seasons, with what the surface looks like.
fn world_stage() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "playtest_20261023_world", |t| {
        settle(t);
        let eco = t.eco_tables().clone();
        let materials = eco.materials();
        let ice = materials.id("ice").expect("ice");
        let water = eco.role_material("water").expect("water");
        let plants = |t: &TestApp| -> usize {
            t.host_world()
                .iter_chunks_sorted()
                .map(|(_, c)| c.plants.len())
                .sum()
        };
        // The topmost non-air, non-water, non-ice cell of every column.
        let mut ground = Vec::new();
        for z in 0..64 {
            for x in 0..64 {
                let g = (0..128)
                    .rev()
                    .find(|&y| {
                        let m = t.host_world().get_cell(CellPos::new(x, y, z));
                        m != Some(MaterialId::AIR) && m != Some(ice) && m != Some(water)
                    })
                    .unwrap_or(0);
                ground.push(g);
            }
        }
        let surface = |t: &TestApp, label: &str| {
            let (mut sum, mut max, mut icen, mut watern, mut deep) = (0i64, 0, 0, 0, 0);
            for z in 0..64 {
                for x in 0..64 {
                    let g = ground[(z * 64 + x) as usize];
                    let mut top = g;
                    for y in g + 1..128 {
                        let m = t.host_world().get_cell(CellPos::new(x, y, z));
                        if m == Some(ice) {
                            icen += 1;
                            top = y;
                        } else if m == Some(water) {
                            watern += 1;
                            top = y;
                        }
                    }
                    let over = top - g;
                    sum += i64::from(over);
                    max = max.max(over);
                    deep += i32::from(over >= 3);
                }
            }
            note(format!(
                "WORLD {label}: plants {}; liquid/ice above the ground: mean {:.2} cells, max {max}, columns >=3 deep {deep}/4096; ice cells {icen}, water cells {watern}",
                plants(t),
                sum as f64 / 4096.0
            ));
        };
        note(format!("WORLD generated: plants {}", plants(t)));
        {
            let w = t.host_world();
            let cols = [(10, 10), (32, 32), (50, 20), (20, 50)];
            let desc: Vec<String> = cols
                .iter()
                .map(|&(x, z)| {
                    let sy = w.surface_y(x, z);
                    format!(
                        "({x},{z}) surface_y {sy} ground {} above={:?}",
                        ground[(z * 64 + x) as usize],
                        w.get_cell(CellPos::new(x, i32::from(sy) + 1, z))
                    )
                })
                .collect();
            note(format!("WORLD columns {desc:?}; bounds {:?}", w.bounds()));
        }
        surface(t, "s0");
        let look = |t: &mut TestApp, what: &str| {
            set_camera(t, Vec3::new(16.0, 62.0, 40.0), Vec3::new(16.0, 46.0, 16.0));
            t.steps(2);
            shot(t, what);
        };
        look(t, "world-overview-s0");
        for season in 1..=8 {
            t.advance_season();
            let l = t.ledger().clone();
            note(format!(
                "WORLD season {season}: rain {} born {} died {} evap {} steam_vent {}",
                l.rain_in, l.plants_born, l.plants_died, l.evap_to_steam, l.steam_vent_out
            ));
            if matches!(season, 2 | 4 | 8) {
                let col: Vec<String> = (88..128)
                    .map(|y| {
                        let m = t.host_world().get_cell(CellPos::new(32, y, 32));
                        match m {
                            Some(m) if m == MaterialId::AIR => ".".to_string(),
                            Some(m) if m == ice => "I".to_string(),
                            Some(m) if m == water => "w".to_string(),
                            Some(m) => {
                                format!("[{}]", materials.get(m).map_or("?", |d| d.key.as_str()))
                            }
                            None => "?".to_string(),
                        }
                    })
                    .collect();
                note(format!(
                    "WORLD column (32,*) y88..127 at s{season}: {}",
                    col.join("")
                ));
                surface(t, &format!("s{season}"));
                settle(t);
                let rcol: Vec<String> = (88..128)
                    .map(|y| {
                        let r = t.app().world().resource::<rmc_client::Replica>().world();
                        match r.get_cell(CellPos::new(32, y, 32)) {
                            Some(m) if m == MaterialId::AIR => ".".to_string(),
                            Some(m) if m == ice => "I".to_string(),
                            Some(m) if m == water => "w".to_string(),
                            Some(_) => "#".to_string(),
                            None => "?".to_string(),
                        }
                    })
                    .collect();
                note(format!(
                    "WORLD REPLICA column at s{season}: {}",
                    rcol.join("")
                ));
                look(t, &format!("world-overview-s{season}"));
            }
        }
        t.editor().overlay = Some(LayerId::Moisture);
        settle(t);
        look(t, "world-moisture-s8");
        t.editor().overlay = None;
        settle(t);
    });
}
