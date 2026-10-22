//! Tailings kill the moss downslope (SAD-P2 §12). The layout of
//! `tailings_acidify_downslope`: a pile of crushed sulfide on a slope just
//! uphill of a contour swale with a shallow pool in it, the site split in
//! two halves along z by a soil ridge and a soil plug in the swale. The
//! polluted half has the pile, the control half the same swale and pool
//! and no pile. The seasons start the same too: two dry seasons that
//! weather and oxidize the pile, then two of light rain that wash its acid
//! down into the swale.
//!
//! Moss is seeded in both swales. Moss needs a wet anchor (§7.8), and only
//! a porous anchor holds water, so each swale's uphill column is a shelf
//! of loam, placed damp, with a row of moss on it. A soil divider keeps
//! the pool off the shelf, so the shelf is not soaked with clean pool
//! water before the acid comes. The pile's runoff fills the swale over the
//! divider; the shelf trades water with it and takes in its acid (§7.6
//! transport). Where the anchor's acid passes moss's cap the moss is
//! stressed, and after `stress_death` sub-steps it dies. Two dry seasons
//! follow the wet ones so that it can. At the end the polluted swale holds
//! less moss than the control, whose moss all lives. The playtester judges
//! the shot of both swales.

use bevy::prelude::*;
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::eco::SLOPE_RUN;
use rmc_testkit::{Fixture, FixtureSite, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// The swale's contour, in cells above the slope's low end (as in
/// `tailings_acidify_downslope`).
const CONTOUR: i32 = 5;
/// Every season's rain, in order: the acidify scenario's two dry seasons
/// and two of light rain, then two dry seasons more. The acid reaches the
/// shelf in the first wet season, and moss dies only after
/// `eco.stress_death` (16) sub-steps of unmet needs: two seasons.
const RAIN: [u8; 6] = [0, 0, 24, 24, 0, 0];
/// Every season's air temperature: spring's, 10.0 °C, inside moss's
/// temperature range, so the swale never freezes over.
const AIR_TEMP: i16 = 100;
/// The halves' boundary along z, in site offsets (see
/// `tailings_acidify_downslope`).
const SPLIT: (i32, i32) = (14, 17);
/// The last site offset along z (and x).
const LAST: i32 = 31;
/// Height of the soil walls that keep each half's runoff.
const RIM: i32 = 2;
/// The pile's lowest level, in site offsets; each level above is one cell
/// smaller on every side.
const PILE_DX: (i32, i32) = (2, 7);
const PILE_DZ: (i32, i32) = (3, 12);
const PILE_LEVELS: i32 = 2;
/// The pool in each swale: the bottom row of cells downhill of the shelf,
/// holding this fill.
const POOL_FILL: i32 = 64;
/// The sump under the slope's low end.
const SUMP_STEPS: i32 = 2;
const SUMP_DEPTH: i32 = 40;
/// The moisture the loam shelf is placed with: what moss needs, with room
/// left for the runoff.
const SHELF_DAMP: i32 = 8;
/// The pH below which the polluted swale's water counts as acid.
const ACID_PH: u8 = 4;

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
fn tailings_kill_moss() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "tailings_kill_moss", run);
}

fn shape(b: CellAabb) -> Shape {
    Shape::Box {
        min: b.min,
        max: b.max,
    }
}

/// The cells of `region`, ascending (y, z, x).
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

/// The substance layers the scenario reads.
struct Layers {
    acid: LayerId,
    cu: LayerId,
}

/// One half's swale: the swale cells, its loam shelf and the moss row on
/// the shelf.
#[derive(Clone, Copy, Debug)]
struct Half {
    swale: CellAabb,
    shelf: CellAabb,
    moss: CellAabb,
}

/// What the scenario reads of one half's moss and shelf.
#[derive(Clone, Copy, Debug, Default)]
struct Reading {
    /// Moss cells in the swale (on the shelf and anywhere it spread).
    moss: usize,
    /// The shelf's top row, the moss's anchors: sums of moisture, acid
    /// and copper.
    moisture: u64,
    acid: u64,
    cu: u64,
}

