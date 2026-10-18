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

use bevy::prelude::*;
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::world::World;

use crate::controller::{Aabb, ControllerSystems, EYE_HEIGHT_M, Player, blocks};
use crate::flycam::{FlyCam, FlyCamSystems};
use crate::interp::InterpolatedEye;
use crate::net::ClientMaterials;
use crate::render::GameCamera;
use crate::replica::Replica;

/// Opens and closes the editor.
pub const TOGGLE_EDITOR_KEY: KeyCode = KeyCode::F1;

/// Largest brush size. A [`BrushShape::Box`] of this size, `2 * size + 1`
/// cells on a side, still fits in one edit command
/// ([`MAX_EDIT_CELLS`](rmc_world::edit::MAX_EDIT_CELLS)).
pub const MAX_BRUSH_SIZE: u16 = 15;

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

/// The debug editor's state and keys, and the drop out of the fly camera.
pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>().add_systems(
            Update,
            (
                editor_keys,
                drop_player_after_flying
                    .after(FlyCamSystems)
                    .before(ControllerSystems::Look),
            )
                .in_set(EditorSystems),
        );
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
    use crate::pickaxe::SWING_BUTTON;
    use bevy::input::mouse::AccumulatedMouseMotion;
    use bevy::time::TimeUpdateStrategy;
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
}
