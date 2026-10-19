//! The debug editor's gizmos (SAD §11.6): chunk bounds, the targeted cell
//! and the mesh-queue state per chunk.
//!
//! [`EditorGizmos`] mirrors what is drawn, so tests assert on it instead of
//! reading pixels; the gizmos are drawn from it and nothing else. It is
//! empty while the editor is closed, and each part is empty while its
//! [`GizmoToggles`](crate::editor::GizmoToggles) flag is off.
//!
//! - Chunk bounds: the replica's chunks within [`CHUNK_BOUNDS_RADIUS`]
//!   chunks (on every axis) of the chunk the view ray starts in.
//! - Targeted cell: the cell a click of the brush would hit, from the same
//!   view ray and reach as the brush (the fly camera while it is on, else
//!   the player's eye; [`BRUSH_REACH_M`](crate::editor::BRUSH_REACH_M)),
//!   and the cell the brush's shape would be centered on
//!   ([`brush_target`]). This is not the pickaxe's target, which the HUD
//!   outlines.
//! - Mesh queue: every chunk the [`MeshQueue`] knows with its earliest
//!   state, each drawn a little inside its bounds in its state's color
//!   ([`mesh_state_color`]; meshed chunks are faint).
//!
//! The draw system is added only to apps with Bevy's `GizmoPlugin`; a
//! logic-only app still fills [`EditorGizmos`].

use bevy::gizmos::GizmoPlugin;
use bevy::prelude::*;
use rmc_world::coords::{CELL_SIZE_M, CHUNK_SIZE, CellPos, ChunkPos};
use rmc_world::world::World;

use crate::controller::{ControllerSystems, Player};
use crate::editor::{EditorState, EditorSystems, brush_hit, brush_target, view_ray};
use crate::flycam::{FlyCam, FlyCamSystems};
use crate::meshpipe::{ChunkMeshState, MeshQueue, MeshSystems};
use crate::render::GameCamera;
use crate::replica::Replica;

/// How many chunks around the view chunk, on each axis, get their bounds
/// drawn.
pub const CHUNK_BOUNDS_RADIUS: i32 = 1;

/// The chunk-bounds color.
pub const CHUNK_BOUNDS_COLOR: Color = Color::srgb(0.9, 0.9, 0.9);

/// The targeted (hit) cell's color.
pub const HIT_CELL_COLOR: Color = Color::srgb(1.0, 0.2, 0.8);

/// The color of the cell the brush is centered on, when it is not the hit
/// cell (Set).
pub const BRUSH_CELL_COLOR: Color = Color::srgb(0.2, 0.9, 1.0);

/// Cell outlines are drawn this much larger than the cell, so the cell's
/// faces do not hide them.
pub const CELL_GIZMO_SCALE: f32 = 1.04;

/// Mesh-queue boxes are drawn this much smaller than the chunk, so they do
/// not cover the chunk bounds.
pub const MESH_QUEUE_INSET: f32 = 0.96;

/// The color a mesh-queue box is drawn in for `state`.
pub fn mesh_state_color(state: ChunkMeshState) -> Color {
    match state {
        ChunkMeshState::Queued => Color::srgb(1.0, 0.85, 0.1),
        ChunkMeshState::InFlight => Color::srgb(1.0, 0.45, 0.1),
        ChunkMeshState::AwaitingUpload => Color::srgb(0.3, 0.5, 1.0),
        // The resting state, faint so pending chunks stand out.
        ChunkMeshState::Meshed => Color::srgba(0.2, 0.8, 0.3, 0.15),
    }
}

/// The targeted-cell gizmo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetGizmo {
    /// The cell the brush's view ray hits.
    pub cell: CellPos,
    /// The hit's face normal (zero when the ray starts inside the cell).
    pub face_normal: IVec3,
    /// The cell the brush's shape is centered on, `None` when the brush
    /// would send nothing for this hit.
    pub brush: Option<CellPos>,
}

/// What the editor's gizmos show this frame.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorGizmos {
    /// Chunks whose bounds are drawn, ascending.
    pub chunk_bounds: Vec<ChunkPos>,
    /// The targeted cell, `None` when nothing is hit.
    pub target: Option<TargetGizmo>,
    /// Every chunk the mesh pipeline knows and its state, ascending.
    pub mesh_queue: Vec<(ChunkPos, ChunkMeshState)>,
}

/// The gizmo systems, in `Update`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GizmoSystems {
    /// Fills [`EditorGizmos`].
    Update,
    /// Draws it (apps with `GizmoPlugin` only).
    Draw,
}

/// Fills and draws [`EditorGizmos`].
/// [`EditorPlugin`](crate::editor::EditorPlugin) adds it.
pub struct EditorGizmoPlugin;

