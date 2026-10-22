//! The two kinds of pass (SAD-P2 §3).
//!
//! **Jacobi passes** (temperature, transitions, weathering, reactions, plant
//! growth) compute each chunk's changes from the previous state of the
//! chunk plus a 1-cell border ([`ChunkView`]), in parallel, then apply them
//! in sorted chunk order ([`run_jacobi`]). A flux across a chunk face must
//! be a pure function of the cell pair, computed identically from both
//! sides (rule 20); each chunk writes only its own cells.
//!
//! **Sweep passes** (water, gases, chemistry transport, plant spread)
//! visit active cells single-threaded in the global order `(y, cz, cx, lz,
//! lx)` and apply each move immediately ([`run_sweep`]). A cell is active
//! for a sweep kind when it holds the moving quantity or received some; it
//! leaves the set when it holds none at the end of its visit. A receiver
//! later in the global order is visited in the same sweep, an earlier one
//! in the next, exactly as a full scan of every cell would
//! ([`reference::flat_sweep`](crate::reference::flat_sweep)).

use std::collections::BTreeMap;

use rayon::prelude::*;
use rmc_world::EcoRules;
use rmc_world::chunk::Chunk;
use rmc_world::coords::{
    CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, CellAabb, CellPos, ChunkPos, LocalPos,
};
use rmc_world::layers::{LayerId, N_SWEEPS, PlantState};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::world::World;

use crate::ledger::merge;
use crate::reference::order_key;

/// How a Jacobi pass computes its chunks. Both give identical results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exec {
    Serial,
    /// One task per chunk on the rayon pool.
    Parallel,
}

/// What a Jacobi pass computed for one chunk from the previous state.
/// Indices are local to the chunk, so a chunk writes only its own cells.
///
/// Applied in this order: material changes through
/// [`World::eco_set`] (which runs the displacement rules), then layer
/// deltas, then plant states, then freeze-thaw bits, then the ledger terms.
/// The [`Queued`] changes of every chunk are applied after all chunks'
/// outputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JacobiOut {
    /// New materials. A cell that already holds the material is skipped.
    pub materials: Vec<(u16, MaterialId)>,
    /// Amounts added to a layer (negative: removed). A sum above the
    /// layer's range is clamped; for moisture, substances and gases the
    /// clamped part is charged to the matching `*saturation_loss` term. A
    /// removal of more moisture, substance or gas than the cell holds is a
    /// pass bug and panics; other layers clamp at their minimum.
    pub layers: Vec<(u16, LayerId, i32)>,
    /// Plant states to write (`None`: removed).
    pub plants: Vec<(u16, Option<PlantState>)>,
    /// Cells whose freeze-thaw bit (SAD-P2 §7.5) to set. The bits are not
    /// hashed, so setting one is not a change.
    pub freeze_thaw: Vec<u16>,
    /// Ledger terms of the computed changes.
    pub ledger: SeasonLedger,
    /// Changes queued for cells that may lie outside the chunk (SAD-P2
    /// §7.6 `Coat`).
    pub queued: Vec<Queued>,
}

impl JacobiOut {
    /// Whether the output asks for no change and books nothing.
    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
            && self.layers.iter().all(|&(_, _, d)| d == 0)
            && self.plants.is_empty()
            && self.freeze_thaw.is_empty()
            && self.ledger == SeasonLedger::default()
            && self.queued.is_empty()
    }
}

/// A layer delta a chunk queues for any resident cell, its own or one in
/// another chunk's (SAD-P2 §7.6: `Coat` effects are queued as deltas).
///
/// After every chunk's output is applied, the queued deltas of all chunks
/// are summed per target cell and layer, so deltas to the same target are
/// additive, and applied in the global cell order like layer deltas
/// (clamped, the excess charged to `*saturation_loss`). Then, in the same
/// order, each target whose layer holds at least the `then_set` threshold
/// changes material through [`World::eco_set`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Queued {
    pub target: CellPos,
    pub layer: LayerId,
    pub amount: i32,
    /// `(material, threshold)`: the target becomes `material` when its
    /// `layer` holds at least `threshold` after the queued deltas.
    pub then_set: Option<(MaterialId, i32)>,
}

impl Queued {
    /// A material change for any resident cell (SAD-P2 §7.8 step 4: a tall
    /// plant's upper cell may lie in the chunk above), applied with the
    /// other queued conversions. It is a conversion at threshold 0 on a
    /// layer it leaves unchanged, so it always fires.
    pub fn set_material(target: CellPos, material: MaterialId) -> Self {
        Self {
            target,
            layer: LayerId::Growth,
            amount: 0,
            then_set: Some((material, 0)),
        }
    }
}

