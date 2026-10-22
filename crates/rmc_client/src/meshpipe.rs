//! The async mesh pipeline (SAD §11.2).
//!
//! Every frame, [`MeshPipelinePlugin`] takes the [`Replica`]'s dirty set into
//! a queue, starts mesh jobs for the queued chunks nearest the camera on the
//! [`AsyncComputeTaskPool`], polls the running jobs with `check_ready`, and
//! uploads finished meshes as [`ChunkMesh`] entities.
//!
//! - A job copies the chunk and its border into a [`MeshInput`] when it
//!   starts, so it never reads the replica again. A result older than the
//!   chunk's current revision is discarded and the chunk re-queued. A chunk
//!   that is dirtied again while its job runs stays queued and gets a new job
//!   when the old one ends (a neighbour's border change does not bump the
//!   chunk's revision, so the old result is still uploaded first).
//! - Budgets ([`MeshBudget`]): at most 8 jobs in flight and 4 uploads per
//!   frame. Until the replica has been fully meshed once (the initial load)
//!   the upload budget is 32. The initial load ends on the first frame where
//!   every chunk inside the bounds is resident and nothing is queued,
//!   running, awaiting upload or dirty, so snapshots that arrive over several
//!   frames all upload at the burst budget.
//! - Nearest first: jobs start, and finished meshes upload, in order of the
//!   squared cell distance from the chunk's center to the camera, ties by
//!   chunk position. The camera is the active `Camera3d` with the lowest
//!   order (then the lowest entity); with none, the world origin.
//! - A new `Welcome` drops every job, queue entry and chunk mesh: the new
//!   replica marks all its chunks dirty itself.
//! - The layer overlay (SAD-P2 §10): jobs mesh with the current
//!   [`MeshColorSource`]. When it changes, every resident chunk is marked
//!   dirty and remeshed with the initial-load (burst) upload budget until the
//!   pipeline is idle again ([`MeshStats::recoloring`]). While the source is
//!   a layer, the replica's layer changes remesh the chunks they touch
//!   ([`Replica::set_layer_remesh`]); otherwise only liquid meshes follow
//!   layers.
//!
//! Each job runs both passes of the [`Mesher`]: the opaque mesh goes on a
//! [`ChunkMesh`] entity, and the liquid pass (SAD-P2 §10) on a separate
//! [`LiquidMesh`] entity at the same corner, which the renderer draws
//! alpha-blended after the opaque pass. The two upload together, and each
//! has an entity only while its mesh is not empty.
//!
//! A chunk whose mesh is empty has no entity. Mesh assets are created only
//! when `Assets<Mesh>` exists (a logic-only app has no asset plugin) and use
//! `RenderAssetUsages::RENDER_WORLD`. [`MeshStats`] mirrors the queue for the
//! stats panel and tests.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::ecs::component::Mutable;
use bevy::ecs::query::QueryFilter;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready};
use rmc_protocol::ServerMsg;
use rmc_world::coords::{CHUNK_SIZE, CellPos, ChunkPos};

use crate::eco::ClientEco;
use crate::mesher::{ChunkMesher, ColorSource, GreedyMesher, MeshData, MeshInput};
use crate::net::FromHost;
use crate::replica::Replica;

/// Job and upload budgets. Insert before [`MeshPipelinePlugin`] (or change
/// the resource at any time) to configure them.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshBudget {
    /// Most mesh jobs running at once.
    pub max_in_flight: usize,
    /// Most meshes uploaded per frame once the replica has been fully meshed.
    pub uploads_per_frame: usize,
    /// Most meshes uploaded per frame during the initial load.
    pub initial_uploads_per_frame: usize,
}

impl Default for MeshBudget {
    fn default() -> Self {
        Self {
            max_in_flight: 8,
            uploads_per_frame: 4,
            initial_uploads_per_frame: 32,
        }
    }
}

/// The mesher the jobs run. [`GreedyMesher`] by default.
#[derive(Resource, Clone)]
pub struct Mesher(pub Arc<dyn ChunkMesher>);

impl Default for Mesher {
    fn default() -> Self {
        Self(Arc::new(GreedyMesher))
    }
}

/// What colors the chunk meshes (SAD-P2 §10): the materials, or the layer
/// overlay the editor picked. The jobs mesh with the current source.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshColorSource(pub ColorSource);

/// The uploaded mesh of one chunk. The entity sits at the chunk's minimum
/// corner, since mesh positions are chunk-local.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkMesh {
    pub pos: ChunkPos,
    /// The chunk revision the mesh was built from.
    pub revision: u64,
    pub quads: u32,
}

/// The uploaded liquid-pass mesh of one chunk (SAD-P2 §10), on its own
/// entity at the chunk's minimum corner.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiquidMesh {
    pub pos: ChunkPos,
    /// The chunk revision the mesh was built from.
    pub revision: u64,
    pub quads: u32,
}

/// Where a chunk is in the pipeline. A chunk can be in several states at
/// once (meshed and queued again, say); [`MeshQueue::state`] reports the
/// earliest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChunkMeshState {
    /// Waiting for a job.
    Queued,
    /// A job is running.
    InFlight,
    /// Finished, waiting for the upload budget.
    AwaitingUpload,
    /// The latest upload is current.
    Meshed,
}

/// Mesh pipeline counters, updated at the end of every frame's pipeline run.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct MeshStats {
    /// Chunks with an uploaded mesh (empty meshes included).
    pub meshed_chunks: usize,
    /// Meshed chunks whose mesh has at least one quad (one entity each).
    pub chunks_with_geometry: usize,
    /// Quads over all uploaded opaque meshes.
    pub quads: u64,
    /// Meshed chunks whose liquid mesh has at least one quad.
    pub chunks_with_liquid: usize,
    /// Quads over all uploaded liquid meshes.
    pub liquid_quads: u64,
    /// Vertex and index bytes over all uploaded meshes, both passes.
    pub mesh_bytes: u64,
    /// Chunks waiting for a job.
    pub queued: usize,
    /// Jobs running.
    pub in_flight: usize,
    /// Finished meshes waiting for the upload budget.
    pub awaiting_upload: usize,
    /// Jobs started since the last `Welcome`.
    pub jobs_started: u64,
    /// Results discarded as older than their chunk since the last `Welcome`.
    pub jobs_discarded: u64,
    /// Meshes uploaded since the last `Welcome`.
    pub uploads: u64,
    /// Meshes uploaded this frame.
    pub uploads_last_frame: usize,
    /// How long the job of the most recent upload took to mesh, in ms.
    pub last_mesh_ms: f32,
    /// Whether the replica has been fully meshed once since the last
    /// `Welcome`: every chunk in its bounds resident and meshed.
    pub initial_load_done: bool,
    /// Whether a change of [`MeshColorSource`] is being remeshed: from the
    /// change until the pipeline is idle and nothing is dirty. Uploads use
    /// the initial-load budget meanwhile.
    pub recoloring: bool,
}

/// A job's output: the opaque mesh and the liquid mesh.
type Meshes = (MeshData, MeshData);

struct Job {
    revision: u64,
    task: Task<(Meshes, Duration)>,
}

struct Finished {
    revision: u64,
    data: MeshData,
    liquid: MeshData,
    took: Duration,
}

struct Meshed {
    revision: u64,
    entity: Option<Entity>,
    quads: u32,
    liquid_entity: Option<Entity>,
    liquid_quads: u32,
    bytes: u64,
}

/// The pipeline's state: queued chunks, running jobs, finished meshes and
/// uploaded meshes.
#[derive(Resource, Default)]
pub struct MeshQueue {
    queued: BTreeSet<ChunkPos>,
    in_flight: BTreeMap<ChunkPos, Job>,
    ready: BTreeMap<ChunkPos, Finished>,
    meshed: BTreeMap<ChunkPos, Meshed>,
    jobs_started: u64,
    jobs_discarded: u64,
    uploads: u64,
    uploads_last_frame: usize,
    last_mesh_ms: f32,
    initial_load_done: bool,
    /// The color source the current meshes were queued with.
    color_source: ColorSource,
    /// A color source change is being remeshed.
    recoloring: bool,
}

