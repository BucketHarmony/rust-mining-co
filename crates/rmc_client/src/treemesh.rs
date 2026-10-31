//! Graph tree meshes (docs/TREES.md §4.4).
//!
//! A tree grown as a metamer graph ([`TreeGraph`], in `Chunk::trees`) is
//! not part of its chunk's mesh: it gets one mesh entity of its own,
//! [`TreeMesh`], at its root's top-face center, rebuilt when its graph's
//! hash or the season changes ([`sync_tree_meshes`]).
//!
//! **Wood.** Every metamer is a generalized cylinder between two
//! [`Ring`]s. Its end ring has its pipe radius ([`TreeGraph::radii`]); its
//! start ring is its parent's end ring when it continues the parent's
//! axis, and its own radius at the parent's end when it starts a lateral.
//! A ring faces the average of the directions meeting there, so an axis
//! bends smoothly through its nodes. A ring has [`sides`] sides, from
//! [`TREE_SIDES_MIN`] for a twig to [`TREE_SIDES_MAX`] for a trunk. The
//! bark is the wood material's color, and its tile through the chunk
//! shader, which looks tiles up by the material id in `uv.x`.
//!
//! **Leaves.** A leafy metamer carries `leaf` leaf cards, lens-shaped
//! quads drawn from both sides, spaced along the internode and turned
//! about it by the species' divergence (phyllotaxis), each leaning out and
//! up. They take the leaf material's color moved toward the season color,
//! and none are drawn in a bare season.
//!
//! **Sway.** `uv.y` is the wind amplitude in meters that the chunk shader
//! turns into sway: the species' `sway` scaled by the node's distance from
//! the root (up to [`TREE_SWAY_REACH_M`]) and by how thin it is (the twig
//! radius over its own), so trunks stand still and twigs move. Both rings
//! at a node and the leaves on it share the node's amplitude.

use std::collections::BTreeMap;

use bevy::asset::RenderAssetUsages;
use bevy::color::Color;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rmc_world::coords::{CELL_SIZE_M, CellPos, LocalPos};
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::query::{GraphLook, PlantLook};
use rmc_world::tree::{NO_PARENT, Q14, TURN, TreeGraph, perpendicular, rotate};

use crate::eco::ClientEco;
use crate::material::EmissiveChunkMaterial;
use crate::mesher::{MeshData, PLANT_SEASON_MIX};
use crate::render::ChunkMaterial;
use crate::replica::Replica;
use crate::weather::Weather;

/// Sides of a twig's cylinder.
pub const TREE_SIDES_MIN: u32 = 4;
/// Sides of a trunk's cylinder.
pub const TREE_SIDES_MAX: u32 = 10;
/// Radius, in micrometers, each side past [`TREE_SIDES_MIN`] takes.
pub const TREE_SIDE_UM: u32 = 6_000;
/// A leaf card's length, in meters.
pub const LEAF_CARD_M: f32 = 0.16;
/// A leaf card's width, as a share of its length.
pub const LEAF_CARD_WIDTH: f32 = 0.5;
/// How far a leaf card leans along its twig, and up, for each unit it
/// points out from the twig.
pub const LEAF_CARD_LEAN: f32 = 0.5;
pub const LEAF_CARD_LIFT: f32 = 0.3;
/// Sway of a twig at [`TREE_SWAY_REACH_M`] or more from the root, for a
/// species of sway 255, in meters.
pub const TREE_SWAY_M: f32 = 0.6 * CELL_SIZE_M;
/// From the root out to this many meters, sway grows with the distance.
pub const TREE_SWAY_REACH_M: f32 = 3.0;

/// Meters per millimeter, and per micrometer.
const M_PER_MM: f32 = 1.0e-3;
const M_PER_UM: f32 = 1.0e-6;

/// The sides of a cylinder of radius `radius_um`.
pub fn sides(radius_um: u32) -> u32 {
    (TREE_SIDES_MIN + radius_um / TREE_SIDE_UM).min(TREE_SIDES_MAX)
}

/// One ring of a cylinder, in meters from the root's top-face center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub center: Vec3,
    /// The axis through the ring, unit length.
    pub normal: Vec3,
    pub radius_m: f32,
    pub sides: u32,
    /// Wind amplitude, meters.
    pub sway: f32,
}

/// A Q14 direction as a unit vector.
fn unit(d: [i16; 3]) -> Vec3 {
    Vec3::new(f32::from(d[0]), f32::from(d[1]), f32::from(d[2])) / Q14 as f32
}

