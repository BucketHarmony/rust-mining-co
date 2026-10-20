//! Activity (SAD-P2 §7.9): skipping chunks where a Jacobi pass has gone
//! quiet.
//!
//! Sweeps are bounded by their active sets (§3, [`run_sweep`]). Jacobi
//! passes use per-chunk `quiet` counters, one per [`JacobiKind`]:
//!
//! - a pass that changes nothing in a chunk increments that chunk's counter
//!   for the pass;
//! - a change resets every counter of the chunk and of its 6 neighbors;
//! - a chunk whose counter reached `eco.quiet_skip` is not computed;
//! - a season boundary ([`Activity::season_boundary`]) resets the
//!   temperature counters of chunks that contain surface or sky-open cells;
//! - an edit resets the touched chunk and its neighbors.
//!
//! A Jacobi pass is a pure function of the state in reach, so a chunk whose
//! output changed nothing keeps producing nothing until the state in its
//! reach changes. [`Activity`] notices every change by comparing each
//! chunk's `revision` and `eco_revision` with the values it last saw, so
//! changes made by sweeps, edits and inserted chunks reset counters too.
//! A material change also resets every chunk of its chunk column, since
//! `surface_y` (which a pass may read for any cell of the column) can move
//! through chunks that did not change. Whatever else a pass reads (the
//! season's `air_temp`) is the caller's to signal.
//!
//! [`run_sweep`]: crate::runner::run_sweep

use std::collections::{BTreeMap, BTreeSet};

use rmc_world::coords::{CHUNK_SIZE, ChunkPos};
use rmc_world::ledger::SeasonLedger;
use rmc_world::world::World;

use crate::runner::{Exec, JacobiPass, JacobiReport, run_jacobi_on};

/// Number of Jacobi passes (SAD-P2 §3).
pub const N_JACOBI: usize = 5;

/// The Jacobi passes, in pass order (SAD-P2 §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JacobiKind {
    Temperature,
    Transitions,
    Weathering,
    Reactions,
    PlantGrowth,
}

impl JacobiKind {
    pub const ALL: [JacobiKind; N_JACOBI] = [
        JacobiKind::Temperature,
        JacobiKind::Transitions,
        JacobiKind::Weathering,
        JacobiKind::Reactions,
        JacobiKind::PlantGrowth,
    ];

    /// Index into a chunk's `quiet` counters.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// What one tracked Jacobi run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityReport {
    /// The underlying run over the computed chunks.
    pub jacobi: JacobiReport,
    /// Chunks computed, in sorted order.
    pub computed: Vec<ChunkPos>,
    /// Chunks skipped as quiet, in sorted order.
    pub skipped: Vec<ChunkPos>,
}

/// One chunk's activity state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ChunkActivity {
    quiet: [u8; N_JACOBI],
    /// `(revision, eco_revision)` when last seen.
    seen: (u64, u64),
}

/// Per-chunk quiet counters of the Jacobi passes (host only).
#[derive(Clone, Debug)]
pub struct Activity {
    quiet_skip: u8,
    chunks: BTreeMap<ChunkPos, ChunkActivity>,
}

impl Activity {
    /// Tracker that skips a chunk once a pass has changed nothing in it
    /// `quiet_skip` times in a row (`eco.quiet_skip`, at least 1).
    pub fn new(quiet_skip: u8) -> Self {
        assert!(quiet_skip > 0, "quiet_skip must be at least 1");
        Self {
            quiet_skip,
            chunks: BTreeMap::new(),
        }
    }

    pub fn quiet_skip(&self) -> u8 {
        self.quiet_skip
    }

    /// The `kind` counter of the chunk at `pos`; 0 for an untracked chunk.
    pub fn quiet(&self, pos: ChunkPos, kind: JacobiKind) -> u8 {
        self.chunks.get(&pos).map_or(0, |a| a.quiet[kind.index()])
    }

    /// Whether `kind` skips the chunk at `pos` on its next run.
    pub fn is_skipped(&self, pos: ChunkPos, kind: JacobiKind) -> bool {
        self.quiet(pos, kind) >= self.quiet_skip
    }