impl MeshQueue {
    /// The earliest state `pos` is in, or `None` if the pipeline does not
    /// know it.
    pub fn state(&self, pos: ChunkPos) -> Option<ChunkMeshState> {
        if self.queued.contains(&pos) {
            Some(ChunkMeshState::Queued)
        } else if self.in_flight.contains_key(&pos) {
            Some(ChunkMeshState::InFlight)
        } else if self.ready.contains_key(&pos) {
            Some(ChunkMeshState::AwaitingUpload)
        } else if self.meshed.contains_key(&pos) {
            Some(ChunkMeshState::Meshed)
        } else {
            None
        }
    }

    /// Every chunk the pipeline knows with its earliest state
    /// ([`MeshQueue::state`]), ascending.
    pub fn states(&self) -> Vec<(ChunkPos, ChunkMeshState)> {
        let known: BTreeSet<ChunkPos> = self
            .queued
            .iter()
            .chain(self.in_flight.keys())
            .chain(self.ready.keys())
            .chain(self.meshed.keys())
            .copied()
            .collect();
        known
            .into_iter()
            .filter_map(|pos| self.state(pos).map(|state| (pos, state)))
            .collect()
    }

    /// Chunks whose job is running, ascending.
    pub fn in_flight(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.in_flight.keys().copied()
    }

    /// Chunks waiting for a job, ascending.
    pub fn queued(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.queued.iter().copied()
    }

    /// The revision of `pos`'s uploaded mesh.
    pub fn meshed_revision(&self, pos: ChunkPos) -> Option<u64> {
        self.meshed.get(&pos).map(|m| m.revision)
    }

    /// The entity of `pos`'s uploaded mesh (`None` if not meshed or empty).
    pub fn entity(&self, pos: ChunkPos) -> Option<Entity> {
        self.meshed.get(&pos).and_then(|m| m.entity)
    }

    /// The entity of `pos`'s uploaded liquid mesh (`None` if not meshed or
    /// empty).
    pub fn liquid_entity(&self, pos: ChunkPos) -> Option<Entity> {
        self.meshed.get(&pos).and_then(|m| m.liquid_entity)
    }

    /// Running jobs whose task has finished (collected at the next poll).
    pub fn jobs_finished(&self) -> usize {
        self.in_flight
            .values()
            .filter(|job| job.task.is_finished())
            .count()
    }

    /// Whether nothing is queued, running or waiting for upload.
    pub fn is_idle(&self) -> bool {
        self.queued.is_empty() && self.in_flight.is_empty() && self.ready.is_empty()
    }

    fn stats(&self) -> MeshStats {
        MeshStats {
            meshed_chunks: self.meshed.len(),
            chunks_with_geometry: self.meshed.values().filter(|m| m.quads > 0).count(),
            quads: self.meshed.values().map(|m| u64::from(m.quads)).sum(),
            chunks_with_liquid: self.meshed.values().filter(|m| m.liquid_quads > 0).count(),
            liquid_quads: self
                .meshed
                .values()
                .map(|m| u64::from(m.liquid_quads))
                .sum(),
            mesh_bytes: self.meshed.values().map(|m| m.bytes).sum(),
            queued: self.queued.len(),
            in_flight: self.in_flight.len(),
            awaiting_upload: self.ready.len(),
            jobs_started: self.jobs_started,
            jobs_discarded: self.jobs_discarded,
            uploads: self.uploads,
            uploads_last_frame: self.uploads_last_frame,
            last_mesh_ms: self.last_mesh_ms,
            initial_load_done: self.initial_load_done,
            recoloring: self.recoloring,
        }
    }
}

/// The pipeline's systems, in `Update`, in order.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshSystems;

/// Meshes the [`Replica`]'s dirty chunks. Needs
/// [`ReplicaPlugin`](crate::replica::ReplicaPlugin).
pub struct MeshPipelinePlugin;

impl Plugin for MeshPipelinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MeshBudget>()
            .init_resource::<MeshColorSource>()
            .init_resource::<Mesher>()
            .init_resource::<MeshQueue>()
            .init_resource::<MeshStats>()
            .add_systems(
                Update,
                (
                    reset_on_welcome,
                    follow_color_source,
                    queue_dirty,
                    poll_jobs,
                    start_jobs,
                    upload_meshes,
                )
                    .chain()
                    .in_set(MeshSystems),
            );
    }
}

/// Squared cell distance from the chunk's center to `focus`, then the chunk
/// position: the nearest-first order.
fn distance_key(pos: ChunkPos, focus: CellPos) -> (i64, ChunkPos) {
    let d = (pos.origin_cell().0 + IVec3::splat(CHUNK_SIZE / 2) - focus.0).as_i64vec3();
    (d.length_squared(), pos)
}

/// The cell the camera is in: the active `Camera3d` with the lowest order,
/// then the lowest entity. The world origin without one.
fn focus_cell(cameras: &Query<(Entity, &Camera, &GlobalTransform), With<Camera3d>>) -> CellPos {
    cameras
        .iter()
        .filter(|(_, camera, _)| camera.is_active)
        .min_by_key(|(entity, camera, _)| (camera.order, *entity))
        .map(|(_, _, transform)| CellPos::from_meters(transform.translation()))
        .unwrap_or_default()
}

/// Either mesh entity of a chunk.
type AnyMesh = Or<(With<ChunkMesh>, With<LiquidMesh>)>;

/// Drops all pipeline state and chunk meshes when a new replica starts.
fn reset_on_welcome(
    mut commands: Commands,
    mut inbox: MessageReader<FromHost>,
    mut queue: ResMut<MeshQueue>,
    meshes: Query<Entity, AnyMesh>,
) {
    let welcomed = inbox
        .read()
        .any(|FromHost(msg)| matches!(msg, ServerMsg::Welcome { .. }));
    if !welcomed {
        return;
    }
    // Dropping a task cancels it.
    *queue = MeshQueue::default();
    for entity in &meshes {
        commands.entity(entity).despawn();
    }
}

/// Follows [`MeshColorSource`]: on a change, marks every resident chunk
/// dirty and starts a recolor (burst upload budget). Keeps the replica's
/// layer remesh on while the source is a layer.
fn follow_color_source(
    source: Res<MeshColorSource>,
    replica: Option<ResMut<Replica>>,
    mut queue: ResMut<MeshQueue>,
) {
    let Some(mut replica) = replica else {
        return;
    };
    let overlay = source.0 != ColorSource::Material;
    if replica.layer_remesh() != overlay {
        replica.set_layer_remesh(overlay);
    }
    if queue.color_source != source.0 {
        queue.color_source = source.0;
        queue.recoloring = true;
        replica.mark_all_dirty();
        info!(source = ?source.0, "mesh color source changed, remeshing");
    }
}

/// Moves the replica's dirty chunks into the queue.
fn queue_dirty(replica: Option<ResMut<Replica>>, mut queue: ResMut<MeshQueue>) {
    let Some(mut replica) = replica else {
        return;
    };
    if replica.dirty().is_empty() {
        return;
    }
    let dirty = replica.take_dirty();
    debug!(count = dirty.len(), "chunks queued for meshing");
    queue.queued.extend(dirty);
}

/// Collects finished jobs. A result older than its chunk is discarded and
/// the chunk re-queued; a result for a chunk that is no longer resident is
/// dropped with the chunk's mesh.
fn poll_jobs(mut commands: Commands, replica: Option<Res<Replica>>, mut queue: ResMut<MeshQueue>) {
    let Some(replica) = replica else {
        return;
    };
    let queue = &mut *queue;
    let mut done = Vec::new();
    for (&pos, job) in queue.in_flight.iter_mut() {
        if let Some(output) = check_ready(&mut job.task) {
            done.push((pos, job.revision, output));
        }
    }
    for (pos, revision, ((data, liquid), took)) in done {
        queue.in_flight.remove(&pos);
        match replica.world().chunk(pos) {
            None => {
                queue.ready.remove(&pos);
                remove_mesh(&mut commands, queue, pos);
            }
            Some(chunk) if revision < chunk.revision => {
                debug!(
                    ?pos,
                    revision,
                    current = chunk.revision,
                    "stale mesh discarded"
                );
                queue.jobs_discarded += 1;
                queue.queued.insert(pos);
            }
            Some(_) => {
                queue.ready.insert(
                    pos,
                    Finished {
                        revision,
                        data,
                        liquid,
                        took,
                    },
                );
            }
        }
    }
}

