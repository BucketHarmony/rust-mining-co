//! The client's copy of the world (SAD §8.5 replica rule, §11.1).
//!
//! [`Replica`] is a Bevy resource holding a [`World`] that only
//! [`ServerMsg`] handlers change: `Welcome` inserts a fresh, empty replica
//! with the host's bounds (a later `Welcome` replaces it), `ChunkSnapshot`
//! makes a chunk resident through [`World::insert_chunk`], and `Applied`
//! writes each delta through [`World::apply_delta`]. [`ReplicaPlugin`] runs
//! the handlers in `PreUpdate`, right after [`NetSystems::Receive`], so the
//! frame's `Update` systems see everything the host has sent so far.
//!
//! Every change marks chunks dirty for meshing. A chunk's mesh reads its own
//! cells plus a one-cell border from its neighbours (the 34³ `MeshInput`), so
//! a changed cell on a chunk border also marks the neighbours whose border
//! holds it: the face-adjacent one, and for cells on an edge or corner the
//! edge- and corner-adjacent ones too. A snapshot or `Full` delta may change
//! any border cell, so it marks every neighbour. A change in a column one
//! step in from the bounds' x or z edge also marks the chunks below it
//! there, because the mesh hides the shell beside a column open to the sky
//! (F095). Only resident chunks are marked; a chunk that becomes resident
//! later is marked then. Layer changes (`LayerSnapshot`, `ChunkDelta::Layers`)
//! change meshes only through the fill of liquid-pass cells (SAD-P2 §10), so
//! they mark a chunk only if it holds a liquid-pass material, together with
//! its neighbours that hold one too. While the layer overlay is shown
//! ([`Replica::set_layer_remesh`]), every mesh shows layer values, so a
//! layer change marks its chunk and every neighbour.
//!
//! If a message was lost, the next delta for an affected chunk is more than
//! one revision ahead of the replica's. The replica then writes a
//! [`ToHost`]`(RequestChunk)` for that chunk and ignores the chunk's deltas
//! until its snapshot arrives. The host handles messages in order, so that
//! snapshot already holds every change the ignored deltas carried.

use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::Arc;

use bevy::prelude::*;
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::chunk::{Chunk, ChunkStorage};
use rmc_world::coords::{CHUNK_MASK, CHUNK_VOLUME, CellAabb, CellPos, ChunkPos, LocalPos};
use rmc_world::edit::{ChunkDelta, PlayerId};
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::world::{DeltaError, World};

use crate::mesher::{is_liquid_pass, is_shell, shell_interior};
use crate::net::{ClientMaterials, FromHost, NetSystems, ToHost};

/// Why a host message could not be applied to the [`Replica`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaError {
    /// A snapshot for a chunk with no cell inside the bounds.
    #[error("snapshot for chunk {0:?} lies outside the world bounds")]
    SnapshotOutOfBounds(ChunkPos),
    /// A snapshot whose storage is not well formed.
    #[error("snapshot storage for chunk {0:?} is malformed")]
    MalformedSnapshot(ChunkPos),
    /// A snapshot whose palette names a material the table does not have.
    #[error("snapshot for chunk {pos:?} uses material {id:?}, which is not in the material table")]
    UnknownSnapshotMaterial { pos: ChunkPos, id: MaterialId },
    /// Deltas of an `Applied` result that the world refused, in delta order.
    /// The result's other deltas were applied.
    #[error("{} delta(s) refused, first: {}", .0.len(), .0[0])]
    Deltas(Vec<DeltaError>),
    /// A `LayerSnapshot` the world refused.
    #[error("layer snapshot refused: {0}")]
    LayerSnapshot(DeltaError),
}

/// The client-side world, changed only by [`ServerMsg`] handlers.
/// Inserted when `Welcome` arrives; absent before.
#[derive(Resource, Debug)]
pub struct Replica {
    world: World,
    player: PlayerId,
    /// Chunks with a revision gap whose snapshot has not arrived yet.
    awaiting: BTreeSet<ChunkPos>,
    /// Resident chunks whose mesh is out of date.
    dirty: BTreeSet<ChunkPos>,
    /// Whether layer changes remesh every chunk they touch (the layer
    /// overlay is on), not only liquid ones.
    layer_remesh: bool,
}

impl Replica {
    fn new(materials: Arc<MaterialTable>, player: PlayerId, bounds: CellAabb) -> Self {
        Self {
            world: World::new(materials, bounds),
            player,
            awaiting: BTreeSet::new(),
            dirty: BTreeSet::new(),
            layer_remesh: false,
        }
    }

    /// A replica holding `world`, for tests of systems that read it.
    #[cfg(test)]
    pub(crate) fn from_world(world: World) -> Self {
        Self {
            world,
            player: PlayerId(1),
            awaiting: BTreeSet::new(),
            dirty: BTreeSet::new(),
            layer_remesh: false,
        }
    }

    /// Marks every resident chunk dirty: the mesh pipeline does this when
    /// the mesh color source changes, and tests of its readers.
    pub(crate) fn mark_all_dirty(&mut self) {
        self.dirty = self
            .world
            .iter_chunks_sorted()
            .map(|(pos, _)| pos)
            .collect();
    }

