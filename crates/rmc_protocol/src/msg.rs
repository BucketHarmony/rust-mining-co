//! Client and server messages (SAD §8.4, SAD-P2 §9).

use std::collections::BTreeMap;

use glam::Vec3;
use rmc_world::chunk::{Chunk, ChunkStorage};
use rmc_world::coords::{CellAabb, ChunkPos};
use rmc_world::edit::{EditCommand, EditResult, PlayerId};
use rmc_world::layers::{ChunkLayers, PlantState};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::MaterialId;
use serde::{Deserialize, Serialize};

/// Sent in `Hello`; the host rejects a client whose version differs.
/// Version 2 is Phase 2: snapshots carry layers and plants, and seasons.
pub const PROTOCOL_VERSION: u32 = 2;

/// Width and depth of a player's box, in meters. The box's bottom face is
/// centered on `PlayerState::pos`, the feet. Narrower than a cell, so the
/// player fits a one-cell hole or shaft: an owner change (F094) to SAD
/// §11.4's 0.6 m.
pub const PLAYER_WIDTH_M: f32 = 0.45;
/// Height of a player's box, in meters (SAD §11.4).
pub const PLAYER_HEIGHT_M: f32 = 1.7;

/// Client → host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on a connection.
    Hello {
        protocol: u32,
        material_hash: u64,
    },
    Edit(EditCommand),
    /// Where the player is: `pos` is the feet (see [`PLAYER_WIDTH_M`]).
    PlayerState {
        pos: Vec3,
        yaw: f32,
        pitch: f32,
    },
    /// The replica asks for a snapshot after a revision gap.
    RequestChunk(ChunkPos),
    /// Editor "Save" button.
    SaveRequest,
    /// Editor "Season" button: run one season tick (SAD-P2 §9).
    AdvanceSeason,
}

/// What a season tick did (SAD-P2 §9): its ledger, the ledger of the edits
/// made since the previous season, and timing and activity per sub-step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonReport {
    pub ledger: SeasonLedger,
    pub between_seasons: SeasonLedger,
    pub substep_ms: Vec<u32>,
    pub active_cells_per_sweep: Vec<u32>,
    pub active_chunks_per_jacobi: Vec<u32>,
}

/// Host → client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        player: PlayerId,
        seed: u64,
        bounds: CellAabb,
        gen_params_hash: u64,
    },
    /// `Hello` mismatch.
    Rejected {
        reason: String,
    },
    /// The join encoding of a chunk (v2): cells, layers and plants.
    ChunkSnapshot {
        pos: ChunkPos,
        revision: u64,
        eco_revision: u64,
        storage: ChunkStorage,
        layers: ChunkLayers,
        plants: BTreeMap<u16, PlantState>,
    },
    /// A chunk whose only change in a season was ecological.
    LayerSnapshot {
        pos: ChunkPos,
        eco_revision: u64,
        layers: ChunkLayers,
        plants: BTreeMap<u16, PlantState>,
    },
    Applied(EditResult),
    Inventory {
        items: Vec<(MaterialId, u32)>,
    },
    /// Unused in Phase 1; the shape is reserved for co-op.
    PlayerStates(Vec<(PlayerId, Vec3, f32, f32)>),
    Saved {
        ok: bool,
        path: String,
    },
    /// A season tick began; edits are paused until `SeasonDone`.
    SeasonStarted {
        season: u32,
    },
    /// A season tick finished, after its snapshots.
    SeasonDone {
        season: u32,
        report: Box<SeasonReport>,
        changed_chunks: u32,
    },
}

impl ServerMsg {
    /// The v2 join encoding of a resident chunk: cells, layers and plants
    /// with both revisions.
    pub fn chunk_snapshot(pos: ChunkPos, chunk: &Chunk) -> Self {
        ServerMsg::ChunkSnapshot {
            pos,
            revision: chunk.revision,
            eco_revision: chunk.eco_revision,
            storage: chunk.storage.clone(),
            layers: chunk.layers.clone(),
            plants: chunk.plants.clone(),
        }
    }
}
