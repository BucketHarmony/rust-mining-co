//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod controller;
pub mod cursor;
pub mod editor;
pub mod flycam;
pub mod gizmo;
pub mod hud;
pub mod interp;
pub mod material;
pub mod mesher;
pub mod meshpipe;
pub mod net;
pub mod panel;
pub mod pickaxe;
pub mod render;
pub mod replica;
pub mod save;

pub use controller::{
    Aabb, ControllerConfig, ControllerPlugin, ControllerSystems, EYE_HEIGHT_M, FIXED_HZ, MoveInput,
    PLAYER_HEIGHT_M, PLAYER_STATE_HZ, PLAYER_WIDTH_M, Player, STEP_SMOOTH_S,
};
pub use cursor::{
    CursorGrab, CursorPlugin, CursorSystems, GRAB_BUTTON, GRABBED_MODE, RELEASE_KEY, look_allowed,
    swing_allowed,
};
pub use editor::{
    ADVANCE_SEASON_LABEL, BRUSH_BUTTON, BRUSH_REACH_M, BrushOp, BrushShape, EDITOR_BINDINGS,
    EditorAction, EditorPlugin, EditorState, EditorSystems, GizmoToggles, MAX_BRUSH_SIZE,
    SNAP_RADIUS_CELLS, TOGGLE_EDITOR_KEY, brush_command, brush_target, free_feet_near,
};
pub use flycam::{FLY_TOGGLE_KEY, FlyCam, FlyCamPlugin, FlyCamSystems};
pub use gizmo::{
    BRUSH_CELL_COLOR, CELL_GIZMO_SCALE, CHUNK_BOUNDS_COLOR, CHUNK_BOUNDS_RADIUS, EditorGizmoPlugin,
    EditorGizmos, GizmoSystems, HIT_CELL_COLOR, MESH_QUEUE_INSET, TargetGizmo, mesh_state_color,
};
pub use hud::{
    BreakEffect, ClientInventory, DamageEstimate, DamageProgress, HudItem, HudPlugin, HudRoot,
    HudState, HudSystems, NEXT_SLOT_KEY, PREV_SLOT_KEY, SELECTED_MARK, SelectedMaterial,
    step_selection,
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
pub use panel::{
    EditorPanelPlugin, EditorStats, HostTickTime, PANEL_TITLE, PanelView, STATS_WINDOW_S,
    brush_summary, editor_panel,
};
pub use pickaxe::{
    PLACE_BUTTON, PickTarget, Pickaxe, PickaxePlugin, PickaxeSystems, PlaceState, SWING_BUTTON,
    SWING_SLACK, SwingState,
};
pub use render::{CameraStart, GameCamera, GameRenderPlugin};
pub use replica::{Replica, ReplicaError, ReplicaPlugin};
pub use save::{
    AUTOSAVE_INTERVAL, Autosave, SaveCause, SaveOutcome, SavePlugin, SaveStatus, SaveSystems,
};
