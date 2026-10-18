//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod controller;
pub mod editor;
pub mod flycam;
pub mod hud;
pub mod interp;
pub mod material;
pub mod mesher;
pub mod meshpipe;
pub mod net;
pub mod pickaxe;
pub mod render;
pub mod replica;

pub use controller::{
    Aabb, ControllerConfig, ControllerPlugin, ControllerSystems, EYE_HEIGHT_M, FIXED_HZ, MoveInput,
    PLAYER_HEIGHT_M, PLAYER_STATE_HZ, PLAYER_WIDTH_M, Player, STEP_SMOOTH_S,
};
pub use editor::{
    BRUSH_BUTTON, BRUSH_REACH_M, BrushOp, BrushShape, EDITOR_BINDINGS, EditorAction, EditorPlugin,
    EditorState, EditorSystems, GizmoToggles, MAX_BRUSH_SIZE, SNAP_RADIUS_CELLS, TOGGLE_EDITOR_KEY,
    brush_command, free_feet_near,
};
pub use flycam::{FLY_TOGGLE_KEY, FlyCam, FlyCamPlugin, FlyCamSystems};
pub use hud::{
    ClientInventory, DamageEstimate, DamageProgress, HudItem, HudPlugin, HudRoot, HudState,
    HudSystems,
};
pub use interp::{InterpolatedEye, render_pos};
pub use material::{
    ChunkEmissiveMaterial, EmissiveChunkMaterial, EmissiveFromAlpha, EmissiveMaterialPlugin,
};
pub use mesher::{ChunkMesher, GreedyMesher, MeshData, MeshInput};
pub use meshpipe::{
    ChunkMesh, ChunkMeshState, MeshBudget, MeshPipelinePlugin, MeshQueue, MeshStats, MeshSystems,
    Mesher,
};
pub use net::{
    ClientMaterials, CommandSeq, FromHost, HostConnection, HostLink, NetPlugin, NetSystems, ToHost,
};
pub use pickaxe::{
    PickTarget, Pickaxe, PickaxePlugin, PickaxeSystems, SWING_BUTTON, SWING_SLACK, SwingState,
};
pub use render::{CameraStart, GameCamera, GameRenderPlugin};
pub use replica::{Replica, ReplicaError, ReplicaPlugin};