impl Half {
    /// The swale and the row above it, where the moss stands.
    fn moss_region(&self) -> CellAabb {
        CellAabb::new(self.swale.min, self.moss.max.with_x(self.swale.max.0.x))
    }

    /// The anchors: the shelf's top row.
    fn anchors(&self) -> CellAabb {
        CellAabb::new(self.shelf.min.with_y(self.shelf.max.0.y), self.shelf.max)
    }

    fn read(&self, t: &TestApp, moss: MaterialId, layers: &Layers) -> Reading {
        let world = t.host_world();
        let anchors = self.anchors();
        Reading {
            moss: cells(self.moss_region())
                .filter(|&c| world.get_cell(c) == Some(moss))
                .count(),
            moisture: cells(anchors)
                .map(|c| u64::from(t.layer_u8(c, LayerId::Moisture)))
                .sum(),
            acid: t.sum_layer(layers.acid, anchors),
            cu: t.sum_layer(layers.cu, anchors),
        }
    }
}

trait WithAxis {
    fn with_x(self, x: i32) -> CellPos;
    fn with_y(self, y: i32) -> CellPos;
}

impl WithAxis for CellPos {
    fn with_x(self, x: i32) -> CellPos {
        CellPos::new(x, self.0.y, self.0.z)
    }
    fn with_y(self, y: i32) -> CellPos {
        CellPos::new(self.0.x, y, self.0.z)
    }
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let rule = eco.ph_rule();
    let cu = eco.substance_index("cu").expect("substances.ron has cu");
    let layers = Layers {
        acid: rule.acid.expect("an acid role"),
        cu: LayerId::Substance(cu),
    };
    let materials = t.host_world().materials().clone();
    let key = |k: &str| materials.id(k).unwrap_or_else(|| panic!("no {k}"));
    let (tailings, loam, moss) = (key("crushed_sulfide"), key("loam"), key("moss"));
    let damp = materials
        .get(tailings)
        .map(|d| d.field_capacity)
        .expect("crushed_sulfide is in the table");
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
    let half = |dz: (i32, i32)| {
        let s = z_slice(&site, swale, dz);
        let shelf = CellAabb::new(s.min, s.max.with_x(s.min.0.x));
        let moss = CellAabb::new(shelf.min.with_y(level + 1), shelf.max.with_y(level + 1));
        Half {
            swale: s,
            shelf,
            moss,
        }
    };
    let polluted = half((swale.min.0.z - z0, SPLIT.0 - 1));
    let control = half((SPLIT.1 + 1, swale.max.0.z - z0));
    assert_eq!(
        x0 + PILE_DX.1 + 1,
        swale.min.0.x,
        "the pile ends just uphill of the swale"
    );

    let mut edits = Vec::new();
    // The plug, the ridge and the rims, as in `tailings_acidify_downslope`.
    edits.push((EditOp::Set(soil), shape(z_slice(&site, swale, SPLIT))));
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
    // The pile, placed damp.
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
    // Each swale: the loam shelf, placed damp, and the pool in the bottom
    // row downhill of it.
    for h in [polluted, control] {
        edits.push((EditOp::Set(loam), shape(h.shelf)));
        edits.push((
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: SHELF_DAMP,
            },
            shape(h.shelf),
        ));
        let divider = CellAabb::new(
            h.swale.min.with_x(h.shelf.max.0.x + 1),
            h.swale
                .max
                .with_x(h.shelf.max.0.x + 1)
                .with_y(h.swale.min.0.y),
        );
        edits.push((EditOp::Set(soil), shape(divider)));
        let bottom = CellAabb::new(
            h.swale.min.with_x(divider.max.0.x + 1),
            h.swale.max.with_y(h.swale.min.0.y),
        );
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
    // The moss on each shelf.
    let seed: Vec<_> = [polluted, control]
        .iter()
        .map(|h| (EditOp::Set(moss), shape(h.moss)))
        .collect();
    t.apply_edits(&seed);