    /// Resets every counter of the chunk at `pos` and of its 6 neighbors
    /// (an edit there).
    pub fn touch(&mut self, pos: ChunkPos) {
        for p in std::iter::once(pos)
            .chain(NEIGHBORS.map(|[x, y, z]| ChunkPos::new(pos.0.x + x, pos.0.y + y, pos.0.z + z)))
        {
            if let Some(a) = self.chunks.get_mut(&p) {
                a.quiet = [0; N_JACOBI];
            }
        }
    }

    /// Resets the counters of every chunk that changed since the last call
    /// and of its neighbors (and, for a material change, of its chunk
    /// column); tracks newly resident chunks and drops the rest.
    pub fn sync(&mut self, world: &World) {
        self.chunks.retain(|&pos, _| world.chunk(pos).is_some());
        let mut changed = Vec::new();
        let mut columns = BTreeSet::new();
        for (pos, chunk) in world.iter_chunks_sorted() {
            let now = (chunk.revision, chunk.eco_revision);
            let a = self.chunks.entry(pos).or_insert_with(|| {
                changed.push(pos);
                ChunkActivity {
                    quiet: [0; N_JACOBI],
                    seen: now,
                }
            });
            if a.seen != now {
                if a.seen.0 != now.0 {
                    columns.insert((pos.0.x, pos.0.z));
                }
                a.seen = now;
                changed.push(pos);
            }
        }
        for pos in changed {
            self.touch(pos);
        }
        for a in self
            .chunks
            .iter_mut()
            .filter(|(p, _)| columns.contains(&(p.0.x, p.0.z)))
            .map(|(_, a)| a)
        {
            a.quiet = [0; N_JACOBI];
        }
    }

    /// A new season (new `air_temp`): resets the temperature counter of
    /// every chunk that contains a surface or sky-open cell.
    pub fn season_boundary(&mut self, world: &World) {
        self.sync(world);
        let k = JacobiKind::Temperature.index();
        for (&pos, a) in &mut self.chunks {
            if has_open_cell(world, pos) {
                a.quiet[k] = 0;
            }
        }
    }

    /// Runs `pass` as `kind` over the resident chunks it does not skip,
    /// then updates the counters.
    pub fn run(
        &mut self,
        world: &mut World,
        kind: JacobiKind,
        pass: &impl JacobiPass,
        exec: Exec,
        ledger: &mut SeasonLedger,
    ) -> ActivityReport {
        self.sync(world);
        let k = kind.index();
        let (computed, skipped): (Vec<ChunkPos>, Vec<ChunkPos>) = self
            .chunks
            .keys()
            .partition(|&&pos| self.quiet(pos, kind) < self.quiet_skip);
        let jacobi = run_jacobi_on(world, pass, &computed, exec, ledger);
        for pos in &computed {
            let a = self.chunks.get_mut(pos).expect("computed chunk is tracked");
            a.quiet[k] = a.quiet[k].saturating_add(1);
        }
        // A ledger-only output changes no revision; reset it here.
        for &pos in &jacobi.changed {
            self.touch(pos);
        }
        self.sync(world);
        tracing::debug!(
            ?kind,
            computed = computed.len(),
            skipped = skipped.len(),
            "tracked jacobi pass"
        );
        ActivityReport {
            jacobi,
            computed,
            skipped,
        }
    }
}

/// The 6 face neighbors of a chunk.
const NEIGHBORS: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 0, -1],
    [-1, 0, 0],
    [1, 0, 0],
    [0, 0, 1],
    [0, 1, 0],
];

