//! Tailings acidify the water downslope (SAD-P2 §12). A pile of crushed
//! sulfide sits on a slope just uphill of a contour swale with a shallow
//! pool in it. A soil ridge down the slope and a soil plug in the swale
//! split the site in two halves along z: the polluted half, with the pile
//! uphill of its swale, and a control half with the same swale and pool
//! and no pile.
//!
//! The tailings are placed damp. In two dry seasons they weather and
//! release their sulfur, copper and iron (§7.5), and the exposed sulfur
//! oxidizes to acid (§7.6 `oxidize`). In two seasons of light rain the
//! runoff washes the acid, and the copper it mobilizes, down into the
//! swale (§7.6 transport). After the four seasons the polluted swale's
//! water has pH < 4 and the swale holds more copper than the control
//! swale. The playtester judges the shot of both swales.
//!
//! While it rains, rain lands on every exposed face and the sheets cover
//! the pile, so `oxidize` (an air face neighbor) stops; at spring's rain
//! it never fires (PROGRESS.md, F147). Hence the dry seasons. Heavy rain
//! also fills the swale with clean water and carries the acid over the
//! berm, so the wet seasons are light.

use bevy::prelude::*;
use rmc_client::{GameCamera, LiquidMesh, MeshQueue, MeshStats};
use rmc_testkit::eco::{SLOPE_RUN, SWALE_DEPTH};
use rmc_testkit::{Fixture, FixtureSite, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::query;
use rmc_world::shape::Shape;

/// The swale's contour, in cells above the slope's low end (as in
/// `season_swale_fills`).
const CONTOUR: i32 = 5;
/// Every season's rain, in order: two dry seasons, then two of light rain.
const RAIN: [u8; 4] = [0, 0, 24, 24];
/// Every season's air temperature: spring's in `assets/seasons.ron`,
/// 10.0 °C, so the swale never freezes over.
const AIR_TEMP: i16 = 100;
/// The halves' boundary along z, in site offsets: the ridge and the plug
/// cover `dz = SPLIT.0..=SPLIT.1`. The polluted half is `dz < SPLIT.0`, the
/// control half `dz > SPLIT.1`; the fixture's swale spans `dz = 2..=29`,
/// so each half's swale is 12 cells long.
const SPLIT: (i32, i32) = (14, 17);
/// The last site offset along z (and x).
const LAST: i32 = 31;
/// Height of the soil walls that keep each half's runoff.
const RIM: i32 = 2;
/// The pile's lowest level, in site offsets: `dx` and `dz` ranges, on the
/// two steps of the slope just uphill of the swale, inside the polluted
/// half. Each level above is one cell smaller on every side.
const PILE_DX: (i32, i32) = (2, 7);
const PILE_DZ: (i32, i32) = (3, 12);
/// Levels of the pile.
const PILE_LEVELS: i32 = 2;
/// The pool in each swale: its bottom row of cells, holding this fill.
const POOL_FILL: i32 = 64;
/// A sump under the slope's low end holds the runoff, as in
/// `season_swale_fills`: the fixture site is a closed basin.
const SUMP_STEPS: i32 = 2;
const SUMP_DEPTH: i32 = 40;
/// The swale's pH must fall below this.
const ACID_PH: i32 = 4;

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
fn tailings_acidify_downslope() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "tailings_acidify_downslope", run);
}

fn shape(b: CellAabb) -> Shape {
    Shape::Box {
        min: b.min,
        max: b.max,
    }
}

/// The cells of `region`.
fn cells(region: CellAabb) -> impl Iterator<Item = CellPos> {
    let (min, max) = (region.min.0, region.max.0);
    (min.y..=max.y).flat_map(move |y| {
        (min.z..=max.z).flat_map(move |z| (min.x..=max.x).map(move |x| CellPos::new(x, y, z)))
    })
}

