//! A Bevy-free client replica (SAD §8.5 replica rule, §11.1).
//!
//! [`Replica`] is the client's copy of the world, built and changed only by
//! the [`ServerMsg`]s it is given: `Welcome` creates an empty world with the
//! host's bounds, `ChunkSnapshot` makes a chunk resident through
//! [`World::insert_chunk`], and `Applied` writes each delta through
//! [`World::apply_delta`]. Nothing else touches its cells, so a replica that
//! saw every message the host sent to its player holds the host's world.
//!
//! If a message was lost, the next delta for an affected chunk has a revision
//! more than one ahead of the replica's. The replica then queues a
//! `RequestChunk` for that chunk (collected with [`Replica::take_requests`])
//! and ignores the chunk's deltas until its snapshot arrives. The host
//! handles messages in order, so that snapshot already holds every change
//! the ignored deltas carried, and the deltas after it apply on top.

use std::collections::BTreeSet;
use std::sync::Arc;

use rmc_protocol::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use rmc_world::chunk::Chunk;
use rmc_world::chunk::ChunkStorage;
use rmc_world::coords::ChunkPos;
use rmc_world::edit::PlayerId;
use rmc_world::inventory::Inventory;
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::world::{DeltaError, World};
use tracing::debug;

/// Why [`Replica::handle`] could not use a message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplicaError {
    /// A world message arrived before `Welcome`, so there is no world for it.
    #[error("{0} arrived before Welcome")]
    NotJoined(&'static str),
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
}

/// The client-side world, fed only by [`ServerMsg`]s.
#[derive(Clone, Debug)]
pub struct Replica {
    materials: Arc<MaterialTable>,
    player: Option<PlayerId>,
    world: Option<World>,
    rejected: Option<String>,
    /// Chunks with a revision gap whose snapshot has not arrived yet.
    awaiting: BTreeSet<ChunkPos>,
    /// Messages for the host that [`Replica::take_requests`] has not taken.
    requests: Vec<ClientMsg>,
    inventory: Inventory,
}

impl Replica {
    /// A replica that has not joined yet, using the client's material table.
    pub fn new(materials: Arc<MaterialTable>) -> Self {
        Self {
            materials,
            player: None,
            world: None,
            rejected: None,
            awaiting: BTreeSet::new(),
            requests: Vec::new(),
            inventory: Inventory::new(),
        }
    }