    /// Whether layer changes remesh every chunk they touch rather than
    /// only liquid ones; see [`Replica::set_layer_remesh`].
    pub fn layer_remesh(&self) -> bool {
        self.layer_remesh
    }

    /// While on (the layer overlay is shown, SAD-P2 §10), a `LayerSnapshot`
    /// or `ChunkDelta::Layers` marks its chunk and all 26 neighbours,
    /// whose mesh border holds its cells. While off, only liquid meshes
    /// follow layers ([`Replica::mark_liquid`]).
    pub fn set_layer_remesh(&mut self, on: bool) {
        self.layer_remesh = on;
    }

    /// The replica world. Read-only: only host messages change it.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The id the host assigned in `Welcome`.
    pub fn player(&self) -> PlayerId {
        self.player
    }

    /// Whether the replica is waiting for a snapshot of `pos` after a
    /// revision gap. Deltas for that chunk are ignored until it arrives.
    pub fn is_awaiting(&self, pos: ChunkPos) -> bool {
        self.awaiting.contains(&pos)
    }

    /// Resident chunks whose cells or border cells changed since the last
    /// [`Replica::take_dirty`], ascending.
    pub fn dirty(&self) -> &BTreeSet<ChunkPos> {
        &self.dirty
    }

    pub fn is_dirty(&self, pos: ChunkPos) -> bool {
        self.dirty.contains(&pos)
    }

    /// Takes the dirty set, leaving it empty.
    pub fn take_dirty(&mut self) -> BTreeSet<ChunkPos> {
        std::mem::take(&mut self.dirty)
    }

    /// Applies one world message from the host. `Welcome` and `Rejected`
    /// are handled by the system that owns the resource; `Inventory`,
    /// `PlayerStates` and `Saved` do not touch the world.
    ///
    /// - `ChunkSnapshot` makes the chunk resident with the host's storage
    ///   and revision, replacing any copy, and ends a wait for it. Its
    ///   storage must be well formed and use only known materials, and
    ///   its layers and plants come with it.
    /// - `LayerSnapshot` replaces a resident chunk's layers and plants,
    ///   unless the replica is waiting for that chunk's full snapshot.
    ///   Layers change only liquid meshes ([`Replica::mark_liquid`]),
    ///   unless the overlay is on ([`Replica::set_layer_remesh`]).
    /// - `Applied` applies each delta in order. A delta for a chunk the
    ///   replica is waiting for is ignored. A delta more than one revision
    ///   ahead is a gap: `pos` goes into `requests` and the replica waits for
    ///   that chunk's snapshot; this is not an error. Other refused deltas
    ///   (including stale ones) change nothing and are reported together
    ///   after the rest have been applied.
    fn handle(
        &mut self,
        msg: &ServerMsg,
        requests: &mut Vec<ChunkPos>,
    ) -> Result<(), ReplicaError> {
        match msg {
            ServerMsg::ChunkSnapshot {
                pos,
                revision,
                eco_revision,
                storage,
                layers,
                plants,
            } => {
                let pos = *pos;
                // insert_chunk trusts its storage; a malformed one would
                // panic later in get or compact.
                if !storage.is_well_formed()
                    || plants.keys().any(|&i| usize::from(i) >= CHUNK_VOLUME)
                {
                    return Err(ReplicaError::MalformedSnapshot(pos));
                }
                let palette = match storage {
                    ChunkStorage::Uniform(id) => std::slice::from_ref(id),
                    ChunkStorage::Paletted { palette, .. } => palette.as_slice(),
                };
                let materials = self.world.materials();
                if let Some(&id) = palette.iter().find(|&&id| materials.get(id).is_none()) {
                    return Err(ReplicaError::UnknownSnapshotMaterial { pos, id });
                }
                let mut chunk = Chunk::new(storage.clone());
                chunk.revision = *revision;
                chunk.eco_revision = *eco_revision;
                chunk.layers = layers.clone();
                chunk.plants = plants.clone();
                self.world
                    .insert_chunk(pos, chunk)
                    .map_err(|_| ReplicaError::SnapshotOutOfBounds(pos))?;
                self.awaiting.remove(&pos);
                self.mark_with_neighbours(pos);
                self.mark_shell_below_chunk(pos);
            }
            ServerMsg::Applied(result) => {
                let mut refused = Vec::new();
                for delta in &result.deltas {
                    let pos = delta.pos();
                    if self.awaiting.contains(&pos) {
                        continue;
                    }
                    match self.world.apply_delta(delta) {
                        Ok(()) => match delta {
                            ChunkDelta::Cells { changes, .. } => {
                                for &(index, _) in changes {
                                    let local = LocalPos::from_index(index);
                                    self.mark_cell(pos, local);
                                    self.mark_shell_below(CellPos::from_chunk_local(pos, local));
                                }
                            }
                            ChunkDelta::Full { .. } => {
                                self.mark_with_neighbours(pos);
                                self.mark_shell_below_chunk(pos);
                            }
                            ChunkDelta::Layers { .. } => self.mark_layers(pos),
                        },
                        Err(DeltaError::RevisionGap { local, got, .. }) if got > local => {
                            debug!(?pos, local, got, "revision gap, requesting chunk");
                            self.awaiting.insert(pos);
                            requests.push(pos);
                        }
                        Err(e) => refused.push(e),
                    }
                }
                if !refused.is_empty() {
                    return Err(ReplicaError::Deltas(refused));
                }
            }
            ServerMsg::LayerSnapshot {
                pos,
                eco_revision,
                layers,
                plants,
            } => {
                if self.awaiting.contains(pos) {
                    return Ok(());
                }
                self.world
                    .apply_eco_snapshot(*pos, *eco_revision, layers.clone(), plants.clone())
                    .map_err(ReplicaError::LayerSnapshot)?;
                self.mark_layers(*pos);
            }
            ServerMsg::Welcome { .. }
            | ServerMsg::Rejected { .. }
            | ServerMsg::Inventory { .. }
            | ServerMsg::PlayerStates(_)
            | ServerMsg::Saved { .. }
            | ServerMsg::SeasonStarted { .. }
            | ServerMsg::SeasonDone { .. }
            | ServerMsg::Clock(_) => {}
        }
        Ok(())
    }

