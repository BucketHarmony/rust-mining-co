//! Debug editor state and its keyboard bindings (SAD §11.6).
//!
//! [`EditorState`] holds everything the editor's controls set: whether the
//! editor is open, the brush (op, shape, size, material), the gizmo toggles
//! and the one-shot Save / Load requests. The egui panel (F072) edits the
//! same resource, and every action also has a key in [`EDITOR_BINDINGS`],
//! which `docs/CONTROLS.md` lists. Tests drive the editor through those keys
//! and the resource, never through egui widgets.
//!
//! - [`TOGGLE_EDITOR_KEY`] (F1) opens and closes the editor. Every other
//!   binding acts only while it is open, so the brush keys never fire during
//!   normal play.
//! - Keys act on `just_pressed`: holding one does not repeat.
//!
//! The fly camera ([`FlyCam`], V) is part of the editor's toolset but keeps
//! its own resource, which the controller, pickaxe and HUD already read.
//! When it turns off, [`EditorPlugin`] drops the player where the camera
//! is: the feet go [`EYE_HEIGHT_M`] below the camera with the camera's yaw
//! and pitch, moved to the nearest free spot ([`free_feet_near`]) if the box
//! would overlap a blocking cell there. If there is no free spot within
//! [`SNAP_RADIUS_CELLS`], the player stays where it was before flying.
//!
//! While the editor is open, a click of [`BRUSH_BUTTON`] applies the brush
//! once at the cell the view ray hits within [`BRUSH_REACH_M`]: from the
//! fly camera while it is on, else from the player's eye. The brush sends
//! one normal [`EditCommand`] ([`brush_command`]); the host decides what
//! changes. The pickaxe does not swing while the editor is open.

use bevy::prelude::*;
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::edit::{EditCommand, EditOp};
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::shape::Shape;
use rmc_world::world::World;
use rmc_world::{Hit, raycast};

use crate::controller::{Aabb, ControllerSystems, EYE_HEIGHT_M, Player, blocks};
use crate::flycam::{FlyCam, FlyCamSystems};
use crate::interp::InterpolatedEye;
use crate::net::{ClientMaterials, CommandSeq, FromHost, ToHost};
use crate::panel::{EditorStats, HostTickTime, sample_stats};
use crate::render::GameCamera;
use crate::replica::Replica;

/// Opens and closes the editor.
pub const TOGGLE_EDITOR_KEY: KeyCode = KeyCode::F1;

/// Largest brush size. A [`BrushShape::Box`] of this size, `2 * size + 1`
/// cells on a side, still fits in one edit command
/// ([`MAX_EDIT_CELLS`](rmc_world::edit::MAX_EDIT_CELLS)).
pub const MAX_BRUSH_SIZE: u16 = 15;

/// The button that applies the brush while the editor is open.
pub const BRUSH_BUTTON: MouseButton = MouseButton::Left;

/// How far the brush reaches along the view ray, in meters.
pub const BRUSH_REACH_M: f32 = 32.0;

/// How far, in cells, [`free_feet_near`] looks for a spot the player fits.
pub const SNAP_RADIUS_CELLS: i32 = 8;

/// What the brush does to the cells it covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BrushOp {
    /// Fills with the brush material, on the face-normal side of the hit.
    #[default]
    Set,
    /// Clears to air, at the hit.
    Remove,
    /// Recolors existing solid cells with the brush material, at the hit.
    Paint,
}

/// The cells the brush covers around its target cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BrushShape {
    /// The target cell only; size is ignored.
    #[default]
    Cell,
    /// A cube `2 * size + 1` cells on a side, centered on the target.
    Box,
    /// A sphere of radius `size` cells, centered on the target.
    Sphere,
}

/// Which debug gizmos are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GizmoToggles {
    pub chunk_bounds: bool,
    pub target_cell: bool,
    pub mesh_queue: bool,
}

/// Everything the editor's controls set. The panel and the keys edit this
/// one resource.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct EditorState {
    /// The editor is open: the panel shows and the brush keys act.
    pub open: bool,
    pub op: BrushOp,
    pub shape: BrushShape,
    /// Brush radius in cells, `1..=MAX_BRUSH_SIZE`.
    pub size: u16,
    /// The brush material. `None` until one is picked: the brush then uses
    /// [`default_material`].
    pub material: Option<MaterialId>,
    pub gizmos: GizmoToggles,
    /// Set by the Save action; whoever saves takes it (sets it back).
    pub save_requested: bool,
    /// Set by the Load action; whoever loads takes it (sets it back).
    pub load_requested: bool,
    /// The worldgen seed the Regenerate action uses. Each `Welcome` sets it
    /// to the host's seed; the panel edits it.
    pub seed: u64,
    /// Set by the Regenerate action; whoever regenerates the world from
    /// [`seed`](Self::seed) takes it (sets it back).
    pub regenerate_requested: bool,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            open: false,
            op: BrushOp::default(),
            shape: BrushShape::default(),
            size: 1,
            material: None,
            gizmos: GizmoToggles::default(),
            save_requested: false,
            load_requested: false,
            seed: 0,
            regenerate_requested: false,
        }
    }
}

impl EditorState {
    /// The material the brush applies: the picked one, else the table's
    /// [`default_material`].
    pub fn brush_material(&self, table: &MaterialTable) -> Option<MaterialId> {
        self.material.or_else(|| default_material(table))
    }

    /// Applies one action, as its key does while the editor is open.
    pub fn apply(&mut self, action: EditorAction, table: Option<&MaterialTable>) {
        match action {
            EditorAction::TogglePanel => self.open = !self.open,
            EditorAction::Op(op) => self.op = op,
            EditorAction::Shape(shape) => self.shape = shape,
            EditorAction::SizeDown => self.size = self.size.saturating_sub(1).max(1),
            EditorAction::SizeUp => self.size = (self.size + 1).min(MAX_BRUSH_SIZE),
            EditorAction::PrevMaterial | EditorAction::NextMaterial => {
                if let Some(table) = table {
                    let forward = action == EditorAction::NextMaterial;
                    self.material = cycle_material(table, self.brush_material(table), forward);
                }
            }
            EditorAction::ToggleChunkBounds => {
                self.gizmos.chunk_bounds = !self.gizmos.chunk_bounds;
            }
            EditorAction::ToggleTargetCell => self.gizmos.target_cell = !self.gizmos.target_cell,
            EditorAction::ToggleMeshQueue => self.gizmos.mesh_queue = !self.gizmos.mesh_queue,
            EditorAction::Save => self.save_requested = true,
            EditorAction::Load => self.load_requested = true,
            EditorAction::Regenerate => self.regenerate_requested = true,
        }
    }
}

/// One editor action, as bound to a key in [`EDITOR_BINDINGS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditorAction {
    TogglePanel,
    Op(BrushOp),
    Shape(BrushShape),
    SizeDown,
    SizeUp,
    PrevMaterial,
    NextMaterial,
    ToggleChunkBounds,
    ToggleTargetCell,
    ToggleMeshQueue,
    Save,
    Load,
    Regenerate,
}