impl Plugin for EditorGizmoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorGizmos>().add_systems(
            Update,
            update_gizmos
                .in_set(GizmoSystems::Update)
                .after(EditorSystems)
                .after(FlyCamSystems)
                .after(ControllerSystems::Look)
                .after(MeshSystems),
        );
        // A logic-only app has no gizmos.
        if app.is_plugin_added::<GizmoPlugin>() {
            app.add_systems(
                Update,
                draw_gizmos
                    .in_set(GizmoSystems::Draw)
                    .after(GizmoSystems::Update),
            );
        }
    }
}

/// Recomputes [`EditorGizmos`] from the editor state, the view ray, the
/// replica and the mesh queue.
#[allow(clippy::too_many_arguments)]
fn update_gizmos(
    state: Res<EditorState>,
    replica: Option<Res<Replica>>,
    queue: Option<Res<MeshQueue>>,
    fly: Option<Res<FlyCam>>,
    cameras: Query<&Transform, With<GameCamera>>,
    players: Query<&Player>,
    mut gizmos: ResMut<EditorGizmos>,
) {
    let mut view = EditorGizmos::default();
    if state.open {
        let toggles = state.gizmos;
        if let Some(replica) = replica.as_deref() {
            let world = replica.world();
            if toggles.chunk_bounds
                && let Some((origin, _)) = view_ray(fly.as_deref(), &cameras, &players)
            {
                view.chunk_bounds = chunks_around(world, CellPos::from_meters(origin).chunk());
            }
            if toggles.target_cell {
                view.target =
                    brush_hit(world, fly.as_deref(), &cameras, &players).map(|hit| TargetGizmo {
                        cell: hit.cell,
                        face_normal: hit.face_normal,
                        brush: brush_target(state.op, hit),
                    });
            }
        }
        if toggles.mesh_queue
            && let Some(queue) = queue
        {
            view.mesh_queue = queue.states();
        }
    }
    gizmos.set_if_neq(view);
}

/// The resident chunks of `world` within [`CHUNK_BOUNDS_RADIUS`] of
/// `center` on every axis, ascending.
fn chunks_around(world: &World, center: ChunkPos) -> Vec<ChunkPos> {
    let r = CHUNK_BOUNDS_RADIUS;
    let mut chunks: Vec<ChunkPos> = (-r..=r)
        .flat_map(|y| (-r..=r).flat_map(move |z| (-r..=r).map(move |x| IVec3::new(x, y, z))))
        .map(|offset| ChunkPos(center.0 + offset))
        .filter(|&pos| world.chunk(pos).is_some())
        .collect();
    chunks.sort_unstable();
    chunks
}

/// Draws [`EditorGizmos`].
fn draw_gizmos(view: Res<EditorGizmos>, mut gizmos: Gizmos) {
    let chunk_m = chunk_size_m();
    let chunk_box = |pos: ChunkPos, scale: f32| {
        let center = pos.origin_cell().min_corner_m() + Vec3::splat(chunk_m / 2.0);
        Transform::from_translation(center).with_scale(Vec3::splat(chunk_m * scale))
    };
    for &pos in &view.chunk_bounds {
        gizmos.cube(chunk_box(pos, 1.0), CHUNK_BOUNDS_COLOR);
    }
    for &(pos, state) in &view.mesh_queue {
        gizmos.cube(chunk_box(pos, MESH_QUEUE_INSET), mesh_state_color(state));
    }
    if let Some(target) = view.target {
        gizmos.cube(cell_box(target.cell), HIT_CELL_COLOR);
        if let Some(brush) = target.brush.filter(|&c| c != target.cell) {
            gizmos.cube(cell_box(brush), BRUSH_CELL_COLOR);
        }
    }
}

/// A chunk's edge length in meters.
fn chunk_size_m() -> f32 {
    CHUNK_SIZE as f32 * CELL_SIZE_M
}

