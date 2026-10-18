//! Edit shapes and their fixed cell iteration order (SAD §8.1).
//!
//! Every shape yields its cells in ascending (y, z, x) order, which is the
//! `Ord` of [`CellPos`]. Edits, deltas and hashes depend on that order.

use std::iter::FusedIterator;

use glam::IVec3;
use serde::{Deserialize, Serialize};

use crate::coords::{CellAabb, CellPos};

/// The cells an edit command touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Shape {
    /// One cell.
    Cell(CellPos),
    /// Every cell with `min <= c <= max` on each axis (inclusive). Empty if
    /// `min` exceeds `max` on any axis.
    Box { min: CellPos, max: CellPos },
    /// Every cell with `dx² + dy² + dz² <= radius²` (integer test).
    Sphere { center: CellPos, radius: u16 },
}

impl Shape {
    /// The shape's cells in ascending (y, z, x) order.
    ///
    /// Cells whose coordinates would leave the `i32` range are skipped; they
    /// are outside every world. So `cells().count() <= cell_count()`, with
    /// equality whenever [`Shape::aabb`] fits in `i32` unclamped.
    pub fn cells(&self) -> ShapeCells {
        ShapeCells::new(Rows::from(*self))
    }

    /// Number of cells in the shape, computed without iterating it.
    /// Saturates at `u64::MAX` (only reachable by enormous boxes).
    ///
    /// A sphere costs O(radius²) integer square roots; to bound the count of
    /// an untrusted shape use [`Shape::cell_count_exceeds`].
    pub fn cell_count(&self) -> u64 {
        match *self {
            Shape::Cell(_) => 1,
            Shape::Box { min, max } => (0..3).fold(1u64, |n, axis| {
                let span = i64::from(max.0[axis]) - i64::from(min.0[axis]) + 1;
                n.saturating_mul(span.max(0) as u64)
            }),
            Shape::Sphere { radius, .. } => sphere_row_lengths(radius).sum(),
        }
    }

    /// Whether the shape has more than `limit` cells. Unlike
    /// [`Shape::cell_count`], a sphere stops summing rows once it is past the
    /// limit; every row has at least one cell, so that costs O(limit) at any
    /// radius. Edit validation uses this on untrusted shapes.
    pub fn cell_count_exceeds(&self, limit: u64) -> bool {
        match *self {
            Shape::Cell(_) | Shape::Box { .. } => self.cell_count() > limit,
            Shape::Sphere { radius, .. } => {
                let mut n = 0u64;
                sphere_row_lengths(radius).any(|len| {
                    n += len;
                    n > limit
                })
            }
        }
    }

    /// The smallest box holding every cell of the shape, or `None` if the
    /// shape is empty. A sphere's box is clamped to the `i32` range.
    pub fn aabb(&self) -> Option<CellAabb> {
        match *self {
            Shape::Cell(c) => Some(CellAabb::new(c, c)),
            Shape::Box { min, max } => min.0.cmple(max.0).all().then(|| CellAabb::new(min, max)),
            Shape::Sphere { center, radius } => {
                let r = i64::from(radius);
                let lo = center.0.to_array().map(|c| clamp_i32(i64::from(c) - r));
                let hi = center.0.to_array().map(|c| clamp_i32(i64::from(c) + r));
                Some(CellAabb::new(
                    CellPos(IVec3::from_array(lo)),
                    CellPos(IVec3::from_array(hi)),
                ))
            }
        }
    }
}

/// The length of each x-run of a sphere of `radius`, one per (y, z) row in
/// (y, z) order. Every length is at least 1.
fn sphere_row_lengths(radius: u16) -> impl Iterator<Item = u64> {
    let r = i64::from(radius);
    let r2 = r * r;
    (-r..=r).flat_map(move |dy| {
        let rem_y = r2 - dy * dy;
        let sz = isqrt(rem_y);
        (-sz..=sz).map(move |dz| 2 * isqrt(rem_y - dz * dz) as u64 + 1)
    })
}

