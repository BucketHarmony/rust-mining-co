//! Chunk generation (SAD §9): surface and strata so far.
//!
//! Steps run over a 34³ [`Volume`] (the chunk plus a 1-cell border) so later
//! steps can read neighbors; only the inner 32³ is written to the chunk.

use rmc_world::chunk::{Chunk, ChunkStorage};
use rmc_world::coords::{CHUNK_SIZE, CHUNK_VOLUME, ChunkPos, LocalPos};
use rmc_world::materials::{MaterialId, MaterialTable};

use crate::noise::{LAYER_SOIL, LAYER_STRATA, LAYER_SURFACE, NoiseLayer};
use crate::params::{GenError, GenParams};

/// Tag every soil layer material must carry.
pub const SOIL_TAG: &str = "soil";
/// Tag every stratum material must carry.
pub const STONE_TAG: &str = "stone";

/// Edge length of the generation volume: a chunk plus a 1-cell border.
pub const VOLUME_SIZE: i32 = CHUNK_SIZE + 2;
const VOLUME_CELLS: usize = (VOLUME_SIZE * VOLUME_SIZE * VOLUME_SIZE) as usize;

/// Cells of one chunk plus a 1-cell border. Coordinates are relative to the
/// chunk origin and run over `-1..=32` on each axis.
pub struct Volume {
    cells: Vec<MaterialId>,
}

impl Volume {
    fn index(x: i32, y: i32, z: i32) -> usize {
        debug_assert!([x, y, z].iter().all(|c| (-1..=CHUNK_SIZE).contains(c)));
        ((x + 1) + (z + 1) * VOLUME_SIZE + (y + 1) * VOLUME_SIZE * VOLUME_SIZE) as usize
    }

    /// Material at `(x, y, z)` relative to the chunk origin.
    pub fn get(&self, x: i32, y: i32, z: i32) -> MaterialId {
        self.cells[Self::index(x, y, z)]
    }

    /// The inner 32³ as compact chunk storage.
    fn to_storage(&self) -> ChunkStorage {
        let at = |i: usize| {
            let l = LocalPos::from_index(i as u16).0.as_ivec3();
            self.get(l.x, l.y, l.z)
        };
        let mut storage = ChunkStorage::Uniform(at(0));
        for i in 1..CHUNK_VOLUME {
            storage.set(LocalPos::from_index(i as u16), at(i));
        }
        storage.compact();
        storage
    }
}

struct SoilLayer {
    material: MaterialId,
    min_cells: i32,
    max_cells: i32,
    noise: NoiseLayer,
}

struct Stratum {
    material: MaterialId,
    /// Boundary under this stratum (before noise) and its noise; `None` for
    /// the last stratum.
    until: Option<(i32, NoiseLayer)>,
}

/// Generation for one seed, parameter set and material table. Build it
/// once and call [`Generator::generate`] for each chunk; it is `Sync`, so
/// chunks can generate in parallel.
pub struct Generator {
    surface_base: i32,
    surface_amplitude: i32,
    surface: NoiseLayer,
    soil: Vec<SoilLayer>,
    boundary_amplitude: i32,
    strata: Vec<Stratum>,
}

fn resolve(
    materials: &MaterialTable,
    field: String,
    key: &str,
    tag: &'static str,
) -> Result<MaterialId, GenError> {
    let Some(id) = materials.id(key) else {
        return Err(GenError::UnknownMaterial {
            field,
            key: key.to_owned(),
        });
    };
    if !materials.has_tag(id, tag) {
        return Err(GenError::MissingTag {
            field,
            key: key.to_owned(),
            tag,
        });
    }
    Ok(id)
}

