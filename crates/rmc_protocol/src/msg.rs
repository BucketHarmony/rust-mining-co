//! Client and server messages (SAD §8.4).

use glam::Vec3;
use rmc_world::chunk::ChunkStorage;
use rmc_world::coords::{CellAabb, ChunkPos};
use rmc_world::edit::{EditCommand, EditResult, PlayerId};
use rmc_world::materials::MaterialId;
use serde::{Deserialize, Serialize};

/// Sent in `Hello`; the host rejects a client whose version differs.
pub const PROTOCOL_VERSION: u32 = 1;

/// Width and depth of a player's box, in meters (SAD §11.4). The box's
/// bottom face is centered on `PlayerState::pos`, the feet.
pub const PLAYER_WIDTH_M: f32 = 0.6;
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
    /// The join encoding and the save encoding of a chunk.
    ChunkSnapshot {
        pos: ChunkPos,
        revision: u64,
        storage: ChunkStorage,
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
}