fn isqrt(v: i64) -> i64 {
    debug_assert!(v >= 0);
    (v as u64).isqrt() as i64
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Inclusive span `[center - half, center + half]`, clamped to the `i32` range.
fn span(center: i32, half: i64) -> (i64, i64) {
    let c = i64::from(center);
    (
        i64::from(clamp_i32(c - half)),
        i64::from(clamp_i32(c + half)),
    )
}

/// A shape as a set of x-runs, one per (y, z) row. Coordinates are `i64` so
/// stepping past `i32::MAX` cannot overflow.
#[derive(Clone, Copy, Debug)]
enum Rows {
    Box { min: [i64; 3], max: [i64; 3] },
    Sphere { center: CellPos, r2: i64 },
}

impl From<Shape> for Rows {
    fn from(shape: Shape) -> Self {
        let wide = |c: CellPos| c.0.to_array().map(i64::from);
        match shape {
            Shape::Cell(c) => Rows::Box {
                min: wide(c),
                max: wide(c),
            },
            Shape::Box { min, max } => Rows::Box {
                min: wide(min),
                max: wide(max),
            },
            Shape::Sphere { center, radius } => Rows::Sphere {
                center,
                r2: i64::from(radius) * i64::from(radius),
            },
        }
    }
}

impl Rows {
    fn is_empty(&self) -> bool {
        match self {
            Rows::Box { min, max } => (0..3).any(|a| min[a] > max[a]),
            Rows::Sphere { .. } => false,
        }
    }

    fn y_range(&self) -> (i64, i64) {
        match *self {
            Rows::Box { min, max } => (min[1], max[1]),
            Rows::Sphere { center, r2 } => span(center.0.y, isqrt(r2)),
        }
    }

    /// Requires `y` inside `y_range()`.
    fn z_range(&self, y: i64) -> (i64, i64) {
        match *self {
            Rows::Box { min, max } => (min[2], max[2]),
            Rows::Sphere { center, r2 } => {
                let dy = y - i64::from(center.0.y);
                span(center.0.z, isqrt(r2 - dy * dy))
            }
        }
    }

    /// Requires `y` inside `y_range()` and `z` inside `z_range(y)`.
    fn x_range(&self, y: i64, z: i64) -> (i64, i64) {
        match *self {
            Rows::Box { min, max } => (min[0], max[0]),
            Rows::Sphere { center, r2 } => {
                let dy = y - i64::from(center.0.y);
                let dz = z - i64::from(center.0.z);
                span(center.0.x, isqrt(r2 - dy * dy - dz * dz))
            }
        }
    }
}

/// Iterator over a shape's cells in ascending (y, z, x) order.
#[derive(Clone, Debug)]
pub struct ShapeCells {
    rows: Rows,
    done: bool,
    y: i64,
    y_hi: i64,
    z: i64,
    z_hi: i64,
    x: i64,
    x_hi: i64,
}

impl ShapeCells {
    fn new(rows: Rows) -> Self {
        let (y, y_hi) = rows.y_range();
        let mut it = Self {
            rows,
            done: rows.is_empty() || y > y_hi,
            y,
            y_hi,
            z: 0,
            z_hi: -1,
            x: 0,
            x_hi: -1,
        };
        if !it.done {
            (it.z, it.z_hi) = rows.z_range(y);
            (it.x, it.x_hi) = rows.x_range(y, it.z);
        }
        it
    }

    /// Moves to the next (y, z) row. Returns false when the shape is exhausted.
    /// Every row of a non-empty shape has at least one cell, so this never
    /// scans empty rows.
    fn next_row(&mut self) -> bool {
        if self.z < self.z_hi {
            self.z += 1;
        } else if self.y < self.y_hi {
            self.y += 1;
            (self.z, self.z_hi) = self.rows.z_range(self.y);
        } else {
            self.done = true;
            return false;
        }
        (self.x, self.x_hi) = self.rows.x_range(self.y, self.z);
        true
    }
}

impl Iterator for ShapeCells {
    type Item = CellPos;

    fn next(&mut self) -> Option<CellPos> {
        if self.done {
            return None;
        }
        if self.x > self.x_hi && !self.next_row() {
            return None;
        }
        // Every coordinate here came from an i32 or was clamped to the i32 range.
        let cell = CellPos::new(self.x as i32, self.y as i32, self.z as i32);
        self.x += 1;
        Some(cell)
    }
}

impl FusedIterator for ShapeCells {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use glam::IVec3;

    use super::*;

    /// Lattice points with dx² + dy² + dz² <= r², r = 0..=8 (OEIS A000605).
    const SPHERE_COUNTS: [u64; 9] = [1, 7, 33, 123, 257, 515, 925, 1419, 2109];

    fn sphere(center: CellPos, radius: u16) -> Shape {
        Shape::Sphere { center, radius }
    }

    fn offsets(shape: &Shape, center: CellPos) -> BTreeSet<CellPos> {
        shape.cells().map(|c| CellPos(c.0 - center.0)).collect()
    }

    #[test]
    fn sphere_counts_r0_to_8() {
        let centers = [
            CellPos::new(0, 0, 0),
            CellPos::new(31, 32, -1),
            CellPos::new(-100, 7, 255),
        ];
        for center in centers {
            for r in 0..=8u16 {
                let s = sphere(center, r);
                let cells: Vec<CellPos> = s.cells().collect();
                let want = SPHERE_COUNTS[r as usize];
                assert_eq!(cells.len() as u64, want, "r={r} center={center:?}");
                assert_eq!(s.cell_count(), want, "r={r} center={center:?}");

                // Exactly the cells of the bounding cube that pass the test.
                let ri = i32::from(r);
                let mut brute = Vec::new();
                for y in -ri..=ri {
                    for z in -ri..=ri {
                        for x in -ri..=ri {
                            if x * x + y * y + z * z <= ri * ri {
                                brute.push(CellPos(center.0 + IVec3::new(x, y, z)));
                            }
                        }
                    }
                }
                assert_eq!(cells, brute, "r={r} center={center:?}");

                let lo = CellPos(center.0 - IVec3::splat(ri));
                let hi = CellPos(center.0 + IVec3::splat(ri));
                assert_eq!(s.aabb(), Some(CellAabb::new(lo, hi)), "r={r}");
            }
        }

        // At the i32 edge the unrepresentable cell is skipped; the count and
        // box describe the ideal sphere, clamped.
        let edge = sphere(CellPos::new(i32::MAX, 0, i32::MIN), 1);
        let (mx, mn) = (i32::MAX, i32::MIN);
        assert_eq!(
            edge.cells().collect::<Vec<_>>(),
            [
                CellPos::new(mx, -1, mn),
                CellPos::new(mx - 1, 0, mn),
                CellPos::new(mx, 0, mn),
                CellPos::new(mx, 0, mn + 1),
                CellPos::new(mx, 1, mn),
            ]
        );
        assert_eq!(edge.cell_count(), 7);
        assert_eq!(
            edge.aabb(),
            Some(CellAabb::new(
                CellPos::new(i32::MAX - 1, -1, i32::MIN),
                CellPos::new(i32::MAX, 1, i32::MIN + 1)
            ))
        );
    }

    #[test]
    fn sphere_is_symmetric() {
        let center = CellPos::new(-33, 64, 31);
        for r in 0..=8u16 {
            let set = offsets(&sphere(center, r), center);
            // Mirror on each axis and every axis permutation map the set to itself.
            let maps: [fn(IVec3) -> IVec3; 8] = [
                |v| IVec3::new(-v.x, v.y, v.z),
                |v| IVec3::new(v.x, -v.y, v.z),
                |v| IVec3::new(v.x, v.y, -v.z),
                |v| IVec3::new(v.y, v.x, v.z),
                |v| IVec3::new(v.z, v.y, v.x),
                |v| IVec3::new(v.x, v.z, v.y),
                |v| IVec3::new(v.y, v.z, v.x),
                |v| IVec3::new(v.z, v.x, v.y),
            ];
            for (i, m) in maps.iter().enumerate() {
                let mapped: BTreeSet<CellPos> = set.iter().map(|c| CellPos(m(c.0))).collect();
                assert_eq!(mapped, set, "r={r} map {i}");
            }
            // The six axis tips are in, one step past them is out.
            let ri = i32::from(r);
            for axis in 0..3 {
                for sign in [-1, 1] {
                    let mut v = IVec3::ZERO;
                    v[axis] = sign * ri;
                    assert!(set.contains(&CellPos(v)), "r={r} tip {v}");
                    v[axis] = sign * (ri + 1);
                    assert!(!set.contains(&CellPos(v)), "r={r} past tip {v}");
                }
            }
        }
        // Spheres at different centers are translates of each other.
        for r in [0u16, 3, 8] {
            let a = offsets(&sphere(CellPos::new(0, 0, 0), r), CellPos::new(0, 0, 0));
            let c = CellPos::new(-1000, -31, 77);
            assert_eq!(offsets(&sphere(c, r), c), a, "r={r}");
        }
    }

    #[test]
    fn box_is_inclusive() {
        let one = Shape::Box {
            min: CellPos::new(4, -2, 9),
            max: CellPos::new(4, -2, 9),
        };
        assert_eq!(one.cells().collect::<Vec<_>>(), [CellPos::new(4, -2, 9)]);
        assert_eq!(one.cell_count(), 1);

        let min = CellPos::new(-1, 30, 31);
        let max = CellPos::new(0, 32, 34);
        let b = Shape::Box { min, max };
        let cells: Vec<CellPos> = b.cells().collect();
        assert_eq!(cells.len(), 2 * 3 * 4);
        assert_eq!(b.cell_count(), 24);
        assert_eq!(cells.first(), Some(&min));
        assert_eq!(cells.last(), Some(&max));
        let aabb = CellAabb::new(min, max);
        assert!(cells.iter().all(|&c| aabb.contains(c)));
        assert_eq!(cells.iter().collect::<BTreeSet<_>>().len(), 24);
        assert_eq!(b.aabb(), Some(aabb));

        // min > max on any axis is empty.
        for axis in 0..3 {
            let mut hi = IVec3::splat(5);
            hi[axis] = -1;
            let e = Shape::Box {
                min: CellPos::new(0, 0, 0),
                max: CellPos(hi),
            };
            assert_eq!(e.cells().count(), 0, "axis {axis}");
            assert_eq!(e.cell_count(), 0, "axis {axis}");
            assert_eq!(e.aabb(), None, "axis {axis}");
        }

        // Boxes on the i32 edges iterate without overflow.
        let edge = Shape::Box {
            min: CellPos::new(i32::MAX - 1, i32::MIN, i32::MAX),
            max: CellPos::new(i32::MAX, i32::MIN + 1, i32::MAX),
        };
        assert_eq!(edge.cells().count(), 4);
        assert_eq!(edge.cell_count(), 4);

        // A huge box counts without iterating (and does not overflow).
        let huge = Shape::Box {
            min: CellPos::new(i32::MIN, i32::MIN, i32::MIN),
            max: CellPos::new(i32::MAX, i32::MAX, i32::MAX),
        };
        assert_eq!(huge.cell_count(), u64::MAX);
        assert_eq!(huge.cells().take(3).count(), 3);

        let cell = Shape::Cell(CellPos::new(-5, 0, 7));
        assert_eq!(cell.cells().collect::<Vec<_>>(), [CellPos::new(-5, 0, 7)]);
        assert_eq!(cell.cell_count(), 1);
        assert_eq!(
            cell.aabb(),
            Some(CellAabb::new(
                CellPos::new(-5, 0, 7),
                CellPos::new(-5, 0, 7)
            ))
        );
    }

    #[test]
    fn iteration_order_is_y_z_x() {
        let b = Shape::Box {
            min: CellPos::new(0, 0, 0),
            max: CellPos::new(1, 1, 1),
        };
        assert_eq!(
            b.cells().collect::<Vec<_>>(),
            [
                CellPos::new(0, 0, 0),
                CellPos::new(1, 0, 0),
                CellPos::new(0, 0, 1),
                CellPos::new(1, 0, 1),
                CellPos::new(0, 1, 0),
                CellPos::new(1, 1, 0),
                CellPos::new(0, 1, 1),
                CellPos::new(1, 1, 1),
            ]
        );

        let r1 = sphere(CellPos::new(0, 0, 0), 1);
        assert_eq!(
            r1.cells().collect::<Vec<_>>(),
            [
                CellPos::new(0, -1, 0),
                CellPos::new(0, 0, -1),
                CellPos::new(-1, 0, 0),
                CellPos::new(0, 0, 0),
                CellPos::new(1, 0, 0),
                CellPos::new(0, 0, 1),
                CellPos::new(0, 1, 0),
            ]
        );

        // Strictly ascending in CellPos order (y, z, x) for larger shapes too.
        let shapes = [
            sphere(CellPos::new(-7, 3, 40), 8),
            Shape::Box {
                min: CellPos::new(-3, -2, 30),
                max: CellPos::new(3, 2, 33),
            },
        ];
        for s in shapes {
            let cells: Vec<CellPos> = s.cells().collect();
            assert!(cells.windows(2).all(|w| w[0] < w[1]), "{s:?}");
        }

        // The iterator is fused: it stays exhausted.
        let mut it = Shape::Cell(CellPos::new(1, 2, 3)).cells();
        assert_eq!(it.next(), Some(CellPos::new(1, 2, 3)));
        assert_eq!(it.next(), None);
        assert_eq!(it.next(), None);
    }
}