impl Generator {
    /// Validates `params`, resolves its material keys against `materials`
    /// (soil layers need the `soil` tag, strata the `stone` tag) and seeds
    /// every noise layer from `(seed, layer id)`.
    pub fn new(seed: u64, params: &GenParams, materials: &MaterialTable) -> Result<Self, GenError> {
        params.validate()?;
        let soil = params
            .soil
            .iter()
            .enumerate()
            .map(|(i, l)| {
                Ok(SoilLayer {
                    material: resolve(materials, format!("soil[{i}]"), &l.material, SOIL_TAG)?,
                    min_cells: l.min_cells,
                    max_cells: l.max_cells,
                    noise: NoiseLayer::new(seed, LAYER_SOIL + i as u32, &l.noise),
                })
            })
            .collect::<Result<_, GenError>>()?;
        let st = &params.strata;
        let strata = st
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| {
                Ok(Stratum {
                    material: resolve(
                        materials,
                        format!("strata.layers[{i}]"),
                        &l.material,
                        STONE_TAG,
                    )?,
                    until: l.until_depth.map(|d| {
                        (
                            d,
                            NoiseLayer::new(seed, LAYER_STRATA + i as u32, &st.boundary_noise),
                        )
                    }),
                })
            })
            .collect::<Result<_, GenError>>()?;
        Ok(Self {
            surface_base: params.surface.base_height,
            surface_amplitude: params.surface.amplitude,
            surface: NoiseLayer::new(seed, LAYER_SURFACE, &params.surface.noise),
            soil,
            boundary_amplitude: st.boundary_amplitude,
            strata,
        })
    }

    /// The chunk at `pos`, with revision 0 and not modified.
    pub fn generate(&self, pos: ChunkPos) -> Chunk {
        let chunk = Chunk::new(self.base_volume(pos).to_storage());
        tracing::debug!(?pos, "chunk generated");
        chunk
    }

    /// y of the topmost solid cell of column `(x, z)` after step 1.
    pub fn surface_height(&self, x: i32, z: i32) -> i32 {
        self.surface_base + self.surface.offset_2d(x, z, self.surface_amplitude)
    }

    /// Steps 1–2 (surface, soil, strata) over the chunk's 34³ volume.
    pub fn base_volume(&self, pos: ChunkPos) -> Volume {
        let origin = pos.origin_cell().0;
        let mut cells = vec![MaterialId::AIR; VOLUME_CELLS];
        let mut soil_ends = Vec::with_capacity(self.soil.len());
        let mut strata_ends = Vec::with_capacity(self.strata.len());
        for z in -1..=CHUNK_SIZE {
            for x in -1..=CHUNK_SIZE {
                let (cx, cz) = (origin.x + x, origin.z + z);
                let surface = self.surface_height(cx, cz);
                // Depth (0 = surface cell) where each soil layer and each
                // bounded stratum ends.
                soil_ends.clear();
                let mut end = 0;
                for l in &self.soil {
                    end += l.noise.pick_2d(cx, cz, l.min_cells, l.max_cells);
                    soil_ends.push(end);
                }
                strata_ends.clear();
                strata_ends.extend(self.strata.iter().filter_map(|s| {
                    s.until.as_ref().map(|(depth, noise)| {
                        depth + noise.offset_2d(cx, cz, self.boundary_amplitude)
                    })
                }));
                for y in -1..=CHUNK_SIZE {
                    let depth = i64::from(surface) - i64::from(origin.y + y);
                    if depth < 0 {
                        continue;
                    }
                    let material = match soil_ends.iter().position(|&e| depth < i64::from(e)) {
                        Some(i) => self.soil[i].material,
                        None => {
                            let i = strata_ends
                                .iter()
                                .position(|&e| depth < i64::from(e))
                                .unwrap_or(self.strata.len() - 1);
                            self.strata[i].material
                        }
                    };
                    cells[Volume::index(x, y, z)] = material;
                }
            }
        }
        Volume { cells }
    }
}

