//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod flycam;
pub mod mesher;
pub mod meshpipe;
pub mod net;
pub mod render;
pub mod replica;

pub use flycam::{FLY_TOGGLE_KEY, FlyCam, FlyCamPlugin, FlyCamSystems};
pub use mesher::{ChunkMesher, GreedyMesher, MeshData, MeshInput};
pub use meshpipe::{
    ChunkMesh, ChunkMeshState, MeshBudget, MeshPipelinePlugin, MeshQueue, MeshStats, MeshSystems,
    Mesher,
};
pub use net::{ClientMaterials, FromHost, HostConnection, HostLink, NetPlugin, NetSystems, ToHost};
pub use render::{CameraStart, GameCamera, GameRenderPlugin};
pub use replica::{Replica, ReplicaError, ReplicaPlugin};
