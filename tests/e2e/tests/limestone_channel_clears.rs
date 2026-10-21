//! A limestone channel clears acid runoff (SAD-P2 §12). The
//! `tailings_acidify_downslope` site, on a lower swale so that two steps of
//! slope lie between the pile and the swale. A soil ridge down the slope and
//! a soil plug in the swale split the site in two halves along z. Each half
//! has a pile of crushed sulfide on the top of the slope and a pool in its
//! swale. The half's slope, between its outer soil rim and the ridge, is the
//! channel its runoff runs down.
//!
//! In the lined half, the channel's floor is limestone, [`LINING`] cells
//! deep, from under the pile to the swale. In the unlined half, it is the
//! fixture's soil.
//!
//! Limestone is porous (§5). The pile drains into the floor under it
//! (seepage goes below first, §7.3), and runoff soaks into the floor
//! downhill. In the floor, `dissolve_carbonate` makes lime and `neutralize`
//! turns acid and lime into salt (§7.6), and the floor's pore water holds
//! back acid that would otherwise reach the swale.
//!
//! After the four seasons of `tailings_acidify_downslope`:
//! - the lined swale's water has pH ≥ 6;
//! - the channel holds salt;
//! - the lined swale holds less acid than the unlined one.
//!
//! The playtester judges the shot of both swales.
//!
//! The seasons are the acidify scenario's: two dry seasons in which the damp
//! pile oxidizes, then two of light rain that wash its acid down the slope
//! (PROGRESS.md, F147).

use bevy::prelude::*;
use rmc_client::{GameCamera, LiquidMesh, MeshQueue, MeshStats};
use rmc_testkit::eco::{SLOPE_RUN, SWALE_DEPTH};
use rmc_testkit::{Fixture, FixtureSite, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::query;
use rmc_world::shape::Shape;

/// The swale's contour, in cells above the slope's low end: two steps lower
/// than `tailings_acidify_downslope`'s, so the two steps between the pile
/// and the swale hold the channel.
const CONTOUR: i32 = 3;
/// Every season's rain, in order: two dry seasons, then two of light rain.
const RAIN: [u8; 4] = [0, 0, 24, 24];
/// Every season's air temperature: spring's in `assets/seasons.ron`,
/// 10.0 °C, so nothing freezes.
const AIR_TEMP: i16 = 100;
/// The halves' boundary along z, in site offsets: the ridge and the plug
/// cover `dz = SPLIT.0..=SPLIT.1`. The lined half is `dz < SPLIT.0`, the
/// unlined half `dz > SPLIT.1`; the fixture's swale spans `dz = 2..=29`.
const SPLIT: (i32, i32) = (14, 17);
/// The last site offset along z (and x).
const LAST: i32 = 31;
/// Height of the soil walls that keep each half's runoff.
const RIM: i32 = 2;
/// The pile's lowest level in the lined half, in site offsets: `dx` and
/// `dz` ranges, on the top two steps of the slope. The unlined half's pile
/// is its mirror image in z. Each level above is one cell smaller on every
/// side.
const PILE_DX: (i32, i32) = (2, 7);
const PILE_DZ: (i32, i32) = (3, 12);
/// Levels of the pile.
const PILE_LEVELS: i32 = 2;
/// The lined half's channel floor, in site offsets along z: the half's
/// width between its outer rim and the ridge. It runs along x from under the
/// pile to the column before the swale. The unlined channel is its mirror
/// image in z.
const CHANNEL_DZ: (i32, i32) = (1, 13);
/// Depth of the limestone floor under the slope's surface. A floor two
/// cells deep filled with rain water and passed the second wet season's
/// acid on to the swale (PROGRESS.md, F148).
const LINING: i32 = 4;
/// The pool in each swale: its bottom row of cells, holding this fill.
const POOL_FILL: i32 = 64;
/// A sump under the slope's low end holds the runoff, as in
/// `season_swale_fills`: the fixture site is a closed basin.
const SUMP_STEPS: i32 = 2;
const SUMP_DEPTH: i32 = 40;
/// The lined swale's pH must stay at or above this.
const CLEAR_PH: i32 = 6;

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
fn limestone_channel_clears() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "limestone_channel_clears", run);
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

/// The site offsets `(dz0, dz1)` mirrored across the site's middle in z.
fn mirror((dz0, dz1): (i32, i32)) -> (i32, i32) {
    (LAST - dz1, LAST - dz0)
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
    salt: LayerId,
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
    /// The sum of acid over the swale's cells, water or not.
    all_acid: u64,
    /// The water cells whose own pH is below [`CLEAR_PH`].
    sour_cells: u64,
}

