//! Seasons on the host (SAD-P2 §7, §9).
//!
//! A host given the Phase 2 tables ([`Host::with_eco`]) runs one season
//! tick per `AdvanceSeason` (or [`Host::advance_season`]), on the host
//! thread, between two messages: edits that arrive while it runs wait in
//! the inbound queue and are handled after it, so they are paused for the
//! whole tick. Every joined player then gets, in order:
//!
//! 1. `SeasonStarted { season }`;
//! 2. for each resident chunk the tick changed, ascending by position, a
//!    `ChunkSnapshot` if its material `revision` changed, else a
//!    `LayerSnapshot` if only its `eco_revision` did;
//! 3. `SeasonDone { season, report, changed_chunks }`, where
//!    `changed_chunks` counts the snapshots of step 2.
//!
//! Between seasons, the ledger terms of every applied edit are merged into
//! the between-seasons ledger, which the next `SeasonDone` reports (and
//! empties). Seasons count from 0: the first tick runs season 0 (row 0 of
//! `seasons.ron`, the row worldgen starts from).

use std::collections::BTreeMap;
use std::sync::Arc;

use rmc_eco::ledger::merge;
use rmc_eco::runner::activate_holders;
use rmc_eco::{Activity, EcoTables, Exec, GasPass, WaterPass, season_tick};
use rmc_protocol::{SeasonReport, ServerMsg};
use rmc_world::coords::ChunkPos;
use rmc_world::edit::PlayerId;
use rmc_world::ledger::SeasonLedger;
use rmc_world::world::World;
use tracing::{info, warn};

use crate::host::Host;
use crate::transport::{Recipient, Transport};

/// The host's season state.
pub(crate) struct Seasons {
    tables: Arc<EcoTables>,
    /// Quiet counters of the Jacobi passes, kept across seasons.
    activity: Activity,
    /// The season the next tick runs.
    next: u32,
    /// Ledger terms of the edits applied since the last tick.
    between_seasons: SeasonLedger,
}

impl Seasons {
    fn new(tables: Arc<EcoTables>) -> Self {
        Self {
            activity: Activity::new(tables.params().quiet_skip),
            tables,
            next: 0,
            between_seasons: SeasonLedger::default(),
        }
    }

    /// Prepares a (new) world for seasons: installs the tables'
    /// displacement rules, fills the water and gas sweeps' active sets
    /// (worldgen and loads keep none) and forgets the activity and edits of the world
    /// before. The season count goes on.
    pub(crate) fn install(&mut self, world: &mut World) {
        assert_eq!(
            world.materials().hash(),
            self.tables.materials().hash(),
            "the Phase 2 tables are built on another material table"
        );
        world.set_eco_rules(self.tables.displacement_rules());
        activate_holders(world, &WaterPass::new(&self.tables));
        for gas in GasPass::all(&self.tables) {
            activate_holders(world, &gas);
        }
        self.activity = Activity::new(self.tables.params().quiet_skip);
        self.between_seasons = SeasonLedger::default();
    }

    /// Merges an applied edit's ledger terms into the between-seasons
    /// ledger.
    pub(crate) fn edited(&mut self, ledger: &SeasonLedger) {
        merge(&mut self.between_seasons, ledger);
    }
}

impl Host {
    /// Runs seasons with the Phase 2 `tables`, which must be built on the
    /// world's material table. Installs their displacement rules in the
    /// world, so edits from now on run them too.
    pub fn with_eco(mut self, tables: Arc<EcoTables>) -> Self {
        let mut eco = Seasons::new(tables);
        eco.install(&mut self.world);
        self.eco = Some(eco);
        self
    }

    /// The Phase 2 tables the season ticks run with, if any.
    pub fn eco_tables(&self) -> Option<&Arc<EcoTables>> {
        self.eco.as_ref().map(|e| &e.tables)
    }

    /// Runs the next season ticks with `tables` (built on the world's
    /// material table, like [`Host::with_eco`]'s) instead, keeping the
    /// season count, the activity and the between-seasons ledger. Tests
    /// use it to override season rows. Returns `false`, changing nothing,
    /// without the Phase 2 tables.
    pub fn set_eco_tables(&mut self, tables: Arc<EcoTables>) -> bool {
        let Some(eco) = self.eco.as_mut() else {
            return false;
        };
        assert_eq!(
            self.world.materials().hash(),
            tables.materials().hash(),
            "the Phase 2 tables are built on another material table"
        );
        self.world.set_eco_rules(tables.displacement_rules());
        eco.tables = tables;
        true
    }

