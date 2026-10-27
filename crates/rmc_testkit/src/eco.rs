//! Phase 2 additions to the test kit (SAD-P2 §11): seasons, layer readers,
//! the season ledger, layer edits, terrain fixtures and season overrides.
//!
//! Everything a scenario changes goes through the host the way the game's
//! does: [`TestApp::advance_season`] raises the editor's Advance-season
//! action (as F6 or the panel's button do), and [`TestApp::set_layer`],
//! [`TestApp::apply_edits`] and [`TestApp::fixture`] send normal edit
//! commands as the local player. The readers read the host's world.
//!
//! [`SeasonLogPlugin`] records, from the client's inbox, every season the
//! host announced: the chunk and layer snapshots between `SeasonStarted`
//! and `SeasonDone`, and the report.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bevy::prelude::*;
use rmc_client::{ClientEco, ClientSeasons, CommandSeq, FromHost, SeasonView, ToHost};
use rmc_protocol::{ClientMsg, SeasonReport, ServerMsg};
use rmc_world::coords::{CellAabb, CellPos, ChunkPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, MAX_EDIT_CELLS};
use rmc_world::inventory::Inventory;
use rmc_world::layers::LayerId;
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::MaterialId;
use rmc_world::query::{self, ToxProfile};
use rmc_world::shape::Shape;

use rmc_eco::EcoTables;
pub use rmc_eco::{SeasonOverride, ToxKind};

use crate::app::TestApp;

/// How many steps [`TestApp::advance_season`] waits for `SeasonDone`. It
/// normally arrives two steps after the request.
pub const MAX_SEASON_STEPS: u32 = 10;

/// How many steps [`TestApp::apply_edits`] waits for the host's results.
/// They normally arrive two steps after the edits are queued.
pub const MAX_EDIT_STEPS: u32 = 10;

/// Side of a fixture's square footprint, in cells.
pub const FIXTURE_SIZE: i32 = 32;
/// Cells of fixture ground under the ground level, the level included.
pub const FIXTURE_BASE_DEPTH: i32 = 10;
/// Cells of soil on top of the fixture ground; stone below.
pub const FIXTURE_SOIL_DEPTH: i32 = 2;
/// A slope rises one cell per this many cells toward -x.
pub const SLOPE_RUN: i32 = 4;
/// How far a slope's top rises above the ground level.
pub const SLOPE_RISE: i32 = (FIXTURE_SIZE - 1) / SLOPE_RUN;
/// Free cells a fixture needs above the ground level.
pub const FIXTURE_HEADROOM: i32 = SLOPE_RISE + 8;
/// Depth of a [`Fixture::Pit`] or [`Fixture::Pool`] below the ground level,
/// the ground level included.
pub const PIT_DEPTH: i32 = 6;
/// Depth of a [`Fixture::Trench`] below the slope's surface.
pub const TRENCH_DEPTH: i32 = 3;
/// Depth of a [`Fixture::Swale`] below its contour.
pub const SWALE_DEPTH: i32 = 2;
/// Levels of a [`Fixture::Pile`].
pub const PILE_HEIGHT: i32 = 4;

/// A terrain fixture (SAD-P2 §11). Each one rebuilds the same square site
/// in the middle of the world ([`FixtureSite`]): everything above the
/// ground level becomes air, the [`FIXTURE_BASE_DEPTH`] cells up to it
/// become stone under [`FIXTURE_SOIL_DEPTH`] cells of soil (the first
/// materials tagged `stone` and `soil`), and then the fixture's shape is
/// cut or built. Site offsets `dx`, `dz` count from the site's min corner;
/// the middle of the site is `dx, dz = 12..=19`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fixture {
    /// Ground rising toward -x by one cell per [`SLOPE_RUN`], up to
    /// [`SLOPE_RISE`] above the ground level at `dx = 0`; soil on top.
    Slope,
    /// Flat ground with an 8 × 8 pit in the middle, [`PIT_DEPTH`] deep.
    Pit,
    /// The slope with a trench along z across it, at `dx = 14..=15`,
    /// `dz = 4..=27`, [`TRENCH_DEPTH`] deep; its ends are closed.
    Trench,
    /// The slope with a swale dug along the contour this many cells above
    /// the ground level (`1..SLOPE_RISE`): the columns whose surface is at
    /// the contour, `dz = 2..=29`, dug [`SWALE_DEPTH`] deep, with a
    /// one-cell soil berm on the next column downhill.
    Swale(i32),
    /// Flat ground with a stepped pile of the material on it in the middle,
    /// 7 × 7 at the bottom, [`PILE_HEIGHT`] levels high.
    Pile(MaterialId),
    /// Flat ground over a closed 8 × 4 × 8 air pocket in the middle whose
    /// roof is four cells thick.
    SealedCave,
    /// The slope with a channel down it, along x at `dz = 15..=16`, two
    /// cells deep, its floor and side walls lined with the material.
    Channel(MaterialId),
    /// The pit filled with water (the `water` material role).
    Pool,
    /// Flat ground whose soil in the middle 8 × 8 becomes the material,
    /// with substance layer `.1` set to `.2` in those cells.
    Patch(MaterialId, u8, u8),
}

/// Where [`TestApp::fixture`] built a fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixtureSite {
    /// Every cell the fixture rebuilt: the footprint, from the bottom of
    /// the fixture ground to the top of the world.
    pub site: CellAabb,
    /// The ground level: the top cell of flat fixture ground, and of a
    /// slope at its low end.
    pub ground_y: i32,
    /// Whether the ground is a [`Fixture::Slope`].
    pub sloped: bool,
    /// The fixture's own cells: the slope's surface band, the pit, trench,
    /// swale, pile, cave or patch, or the channel's open cells.
    pub feature: CellAabb,
}

impl FixtureSite {
    /// The top cell of the fixture ground in column `x` before the feature
    /// was cut or built.
    pub fn surface_y(&self, x: i32) -> i32 {
        if self.sloped {
            slope_surface(self.ground_y, x - self.site.min.0.x)
        } else {
            self.ground_y
        }
    }
}

