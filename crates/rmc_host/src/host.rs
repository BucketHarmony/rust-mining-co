//! The authoritative host (SAD §8.5).
//!
//! [`Host`] owns the [`World`] and talks to its clients only through a
//! [`Transport`]. Each [`Host::tick`] drains the inbound messages and handles
//! them one at a time in arrival order, sending replies as it goes, so the
//! outbound stream is in the same order as the inbound one.
//!
//! A connection starts with `Hello`. If its protocol version and material
//! table hash match the host's, the player joins: it gets `Welcome` and then
//! one `ChunkSnapshot` per resident chunk, ascending by position. Otherwise
//! it gets `Rejected` and stays out. Until a player has joined, the host
//! ignores everything else it sends and sends it nothing but replies to
//! `Hello`.

use std::collections::BTreeSet;

use rmc_protocol::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use rmc_world::edit::{EditCommand, EditStatus, PlayerId};
use rmc_world::world::World;
use tracing::{debug, info};

use crate::transport::{Recipient, Transport};

/// World facts the host reports in `Welcome` that the [`World`] itself does
/// not hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostConfig {
    /// The worldgen seed.
    pub seed: u64,
    /// Hash of the `GenParams` the world was generated with.
    pub gen_params_hash: u64,
}

/// The authoritative world and its connection to the clients.
pub struct Host {
    world: World,
    config: HostConfig,
    transport: Box<dyn Transport>,
    /// Players that sent a matching `Hello`, ascending.
    joined: BTreeSet<PlayerId>,
}

impl Host {
    pub fn new(world: World, config: HostConfig, transport: Box<dyn Transport>) -> Self {
        Self {
            world,
            config,
            transport,
            joined: BTreeSet::new(),
        }
    }

    /// Drains the inbound messages and handles each in arrival order.
    pub fn tick(&mut self) {
        for (player, msg) in self.transport.recv() {
            self.handle(player, msg);
        }
    }

    /// The authoritative world. Tests read this; the client never has access to it.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Whether `player` has joined (sent a matching `Hello`).
    pub fn is_joined(&self, player: PlayerId) -> bool {
        self.joined.contains(&player)
    }

