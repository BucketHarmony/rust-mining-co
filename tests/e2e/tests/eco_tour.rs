//! A video tour of the Phase 2 ecology (SAD-P2 §7, §12): not a check of the
//! game, a recording of it. Each segment builds a fixture in a fresh
//! 64 × 128 × 64 world, runs seasons, and saves orbiting camera frames to
//! `<out>/eco_tour/<segment>/NNNNN.png` with a `manifest.tsv` of captions.
//! `scripts/eco-tour.sh` runs this scenario and turns the frames into
//! `target/video/eco_tour.mp4`.
//!
//! Environment:
//! - `TOUR_ONLY=water,meadow`: record only these segments.
//! - `TOUR_SIZE=1280x720`: frame size (default 960x540).
//! - `TOUR_FRAMES=0.25`: scale every orbit's frame count, for a quick draft.

use std::fmt::Write as _;
use std::path::PathBuf;

use bevy::prelude::*;
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::eco::{FIXTURE_SOIL_DEPTH, SLOPE_RUN};
use rmc_testkit::{Fixture, FixtureSite, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

const NAME: &str = "eco_tour";
/// Frames in a full-length orbit move (before `TOUR_FRAMES`).
const ORBIT: u32 = 150;
/// Mild, dry-ish weather for segments that do not test the weather.
const MILD: i16 = 120;
/// The water segment's wet seasons: enough to fill the swale without
/// drowning the slope.
const RAIN: u8 = 16;
/// Rain in the tailings and limestone segments' wet seasons.
const ACID_RAIN: u8 = 12;

type Segment = (&'static str, fn(&mut Tour));

const SEGMENTS: [Segment; 9] = [
    ("intro", intro),
    ("water", water),
    ("seepage", seepage),
    ("meadow", meadow),
    ("tailings", tailings),
    ("limestone", limestone),
    ("freeze", freeze),
    ("magma", magma),
    ("firedamp", firedamp),
];

#[test]
#[ignore]
fn eco_tour() {
    let only: Option<Vec<String>> = std::env::var("TOUR_ONLY")
        .ok()
        .map(|s| s.split(',').map(|w| w.trim().to_string()).collect());
    let size = std::env::var("TOUR_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((960, 540));
    let scale: f32 = std::env::var("TOUR_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    for (name, body) in SEGMENTS {
        if only.as_ref().is_some_and(|o| !o.iter().any(|w| w == name)) {
            continue;
        }
        let started = std::time::Instant::now();
        let mut t = TestApp::new(TestConfig {
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
            size,
            ..TestConfig::default()
        });
        let mut tour = Tour::new(name, &mut t, scale);
        settle(tour.t);
        body(&mut tour);
        tour.finish();
        eprintln!(
            "segment {name}: {} frames in {:.1} s",
            tour.n,
            started.elapsed().as_secs_f32()
        );
        assert_eq!(t.render_failure(), None);
    }
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
    eprintln!("meshing did not settle in 240 steps");
}

fn cube(min: CellPos, max: CellPos) -> Shape {
    Shape::Box { min, max }
}

fn boxed(b: CellAabb) -> Shape {
    cube(b.min, b.max)
}

fn set_camera(t: &mut TestApp, eye: Vec3, at: Vec3) {
    let world = t.app_mut().world_mut();
    let mut camera = world
        .query_filtered::<&mut Transform, With<GameCamera>>()
        .single_mut(world)
        .expect("one game camera");
    *camera = Transform::from_translation(eye).looking_at(at, Vec3::Y);
}

/// A camera path around `at`: `radius` metres out, `height` above it,
/// starting at `angle` degrees (0 is +x, 90 is +z).
#[derive(Clone, Copy, Debug)]
struct Orbit {
    at: Vec3,
    radius: f32,
    height: f32,
    angle: f32,
}

impl Orbit {
    fn eye(&self, angle: f32) -> Vec3 {
        let a = angle.to_radians();
        self.at + Vec3::new(self.radius * a.cos(), self.height, self.radius * a.sin())
    }
}

/// One segment's recorder.
struct Tour<'a> {
    t: &'a mut TestApp,
    name: &'static str,
    dir: PathBuf,
    manifest: String,
    n: u32,
    scale: f32,
    orbit: Orbit,
}

impl<'a> Tour<'a> {
    fn new(name: &'static str, t: &'a mut TestApp, scale: f32) -> Self {
        let dir = e2e::out_dir().join(NAME).join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear the segment's frames");
        }
        std::fs::create_dir_all(&dir).expect("create the segment's directory");
        Self {
            t,
            name,
            dir,
            manifest: String::new(),
            n: 0,
            scale,
            orbit: Orbit {
                at: Vec3::ZERO,
                radius: 10.0,
                height: 6.0,
                angle: 0.0,
            },
        }
    }

    fn finish(&mut self) {
        std::fs::write(self.dir.join("manifest.tsv"), &self.manifest).expect("write the manifest");
    }

    /// A title card the assembly script draws: `seconds` long.
    fn title(&mut self, title: &str, subtitle: &str, seconds: f32) {
        writeln!(self.manifest, "title\t{title}\t{subtitle}\t{seconds}").unwrap();
    }

    /// Saves what the camera sees as the next frame, captioned.
    fn frame(&mut self, caption: &str) {
        let image = self.t.screenshot();
        let file = format!("{:05}.png", self.n);
        image.save(self.dir.join(&file)).expect("save a frame");
        writeln!(self.manifest, "frame\t{file}\t{caption}").unwrap();
        self.n += 1;
    }

    /// The current view held still for `seconds`.
    fn hold(&mut self, caption: &str, seconds: f32) {
        let image = self.t.screenshot();
        let file = format!("{:05}.png", self.n);
        image.save(self.dir.join(&file)).expect("save a frame");
        writeln!(self.manifest, "hold\t{file}\t{caption}\t{seconds}").unwrap();
        self.n += 1;
    }

    /// Points the camera from the orbit's current angle.
    fn place(&mut self) {
        let o = self.orbit;
        set_camera(self.t, o.eye(o.angle), o.at);
        self.t.steps(2);
    }

    /// Turns the orbit `degrees` over a full-length move scaled by
    /// `length`, a frame each step.
    fn sweep(&mut self, degrees: f32, length: f32, caption: &str) {
        let frames = ((ORBIT as f32 * length * self.scale).round() as u32).max(2);
        let start = self.orbit.angle;
        for i in 0..frames {
            // Ease in and out so cuts between moves are not abrupt.
            let s = i as f32 / (frames - 1) as f32;
            let e = s * s * (3.0 - 2.0 * s);
            self.orbit.angle = start + degrees * e;
            let o = self.orbit;
            set_camera(self.t, o.eye(o.angle), o.at);
            self.frame(caption);
        }
    }

    /// Shows `layer` as the editor overlay (or none), then settles.
    fn overlay(&mut self, layer: Option<LayerId>) {
        self.t.editor().overlay = layer;
        settle(self.t);
        self.t.steps(2);
    }

    /// Runs one season and settles the mesh.
    fn season(&mut self) {
        self.t.advance_season();
        settle(self.t);
        eprintln!("{}: season {:?}", self.name, self.t.host().season());
    }
}

fn site_centre(site: &FixtureSite, y: i32) -> Vec3 {
    let (lo, hi) = (site.site.min.0, site.site.max.0);
    Vec3::new(
        (lo.x + hi.x + 1) as f32 / 2.0,
        y as f32,
        (lo.z + hi.z + 1) as f32 / 2.0,
    ) * CELL_SIZE_M
}

/// The fixtures' sump: the last `steps` slope steps dug `depth` below the
/// ground level, so water that runs off the site drains away from it.
fn sump(site: &FixtureSite, steps: i32, depth: i32) -> CellAabb {
    let x = site.site.max.0.x - steps * SLOPE_RUN + 1;
    CellAabb::new(
        CellPos::new(x, site.ground_y - depth, site.site.min.0.z),
        CellPos::new(site.site.max.0.x, site.surface_y(x), site.site.max.0.z),
    )
}

fn substance(t: &TestApp, key: &str) -> LayerId {
    LayerId::Substance(
        t.eco_tables()
            .substance_index(key)
            .unwrap_or_else(|| panic!("no substance {key}")),
    )
}

fn gas(t: &TestApp, key: &str) -> LayerId {
    LayerId::Gas(
        t.eco_tables()
            .gas_index(key)
            .unwrap_or_else(|| panic!("no gas {key}")),
    )
}

fn material(t: &TestApp, key: &str) -> MaterialId {
    t.eco_tables()
        .materials()
        .id(key)
        .unwrap_or_else(|| panic!("no material {key}"))
}

// ---------------------------------------------------------------- segments

fn intro(tour: &mut Tour) {
    tour.title(
        "Rust Mining Co",
        "Phase 2: the living ground. A tour of the ecology.",
        4.0,
    );
    let ground = i32::from(tour.t.host_world().surface_y(32, 32));
    tour.orbit = Orbit {
        at: Vec3::new(32.0, ground as f32, 32.0) * CELL_SIZE_M,
        radius: 24.0,
        height: 12.0,
        angle: 200.0,
    };
    tour.place();
    tour.sweep(
        60.0,
        1.2,
        "A generated world: every cell carries temperature, water, nutrients and gas",
    );
    for (layer, caption) in [
        (
            LayerId::Temperature,
            "Temperature overlay: warm deep rock, cold surface",
        ),
        (LayerId::Moisture, "Moisture overlay: the water table"),
        (
            LayerId::Light,
            "Light overlay: sun on the surface, dark below",
        ),
    ] {
        tour.overlay(Some(layer));
        tour.sweep(25.0, 0.6, caption);
    }
    tour.overlay(None);
}

fn water(tour: &mut Tour) {
    tour.title(
        "Water",
        "Rain falls, runs downhill and pools. A swale dug along the contour catches it.",
        4.0,
    );
    let site = tour.t.fixture(Fixture::Swale(5));
    let swale = site.feature;
    tour.t
        .apply_edits(&[(EditOp::Remove, boxed(sump(&site, 2, 40)))]);
    tour.orbit = Orbit {
        at: site_centre(&site, swale.max.0.y),
        radius: 11.0,
        height: 7.0,
        angle: 0.0,
    };
    tour.place();
    tour.sweep(
        -30.0,
        0.8,
        "A slope with a swale dug along its contour, dry",
    );
    tour.t.set_season_params(SeasonOverride {
        rain: Some(RAIN),
        air_temp: Some(MILD),
    });
    for s in 1..=2 {
        tour.season();
        tour.sweep(
            -25.0,
            0.7,
            &format!("Season {s}: rain. Water pools on every flat step and in the swale"),
        );
    }
    tour.t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: Some(MILD),
    });
    tour.season();
    tour.sweep(
        -20.0,
        0.6,
        "A dry season: the steps dry out; the swale holds its water",
    );
    tour.overlay(Some(LayerId::Moisture));
    tour.sweep(
        -40.0,
        0.9,
        "Moisture overlay: the soil under the swale is wet",
    );
    tour.overlay(None);
}

