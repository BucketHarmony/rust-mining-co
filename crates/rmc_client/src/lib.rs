//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod mesher;
pub mod meshpipe;
pub mod net;
pub mod replica;

pub use mesher::{ChunkMesher, GreedyMesher, MeshData, MeshInput};
pub use meshpipe::{
    ChunkMesh, ChunkMeshState, MeshBudget, MeshPipelinePlugin, MeshQueue, MeshStats, MeshSystems,
    Mesher,
};
pub use net::{ClientMaterials, FromHost, HostConnection, HostLink, NetPlugin, NetSystems, ToHost};
pub use replica::{Replica, ReplicaError, ReplicaPlugin};
