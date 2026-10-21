//! Chunk meshing (SAD §11.2).
//!
//! A [`MeshInput`] holds one chunk's 32³ cells plus a one-cell border taken
//! from its neighbours (34³ in all), so faces and ambient occlusion on the
//! chunk's border read the right cells. Cells outside the world bounds and
//! cells of chunks that are not resident read as air, and so does the
//! bounds shell above the surface (F095; see [`MeshInput::from_world`]).
//!
//! [`GreedyMesher`] emits a face wherever an opaque cell of the chunk
//! ([`is_opaque`]: `Solid`, or an opaque liquid such as magma) meets a cell
//! that is not opaque, and merges equal faces in each slice into
//! rectangles. Faces merge only when their material, normal and all four
//! corner AO values match (the 0fps scheme), so a merged quad never smears AO
//! across cells that should be shaded differently. AO is 3-sample voxel AO
//! (two sides and the corner in the layer in front of the face), four levels.
//!
//! Positions are chunk-local meters: the chunk's minimum corner is the origin,
//! so the mesh entity sits at the chunk's origin cell. Vertex colors are the
//! material color in linear RGB times the face's shade for its normal
//! ([`face_shade`]) times the vertex's AO factor; alpha holds the material's
//! emissive strength.
//!
//! The liquid pass ([`ChunkMesher::mesh_liquid`], SAD-P2 §10) is a second
//! [`MeshData`] with the faces of translucent liquid cells
//! ([`is_liquid_pass`]: water): the top at the cell's fill height, the sides
//! clipped to the fill, no AO. Its vertex alpha is the liquid's opacity, and
//! the renderer draws it alpha-blended after the opaque pass. The fill is
//! the cell's `moisture` layer, which [`MeshInput`] carries for liquid cells.

use std::sync::Arc;

use bevy::color::Color;
use bevy::prelude::{IVec3, Vec3};
use rmc_world::coords::{CELL_SIZE_M, CHUNK_SIZE, CellAabb, CellPos, ChunkPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::{MaterialDef, MaterialId, MaterialTable, Phase};
use rmc_world::world::World;

/// Edge length of a [`MeshInput`] in cells: the chunk plus one border cell on
/// each side.
pub const MESH_INPUT_EDGE: usize = CHUNK_SIZE as usize + 2;
/// Cells in a [`MeshInput`].
pub const MESH_INPUT_VOLUME: usize = MESH_INPUT_EDGE * MESH_INPUT_EDGE * MESH_INPUT_EDGE;

/// Color factor per AO level. Level 3 is unoccluded, level 0 is a vertex
/// with both side cells solid.
pub const AO_FACTORS: [f32; 4] = [0.4, 0.6, 0.8, 1.0];

/// Color factor per face direction, so that faces at an angle to each other
/// read apart even under flat light: top, ±X sides, ±Z sides, bottom.
pub const FACE_SHADES: [f32; 4] = [1.0, 0.8, 0.7, 0.55];

/// Color factor for a face with outward normal `normal` (an axis unit
/// vector). It multiplies the vertex color on top of AO. It depends only on
/// the normal, which is fixed per meshing pass, so it never splits a merge.
pub fn face_shade(normal: IVec3) -> f32 {
    let [top, x_side, z_side, bottom] = FACE_SHADES;
    if normal.y > 0 {
        top
    } else if normal.y < 0 {
        bottom
    } else if normal.x != 0 {
        x_side
    } else {
        z_side
    }
}

/// Builds a mesh from one chunk's cells.
pub trait ChunkMesher: Send + Sync {
    /// The opaque pass.
    fn mesh(&self, input: &MeshInput) -> MeshData;

    /// The liquid pass (SAD-P2 §10): the faces of [`is_liquid_pass`] cells,
    /// drawn alpha-blended after the opaque pass. Empty by default.
    fn mesh_liquid(&self, _input: &MeshInput) -> MeshData {
        MeshData::default()
    }
}

/// Whether `def` is meshed in the liquid pass (SAD-P2 §10): a `Liquid`
/// whose color is translucent (alpha below 255), such as water. An opaque
/// liquid (magma) is a full cell in the opaque pass.
pub fn is_liquid_pass(def: &MaterialDef) -> bool {
    def.phase == Phase::Liquid && def.color[3] < u8::MAX
}

/// Whether `def` is meshed in the opaque pass: `Solid`, or a `Liquid` that
/// is not [`is_liquid_pass`]. Opaque cells hide the faces behind them in
/// both passes.
pub fn is_opaque(def: &MaterialDef) -> bool {
    def.is_solid_or_liquid() && !is_liquid_pass(def)
}

/// One chunk's cells with a one-cell border, indexed by chunk-local
/// coordinates in `-1..=32` per axis (`x + z*34 + y*34²` after shifting by 1).
#[derive(Clone, Debug)]
pub struct MeshInput {
    pub pos: ChunkPos,
    pub revision: u64,
    pub cells: Box<[MaterialId; MESH_INPUT_VOLUME]>,
    /// The fill (`moisture` layer) of every [`is_liquid_pass`] cell; 0 for
    /// every other cell.
    pub fills: Box<[u8; MESH_INPUT_VOLUME]>,
    pub materials: Arc<MaterialTable>,
}

impl MeshInput {
    /// An input whose cells are all air.
    pub fn empty(pos: ChunkPos, revision: u64, materials: Arc<MaterialTable>) -> Self {
        let cells: Box<[MaterialId]> = vec![MaterialId::AIR; MESH_INPUT_VOLUME].into_boxed_slice();
        let cells = cells
            .try_into()
            .unwrap_or_else(|_| unreachable!("the vector has MESH_INPUT_VOLUME cells"));
        let fills: Box<[u8]> = vec![0; MESH_INPUT_VOLUME].into_boxed_slice();
        let fills = fills
            .try_into()
            .unwrap_or_else(|_| unreachable!("the vector has MESH_INPUT_VOLUME cells"));
        Self {
            pos,
            revision,
            cells,
            fills,
            materials,
        }
    }

    /// Copies the chunk at `pos` and its border from `world`, or `None` if
    /// that chunk is not resident. The revision is the chunk's. Border cells in
    /// chunks that are not resident, or outside the bounds, are air (the same
    /// as [`World::get_cell`] returning `None`). The bounds shell above the
    /// surface is air too (see `open_shell_above_surface`). Liquid-pass cells
    /// bring their fill.
    pub fn from_world(world: &World, pos: ChunkPos) -> Option<Self> {
        let revision = world.chunk(pos)?.revision;
        let mut input = Self::empty(pos, revision, world.materials().clone());
        let liquid: Vec<bool> = input.materials.iter().map(is_liquid_pass).collect();
        // The 27 chunks the 34³ box touches, indexed by offset + 1.
        let mut near = [None; 27];
        for (i, slot) in near.iter_mut().enumerate() {
            let offset = IVec3::new(i as i32 % 3, i as i32 / 9, (i as i32 / 3) % 3) - IVec3::ONE;
            *slot = world.chunk(ChunkPos(pos.0 + offset));
        }
        let bounds = world.bounds();
        let origin = pos.origin_cell().0;
        for y in -1..=CHUNK_SIZE {
            for z in -1..=CHUNK_SIZE {
                for x in -1..=CHUNK_SIZE {
                    let cell = CellPos(origin + IVec3::new(x, y, z));
                    if !bounds.contains(cell) {
                        continue;
                    }
                    let (cp, local) = cell.split();
                    let o = cp.0 - pos.0 + IVec3::ONE;
                    if let Some(chunk) = near[(o.x + o.z * 3 + o.y * 9) as usize] {
                        let id = chunk.storage.get(local);
                        let i = Self::index(x, y, z);
                        input.cells[i] = id;
                        if liquid.get(usize::from(id.0)).copied().unwrap_or(false) {
                            input.fills[i] = chunk.layers.get_u8(LayerId::Moisture, local);
                        }
                    }
                }
            }
        }
        input.open_shell_above_surface(world);
        Some(input)
    }

    /// Turns the bounds shell above the surface into air (F095), so the
    /// world's edge is the terrain's edge against the sky rather than a
    /// wall standing over it. A cell is that shell when it lies in the
    /// outermost x or z column ring of the bounds, is `Solid` and not
    /// diggable, and its interior column (the nearest cell one step in from
    /// the ring, at the same height) is not `Solid` from that height to the
    /// top of the bounds: the sky is open above it.
    ///
    /// Below the surface (beside the rock, a cave or a tunnel, or under an
    /// overhang) the shell stays, and the floor is never touched. Only the
    /// mesh input changes: the replica keeps the cells, and the player still
    /// stops at them.
    fn open_shell_above_surface(&mut self, world: &World) {
        let bounds = world.bounds();
        let (lo, hi) = (bounds.min.0, bounds.max.0);
        let origin = self.pos.origin_cell().0;
        let ys: Vec<i32> = (-1..=CHUNK_SIZE)
            .filter(|&y| (lo.y..=hi.y).contains(&(origin.y + y)))
            .collect();
        for z in -1..=CHUNK_SIZE {
            for x in -1..=CHUNK_SIZE {
                let (wx, wz) = (origin.x + x, origin.z + z);
                let Some((ix, iz)) = shell_interior(bounds, wx, wz) else {
                    continue;
                };
                // The lowest height from which the interior column is open
                // to the top of the bounds.
                let mut open_from = hi.y + 1;
                while open_from > lo.y && !world.is_solid(CellPos::new(ix, open_from - 1, iz)) {
                    open_from -= 1;
                }
                for &y in &ys {
                    if origin.y + y >= open_from && is_shell(&self.materials, self.get(x, y, z)) {
                        self.set(x, y, z, MaterialId::AIR);
                    }
                }
            }
        }
    }

    /// Index of the chunk-local cell `(x, y, z)`, each in `-1..=32`.
    pub fn index(x: i32, y: i32, z: i32) -> usize {
        let e = MESH_INPUT_EDGE as i32;
        debug_assert!(
            (-1..e - 1).contains(&x) && (-1..e - 1).contains(&y) && (-1..e - 1).contains(&z),
            "mesh input cell ({x}, {y}, {z}) out of range"
        );
        ((x + 1) + (z + 1) * e + (y + 1) * e * e) as usize
    }

    /// Material at the chunk-local cell `(x, y, z)`, each in `-1..=32`.
    pub fn get(&self, x: i32, y: i32, z: i32) -> MaterialId {
        self.cells[Self::index(x, y, z)]
    }

    /// Writes the chunk-local cell `(x, y, z)`, each in `-1..=32`. This only
    /// edits the input; the world is unchanged.
    pub fn set(&mut self, x: i32, y: i32, z: i32, id: MaterialId) {
        self.cells[Self::index(x, y, z)] = id;
    }

    /// Fill at the chunk-local cell `(x, y, z)`, each in `-1..=32`.
    pub fn fill(&self, x: i32, y: i32, z: i32) -> u8 {
        self.fills[Self::index(x, y, z)]
    }

    /// Writes the cell `(x, y, z)` as `id` with fill `fill`. This only edits
    /// the input; the world is unchanged.
    pub fn set_liquid(&mut self, x: i32, y: i32, z: i32, id: MaterialId, fill: u8) {
        let i = Self::index(x, y, z);
        self.cells[i] = id;
        self.fills[i] = fill;
    }
}

/// Whether `id` is a shell material: `Solid` and not diggable (F095).
pub(crate) fn is_shell(materials: &MaterialTable, id: MaterialId) -> bool {
    materials
        .get(id)
        .is_some_and(|d| d.is_solid() && !d.diggable)
}

/// For a column `(x, z)` in the outermost x or z ring of `bounds`, the
/// interior column one step in from it (for a corner, the diagonal one),
/// whose sky decides whether the ring's shell is drawn (F095). `None` for
/// any other column, and when the bounds have no interior.
pub(crate) fn shell_interior(bounds: CellAabb, x: i32, z: i32) -> Option<(i32, i32)> {
    let (lo, hi) = (bounds.min.0, bounds.max.0);
    let inside = (lo.x..=hi.x).contains(&x) && (lo.z..=hi.z).contains(&z);
    let ring = x == lo.x || x == hi.x || z == lo.z || z == hi.z;
    if !inside || !ring || hi.x - lo.x < 2 || hi.z - lo.z < 2 {
        return None;
    }
    Some((x.clamp(lo.x + 1, hi.x - 1), z.clamp(lo.z + 1, hi.z - 1)))
}

/// Vertex data for one chunk. Every quad is 4 consecutive vertices and
/// 6 consecutive indices (two triangles, counter-clockwise seen from outside).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub quad_count: u32,
}