/// Starts jobs for the queued chunks nearest the camera, up to the in-flight
/// budget. A chunk whose job is still running waits for it to end.
fn start_jobs(
    mut commands: Commands,
    replica: Option<Res<Replica>>,
    budget: Res<MeshBudget>,
    mesher: Res<Mesher>,
    eco: Option<Res<ClientEco>>,
    cameras: Query<(Entity, &Camera, &GlobalTransform), With<Camera3d>>,
    mut queue: ResMut<MeshQueue>,
) {
    let Some(replica) = replica else {
        return;
    };
    let queue = &mut *queue;
    let slots = budget.max_in_flight.saturating_sub(queue.in_flight.len());
    if slots == 0 || queue.queued.is_empty() {
        return;
    }
    let focus = focus_cell(&cameras);
    let mut candidates: Vec<_> = queue
        .queued
        .iter()
        .copied()
        .filter(|pos| !queue.in_flight.contains_key(pos))
        .map(|pos| distance_key(pos, focus))
        .collect();
    candidates.sort_unstable();
    let pool = AsyncComputeTaskPool::get();
    let mut started = 0;
    for (_, pos) in candidates {
        if started == slots {
            break;
        }
        queue.queued.remove(&pos);
        // Water's color follows its dissolved substances (SAD-P2 §10).
        let tint = eco.as_ref().map_or(&[][..], |e| &e.tint[..]);
        // The color source as `follow_color_source` last saw it.
        let source = queue.color_source;
        let Some(input) = MeshInput::from_world_colored(replica.world(), pos, tint, source) else {
            // No longer resident.
            queue.ready.remove(&pos);
            remove_mesh(&mut commands, queue, pos);
            continue;
        };
        let revision = input.revision;
        let mesher = Arc::clone(&mesher.0);
        let task = pool.spawn(async move {
            let start = Instant::now();
            let data = mesher.mesh(&input);
            let liquid = mesher.mesh_liquid(&input);
            ((data, liquid), start.elapsed())
        });
        queue.in_flight.insert(pos, Job { revision, task });
        queue.jobs_started += 1;
        started += 1;
    }
}

/// Uploads finished meshes nearest the camera first, up to the frame's
/// upload budget, then refreshes [`MeshStats`].
#[allow(clippy::too_many_arguments)]
fn upload_meshes(
    mut commands: Commands,
    replica: Option<Res<Replica>>,
    budget: Res<MeshBudget>,
    cameras: Query<(Entity, &Camera, &GlobalTransform), With<Camera3d>>,
    mut assets: Option<ResMut<Assets<Mesh>>>,
    mut existing: OpaqueMeshes,
    mut liquids: LiquidMeshes,
    mut queue: ResMut<MeshQueue>,
    mut stats: ResMut<MeshStats>,
) {
    let queue = &mut *queue;
    queue.uploads_last_frame = 0;
    if let Some(replica) = replica {
        let limit = if queue.initial_load_done && !queue.recoloring {
            budget.uploads_per_frame
        } else {
            budget.initial_uploads_per_frame
        };
        let focus = focus_cell(&cameras);
        let mut order: Vec<_> = queue
            .ready
            .keys()
            .map(|&pos| distance_key(pos, focus))
            .collect();
        order.sort_unstable();
        for (_, pos) in order {
            if queue.uploads_last_frame == limit {
                break;
            }
            let finished = queue.ready.remove(&pos).expect("ready chunk");
            let current = replica.world().chunk(pos).map(|c| c.revision);
            match current {
                None => {
                    remove_mesh(&mut commands, queue, pos);
                    continue;
                }
                Some(current) if finished.revision < current => {
                    queue.jobs_discarded += 1;
                    queue.queued.insert(pos);
                    continue;
                }
                Some(_) => {}
            }
            upload(
                &mut commands,
                assets.as_deref_mut(),
                &mut existing,
                &mut liquids,
                queue,
                pos,
                finished,
            );
        }
        if !queue.initial_load_done
            && queue.is_idle()
            && replica.dirty().is_empty()
            && !queue.meshed.is_empty()
            && fully_resident(replica.world())
        {
            queue.initial_load_done = true;
            info!(chunks = queue.meshed.len(), "initial meshing done");
        }
        if queue.recoloring && queue.is_idle() && replica.dirty().is_empty() {
            queue.recoloring = false;
            debug!(chunks = queue.meshed.len(), "recolor done");
        }
    }
    *stats = queue.stats();
}

/// Whether every chunk inside the world's bounds is resident. The host sends
/// one snapshot per chunk after `Welcome`, and they can arrive over several
/// frames, so an idle pipeline alone does not mean the replica is fully
/// meshed.
fn fully_resident(world: &rmc_world::world::World) -> bool {
    world.chunk_count() == world.bounds().chunks().count()
}

/// The mesh entities [`upload`] updates in place: opaque, then liquid.
type OpaqueEntity = (&'static mut ChunkMesh, Option<&'static mut Mesh3d>);
type LiquidEntity = (&'static mut LiquidMesh, Option<&'static mut Mesh3d>);
type OpaqueMeshes<'w, 's> = Query<'w, 's, OpaqueEntity, Without<LiquidMesh>>;
type LiquidMeshes<'w, 's> = Query<'w, 's, LiquidEntity, Without<ChunkMesh>>;

/// Replaces `pos`'s opaque and liquid meshes with `finished`.
fn upload(
    commands: &mut Commands,
    mut assets: Option<&mut Assets<Mesh>>,
    existing: &mut OpaqueMeshes,
    liquids: &mut LiquidMeshes,
    queue: &mut MeshQueue,
    pos: ChunkPos,
    finished: Finished,
) {
    let Finished {
        revision,
        data,
        liquid,
        took,
    } = finished;
    let quads = data.quad_count;
    let liquid_quads = liquid.quad_count;
    let bytes = mesh_bytes(&data) + mesh_bytes(&liquid);
    let old = queue.meshed.get(&pos);
    let (old, old_liquid) = (
        old.and_then(|m| m.entity),
        old.and_then(|m| m.liquid_entity),
    );
    let component = ChunkMesh {
        pos,
        revision,
        quads,
    };
    let entity = replace_mesh(
        commands,
        assets.as_deref_mut(),
        existing,
        old,
        pos,
        data,
        component,
    );
    let component = LiquidMesh {
        pos,
        revision,
        quads: liquid_quads,
    };
    let liquid_entity = replace_mesh(
        commands, assets, liquids, old_liquid, pos, liquid, component,
    );
    queue.meshed.insert(
        pos,
        Meshed {
            revision,
            entity,
            quads,
            liquid_entity,
            liquid_quads,
            bytes,
        },
    );
    queue.uploads += 1;
    queue.uploads_last_frame += 1;
    queue.last_mesh_ms = took.as_secs_f32() * 1000.0;
}

/// Puts `data` on the mesh entity `old` with `component`, spawning one at
/// `pos`'s corner if there is none, or despawns `old` when `data` is empty.
/// Returns the entity that holds the mesh.
fn replace_mesh<C, F>(
    commands: &mut Commands,
    assets: Option<&mut Assets<Mesh>>,
    existing: &mut Query<(&mut C, Option<&mut Mesh3d>), F>,
    old: Option<Entity>,
    pos: ChunkPos,
    data: MeshData,
    component: C,
) -> Option<Entity>
where
    C: Component<Mutability = Mutable>,
    F: QueryFilter,
{
    if data.is_empty() {
        if let Some(entity) = old {
            commands.entity(entity).despawn();
        }
        return None;
    }
    let handle = assets.map(|assets| assets.add(mesh_asset(data)));
    match old.and_then(|e| existing.get_mut(e).ok().map(|q| (e, q))) {
        Some((entity, (mut mesh, mesh3d))) => {
            *mesh = component;
            match (handle, mesh3d) {
                (Some(handle), Some(mut mesh3d)) => mesh3d.0 = handle,
                (Some(handle), None) => {
                    commands.entity(entity).insert(Mesh3d(handle));
                }
                (None, _) => {}
            }
            Some(entity)
        }
        None => {
            let origin = pos.origin_cell().min_corner_m();
            let mut e = commands.spawn((component, Transform::from_translation(origin)));
            if let Some(handle) = handle {
                e.insert(Mesh3d(handle));
            }
            Some(e.id())
        }
    }
}