/// A Jacobi pass: a pure function of the previous state.
pub trait JacobiPass: Sync {
    /// Computes one chunk's changes. Must read only through `view`.
    fn compute(&self, view: &ChunkView<'_>) -> JacobiOut;
}

/// Read access to one chunk and its 1-cell border in the previous state.
/// Reading a cell outside that box panics: a Jacobi pass never depends on
/// anything further away.
#[derive(Clone, Copy)]
pub struct ChunkView<'w> {
    world: &'w World,
    pos: ChunkPos,
    chunk: &'w Chunk,
}

impl<'w> ChunkView<'w> {
    /// A view of the resident chunk at `pos`; `None` if it is not resident.
    pub fn new(world: &'w World, pos: ChunkPos) -> Option<Self> {
        let chunk = world.chunk(pos)?;
        Some(Self { world, pos, chunk })
    }

    pub fn pos(&self) -> ChunkPos {
        self.pos
    }

    /// The world bounds.
    pub fn bounds(&self) -> CellAabb {
        self.world.bounds()
    }

    /// The chunk itself.
    pub fn chunk(&self) -> &'w Chunk {
        self.chunk
    }

    /// The world cell of a local position in this chunk.
    pub fn cell(&self, local: LocalPos) -> CellPos {
        CellPos::from_chunk_local(self.pos, local)
    }

    /// The chunk's in-bounds cells in local index order, with their local
    /// index.
    pub fn cells(&self) -> impl Iterator<Item = (u16, CellPos)> + '_ {
        let bounds = self.world.bounds();
        (0..CHUNK_VOLUME as u16).filter_map(move |i| {
            let cell = self.cell(LocalPos::from_index(i));
            bounds.contains(cell).then_some((i, cell))
        })
    }

    /// Whether `cell` is in the chunk or its 1-cell border.
    pub fn in_reach(&self, cell: CellPos) -> bool {
        let lo = self.pos.origin_cell().0 - 1;
        let hi = lo + CHUNK_SIZE + 1;
        lo.cmple(cell.0).all() && cell.0.cmple(hi).all()
    }

    fn reach(&self, cell: CellPos) {
        assert!(
            self.in_reach(cell),
            "Jacobi pass read {cell:?} outside chunk {:?} and its 1-cell border",
            self.pos
        );
    }

    /// Material at `cell`; `None` outside the bounds or in a non-resident
    /// chunk.
    pub fn material(&self, cell: CellPos) -> Option<MaterialId> {
        self.reach(cell);
        self.world.get_cell(cell)
    }

    /// Layer `id` at `cell`, widened; 0 outside the bounds, in a
    /// non-resident chunk or when absent.
    pub fn layer(&self, cell: CellPos, id: LayerId) -> i32 {
        self.reach(cell);
        self.world.layer(cell, id)
    }

    /// Plant state at `cell`.
    pub fn plant(&self, cell: CellPos) -> Option<&'w PlantState> {
        self.reach(cell);
        self.world.plant(cell)
    }

    /// `surface_y` of a cell column in reach.
    pub fn surface_y(&self, x: i32, z: i32) -> i16 {
        self.reach(CellPos::new(x, self.pos.origin_cell().0.y, z));
        self.world.surface_y(x, z)
    }

    pub fn materials(&self) -> &'w MaterialTable {
        self.world.materials()
    }

    pub fn rules(&self) -> &'w EcoRules {
        self.world.eco_rules()
    }
}

/// What a Jacobi run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JacobiReport {
    /// Chunks whose output changed a material, layer value or plant, or
    /// booked a ledger term, in the order they were applied (sorted chunk
    /// order). Outputs that turn out to change nothing are not listed.
    pub changed: Vec<ChunkPos>,
}

/// Runs `pass` over every resident chunk. See [`run_jacobi_on`].
pub fn run_jacobi(
    world: &mut World,
    pass: &impl JacobiPass,
    exec: Exec,
    ledger: &mut SeasonLedger,
) -> JacobiReport {
    let chunks: Vec<ChunkPos> = world.iter_chunks_sorted().map(|(p, _)| p).collect();
    run_jacobi_on(world, pass, &chunks, exec, ledger)
}

/// Runs `pass` over the resident chunks among `chunks`: computes every
/// chunk from the current state (in parallel with [`Exec::Parallel`]),
/// then applies the outputs in sorted chunk order, merging their ledger
/// terms and the displacement terms of their material changes into
/// `ledger`.
pub fn run_jacobi_on(
    world: &mut World,
    pass: &impl JacobiPass,
    chunks: &[ChunkPos],
    exec: Exec,
    ledger: &mut SeasonLedger,
) -> JacobiReport {
    let mut sorted: Vec<ChunkPos> = chunks
        .iter()
        .copied()
        .filter(|&p| world.chunk(p).is_some())
        .collect();
    sorted.sort_unstable();
    sorted.dedup();
    let outs: Vec<JacobiOut> = {
        let world: &World = world;
        let compute = |&pos: &ChunkPos| {
            let view = ChunkView::new(world, pos).expect("listed chunk is resident");
            pass.compute(&view)
        };
        match exec {
            Exec::Serial => sorted.iter().map(compute).collect(),
            Exec::Parallel => sorted.par_iter().map(compute).collect(),
        }
    };
    let mut report = JacobiReport::default();
    for (&pos, out) in sorted.iter().zip(&outs) {
        if !out.is_empty() && apply_out(world, pos, out, ledger) {
            report.changed.push(pos);
        }
    }
    let queued: Vec<Queued> = outs.iter().flat_map(|o| o.queued.iter().copied()).collect();
    for pos in apply_queued(world, &queued, ledger) {
        if !report.changed.contains(&pos) {
            report.changed.push(pos);
        }
    }
    tracing::debug!(chunks = report.changed.len(), "jacobi pass applied");
    report
}