impl MeshData {
    pub fn is_empty(&self) -> bool {
        self.quad_count == 0
    }
}

/// Greedy mesher with AO in the merge key.
#[derive(Clone, Copy, Debug, Default)]
pub struct GreedyMesher;

/// What a face must share with its neighbour in the slice to merge. The
/// normal is fixed per pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FaceKey {
    material: MaterialId,
    /// AO level per corner, in the order (-u,-v), (+u,-v), (+u,+v), (-u,+v).
    ao: [u8; 4],
}

/// Unit vector along axis `a` (0 = x, 1 = y, 2 = z).
fn unit(a: usize) -> IVec3 {
    let mut v = IVec3::ZERO;
    v[a] = 1;
    v
}

/// AO level of one vertex from its two side samples and its corner sample.
fn ao_level(side1: bool, side2: bool, corner: bool) -> u8 {
    if side1 && side2 {
        0
    } else {
        3 - u8::from(side1) - u8::from(side2) - u8::from(corner)
    }
}

impl ChunkMesher for GreedyMesher {
    fn mesh(&self, input: &MeshInput) -> MeshData {
        const N: i32 = CHUNK_SIZE;
        let table = &input.materials;
        let solid: Vec<bool> = input
            .cells
            .iter()
            .map(|&id| table.get(id).is_some_and(is_opaque))
            .collect();
        let is_solid = |p: IVec3| solid[MeshInput::index(p.x, p.y, p.z)];
        let base_colors: Vec<[f32; 4]> = table
            .iter()
            .map(|def| vertex_base_color(table, def.id))
            .collect();

        let mut out = MeshData::default();
        let mut mask: Vec<Option<FaceKey>> = vec![None; (N * N) as usize];
        // Fixed pass order: +X, -X, +Y, -Y, +Z, -Z.
        for d in 0..3 {
            // u × v = +d, so (u, v) corners in order wind counter-clockwise
            // seen from +d.
            let (u, v) = ((d + 1) % 3, (d + 2) % 3);
            let (eu, ev) = (unit(u), unit(v));
            for positive in [true, false] {
                let n = if positive { unit(d) } else { -unit(d) };
                for s in 0..N {
                    // Faces of slice `s`, indexed `i + j*N` for (u, v) = (i, j).
                    for j in 0..N {
                        for i in 0..N {
                            let mut p = IVec3::ZERO;
                            p[d] = s;
                            p[u] = i;
                            p[v] = j;
                            let q = p + n;
                            mask[(i + j * N) as usize] = if is_solid(p) && !is_solid(q) {
                                let ao = [(-1, -1), (1, -1), (1, 1), (-1, 1)].map(|(su, sv)| {
                                    let a = is_solid(q + eu * su);
                                    let b = is_solid(q + ev * sv);
                                    let c = is_solid(q + eu * su + ev * sv);
                                    ao_level(a, b, c)
                                });
                                Some(FaceKey {
                                    material: input.get(p.x, p.y, p.z),
                                    ao,
                                })
                            } else {
                                None
                            };
                        }
                    }
                    merge_rects(&mut mask, |i, j, w, h, key| {
                        let mut origin = IVec3::ZERO;
                        origin[d] = if positive { s + 1 } else { s };
                        origin[u] = i;
                        origin[v] = j;
                        let corners = [
                            origin,
                            origin + eu * w,
                            origin + eu * w + ev * h,
                            origin + ev * h,
                        ];
                        let base = base_colors
                            .get(usize::from(key.material.0))
                            .copied()
                            .unwrap_or_default();
                        push_quad(&mut out, corners, n, base, key.ao, positive);
                    });
                }
            }
        }
        out
    }

    /// Emits a face of a liquid-pass cell with fill above 0 wherever it does
    /// not meet an opaque cell or the same liquid (SAD-P2 §10):
    ///
    /// - the top at `y + fill/255`, unless the cell above is the same liquid,
    ///   or is opaque and the cell is full;
    /// - the bottom on the cell's floor;
    /// - each side from the floor to the fill, or, toward the same liquid at
    ///   a lower fill, from that fill up; none toward an equal or higher one.
    ///
    /// Faces merge as in the opaque pass when their material, color and
    /// extent match, without AO. A side that is not the full cell height
    /// merges only along its row. The color is the material's in linear
    /// RGB times [`face_shade`], through [`liquid_tint`], with the
    /// material's alpha as opacity.
    fn mesh_liquid(&self, input: &MeshInput) -> MeshData {
        const N: i32 = CHUNK_SIZE;
        let table = &input.materials;
        let flags = |f: fn(&MaterialDef) -> bool| -> Vec<bool> { table.iter().map(f).collect() };
        let (liquid, opaque) = (flags(is_liquid_pass), flags(is_opaque));
        let has = |v: &[bool], id: MaterialId| v.get(usize::from(id.0)).copied().unwrap_or(false);
        let base_colors: Vec<[f32; 4]> = table
            .iter()
            .map(|def| liquid_base_color(table, def.id))
            .collect();
        let cell = |p: IVec3| (input.get(p.x, p.y, p.z), input.fill(p.x, p.y, p.z));

        let mut out = MeshData::default();
        let mut mask: Vec<Option<LiquidKey>> = vec![None; (N * N) as usize];
        for d in 0..3 {
            let (u, v) = ((d + 1) % 3, (d + 2) % 3);
            for positive in [true, false] {
                let n = if positive { unit(d) } else { -unit(d) };
                for s in 0..N {
                    for j in 0..N {
                        for i in 0..N {
                            let mut p = IVec3::ZERO;
                            p[d] = s;
                            p[u] = i;
                            p[v] = j;
                            let (m, fill) = cell(p);
                            mask[(i + j * N) as usize] = if has(&liquid, m) && fill > 0 {
                                let (qm, q_fill) = cell(p + n);
                                let same = qm == m && q_fill > 0;
                                let q_opaque = has(&opaque, qm);
                                liquid_extent(d, positive, fill, same, q_fill, q_opaque).map(
                                    |(lo, hi)| {
                                        let base = base_colors
                                            .get(usize::from(m.0))
                                            .copied()
                                            .unwrap_or_default();
                                        let color = liquid_tint(input, p, base);
                                        let partial = d != 1 && (lo, hi) != (0, u8::MAX);
                                        LiquidKey {
                                            material: m,
                                            color: color.map(f32::to_bits),
                                            lo,
                                            hi,
                                            row: if partial { Some(p.y) } else { None },
                                        }
                                    },
                                )
                            } else {
                                None
                            };
                        }
                    }
                    merge_rects(&mut mask, |i, j, w, h, key| {
                        let height = |f: u8| f32::from(f) / f32::from(u8::MAX);
                        // Per axis, the quad's extent in cells.
                        let mut lo = Vec3::ZERO;
                        let mut hi = Vec3::ZERO;
                        let plane = if d == 1 && positive {
                            s as f32 + height(key.hi)
                        } else if positive {
                            (s + 1) as f32
                        } else {
                            s as f32
                        };
                        lo[d] = plane;
                        hi[d] = plane;
                        (lo[u], hi[u]) = (i as f32, (i + w) as f32);
                        (lo[v], hi[v]) = (j as f32, (j + h) as f32);
                        if d != 1 {
                            // A side: from `lo` in its bottom row to `hi` in
                            // its top row.
                            let (y0, y1) = (lo.y, hi.y);
                            lo.y = y0 + height(key.lo);
                            hi.y = y1 - 1.0 + height(key.hi);
                        }
                        let corners = [lo, with_axis(lo, u, hi[u]), hi, with_axis(lo, v, hi[v])];
                        let color = key.color.map(f32::from_bits);
                        push_liquid_quad(&mut out, corners, n, color, positive);
                    });
                }
            }
        }
        out
    }
}