fn seepage(tour: &mut Tour) {
    tour.title(
        "Groundwater",
        "Dig below the water table and the pit becomes a pond, with no rain at all.",
        4.0,
    );
    let (x0, z0) = (28, 28);
    let table = (x0..x0 + 8)
        .flat_map(|x| (z0..z0 + 8).map(move |z| (x, z)))
        .filter_map(|(x, z)| tour.t.water_table_y(x, z))
        .min()
        .expect("a water table");
    let top = (x0..x0 + 8)
        .flat_map(|x| (z0..z0 + 8).map(move |z| (x, z)))
        .map(|(x, z)| i32::from(tour.t.host_world().surface_y(x, z)))
        .max()
        .unwrap();
    eprintln!("seepage: water table {table}, surface {top}");
    let pit = CellAabb::new(
        CellPos::new(x0, table - 3, z0),
        CellPos::new(x0 + 7, top + 2, z0 + 7),
    );
    let at = Vec3::new(x0 as f32 + 4.0, table as f32, z0 as f32 + 4.0) * CELL_SIZE_M;
    tour.orbit = Orbit {
        at,
        radius: 3.0,
        height: (top + 1 - table) as f32 * CELL_SIZE_M + 6.0,
        angle: 30.0,
    };
    tour.place();
    tour.overlay(Some(LayerId::Moisture));
    tour.sweep(
        25.0,
        0.6,
        "Moisture overlay: wet ground below the water table",
    );
    tour.overlay(None);
    tour.t.apply_edits(&[(EditOp::Remove, boxed(pit))]);
    settle(tour.t);
    tour.sweep(25.0, 0.6, "A pit dug three cells below the water table");
    tour.t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: Some(MILD),
    });
    for s in 1..=2 {
        tour.season();
        tour.sweep(
            25.0,
            0.6,
            &format!("Season {s}, no rain: groundwater seeps in"),
        );
    }
}