/// Whether the chunk at `pos` holds an in-bounds cell at or above its
/// column's `surface_y` (a surface or sky-open cell).
fn has_open_cell(world: &World, pos: ChunkPos) -> bool {
    let (b, o) = (world.bounds(), pos.origin_cell().0);
    let span = |o: i32, min: i32, max: i32| o.max(min)..=(o + CHUNK_SIZE - 1).min(max);
    let (xs, zs) = (
        span(o.x, b.min.0.x, b.max.0.x),
        span(o.z, b.min.0.z, b.max.0.z),
    );
    let top = *span(o.y, b.min.0.y, b.max.0.y).end();
    if top < o.y.max(b.min.0.y) {
        return false;
    }
    zs.into_iter()
        .any(|z| xs.clone().any(|x| i32::from(world.surface_y(x, z)) <= top))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, CellPos, LocalPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::layers::LayerId;
    use rmc_world::materials::MaterialId;
    use rmc_world::shape::Shape;

    use crate::EcoTables;
    use crate::reference::global_order;
    use crate::runner::{ChunkView, JacobiOut, run_jacobi};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    struct Mats {
        air: MaterialId,
        stone: MaterialId,
    }

    /// Air everywhere except a stone floor at `y < floor`.
    fn world(bounds: CellAabb, floor: i32) -> (World, Mats) {
        let t = tables();
        let m = t.materials().clone();
        let mats = Mats {
            air: m.id("air").unwrap(),
            stone: m.id("stone").unwrap(),
        };
        let mut w = World::new(Arc::clone(&m), bounds);
        w.set_eco_rules(t.displacement_rules());
        for pos in bounds.chunks() {
            let mut c = Chunk::uniform(mats.air);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                if CellPos::from_chunk_local(pos, l).0.y < floor {
                    c.storage.set(l, mats.stone);
                }
            }
            w.insert_chunk(pos, c).unwrap();
        }
        (w, mats)
    }

    /// 3 × 2 × 2 chunks: x in -1..=1, y in -1..=0, z in 0..=1.
    fn bounds_12() -> CellAabb {
        CellAabb::new(CellPos::new(-32, -32, 0), CellPos::new(63, 31, 63))
    }

    /// 3 × 3 × 3 chunks from the origin.
    fn bounds_27() -> CellAabb {
        CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(95, 95, 95))
    }

    /// A Jacobi pass from a closure that records which chunks it computed.
    struct Recording<F> {
        f: F,
        computed: Mutex<BTreeSet<ChunkPos>>,
    }

    impl<F: Fn(&ChunkView<'_>) -> JacobiOut + Sync> Recording<F> {
        fn new(f: F) -> Self {
            Self {
                f,
                computed: Mutex::new(BTreeSet::new()),
            }
        }

        fn take(&self) -> BTreeSet<ChunkPos> {
            std::mem::take(&mut *self.computed.lock().unwrap())
        }
    }

    impl<F: Fn(&ChunkView<'_>) -> JacobiOut + Sync> JacobiPass for Recording<F> {
        fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
            self.computed.lock().unwrap().insert(view.pos());
            (self.f)(view)
        }
    }

    fn noop() -> Recording<impl Fn(&ChunkView<'_>) -> JacobiOut + Sync> {
        Recording::new(|_: &ChunkView<'_>| JacobiOut::default())
    }

    fn set(chunks: &[(i32, i32, i32)]) -> BTreeSet<ChunkPos> {
        chunks
            .iter()
            .map(|&(x, y, z)| ChunkPos::new(x, y, z))
            .collect()
    }

    fn all(w: &World) -> BTreeSet<ChunkPos> {
        w.iter_chunks_sorted().map(|(p, _)| p).collect()
    }

    /// Runs `pass` until every chunk is skipped for `kind`.
    fn settle(
        act: &mut Activity,
        w: &mut World,
        kind: JacobiKind,
        pass: &Recording<impl Fn(&ChunkView<'_>) -> JacobiOut + Sync>,
    ) {
        for _ in 0..act.quiet_skip() {
            act.run(w, kind, pass, Exec::Parallel, &mut SeasonLedger::default());
        }
        pass.take();
        let r = act.run(w, kind, pass, Exec::Parallel, &mut SeasonLedger::default());
        assert!(r.computed.is_empty(), "settled: {:?}", r.computed);
        assert!(pass.take().is_empty());
    }

    fn edit(w: &mut World, seq: u32, op: EditOp, cell: CellPos) {
        let issuer = PlayerId(1);
        let r = w.apply(&EditCommand {
            id: CommandId { issuer, seq },
            issuer,
            op,
            shape: Shape::Cell(cell),
        });
        assert_eq!(r.status, EditStatus::Applied);
        assert!(!r.deltas.is_empty(), "the edit changed something");
    }

    #[test]
    fn quiet_chunk_skipped_after_threshold() {
        let skip = tables().params().quiet_skip;
        assert_eq!(skip, 2, "the expectations below assume eco.ron's value");
        let (mut w, _) = world(bounds_12(), 20);
        let every = all(&w);
        // The busy chunk raises one regolith value by 1 per run up to 3:
        // it changes in runs 0, 1 and 2.
        let busy = ChunkPos::new(0, 0, 0);
        let near = set(&[(0, 0, 0), (-1, 0, 0), (1, 0, 0), (0, -1, 0), (0, 0, 1)]);
        let far = ChunkPos::new(1, -1, 1);
        let pass = Recording::new(move |v: &ChunkView<'_>| {
            let mut out = JacobiOut::default();
            let cell = v.cell(LocalPos::from_index(0));
            if v.pos() == busy && v.layer(cell, LayerId::Regolith) < 3 {
                out.layers.push((0, LayerId::Regolith, 1));
            }
            out
        });
        let kind = JacobiKind::Weathering;
        let mut act = Activity::new(skip);
        let expect: [&BTreeSet<ChunkPos>; 6] =
            [&every, &every, &near, &near, &near, &BTreeSet::new()];
        for (run, want) in expect.into_iter().enumerate() {
            let r = act.run(
                &mut w,
                kind,
                &pass,
                Exec::Parallel,
                &mut SeasonLedger::default(),
            );
            assert_eq!(&pass.take(), want, "computed in run {run}");
            let computed: BTreeSet<ChunkPos> = r.computed.iter().copied().collect();
            assert_eq!(&computed, want, "reported in run {run}");
            let skipped: BTreeSet<ChunkPos> = r.skipped.iter().copied().collect();
            assert_eq!(skipped, &every - want, "skipped in run {run}");
            let changed = if run < 3 { vec![busy] } else { vec![] };
            assert_eq!(r.jacobi.changed, changed, "changed in run {run}");
            if run >= 1 {
                assert_eq!(act.quiet(far, kind), skip, "far chunk after run {run}");
                assert!(act.is_skipped(far, kind));
            }
        }
        assert_eq!(act.quiet(busy, kind), skip);
        assert_eq!(w.layer(busy.origin_cell(), LayerId::Regolith), 3);

        // The counters are per pass: another pass still computes every chunk.
        let other = noop();
        let r = act.run(
            &mut w,
            JacobiKind::Reactions,
            &other,
            Exec::Serial,
            &mut SeasonLedger::default(),
        );
        assert_eq!(other.take(), every);
        assert!(r.skipped.is_empty());
        assert_eq!(act.quiet(far, JacobiKind::Reactions), 1);
        assert_eq!(act.quiet(far, kind), skip);
    }

    #[test]
    fn edit_reactivates_neighbors() {
        let (mut w, m) = world(bounds_27(), 48);
        let kind = JacobiKind::Transitions;
        let pass = noop();
        let mut act = Activity::new(tables().params().quiet_skip);
        settle(&mut act, &mut w, kind, &pass);

        // A layer edit in the center chunk: it and its 6 face neighbors run.
        edit(
            &mut w,
            1,
            EditOp::SetLayer {
                layer: LayerId::Temperature,
                value: 50,
            },
            CellPos::new(40, 40, 40),
        );
        let r = act.run(
            &mut w,
            kind,
            &pass,
            Exec::Parallel,
            &mut SeasonLedger::default(),
        );
        let star = set(&[
            (1, 1, 1),
            (1, 0, 1),
            (1, 2, 1),
            (0, 1, 1),
            (2, 1, 1),
            (1, 1, 0),
            (1, 1, 2),
        ]);
        assert_eq!(pass.take(), star);
        assert_eq!(r.skipped.len(), 27 - 7);
        assert_eq!(act.quiet(ChunkPos::new(1, 1, 1), kind), 1);
        assert!(act.is_skipped(ChunkPos::new(0, 0, 0), kind));
        settle(&mut act, &mut w, kind, &pass);

        // A material edit at the top of chunk (1,2,1) moves `surface_y`,
        // which reaches through the whole chunk column: its neighbors and
        // its column run.
        edit(&mut w, 2, EditOp::Set(m.stone), CellPos::new(40, 90, 40));
        act.run(
            &mut w,
            kind,
            &pass,
            Exec::Parallel,
            &mut SeasonLedger::default(),
        );
        let reached = set(&[
            (1, 2, 1),
            (0, 2, 1),
            (2, 2, 1),
            (1, 2, 0),
            (1, 2, 2),
            (1, 1, 1),
            (1, 0, 1),
        ]);
        assert_eq!(pass.take(), reached);
        settle(&mut act, &mut w, kind, &pass);

        // An edit the host reports directly: the chunk and its neighbors.
        act.touch(ChunkPos::new(0, 0, 0));
        for k in JacobiKind::ALL {
            assert_eq!(act.quiet(ChunkPos::new(0, 1, 0), k), 0);
        }
        act.run(
            &mut w,
            kind,
            &pass,
            Exec::Parallel,
            &mut SeasonLedger::default(),
        );
        assert_eq!(
            pass.take(),
            set(&[(0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1)])
        );
    }

    #[test]
    fn season_boundary_reactivates_surface_chunks() {
        // Stone below y = 20: the y = -1 chunks hold neither surface nor
        // sky-open cells, except (1,-1,1), where a shaft lowers one
        // column's surface to y = -11.
        let (mut w, m) = world(bounds_12(), 20);
        let mut setup = SeasonLedger::default();
        for y in -10..20 {
            w.eco_set(CellPos::new(40, y, 40), m.air, &mut setup);
        }
        assert_eq!(w.surface_y(40, 40), -11);
        let every = all(&w);
        let open = set(&[
            (-1, 0, 0),
            (0, 0, 0),
            (1, 0, 0),
            (-1, 0, 1),
            (0, 0, 1),
            (1, 0, 1),
            (1, -1, 1),
        ]);
        let temperature = noop();
        let transitions = noop();
        let mut act = Activity::new(tables().params().quiet_skip);
        settle(&mut act, &mut w, JacobiKind::Temperature, &temperature);
        settle(&mut act, &mut w, JacobiKind::Transitions, &transitions);

        act.season_boundary(&w);
        for &pos in &every {
            let want = if open.contains(&pos) {
                0
            } else {
                act.quiet_skip()
            };
            assert_eq!(act.quiet(pos, JacobiKind::Temperature), want, "{pos:?}");
            assert!(act.is_skipped(pos, JacobiKind::Transitions), "{pos:?}");
        }
        let r = act.run(
            &mut w,
            JacobiKind::Temperature,
            &temperature,
            Exec::Parallel,
            &mut SeasonLedger::default(),
        );
        assert_eq!(temperature.take(), open);
        assert_eq!(r.skipped.len(), every.len() - open.len());
        let r = act.run(
            &mut w,
            JacobiKind::Transitions,
            &transitions,
            Exec::Parallel,
            &mut SeasonLedger::default(),
        );
        assert!(r.computed.is_empty(), "other passes stay quiet");
    }

    /// Temperature: pair-symmetric diffusion across faces, and every
    /// sky-open cell clamped to `air_temp`.
    fn temperature_pass(air_temp: i32) -> impl JacobiPass {
        Recording::new(move |v: &ChunkView<'_>| {
            const SIX: [[i32; 3]; 6] = [
                [0, -1, 0],
                [0, 0, -1],
                [-1, 0, 0],
                [1, 0, 0],
                [0, 0, 1],
                [0, 1, 0],
            ];
            let mut out = JacobiOut::default();
            for (i, cell) in v.cells() {
                let c = cell.0;
                let t = v.layer(cell, LayerId::Temperature);
                let d = if c.y > i32::from(v.surface_y(c.x, c.z)) {
                    air_temp - t
                } else {
                    SIX.iter()
                        .map(|o| CellPos::new(c.x + o[0], c.y + o[1], c.z + o[2]))
                        .filter(|&n| v.material(n).is_some())
                        .map(|n| (v.layer(n, LayerId::Temperature) - t) / 8)
                        .sum()
                };
                if d != 0 {
                    out.layers.push((i, LayerId::Temperature, d));
                }
            }
            out
        })
    }

    /// Transitions: stone hotter than 1000 melts out to air.
    fn transition_pass(stone: MaterialId, air: MaterialId) -> impl JacobiPass {
        Recording::new(move |v: &ChunkView<'_>| {
            let mut out = JacobiOut::default();
            for (i, cell) in v.cells() {
                if v.material(cell) == Some(stone) && v.layer(cell, LayerId::Temperature) > 1000 {
                    out.materials.push((i, air));
                }
            }
            out
        })
    }

    #[test]
    fn skipping_changes_no_result_on_constant_fixture() {
        // Constant air_temp, no rain, no plants: only the Jacobi passes run.
        let t = tables();
        let air_temp = i32::from(t.seasons().seasons[0].air_temp);
        let substeps = usize::from(t.seasons().substeps);
        let (mut plain, m) = world(bounds_12(), 20);
        // The world starts at air_temp, so the clamp holds everywhere.
        for cell in global_order(bounds_12()) {
            plain.eco_put_layer(cell, LayerId::Temperature, air_temp);
        }
        // Hot 3×3×3 blocks with some moisture that melting releases: one
        // across a chunk face, one whose heat front reaches the face at
        // x = 0 only after the chunk beyond it has gone quiet.
        for (center, hot) in [
            (CellPos::new(-5, -16, 16), 30000),
            (CellPos::new(31, 10, 40), 2000),
        ] {
            for cell in global_order(CellAabb::new(
                CellPos::new(center.0.x - 1, center.0.y - 1, center.0.z - 1),
                CellPos::new(center.0.x + 1, center.0.y + 1, center.0.z + 1),
            )) {
                plain.eco_put_layer(cell, LayerId::Temperature, hot);
                plain.eco_put_layer(cell, LayerId::Moisture, 40);
            }
        }
        let start = plain.hash();
        let mut skipping = plain.clone();
        let temperature = temperature_pass(air_temp);
        let transitions = transition_pass(m.stone, m.air);
        let mut act = Activity::new(t.params().quiet_skip);
        let (mut lp, mut ls) = (SeasonLedger::default(), SeasonLedger::default());
        let mut skipped = 0;
        for season in 0..3 {
            act.season_boundary(&skipping);
            for substep in 0..substeps {
                run_jacobi(&mut plain, &temperature, Exec::Parallel, &mut lp);
                run_jacobi(&mut plain, &transitions, Exec::Parallel, &mut lp);
                let r = act.run(
                    &mut skipping,
                    JacobiKind::Temperature,
                    &temperature,
                    Exec::Parallel,
                    &mut ls,
                );
                skipped += r.skipped.len();
                let r = act.run(
                    &mut skipping,
                    JacobiKind::Transitions,
                    &transitions,
                    Exec::Parallel,
                    &mut ls,
                );
                skipped += r.skipped.len();
                assert_eq!(
                    plain.hash(),
                    skipping.hash(),
                    "season {season} substep {substep}"
                );
                assert_eq!(lp, ls, "ledger, season {season} substep {substep}");
            }
        }
        assert_ne!(plain.hash(), start, "the passes did something");
        assert!(
            plain.iter_chunks_sorted().any(|(_, c)| c.revision > 0),
            "some stone melted"
        );
        assert!(skipped > 0, "some chunks were skipped");
    }
}
