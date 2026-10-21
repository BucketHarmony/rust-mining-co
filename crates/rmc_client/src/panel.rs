//! The debug editor's egui panel and its stats (SAD §11.6).
//!
//! [`editor_panel`] draws the panel into an egui context while
//! [`EditorState::open`] is set: the brush (op, shape, size, material), the
//! fly camera, the gizmo toggles, the [`EditorStats`], the targeted-cell
//! readout ([`EditorReadout`](crate::eco::EditorReadout)), the worldgen seed with
//! Regenerate, Advance season, Save / Load, and a `bevy-inspector-egui` world inspector. It
//! edits the same [`EditorState`] the editor keys do; its buttons apply the
//! same [`EditorAction`]s. F1 opens and closes it.
//!
//! [`EditorPanelPlugin`] runs it on the primary egui context. It needs
//! `bevy_egui`'s renderer, so only an app that renders adds it; unit tests
//! call [`editor_panel`] on a bare `egui::Context`.
//!
//! [`EditorStats`] is sampled by [`EditorPlugin`](crate::editor::EditorPlugin)
//! every frame, so it is current whether or not the panel is drawn.

use bevy::prelude::*;
use bevy_egui::{
    EguiContext, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, PrimaryEguiContext, egui,
};
use bevy_inspector_egui::DefaultInspectorConfigPlugin;
use rmc_protocol::ServerMsg;
use rmc_world::edit::EditStatus;
use rmc_world::materials::MaterialTable;

use crate::eco::EditorReadout;
use crate::editor::{
    ADVANCE_SEASON_LABEL, BrushOp, BrushShape, EditorAction, EditorState, MAX_BRUSH_SIZE,
    brush_materials,
};
use crate::flycam::FlyCam;
use crate::meshpipe::MeshStats;
use crate::net::{ClientMaterials, FromHost};
use crate::replica::Replica;

/// The panel's heading, also its egui id.
pub const PANEL_TITLE: &str = "Editor";

/// How long [`EditorStats::fps`] and [`EditorStats::edits_per_s`] average
/// over, in seconds.
pub const STATS_WINDOW_S: f32 = 1.0;

/// What the panel's Stats section shows.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct EditorStats {
    /// Frames per second over the last [`STATS_WINDOW_S`].
    pub fps: f32,
    /// Chunks resident in the replica.
    pub chunks: usize,
    /// Chunks waiting for a mesh job.
    pub mesh_queued: usize,
    /// Mesh jobs running.
    pub mesh_in_flight: usize,
    /// How long the most recently uploaded mesh took to build, in ms.
    pub last_remesh_ms: f32,
    /// Vertex and index bytes over all uploaded chunk meshes (SAD §13 mesh
    /// memory).
    pub mesh_bytes: u64,
    /// How long the host's last tick took, in ms, if whoever runs the host
    /// reports it ([`HostTickTime`]).
    pub host_tick_ms: Option<f32>,
    /// Edits the host applied per second over the last [`STATS_WINDOW_S`].
    pub edits_per_s: f32,
}

/// How long the host's last tick took, in ms. Whoever runs the host sets it;
/// `None` when nobody does.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct HostTickTime(pub Option<f32>);

/// The read-only inputs of [`editor_panel`].
#[derive(Clone, Copy)]
pub struct PanelView<'a> {
    pub stats: &'a EditorStats,
    /// The material table, for the material picker and names. Without it the
    /// picker is hidden.
    pub materials: Option<&'a MaterialTable>,
    /// The targeted-cell readout's lines ([`EditorReadout`]); the Cell
    /// section is hidden when empty.
    pub readout: &'a [String],
}

/// One line describing the brush: op, shape, size and material.
pub fn brush_summary(state: &EditorState, materials: Option<&MaterialTable>) -> String {
    let material = materials
        .and_then(|table| table.get(state.brush_material(table)?))
        .map_or("none", |def| def.key.as_str());
    format!(
        "{} · {} · size {} · {}",
        op_name(state.op),
        shape_name(state.shape),
        state.size,
        material
    )
}

fn op_name(op: BrushOp) -> &'static str {
    match op {
        BrushOp::Set => "Set",
        BrushOp::Remove => "Remove",
        BrushOp::Paint => "Paint",
    }
}

fn shape_name(shape: BrushShape) -> &'static str {
    match shape {
        BrushShape::Cell => "Cell",
        BrushShape::Box => "Box",
        BrushShape::Sphere => "Sphere",
    }
}