fn meadow(tour: &mut Tour) {
    tour.title(
        "Plants",
        "Grass, moss, shrubs and fireweed grow from water, nitrogen and phosphorus.",
        4.0,
    );
    let t = &mut *tour.t;
    let (loam, grass, moss, fireweed, shrub) = (
        material(t, "loam"),
        material(t, "grass"),
        material(t, "moss"),
        material(t, "fireweed"),
        material(t, "shrub"),
    );
    let water = t.eco_tables().role_material("water").expect("water");
    let (n, p, cu) = (substance(t, "n"), substance(t, "p"), substance(t, "cu"));
    let site = t.fixture(Fixture::Swale(2));
    let swale = site.feature;
    let (sx0, sx1) = (site.site.min.0.x, site.site.max.0.x);
    let (z0, z1) = (site.site.min.0.z, site.site.max.0.z);
    let mut edits = Vec::new();
    for x in sx0..=sx1 {
        let top = site.surface_y(x);
        let soil = (
            CellPos::new(x, top - FIXTURE_SOIL_DEPTH + 1, z0),
            CellPos::new(x, top + 1, z1),
        );
        edits.push((EditOp::Paint(loam), cube(soil.0, soil.1)));
        for layer in [n, p] {
            edits.push((EditOp::SetLayer { layer, value: 96 }, cube(soil.0, soil.1)));
        }
    }
    edits.push((EditOp::Set(water), boxed(swale)));
    // A copper patch uphill.
    let (cx0, cx1, cz0, cz1) = (sx0 + 3, sx0 + 8, z0 + 10, z0 + 15);
    for x in cx0..=cx1 {
        let top = site.surface_y(x);
        edits.push((
            EditOp::SetLayer {
                layer: cu,
                value: 120,
            },
            cube(CellPos::new(x, top - 1, cz0), CellPos::new(x, top, cz1)),
        ));
    }
    t.apply_edits(&edits);
    let mut plant = Vec::new();
    for x in sx0..=sx1 {
        let top = site.surface_y(x);
        for z in z0..=z1 {
            if t.host_world().get_cell(CellPos::new(x, top, z)) != Some(loam) {
                continue;
            }
            let m = if (cx0..=cx1).contains(&x) && (cz0..=cz1).contains(&z) {
                if (x + z) % 2 == 0 { fireweed } else { grass }
            } else if (x + z) % 11 == 0 {
                moss
            } else if (x + z) % 7 == 0 && x > swale.max.0.x {
                shrub
            } else if (x * 3 + z) % 4 == 0 {
                grass
            } else {
                continue;
            };
            plant.push((EditOp::Set(m), Shape::Cell(CellPos::new(x, top + 1, z))));
        }
    }
    t.apply_edits(&plant);
    t.set_season_params(SeasonOverride {
        rain: Some(8),
        air_temp: Some(MILD),
    });
    settle(t);
    tour.orbit = Orbit {
        at: site_centre(&site, site.ground_y + 3),
        radius: 12.0,
        height: 7.5,
        angle: 0.0,
    };
    tour.place();
    tour.sweep(
        -30.0,
        0.8,
        "Seedlings on fertile loam; fireweed (pink) on a copper-rich patch",
    );
    for s in 1..=4 {
        tour.season();
        tour.sweep(
            -15.0,
            0.55,
            &format!("Season {s}: plants grow, spread into bare soil, and die back"),
        );
    }
    tour.overlay(Some(LayerId::Growth));
    tour.sweep(-30.0, 0.7, "Growth overlay: how far along each plant is");
    tour.overlay(Some(cu));
    tour.sweep(
        -30.0,
        0.7,
        "Copper overlay: fireweed draws copper out of the soil into its leaves",
    );
    tour.overlay(None);
}