impl EditorAction {
    /// What the action does, as `docs/CONTROLS.md` words it.
    pub fn description(self) -> &'static str {
        match self {
            EditorAction::TogglePanel => "Open / close the editor",
            EditorAction::Op(BrushOp::Set) => "Brush op: Set",
            EditorAction::Op(BrushOp::Remove) => "Brush op: Remove",
            EditorAction::Op(BrushOp::Paint) => "Brush op: Paint",
            EditorAction::Shape(BrushShape::Cell) => "Brush shape: Cell",
            EditorAction::Shape(BrushShape::Box) => "Brush shape: Box",
            EditorAction::Shape(BrushShape::Sphere) => "Brush shape: Sphere",
            EditorAction::SizeDown => "Brush size down",
            EditorAction::SizeUp => "Brush size up",
            EditorAction::PrevMaterial => "Previous brush material",
            EditorAction::NextMaterial => "Next brush material",
            EditorAction::ToggleChunkBounds => "Toggle chunk-bounds gizmo",
            EditorAction::ToggleTargetCell => "Toggle targeted-cell gizmo",
            EditorAction::ToggleMeshQueue => "Toggle mesh-queue gizmo",
            EditorAction::Save => "Save the world",
            EditorAction::Load => "Load the saved world",
            EditorAction::Regenerate => "Regenerate the world from the seed",
        }
    }
}

/// Every editor key and its action. Each key appears once.
pub const EDITOR_BINDINGS: &[(KeyCode, EditorAction)] = &[
    (TOGGLE_EDITOR_KEY, EditorAction::TogglePanel),
    (KeyCode::Digit1, EditorAction::Op(BrushOp::Set)),
    (KeyCode::Digit2, EditorAction::Op(BrushOp::Remove)),
    (KeyCode::Digit3, EditorAction::Op(BrushOp::Paint)),
    (KeyCode::Digit4, EditorAction::Shape(BrushShape::Cell)),
    (KeyCode::Digit5, EditorAction::Shape(BrushShape::Box)),
    (KeyCode::Digit6, EditorAction::Shape(BrushShape::Sphere)),
    (KeyCode::BracketLeft, EditorAction::SizeDown),
    (KeyCode::BracketRight, EditorAction::SizeUp),
    (KeyCode::Comma, EditorAction::PrevMaterial),
    (KeyCode::Period, EditorAction::NextMaterial),
    (KeyCode::F2, EditorAction::ToggleChunkBounds),
    (KeyCode::F3, EditorAction::ToggleTargetCell),
    (KeyCode::F4, EditorAction::ToggleMeshQueue),
    (KeyCode::F5, EditorAction::Save),
    (KeyCode::F8, EditorAction::Regenerate),
    (KeyCode::F9, EditorAction::Load),
];

/// The materials the brush can pick: every `Solid` material, in id order.
pub fn brush_materials(table: &MaterialTable) -> impl Iterator<Item = MaterialId> + '_ {
    table.iter().filter(|d| d.is_solid()).map(|d| d.id)
}

/// The brush material before one is picked: the first material tagged
/// `placeable`, else the first brush material.
pub fn default_material(table: &MaterialTable) -> Option<MaterialId> {
    brush_materials(table)
        .find(|&id| table.has_tag(id, "placeable"))
        .or_else(|| brush_materials(table).next())
}

/// The brush material after (`forward`) or before `current`, wrapping. A
/// `current` that is not a brush material goes to the first (or last).
fn cycle_material(
    table: &MaterialTable,
    current: Option<MaterialId>,
    forward: bool,
) -> Option<MaterialId> {
    let list: Vec<MaterialId> = brush_materials(table).collect();
    let n = list.len();
    if n == 0 {
        return None;
    }
    let next = match current.and_then(|c| list.iter().position(|&id| id == c)) {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None if forward => 0,
        None => n - 1,
    };
    Some(list[next])
}

/// The op and shape the brush sends for a ray `hit`, or `None` when it
/// sends nothing.
///
/// - Set fills on the face-normal side of the hit: the empty cell the ray
///   crossed last. A hit with a zero normal (the ray started inside the
///   cell) has no such side, so Set sends nothing.
/// - Remove and Paint act on the hit cell itself.
/// - The shape is centered on that target cell: [`BrushShape`] with the
///   state's size.
/// - Set and Paint need a brush material ([`EditorState::brush_material`]).
pub fn brush_command(
    state: &EditorState,
    table: &MaterialTable,
    hit: Hit,
) -> Option<(EditOp, Shape)> {
    let (op, target) = match state.op {
        BrushOp::Set => {
            if hit.face_normal == IVec3::ZERO {
                return None;
            }
            let material = state.brush_material(table)?;
            (EditOp::Set(material), CellPos(hit.cell.0 + hit.face_normal))
        }
        BrushOp::Remove => (EditOp::Remove, hit.cell),
        BrushOp::Paint => (EditOp::Paint(state.brush_material(table)?), hit.cell),
    };
    let size = state.size.clamp(1, MAX_BRUSH_SIZE);
    let half = IVec3::splat(i32::from(size));
    let shape = match state.shape {
        BrushShape::Cell => Shape::Cell(target),
        BrushShape::Box => Shape::Box {
            min: CellPos(target.0 - half),
            max: CellPos(target.0 + half),
        },
        BrushShape::Sphere => Shape::Sphere {
            center: target,
            radius: size,
        },
    };
    Some((op, shape))
}

/// The feet nearest `feet` where the player's box overlaps no blocking cell
/// ([`blocks`]): `feet` itself if the box is free there, else the
/// bottom-center of the nearest cell within [`SNAP_RADIUS_CELLS`] where it
/// is free. Cells are tried by squared distance from the cell holding
/// `feet`, then higher first, then by z and x, so the result is
/// deterministic. `None` if no cell in range is free.
pub fn free_feet_near(world: &World, feet: Vec3) -> Option<Vec3> {
    if box_is_free(world, Aabb::of_feet(feet)) {
        return Some(feet);
    }
    let base = (feet / CELL_SIZE_M).floor().as_ivec3();
    let r = SNAP_RADIUS_CELLS;
    let mut offsets: Vec<IVec3> = (-r..=r)
        .flat_map(|y| (-r..=r).flat_map(move |z| (-r..=r).map(move |x| IVec3::new(x, y, z))))
        .filter(|o| o.length_squared() <= r * r)
        .collect();
    offsets.sort_by_key(|o| (o.length_squared(), -o.y, o.z, o.x));
    offsets.into_iter().find_map(|o| {
        let c = base + o;
        let candidate = Vec3::new(
            (c.x as f32 + HALF) * CELL_SIZE_M,
            c.y as f32 * CELL_SIZE_M,
            (c.z as f32 + HALF) * CELL_SIZE_M,
        );
        box_is_free(world, Aabb::of_feet(candidate)).then_some(candidate)
    })
}