/// What a liquid face must share with its neighbour in the slice to merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiquidKey {
    material: MaterialId,
    /// The face color's bits, before the face shade.
    color: [u32; 4],
    /// For a side, the fill it starts from and the fill it ends at, in its
    /// own cell; for a top, the fill (`hi`); 0 for a bottom.
    lo: u8,
    hi: u8,
    /// A side that is not the full cell height keeps to its row `y`, so
    /// stacked partial sides never merge into one tall quad.
    row: Option<i32>,
}

/// The extent of a liquid cell's face toward a neighbour along axis `d`
/// (`positive` toward +d), as `(lo, hi)` fills (see [`LiquidKey`]), or
/// `None` when no face is drawn there. `fill` is above 0; `same` says the
/// neighbour is the same liquid with fill `q_fill` above 0.
fn liquid_extent(
    d: usize,
    positive: bool,
    fill: u8,
    same: bool,
    q_fill: u8,
    q_opaque: bool,
) -> Option<(u8, u8)> {
    if d == 1 {
        let hidden = if positive {
            same || (q_opaque && fill == u8::MAX)
        } else {
            same || q_opaque
        };
        let hi = if positive { fill } else { 0 };
        (!hidden).then_some((0, hi))
    } else {
        let lo = if same { q_fill } else { 0 };
        (!q_opaque && fill > lo).then_some((lo, fill))
    }
}

/// `p` with its component on axis `a` replaced by `value`.
fn with_axis(mut p: Vec3, a: usize, value: f32) -> Vec3 {
    p[a] = value;
    p
}

/// Greedy merge of one slice's face mask (`N`×`N`, indexed `i + j*N`):
/// grows each face along u, then along v while the whole row matches, and
/// calls `emit(i, j, w, h, key)` per rectangle, clearing the mask.
fn merge_rects<K: Copy + PartialEq>(
    mask: &mut [Option<K>],
    mut emit: impl FnMut(i32, i32, i32, i32, K),
) {
    const N: i32 = CHUNK_SIZE;
    for j in 0..N {
        let mut i = 0;
        while i < N {
            let Some(key) = mask[(i + j * N) as usize] else {
                i += 1;
                continue;
            };
            let mut w = 1;
            while i + w < N && mask[(i + w + j * N) as usize] == Some(key) {
                w += 1;
            }
            let mut h = 1;
            while j + h < N && (i..i + w).all(|k| mask[(k + (j + h) * N) as usize] == Some(key)) {
                h += 1;
            }
            for jj in j..j + h {
                for k in i..i + w {
                    mask[(k + jj * N) as usize] = None;
                }
            }
            emit(i, j, w, h, key);
            i += w;
        }
    }
}

/// Appends one liquid quad. `corners` are in cells, in (u, v) order. The
/// color is shaded for the normal; its alpha (opacity) is kept.
fn push_liquid_quad(
    out: &mut MeshData,
    corners: [Vec3; 4],
    normal: IVec3,
    color: [f32; 4],
    positive: bool,
) {
    let first = out.positions.len() as u32;
    let shade = face_shade(normal);
    for c in corners {
        out.positions.push((c * CELL_SIZE_M).to_array());
        out.normals.push(normal.as_vec3().to_array());
        out.colors.push([
            color[0] * shade,
            color[1] * shade,
            color[2] * shade,
            color[3],
        ]);
    }
    for [a, b, c] in [[0, 1, 2], [0, 2, 3]] {
        let tri = if positive { [a, b, c] } else { [a, c, b] };
        out.indices.extend(tri.map(|k| first + k));
    }
    out.quad_count += 1;
}

/// Linear RGB of a liquid's sRGB color, with its alpha as opacity `0..=1`.
fn liquid_base_color(materials: &MaterialTable, id: MaterialId) -> [f32; 4] {
    materials.get(id).map_or([0.0; 4], |def| {
        let [r, g, b, a] = def.color;
        let c = Color::srgba_u8(r, g, b, a).to_linear();
        [c.red, c.green, c.blue, f32::from(a) / f32::from(u8::MAX)]
    })
}

/// The color of the liquid cell `p` (chunk-local): its material's color,
/// unchanged. This is the hook for the substance tint (SAD-P2 §10,
/// `eco.tint`): water's color will shift here by the cell's dissolved
/// substances. The color is part of [`LiquidKey`], so cells merge only with
/// equal tints.
fn liquid_tint(_input: &MeshInput, _p: IVec3, base: [f32; 4]) -> [f32; 4] {
    base
}

/// Appends one quad. `corners` are in cells, in (u, v) order; `positive`
/// says whether the normal points along +d, which fixes the winding.
fn push_quad(
    out: &mut MeshData,
    corners: [IVec3; 4],
    normal: IVec3,
    base: [f32; 4],
    ao: [u8; 4],
    positive: bool,
) {
    let first = out.positions.len() as u32;
    let shade = face_shade(normal);
    for (c, level) in corners.iter().zip(ao) {
        out.positions.push((c.as_vec3() * CELL_SIZE_M).to_array());
        out.normals.push(normal.as_vec3().to_array());
        let f = AO_FACTORS[usize::from(level)];
        out.colors.push([
            base[0] * shade * f,
            base[1] * shade * f,
            base[2] * shade * f,
            base[3],
        ]);
    }
    // Split along the diagonal whose ends are brighter in sum, so a single
    // dark corner stays in one triangle (0fps anisotropy fix).
    let tris: [[u32; 3]; 2] = if ao[0] + ao[2] >= ao[1] + ao[3] {
        [[0, 1, 2], [0, 2, 3]]
    } else {
        [[0, 1, 3], [1, 2, 3]]
    };
    for [a, b, c] in tris {
        let tri = if positive { [a, b, c] } else { [a, c, b] };
        out.indices.extend(tri.map(|k| first + k));
    }
    out.quad_count += 1;
}