/// Builds a damp pile of `tailings` on the slope's steps `dx`, across
/// `dz`, two levels high.
fn pile(t: &mut TestApp, site: &FixtureSite, dx: (i32, i32), dz: (i32, i32)) {
    let tailings = material(t, "crushed_sulfide");
    let damp = t
        .eco_tables()
        .materials()
        .get(tailings)
        .map(|d| d.field_capacity)
        .expect("crushed_sulfide");
    let (x0, z0) = (site.site.min.0.x, site.site.min.0.z);
    let mut edits = Vec::new();
    for k in 0..2 {
        for x in x0 + dx.0 + k..=x0 + dx.1 - k {
            let y = site.surface_y(x) + 1 + k;
            let row = cube(
                CellPos::new(x, y, z0 + dz.0 + k),
                CellPos::new(x, y, z0 + dz.1 - k),
            );
            edits.push((EditOp::Set(tailings), row));
            edits.push((
                EditOp::SetLayer {
                    layer: LayerId::Moisture,
                    value: i32::from(damp),
                },
                row,
            ));
        }
    }
    t.apply_edits(&edits);
}

fn tailings(tour: &mut Tour) {
    tour.title(
        "Acid drainage",
        "Crushed sulfide ore left in the rain makes acid, and the acid carries copper downhill.",
        4.0,
    );
    let site = tour.t.fixture(Fixture::Swale(5));
    let swale = site.feature;
    let water = tour.t.eco_tables().role_material("water").expect("water");
    let bottom = CellAabb::new(
        swale.min,
        CellPos::new(swale.max.0.x, swale.min.0.y, swale.max.0.z),
    );
    tour.t.apply_edits(&[
        (EditOp::Remove, boxed(sump(&site, 2, 40))),
        (EditOp::Set(water), boxed(bottom)),
    ]);
    pile(tour.t, &site, (2, 7), (3, 28));
    settle(tour.t);
    let (acid, cu) = (substance(tour.t, "acid"), substance(tour.t, "cu"));
    tour.orbit = Orbit {
        at: site_centre(&site, swale.max.0.y),
        radius: 12.0,
        height: 8.5,
        angle: 20.0,
    };
    tour.place();
    tour.sweep(-30.0, 0.8, "A tailings pile above a pond in the swale");
    for (s, rain) in [(1, 0), (2, 0), (3, ACID_RAIN), (4, ACID_RAIN)] {
        tour.t.set_season_params(SeasonOverride {
            rain: Some(rain),
            air_temp: Some(100),
        });
        tour.season();
        let what = if rain > 0 {
            "rain washes it downhill"
        } else {
            "the damp pile oxidizes"
        };
        tour.sweep(-15.0, 0.55, &format!("Season {s}: {what}"));
    }
    tour.overlay(Some(acid));
    tour.sweep(
        -30.0,
        0.8,
        "Acid overlay: where the pile's acid has reached",
    );
    tour.overlay(Some(cu));
    tour.sweep(-30.0, 0.8, "Copper overlay: metal leached out of the ore");
    tour.overlay(None);
}