fn slope_surface(ground_y: i32, dx: i32) -> i32 {
    ground_y + (FIXTURE_SIZE - 1 - dx) / SLOPE_RUN
}

/// One season as the client saw it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeasonFrames {
    pub season: u32,
    /// Chunks of the `ChunkSnapshot`s between `SeasonStarted` and
    /// `SeasonDone`, in arrival order.
    pub chunk_snapshots: Vec<ChunkPos>,
    /// Chunks of the `LayerSnapshot`s, in arrival order.
    pub layer_snapshots: Vec<ChunkPos>,
    pub report: SeasonReport,
    pub changed_chunks: u32,
}

/// What [`SeasonLogPlugin`] recorded.
#[derive(Resource, Default)]
pub(crate) struct SeasonLog {
    open: Option<SeasonFrames>,
    done: Vec<SeasonFrames>,
    /// Edits [`TestApp::apply_edits`] waits for, and their results.
    awaited: BTreeSet<CommandId>,
    results: BTreeMap<CommandId, EditStatus>,
}

/// Records seasons and awaited edit results from the client's inbox.
pub(crate) struct SeasonLogPlugin;

impl Plugin for SeasonLogPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<FromHost>()
            .init_resource::<SeasonLog>()
            .add_systems(Update, record_seasons);
    }
}

fn record_seasons(mut inbox: MessageReader<FromHost>, mut log: ResMut<SeasonLog>) {
    for FromHost(msg) in inbox.read() {
        match msg {
            ServerMsg::SeasonStarted { season } => {
                log.open = Some(SeasonFrames {
                    season: *season,
                    ..SeasonFrames::default()
                });
            }
            ServerMsg::ChunkSnapshot { pos, .. } => {
                if let Some(open) = &mut log.open {
                    open.chunk_snapshots.push(*pos);
                }
            }
            ServerMsg::LayerSnapshot { pos, .. } => {
                if let Some(open) = &mut log.open {
                    open.layer_snapshots.push(*pos);
                }
            }
            ServerMsg::SeasonDone {
                season,
                report,
                changed_chunks,
            } => {
                let mut frames = log.open.take().unwrap_or_default();
                frames.season = *season;
                frames.report = (**report).clone();
                frames.changed_chunks = *changed_chunks;
                log.done.push(frames);
            }
            ServerMsg::Applied(result) if log.awaited.remove(&result.command) => {
                log.results.insert(result.command, result.status.clone());
            }
            _ => {}
        }
    }
}

impl TestApp {
    /// Runs one season: raises the editor's Advance-season action and
    /// steps until the client has the host's `SeasonDone`, by which time
    /// the replica has applied every snapshot of the season.
    ///
    /// Panics if `SeasonDone` does not arrive within [`MAX_SEASON_STEPS`].
    pub fn advance_season(&mut self) {
        let seen = self.season_log().len();
        self.editor().season_requested = true;
        for _ in 0..MAX_SEASON_STEPS {
            self.step();
            if self.season_log().len() > seen {
                return;
            }
        }
        panic!("no SeasonDone within {MAX_SEASON_STEPS} steps");
    }

    /// Runs the host's seasons on the game `calendar` (`calendar.ron`,
    /// [`rmc_eco::Calendar`]), as the windowed game does; `None` goes back
    /// to the `seasons.ron` rows.
    pub fn set_calendar(&mut self, calendar: Option<rmc_eco::Calendar>) {
        assert!(
            self.host_mut().set_calendar(calendar),
            "the host has seasons"
        );
    }

    /// Runs one ecology sub-step on the host (an hour of the calendar, as
    /// the game's clock does) and steps until the client has its messages.
    pub fn eco_hour(&mut self) {
        self.host_mut().eco_substep();
        self.steps(2);
    }

    /// Runs `n` seasons ([`TestApp::advance_season`]).
    pub fn seasons(&mut self, n: u32) {
        for _ in 0..n {
            self.advance_season();
        }
    }

    /// Every season the client has seen end, oldest first.
    pub fn season_log(&self) -> &[SeasonFrames] {
        &self.app().world().resource::<SeasonLog>().done
    }

    /// The last season the client has seen end.
    pub fn last_season(&self) -> Option<&SeasonFrames> {
        self.season_log().last()
    }

    /// The ledger of the last season.
    ///
    /// Panics if no season has run.
    pub fn ledger(&self) -> &SeasonLedger {
        &self
            .last_season()
            .expect("no season has run; call advance_season first")
            .report
            .ledger
    }

    /// The `u8` layer `id` of `cell` on the host (0 where it is absent).
    ///
    /// Panics if `cell` is not in a resident chunk or `id` is an `i16`
    /// layer.
    pub fn layer_u8(&self, cell: CellPos, id: LayerId) -> u8 {
        assert!(!id.is_i16(), "{id:?} is an i16 layer");
        let (pos, local) = cell.split();
        self.host_chunk(pos, cell).layers.get_u8(id, local)
    }

    /// The temperature of `cell` on the host, in 0.1 °C.
    ///
    /// Panics if `cell` is not in a resident chunk.
    pub fn temp(&self, cell: CellPos) -> i16 {
        let (pos, local) = cell.split();
        self.host_chunk(pos, cell)
            .layers
            .get_i16(LayerId::Temperature, local)
    }

    fn host_chunk(&self, pos: ChunkPos, cell: CellPos) -> &rmc_world::chunk::Chunk {
        self.host_world()
            .chunk(pos)
            .unwrap_or_else(|| panic!("{cell:?} is not in a resident chunk"))
    }

    /// The derived pH of `cell` on the host (SAD-P2 §7.6), from its `acid`
    /// and `lime` substance roles. Cells outside resident chunks read
    /// neutral.
    pub fn ph(&self, cell: CellPos) -> u8 {
        let ph = self.eco_tables().ph_rule().at(self.host_world(), cell);
        u8::try_from(ph).expect("pH is clamped to 0..=14")
    }