    /// Marks `pos` dirty if it is resident.
    fn mark(&mut self, pos: ChunkPos) {
        if self.world.chunk(pos).is_some() {
            self.dirty.insert(pos);
        }
    }

    /// Marks the chunk holding a changed cell, and every neighbour whose
    /// one-cell mesh border holds it.
    fn mark_cell(&mut self, pos: ChunkPos, local: LocalPos) {
        fn reach(c: u32) -> RangeInclusive<i32> {
            let lo = if c == 0 { -1 } else { 0 };
            let hi = if c == CHUNK_MASK as u32 { 1 } else { 0 };
            lo..=hi
        }
        let p = pos.0;
        for dy in reach(local.0.y) {
            for dz in reach(local.0.z) {
                for dx in reach(local.0.x) {
                    self.mark(ChunkPos::new(p.x + dx, p.y + dy, p.z + dz));
                }
            }
        }
    }

    /// Marks the resident chunks below the one holding `cell` whose mesh
    /// depends on `cell` (F095). The mesh input drops the bounds shell beside an
    /// interior column that is open to the sky (see [`MeshInput::from_world`]),
    /// so a change in a column one step in from the ring can show or hide
    /// shell cells in chunks far below it, not only in its neighbours. Only
    /// such a column, with shell under its own chunk, marks anything.
    ///
    /// [`MeshInput::from_world`]: crate::mesher::MeshInput::from_world
    fn mark_shell_below(&mut self, cell: CellPos) {
        let bounds = self.world.bounds();
        let c = cell.0;
        let floor = cell.chunk().origin_cell().0.y;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (rx, rz) = (c.x + dx, c.z + dz);
                if shell_interior(bounds, rx, rz) != Some((c.x, c.z)) {
                    continue;
                }
                let materials = self.world.materials();
                let shell_below = (bounds.min.0.y..floor).rev().any(|y| {
                    self.world
                        .get_cell(CellPos::new(rx, y, rz))
                        .is_some_and(|id| is_shell(materials, id))
                });
                if !shell_below {
                    continue;
                }
                // Every chunk column whose mesh input (with its one-cell
                // border) holds the ring column, below the changed cell.
                let top = cell.chunk().0.y;
                let bottom = CellPos::new(rx, bounds.min.0.y, rz).chunk().0.y;
                let mut columns = BTreeSet::new();
                for ez in -1..=1 {
                    for ex in -1..=1 {
                        let p = CellPos::new(rx + ex, c.y, rz + ez).chunk().0;
                        columns.insert((p.x, p.z));
                    }
                }
                for (x, z) in columns {
                    for y in bottom..top {
                        self.mark(ChunkPos::new(x, y, z));
                    }
                }
            }
        }
    }

    /// [`Replica::mark_shell_below`] for every column of a chunk whose every
    /// cell may have changed, from its top cell.
    fn mark_shell_below_chunk(&mut self, pos: ChunkPos) {
        let bounds = self.world.bounds();
        let origin = pos.origin_cell().0;
        let top = (origin.y + CHUNK_MASK).min(bounds.max.0.y);
        for z in origin.z..=origin.z + CHUNK_MASK {
            for x in origin.x..=origin.x + CHUNK_MASK {
                let cell = CellPos::new(x, top, z);
                if bounds.contains(cell) {
                    self.mark_shell_below(cell);
                }
            }
        }
    }

    /// Whether the chunk at `pos` is resident and its storage names a
    /// liquid-pass material.
    fn holds_liquid(&self, pos: ChunkPos) -> bool {
        let Some(chunk) = self.world.chunk(pos) else {
            return false;
        };
        let palette = match &chunk.storage {
            ChunkStorage::Uniform(id) => std::slice::from_ref(id),
            ChunkStorage::Paletted { palette, .. } => palette.as_slice(),
        };
        let materials = self.world.materials();
        palette
            .iter()
            .any(|&id| materials.get(id).is_some_and(is_liquid_pass))
    }

    /// After a layer change in `pos`: the fills of its liquid cells may have
    /// changed, which moves liquid faces in its own mesh and, through the
    /// one-cell border, in its neighbours' liquid meshes. Marks `pos` and
    /// each neighbour that holds a liquid, if `pos` holds one.
    fn mark_liquid(&mut self, pos: ChunkPos) {
        if !self.holds_liquid(pos) {
            return;
        }
        let p = pos.0;
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let n = ChunkPos::new(p.x + dx, p.y + dy, p.z + dz);
                    if self.holds_liquid(n) {
                        self.dirty.insert(n);
                    }
                }
            }
        }
    }

    /// After a layer change in `pos`: with [`Replica::layer_remesh`] on,
    /// `pos` and its neighbours; else [`Replica::mark_liquid`].
    fn mark_layers(&mut self, pos: ChunkPos) {
        if self.layer_remesh {
            self.mark_with_neighbours(pos);
        } else {
            self.mark_liquid(pos);
        }
    }

    /// Marks `pos` and all 26 neighbours, for a chunk whose every cell may
    /// have changed.
    fn mark_with_neighbours(&mut self, pos: ChunkPos) {
        let p = pos.0;
        for dy in -1..=1 {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    self.mark(ChunkPos::new(p.x + dx, p.y + dy, p.z + dz));
                }
            }
        }
    }
}