fn limestone(tour: &mut Tour) {
    tour.title(
        "Neutralizing",
        "Water running over limestone loses its acid, leaving salt behind.",
        4.0,
    );
    let limestone = material(tour.t, "limestone");
    let site = tour.t.fixture(Fixture::Channel(limestone));
    tour.t
        .apply_edits(&[(EditOp::Remove, boxed(sump(&site, 1, 40)))]);
    pile(tour.t, &site, (0, 4), (12, 19));
    settle(tour.t);
    let (acid, salt) = (substance(tour.t, "acid"), substance(tour.t, "salt"));
    tour.orbit = Orbit {
        at: site_centre(&site, site.ground_y + 4),
        radius: 12.0,
        height: 8.0,
        angle: 60.0,
    };
    tour.place();
    tour.sweep(
        -30.0,
        0.8,
        "A limestone-lined channel runs down from a tailings pile",
    );
    for (s, rain) in [(1, 0), (2, 0), (3, ACID_RAIN), (4, ACID_RAIN)] {
        tour.t.set_season_params(SeasonOverride {
            rain: Some(rain),
            air_temp: Some(100),
        });
        tour.season();
        tour.sweep(-15.0, 0.55, &format!("Season {s}"));
    }
    tour.overlay(Some(acid));
    tour.sweep(
        -30.0,
        0.8,
        "Acid overlay: acid around the pile and down the channel",
    );
    tour.overlay(Some(salt));
    tour.sweep(
        -30.0,
        0.8,
        "Salt overlay: what the limestone left in the water",
    );
    tour.overlay(None);
}