    /// The `Hello` this replica's client sends to join.
    pub fn hello(&self) -> ClientMsg {
        ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            material_hash: self.materials.hash(),
        }
    }

    /// The id the host assigned in `Welcome`.
    pub fn player(&self) -> Option<PlayerId> {
        self.player
    }

    /// The replica world, once `Welcome` has arrived.
    pub fn world(&self) -> Option<&World> {
        self.world.as_ref()
    }

    /// The reason of the last `Rejected`, until a `Welcome` arrives.
    pub fn rejected(&self) -> Option<&str> {
        self.rejected.as_deref()
    }

    /// Takes the messages the replica needs sent to the host, in the order
    /// they arose: one `RequestChunk` per chunk with a revision gap.
    pub fn take_requests(&mut self) -> Vec<ClientMsg> {
        std::mem::take(&mut self.requests)
    }

    /// Whether the replica is waiting for a snapshot of `pos` after a
    /// revision gap. Deltas for that chunk are ignored until it arrives.
    pub fn is_awaiting(&self, pos: ChunkPos) -> bool {
        self.awaiting.contains(&pos)
    }

    /// The player's materials, as of the last `Inventory` message (empty
    /// before the first one and after a `Welcome`).
    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    /// Applies one message from the host.
    ///
    /// - `Welcome` starts a fresh, empty world with the host's bounds (a
    ///   repeated `Welcome` starts over) and records the player id.
    /// - `Rejected` records the reason if the replica has not joined, and is
    ///   ignored otherwise (the host only sends it in reply to `Hello`).
    /// - `ChunkSnapshot` makes the chunk resident with the host's storage and
    ///   revision, replacing any copy already held. Its storage must be well
    ///   formed and every palette entry must be in the material table.
    ///   A snapshot of a chunk the replica was waiting for ends the wait.
    /// - `Applied` applies each delta in order. A rejected result has none.
    ///   A delta for a chunk the replica is waiting for is ignored. A delta
    ///   more than one revision ahead of the chunk is a gap: the replica
    ///   queues a `RequestChunk` and waits for that chunk's snapshot, and
    ///   this is not an error. Other deltas the world refuses (including
    ///   stale ones, at or below the chunk's revision) are reported together
    ///   after the others have been applied; a refused delta changes nothing.
    /// - `Inventory` replaces the replica's inventory.
    /// - `PlayerStates` and `Saved` do not touch the replica.
    pub fn handle(&mut self, msg: ServerMsg) -> Result<(), ReplicaError> {
        match msg {
            ServerMsg::Welcome { player, bounds, .. } => {
                self.player = Some(player);
                self.world = Some(World::new(Arc::clone(&self.materials), bounds));
                self.rejected = None;
                self.awaiting.clear();
                self.requests.clear();
                self.inventory = Inventory::new();
            }
            ServerMsg::Rejected { reason } => {
                if self.world.is_none() {
                    self.rejected = Some(reason);
                }
            }
            ServerMsg::ChunkSnapshot {
                pos,
                revision,
                storage,
            } => {
                let world = self
                    .world
                    .as_mut()
                    .ok_or(ReplicaError::NotJoined("ChunkSnapshot"))?;
                // insert_chunk trusts its storage; a malformed one would
                // panic later in get or compact.
                if !storage.is_well_formed() {
                    return Err(ReplicaError::MalformedSnapshot(pos));
                }
                let palette = match &storage {
                    ChunkStorage::Uniform(id) => std::slice::from_ref(id),
                    ChunkStorage::Paletted { palette, .. } => palette.as_slice(),
                };
                if let Some(&id) = palette.iter().find(|&&id| self.materials.get(id).is_none()) {
                    return Err(ReplicaError::UnknownSnapshotMaterial { pos, id });
                }
                let mut chunk = Chunk::new(storage);
                chunk.revision = revision;
                world
                    .insert_chunk(pos, chunk)
                    .map_err(|_| ReplicaError::SnapshotOutOfBounds(pos))?;
                self.awaiting.remove(&pos);
            }
            ServerMsg::Applied(result) => {
                if result.deltas.is_empty() {
                    return Ok(());
                }
                let world = self
                    .world
                    .as_mut()
                    .ok_or(ReplicaError::NotJoined("Applied"))?;
                let mut refused = Vec::new();
                for delta in &result.deltas {
                    let pos = delta.pos();
                    if self.awaiting.contains(&pos) {
                        continue;
                    }
                    match world.apply_delta(delta) {
                        Ok(()) => {}
                        Err(DeltaError::RevisionGap { local, got, .. }) if got > local => {
                            debug!(?pos, local, got, "revision gap, requesting chunk");
                            self.awaiting.insert(pos);
                            self.requests.push(ClientMsg::RequestChunk(pos));
                        }
                        Err(e) => refused.push(e),
                    }
                }
                if !refused.is_empty() {
                    return Err(ReplicaError::Deltas(refused));
                }
            }
            ServerMsg::Inventory { items } => {
                self.inventory = Inventory::from_items(&items);
            }
            ServerMsg::PlayerStates(_) | ServerMsg::Saved { .. } => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use glam::IVec3;
    use rmc_protocol::{decode_server, encode_server};
    use rmc_world::coords::{CellAabb, CellPos};
    use rmc_world::edit::{
        ChunkDelta, CommandId, EditCommand, EditOp, EditStatus, MAX_EDIT_CELLS, RejectReason,
    };
    use rmc_world::shape::Shape;

    use crate::host::{Host, HostConfig};
    use crate::transport::{ChannelTransport, ClientEndpoint};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    const CONFIG: HostConfig = HostConfig {
        seed: 0x5EED_0033,
        gen_params_hash: 0xF033_F033,
    };

    /// Edits sent in the equivalence test.
    const EDITS: u32 = 10_000;
    /// The third client joins after this many edits.
    const LATE_JOIN_AT: u32 = 5_000;
    /// Full world hashes are compared every this many edits.
    const CHECKPOINT_EVERY: u32 = 500;

    /// Chunks inside the bounds that are never resident.
    const MISSING: [ChunkPos; 2] = [ChunkPos::new(1, 1, 1), ChunkPos::new(-2, 0, 0)];

    /// Damage hot spot: a 6³ box across the x = 0 and z = 32 chunk borders,
    /// hit often enough that cells break.
    const HOT_MIN: CellPos = CellPos::new(-3, 3, 29);
    const HOT_MAX: CellPos = CellPos::new(2, 8, 34);

    fn materials() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"))
    }

    fn mat(world: &World, key: &str) -> MaterialId {
        world.materials().id(key).expect("material in table")
    }

    fn bounds() -> CellAabb {
        // 4 × 2 × 4 chunks, negative x and z included.
        CellAabb::new(CellPos::new(-64, 0, -64), CellPos::new(63, 63, 63))
    }

    /// SplitMix64: a fixed-seed generator so the test is reproducible.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        /// Uniform in `lo..=hi`.
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + self.below((hi - lo + 1) as u64) as i32
        }

        fn cell_in(&mut self, min: CellPos, max: CellPos) -> CellPos {
            CellPos::new(
                self.range(min.0.x, max.0.x),
                self.range(min.0.y, max.0.y),
                self.range(min.0.z, max.0.z),
            )
        }

        /// A cell in the bounds or up to two cells outside them.
        fn cell_near_bounds(&mut self) -> CellPos {
            let b = bounds();
            self.cell_in(
                CellPos(b.min.0 - IVec3::splat(2)),
                CellPos(b.max.0 + IVec3::splat(2)),
            )
        }
    }

    /// The host's starting world: a bedrock floor under stone, dirt and air
    /// chunks, some paletted chunks with revisions above zero, and two
    /// in-bounds chunks left non-resident.
    fn initial_world() -> World {
        let mut world = World::new(materials(), bounds());
        let stone = mat(&world, "stone");
        let dirt = mat(&world, "dirt");
        for pos in bounds().chunks() {
            if MISSING.contains(&pos) {
                continue;
            }
            let fill = match pos.0.y {
                0 => stone,
                _ if pos.0.x == 0 => dirt,
                _ => MaterialId::AIR,
            };
            world.insert_chunk(pos, Chunk::uniform(fill)).unwrap();
        }
        let setup = [
            (
                EditOp::Set(mat(&world, "bedrock")),
                Shape::Box {
                    min: CellPos::new(-64, 0, -64),
                    max: CellPos::new(63, 1, 63),
                },
            ),
            (
                EditOp::Set(mat(&world, "granite")),
                Shape::Sphere {
                    center: CellPos::new(-20, 20, 10),
                    radius: 9,
                },
            ),
            (
                EditOp::Set(mat(&world, "copper_ore")),
                Shape::Box {
                    min: CellPos::new(32, 2, 32),
                    max: CellPos::new(63, 9, 63),
                },
            ),
            (
                EditOp::Set(mat(&world, "glowcrystal")),
                Shape::Cell(CellPos::new(-1, 40, -1)),
            ),
        ];
        for (seq, (op, shape)) in setup.into_iter().enumerate() {
            let result = world.apply(&EditCommand {
                id: CommandId {
                    issuer: PlayerId(0),
                    seq: seq as u32,
                },
                issuer: PlayerId(0),
                op,
                shape,
            });
            assert_eq!(result.status, EditStatus::Applied);
            assert!(!result.deltas.is_empty());
        }
        world
    }

    /// A random edit. Mostly small edits anywhere near the bounds, plus big
    /// boxes and spheres (which produce `Full` deltas), damage aimed at a hot
    /// spot (which breaks cells), and commands the host must reject.
    fn random_edit(rng: &mut Rng, ids: &[MaterialId], unknown: MaterialId) -> (EditOp, Shape) {
        let pick = |rng: &mut Rng| ids[rng.below(ids.len() as u64) as usize];
        let op = match rng.below(100) {
            0..35 => EditOp::Set(pick(rng)),
            35..50 => EditOp::Remove,
            50..85 => EditOp::Damage(match rng.below(4) {
                0 => 1 + rng.below(9) as u16,
                1 => 10 + rng.below(50) as u16,
                2 => u16::MAX,
                _ => 30,
            }),
            85..99 => EditOp::Paint(pick(rng)),
            _ => EditOp::Set(unknown),
        };
        if matches!(op, EditOp::Damage(_)) && rng.below(3) == 0 {
            return (op, Shape::Cell(rng.cell_in(HOT_MIN, HOT_MAX)));
        }
        let shape = match rng.below(100) {
            0..45 => Shape::Cell(rng.cell_near_bounds()),
            45..70 => {
                let min = rng.cell_near_bounds();
                let ext = IVec3::new(rng.range(0, 6), rng.range(0, 6), rng.range(0, 6));
                Shape::Box {
                    min,
                    max: CellPos(min.0 + ext),
                }
            }
            70..85 => Shape::Sphere {
                center: rng.cell_near_bounds(),
                radius: rng.below(9) as u16,
            },
            85..91 => {
                // Big box, chunk-aligned a third of the time.
                let mut min = rng.cell_near_bounds();
                if rng.below(3) == 0 {
                    min = min.chunk().origin_cell();
                }
                let ext = IVec3::new(rng.range(8, 31), rng.range(4, 31), rng.range(8, 31));
                Shape::Box {
                    min,
                    max: CellPos(min.0 + ext),
                }
            }
            // Radius 20 and up exceeds MAX_EDIT_CELLS and is rejected.
            91..95 => Shape::Sphere {
                center: rng.cell_near_bounds(),
                radius: rng.range(9, 22) as u16,
            },
            95..97 => {
                // 33³ cells: too many.
                let min = rng.cell_near_bounds();
                Shape::Box {
                    min,
                    max: CellPos(min.0 + IVec3::splat(32)),
                }
            }
            97..99 => {
                // Entirely above the world.
                let c = rng.cell_near_bounds();
                let min = CellPos::new(c.0.x, 70 + rng.range(0, 20), c.0.z);
                Shape::Box {
                    min,
                    max: CellPos(min.0 + IVec3::splat(3)),
                }
            }
            _ => Shape::Box {
                min: HOT_MIN,
                max: HOT_MAX,
            },
        };
        (op, shape)
    }

    /// What the issuers saw: one entry per command, counted when its result
    /// reaches the player that sent it.
    #[derive(Debug, Default)]
    struct Stats {
        applied: u32,
        no_change: u32,
        breaking: u32,
        cells_deltas: u32,
        full_deltas: u32,
        rejected: BTreeMap<&'static str, u32>,
    }

    impl Stats {
        fn record(
            &mut self,
            status: &EditStatus,
            deltas: &[ChunkDelta],
            yields: &[(MaterialId, u32)],
        ) {
            match status {
                EditStatus::Applied => {
                    self.applied += 1;
                    if deltas.is_empty() {
                        self.no_change += 1;
                    }
                }
                EditStatus::Rejected(reason) => {
                    let key = match reason {
                        RejectReason::UnknownMaterial(_) => "unknown_material",
                        RejectReason::TooManyCells => "too_many_cells",
                        RejectReason::OutsideBounds => "outside_bounds",
                    };
                    *self.rejected.entry(key).or_default() += 1;
                }
            }
            if !yields.is_empty() {
                self.breaking += 1;
            }
            for delta in deltas {
                match delta {
                    ChunkDelta::Cells { .. } => self.cells_deltas += 1,
                    ChunkDelta::Full { .. } => self.full_deltas += 1,
                }
            }
        }
    }

    /// A connected client: its endpoint, its replica and what it has sent.
    struct Client {
        ep: ClientEndpoint,
        replica: Replica,
        /// Messages go through the postcard codec, as over a network.
        wire: bool,
        joined: bool,
        next_seq: u32,
        /// Sequence numbers of its own commands whose results arrived, in order.
        results: Vec<u32>,
    }

    impl Client {
        fn new(ep: ClientEndpoint, wire: bool) -> Self {
            Self {
                ep,
                replica: Replica::new(materials()),
                wire,
                joined: false,
                next_seq: 0,
                results: Vec::new(),
            }
        }

        fn join(&mut self) {
            self.ep.send(self.replica.hello()).unwrap();
            self.joined = true;
        }

        fn send_edit(&mut self, op: EditOp, shape: Shape) {
            let me = self.ep.player();
            let cmd = EditCommand {
                id: CommandId {
                    issuer: me,
                    seq: self.next_seq,
                },
                issuer: me,
                op,
                shape,
            };
            self.next_seq += 1;
            self.ep.send(ClientMsg::Edit(cmd)).unwrap();
        }

        /// Feeds everything the host sent to the replica, and only that.
        fn pump(&mut self, stats: &mut Stats) {
            let me = self.ep.player();
            for msg in self.ep.drain() {
                let msg = if self.wire {
                    decode_server(&encode_server(&msg).expect("encodes")).expect("decodes")
                } else {
                    msg
                };
                if let ServerMsg::Applied(result) = &msg {
                    if result.command.issuer == me {
                        self.results.push(result.command.seq);
                        stats.record(&result.status, &result.deltas, &result.yields);
                    } else {
                        assert_eq!(
                            result.status,
                            EditStatus::Applied,
                            "{me:?} got someone else's rejection"
                        );
                    }
                }
                if let Err(e) = self.replica.handle(msg) {
                    panic!("{me:?}: replica refused a host message: {e}");
                }
            }
        }

        fn world(&self) -> &World {
            self.replica
                .world()
                .unwrap_or_else(|| panic!("{:?} has no world", self.ep.player()))
        }

        /// Drains what the host sent and feeds the replica every message for
        /// which `keep` is true; the others are lost. Returns all the drained
        /// messages, lost ones included.
        fn pump_lossy(&mut self, mut keep: impl FnMut(&ServerMsg) -> bool) -> Vec<ServerMsg> {
            let me = self.ep.player();
            let msgs = self.ep.drain();
            for msg in &msgs {
                if !keep(msg) {
                    continue;
                }
                let msg = if self.wire {
                    decode_server(&encode_server(msg).expect("encodes")).expect("decodes")
                } else {
                    msg.clone()
                };
                if let Err(e) = self.replica.handle(msg) {
                    panic!("{me:?}: replica refused a host message: {e}");
                }
            }
            msgs
        }

        /// Sends the replica's pending requests to the host and returns them.
        fn send_requests(&mut self) -> Vec<ClientMsg> {
            let requests = self.replica.take_requests();
            for msg in &requests {
                self.ep.send(msg.clone()).unwrap();
            }
            requests
        }
    }

    /// Every resident chunk has the host's revision, and nothing else is resident.
    fn assert_revisions(host: &World, client: &Client) {
        let replica = client.world();
        assert_eq!(replica.chunk_count(), host.chunk_count());
        for (pos, chunk) in host.iter_chunks_sorted() {
            let got = replica.chunk(pos).unwrap_or_else(|| {
                panic!("{:?} lacks chunk {pos:?}", client.ep.player());
            });
            assert_eq!(got.revision, chunk.revision, "revision of {pos:?}");
        }
    }

    fn assert_identical(host: &World, client: &Client) {
        assert_revisions(host, client);
        let replica = client.world();
        assert_eq!(replica.bounds(), host.bounds());
        for (pos, chunk) in host.iter_chunks_sorted() {
            assert_eq!(
                replica.chunk(pos).unwrap().hash(),
                chunk.hash(),
                "{:?} chunk {pos:?}",
                client.ep.player()
            );
        }
        assert_eq!(replica.hash(), host.hash());
    }

    #[test]
    fn equivalence_10k_edits() {
        let world = initial_world();
        let start_hash = world.hash();
        let table = Arc::clone(world.materials());
        let ids: Vec<MaterialId> = table.iter().map(|d| d.id).collect();
        let unknown = MaterialId(1000);
        assert!(table.get(unknown).is_none());
        assert_eq!(MAX_EDIT_CELLS, 32_768);

        let mut transport = ChannelTransport::new();
        // A and C take messages typed, B through the wire codec. C joins late.
        let mut clients: Vec<Client> = [false, true, false]
            .into_iter()
            .map(|wire| Client::new(transport.connect(), wire))
            .collect();
        let mut host = Host::new(world, CONFIG, Box::new(transport));
        clients[0].join();
        clients[1].join();

        let mut rng = Rng(0x0F03_3000_1234_5678);
        let mut stats = Stats::default();
        let mut sent = 0;
        let mut ticks = 0;
        let mut late_snapshot_revision = 0;
        while sent < EDITS {
            let batch = (1 + rng.below(16) as u32).min(EDITS - sent);
            for _ in 0..batch {
                if sent == LATE_JOIN_AT {
                    clients[2].join();
                }
                let joined: Vec<usize> =
                    (0..clients.len()).filter(|&i| clients[i].joined).collect();
                let who = joined[rng.below(joined.len() as u64) as usize];
                let (op, shape) = random_edit(&mut rng, &ids, unknown);
                clients[who].send_edit(op, shape);
                sent += 1;
                if rng.below(10) == 0 {
                    // Ignored by a single-player host; must not disturb anything.
                    clients[who]
                        .ep
                        .send(ClientMsg::PlayerState {
                            pos: glam::Vec3::new(1.0, 2.0, 3.0),
                            yaw: 0.5,
                            pitch: -0.25,
                        })
                        .unwrap();
                }
            }
            host.tick();
            ticks += 1;
            for c in clients.iter_mut().filter(|c| c.joined) {
                c.pump(&mut stats);
            }
            if late_snapshot_revision == 0 && clients[2].joined {
                late_snapshot_revision = clients[2]
                    .world()
                    .iter_chunks_sorted()
                    .map(|(_, c)| c.revision)
                    .max()
                    .unwrap();
            }
            for c in clients.iter().filter(|c| c.joined) {
                assert_revisions(host.world(), c);
            }
            if sent % CHECKPOINT_EVERY < batch {
                for c in clients.iter().filter(|c| c.joined) {
                    assert_eq!(c.world().hash(), host.world().hash(), "after {sent} edits");
                }
            }
        }
        // One more tick with nothing new changes nothing.
        host.tick();
        for c in clients.iter_mut() {
            c.pump(&mut stats);
        }

        for c in &clients {
            assert_identical(host.world(), c);
            assert_eq!(c.replica.player(), Some(c.ep.player()));
            assert!(c.replica.rejected().is_none());
            // Every command got exactly one result, back to its issuer, in order.
            assert_eq!(c.results, (0..c.next_seq).collect::<Vec<_>>());
            // A replica holds no damage; that lives on the host.
            assert!(c.world().damage().is_empty());
        }
        assert_ne!(host.world().hash(), start_hash);
        assert!(!host.world().damage().is_empty());
        let total: u32 = clients.iter().map(|c| c.next_seq).sum();
        assert_eq!(total, EDITS);

        println!("{ticks} ticks, {stats:?}, late join saw revision {late_snapshot_revision}");
        let rejected: u32 = stats.rejected.values().sum();
        assert_eq!(stats.applied + rejected, EDITS);
        assert!(stats.full_deltas >= 200, "{stats:?}");
        assert!(stats.cells_deltas >= 4_000, "{stats:?}");
        assert!(stats.breaking >= 100, "{stats:?}");
        assert!(stats.no_change >= 100, "{stats:?}");
        for key in ["unknown_material", "too_many_cells", "outside_bounds"] {
            assert!(
                stats.rejected.get(key).copied().unwrap_or(0) >= 20,
                "{stats:?}"
            );
        }
        // The late joiner started from an edited world, not the initial one.
        assert!(late_snapshot_revision > 100, "{late_snapshot_revision}");
        assert!(clients[2].next_seq > 1_000);
    }

    #[test]
    fn gap_repaired_by_request_chunk() {
        let world = initial_world();
        let stone = mat(&world, "stone");
        let granite = mat(&world, "granite");
        let clay = mat(&world, "clay");
        let basalt = mat(&world, "basalt");
        let brass = mat(&world, "brass_plate");
        let ids: Vec<MaterialId> = world.materials().iter().map(|d| d.id).collect();
        let mut transport = ChannelTransport::new();
        // a receives everything; b, over the wire codec, loses the Applied
        // messages each step chooses to drop.
        let mut a = Client::new(transport.connect(), false);
        let mut b = Client::new(transport.connect(), true);
        let mut host = Host::new(world, CONFIG, Box::new(transport));
        let mut stats = Stats::default();
        a.join();
        b.join();
        host.tick();
        a.pump(&mut stats);
        b.pump_lossy(|_| true);
        assert_identical(host.world(), &b);

        let p = ChunkPos::new(0, 0, 0);
        let q = ChunkPos::new(-1, 0, 0);
        let r = ChunkPos::new(0, 0, -1);
        let rev = |w: &World, pos| w.chunk(pos).unwrap().revision;
        let cell = |x, y, z| Shape::Cell(CellPos::new(x, y, z));

        // E1 changes p and q; b loses it. Nothing tells b yet.
        a.send_edit(
            EditOp::Set(granite),
            Shape::Box {
                min: CellPos::new(-2, 10, 5),
                max: CellPos::new(1, 10, 5),
            },
        );
        host.tick();
        a.pump(&mut stats);
        let lost = b.pump_lossy(|_| false);
        assert!(
            matches!(&lost[..], [ServerMsg::Applied(res)] if res.deltas.len() == 2),
            "{lost:?}"
        );
        assert_eq!(rev(b.world(), p) + 1, rev(host.world(), p));
        assert_eq!(rev(b.world(), q) + 1, rev(host.world(), q));
        assert!(b.replica.take_requests().is_empty());

        // E2 changes p: b sees the gap, asks for p and leaves p alone. A gap
        // is repaired, not an error.
        a.send_edit(EditOp::Set(clay), cell(5, 12, 5));
        host.tick();
        a.pump(&mut stats);
        b.pump_lossy(|_| true);
        assert!(b.replica.is_awaiting(p));
        assert!(!b.replica.is_awaiting(q) && !b.replica.is_awaiting(r));
        assert_eq!(rev(b.world(), p) + 2, rev(host.world(), p));
        let held = b.replica.take_requests();
        assert_eq!(held, vec![ClientMsg::RequestChunk(p)]);
        assert!(b.replica.take_requests().is_empty(), "taken once");

        // E3 changes p and r before the request reaches the host: p's delta
        // is ignored without a second request, r's applies.
        a.send_edit(
            EditOp::Set(brass),
            Shape::Box {
                min: CellPos::new(5, 13, -1),
                max: CellPos::new(5, 13, 0),
            },
        );
        host.tick();
        a.pump(&mut stats);
        let e3 = b.pump_lossy(|_| true);
        assert_eq!(e3.len(), 1);
        assert!(b.replica.is_awaiting(p));
        assert_eq!(rev(b.world(), p) + 3, rev(host.world(), p));
        assert_eq!(rev(b.world(), r), rev(host.world(), r));
        assert_eq!(
            b.world().chunk(r).unwrap().hash(),
            host.world().chunk(r).unwrap().hash()
        );
        assert!(b.replica.take_requests().is_empty());

        // In one tick: E3b (p) arrives before b's request, E4 (p) after it.
        // b ignores E3b, takes the snapshot (which includes E3b) and applies
        // E4 on top. a gets the two results and no snapshot.
        a.send_edit(EditOp::Set(basalt), cell(7, 12, 5));
        for msg in &held {
            b.ep.send(msg.clone()).unwrap();
        }
        a.send_edit(EditOp::Set(granite), cell(6, 12, 5));
        host.tick();
        let to_a = a.ep.drain();
        assert_eq!(to_a.len(), 2);
        assert!(to_a.iter().all(|m| matches!(m, ServerMsg::Applied(_))));
        for msg in to_a {
            a.replica.handle(msg).unwrap();
        }
        let to_b = b.pump_lossy(|_| true);
        assert_eq!(to_b.len(), 3);
        assert!(matches!(to_b[0], ServerMsg::Applied(_)));
        let ServerMsg::ChunkSnapshot { pos, revision, .. } = &to_b[1] else {
            panic!("expected a snapshot, got {:?}", to_b[1]);
        };
        assert_eq!((*pos, *revision + 1), (p, rev(host.world(), p)));
        assert!(matches!(to_b[2], ServerMsg::Applied(_)));
        assert!(!b.replica.is_awaiting(p));
        assert_eq!(rev(b.world(), p), rev(host.world(), p));
        assert_eq!(
            b.world().chunk(p).unwrap().hash(),
            host.world().chunk(p).unwrap().hash()
        );
        assert_eq!(b.world().get_cell(CellPos::new(7, 12, 5)), Some(basalt));
        assert_eq!(b.world().get_cell(CellPos::new(6, 12, 5)), Some(granite));
        assert!(b.replica.take_requests().is_empty());

        // q is still one edit behind: a gap only shows on the next delta.
        assert_eq!(rev(b.world(), q) + 1, rev(host.world(), q));
        assert_ne!(b.world().hash(), host.world().hash());

        // A duplicated old result is refused and reported, and needs no
        // repair: the replica already holds newer chunks.
        let before = b.world().hash();
        let ServerMsg::Applied(old) = e3[0].clone() else {
            unreachable!()
        };
        assert!(matches!(
            b.replica.handle(ServerMsg::Applied(old)),
            Err(ReplicaError::Deltas(refused)) if refused.len() == 2
        ));
        assert_eq!(b.world().hash(), before);
        assert!(!b.replica.is_awaiting(p) && !b.replica.is_awaiting(r));
        assert!(b.replica.take_requests().is_empty());

        // b breaks a stone cell in q itself: its own result shows the gap,
        // its inventory still arrives, and the snapshot repairs q.
        b.send_edit(EditOp::Damage(u16::MAX), cell(-5, 10, 5));
        host.tick();
        a.pump(&mut stats);
        let msgs = b.pump_lossy(|_| true);
        assert!(matches!(
            msgs[..],
            [ServerMsg::Applied(_), ServerMsg::Inventory { .. }]
        ));
        assert!(b.replica.is_awaiting(q));
        assert_eq!(b.replica.inventory().items(), vec![(stone, 1)]);
        assert_eq!(Some(b.replica.inventory()), host.inventory(b.ep.player()));
        assert_eq!(b.send_requests(), vec![ClientMsg::RequestChunk(q)]);
        host.tick();
        assert!(a.ep.drain().is_empty(), "the snapshot goes to b only");
        let msgs = b.pump_lossy(|_| true);
        assert!(matches!(msgs[..], [ServerMsg::ChunkSnapshot { pos, .. }] if pos == q));
        assert!(!b.replica.is_awaiting(q));
        assert_identical(host.world(), &a);
        assert_identical(host.world(), &b);

        // The host answers a request for a resident chunk with its snapshot,
        // to the requester only, and ignores chunks it does not hold.
        for pos in [r, MISSING[0], ChunkPos::new(100, 0, 0)] {
            a.ep.send(ClientMsg::RequestChunk(pos)).unwrap();
        }
        host.tick();
        let host_r = host.world().chunk(r).unwrap();
        assert_eq!(
            a.ep.drain(),
            vec![ServerMsg::ChunkSnapshot {
                pos: r,
                revision: host_r.revision,
                storage: host_r.storage.clone(),
            }]
        );
        assert!(b.ep.drain().is_empty());

        // Random edits from both players while b loses one Applied in eight.
        // b sends its requests after every tick.
        let mut rng = Rng(0x0F03_4000_5EED_0001);
        let unknown = MaterialId(1000);
        let (mut dropped, mut requests) = (0, 0);
        let mut sent = 0;
        while sent < 3_000 {
            for _ in 0..1 + rng.below(8) {
                let (op, shape) = random_edit(&mut rng, &ids, unknown);
                if rng.below(2) == 0 {
                    a.send_edit(op, shape);
                } else {
                    b.send_edit(op, shape);
                }
                sent += 1;
            }
            host.tick();
            a.pump(&mut stats);
            b.pump_lossy(|m| {
                let lose = matches!(m, ServerMsg::Applied(_)) && rng.below(8) == 0;
                dropped += u32::from(lose);
                !lose
            });
            requests += b.send_requests().len();
            for (pos, chunk) in host.world().iter_chunks_sorted() {
                assert!(rev(b.world(), pos) <= chunk.revision);
            }
        }
        assert_identical(host.world(), &a);

        // Touch every chunk so each remaining gap shows, then deliver
        // everything until b has nothing left to ask for.
        let sweep: Vec<(CellPos, MaterialId)> = host
            .world()
            .iter_chunks_sorted()
            .map(|(pos, _)| {
                let at = pos.origin_cell();
                let m = if host.world().get_cell(at) == Some(brass) {
                    stone
                } else {
                    brass
                };
                (at, m)
            })
            .collect();
        for (at, m) in sweep {
            a.send_edit(EditOp::Set(m), Shape::Cell(at));
        }
        let mut rounds = 0;
        loop {
            host.tick();
            a.pump(&mut stats);
            b.pump_lossy(|_| true);
            let sent = b.send_requests();
            requests += sent.len();
            if sent.is_empty() {
                break;
            }
            rounds += 1;
            assert!(rounds < 4, "repair does not settle");
        }
        assert!(
            host.world()
                .iter_chunks_sorted()
                .all(|(pos, _)| !b.replica.is_awaiting(pos))
        );
        assert_identical(host.world(), &a);
        assert_identical(host.world(), &b);
        assert!(dropped > 100, "{dropped}");
        assert!(requests > 20, "{requests}");
        assert_eq!(Some(b.replica.inventory()), host.inventory(b.ep.player()));

        // A new Welcome starts over: nothing awaited, nothing to request.
        a.send_edit(EditOp::Set(clay), cell(5, 12, 5));
        host.tick();
        b.pump_lossy(|_| false);
        a.send_edit(EditOp::Set(basalt), cell(5, 12, 5));
        host.tick();
        b.pump_lossy(|_| true);
        assert!(b.replica.is_awaiting(p));
        b.replica
            .handle(ServerMsg::Welcome {
                player: b.ep.player(),
                seed: CONFIG.seed,
                bounds: bounds(),
                gen_params_hash: CONFIG.gen_params_hash,
            })
            .unwrap();
        assert!(!b.replica.is_awaiting(p));
        assert!(b.replica.take_requests().is_empty());
        assert!(b.replica.inventory().is_empty());
    }
    #[test]
    fn handle_edge_cases() {
        let table = materials();
        let stone = table.id("stone").unwrap();
        let welcome = |player: u32| ServerMsg::Welcome {
            player: PlayerId(player),
            seed: 1,
            bounds: bounds(),
            gen_params_hash: 2,
        };
        let snapshot =
            |pos: ChunkPos, revision: u64, storage: ChunkStorage| ServerMsg::ChunkSnapshot {
                pos,
                revision,
                storage,
            };
        let applied = |deltas: Vec<ChunkDelta>| {
            ServerMsg::Applied(rmc_world::edit::EditResult {
                command: CommandId::default(),
                status: EditStatus::Applied,
                deltas,
                yields: Vec::new(),
            })
        };
        let a = ChunkPos::new(0, 0, 0);
        let b = ChunkPos::new(-1, 0, 0);
        let cells = |pos, revision| ChunkDelta::Cells {
            pos,
            revision,
            changes: vec![(7, stone)],
        };

        let mut r = Replica::new(Arc::clone(&table));
        assert_eq!(
            r.hello(),
            ClientMsg::Hello {
                protocol: PROTOCOL_VERSION,
                material_hash: table.hash(),
            }
        );
        // World messages before Welcome have nowhere to go.
        assert_eq!(
            r.handle(snapshot(a, 0, ChunkStorage::Uniform(MaterialId::AIR))),
            Err(ReplicaError::NotJoined("ChunkSnapshot"))
        );
        assert_eq!(
            r.handle(applied(vec![cells(a, 1)])),
            Err(ReplicaError::NotJoined("Applied"))
        );
        // A rejected result (no deltas) and non-world messages are fine unjoined.
        assert_eq!(r.handle(applied(Vec::new())), Ok(()));
        assert_eq!(r.handle(ServerMsg::Inventory { items: Vec::new() }), Ok(()));
        assert_eq!(
            r.handle(ServerMsg::Rejected {
                reason: "no".into()
            }),
            Ok(())
        );
        assert_eq!(r.rejected(), Some("no"));
        assert!(r.world().is_none() && r.player().is_none());

        assert_eq!(r.handle(welcome(4)), Ok(()));
        assert_eq!(r.player(), Some(PlayerId(4)));
        assert!(r.rejected().is_none());
        assert_eq!(r.world().unwrap().bounds(), bounds());
        assert_eq!(r.world().unwrap().chunk_count(), 0);

        // Snapshots keep the host's revision; a later one replaces the chunk.
        r.handle(snapshot(a, 3, ChunkStorage::Uniform(MaterialId::AIR)))
            .unwrap();
        r.handle(snapshot(b, 9, ChunkStorage::Uniform(MaterialId::AIR)))
            .unwrap();
        r.handle(snapshot(a, 5, ChunkStorage::Uniform(stone)))
            .unwrap();
        let w = r.world().unwrap();
        assert_eq!(w.chunk(a).unwrap().revision, 5);
        assert_eq!(w.get_cell(CellPos::new(0, 0, 0)), Some(stone));
        assert_eq!(w.chunk_count(), 2);

        // Bad snapshots are refused and change nothing.
        let far = ChunkPos::new(10, 0, 0);
        assert_eq!(
            r.handle(snapshot(far, 0, ChunkStorage::Uniform(stone))),
            Err(ReplicaError::SnapshotOutOfBounds(far))
        );
        let malformed = ChunkStorage::Paletted {
            palette: vec![MaterialId::AIR, stone],
            bits: 3,
            data: Vec::new(),
        };
        assert_eq!(
            r.handle(snapshot(a, 6, malformed)),
            Err(ReplicaError::MalformedSnapshot(a))
        );
        for (bad, id) in [
            (ChunkStorage::Uniform(MaterialId(1000)), MaterialId(1000)),
            (
                ChunkStorage::Paletted {
                    palette: vec![stone, MaterialId(999)],
                    bits: 1,
                    data: vec![0; 512],
                },
                MaterialId(999),
            ),
        ] {
            assert!(bad.is_well_formed());
            assert_eq!(
                r.handle(snapshot(a, 6, bad)),
                Err(ReplicaError::UnknownSnapshotMaterial { pos: a, id })
            );
        }
        assert_eq!(r.world().unwrap().chunk(a).unwrap().revision, 5);
        assert_eq!(
            r.world().unwrap().chunk(a).unwrap().hash(),
            Chunk::uniform(stone).hash()
        );

        // Rejected after joining is not recorded and changes nothing.
        r.handle(ServerMsg::Rejected {
            reason: "late".into(),
        })
        .unwrap();
        assert!(r.rejected().is_none());
        assert_eq!(r.world().unwrap().chunk_count(), 2);

        // A refused delta is reported; the result's other deltas still apply.
        let before_b = r.world().unwrap().chunk(b).unwrap().hash();
        let err = r
            .handle(applied(vec![cells(b, 7), cells(a, 6)]))
            .unwrap_err();
        assert_eq!(
            err,
            ReplicaError::Deltas(vec![DeltaError::RevisionGap {
                pos: b,
                local: 9,
                got: 7,
            }])
        );
        assert!(err.to_string().contains("1 delta(s) refused"));
        let w = r.world().unwrap();
        assert_eq!(w.chunk(a).unwrap().revision, 6);
        assert_eq!(w.chunk(b).unwrap().revision, 9);
        assert_eq!(w.chunk(b).unwrap().hash(), before_b);
        r.handle(applied(vec![cells(b, 10)])).unwrap();
        assert_eq!(r.world().unwrap().chunk(b).unwrap().revision, 10);

        // A second Welcome starts over with an empty world.
        r.handle(welcome(5)).unwrap();
        assert_eq!(r.player(), Some(PlayerId(5)));
        assert_eq!(r.world().unwrap().chunk_count(), 0);
    }
}