/// The start and end ring of every metamer of `graph`, in its order, for a
/// species of twig radius `tip_um` and sway `sway`.
pub fn rings(graph: &TreeGraph, tip_um: u32, sway: u8) -> Vec<[Ring; 2]> {
    let ms = graph.metamers();
    let ends = graph.ends();
    let radii = graph.radii(tip_um);
    // The child continuing each metamer's axis, if any.
    let mut next = vec![None; ms.len()];
    for (i, m) in ms.iter().enumerate() {
        if m.parent != NO_PARENT && !m.lateral {
            next[usize::from(m.parent)].get_or_insert(i);
        }
    }
    let amplitude = f32::from(sway) / f32::from(u8::MAX) * TREE_SWAY_M;
    let mut path = vec![0.0f32; ms.len()];
    let mut node_sway = vec![0.0f32; ms.len()];
    let mut end_normal = vec![Vec3::Y; ms.len()];
    let mut out = Vec::with_capacity(ms.len());
    for (i, m) in ms.iter().enumerate() {
        let parent = (m.parent != NO_PARENT).then(|| usize::from(m.parent));
        let dir = unit(m.dir);
        path[i] = parent.map_or(0.0, |p| path[p]) + f32::from(m.length_mm) * M_PER_MM;
        let thin = tip_um as f32 / radii[i].max(1) as f32;
        node_sway[i] = amplitude * (path[i] / TREE_SWAY_REACH_M).min(1.0) * thin.min(1.0);
        end_normal[i] = next[i]
            .map(|c| (dir + unit(ms[c].dir)).normalize_or(dir))
            .unwrap_or(dir);
        let at = |e: [i32; 3]| Vec3::new(e[0] as f32, e[1] as f32, e[2] as f32) * M_PER_MM;
        let (start, start_r, start_n) = match parent {
            Some(p) if !m.lateral => (at(ends[p]), radii[p], end_normal[p]),
            Some(p) => (at(ends[p]), radii[i], dir),
            None => (Vec3::ZERO, radii[i], dir),
        };
        let n = sides(start_r);
        let start_sway = parent.map_or(0.0, |p| node_sway[p]);
        out.push([
            Ring {
                center: start,
                normal: start_n,
                radius_m: start_r as f32 * M_PER_UM,
                sides: n,
                sway: start_sway,
            },
            Ring {
                center: at(ends[i]),
                normal: end_normal[i],
                radius_m: radii[i] as f32 * M_PER_UM,
                sides: n,
                sway: node_sway[i],
            },
        ]);
    }
    out
}

/// Two unit vectors square to `n` and to each other.
fn frame(n: Vec3) -> (Vec3, Vec3) {
    let reference = if n.x.abs() < 0.9 { Vec3::X } else { Vec3::Z };
    let u = n.cross(reference).normalize();
    (u, n.cross(u))
}

/// The leaf color of a species in `season`, or `None` when it stands bare.
pub fn leaf_color(look: &PlantLook, materials: &MaterialTable, season: u32) -> Option<[u8; 3]> {
    let row = |n: usize| usize::try_from(season).unwrap_or(0) % n.max(1);
    if look
        .bare
        .get(row(look.bare.len()))
        .copied()
        .unwrap_or(false)
    {
        return None;
    }
    let def = materials.get(look.leaf?)?;
    let mut color = [def.color[0], def.color[1], def.color[2]];
    if let Some(Some(c)) = look.season_colors.get(row(look.season_colors.len())) {
        let t = PLANT_SEASON_MIX;
        color = std::array::from_fn(|k| {
            let (x, y) = (f32::from(color[k]), f32::from(c[k]));
            (x + (y - x) * t).round() as u8
        });
    }
    Some(color)
}