fn freeze(tour: &mut Tour) {
    tour.title(
        "Frost",
        "Water in the cracks freezes and thaws, and the stone weathers to rubble.",
        4.0,
    );
    let materials = tour.t.host_world().materials().clone();
    let wall = materials
        .with_tag("stone")
        .find(|d| d.weathering_rate > 0 && d.weathers_to.is_some())
        .expect("a stone that weathers")
        .id;
    let site = tour.t.fixture(Fixture::Trench);
    let trench = site.feature;
    let top = trench.max.0.y;
    let lining = CellAabb::new(
        CellPos::new(trench.min.0.x - 1, top - 3, trench.min.0.z - 1),
        CellPos::new(trench.max.0.x + 1, top - 1, trench.max.0.z + 1),
    );
    tour.t.apply_edits(&[
        (EditOp::Set(wall), boxed(lining)),
        (EditOp::Remove, boxed(trench)),
        (EditOp::Remove, boxed(sump(&site, 2, 40))),
    ]);
    // Dry seasons until the one before the first freezing one, so the
    // slope is not under ice before the tour starts.
    tour.t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });
    settle(tour.t);
    let rows = tour.t.eco_tables().seasons().seasons.clone();
    let len = rows.len();
    let freeze = rows.iter().position(|r| r.air_temp < 0).expect("a frost");
    let first = (freeze + len - 1) % len;
    while tour.t.host().season().expect("seasons") as usize % len != first {
        tour.t.advance_season();
    }
    settle(tour.t);
    tour.orbit = Orbit {
        at: site_centre(&site, top),
        radius: 10.0,
        height: 7.5,
        angle: 10.0,
    };
    tour.place();
    tour.sweep(-30.0, 0.8, "A trench cut through weatherable stone");
    tour.t.set_season_params(SeasonOverride {
        rain: Some(RAIN),
        air_temp: None,
    });
    for s in 1..=4 {
        let row = &rows[(first + s - 1) % len];
        let caption = format!(
            "Season {s}: air {:.0} °C{}",
            f32::from(row.air_temp) / 10.0,
            if row.air_temp < 0 {
                ", water freezes to ice"
            } else {
                ", thaw"
            }
        );
        tour.season();
        tour.sweep(-15.0, 0.55, &caption);
    }
    tour.overlay(Some(LayerId::Regolith));
    tour.sweep(
        -30.0,
        0.8,
        "Regolith overlay: rubble weathered from the walls",
    );
    tour.overlay(Some(LayerId::Temperature));
    tour.sweep(-20.0, 0.6, "Temperature overlay");
    tour.overlay(None);
}

fn magma(tour: &mut Tour) {
    tour.title(
        "Fire and water",
        "A shaft lets a pool drain onto magma: the water boils to steam, the magma sets to obsidian.",
        4.0,
    );
    let t = &mut *tour.t;
    let site = t.fixture(Fixture::Pool);
    let pool = site.feature;
    let floor = pool.min.0.y;
    let (x0, z0) = (site.site.min.0.x, site.site.min.0.z);
    let pocket_y = floor - 3;
    let square = |lo: i32, hi: i32, y0: i32, y1: i32| {
        CellAabb::new(
            CellPos::new(x0 + lo, y0, z0 + lo),
            CellPos::new(x0 + hi, y1, z0 + hi),
        )
    };
    let pocket = square(14, 17, pocket_y, pocket_y);
    let shaft = square(15, 16, pocket_y + 1, floor - 1);
    let magma = t.eco_tables().role_material("magma").expect("magma");
    let warm = CellAabb::new(
        CellPos::new(pool.min.0.x, pocket_y + 1, pool.min.0.z),
        pool.max,
    );
    t.apply_edits(&[
        (EditOp::Set(magma), boxed(pocket)),
        (
            EditOp::SetLayer {
                layer: LayerId::Temperature,
                value: 100,
            },
            boxed(warm),
        ),
    ]);
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: Some(100),
    });
    settle(t);
    let steam = gas(t, "steam");
    tour.orbit = Orbit {
        at: site_centre(&site, floor),
        radius: 7.0,
        height: 8.0,
        angle: 45.0,
    };
    tour.place();
    tour.sweep(
        -25.0,
        0.6,
        "A pool, with a magma pocket two cells under its floor",
    );
    tour.overlay(Some(LayerId::Temperature));
    tour.sweep(
        -25.0,
        0.6,
        "Temperature overlay: the pocket's heat in the rock",
    );
    tour.overlay(None);
    tour.t.apply_edits(&[(EditOp::Remove, boxed(shaft))]);
    settle(tour.t);
    tour.hold("A shaft is dug through the pool's floor", 1.5);
    tour.season();
    tour.sweep(
        -25.0,
        0.6,
        "One season later: the pool has boiled away into the pocket",
    );
    tour.overlay(Some(steam));
    tour.sweep(
        -25.0,
        0.6,
        "Steam overlay: the vapour rising out of the shaft",
    );
    tour.overlay(None);
    // Down the shaft: obsidian where the magma met the water.
    let eye = Vec3::new((x0 + 16) as f32, (floor + 4) as f32, (z0 + 16) as f32 + 0.3) * CELL_SIZE_M;
    let down = Vec3::new((x0 + 16) as f32, pocket_y as f32, (z0 + 16) as f32) * CELL_SIZE_M;
    set_camera(tour.t, eye, down);
    tour.t.steps(2);
    tour.hold(
        "Looking down the shaft: dark obsidian where magma met water",
        3.0,
    );
}