/// The sort key of a queued delta: target in the global cell order, then
/// layer.
type QueueKey = ((i32, i32, i32, i32, i32), LayerId);

/// Applies the queued deltas of a run (see [`Queued`]). Returns the chunks
/// of the targets that changed, in the order they first changed.
fn apply_queued(world: &mut World, queued: &[Queued], ledger: &mut SeasonLedger) -> Vec<ChunkPos> {
    let mut sums: BTreeMap<QueueKey, (CellPos, i32)> = BTreeMap::new();
    let mut converts: BTreeMap<QueueKey, (CellPos, MaterialId, i32)> = BTreeMap::new();
    for q in queued {
        let key = (order_key(q.target), q.layer);
        sums.entry(key).or_insert((q.target, 0)).1 += q.amount;
        if let Some((m, threshold)) = q.then_set {
            converts.insert(key, (q.target, m, threshold));
        }
    }
    let mut changed: Vec<ChunkPos> = Vec::new();
    let mut note = |c: CellPos| {
        let (pos, _) = c.split();
        if !changed.contains(&pos) {
            changed.push(pos);
        }
    };
    for (&(_, id), &(c, d)) in &sums {
        if put_delta(world, c, id, d, ledger) {
            note(c);
        }
    }
    for (&(_, id), &(c, m, threshold)) in &converts {
        if world.layer(c, id) >= threshold && world.get_cell(c).is_some_and(|from| from != m) {
            world.eco_set(c, m, ledger);
            note(c);
        }
    }
    changed
}

/// Applies one chunk's output: materials, layer deltas, plants, freeze-thaw
/// bits, ledger.
/// Returns whether anything changed or a ledger term was booked.
///
/// Panics if a delta would take a moisture, substance or gas layer below 0:
/// the pass booked a removal of more than the cell holds, and the ledger
/// records actual amounts only (SAD-P2 §3).
fn apply_out(world: &mut World, pos: ChunkPos, out: &JacobiOut, ledger: &mut SeasonLedger) -> bool {
    let cell = |i: u16| CellPos::from_chunk_local(pos, LocalPos::from_index(i));
    let mut changed = out.ledger != SeasonLedger::default();
    for &(i, to) in &out.materials {
        let c = cell(i);
        if world.get_cell(c).is_some_and(|from| from != to) {
            world.eco_set(c, to, ledger);
            changed = true;
        }
    }
    for &(i, id, d) in &out.layers {
        changed |= put_delta(world, cell(i), id, d, ledger);
    }
    for &(i, plant) in &out.plants {
        changed |= world.eco_put_plant(cell(i), plant);
    }
    for &i in &out.freeze_thaw {
        world.eco_mark_freeze_thaw(cell(i));
    }
    merge(ledger, &out.ledger);
    changed
}

/// Adds `d` to layer `id` at `c`, clamped to the layer's range; for
/// moisture, substances and gases the clamped part is charged to the
/// matching `*saturation_loss` term. Returns whether the value changed. A
/// cell outside the bounds or not resident is skipped.
///
/// Panics if `d` would take a moisture, substance or gas layer below 0.
fn put_delta(
    world: &mut World,
    c: CellPos,
    id: LayerId,
    d: i32,
    ledger: &mut SeasonLedger,
) -> bool {
    if d == 0 || world.get_cell(c).is_none() {
        return false;
    }
    let want = world.layer(c, id) + d;
    let (lo, hi) = layer_range(id);
    let conserved = matches!(
        id,
        LayerId::Moisture | LayerId::Substance(_) | LayerId::Gas(_)
    );
    assert!(
        !conserved || want >= lo,
        "Jacobi pass removed {} of {id:?} at {c:?}, which holds {}",
        -d,
        want - d
    );
    let got = want.clamp(lo, hi);
    let lost = i64::from(want - got);
    match id {
        LayerId::Moisture => ledger.water_saturation_loss += lost,
        LayerId::Substance(x) => ledger.saturation_loss[usize::from(x)] += lost,
        LayerId::Gas(g) => {
            ledger.gas_saturation_loss[usize::from(g)] += lost;
            if world.eco_rules().steam == Some(g) {
                ledger.water_saturation_loss += lost;
            }
        }
        _ => {}
    }
    world.eco_put_layer(c, id, got)
}