/// `region` with its z range replaced by the site offsets `dz0..=dz1`.
fn z_slice(site: &FixtureSite, region: CellAabb, (dz0, dz1): (i32, i32)) -> CellAabb {
    let z0 = site.site.min.0.z;
    CellAabb::new(
        CellPos::new(region.min.0.x, region.min.0.y, z0 + dz0),
        CellPos::new(region.max.0.x, region.max.0.y, z0 + dz1),
    )
}

/// The row of a substance layer: its index in the ledger's arrays.
fn substance_row(id: LayerId) -> usize {
    match id {
        LayerId::Substance(i) => usize::from(i),
        other => panic!("{other:?} is not a substance layer"),
    }
}

/// The substance layers the scenario reads.
struct Layers {
    acid: LayerId,
    lime: LayerId,
    cu: LayerId,
}

/// What the scenario reads of one swale on the host.
#[derive(Clone, Copy, Debug, Default)]
struct Swale {
    water_cells: u64,
    /// The sum of the water cells' fill.
    fill: u64,
    /// The sums of acid and lime over the water cells.
    acid: u64,
    lime: u64,
    /// The sum of copper over the swale's cells.
    cu: u64,
    /// The water cells whose own pH is below [`ACID_PH`].
    acid_cells: u64,
}

impl Swale {
    /// Reads the swale `region`.
    fn read(t: &TestApp, region: CellAabb, layers: &Layers) -> Self {
        let water = t
            .eco_tables()
            .role_material("water")
            .expect("a water material");
        let mut s = Swale {
            cu: t.sum_layer(layers.cu, region),
            ..Swale::default()
        };
        for c in cells(region) {
            if t.host_world().get_cell(c) != Some(water) {
                continue;
            }
            s.water_cells += 1;
            s.fill += u64::from(t.layer_u8(c, LayerId::Moisture));
            s.acid += u64::from(t.layer_u8(c, layers.acid));
            s.lime += u64::from(t.layer_u8(c, layers.lime));
            if i32::from(t.ph(c)) < ACID_PH {
                s.acid_cells += 1;
            }
        }
        s
    }

    /// The pH of the swale's water as a whole: the derived pH (§7.6) of its
    /// mean water cell, with every water cell counted alike. Neutral when
    /// it holds no water.
    fn ph(&self, ph_shift: u8) -> i32 {
        let mean = |x: u64| {
            i32::try_from(x.checked_div(self.water_cells).unwrap_or(0)).expect("a mean of u8s")
        };
        query::ph(mean(self.lime), mean(self.acid), ph_shift)
    }
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let rule = eco.ph_rule();
    let cu = eco.substance_index("cu").expect("substances.ron has cu");
    let layers = Layers {
        acid: rule.acid.expect("an acid role"),
        lime: rule.lime.expect("a lime role"),
        cu: LayerId::Substance(cu),
    };
    let materials = t.host_world().materials().clone();
    let tailings = materials
        .id("crushed_sulfide")
        .expect("materials.ron has crushed_sulfide");
    let damp = materials
        .get(tailings)
        .map(|d| d.field_capacity)
        .expect("crushed_sulfide is in the table");
    assert!(damp > 0, "crushed_sulfide holds water");
    let soil = materials
        .with_tag("soil")
        .next()
        .expect("a soil material")
        .id;
    let water = eco.role_material("water").expect("a water material");

    let site = t.fixture(Fixture::Swale(CONTOUR));
    let swale = site.feature;
    let (x0, z0) = (site.site.min.0.x, site.site.min.0.z);
    let level = swale.max.0.y;
    let berm_x = swale.max.0.x + 1;
    let polluted = z_slice(&site, swale, (swale.min.0.z - z0, SPLIT.0 - 1));
    let control = z_slice(&site, swale, (SPLIT.1 + 1, swale.max.0.z - z0));
    assert_eq!(
        polluted.max.0.z - polluted.min.0.z,
        control.max.0.z - control.min.0.z,
        "the halves' swales are the same size"
    );
    assert_eq!(
        x0 + PILE_DX.1 + 1,
        swale.min.0.x,
        "the pile ends just uphill of the swale"
    );
    assert!(
        z0 + PILE_DZ.0 > polluted.min.0.z && z0 + PILE_DZ.1 < polluted.max.0.z,
        "the pile is over the polluted half's swale"
    );