/// Draws the editor panel on `ctx` if the editor is open; returns whether
/// it did. See the module docs for what it holds.
///
/// The panel runs down the left edge of the view and scrolls when the view
/// is shorter than its contents.
///
/// - `fly` is the fly camera's on switch ([`FlyCam::enabled`]).
/// - `inspector` fills the collapsed Inspector section when it is opened.
pub fn editor_panel(
    ctx: &egui::Context,
    state: &mut EditorState,
    fly: &mut bool,
    view: PanelView<'_>,
    inspector: impl FnOnce(&mut egui::Ui),
) -> bool {
    if !state.open {
        return false;
    }
    let mut actions = Vec::new();
    let mut root = egui::Ui::new(
        ctx.clone(),
        egui::Id::new(PANEL_TITLE),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    egui::Panel::left(PANEL_TITLE).show(&mut root, |ui| {
        ui.heading(PANEL_TITLE);
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            panel_body(ui, state, fly, view, &mut actions);
            ui.separator();
            egui::CollapsingHeader::new("Inspector")
                .default_open(false)
                .show(ui, inspector);
        });
    });
    for action in actions {
        state.apply(action, view.materials);
    }
    true
}

/// The panel's sections above the inspector. Buttons push their action to
/// `actions`; the caller applies them.
fn panel_body(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    fly: &mut bool,
    view: PanelView<'_>,
    actions: &mut Vec<EditorAction>,
) {
    ui.strong("Brush");
    ui.label(brush_summary(state, view.materials));
    ui.horizontal(|ui| {
        for op in [BrushOp::Set, BrushOp::Remove, BrushOp::Paint] {
            ui.selectable_value(&mut state.op, op, op_name(op));
        }
    });
    ui.horizontal(|ui| {
        for shape in [BrushShape::Cell, BrushShape::Box, BrushShape::Sphere] {
            ui.selectable_value(&mut state.shape, shape, shape_name(shape));
        }
    });
    ui.add(egui::Slider::new(&mut state.size, 1..=MAX_BRUSH_SIZE).text("size"));
    if let Some(table) = view.materials {
        let current = state.brush_material(table);
        let name = |id| table.get(id).map_or("?", |def| def.key.as_str());
        egui::ComboBox::from_label("material")
            .selected_text(current.map_or("none", name))
            .show_ui(ui, |ui| {
                for id in brush_materials(table) {
                    if ui.selectable_label(current == Some(id), name(id)).clicked() {
                        state.material = Some(id);
                    }
                }
            });
    }

    ui.separator();
    ui.checkbox(fly, "Fly camera (noclip)");

    ui.separator();
    ui.strong("Gizmos");
    ui.checkbox(&mut state.gizmos.chunk_bounds, "Chunk bounds");
    ui.checkbox(&mut state.gizmos.target_cell, "Targeted cell");
    ui.checkbox(&mut state.gizmos.mesh_queue, "Mesh queue");

    ui.separator();
    ui.strong("Stats");
    let stats = view.stats;
    ui.label(format!("FPS: {:.0}", stats.fps));
    ui.label(format!("Chunks: {}", stats.chunks));
    ui.label(format!(
        "Mesh jobs: {} queued, {} running",
        stats.mesh_queued, stats.mesh_in_flight
    ));
    ui.label(format!("Last remesh: {:.1} ms", stats.last_remesh_ms));
    ui.label(format!("Mesh memory: {}", mesh_memory(stats.mesh_bytes)));
    ui.label(match stats.host_tick_ms {
        Some(ms) => format!("Host tick: {ms:.1} ms"),
        None => "Host tick: n/a".to_string(),
    });
    ui.label(format!("Edits/s: {:.1}", stats.edits_per_s));

    if !view.readout.is_empty() {
        ui.separator();
        ui.strong("Cell");
        for line in view.readout {
            ui.label(line);
        }
    }

    ui.separator();
    ui.strong("Worldgen");
    ui.horizontal(|ui| {
        ui.label("seed");
        ui.add(egui::DragValue::new(&mut state.seed));
        if ui.button("Regenerate").clicked() {
            actions.push(EditorAction::Regenerate);
        }
    });

    ui.separator();
    ui.strong("Season");
    if ui.button(ADVANCE_SEASON_LABEL).clicked() {
        actions.push(EditorAction::AdvanceSeason);
    }

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            actions.push(EditorAction::Save);
        }
        if ui.button("Load").clicked() {
            actions.push(EditorAction::Load);
        }
    });
}