/// The values a layer's type holds.
fn layer_range(id: LayerId) -> (i32, i32) {
    if id.is_i16() {
        (i32::from(i16::MIN), i32::from(i16::MAX))
    } else {
        (0, i32::from(u8::MAX))
    }
}

/// A sweep pass: moves visited in global order, applied immediately.
pub trait SweepPass {
    /// Which active set the sweep uses (`SWEEP_WATER`, `SWEEP_GAS + g`, ...).
    fn kind(&self) -> usize;

    /// Visits one active cell. Every move checks donor content and
    /// receiver room against the current state, writes its ledger entry,
    /// and marks the receiver with [`SweepCtx::receive`].
    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos);

    /// Whether `cell` holds the moving quantity. A visited cell that holds
    /// none leaves the active set.
    fn holds(&self, world: &World, cell: CellPos) -> bool;
}

/// What a sweep visit may change.
pub struct SweepCtx<'a> {
    pub world: &'a mut World,
    pub ledger: &'a mut SeasonLedger,
    kind: usize,
}

impl SweepCtx<'_> {
    /// Marks `cell` as having received the moving quantity: it is visited
    /// later in this sweep if it comes later in the global order, else in
    /// the next sweep.
    pub fn receive(&mut self, cell: CellPos) {
        self.world.eco_activate(cell, self.kind);
    }

    /// The sweep's kind.
    pub fn kind(&self) -> usize {
        self.kind
    }
}

/// What a sweep did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Cells visited.
    pub visits: u64,
}

/// Runs one sweep of `pass` over the active cells of its kind, in the
/// global order `(y, cz, cx, lz, lx)`.
pub fn run_sweep(
    world: &mut World,
    pass: &mut impl SweepPass,
    ledger: &mut SeasonLedger,
) -> SweepReport {
    let kind = pass.kind();
    assert!(kind < N_SWEEPS, "sweep kind {kind} out of range");
    let bounds = world.bounds();
    // Resident chunks per chunk-y, each list in (cz, cx) order.
    let mut layers: BTreeMap<i32, Vec<ChunkPos>> = BTreeMap::new();
    for (pos, _) in world.iter_chunks_sorted() {
        layers.entry(pos.0.y).or_default().push(pos);
    }
    let mut ctx = SweepCtx {
        world,
        ledger,
        kind,
    };
    let mut report = SweepReport::default();
    for y in bounds.min.0.y..=bounds.max.0.y {
        let Some(chunks) = layers.get(&(y >> CHUNK_SHIFT)) else {
            continue;
        };
        let plane = (y & CHUNK_MASK) as usize * WORDS_PER_PLANE;
        for &pos in chunks {
            // Nothing active in this plane of the chunk yet (earlier visits
            // of this sweep included): skip it.
            let idle = ctx.world.chunk(pos).is_none_or(|c| {
                (plane..plane + WORDS_PER_PLANE).all(|w| c.active[kind].word(w) == 0)
            });
            if idle {
                continue;
            }
            for w in plane..plane + WORDS_PER_PLANE {
                // Re-read the word after every visit: a visit may activate
                // a later cell of the same word, visited in this sweep.
                let mut from = 0;
                while from < u64::BITS {
                    let word = ctx.world.chunk(pos).map_or(0, |c| c.active[kind].word(w))
                        & (u64::MAX << from);
                    if word == 0 {
                        break;
                    }
                    let bit = word.trailing_zeros();
                    from = bit + 1;
                    let local = LocalPos::from_index((w * WORD_BITS + bit as usize) as u16);
                    let cell = CellPos::from_chunk_local(pos, local);
                    if bounds.contains(cell) {
                        pass.visit(&mut ctx, cell);
                        report.visits += 1;
                    }
                    if !bounds.contains(cell) || !pass.holds(ctx.world, cell) {
                        ctx.world.eco_deactivate(cell, kind);
                    }
                }
            }
        }
    }
    tracing::debug!(kind, visits = report.visits, "sweep done");
    report
}

/// Bits per active-set word.
const WORD_BITS: usize = u64::BITS as usize;
/// Active-set words per y-plane of a chunk: local (z, x) in index order.
const WORDS_PER_PLANE: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize / WORD_BITS;