    let mut edits = Vec::new();
    // The plug: the swale's cells between the halves, back to soil.
    edits.push((EditOp::Set(soil), shape(z_slice(&site, swale, SPLIT))));
    // Soil walls RIM cells high on the ground: the ridge between the
    // halves, a rim along each half's outer z edge and one across the top
    // of the slope. They run from the top of the slope over the swale to
    // the berm, so each half keeps its own runoff: the terrain generated
    // around the site is lower than the slope's top, and water would leave
    // over the site's edges. The ground's top in column `x` is the slope's
    // surface, and the swale's contour over the swale and the berm.
    let top = |x: i32| {
        if x >= swale.min.0.x {
            level
        } else {
            site.surface_y(x)
        }
    };
    for x in x0..=berm_x {
        for (dz0, dz1) in [(0, 0), SPLIT, (LAST, LAST)] {
            edits.push((
                EditOp::Set(soil),
                Shape::Box {
                    min: CellPos::new(x, top(x) + 1, z0 + dz0),
                    max: CellPos::new(x, top(x) + RIM, z0 + dz1),
                },
            ));
        }
    }
    edits.push((
        EditOp::Set(soil),
        Shape::Box {
            min: CellPos::new(x0, top(x0) + 1, z0),
            max: CellPos::new(x0, top(x0) + RIM, z0 + LAST),
        },
    ));
    // The pile, a level at a time on the slope's surface, placed damp: at
    // the tailings' field capacity.
    let mut pile = Vec::new();
    for k in 0..PILE_LEVELS {
        for dx in PILE_DX.0 + k..=PILE_DX.1 - k {
            let y = site.surface_y(x0 + dx) + 1 + k;
            let row = CellAabb::new(
                CellPos::new(x0 + dx, y, z0 + PILE_DZ.0 + k),
                CellPos::new(x0 + dx, y, z0 + PILE_DZ.1 - k),
            );
            pile.extend(cells(row));
            edits.push((EditOp::Set(tailings), shape(row)));
            edits.push((
                EditOp::SetLayer {
                    layer: LayerId::Moisture,
                    value: i32::from(damp),
                },
                shape(row),
            ));
        }
    }
    // The pools: each swale's bottom row of cells, part full.
    for r in [polluted, control] {
        let bottom = CellAabb::new(r.min, CellPos::new(r.max.0.x, r.min.0.y, r.max.0.z));
        edits.push((EditOp::Set(water), shape(bottom)));
        edits.push((
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: POOL_FILL,
            },
            shape(bottom),
        ));
    }
    // The sump.
    let sump_x = site.site.max.0.x - SUMP_STEPS * SLOPE_RUN + 1;
    let sump = CellAabb::new(
        CellPos::new(sump_x, site.ground_y - SUMP_DEPTH, site.site.min.0.z),
        CellPos::new(site.site.max.0.x, site.surface_y(sump_x), site.site.max.0.z),
    );
    assert!(
        sump.min.0.x > berm_x + 1,
        "the sump is downhill of the berm"
    );
    edits.push((EditOp::Remove, shape(sump)));
    t.apply_edits(&edits);

    for &c in &pile {
        assert_eq!(
            t.host_world().get_cell(c),
            Some(tailings),
            "{c:?} is tailings"
        );
        assert_eq!(t.layer_u8(c, LayerId::Moisture), damp, "{c:?} is damp");
    }
    let pool_cells = u64::try_from(cells(polluted).count() / SWALE_DEPTH as usize).unwrap();
    for (name, region) in [("polluted", polluted), ("control", control)] {
        let s = Swale::read(t, region, &layers);
        assert_eq!(s.water_cells, pool_cells, "the {name} swale holds its pool");
        assert_eq!(
            (s.acid, s.lime, s.cu),
            (0, 0, 0),
            "the {name} swale starts clean"
        );
        assert_eq!(s.ph(rule.ph_shift), 7, "the {name} swale starts neutral");
    }

    let sum_pile = |t: &TestApp, id: LayerId| -> u64 {
        pile.iter().map(|&c| u64::from(t.layer_u8(c, id))).sum()
    };
    // The pile's acid and copper at the end of the last dry season.
    let mut after_dry = (0, 0);
    for (season, &rain) in RAIN.iter().enumerate() {
        t.set_season_params(SeasonOverride {
            rain: Some(rain),
            air_temp: Some(AIR_TEMP),
        });
        t.advance_season();
        let (p, c) = (
            Swale::read(t, polluted, &layers),
            Swale::read(t, control, &layers),
        );
        let oxidized = t.ledger().react_delta[substance_row(layers.acid)];
        eprintln!(
            "season {season} (rain {rain}): acid made {oxidized}; pile acid {}, cu {}; \
             polluted {p:?} pH {}; control {c:?} pH {}",
            sum_pile(t, layers.acid),
            sum_pile(t, layers.cu),
            p.ph(rule.ph_shift),
            c.ph(rule.ph_shift),
        );
        if rain == 0 {
            assert!(oxidized > 0, "season {season}: the dry pile oxidizes");
            after_dry = (sum_pile(t, layers.acid), sum_pile(t, layers.cu));
        }
    }
    // The runoff took them from the pile.
    let washed = (sum_pile(t, layers.acid), sum_pile(t, layers.cu));
    assert!(
        washed.0 < after_dry.0 && washed.1 < after_dry.1,
        "the rain washes acid and copper out of the pile: {after_dry:?} -> {washed:?}"
    );
    assert_eq!(t.season_log().len(), RAIN.len());

    let (p, c) = (
        Swale::read(t, polluted, &layers),
        Swale::read(t, control, &layers),
    );
    assert!(p.water_cells > 0, "the polluted swale holds water");
    assert!(c.water_cells > 0, "the control swale holds water");
    assert!(
        p.ph(rule.ph_shift) < ACID_PH,
        "the polluted swale's water has pH {} (acid {}, lime {} over {} water cells), \
         want < {ACID_PH}",
        p.ph(rule.ph_shift),
        p.acid,
        p.lime,
        p.water_cells
    );
    assert!(
        p.acid_cells > 0,
        "some of the polluted swale's water cells are below pH {ACID_PH}: {p:?}"
    );
    assert!(
        p.cu > c.cu,
        "copper in the polluted swale {} > the control swale {}",
        p.cu,
        c.cu
    );
    assert!(
        c.ph(rule.ph_shift) >= ACID_PH && c.acid_cells == 0,
        "the control swale stays at pH >= {ACID_PH}: {c:?}"
    );

    shoot(t, polluted, control, "tailings_acidify_downslope");
}

/// Saves a screenshot from above the slope downhill of the berm, looking
/// up the slope at both swales and the pile: the polluted swale's water is
/// tinted by what it carries (F146), the control's is clear.
fn shoot(t: &mut TestApp, polluted: CellAabb, control: CellAabb, name: &str) {
    t.steps(3);
    settle(t);
    {
        let (min, max) = (polluted.min.0, control.max.0);
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            (max.y + 1) as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let eye = centre + Vec3::new(9.0, 8.0, 0.0);
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
    }
    t.steps(2);
    {
        let world = t.app_mut().world_mut();
        let quads: u32 = world
            .query::<&LiquidMesh>()
            .iter(world)
            .map(|m| m.quads)
            .sum();
        assert!(quads > 0, "{name}: the water is meshed");
    }
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, name, t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "{name}: render failure: {:?}",
        t.render_failure()
    );
}