/// `bytes` in MiB with one decimal, the stats panel's mesh memory.
fn mesh_memory(bytes: u64) -> String {
    const MIB: f64 = (1u64 << 20) as f64;
    format!("{:.1} MiB", bytes as f64 / MIB)
}

/// Samples [`EditorStats`] once a frame.
pub(crate) fn sample_stats(
    time: Res<Time<Real>>,
    mut inbox: MessageReader<FromHost>,
    replica: Option<Res<Replica>>,
    mesh: Option<Res<MeshStats>>,
    host: Res<HostTickTime>,
    mut window: Local<StatsWindow>,
    mut stats: ResMut<EditorStats>,
) {
    let applied = inbox
        .read()
        .filter(|FromHost(msg)| {
            matches!(msg, ServerMsg::Applied(result) if result.status == EditStatus::Applied)
        })
        .count();
    // A frame with no time behind it (the first) has no rate to count.
    if !time.delta().is_zero() {
        window.elapsed_s += time.delta_secs();
        window.frames += 1;
    }
    window.edits += u32::try_from(applied).unwrap_or(u32::MAX);
    if window.elapsed_s >= STATS_WINDOW_S {
        stats.fps = window.frames as f32 / window.elapsed_s;
        stats.edits_per_s = window.edits as f32 / window.elapsed_s;
        *window = StatsWindow::default();
    }
    stats.chunks = replica.map_or(0, |r| r.world().chunk_count());
    if let Some(mesh) = mesh {
        stats.mesh_queued = mesh.queued;
        stats.mesh_in_flight = mesh.in_flight;
        stats.last_remesh_ms = mesh.last_mesh_ms;
        stats.mesh_bytes = mesh.mesh_bytes;
    }
    stats.host_tick_ms = host.0;
}

/// Frames and applied edits since the window started.
#[derive(Default)]
pub(crate) struct StatsWindow {
    elapsed_s: f32,
    frames: u32,
    edits: u32,
}

/// Adds `bevy_egui`, the inspector's defaults and the panel. Add it after
/// [`EditorPlugin`](crate::editor::EditorPlugin), in an app that renders.
pub struct EditorPanelPlugin;

impl Plugin for EditorPanelPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<EguiPlugin>() {
            app.add_plugins(EguiPlugin::default());
        }
        if !app.is_plugin_added::<DefaultInspectorConfigPlugin>() {
            app.add_plugins(DefaultInspectorConfigPlugin);
        }
        // Clicks on the panel do not reach the brush, and typing in its
        // fields does not trigger the editor keys.
        if let Some(mut settings) = app.world_mut().get_resource_mut::<EguiGlobalSettings>() {
            settings.enable_absorb_bevy_input_system = true;
        }
        app.add_systems(EguiPrimaryContextPass, draw_panel.run_if(editor_open));
    }
}

fn editor_open(state: Option<Res<EditorState>>) -> bool {
    state.is_some_and(|s| s.open)
}

/// Draws [`editor_panel`] on the primary egui context, with the world
/// inspector in its Inspector section.
fn draw_panel(world: &mut World) {
    let mut contexts = world.query_filtered::<&mut EguiContext, With<PrimaryEguiContext>>();
    let Ok(mut context) = contexts.single_mut(world) else {
        return;
    };
    let ctx = context.get_mut().clone();
    let stats = world
        .get_resource::<EditorStats>()
        .cloned()
        .unwrap_or_default();
    let materials = world.get_resource::<ClientMaterials>().map(|m| m.0.clone());
    let readout = world
        .get_resource::<EditorReadout>()
        .map(|r| r.lines.clone())
        .unwrap_or_default();
    let fly_was = world.get_resource::<FlyCam>().map(|f| f.enabled);
    let mut fly = fly_was.unwrap_or(false);
    world.resource_scope(|world, mut state: Mut<EditorState>| {
        let view = PanelView {
            stats: &stats,
            materials: materials.as_deref(),
            readout: &readout,
        };
        editor_panel(&ctx, &mut state, &mut fly, view, |ui| {
            bevy_inspector_egui::bevy_inspector::ui_for_world(world, ui);
        });
    });
    if fly_was.is_some_and(|was| was != fly)
        && let Some(mut cam) = world.get_resource_mut::<FlyCam>()
    {
        cam.enabled = fly;
    }
}