    /// The season the next tick runs; `None` without the Phase 2 tables.
    pub fn season(&self) -> Option<u32> {
        self.eco.as_ref().map(|e| e.next)
    }

    /// The ledger of the edits applied since the last season tick; `None`
    /// without the Phase 2 tables.
    pub fn between_seasons(&self) -> Option<&SeasonLedger> {
        self.eco.as_ref().map(|e| &e.between_seasons)
    }

    /// Runs the next season tick and sends every joined player
    /// `SeasonStarted`, the snapshots of the changed chunks and
    /// `SeasonDone` (see the module docs). Returns the season's report;
    /// `None`, with nothing run or sent, without the Phase 2 tables.
    pub fn advance_season(&mut self) -> Option<SeasonReport> {
        let seed = self.config().seed;
        let Some(eco) = self.eco.as_mut() else {
            warn!("AdvanceSeason ignored: the host has no Phase 2 tables");
            return None;
        };
        let season = eco.next;
        let to: Vec<PlayerId> = self.joined.iter().copied().collect();
        let transport = self.transport.as_mut();
        broadcast(transport, &to, &ServerMsg::SeasonStarted { season });
        let before = revisions(&self.world);
        let tick = season_tick(
            &mut self.world,
            &eco.tables,
            season,
            seed,
            &mut eco.activity,
            Exec::Parallel,
        );
        let snapshots = season_snapshots(&self.world, &before);
        for msg in &snapshots {
            broadcast(transport, &to, msg);
        }
        let report = SeasonReport {
            ledger: tick.ledger,
            between_seasons: std::mem::take(&mut eco.between_seasons),
            substep_ms: tick.substep_ms,
            active_cells_per_sweep: tick.active_cells_per_sweep,
            active_chunks_per_jacobi: tick.active_chunks_per_jacobi,
        };
        let changed_chunks = u32::try_from(snapshots.len()).unwrap_or(u32::MAX);
        broadcast(
            transport,
            &to,
            &ServerMsg::SeasonDone {
                season,
                report: Box::new(report.clone()),
                changed_chunks,
            },
        );
        eco.next += 1;
        info!(
            season,
            changed_chunks,
            players = to.len(),
            "season advanced"
        );
        Some(report)
    }
}

fn broadcast(transport: &mut dyn Transport, to: &[PlayerId], msg: &ServerMsg) {
    for &p in to {
        transport.send(Recipient::One(p), msg.clone());
    }
}

/// `(revision, eco_revision)` of every resident chunk.
fn revisions(world: &World) -> BTreeMap<ChunkPos, (u64, u64)> {
    world
        .iter_chunks_sorted()
        .map(|(pos, c)| (pos, (c.revision, c.eco_revision)))
        .collect()
}

