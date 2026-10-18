//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod controller;
pub mod flycam;
pub mod interp;
pub mod material;
pub mod mesher;
pub mod meshpipe;
pub mod net;
pub mod render;
pub mod replica;

pub use controller::{
    Aabb, ControllerConfig, ControllerPlugin, ControllerSystems, EYE_HEIGHT_M, FIXED_HZ, MoveInput,
    PLAYER_HEIGHT_M, PLAYER_WIDTH_M, Player, STEP_SMOOTH_S,
};
pub use flycam::{FLY_TOGGLE_KEY, FlyCam, FlyCamPlugin, FlyCamSystems};
pub use interp::{InterpolatedEye, render_pos};
pub use material::{
    ChunkEmissiveMaterial, EmissiveChunkMaterial, EmissiveFromAlpha, EmissiveMaterialPlugin,
};
pub use mesher::{ChunkMesher, GreedyMesher, MeshData, MeshInput};
pub use meshpipe::{
    ChunkMesh, ChunkMeshState, MeshBudget, MeshPipelinePlugin, MeshQueue, MeshStats, MeshSystems,
    Mesher,
};
pub use net::{ClientMaterials, FromHost, HostConnection, HostLink, NetPlugin, NetSystems, ToHost};
pub use render::{CameraStart, GameCamera, GameRenderPlugin};
pub use replica::{Replica, ReplicaError, ReplicaPlugin};
