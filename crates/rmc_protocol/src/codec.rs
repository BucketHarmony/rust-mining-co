//! postcard encoding of [`ClientMsg`] and [`ServerMsg`] (SAD §8.4).
//!
//! One message is one byte buffer; framing belongs to the transport. Decoding
//! is strict: the buffer must hold exactly one message, with no trailing
//! bytes. Decoded server messages also have every chunk storage checked with
//! [`ChunkStorage::is_well_formed`](rmc_world::chunk::ChunkStorage::is_well_formed), so the client can hand a snapshot to
//! `World::insert_chunk` without it panicking later, and every snapshot's
//! plant indices checked to lie inside the chunk. Layer data is checked by
//! its decoder (`ChunkLayers`), and `ChunkDelta::Layers` values by
//! `World::apply_delta`.

use std::collections::BTreeMap;

use rmc_world::coords::CHUNK_VOLUME;
use rmc_world::edit::ChunkDelta;
use rmc_world::layers::PlantState;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::msg::{ClientMsg, ServerMsg};

/// Why a message could not be encoded or decoded.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("postcard encode failed: {0}")]
    Encode(postcard::Error),
    #[error("postcard decode failed: {0}")]
    Decode(postcard::Error),
    #[error("{0} trailing bytes after the message")]
    TrailingBytes(usize),
    #[error("message carries malformed chunk storage or plant indices")]
    MalformedStorage,
}

pub fn encode_client(msg: &ClientMsg) -> Result<Vec<u8>, CodecError> {
    encode(msg)
}

pub fn decode_client(bytes: &[u8]) -> Result<ClientMsg, CodecError> {
    decode(bytes)
}

pub fn encode_server(msg: &ServerMsg) -> Result<Vec<u8>, CodecError> {
    encode(msg)
}

/// Decodes one server message and rejects any chunk storage that is not
/// well formed (`ChunkSnapshot` and `Full` deltas in `Applied`) and any
/// snapshot plant index outside the chunk.
pub fn decode_server(bytes: &[u8]) -> Result<ServerMsg, CodecError> {
    let msg: ServerMsg = decode(bytes)?;
    let plants_ok =
        |plants: &BTreeMap<u16, PlantState>| plants.keys().all(|&i| usize::from(i) < CHUNK_VOLUME);
    let well_formed = match &msg {
        ServerMsg::ChunkSnapshot {
            storage, plants, ..
        } => storage.is_well_formed() && plants_ok(plants),
        ServerMsg::LayerSnapshot { plants, .. } => plants_ok(plants),
        ServerMsg::Applied(result) => result.deltas.iter().all(|delta| match delta {
            ChunkDelta::Full { storage, .. } => storage.is_well_formed(),
            ChunkDelta::Cells { .. } | ChunkDelta::Layers { .. } => true,
        }),
        ServerMsg::Welcome { .. }
        | ServerMsg::Rejected { .. }
        | ServerMsg::Inventory { .. }
        | ServerMsg::PlayerStates(_)
        | ServerMsg::Saved { .. }
        | ServerMsg::SeasonStarted { .. }
        | ServerMsg::SeasonDone { .. }
        | ServerMsg::Clock(_)
        | ServerMsg::Bag(_) => true,
    };
    if well_formed {
        Ok(msg)
    } else {
        Err(CodecError::MalformedStorage)
    }
}

fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, CodecError> {
    postcard::to_stdvec(msg).map_err(CodecError::Encode)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    let (msg, rest) = postcard::take_from_bytes(bytes).map_err(CodecError::Decode)?;
    if rest.is_empty() {
        Ok(msg)
    } else {
        Err(CodecError::TrailingBytes(rest.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{IVec3, Vec3};
    use rmc_world::chunk::Chunk;
    use rmc_world::chunk::ChunkStorage;
    use rmc_world::coords::{CellAabb, CellPos, ChunkPos, LocalPos};
    use rmc_world::edit::{
        CommandId, EditCommand, EditOp, EditResult, EditStatus, PlayerId, RejectReason,
    };
    use rmc_world::items::{Bag, EquipSlot, ItemId, ItemTable, SlotRef, Thing};
    use rmc_world::materials::MaterialId;
    use rmc_world::shape::Shape;

    use std::collections::BTreeMap;

    use rmc_world::layers::{ChunkLayers, LayerId, N_GASES, N_SUBSTANCES, PlantState};
    use rmc_world::ledger::SeasonLedger;

    use crate::msg::{PROTOCOL_VERSION, SeasonReport};

    const EXTREME_CELLS: [CellPos; 5] = [
        CellPos::new(0, 0, 0),
        CellPos::new(-1, -1, -1),
        CellPos::new(255, 127, 255),
        CellPos::new(i32::MIN, 0, i32::MAX),
        CellPos::new(i32::MAX, i32::MIN, -7),
    ];

    const FLOATS: [f32; 10] = [
        0.0,
        -0.0,
        1.0,
        -1.5,
        0.1,
        f32::MIN_POSITIVE,
        1e-45, // smallest subnormal
        f32::MAX,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];

    /// Every variant index of `ClientMsg`. The match has no wildcard, so a
    /// new variant fails to compile here until the test covers it.
    fn client_variant(msg: &ClientMsg) -> usize {
        match msg {
            ClientMsg::Hello { .. } => 0,
            ClientMsg::Edit(_) => 1,
            ClientMsg::PlayerState { .. } => 2,
            ClientMsg::RequestChunk(_) => 3,
            ClientMsg::SaveRequest => 4,
            ClientMsg::AdvanceSeason => 5,
            ClientMsg::MoveItem { .. } => 6,
        }
    }
    const CLIENT_VARIANTS: usize = 7;

    fn server_variant(msg: &ServerMsg) -> usize {
        match msg {
            ServerMsg::Welcome { .. } => 0,
            ServerMsg::Rejected { .. } => 1,
            ServerMsg::ChunkSnapshot { .. } => 2,
            ServerMsg::Applied(_) => 3,
            ServerMsg::Inventory { .. } => 4,
            ServerMsg::PlayerStates(_) => 5,
            ServerMsg::Saved { .. } => 6,
            ServerMsg::LayerSnapshot { .. } => 7,
            ServerMsg::SeasonStarted { .. } => 8,
            ServerMsg::SeasonDone { .. } => 9,
            ServerMsg::Clock(_) => 10,
            ServerMsg::Bag(_) => 11,
        }
    }
    const SERVER_VARIANTS: usize = 12;

    fn ops() -> Vec<EditOp> {
        vec![
            EditOp::Set(MaterialId(0)),
            EditOp::Set(MaterialId(3)),
            EditOp::Set(MaterialId(u16::MAX)),
            EditOp::Remove,
            EditOp::Damage(0),
            EditOp::Damage(1),
            EditOp::Damage(u16::MAX),
            EditOp::Paint(MaterialId(7)),
            EditOp::Paint(MaterialId(u16::MAX)),
            EditOp::Place(MaterialId(0)),
            EditOp::Place(MaterialId(u16::MAX)),
        ]
    }

    fn shapes() -> Vec<Shape> {
        let mut shapes = Vec::new();
        for &c in &EXTREME_CELLS {
            shapes.push(Shape::Cell(c));
            shapes.push(Shape::Sphere {
                center: c,
                radius: 0,
            });
            shapes.push(Shape::Sphere {
                center: c,
                radius: u16::MAX,
            });
            for &d in &EXTREME_CELLS {
                // Includes empty boxes (min > max): the wire carries them as is.
                shapes.push(Shape::Box { min: c, max: d });
            }
        }
        shapes
    }

    fn client_samples() -> Vec<ClientMsg> {
        let mut msgs = vec![
            ClientMsg::Hello {
                protocol: PROTOCOL_VERSION,
                material_hash: 0x0123_4567_89ab_cdef,
            },
            ClientMsg::Hello {
                protocol: 0,
                material_hash: 0,
            },
            ClientMsg::Hello {
                protocol: u32::MAX,
                material_hash: u64::MAX,
            },
            ClientMsg::SaveRequest,
            ClientMsg::AdvanceSeason,
        ];
        let mut seq = 0u32;
        for op in ops() {
            for shape in shapes() {
                seq = seq.wrapping_mul(2_654_435_761).wrapping_add(1);
                let issuer = PlayerId(seq % 5);
                msgs.push(ClientMsg::Edit(EditCommand {
                    id: CommandId { issuer, seq },
                    issuer,
                    op,
                    shape,
                }));
            }
        }
        msgs.push(ClientMsg::Edit(EditCommand {
            id: CommandId {
                issuer: PlayerId(u32::MAX),
                seq: u32::MAX,
            },
            issuer: PlayerId(0),
            op: EditOp::Remove,
            shape: Shape::Cell(CellPos::new(1, 2, 3)),
        }));
        for (i, &x) in FLOATS.iter().enumerate() {
            let y = FLOATS[(i + 3) % FLOATS.len()];
            let z = FLOATS[(i + 7) % FLOATS.len()];
            msgs.push(ClientMsg::PlayerState {
                pos: Vec3::new(x, y, z),
                yaw: FLOATS[(i + 1) % FLOATS.len()],
                pitch: FLOATS[(i + 5) % FLOATS.len()],
            });
        }
        msgs.push(ClientMsg::PlayerState {
            pos: Vec3::new(f32::NAN, 1.0, -f32::NAN),
            yaw: f32::NAN,
            pitch: -0.0,
        });
        for &c in &EXTREME_CELLS {
            msgs.push(ClientMsg::RequestChunk(ChunkPos(c.0)));
        }
        let slots = [
            SlotRef::Hotbar(0),
            SlotRef::Hotbar(u8::MAX),
            SlotRef::Pack(0),
            SlotRef::Pack(u16::MAX),
            SlotRef::Equip(EquipSlot::Head),
            SlotRef::Equip(EquipSlot::Hands),
        ];
        for (i, &from) in slots.iter().enumerate() {
            msgs.push(ClientMsg::MoveItem {
                from,
                to: slots[(i + 1) % slots.len()],
                count: [None, Some(0), Some(1), Some(u32::MAX)][i % 4],
            });
        }
        msgs
    }

    /// Bags: empty, a starting kit, and full with materials of every size.
    fn sample_bags() -> Vec<Bag> {
        let table = ItemTable::from_ron_str(
            r#"(
                bag: (hotbar: 9, pack: 4),
                material_stack: 4000000000,
                items: [
                    (key: "pick", name: "Pick", tags: ["tool"], stack: 1, tool: Some("pickaxe")),
                    (key: "cap", name: "Cap", tags: ["clothing"], stack: 1, slot: Some(Head)),
                    (key: "boots", name: "Boots", tags: ["clothing"], stack: 1, slot: Some(Feet)),
                ],
                start: [("pick", 1), ("cap", 2), ("boots", 1)],
            )"#,
        )
        .unwrap();
        let mut full = Bag::starter(&table);
        for (i, count) in [1, 999, 4_000_000_000, 7].into_iter().enumerate() {
            full.insert(&table, Thing::Material(MaterialId(i as u16 * 300)), count);
        }
        full.insert(&table, Thing::Material(MaterialId(u16::MAX)), 4_000_000_000);
        full.insert(&table, Thing::Item(ItemId(u16::MAX)), 3);
        vec![
            Bag::default(),
            Bag::new(table.bag()),
            Bag::starter(&table),
            full,
        ]
    }

    /// A paletted chunk with `materials` cycling through every cell.
    fn paletted(materials: &[MaterialId]) -> ChunkStorage {
        let mut chunk = Chunk::uniform(materials[0]);
        for i in 0..32 * 32 * 32u32 {
            let m = materials[(i as usize * 7 + i as usize / 33) % materials.len()];
            chunk
                .storage
                .set(LocalPos::new(i % 32, (i / 1024) % 32, (i / 32) % 32), m);
        }
        assert!(matches!(chunk.storage, ChunkStorage::Paletted { .. }));
        assert!(chunk.storage.is_well_formed());
        chunk.storage
    }

    fn storages() -> Vec<ChunkStorage> {
        let many: Vec<MaterialId> = (0..300).map(MaterialId).collect();
        vec![
            ChunkStorage::Uniform(MaterialId::AIR),
            ChunkStorage::Uniform(MaterialId(u16::MAX)),
            paletted(&[MaterialId(0), MaterialId(1)]),
            paletted(&[MaterialId(2), MaterialId(9), MaterialId(4)]),
            paletted(&many),
        ]
    }

    fn command(seq: u32) -> CommandId {
        CommandId {
            issuer: PlayerId(seq % 3),
            seq,
        }
    }

    fn server_samples() -> Vec<ServerMsg> {
        let mut msgs = vec![
            ServerMsg::Welcome {
                player: PlayerId(1),
                seed: 42,
                bounds: CellAabb::PHASE1_DEFAULT,
                gen_params_hash: 0xdead_beef_cafe_f00d,
            },
            ServerMsg::Welcome {
                player: PlayerId(u32::MAX),
                seed: u64::MAX,
                bounds: CellAabb::new(
                    CellPos::new(i32::MIN, i32::MIN, i32::MIN),
                    CellPos::new(i32::MAX, i32::MAX, i32::MAX),
                ),
                gen_params_hash: 0,
            },
            ServerMsg::Rejected {
                reason: String::new(),
            },
            ServerMsg::Rejected {
                reason: "material hash mismatch: expected 0x1, got 0x2".into(),
            },
            ServerMsg::Rejected {
                reason: "protocolo — versión ✗ 鉱山 🪨\n\t\"quoted\"\0".into(),
            },
            ServerMsg::Rejected {
                reason: "x".repeat(70_000),
            },
            ServerMsg::Inventory { items: Vec::new() },
            ServerMsg::Inventory {
                items: vec![
                    (MaterialId(2), 1),
                    (MaterialId(5), 127),
                    (MaterialId(6), 128),
                    (MaterialId(u16::MAX), u32::MAX),
                ],
            },
            ServerMsg::PlayerStates(Vec::new()),
            ServerMsg::PlayerStates(vec![
                (PlayerId(0), Vec3::new(1.0, 2.0, 3.0), 0.5, -0.25),
                (
                    PlayerId(7),
                    Vec3::new(-0.0, f32::MAX, 1e-45),
                    f32::INFINITY,
                    -0.0,
                ),
                (
                    PlayerId(u32::MAX),
                    Vec3::splat(f32::NAN),
                    f32::NAN,
                    f32::NEG_INFINITY,
                ),
            ]),
            ServerMsg::Saved {
                ok: true,
                path: "saves/default".into(),
            },
            ServerMsg::Saved {
                ok: false,
                path: String::new(),
            },
            ServerMsg::Saved {
                ok: true,
                path: r"C:\Users\miner\saves\wörld 1".into(),
            },
        ];
        for (i, storage) in storages().into_iter().enumerate() {
            let c = EXTREME_CELLS[i % EXTREME_CELLS.len()];
            msgs.push(ServerMsg::ChunkSnapshot {
                pos: ChunkPos(c.0),
                revision: [0, 1, 4097, u64::MAX, 1 << 40][i % 5],
                eco_revision: 0,
                storage,
                layers: ChunkLayers::default(),
                plants: BTreeMap::new(),
            });
        }

        // Applied: every status, empty and full delta lists, both delta kinds.
        let reasons = [
            RejectReason::UnknownMaterial(MaterialId(999)),
            RejectReason::UnknownMaterial(MaterialId(u16::MAX)),
            RejectReason::TooManyCells,
            RejectReason::OutsideBounds,
            RejectReason::PlaceNeedsCell,
            RejectReason::PlaceNotSolid(MaterialId(u16::MAX)),
            RejectReason::PlaceOnSolid(CellPos::new(i32::MIN, 0, i32::MAX)),
            RejectReason::NotInInventory(MaterialId(4)),
        ];
        for (i, reason) in reasons.into_iter().enumerate() {
            msgs.push(ServerMsg::Applied(EditResult {
                command: command(i as u32),
                status: EditStatus::Rejected(reason),
                deltas: Vec::new(),
                yields: Vec::new(),
                ledger: Box::default(),
            }));
        }
        msgs.push(ServerMsg::Applied(EditResult {
            command: command(10),
            status: EditStatus::Applied,
            deltas: Vec::new(),
            yields: Vec::new(),
            ledger: Box::default(),
        }));
        msgs.push(ServerMsg::Applied(EditResult {
            command: command(11),
            status: EditStatus::Applied,
            deltas: vec![ChunkDelta::Cells {
                pos: ChunkPos(IVec3::new(0, 0, 0)),
                revision: 1,
                changes: Vec::new(),
            }],
            yields: Vec::new(),
            ledger: Box::default(),
        }));
        let mut deltas = vec![
            ChunkDelta::Cells {
                pos: ChunkPos(IVec3::new(-1, 0, -1)),
                revision: 3,
                changes: vec![
                    (0, MaterialId::AIR),
                    (127, MaterialId(4)),
                    (128, MaterialId(5)),
                ],
            },
            ChunkDelta::Cells {
                pos: ChunkPos(IVec3::new(0, 0, 0)),
                revision: u64::MAX,
                changes: (0..4096u16).map(|i| (i * 8, MaterialId(i % 13))).collect(),
            },
            ChunkDelta::Cells {
                pos: ChunkPos(IVec3::new(i32::MAX, i32::MIN, 0)),
                revision: 2,
                changes: vec![(32_767, MaterialId(u16::MAX)), (u16::MAX, MaterialId(1))],
            },
        ];
        for (i, storage) in storages().into_iter().enumerate() {
            deltas.push(ChunkDelta::Full {
                pos: ChunkPos(IVec3::new(i as i32, 1, -(i as i32))),
                revision: 1 + i as u64 * 1000,
                storage,
            });
        }
        msgs.push(ServerMsg::Applied(EditResult {
            command: CommandId {
                issuer: PlayerId(u32::MAX),
                seq: u32::MAX,
            },
            status: EditStatus::Applied,
            deltas,
            yields: vec![
                (MaterialId(2), 1),
                (MaterialId(5), 4096),
                (MaterialId(u16::MAX), u32::MAX),
            ],
            ledger: Box::default(),
        }));
        msgs.extend(phase2_server_samples());
        msgs.extend(
            sample_bags()
                .into_iter()
                .map(|b| ServerMsg::Bag(Box::new(b))),
        );
        msgs
    }

    /// Layers with every id kind materialized and a few nonzero cells.
    fn sample_layers(salt: u16) -> ChunkLayers {
        let mut layers = ChunkLayers::default();
        layers.materialize(LayerId::Temperature);
        layers.materialize(LayerId::Light);
        let u8_layers = [
            LayerId::Moisture,
            LayerId::Regolith,
            LayerId::Substance(0),
            LayerId::Substance(N_SUBSTANCES as u8 - 1),
            LayerId::Gas(0),
            LayerId::Gas(N_GASES as u8 - 1),
            LayerId::Light,
            LayerId::Growth,
        ];
        for (k, id) in u8_layers.into_iter().enumerate() {
            let local = LocalPos::from_index((k as u16 * 4099 + salt) % 32_768);
            layers.set_u8(id, local, (k as u8).wrapping_mul(37).wrapping_add(1));
            layers.set_u8(id, LocalPos::from_index(32_767), u8::MAX);
        }
        for (i, t) in [i16::MIN, -1, 0, 1, i16::MAX].into_iter().enumerate() {
            layers.set_i16(LayerId::Temperature, LocalPos::from_index(i as u16 * 7), t);
        }
        layers.set_i16(
            LayerId::Temperature,
            LocalPos::from_index(100),
            salt as i16 * 13,
        );
        layers
    }

    fn sample_plants() -> BTreeMap<u16, PlantState> {
        let mut held = [0u8; N_SUBSTANCES];
        held[0] = 40;
        held[N_SUBSTANCES - 1] = u8::MAX;
        BTreeMap::from([
            (0, PlantState::default()),
            (
                1234,
                PlantState {
                    species: 2,
                    stress: 15,
                    age_seasons: 3,
                    held,
                },
            ),
            (
                32_767,
                PlantState {
                    species: u8::MAX,
                    stress: u8::MAX,
                    age_seasons: u8::MAX,
                    held: [u8::MAX; N_SUBSTANCES],
                },
            ),
        ])
    }

    /// A ledger with every field nonzero, negative and extreme values
    /// included.
    fn sample_ledger(k: i64) -> SeasonLedger {
        fn arr<const N: usize>(base: i64, k: i64) -> [i64; N] {
            std::array::from_fn(|i| base * (i as i64 + 1) - k)
        }
        SeasonLedger {
            rain_in: 1 + k,
            steam_vent_out: 2,
            water_edit_in: 3,
            water_edit_out: 4,
            water_saturation_loss: 5,
            evap_to_steam: 6,
            boil_to_steam: 7,
            condense_from_steam: 8,
            weather_in: arr(1, k),
            litter_in: arr(2, k),
            uptake_sink: arr(3, k),
            harvest_out: arr(4, k),
            edit_in: arr(5, k),
            edit_out: arr(6, k),
            react_delta: arr(-7, k),
            saturation_loss: arr(8, k),
            emit_in: arr(9, k),
            vent_out: arr(10, k),
            react_out: arr(11, k),
            react_in: arr(12, k),
            gas_edit_in: arr(13, k),
            gas_edit_out: arr(14, k),
            gas_saturation_loss: arr(15, k),
            magma_set: i64::MAX,
            magma_melt_in: i64::MIN,
            magma_edit: -1,
            transitions: vec![
                (MaterialId(1), MaterialId(2), 3),
                (MaterialId(u16::MAX), MaterialId(0), u32::MAX),
            ],
            weathered_cells: 9,
            plants_born: 10,
            plants_died: 11,
            ignitions: u32::MAX,
        }
    }

    /// Every Phase 2 server message shape (SAD-P2 §9).
    fn phase2_server_samples() -> Vec<ServerMsg> {
        let mut msgs = Vec::new();
        for (i, storage) in storages().into_iter().enumerate() {
            let c = EXTREME_CELLS[i % EXTREME_CELLS.len()];
            msgs.push(ServerMsg::ChunkSnapshot {
                pos: ChunkPos(c.0),
                revision: 5 + i as u64,
                eco_revision: [1, 0, u64::MAX, 77, 1 << 50][i % 5],
                storage,
                layers: sample_layers(i as u16),
                plants: if i % 2 == 0 {
                    sample_plants()
                } else {
                    BTreeMap::new()
                },
            });
        }
        msgs.push(ServerMsg::LayerSnapshot {
            pos: ChunkPos(IVec3::new(-3, 1, 2)),
            eco_revision: 9,
            layers: sample_layers(3),
            plants: sample_plants(),
        });
        msgs.push(ServerMsg::LayerSnapshot {
            pos: ChunkPos(IVec3::new(i32::MIN, 0, i32::MAX)),
            eco_revision: u64::MAX,
            layers: ChunkLayers::default(),
            plants: BTreeMap::new(),
        });
        msgs.push(ServerMsg::Clock(crate::msg::GameClock::default()));
        msgs.push(ServerMsg::Clock(crate::msg::GameClock {
            season: u32::MAX,
            day: 6,
            days: 7,
            hour: 23,
            hours: 24,
            air_temp: i16::MIN,
            rain: u8::MAX,
            peak_rain: 48,
            snow: true,
            daylight: (6, 19),
        }));
        msgs.push(ServerMsg::SeasonStarted { season: 0 });
        msgs.push(ServerMsg::SeasonStarted { season: u32::MAX });
        msgs.push(ServerMsg::SeasonDone {
            season: 1,
            report: Box::default(),
            changed_chunks: 0,
        });
        msgs.push(ServerMsg::SeasonDone {
            season: u32::MAX,
            report: Box::new(SeasonReport {
                ledger: sample_ledger(1),
                between_seasons: sample_ledger(-5),
                substep_ms: vec![0, 12, u32::MAX],
                active_cells_per_sweep: vec![1, 2, 3, 4, 5, 6, 7],
                active_chunks_per_jacobi: vec![256; 5],
            }),
            changed_chunks: u32::MAX,
        });
        // Applied with Layers deltas, plant changes and a non-empty ledger.
        msgs.push(ServerMsg::Applied(EditResult {
            command: command(20),
            status: EditStatus::Applied,
            deltas: vec![
                ChunkDelta::Cells {
                    pos: ChunkPos(IVec3::ZERO),
                    revision: 2,
                    changes: vec![(5, MaterialId(3))],
                },
                ChunkDelta::Layers {
                    pos: ChunkPos(IVec3::ZERO),
                    eco_revision: 1,
                    changes: vec![
                        (5, LayerId::Temperature, i32::from(i16::MIN)),
                        (5, LayerId::Moisture, 255),
                        (6, LayerId::Substance(9), 0),
                        (7, LayerId::Gas(3), 128),
                        (32_767, LayerId::Growth, 1),
                    ],
                    plants: vec![(5, None), (6, Some(sample_plants()[&1234]))],
                },
                ChunkDelta::Layers {
                    pos: ChunkPos(IVec3::X),
                    eco_revision: u64::MAX,
                    changes: Vec::new(),
                    plants: Vec::new(),
                },
            ],
            yields: vec![(MaterialId(8), 5)],
            ledger: Box::new(sample_ledger(2)),
        }));
        msgs.push(ServerMsg::Applied(EditResult {
            command: command(21),
            status: EditStatus::Rejected(RejectReason::InvalidLayerValue {
                layer: LayerId::Gas(9),
                value: i32::MIN,
            }),
            deltas: Vec::new(),
            yields: Vec::new(),
            ledger: Box::default(),
        }));
        msgs
    }

    /// `AdvanceSeason` and `SetLayer` edits over every layer kind.
    fn phase2_client_samples() -> Vec<ClientMsg> {
        let layer_ops = [
            (LayerId::Temperature, i32::from(i16::MIN)),
            (LayerId::Temperature, i32::from(i16::MAX)),
            (LayerId::Moisture, 255),
            (LayerId::Regolith, 0),
            (LayerId::Substance(0), 1),
            (LayerId::Substance(9), 200),
            (LayerId::Gas(0), 64),
            (LayerId::Gas(3), 3),
            (LayerId::Light, 255),
            (LayerId::Growth, 7),
            // Out of range: the wire carries it, the host rejects it.
            (LayerId::Substance(u8::MAX), i32::MAX),
        ];
        let shapes = shapes();
        let mut msgs = vec![ClientMsg::AdvanceSeason];
        for (i, (layer, value)) in layer_ops.into_iter().enumerate() {
            let issuer = PlayerId(i as u32);
            msgs.push(ClientMsg::Edit(EditCommand {
                id: CommandId {
                    issuer,
                    seq: i as u32,
                },
                issuer,
                op: EditOp::SetLayer { layer, value },
                shape: shapes[i % shapes.len()],
            }));
        }
        msgs
    }

    /// Checks one message: it encodes, decodes back to an equal value, and
    /// re-encodes to identical bytes (which also checks the bit patterns of
    /// floats that `==` cannot: -0.0 and NaN). Every strict prefix of the
    /// bytes, and the bytes plus one trailing byte, fail to decode.
    fn check_roundtrip<M: PartialEq + std::fmt::Debug>(
        msg: &M,
        encode: impl Fn(&M) -> Result<Vec<u8>, CodecError>,
        decode: impl Fn(&[u8]) -> Result<M, CodecError>,
        has_nan: bool,
    ) -> usize {
        let bytes = encode(msg).unwrap_or_else(|e| panic!("encode {msg:?}: {e}"));
        assert!(!bytes.is_empty(), "empty encoding for {msg:?}");
        assert_eq!(encode(msg).unwrap(), bytes, "encoding is not deterministic");
        let back = decode(&bytes).unwrap_or_else(|e| panic!("decode {msg:?}: {e}"));
        if !has_nan {
            assert_eq!(&back, msg);
        }
        assert_eq!(
            encode(&back).unwrap(),
            bytes,
            "re-encoding differs for {msg:?}"
        );

        // Long messages (whole chunks) check a sample of prefixes to stay fast.
        let step = (bytes.len() / 512).max(1);
        for len in (0..bytes.len()).step_by(step).chain([bytes.len() - 1]) {
            assert!(
                decode(&bytes[..len]).is_err(),
                "a {len}-byte prefix of {} bytes decoded",
                bytes.len()
            );
        }
        for extra in [0u8, 1, 0xff] {
            let mut longer = bytes.clone();
            longer.push(extra);
            assert!(
                matches!(decode(&longer), Err(CodecError::TrailingBytes(1))),
                "trailing byte {extra:#x} accepted"
            );
        }
        bytes.len()
    }

    fn client_has_nan(msg: &ClientMsg) -> bool {
        matches!(msg, ClientMsg::PlayerState { pos, yaw, pitch }
            if pos.is_nan() || yaw.is_nan() || pitch.is_nan())
    }

    fn server_has_nan(msg: &ServerMsg) -> bool {
        matches!(msg, ServerMsg::PlayerStates(states)
            if states.iter().any(|(_, p, y, x)| p.is_nan() || y.is_nan() || x.is_nan()))
    }

    #[test]
    fn roundtrip_every_client_variant() {
        let samples = client_samples();
        let mut seen = [0usize; CLIENT_VARIANTS];
        for msg in &samples {
            seen[client_variant(msg)] += 1;
            check_roundtrip(msg, encode_client, decode_client, client_has_nan(msg));
        }
        assert!(
            seen.iter().all(|&n| n > 0),
            "variants not covered: {seen:?}"
        );

        // A negative zero survives as negative zero, not just as `== 0.0`.
        let msg = ClientMsg::PlayerState {
            pos: Vec3::new(-0.0, 0.0, -0.0),
            yaw: -0.0,
            pitch: f32::from_bits(0x7fc0_1234),
        };
        let ClientMsg::PlayerState { pos, yaw, pitch } =
            decode_client(&encode_client(&msg).unwrap()).unwrap()
        else {
            panic!("decoded to another variant");
        };
        assert_eq!(pos.x.to_bits(), (-0.0f32).to_bits());
        assert_eq!(pos.y.to_bits(), 0.0f32.to_bits());
        assert_eq!(yaw.to_bits(), (-0.0f32).to_bits());
        assert_eq!(pitch.to_bits(), 0x7fc0_1234);

        // Garbage is an error, never a panic.
        assert!(decode_client(&[]).is_err());
        assert!(
            decode_client(&[CLIENT_VARIANTS as u8]).is_err(),
            "unknown tag"
        );
        assert!(decode_client(&[0xff; 16]).is_err());
        // SaveRequest is a lone tag byte; any second byte is trailing.
        let save = encode_client(&ClientMsg::SaveRequest).unwrap();
        assert_eq!(save.len(), 1);
    }

    #[test]
    fn roundtrip_every_server_variant() {
        let samples = server_samples();
        let mut seen = [0usize; SERVER_VARIANTS];
        for msg in &samples {
            seen[server_variant(msg)] += 1;
            check_roundtrip(msg, encode_server, decode_server, server_has_nan(msg));
        }
        assert!(
            seen.iter().all(|&n| n > 0),
            "variants not covered: {seen:?}"
        );

        // Decoded storage keeps its exact structure (no compaction on the wire).
        for storage in storages() {
            let msg = ServerMsg::ChunkSnapshot {
                pos: ChunkPos(IVec3::new(2, 0, 3)),
                revision: 9,
                eco_revision: 0,
                storage: storage.clone(),
                layers: ChunkLayers::default(),
                plants: BTreeMap::new(),
            };
            let ServerMsg::ChunkSnapshot { storage: back, .. } =
                decode_server(&encode_server(&msg).unwrap()).unwrap()
            else {
                panic!("decoded to another variant");
            };
            assert_eq!(back, storage);
            assert_eq!(back.hash(), storage.hash());
        }

        // Malformed storage decodes as postcard but is refused, both in a
        // snapshot and in a Full delta.
        let ChunkStorage::Paletted {
            palette,
            bits,
            data,
        } = paletted(&[MaterialId(1), MaterialId(2), MaterialId(3)])
        else {
            unreachable!()
        };
        let malformed = [
            ChunkStorage::Paletted {
                palette: palette.clone(),
                bits: 3,
                data: data.clone(),
            },
            ChunkStorage::Paletted {
                palette: palette.clone(),
                bits,
                data: data[1..].to_vec(),
            },
            ChunkStorage::Paletted {
                palette: Vec::new(),
                bits,
                data: data.clone(),
            },
            ChunkStorage::Paletted {
                palette: palette[..2].to_vec(),
                bits,
                data: data.clone(),
            },
        ];
        for storage in malformed {
            let snapshot = ServerMsg::ChunkSnapshot {
                pos: ChunkPos(IVec3::ZERO),
                revision: 1,
                eco_revision: 0,
                storage: storage.clone(),
                layers: ChunkLayers::default(),
                plants: BTreeMap::new(),
            };
            let bytes = encode_server(&snapshot).unwrap();
            assert!(matches!(
                decode_server(&bytes),
                Err(CodecError::MalformedStorage)
            ));
            let applied = ServerMsg::Applied(EditResult {
                command: command(1),
                status: EditStatus::Applied,
                deltas: vec![
                    ChunkDelta::Cells {
                        pos: ChunkPos(IVec3::ZERO),
                        revision: 1,
                        changes: vec![(0, MaterialId(1))],
                    },
                    ChunkDelta::Full {
                        pos: ChunkPos(IVec3::X),
                        revision: 1,
                        storage,
                    },
                ],
                yields: Vec::new(),
                ledger: Box::default(),
            });
            let bytes = encode_server(&applied).unwrap();
            assert!(matches!(
                decode_server(&bytes),
                Err(CodecError::MalformedStorage)
            ));
        }

        assert!(decode_server(&[]).is_err());
        assert!(
            decode_server(&[SERVER_VARIANTS as u8]).is_err(),
            "unknown tag"
        );
        assert!(decode_server(&[0xff; 16]).is_err());
        // A huge declared length with no data is an error, not an allocation.
        assert!(decode_server(&[1, 0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
    }

    #[test]
    fn roundtrip_phase2_variants() {
        // Every Phase 2 shape round-trips bit-exactly.
        for msg in phase2_client_samples() {
            check_roundtrip(&msg, encode_client, decode_client, false);
        }
        let samples = phase2_server_samples();
        for msg in &samples {
            check_roundtrip(msg, encode_server, decode_server, false);
        }
        assert_eq!(PROTOCOL_VERSION, 2);
        let kinds: Vec<usize> = samples.iter().map(server_variant).collect();
        for want in [2, 3, 7, 8, 9] {
            assert!(kinds.contains(&want), "variant {want} not sampled");
        }
        let advance = encode_client(&ClientMsg::AdvanceSeason).unwrap();
        assert_eq!(
            advance,
            [client_variant(&ClientMsg::AdvanceSeason) as u8],
            "AdvanceSeason is a lone tag"
        );

        // Layers keep their materialized ids and content hash; plants and
        // ledgers come back equal.
        for msg in &samples {
            let back = decode_server(&encode_server(msg).unwrap()).unwrap();
            match (msg, &back) {
                (
                    ServerMsg::ChunkSnapshot {
                        layers: a,
                        plants: pa,
                        ..
                    },
                    ServerMsg::ChunkSnapshot {
                        layers: b,
                        plants: pb,
                        ..
                    },
                )
                | (
                    ServerMsg::LayerSnapshot {
                        layers: a,
                        plants: pa,
                        ..
                    },
                    ServerMsg::LayerSnapshot {
                        layers: b,
                        plants: pb,
                        ..
                    },
                ) => {
                    assert_eq!(a.hash(), b.hash());
                    assert_eq!(a.ids().collect::<Vec<_>>(), b.ids().collect::<Vec<_>>());
                    assert_eq!(pa, pb);
                }
                _ => assert_eq!(&back, msg),
            }
        }

        // A snapshot whose plant index lies outside the chunk is refused.
        let mut plants = sample_plants();
        plants.insert(CHUNK_VOLUME as u16, PlantState::default());
        for msg in [
            ServerMsg::ChunkSnapshot {
                pos: ChunkPos(IVec3::ZERO),
                revision: 1,
                eco_revision: 1,
                storage: ChunkStorage::Uniform(MaterialId::AIR),
                layers: ChunkLayers::default(),
                plants: plants.clone(),
            },
            ServerMsg::LayerSnapshot {
                pos: ChunkPos(IVec3::ZERO),
                eco_revision: 1,
                layers: ChunkLayers::default(),
                plants,
            },
        ] {
            let bytes = encode_server(&msg).unwrap();
            assert!(matches!(
                decode_server(&bytes),
                Err(CodecError::MalformedStorage)
            ));
        }
    }
}