/// Linear RGB of a material's sRGB color, with the emissive strength as alpha.
fn vertex_base_color(materials: &MaterialTable, id: MaterialId) -> [f32; 4] {
    materials.get(id).map_or([0.0; 4], |def| {
        let [r, g, b, a] = def.color;
        let c = Color::srgba_u8(r, g, b, a).to_linear();
        [c.red, c.green, c.blue, def.emissive]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, LocalPos};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn table() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"))
    }

    fn id(table: &MaterialTable, key: &str) -> MaterialId {
        table.id(key).unwrap_or_else(|| panic!("material {key:?}"))
    }

    fn origin_input(table: &Arc<MaterialTable>) -> MeshInput {
        MeshInput::empty(ChunkPos::new(0, 0, 0), 0, table.clone())
    }

    fn mesh(input: &MeshInput) -> MeshData {
        GreedyMesher.mesh(input)
    }

    /// `base` with its face shade for `normal`: the color of an unoccluded
    /// corner of a face facing `normal`.
    fn shaded(base: [f32; 4], normal: IVec3) -> [f32; 4] {
        let f = face_shade(normal);
        [base[0] * f, base[1] * f, base[2] * f, base[3]]
    }

    /// One decoded quad, in cells (positions divided by the cell size).
    #[derive(Clone, Debug)]
    struct Quad {
        normal: IVec3,
        corners: [Vec3; 4],
        colors: [[f32; 4]; 4],
        /// Indices into `corners`, two triangles.
        tris: [[usize; 3]; 2],
    }

    impl Quad {
        fn min(&self) -> Vec3 {
            self.corners.iter().copied().reduce(Vec3::min).unwrap()
        }

        fn max(&self) -> Vec3 {
            self.corners.iter().copied().reduce(Vec3::max).unwrap()
        }

        /// Size in cells along the two axes the quad spans, in (x, y, z) order
        /// with the normal axis left out.
        fn size(&self) -> (f32, f32) {
            let s = self.max() - self.min();
            let v: Vec<f32> = (0..3)
                .filter(|&a| self.normal[a] == 0)
                .map(|a| s[a])
                .collect();
            (v[0], v[1])
        }

        /// Cells of the face plane, in cells along the normal axis.
        fn plane(&self) -> f32 {
            let a = (0..3).find(|&a| self.normal[a] != 0).unwrap();
            self.corners[0][a]
        }

        /// AO level of corner `i`: its color relative to the unoccluded
        /// `base` color shaded for this quad's normal, matched against
        /// [`AO_FACTORS`].
        fn level(&self, i: usize, base: [f32; 4]) -> usize {
            let base = shaded(base, self.normal);
            let c = self.colors[i];
            assert_eq!(c[3], base[3], "alpha is the emissive strength");
            let fs: Vec<f32> = (0..3)
                .filter(|&k| base[k] > 0.0)
                .map(|k| c[k] / base[k])
                .collect();
            AO_FACTORS
                .iter()
                .position(|f| fs.iter().all(|x| (x - f).abs() < 1e-4))
                .unwrap_or_else(|| panic!("color {c:?} is not base {base:?} times one AO factor"))
        }

        /// Corner nearest to `cell_corner` (in cells).
        fn corner_at(&self, cell_corner: Vec3) -> usize {
            (0..4)
                .find(|&i| self.corners[i].distance(cell_corner) < 1e-4)
                .unwrap_or_else(|| panic!("{cell_corner} not a corner of {:?}", self.corners))
        }
    }

    /// Decodes `data` into quads and checks the layout invariants every mesh
    /// must hold: 4 vertices and 6 indices per quad, indices inside their own
    /// quad, one axis-aligned unit normal per quad, a planar rectangle, and
    /// triangles wound counter-clockwise seen from the normal side.
    fn quads(data: &MeshData) -> Vec<Quad> {
        decode(data, true)
    }

    /// [`quads`] with the whole-cell check only when `whole_cells`: liquid
    /// quads may end at a fill height inside a cell.
    fn decode(data: &MeshData, whole_cells: bool) -> Vec<Quad> {
        let q = data.quad_count as usize;
        assert_eq!(data.positions.len(), 4 * q, "4 positions per quad");
        assert_eq!(data.normals.len(), 4 * q, "4 normals per quad");
        assert_eq!(data.colors.len(), 4 * q, "4 colors per quad");
        assert_eq!(data.indices.len(), 6 * q, "6 indices per quad");
        (0..q)
            .map(|i| {
                let n = data.normals[4 * i];
                for k in 0..4 {
                    assert_eq!(data.normals[4 * i + k], n, "one normal per quad");
                }
                let normal = IVec3::new(n[0] as i32, n[1] as i32, n[2] as i32);
                assert_eq!(
                    Vec3::from(n),
                    normal.as_vec3(),
                    "normal {n:?} is an axis unit vector"
                );
                assert_eq!(normal.abs().element_sum(), 1, "normal {n:?} is a unit axis");
                let corners: [Vec3; 4] =
                    std::array::from_fn(|k| Vec3::from(data.positions[4 * i + k]) / CELL_SIZE_M);
                let colors = std::array::from_fn(|k| data.colors[4 * i + k]);
                let idx = &data.indices[6 * i..6 * i + 6];
                let local = |v: u32| {
                    let v = v as usize;
                    assert!(
                        (4 * i..4 * i + 4).contains(&v),
                        "index {v} outside quad {i}"
                    );
                    v - 4 * i
                };
                let tris = [
                    [local(idx[0]), local(idx[1]), local(idx[2])],
                    [local(idx[3]), local(idx[4]), local(idx[5])],
                ];
                let axis = (0..3).find(|&a| normal[a] != 0).unwrap();
                for c in &corners {
                    assert_eq!(c[axis], corners[0][axis], "quad is planar");
                }
                let mut used = [0; 4];
                for t in &tris {
                    let [a, b, c] = t.map(|k| corners[k]);
                    let cross = (b - a).cross(c - a);
                    assert!(
                        cross.dot(normal.as_vec3()) > 0.0,
                        "triangle {t:?} of {corners:?} wound counter-clockwise toward {normal}"
                    );
                    for &k in t {
                        used[k] += 1;
                    }
                }
                assert!(
                    used.iter().all(|&u| u > 0),
                    "both triangles cover all 4 corners"
                );
                let quad = Quad {
                    normal,
                    corners,
                    colors,
                    tris,
                };
                let (w, h) = quad.size();
                let area: f32 = tris
                    .iter()
                    .map(|t| {
                        let [a, b, c] = t.map(|k| corners[k]);
                        (b - a).cross(c - a).length() / 2.0
                    })
                    .sum();
                if whole_cells {
                    assert!(w >= 1.0 && h >= 1.0, "quad {corners:?} spans whole cells");
                } else {
                    assert!(w > 0.0 && h > 0.0, "quad {corners:?} is not degenerate");
                }
                assert!(
                    (area - w * h).abs() < 1e-3,
                    "quad {corners:?} is a rectangle"
                );
                quad
            })
            .collect()
    }

    fn with_normal(qs: &[Quad], normal: IVec3) -> Vec<&Quad> {
        qs.iter().filter(|q| q.normal == normal).collect()
    }

    const DIRS: [IVec3; 6] = [
        IVec3::X,
        IVec3::NEG_X,
        IVec3::Y,
        IVec3::NEG_Y,
        IVec3::Z,
        IVec3::NEG_Z,
    ];

    #[test]
    fn single_cell_six_quads() {
        let t = table();
        let stone = id(&t, "stone");
        let crystal = id(&t, "glowcrystal");

        assert!(mesh(&origin_input(&t)).is_empty(), "all air: no quads");
        let mut full = origin_input(&t);
        full.cells.fill(stone);
        assert!(mesh(&full).is_empty(), "all solid: no quads");

        for (cell, mat) in [
            (IVec3::new(10, 7, 20), stone),
            (IVec3::new(0, 0, 0), stone),
            (IVec3::new(31, 31, 31), stone),
            (IVec3::new(5, 5, 5), crystal),
        ] {
            let mut input = origin_input(&t);
            input.set(cell.x, cell.y, cell.z, mat);
            let data = mesh(&input);
            assert_eq!(data.quad_count, 6, "one cell at {cell}: 6 quads");
            assert_eq!(data.positions.len(), 24);
            assert_eq!(data.indices.len(), 36);
            let qs = quads(&data);
            let base = vertex_base_color(&t, mat);
            assert_eq!(base[3], t.get(mat).unwrap().emissive);
            for dir in DIRS {
                let q = with_normal(&qs, dir);
                assert_eq!(q.len(), 1, "cell {cell}: one quad facing {dir}");
                let q = q[0];
                assert_eq!(q.size(), (1.0, 1.0), "a 1×1 face");
                let c = cell.as_vec3();
                let face = c + (dir.as_vec3() + Vec3::ONE) / 2.0;
                let lo = c.min(face).with(dir, face);
                let hi = (c + Vec3::ONE).with(dir, face);
                assert_eq!(q.min(), lo, "cell {cell} face {dir} lower corner");
                assert_eq!(q.max(), hi, "cell {cell} face {dir} upper corner");
                for (i, color) in q.colors.iter().enumerate() {
                    assert_eq!(
                        *color,
                        shaded(base, dir),
                        "unoccluded corner {i}: material color with its face shade, AO 1"
                    );
                }
            }
        }

        // A solid cell in the border is never meshed itself.
        let mut border = origin_input(&t);
        border.set(-1, 4, 4, stone);
        border.set(32, 4, 4, stone);
        border.set(4, 32, -1, stone);
        assert!(mesh(&border).is_empty(), "border cells are not meshed");
    }

    trait WithAxis {
        fn with(self, dir: IVec3, face: Vec3) -> Self;
    }

    impl WithAxis for Vec3 {
        /// Replaces the component on `dir`'s axis with `face`'s.
        fn with(mut self, dir: IVec3, face: Vec3) -> Self {
            let a = (0..3).find(|&a| dir[a] != 0).unwrap();
            self[a] = face[a];
            self
        }
    }

    /// Sorted (normal, size) pairs for comparing against an expected list.
    fn shape(qs: &[Quad]) -> Vec<(IVec3, (f32, f32))> {
        let mut v: Vec<_> = qs.iter().map(|q| (q.normal, q.size())).collect();
        v.sort_by(|a, b| {
            a.0.to_array()
                .cmp(&b.0.to_array())
                .then(a.1.partial_cmp(&b.1).unwrap())
        });
        v
    }

    fn expect_shape(mut expected: Vec<(IVec3, (f32, f32))>) -> Vec<(IVec3, (f32, f32))> {
        expected.sort_by(|a, b| {
            a.0.to_array()
                .cmp(&b.0.to_array())
                .then(a.1.partial_cmp(&b.1).unwrap())
        });
        expected
    }

    #[test]
    fn slab_2x2_merges() {
        let t = table();
        let stone = id(&t, "stone");
        let granite = id(&t, "granite");

        // A 2×2×1 slab: one quad per side.
        let mut input = origin_input(&t);
        for x in 4..6 {
            for z in 8..10 {
                input.set(x, 3, z, stone);
            }
        }
        let qs = quads(&mesh(&input));
        assert_eq!(
            shape(&qs),
            expect_shape(vec![
                (IVec3::Y, (2.0, 2.0)),
                (IVec3::NEG_Y, (2.0, 2.0)),
                (IVec3::X, (1.0, 2.0)),
                (IVec3::NEG_X, (1.0, 2.0)),
                (IVec3::Z, (2.0, 1.0)),
                (IVec3::NEG_Z, (2.0, 1.0)),
            ]),
            "2×2 slab merges to 6 quads"
        );
        let top = with_normal(&qs, IVec3::Y)[0];
        assert_eq!(top.min(), Vec3::new(4.0, 4.0, 8.0));
        assert_eq!(top.max(), Vec3::new(6.0, 4.0, 10.0));
        let base = vertex_base_color(&t, stone);
        assert!(
            qs.iter()
                .all(|q| q.colors.iter().all(|c| *c == shaded(base, q.normal)))
        );

        // A 3×2 rectangle: width and height merge independently.
        let mut input = origin_input(&t);
        for x in 10..13 {
            for z in 0..2 {
                input.set(x, 0, z, stone);
            }
        }
        let qs = quads(&mesh(&input));
        assert_eq!(qs.len(), 6, "3×2 slab: 6 quads");
        assert_eq!(with_normal(&qs, IVec3::Y)[0].size(), (3.0, 2.0));
        assert_eq!(with_normal(&qs, IVec3::NEG_Z)[0].size(), (3.0, 1.0));

        // A whole 32×32 layer whose neighbours continue it: top and bottom only.
        let mut input = origin_input(&t);
        for x in -1..=32 {
            for z in -1..=32 {
                input.set(x, 16, z, stone);
            }
        }
        let qs = quads(&mesh(&input));
        assert_eq!(
            shape(&qs),
            expect_shape(vec![(IVec3::Y, (32.0, 32.0)), (IVec3::NEG_Y, (32.0, 32.0))]),
            "a full layer is 2 quads"
        );
        assert_eq!(with_normal(&qs, IVec3::Y)[0].plane(), 17.0);
        assert_eq!(with_normal(&qs, IVec3::NEG_Y)[0].plane(), 16.0);

        // Two materials side by side do not merge with each other.
        let mut input = origin_input(&t);
        for z in 0..2 {
            input.set(20, 5, z, stone);
            input.set(21, 5, z, granite);
        }
        let qs = quads(&mesh(&input));
        assert_eq!(
            shape(&qs),
            expect_shape(vec![
                (IVec3::Y, (1.0, 2.0)),
                (IVec3::Y, (1.0, 2.0)),
                (IVec3::NEG_Y, (1.0, 2.0)),
                (IVec3::NEG_Y, (1.0, 2.0)),
                (IVec3::X, (1.0, 2.0)),
                (IVec3::NEG_X, (1.0, 2.0)),
                (IVec3::Z, (1.0, 1.0)),
                (IVec3::Z, (1.0, 1.0)),
                (IVec3::NEG_Z, (1.0, 1.0)),
                (IVec3::NEG_Z, (1.0, 1.0)),
            ]),
            "a material change splits the merge"
        );

        // An L of 3 cells: the top cannot be one rectangle.
        let mut input = origin_input(&t);
        input.set(0, 20, 0, stone);
        input.set(1, 20, 0, stone);
        input.set(0, 20, 1, stone);
        let qs = quads(&mesh(&input));
        let tops = with_normal(&qs, IVec3::Y);
        assert_eq!(tops.len(), 2, "L-shaped top: 2 quads");
        let area: f32 = tops.iter().map(|q| q.size().0 * q.size().1).sum();
        assert_eq!(area, 3.0, "the 2 quads cover the 3 cells");
    }

    #[test]
    fn checkerboard_no_merge() {
        let t = table();
        let stone = id(&t, "stone");
        let granite = id(&t, "granite");

        // Solid/air checkerboard: no two faces share an edge in their plane.
        let mut input = origin_input(&t);
        let mut solid = 0;
        for y in 8..16 {
            for z in 8..16 {
                for x in 8..16 {
                    if (x + y + z) % 2 == 0 {
                        input.set(x, y, z, stone);
                        solid += 1;
                    }
                }
            }
        }
        assert_eq!(solid, 256);
        let qs = quads(&mesh(&input));
        assert_eq!(
            qs.len(),
            6 * solid,
            "every face of every cell is its own quad"
        );
        assert!(qs.iter().all(|q| q.size() == (1.0, 1.0)));

        // Material checkerboard in one solid layer: every face differs from
        // its neighbours in material.
        let mut input = origin_input(&t);
        for z in 0..8 {
            for x in 0..8 {
                let m = if (x + z) % 2 == 0 { stone } else { granite };
                input.set(x, 4, z, m);
            }
        }
        let qs = quads(&mesh(&input));
        assert_eq!(with_normal(&qs, IVec3::Y).len(), 64);
        assert_eq!(with_normal(&qs, IVec3::NEG_Y).len(), 64);
        for dir in [IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z] {
            assert_eq!(with_normal(&qs, dir).len(), 8, "side {dir}: 8 quads");
        }
        assert_eq!(qs.len(), 160);
        assert!(qs.iter().all(|q| q.size() == (1.0, 1.0)));
    }

    /// The quads facing +Y whose plane is the top of cell row `y`.
    fn tops_at(qs: &[Quad], y: i32) -> Vec<&Quad> {
        qs.iter()
            .filter(|q| q.normal == IVec3::Y && q.plane() == (y + 1) as f32)
            .collect()
    }

    #[test]
    fn ao_mismatch_prevents_merge() {
        let t = table();
        let stone = id(&t, "stone");
        let base = vertex_base_color(&t, stone);
        let strip = |input: &mut MeshInput| {
            input.set(4, 4, 4, stone);
            input.set(5, 4, 4, stone);
        };

        // No occluder: the strip's top is one 2×1 quad, fully lit.
        let mut input = origin_input(&t);
        strip(&mut input);
        let qs = quads(&mesh(&input));
        let tops = tops_at(&qs, 4);
        assert_eq!(tops.len(), 1, "no occluder: one merged top");
        assert_eq!(tops[0].size(), (2.0, 1.0));

        // One occluder above-and-behind cell (4,4,4): it is a side sample for
        // both of that cell's z+ corners (level 2) but only the corner sample
        // for one of (5,4,4)'s (level 2 there, 3 at the other). The AO keys
        // differ, so the tops must not merge.
        let mut input = origin_input(&t);
        strip(&mut input);
        input.set(4, 5, 5, stone);
        let qs = quads(&mesh(&input));
        let tops = tops_at(&qs, 4);
        assert_eq!(tops.len(), 2, "AO differs: the tops stay apart");
        let a = tops
            .iter()
            .find(|q| q.min() == Vec3::new(4.0, 5.0, 4.0))
            .expect("top of (4,4,4)");
        let b = tops
            .iter()
            .find(|q| q.min() == Vec3::new(5.0, 5.0, 4.0))
            .expect("top of (5,4,4)");
        assert_eq!(a.size(), (1.0, 1.0));
        assert_eq!(b.size(), (1.0, 1.0));
        for (q, corner, level) in [
            (a, Vec3::new(4.0, 5.0, 4.0), 3),
            (a, Vec3::new(5.0, 5.0, 4.0), 3),
            (a, Vec3::new(4.0, 5.0, 5.0), 2),
            (a, Vec3::new(5.0, 5.0, 5.0), 2),
            (b, Vec3::new(5.0, 5.0, 4.0), 3),
            (b, Vec3::new(6.0, 5.0, 4.0), 3),
            (b, Vec3::new(5.0, 5.0, 5.0), 2),
            (b, Vec3::new(6.0, 5.0, 5.0), 3),
        ] {
            let got = q.level(q.corner_at(corner), base);
            assert_eq!(got, level, "corner {corner}: AO level");
        }

        // A wall above the strip's z+ edge, longer than the strip: every top
        // has the same AO (z+ corners level 1), so they merge again.
        let mut input = origin_input(&t);
        strip(&mut input);
        for x in 3..=6 {
            input.set(x, 5, 5, stone);
        }
        let qs = quads(&mesh(&input));
        let tops = tops_at(&qs, 4);
        assert_eq!(tops.len(), 1, "equal AO: the tops merge");
        let q = tops[0];
        assert_eq!(q.size(), (2.0, 1.0));
        for (corner, level) in [
            (Vec3::new(4.0, 5.0, 4.0), 3),
            (Vec3::new(6.0, 5.0, 4.0), 3),
            (Vec3::new(4.0, 5.0, 5.0), 1),
            (Vec3::new(6.0, 5.0, 5.0), 1),
        ] {
            assert_eq!(q.level(q.corner_at(corner), base), level, "corner {corner}");
        }

        // Inner corner: both side samples solid gives level 0 even though the
        // corner sample is air. The quad is split along the diagonal that
        // keeps the dark corner in one triangle.
        let mut input = origin_input(&t);
        input.set(10, 4, 10, stone);
        input.set(10, 5, 11, stone);
        input.set(11, 5, 10, stone);
        let qs = quads(&mesh(&input));
        let tops = tops_at(&qs, 4);
        let q = tops
            .iter()
            .find(|q| q.min() == Vec3::new(10.0, 5.0, 10.0))
            .expect("top of (10,4,10)");
        let dark = q.corner_at(Vec3::new(11.0, 5.0, 11.0));
        assert_eq!(q.level(dark, base), 0, "both sides solid: level 0");
        assert_eq!(q.level(q.corner_at(Vec3::new(10.0, 5.0, 10.0)), base), 3);
        assert_eq!(q.level(q.corner_at(Vec3::new(11.0, 5.0, 10.0)), base), 2);
        assert_eq!(q.level(q.corner_at(Vec3::new(10.0, 5.0, 11.0)), base), 2);
        let in_tris = q.tris.iter().filter(|t| t.contains(&dark)).count();
        assert_eq!(in_tris, 1, "the dark corner is not on the split diagonal");
    }

    /// The face shade each normal must get: the top lit fully, the two side
    /// axes apart from each other, the bottom darkest.
    const SHADES: [(IVec3, f32); 6] = [
        (IVec3::Y, 1.0),
        (IVec3::X, 0.8),
        (IVec3::NEG_X, 0.8),
        (IVec3::Z, 0.7),
        (IVec3::NEG_Z, 0.7),
        (IVec3::NEG_Y, 0.55),
    ];

    #[test]
    fn face_shade_differs_by_normal() {
        let t = table();
        for (n, want) in SHADES {
            assert_eq!(face_shade(n), want, "face shade for normal {n}");
        }
        let shade = |n: IVec3| face_shade(n);
        assert!(
            shade(IVec3::Y) > shade(IVec3::X),
            "top is brighter than a side"
        );
        assert!(
            shade(IVec3::X) != shade(IVec3::Z),
            "the two side axes differ"
        );
        assert!(
            shade(IVec3::X).min(shade(IVec3::Z)) > shade(IVec3::NEG_Y),
            "the bottom is darker than every side"
        );
        // The step between the top and the darker side is visible after the
        // sRGB encode: at least 10% in display value.
        let encode = |f: f32| Color::linear_rgb(f, f, f).to_srgba().red;
        assert!(
            encode(shade(IVec3::Z)) <= 0.9 * encode(shade(IVec3::Y)),
            "side vs top in sRGB: {} vs {}",
            encode(shade(IVec3::Z)),
            encode(shade(IVec3::Y))
        );

        // Every material's unoccluded faces carry exactly that shade, scaled
        // equally on all channels (the hue is kept), with the emissive
        // strength in alpha untouched.
        for def in t.iter().filter(|d| d.is_solid()) {
            let mut input = origin_input(&t);
            input.set(12, 12, 12, def.id);
            let qs = quads(&mesh(&input));
            assert_eq!(qs.len(), 6);
            let base = vertex_base_color(&t, def.id);
            for (n, f) in SHADES {
                let q = with_normal(&qs, n);
                assert_eq!(q.len(), 1);
                for c in q[0].colors {
                    let want = [base[0] * f, base[1] * f, base[2] * f, base[3]];
                    assert_eq!(c, want, "{} face {n}", def.key);
                    assert_eq!(c[3], def.emissive, "{} face {n}: alpha", def.key);
                }
            }
        }

        // AO multiplies on top of the shade: a wall beside a floor darkens the
        // floor's corners by the AO factors, and the wall's own face keeps its
        // side shade.
        let stone = id(&t, "stone");
        let base = vertex_base_color(&t, stone);
        let mut input = origin_input(&t);
        input.set(4, 4, 4, stone);
        input.set(4, 5, 5, stone);
        let qs = quads(&mesh(&input));
        let top = tops_at(&qs, 4)
            .into_iter()
            .find(|q| q.min() == Vec3::new(4.0, 5.0, 4.0))
            .expect("top of (4,4,4)");
        for (corner, level) in [(Vec3::new(4.0, 5.0, 4.0), 3), (Vec3::new(4.0, 5.0, 5.0), 2)] {
            let c = top.colors[top.corner_at(corner)];
            let f = AO_FACTORS[level];
            for k in 0..3 {
                assert!((c[k] - base[k] * f).abs() < 1e-6, "corner {corner}: {c:?}");
            }
        }
        let wall = qs
            .iter()
            .find(|q| q.normal == IVec3::NEG_Z && q.plane() == 5.0)
            .expect("the -Z face of (4,5,5)");
        for (i, c) in wall.colors.iter().enumerate() {
            let level = wall.level(i, base);
            let f = 0.7 * AO_FACTORS[level];
            for k in 0..3 {
                assert!((c[k] - base[k] * f).abs() < 1e-6, "wall corner {i}: {c:?}");
            }
        }
    }

    /// Terrain over the whole 34³ input: plateaus of 3×4 columns at heights
    /// 6..=10, topsoil on top of two dirt cells on top of stone.
    fn plateau_terrain(t: &Arc<MaterialTable>) -> MeshInput {
        let (topsoil, dirt, stone) = (id(t, "topsoil"), id(t, "dirt"), id(t, "stone"));
        let mut input = origin_input(t);
        for z in -1..=32_i32 {
            for x in -1..=32_i32 {
                let h = 6 + (x.div_euclid(3) * 7 + z.div_euclid(4) * 3).rem_euclid(5);
                for y in -1..=h {
                    let m = match h - y {
                        0 => topsoil,
                        1 | 2 => dirt,
                        _ => stone,
                    };
                    input.set(x, y, z, m);
                }
            }
        }
        input
    }

    #[test]
    fn face_shade_keeps_greedy_merging() {
        let t = table();
        let stone = id(&t, "stone");

        // A flat 32×32 slab whose neighbours continue it: still 2 quads, one
        // per face, each a single shade.
        let mut input = origin_input(&t);
        for x in -1..=32 {
            for z in -1..=32 {
                input.set(x, 16, z, stone);
            }
        }
        let qs = quads(&mesh(&input));
        assert_eq!(
            shape(&qs),
            expect_shape(vec![(IVec3::Y, (32.0, 32.0)), (IVec3::NEG_Y, (32.0, 32.0))]),
            "a full layer is 2 quads"
        );

        // A free-standing 32×32×1 slab: one quad per face, sides included.
        let mut input = origin_input(&t);
        for x in 0..32 {
            for z in 0..32 {
                input.set(x, 3, z, stone);
            }
        }
        let qs = quads(&mesh(&input));
        assert_eq!(
            shape(&qs),
            expect_shape(vec![
                (IVec3::Y, (32.0, 32.0)),
                (IVec3::NEG_Y, (32.0, 32.0)),
                (IVec3::X, (1.0, 32.0)),
                (IVec3::NEG_X, (1.0, 32.0)),
                (IVec3::Z, (32.0, 1.0)),
                (IVec3::NEG_Z, (32.0, 1.0)),
            ]),
            "a free slab is 6 quads"
        );

        // Stepped terrain: the quad count per normal is the one the mesher
        // produced before face shading (measured with every shade at 1.0).
        let data = mesh(&plateau_terrain(&t));
        let qs = quads(&data);
        let count = |n: IVec3| with_normal(&qs, n).len();
        let counts: Vec<(IVec3, usize)> = DIRS.iter().map(|&n| (n, count(n))).collect();
        assert_eq!(counts, PLATEAU_QUADS.to_vec(), "quads per normal");
        assert_eq!(
            data.quad_count as usize,
            PLATEAU_QUADS.iter().map(|c| c.1).sum::<usize>()
        );
        // Each quad has one shade on all four corners: its colors are the
        // material color times that shade times one AO factor each.
        let fits = |q: &Quad, base: [f32; 4]| {
            let want = shaded(base, q.normal);
            q.colors.iter().all(|c| {
                c[3] == want[3]
                    && AO_FACTORS
                        .iter()
                        .any(|f| (0..3).all(|k| (c[k] - want[k] * f).abs() < 1e-6))
            })
        };
        for q in &qs {
            assert!(
                t.iter().any(|d| fits(q, vertex_base_color(&t, d.id))),
                "quad {:?}: one material, one shade, AO per corner",
                q.corners
            );
        }
    }

    /// Quads per normal, in [`DIRS`] order, of [`plateau_terrain`] before
    /// face shading.
    const PLATEAU_QUADS: [(IVec3, usize); 6] = [
        (IVec3::X, 288),
        (IVec3::NEG_X, 295),
        (IVec3::Y, 516),
        (IVec3::NEG_Y, 0),
        (IVec3::Z, 287),
        (IVec3::NEG_Z, 306),
    ];

    /// A world over chunks -1..=1 on every axis.
    fn world_3x3x3(table: &Arc<MaterialTable>) -> World {
        World::new(
            table.clone(),
            CellAabb::new(CellPos::new(-32, -32, -32), CellPos::new(63, 63, 63)),
        )
    }

    fn insert(world: &mut World, pos: ChunkPos, chunk: Chunk) {
        world.insert_chunk(pos, chunk).expect("chunk in bounds");
    }

    /// A chunk of air with `cells` (world cells inside `pos`) set to `id`.
    fn chunk_with(pos: ChunkPos, cells: &[IVec3], id: MaterialId) -> Chunk {
        let mut chunk = Chunk::uniform(MaterialId::AIR);
        for &c in cells {
            let (cp, local) = CellPos(c).split();
            assert_eq!(cp, pos, "cell {c} lies in chunk {pos:?}");
            chunk.storage.set(local, id);
        }
        chunk
    }

    #[test]
    fn border_faces_use_neighbor_cells() {
        let t = table();
        let stone = id(&t, "stone");
        let granite = id(&t, "granite");
        let center = ChunkPos::new(0, 0, 0);

        // The input copies every cell of the 34³ box from the owning chunk;
        // missing chunks and cells outside the bounds read as air.
        let mut world = World::new(
            t.clone(),
            CellAabb::new(CellPos::new(-32, -20, -32), CellPos::new(63, 20, 63)),
        );
        let mats = [stone, granite, id(&t, "dirt"), id(&t, "basalt")];
        for (i, pos) in world.bounds().chunks().enumerate() {
            if i % 5 == 3 && pos != center {
                continue; // not resident
            }
            let mut chunk = Chunk::uniform(MaterialId::AIR);
            for j in 0..rmc_world::coords::CHUNK_VOLUME as u16 {
                let h = (u32::from(j).wrapping_mul(2_654_435_761) ^ (i as u32 * 40_503)) >> 7;
                chunk
                    .storage
                    .set(LocalPos::from_index(j), mats[(h % 4) as usize]);
            }
            chunk.revision = i as u64 + 7;
            insert(&mut world, pos, chunk);
        }
        assert!(
            MeshInput::from_world(&world, ChunkPos::new(5, 0, 0)).is_none(),
            "no input for a chunk that is not resident"
        );
        let missing = world
            .bounds()
            .chunks()
            .filter(|p| world.chunk(*p).is_none())
            .count();
        assert!(missing >= 2, "some neighbours are not resident");
        let input = MeshInput::from_world(&world, center).expect("center is resident");
        assert_eq!(input.pos, center);
        assert_eq!(input.revision, world.chunk(center).unwrap().revision);
        let (mut air, mut solid) = (0, 0);
        for y in -1..=32 {
            for z in -1..=32 {
                for x in -1..=32 {
                    let want = world
                        .get_cell(CellPos::new(x, y, z))
                        .unwrap_or(MaterialId::AIR);
                    assert_eq!(input.get(x, y, z), want, "cell ({x}, {y}, {z})");
                    if want == MaterialId::AIR {
                        air += 1;
                    } else {
                        solid += 1;
                    }
                }
            }
        }
        assert!(
            air > 0 && solid > 0,
            "both missing and resident border cells seen"
        );
        assert_eq!(
            input.get(5, 25, 5),
            MaterialId::AIR,
            "a stored cell outside the bounds reads as air"
        );
        assert_ne!(
            world
                .chunk(center)
                .unwrap()
                .storage
                .get(LocalPos::new(5, 25, 5)),
            MaterialId::AIR
        );
        // The same for a chunk at negative coordinates, whose border reaches
        // the bottom edge of the bounds.
        let corner = ChunkPos::new(-1, -1, -1);
        let input = MeshInput::from_world(&world, corner).expect("corner is resident");
        let origin = corner.origin_cell().0;
        for y in -1..=32 {
            for z in -1..=32 {
                for x in -1..=32 {
                    let want = world
                        .get_cell(CellPos(origin + IVec3::new(x, y, z)))
                        .unwrap_or(MaterialId::AIR);
                    assert_eq!(
                        input.get(x, y, z),
                        want,
                        "corner chunk cell ({x}, {y}, {z})"
                    );
                }
            }
        }

        // A cell on each face of the center chunk: the face toward the border
        // is hidden when the neighbour's cell there is solid, and drawn when it
        // is air or its chunk is not resident.
        for dir in DIRS {
            let local = IVec3::splat(10) + dir * 10 + dir.max(IVec3::ZERO) * 11;
            assert!(local.cmpge(IVec3::ZERO).all() && local.cmplt(IVec3::splat(32)).all());
            assert!(
                local.to_array().contains(&0) || local.to_array().contains(&31),
                "cell {local} is on the border"
            );
            let beyond = local + dir;
            let npos = ChunkPos(dir);
            for case in ["solid", "air", "missing"] {
                let mut world = world_3x3x3(&t);
                insert(&mut world, center, chunk_with(center, &[local], stone));
                match case {
                    "solid" => insert(&mut world, npos, chunk_with(npos, &[beyond], granite)),
                    "air" => insert(&mut world, npos, chunk_with(npos, &[], granite)),
                    _ => {}
                }
                let input = MeshInput::from_world(&world, center).unwrap();
                let qs = quads(&mesh(&input));
                let toward = with_normal(&qs, dir).len();
                let hidden = case == "solid";
                assert_eq!(
                    qs.len(),
                    if hidden { 5 } else { 6 },
                    "face {dir} at {local}, neighbour {case}"
                );
                assert_eq!(toward, usize::from(!hidden), "face {dir}, neighbour {case}");
                for other in DIRS.iter().filter(|d| **d != dir) {
                    assert_eq!(with_normal(&qs, *other).len(), 1);
                }
            }
        }

        // AO reads the neighbour: a cell above-and-across the +X border
        // darkens the two +X corners of a top face on the border.
        let mut world = world_3x3x3(&t);
        insert(
            &mut world,
            center,
            chunk_with(center, &[IVec3::new(31, 10, 10)], stone),
        );
        let east = ChunkPos::new(1, 0, 0);
        insert(
            &mut world,
            east,
            chunk_with(east, &[IVec3::new(32, 11, 10)], stone),
        );
        let qs = quads(&mesh(&MeshInput::from_world(&world, center).unwrap()));
        let top = tops_at(&qs, 10);
        assert_eq!(top.len(), 1);
        let base = vertex_base_color(&t, stone);
        for (corner, level) in [
            (Vec3::new(31.0, 11.0, 10.0), 3),
            (Vec3::new(31.0, 11.0, 11.0), 3),
            (Vec3::new(32.0, 11.0, 10.0), 2),
            (Vec3::new(32.0, 11.0, 11.0), 2),
        ] {
            let got = top[0].level(top[0].corner_at(corner), base);
            assert_eq!(got, level, "corner {corner}");
        }

        // A solid chunk among solid neighbours has no faces; remove the layer
        // of chunks above and its whole top is a single, fully lit quad.
        let mut world = world_3x3x3(&t);
        for pos in world.bounds().chunks().collect::<Vec<_>>() {
            insert(&mut world, pos, Chunk::uniform(stone));
        }
        let input = MeshInput::from_world(&world, center).unwrap();
        assert!(mesh(&input).is_empty(), "buried chunk: no quads");
        let mut world = world_3x3x3(&t);
        for pos in world.bounds().chunks().collect::<Vec<_>>() {
            if pos.0.y < 1 {
                insert(&mut world, pos, Chunk::uniform(stone));
            }
        }
        let qs = quads(&mesh(&MeshInput::from_world(&world, center).unwrap()));
        assert_eq!(shape(&qs), expect_shape(vec![(IVec3::Y, (32.0, 32.0))]));
        assert_eq!(qs[0].plane(), 32.0);
        assert!((0..4).all(|i| qs[0].level(i, vertex_base_color(&t, stone)) == 3));

        // With only the chunk straight above removed, the solid chunks above
        // the edges occlude the top's border row, which splits off by AO.
        let mut world = world_3x3x3(&t);
        for pos in world.bounds().chunks().collect::<Vec<_>>() {
            if pos != ChunkPos::new(0, 1, 0) {
                insert(&mut world, pos, Chunk::uniform(stone));
            }
        }
        let qs = quads(&mesh(&MeshInput::from_world(&world, center).unwrap()));
        assert!(qs.iter().all(|q| q.normal == IVec3::Y && q.plane() == 32.0));
        assert_eq!(
            qs.iter().map(|q| q.size().0 * q.size().1).sum::<f32>(),
            1024.0
        );
        assert!(
            qs.iter().any(|q| q.size() == (30.0, 30.0)),
            "the unoccluded middle merges"
        );

        // An empty chunk among solid neighbours has no faces either: the
        // neighbours' faces toward it belong to their own meshes.
        let mut world = world_3x3x3(&t);
        for pos in world.bounds().chunks().collect::<Vec<_>>() {
            let fill = if pos == center {
                MaterialId::AIR
            } else {
                stone
            };
            insert(&mut world, pos, Chunk::uniform(fill));
        }
        let input = MeshInput::from_world(&world, center).unwrap();
        assert!(mesh(&input).is_empty(), "neighbour faces are not ours");
    }

    /// The liquid pass's quads, decoded with fractional heights allowed.
    fn liquid(input: &MeshInput) -> Vec<Quad> {
        decode(&GreedyMesher.mesh_liquid(input), false)
    }

    /// Height of `fill` in cells.
    fn h(fill: u8) -> f32 {
        f32::from(fill) / 255.0
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance(b) < 1e-5
    }

    /// The color every corner of a liquid face facing `normal` has: the
    /// material color in linear RGB times the face shade, with the
    /// material's alpha as opacity.
    fn liquid_color(t: &MaterialTable, mat: MaterialId, normal: IVec3) -> [f32; 4] {
        let def = t.get(mat).unwrap();
        let [r, g, b, a] = def.color;
        let c = Color::srgba_u8(r, g, b, a).to_linear();
        let f = face_shade(normal);
        [c.red * f, c.green * f, c.blue * f, f32::from(a) / 255.0]
    }

    #[test]
    fn liquid_top_face_at_fill_height() {
        let t = table();
        let water = id(&t, "water");
        let stone = id(&t, "stone");

        // A lone water cell in air: six faces, the top at y + fill/255 and
        // the sides clipped to it, the bottom on the cell's floor.
        for fill in [1u8, 64, 128, 200, 254, 255] {
            let mut input = origin_input(&t);
            input.set_liquid(5, 5, 5, water, fill);
            let qs = liquid(&input);
            assert_eq!(qs.len(), 6, "fill {fill}: 6 faces");
            let top = with_normal(&qs, IVec3::Y)[0];
            assert!(
                (top.plane() - (5.0 + h(fill))).abs() < 1e-5,
                "fill {fill}: top"
            );
            assert!(close(top.min(), Vec3::new(5.0, 5.0 + h(fill), 5.0)));
            assert!(close(top.max(), Vec3::new(6.0, 5.0 + h(fill), 6.0)));
            let bottom = with_normal(&qs, IVec3::NEG_Y)[0];
            assert_eq!(bottom.plane(), 5.0, "fill {fill}: bottom on the floor");
            for dir in [IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z] {
                let side = with_normal(&qs, dir);
                assert_eq!(side.len(), 1, "fill {fill}: one side {dir}");
                assert!((side[0].min().y - 5.0).abs() < 1e-5, "side from the floor");
                assert!(
                    (side[0].max().y - (5.0 + h(fill))).abs() < 1e-5,
                    "fill {fill}: side {dir} clipped to the fill"
                );
            }
            for q in &qs {
                for c in q.colors {
                    assert_eq!(c, liquid_color(&t, water, q.normal), "fill {fill}");
                }
            }
            assert!(
                mesh(&input).is_empty(),
                "fill {fill}: not in the opaque pass"
            );
        }

        // Fill 0 draws nothing.
        let mut input = origin_input(&t);
        input.set_liquid(5, 5, 5, water, 0);
        assert!(GreedyMesher.mesh_liquid(&input).is_empty(), "fill 0");

        // Water on a stone floor: no bottom face; the stone's top shows
        // through the water in the opaque pass, without AO from the water.
        let mut input = origin_input(&t);
        input.set(5, 4, 5, stone);
        input.set_liquid(5, 5, 5, water, 100);
        let qs = liquid(&input);
        assert_eq!(qs.len(), 5, "no face against the floor");
        assert!(with_normal(&qs, IVec3::NEG_Y).is_empty());
        let opaque = quads(&mesh(&input));
        assert_eq!(opaque.len(), 6, "the stone keeps all six faces");
        let top = tops_at(&opaque, 4);
        assert_eq!(top.len(), 1, "the stone top under the water");
        let base = vertex_base_color(&t, stone);
        assert!((0..4).all(|i| top[0].level(i, base) == 3), "no AO");

        // A column: a full cell under a partial one has no face between them;
        // one top at the upper cell's fill; sides from the floor of the lower
        // cell to that fill.
        let mut input = origin_input(&t);
        input.set_liquid(8, 3, 8, water, 255);
        input.set_liquid(8, 4, 8, water, 90);
        let qs = liquid(&input);
        let tops = with_normal(&qs, IVec3::Y);
        assert_eq!(tops.len(), 1, "one top for the column");
        assert!((tops[0].plane() - (4.0 + h(90))).abs() < 1e-5);
        let bottoms = with_normal(&qs, IVec3::NEG_Y);
        assert_eq!(bottoms.len(), 1);
        assert_eq!(bottoms[0].plane(), 3.0);
        for dir in [IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z] {
            let mut spans: Vec<(f32, f32)> = with_normal(&qs, dir)
                .iter()
                .map(|q| (q.min().y, q.max().y))
                .collect();
            spans.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let covered: f32 = spans.iter().map(|(lo, hi)| hi - lo).sum();
            assert!(
                (covered - (1.0 + h(90))).abs() < 1e-5,
                "side {dir}: {spans:?}"
            );
            assert!((spans[0].0 - 3.0).abs() < 1e-5);
            assert!((spans.last().unwrap().1 - (4.0 + h(90))).abs() < 1e-5);
        }

        // Two cells side by side at different fills: the higher has a side
        // toward the lower only above the lower one's level, the lower none
        // toward the higher, and the tops do not merge.
        let mut input = origin_input(&t);
        input.set_liquid(10, 2, 10, water, 200);
        input.set_liquid(11, 2, 10, water, 50);
        let qs = liquid(&input);
        let east: Vec<&Quad> = with_normal(&qs, IVec3::X)
            .into_iter()
            .filter(|q| q.plane() == 11.0)
            .collect();
        assert_eq!(east.len(), 1, "the high cell's face toward the low one");
        assert!(
            (east[0].min().y - (2.0 + h(50))).abs() < 1e-5,
            "from the low fill"
        );
        assert!(
            (east[0].max().y - (2.0 + h(200))).abs() < 1e-5,
            "to the high fill"
        );
        assert!(
            with_normal(&qs, IVec3::NEG_X)
                .iter()
                .all(|q| q.plane() != 11.0),
            "the low cell has no face toward the high one"
        );
        assert_eq!(with_normal(&qs, IVec3::Y).len(), 2, "different fills");
        // Equal fills side by side: no face between them, one merged top.
        let mut input = origin_input(&t);
        input.set_liquid(10, 2, 10, water, 120);
        input.set_liquid(11, 2, 10, water, 120);
        let qs = liquid(&input);
        assert!(
            qs.iter().all(|q| q.normal.x == 0 || q.plane() != 11.0),
            "no face between equal fills"
        );
        let tops = with_normal(&qs, IVec3::Y);
        assert_eq!(tops.len(), 1);
        assert_eq!(tops[0].size(), (2.0, 1.0));

        // Under a ceiling: a full cell hides its top, a partial one does not.
        for (fill, tops) in [(255u8, 0usize), (180, 1)] {
            let mut input = origin_input(&t);
            input.set_liquid(3, 3, 3, water, fill);
            input.set(3, 4, 3, stone);
            let qs = liquid(&input);
            assert_eq!(with_normal(&qs, IVec3::Y).len(), tops, "fill {fill}");
        }

        // A walled 4×4 pool at one fill: its surface is one merged quad.
        let mut input = origin_input(&t);
        for x in 3..=8 {
            for z in 3..=8 {
                input.set(x, 1, z, stone);
                if (4..=7).contains(&x) && (4..=7).contains(&z) {
                    input.set_liquid(x, 2, z, water, 230);
                } else {
                    input.set(x, 2, z, stone);
                }
            }
        }
        let qs = liquid(&input);
        assert_eq!(
            qs.len(),
            1,
            "only the surface: walls and floor hide the rest"
        );
        assert_eq!(qs[0].normal, IVec3::Y);
        assert_eq!(qs[0].size(), (4.0, 4.0));
        assert!((qs[0].plane() - (2.0 + h(230))).abs() < 1e-5);

        // A full lake face against air merges over its height and width.
        let mut input = origin_input(&t);
        for y in 0..4 {
            for z in 0..32 {
                input.set_liquid(0, y, z, water, 255);
            }
        }
        let qs = liquid(&input);
        let west = with_normal(&qs, IVec3::NEG_X);
        assert_eq!(west.len(), 1);
        assert_eq!(west[0].size(), (4.0, 32.0));

        // Fills come from the world's moisture layer, for liquid cells only.
        let mut world = world_3x3x3(&t);
        let center = ChunkPos::new(0, 0, 0);
        let mut chunk = chunk_with(center, &[IVec3::new(6, 6, 6)], water);
        chunk
            .layers
            .set_u8(LayerId::Moisture, LocalPos::new(6, 6, 6), 77);
        chunk.storage.set(LocalPos::new(6, 5, 6), stone);
        chunk
            .layers
            .set_u8(LayerId::Moisture, LocalPos::new(6, 5, 6), 9);
        insert(&mut world, center, chunk);
        let input = MeshInput::from_world(&world, center).unwrap();
        assert_eq!(input.fill(6, 6, 6), 77, "a water cell brings its fill");
        assert_eq!(input.fill(6, 5, 6), 0, "a wet stone brings none");
        let qs = liquid(&input);
        let top = with_normal(&qs, IVec3::Y);
        assert_eq!(top.len(), 1);
        assert!((top[0].plane() - (6.0 + h(77))).abs() < 1e-5);
    }

    /// The shipped table plus `lava`: an opaque, glowing liquid, as magma is.
    fn table_with_opaque_liquid() -> Arc<MaterialTable> {
        let t = table();
        let mut defs: Vec<MaterialDef> = t.iter().cloned().collect();
        let mut lava = t.get(id(&t, "water")).unwrap().clone();
        lava.id = MaterialId(defs.len() as u16);
        lava.key = "lava".into();
        lava.color = [230, 90, 20, 255];
        lava.emissive = 2.0;
        defs.push(lava);
        Arc::new(MaterialTable::from_defs(defs).expect("table with lava"))
    }

    #[test]
    fn liquid_faces_in_second_mesh() {
        let t = table_with_opaque_liquid();
        let water = id(&t, "water");
        let lava = id(&t, "lava");
        let stone = id(&t, "stone");

        // Which materials go where: translucent liquids in the liquid pass,
        // solids and opaque liquids in the opaque pass, air in neither.
        assert!(is_liquid_pass(t.get(water).unwrap()), "water");
        assert!(!is_liquid_pass(t.get(lava).unwrap()), "an opaque liquid");
        for def in t.iter() {
            let translucent = def.phase == Phase::Liquid && def.color[3] < u8::MAX;
            assert_eq!(is_liquid_pass(def), translucent, "{}", def.key);
            let opaque = def.phase == Phase::Solid || (def.phase == Phase::Liquid && !translucent);
            assert_eq!(is_opaque(def), opaque, "{}", def.key);
        }

        // Water is only in the second mesh, solids only in the first.
        let mut input = origin_input(&t);
        input.set_liquid(4, 4, 4, water, 255);
        input.set(10, 4, 4, stone);
        let opaque = quads(&mesh(&input));
        let liquid_qs = liquid(&input);
        assert_eq!(opaque.len(), 6, "the stone");
        assert!(
            opaque.iter().all(|q| q.min().x >= 10.0),
            "no water in the first"
        );
        assert_eq!(liquid_qs.len(), 6, "the water");
        assert!(
            liquid_qs.iter().all(|q| q.max().x <= 5.0),
            "no stone in the second"
        );
        let base = vertex_base_color(&t, stone);
        for q in &opaque {
            assert!(q.colors.iter().all(|c| *c == shaded(base, q.normal)));
        }
        let alpha = f32::from(t.get(water).unwrap().color[3]) / 255.0;
        for q in &liquid_qs {
            for c in q.colors {
                assert_eq!(c[3], alpha, "opacity from the material color");
                assert!(c[3] < 1.0, "translucent");
            }
        }

        // Stone beside water: the stone's face toward the water is drawn
        // (seen through it), and the water's face toward the stone is not.
        let mut input = origin_input(&t);
        input.set(4, 4, 4, stone);
        input.set_liquid(5, 4, 4, water, 255);
        assert_eq!(quads(&mesh(&input)).len(), 6, "water hides no stone face");
        let liquid_qs = liquid(&input);
        assert_eq!(liquid_qs.len(), 5, "stone hides the water's face");
        assert!(with_normal(&liquid_qs, IVec3::NEG_X).is_empty());

        // A submerged stone has all six faces; the water body around it only
        // its outside.
        let mut input = origin_input(&t);
        for y in 3..=5 {
            for z in 3..=5 {
                for x in 3..=5 {
                    input.set_liquid(x, y, z, water, 255);
                }
            }
        }
        input.set(4, 4, 4, stone);
        assert_eq!(quads(&mesh(&input)).len(), 6, "submerged stone");
        assert_eq!(
            shape(&liquid(&input)),
            expect_shape(DIRS.iter().map(|&d| (d, (3.0, 3.0))).collect()),
            "the 3×3×3 water body's outside only"
        );

        // An opaque liquid is a full cell in the opaque pass whatever its
        // fill, with its emissive strength in alpha; nothing in the second.
        let lava_base = vertex_base_color(&t, lava);
        assert_eq!(lava_base[3], 2.0);
        for fill in [0u8, 40, 255] {
            let mut input = origin_input(&t);
            input.set_liquid(7, 7, 7, lava, fill);
            let qs = quads(&mesh(&input));
            assert_eq!(qs.len(), 6, "lava fill {fill}");
            assert!(qs.iter().all(|q| q.size() == (1.0, 1.0)));
            assert!(
                qs.iter()
                    .all(|q| q.colors.iter().all(|c| *c == shaded(lava_base, q.normal)))
            );
            assert!(
                GreedyMesher.mesh_liquid(&input).is_empty(),
                "lava fill {fill}"
            );
        }
        // Lava hides the water's face toward it, as a solid does.
        let mut input = origin_input(&t);
        input.set_liquid(7, 7, 7, lava, 0);
        input.set_liquid(8, 7, 7, water, 255);
        assert_eq!(liquid(&input).len(), 5);
        assert_eq!(quads(&mesh(&input)).len(), 6, "water hides no lava face");

        // The default ChunkMesher has an empty liquid pass; GreedyMesher,
        // the pipeline's mesher, fills it.
        struct OpaqueOnly;
        impl ChunkMesher for OpaqueOnly {
            fn mesh(&self, input: &MeshInput) -> MeshData {
                GreedyMesher.mesh(input)
            }
        }
        let mut input = origin_input(&t);
        input.set_liquid(4, 4, 4, water, 255);
        assert!(OpaqueOnly.mesh_liquid(&input).is_empty());
        assert!(!GreedyMesher.mesh_liquid(&input).is_empty());

        // Terrain without water has an empty second mesh.
        let terrain = plateau_terrain(&table());
        assert!(GreedyMesher.mesh_liquid(&terrain).is_empty(), "no water");
    }
}