/// Half a cell, in cells: the offset from a cell's corner to its middle.
const HALF: f32 = 0.5;

/// Whether `aabb` overlaps no blocking cell.
fn box_is_free(world: &World, aabb: Aabb) -> bool {
    let lo = (aabb.min / CELL_SIZE_M).floor().as_ivec3();
    let hi = (aabb.max / CELL_SIZE_M).floor().as_ivec3();
    (lo.y..=hi.y).all(|y| {
        (lo.z..=hi.z).all(|z| {
            (lo.x..=hi.x).all(|x| {
                let cell = CellPos::new(x, y, z);
                !aabb.overlaps_cell(cell) || !blocks(world, cell)
            })
        })
    })
}

/// `key` as `docs/CONTROLS.md` names it.
pub fn key_label(key: KeyCode) -> &'static str {
    match key {
        KeyCode::F1 => "F1",
        KeyCode::F2 => "F2",
        KeyCode::F3 => "F3",
        KeyCode::F4 => "F4",
        KeyCode::F5 => "F5",
        KeyCode::F8 => "F8",
        KeyCode::F9 => "F9",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::BracketLeft => "[",
        KeyCode::BracketRight => "]",
        KeyCode::Comma => ",",
        KeyCode::Period => ".",
        KeyCode::KeyW => "W",
        KeyCode::KeyA => "A",
        KeyCode::KeyS => "S",
        KeyCode::KeyD => "D",
        KeyCode::KeyV => "V",
        KeyCode::Space => "Space",
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "Shift",
        KeyCode::ControlLeft | KeyCode::ControlRight => "Ctrl",
        _ => "?",
    }
}

/// `button` as `docs/CONTROLS.md` names it.
pub fn mouse_label(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "Left mouse",
        MouseButton::Right => "Right mouse",
        MouseButton::Middle => "Middle mouse",
        _ => "?",
    }
}

/// The editor's systems, in `Update`.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EditorSystems;

/// The debug editor's state, keys, brush and stats, and the drop out of the
/// fly camera. The egui panel is [`EditorPanelPlugin`](crate::panel::EditorPanelPlugin).
pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ToHost>()
            .add_message::<FromHost>()
            .init_resource::<EditorState>()
            .init_resource::<CommandSeq>()
            .init_resource::<EditorStats>()
            .init_resource::<HostTickTime>()
            .add_systems(
                Update,
                (
                    follow_welcome_seed.before(editor_keys),
                    editor_keys,
                    sample_stats,
                    drop_player_after_flying
                        .after(FlyCamSystems)
                        .before(ControllerSystems::Look),
                    apply_brush
                        .after(editor_keys)
                        .after(drop_player_after_flying)
                        .after(FlyCamSystems)
                        .after(ControllerSystems::Look),
                )
                    .in_set(EditorSystems),
            );
    }
}

/// Each `Welcome` sets the editor's seed to the host's.
fn follow_welcome_seed(mut inbox: MessageReader<FromHost>, mut state: ResMut<EditorState>) {
    for FromHost(msg) in inbox.read() {
        if let ServerMsg::Welcome { seed, .. } = msg {
            state.seed = *seed;
        }
    }
}

/// Applies the action of every editor key pressed this frame, in
/// [`EDITOR_BINDINGS`] order. Only F1 acts while the editor is closed.
fn editor_keys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    materials: Option<Res<ClientMaterials>>,
    mut state: ResMut<EditorState>,
) {
    let Some(keys) = keys else { return };
    for &(key, action) in EDITOR_BINDINGS {
        if !keys.just_pressed(key) || !(state.open || action == EditorAction::TogglePanel) {
            continue;
        }
        state.apply(action, materials.as_deref().map(|m| &*m.0));
        debug!(?action, "editor key");
    }
}

/// On a click of [`BRUSH_BUTTON`] while the editor is open, sends the
/// brush's edit at the cell the view ray hits (see the module docs).
#[allow(clippy::too_many_arguments)]
fn apply_brush(
    state: Res<EditorState>,
    buttons: Option<Res<ButtonInput<MouseButton>>>,
    materials: Option<Res<ClientMaterials>>,
    replica: Option<Res<Replica>>,
    fly: Option<Res<FlyCam>>,
    cameras: Query<&Transform, With<GameCamera>>,
    players: Query<&Player>,
    mut seq: ResMut<CommandSeq>,
    mut out: MessageWriter<ToHost>,
) {
    if !state.open || !buttons.is_some_and(|b| b.just_pressed(BRUSH_BUTTON)) {
        return;
    }
    let (Some(materials), Some(replica)) = (materials, replica) else {
        return;
    };
    let ray = if fly.is_some_and(|f| f.enabled) {
        cameras
            .iter()
            .next()
            .map(|t| (t.translation, t.rotation * Vec3::NEG_Z))
    } else {
        players
            .iter()
            .next()
            .map(|p| (p.eye(), p.rotation() * Vec3::NEG_Z))
    };
    let Some((origin, dir)) = ray else { return };
    let Some(hit) = raycast(replica.world(), origin, dir, BRUSH_REACH_M) else {
        return;
    };
    let Some((op, shape)) = brush_command(&state, &materials.0, hit) else {
        return;
    };
    let issuer = replica.player();
    out.write(ToHost(ClientMsg::Edit(EditCommand {
        id: seq.next_id(issuer),
        issuer,
        op,
        shape,
    })));
    debug!(?op, ?shape, "editor brush");
}