    /// Sets layer `id` of `cell` to `value` with an `EditOp::SetLayer` from
    /// the local player, and steps until it is applied.
    pub fn set_layer(&mut self, cell: CellPos, id: LayerId, value: i32) {
        self.apply_edits(&[(EditOp::SetLayer { layer: id, value }, Shape::Cell(cell))]);
    }

    /// Sends `edits` as the local player's edit commands, in order, in one
    /// frame, and steps until the client has every result.
    ///
    /// Panics if one is rejected or the results do not arrive within
    /// [`MAX_EDIT_STEPS`].
    pub fn apply_edits(&mut self, edits: &[(EditOp, Shape)]) {
        let issuer = self.player_id();
        let world = self.app_mut().world_mut();
        let commands: Vec<EditCommand> = edits
            .iter()
            .map(|&(op, shape)| EditCommand {
                id: world.resource_mut::<CommandSeq>().next_id(issuer),
                issuer,
                op,
                shape,
            })
            .collect();
        world
            .resource_mut::<SeasonLog>()
            .awaited
            .extend(commands.iter().map(|c| c.id));
        for &cmd in &commands {
            world.write_message(ToHost(ClientMsg::Edit(cmd)));
        }
        for _ in 0..MAX_EDIT_STEPS {
            if commands.iter().all(|c| self.edit_result(c.id).is_some()) {
                break;
            }
            self.step();
        }
        let mut log = self.app_mut().world_mut().resource_mut::<SeasonLog>();
        for cmd in &commands {
            match log.results.remove(&cmd.id) {
                Some(EditStatus::Applied) => {}
                Some(status) => panic!("{cmd:?} was not applied: {status:?}"),
                None => panic!("no result for {cmd:?} within {MAX_EDIT_STEPS} steps"),
            }
        }
    }

    /// Digs the plant at `cell` with one pickaxe edit from the local player
    /// and returns what the harvest added to the player's inventory on the
    /// host (SAD-P2 §7.8): the plant's material and the yield materials of
    /// what it held.
    ///
    /// Panics if `cell` holds no plant on the host or the edit is not
    /// applied.
    pub fn harvest(&mut self, cell: CellPos) -> Inventory {
        assert!(
            self.host_world().plant(cell).is_some(),
            "{cell:?} holds no plant"
        );
        let held = |t: &Self| {
            t.host()
                .inventory(t.player_id())
                .cloned()
                .unwrap_or_default()
        };
        let before = held(self);
        self.apply_edits(&[(EditOp::Damage(u16::MAX), Shape::Cell(cell))]);
        let gained: Vec<(MaterialId, u32)> = held(self)
            .items()
            .into_iter()
            .map(|(id, n)| (id, n - before.count(id)))
            .collect();
        Inventory::from_items(&gained)
    }

    fn edit_result(&self, id: CommandId) -> Option<&EditStatus> {
        self.app().world().resource::<SeasonLog>().results.get(&id)
    }

    /// How many cells of `region` on the host hold a material that is the
    /// `tag_or_role` material role of `eco.ron`, or else carries it as a
    /// tag. A role whose material does not exist yet counts nothing.
    ///
    /// Panics if `tag_or_role` is neither a material role nor any
    /// material's tag.
    pub fn count(&self, tag_or_role: &str, region: CellAabb) -> u32 {
        let eco = self.eco_tables();
        let materials = eco.materials();
        let matches: Vec<bool> = if eco.roles().materials.contains_key(tag_or_role) {
            let role = eco.role_material(tag_or_role);
            materials.iter().map(|d| Some(d.id) == role).collect()
        } else {
            let tagged: Vec<bool> = materials.iter().map(|d| d.has_tag(tag_or_role)).collect();
            assert!(
                tagged.contains(&true),
                "{tag_or_role:?} is neither a material role nor a tag"
            );
            tagged
        };
        let world = self.host_world();
        let mut n = 0;
        for cell in region_cells(region) {
            if let Some(m) = world.get_cell(cell)
                && matches.get(usize::from(m.0)).copied().unwrap_or(false)
            {
                n += 1;
            }
        }
        n
    }

    /// The sum of the `u8` layer `id` over `region` on the host. Cells
    /// outside resident chunks count 0.
    ///
    /// Panics if `id` is an `i16` layer.
    pub fn sum_layer(&self, id: LayerId, region: CellAabb) -> u64 {
        assert!(!id.is_i16(), "{id:?} is an i16 layer");
        let world = self.host_world();
        region_cells(region)
            .filter_map(|cell| {
                let (pos, local) = cell.split();
                world
                    .chunk(pos)
                    .map(|c| u64::from(c.layers.get_u8(id, local)))
            })
            .sum()
    }

    /// The spoil cells of `region` on the host for `profile` (SAD-P2 §3,
    /// §11), sorted by `(y, z, x)`. Build a profile with
    /// `eco_tables().tox_profile(ToxKind::Plant)` or `ToxKind::Player`.
    pub fn spoil_cells(&self, region: CellAabb, profile: ToxProfile) -> Vec<CellPos> {
        query::spoil(self.host_world(), region, &profile)
    }

    /// From the next season on, every season row's `rain` and `air_temp`
    /// are the override's where it gives them, else `assets/seasons.ron`'s.
    /// Each call replaces the previous override; the default restores the
    /// tables.
    pub fn set_season_params(&mut self, o: SeasonOverride) {
        let tables = Arc::new(self.eco_tables().with_season_override(o));
        assert!(
            self.host_mut().set_eco_tables(tables),
            "the host runs seasons"
        );
    }

