//! Voxel raycast (SAD §11.5): Amanatides–Woo traversal over solid cells.
//!
//! This is the one float entry point of `rmc_world`. The ray is traversed in
//! f64 meters and the result is an integer cell plus an integer face normal.

use glam::{DVec3, I64Vec3, IVec3, Vec3};

use crate::coords::{CELL_SIZE_M, CellPos};
use crate::world::World;

/// The first solid cell a ray enters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub cell: CellPos,
    /// Unit axis vector pointing out of the face the ray entered through, so
    /// `cell + face_normal` is the empty cell in front of it. Zero when the
    /// ray starts inside the hit cell.
    pub face_normal: IVec3,
    /// Meters from `origin` to the entry point along the ray.
    pub distance: f32,
}

/// Casts a ray from `origin` (meters) along `dir` (any length) and returns
/// the first cell whose material phase is `Solid` or `Liquid` (see
/// [`World::is_solid_or_liquid`]; SAD-P2 §5 lets tools target liquids), entered within `max_dist` meters (inclusive).
///
/// - A ray that starts in a solid cell hits it at distance 0 with a zero
///   normal. Cells span `[c·0.5, (c+1)·0.5)`, as in [`CellPos::from_meters`].
/// - The ray is clipped to the world bounds first, so an infinite `max_dist`
///   or an origin far outside the world costs at most one walk across it.
/// - When the ray crosses an edge or corner exactly, it steps x, then y,
///   then z, so the result is the same on every machine.
/// - A zero or non-finite `dir`, a non-finite `origin`, or a negative or NaN
///   `max_dist` hits nothing.
pub fn raycast(world: &World, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit> {
    raycast_where(world, origin, dir, max_dist, World::is_solid_or_liquid)
}

/// [`raycast`], but hits only `Solid` cells ([`World::is_solid`]): the ray
/// passes through liquids, as the pickaxe's does.
pub fn raycast_solid(world: &World, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<Hit> {
    raycast_where(world, origin, dir, max_dist, World::is_solid)
}

/// [`raycast`] with the cells it stops at chosen by `hits`.
fn raycast_where(
    world: &World,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    hits: impl Fn(&World, CellPos) -> bool,
) -> Option<Hit> {
    let o = origin.as_dvec3();
    let d = dir.as_dvec3().try_normalize()?;
    let max_dist = f64::from(max_dist);
    if !o.is_finite() || max_dist.is_nan() || max_dist < 0.0 {
        return None;
    }
    let cell_m = f64::from(CELL_SIZE_M);
    let bounds = world.bounds();
    let min = bounds.min.0.as_i64vec3();
    let max = bounds.max.0.as_i64vec3();

    // Slab-clip the ray against the bounds box in meters. `entry_axis` is
    // the axis whose face the ray enters through, if it starts outside.
    let lo = bounds.min.0.as_dvec3() * cell_m;
    let hi = (bounds.max.0.as_dvec3() + DVec3::ONE) * cell_m;
    let (mut t_enter, mut t_end) = (0.0f64, max_dist);
    let mut entry_axis = None;
    for a in 0..3 {
        if d[a] == 0.0 {
            if o[a] < lo[a] || o[a] >= hi[a] {
                return None;
            }
            continue;
        }
        let t1 = (lo[a] - o[a]) / d[a];
        let t2 = (hi[a] - o[a]) / d[a];
        let (tn, tf) = (t1.min(t2), t1.max(t2));
        if tn > t_enter {
            t_enter = tn;
            entry_axis = Some(a);
        }
        t_end = t_end.min(tf);
    }
    if t_enter > t_end {
        return None;
    }

    let step = IVec3::new(sign(d.x), sign(d.y), sign(d.z));
    let mut cell = (o / cell_m).floor().as_i64vec3();
    let mut t = 0.0;
    let mut normal = IVec3::ZERO;
    if let Some(a) = entry_axis {
        // Start at the entry point, in the boundary cell on the entry axis.
        // The other axes are inside their slabs there, up to rounding.
        t = t_enter;
        cell = ((o + d * t_enter) / cell_m).floor().as_i64vec3();
        cell = cell.clamp(min, max);
        cell[a] = if step[a] > 0 { min[a] } else { max[a] };
        normal[a] = -step[a];
    }

    // Ray parameter (meters from `o`) of the next cell boundary per axis.
    let boundary = |cell: I64Vec3, a: usize| -> f64 {
        match step[a] {
            0 => f64::INFINITY,
            s => {
                let face = if s > 0 { cell[a] + 1 } else { cell[a] };
                (face as f64 * cell_m - o[a]) / d[a]
            }
        }
    };
    let mut t_max = DVec3::new(boundary(cell, 0), boundary(cell, 1), boundary(cell, 2));

    loop {
        if cell.cmpge(min).all() && cell.cmple(max).all() {
            let pos = CellPos(cell.as_ivec3());
            if hits(world, pos) {
                return Some(Hit {
                    cell: pos,
                    face_normal: normal,
                    distance: t as f32,
                });
            }
        }
        // Smallest boundary first; ties go x, then y, then z.
        let mut a = 0;
        for b in 1..3 {
            if t_max[b] < t_max[a] {
                a = b;
            }
        }
        t = t_max[a];
        if t > t_end {
            return None;
        }
        cell[a] += i64::from(step[a]);
        normal = IVec3::ZERO;
        normal[a] = -step[a];
        t_max[a] = boundary(cell, a);
    }
}

fn sign(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use crate::chunk::Chunk;
    use crate::coords::CellAabb;
    use crate::materials::{MaterialId, MaterialTable};

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn phase1() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(PHASE1_PATH).expect("assets/materials.ron loads"))
    }

    fn id(table: &MaterialTable, key: &str) -> MaterialId {
        table
            .id(key)
            .unwrap_or_else(|| panic!("material {key} in table"))
    }

    /// An all-air world over `bounds` with every chunk resident except those
    /// in `missing`, and `solids` set to stone.
    fn world_with(
        bounds: CellAabb,
        solids: &[CellPos],
        missing: &[crate::coords::ChunkPos],
    ) -> World {
        let table = phase1();
        let stone = id(&table, "stone");
        let mut w = World::new(table, bounds);
        for pos in bounds.chunks() {
            if missing.contains(&pos) {
                continue;
            }
            let mut chunk = Chunk::uniform(MaterialId::AIR);
            for c in solids.iter().filter(|c| c.chunk() == pos) {
                chunk.storage.set(c.local(), stone);
            }
            w.insert_chunk(pos, chunk).unwrap();
        }
        for c in solids {
            assert!(w.is_solid(*c), "fixture cell {c:?} is solid");
        }
        w
    }

    /// 2 × 1 × 1 chunks: cells 0..=63 × 0..=31 × 0..=31.
    fn two_chunks() -> CellAabb {
        CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31))
    }

    fn c(x: i32, y: i32, z: i32) -> CellPos {
        CellPos::new(x, y, z)
    }

    fn assert_hit(got: Option<Hit>, cell: CellPos, normal: IVec3, distance: f32) {
        let hit = got.unwrap_or_else(|| panic!("expected hit on {cell:?}, got none"));
        assert_eq!(hit.cell, cell, "hit cell");
        assert_eq!(hit.face_normal, normal, "face normal for {cell:?}");
        assert!(
            (hit.distance - distance).abs() < 1e-4,
            "distance to {cell:?}: got {}, want {distance}",
            hit.distance
        );
    }

    #[test]
    fn liquid_cells_are_hit() {
        // SAD-P2 §5: raycasts hit Liquid cells so tools can target water and
        // magma; collision (`is_solid`) still passes through them.
        let base = phase1();
        let mut defs: Vec<_> = base.iter().cloned().collect();
        let liquid = MaterialId(u16::try_from(defs.len()).unwrap());
        defs.push(crate::materials::MaterialDef {
            id: liquid,
            key: "test_liquid".into(),
            phase: crate::materials::Phase::Liquid,
            ..defs[0].clone()
        });
        let table = Arc::new(MaterialTable::from_defs(defs).unwrap());
        let bounds = two_chunks();
        let mut w = World::new(table, bounds);
        for pos in bounds.chunks() {
            w.insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let target = c(20, 16, 16);
        let (cpos, local) = target.split();
        let mut chunk = w.chunk(cpos).unwrap().clone();
        chunk.storage.set(local, liquid);
        w.insert_chunk(cpos, chunk).unwrap();
        assert!(!w.is_solid(target));
        assert!(w.is_solid_or_liquid(target));
        assert_hit(
            raycast(&w, c(16, 16, 16).center_m(), Vec3::X, 10.0),
            target,
            IVec3::NEG_X,
            1.75,
        );

        // `raycast_solid` passes through the liquid to the stone behind it.
        let behind = c(22, 16, 16);
        let (cpos, local) = behind.split();
        let mut chunk = w.chunk(cpos).unwrap().clone();
        chunk.storage.set(local, id(w.materials(), "stone"));
        w.insert_chunk(cpos, chunk).unwrap();
        assert_hit(
            raycast_solid(&w, c(16, 16, 16).center_m(), Vec3::X, 10.0),
            behind,
            IVec3::NEG_X,
            2.75,
        );
        assert_eq!(
            raycast_solid(&w, c(16, 16, 16).center_m(), Vec3::X, 2.5),
            None,
            "the liquid alone is not a hit"
        );
    }

    #[test]
    fn axis_aligned_hit() {
        // Origin in the centre of cell (16, 16, 16); one solid cell 5 cells
        // away along each of the six axis directions.
        let o = c(16, 16, 16);
        let origin = o.center_m();
        let dirs = [
            IVec3::X,
            IVec3::NEG_X,
            IVec3::Y,
            IVec3::NEG_Y,
            IVec3::Z,
            IVec3::NEG_Z,
        ];
        let solids: Vec<CellPos> = dirs.iter().map(|d| CellPos(o.0 + *d * 5)).collect();
        let w = world_with(two_chunks(), &solids, &[]);
        for (d, target) in dirs.iter().zip(&solids) {
            // 4.5 cells from the centre to the near face = 2.25 m.
            assert_hit(raycast(&w, origin, d.as_vec3(), 10.0), *target, -*d, 2.25);
            // Direction length does not matter.
            assert_hit(
                raycast(&w, origin, d.as_vec3() * 7.5, 10.0),
                *target,
                -*d,
                2.25,
            );
            assert_hit(
                raycast(&w, origin, d.as_vec3() * 1e-3, 10.0),
                *target,
                -*d,
                2.25,
            );
        }

        // A ray between the targets along a diagonal plane meets nothing.
        assert_eq!(raycast(&w, origin, Vec3::new(1.0, 1.0, 0.0), 10.0), None);

        // Origin off-centre in its cell: distance is measured from the origin.
        let off = origin + Vec3::new(0.2, -0.1, 0.05);
        assert_hit(
            raycast(&w, off, Vec3::X, 10.0),
            solids[0],
            IVec3::NEG_X,
            2.05,
        );
        assert_hit(
            raycast(&w, off, Vec3::NEG_Y, 10.0),
            solids[3],
            IVec3::Y,
            2.15,
        );

        // Origin exactly on a cell boundary: the cell is the one floor gives
        // (cells span [c·0.5, (c+1)·0.5)).
        // x = 10.0 m is the min face of cell 20 and the max face of cell 19.
        let on_face = Vec3::new(10.0, origin.y, origin.z);
        assert_hit(
            raycast(&w, on_face, Vec3::X, 10.0),
            solids[0],
            IVec3::NEG_X,
            0.5,
        );
        // Far face of cell 11 is at 6.0 m.
        assert_hit(
            raycast(&w, on_face, Vec3::NEG_X, 10.0),
            solids[1],
            IVec3::X,
            4.0,
        );
    }

    #[test]
    fn diagonal_hit_face_normal() {
        let bounds = two_chunks();
        let origin = c(2, 2, 2).center_m(); // (1.25, 1.25, 1.25)

        // Floor of solid cells at y = 0 (top face at y = 0.5 m). Going
        // (1, -0.5, 0): y drops 0.75 m after 1.5 m in x, at x = 2.75 m (cell 5).
        let floor: Vec<CellPos> = (0..64)
            .flat_map(|x| (0..32).map(move |z| c(x, 0, z)))
            .collect();
        let w = world_with(bounds, &floor, &[]);
        let dir = Vec3::new(1.0, -0.5, 0.0);
        assert_hit(
            raycast(&w, origin, dir, 10.0),
            c(5, 0, 2),
            IVec3::Y,
            1.5 * 1.25f32.sqrt(),
        );

        // Wall of solid cells at x = 8 (near face at x = 4.0 m). Going
        // (1, 0.3, 0.2): reaches x = 4 after 2.75 m in x, at y = 2.075, z = 1.8.
        let wall: Vec<CellPos> = (0..32)
            .flat_map(|y| (0..32).map(move |z| c(8, y, z)))
            .collect();
        let w = world_with(bounds, &wall, &[]);
        let dir = Vec3::new(1.0, 0.3, 0.2);
        assert_hit(
            raycast(&w, origin, dir, 10.0),
            c(8, 4, 3),
            IVec3::NEG_X,
            2.75 * dir.length(),
        );
        // Same wall from the other side, going (-1, -0.2, 0.3) from cell 14.
        let from = c(14, 10, 10).center_m(); // (7.25, 5.25, 5.25)
        let dir = Vec3::new(-1.0, -0.2, 0.3);
        // Far face of cell 8 at x = 4.5 m: 2.75 m in x, y = 4.7, z = 6.075.
        assert_hit(
            raycast(&w, from, dir, 10.0),
            c(8, 9, 12),
            IVec3::X,
            2.75 * dir.length(),
        );

        // Ceiling plane z = 20 approached steeply in z: normal is -z.
        let plane: Vec<CellPos> = (0..64)
            .flat_map(|x| (0..32).map(move |y| c(x, y, 20)))
            .collect();
        let w = world_with(bounds, &plane, &[]);
        let dir = Vec3::new(0.1, 0.25, 1.0);
        // z from 1.25 to 10.0 m: 8.75 m, x = 2.125 (cell 4), y = 3.4375 (cell 6).
        assert_hit(
            raycast(&w, origin, dir, 20.0),
            c(4, 6, 20),
            IVec3::NEG_Z,
            8.75 * dir.length(),
        );

        // Exact edge crossing: (1, 1, 0) from a cell centre passes through
        // the shared edge of four cells. Ties step x first, then y, then z,
        // so the cell beside the edge in +x is visited and blocks the ray.
        let w = world_with(bounds, &[c(3, 2, 2)], &[]);
        assert_hit(
            raycast(&w, origin, Vec3::new(1.0, 1.0, 0.0), 10.0),
            c(3, 2, 2),
            IVec3::NEG_X,
            0.25 * 2f32.sqrt(),
        );
        let w = world_with(bounds, &[c(3, 3, 2)], &[]);
        assert_hit(
            raycast(&w, origin, Vec3::new(1.0, 1.0, 0.0), 10.0),
            c(3, 3, 2),
            IVec3::NEG_Y,
            0.25 * 2f32.sqrt(),
        );

        // Random rays against a sparse random world, checked against a brute
        // force slab test over every solid cell.
        reference_comparison();
    }

    /// Fixed-seed LCG so the test is reproducible without proptest.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }

        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }

        /// Uniform in [lo, hi).
        fn f(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * (self.next() as f32 / (1u64 << 31) as f32)
        }
    }

    /// Brute force: entry distance and entry normal of the ray into the
    /// closed box of `cell`, in f64.
    fn slab(origin: Vec3, dir: Vec3, cell: CellPos) -> Option<(f64, f64, IVec3)> {
        let o = origin.as_dvec3();
        let d = dir.as_dvec3().normalize();
        let lo = cell.0.as_dvec3() * f64::from(CELL_SIZE_M);
        let hi = lo + f64::from(CELL_SIZE_M);
        let (mut t_in, mut t_out) = (f64::NEG_INFINITY, f64::INFINITY);
        let mut normal = IVec3::ZERO;
        for a in 0..3 {
            if d[a] == 0.0 {
                if o[a] < lo[a] || o[a] > hi[a] {
                    return None;
                }
                continue;
            }
            let t1 = (lo[a] - o[a]) / d[a];
            let t2 = (hi[a] - o[a]) / d[a];
            let (tn, tf) = (t1.min(t2), t1.max(t2));
            if tn > t_in {
                t_in = tn;
                normal = IVec3::ZERO;
                normal[a] = if d[a] > 0.0 { -1 } else { 1 };
            }
            t_out = t_out.min(tf);
        }
        (t_in <= t_out && t_out >= 0.0).then_some((t_in, t_out, normal))
    }

    fn reference_comparison() {
        let bounds = CellAabb::new(c(-16, -8, -16), c(47, 23, 15));
        let mut rng = Lcg(0x5eed_ca57);
        let mut checked = 0;
        for _world in 0..20 {
            let solids: BTreeSet<CellPos> = (0..150)
                .map(|_| {
                    c(
                        -16 + rng.below(64) as i32,
                        -8 + rng.below(32) as i32,
                        -16 + rng.below(32) as i32,
                    )
                })
                .collect();
            let solids: Vec<CellPos> = solids.into_iter().collect();
            let w = world_with(bounds, &solids, &[]);
            for _ray in 0..200 {
                let origin = Vec3::new(rng.f(-8.0, 24.0), rng.f(-4.0, 12.0), rng.f(-8.0, 8.0));
                let dir = Vec3::new(rng.f(-1.0, 1.0), rng.f(-1.0, 1.0), rng.f(-1.0, 1.0));
                if dir.length() < 0.05 || w.is_solid(CellPos::from_meters(origin)) {
                    continue;
                }
                let max_dist = rng.f(0.5, 30.0);
                // Nearest box entered at t >= 0 within reach; skip rays that
                // graze a box or tie between two boxes, where the half-open
                // cell convention decides.
                let mut hits: Vec<(f64, f64, IVec3, CellPos)> = solids
                    .iter()
                    .filter_map(|s| slab(origin, dir, *s).map(|(a, b, n)| (a, b, n, *s)))
                    .collect();
                hits.sort_by(|a, b| a.0.total_cmp(&b.0));
                if hits.iter().any(|h| h.1 - h.0 < 1e-6) {
                    continue;
                }
                if hits.len() >= 2 && hits[1].0 - hits[0].0 < 1e-6 {
                    continue;
                }
                let want = hits.first().filter(|h| h.0 <= f64::from(max_dist));
                if want.is_some_and(|h| (h.0 - f64::from(max_dist)).abs() < 1e-5) {
                    continue;
                }
                let got = raycast(&w, origin, dir, max_dist);
                match want {
                    None => assert_eq!(got, None, "ray {origin:?} {dir:?} {max_dist}"),
                    Some(&(t, _, n, cell)) => {
                        let hit = got.unwrap_or_else(|| {
                            panic!("ray {origin:?} {dir:?} {max_dist}: want {cell:?}, got none")
                        });
                        assert_eq!(
                            (hit.cell, hit.face_normal),
                            (cell, n),
                            "ray {origin:?} {dir:?}"
                        );
                        assert!((f64::from(hit.distance) - t).abs() < 1e-4, "distance");
                    }
                }
                checked += 1;
            }
        }
        assert!(checked > 3_000, "only {checked} rays checked");
    }

    #[test]
    fn crosses_chunk_border() {
        // Solid cell just past the x = 32 chunk border.
        let w = world_with(two_chunks(), &[c(33, 4, 4), c(30, 9, 9)], &[]);
        assert_hit(
            raycast(&w, c(28, 4, 4).center_m(), Vec3::X, 10.0),
            c(33, 4, 4),
            IVec3::NEG_X,
            2.25,
        );
        // Back across the border from chunk 1 into chunk 0.
        assert_hit(
            raycast(&w, c(35, 9, 9).center_m(), Vec3::NEG_X, 10.0),
            c(30, 9, 9),
            IVec3::X,
            2.25,
        );
        // Diagonally across the border: (1, 0, 0.5) from (30.5, 4.5) cells.
        let w = world_with(two_chunks(), &[c(34, 4, 6)], &[]);
        let dir = Vec3::new(1.0, 0.0, 0.5);
        // Near face x = 17.0 m after 1.75 m in x: z = 2.25 + 0.875 = 3.125 (cell 6).
        assert_hit(
            raycast(&w, c(30, 4, 4).center_m(), dir, 10.0),
            c(34, 4, 6),
            IVec3::NEG_X,
            1.75 * dir.length(),
        );

        // Negative coordinates: chunks -1..=2 in x. Chunk x = 1 is not
        // resident and counts as empty; the ray crosses it and hits in x = 2.
        let bounds = CellAabb::new(c(-32, 0, 0), c(95, 31, 31));
        let missing = [crate::coords::ChunkPos::new(1, 0, 0)];
        let w = world_with(bounds, &[c(-2, 7, 7), c(70, 7, 7)], &missing);
        // Cell 1 → cell -2 across the 0 / -1 chunk border.
        assert_hit(
            raycast(&w, c(1, 7, 7).center_m(), Vec3::NEG_X, 10.0),
            c(-2, 7, 7),
            IVec3::X,
            1.25,
        );
        // Cell 20 → cell 70 through the missing chunk (cells 32..=63).
        assert_hit(
            raycast(&w, c(20, 7, 7).center_m(), Vec3::X, 100.0),
            c(70, 7, 7),
            IVec3::NEG_X,
            24.75,
        );
        // Going up y across the chunk border at y = 32 leaves the world.
        assert_eq!(raycast(&w, c(5, 30, 5).center_m(), Vec3::Y, 100.0), None);
    }

    #[test]
    fn max_distance_miss() {
        let target = c(21, 16, 16);
        let w = world_with(two_chunks(), &[target], &[]);
        let origin = c(16, 16, 16).center_m();
        // Near face is 2.25 m away; reach is inclusive.
        assert_eq!(raycast(&w, origin, Vec3::X, 2.24), None);
        assert_hit(
            raycast(&w, origin, Vec3::X, 2.25),
            target,
            IVec3::NEG_X,
            2.25,
        );
        assert_hit(
            raycast(&w, origin, Vec3::X, 2.26),
            target,
            IVec3::NEG_X,
            2.25,
        );
        assert_eq!(raycast(&w, origin, Vec3::X, 0.0), None);

        // Empty directions run to the bounds and miss, even with no limit.
        assert_eq!(raycast(&w, origin, Vec3::NEG_X, 100.0), None);
        assert_eq!(raycast(&w, origin, Vec3::Y, f32::INFINITY), None);
        assert_eq!(
            raycast(&w, origin, Vec3::new(1.0, 0.2, 0.0), f32::MAX),
            None
        );

        // Invalid input misses.
        assert_eq!(raycast(&w, origin, Vec3::ZERO, 10.0), None);
        assert_eq!(
            raycast(&w, origin, Vec3::new(f32::NAN, 0.0, 0.0), 10.0),
            None
        );
        assert_eq!(
            raycast(&w, origin, Vec3::new(f32::INFINITY, 0.0, 0.0), 10.0),
            None
        );
        assert_eq!(raycast(&w, Vec3::splat(f32::NAN), Vec3::X, 10.0), None);
        assert_eq!(raycast(&w, origin, Vec3::X, -1.0), None);
        assert_eq!(raycast(&w, origin, Vec3::X, f32::NAN), None);

        // From outside the bounds: entering through the world's min face.
        // Origin 3 m before x = 0; target near face at x = 10.5 m.
        let outside = Vec3::new(-3.0, origin.y, origin.z);
        assert_hit(
            raycast(&w, outside, Vec3::X, 20.0),
            target,
            IVec3::NEG_X,
            13.5,
        );
        assert_eq!(raycast(&w, outside, Vec3::X, 13.4), None);
        assert_eq!(raycast(&w, outside, Vec3::NEG_X, f32::INFINITY), None);
        // Parallel to a face but outside the slab: never enters.
        assert_eq!(
            raycast(&w, Vec3::new(5.0, -1.0, 5.0), Vec3::X, f32::INFINITY),
            None
        );
        // Far away (beyond i32 cells) and pointed at the world: no long walk.
        let far = Vec3::new(-1.0e12, origin.y, origin.z);
        let hit = raycast(&w, far, Vec3::X, f32::INFINITY).expect("far ray hits");
        assert_eq!((hit.cell, hit.face_normal), (target, IVec3::NEG_X));
        assert_eq!(raycast(&w, far, Vec3::X, 1.0e6), None);

        // A cell outside the bounds is never hit, even if a ray starts in it.
        assert_eq!(
            raycast(&w, Vec3::new(-0.25, 1.0, 1.0), Vec3::NEG_X, 10.0),
            None
        );

        // Entering exactly on the world's max face going inward: the first
        // cell is the last in-bounds one, with a +x normal.
        let w = world_with(two_chunks(), &[c(63, 3, 3)], &[]);
        let on_max = Vec3::new(32.0, c(0, 3, 3).center_m().y, c(0, 3, 3).center_m().z);
        assert_hit(
            raycast(&w, on_max, Vec3::NEG_X, 1.0),
            c(63, 3, 3),
            IVec3::X,
            0.0,
        );
    }

    #[test]
    fn start_inside_solid() {
        let w = world_with(two_chunks(), &[c(10, 10, 10), c(11, 10, 10)], &[]);
        let inside = c(10, 10, 10).center_m();
        for dir in [Vec3::X, Vec3::NEG_X, Vec3::new(0.3, -0.7, 0.2)] {
            assert_hit(
                raycast(&w, inside, dir, 3.0),
                c(10, 10, 10),
                IVec3::ZERO,
                0.0,
            );
            // Zero reach still reports the cell the ray starts in.
            assert_hit(
                raycast(&w, inside, dir, 0.0),
                c(10, 10, 10),
                IVec3::ZERO,
                0.0,
            );
        }
        // A corner of the cell (its minimum corner belongs to it).
        let corner = c(10, 10, 10).min_corner_m();
        assert_hit(
            raycast(&w, corner, Vec3::NEG_Y, 3.0),
            c(10, 10, 10),
            IVec3::ZERO,
            0.0,
        );
        // The max face belongs to the next cell: starting there in +x hits
        // the second solid cell, also from inside.
        let max_face = Vec3::new(5.5, inside.y, inside.z);
        assert_hit(
            raycast(&w, max_face, Vec3::X, 3.0),
            c(11, 10, 10),
            IVec3::ZERO,
            0.0,
        );
        // Just outside the solid pair going away misses; going back hits.
        let after = c(12, 10, 10).center_m();
        assert_eq!(raycast(&w, after, Vec3::X, 30.0), None);
        assert_hit(
            raycast(&w, after, Vec3::NEG_X, 3.0),
            c(11, 10, 10),
            IVec3::X,
            0.25,
        );
    }
}