/// When the fly camera turns off, moves the player under the camera (see
/// the module docs) and restarts its interpolation there, so the view does
/// not slide back from where the player stood.
fn drop_player_after_flying(
    fly: Option<Res<FlyCam>>,
    replica: Option<Res<Replica>>,
    mut was_flying: Local<bool>,
    cameras: Query<&Transform, With<GameCamera>>,
    mut players: Query<(&mut Player, Option<&mut InterpolatedEye>)>,
) {
    let flying = fly.is_some_and(|f| f.enabled);
    let landed = *was_flying && !flying;
    *was_flying = flying;
    if !landed {
        return;
    }
    let (Some(replica), Some(camera)) = (replica, cameras.iter().next()) else {
        return;
    };
    let wanted = camera.translation - Vec3::Y * EYE_HEIGHT_M;
    let Some(feet) = free_feet_near(replica.world(), wanted) else {
        info!(
            ?wanted,
            "no room under the fly camera; the player stays put"
        );
        return;
    };
    let (yaw, pitch, _roll) = camera.rotation.to_euler(EulerRot::YXZ);
    for (mut player, eye) in &mut players {
        *player = Player {
            yaw,
            pitch,
            ..Player::at(feet)
        };
        if let Some(mut eye) = eye {
            *eye = InterpolatedEye::at(player.eye());
        }
    }
    info!(?feet, "player dropped at the fly camera");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::ControllerPlugin;
    use crate::flycam::{FLY_TOGGLE_KEY, FlyCamPlugin};
    use crate::panel::{PANEL_TITLE, PanelView, brush_summary, editor_panel};
    use crate::pickaxe::{Pickaxe, PickaxePlugin, SWING_BUTTON};
    use bevy::input::mouse::AccumulatedMouseMotion;
    use bevy::time::TimeUpdateStrategy;
    use bevy_egui::egui;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::CellAabb;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, MAX_EDIT_CELLS, PlayerId};
    use rmc_world::shape::Shape;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const CONTROLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/CONTROLS.md");
    const S: f32 = CELL_SIZE_M;

    fn materials() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"))
    }

    /// MinimalPlugins + EditorPlugin with the input resources set by hand;
    /// [`frame`] clears them like InputPlugin.
    fn editor_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 60.0,
            )))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<AccumulatedMouseMotion>()
            .insert_resource(ClientMaterials(materials()))
            .add_plugins(EditorPlugin);
        app.update();
        app
    }

    fn frame(app: &mut App) {
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        if let Some(mut buttons) = app
            .world_mut()
            .get_resource_mut::<ButtonInput<MouseButton>>()
        {
            buttons.clear();
        }
        app.world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = Vec2::ZERO;
    }

    /// Presses `key` for one frame and releases it.
    fn tap(app: &mut App, key: KeyCode) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        frame(app);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(key);
        frame(app);
    }

    fn state(app: &App) -> EditorState {
        app.world().resource::<EditorState>().clone()
    }

    fn key_of(action: EditorAction) -> KeyCode {
        EDITOR_BINDINGS
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| *k)
            .unwrap_or_else(|| panic!("{action:?} has no key"))
    }

    #[test]
    fn keys_change_editor_state() {
        let table = materials();
        let mut app = editor_app();
        assert_eq!(
            state(&app),
            EditorState::default(),
            "starts closed, defaults"
        );

        // Each key is bound once, and each action has one key.
        let keys: BTreeSet<_> = EDITOR_BINDINGS
            .iter()
            .map(|(k, _)| format!("{k:?}"))
            .collect();
        assert_eq!(keys.len(), EDITOR_BINDINGS.len(), "a key is bound twice");
        let actions: Vec<_> = EDITOR_BINDINGS.iter().map(|(_, a)| *a).collect();
        for (i, a) in actions.iter().enumerate() {
            assert!(!actions[..i].contains(a), "{a:?} bound twice");
        }
        assert!(
            !EDITOR_BINDINGS.iter().any(|(k, _)| *k == FLY_TOGGLE_KEY),
            "the fly toggle keeps its own key"
        );

        // Closed: no binding but F1 changes anything.
        for &(key, action) in EDITOR_BINDINGS {
            if action == EditorAction::TogglePanel {
                continue;
            }
            tap(&mut app, key);
            assert_eq!(state(&app), EditorState::default(), "{key:?} while closed");
        }

        tap(&mut app, TOGGLE_EDITOR_KEY);
        assert!(state(&app).open, "F1 opens");

        // Op and shape.
        for op in [BrushOp::Remove, BrushOp::Paint, BrushOp::Set] {
            tap(&mut app, key_of(EditorAction::Op(op)));
            assert_eq!(state(&app).op, op);
        }
        for shape in [BrushShape::Sphere, BrushShape::Box, BrushShape::Cell] {
            tap(&mut app, key_of(EditorAction::Shape(shape)));
            assert_eq!(state(&app).shape, shape);
        }

        // Size: up to the cap, down to 1, never past either.
        let up = key_of(EditorAction::SizeUp);
        let down = key_of(EditorAction::SizeDown);
        tap(&mut app, up);
        assert_eq!(state(&app).size, 2);
        for _ in 0..MAX_BRUSH_SIZE + 3 {
            tap(&mut app, up);
        }
        assert_eq!(state(&app).size, MAX_BRUSH_SIZE);
        let biggest = Shape::Box {
            min: CellPos::new(0, 0, 0),
            max: CellPos::new(
                2 * i32::from(MAX_BRUSH_SIZE),
                2 * i32::from(MAX_BRUSH_SIZE),
                2 * i32::from(MAX_BRUSH_SIZE),
            ),
        };
        assert!(
            biggest.cell_count() <= MAX_EDIT_CELLS,
            "max box fits one edit"
        );
        for _ in 0..MAX_BRUSH_SIZE + 3 {
            tap(&mut app, down);
        }
        assert_eq!(state(&app).size, 1);

        // Held keys do not repeat.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(up);
        for _ in 0..5 {
            frame(&mut app);
        }
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(up);
        frame(&mut app);
        assert_eq!(state(&app).size, 2, "one step per press");
        tap(&mut app, down);

        // Material: the default is the first placeable solid; next and
        // previous walk the solid materials in id order and wrap.
        let solids: Vec<MaterialId> = table
            .iter()
            .filter(|d| d.is_solid())
            .map(|d| d.id)
            .collect();
        let placeable = table
            .with_tag("placeable")
            .find(|d| d.is_solid())
            .unwrap()
            .id;
        assert_eq!(state(&app).material, None);
        assert_eq!(state(&app).brush_material(&table), Some(placeable));
        let at = solids.iter().position(|&id| id == placeable).unwrap();
        let next = key_of(EditorAction::NextMaterial);
        let prev = key_of(EditorAction::PrevMaterial);
        for i in 1..=solids.len() {
            tap(&mut app, next);
            let expected = solids[(at + i) % solids.len()];
            assert_eq!(state(&app).material, Some(expected), "next ×{i}");
        }
        assert_eq!(state(&app).material, Some(placeable), "full cycle");
        tap(&mut app, prev);
        assert_eq!(
            state(&app).material,
            Some(solids[(at + solids.len() - 1) % solids.len()])
        );
        assert!(
            !state(&app).material.is_some_and(|id| id == MaterialId::AIR),
            "air is never a brush material"
        );

        // Gizmos toggle independently.
        tap(&mut app, key_of(EditorAction::ToggleChunkBounds));
        assert_eq!(
            state(&app).gizmos,
            GizmoToggles {
                chunk_bounds: true,
                target_cell: false,
                mesh_queue: false
            }
        );
        tap(&mut app, key_of(EditorAction::ToggleTargetCell));
        tap(&mut app, key_of(EditorAction::ToggleMeshQueue));
        assert_eq!(
            state(&app).gizmos,
            GizmoToggles {
                chunk_bounds: true,
                target_cell: true,
                mesh_queue: true
            }
        );
        tap(&mut app, key_of(EditorAction::ToggleChunkBounds));
        assert!(!state(&app).gizmos.chunk_bounds);

        // Save and load raise one-shot requests.
        tap(&mut app, key_of(EditorAction::Save));
        assert!(state(&app).save_requested && !state(&app).load_requested);
        app.world_mut().resource_mut::<EditorState>().save_requested = false;
        tap(&mut app, key_of(EditorAction::Load));
        assert!(state(&app).load_requested && !state(&app).save_requested);

        // F1 closes; the brush is kept and the keys stop acting.
        let before = EditorState {
            open: false,
            ..state(&app)
        };
        tap(&mut app, TOGGLE_EDITOR_KEY);
        assert_eq!(state(&app), before, "F1 closes and keeps the brush");
        tap(&mut app, key_of(EditorAction::Op(BrushOp::Remove)));
        tap(&mut app, up);
        assert_eq!(state(&app), before, "closed again: keys do nothing");

        // State set directly (as the panel does) is what the keys build on.
        app.world_mut().resource_mut::<EditorState>().open = true;
        app.world_mut().resource_mut::<EditorState>().size = 7;
        tap(&mut app, up);
        assert_eq!(state(&app).size, 8);
    }

    /// The table rows of `doc`: `(first cell, second cell)`, backticks and
    /// surrounding spaces stripped from neither.
    fn table_rows(doc: &str) -> Vec<(String, String)> {
        doc.lines()
            .filter_map(|line| {
                let line = line.trim();
                let cells: Vec<&str> = line
                    .strip_prefix('|')?
                    .strip_suffix('|')?
                    .split('|')
                    .map(str::trim)
                    .collect();
                (cells.len() >= 2 && !cells[0].starts_with('-'))
                    .then(|| (cells[0].to_string(), cells[1].to_string()))
            })
            .collect()
    }

    #[test]
    fn controls_doc_lists_every_binding() {
        let doc = std::fs::read_to_string(CONTROLS_PATH).expect("docs/CONTROLS.md exists");
        let rows = table_rows(&doc);

        // Every editor binding has a row: its key label, its description.
        for &(key, action) in EDITOR_BINDINGS {
            let label = format!("`{}`", key_label(key));
            assert!(
                rows.iter()
                    .any(|(k, a)| *k == label && a == action.description()),
                "CONTROLS.md lacks | {label} | {} |",
                action.description()
            );
        }

        // The player, pickaxe and fly-camera inputs are listed too.
        let play_keys = [
            KeyCode::KeyW,
            KeyCode::KeyA,
            KeyCode::KeyS,
            KeyCode::KeyD,
            KeyCode::Space,
            KeyCode::ShiftLeft,
            KeyCode::ControlLeft,
            FLY_TOGGLE_KEY,
        ];
        for key in play_keys {
            let label = format!("`{}`", key_label(key));
            assert!(
                rows.iter().any(|(k, _)| k.contains(&label)),
                "CONTROLS.md has no row for {label}"
            );
        }
        let swing = format!("`{}`", mouse_label(SWING_BUTTON));
        assert!(
            rows.iter().any(|(k, _)| k.contains(&swing)),
            "CONTROLS.md has no row for {swing}"
        );
        assert!(
            rows.iter().any(|(k, _)| k.contains("`Mouse`")),
            "CONTROLS.md has no row for mouse look"
        );

        // No editor row names a key the editor does not bind.
        let section = doc
            .split("## Editor")
            .nth(1)
            .expect("CONTROLS.md has an ## Editor section");
        let section = section.split("\n## ").next().unwrap();
        let bound: Vec<String> = EDITOR_BINDINGS
            .iter()
            .map(|(k, _)| format!("`{}`", key_label(*k)))
            .collect();
        for (k, a) in table_rows(section) {
            if k == "Key" {
                continue;
            }
            assert!(bound.contains(&k), "editor row {k} ({a}) is not a binding");
        }
    }

    // ---- The egui panel ----

    /// Every text the shapes draw, in paint order.
    fn shape_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
                egui::Shape::Vec(inner) => inner.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    /// Input for a screen tall enough that the panel does not scroll.
    fn screen() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 2000.0),
            )),
            ..Default::default()
        }
    }

    /// Runs the panel on `ctx` over the app's editor state and stats, as the
    /// panel system does, for two egui frames (egui may spend a new area's
    /// first frame sizing it). Writes the state back and returns whether the
    /// panel drew, the texts of the last frame and the fly switch.
    fn draw_panel(app: &mut App, ctx: &egui::Context) -> (bool, Vec<String>, bool) {
        let table = materials();
        let stats = app.world().resource::<EditorStats>().clone();
        let mut state = state(app);
        let mut fly = false;
        let mut shown = false;
        let mut texts = Vec::new();
        for _ in 0..2 {
            let out = ctx.run_ui(screen(), |ui| {
                let view = PanelView {
                    stats: &stats,
                    materials: Some(&table),
                };
                shown = editor_panel(ui.ctx(), &mut state, &mut fly, view, |_| {});
            });
            texts = shape_texts(&out.shapes);
            // No renderer takes the font texture here.
            out.drop_without_applying_deltas();
        }
        *app.world_mut().resource_mut::<EditorState>() = state;
        (shown, texts, fly)
    }

    #[test]
    fn panel_toggles_with_f1() {
        let table = materials();
        let mut app = editor_app();
        let ctx = egui::Context::default();

        // Closed: nothing is drawn.
        let (shown, texts, _) = draw_panel(&mut app, &ctx);
        assert!(!shown, "closed editor draws no panel");
        assert!(texts.is_empty(), "closed editor draws nothing: {texts:?}");

        // F1 opens the panel with every section of SAD §11.6.
        tap(&mut app, TOGGLE_EDITOR_KEY);
        assert!(state(&app).open);
        let before = state(&app);
        let (shown, texts, fly) = draw_panel(&mut app, &ctx);
        assert!(shown, "F1 opens the panel");
        for want in [
            PANEL_TITLE,
            "Brush",
            "Set",
            "Remove",
            "Paint",
            "Cell",
            "Box",
            "Sphere",
            "Fly camera",
            "Gizmos",
            "Chunk bounds",
            "Targeted cell",
            "Mesh queue",
            "Stats",
            "FPS",
            "Chunks",
            "Mesh jobs",
            "Last remesh",
            "Host tick",
            "Edits/s",
            "Worldgen",
            "Regenerate",
            "Save",
            "Load",
            "Inspector",
        ] {
            assert!(
                texts.iter().any(|t| t.contains(want)),
                "panel lacks {want:?}: {texts:?}"
            );
        }
        assert!(
            texts.contains(&brush_summary(&before, Some(&table))),
            "panel shows the brush: {texts:?}"
        );
        assert_eq!(state(&app), before, "drawing alone changes nothing");
        assert!(!fly, "drawing alone leaves the fly camera off");

        // Keys change what the panel shows.
        tap(&mut app, key_of(EditorAction::Shape(BrushShape::Sphere)));
        tap(&mut app, key_of(EditorAction::SizeUp));
        tap(&mut app, key_of(EditorAction::NextMaterial));
        let edited = state(&app);
        let summary = brush_summary(&edited, Some(&table));
        assert!(summary.contains("Sphere") && summary.contains("size 2"));
        let (_, texts, _) = draw_panel(&mut app, &ctx);
        assert!(
            texts.contains(&summary),
            "panel follows the keys: {texts:?}"
        );

        // Stats are sampled every frame: 60 frames of 1/60 s read 60 FPS.
        for _ in 0..90 {
            frame(&mut app);
        }
        let stats = app.world().resource::<EditorStats>().clone();
        assert!((stats.fps - 60.0).abs() < 1.0, "fps {}", stats.fps);
        assert_eq!(stats.chunks, 0, "no replica");
        assert_eq!(stats.host_tick_ms, None, "nobody reports the host");
        app.world_mut().resource_mut::<HostTickTime>().0 = Some(2.5);
        frame(&mut app);
        let (_, texts, _) = draw_panel(&mut app, &ctx);
        for want in ["FPS: 60", "Host tick: 2.5 ms"] {
            assert!(
                texts.iter().any(|t| t.contains(want)),
                "panel lacks {want:?}: {texts:?}"
            );
        }

        // The panel's Regenerate has a key too.
        tap(&mut app, key_of(EditorAction::Regenerate));
        assert!(state(&app).regenerate_requested);

        // F1 again closes it and keeps the brush.
        tap(&mut app, TOGGLE_EDITOR_KEY);
        let closed = state(&app);
        assert!(!closed.open);
        assert_eq!(
            (closed.op, closed.shape, closed.size),
            (edited.op, edited.shape, edited.size)
        );
        let (shown, _, _) = draw_panel(&mut app, &ctx);
        assert!(!shown, "F1 closes the panel");
        // And it stays gone on the frames after.
        let (_, texts, _) = draw_panel(&mut app, &ctx);
        assert!(texts.is_empty(), "closed panel draws nothing: {texts:?}");
    }

    // ---- Leaving the fly camera ----

    /// A 64³-cell world, all chunks resident, with a stone floor in cell
    /// rows y = 0..=3 (top at 2.0 m) and a stone block over cells
    /// (20..=27, 4..=11, 20..=27).
    fn rock_world() -> World {
        let table = materials();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 63));
        let mut world = World::new(table.clone(), bounds);
        for pos in bounds.chunks() {
            world
                .insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let stone = table.id("stone").unwrap();
        for (min, max) in [((0, 0, 0), (63, 3, 63)), ((20, 4, 20), (27, 11, 27))] {
            let cmd = EditCommand {
                id: CommandId::default(),
                issuer: PlayerId(0),
                op: EditOp::Set(stone),
                shape: Shape::Box {
                    min: CellPos::new(min.0, min.1, min.2),
                    max: CellPos::new(max.0, max.1, max.2),
                },
            };
            assert_eq!(world.apply(&cmd).status, EditStatus::Applied);
        }
        world
    }

    /// Every blocking cell `feet`'s box shares volume with.
    fn penetrated(world: &World, feet: Vec3) -> Vec<CellPos> {
        let b = Aabb::of_feet(feet);
        let lo = (b.min / S).floor().as_ivec3() - IVec3::ONE;
        let hi = (b.max / S).floor().as_ivec3() + IVec3::ONE;
        let mut out = Vec::new();
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let c = CellPos::new(x, y, z);
                    if b.overlaps_cell(c) && blocks(world, c) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// Fly camera, controller and editor over `world`, a player at `feet`
    /// and the game camera.
    fn fly_app(world: World, feet: Vec3) -> (App, Entity, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 60.0,
            )))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<AccumulatedMouseMotion>()
            .insert_resource(ClientMaterials(materials()))
            .insert_resource(Replica::from_world(world))
            .add_plugins((FlyCamPlugin, ControllerPlugin, EditorPlugin));
        let player = app.world_mut().spawn(Player::at(feet)).id();
        let camera = app
            .world_mut()
            .spawn((GameCamera, Transform::default()))
            .id();
        app.update();
        (app, player, camera)
    }

    fn player(app: &App, e: Entity) -> Player {
        app.world().get::<Player>(e).unwrap().clone()
    }

    /// Turns the fly camera on with V, puts the camera at `eye` looking
    /// `yaw`/`pitch`, then turns it off with V.
    fn fly_to(app: &mut App, camera: Entity, eye: Vec3, yaw: f32, pitch: f32) {
        tap(app, FLY_TOGGLE_KEY);
        assert!(app.world().resource::<FlyCam>().enabled);
        *app.world_mut().get_mut::<Transform>(camera).unwrap() = Transform::from_translation(eye)
            .with_rotation(Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0));
        frame(app);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(FLY_TOGGLE_KEY);
        frame(app);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(FLY_TOGGLE_KEY);
        assert!(!app.world().resource::<FlyCam>().enabled);
    }

    #[test]
    fn controller_reenable_snaps_to_free_cell() {
        let world = rock_world();
        let start = Vec3::new(5.0, 2.0, 5.0);
        let (mut app, p, camera) = fly_app(world.clone(), start);
        for _ in 0..10 {
            frame(&mut app);
        }
        assert!(player(&app, p).grounded, "stands on the floor");

        // Free air: the feet go straight under the camera, with its view,
        // and the camera keeps following the player from there.
        let eye = Vec3::new(30.0, 8.0, 7.0);
        fly_to(&mut app, camera, eye, 0.7, -0.3);
        let pl = player(&app, p);
        assert!(
            (pl.feet - (eye - Vec3::Y * EYE_HEIGHT_M)).length() < 1e-5,
            "{:?}",
            pl.feet
        );
        assert!((pl.yaw - 0.7).abs() < 1e-5 && (pl.pitch + 0.3).abs() < 1e-5);
        assert_eq!(pl.velocity, Vec3::ZERO);
        let cam = *app.world().get::<Transform>(camera).unwrap();
        assert!(
            (cam.translation - eye).length() < 1e-4,
            "no jump back: {cam:?}"
        );
        for _ in 0..120 {
            frame(&mut app);
            assert!(penetrated(&world, player(&app, p).feet).is_empty());
        }
        let pl = player(&app, p);
        assert!(
            pl.grounded && (pl.feet.y - 2.0).abs() < 0.01,
            "fell to the floor"
        );
        assert!((pl.feet.xz() - eye.xz()).length() < 1e-4, "straight down");

        // Inside the block: the player lands on the nearest free spot,
        // the one free_feet_near picks, which is near the camera and free.
        let eye = Vec3::new(11.9, 3.9, 12.1);
        let wanted = eye - Vec3::Y * EYE_HEIGHT_M;
        assert!(!penetrated(&world, wanted).is_empty(), "camera is in rock");
        let expected = free_feet_near(&world, wanted).expect("a free spot nearby");
        assert!(penetrated(&world, expected).is_empty());
        let reach = SNAP_RADIUS_CELLS as f32 * S * 3f32.sqrt() + S;
        assert!((expected - wanted).length() <= reach);
        fly_to(&mut app, camera, eye, -1.2, 0.4);
        let pl = player(&app, p);
        assert_eq!(pl.feet, expected, "snapped");
        assert!((pl.yaw + 1.2).abs() < 1e-5 && (pl.pitch - 0.4).abs() < 1e-5);
        for _ in 0..120 {
            frame(&mut app);
            assert!(penetrated(&world, player(&app, p).feet).is_empty());
        }

        // The snap is the nearest free cell: no free cell bottom is closer.
        let base = (wanted / S).floor().as_ivec3();
        let d2 = |feet: Vec3| {
            let c = (feet / S).floor().as_ivec3() - base;
            c.length_squared()
        };
        let r = SNAP_RADIUS_CELLS;
        for y in -r..=r {
            for z in -r..=r {
                for x in -r..=r {
                    let c = base + IVec3::new(x, y, z);
                    let feet = Vec3::new(
                        (c.x as f32 + 0.5) * S,
                        c.y as f32 * S,
                        (c.z as f32 + 0.5) * S,
                    );
                    if penetrated(&world, feet).is_empty() {
                        assert!(d2(feet) >= d2(expected), "{c:?} is free and closer");
                    }
                }
            }
        }

        // Deep in rock with nothing free in range: the player stays where
        // it stood before flying.
        let before = player(&app, p).feet;
        let deep_world = {
            let mut w = world.clone();
            let cmd = EditCommand {
                id: CommandId::default(),
                issuer: PlayerId(0),
                op: EditOp::Set(materials().id("stone").unwrap()),
                shape: Shape::Box {
                    min: CellPos::new(40, 0, 40),
                    max: CellPos::new(63, 50, 63),
                },
            };
            assert_eq!(w.apply(&cmd).status, EditStatus::Applied);
            w
        };
        let buried = Vec3::new(26.0, 15.0, 26.0);
        assert_eq!(
            free_feet_near(&deep_world, buried),
            None,
            "no room in range"
        );
        app.insert_resource(Replica::from_world(deep_world));
        fly_to(&mut app, camera, buried + Vec3::Y * EYE_HEIGHT_M, 0.0, 0.0);
        assert_eq!(player(&app, p).feet, before, "stays put");
    }

    // ---- The brush ----

    const TOOLS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/tools.ron");

    /// Every message the client wrote for the host.
    #[derive(Resource, Default)]
    struct Sent(Vec<ClientMsg>);

    fn record(mut out: MessageReader<ToHost>, mut sent: ResMut<Sent>) {
        sent.0.extend(out.read().map(|m| m.0.clone()));
    }

    /// Feet south of [`rock_world`]'s block, in cell column (24, 32), facing
    /// -Z. The eye (3.55 m) is in cell row 7, 2 m from the block's +Z face,
    /// so it looks at cell (24, 7, 27).
    const FEET: Vec3 = Vec3::new(12.25, 2.0, 16.0);
    const WALL_HIT: CellPos = CellPos(IVec3::new(24, 7, 27));

    /// The editor with the fly camera, the controller and the pickaxe, over
    /// [`rock_world`], a player at [`FEET`] and the game camera. Records
    /// everything sent to the host.
    struct Brush {
        app: App,
        player: Entity,
        camera: Entity,
        world: World,
    }

    impl Brush {
        fn new() -> Self {
            let world = rock_world();
            let pickaxe = rmc_world::ToolTable::load(TOOLS_PATH)
                .expect("assets/tools.ron loads")
                .pickaxe;
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                    1.0 / 60.0,
                )))
                .init_resource::<ButtonInput<KeyCode>>()
                .init_resource::<ButtonInput<MouseButton>>()
                .init_resource::<AccumulatedMouseMotion>()
                .insert_resource(ClientMaterials(materials()))
                .insert_resource(Replica::from_world(world.clone()))
                .insert_resource(Pickaxe(pickaxe))
                .init_resource::<Sent>()
                .add_plugins((FlyCamPlugin, ControllerPlugin, PickaxePlugin, EditorPlugin))
                .add_systems(Last, record);
            let player = app.world_mut().spawn(Player::at(FEET)).id();
            let camera = app
                .world_mut()
                .spawn((GameCamera, Transform::default()))
                .id();
            app.update();
            let mut brush = Self {
                app,
                player,
                camera,
                world,
            };
            for _ in 0..5 {
                brush.frame();
            }
            brush
        }

        fn frame(&mut self) {
            frame(&mut self.app);
        }

        fn tap(&mut self, key: KeyCode) {
            tap(&mut self.app, key);
        }

        fn act(&mut self, action: EditorAction) {
            self.tap(key_of(action));
        }

        /// Opens the editor with F1.
        fn open(&mut self) {
            self.tap(TOGGLE_EDITOR_KEY);
            assert!(self.state().open);
        }

        fn state(&self) -> EditorState {
            state(&self.app)
        }

        /// Presses the brush button for one frame and releases it.
        fn click(&mut self) {
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .press(BRUSH_BUTTON);
            self.frame();
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<MouseButton>>()
                .release(BRUSH_BUTTON);
            self.frame();
        }

        /// Clicks and returns the edits sent during the click.
        fn click_edits(&mut self) -> Vec<EditCommand> {
            let before = self.edits().len();
            self.click();
            self.edits()[before..].to_vec()
        }

        /// Clicks and returns the one edit the click sent.
        fn click_one(&mut self) -> EditCommand {
            let edits = self.click_edits();
            assert_eq!(edits.len(), 1, "one click, one edit: {edits:?}");
            edits[0]
        }

        fn edits(&self) -> Vec<EditCommand> {
            self.app
                .world()
                .resource::<Sent>()
                .0
                .iter()
                .filter_map(|m| match m {
                    ClientMsg::Edit(cmd) => Some(*cmd),
                    _ => None,
                })
                .collect()
        }

        fn player_mut(&mut self) -> Mut<'_, Player> {
            self.app.world_mut().get_mut::<Player>(self.player).unwrap()
        }

        /// Turns the fly camera on and puts it at `eye` looking along -Z.
        fn fly_at(&mut self, eye: Vec3) {
            if !self.app.world().resource::<FlyCam>().enabled {
                self.tap(FLY_TOGGLE_KEY);
            }
            assert!(self.app.world().resource::<FlyCam>().enabled);
            *self
                .app
                .world_mut()
                .get_mut::<Transform>(self.camera)
                .unwrap() = Transform::from_translation(eye);
        }

        fn material(&self) -> MaterialId {
            self.state()
                .brush_material(&materials())
                .expect("a brush material")
        }

        /// The world after the host applies `cmd`.
        fn applied(&self, cmd: &EditCommand) -> World {
            let mut world = self.world.clone();
            assert_eq!(world.apply(cmd).status, EditStatus::Applied, "{cmd:?}");
            world
        }
    }

    fn cell(x: i32, y: i32, z: i32) -> CellPos {
        CellPos::new(x, y, z)
    }

    #[test]
    fn set_targets_adjacent_cell() {
        let mut b = Brush::new();
        let hit = raycast(
            &b.world,
            FEET + Vec3::Y * EYE_HEIGHT_M,
            Vec3::NEG_Z,
            BRUSH_REACH_M,
        )
        .expect("the block is ahead");
        assert_eq!(hit.cell, WALL_HIT);
        assert_eq!(hit.face_normal, IVec3::Z);

        // Closed: the click is the pickaxe's, not the brush's.
        let edits = b.click_edits();
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert!(matches!(edits[0].op, EditOp::Damage(_)), "{edits:?}");
        let pickaxe_id = edits[0].id;

        // Open, Set, Cell: one Set on the face-normal side of the hit.
        b.open();
        assert_eq!(
            (b.state().op, b.state().shape),
            (BrushOp::Set, BrushShape::Cell)
        );
        let material = b.material();
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Set(material));
        assert_eq!(cmd.shape, Shape::Cell(cell(24, 7, 28)));
        assert_eq!(cmd.issuer, PlayerId(1));
        assert_eq!(
            cmd.id,
            CommandId {
                issuer: PlayerId(1),
                seq: pickaxe_id.seq + 1
            },
            "the brush and the pickaxe share the command sequence"
        );
        let after = b.applied(&cmd);
        assert_eq!(
            after.get_cell(cell(24, 7, 28)),
            Some(material),
            "placed in front"
        );
        assert_eq!(
            after.get_cell(WALL_HIT),
            b.world.get_cell(WALL_HIT),
            "hit cell kept"
        );

        // Holding the button sends nothing more: one edit per click, and the
        // pickaxe does not swing while the editor is open.
        let n = b.edits().len();
        b.app
            .world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(BRUSH_BUTTON);
        for _ in 0..60 {
            b.frame();
        }
        b.app
            .world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .release(BRUSH_BUTTON);
        b.frame();
        assert_eq!(b.edits().len(), n + 1, "{:?}", &b.edits()[n..]);
        assert!(matches!(b.edits()[n].op, EditOp::Set(_)));

        // The picked material is the one placed.
        b.act(EditorAction::NextMaterial);
        let next = b.material();
        assert_ne!(next, material);
        assert_eq!(b.click_one().op, EditOp::Set(next));

        // Box of size 2: a 5-cell cube centered on the adjacent cell.
        b.act(EditorAction::Shape(BrushShape::Box));
        b.act(EditorAction::SizeUp);
        assert_eq!(b.state().size, 2);
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Set(next));
        assert_eq!(
            cmd.shape,
            Shape::Box {
                min: cell(22, 5, 26),
                max: cell(26, 9, 30)
            }
        );

        // Flying, the ray starts at the camera: 6 m from the block, beyond
        // the pickaxe's reach, one cell row above the player's eye.
        b.act(EditorAction::Shape(BrushShape::Cell));
        b.fly_at(Vec3::new(12.25, 4.25, 20.0));
        let cmd = b.click_one();
        assert_eq!(cmd.shape, Shape::Cell(cell(24, 8, 28)));

        // A camera inside rock hits its own cell with no face: Set sends
        // nothing.
        b.fly_at(Vec3::new(12.25, 4.25, 12.25));
        assert!(b.click_edits().is_empty());

        // Looking at the sky, nothing is hit.
        b.tap(FLY_TOGGLE_KEY);
        b.player_mut().feet = FEET;
        b.player_mut().pitch = 1.2;
        b.frame();
        assert!(b.click_edits().is_empty());
    }

    #[test]
    fn remove_targets_hit_cell() {
        let mut b = Brush::new();
        b.open();
        let material = b.material();

        // Remove: the hit cell itself, which the host clears.
        b.act(EditorAction::Op(BrushOp::Remove));
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Remove);
        assert_eq!(cmd.shape, Shape::Cell(WALL_HIT));
        let after = b.applied(&cmd);
        assert_eq!(after.get_cell(WALL_HIT), Some(MaterialId::AIR));
        assert_eq!(
            after.get_cell(cell(24, 7, 26)),
            b.world.get_cell(cell(24, 7, 26))
        );

        // Paint: the hit cell, with the brush material.
        b.act(EditorAction::Op(BrushOp::Paint));
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Paint(material));
        assert_eq!(cmd.shape, Shape::Cell(WALL_HIT));
        assert_eq!(b.applied(&cmd).get_cell(WALL_HIT), Some(material));

        // Box of size 3 around the hit cell.
        b.act(EditorAction::Op(BrushOp::Remove));
        b.act(EditorAction::Shape(BrushShape::Box));
        b.act(EditorAction::SizeUp);
        b.act(EditorAction::SizeUp);
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Remove);
        assert_eq!(
            cmd.shape,
            Shape::Box {
                min: cell(21, 4, 24),
                max: cell(27, 10, 30)
            }
        );

        // From a camera inside rock, Remove takes the cell the camera is in.
        b.act(EditorAction::Shape(BrushShape::Cell));
        b.fly_at(Vec3::new(12.25, 4.25, 12.25));
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Remove);
        assert_eq!(cmd.shape, Shape::Cell(cell(24, 8, 24)));

        // Closing the editor stops the brush.
        b.tap(FLY_TOGGLE_KEY);
        b.player_mut().feet = FEET;
        b.frame();
        b.tap(TOGGLE_EDITOR_KEY);
        let edits = b.click_edits();
        assert!(
            edits.iter().all(|c| matches!(c.op, EditOp::Damage(_))),
            "{edits:?}"
        );
    }

    #[test]
    fn sphere_brush_sends_sphere_shape() {
        let mut b = Brush::new();
        b.open();
        b.act(EditorAction::Shape(BrushShape::Sphere));
        for _ in 0..3 {
            b.act(EditorAction::SizeUp);
        }
        assert_eq!(b.state().size, 4);

        // Remove: a sphere of radius `size` centered on the hit cell. The
        // host clears every cell of it.
        b.act(EditorAction::Op(BrushOp::Remove));
        let cmd = b.click_one();
        let sphere = Shape::Sphere {
            center: WALL_HIT,
            radius: 4,
        };
        assert_eq!(cmd.op, EditOp::Remove);
        assert_eq!(cmd.shape, sphere);
        let after = b.applied(&cmd);
        let cells: Vec<CellPos> = sphere.cells().collect();
        assert_eq!(cells.len() as u64, sphere.cell_count());
        assert!(
            cells
                .iter()
                .any(|&c| b.world.get_cell(c) != Some(MaterialId::AIR)),
            "the sphere reaches into the block"
        );
        for c in &cells {
            assert_eq!(after.get_cell(*c), Some(MaterialId::AIR), "{c:?}");
        }

        // Set: centered on the face-normal side.
        b.act(EditorAction::Op(BrushOp::Set));
        let material = b.material();
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Set(material));
        assert_eq!(
            cmd.shape,
            Shape::Sphere {
                center: cell(24, 7, 28),
                radius: 4
            }
        );

        // Paint: centered on the hit; the radius follows the size.
        b.act(EditorAction::Op(BrushOp::Paint));
        b.act(EditorAction::SizeDown);
        let cmd = b.click_one();
        assert_eq!(cmd.op, EditOp::Paint(material));
        assert_eq!(
            cmd.shape,
            Shape::Sphere {
                center: WALL_HIT,
                radius: 3
            }
        );
    }
}