    /// Builds `f` on the fixture site with edits from the local player
    /// (see [`Fixture`]) and steps until they are applied.
    ///
    /// Panics if the world is narrower than [`FIXTURE_SIZE`] or too low for
    /// the fixture, or a material it needs does not exist.
    pub fn fixture(&mut self, f: Fixture) -> FixtureSite {
        let world = self.host_world();
        let bounds = world.bounds();
        let materials = world.materials();
        let tagged = |tag: &str| {
            materials
                .with_tag(tag)
                .next()
                .unwrap_or_else(|| panic!("no material tagged {tag:?}"))
                .id
        };
        let (soil, stone) = (tagged("soil"), tagged("stone"));
        let (min, max) = (bounds.min.0, bounds.max.0);
        assert!(
            max.x - min.x + 1 >= FIXTURE_SIZE && max.z - min.z + 1 >= FIXTURE_SIZE,
            "the world is narrower than a fixture site"
        );
        let x0 = (min.x + max.x + 1) / 2 - FIXTURE_SIZE / 2;
        let z0 = (min.z + max.z + 1) / 2 - FIXTURE_SIZE / 2;
        let last = FIXTURE_SIZE - 1;

        // The ground level: the generated surface in the middle of the
        // site, kept clear of the floor and the top of the world.
        let lowest = min.y + FIXTURE_BASE_DEPTH;
        let highest = max.y - FIXTURE_HEADROOM;
        assert!(lowest <= highest, "the world is too low for a fixture site");
        let middle = world.surface_y(x0 + FIXTURE_SIZE / 2, z0 + FIXTURE_SIZE / 2);
        let g = i32::from(middle).clamp(lowest, highest);
        let bottom = g - FIXTURE_BASE_DEPTH + 1;
        let at = |dx: i32, y: i32, dz: i32| CellPos::new(x0 + dx, y, z0 + dz);
        let sloped = matches!(
            f,
            Fixture::Slope | Fixture::Trench | Fixture::Swale(_) | Fixture::Channel(_)
        );
        let surface = |dx: i32| if sloped { slope_surface(g, dx) } else { g };
        let steps = ground_steps(surface);

        // Clear the site and lay the ground: stone under soil, flat at g
        // or one cell higher every SLOPE_RUN columns toward -x.
        let mut edits = box_edits(EditOp::Remove, at(0, g + 1, 0), at(last, max.y, last));
        for &(from, to, top) in &steps {
            let soil_from = top - FIXTURE_SOIL_DEPTH + 1;
            edits.extend(box_edits(
                EditOp::Set(stone),
                at(from, bottom, 0),
                at(to, soil_from - 1, last),
            ));
            edits.extend(box_edits(
                EditOp::Set(soil),
                at(from, soil_from, 0),
                at(to, top, last),
            ));
        }

        // The middle 8 × 8 columns.
        let (lo, hi) = (12, 19);
        let feature = match f {
            Fixture::Slope => CellAabb::new(at(0, g, 0), at(last, surface(0), last)),
            Fixture::Pit | Fixture::Pool => {
                let pit = CellAabb::new(at(lo, g - PIT_DEPTH + 1, lo), at(hi, g, hi));
                let fill = match f {
                    Fixture::Pool => EditOp::Set(self.role_material("water")),
                    _ => EditOp::Remove,
                };
                edits.extend(box_edits(fill, pit.min, pit.max));
                pit
            }
            Fixture::Trench => {
                let (from, to) = (14, 15);
                let top = surface(from);
                debug_assert_eq!(top, surface(to), "the trench sits on one step");
                let trench = CellAabb::new(at(from, top - TRENCH_DEPTH + 1, 4), at(to, top, 27));
                edits.extend(box_edits(EditOp::Remove, trench.min, trench.max));
                trench
            }
            Fixture::Swale(contour) => {
                assert!(
                    (1..SLOPE_RISE).contains(&contour),
                    "swale contour {contour} outside 1..{SLOPE_RISE}"
                );
                let level = g + contour;
                let &(from, to, _) = steps
                    .iter()
                    .find(|s| s.2 == level)
                    .expect("every level below the top is a step");
                let swale = CellAabb::new(at(from, level - SWALE_DEPTH + 1, 2), at(to, level, 29));
                edits.extend(box_edits(EditOp::Remove, swale.min, swale.max));
                let (berm, berm_y) = (to + 1, surface(to + 1) + 1);
                edits.extend(box_edits(
                    EditOp::Set(soil),
                    at(berm, berm_y, 2),
                    at(berm, berm_y, 29),
                ));
                swale
            }
            Fixture::Pile(m) => {
                for k in 0..PILE_HEIGHT {
                    edits.extend(box_edits(
                        EditOp::Set(m),
                        at(13 + k, g + 1 + k, 13 + k),
                        at(19 - k, g + 1 + k, 19 - k),
                    ));
                }
                CellAabb::new(at(13, g + 1, 13), at(19, g + PILE_HEIGHT, 19))
            }
            Fixture::SealedCave => {
                let roof = 4;
                let cave = CellAabb::new(at(lo, g - roof - 3, lo), at(hi, g - roof, hi));
                edits.extend(box_edits(EditOp::Remove, cave.min, cave.max));
                cave
            }
            Fixture::Channel(m) => {
                let (open, lined) = ((15, 16), (14, 17));
                for &(from, to, top) in &steps {
                    edits.extend(box_edits(
                        EditOp::Set(m),
                        at(from, top - 2, lined.0),
                        at(to, top, lined.1),
                    ));
                    edits.extend(box_edits(
                        EditOp::Remove,
                        at(from, top - 1, open.0),
                        at(to, top, open.1),
                    ));
                }
                CellAabb::new(at(0, g - 1, open.0), at(last, surface(0), open.1))
            }
            Fixture::Patch(m, substance, amount) => {
                let patch = CellAabb::new(at(lo, g - FIXTURE_SOIL_DEPTH + 1, lo), at(hi, g, hi));
                edits.extend(box_edits(EditOp::Set(m), patch.min, patch.max));
                edits.extend(box_edits(
                    EditOp::SetLayer {
                        layer: LayerId::Substance(substance),
                        value: i32::from(amount),
                    },
                    patch.min,
                    patch.max,
                ));
                patch
            }
        };
        self.apply_edits(&edits);
        FixtureSite {
            site: CellAabb::new(at(0, bottom, 0), at(last, max.y, last)),
            ground_y: g,
            sloped,
            feature,
        }
    }

