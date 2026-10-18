//! Cell, chunk, local and world-space coordinate conversions (SAD §5).
//!
//! A cell is 0.5 m on a side. A chunk is 32³ cells. Chunk and local
//! coordinates come from an arithmetic shift and a mask, so negative cells
//! land in the right chunk (cell -1 is local 31 of chunk -1).

use std::cmp::Ordering;

use glam::{IVec3, UVec3, Vec3};
use serde::{Deserialize, Serialize};

/// log2 of the chunk edge length in cells.
pub const CHUNK_SHIFT: u32 = 5;
/// Chunk edge length in cells.
pub const CHUNK_SIZE: i32 = 1 << CHUNK_SHIFT;
/// Mask that extracts the local coordinate from a cell coordinate.
pub const CHUNK_MASK: i32 = CHUNK_SIZE - 1;
/// Cells per chunk.
pub const CHUNK_VOLUME: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;
/// Edge length of one cell in meters.
pub const CELL_SIZE_M: f32 = 0.5;

/// Fixed total order on integer positions: y, then z, then x. This matches the
/// local linear index (`x + z*32 + y*1024`) and the shape iteration order.
fn cmp_yzx(a: IVec3, b: IVec3) -> Ordering {
    (a.y, a.z, a.x).cmp(&(b.y, b.z, b.x))
}

/// Integer cell index in the world.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellPos(pub IVec3);

/// Chunk index: `cell >> 5` per axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkPos(pub IVec3);

/// Position inside a chunk: `cell & 31` per axis, each component in `0..32`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalPos(pub UVec3);

impl Ord for CellPos {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_yzx(self.0, other.0)
    }
}

impl PartialOrd for CellPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ChunkPos {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_yzx(self.0, other.0)
    }
}

impl PartialOrd for ChunkPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl CellPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self(IVec3::new(x, y, z))
    }

    /// The chunk that contains this cell.
    pub fn chunk(self) -> ChunkPos {
        ChunkPos(self.0 >> CHUNK_SHIFT as i32)
    }

    /// This cell's position inside its chunk.
    pub fn local(self) -> LocalPos {
        LocalPos((self.0 & IVec3::splat(CHUNK_MASK)).as_uvec3())
    }

    /// Splits the cell into its chunk and local position.
    pub fn split(self) -> (ChunkPos, LocalPos) {
        (self.chunk(), self.local())
    }

    /// Rebuilds a cell from a chunk and a local position.
    pub fn from_chunk_local(chunk: ChunkPos, local: LocalPos) -> Self {
        Self(chunk.origin_cell().0 + local.0.as_ivec3())
    }

    /// The cell's minimum corner in meters. The cell occupies
    /// `[c*0.5, (c+1)*0.5)` on each axis.
    pub fn min_corner_m(self) -> Vec3 {
        self.0.as_vec3() * CELL_SIZE_M
    }

    /// The cell's center in meters.
    pub fn center_m(self) -> Vec3 {
        (self.0.as_vec3() + Vec3::splat(0.5)) * CELL_SIZE_M
    }

    /// The cell that contains a point given in meters (floor per axis).
    pub fn from_meters(p: Vec3) -> Self {
        Self((p / CELL_SIZE_M).floor().as_ivec3())
    }
}

impl ChunkPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self(IVec3::new(x, y, z))
    }

    /// The chunk's minimum cell (local 0,0,0).
    pub fn origin_cell(self) -> CellPos {
        CellPos(self.0 << CHUNK_SHIFT as i32)
    }
}

impl LocalPos {
    /// Builds a local position. Panics in debug if a component is not in `0..32`.
    pub const fn new(x: u32, y: u32, z: u32) -> Self {
        debug_assert!(x < CHUNK_SIZE as u32 && y < CHUNK_SIZE as u32 && z < CHUNK_SIZE as u32);
        Self(UVec3::new(x, y, z))
    }

    /// Linear index `x + z*32 + y*1024`.
    pub fn index(self) -> u16 {
        let s = CHUNK_SIZE as u32;
        (self.0.x + self.0.z * s + self.0.y * s * s) as u16
    }