/// The mesh of `graph`, a tree of `look` (with its `graph` look `g`) in
/// `season`, in meters from the root's top-face center.
pub fn tree_mesh(
    graph: &TreeGraph,
    look: &PlantLook,
    g: &GraphLook,
    materials: &MaterialTable,
    season: u32,
) -> MeshData {
    let mut out = MeshData::default();
    let linear = |c: [u8; 3]| {
        let c = Color::srgb_u8(c[0], c[1], c[2]).to_linear();
        [c.red, c.green, c.blue, 0.0]
    };
    let bark = materials
        .get(g.wood)
        .map_or([0; 3], |d| [d.color[0], d.color[1], d.color[2]]);
    let bark = linear(bark);
    let wood_id = f32::from(g.wood.0);
    let all = rings(graph, g.tip_um, look.sway);
    for [a, b] in &all {
        push_cylinder(&mut out, a, b, bark, wood_id);
    }
    let Some(leaf) = leaf_color(look, materials, season) else {
        return out;
    };
    let leaf = linear(leaf);
    let leaf_id = f32::from(look.leaf.unwrap_or(MaterialId::AIR).0);
    for (i, (m, [a, b])) in graph.metamers().iter().zip(&all).enumerate() {
        let cards = u32::from(m.leaf);
        for k in 0..cards {
            let turn = (i as u32 * u32::from(g.divergence) + k * TURN / cards) % TURN;
            let radial = unit(rotate(perpendicular(m.dir), m.dir, turn));
            let along = (b.center - a.center).normalize_or(Vec3::Y);
            let base = a.center.lerp(b.center, (k + 1) as f32 / (cards + 1) as f32);
            let point =
                (radial + along * LEAF_CARD_LEAN + Vec3::Y * LEAF_CARD_LIFT).normalize_or(radial);
            push_card(&mut out, base, point, along, leaf, leaf_id, b.sway);
        }
    }
    out
}

/// Appends the side of the cylinder from ring `a` to ring `b`.
fn push_cylinder(out: &mut MeshData, a: &Ring, b: &Ring, color: [f32; 4], id: f32) {
    let first = out.positions.len() as u32;
    let n = a.sides;
    for ring in [a, b] {
        let (u, v) = frame(ring.normal);
        for k in 0..n {
            let angle = k as f32 / n as f32 * std::f32::consts::TAU;
            let radial = u * angle.cos() + v * angle.sin();
            out.positions
                .push((ring.center + radial * ring.radius_m).to_array());
            out.normals.push(radial.to_array());
            out.colors.push(color);
            out.uvs.push([id, ring.sway]);
        }
    }
    for k in 0..n {
        let k1 = (k + 1) % n;
        let (a0, a1, b0, b1) = (first + k, first + k1, first + n + k, first + n + k1);
        out.indices.extend([a0, a1, b1, a0, b1, b0]);
        out.quad_count += 1;
    }
}

/// Appends a lens-shaped leaf card from `base` along `point`, its width
/// square to `point` and `along`, drawn from both sides.
fn push_card(
    out: &mut MeshData,
    base: Vec3,
    point: Vec3,
    along: Vec3,
    color: [f32; 4],
    id: f32,
    sway: f32,
) {
    let side = point
        .cross(along)
        .normalize_or(point.any_orthonormal_vector());
    let normal = side.cross(point).normalize_or(Vec3::Y);
    let tip = base + point * LEAF_CARD_M;
    let middle = base + point * (LEAF_CARD_M / 2.0);
    let half = side * (LEAF_CARD_M * LEAF_CARD_WIDTH / 2.0);
    let corners = [base, middle + half, tip, middle - half];
    for facing in [normal, -normal] {
        let first = out.positions.len() as u32;
        for c in corners {
            out.positions.push(c.to_array());
            out.normals.push(facing.to_array());
            out.colors.push(color);
            out.uvs.push([id, sway]);
        }
        if facing == normal {
            out.indices
                .extend([first, first + 1, first + 2, first, first + 2, first + 3]);
        } else {
            out.indices
                .extend([first, first + 2, first + 1, first, first + 3, first + 2]);
        }
        out.quad_count += 1;
    }
}

/// The mesh entity of the graph tree rooted at `root`.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct TreeMesh {
    pub root: CellPos,
    /// [`TreeGraph::hash`] of the graph drawn.
    pub hash: u64,
    /// The season it was drawn for.
    pub season: u32,
    pub vertices: usize,
}

/// Where a tree's mesh sits: its root cell's top-face center, in meters.
pub fn tree_origin(root: CellPos) -> Vec3 {
    (root.0.as_vec3() + Vec3::new(0.5, 1.0, 0.5)) * CELL_SIZE_M
}