/// Keeps the [`Replica`] resource in step with the host. Needs
/// [`NetPlugin`](crate::net::NetPlugin) and [`ClientMaterials`].
pub struct ReplicaPlugin;

impl Plugin for ReplicaPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreUpdate, apply_host_messages.after(NetSystems::Receive));
    }
}

/// The `ServerMsg` handler: the only place the client changes cells.
pub(crate) fn apply_host_messages(
    mut commands: Commands,
    mut inbox: MessageReader<FromHost>,
    mut outbox: MessageWriter<ToHost>,
    materials: Option<Res<ClientMaterials>>,
    mut replica: Option<ResMut<Replica>>,
) {
    // A replica started by a Welcome in this batch. Later messages in the
    // batch apply to it; it replaces the resource when the system ends.
    let mut joined: Option<Replica> = None;
    let mut requests = Vec::new();
    for FromHost(msg) in inbox.read() {
        match msg {
            ServerMsg::Welcome { player, bounds, .. } => {
                let Some(materials) = &materials else {
                    warn!("no ClientMaterials resource; Welcome ignored");
                    continue;
                };
                info!(?player, "joined host");
                joined = Some(Replica::new(Arc::clone(&materials.0), *player, *bounds));
                continue;
            }
            ServerMsg::Rejected { reason } => {
                warn!(%reason, "host rejected Hello");
                continue;
            }
            _ => {}
        }
        let target = match (joined.as_mut(), replica.as_deref_mut()) {
            (Some(r), _) | (None, Some(r)) => r,
            (None, None) => {
                if matches!(msg, ServerMsg::ChunkSnapshot { .. } | ServerMsg::Applied(_)) {
                    warn!("world message before Welcome ignored");
                }
                continue;
            }
        };
        if let Err(e) = target.handle(msg, &mut requests) {
            warn!(%e, "host message not fully applied");
        }
        outbox.write_batch(
            requests
                .drain(..)
                .map(|pos| ToHost(ClientMsg::RequestChunk(pos))),
        );
    }
    if let Some(replica) = joined {
        commands.insert_resource(replica);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use rmc_world::coords::CellPos;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditResult};
    use rmc_world::layers::LayerId;
    use rmc_world::shape::Shape;

    use crate::net::{HostConnection, HostLink, NetPlugin};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    const PLAYER: PlayerId = PlayerId(3);

    /// A link whose host side is the test.
    #[derive(Clone, Default)]
    struct FakeLink {
        sent: Arc<Mutex<Vec<ClientMsg>>>,
        inbound: Arc<Mutex<Vec<ServerMsg>>>,
    }

    impl HostLink for FakeLink {
        fn send(&self, msg: ClientMsg) -> bool {
            self.sent.lock().unwrap().push(msg);
            true
        }

        fn drain(&self) -> Vec<ServerMsg> {
            std::mem::take(&mut *self.inbound.lock().unwrap())
        }
    }

    /// 3×3×3 chunks, cells (0,0,0)..=(95,95,95).
    fn bounds() -> CellAabb {
        CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(95, 95, 95))
    }

    fn c(x: i32, y: i32, z: i32) -> ChunkPos {
        ChunkPos::new(x, y, z)
    }

    fn set(xs: &[ChunkPos]) -> BTreeSet<ChunkPos> {
        xs.iter().copied().collect()
    }

    /// A host-side world (built here with `World::apply`, as the host does)
    /// and a client app with NetPlugin + ReplicaPlugin over a fake link.
    struct Fixture {
        app: App,
        link: FakeLink,
        host: World,
        seq: u32,
        brass: MaterialId,
    }

    impl Fixture {
        /// Joins with every chunk resident except `missing`, then takes the
        /// join's dirty set (which must be every resident chunk).
        fn new(missing: &[ChunkPos]) -> Self {
            let materials =
                Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
            let stone = materials.id("stone").unwrap();
            let brass = materials.id("brass_plate").unwrap();
            let mut host = World::new(Arc::clone(&materials), bounds());
            for pos in bounds().chunks() {
                if missing.contains(&pos) {
                    continue;
                }
                let fill = if pos.0.y == 0 { stone } else { MaterialId::AIR };
                host.insert_chunk(pos, Chunk::uniform(fill)).unwrap();
            }
            let link = FakeLink::default();
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, NetPlugin, ReplicaPlugin))
                .insert_resource(ClientMaterials(materials))
                .insert_resource(HostConnection::new(link.clone()));
            app.update();
            assert!(
                app.world().get_resource::<Replica>().is_none(),
                "no replica before Welcome"
            );
            link.sent.lock().unwrap().clear();
            let mut f = Self {
                app,
                link,
                host,
                seq: 0,
                brass,
            };
            let mut join = vec![ServerMsg::Welcome {
                player: PLAYER,
                seed: 1,
                bounds: bounds(),
                gen_params_hash: 2,
            }];
            join.extend(f.snapshots(f.host.iter_chunks_sorted().map(|(p, _)| p).collect()));
            f.deliver(join);
            assert_eq!(f.replica().player(), PLAYER);
            assert_eq!(f.replica().world().hash(), f.host.hash(), "joined world");
            let resident: BTreeSet<_> = f.host.iter_chunks_sorted().map(|(p, _)| p).collect();
            assert_eq!(f.take_dirty(), resident, "join marks every resident chunk");
            f.deliver(vec![]);
            assert!(f.take_dirty().is_empty(), "quiet frame marks nothing");
            f
        }

        fn snapshots(&self, positions: Vec<ChunkPos>) -> Vec<ServerMsg> {
            positions
                .into_iter()
                .map(|pos| {
                    let chunk = self.host.chunk(pos).expect("host chunk resident");
                    ServerMsg::chunk_snapshot(pos, chunk)
                })
                .collect()
        }

        /// Applies a brass `Set` over `shape` to the host world.
        fn edit(&mut self, shape: Shape) -> EditResult {
            self.seq += 1;
            self.host.apply(&EditCommand {
                id: CommandId {
                    issuer: PLAYER,
                    seq: self.seq,
                },
                issuer: PLAYER,
                op: EditOp::Set(self.brass),
                shape,
            })
        }

        fn edit_cell(&mut self, x: i32, y: i32, z: i32) -> EditResult {
            let r = self.edit(Shape::Cell(CellPos::new(x, y, z)));
            assert_eq!(r.deltas.len(), 1, "one chunk changed");
            r
        }

        /// Queues `msgs` on the link and runs one frame.
        fn deliver(&mut self, msgs: Vec<ServerMsg>) {
            self.link.inbound.lock().unwrap().extend(msgs);
            self.app.update();
        }

        fn replica(&self) -> &Replica {
            self.app.world().resource::<Replica>()
        }

        fn take_dirty(&mut self) -> BTreeSet<ChunkPos> {
            self.app.world_mut().resource_mut::<Replica>().take_dirty()
        }

        fn take_sent(&self) -> Vec<ClientMsg> {
            std::mem::take(&mut *self.link.sent.lock().unwrap())
        }

        fn assert_matches_host(&self, pos: ChunkPos) {
            let ours = self
                .replica()
                .world()
                .chunk(pos)
                .expect("replica chunk resident");
            let theirs = self.host.chunk(pos).expect("host chunk resident");
            assert_eq!(ours.revision, theirs.revision, "{pos:?} revision");
            assert_eq!(ours.hash(), theirs.hash(), "{pos:?} cells");
        }
    }

    #[test]
    fn delta_marks_chunk_dirty() {
        // World messages before Welcome are dropped, not applied later.
        {
            let mut app = App::new();
            let materials =
                Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
            let link = FakeLink::default();
            app.add_plugins((MinimalPlugins, NetPlugin, ReplicaPlugin))
                .insert_resource(ClientMaterials(materials))
                .insert_resource(HostConnection::new(link.clone()));
            link.inbound.lock().unwrap().extend([
                ServerMsg::Rejected {
                    reason: "material hash mismatch".into(),
                },
                ServerMsg::ChunkSnapshot {
                    pos: c(0, 0, 0),
                    revision: 0,
                    eco_revision: 0,
                    storage: ChunkStorage::Uniform(MaterialId::AIR),
                    layers: Default::default(),
                    plants: Default::default(),
                },
            ]);
            app.update();
            app.update();
            assert!(app.world().get_resource::<Replica>().is_none());
        }

        let mut f = Fixture::new(&[]);

        // One interior cell: only its own chunk.
        let r = f.edit_cell(32 + 5, 32 + 6, 32 + 7);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(1, 1, 1)]));
        assert_eq!(
            f.replica().world().get_cell(CellPos::new(37, 38, 39)),
            Some(f.brass)
        );
        assert_eq!(f.replica().world().chunk(c(1, 1, 1)).unwrap().revision, 1);
        f.assert_matches_host(c(1, 1, 1));

        // Several results in one frame; one chunk changed twice.
        let a = f.edit_cell(64 + 3, 4, 32 + 9);
        let b = f.edit_cell(10, 64 + 20, 64 + 30);
        let b2 = f.edit_cell(11, 64 + 20, 64 + 30);
        f.deliver(vec![a, b, b2].into_iter().map(ServerMsg::Applied).collect());
        assert_eq!(f.take_dirty(), set(&[c(2, 0, 1), c(0, 2, 2)]));
        assert_eq!(f.replica().world().chunk(c(0, 2, 2)).unwrap().revision, 2);
        f.assert_matches_host(c(2, 0, 1));
        f.assert_matches_host(c(0, 2, 2));

        // A Full delta may change any border cell: the chunk and every
        // resident neighbour (a corner chunk has 7).
        let full = f.edit(Shape::Box {
            min: CellPos::new(1, 1, 1),
            max: CellPos::new(30, 30, 30),
        });
        assert!(
            matches!(full.deltas[..], [ChunkDelta::Full { .. }]),
            "Full delta"
        );
        f.deliver(vec![ServerMsg::Applied(full)]);
        let corner: Vec<ChunkPos> = (0..8).map(|i| c(i & 1, (i >> 1) & 1, i >> 2)).collect();
        assert_eq!(f.take_dirty(), set(&corner));
        f.assert_matches_host(c(0, 0, 0));

        // A result that changed nothing marks nothing.
        let same = f.edit(Shape::Cell(CellPos::new(37, 38, 39)));
        assert!(same.deltas.is_empty());
        f.deliver(vec![ServerMsg::Applied(same)]);
        assert!(f.take_dirty().is_empty(), "no delta, nothing dirty");

        // A stale (already applied) delta is refused and marks nothing.
        let r = f.edit_cell(32 + 8, 32 + 8, 32 + 8);
        f.deliver(vec![ServerMsg::Applied(r.clone())]);
        assert_eq!(f.take_dirty(), set(&[c(1, 1, 1)]));
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert!(f.take_dirty().is_empty(), "stale delta marks nothing");
        f.assert_matches_host(c(1, 1, 1));

        // Messages that are not about cells mark nothing.
        f.deliver(vec![
            ServerMsg::Inventory { items: vec![] },
            ServerMsg::PlayerStates(vec![]),
            ServerMsg::Saved {
                ok: true,
                path: "x".into(),
            },
        ]);
        assert!(f.take_dirty().is_empty());

        // A repeated snapshot marks the chunk and its resident neighbours.
        let snap = f.snapshots(vec![c(2, 2, 2)]);
        f.deliver(snap);
        let far: Vec<ChunkPos> = (0..8)
            .map(|i| c(1 + (i & 1), 1 + ((i >> 1) & 1), 1 + (i >> 2)))
            .collect();
        assert_eq!(f.take_dirty(), set(&far));

        assert!(f.take_sent().is_empty(), "no requests without a gap");
        assert_eq!(f.replica().world().hash(), f.host.hash());
    }

    /// A local cell and the neighbour offsets that must be marked with it.
    type BorderCase = ((i32, i32, i32), &'static [(i32, i32, i32)]);

    #[test]
    fn shell_column_change_marks_chunks_below() {
        let mut f = Fixture::new(&[]);
        // A bedrock wall along x = 0, the full height, as worldgen makes
        // the shell.
        let brass = f.brass;
        f.brass = f.replica().world().materials().id("bedrock").unwrap();
        let wall = f.edit(Shape::Box {
            min: CellPos::new(0, 0, 0),
            max: CellPos::new(0, 95, 95),
        });
        f.brass = brass;
        assert!(!wall.deltas.is_empty(), "the wall is built");
        f.deliver(vec![ServerMsg::Applied(wall)]);
        f.take_dirty();

        // One step in from the wall, high up: the wall's chunks below too,
        // since the mesh may now show or hide the shell there.
        let r = f.edit_cell(1, 64 + 20, 32 + 5);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(0, 2, 1), c(0, 1, 1), c(0, 0, 1)]));
        // In the first column of a chunk, the chunks below in the column
        // before it too: their mesh border holds the wall beside it.
        let r = f.edit_cell(1, 64 + 20, 32);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(
            f.take_dirty(),
            set(&[
                c(0, 2, 1),
                c(0, 2, 0),
                c(0, 1, 1),
                c(0, 0, 1),
                c(0, 1, 0),
                c(0, 0, 0)
            ])
        );
        // Two steps in, or on the wall itself: only the cell's own chunk.
        let r = f.edit_cell(2, 64 + 20, 32 + 5);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(0, 2, 1)]));
        let r = f.edit_cell(0, 64 + 20, 32 + 6);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(0, 2, 1)]));
        // Next to the -Z edge, where no shell stands (stone is diggable):
        // only its own chunk.
        let r = f.edit_cell(10, 64 + 20, 1);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(0, 2, 0)]));

        // A snapshot of a chunk beside the wall marks the wall's chunks
        // below it as well as its neighbours: in its own column, and in the
        // columns on either side, whose borders hold its first and last
        // rows of wall. Chunks away from the wall stay clean.
        let snapshot = f.snapshots(vec![c(0, 2, 1)]);
        f.deliver(snapshot);
        let dirty = f.take_dirty();
        for z in 0..3 {
            assert!(dirty.contains(&c(0, 0, z)), "{z}: {dirty:?}");
        }
        assert!(!dirty.contains(&c(1, 0, 1)), "{dirty:?}");
    }

    #[test]
    fn layer_change_marks_chunks_with_liquid() {
        let mut f = Fixture::new(&[]);
        let water = f.host.materials().id("water").unwrap();
        let layers_of = |f: &Fixture, pos: ChunkPos| {
            let chunk = f.replica().world().chunk(pos).unwrap();
            (chunk.eco_revision, chunk.layers.clone())
        };
        let snapshot = |pos: ChunkPos, eco_revision: u64, layers| ServerMsg::LayerSnapshot {
            pos,
            eco_revision,
            layers,
            plants: Default::default(),
        };

        // No liquid anywhere: a layer snapshot marks nothing.
        let (rev, layers) = layers_of(&f, c(1, 1, 1));
        f.deliver(vec![snapshot(c(1, 1, 1), rev + 1, layers)]);
        assert!(f.take_dirty().is_empty(), "layers alone change no mesh");

        // Water in (1,1,1) and in two of its neighbours, none in the rest.
        for cell in [
            CellPos::new(40, 40, 40),
            CellPos::new(70, 40, 40),
            CellPos::new(40, 10, 40),
        ] {
            f.seq += 1;
            let r = f.host.apply(&EditCommand {
                id: CommandId {
                    issuer: PLAYER,
                    seq: f.seq,
                },
                issuer: PLAYER,
                op: EditOp::Set(water),
                shape: Shape::Cell(cell),
            });
            f.deliver(vec![ServerMsg::Applied(r)]);
        }
        f.take_dirty();

        // A layer snapshot of a chunk with water: it and its neighbours
        // with water, whose border holds its fills.
        let (rev, mut layers) = layers_of(&f, c(1, 1, 1));
        layers.set_u8(LayerId::Moisture, LocalPos::new(8, 8, 8), 99);
        f.deliver(vec![snapshot(c(1, 1, 1), rev + 1, layers)]);
        assert_eq!(f.take_dirty(), set(&[c(1, 1, 1), c(2, 1, 1), c(1, 0, 1)]));
        // Of a chunk without water, next to water: nothing.
        let (rev, layers) = layers_of(&f, c(0, 1, 1));
        f.deliver(vec![snapshot(c(0, 1, 1), rev + 1, layers)]);
        assert!(f.take_dirty().is_empty(), "no liquid in the chunk");
        // While waiting for a chunk's full snapshot, its layers are ignored.
        f.app
            .world_mut()
            .resource_mut::<Replica>()
            .awaiting
            .insert(c(2, 1, 1));
        let (rev, layers) = layers_of(&f, c(2, 1, 1));
        f.deliver(vec![snapshot(c(2, 1, 1), rev + 1, layers)]);
        assert!(f.take_dirty().is_empty(), "awaiting: ignored");
        f.app.world_mut().resource_mut::<Replica>().awaiting.clear();

        // A Layers delta does the same.
        let (rev, _) = layers_of(&f, c(1, 0, 1));
        let mut r = f.edit(Shape::Cell(CellPos::new(40, 40, 40)));
        r.deltas = vec![ChunkDelta::Layers {
            pos: c(1, 0, 1),
            eco_revision: rev + 1,
            changes: vec![(LocalPos::new(8, 10, 8).index(), LayerId::Moisture, 200)],
            plants: vec![],
        }];
        f.deliver(vec![ServerMsg::Applied(r)]);
        // (2,1,1) is (1,0,1)'s edge neighbour.
        assert_eq!(f.take_dirty(), set(&[c(1, 0, 1), c(1, 1, 1), c(2, 1, 1)]));
        let chunk = f.replica().world().chunk(c(1, 0, 1)).unwrap();
        assert_eq!(
            chunk
                .layers
                .get_u8(LayerId::Moisture, LocalPos::new(8, 10, 8)),
            200
        );
    }

    #[test]
    fn border_edit_marks_neighbor_dirty() {
        let mut f = Fixture::new(&[]);
        // Cells of the centre chunk (1,1,1), by local position, and the
        // neighbour offsets whose 34³ mesh volume holds them.
        #[rustfmt::skip]
        let cases: [BorderCase; 11] = [
            ((10, 10, 10), &[]),
            ((1, 30, 1), &[]),
            ((0, 10, 10), &[(-1, 0, 0)]),
            ((31, 10, 10), &[(1, 0, 0)]),
            ((10, 0, 10), &[(0, -1, 0)]),
            ((10, 31, 10), &[(0, 1, 0)]),
            ((10, 10, 0), &[(0, 0, -1)]),
            ((10, 10, 31), &[(0, 0, 1)]),
            ((0, 31, 10), &[(-1, 0, 0), (0, 1, 0), (-1, 1, 0)]),
            ((10, 0, 0), &[(0, -1, 0), (0, 0, -1), (0, -1, -1)]),
            ((31, 0, 31), &[(1, 0, 0), (0, -1, 0), (0, 0, 1), (1, -1, 0), (1, 0, 1), (0, -1, 1), (1, -1, 1)]),
        ];
        for ((x, y, z), offsets) in cases {
            let r = f.edit_cell(32 + x, 32 + y, 32 + z);
            f.deliver(vec![ServerMsg::Applied(r)]);
            let mut want = set(&[c(1, 1, 1)]);
            want.extend(
                offsets
                    .iter()
                    .map(|&(dx, dy, dz)| c(1 + dx, 1 + dy, 1 + dz)),
            );
            let got = f.take_dirty();
            assert_eq!(got, want, "local ({x},{y},{z})");
            // SAD §11.1: the face-adjacent neighbour across each border.
            for (coord, axis) in [(x, 0), (y, 1), (z, 2)] {
                let mut n = [1, 1, 1];
                match coord {
                    0 => n[axis] = 0,
                    31 => n[axis] = 2,
                    _ => continue,
                }
                assert!(
                    got.contains(&c(n[0], n[1], n[2])),
                    "face neighbour of ({x},{y},{z})"
                );
            }
        }

        // An edit over a border: both chunks, and both sides' neighbours.
        let r = f.edit(Shape::Box {
            min: CellPos::new(31, 63, 40),
            max: CellPos::new(32, 63, 40),
        });
        assert_eq!(r.deltas.len(), 2);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(
            f.take_dirty(),
            set(&[c(0, 1, 1), c(1, 1, 1), c(0, 2, 1), c(1, 2, 1)])
        );

        // At the edge of the world there is no neighbour to mark.
        let r = f.edit_cell(0, 5, 5);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(0, 0, 0)]));
        let r = f.edit_cell(95, 95, 95);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(2, 2, 2)]));
        assert_eq!(f.replica().world().hash(), f.host.hash());

        // A neighbour that is not resident is not marked; it is marked (with
        // its resident neighbours) when its snapshot arrives.
        let missing = c(2, 1, 1);
        let mut f = Fixture::new(&[missing]);
        let r = f.edit_cell(32 + 31, 32 + 10, 32 + 10);
        f.deliver(vec![ServerMsg::Applied(r)]);
        assert_eq!(f.take_dirty(), set(&[c(1, 1, 1)]));
        f.host
            .insert_chunk(missing, Chunk::uniform(MaterialId::AIR))
            .unwrap();
        let snap = f.snapshots(vec![missing]);
        f.deliver(snap);
        let mut want = BTreeSet::new();
        for dy in 0..=2 {
            for dz in 0..=2 {
                for dx in 1..=2 {
                    want.insert(c(dx, dy, dz));
                }
            }
        }
        assert_eq!(f.take_dirty(), want);
        assert_eq!(f.replica().world().hash(), f.host.hash());
    }

    #[test]
    fn gap_sends_request_chunk() {
        let mut f = Fixture::new(&[]);
        let p = c(1, 0, 1);
        let q = c(2, 0, 1);

        // The result of one edit to P is lost; the next one has a gap.
        let _lost = f.edit_cell(32 + 4, 4, 32 + 4);
        let gap = f.edit_cell(32 + 5, 5, 32 + 5);
        let fine = f.edit_cell(64 + 5, 5, 32 + 5);
        let before = f.replica().world().chunk(p).unwrap().hash();
        f.deliver(vec![ServerMsg::Applied(gap), ServerMsg::Applied(fine)]);
        assert_eq!(
            f.take_sent(),
            vec![ClientMsg::RequestChunk(p)],
            "RequestChunk sent in the frame the gap arrived"
        );
        assert!(f.replica().is_awaiting(p));
        assert!(!f.replica().is_awaiting(q));
        let chunk = f.replica().world().chunk(p).unwrap();
        assert_eq!(
            (chunk.revision, chunk.hash()),
            (0, before),
            "gap delta not applied"
        );
        assert_eq!(f.take_dirty(), set(&[q]), "the gap chunk is not dirty");
        f.assert_matches_host(q);

        // Further deltas for P are ignored without asking again.
        let ignored = f.edit_cell(32 + 6, 6, 32 + 6);
        f.deliver(vec![ServerMsg::Applied(ignored)]);
        assert!(f.take_sent().is_empty(), "one request per gap");
        assert!(f.replica().is_awaiting(p));
        assert_eq!(f.replica().world().chunk(p).unwrap().revision, 0);
        assert!(f.take_dirty().is_empty());

        // The host's snapshot ends the wait and holds every lost change.
        let snap = f.snapshots(vec![p]);
        f.deliver(snap);
        assert!(!f.replica().is_awaiting(p));
        f.assert_matches_host(p);
        assert!(f.take_dirty().contains(&p));

        // Deltas after the snapshot apply on top.
        let after = f.edit_cell(32 + 7, 7, 32 + 7);
        f.deliver(vec![ServerMsg::Applied(after)]);
        assert_eq!(f.take_dirty(), set(&[p]));
        f.assert_matches_host(p);
        assert!(f.take_sent().is_empty());

        // Gaps in two chunks of one result: one request each, in delta order.
        let across = |y| Shape::Box {
            min: CellPos::new(63, y, 40),
            max: CellPos::new(64, y, 40),
        };
        let _lost = f.edit(across(10));
        let gap2 = f.edit(across(11));
        assert_eq!(gap2.deltas.len(), 2);
        f.deliver(vec![ServerMsg::Applied(gap2)]);
        assert_eq!(
            f.take_sent(),
            vec![ClientMsg::RequestChunk(p), ClientMsg::RequestChunk(q)]
        );
        assert!(f.take_dirty().is_empty());
        let snap = f.snapshots(vec![p, q]);
        f.deliver(snap);
        assert!(!f.replica().is_awaiting(p) && !f.replica().is_awaiting(q));
        assert_eq!(f.replica().world().hash(), f.host.hash());
        assert!(f.take_sent().is_empty());
    }
}