    /// Inverse of [`LocalPos::index`]. Panics in debug if `i >= 32768`.
    pub fn from_index(i: u16) -> Self {
        debug_assert!((i as usize) < CHUNK_VOLUME);
        let i = u32::from(i);
        let m = CHUNK_MASK as u32;
        Self(UVec3::new(
            i & m,
            (i >> (2 * CHUNK_SHIFT)) & m,
            (i >> CHUNK_SHIFT) & m,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_compiles() {
        // The workspace builds and the core crate links glam at Bevy's minor.
        assert_eq!(glam::IVec3::ZERO + glam::IVec3::ONE, glam::IVec3::ONE);
    }

    #[test]
    fn cell_to_chunk_negative() {
        let cases = [
            (0, 0, 0),
            (31, 0, 31),
            (32, 1, 0),
            (63, 1, 31),
            (64, 2, 0),
            (-1, -1, 31),
            (-31, -1, 1),
            (-32, -1, 0),
            (-33, -2, 31),
            (-64, -2, 0),
            (-65, -3, 31),
            (i32::MIN, i32::MIN >> 5, 0),
            (i32::MAX, i32::MAX >> 5, 31),
        ];
        for (c, chunk, local) in cases {
            // Same value on every axis, then each axis alone.
            let cell = CellPos::new(c, c, c);
            assert_eq!(cell.chunk(), ChunkPos::new(chunk, chunk, chunk), "cell {c}");
            assert_eq!(cell.local(), LocalPos::new(local, local, local), "cell {c}");
            for axis in 0..3 {
                let mut v = IVec3::splat(5);
                v[axis] = c;
                let mut want_chunk = IVec3::ZERO;
                want_chunk[axis] = chunk;
                let mut want_local = UVec3::splat(5);
                want_local[axis] = local;
                assert_eq!(
                    CellPos(v).chunk(),
                    ChunkPos(want_chunk),
                    "axis {axis} cell {c}"
                );
                assert_eq!(
                    CellPos(v).local(),
                    LocalPos(want_local),
                    "axis {axis} cell {c}"
                );
            }
        }
        // Mixed signs across a chunk border.
        let cell = CellPos::new(-1, 32, -33);
        assert_eq!(
            cell.split(),
            (ChunkPos::new(-1, 1, -2), LocalPos::new(31, 0, 31))
        );
        assert_eq!(
            ChunkPos::new(-1, 1, -2).origin_cell(),
            CellPos::new(-32, 32, -64)
        );
    }

    #[test]
    fn local_index_roundtrip() {
        assert_eq!(LocalPos::new(0, 0, 0).index(), 0);
        assert_eq!(LocalPos::new(1, 0, 0).index(), 1);
        assert_eq!(LocalPos::new(0, 0, 1).index(), 32);
        assert_eq!(LocalPos::new(0, 1, 0).index(), 1024);
        assert_eq!(LocalPos::new(31, 31, 31).index(), (CHUNK_VOLUME - 1) as u16);

        // Every index maps to a distinct in-range local position and back.
        let mut seen = vec![false; CHUNK_VOLUME];
        for i in 0..CHUNK_VOLUME as u16 {
            let l = LocalPos::from_index(i);
            assert!(
                l.0.cmplt(UVec3::splat(CHUNK_SIZE as u32)).all(),
                "{i} -> {l:?}"
            );
            assert_eq!(l.index(), i);
            let flat = (l.0.x + l.0.z * 32 + l.0.y * 1024) as usize;
            assert!(!seen[flat]);
            seen[flat] = true;
        }

        // cell -> (chunk, local) -> cell across negative and positive borders.
        for x in -70..70 {
            for y in [-65, -33, -32, -1, 0, 31, 32, 64] {
                for z in [-64, -32, -31, -1, 0, 1, 31, 32, 33] {
                    let cell = CellPos::new(x, y, z);
                    let (chunk, local) = cell.split();
                    assert_eq!(CellPos::from_chunk_local(chunk, local), cell);
                    assert_eq!(
                        CellPos::from_chunk_local(chunk, LocalPos::from_index(local.index())),
                        cell
                    );
                }
            }
        }
    }

    #[test]
    fn world_meters_roundtrip() {
        assert_eq!(CellPos::new(0, 0, 0).min_corner_m(), Vec3::ZERO);
        assert_eq!(
            CellPos::new(1, 2, -3).min_corner_m(),
            Vec3::new(0.5, 1.0, -1.5)
        );
        assert_eq!(
            CellPos::new(-1, 0, 0).center_m(),
            Vec3::new(-0.25, 0.25, 0.25)
        );

        for x in -40..40 {
            for y in [-33, -1, 0, 1, 64, 127] {
                for z in [-64, -2, -1, 0, 31, 255] {
                    let cell = CellPos::new(x, y, z);
                    // The cell is half-open: min corner and center map back,
                    // the next cell's min corner does not.
                    assert_eq!(CellPos::from_meters(cell.min_corner_m()), cell);
                    assert_eq!(CellPos::from_meters(cell.center_m()), cell);
                    let just_below_max = cell.min_corner_m() + Vec3::splat(CELL_SIZE_M * 0.999);
                    assert_eq!(CellPos::from_meters(just_below_max), cell);
                    let max = CellPos::new(x + 1, y + 1, z + 1).min_corner_m();
                    assert_eq!(CellPos::from_meters(max), CellPos::new(x + 1, y + 1, z + 1));
                }
            }
        }

        // Negative meters floor, they do not truncate toward zero.
        assert_eq!(
            CellPos::from_meters(Vec3::new(-0.01, -0.5, -0.51)),
            CellPos::new(-1, -1, -2)
        );
    }

    #[test]
    fn ordering_is_y_z_x() {
        let mut v = vec![
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(0, 1, 0),
            ChunkPos::new(0, 0, 1),
            ChunkPos::new(-1, 0, 0),
        ];
        v.sort();
        assert_eq!(
            v,
            [
                ChunkPos::new(-1, 0, 0),
                ChunkPos::new(1, 0, 0),
                ChunkPos::new(0, 0, 1),
                ChunkPos::new(0, 1, 0)
            ]
        );
        assert!(CellPos::new(5, 0, 0) < CellPos::new(0, 0, 1));
    }
}