/// Keeps one [`TreeMesh`] per graph tree of the replica whose species has
/// a graph look: spawns new ones, rebuilds those whose graph or season
/// changed, and despawns those whose tree is gone.
#[allow(clippy::too_many_arguments)]
pub fn sync_tree_meshes(
    mut commands: Commands,
    replica: Option<Res<Replica>>,
    eco: Option<Res<ClientEco>>,
    weather: Option<Res<Weather>>,
    mut assets: Option<ResMut<Assets<Mesh>>>,
    plain: Option<Res<ChunkMaterial>>,
    emissive: Option<Res<EmissiveChunkMaterial>>,
    mut existing: Query<(Entity, &mut TreeMesh, Option<&mut Mesh3d>)>,
) {
    let (Some(replica), Some(eco)) = (replica, eco) else {
        return;
    };
    let season = weather.as_ref().map_or(0, |w| w.season);
    let season_changed = existing.iter().any(|(_, t, _)| t.season != season);
    if !replica.is_changed() && !eco.is_changed() && !season_changed {
        return;
    }
    let world = replica.world();
    let materials = world.materials();
    let mut wanted: BTreeMap<CellPos, (&TreeGraph, &PlantLook, GraphLook)> = BTreeMap::new();
    for (pos, chunk) in world.iter_chunks_sorted() {
        for (&index, graph) in &chunk.trees {
            let Some(plant) = chunk.plants.get(&index) else {
                continue;
            };
            let Some(look) = eco.plant_looks.get(usize::from(plant.species)) else {
                continue;
            };
            if let Some(g) = look.graph {
                let root = CellPos::from_chunk_local(pos, LocalPos::from_index(index));
                wanted.insert(root, (graph, look, g));
            }
        }
    }
    let mut seen = BTreeMap::new();
    for (entity, mut tree, mesh3d) in &mut existing {
        let Some(&(graph, look, g)) = wanted.get(&tree.root) else {
            commands.entity(entity).despawn();
            continue;
        };
        seen.insert(tree.root, ());
        let hash = graph.hash();
        if tree.hash == hash && tree.season == season {
            continue;
        }
        let data = tree_mesh(graph, look, &g, materials, season);
        *tree = TreeMesh {
            root: tree.root,
            hash,
            season,
            vertices: data.positions.len(),
        };
        if let (Some(assets), Some(mut mesh3d)) = (assets.as_deref_mut(), mesh3d) {
            mesh3d.0 = assets.add(tree_mesh_asset(data));
        }
    }
    for (root, (graph, look, g)) in wanted {
        if seen.contains_key(&root) {
            continue;
        }
        let data = tree_mesh(graph, look, &g, materials, season);
        let tree = TreeMesh {
            root,
            hash: graph.hash(),
            season,
            vertices: data.positions.len(),
        };
        let mut entity = commands.spawn((tree, Transform::from_translation(tree_origin(root))));
        if let Some(assets) = assets.as_deref_mut() {
            entity.insert(Mesh3d(assets.add(tree_mesh_asset(data))));
        }
        if let Some(m) = &emissive {
            entity.insert(MeshMaterial3d(m.0.clone()));
        } else if let Some(m) = &plain {
            entity.insert(MeshMaterial3d(m.0.clone()));
        }
    }
}