/// SAD §9's pure entry point. Builds a [`Generator`] for this one chunk;
/// prefer a shared `Generator` when generating many.
///
/// Panics if `params` is invalid or names materials `materials` lacks (or
/// that lack the required tag). Load parameters with [`GenParams::load`] and
/// check them with [`Generator::new`] at startup to get an error instead.
pub fn generate_chunk(
    seed: u64,
    pos: ChunkPos,
    params: &GenParams,
    materials: &MaterialTable,
) -> Chunk {
    Generator::new(seed, params, materials)
        .unwrap_or_else(|e| panic!("worldgen params do not fit the material table: {e}"))
        .generate(pos)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use glam::IVec3;

    use super::*;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const PARAMS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/worldgen.ron");

    /// Chunk rows of the default 256 × 128 × 256 world.
    const WORLD_CHUNKS_Y: i32 = 4;
    const WORLD_TOP: i32 = WORLD_CHUNKS_Y * CHUNK_SIZE - 1;

    fn materials() -> MaterialTable {
        MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads")
    }

    fn params() -> GenParams {
        GenParams::load(PARAMS_PATH).expect("assets/worldgen.ron loads")
    }

    fn id(m: &MaterialTable, key: &str) -> MaterialId {
        m.id(key).unwrap_or_else(|| panic!("no material {key:?}"))
    }

    /// Cells of a 32 × 128 × 32 chunk column, generated chunk by chunk.
    /// `cells[x][z]` lists the column top down, from y = 127 to y = 0.
    struct ChunkColumn {
        origin: (i32, i32),
        cells: Vec<Vec<Vec<MaterialId>>>,
        chunk_hashes: Vec<u64>,
    }

    impl ChunkColumn {
        fn generate(g: &Generator, cx: i32, cz: i32) -> Self {
            let chunks: Vec<Chunk> = (0..WORLD_CHUNKS_Y)
                .map(|cy| g.generate(ChunkPos::new(cx, cy, cz)))
                .collect();
            let n = CHUNK_SIZE as u32;
            let cells = (0..n)
                .map(|x| {
                    (0..n)
                        .map(|z| {
                            (0..=WORLD_TOP)
                                .rev()
                                .map(|y| {
                                    let local = LocalPos::new(x, (y & 31) as u32, z);
                                    chunks[(y >> 5) as usize].storage.get(local)
                                })
                                .collect()
                        })
                        .collect()
                })
                .collect();
            Self {
                origin: (cx * CHUNK_SIZE, cz * CHUNK_SIZE),
                cells,
                chunk_hashes: chunks.iter().map(Chunk::hash).collect(),
            }
        }

        /// (x, z, top-down cells) for every cell column.
        fn columns(&self) -> impl Iterator<Item = (i32, i32, &[MaterialId])> {
            self.cells.iter().enumerate().flat_map(move |(x, row)| {
                row.iter().enumerate().map(move |(z, col)| {
                    (self.origin.0 + x as i32, self.origin.1 + z as i32, &col[..])
                })
            })
        }

        /// y of the topmost non-air cell of each column, in column order.
        fn heights(&self) -> Vec<Option<i32>> {
            self.columns()
                .map(|(_, _, col)| {
                    col.iter()
                        .position(|&m| m != MaterialId::AIR)
                        .map(|i| WORLD_TOP - i as i32)
                })
                .collect()
        }
    }

    /// Length of the run of `id` starting at `col[from]`.
    fn run(col: &[MaterialId], from: usize, id: MaterialId) -> usize {
        col[from..].iter().take_while(|&&m| m == id).count()
    }

    #[test]
    fn same_seed_same_chunk() {
        let table = materials();
        let p = params();
        let positions = [
            ChunkPos::new(0, 0, 0),
            ChunkPos::new(0, 2, 0),
            ChunkPos::new(3, 2, 5),
            ChunkPos::new(3, 3, 5),
            ChunkPos::new(7, 1, 7),
            ChunkPos::new(-1, 2, -1),
            ChunkPos::new(-40, 3, 25),
            ChunkPos::new(5, -2, 1),
            ChunkPos::new(-1000, 2, 2000),
        ];
        let mut layered = 0;
        for seed in [0, 1, 0xDEAD_BEEF, 1 << 40, u64::MAX] {
            // Independent loads of the inputs, and both entry points.
            let g = Generator::new(seed, &params(), &materials()).unwrap();
            let first: Vec<Chunk> = positions
                .iter()
                .map(|&pos| generate_chunk(seed, pos, &p, &table))
                .collect();
            for (&pos, a) in positions.iter().zip(&first) {
                let b = g.generate(pos);
                assert_eq!(*a, b, "seed {seed} {pos:?}: storage differs");
                assert_eq!(a.hash(), b.hash());
                assert_eq!(a.revision, 0);
                assert!(!a.modified);
                assert!(a.storage.is_well_formed());
                for i in 0..CHUNK_VOLUME {
                    let m = a.storage.get(LocalPos::from_index(i as u16));
                    assert!(table.get(m).is_some(), "{pos:?} holds unknown {m:?}");
                }
                if let ChunkStorage::Paletted { palette, .. } = &a.storage
                    && palette.len() >= 3
                {
                    layered += 1;
                }
            }

            // Order does not matter.
            let reversed: Vec<Chunk> = positions.iter().rev().map(|&pos| g.generate(pos)).collect();
            assert!(
                reversed.iter().rev().eq(first.iter()),
                "seed {seed}: order matters"
            );

            // Neither does the thread.
            let other = std::thread::spawn(move || {
                let g = Generator::new(seed, &params(), &materials()).unwrap();
                positions.map(|pos| g.generate(pos))
            })
            .join()
            .unwrap();
            assert!(other.iter().eq(first.iter()), "seed {seed}: thread matters");
        }
        // The positions include surface chunks, so this is not all air.
        assert!(
            layered >= 5,
            "only {layered} chunks hold three or more materials"
        );
    }

    #[test]
    fn different_seed_differs() {
        let table = materials();
        let p = params();
        let columns = [(0, 0), (2, 5), (6, 1), (7, 7)];
        // Plain neighbours, and seeds that differ only in their high bits.
        let pairs: [(u64, u64); 4] = [(1, 2), (0, 1 << 32), (7, 7 ^ (1 << 63)), (42, 43)];
        for (s1, s2) in pairs {
            let g1 = Generator::new(s1, &p, &table).unwrap();
            let g2 = Generator::new(s2, &p, &table).unwrap();
            let mut same_height = 0;
            let mut total = 0;
            for &(cx, cz) in &columns {
                let a = ChunkColumn::generate(&g1, cx, cz);
                let b = ChunkColumn::generate(&g2, cx, cz);
                assert_ne!(
                    a.chunk_hashes, b.chunk_hashes,
                    "seeds {s1} and {s2} give the same chunk column ({cx}, {cz})"
                );
                let (ha, hb) = (a.heights(), b.heights());
                assert!(ha.iter().chain(&hb).all(Option::is_some));
                same_height += ha.iter().zip(&hb).filter(|(x, y)| x == y).count();
                total += ha.len();
            }
            assert!(
                same_height * 4 < total,
                "seeds {s1} and {s2}: {same_height} of {total} surface heights equal"
            );
        }
        // A surface chunk differs on its own, too.
        let pos = ChunkPos::new(3, 2, 3);
        let a = generate_chunk(5, pos, &p, &table);
        let b = generate_chunk(6, pos, &p, &table);
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn surface_has_topsoil_then_dirt() {
        let table = materials();
        let p = params();
        let topsoil = id(&table, "topsoil");
        let dirt = id(&table, "dirt");
        // SAD §9: topsoil 1–2 cells, then dirt, both tagged soil.
        assert_eq!(p.soil[0].material, "topsoil");
        assert_eq!((p.soil[0].min_cells, p.soil[0].max_cells), (1, 2));
        assert_eq!(p.soil[1].material, "dirt");
        assert!(table.has_tag(topsoil, SOIL_TAG) && table.has_tag(dirt, SOIL_TAG));
        let dirt_max = p.soil[1].max_cells as usize;
        let lo = p.surface.base_height - p.surface.amplitude;
        let hi = p.surface.base_height + p.surface.amplitude;
        assert!(hi < WORLD_TOP, "surface reaches the top of the world");

        let mut thicknesses = BTreeSet::new();
        let mut heights = BTreeSet::new();
        for seed in [3, 0xC0FFEE] {
            let g = Generator::new(seed, &p, &table).unwrap();
            // Columns away from the world edges, which F044 turns to bedrock.
            for (cx, cz) in [(1, 1), (1, 6), (3, 4), (6, 2), (6, 6)] {
                let column = ChunkColumn::generate(&g, cx, cz);
                for (x, z, col) in column.columns() {
                    let at = format!("seed {seed} column ({x}, {z})");
                    // Air down to the surface, which is topsoil.
                    let top = col
                        .iter()
                        .position(|&m| m != MaterialId::AIR)
                        .unwrap_or_else(|| panic!("{at}: no solid cell"));
                    let y = WORLD_TOP - top as i32;
                    assert!((lo..=hi).contains(&y), "{at}: surface at y = {y}");
                    assert_eq!(y, g.surface_height(x, z), "{at}");
                    assert_eq!(col[top], topsoil, "{at}: surface is {:?}", col[top]);
                    assert!(table.get(col[top]).unwrap().is_solid());

                    let t = run(col, top, topsoil);
                    assert!((1..=2).contains(&t), "{at}: {t} topsoil cells");
                    // No later step touches the top 4 cells (caves stay 4
                    // cells under the surface), so they are exactly soil.
                    for d in 0..4 {
                        let want = if d < t { topsoil } else { dirt };
                        assert_eq!(col[top + d], want, "{at}: depth {d}");
                    }
                    let d = run(col, top + t, dirt);
                    assert!(d <= dirt_max, "{at}: {d} dirt cells");
                    let below = col[top + t + d];
                    assert!(
                        !table.has_tag(below, SOIL_TAG),
                        "{at}: soil {below:?} under the dirt"
                    );
                    thicknesses.insert(t);
                    heights.insert(y);
                }
            }
        }
        assert_eq!(thicknesses.len(), 2, "topsoil thickness never varies");
        assert!(
            heights.len() >= 8,
            "surface heights {heights:?} barely vary"
        );
    }

    #[test]
    fn base_profile_matches_params() {
        let table = materials();
        let p = params();
        let soil: Vec<MaterialId> = p.soil.iter().map(|l| id(&table, &l.material)).collect();
        let strata: Vec<MaterialId> = p
            .strata
            .layers
            .iter()
            .map(|l| id(&table, &l.material))
            .collect();
        let amp = p.strata.boundary_amplitude;
        let g = Generator::new(11, &p, &table).unwrap();

        // First depth of each stratum below the top one, per column.
        let mut starts: Vec<BTreeSet<i32>> = vec![BTreeSet::new(); strata.len()];
        for (cx, cz) in [(2, 2), (5, 3)] {
            let vols: Vec<Volume> = (0..WORLD_CHUNKS_Y)
                .map(|cy| g.base_volume(ChunkPos::new(cx, cy, cz)))
                .collect();
            for x in 0..CHUNK_SIZE {
                for z in 0..CHUNK_SIZE {
                    let col: Vec<MaterialId> = (0..=WORLD_TOP)
                        .rev()
                        .map(|y| vols[(y >> 5) as usize].get(x, y & 31, z))
                        .collect();
                    let (wx, wz) = (cx * CHUNK_SIZE + x, cz * CHUNK_SIZE + z);
                    let top = (WORLD_TOP - g.surface_height(wx, wz)) as usize;
                    assert!(col[..top].iter().all(|&m| m == MaterialId::AIR));
                    // Soil layers in order, each within its thickness range.
                    let mut d = top;
                    for (l, &m) in p.soil.iter().zip(&soil) {
                        let t = run(&col, d, m) as i32;
                        assert!(
                            (l.min_cells..=l.max_cells).contains(&t),
                            "{t} {:?}",
                            l.material
                        );
                        d += t as usize;
                    }
                    // Then strata top down to y = 0, never going back up,
                    // with no air or soil.
                    let mut layer = 0;
                    for (i, &m) in col.iter().enumerate().skip(d) {
                        let k = strata.iter().position(|&s| s == m).unwrap_or_else(|| {
                            panic!(
                                "({wx}, {wz}) y = {}: {m:?} is not a stratum",
                                WORLD_TOP - i as i32
                            )
                        });
                        assert!(table.has_tag(m, STONE_TAG));
                        assert!(k >= layer, "({wx}, {wz}): strata out of order");
                        if k > layer || (i == d && k > 0) {
                            for set in &mut starts[layer + 1..=k] {
                                set.insert((i - top) as i32);
                            }
                            layer = k;
                        }
                    }
                }
            }
        }
        // Every stratum shows up, starting near its noise-moved boundary.
        for (j, set) in starts.iter().enumerate().skip(1) {
            let base = p.strata.layers[j - 1].until_depth.unwrap();
            let (lo, hi) = (*set.first().unwrap(), *set.last().unwrap());
            assert!(
                base - amp <= lo && hi <= base + amp,
                "stratum {j}: {lo}..={hi}"
            );
            assert!(set.len() >= 3, "stratum {j} boundary barely moves: {set:?}");
        }

        // The volume border holds the neighbours' cells.
        let pos = ChunkPos::new(3, 2, 4);
        let v = g.base_volume(pos);
        for (axis, step) in [(0, -1), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)] {
            let mut d = IVec3::ZERO;
            d[axis] = step;
            let n = g.base_volume(ChunkPos(pos.0 + d));
            // Border cell c of this volume is cell c - 32·d of the neighbour's.
            for a in -1..=CHUNK_SIZE {
                for b in -1..=CHUNK_SIZE {
                    let mut c = IVec3::ZERO;
                    c[axis] = if step > 0 { CHUNK_SIZE } else { -1 };
                    c[(axis + 1) % 3] = a;
                    c[(axis + 2) % 3] = b;
                    let nc = c - d * CHUNK_SIZE;
                    assert_eq!(v.get(c.x, c.y, c.z), n.get(nc.x, nc.y, nc.z), "{c} vs {nc}");
                }
            }
        }
    }

    #[test]
    fn generator_checks_materials() {
        let table = materials();
        let mut p = params();
        p.soil[1].material = "mud".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::UnknownMaterial { ref key, .. }) if key == "mud"
        ));
        let mut p = params();
        p.soil[0].material = "stone".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::MissingTag { tag: SOIL_TAG, .. })
        ));
        let mut p = params();
        p.strata.layers[2].material = "copper_ore".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::MissingTag { tag: STONE_TAG, .. })
        ));
        let mut p = params();
        p.strata.layers.clear();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::Invalid { .. })
        ));
        let result =
            std::panic::catch_unwind(|| generate_chunk(1, ChunkPos::new(0, 0, 0), &p, &table));
        assert!(result.is_err(), "generate_chunk accepted invalid params");
    }
}