/// The outline transform of `cell`.
fn cell_box(cell: CellPos) -> Transform {
    Transform::from_translation(cell.center_m()).with_scale(Vec3::splat(
        rmc_world::coords::CELL_SIZE_M * CELL_GIZMO_SCALE,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{ControllerPlugin, EYE_HEIGHT_M};
    use crate::editor::{
        BRUSH_BUTTON, BRUSH_REACH_M, BrushOp, EDITOR_BINDINGS, EditorAction, EditorPlugin,
        TOGGLE_EDITOR_KEY,
    };
    use crate::flycam::{FLY_TOGGLE_KEY, FlyCamPlugin};
    use crate::meshpipe::{MeshBudget, MeshPipelinePlugin};
    use crate::net::{ClientMaterials, ToHost};
    use bevy::input::mouse::AccumulatedMouseMotion;
    use bevy::time::TimeUpdateStrategy;
    use rmc_protocol::ClientMsg;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::CellAabb;
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::raycast;
    use rmc_world::shape::Shape;
    use std::sync::Arc;
    use std::time::Duration;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// Feet south of [`rock_world`]'s block, facing -Z; the eye looks at
    /// cell (24, 7, 27) through its +Z face (as in the editor's brush tests).
    const FEET: Vec3 = Vec3::new(12.25, 2.0, 16.0);

    fn materials() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"))
    }

    /// 64³ cells (2 × 2 × 2 chunks) of air on a 4-cell stone floor, with an
    /// 8-cell stone block at (20..=27, 4..=11, 20..=27).
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

    #[derive(Resource, Default)]
    struct Sent(Vec<ClientMsg>);

    fn record(mut out: MessageReader<ToHost>, mut sent: ResMut<Sent>) {
        sent.0.extend(out.read().map(|m| m.0.clone()));
    }

    struct Fixture {
        app: App,
        player: Entity,
        camera: Entity,
        world: World,
    }

    impl Fixture {
        /// The editor, fly camera, controller and a mesh pipeline that
        /// starts no jobs, over [`rock_world`] with every chunk dirty, a
        /// player at [`FEET`] and the game camera.
        fn new() -> Self {
            let world = rock_world();
            let mut replica = Replica::from_world(world.clone());
            replica.mark_all_dirty();
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                    1.0 / 60.0,
                )))
                .init_resource::<ButtonInput<KeyCode>>()
                .init_resource::<ButtonInput<MouseButton>>()
                .init_resource::<AccumulatedMouseMotion>()
                .insert_resource(ClientMaterials(materials()))
                .insert_resource(replica)
                .insert_resource(MeshBudget {
                    max_in_flight: 0,
                    ..MeshBudget::default()
                })
                .init_resource::<Sent>()
                .add_plugins((
                    FlyCamPlugin,
                    ControllerPlugin,
                    MeshPipelinePlugin,
                    EditorPlugin,
                ))
                .add_systems(Last, record);
            let player = app.world_mut().spawn(Player::at(FEET)).id();
            let camera = app
                .world_mut()
                .spawn((GameCamera, Transform::default()))
                .id();
            let mut f = Self {
                app,
                player,
                camera,
                world,
            };
            for _ in 0..5 {
                f.frame();
            }
            f
        }

        fn frame(&mut self) {
            self.app.update();
            let world = self.app.world_mut();
            world.resource_mut::<ButtonInput<KeyCode>>().clear();
            world.resource_mut::<ButtonInput<MouseButton>>().clear();
            world.resource_mut::<AccumulatedMouseMotion>().delta = Vec2::ZERO;
        }

        fn tap(&mut self, key: KeyCode) {
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(key);
            self.frame();
            self.app
                .world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .release(key);
            self.frame();
        }

        fn act(&mut self, action: EditorAction) {
            let key = EDITOR_BINDINGS
                .iter()
                .find(|(_, a)| *a == action)
                .map(|(k, _)| *k)
                .unwrap_or_else(|| panic!("{action:?} has no key"));
            self.tap(key);
        }

        fn gizmos(&self) -> EditorGizmos {
            self.app.world().resource::<EditorGizmos>().clone()
        }

        fn target(&self) -> Option<TargetGizmo> {
            self.gizmos().target
        }

        /// Clicks the brush and returns the shape of the one edit it sent.
        fn click_shape(&mut self) -> Shape {
            let before = self.app.world().resource::<Sent>().0.len();
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
            let edits: Vec<EditCommand> = self.app.world().resource::<Sent>().0[before..]
                .iter()
                .filter_map(|m| match m {
                    ClientMsg::Edit(cmd) => Some(*cmd),
                    _ => None,
                })
                .collect();
            assert_eq!(edits.len(), 1, "one click, one edit: {edits:?}");
            edits[0].shape
        }

        /// Turns the fly camera on and puts it at `eye` looking along -Z.
        fn fly_at(&mut self, eye: Vec3) {
            if !self.app.world().resource::<FlyCam>().enabled {
                self.tap(FLY_TOGGLE_KEY);
            }
            *self
                .app
                .world_mut()
                .get_mut::<Transform>(self.camera)
                .unwrap() = Transform::from_translation(eye);
            self.frame();
        }

        /// The gizmo the editor should show for a ray from `eye` along -Z.
        fn expected(&self, eye: Vec3, op: BrushOp) -> Option<TargetGizmo> {
            raycast(&self.world, eye, Vec3::NEG_Z, BRUSH_REACH_M).map(|hit| TargetGizmo {
                cell: hit.cell,
                face_normal: hit.face_normal,
                brush: brush_target(op, hit),
            })
        }
    }

    #[test]
    fn targeted_cell_gizmo_matches_hit() {
        let mut f = Fixture::new();
        let eye = FEET + Vec3::Y * EYE_HEIGHT_M;

        // Closed: nothing is drawn, and the gizmo keys do nothing.
        assert_eq!(f.gizmos(), EditorGizmos::default(), "closed");
        f.act(EditorAction::ToggleTargetCell);
        assert_eq!(f.gizmos(), EditorGizmos::default(), "F3 while closed");

        // Open with every toggle off: still nothing.
        f.tap(TOGGLE_EDITOR_KEY);
        assert_eq!(f.gizmos(), EditorGizmos::default(), "toggles off");

        // The targeted cell is the brush's raycast hit from the eye.
        f.act(EditorAction::ToggleTargetCell);
        let g = f.gizmos();
        assert!(
            g.chunk_bounds.is_empty() && g.mesh_queue.is_empty(),
            "{g:?}"
        );
        let want = f.expected(eye, BrushOp::Set).expect("the block is ahead");
        assert_eq!(
            want,
            TargetGizmo {
                cell: CellPos::new(24, 7, 27),
                face_normal: IVec3::Z,
                brush: Some(CellPos::new(24, 7, 28)),
            }
        );
        assert_eq!(f.target(), Some(want), "Set: hit, and the cell in front");
        assert_eq!(
            f.click_shape(),
            Shape::Cell(want.brush.unwrap()),
            "the brush acts where the gizmo shows"
        );

        // Remove centers the brush on the hit cell itself.
        f.act(EditorAction::Op(BrushOp::Remove));
        let want = f.expected(eye, BrushOp::Remove).unwrap();
        assert_eq!(want.brush, Some(want.cell));
        assert_eq!(f.target(), Some(want), "Remove: the hit cell");
        assert_eq!(f.click_shape(), Shape::Cell(want.cell));

        // The target follows the player's view.
        f.act(EditorAction::Op(BrushOp::Set));
        f.app.world_mut().get_mut::<Player>(f.player).unwrap().feet = FEET + Vec3::Y * 1.5;
        f.frame();
        let raised = FEET + Vec3::Y * (1.5 + EYE_HEIGHT_M);
        let want = f.expected(raised, BrushOp::Set).unwrap();
        assert_eq!(want.cell, CellPos::new(24, 10, 27));
        assert_eq!(f.target(), Some(want), "after the player moved");

        // Flying, the ray starts at the camera, beyond the pickaxe's reach.
        let cam = Vec3::new(12.25, 4.25, 20.0);
        f.fly_at(cam);
        let want = f.expected(cam, BrushOp::Set).unwrap();
        assert_eq!(want.cell, CellPos::new(24, 8, 27));
        assert_eq!(f.target(), Some(want), "from the fly camera");
        assert_eq!(f.click_shape(), Shape::Cell(want.brush.unwrap()));

        // A camera inside rock hits its own cell with no face: Set has no
        // brush cell.
        let inside = Vec3::new(12.25, 4.25, 12.25);
        f.fly_at(inside);
        let want = f.expected(inside, BrushOp::Set).unwrap();
        assert_eq!(want.face_normal, IVec3::ZERO);
        assert_eq!(
            f.target(),
            Some(TargetGizmo {
                cell: CellPos::from_meters(inside),
                face_normal: IVec3::ZERO,
                brush: None,
            })
        );

        // Looking at the sky, nothing is targeted.
        f.app
            .world_mut()
            .get_mut::<Transform>(f.camera)
            .unwrap()
            .rotation = Quat::from_rotation_x(1.2);
        f.app
            .world_mut()
            .get_mut::<Transform>(f.camera)
            .unwrap()
            .translation = Vec3::new(12.25, 8.0, 20.0);
        f.frame();
        assert_eq!(f.target(), None, "sky");

        // Chunk bounds: the chunks around the camera's chunk (0, 0, 1),
        // which here is every chunk of the 2 × 2 × 2 world.
        f.act(EditorAction::ToggleChunkBounds);
        let all: Vec<ChunkPos> = f.world.iter_chunks_sorted().map(|(p, _)| p).collect();
        assert_eq!(all.len(), 8);
        assert_eq!(f.gizmos().chunk_bounds, all, "chunk bounds");

        // Mesh queue: no job ever starts, so every chunk stays queued.
        f.act(EditorAction::ToggleMeshQueue);
        let queued: Vec<_> = all.iter().map(|&p| (p, ChunkMeshState::Queued)).collect();
        assert_eq!(f.gizmos().mesh_queue, queued, "mesh queue");

        // Each toggle turns its part off alone.
        f.act(EditorAction::ToggleChunkBounds);
        let g = f.gizmos();
        assert!(g.chunk_bounds.is_empty());
        assert_eq!(g.mesh_queue, queued);

        // Closing the editor clears every gizmo.
        f.tap(TOGGLE_EDITOR_KEY);
        assert_eq!(f.gizmos(), EditorGizmos::default(), "closed again");
    }
}