fn tree_mesh_asset(data: MeshData) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, data.positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, data.normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, data.colors)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, data.uvs)
    .with_inserted_indices(Indices::U32(data.indices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmc_world::tree::{BudState, Dir, Metamer};

    fn metamer(parent: u16, dir: Dir, lateral: bool, leaf: u8) -> Metamer {
        Metamer {
            parent,
            dir,
            length_mm: 250,
            born: 0,
            bud: BudState::Dormant,
            leaf,
            shade: 0,
            lateral,
        }
    }

    /// A trunk of five with a lateral of three from the second metamer
    /// and one of two from the fourth, every metamer with `leaf` leaves.
    fn tree(leaf: u8) -> TreeGraph {
        let up: Dir = [0, Q14 as i16, 0];
        let east: Dir = [Q14 as i16, 0, 0];
        let west: Dir = [-(Q14 as i16), 0, 0];
        let mut g = TreeGraph::default();
        g.push(metamer(NO_PARENT, up, false, leaf)).unwrap();
        for p in 0..4 {
            g.push(metamer(p, up, false, leaf)).unwrap();
        }
        let a = g.push(metamer(1, east, true, leaf)).unwrap();
        g.push(metamer(a, east, false, leaf)).unwrap();
        g.push(metamer(a + 1, east, false, leaf)).unwrap();
        let b = g.push(metamer(3, west, true, leaf)).unwrap();
        g.push(metamer(b, west, false, leaf)).unwrap();
        g
    }

    fn materials() -> MaterialTable {
        let assets = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
        MaterialTable::load(assets).unwrap()
    }

    fn look(t: &MaterialTable, bare: bool) -> (PlantLook, GraphLook) {
        let wood = t.id("oak_wood").unwrap();
        let look = PlantLook {
            material: t.id("acorn").unwrap(),
            needs: Vec::new(),
            sway: 128,
            season_colors: vec![None, None, Some([176, 96, 40]), None],
            leaf: t.id("oak_leaves"),
            tuft: None,
            bare: vec![false, false, false, bare],
            graph: Some(GraphLook {
                wood,
                tip_um: 2000,
                divergence: 1564,
            }),
        };
        let g = look.graph.unwrap();
        (look, g)
    }

    #[test]
    fn the_mesh_has_a_cylinder_per_metamer_and_cards_per_leaf() {
        let t = materials();
        let (look, g) = look(&t, true);
        let graph = tree(3);
        let rings = rings(&graph, g.tip_um, look.sway);
        assert_eq!(rings.len(), graph.len());
        let wood: usize = rings.iter().map(|[a, _]| 2 * a.sides as usize).sum();
        // Two sides of four corners per card.
        let leaves: usize = graph
            .metamers()
            .iter()
            .map(|m| 8 * usize::from(m.leaf))
            .sum();
        let summer = tree_mesh(&graph, &look, &g, &t, 1);
        assert_eq!(summer.positions.len(), wood + leaves);
        assert_eq!(summer.indices.len(), 6 * summer.quad_count as usize);
        assert!(
            summer.positions.len() == summer.normals.len()
                && summer.normals.len() == summer.colors.len()
                && summer.colors.len() == summer.uvs.len()
        );
        // Bare in winter: the wood only.
        let winter = tree_mesh(&graph, &look, &g, &t, 3);
        assert_eq!(winter.positions.len(), wood);
        // Autumn leaves turn toward the season color; the wood does not.
        let autumn = tree_mesh(&graph, &look, &g, &t, 2);
        assert_eq!(autumn.positions.len(), summer.positions.len());
        assert_ne!(autumn.colors[wood], summer.colors[wood]);
        assert_eq!(autumn.colors[0], summer.colors[0]);
        // Every vertex carries the material whose tile it takes.
        assert_eq!(summer.uvs[0][0], f32::from(g.wood.0));
        assert_eq!(summer.uvs[wood][0], f32::from(look.leaf.unwrap().0));
        // Another tree, another vertex count.
        let mut more = tree(3);
        more.push(metamer(4, [0, Q14 as i16, 0], false, 3)).unwrap();
        assert!(tree_mesh(&more, &look, &g, &t, 1).positions.len() > summer.positions.len());
    }

    #[test]
    fn radii_taper_along_every_axis_and_sides_follow_them() {
        let t = materials();
        let (look, g) = look(&t, false);
        let graph = tree(1);
        let rings = rings(&graph, g.tip_um, look.sway);
        for (i, (m, [a, b])) in graph.metamers().iter().zip(&rings).enumerate() {
            assert!(a.radius_m >= b.radius_m, "metamer {i} widens");
            if m.parent != NO_PARENT && !m.lateral {
                // An axis continues from its parent's end ring.
                let [_, pb] = rings[usize::from(m.parent)];
                assert_eq!(
                    (a.center, a.radius_m, a.normal),
                    (pb.center, pb.radius_m, pb.normal)
                );
            }
            if m.parent != NO_PARENT && m.lateral {
                assert!(a.radius_m <= rings[usize::from(m.parent)][1].radius_m);
            }
        }
        // The trunk base is the widest, with the most sides; twigs are thin.
        let r0 = rings[0][0];
        assert!(
            rings
                .iter()
                .all(|[a, _]| a.radius_m <= r0.radius_m && a.sides <= r0.sides)
        );
        assert_eq!(sides(g.tip_um), TREE_SIDES_MIN);
        assert_eq!(sides(u32::MAX), TREE_SIDES_MAX);
        // The trunk base stands still; the tips sway most.
        assert_eq!(r0.sway, 0.0);
        let tip = rings.iter().map(|[_, b]| b.sway).fold(0.0, f32::max);
        assert!(tip > 0.0 && rings.iter().all(|[a, b]| a.sway <= tip && b.sway <= tip));
    }
}