    let rows = cells(polluted.moss).count();
    assert_eq!(rows, cells(control.moss).count(), "the moss rows match");
    for (name, h) in [("polluted", polluted), ("control", control)] {
        for c in cells(h.moss) {
            assert_eq!(
                t.host_world().get_cell(c),
                Some(moss),
                "{name}: {c:?} is moss"
            );
            assert!(
                t.host_world().plant(c).is_some(),
                "{name}: {c:?} is a plant"
            );
        }
        let r = h.read(t, moss, &layers);
        assert_eq!(r.moss, rows, "the {name} swale starts with its moss row");
        assert_eq!((r.acid, r.cu), (0, 0), "the {name} shelf starts clean");
        assert_eq!(
            r.moisture,
            u64::try_from(rows).unwrap() * SHELF_DAMP as u64,
            "the {name} shelf is damp"
        );
    }

    let sum_pile = |t: &TestApp, id: LayerId| -> u64 {
        pile.iter().map(|&c| u64::from(t.layer_u8(c, id))).sum()
    };
    let mut died = 0;
    for (season, &rain) in RAIN.iter().enumerate() {
        t.set_season_params(SeasonOverride {
            rain: Some(rain),
            air_temp: Some(AIR_TEMP),
        });
        t.advance_season();
        died += t.ledger().plants_died;
        let (p, c) = (
            polluted.read(t, moss, &layers),
            control.read(t, moss, &layers),
        );
        eprintln!(
            "season {season} (rain {rain}): pile acid {}, cu {}; polluted {p:?}; control {c:?}; \
             died {}, born {}",
            sum_pile(t, layers.acid),
            sum_pile(t, layers.cu),
            t.ledger().plants_died,
            t.ledger().plants_born,
        );
    }
    assert_eq!(t.season_log().len(), RAIN.len());

    let (p, c) = (
        polluted.read(t, moss, &layers),
        control.read(t, moss, &layers),
    );
    assert!(died > 0, "some moss died");
    assert_eq!(c.moss, rows, "all the control swale's moss lives: {c:?}");
    assert_eq!((c.acid, c.cu), (0, 0), "the control shelf stays clean");
    assert!(
        p.moss < c.moss,
        "moss in the polluted swale {} < the control swale {} (polluted {p:?}, control {c:?})",
        p.moss,
        c.moss
    );
    // The moss that died is the moss whose anchor took in more acid than
    // moss tolerates; the rest lives.
    let cap = eco
        .plants()
        .iter()
        .find(|row| row.key == "moss")
        .and_then(|row| row.needs.substance_max.iter().find(|(x, _)| x == "acid"))
        .map(|&(_, n)| n)
        .expect("moss has an acid cap");
    for c in cells(polluted.moss) {
        let anchor = c.with_y(c.0.y - 1);
        let acid = t.layer_u8(anchor, layers.acid);
        let alive = t.host_world().get_cell(c) == Some(moss);
        assert_eq!(
            alive,
            acid <= cap,
            "{c:?}: anchor acid {acid} against moss's cap {cap}, moss alive {alive}"
        );
        assert_eq!(
            alive,
            t.host_world().plant(c).is_some(),
            "{c:?}: plant state"
        );
    }
    // The acid came down with the swale's water: the pool beside the shelf
    // is below pH 4, as in `tailings_acidify_downslope`.
    let acid_water = cells(polluted.swale)
        .filter(|&c| t.host_world().get_cell(c) == Some(water) && t.ph(c) < ACID_PH)
        .count();
    assert!(acid_water > 0, "the polluted swale's water is acid");

    shoot(t, polluted.swale, control.swale, "tailings_kill_moss");
}

/// Saves a screenshot from above the slope downhill of the berm, looking
/// up the slope at both swales and the pile: the control's shelf is green
/// with moss, the polluted one's bare.
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
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, name, t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "{name}: render failure: {:?}",
        t.render_failure()
    );
}