/// Adds every in-bounds resident cell that holds `pass`'s quantity to its
/// active set (after worldgen, a load or a snapshot, which do not keep
/// active sets). Returns how many cells were added.
pub fn activate_holders(world: &mut World, pass: &impl SweepPass) -> usize {
    let kind = pass.kind();
    let bounds = world.bounds();
    let holders: Vec<CellPos> = world
        .iter_chunks_sorted()
        .flat_map(|(pos, _)| {
            (0..CHUNK_VOLUME as u16)
                .map(move |i| CellPos::from_chunk_local(pos, LocalPos::from_index(i)))
        })
        .filter(|&cell| bounds.contains(cell) && pass.holds(world, cell))
        .collect();
    holders
        .into_iter()
        .filter(|&cell| world.eco_activate(cell, kind))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use rmc_world::coords::CellAabb;
    use rmc_world::layers::{SWEEP_TRANSPORT, SWEEP_WATER};

    use crate::EcoTables;
    use crate::invariants::InvariantChecker;
    use crate::reference::{FlatWorld, flat_sweep, global_order, order_key};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    struct Mats {
        air: MaterialId,
        stone: MaterialId,
    }

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    /// Air everywhere except a stone floor at `y < floor`, over `bounds`,
    /// chunks inserted in reverse sorted order so that nothing depends on
    /// insertion order.
    fn world(bounds: CellAabb, floor: i32) -> (World, Mats) {
        let t = tables();
        let m = t.materials().clone();
        let mats = Mats {
            air: m.id("air").unwrap(),
            stone: m.id("stone").unwrap(),
        };
        let mut w = World::new(Arc::clone(&m), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut chunks: Vec<ChunkPos> = bounds.chunks().collect();
        chunks.reverse();
        for pos in chunks {
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

    /// 3 × 2 × 2 chunks, with negative x and y.
    fn bounds_12() -> CellAabb {
        CellAabb::new(CellPos::new(-32, -32, 0), CellPos::new(63, 31, 63))
    }

    /// Deterministic per-cell noise.
    fn noise(cell: CellPos, salt: u64) -> u64 {
        let c = cell.0;
        let mut h = salt ^ 0x9E37_79B9_7F4A_7C15;
        for v in [c.x, c.y, c.z] {
            h = (h ^ (v as u32 as u64)).wrapping_mul(0x100_0000_01B3);
            h ^= h >> 29;
        }
        h
    }

    /// A Jacobi pass from a closure.
    struct FnPass<F>(F);

    impl<F: Fn(&ChunkView<'_>) -> JacobiOut + Sync> JacobiPass for FnPass<F> {
        fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
            (self.0)(view)
        }
    }

    const SIX: [[i32; 3]; 6] = [
        [0, -1, 0],
        [0, 0, -1],
        [-1, 0, 0],
        [1, 0, 0],
        [0, 0, 1],
        [0, 1, 0],
    ];

    fn offset(cell: CellPos, d: [i32; 3]) -> CellPos {
        CellPos::new(cell.0.x + d[0], cell.0.y + d[1], cell.0.z + d[2])
    }

    /// Pair-symmetric diffusion of temperature across every face, borders
    /// included; an air cell hotter than 900 turns to stone.
    fn diffusion(stone: MaterialId, air: MaterialId) -> impl JacobiPass {
        FnPass(move |v: &ChunkView<'_>| {
            let mut out = JacobiOut::default();
            for (i, cell) in v.cells() {
                let t = v.layer(cell, LayerId::Temperature);
                let mut d = 0;
                for o in SIX {
                    let n = offset(cell, o);
                    if v.material(n).is_some() {
                        d += (v.layer(n, LayerId::Temperature) - t) / 8;
                    }
                }
                if d != 0 {
                    out.layers.push((i, LayerId::Temperature, d));
                }
                if t > 900 && v.material(cell) == Some(air) {
                    out.materials.push((i, stone));
                }
            }
            out
        })
    }

    #[test]
    fn jacobi_parallel_equals_serial_hash() {
        let (mut serial, m) = world(bounds_12(), 20);
        for cell in global_order(bounds_12()) {
            let n = noise(cell, 1);
            if n.is_multiple_of(5) {
                serial.eco_put_layer(cell, LayerId::Temperature, (n % 1200) as i32 - 200);
            }
            if n.is_multiple_of(11) {
                serial.eco_put_layer(cell, LayerId::Moisture, (n % 200) as i32);
            }
        }
        let start = serial.hash();
        let mut parallel = serial.clone();
        let pass = diffusion(m.stone, m.air);
        let (mut ls, mut lp) = (SeasonLedger::default(), SeasonLedger::default());
        for step in 0..3 {
            let rs = run_jacobi(&mut serial, &pass, Exec::Serial, &mut ls);
            let rp = run_jacobi(&mut parallel, &pass, Exec::Parallel, &mut lp);
            assert_eq!(rs, rp, "reports after step {step}");
            assert_eq!(serial.hash(), parallel.hash(), "hash after step {step}");
            assert_eq!(ls, lp, "ledgers after step {step}");
            assert_eq!(rs.changed.len(), 12, "every chunk changes");
        }
        assert_ne!(serial.hash(), start, "the pass did something");
        assert!(
            serial.iter_chunks_sorted().any(|(_, c)| c.revision > 0),
            "some hot air turned to stone"
        );
    }

    /// One Jacobi step of the diffusion rule on flat arrays: every cell
    /// computed from a copy of the previous state.
    fn flat_diffusion_step(flat: &mut FlatWorld) {
        let prev = flat.clone();
        for cell in global_order(prev.bounds()) {
            let t = prev.layer(cell, LayerId::Temperature);
            let mut d = 0;
            for o in SIX {
                let n = offset(cell, o);
                if prev.contains(n) {
                    d += (prev.layer(n, LayerId::Temperature) - t) / 8;
                }
            }
            flat.set_layer(cell, LayerId::Temperature, t + d);
        }
    }

    #[test]
    fn jacobi_matches_flat_reference_across_chunk_faces() {
        let (mut w, m) = world(bounds_12(), 20);
        for cell in global_order(bounds_12()) {
            let n = noise(cell, 3);
            if n.is_multiple_of(3) {
                w.eco_put_layer(cell, LayerId::Temperature, (n % 900) as i32 - 200);
            }
        }
        let mut flat = FlatWorld::from_world(&w, &[LayerId::Temperature]);
        let pass = diffusion(m.stone, m.air);
        for step in 0..2 {
            run_jacobi(&mut w, &pass, Exec::Parallel, &mut SeasonLedger::default());
            flat_diffusion_step(&mut flat);
            assert_eq!(flat.first_difference(&w), None, "step {step}");
        }
    }

    #[test]
    fn jacobi_reads_only_the_1_cell_border() {
        let (w, _) = world(bounds_12(), 20);
        let v = ChunkView::new(&w, ChunkPos::new(0, 0, 0)).unwrap();
        assert!(v.in_reach(CellPos::new(-1, -1, -1)));
        assert!(v.in_reach(CellPos::new(32, 32, 32)));
        assert!(!v.in_reach(CellPos::new(33, 5, 5)));
        assert!(!v.in_reach(CellPos::new(5, -2, 5)));
        let far = std::panic::catch_unwind(|| v.layer(CellPos::new(33, 5, 5), LayerId::Moisture));
        assert!(far.is_err(), "a read two cells out panics");
    }

    #[test]
    fn apply_in_sorted_chunk_order() {
        // Chunk A (0,0,0) and B (1,0,0); a = (31,5,5) in A holds moisture,
        // b = (32,5,5) in B is its only air neighbor, and c = (33,5,5) is
        // b's only other air neighbor. Both chunks turn their cell to
        // stone. A first: a's moisture is pushed into b, then b's into c.
        // B first: b holds nothing when it turns, then a has no air
        // neighbor and the moisture is lost.
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31));
        let (mut w, m) = world(bounds, 32);
        let (a, b, c) = (
            CellPos::new(31, 5, 5),
            CellPos::new(32, 5, 5),
            CellPos::new(33, 5, 5),
        );
        let mut setup = SeasonLedger::default();
        for cell in [a, b, c] {
            w.eco_set(cell, m.air, &mut setup);
        }
        w.eco_put_layer(a, LayerId::Moisture, 100);
        let checker = InvariantChecker::new(&w);
        let stone = m.stone;
        let pass = FnPass(move |v: &ChunkView<'_>| {
            let mut out = JacobiOut::default();
            for cell in [a, b] {
                if cell.chunk() == v.pos() {
                    out.materials.push((cell.local().index(), stone));
                }
            }
            out
        });
        for exec in [Exec::Serial, Exec::Parallel] {
            let mut w = w.clone();
            let mut ledger = SeasonLedger::default();
            let r = run_jacobi(&mut w, &pass, exec, &mut ledger);
            assert_eq!(r.changed, vec![a.chunk(), b.chunk()], "{exec:?}");
            assert_eq!(w.layer(c, LayerId::Moisture), 100, "{exec:?}");
            assert_eq!(ledger.water_saturation_loss, 0, "{exec:?}");
            assert_eq!(checker.check(&w, &ledger), Ok(()), "{exec:?}");
        }

        // Over many chunks the application order is the sorted order,
        // whatever the insertion or completion order.
        let (mut w, _) = world(bounds_12(), 20);
        let pass = FnPass(|_: &ChunkView<'_>| JacobiOut {
            layers: vec![(0, LayerId::Regolith, 1)],
            ..Default::default()
        });
        let r = run_jacobi(&mut w, &pass, Exec::Parallel, &mut SeasonLedger::default());
        let sorted: Vec<ChunkPos> = w.iter_chunks_sorted().map(|(p, _)| p).collect();
        assert_eq!(r.changed, sorted);
        // Only the listed chunks run, still in sorted order.
        let some = [sorted[7], sorted[2], sorted[9]];
        let r = run_jacobi_on(
            &mut w,
            &pass,
            &some,
            Exec::Parallel,
            &mut SeasonLedger::default(),
        );
        assert_eq!(r.changed, vec![sorted[2], sorted[7], sorted[9]]);
    }

    #[test]
    fn jacobi_layer_overflow_is_clamped_and_charged() {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let (mut w, _) = world(bounds, 0);
        let cell = CellPos::new(3, 3, 3);
        w.eco_put_layer(cell, LayerId::Moisture, 250);
        w.eco_put_layer(cell, LayerId::Substance(1), 20);
        let checker = InvariantChecker::new(&w);
        let i = cell.local().index();
        let pass = FnPass(move |_: &ChunkView<'_>| {
            let mut out = JacobiOut {
                layers: vec![
                    (i, LayerId::Moisture, 10),
                    (i, LayerId::Substance(1), -9),
                    (i, LayerId::Temperature, i32::from(i16::MAX)),
                ],
                ..Default::default()
            };
            out.ledger.rain_in = 10;
            out.ledger.uptake_sink[1] = 9;
            out
        });
        let mut ledger = SeasonLedger::default();
        run_jacobi(&mut w, &pass, Exec::Serial, &mut ledger);
        run_jacobi(&mut w, &pass, Exec::Serial, &mut ledger);
        assert_eq!(w.layer(cell, LayerId::Moisture), 255);
        assert_eq!(w.layer(cell, LayerId::Substance(1)), 2);
        assert_eq!(w.layer(cell, LayerId::Temperature), i32::from(i16::MAX));
        assert_eq!(ledger.water_saturation_loss, 15);
        assert_eq!(ledger.saturation_loss[1], 0);
        assert_eq!(checker.check(&w, &ledger), Ok(()));
        // A third run asks for 9 of the 2 left: the pass is wrong.
        let third = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_jacobi(&mut w, &pass, Exec::Serial, &mut ledger)
        }));
        assert!(third.is_err(), "over-removal panics instead of booking");
    }

    #[test]
    fn jacobi_report_omits_outputs_that_change_nothing() {
        let (mut w, _) = world(bounds_12(), 20);
        // Every chunk asks for the material its cell already holds, and
        // adds 0 to a layer.
        let pass = FnPass(|v: &ChunkView<'_>| {
            let top = LocalPos::new(1, 31, 1);
            let here = v.material(v.cell(top)).expect("resident");
            JacobiOut {
                materials: vec![(top.index(), here)],
                layers: vec![(top.index(), LayerId::Moisture, 0)],
                ..Default::default()
            }
        });
        let hash = w.hash();
        let r = run_jacobi(&mut w, &pass, Exec::Serial, &mut SeasonLedger::default());
        assert_eq!(r.changed, Vec::<ChunkPos>::new());
        assert_eq!(w.hash(), hash);
    }

    /// Records the order of visits; nothing is held after a visit.
    struct Recorder {
        kind: usize,
        seen: Vec<CellPos>,
    }

    impl SweepPass for Recorder {
        fn kind(&self) -> usize {
            self.kind
        }
        fn visit(&mut self, _: &mut SweepCtx<'_>, cell: CellPos) {
            self.seen.push(cell);
        }
        fn holds(&self, _: &World, _: CellPos) -> bool {
            false
        }
    }

    #[test]
    fn sweep_visits_global_y_cz_cx_lz_lx_order() {
        let bounds = bounds_12();
        let (mut w, _) = world(bounds, 20);
        let mut active = BTreeSet::new();
        for cell in global_order(bounds) {
            let n = noise(cell, 7);
            // Scattered cells, plus a slab across all chunk faces.
            let slab = (-2..=1).contains(&cell.0.y) && (28..36).contains(&cell.0.z);
            if n.is_multiple_of(61) || slab {
                w.eco_activate(cell, SWEEP_TRANSPORT);
                active.insert(cell);
            }
        }
        // Another kind's set is not visited.
        w.eco_activate(CellPos::new(1, 1, 1), SWEEP_WATER);
        let mut pass = Recorder {
            kind: SWEEP_TRANSPORT,
            seen: Vec::new(),
        };
        let r = run_sweep(&mut w, &mut pass, &mut SeasonLedger::default());
        let want: Vec<CellPos> = global_order(bounds)
            .filter(|c| active.contains(c))
            .collect();
        assert!(want.len() > 3000);
        assert_eq!(pass.seen, want);
        assert_eq!(r.visits, want.len() as u64);
        assert!(
            pass.seen
                .windows(2)
                .all(|p| order_key(p[0]) < order_key(p[1]))
        );
        // Visited cells holding nothing left the set.
        let rest: usize = w
            .iter_chunks_sorted()
            .map(|(_, c)| c.active[SWEEP_TRANSPORT].len())
            .sum();
        assert_eq!(rest, 0);
        assert!(
            w.chunk(ChunkPos::new(0, 0, 0)).unwrap().active[SWEEP_WATER]
                .contains(LocalPos::new(1, 1, 1))
        );
    }

    /// Moves a cell's whole substance 0 one step: down while above y 16,
    /// then +x up to x 40, limited by the receiver's room.
    struct Token;

    const TOKEN: LayerId = LayerId::Substance(0);

    fn token_next(cell: CellPos) -> Option<CellPos> {
        let c = cell.0;
        if c.y > 16 {
            Some(CellPos::new(c.x, c.y - 1, c.z))
        } else if c.x < 40 {
            Some(CellPos::new(c.x + 1, c.y, c.z))
        } else {
            None
        }
    }

    impl SweepPass for Token {
        fn kind(&self) -> usize {
            SWEEP_TRANSPORT
        }
        fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
            let have = ctx.world.layer(cell, TOKEN);
            let Some(to) = token_next(cell) else { return };
            if have == 0 || ctx.world.get_cell(to).is_none() {
                return;
            }
            let room = i32::from(u8::MAX) - ctx.world.layer(to, TOKEN);
            let moved = have.min(room);
            if moved == 0 {
                return;
            }
            let there = ctx.world.layer(to, TOKEN);
            ctx.world.eco_put_layer(cell, TOKEN, have - moved);
            ctx.world.eco_put_layer(to, TOKEN, there + moved);
            ctx.receive(to);
        }
        fn holds(&self, world: &World, cell: CellPos) -> bool {
            world.layer(cell, TOKEN) > 0
        }
    }

    fn flat_token(flat: &mut FlatWorld, cell: CellPos) {
        let have = flat.layer(cell, TOKEN);
        let Some(to) = token_next(cell) else { return };
        if have == 0 || !flat.contains(to) {
            return;
        }
        let there = flat.layer(to, TOKEN);
        let moved = have.min(i32::from(u8::MAX) - there);
        if moved == 0 {
            return;
        }
        flat.set_layer(cell, TOKEN, have - moved);
        flat.set_layer(to, TOKEN, there + moved);
    }

    fn active(w: &World, cell: CellPos) -> bool {
        w.chunk(cell.chunk())
            .is_some_and(|c| c.active[SWEEP_TRANSPORT].contains(cell.local()))
    }

    #[test]
    fn active_set_includes_receivers() {
        let bounds = bounds_12();
        let (mut w, _) = world(bounds, 0);
        let falling = CellPos::new(5, 20, 5);
        let sliding = CellPos::new(28, 10, 40);
        w.eco_put_layer(falling, TOKEN, 10);
        w.eco_put_layer(sliding, TOKEN, 7);
        // Substances are not activated by the write itself: the runner
        // seeds the set from what the cells hold.
        assert!(!active(&w, falling));
        assert_eq!(activate_holders(&mut w, &Token), 2);
        assert!(active(&w, falling) && active(&w, sliding));

        let mut flat = FlatWorld::from_world(&w, &[TOKEN]);
        let checker = InvariantChecker::new(&w);
        let ledger = SeasonLedger::default();

        let r = run_sweep(&mut w, &mut Token, &mut SeasonLedger::default());
        flat_sweep(&mut flat, flat_token);
        // The falling token moved one cell down; its receiver comes earlier
        // in the global order, so it is active and waits for the next sweep.
        let below = CellPos::new(5, 19, 5);
        assert_eq!(w.layer(below, TOKEN), 10);
        assert!(active(&w, below), "the receiver is active");
        assert!(!active(&w, falling), "the emptied donor left the set");
        // The sliding token's receivers come later: it crossed the chunk
        // face at x 32 within one sweep, to x 40.
        let end = CellPos::new(40, 10, 40);
        assert_eq!(w.layer(end, TOKEN), 7);
        assert!(active(&w, end), "a holder stays active");
        assert_eq!(r.visits, 1 + (40 - 28 + 1));
        assert_eq!(
            flat.first_difference(&w),
            None,
            "sweep 1 matches the flat scan"
        );

        let r = run_sweep(&mut w, &mut Token, &mut SeasonLedger::default());
        flat_sweep(&mut flat, flat_token);
        assert_eq!(w.layer(CellPos::new(5, 18, 5), TOKEN), 10);
        assert!(active(&w, CellPos::new(5, 18, 5)) && !active(&w, below));
        assert_eq!(r.visits, 2, "the receiver and the parked token");
        assert_eq!(
            flat.first_difference(&w),
            None,
            "sweep 2 matches the flat scan"
        );

        // Many sweeps, a full receiver: still the flat result, still
        // conserved.
        w.eco_put_layer(CellPos::new(5, 16, 5), TOKEN, 250);
        flat.set_layer(CellPos::new(5, 16, 5), TOKEN, 250);
        activate_holders(&mut w, &Token);
        for sweep in 0..12 {
            run_sweep(&mut w, &mut Token, &mut SeasonLedger::default());
            flat_sweep(&mut flat, flat_token);
            assert_eq!(flat.first_difference(&w), None, "sweep {sweep}");
        }
        let mut booked = ledger;
        booked.edit_in[0] = 250;
        assert_eq!(checker.check(&w, &booked), Ok(()));
    }
}