    fn handle(&mut self, player: PlayerId, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello {
                protocol,
                material_hash,
            } => self.hello(player, protocol, material_hash),
            _ if !self.joined.contains(&player) => {
                debug!(player = player.0, "message before join ignored");
            }
            ClientMsg::Edit(cmd) => self.edit(player, cmd),
            // Single player: there is nobody to relay positions to.
            ClientMsg::PlayerState { .. } => {}
            ClientMsg::RequestChunk(pos) => {
                debug!(player = player.0, ?pos, "RequestChunk not handled yet");
            }
            ClientMsg::SaveRequest => {
                debug!(player = player.0, "SaveRequest not handled yet");
            }
        }
    }

    /// Joins `player` if its protocol version and material table match:
    /// `Welcome`, then a snapshot of every resident chunk in position order.
    /// A mismatch gets `Rejected`. A joined player's repeated `Hello` is ignored.
    fn hello(&mut self, player: PlayerId, protocol: u32, material_hash: u64) {
        if self.joined.contains(&player) {
            debug!(player = player.0, "repeated Hello ignored");
            return;
        }
        let host_hash = self.world.materials().hash();
        let mismatch = if protocol != PROTOCOL_VERSION {
            Some(format!(
                "protocol version mismatch: host {PROTOCOL_VERSION}, client {protocol}"
            ))
        } else if material_hash != host_hash {
            Some(format!(
                "material table mismatch: host hash {host_hash:016x}, client {material_hash:016x}"
            ))
        } else {
            None
        };
        if let Some(reason) = mismatch {
            info!(player = player.0, %reason, "Hello rejected");
            self.transport
                .send(Recipient::One(player), ServerMsg::Rejected { reason });
            return;
        }

        let to = Recipient::One(player);
        self.transport.send(
            to,
            ServerMsg::Welcome {
                player,
                seed: self.config.seed,
                bounds: self.world.bounds(),
                gen_params_hash: self.config.gen_params_hash,
            },
        );
        for (pos, chunk) in self.world.iter_chunks_sorted() {
            self.transport.send(
                to,
                ServerMsg::ChunkSnapshot {
                    pos,
                    revision: chunk.revision,
                    storage: chunk.storage.clone(),
                },
            );
        }
        self.joined.insert(player);
        info!(
            player = player.0,
            chunks = self.world.chunk_count(),
            "player joined"
        );
    }

    /// Applies `cmd` as an edit by `player`, whatever issuer the command
    /// names. An applied result goes to every joined player; a rejected one
    /// only to `player`.
    fn edit(&mut self, player: PlayerId, mut cmd: EditCommand) {
        cmd.issuer = player;
        cmd.id.issuer = player;
        let result = self.world.apply(&cmd);
        if let EditStatus::Rejected(_) = result.status {
            self.transport
                .send(Recipient::One(player), ServerMsg::Applied(result));
            return;
        }
        // Players that have not joined hold no snapshots to apply deltas to,
        // so the result goes to joined players only, not Recipient::All.
        let msg = ServerMsg::Applied(result);
        for &p in &self.joined {
            self.transport.send(Recipient::One(p), msg.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rmc_world::chunk::{Chunk, ChunkStorage};
    use rmc_world::coords::{CellAabb, CellPos, ChunkPos};
    use rmc_world::edit::{ChunkDelta, CommandId, EditOp, RejectReason};
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::shape::Shape;

    use crate::transport::{ChannelTransport, ClientEndpoint};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    const CONFIG: HostConfig = HostConfig {
        seed: 0xDEAD_BEEF_0123_4567,
        gen_params_hash: 0x0F0F_1234_5678_9ABC,
    };

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

    /// A world with uniform and paletted chunks, some revisions above zero,
    /// and two in-bounds chunks left non-resident.
    fn test_world() -> World {
        let mut world = World::new(materials(), bounds());
        let stone = mat(&world, "stone");
        let dirt = mat(&world, "dirt");
        let missing = [ChunkPos::new(1, 1, 1), ChunkPos::new(-2, 1, 0)];
        for pos in bounds().chunks() {
            if missing.contains(&pos) {
                continue;
            }
            let fill = match pos.0.y {
                0 => stone,
                _ if pos.0.x == 0 => dirt,
                _ => MaterialId::AIR,
            };
            world.insert_chunk(pos, Chunk::uniform(fill)).unwrap();
        }
        // Edits that make some chunks paletted and bump their revisions,
        // including one Full and one chunk edited twice.
        let edits = [
            (
                EditOp::Set(mat(&world, "granite")),
                Shape::Cell(CellPos::new(-1, 5, -1)),
            ),
            (
                EditOp::Set(mat(&world, "basalt")),
                Shape::Box {
                    min: CellPos::new(-40, 20, 0),
                    max: CellPos::new(-25, 40, 10),
                },
            ),
            (
                EditOp::Remove,
                Shape::Sphere {
                    center: CellPos::new(0, 31, 0),
                    radius: 6,
                },
            ),
            (
                EditOp::Set(mat(&world, "copper_ore")),
                Shape::Box {
                    min: CellPos::new(32, 0, 32),
                    max: CellPos::new(63, 7, 63),
                },
            ),
            (
                EditOp::Paint(mat(&world, "clay")),
                Shape::Cell(CellPos::new(-1, 6, -1)),
            ),
            (
                EditOp::Set(mat(&world, "dirt")),
                Shape::Cell(CellPos::new(-1, 6, -1)),
            ),
        ];
        for (seq, (op, shape)) in edits.into_iter().enumerate() {
            let cmd = EditCommand {
                id: CommandId {
                    issuer: PlayerId(0),
                    seq: seq as u32,
                },
                issuer: PlayerId(0),
                op,
                shape,
            };
            let result = world.apply(&cmd);
            assert_eq!(result.status, EditStatus::Applied);
            assert!(!result.deltas.is_empty());
        }
        assert!(
            world
                .iter_chunks_sorted()
                .any(|(_, c)| matches!(c.storage, ChunkStorage::Paletted { .. }))
        );
        assert!(world.iter_chunks_sorted().any(|(_, c)| c.revision >= 2));
        world
    }

    /// A host over `world` with `clients` connected endpoints (players 1, 2, ...).
    fn host_with(world: World, clients: usize) -> (Host, Vec<ClientEndpoint>) {
        let mut transport = ChannelTransport::new();
        let endpoints = (0..clients).map(|_| transport.connect()).collect();
        (Host::new(world, CONFIG, Box::new(transport)), endpoints)
    }

    fn hello(world: &World) -> ClientMsg {
        ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            material_hash: world.materials().hash(),
        }
    }

    fn edit(issuer: u32, seq: u32, op: EditOp, shape: Shape) -> ClientMsg {
        ClientMsg::Edit(EditCommand {
            id: CommandId {
                issuer: PlayerId(issuer),
                seq,
            },
            issuer: PlayerId(issuer),
            op,
            shape,
        })
    }

    fn welcome_for(host: &Host, player: PlayerId) -> ServerMsg {
        ServerMsg::Welcome {
            player,
            seed: CONFIG.seed,
            bounds: host.world().bounds(),
            gen_params_hash: CONFIG.gen_params_hash,
        }
    }

    /// Checks that `msgs` is exactly a join (Welcome for `player`, then one
    /// snapshot per resident chunk of the host world, ascending) and returns
    /// the replica those messages build.
    fn assert_join(host: &Host, player: PlayerId, msgs: &[ServerMsg]) -> World {
        assert!(!msgs.is_empty(), "no messages for {player:?}");
        assert_eq!(msgs[0], welcome_for(host, player));
        let ServerMsg::Welcome { bounds, .. } = msgs[0] else {
            unreachable!()
        };
        let world = host.world();
        let expected: Vec<(ChunkPos, &Chunk)> = world.iter_chunks_sorted().collect();
        let snaps = &msgs[1..];
        assert_eq!(
            snaps.len(),
            expected.len(),
            "one snapshot per resident chunk"
        );
        let mut replica = World::new(Arc::clone(world.materials()), bounds);
        for (msg, (pos, chunk)) in snaps.iter().zip(&expected) {
            let ServerMsg::ChunkSnapshot {
                pos: got,
                revision,
                storage,
            } = msg
            else {
                panic!("expected ChunkSnapshot, got {msg:?}");
            };
            assert_eq!(got, pos, "snapshots ascend by position");
            assert_eq!(*revision, chunk.revision);
            assert_eq!(*storage, chunk.storage);
            let mut c = Chunk::new(storage.clone());
            c.revision = *revision;
            assert!(
                replica.insert_chunk(*got, c).unwrap().is_none(),
                "duplicate"
            );
        }
        assert_eq!(replica.chunk_count(), world.chunk_count());
        assert_eq!(replica.hash(), world.hash());
        replica
    }

    /// Applies every delta of every `Applied` message to `replica`, checking
    /// that nothing else arrives.
    fn replay(replica: &mut World, msgs: &[ServerMsg]) {
        for msg in msgs {
            let ServerMsg::Applied(result) = msg else {
                panic!("expected Applied, got {msg:?}");
            };
            for delta in &result.deltas {
                replica.apply_delta(delta).expect("delta applies in order");
            }
        }
    }

    #[test]
    fn hello_mismatch_rejected() {
        let world = test_world();
        let good_hash = world.materials().hash();
        let (mut host, eps) = host_with(world, 5);
        let [a, b, c, d, e] = &eps[..] else {
            unreachable!()
        };
        let before = host.world().hash();

        // Nothing is sent before a tick.
        a.send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            material_hash: good_hash ^ 1,
        })
        .unwrap();
        b.send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION + 1,
            material_hash: good_hash,
        })
        .unwrap();
        c.send(ClientMsg::Hello {
            protocol: 0,
            material_hash: 0,
        })
        .unwrap();
        d.send(hello(host.world())).unwrap();
        e.send(ClientMsg::Hello {
            protocol: u32::MAX,
            material_hash: u64::MAX,
        })
        .unwrap();
        assert!(eps.iter().all(|ep| ep.drain().is_empty()));
        host.tick();

        // Each mismatch gets exactly one Rejected naming what differed, and
        // no Welcome or snapshots.
        let reason_of = |ep: &ClientEndpoint| -> String {
            let msgs = ep.drain();
            assert_eq!(msgs.len(), 1, "{:?}: {msgs:?}", ep.player());
            match &msgs[0] {
                ServerMsg::Rejected { reason } => reason.clone(),
                other => panic!("expected Rejected, got {other:?}"),
            }
        };
        let ra = reason_of(a);
        assert!(ra.contains("material"), "{ra}");
        let rb = reason_of(b);
        assert!(rb.contains("protocol"), "{rb}");
        let rc = reason_of(c);
        assert!(!rc.is_empty());
        let re = reason_of(e);
        assert!(!re.is_empty());
        for p in [a, b, c, e] {
            assert!(!host.is_joined(p.player()));
        }

        // The matching client joined normally beside them.
        assert!(host.is_joined(d.player()));
        let mut replica_d = assert_join(&host, d.player(), &d.drain());

        // A rejected player's other messages are ignored: nothing changes and
        // nobody hears about them.
        let stone_cell = CellPos::new(5, 3, 5);
        let granite = mat(host.world(), "granite");
        a.send(edit(1, 0, EditOp::Set(granite), Shape::Cell(stone_cell)))
            .unwrap();
        a.send(edit(
            1,
            1,
            EditOp::Damage(u16::MAX),
            Shape::Cell(stone_cell),
        ))
        .unwrap();
        a.send(ClientMsg::RequestChunk(ChunkPos::new(0, 0, 0)))
            .unwrap();
        a.send(ClientMsg::PlayerState {
            pos: glam::Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
        })
        .unwrap();
        b.send(edit(2, 0, EditOp::Remove, Shape::Cell(stone_cell)))
            .unwrap();
        host.tick();
        assert_eq!(host.world().hash(), before);
        assert!(eps.iter().all(|ep| ep.drain().is_empty()));

        // A rejected player may try again; a matching Hello then joins it
        // under its own id.
        a.send(hello(host.world())).unwrap();
        host.tick();
        assert!(host.is_joined(a.player()));
        let mut replica_a = assert_join(&host, a.player(), &a.drain());
        assert!(eps.iter().all(|ep| ep.drain().is_empty()));

        // A second Hello from a joined player is ignored, matching or not.
        a.send(hello(host.world())).unwrap();
        d.send(ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            material_hash: good_hash ^ 1,
        })
        .unwrap();
        host.tick();
        assert!(host.is_joined(a.player()) && host.is_joined(d.player()));
        assert!(eps.iter().all(|ep| ep.drain().is_empty()));

        // Now a's edits count, and both joined players see them; the still
        // rejected players do not.
        a.send(edit(1, 2, EditOp::Set(granite), Shape::Cell(stone_cell)))
            .unwrap();
        host.tick();
        assert_eq!(host.world().get_cell(stone_cell), Some(granite));
        let to_a = a.drain();
        let to_d = d.drain();
        assert_eq!(to_a.len(), 1);
        assert_eq!(to_a, to_d);
        replay(&mut replica_a, &to_a);
        replay(&mut replica_d, &to_d);
        assert_eq!(replica_a.hash(), host.world().hash());
        assert_eq!(replica_d.hash(), host.world().hash());
        for p in [b, c, e] {
            assert!(p.drain().is_empty());
        }
    }

    #[test]
    fn join_receives_all_snapshots() {
        let world = test_world();
        let resident = world.chunk_count();
        assert_eq!(resident, 30);
        let (mut host, eps) = host_with(world, 4);
        let [a, b, c, idle] = &eps[..] else {
            unreachable!()
        };

        // A tick with nothing inbound sends nothing.
        host.tick();
        assert!(eps.iter().all(|ep| ep.drain().is_empty()));

        // Hello is answered on the next tick with Welcome and every resident
        // chunk; the replica built from them hashes equal to the host.
        a.send(hello(host.world())).unwrap();
        assert!(a.drain().is_empty());
        host.tick();
        let msgs = a.drain();
        assert_eq!(msgs.len(), 1 + resident);
        let mut replica_a = assert_join(&host, a.player(), &msgs);
        for pos in [ChunkPos::new(1, 1, 1), ChunkPos::new(-2, 1, 0)] {
            assert!(replica_a.chunk(pos).is_none(), "non-resident chunk sent");
        }
        for (pos, chunk) in host.world().iter_chunks_sorted() {
            assert_eq!(replica_a.chunk(pos).unwrap().revision, chunk.revision);
        }

        // An Edit from a player that has not said Hello is ignored.
        let before = host.world().hash();
        let cell = CellPos::new(-10, 2, 12);
        let iron = mat(host.world(), "iron_ore");
        b.send(edit(2, 0, EditOp::Set(iron), Shape::Cell(cell)))
            .unwrap();
        host.tick();
        assert_eq!(host.world().hash(), before);
        assert!(eps.iter().all(|ep| ep.drain().is_empty()));

        // a edits; b joins later and gets the edited state, including the
        // bumped revision. a gets no snapshots again, only the result.
        a.send(edit(1, 0, EditOp::Set(iron), Shape::Cell(cell)))
            .unwrap();
        host.tick();
        let to_a = a.drain();
        assert_eq!(to_a.len(), 1);
        replay(&mut replica_a, &to_a);
        b.send(hello(host.world())).unwrap();
        host.tick();
        assert!(a.drain().is_empty());
        let mut replica_b = assert_join(&host, b.player(), &b.drain());
        assert_eq!(replica_b.get_cell(cell), Some(iron));
        assert_eq!(
            replica_b.chunk(cell.chunk()).unwrap().revision,
            host.world().chunk(cell.chunk()).unwrap().revision
        );

        // Within one tick, arrival order decides what a joiner sees. An edit
        // that arrives before c's Hello is in c's snapshots and c gets no
        // result for it; one that arrives after reaches c as a delta on top
        // of those snapshots.
        let early = CellPos::new(33, 1, -33);
        let late = CellPos::new(33, 2, -33);
        let late_rev_before = host.world().chunk(late.chunk()).unwrap().revision;
        a.send(edit(1, 1, EditOp::Remove, Shape::Cell(early)))
            .unwrap();
        c.send(hello(host.world())).unwrap();
        b.send(edit(2, 1, EditOp::Set(iron), Shape::Cell(late)))
            .unwrap();
        host.tick();
        let to_c = c.drain();
        let welcome_c = welcome_for(&host, c.player());
        assert_eq!(to_c[0], welcome_c);
        assert_eq!(to_c.len(), 1 + resident + 1);
        let (join_c, after_c) = to_c.split_at(1 + resident);
        let mut replica_c = World::new(Arc::clone(host.world().materials()), bounds());
        for msg in &join_c[1..] {
            let ServerMsg::ChunkSnapshot {
                pos,
                revision,
                storage,
            } = msg
            else {
                panic!("expected ChunkSnapshot, got {msg:?}");
            };
            let mut chunk = Chunk::new(storage.clone());
            chunk.revision = *revision;
            replica_c.insert_chunk(*pos, chunk).unwrap();
        }
        assert_eq!(replica_c.get_cell(early), Some(MaterialId::AIR));
        assert_ne!(replica_c.get_cell(late), Some(iron));
        assert_eq!(
            replica_c.chunk(late.chunk()).unwrap().revision,
            late_rev_before + 1,
            "the early edit was in the same chunk"
        );
        replay(&mut replica_c, after_c);
        assert_eq!(replica_c.get_cell(late), Some(iron));

        // a and b got both results, in arrival order.
        let to_a = a.drain();
        let to_b = b.drain();
        assert_eq!(to_a.len(), 2);
        assert_eq!(to_a, to_b);
        assert_eq!(to_a[1], after_c[0]);
        replay(&mut replica_a, &to_a);
        replay(&mut replica_b, &to_b);
        for replica in [&replica_a, &replica_b, &replica_c] {
            assert_eq!(replica.hash(), host.world().hash());
        }

        // A connected endpoint that never said Hello received nothing at all.
        assert!(idle.drain().is_empty());
        assert!(!host.is_joined(idle.player()));
    }

    #[test]
    fn edits_applied_in_arrival_order() {
        let world = test_world();
        let mut reference = world.clone();
        let (mut host, eps) = host_with(world, 3);
        let [a, b, watcher] = &eps[..] else {
            unreachable!()
        };
        for ep in &eps {
            ep.send(hello(host.world())).unwrap();
        }
        host.tick();
        let mut replicas: Vec<World> = eps
            .iter()
            .map(|ep| assert_join(&host, ep.player(), &ep.drain()))
            .collect();

        let w = host.world();
        let granite = mat(w, "granite");
        let basalt = mat(w, "basalt");
        let brass = mat(w, "brass_plate");
        let clay = mat(w, "clay");
        let shared = CellPos::new(-3, 10, 4);
        let border = Shape::Box {
            min: CellPos::new(28, 28, -4),
            max: CellPos::new(35, 35, 3),
        };

        // Interleaved commands from a and b, several touching the same cells,
        // so any reordering changes the result. Some carry a forged issuer;
        // two are rejected whole.
        let script: Vec<(&ClientEndpoint, ClientMsg)> = vec![
            (a, edit(1, 0, EditOp::Set(granite), Shape::Cell(shared))),
            (b, edit(2, 0, EditOp::Set(basalt), Shape::Cell(shared))),
            (a, edit(1, 1, EditOp::Set(brass), border)),
            (b, edit(2, 1, EditOp::Paint(clay), border)),
            (
                a,
                edit(1, 2, EditOp::Remove, Shape::Cell(CellPos::new(30, 30, 0))),
            ),
            // b claims to be a.
            (
                b,
                edit(
                    1,
                    77,
                    EditOp::Set(granite),
                    Shape::Cell(CellPos::new(30, 30, 0)),
                ),
            ),
            (
                a,
                edit(
                    1,
                    3,
                    EditOp::Damage(1),
                    Shape::Sphere {
                        center: CellPos::new(-20, 20, -20),
                        radius: 3,
                    },
                ),
            ),
            (
                b,
                edit(
                    2,
                    2,
                    EditOp::Set(basalt),
                    Shape::Box {
                        min: CellPos::new(-64, 0, -64),
                        max: CellPos::new(-1, 63, 63),
                    },
                ),
            ),
            (
                a,
                edit(
                    1,
                    4,
                    EditOp::Remove,
                    Shape::Box {
                        min: CellPos::new(500, 0, 0),
                        max: CellPos::new(510, 5, 5),
                    },
                ),
            ),
            (
                b,
                edit(2, 3, EditOp::Set(MaterialId::AIR), Shape::Cell(shared)),
            ),
            (
                a,
                edit(
                    1,
                    5,
                    EditOp::Damage(u16::MAX),
                    Shape::Cell(CellPos::new(10, 3, 10)),
                ),
            ),
            // 6,400 cells in one air chunk: a Full delta.
            (
                b,
                edit(
                    2,
                    4,
                    EditOp::Set(granite),
                    Shape::Box {
                        min: CellPos::new(-32, 32, -32),
                        max: CellPos::new(-13, 47, -13),
                    },
                ),
            ),
            (a, edit(1, 6, EditOp::Set(granite), Shape::Cell(shared))),
        ];

        // The reference applies the same commands in send order, each with
        // the sender as issuer.
        let mut expected = Vec::new();
        for (ep, msg) in &script {
            let ClientMsg::Edit(mut cmd) = msg.clone() else {
                unreachable!()
            };
            cmd.issuer = ep.player();
            cmd.id.issuer = ep.player();
            expected.push(reference.apply(&cmd));
        }
        let statuses: Vec<_> = expected.iter().map(|r| r.status.clone()).collect();
        assert_eq!(
            statuses[7],
            EditStatus::Rejected(RejectReason::TooManyCells)
        );
        assert_eq!(
            statuses[8],
            EditStatus::Rejected(RejectReason::OutsideBounds)
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|s| **s == EditStatus::Applied)
                .count(),
            11
        );
        assert!(
            expected
                .iter()
                .flat_map(|r| &r.deltas)
                .any(|d| matches!(d, ChunkDelta::Full { .. })),
            "the 6,400-cell Set gives a Full delta"
        );

        for (ep, msg) in &script {
            ep.send(msg.clone()).unwrap();
        }
        host.tick();

        // The host world matches the reference: arrival order, last writer wins.
        assert_eq!(host.world().hash(), reference.hash());
        assert_eq!(host.world().get_cell(shared), Some(granite));
        assert_eq!(
            host.world().get_cell(CellPos::new(30, 30, 0)),
            Some(granite)
        );
        for (pos, chunk) in host.world().iter_chunks_sorted() {
            assert_eq!(chunk.revision, reference.chunk(pos).unwrap().revision);
        }

        // b's forged issuer was replaced by b.
        assert_eq!(
            expected[5].command,
            CommandId {
                issuer: b.player(),
                seq: 77
            }
        );

        // Every joined player gets every applied result in arrival order; a
        // rejected result goes only to its issuer, in its place in the order.
        for (i, ep) in eps.iter().enumerate() {
            let want: Vec<ServerMsg> = expected
                .iter()
                .filter(|r| r.status == EditStatus::Applied || r.command.issuer == ep.player())
                .cloned()
                .map(ServerMsg::Applied)
                .collect();
            let got = ep.drain();
            assert_eq!(got, want, "{:?}", ep.player());
            replay(&mut replicas[i], &got);
            assert_eq!(replicas[i].hash(), host.world().hash());
        }

        // Order holds across ticks too: commands split over two ticks, then
        // two ticks with nothing inbound.
        let second: Vec<(&ClientEndpoint, ClientMsg)> = vec![
            (b, edit(2, 10, EditOp::Set(clay), Shape::Cell(shared))),
            (a, edit(1, 10, EditOp::Set(brass), Shape::Cell(shared))),
            (b, edit(2, 11, EditOp::Remove, border)),
            (
                a,
                edit(
                    1,
                    11,
                    EditOp::Set(basalt),
                    Shape::Cell(CellPos::new(31, 31, 1)),
                ),
            ),
        ];
        let mut expected = Vec::new();
        for (i, (ep, msg)) in second.iter().enumerate() {
            let ClientMsg::Edit(mut cmd) = msg.clone() else {
                unreachable!()
            };
            cmd.issuer = ep.player();
            cmd.id.issuer = ep.player();
            expected.push(ServerMsg::Applied(reference.apply(&cmd)));
            ep.send(msg.clone()).unwrap();
            if i == 1 {
                host.tick();
            }
        }
        host.tick();
        host.tick();
        host.tick();
        assert_eq!(host.world().hash(), reference.hash());
        assert_eq!(host.world().get_cell(shared), Some(brass));
        for (i, ep) in eps.iter().enumerate() {
            let got = ep.drain();
            assert_eq!(got, expected, "{:?}", ep.player());
            replay(&mut replicas[i], &got);
            assert_eq!(replicas[i].hash(), host.world().hash());
        }
        assert_eq!(watcher.player(), PlayerId(3));
    }
}