    /// The material of `role` in `eco.ron`.
    ///
    /// Panics if the material table does not have it yet.
    fn role_material(&self, role: &str) -> MaterialId {
        self.eco_tables()
            .role_material(role)
            .unwrap_or_else(|| panic!("no material for the {role:?} role yet"))
    }
}

/// `Shape::Box` edits covering `min..=max` with `op`, split along y so each
/// stays within [`MAX_EDIT_CELLS`]. Nothing if the box is empty.
fn box_edits(op: EditOp, min: CellPos, max: CellPos) -> Vec<(EditOp, Shape)> {
    let size = max.0 - min.0 + IVec3::ONE;
    if size.cmple(IVec3::ZERO).any() {
        return Vec::new();
    }
    let layer = i64::from(size.x) * i64::from(size.z);
    let rows = (i64::try_from(MAX_EDIT_CELLS).unwrap_or(i64::MAX) / layer).max(1);
    let rows = i32::try_from(rows).unwrap_or(i32::MAX);
    let mut out = Vec::new();
    let mut y = min.0.y;
    while y <= max.0.y {
        let top = max.0.y.min(y.saturating_add(rows - 1));
        out.push((
            op,
            Shape::Box {
                min: CellPos::new(min.0.x, y, min.0.z),
                max: CellPos::new(max.0.x, top, max.0.z),
            },
        ));
        if top == max.0.y {
            break;
        }
        y = top + 1;
    }
    out
}

/// The site's columns `dx` grouped into runs of one surface height:
/// `(first dx, last dx, top y)`, ascending by `dx`.
fn ground_steps(surface: impl Fn(i32) -> i32) -> Vec<(i32, i32, i32)> {
    let mut steps: Vec<(i32, i32, i32)> = Vec::new();
    for dx in 0..FIXTURE_SIZE {
        let top = surface(dx);
        match steps.last_mut() {
            Some(step) if step.2 == top => step.1 = dx,
            _ => steps.push((dx, dx, top)),
        }
    }
    steps
}

/// The cells of `region`, ascending (y, z, x).
fn region_cells(region: CellAabb) -> impl Iterator<Item = CellPos> {
    Shape::Box {
        min: region.min,
        max: region.max,
    }
    .cells()
}

/// What the client reads of the eco tables (SAD-P2 §10), as the game
/// inserts it: substance and gas keys, the pH rule, the toxicity
/// profiles, the water tint, the gas tint, the flammable gases and the plant species
/// keys.
pub(crate) fn client_eco(eco: &EcoTables) -> ClientEco {
    ClientEco {
        substances: eco.substance_layers().clone(),
        gases: eco.gases().iter().map(|g| g.key.clone()).collect(),
        ph: eco.ph_rule(),
        player_tox: eco.tox_profile(ToxKind::Player),
        plant_tox: eco.tox_profile(ToxKind::Plant),
        tint: eco.substance_tints(),
        gas_tint: eco.gas_tints(),
        flammable: eco.flammable_gases(),
        plants: eco.plants().iter().map(|p| p.key.clone()).collect(),
    }
}