/// The snapshots that replicate what changed since `before`, ascending by
/// position: `ChunkSnapshot` for a chunk whose `revision` changed (or that
/// was not resident), `LayerSnapshot` for one whose only change is its
/// `eco_revision`.
fn season_snapshots(world: &World, before: &BTreeMap<ChunkPos, (u64, u64)>) -> Vec<ServerMsg> {
    world
        .iter_chunks_sorted()
        .filter_map(|(pos, c)| match before.get(&pos) {
            Some(&(r, _)) if r != c.revision => Some(ServerMsg::chunk_snapshot(pos, c)),
            None => Some(ServerMsg::chunk_snapshot(pos, c)),
            Some(&(_, e)) if e != c.eco_revision => Some(ServerMsg::LayerSnapshot {
                pos,
                eco_revision: c.eco_revision,
                layers: c.layers.clone(),
                plants: c.plants.clone(),
            }),
            Some(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmc_protocol::{ClientMsg, PROTOCOL_VERSION, decode_server, encode_server};
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, CellPos, LocalPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus};
    use rmc_world::layers::LayerId;
    use rmc_world::materials::MaterialId;
    use rmc_world::shape::Shape;

    use crate::host::HostConfig;
    use crate::replica::Replica;
    use crate::transport::{ChannelTransport, ClientEndpoint};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    const CONFIG: HostConfig = HostConfig {
        seed: 0x5EA5_0107,
        gen_params_hash: 0xF107_F107,
    };

    /// A cell far hotter than the ground, in chunk (0, 0, 0) and more than
    /// a season's spread away from its faces.
    const HOT: CellPos = CellPos::new(8, 12, 16);
    const HOT_TEMP: i16 = 3000;

    fn tables() -> Arc<EcoTables> {
        Arc::new(EcoTables::load(ASSETS).expect("Phase 2 tables load"))
    }

    /// Three chunks along x of stone at a uniform `geothermal_base` (the
    /// bottom clamp, so a fixed point) with one hot cell at [`HOT`]; with
    /// `sky`, a layer of air chunks above at season 0's `air_temp` (its
    /// clamp), which cools the ground's top.
    fn world(t: &EcoTables, sky: bool) -> World {
        let top = if sky { 63 } else { 31 };
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(95, top, 31));
        let stone = t.materials().id("stone").unwrap();
        let ground = t.params().geothermal_base;
        let air_temp = t.seasons().seasons[0].air_temp;
        let mut w = World::new(t.materials().clone(), bounds);
        for pos in bounds.chunks() {
            let (fill, temp) = if pos.0.y == 0 {
                (stone, ground)
            } else {
                (MaterialId::AIR, air_temp)
            };
            let mut c = Chunk::uniform(fill);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let at = CellPos::from_chunk_local(pos, l);
                let temp = if at == HOT { HOT_TEMP } else { temp };
                c.layers.set_i16(LayerId::Temperature, l, temp);
            }
            w.insert_chunk(pos, c).unwrap();
        }
        w
    }

    fn host_with(world: World, clients: usize) -> (Host, Vec<ClientEndpoint>) {
        let mut transport = ChannelTransport::new();
        let endpoints = (0..clients).map(|_| transport.connect()).collect();
        let host = Host::new(world, CONFIG, Box::new(transport)).with_eco(tables());
        (host, endpoints)
    }

    fn hello(world: &World) -> ClientMsg {
        ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            material_hash: world.materials().hash(),
        }
    }

    fn edit(ep: &ClientEndpoint, seq: u32, op: EditOp, shape: Shape) -> ClientMsg {
        let me = ep.player();
        ClientMsg::Edit(EditCommand {
            id: CommandId { issuer: me, seq },
            issuer: me,
            op,
            shape,
        })
    }

    /// Splits one season's messages into its snapshots and its
    /// `SeasonDone`, checking the frame: `SeasonStarted { season }` first,
    /// `SeasonDone { season, .. }` last, snapshots in between.
    fn season_frame(msgs: &[ServerMsg], season: u32) -> (&[ServerMsg], &SeasonReport, u32) {
        assert!(msgs.len() >= 2, "{msgs:?}");
        assert_eq!(msgs[0], ServerMsg::SeasonStarted { season });
        let ServerMsg::SeasonDone {
            season: done,
            report,
            changed_chunks,
        } = &msgs[msgs.len() - 1]
        else {
            panic!("expected SeasonDone last, got {:?}", msgs.last());
        };
        assert_eq!(*done, season);
        let snaps = &msgs[1..msgs.len() - 1];
        assert_eq!(*changed_chunks as usize, snaps.len());
        (snaps, report, *changed_chunks)
    }

    fn snapshot_pos(msg: &ServerMsg) -> ChunkPos {
        match msg {
            ServerMsg::ChunkSnapshot { pos, .. } | ServerMsg::LayerSnapshot { pos, .. } => *pos,
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    #[test]
    fn advance_season_roundtrip() {
        let t = tables();
        let substeps = usize::from(t.seasons().substeps);
        let (mut host, eps) = host_with(world(&t, true), 2);
        let (a, outsider) = (&eps[0], &eps[1]);
        assert_eq!(host.season(), Some(0));

        // An AdvanceSeason before joining is ignored like any other message.
        outsider.send(ClientMsg::AdvanceSeason).unwrap();
        host.tick();
        assert_eq!(host.season(), Some(0));
        assert!(outsider.drain().is_empty());

        let mut replica = Replica::new(t.materials().clone());
        a.send(replica.hello()).unwrap();
        host.tick();
        for msg in a.drain() {
            replica.handle(msg).unwrap();
        }
        let start = host.world().hash();
        assert_eq!(replica.world().unwrap().hash(), start);

        // Edit, AdvanceSeason, edit in one batch: the first edit's result
        // comes before the season, the second's after it (edits pause).
        let stone = t.materials().id("stone").unwrap();
        let cell = |x| Shape::Cell(CellPos::new(x, 40, 5));
        a.send(edit(a, 0, EditOp::Set(stone), cell(1))).unwrap();
        a.send(ClientMsg::AdvanceSeason).unwrap();
        a.send(edit(a, 1, EditOp::Set(stone), cell(2))).unwrap();
        host.tick();
        let msgs = a.drain();
        assert!(outsider.drain().is_empty(), "not joined, sent nothing");
        let ServerMsg::Applied(first) = &msgs[0] else {
            panic!("expected the first edit's result, got {:?}", msgs[0]);
        };
        assert_eq!(
            (first.command.seq, &first.status),
            (0, &EditStatus::Applied)
        );
        let ServerMsg::Applied(second) = &msgs[msgs.len() - 1] else {
            panic!("expected the second edit's result last");
        };
        assert_eq!(second.command.seq, 1);
        let (snaps, report, changed) = season_frame(&msgs[1..msgs.len() - 1], 0);
        assert!(changed > 0);
        assert_eq!(report.substep_ms.len(), substeps);
        // Temperature, transitions and weathering, once per sub-step.
        assert_eq!(report.active_chunks_per_jacobi.len(), 3 * substeps);
        assert!(report.active_chunks_per_jacobi[0] > 0);
        let positions: Vec<ChunkPos> = snaps.iter().map(snapshot_pos).collect();
        assert!(positions.is_sorted(), "{positions:?}");
        assert_eq!(host.season(), Some(1));

        // Through the wire codec into the replica: it ends with the host's
        // world, revisions included.
        for msg in msgs {
            let msg = decode_server(&encode_server(&msg).unwrap()).unwrap();
            replica.handle(msg).unwrap();
        }
        let r = replica.world().unwrap();
        assert_ne!(host.world().hash(), start);
        assert_eq!(r.hash(), host.world().hash());
        for (pos, c) in host.world().iter_chunks_sorted() {
            let got = r.chunk(pos).unwrap();
            assert_eq!(
                (got.revision, got.eco_revision),
                (c.revision, c.eco_revision),
                "{pos:?}"
            );
        }

        // The next one is season 1, and so on.
        a.send(ClientMsg::AdvanceSeason).unwrap();
        host.tick();
        let msgs = a.drain();
        let (_, report, _) = season_frame(&msgs, 1);
        assert_eq!(report.substep_ms.len(), substeps);
        for msg in msgs {
            replica.handle(msg).unwrap();
        }
        assert_eq!(replica.world().unwrap().hash(), host.world().hash());
        assert_eq!(host.season(), Some(2));

        // Without the Phase 2 tables there is no season: nothing is sent.
        let mut transport = ChannelTransport::new();
        let ep = transport.connect();
        let w = world(&t, true);
        let mut plain = Host::new(w.clone(), CONFIG, Box::new(transport));
        ep.send(hello(&w)).unwrap();
        plain.tick();
        ep.drain();
        ep.send(ClientMsg::AdvanceSeason).unwrap();
        plain.tick();
        assert!(ep.drain().is_empty());
        assert_eq!(plain.season(), None);
        assert_eq!(plain.advance_season(), None);
        assert_eq!(plain.world().hash(), w.hash());
    }

    #[test]
    fn layer_snapshot_sent_for_eco_only_changes() {
        let t = tables();
        let (mut host, eps) = host_with(world(&t, false), 1);
        let ep = &eps[0];
        ep.send(hello(host.world())).unwrap();
        host.tick();
        ep.drain();
        let before = revisions(host.world());

        // Only the hot cell's chunk changes, and only its layers: one
        // LayerSnapshot with the host's layers, no ChunkSnapshot.
        ep.send(ClientMsg::AdvanceSeason).unwrap();
        host.tick();
        let msgs = ep.drain();
        let (snaps, _, changed) = season_frame(&msgs, 0);
        assert_eq!(changed, 1, "{snaps:?}");
        let hot = HOT.chunk();
        let c = host.world().chunk(hot).unwrap();
        assert_eq!(
            snaps[0],
            ServerMsg::LayerSnapshot {
                pos: hot,
                eco_revision: c.eco_revision,
                layers: c.layers.clone(),
                plants: c.plants.clone(),
            }
        );
        assert_eq!(c.revision, before[&hot].0, "materials unchanged");
        assert!(c.eco_revision > before[&hot].1);
        for (pos, c) in host.world().iter_chunks_sorted() {
            if pos != hot {
                assert_eq!((c.revision, c.eco_revision), before[&pos], "{pos:?}");
            }
        }

        // The choice is by revision kind: a material change in a season
        // sends the whole chunk, a layer-only change its layers, and an
        // unchanged chunk nothing.
        let mut w = world(&t, false);
        w.set_eco_rules(t.displacement_rules());
        let before = revisions(&w);
        let granite = t.materials().id("granite").unwrap();
        let mut ledger = SeasonLedger::default();
        w.eco_set(CellPos::new(40, 3, 3), granite, &mut ledger);
        w.eco_put_layer(CellPos::new(40, 4, 3), LayerId::Moisture, 9);
        w.eco_put_layer(CellPos::new(70, 3, 3), LayerId::Moisture, 9);
        let snaps = season_snapshots(&w, &before);
        assert_eq!(snaps.len(), 2, "{snaps:?}");
        assert_eq!(
            snaps[0],
            ServerMsg::chunk_snapshot(
                ChunkPos::new(1, 0, 0),
                w.chunk(ChunkPos::new(1, 0, 0)).unwrap()
            )
        );
        assert!(matches!(
            snaps[1],
            ServerMsg::LayerSnapshot { pos, .. } if pos == ChunkPos::new(2, 0, 0)
        ));
    }

    #[test]
    fn between_seasons_ledger_merged() {
        let t = tables();
        let (mut host, eps) = host_with(world(&t, true), 2);
        let (a, b) = (&eps[0], &eps[1]);
        for ep in [a, b] {
            ep.send(hello(host.world())).unwrap();
        }
        host.tick();
        a.drain();
        b.drain();
        assert_eq!(host.between_seasons(), Some(&SeasonLedger::default()));

        let stone = t.materials().id("stone").unwrap();
        let gas = LayerId::Gas(t.gas_index("firedamp").unwrap());
        let boxed = |min: (i32, i32, i32), max: (i32, i32, i32)| Shape::Box {
            min: CellPos::new(min.0, min.1, min.2),
            max: CellPos::new(max.0, max.1, max.2),
        };
        // Water into the ground, gas into the air, then stone over part of
        // the gas (pushed aside or lost), some water taken out again, from
        // two players; plus a rejected edit, which books nothing.
        let edits = [
            (
                a,
                EditOp::SetLayer {
                    layer: LayerId::Moisture,
                    value: 120,
                },
                boxed((2, 20, 2), (5, 23, 5)),
            ),
            (
                b,
                EditOp::SetLayer {
                    layer: gas,
                    value: 200,
                },
                boxed((40, 32, 2), (44, 36, 6)),
            ),
            (a, EditOp::Set(stone), boxed((40, 32, 2), (43, 35, 5))),
            (
                b,
                EditOp::SetLayer {
                    layer: LayerId::Moisture,
                    value: 7,
                },
                boxed((2, 20, 2), (3, 23, 5)),
            ),
            (
                a,
                EditOp::Set(MaterialId(999)),
                boxed((0, 40, 0), (1, 41, 1)),
            ),
        ];
        for (seq, (ep, op, shape)) in edits.into_iter().enumerate() {
            ep.send(edit(ep, seq as u32, op, shape)).unwrap();
        }
        host.tick();
        let mut expected = SeasonLedger::default();
        let mut results = 0;
        for msg in a.drain() {
            if let ServerMsg::Applied(r) = msg {
                results += 1;
                if r.status == EditStatus::Applied {
                    merge(&mut expected, &r.ledger);
                } else {
                    assert_eq!(*r.ledger, SeasonLedger::default());
                }
            }
        }
        assert_eq!(
            results, 5,
            "a sees every applied result and its own rejection"
        );
        // b sees the applied results only (and reads the seasons below).
        let b_results = b.drain();
        assert_eq!(b_results.len(), 4, "{b_results:?}");
        assert!(
            expected.water_edit_in > 0 && expected.water_edit_out > 0,
            "{expected:?}"
        );
        let g = usize::from(t.gas_index("firedamp").unwrap());
        assert!(expected.gas_edit_in[g] > 0, "{expected:?}");
        assert_eq!(host.between_seasons(), Some(&expected));

        // The season reports them, and the ledger starts over. An edit in
        // the same batch after AdvanceSeason belongs to the next season.
        b.send(ClientMsg::AdvanceSeason).unwrap();
        a.send(edit(
            a,
            5,
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: 50,
            },
            boxed((8, 8, 8), (8, 8, 8)),
        ))
        .unwrap();
        host.tick();
        let msgs = b.drain();
        let (_, report, _) = season_frame(&msgs[..msgs.len() - 1], 0);
        assert_eq!(report.between_seasons, expected);
        let late = SeasonLedger {
            water_edit_in: 50,
            ..SeasonLedger::default()
        };
        assert_eq!(host.between_seasons(), Some(&late));

        b.send(ClientMsg::AdvanceSeason).unwrap();
        host.tick();
        let msgs = b.drain();
        let (_, report, _) = season_frame(&msgs, 1);
        assert_eq!(report.between_seasons, late);
        assert_eq!(host.between_seasons(), Some(&SeasonLedger::default()));

        // A season with no edits before it reports an empty one.
        host.advance_season().unwrap();
        let msgs = b.drain();
        let (_, report, _) = season_frame(&msgs, 2);
        assert_eq!(report.between_seasons, SeasonLedger::default());
    }

    #[test]
    fn season_turns_moist_air_into_water_on_the_replica() {
        let t = tables();
        let water = t.role_material("water").expect("materials.ron has water");
        let (mut host, eps) = host_with(world(&t, true), 1);
        let ep = &eps[0];
        let mut replica = Replica::new(t.materials().clone());
        ep.send(replica.hello()).unwrap();
        host.tick();
        for msg in ep.drain() {
            replica.handle(msg).unwrap();
        }

        // A sealed one-cell cavity in the stone, given moisture between
        // seasons: nothing can flow, evaporate, freeze or boil there.
        let cavity = CellPos::new(40, 10, 5);
        ep.send(edit(ep, 0, EditOp::Remove, Shape::Cell(cavity)))
            .unwrap();
        ep.send(edit(
            ep,
            1,
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: 200,
            },
            Shape::Cell(cavity),
        ))
        .unwrap();
        host.tick();
        for msg in ep.drain() {
            replica.handle(msg).unwrap();
        }
        assert_eq!(host.world().get_cell(cavity), Some(MaterialId::AIR));
        let rev = host.world().chunk(cavity.chunk()).unwrap().revision;

        // The water sweep turns it into water; the chunk goes out whole.
        ep.send(ClientMsg::AdvanceSeason).unwrap();
        host.tick();
        let msgs = ep.drain();
        let (snaps, report, _) = season_frame(&msgs, 0);
        assert!(report.active_cells_per_sweep.iter().any(|&n| n > 0));
        let c = host.world().chunk(cavity.chunk()).unwrap();
        assert!(c.revision > rev);
        assert!(
            snaps.contains(&ServerMsg::chunk_snapshot(cavity.chunk(), c)),
            "{snaps:?}"
        );
        for msg in msgs {
            replica.handle(msg).unwrap();
        }
        let r = replica.world().unwrap();
        assert_eq!(host.world().get_cell(cavity), Some(water));
        assert_eq!(r.get_cell(cavity), Some(water));
        assert_eq!(r.layer(cavity, LayerId::Moisture), 200);
        assert_eq!(r.hash(), host.world().hash());
    }
}