/// Forgets `pos`'s meshes and despawns their entities.
fn remove_mesh(commands: &mut Commands, queue: &mut MeshQueue, pos: ChunkPos) {
    if let Some(meshed) = queue.meshed.remove(&pos) {
        for entity in [meshed.entity, meshed.liquid_entity].into_iter().flatten() {
            commands.entity(entity).despawn();
        }
    }
}

/// Vertex and index bytes of a mesh.
fn mesh_bytes(data: &MeshData) -> u64 {
    let vertex = size_of::<[f32; 3]>() * 2 + size_of::<[f32; 4]>();
    (data.positions.len() * vertex + data.indices.len() * size_of::<u32>()) as u64
}

/// A render-world-only triangle list with positions, normals and colors.
fn mesh_asset(data: MeshData) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, data.positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, data.colors)
    .with_inserted_indices(Indices::U32(data.indices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};

    use rmc_protocol::ClientMsg;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, LocalPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditResult, PlayerId};
    use rmc_world::layers::LayerId;
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::shape::Shape;
    use rmc_world::world::World;

    use crate::net::{ClientMaterials, HostConnection, HostLink, NetPlugin};
    use crate::replica::ReplicaPlugin;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const PLAYER: PlayerId = PlayerId(5);
    /// How long a test waits for jobs on the shared task pool.
    const WAIT: Duration = Duration::from_secs(20);

    #[derive(Clone, Default)]
    struct FakeLink {
        inbound: Arc<Mutex<Vec<ServerMsg>>>,
    }

    impl HostLink for FakeLink {
        fn send(&self, _msg: ClientMsg) -> bool {
            true
        }

        fn drain(&self) -> Vec<ServerMsg> {
            std::mem::take(&mut *self.inbound.lock().unwrap())
        }
    }

    #[derive(Default)]
    struct GateState {
        open: bool,
        started: Vec<(ChunkPos, u64)>,
        running: usize,
        max_running: usize,
    }

    /// A [`GreedyMesher`] whose jobs wait until the test opens the gate.
    /// Jobs on the shared task pool must never block forever, so a closed
    /// gate gives way after [`WAIT`], and dropping the fixture opens it.
    #[derive(Default)]
    struct Gate {
        state: Mutex<GateState>,
        cv: Condvar,
    }

    impl Gate {
        fn set_open(&self, open: bool) {
            self.state.lock().unwrap().open = open;
            self.cv.notify_all();
        }

        fn started(&self) -> Vec<(ChunkPos, u64)> {
            self.state.lock().unwrap().started.clone()
        }

        fn max_running(&self) -> usize {
            self.state.lock().unwrap().max_running
        }
    }

    struct GateMesher(Arc<Gate>);

    impl ChunkMesher for GateMesher {
        fn mesh(&self, input: &MeshInput) -> MeshData {
            let gate = &self.0;
            {
                let mut s = gate.state.lock().unwrap();
                s.started.push((input.pos, input.revision));
                s.running += 1;
                s.max_running = s.max_running.max(s.running);
                let deadline = Instant::now() + WAIT;
                while !s.open {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    s = gate.cv.wait_timeout(s, left).unwrap().0;
                }
            }
            let data = GreedyMesher.mesh(input);
            let mut s = gate.state.lock().unwrap();
            s.running -= 1;
            data
        }
    }

    /// 3×3×3 chunks, cells (0,0,0)..=(95,95,95).
    fn bounds() -> CellAabb {
        CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(95, 95, 95))
    }

    fn c(x: i32, y: i32, z: i32) -> ChunkPos {
        ChunkPos::new(x, y, z)
    }

    /// A host-side world (stone below y = 40, air above), a client app with
    /// NetPlugin + ReplicaPlugin + MeshPipelinePlugin over a fake link, and a
    /// gated mesher.
    struct Fixture {
        app: App,
        link: FakeLink,
        host: World,
        gate: Arc<Gate>,
        seq: u32,
        brass: MaterialId,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.gate.set_open(true);
        }
    }

    impl Fixture {
        /// Builds the app with the gate `open` or closed and `budget`. With
        /// `assets`, the app has `Assets<Mesh>`. The player has not joined
        /// yet.
        fn new(open: bool, budget: MeshBudget, assets: bool) -> Self {
            let materials =
                Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
            let stone = materials.id("stone").unwrap();
            let brass = materials.id("brass_plate").unwrap();
            let mut host = World::new(Arc::clone(&materials), bounds());
            for pos in bounds().chunks() {
                let fill = if pos.0.y == 0 { stone } else { MaterialId::AIR };
                host.insert_chunk(pos, Chunk::uniform(fill)).unwrap();
            }
            let gate = Arc::new(Gate::default());
            gate.set_open(open);
            let link = FakeLink::default();
            let mut app = App::new();
            app.add_plugins(MinimalPlugins);
            if assets {
                // Mesh storage without an asset plugin or asset server.
                app.insert_resource(Assets::<Mesh>::default());
            }
            app.add_plugins((NetPlugin, ReplicaPlugin, MeshPipelinePlugin))
                .insert_resource(ClientMaterials(materials))
                .insert_resource(HostConnection::new(link.clone()))
                .insert_resource(budget)
                .insert_resource(Mesher(Arc::new(GateMesher(Arc::clone(&gate)))));
            app.update();
            let mut f = Self {
                app,
                link,
                host,
                gate,
                seq: 0,
                brass,
            };
            // Stone up to y = 39: chunk row 1 gets a stone floor, one row of
            // chunks per edit (MAX_EDIT_CELLS).
            for z in 0..3 {
                f.seq += 1;
                let r = f.host.apply(&EditCommand {
                    id: CommandId {
                        issuer: PLAYER,
                        seq: f.seq,
                    },
                    issuer: PLAYER,
                    op: EditOp::Set(stone),
                    shape: Shape::Box {
                        min: CellPos::new(0, 32, z * 32),
                        max: CellPos::new(95, 39, z * 32 + 31),
                    },
                });
                assert_eq!(r.deltas.len(), 3, "stone floor in chunk row 1");
            }
            f
        }

        /// Delivers Welcome and every host chunk in one frame.
        fn join(&mut self) {
            let mut msgs = vec![ServerMsg::Welcome {
                player: PLAYER,
                seed: 1,
                bounds: bounds(),
                gen_params_hash: 2,
            }];
            msgs.extend(
                self.host
                    .iter_chunks_sorted()
                    .map(|(pos, chunk)| ServerMsg::chunk_snapshot(pos, chunk)),
            );
            self.deliver(msgs);
        }

        /// Applies a brass `Set` over `shape` to the host world.
        fn edit(&mut self, shape: Shape) -> EditResult {
            self.seq += 1;
            self.host.apply(&EditCommand {
                id: CommandId {
                    issuer: PLAYER,
                    seq: self.seq,
                },
                issuer: PLAYER,
                op: EditOp::Set(self.brass),
                shape,
            })
        }

        /// Queues `msgs` on the link and runs one frame.
        fn deliver(&mut self, msgs: Vec<ServerMsg>) {
            self.link.inbound.lock().unwrap().extend(msgs);
            self.app.update();
        }

        fn step(&mut self) {
            self.app.update();
        }

        fn stats(&self) -> MeshStats {
            self.app.world().resource::<MeshStats>().clone()
        }

        fn queue(&self) -> &MeshQueue {
            self.app.world().resource::<MeshQueue>()
        }

        fn set_budget(&mut self, budget: MeshBudget) {
            self.app.insert_resource(budget);
        }

        /// Steps until the pipeline is idle with the replica fully meshed,
        /// waiting for running jobs between steps. Checks the budgets on
        /// every frame. Returns the frames stepped.
        fn drain(&mut self) -> usize {
            let deadline = Instant::now() + WAIT;
            let mut frames = 0;
            loop {
                let budget = *self.app.world().resource::<MeshBudget>();
                let before = self.stats();
                self.step();
                frames += 1;
                let s = self.stats();
                assert!(s.in_flight <= budget.max_in_flight, "in flight {s:?}");
                let limit = if before.initial_load_done {
                    budget.uploads_per_frame
                } else {
                    budget.initial_uploads_per_frame
                };
                assert!(s.uploads_last_frame <= limit, "uploads {s:?}");
                let dirty = !self.app.world().resource::<Replica>().dirty().is_empty();
                if self.queue().is_idle() && !dirty {
                    return frames;
                }
                assert!(Instant::now() < deadline, "pipeline did not drain: {s:?}");
                self.wait_jobs();
            }
        }

        /// Blocks until every running job's task has finished, so the next
        /// step collects them all.
        fn wait_jobs(&self) {
            let deadline = Instant::now() + WAIT;
            while self.queue().jobs_finished() < self.queue().in_flight().count() {
                assert!(
                    Instant::now() < deadline,
                    "mesh jobs did not finish: {:?}",
                    self.stats()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        fn spawn_camera(&mut self, at: Vec3) -> Entity {
            self.app
                .world_mut()
                .spawn((Camera3d::default(), GlobalTransform::from_translation(at)))
                .id()
        }

        /// The greedy mesh of `pos` from the host world.
        fn expected(&self, pos: ChunkPos) -> MeshData {
            GreedyMesher.mesh(&MeshInput::from_world(&self.host, pos).expect("host chunk"))
        }

        fn chunk_meshes(&mut self) -> BTreeMap<ChunkPos, (Entity, ChunkMesh)> {
            let mut q = self.app.world_mut().query::<(Entity, &ChunkMesh)>();
            q.iter(self.app.world())
                .map(|(e, m)| (m.pos, (e, *m)))
                .collect()
        }
    }

    /// Nearest-first order of `chunks` as seen from `at` (meters).
    fn nearest_order(chunks: impl Iterator<Item = ChunkPos>, at: Vec3) -> Vec<ChunkPos> {
        let focus = CellPos::from_meters(at).0;
        let mut v: Vec<_> = chunks
            .map(|p| {
                let center = p.origin_cell().0 + IVec3::splat(16);
                ((center - focus).as_i64vec3().length_squared(), p)
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn stale_result_discarded_and_requeued() {
        let mut f = Fixture::new(false, MeshBudget::default(), true);
        let p = c(1, 1, 1);
        // Join and mesh everything once, so the initial load is over.
        f.gate.set_open(true);
        f.join();
        f.drain();
        let r0 = f.host.chunk(p).unwrap().revision;
        assert_eq!(f.queue().meshed_revision(p), Some(r0));
        let before = f.stats();
        assert_eq!(
            before.jobs_discarded, 0,
            "no discards while nothing changes"
        );

        // A delta moves P to r1 and starts a job that copies P at r1.
        f.gate.set_open(false);
        let first = f.edit(Shape::Cell(CellPos::new(40, 40, 40)));
        f.deliver(vec![ServerMsg::Applied(first)]);
        let r1 = f.host.chunk(p).unwrap().revision;
        assert_eq!(r1, r0 + 1);
        assert_eq!(f.queue().state(p), Some(ChunkMeshState::InFlight));
        let second = f.edit(Shape::Cell(CellPos::new(41, 40, 40)));
        // While that job runs, a second delta moves P to r2.
        f.deliver(vec![ServerMsg::Applied(second)]);
        let r2 = f.host.chunk(p).unwrap().revision;
        assert_eq!(r2, r1 + 1);
        assert_eq!(
            f.app
                .world()
                .resource::<Replica>()
                .world()
                .chunk(p)
                .unwrap()
                .revision,
            r2
        );
        assert_eq!(f.stats().in_flight, 1, "one job per chunk at a time");
        assert_eq!(
            f.queue().queued().collect::<Vec<_>>(),
            vec![p],
            "P waits for its job"
        );
        for _ in 0..3 {
            f.step();
        }
        assert_eq!(
            f.stats().jobs_started,
            before.jobs_started + 1,
            "no second job"
        );
        assert_eq!(
            f.queue().meshed_revision(p),
            Some(r0),
            "old mesh still shown"
        );

        // The r1 result arrives: discarded, P re-queued and meshed again at r2.
        f.gate.set_open(true);
        f.wait_jobs();
        f.step();
        let s = f.stats();
        assert_eq!(s.jobs_discarded, 1, "the r1 result is discarded");
        assert_eq!(s.uploads, before.uploads, "the r1 result is never uploaded");
        assert_eq!(f.queue().meshed_revision(p), Some(r0));
        assert_eq!(s.jobs_started, before.jobs_started + 2, "P started again");
        assert_eq!(f.queue().state(p), Some(ChunkMeshState::InFlight));
        f.drain();
        let started = f.gate.started();
        let tail: Vec<_> = started[started.len() - 2..].to_vec();
        assert_eq!(tail, vec![(p, r1), (p, r2)], "jobs read r1, then r2");
        let s = f.stats();
        assert_eq!(s.jobs_discarded, 1);
        assert_eq!(s.uploads, before.uploads + 1);
        assert_eq!(f.queue().meshed_revision(p), Some(r2));
        let meshes = f.chunk_meshes();
        let (entity, mesh) = meshes[&p];
        assert_eq!(mesh.revision, r2);
        assert_eq!(
            mesh.quads,
            f.expected(p).quad_count,
            "the mesh shows both edits"
        );
        let handle = f.app.world().get::<Mesh3d>(entity).unwrap().0.clone();
        let assets = f.app.world().resource::<Assets<Mesh>>();
        let asset = assets.get(&handle).expect("mesh asset");
        assert_eq!(asset.count_vertices(), f.expected(p).positions.len());

        // A result that waits for the upload budget and goes stale there is
        // discarded too.
        f.set_budget(MeshBudget {
            max_in_flight: 8,
            uploads_per_frame: 0,
            initial_uploads_per_frame: 0,
        });
        let third = f.edit(Shape::Cell(CellPos::new(42, 40, 40)));
        f.deliver(vec![ServerMsg::Applied(third)]);
        let r3 = r2 + 1;
        f.wait_jobs();
        f.step();
        assert_eq!(f.queue().state(p), Some(ChunkMeshState::AwaitingUpload));
        f.gate.set_open(false);
        let fourth = f.edit(Shape::Cell(CellPos::new(43, 40, 40)));
        f.deliver(vec![ServerMsg::Applied(fourth)]);
        f.set_budget(MeshBudget::default());
        f.step();
        let s = f.stats();
        assert_eq!(s.jobs_discarded, 2, "the r3 result is discarded at upload");
        assert_eq!(f.queue().meshed_revision(p), Some(r2));
        f.gate.set_open(true);
        f.drain();
        assert_eq!(f.queue().meshed_revision(p), Some(r3 + 1));
        assert_eq!(f.chunk_meshes()[&p].1.quads, f.expected(p).quad_count);
    }

    #[test]
    fn in_flight_budget_respected() {
        // Closed gate: exactly 8 of 27 chunks start, and stay 8.
        let mut f = Fixture::new(false, MeshBudget::default(), true);
        f.join();
        let s = f.stats();
        assert_eq!((s.in_flight, s.queued), (8, 19), "{s:?}");
        assert_eq!(s.jobs_started, 8);
        for _ in 0..5 {
            f.step();
            let s = f.stats();
            assert_eq!((s.in_flight, s.queued, s.uploads), (8, 19, 0), "{s:?}");
        }
        f.gate.set_open(true);
        f.drain();
        let s = f.stats();
        assert_eq!(s.meshed_chunks, 27);
        assert_eq!(s.jobs_started, 27);
        assert!(
            f.gate.max_running() <= 8,
            "{} jobs ran at once",
            f.gate.max_running()
        );
        assert!(s.initial_load_done);
        drop(f);

        // The in-flight budget is configurable.
        let mut f = Fixture::new(
            false,
            MeshBudget {
                max_in_flight: 3,
                ..MeshBudget::default()
            },
            true,
        );
        f.join();
        assert_eq!((f.stats().in_flight, f.stats().queued), (3, 24));
        f.gate.set_open(true);
        f.drain();
        assert!(f.gate.max_running() <= 3);
        drop(f);

        // Initial load: all 27 results ready at once upload in one frame
        // (budget 32); afterwards 4 per frame.
        let mut f = Fixture::new(
            true,
            MeshBudget {
                max_in_flight: 64,
                ..MeshBudget::default()
            },
            true,
        );
        f.join();
        assert_eq!(f.stats().in_flight, 27);
        f.wait_jobs();
        f.step();
        let s = f.stats();
        assert_eq!((s.uploads_last_frame, s.meshed_chunks), (27, 27), "{s:?}");
        assert!(s.initial_load_done);

        // Ten interior edits in ten chunks: 4, 4, 2 uploads.
        let mut results = Vec::new();
        let edited: Vec<ChunkPos> = bounds().chunks().take(10).collect();
        for &pos in &edited {
            let cell = CellPos(pos.origin_cell().0 + IVec3::splat(10));
            results.push(ServerMsg::Applied(f.edit(Shape::Cell(cell))));
        }
        f.deliver(results);
        assert_eq!(f.stats().in_flight, 10);
        f.wait_jobs();
        let mut per_frame = Vec::new();
        for _ in 0..4 {
            f.step();
            per_frame.push(f.stats().uploads_last_frame);
        }
        assert_eq!(per_frame, vec![4, 4, 2, 0]);
        for &pos in &edited {
            let rev = f.host.chunk(pos).unwrap().revision;
            assert_eq!(f.queue().meshed_revision(pos), Some(rev), "{pos:?}");
        }

        // The upload budget is configurable.
        f.set_budget(MeshBudget {
            max_in_flight: 64,
            uploads_per_frame: 7,
            initial_uploads_per_frame: 32,
        });
        let mut results = Vec::new();
        for &pos in &edited {
            let cell = CellPos(pos.origin_cell().0 + IVec3::splat(11));
            results.push(ServerMsg::Applied(f.edit(Shape::Cell(cell))));
        }
        f.deliver(results);
        f.wait_jobs();
        f.step();
        assert_eq!(f.stats().uploads_last_frame, 7);
        f.step();
        assert_eq!(f.stats().uploads_last_frame, 3);
    }

    #[test]
    fn initial_load_uses_burst_budget() {
        let budget = MeshBudget {
            max_in_flight: 64,
            uploads_per_frame: 4,
            initial_uploads_per_frame: 10,
        };
        let mut f = Fixture::new(true, budget, true);
        let snapshots: Vec<ServerMsg> = f
            .host
            .iter_chunks_sorted()
            .map(|(pos, chunk)| ServerMsg::chunk_snapshot(pos, chunk))
            .collect();
        assert_eq!(snapshots.len(), 27);
        let welcome = ServerMsg::Welcome {
            player: PLAYER,
            seed: 1,
            bounds: bounds(),
            gen_params_hash: 2,
        };

        // The snapshots arrive over two frames, as they can from a host on
        // another thread. Meshing the first part empties the pipeline, but
        // the replica is not fully meshed yet: the initial load goes on.
        let mut first = vec![welcome.clone()];
        first.extend(snapshots[..13].iter().cloned());
        f.deliver(first);
        assert_eq!(f.stats().in_flight, 13);
        f.wait_jobs();
        let mut per_frame = Vec::new();
        for _ in 0..3 {
            f.step();
            per_frame.push(f.stats().uploads_last_frame);
        }
        assert_eq!(per_frame, vec![10, 3, 0], "burst budget for the first part");
        let s = f.stats();
        assert!(f.queue().is_idle(), "{s:?}");
        assert_eq!(s.meshed_chunks, 13);
        assert!(!s.initial_load_done, "13 of 27 chunks meshed: {s:?}");

        // The rest arrives: still the burst budget, then the load is done.
        // The new snapshots also re-dirty their meshed neighbours.
        f.deliver(snapshots[13..].to_vec());
        let jobs = f.stats().in_flight;
        assert!(jobs > 20, "14 new chunks and their neighbours: {jobs}");
        f.wait_jobs();
        let mut per_frame = Vec::new();
        for _ in 0..4 {
            f.step();
            per_frame.push(f.stats().uploads_last_frame);
        }
        assert_eq!(
            per_frame,
            vec![10, 10, jobs - 20, 0],
            "burst budget for the rest"
        );
        let s = f.stats();
        assert_eq!(s.meshed_chunks, 27);
        assert!(s.initial_load_done, "{s:?}");

        // Once loaded, edits upload at the steady budget.
        let mut results = Vec::new();
        let edited: Vec<ChunkPos> = bounds().chunks().take(10).collect();
        for &pos in &edited {
            let cell = CellPos(pos.origin_cell().0 + IVec3::splat(10));
            results.push(ServerMsg::Applied(f.edit(Shape::Cell(cell))));
        }
        f.deliver(results);
        f.wait_jobs();
        let mut per_frame = Vec::new();
        for _ in 0..4 {
            f.step();
            per_frame.push(f.stats().uploads_last_frame);
        }
        assert_eq!(per_frame, vec![4, 4, 2, 0], "steady budget after the load");

        // A new Welcome starts a new replica and a new initial load.
        let mut rejoin = vec![welcome];
        rejoin.extend(
            f.host
                .iter_chunks_sorted()
                .map(|(pos, chunk)| ServerMsg::chunk_snapshot(pos, chunk)),
        );
        f.deliver(rejoin);
        assert!(!f.stats().initial_load_done);
        f.wait_jobs();
        let mut per_frame = Vec::new();
        for _ in 0..4 {
            f.step();
            per_frame.push(f.stats().uploads_last_frame);
        }
        assert_eq!(per_frame, vec![10, 10, 7, 0], "burst budget after a rejoin");
        assert!(f.stats().initial_load_done);
    }

    #[test]
    fn nearest_chunk_first() {
        // One job at a time, so jobs start in exactly the queue order.
        let budget = MeshBudget {
            max_in_flight: 1,
            ..MeshBudget::default()
        };
        let mut f = Fixture::new(true, budget, true);
        let at = Vec3::new(3.2, 20.7, 41.0);
        let camera = f.spawn_camera(at);
        // A second, inactive camera and a later-ordered one are ignored.
        f.app.world_mut().spawn((
            Camera3d::default(),
            Camera {
                is_active: false,
                ..default()
            },
            GlobalTransform::from_translation(Vec3::new(47.0, 47.0, 47.0)),
        ));
        f.app.world_mut().spawn((
            Camera3d::default(),
            Camera {
                order: 1,
                ..default()
            },
            GlobalTransform::from_translation(Vec3::new(47.0, 0.0, 0.0)),
        ));
        f.join();
        f.drain();
        let order: Vec<ChunkPos> = f.gate.started().iter().map(|&(p, _)| p).collect();
        let expected = nearest_order(bounds().chunks(), at);
        assert_eq!(order, expected);
        assert_eq!(order[0], c(0, 1, 2), "the camera's own chunk first");
        assert_eq!(order[26], c(2, 2, 0), "the far corner last");

        // Move the camera: the next dirty chunks start from the new spot.
        let at = Vec3::new(47.0, 1.0, 2.0);
        *f.app
            .world_mut()
            .get_mut::<GlobalTransform>(camera)
            .unwrap() = GlobalTransform::from_translation(at);
        let r = f.edit(Shape::Box {
            min: CellPos::new(0, 1, 0),
            max: CellPos::new(95, 1, 95),
        });
        let changed: Vec<ChunkPos> = r.deltas.iter().map(|d| d.pos()).collect();
        assert_eq!(changed.len(), 9);
        f.deliver(vec![ServerMsg::Applied(r)]);
        f.drain();
        let order: Vec<ChunkPos> = f.gate.started()[27..].iter().map(|&(p, _)| p).collect();
        assert_eq!(order, nearest_order(changed.into_iter(), at));
        assert_eq!(order[0], c(2, 0, 0));
        drop(f);

        // Uploads go nearest first too: 27 results ready at once, 4 per
        // frame after the initial load.
        let mut f = Fixture::new(
            true,
            MeshBudget {
                max_in_flight: 64,
                ..MeshBudget::default()
            },
            true,
        );
        f.join();
        f.drain();
        let at = Vec3::new(30.0, 30.0, 30.0);
        f.spawn_camera(at);
        let mut results = Vec::new();
        for pos in bounds().chunks() {
            let cell = CellPos(pos.origin_cell().0 + IVec3::splat(10));
            results.push(ServerMsg::Applied(f.edit(Shape::Cell(cell))));
        }
        f.deliver(results);
        f.wait_jobs();
        let mut uploaded = Vec::new();
        for _ in 0..7 {
            let before: BTreeMap<_, _> = f
                .chunk_meshes()
                .into_iter()
                .map(|(p, (_, m))| (p, m.revision))
                .collect();
            f.step();
            let mut frame: Vec<ChunkPos> = f
                .chunk_meshes()
                .into_iter()
                .filter(|(p, (_, m))| before.get(p) != Some(&m.revision))
                .map(|(p, _)| p)
                .collect();
            // Within a frame the set matters, not the spawn order.
            frame.sort();
            uploaded.push(frame);
        }
        let expected = nearest_order(bounds().chunks(), at);
        // Empty chunks have no entity, so compare against the chunks with
        // geometry only.
        let with_geometry: Vec<ChunkPos> = expected
            .iter()
            .copied()
            .filter(|&p| !f.expected(p).is_empty())
            .collect();
        let mut got: Vec<ChunkPos> = Vec::new();
        let mut want: Vec<ChunkPos> = Vec::new();
        for (i, frame) in uploaded.iter().enumerate() {
            got.extend(frame);
            let mut w: Vec<ChunkPos> = expected[(i * 4).min(27)..((i + 1) * 4).min(27)]
                .iter()
                .copied()
                .filter(|p| with_geometry.contains(p))
                .collect();
            w.sort();
            want.extend(w);
        }
        assert_eq!(got, want, "uploads in nearest-first groups of 4");
    }

    #[test]
    fn mesh_stats_count_meshed_chunks() {
        let mut f = Fixture::new(true, MeshBudget::default(), true);
        assert_eq!(f.stats(), MeshStats::default(), "nothing before Welcome");
        f.join();
        f.drain();
        let s = f.stats();
        let all: Vec<ChunkPos> = bounds().chunks().collect();
        let expected: BTreeMap<ChunkPos, MeshData> =
            all.iter().map(|&p| (p, f.expected(p))).collect();
        let with_geometry = expected.values().filter(|m| !m.is_empty()).count();
        assert!(
            with_geometry > 0 && with_geometry < 27,
            "the fixture has empty and non-empty chunks"
        );
        assert_eq!(s.meshed_chunks, 27);
        assert_eq!(s.chunks_with_geometry, with_geometry);
        let quads: u64 = expected.values().map(|m| u64::from(m.quad_count)).sum();
        assert_eq!(s.quads, quads);
        let bytes: u64 = expected
            .values()
            .map(|m| (m.positions.len() * 40 + m.indices.len() * 4) as u64)
            .sum();
        assert_eq!(s.mesh_bytes, bytes);
        assert_eq!((s.queued, s.in_flight, s.awaiting_upload), (0, 0, 0));
        assert_eq!((s.jobs_started, s.uploads, s.jobs_discarded), (27, 27, 0));
        assert!(s.initial_load_done);
        assert!(s.last_mesh_ms >= 0.0);

        // One entity per chunk with geometry, at the chunk's corner, with a
        // render-world mesh equal to the greedy mesh.
        let meshes = f.chunk_meshes();
        assert_eq!(meshes.len(), with_geometry);
        for (pos, (entity, mesh)) in &meshes {
            let want = &expected[pos];
            assert_eq!(mesh.quads, want.quad_count);
            assert_eq!(mesh.revision, f.host.chunk(*pos).unwrap().revision);
            let world = f.app.world();
            let t = world.get::<Transform>(*entity).unwrap();
            assert_eq!(t.translation, pos.origin_cell().min_corner_m());
            let handle = &world.get::<Mesh3d>(*entity).unwrap().0;
            let asset = world.resource::<Assets<Mesh>>().get(handle).unwrap();
            assert_eq!(asset.asset_usage, RenderAssetUsages::RENDER_WORLD);
            assert_eq!(asset.primitive_topology(), PrimitiveTopology::TriangleList);
            assert_eq!(asset.count_vertices(), want.positions.len());
            assert_eq!(asset.indices().unwrap().len(), want.indices.len());
            assert_eq!(f.queue().state(*pos), Some(ChunkMeshState::Meshed));
        }

        // An edit that fills an empty chunk's cell: one more chunk with
        // geometry, still 27 meshed, one more upload.
        let empty = c(1, 2, 1);
        assert!(expected[&empty].is_empty());
        let r = f.edit(Shape::Cell(CellPos::new(48, 80, 48)));
        f.deliver(vec![ServerMsg::Applied(r)]);
        f.drain();
        let s = f.stats();
        assert_eq!(s.meshed_chunks, 27);
        assert_eq!(s.chunks_with_geometry, with_geometry + 1);
        assert_eq!(s.quads, quads + 6, "a floating cell adds 6 quads");
        assert_eq!(s.uploads, 28);
        assert_eq!(f.chunk_meshes().len(), with_geometry + 1);
        // Removing it again empties the chunk: its entity goes away.
        f.seq += 1;
        let r = f.host.apply(&EditCommand {
            id: CommandId {
                issuer: PLAYER,
                seq: f.seq,
            },
            issuer: PLAYER,
            op: EditOp::Set(MaterialId::AIR),
            shape: Shape::Cell(CellPos::new(48, 80, 48)),
        });
        f.deliver(vec![ServerMsg::Applied(r)]);
        f.drain();
        let s = f.stats();
        assert_eq!(
            (s.meshed_chunks, s.chunks_with_geometry),
            (27, with_geometry)
        );
        assert_eq!(s.quads, quads);
        assert!(!f.chunk_meshes().contains_key(&empty));

        // A new Welcome drops every mesh and counter, then meshes again.
        f.gate.set_open(false);
        f.join();
        let s = f.stats();
        assert_eq!(
            (s.meshed_chunks, s.uploads, s.jobs_discarded),
            (0, 0, 0),
            "{s:?}"
        );
        assert_eq!(s.jobs_started, 8);
        assert!(!s.initial_load_done);
        f.step();
        assert!(f.chunk_meshes().is_empty(), "old meshes despawned");
        f.gate.set_open(true);
        f.drain();
        let s = f.stats();
        assert_eq!(
            (s.meshed_chunks, s.chunks_with_geometry),
            (27, with_geometry)
        );
        assert_eq!(s.quads, quads);
        assert_eq!(f.chunk_meshes().len(), with_geometry);
        drop(f);

        // Without Assets<Mesh> (a logic-only app) the counts are the same and
        // the entities have no mesh asset.
        let mut f = Fixture::new(true, MeshBudget::default(), false);
        f.join();
        f.drain();
        let s = f.stats();
        assert_eq!(
            (s.meshed_chunks, s.chunks_with_geometry),
            (27, with_geometry)
        );
        assert_eq!(s.quads, quads);
        let meshes = f.chunk_meshes();
        assert_eq!(meshes.len(), with_geometry);
        for (_, (entity, _)) in meshes {
            assert!(f.app.world().get::<Mesh3d>(entity).is_none());
        }
    }

    /// The render-world positions of the mesh on `entity`.
    fn asset_positions(app: &App, entity: Entity) -> Vec<[f32; 3]> {
        let world = app.world();
        let handle = &world.get::<Mesh3d>(entity).expect("a mesh").0;
        let asset = world.resource::<Assets<Mesh>>().get(handle).unwrap();
        match asset.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(v)) => v.clone(),
            other => panic!("positions {other:?}"),
        }
    }

    fn asset_colors(app: &App, entity: Entity) -> Vec<[f32; 4]> {
        let world = app.world();
        let handle = &world.get::<Mesh3d>(entity).expect("a mesh").0;
        let asset = world.resource::<Assets<Mesh>>().get(handle).unwrap();
        match asset.attribute(Mesh::ATTRIBUTE_COLOR) {
            Some(bevy::mesh::VertexAttributeValues::Float32x4(v)) => v.clone(),
            other => panic!("colors {other:?}"),
        }
    }

    #[test]
    fn liquid_mesh_follows_dissolved_tint() {
        use rmc_world::layers::SubstanceLayers;
        use rmc_world::query::SubstanceTint;

        let fe_tint = SubstanceTint {
            layer: LayerId::Substance(1),
            color: [184, 98, 36],
            full: 32,
            strength: 192,
        };
        let mut f = Fixture::new(true, MeshBudget::default(), true);
        f.app.insert_resource(Mesher(Arc::new(GreedyMesher)));
        f.app.insert_resource(ClientEco {
            substances: SubstanceLayers::new(["cu", "fe"]).unwrap(),
            gases: Vec::new(),
            ph: Default::default(),
            player_tox: Default::default(),
            plant_tox: Default::default(),
            tint: vec![fe_tint],
            gas_tint: Vec::new(),
            flammable: Vec::new(),
        });
        f.join();
        f.drain();

        // A clean pool meshes in water's own color.
        let pos = c(1, 1, 1);
        let water = f.host.materials().id("water").unwrap();
        let pool: Vec<LocalPos> = (8..11)
            .flat_map(|x| (8..11).map(move |z| LocalPos::new(x, 8, z)))
            .collect();
        let mut chunk = f.host.chunk(pos).unwrap().clone();
        for &local in &pool {
            chunk.storage.set(local, water);
            chunk.layers.set_u8(LayerId::Moisture, local, 200);
        }
        chunk.revision += 1;
        f.host.insert_chunk(pos, chunk.clone()).unwrap();
        f.deliver(vec![ServerMsg::chunk_snapshot(pos, &chunk)]);
        f.drain();
        let entity = f.queue().liquid_entity(pos).expect("the pool meshes");
        let clean = asset_colors(&f.app, entity);
        let untinted = GreedyMesher.mesh_liquid(&MeshInput::from_world(&f.host, pos).unwrap());
        assert_eq!(clean, untinted.colors, "clean water is untinted");

        // A layer snapshot that dissolves iron in it: remeshed in the tint.
        let mut layers = chunk.layers.clone();
        for &local in &pool {
            layers.set_u8(fe_tint.layer, local, 24);
        }
        let eco_revision = chunk.eco_revision + 1;
        f.host
            .apply_eco_snapshot(pos, eco_revision, layers.clone(), Default::default())
            .unwrap();
        f.deliver(vec![ServerMsg::LayerSnapshot {
            pos,
            eco_revision,
            layers,
            plants: Default::default(),
        }]);
        f.drain();
        let input = MeshInput::from_world_tinted(&f.host, pos, &[fe_tint]).unwrap();
        let want = GreedyMesher.mesh_liquid(&input);
        assert_eq!(f.queue().liquid_entity(pos), Some(entity), "same entity");
        let rusty = asset_colors(&f.app, entity);
        assert_eq!(rusty, want.colors, "the pipeline passes the tint rows");
        assert_ne!(rusty, clean, "iron tints the pool");
        assert_eq!(asset_positions(&f.app, entity), untinted.positions);
    }

    #[test]
    fn liquid_mesh_uploads_on_its_own_entity() {
        let mut f = Fixture::new(true, MeshBudget::default(), true);
        f.app.insert_resource(Mesher(Arc::new(GreedyMesher)));
        f.join();
        f.drain();
        let s = f.stats();
        assert_eq!(
            (s.chunks_with_liquid, s.liquid_quads),
            (0, 0),
            "no water yet"
        );
        let liquids = |f: &mut Fixture| {
            let world = f.app.world_mut();
            let mut q = world.query::<(Entity, &LiquidMesh, &Transform)>();
            q.iter(world)
                .map(|(e, m, t)| (e, *m, t.translation))
                .collect::<Vec<_>>()
        };
        assert!(liquids(&mut f).is_empty());
        let (quads, bytes) = (s.quads, s.mesh_bytes);

        // A shallow pool on the stone floor of chunk (1,1,1), by snapshot.
        let pos = c(1, 1, 1);
        let water = f.host.materials().id("water").unwrap();
        let mut chunk = f.host.chunk(pos).unwrap().clone();
        for x in 8..11 {
            for z in 8..11 {
                let local = LocalPos::new(x, 8, z);
                chunk.storage.set(local, water);
                chunk.layers.set_u8(LayerId::Moisture, local, 128);
            }
        }
        chunk.revision += 1;
        f.host.insert_chunk(pos, chunk.clone()).unwrap();
        f.deliver(vec![ServerMsg::chunk_snapshot(pos, &chunk)]);
        f.drain();
        let input = MeshInput::from_world(&f.host, pos).unwrap();
        let want = GreedyMesher.mesh_liquid(&input);
        assert_eq!(
            want.quad_count, 5,
            "a 3×3 pool on the floor: the top and 4 sides"
        );
        let s = f.stats();
        assert_eq!(s.chunks_with_liquid, 1);
        assert_eq!(s.liquid_quads, u64::from(want.quad_count));
        assert_eq!(
            s.quads, quads,
            "the floor's top stays: water hides no stone face"
        );
        assert_eq!(
            s.mesh_bytes,
            bytes + (want.positions.len() * 40 + want.indices.len() * 4) as u64,
            "both passes count"
        );
        let found = liquids(&mut f);
        assert_eq!(found.len(), 1, "one liquid entity");
        let (entity, mesh, at) = found[0];
        assert_eq!(f.queue().liquid_entity(pos), Some(entity));
        assert_ne!(
            f.queue().entity(pos),
            Some(entity),
            "apart from the opaque one"
        );
        assert_eq!(
            mesh,
            LiquidMesh {
                pos,
                revision: chunk.revision,
                quads: want.quad_count
            }
        );
        assert_eq!(at, pos.origin_cell().min_corner_m());
        assert!(f.app.world().get::<ChunkMesh>(entity).is_none());
        assert_eq!(asset_positions(&f.app, entity), want.positions);

        // A layer snapshot that fills the pool: remeshed at the new height,
        // on the same entity.
        let mut layers = chunk.layers.clone();
        for x in 8..11 {
            for z in 8..11 {
                layers.set_u8(LayerId::Moisture, LocalPos::new(x, 8, z), 255);
            }
        }
        let eco_revision = chunk.eco_revision + 1;
        f.host
            .apply_eco_snapshot(pos, eco_revision, layers.clone(), Default::default())
            .unwrap();
        f.deliver(vec![ServerMsg::LayerSnapshot {
            pos,
            eco_revision,
            layers,
            plants: Default::default(),
        }]);
        f.drain();
        let want_full = GreedyMesher.mesh_liquid(&MeshInput::from_world(&f.host, pos).unwrap());
        assert_ne!(want_full.positions, want.positions, "the surface moved up");
        assert_eq!(f.queue().liquid_entity(pos), Some(entity), "same entity");
        assert_eq!(asset_positions(&f.app, entity), want_full.positions);

        // A new Welcome despawns liquid meshes with the rest.
        f.join();
        f.step();
        assert!(liquids(&mut f).is_empty(), "old liquid meshes despawned");
        f.drain();
        assert_eq!(liquids(&mut f).len(), 1, "meshed again");

        // Draining the pool removes the liquid entity; the chunk's opaque
        // mesh stays.
        let mut dry = f.host.chunk(pos).unwrap().clone();
        for x in 8..11 {
            for z in 8..11 {
                dry.storage.set(LocalPos::new(x, 8, z), MaterialId::AIR);
            }
        }
        dry.revision += 1;
        f.host.insert_chunk(pos, dry.clone()).unwrap();
        f.deliver(vec![ServerMsg::chunk_snapshot(pos, &dry)]);
        f.drain();
        let s = f.stats();
        assert_eq!((s.chunks_with_liquid, s.liquid_quads), (0, 0));
        assert_eq!(f.queue().liquid_entity(pos), None);
        assert!(liquids(&mut f).is_empty(), "liquid entity despawned");
        assert!(f.queue().entity(pos).is_some(), "the opaque mesh stays");
    }
}