/// The season rows as the client reads them (`rmc_client::weather`), as
/// the game inserts them.
pub(crate) fn client_seasons(eco: &EcoTables) -> ClientSeasons {
    ClientSeasons {
        rows: eco
            .seasons()
            .seasons
            .iter()
            .map(|s| SeasonView {
                key: s.key.clone(),
                air_temp: s.air_temp,
                rain: s.rain,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::TestConfig;
    use rmc_client::Replica;

    /// 2 × 4 × 2 chunks: full height, so the fixture site sits on the
    /// generated surface with room above.
    fn logic_config() -> TestConfig {
        TestConfig {
            seed: 11,
            world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
            render: false,
            ..TestConfig::default()
        }
    }

    /// A joined test app.
    fn joined() -> TestApp {
        let mut t = TestApp::new(logic_config());
        t.step();
        assert!(t.host().is_joined(t.player_id()));
        t
    }

    fn replica_hash(t: &TestApp) -> u64 {
        t.app().world().resource::<Replica>().world().hash()
    }

    fn replica_temp(t: &TestApp, cell: CellPos) -> i16 {
        let (pos, local) = cell.split();
        t.app()
            .world()
            .resource::<Replica>()
            .world()
            .chunk(pos)
            .expect("resident in the replica")
            .layers
            .get_i16(LayerId::Temperature, local)
    }

    #[test]
    fn pool_fixture_is_full_water_through_a_season() {
        let mut t = joined();
        let s = t.fixture(Fixture::Pool);
        let size = s.feature.max.0 - s.feature.min.0 + IVec3::ONE;
        let volume = u32::try_from(size.x * size.y * size.z).unwrap();
        assert_eq!(t.count("water", s.feature), volume);
        assert_eq!(
            t.sum_layer(LayerId::Moisture, s.feature),
            u64::from(volume) * u64::from(u8::MAX),
            "brush water is a full cell"
        );
        t.advance_season();
        assert!(t.count("water", s.feature) > 0);
        assert_eq!(replica_hash(&t), t.host_world().hash());
    }

    #[test]
    fn advance_season_applies_both_snapshot_kinds() {
        let mut t = joined();
        assert_eq!(t.host().season(), Some(0), "the testkit host runs seasons");
        let corner = t.host_world().bounds().max;
        assert_eq!(replica_hash(&t), t.host_world().hash());

        // A real season. A chunk whose materials changed (its `revision`:
        // rain, and steam condensing in caves, turn air to water, SAD-P2
        // §7.3) comes as a ChunkSnapshot; one whose layers alone changed
        // (the temperature pass's) as a LayerSnapshot; the rest not at all.
        let revisions = |t: &TestApp| -> Vec<(ChunkPos, u64, u64)> {
            t.host_world()
                .iter_chunks_sorted()
                .map(|(p, c)| (p, c.revision, c.eco_revision))
                .collect()
        };
        let before = revisions(&t);
        t.advance_season();
        assert_eq!(t.host().season(), Some(1));
        let s = t.last_season().expect("a season").clone();
        assert_eq!(s.season, 0);
        let (mut want_chunk, mut want_layer) = (Vec::new(), Vec::new());
        for (&(p, rev, eco), (q, rev_after, eco_after)) in before.iter().zip(revisions(&t)) {
            assert_eq!(p, q, "the same resident chunks");
            if rev_after != rev {
                want_chunk.push(p);
            } else if eco_after != eco {
                want_layer.push(p);
            }
        }
        assert_eq!(s.chunk_snapshots, want_chunk, "material changes");
        assert_eq!(s.layer_snapshots, want_layer, "layer-only changes");
        assert!(!s.layer_snapshots.is_empty(), "temperatures changed");
        assert_eq!(
            s.changed_chunks as usize,
            s.chunk_snapshots.len() + s.layer_snapshots.len()
        );
        let mut sorted = s.layer_snapshots.clone();
        sorted.sort();
        assert_eq!(sorted, s.layer_snapshots, "ascending");
        assert_eq!(t.ledger(), &s.report.ledger);
        assert_eq!(s.report.substep_ms.len(), 8, "seasons.ron sub-steps");
        assert_eq!(
            replica_hash(&t),
            t.host_world().hash(),
            "the replica applied every LayerSnapshot"
        );
        for cell in [corner, CellPos::new(5, 20, 9), CellPos::new(40, 90, 33)] {
            assert_eq!(replica_temp(&t, cell), t.temp(cell), "{cell:?}");
        }

        // A ChunkSnapshot inside a season frame, written to the client's
        // inbox as the host would send it, so that both kinds arrive
        // whatever the season changed.
        let pos = ChunkPos::new(1, 2, 0);
        let host_chunk = t.host_world().chunk(pos).expect("resident").clone();
        let stone = t
            .host_world()
            .materials()
            .with_tag("stone")
            .next()
            .expect("a stone material")
            .id;
        let mut changed = rmc_world::chunk::Chunk::uniform(stone);
        changed.revision = host_chunk.revision + 1;
        changed.eco_revision = host_chunk.eco_revision + 1;
        changed.layers = host_chunk.layers.clone();
        let other = ChunkPos::new(0, 0, 0);
        let other_chunk = t.host_world().chunk(other).expect("resident").clone();
        let frame = [
            ServerMsg::SeasonStarted { season: 7 },
            ServerMsg::chunk_snapshot(pos, &changed),
            ServerMsg::LayerSnapshot {
                pos: other,
                eco_revision: other_chunk.eco_revision + 1,
                layers: other_chunk.layers.clone(),
                plants: other_chunk.plants.clone(),
            },
            ServerMsg::SeasonDone {
                season: 7,
                report: Box::default(),
                changed_chunks: 2,
            },
        ];
        for msg in frame {
            t.app_mut().world_mut().write_message(FromHost(msg));
        }
        t.step();
        let s = t.last_season().expect("the injected season").clone();
        assert_eq!(
            (
                s.season,
                s.chunk_snapshots,
                s.layer_snapshots,
                s.changed_chunks
            ),
            (7, vec![pos], vec![other], 2)
        );
        let replica = t.app().world().resource::<Replica>().world();
        let probe = CellPos::from_chunk_local(pos, rmc_world::coords::LocalPos::new(3, 3, 3));
        assert_eq!(
            replica.get_cell(probe),
            Some(stone),
            "ChunkSnapshot applied"
        );
        assert_eq!(
            replica.chunk(pos).map(|c| c.revision),
            Some(changed.revision)
        );
        assert_eq!(
            replica.chunk(other).map(|c| c.eco_revision),
            Some(other_chunk.eco_revision + 1),
            "LayerSnapshot applied"
        );
        assert_ne!(replica_hash(&t), t.host_world().hash());

        // The next real season brings the replica back in step for the
        // chunks it changes; a host snapshot of the rest restores it.
        for p in [pos, other] {
            let c = t.host_world().chunk(p).unwrap().clone();
            t.app_mut()
                .world_mut()
                .write_message(FromHost(ServerMsg::chunk_snapshot(p, &c)));
        }
        t.step();
        assert_eq!(replica_hash(&t), t.host_world().hash());
        t.seasons(2);
        assert_eq!(t.host().season(), Some(3));
        assert_eq!(
            t.season_log().iter().map(|s| s.season).collect::<Vec<_>>(),
            [0, 7, 1, 2]
        );
        assert_eq!(replica_hash(&t), t.host_world().hash());
    }

    #[test]
    fn fixture_pit_builds_expected_cells() {
        let mut t = joined();
        let bounds = t.host_world().bounds();
        let site = t.fixture(Fixture::Pit);
        let g = site.ground_y;

        // The site: the middle 32 × 32 columns, from the fixture ground's
        // bottom to the top of the world.
        assert_eq!(
            site.site,
            CellAabb::new(
                CellPos::new(16, g - FIXTURE_BASE_DEPTH + 1, 16),
                CellPos::new(47, bounds.max.0.y, 47)
            )
        );
        assert!(!site.sloped);
        assert!(bounds.max.0.y - g >= FIXTURE_HEADROOM, "room above");
        assert_eq!(
            site.feature,
            CellAabb::new(
                CellPos::new(28, g - PIT_DEPTH + 1, 28),
                CellPos::new(35, g, 35)
            )
        );

        // Every cell of the site, by where it lies.
        let world = t.host_world();
        let materials = world.materials();
        let soil = materials.with_tag("soil").next().unwrap().id;
        let stone = materials.with_tag("stone").next().unwrap().id;
        for cell in region_cells(site.site) {
            let want = if site.feature.contains(cell) || cell.0.y > g {
                MaterialId::AIR
            } else if cell.0.y > g - FIXTURE_SOIL_DEPTH {
                soil
            } else {
                stone
            };
            assert_eq!(world.get_cell(cell), Some(want), "{cell:?}");
        }

        // The readers agree, and the pit is open to the sky.
        assert_eq!(t.count("air", site.feature), 8 * 8 * 6);
        assert_eq!(t.count("soil", site.feature), 0);
        let floor = CellAabb::new(
            CellPos::new(28, g - PIT_DEPTH, 28),
            CellPos::new(35, g - PIT_DEPTH, 35),
        );
        assert_eq!(t.count("stone", floor), 64);
        assert_eq!(i32::from(world.surface_y(30, 30)), g - PIT_DEPTH);
        assert_eq!(i32::from(world.surface_y(20, 20)), g);

        // The edits went through the host to the replica.
        assert_eq!(replica_hash(&t), t.host_world().hash());

        // Layer edits too.
        let cell = CellPos::new(30, g - PIT_DEPTH, 30);
        t.set_layer(cell, LayerId::Moisture, 77);
        assert_eq!(t.layer_u8(cell, LayerId::Moisture), 77);
        assert_eq!(t.sum_layer(LayerId::Moisture, floor), 77);
        assert_eq!(replica_hash(&t), t.host_world().hash());
    }

    #[test]
    fn ph_reads_the_host_acid_and_lime() {
        let mut t = joined();
        let eco = t.eco_tables().clone();
        let rule = eco.ph_rule();
        let (acid, lime) = (
            rule.acid.expect("an acid role"),
            rule.lime.expect("a lime role"),
        );
        let step = 1i32 << rule.ph_shift;
        let site = t.fixture(Fixture::Pool);
        let c = site.feature.min;
        assert_eq!(t.ph(c), 7, "clean water is neutral");
        t.set_layer(c, acid, 4 * step);
        assert_eq!(t.ph(c), 3);
        t.set_layer(c, lime, step);
        assert_eq!(t.ph(c), 4);
        t.set_layer(c, acid, 0);
        assert_eq!(t.ph(c), 8);
        t.set_layer(c, lime, i32::from(u8::MAX));
        assert_eq!(t.ph(c), 14, "clamped");
        assert_eq!(
            i32::from(t.ph(c)),
            rule.at(t.host_world(), c),
            "the pH rule the client reads"
        );
    }

    #[test]
    fn spoil_cells_follow_layer_edits() {
        let mut t = joined();
        let eco = t.eco_tables().clone();
        let player = eco.tox_profile(ToxKind::Player);
        let (id, limit) = player.limits().next().expect("a player limit");
        let site = t.fixture(Fixture::Pit);
        let region = site.site;
        let before = t.spoil_cells(region, player);
        let (a, b) = (
            CellPos(site.feature.min.0 - IVec3::Y),
            CellPos(site.feature.min.0 - IVec3::Y + IVec3::X),
        );
        assert!(!before.contains(&a) && !before.contains(&b));
        t.set_layer(a, id, i32::from(limit));
        t.set_layer(b, id, i32::from(limit) - 1);
        let after = t.spoil_cells(region, player);
        assert!(after.contains(&a) && !after.contains(&b));
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(t.spoil_cells(region, ToxProfile::default()), vec![]);

        // The client got the same tables the host runs.
        let client = t.app().world().resource::<ClientEco>();
        assert_eq!(client, &client_eco(&eco));
        assert_eq!(client.player_tox, player);
    }

    #[test]
    fn other_fixtures_build_their_features() {
        let mut t = joined();
        let materials = t.host_world().materials().clone();
        let stone = materials.with_tag("stone").next().unwrap().id;
        let ore = materials.with_tag("ore").next().unwrap().id;
        let air = |t: &TestApp, b: CellAabb| t.count("air", b);
        let volume = |b: CellAabb| {
            let s = b.max.0 - b.min.0 + IVec3::ONE;
            u32::try_from(s.x * s.y * s.z).unwrap()
        };

        // Slope: one cell higher every SLOPE_RUN columns toward -x.
        let s = t.fixture(Fixture::Slope);
        assert!(s.sloped);
        let (x0, z) = (s.site.min.0.x, s.site.min.0.z + 5);
        for dx in 0..FIXTURE_SIZE {
            let x = x0 + dx;
            let top = s.surface_y(x);
            assert_eq!(top, s.ground_y + (FIXTURE_SIZE - 1 - dx) / SLOPE_RUN);
            assert_eq!(i32::from(t.host_world().surface_y(x, z)), top, "dx {dx}");
        }
        assert_eq!(s.surface_y(x0) - s.ground_y, SLOPE_RISE);

        // Trench, swale, cave: open cells, closed around.
        let s = t.fixture(Fixture::Trench);
        assert_eq!(air(&t, s.feature), volume(s.feature));
        assert_eq!(s.feature.max.0.y - s.feature.min.0.y + 1, TRENCH_DEPTH);
        let s = t.fixture(Fixture::Swale(3));
        assert_eq!(air(&t, s.feature), volume(s.feature));
        assert_eq!(s.feature.max.0.y, s.ground_y + 3);
        let berm = CellPos::new(s.feature.max.0.x + 1, s.ground_y + 3, s.feature.min.0.z);
        assert!(t.host_world().is_solid(berm), "berm on the downhill side");
        let s = t.fixture(Fixture::SealedCave);
        assert_eq!(air(&t, s.feature), volume(s.feature));
        let above = CellAabb::new(
            CellPos::new(s.feature.min.0.x, s.feature.max.0.y + 1, s.feature.min.0.z),
            CellPos::new(s.feature.max.0.x, s.ground_y, s.feature.max.0.z),
        );
        assert_eq!(air(&t, above), 0, "sealed");

        // Pile and channel of a material; a patch with a substance.
        let s = t.fixture(Fixture::Pile(ore));
        assert_eq!(t.count("ore", s.feature), 49 + 25 + 9 + 1);
        let s = t.fixture(Fixture::Channel(stone));
        let open = CellPos::new(s.site.min.0.x + 20, 0, s.feature.min.0.z);
        let top = s.surface_y(open.0.x);
        for y in [top, top - 1] {
            assert!(!t.host_world().is_solid(CellPos::new(open.0.x, y, open.0.z)));
        }
        let floor = CellPos::new(open.0.x, top - 2, open.0.z);
        assert_eq!(t.host_world().get_cell(floor), Some(stone), "lined floor");
        let s = t.fixture(Fixture::Patch(ore, 1, 50));
        assert_eq!(t.count("ore", s.feature), volume(s.feature));
        assert_eq!(
            t.sum_layer(LayerId::Substance(1), s.feature),
            u64::from(volume(s.feature)) * 50
        );
        assert_eq!(replica_hash(&t), t.host_world().hash());
    }

    #[test]
    fn harvest_yields_copper_dust_and_ledgers() {
        let mut t = joined();
        let eco = t.eco_tables().clone();
        let materials = eco.materials();
        let key = |k: &str| materials.id(k).unwrap();
        let cu = eco.substance_index("cu").unwrap();
        let x = usize::from(cu);
        let soil = materials.with_tag("soil").next().unwrap().id;
        let s = t.fixture(Fixture::Patch(soil, cu, 120));

        // A fireweed planted with the brush on the copper patch, and one on
        // clean ground outside it.
        let top = s.feature.max.0.y;
        let (mid_x, mid_z) = (s.feature.min.0.x + 3, s.feature.min.0.z + 3);
        let on_copper = CellPos::new(mid_x, top + 1, mid_z);
        let clean = CellPos::new(s.site.min.0.x + 2, top + 1, s.site.min.0.z + 2);
        let fireweed = key("fireweed");
        for c in [on_copper, clean] {
            t.apply_edits(&[(EditOp::Set(fireweed), Shape::Cell(c))]);
            let p = t
                .host_world()
                .plant(c)
                .expect("the brush plants a fireweed");
            assert_eq!(
                usize::from(p.species),
                eco.plants()
                    .iter()
                    .position(|p| p.key == "fireweed")
                    .unwrap()
            );
        }
        let anchor = CellPos::new(on_copper.0.x, top, on_copper.0.z);
        let anchor_cu = t.layer_u8(anchor, LayerId::Substance(cu));
        assert_eq!(anchor_cu, 120);

        // A season of uptake: the fireweed holds copper from its anchor.
        t.advance_season();
        let held = t.host_world().plant(on_copper).expect("it lives").held[x];
        assert!(held > 0, "the fireweed took up copper");
        assert_eq!(t.host_world().plant(clean).map(|p| p.held[x]), Some(0));

        // Harvest: the plant and max(1, held >> harvest_shift) copper_dust
        // come to the inventory, and the held copper leaves the world
        // through the between-seasons ledger's harvest_out.
        let before_out = t.host().between_seasons().unwrap().harvest_out[x];
        let total_before = t.sum_layer(LayerId::Substance(cu), s.site);
        let got = t.harvest(on_copper);
        let dust = (u32::from(held) >> eco.params().harvest_shift).max(1);
        assert_eq!(
            got,
            Inventory::from_items(&[(fireweed, 1), (key("copper_dust"), dust)])
        );
        assert_eq!(t.host_world().get_cell(on_copper), Some(MaterialId::AIR));
        assert!(t.host_world().plant(on_copper).is_none());
        let ledger = t.host().between_seasons().unwrap();
        assert_eq!(ledger.harvest_out[x] - before_out, i64::from(held));
        assert_eq!(
            t.sum_layer(LayerId::Substance(cu), s.site),
            total_before,
            "nothing held goes back into the ground"
        );
        // A fireweed that held nothing yields only itself.
        assert_eq!(t.harvest(clean), Inventory::from_items(&[(fireweed, 1)]));
        t.step();
        assert_eq!(replica_hash(&t), t.host_world().hash());
        // The next season's report carries the harvest.
        t.advance_season();
        let report = &t.last_season().unwrap().report;
        assert_eq!(report.between_seasons.harvest_out[x], i64::from(held));
    }

    #[test]
    fn set_season_params_overrides_rain() {
        let mut t = joined();
        let base = t.eco_tables().clone();
        let rows = |t: &TestApp| -> Vec<(i16, u8)> {
            t.host()
                .eco_tables()
                .expect("Phase 2 tables")
                .seasons()
                .seasons
                .iter()
                .map(|s| (s.air_temp, s.rain))
                .collect()
        };
        let base_rows: Vec<(i16, u8)> = base
            .seasons()
            .seasons
            .iter()
            .map(|s| (s.air_temp, s.rain))
            .collect();
        assert_eq!(rows(&t), base_rows);
        assert!(base_rows.iter().all(|&(_, rain)| rain > 0));

        // Rain only.
        t.set_season_params(SeasonOverride {
            rain: Some(0),
            air_temp: None,
        });
        assert_eq!(
            rows(&t),
            base_rows.iter().map(|&(a, _)| (a, 0)).collect::<Vec<_>>()
        );
        assert_eq!(**t.eco_tables(), *base, "the base tables stay");

        // A new override replaces the last; air_temp reaches the tick: the
        // sky-open air at the top of the world takes it.
        t.set_season_params(SeasonOverride {
            rain: Some(255),
            air_temp: Some(-300),
        });
        assert!(rows(&t).iter().all(|&r| r == (-300, 255)));
        // The middle of the top layer: open air (the edge columns are bedrock).
        let top = t.host_world().bounds().max.0.y;
        let sky = CellPos::new(32, top, 32);
        assert_eq!(t.host_world().get_cell(sky), Some(MaterialId::AIR));
        t.advance_season();
        assert_eq!(t.temp(sky), -300);
        assert_eq!(replica_temp(&t, sky), -300);

        // The default restores the tables: season 1 runs its own row.
        t.set_season_params(SeasonOverride::default());
        assert_eq!(rows(&t), base_rows);
        assert_eq!(t.host().eco_tables().unwrap().hash(), base.hash());
        t.advance_season();
        assert_eq!(t.temp(sky), base_rows[1].0);
    }
}