fn firedamp(tour: &mut Tour) {
    tour.title(
        "Bad air",
        "Coal gives off firedamp. In a sealed working it builds up under the roof.",
        4.0,
    );
    let t = &mut *tour.t;
    let seam = material(t, "coal_seam");
    let fd = gas(t, "firedamp");
    let site = t.fixture(Fixture::SealedCave);
    let f = site.feature;
    let cave = CellAabb::new(CellPos::new(f.min.0.x, f.min.0.y + 2, f.min.0.z), f.max);
    let grown = CellAabb::new(
        CellPos::new(f.min.0.x - 1, f.min.0.y - 1, f.min.0.z - 1),
        CellPos::new(f.max.0.x + 1, f.max.0.y + 1, f.max.0.z + 1),
    );
    let shaft = CellAabb::new(
        CellPos::new(cave.min.0.x, cave.max.0.y + 1, cave.min.0.z),
        CellPos::new(cave.max.0.x, site.ground_y, cave.max.0.z),
    );
    t.apply_edits(&[
        (EditOp::Set(seam), boxed(grown)),
        (EditOp::Remove, boxed(cave)),
    ]);
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });
    settle(t);
    // Inside the cave, at one end, looking along it.
    let y = cave.max.0.y;
    let inside = Vec3::new(
        cave.min.0.x as f32 + 0.5,
        y as f32 + 0.3,
        cave.min.0.z as f32 + 0.5,
    ) * CELL_SIZE_M;
    let along = Vec3::new(
        cave.max.0.x as f32 + 0.5,
        cave.min.0.y as f32,
        cave.max.0.z as f32 + 0.5,
    ) * CELL_SIZE_M;
    set_camera(tour.t, inside, along);
    tour.t.steps(2);
    tour.hold("Inside a sealed cave cut into a coal seam", 2.5);
    for s in 1..=3 {
        tour.season();
        set_camera(tour.t, inside, along);
        tour.t.steps(2);
        tour.hold(
            &format!("Season {s}, sealed: the air thickens; the lantern warns"),
            2.0,
        );
    }
    tour.overlay(Some(fd));
    set_camera(tour.t, inside, along);
    tour.t.steps(2);
    tour.hold("Firedamp overlay: the gas pooled under the roof", 3.0);
    tour.overlay(None);
    tour.t.apply_edits(&[(EditOp::Remove, boxed(shaft))]);
    settle(tour.t);
    let centre = Vec3::new(
        (cave.min.0.x + cave.max.0.x + 1) as f32 / 2.0,
        cave.min.0.y as f32,
        (cave.min.0.z + cave.max.0.z + 1) as f32 / 2.0,
    ) * CELL_SIZE_M;
    tour.orbit = Orbit {
        at: centre,
        radius: 6.0,
        height: (site.ground_y - cave.min.0.y) as f32 * CELL_SIZE_M + 6.0,
        angle: 30.0,
    };
    tour.place();
    tour.sweep(-25.0, 0.6, "A shaft is opened to the surface");
    tour.season();
    tour.overlay(Some(fd));
    tour.sweep(
        -25.0,
        0.6,
        "One season later: the light gas has vented up the shaft",
    );
    tour.overlay(None);
    set_camera(tour.t, inside, along);
    tour.t.steps(2);
    tour.hold("Back inside: clear air, the lantern is calm", 2.5);
    tour.title(
        "Rust Mining Co",
        "Every rate and threshold in this tour lives in assets/eco.ron.",
        4.0,
    );
}