impl Swale {
    /// Reads the swale `region`.
    fn read(t: &TestApp, region: CellAabb, layers: &Layers) -> Self {
        let water = t
            .eco_tables()
            .role_material("water")
            .expect("a water material");
        let mut s = Swale {
            all_acid: t.sum_layer(layers.acid, region),
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
            if i32::from(t.ph(c)) < CLEAR_PH {
                s.sour_cells += 1;
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

/// One half of the site.
struct Half {
    name: &'static str,
    /// The half's part of the swale.
    swale: CellAabb,
    /// The pile's cells.
    pile: Vec<CellPos>,
    /// The channel's floor cells.
    floor: Vec<CellPos>,
    /// The row of cells over the channel's floor downhill of the pile.
    sheet: Vec<CellPos>,
}

impl Half {
    fn sum(&self, t: &TestApp, of: &[CellPos], id: LayerId) -> u64 {
        of.iter().map(|&c| u64::from(t.layer_u8(c, id))).sum()
    }
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let rule = eco.ph_rule();
    let salt = eco
        .substance_index("salt")
        .expect("substances.ron has salt");
    let layers = Layers {
        acid: rule.acid.expect("an acid role"),
        lime: rule.lime.expect("a lime role"),
        salt: LayerId::Substance(salt),
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
    let limestone = materials
        .id("limestone")
        .expect("materials.ron has limestone");
    let lime_tags = &materials.get(limestone).expect("limestone").tags;
    assert!(
        lime_tags.iter().any(|t| t == "carbonate") && lime_tags.iter().any(|t| t == "porous"),
        "limestone is a porous carbonate (§5)"
    );
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
    // The channel's floor runs from under the pile to the column before the
    // swale; downhill of the pile, it covers two steps of the slope.
    let channel_x = (x0 + PILE_DX.0, swale.min.0.x - 1);
    assert!(
        channel_x.1 - (x0 + PILE_DX.1) >= 2 * SLOPE_RUN,
        "the channel runs down two steps below the pile"
    );

    let mut edits = Vec::new();
    // The plug: the swale's cells between the halves, back to soil.
    edits.push((EditOp::Set(soil), shape(z_slice(&site, swale, SPLIT))));
    // Soil walls RIM cells high on the ground, as in
    // `tailings_acidify_downslope`: the ridge between the halves, a rim
    // along each half's outer z edge and one across the top of the slope.
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

    let mut halves = Vec::new();
    for (name, lining, flip) in [("lined", limestone, false), ("unlined", soil, true)] {
        let dz = |r: (i32, i32)| if flip { mirror(r) } else { r };
        let half_dz = dz((swale.min.0.z - z0, SPLIT.0 - 1));
        let mut half = Half {
            name,
            swale: z_slice(&site, swale, half_dz),
            pile: Vec::new(),
            floor: Vec::new(),
            sheet: Vec::new(),
        };
        // The channel: its floor of the lining, LINING cells deep under
        // the slope's surface. Its sheet is the row of air over the floor
        // downhill of the pile, where the runoff runs.
        let (dz0, dz1) = dz(CHANNEL_DZ);
        for x in channel_x.0..=channel_x.1 {
            let s = site.surface_y(x);
            let floor = CellAabb::new(
                CellPos::new(x, s - LINING + 1, z0 + dz0),
                CellPos::new(x, s, z0 + dz1),
            );
            edits.push((EditOp::Set(lining), shape(floor)));
            half.floor.extend(cells(floor));
            if x > x0 + PILE_DX.1 {
                half.sheet.extend(cells(CellAabb::new(
                    CellPos::new(x, s + 1, z0 + dz0),
                    CellPos::new(x, s + 1, z0 + dz1),
                )));
            }
        }
        // The pile, a level at a time on the slope's surface, placed damp:
        // at the tailings' field capacity.
        let pile_dz = dz(PILE_DZ);
        for k in 0..PILE_LEVELS {
            for dx in PILE_DX.0 + k..=PILE_DX.1 - k {
                let y = site.surface_y(x0 + dx) + 1 + k;
                let row = CellAabb::new(
                    CellPos::new(x0 + dx, y, z0 + pile_dz.0 + k),
                    CellPos::new(x0 + dx, y, z0 + pile_dz.1 - k),
                );
                half.pile.extend(cells(row));
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
        // The pool: the swale's bottom row of cells, part full.
        let r = half.swale;
        let bottom = CellAabb::new(r.min, CellPos::new(r.max.0.x, r.min.0.y, r.max.0.z));
        edits.push((EditOp::Set(water), shape(bottom)));
        edits.push((
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: POOL_FILL,
            },
            shape(bottom),
        ));
        halves.push(half);
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

    let (lined, unlined) = (&halves[0], &halves[1]);
    assert_eq!(
        lined.swale.max.0.z - lined.swale.min.0.z,
        unlined.swale.max.0.z - unlined.swale.min.0.z,
        "the halves' swales are the same size"
    );
    assert!(
        lined.swale.max.0.z < z0 + SPLIT.0 && unlined.swale.min.0.z > z0 + SPLIT.1,
        "each swale is in its half"
    );
    let pool_cells = u64::try_from(cells(lined.swale).count() / SWALE_DEPTH as usize).unwrap();
    for (half, lining) in [(lined, limestone), (unlined, soil)] {
        let name = half.name;
        for &c in &half.pile {
            assert_eq!(
                t.host_world().get_cell(c),
                Some(tailings),
                "{c:?} is tailings"
            );
            assert_eq!(t.layer_u8(c, LayerId::Moisture), damp, "{c:?} is damp");
        }
        for &c in &half.floor {
            assert_eq!(
                t.host_world().get_cell(c),
                Some(lining),
                "{name}: {c:?} floors the channel"
            );
        }
        for &c in &half.sheet {
            assert_eq!(
                t.host_world().get_cell(c),
                Some(MaterialId::AIR),
                "{name}: {c:?} is open over the floor"
            );
        }
        let s = Swale::read(t, half.swale, &layers);
        assert_eq!(s.water_cells, pool_cells, "the {name} swale holds its pool");
        assert_eq!((s.acid, s.lime), (0, 0), "the {name} swale starts clean");
        assert_eq!(s.ph(rule.ph_shift), 7, "the {name} swale starts neutral");
        assert_eq!(
            half.sum(t, &half.floor, layers.salt),
            0,
            "the {name} channel starts without salt"
        );
    }

    // Each pile's acid at the end of the last dry season.
    let mut after_dry = [0; 2];
    for (season, &rain) in RAIN.iter().enumerate() {
        t.set_season_params(SeasonOverride {
            rain: Some(rain),
            air_temp: Some(AIR_TEMP),
        });
        t.advance_season();
        let oxidized = t.ledger().react_delta[substance_row(layers.acid)];
        let salted = t.ledger().react_delta[substance_row(layers.salt)];
        eprintln!("season {season} (rain {rain}): acid made {oxidized}, salt made {salted}");
        for (i, half) in halves.iter().enumerate() {
            let s = Swale::read(t, half.swale, &layers);
            eprintln!(
                "  {}: pile acid {}; floor acid {} lime {} salt {}, sheet acid {}; \
                 swale {s:?} pH {}",
                half.name,
                half.sum(t, &half.pile, layers.acid),
                half.sum(t, &half.floor, layers.acid),
                half.sum(t, &half.floor, layers.lime),
                half.sum(t, &half.floor, layers.salt),
                half.sum(t, &half.sheet, layers.acid),
                s.ph(rule.ph_shift),
            );
            if rain == 0 {
                after_dry[i] = half.sum(t, &half.pile, layers.acid);
            }
        }
        if rain == 0 {
            assert!(oxidized > 0, "season {season}: the dry piles oxidize");
        }
    }
    assert_eq!(t.season_log().len(), RAIN.len());

    for (i, half) in halves.iter().enumerate() {
        let washed = half.sum(t, &half.pile, layers.acid);
        assert!(
            washed < after_dry[i],
            "{}: the rain washes acid out of the pile: {} -> {washed}",
            half.name,
            after_dry[i]
        );
    }
    let (l, u) = (
        Swale::read(t, lined.swale, &layers),
        Swale::read(t, unlined.swale, &layers),
    );
    assert!(l.water_cells > 0, "the lined swale holds water");
    assert!(u.water_cells > 0, "the unlined swale holds water");
    assert!(
        l.ph(rule.ph_shift) >= CLEAR_PH,
        "the lined swale's water has pH {} (acid {}, lime {} over {} water cells), \
         want >= {CLEAR_PH}",
        l.ph(rule.ph_shift),
        l.acid,
        l.lime,
        l.water_cells
    );
    let channel_salt =
        lined.sum(t, &lined.floor, layers.salt) + lined.sum(t, &lined.sheet, layers.salt);
    assert!(
        channel_salt > 0,
        "the limestone channel neutralized acid into salt"
    );
    assert!(
        l.all_acid < u.all_acid,
        "the lined swale holds less acid {} than the unlined one {}",
        l.all_acid,
        u.all_acid
    );

    shoot(t, lined.swale, unlined.swale, "limestone_channel_clears");
}

/// Saves a screenshot from above the slope downhill of the berm, looking up
/// the slope at both swales and channels.
fn shoot(t: &mut TestApp, lined: CellAabb, unlined: CellAabb, name: &str) {
    t.steps(3);
    settle(t);
    {
        let (min, max) = (lined.min.0, unlined.max.0);
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0 - 3.0,
            (max.y + 1) as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let eye = centre + Vec3::new(10.0, 9.0, 0.0);
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
