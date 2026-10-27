//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod controller;
pub mod cursor;
pub mod eco;
pub mod editor;
pub mod flycam;
pub mod gizmo;
pub mod hud;
pub mod interp;
pub mod liquid;
pub mod material;
pub mod mesher;
pub mod meshpipe;
pub mod net;
pub mod panel;
pub mod pickaxe;
pub mod render;
pub mod replica;
pub mod save;
pub mod sound;
pub mod surfaces;
pub mod tiles;
pub mod ui;
pub mod viewmodel;
pub mod weather;

pub use controller::{
    Aabb, ControllerConfig, ControllerPlugin, ControllerSystems, EYE_HEIGHT_M, FIXED_HZ, MoveInput,
    PLAYER_HEIGHT_M, PLAYER_STATE_HZ, PLAYER_WIDTH_M, Player, STEP_SMOOTH_S,
};
pub use cursor::{
    CursorGrab, CursorPlugin, CursorSystems, GRAB_BUTTON, GRABBED_MODE, RELEASE_KEY, look_allowed,
    swing_allowed,
};
pub use eco::{
    CellReadout, ClientEco, EditorReadout, PlantReadout, SeasonDone, SeasonSummary, cell_readout,
    readout_lines, season_lines,
};
pub use editor::{
    ADVANCE_SEASON_LABEL, BRUSH_BUTTON, BRUSH_REACH_M, BrushOp, BrushShape, EDITOR_BINDINGS,
    EditorAction, EditorPlugin, EditorState, EditorSystems, GizmoToggles, MAX_BRUSH_SIZE,
    OVERLAY_LAYERS, SNAP_RADIUS_CELLS, TOGGLE_EDITOR_KEY, brush_command, brush_target,
    free_feet_near, next_overlay, overlay_choices, overlay_name, overlay_source,
};
pub use flycam::{FLY_TOGGLE_KEY, FlyCam, FlyCamPlugin, FlyCamSystems};
pub use gizmo::{
    BRUSH_CELL_COLOR, CELL_GIZMO_SCALE, CHUNK_BOUNDS_COLOR, CHUNK_BOUNDS_RADIUS, EditorGizmoPlugin,
    EditorGizmos, GizmoSystems, HIT_CELL_COLOR, MESH_QUEUE_INSET, TargetGizmo, mesh_state_color,
};
pub use hud::{
    BREAK_CHIPS, BREAK_EFFECT_S, BreakChip, BreakEffect, CRACK_OVERLAY_SCALE, CellBroke,
    ClientInventory, CrackOverlay, DamageEstimate, DamageProgress, GasTintOverlay,
    HUD_GIZMO_DEPTH_BIAS, HudChip, HudGas, HudGizmos, HudItem, HudPlugin, HudRoot, HudState,
    HudSystems, LANTERN_DANGER_COLOR, LANTERN_FLARING_COLOR, LANTERN_ICON_SIZE,
    LANTERN_STEADY_COLOR, LanternIcon, LanternState, NEXT_SLOT_KEY, PREV_SLOT_KEY, SELECTED_MARK,
    SPOIL_COLOR, SPOIL_LABEL, SelectedMaterial, SpoilIndicator, crack_color, flash_scale,
    gas_overlay, lantern_state, step_selection,
};
pub use interp::{InterpolatedEye, render_pos};
pub use material::{
    CELL_JITTER_AMPLITUDE, CELL_JITTER_INSET_M, ChunkEmissiveMaterial, EmissiveChunkMaterial,
    EmissiveFromAlpha, EmissiveMaterialPlugin, cell_jitter, fragment_cell,
};
pub use mesher::{
    ChunkMesher, ColorSource, Colormap, GreedyMesher, MeshData, MeshInput, is_liquid_pass,
    is_opaque,
};
pub use meshpipe::{
    ChunkMesh, ChunkMeshState, LiquidMesh, MeshBudget, MeshColorSource, MeshPipelinePlugin,
    MeshQueue, MeshStats, MeshSystems, Mesher,
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
pub use render::{CameraStart, GameCamera, GameRenderPlugin, LiquidMaterial};
pub use replica::{Replica, ReplicaError, ReplicaPlugin};
pub use save::{
    AUTOSAVE_INTERVAL, Autosave, SaveCause, SaveOutcome, SavePlugin, SaveStatus, SaveSystems,
};
pub use sound::{SoundConfig, SoundCue, SoundPlugin, SoundSystems, WindLevel};
pub use ui::{
    HudAnchor, HudSlot, Screen, Toast, UiAction, UiKitPlugin, UiLayer, UiScreens, UiSettings,
    UiSystems, UiTheme,
};
pub use viewmodel::{ViewModel, ViewModelPlugin, ViewModelSystems};
pub use weather::{
    ClientSeasons, SeasonClockPaused, SeasonView, Sky, Weather, WeatherPlugin, WeatherSystems,
};
