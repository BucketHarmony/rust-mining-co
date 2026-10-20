//! Chunk generation (SAD §9): surface, strata, caves, ores, crystals and the
//! bedrock shell.
//!
//! Steps 1–4 and 6 run over a 34³ [`Volume`] (the chunk plus a 1-cell
//! border) so step 5 can read neighbors; only the inner 32³ is written to the
//! chunk.

use glam::IVec3;
use rayon::prelude::*;
use rmc_world::chunk::{Chunk, ChunkStorage};
use rmc_world::coords::{CHUNK_SIZE, CHUNK_VOLUME, CellAabb, CellPos, ChunkPos, LocalPos};
use rmc_world::materials::{MaterialId, MaterialTable};

use crate::noise::{
    LAYER_CAVE_BLOB, LAYER_CAVE_WORM_A, LAYER_CAVE_WORM_B, LAYER_CRYSTAL, LAYER_ORE, LAYER_SOIL,
    LAYER_STRATA, LAYER_SURFACE, NoiseLayer, cell_hash,
};
use crate::params::{GenError, GenParams, ONE_MILLION};
use crate::temperature::TemperatureInit;

/// Tag every soil layer material must carry.
pub const SOIL_TAG: &str = "soil";
/// Tag every stratum material must carry, and the only cells ores replace.
pub const STONE_TAG: &str = "stone";
/// Tag every ore material must carry.
pub const ORE_TAG: &str = "ore";
/// Tag the crystal material must carry.
pub const CRYSTAL_TAG: &str = "crystal";

/// Edge length of the generation volume: a chunk plus a 1-cell border.
pub const VOLUME_SIZE: i32 = CHUNK_SIZE + 2;
const VOLUME_CELLS: usize = (VOLUME_SIZE * VOLUME_SIZE * VOLUME_SIZE) as usize;

/// Cells of one chunk plus a 1-cell border. Coordinates are relative to the
/// chunk origin and run over `-1..=32` on each axis.
pub struct Volume {
    cells: Vec<MaterialId>,
    /// Step 1 surface height of each of the 34² columns.
    surface: Vec<i32>,
}

impl Volume {
    fn column(x: i32, z: i32) -> usize {
        ((x + 1) + (z + 1) * VOLUME_SIZE) as usize
    }

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

struct Caves {
    surface_margin: i32,
    blob_threshold: f32,
    blob: NoiseLayer,
    worm_radius: f32,
    worm_a: NoiseLayer,
    worm_b: NoiseLayer,
}

impl Caves {
    /// Whether cell `(x, y, z)` is cave: blob noise above its threshold, or
    /// both worm noises within the radius of zero.
    fn is_cave(&self, x: i32, y: i32, z: i32) -> bool {
        if self.blob.sample_3d(x, y, z) > self.blob_threshold {
            return true;
        }
        self.worm_radius > 0.0
            && self.worm_a.sample_3d(x, y, z).abs() < self.worm_radius
            && self.worm_b.sample_3d(x, y, z).abs() < self.worm_radius
    }
}

struct Ore {
    material: MaterialId,
    min_depth: i32,
    max_depth: i32,
    threshold: f32,
    noise: NoiseLayer,
}

struct Crystals {
    material: MaterialId,
    chance_per_million: u32,
    seed: u64,
}

impl Crystals {
    /// Whether the cell hash picks cell `(x, y, z)`.
    fn picks(&self, x: i32, y: i32, z: i32) -> bool {
        let roll = cell_hash(self.seed, LAYER_CRYSTAL, x, y, z) % u64::from(ONE_MILLION);
        roll < u64::from(self.chance_per_million)
    }
}

struct Shell {
    material: MaterialId,
    bounds: CellAabb,
    floor_cells: i32,
    wall_cells: i32,
}

impl Shell {
    /// Whether world cell `c` is in the shell: inside the bounds, and in the
    /// bottom `floor_cells` layers or within `wall_cells` of an x or z edge.
    fn contains(&self, c: IVec3) -> bool {
        if !self.bounds.contains(CellPos(c)) {
            return false;
        }
        let (lo, hi) = (self.bounds.min.0, self.bounds.max.0);
        let wall = |a: usize| c[a] - lo[a] < self.wall_cells || hi[a] - c[a] < self.wall_cells;
        c.y - lo.y < self.floor_cells || wall(0) || wall(2)
    }
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
    caves: Caves,
    /// Top priority first.
    ores: Vec<Ore>,
    crystals: Crystals,
    shell: Shell,
    /// Whether each material id carries the `stone` tag.
    stone: Vec<bool>,
    /// The initial temperature layer (SAD-P2 §8), when set.
    temperature: Option<TemperatureInit>,
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
    /// (soil layers need the `soil` tag, strata `stone`, ores `ore`, crystals
    /// `crystal`; bedrock must be solid and not diggable) and seeds every
    /// noise layer from `(seed, layer id)`.
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
        let c = &params.caves;
        let caves = Caves {
            surface_margin: c.surface_margin,
            blob_threshold: c.blob.threshold,
            blob: NoiseLayer::new(seed, LAYER_CAVE_BLOB, &c.blob.noise),
            worm_radius: c.worm.radius,
            worm_a: NoiseLayer::new(seed, LAYER_CAVE_WORM_A, &c.worm.a),
            worm_b: NoiseLayer::new(seed, LAYER_CAVE_WORM_B, &c.worm.b),
        };
        let ores = params
            .ores
            .iter()
            .enumerate()
            .map(|(i, o)| {
                Ok(Ore {
                    material: resolve(materials, format!("ores[{i}]"), &o.material, ORE_TAG)?,
                    min_depth: o.min_depth,
                    max_depth: o.max_depth,
                    threshold: o.threshold,
                    noise: NoiseLayer::new(seed, LAYER_ORE + i as u32, &o.noise),
                })
            })
            .collect::<Result<_, GenError>>()?;
        let crystals = Crystals {
            material: resolve(
                materials,
                "crystals".to_owned(),
                &params.crystals.material,
                CRYSTAL_TAG,
            )?,
            chance_per_million: params.crystals.chance_per_million,
            seed,
        };
        let b = &params.bedrock;
        let Some(bedrock) = materials.id(&b.material) else {
            return Err(GenError::UnknownMaterial {
                field: "bedrock".to_owned(),
                key: b.material.clone(),
            });
        };
        if materials
            .get(bedrock)
            .is_none_or(|d| !d.is_solid() || d.diggable)
        {
            return Err(GenError::Diggable {
                field: "bedrock".to_owned(),
                key: b.material.clone(),
            });
        }
        let shell = Shell {
            material: bedrock,
            bounds: params.bounds.aabb(),
            floor_cells: b.floor_cells,
            wall_cells: b.wall_cells,
        };
        // Ids equal table positions.
        let stone = materials.iter().map(|d| d.has_tag(STONE_TAG)).collect();
        Ok(Self {
            surface_base: params.surface.base_height,
            surface_amplitude: params.surface.amplitude,
            surface: NoiseLayer::new(seed, LAYER_SURFACE, &params.surface.noise),
            soil,
            boundary_amplitude: st.boundary_amplitude,
            strata,
            caves,
            ores,
            crystals,
            shell,
            stone,
            temperature: None,
        })
    }

    /// Also writes each chunk's temperature layer (SAD-P2 §8). Without it
    /// chunks carry no layers, as in Phase 1.
    pub fn with_temperature(mut self, init: TemperatureInit) -> Self {
        self.temperature = Some(init);
        self
    }

    /// The chunk at `pos`, with revision 0 and not modified.
    pub fn generate(&self, pos: ChunkPos) -> Chunk {
        let mut chunk = Chunk::new(self.volume(pos).to_storage());
        if let Some(t) = &self.temperature {
            let bottom_y = self.shell.bounds.min.0.y;
            t.fill(&mut chunk, pos, bottom_y, |x, z| self.surface_height(x, z));
        }
        tracing::debug!(?pos, "chunk generated");
        chunk
    }

    /// The world bounds from [`GenParams::bounds`].
    pub fn bounds(&self) -> CellAabb {
        self.shell.bounds
    }

    /// Every chunk that overlaps the world bounds, generated in parallel,
    /// in the bounds' (y, z, x) chunk order.
    pub fn generate_world(&self) -> Vec<(ChunkPos, Chunk)> {
        self.generate_chunks(self.bounds().chunks())
    }

    /// The chunks at `positions`, generated in parallel (rayon) and
    /// returned in the order given.
    pub fn generate_chunks(
        &self,
        positions: impl IntoIterator<Item = ChunkPos>,
    ) -> Vec<(ChunkPos, Chunk)> {
        let positions: Vec<ChunkPos> = positions.into_iter().collect();
        let chunks: Vec<(ChunkPos, Chunk)> = positions
            .into_par_iter()
            .map(|pos| (pos, self.generate(pos)))
            .collect();
        tracing::info!(chunks = chunks.len(), "chunks generated");
        chunks
    }

    /// Every step over the chunk's 34³ volume; [`Generator::generate`]
    /// writes its inner 32³. Step 5 reads the border but only changes the
    /// inner 32³, so the border cells hold the other steps only.
    pub fn volume(&self, pos: ChunkPos) -> Volume {
        let mut volume = self.shell_volume(pos);
        self.place_crystals(pos, &mut volume);
        volume
    }

    /// Every step except crystals (steps 1–4, then 6) over the chunk's 34³
    /// volume: the cells step 5 reads. Crystals never replace bedrock, so
    /// the shell is still the last word, and crystals face air in the
    /// finished world.
    pub fn shell_volume(&self, pos: ChunkPos) -> Volume {
        let mut volume = self.ore_volume(pos);
        self.place_shell(pos, &mut volume);
        volume
    }

    /// Step 6: bedrock over every cell of the shell.
    fn place_shell(&self, pos: ChunkPos, volume: &mut Volume) {
        let shell = &self.shell;
        if shell.floor_cells == 0 && shell.wall_cells == 0 {
            return;
        }
        let origin = pos.origin_cell().0;
        for y in -1..=CHUNK_SIZE {
            for z in -1..=CHUNK_SIZE {
                for x in -1..=CHUNK_SIZE {
                    if shell.contains(origin + IVec3::new(x, y, z)) {
                        volume.cells[Volume::index(x, y, z)] = shell.material;
                    }
                }
            }
        }
    }

    /// Steps 1–4 (surface, soil, strata, caves, ores) over the chunk's 34³
    /// volume.
    pub fn ore_volume(&self, pos: ChunkPos) -> Volume {
        let mut volume = self.cave_volume(pos);
        self.place_ores(pos, &mut volume);
        volume
    }

    /// Steps 1–3 (surface, soil, strata, caves) over the chunk's 34³ volume.
    pub fn cave_volume(&self, pos: ChunkPos) -> Volume {
        let mut volume = self.base_volume(pos);
        self.carve_caves(pos, &mut volume);
        volume
    }

    /// Step 3: turns cave cells to air, except in the surface cell and the
    /// `surface_margin` cells under it.
    fn carve_caves(&self, pos: ChunkPos, volume: &mut Volume) {
        let origin = pos.origin_cell().0;
        let margin = i64::from(self.caves.surface_margin);
        for z in -1..=CHUNK_SIZE {
            for x in -1..=CHUNK_SIZE {
                let (cx, cz) = (origin.x + x, origin.z + z);
                // Highest y a cave may reach in this column.
                let top = i64::from(volume.surface[Volume::column(x, z)]) - margin - 1;
                for y in -1..=CHUNK_SIZE {
                    let cy = origin.y + y;
                    if i64::from(cy) > top {
                        break;
                    }
                    let i = Volume::index(x, y, z);
                    if volume.cells[i] != MaterialId::AIR && self.caves.is_cave(cx, cy, cz) {
                        volume.cells[i] = MaterialId::AIR;
                    }
                }
            }
        }
    }

    /// Step 4: replaces `stone`-tagged cells with the first ore whose depth
    /// range holds the cell and whose noise exceeds its threshold.
    fn place_ores(&self, pos: ChunkPos, volume: &mut Volume) {
        if self.ores.is_empty() {
            return;
        }
        let origin = pos.origin_cell().0;
        for z in -1..=CHUNK_SIZE {
            for x in -1..=CHUNK_SIZE {
                let (cx, cz) = (origin.x + x, origin.z + z);
                let surface = i64::from(volume.surface[Volume::column(x, z)]);
                for y in -1..=CHUNK_SIZE {
                    let cy = origin.y + y;
                    let depth = surface - i64::from(cy);
                    let i = Volume::index(x, y, z);
                    let cell = volume.cells[i];
                    if !self
                        .stone
                        .get(usize::from(cell.0))
                        .copied()
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let ore = self.ores.iter().find(|o| {
                        (i64::from(o.min_depth)..=i64::from(o.max_depth)).contains(&depth)
                            && o.noise.sample_3d(cx, cy, cz) > o.threshold
                    });
                    if let Some(ore) = ore {
                        volume.cells[i] = ore.material;
                    }
                }
            }
        }
    }

    /// Step 5: turns the `stone`-tagged inner cells that share a face with
    /// air into crystal where the cell hash picks them. Shell cells are
    /// skipped, so the shell stays the last word even for a stone-tagged
    /// bedrock material.
    fn place_crystals(&self, pos: ChunkPos, volume: &mut Volume) {
        if self.crystals.chance_per_million == 0 {
            return;
        }
        let origin = pos.origin_cell().0;
        let faces = [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ];
        // Placing a crystal never adds or removes air, so changing cells in
        // place gives the same result as reading a copy of steps 1–4 and 6.
        for z in 0..CHUNK_SIZE {
            for y in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let i = Volume::index(x, y, z);
                    let stone = self.stone.get(usize::from(volume.cells[i].0));
                    if !stone.copied().unwrap_or(false) {
                        continue;
                    }
                    let open = faces
                        .iter()
                        .any(|(dx, dy, dz)| volume.get(x + dx, y + dy, z + dz) == MaterialId::AIR);
                    let cell = origin + IVec3::new(x, y, z);
                    if open
                        && !self.shell.contains(cell)
                        && self.crystals.picks(cell.x, cell.y, cell.z)
                    {
                        volume.cells[i] = self.crystals.material;
                    }
                }
            }
        }
    }

    /// y of the topmost solid cell of column `(x, z)` after step 1.
    pub fn surface_height(&self, x: i32, z: i32) -> i32 {
        self.surface_base + self.surface.offset_2d(x, z, self.surface_amplitude)
    }

    /// Steps 1–2 (surface, soil, strata) over the chunk's 34³ volume.
    pub fn base_volume(&self, pos: ChunkPos) -> Volume {
        let origin = pos.origin_cell().0;
        let mut cells = vec![MaterialId::AIR; VOLUME_CELLS];
        let mut heights = vec![0; (VOLUME_SIZE * VOLUME_SIZE) as usize];
        let mut soil_ends = Vec::with_capacity(self.soil.len());
        let mut strata_ends = Vec::with_capacity(self.strata.len());
        for z in -1..=CHUNK_SIZE {
            for x in -1..=CHUNK_SIZE {
                let (cx, cz) = (origin.x + x, origin.z + z);
                let surface = self.surface_height(cx, cz);
                heights[Volume::column(x, z)] = surface;
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
        Volume {
            cells,
            surface: heights,
        }
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
    use std::collections::{BTreeMap, BTreeSet};

    use glam::IVec3;
    use rmc_world::coords::{CellAabb, CellPos};

    use super::*;
    use crate::params::WorldBounds;

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

    /// `p` with only blob caves (`blobs`) or only worm tunnels.
    fn caves_only(p: &GenParams, blobs: bool) -> GenParams {
        let mut p = p.clone();
        if blobs {
            p.caves.worm.radius = 0.0;
        } else {
            p.caves.blob.threshold = 2.0;
        }
        p
    }

    /// Calls `f(x, y, z, depth)` for every cell of a volume, with coordinates
    /// relative to the chunk origin and depth under the surface cell.
    fn for_each_cell(g: &Generator, pos: ChunkPos, mut f: impl FnMut(i32, i32, i32, i64)) {
        let o = pos.origin_cell().0;
        for x in -1..=CHUNK_SIZE {
            for z in -1..=CHUNK_SIZE {
                let surface = i64::from(g.surface_height(o.x + x, o.z + z));
                for y in -1..=CHUNK_SIZE {
                    f(x, y, z, surface - i64::from(o.y + y));
                }
            }
        }
    }

    #[test]
    fn no_caves_within_4_of_surface() {
        let table = materials();
        let shipped = params();
        // SAD §9: suppressed within 4 cells of the surface.
        assert_eq!(shipped.caves.surface_margin, 4);
        let margin = i64::from(shipped.caves.surface_margin);
        // Caves so wide that most rock would be air without the margin.
        let mut wide = params();
        wide.caves.blob.threshold = -0.3;
        wide.caves.worm.radius = 0.5;

        let mut carved = [[0usize; 8]; 2];
        for (k, p) in [&shipped, &wide].into_iter().enumerate() {
            for seed in [5, 0xBAD_C0DE] {
                let g = Generator::new(seed, p, &table).unwrap();
                for (cx, cz) in [(0, 0), (2, 3), (5, 7), (-3, 4)] {
                    for cy in 0..WORLD_CHUNKS_Y {
                        let pos = ChunkPos::new(cx, cy, cz);
                        let base = g.base_volume(pos);
                        let v = g.cave_volume(pos);
                        for_each_cell(&g, pos, |x, y, z, depth| {
                            let (b, c) = (base.get(x, y, z), v.get(x, y, z));
                            let at =
                                || format!("seed {seed} {pos:?} ({x}, {y}, {z}) depth {depth}");
                            if depth <= margin {
                                assert_eq!(c, b, "{}: carved near the surface", at());
                            } else if c != b {
                                assert_eq!(c, MaterialId::AIR, "{}: caves only carve air", at());
                                if depth < 8 + margin {
                                    carved[k][(depth - margin) as usize] += 1;
                                }
                            }
                        });
                        // The chunk is the full volume's inner 32³.
                        let chunk = g.generate(pos);
                        let full = g.volume(pos);
                        for i in 0..CHUNK_VOLUME {
                            let l = LocalPos::from_index(i as u16);
                            let c = l.0.as_ivec3();
                            assert_eq!(chunk.storage.get(l), full.get(c.x, c.y, c.z));
                        }
                    }
                }
            }
        }
        // The margin is exact: wide caves reach the first cell under it.
        assert!(
            carved[1][1] > 1000,
            "wide caves carve only {} cells at depth {}",
            carved[1][1],
            margin + 1
        );
        assert!(
            carved[0].iter().sum::<usize>() > 0,
            "shipped caves never come within 8 cells of the margin"
        );
    }

    #[test]
    fn caves_exist_at_depth() {
        let table = materials();
        let p = params();
        let margin = i64::from(p.caves.surface_margin);
        let strata: Vec<MaterialId> = p
            .strata
            .layers
            .iter()
            .map(|l| id(&table, &l.material))
            .collect();
        let seed = 21;
        let g = Generator::new(seed, &p, &table).unwrap();
        let blob = Generator::new(seed, &caves_only(&p, true), &table).unwrap();
        let worm = Generator::new(seed, &caves_only(&p, false), &table).unwrap();

        let (mut rock, mut cave, mut blob_air, mut worm_air, mut worm_alone) = (0, 0, 0, 0, 0);
        let mut connected = 0;
        let mut per_stratum = vec![0usize; strata.len()];
        for (cx, cz) in [(1, 1), (2, 6), (5, 2), (6, 5)] {
            for cy in 0..WORLD_CHUNKS_Y {
                let pos = ChunkPos::new(cx, cy, cz);
                let base = g.base_volume(pos);
                let (v, vb, vw) = (g.volume(pos), blob.volume(pos), worm.volume(pos));
                for_each_cell(&g, pos, |x, y, z, depth| {
                    let inner = [x, y, z].iter().all(|c| (0..CHUNK_SIZE).contains(c));
                    if !inner || depth <= margin || base.get(x, y, z) == MaterialId::AIR {
                        return;
                    }
                    rock += 1;
                    let air = |vol: &Volume| vol.get(x, y, z) == MaterialId::AIR;
                    let (a, ab, aw) = (air(&v), air(&vb), air(&vw));
                    // Blobs plus worms: the union, nothing else.
                    assert_eq!(a, ab || aw, "{pos:?} ({x}, {y}, {z})");
                    blob_air += usize::from(ab);
                    worm_air += usize::from(aw);
                    worm_alone += usize::from(aw && !ab);
                    if a {
                        cave += 1;
                        let k = strata.iter().position(|&s| s == base.get(x, y, z));
                        if let Some(k) = k {
                            per_stratum[k] += 1;
                        }
                        let faces = [
                            (1, 0, 0),
                            (-1, 0, 0),
                            (0, 1, 0),
                            (0, -1, 0),
                            (0, 0, 1),
                            (0, 0, -1),
                        ];
                        let open = faces
                            .iter()
                            .any(|(dx, dy, dz)| v.get(x + dx, y + dy, z + dz) == MaterialId::AIR);
                        connected += usize::from(open);
                    }
                });
            }
        }
        let share = cave as f64 / rock as f64;
        assert!(
            (0.01..=0.25).contains(&share),
            "caves are {:.2}% of {rock} rock cells",
            share * 100.0
        );
        assert!(blob_air > 1000, "{blob_air} blob cave cells");
        assert!(worm_air > 1000, "{worm_air} worm tunnel cells");
        assert!(
            worm_alone * 2 > worm_air,
            "worm tunnels mostly lie inside blobs"
        );
        // Every stratum, down to the deepest, has caves.
        assert!(
            per_stratum.iter().all(|&n| n > 100),
            "cave cells per stratum: {per_stratum:?}"
        );
        // Caves are open bodies, not single-cell speckle.
        assert!(
            connected * 100 >= cave * 98,
            "{connected} of {cave} cave cells touch another"
        );
    }

    #[test]
    fn cave_volume_border_matches_neighbors() {
        let table = materials();
        let p = params();
        let g = Generator::new(8, &p, &table).unwrap();
        let mut carved = 0;
        for pos in [ChunkPos::new(3, 1, 4), ChunkPos::new(-2, 2, 1)] {
            let base = g.base_volume(pos);
            let v = g.ore_volume(pos);
            for (axis, step) in [(0, -1), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)] {
                let mut d = IVec3::ZERO;
                d[axis] = step;
                let n = g.ore_volume(ChunkPos(pos.0 + d));
                for a in -1..=CHUNK_SIZE {
                    for b in -1..=CHUNK_SIZE {
                        let mut c = IVec3::ZERO;
                        c[axis] = if step > 0 { CHUNK_SIZE } else { -1 };
                        c[(axis + 1) % 3] = a;
                        c[(axis + 2) % 3] = b;
                        let nc = c - d * CHUNK_SIZE;
                        let here = v.get(c.x, c.y, c.z);
                        assert_eq!(here, n.get(nc.x, nc.y, nc.z), "{pos:?} {c} vs {nc}");
                        carved += usize::from(here != base.get(c.x, c.y, c.z));
                    }
                }
            }
        }
        assert!(carved > 0, "no cave crosses a chunk face");
    }

    /// `p` with every ore everywhere in its depth range (threshold below any
    /// noise value), so only depth and tags decide.
    fn ores_everywhere(p: &GenParams) -> GenParams {
        let mut p = p.clone();
        for ore in &mut p.ores {
            ore.threshold = -2.0;
        }
        p
    }

    /// The shipped table with dirt tagged both `soil` and `stone`.
    fn stony_dirt_table() -> MaterialTable {
        let text = std::fs::read_to_string(MATERIALS_PATH).unwrap();
        let from = r#"key: "dirt", phase: Solid, tags: ["soil"]"#;
        assert!(text.contains(from));
        let to = r#"key: "dirt", phase: Solid, tags: ["soil", "stone"]"#;
        MaterialTable::from_ron_str(&text.replace(from, to)).unwrap()
    }

    #[test]
    fn ores_only_replace_stone_tag() {
        let shipped_table = materials();
        let shipped = params();
        let ores: Vec<MaterialId> = shipped
            .ores
            .iter()
            .map(|o| id(&shipped_table, &o.material))
            .collect();
        // SAD §6 Phase 1 ores.
        assert_eq!(
            ores,
            ["coal_seam", "copper_ore", "iron_ore"].map(|k| id(&shipped_table, k))
        );
        let (lo, hi) = (
            i64::from(shipped.ores.iter().map(|o| o.min_depth).min().unwrap()),
            i64::from(shipped.ores.iter().map(|o| o.max_depth).max().unwrap()),
        );
        // Every ore everywhere from the surface cell down, so soil and air
        // are in range too.
        let mut shallow = ores_everywhere(&shipped);
        for ore in &mut shallow.ores {
            ore.min_depth = 0;
        }
        let stony_dirt = stony_dirt_table();
        let dirt = id(&stony_dirt, "dirt");

        let mut sparse_count = vec![0usize; ores.len()];
        let mut sparse_stone = 0;
        // In the "everywhere" runs: stone replaced, other solid cells kept
        // in range, and (stone-tagged) dirt replaced.
        let (mut replaced, mut kept, mut dirt_replaced) = (0, 0, 0);
        let runs = [
            (&shipped, &shipped_table),
            (&shallow, &shipped_table),
            (&shallow, &stony_dirt),
        ];
        for (k, (p, table)) in runs.into_iter().enumerate() {
            for seed in [9, 0xFACE] {
                let g = Generator::new(seed, p, table).unwrap();
                for (cx, cz) in [(1, 2), (4, 4), (6, 1), (-2, 5)] {
                    for cy in 0..WORLD_CHUNKS_Y {
                        let pos = ChunkPos::new(cx, cy, cz);
                        let before = g.cave_volume(pos);
                        let after = g.ore_volume(pos);
                        for_each_cell(&g, pos, |x, y, z, depth| {
                            let (b, a) = (before.get(x, y, z), after.get(x, y, z));
                            let at = || format!("run {k} seed {seed} {pos:?} ({x}, {y}, {z})");
                            if !table.has_tag(b, STONE_TAG) {
                                assert_eq!(a, b, "{}: replaced non-stone {b:?}", at());
                                let in_range = (0..=hi).contains(&depth);
                                kept += usize::from(k > 0 && in_range && b != MaterialId::AIR);
                                return;
                            }
                            if a != b {
                                assert!(ores.contains(&a), "{}: {a:?} is not an ore", at());
                                assert!(table.has_tag(a, ORE_TAG), "{}", at());
                                assert!(!table.has_tag(a, STONE_TAG), "{}", at());
                            }
                            if k == 0 {
                                if let Some(i) = ores.iter().position(|&o| o == a) {
                                    sparse_count[i] += 1;
                                }
                                sparse_stone += usize::from((lo..=hi).contains(&depth));
                            } else if (0..=hi).contains(&depth) {
                                assert_ne!(a, b, "{}: stone left at depth {depth}", at());
                                replaced += 1;
                                dirt_replaced += usize::from(b == dirt);
                            }
                        });
                    }
                }
            }
        }
        // Shipped ores are sparse but present, every one of them.
        let total: usize = sparse_count.iter().sum();
        assert!(
            sparse_count.iter().all(|&n| n > 200),
            "ore cells per ore: {sparse_count:?}"
        );
        assert!(
            total * 100 < sparse_stone * 15,
            "ores are {total} of {sparse_stone} stone cells in range"
        );
        // Soil in range stays; stone in range all goes.
        assert!(
            kept > 10_000 && replaced > 100_000,
            "{kept} kept, {replaced} replaced"
        );
        // The tag decides, not the key: stone-tagged dirt is replaced too.
        assert!(dirt_replaced > 1000, "{dirt_replaced} dirt cells replaced");
    }

    #[test]
    fn ores_respect_depth_range() {
        let table = materials();
        let shipped = params();
        let ores: Vec<MaterialId> = shipped
            .ores
            .iter()
            .map(|o| id(&table, &o.material))
            .collect();
        let range = |p: &GenParams, i: usize| {
            i64::from(p.ores[i].min_depth)..=i64::from(p.ores[i].max_depth)
        };
        // Each shipped range starts under the cave margin and ends above
        // y = 0 wherever the surface is at its lowest.
        let lowest = i64::from(shipped.surface.base_height - shipped.surface.amplitude);
        for i in 0..ores.len() {
            let r = range(&shipped, i);
            assert!(*r.start() > i64::from(shipped.caves.surface_margin));
            assert!(*r.end() <= lowest, "ore {i} reaches under the world");
        }
        let everywhere = ores_everywhere(&shipped);

        // Depths at which each ore occurs, per run.
        let mut seen = vec![vec![BTreeSet::new(); ores.len()]; 2];
        for (k, p) in [&shipped, &everywhere].into_iter().enumerate() {
            for seed in [17, 1 << 50] {
                let g = Generator::new(seed, p, &table).unwrap();
                for (cx, cz) in [(2, 2), (5, 6), (0, 7)] {
                    for cy in 0..WORLD_CHUNKS_Y {
                        let pos = ChunkPos::new(cx, cy, cz);
                        let before = g.cave_volume(pos);
                        let after = g.ore_volume(pos);
                        for_each_cell(&g, pos, |x, y, z, depth| {
                            let (b, a) = (before.get(x, y, z), after.get(x, y, z));
                            let at = || {
                                format!("run {k} seed {seed} {pos:?} ({x}, {y}, {z}) depth {depth}")
                            };
                            if let Some(i) = ores.iter().position(|&o| o == a) {
                                assert!(range(p, i).contains(&depth), "{}: {a:?}", at());
                                seen[k][i].insert(depth);
                            }
                            if k == 1 && table.has_tag(b, STONE_TAG) {
                                // The first ore whose range holds the depth.
                                let want = (0..ores.len())
                                    .find(|&i| range(p, i).contains(&depth))
                                    .map_or(b, |i| ores[i]);
                                assert_eq!(a, want, "{}", at());
                            }
                        });
                    }
                }
            }
        }
        let [sparse_seen, all_seen] = &seen[..] else {
            unreachable!()
        };
        for (i, (sparse, all)) in sparse_seen.iter().zip(all_seen).enumerate() {
            let r = range(&shipped, i);
            // Everywhere: each ore fills its range end to end, except where
            // an earlier ore's range overlaps it.
            let first = (0..i).fold(*r.start(), |d, j| {
                let rj = range(&shipped, j);
                if rj.contains(&d) { *rj.end() + 1 } else { d }
            });
            assert_eq!(all.first(), Some(&first), "ore {i}: {all:?}");
            assert_eq!(all.last(), Some(r.end()), "ore {i}: {all:?}");
            // Shipped: the ore spreads over most of its range.
            let span = r.end() - r.start() + 1;
            assert!(
                sparse.len() as i64 * 2 > span,
                "ore {i} occurs at only {} of {span} depths",
                sparse.len()
            );
        }
    }

    const FACES: [IVec3; 6] = [
        IVec3::X,
        IVec3::NEG_X,
        IVec3::Y,
        IVec3::NEG_Y,
        IVec3::Z,
        IVec3::NEG_Z,
    ];

    /// Chunk of world cell `c` and `c`'s local coordinates in it.
    fn split(c: IVec3) -> (ChunkPos, IVec3) {
        let chunk = c.div_euclid(IVec3::splat(CHUNK_SIZE));
        (ChunkPos(chunk), c - chunk * CHUNK_SIZE)
    }

    fn local(l: IVec3) -> LocalPos {
        LocalPos::new(l.x as u32, l.y as u32, l.z as u32)
    }

    /// Every step but crystals at world cell `c`, read from the inner 32³
    /// of the shell_volume of the chunk that holds `c` (never from a border).
    fn step4_at(g: &Generator, cache: &mut BTreeMap<[i32; 3], Volume>, c: IVec3) -> MaterialId {
        let (pos, l) = split(c);
        cache
            .entry(pos.0.to_array())
            .or_insert_with(|| g.shell_volume(pos))
            .get(l.x, l.y, l.z)
    }

    /// The generated chunk's cell at world cell `c`.
    fn final_at(g: &Generator, cache: &mut BTreeMap<[i32; 3], Chunk>, c: IVec3) -> MaterialId {
        let (pos, l) = split(c);
        cache
            .entry(pos.0.to_array())
            .or_insert_with(|| g.generate(pos))
            .storage
            .get(local(l))
    }

    /// `p` with crystals on every candidate cell.
    fn crystals_everywhere(p: &GenParams) -> GenParams {
        let mut p = p.clone();
        p.crystals.chance_per_million = ONE_MILLION;
        p
    }

    #[test]
    fn glowcrystal_adjacent_to_air() {
        let table = materials();
        let shipped = params();
        let crystal = id(&table, "glowcrystal");
        assert_eq!(id(&table, &shipped.crystals.material), crystal);
        assert!(table.has_tag(crystal, CRYSTAL_TAG));
        let mut none = params();
        none.crystals.chance_per_million = 0;
        let every = crystals_everywhere(&shipped);

        let (mut crystals, mut candidates) = ([0usize; 3], [0usize; 3]);
        for (k, p) in [&shipped, &every, &none].into_iter().enumerate() {
            for seed in [12, 1 << 45] {
                let g = Generator::new(seed, p, &table).unwrap();
                let (mut step4, mut world) = (BTreeMap::new(), BTreeMap::new());
                for (cx, cz) in [(1, 3), (4, 2), (6, 6)] {
                    for cy in 0..WORLD_CHUNKS_Y {
                        let pos = ChunkPos::new(cx, cy, cz);
                        let o = pos.origin_cell().0;
                        let v = g.volume(pos);
                        for i in 0..CHUNK_VOLUME {
                            let l = LocalPos::from_index(i as u16).0.as_ivec3();
                            let c = o + l;
                            let at = || format!("run {k} seed {seed} {pos:?} cell {c}");
                            let got = final_at(&g, &mut world, c);
                            assert_eq!(got, v.get(l.x, l.y, l.z), "{}: chunk != volume", at());
                            let before = step4_at(&g, &mut step4, c);
                            let open = FACES
                                .iter()
                                .any(|&f| step4_at(&g, &mut step4, c + f) == MaterialId::AIR);
                            let candidate = table.has_tag(before, STONE_TAG) && open;
                            candidates[k] += usize::from(candidate);
                            if got != crystal {
                                assert_eq!(got, before, "{}: step 5 changed a cell", at());
                                assert!(k != 1 || !candidate, "{}: candidate skipped", at());
                                continue;
                            }
                            crystals[k] += 1;
                            assert!(candidate, "{}: crystal on {before:?}, open {open}", at());
                            // Air stays air, so the crystal faces air in the
                            // finished world too.
                            assert!(
                                FACES
                                    .iter()
                                    .any(|&f| final_at(&g, &mut world, c + f) == MaterialId::AIR),
                                "{}: crystal without air",
                                at()
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(crystals[1], candidates[1], "every: all candidates");
        assert!(candidates[1] > 10_000, "only {} candidates", candidates[1]);
        assert_eq!(crystals[2], 0, "chance 0 placed crystals");
        // Shipped: sparse, near the configured share of candidates.
        let share = crystals[0] as f64 / candidates[0] as f64;
        let want = f64::from(shipped.crystals.chance_per_million) / f64::from(ONE_MILLION);
        assert!(crystals[0] > 100, "only {} crystals", crystals[0]);
        assert!(
            (want * 0.8..=want * 1.2).contains(&share) && share < 0.1,
            "{} crystals on {} candidates ({:.2}%)",
            crystals[0],
            candidates[0],
            share * 100.0
        );
    }

    #[test]
    fn glowcrystal_on_chunk_border_matches_neighbor() {
        let table = materials();
        let crystal = id(&table, "glowcrystal");
        let shipped = params();
        let every = crystals_everywhere(&shipped);
        // Crystals on a chunk face whose only air neighbors lie in other
        // chunks: step 5 needs the 34³ border to place them.
        let mut across = [0usize; 2];
        for (k, p) in [&shipped, &every].into_iter().enumerate() {
            for seed in [4, 0xC0FFEE] {
                let g = Generator::new(seed, p, &table).unwrap();
                let mut step4 = BTreeMap::new();
                for a in [(3, 1, 4), (-2, 2, 1), (5, 0, 6), (1, 2, -3)] {
                    let a = ChunkPos::new(a.0, a.1, a.2);
                    let (va, chunk_a) = (g.shell_volume(a), g.generate(a));
                    for (axis, step) in [(0, -1), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)] {
                        let mut d = IVec3::ZERO;
                        d[axis] = step;
                        let b = ChunkPos(a.0 + d);
                        let chunk_b = g.generate(b);
                        for u in 0..CHUNK_SIZE {
                            for w in 0..CHUNK_SIZE {
                                // Face cell of a, and the cell across in b.
                                let mut la = IVec3::ZERO;
                                la[axis] = if step > 0 { CHUNK_SIZE - 1 } else { 0 };
                                la[(axis + 1) % 3] = u;
                                la[(axis + 2) % 3] = w;
                                let ca = a.origin_cell().0 + la;
                                let cb = ca + d;
                                // a's pre-crystal border is b's own cell.
                                let border = la + d;
                                assert_eq!(
                                    va.get(border.x, border.y, border.z),
                                    step4_at(&g, &mut step4, cb),
                                    "seed {seed} {a:?} border {border}"
                                );
                                for (chunk, pos, c) in [(&chunk_a, a, ca), (&chunk_b, b, cb)] {
                                    let before = step4_at(&g, &mut step4, c);
                                    let open: Vec<IVec3> = FACES
                                        .iter()
                                        .map(|&f| c + f)
                                        .filter(|&n| step4_at(&g, &mut step4, n) == MaterialId::AIR)
                                        .collect();
                                    let picked = table.has_tag(before, STONE_TAG)
                                        && !open.is_empty()
                                        && g.crystals.picks(c.x, c.y, c.z);
                                    let want = if picked { crystal } else { before };
                                    let got = chunk.storage.get(local(c - pos.origin_cell().0));
                                    assert_eq!(got, want, "run {k} seed {seed} {pos:?} cell {c}");
                                    let outside = |n: &IVec3| split(*n).0 != pos;
                                    if picked && open.iter().all(outside) {
                                        across[k] += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            across[0] > 0 && across[1] > 100,
            "crystals placed only through a chunk face: {across:?}"
        );
    }

    /// Whether world cell `c` lies in the bedrock shell `p` describes: inside
    /// the bounds, and in the bottom `floor_cells` layers or within
    /// `wall_cells` of an x or z edge.
    fn in_shell(p: &GenParams, c: IVec3) -> bool {
        let b = p.bounds.aabb();
        if !b.contains(CellPos(c)) {
            return false;
        }
        let (lo, hi) = (b.min.0, b.max.0);
        let w = p.bedrock.wall_cells;
        let wall = |a: usize| c[a] - lo[a] < w || hi[a] - c[a] < w;
        c.y - lo.y < p.bedrock.floor_cells || wall(0) || wall(2)
    }

    /// `p` with caves that reach the floor and a crystal on every
    /// candidate, so the shell has caves and crystals to overwrite.
    fn busy(p: &GenParams) -> GenParams {
        let mut p = crystals_everywhere(p);
        p.caves.blob.threshold = -0.3;
        p.caves.worm.radius = 0.5;
        p
    }

    #[test]
    fn bedrock_shell_complete() {
        let table = materials();
        let shipped = params();
        let bedrock = id(&table, &shipped.bedrock.material);
        let crystal = id(&table, &shipped.crystals.material);
        let def = table.get(bedrock).unwrap();
        assert!(def.is_solid() && !def.diggable, "bedrock can be dug");
        // SAD §5 default world; SAD §9 bottom 2 layers and edge columns.
        assert_eq!(shipped.bounds.aabb(), CellAabb::PHASE1_DEFAULT);
        let c = IVec3::new;
        for (cell, want) in [
            (c(100, 0, 100), true),
            (c(100, 1, 100), true),
            (c(100, 2, 100), false),
            (c(0, 64, 100), true),
            (c(255, 127, 0), true),
            (c(100, 127, 255), true),
            (c(1, 64, 1), false),
            (c(254, 100, 254), false),
            (c(100, -1, 100), false),
            (c(-1, 64, 100), false),
            (c(100, 128, 0), false),
        ] {
            assert_eq!(in_shell(&shipped, cell), want, "{cell}");
        }
        // A box that is not chunk aligned, with a thicker shell.
        let mut odd = busy(&shipped);
        odd.bounds = WorldBounds {
            min: [-20, -7, 5],
            max: [40, 50, 70],
        };
        odd.bedrock.floor_cells = 3;
        odd.bedrock.wall_cells = 2;
        // Bedrock tagged stone: crystals must still leave the shell alone.
        let stony_bedrock = {
            let text = std::fs::read_to_string(MATERIALS_PATH).unwrap();
            let from = r#"key: "bedrock", phase: Solid, tags: []"#;
            assert!(text.contains(from));
            let to = r#"key: "bedrock", phase: Solid, tags: ["stone"]"#;
            MaterialTable::from_ron_str(&text.replace(from, to)).unwrap()
        };

        let (mut shell, mut overwritten, mut crystals) = ([0usize; 3], [0usize; 3], [0usize; 3]);
        let runs = [
            (&shipped, 2, &table),
            (&busy(&shipped), 1 << 33, &table),
            (&odd, 0xB0770, &stony_bedrock),
        ];
        for (k, (p, seed, table)) in runs.into_iter().enumerate() {
            // The same world without a shell.
            let mut bare = p.clone();
            bare.bedrock.floor_cells = 0;
            bare.bedrock.wall_cells = 0;
            let g = Generator::new(seed, p, table).unwrap();
            let none = Generator::new(seed, &bare, table).unwrap();
            let mut world = BTreeMap::new();
            // Chunks on and just outside every x and z edge, top to
            // bottom and beyond, plus a column inside.
            let b = p.bounds.aabb();
            let (lo, hi) = (b.min.chunk().0, b.max.chunk().0);
            let mid = (lo + hi) / 2;
            let around = |a: usize| [lo[a] - 1, lo[a], hi[a], hi[a] + 1];
            let mut positions = Vec::new();
            for cy in lo.y - 1..=hi.y + 1 {
                for cx in around(0) {
                    for cz in around(2) {
                        positions.push(IVec3::new(cx, cy, cz));
                    }
                }
                positions.push(IVec3::new(mid.x, cy, mid.z));
            }
            for pos in positions.into_iter().map(ChunkPos) {
                let (pre, bare_pre) = (g.shell_volume(pos), none.shell_volume(pos));
                let o = pos.origin_cell().0;
                for x in -1..=CHUNK_SIZE {
                    for y in -1..=CHUNK_SIZE {
                        for z in -1..=CHUNK_SIZE {
                            let cell = o + IVec3::new(x, y, z);
                            let at = || format!("run {k} seed {seed} {pos:?} cell {cell}");
                            let (got, without) = (pre.get(x, y, z), bare_pre.get(x, y, z));
                            if in_shell(p, cell) {
                                assert_eq!(got, bedrock, "{}: hole in the shell", at());
                                shell[k] += 1;
                                overwritten[k] += usize::from(without == MaterialId::AIR);
                            } else {
                                assert_eq!(got, without, "{}: shell spilled", at());
                                assert_ne!(got, bedrock, "{}: stray bedrock", at());
                            }
                            let inner = [x, y, z].iter().all(|c| (0..CHUNK_SIZE).contains(c));
                            if !inner {
                                continue;
                            }
                            // In the finished chunk the shell stays whole
                            // and crystals still face air.
                            let fin = final_at(&g, &mut world, cell);
                            if in_shell(p, cell) {
                                assert_eq!(fin, bedrock, "{}: shell overwritten", at());
                            } else if fin == crystal {
                                crystals[k] += 1;
                                assert!(
                                    FACES.iter().any(|&f| {
                                        final_at(&g, &mut world, cell + f) == MaterialId::AIR
                                    }),
                                    "{}: crystal without air",
                                    at()
                                );
                            } else {
                                assert_eq!(fin, got, "{}", at());
                            }
                        }
                    }
                }
            }
        }
        assert!(
            shell.iter().all(|&n| n > 10_000),
            "shell cells checked: {shell:?}"
        );
        // Busy caves reach the floor and the walls; the shell closes them.
        assert!(
            overwritten[1] > 1000 && overwritten[2] > 1000,
            "cave cells under the shell: {overwritten:?}"
        );
        assert!(crystals[1] > 1000, "crystals near the shell: {crystals:?}");
    }

    #[test]
    fn border_continuity_20_random_pairs() {
        let table = materials();
        let shipped = params();
        let bedrock = id(&table, &shipped.bedrock.material);
        let crystal = id(&table, &shipped.crystals.material);
        let ores: Vec<MaterialId> = shipped
            .ores
            .iter()
            .map(|o| id(&table, &o.material))
            .collect();
        // 20 chunks drawn by a fixed hash from the world and the chunks just
        // outside it, each paired with its neighbour across one face; every
        // face direction comes up at least 3 times.
        let b = shipped.bounds.aabb();
        let (lo, hi) = (b.min.chunk().0, b.max.chunk().0);
        let pairs: Vec<(ChunkPos, IVec3)> = (0..20)
            .map(|i| {
                let pick = |axis: usize| {
                    let r = cell_hash(0x5EED, 0xF044, i, axis as i32, 0);
                    let span = (hi[axis] - lo[axis] + 3) as u64;
                    lo[axis] - 1 + (r % span) as i32
                };
                let a = IVec3::new(pick(0), pick(1), pick(2));
                (ChunkPos(a), FACES[i as usize % 6])
            })
            .collect();

        // Face cells carved by caves or holding an ore, a crystal or
        // bedrock: every step crosses faces.
        let (mut caves, mut ore, mut crystals, mut shell) = (0, 0, 0, 0);
        for p in [&shipped, &busy(&shipped)] {
            for seed in [6, 0xF00D_F00D] {
                let g = Generator::new(seed, p, &table).unwrap();
                let mut pre = BTreeMap::new();
                for &(a, d) in &pairs {
                    let bpos = ChunkPos(a.0 + d);
                    let (va, vb) = (g.shell_volume(a), g.shell_volume(bpos));
                    let (base_a, base_b) = (g.base_volume(a), g.base_volume(bpos));
                    let (ca, cb) = (g.generate(a), g.generate(bpos));
                    let axis = d.abs().max_position();
                    for u in 0..CHUNK_SIZE {
                        for w in 0..CHUNK_SIZE {
                            // Face cell of a, and the cell across in b.
                            let mut la = IVec3::ZERO;
                            la[axis] = if d[axis] > 0 { CHUNK_SIZE - 1 } else { 0 };
                            la[(axis + 1) % 3] = u;
                            la[(axis + 2) % 3] = w;
                            let lb = la + d - d * CHUNK_SIZE;
                            let at = || format!("seed {seed} {a:?} -> {bpos:?} local {la}");
                            // Each volume's border holds the other chunk's cell.
                            let (ba, bb) = (la + d, lb - d);
                            let a_border = va.get(ba.x, ba.y, ba.z);
                            assert_eq!(a_border, vb.get(lb.x, lb.y, lb.z), "{}", at());
                            let b_border = vb.get(bb.x, bb.y, bb.z);
                            assert_eq!(b_border, va.get(la.x, la.y, la.z), "{}", at());
                            let base_border = base_a.get(ba.x, ba.y, ba.z);
                            assert_eq!(base_border, base_b.get(lb.x, lb.y, lb.z), "{}", at());
                            // The finished cells on both sides follow from the
                            // pre-crystal world, read from the owning chunks.
                            for (chunk, pos, l, vol, base) in
                                [(&ca, a, la, &va, &base_a), (&cb, bpos, lb, &vb, &base_b)]
                            {
                                let c = pos.origin_cell().0 + l;
                                let before = step4_at(&g, &mut pre, c);
                                assert_eq!(before, vol.get(l.x, l.y, l.z), "{}", at());
                                let open = FACES
                                    .iter()
                                    .any(|&f| step4_at(&g, &mut pre, c + f) == MaterialId::AIR);
                                let picked = table.has_tag(before, STONE_TAG)
                                    && open
                                    && g.crystals.picks(c.x, c.y, c.z);
                                let want = if picked { crystal } else { before };
                                let got = chunk.storage.get(local(l));
                                assert_eq!(got, want, "{} cell {c}", at());
                                let solid = base.get(l.x, l.y, l.z) != MaterialId::AIR;
                                caves += usize::from(solid && before == MaterialId::AIR);
                                ore += usize::from(ores.contains(&got));
                                crystals += usize::from(got == crystal);
                                shell += usize::from(got == bedrock);
                            }
                        }
                    }
                }
            }
        }
        assert!(
            caves > 100 && ore > 100 && crystals > 100 && shell > 1000,
            "face cells: {caves} cave, {ore} ore, {crystals} crystal, {shell} bedrock"
        );
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
        p.ores[1].material = "granite".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::MissingTag { tag: ORE_TAG, .. })
        ));
        let mut p = params();
        p.ores[2].material = "gold_ore".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::UnknownMaterial { ref key, .. }) if key == "gold_ore"
        ));
        let mut p = params();
        p.crystals.material = "stone".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::MissingTag {
                tag: CRYSTAL_TAG,
                ..
            })
        ));
        let mut p = params();
        p.crystals.material = "ruby".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::UnknownMaterial { ref key, .. }) if key == "ruby"
        ));
        for key in ["stone", "air"] {
            let mut p = params();
            p.bedrock.material = key.into();
            assert!(
                matches!(
                    Generator::new(1, &p, &table),
                    Err(GenError::Diggable { key: ref k, .. }) if k == key
                ),
                "{key}"
            );
        }
        let mut p = params();
        p.bedrock.material = "obsidian".into();
        assert!(matches!(
            Generator::new(1, &p, &table),
            Err(GenError::UnknownMaterial { ref key, .. }) if key == "obsidian"
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

    #[test]
    fn generate_world_256_under_10s_dev() {
        let table = materials();
        let p = params();
        assert_eq!(p.bounds.aabb(), CellAabb::PHASE1_DEFAULT);
        let expected: Vec<ChunkPos> = CellAabb::PHASE1_DEFAULT.chunks().collect();
        assert_eq!(expected.len(), 256);

        let start = std::time::Instant::now();
        let g = Generator::new(45, &p, &table).expect("shipped params fit the table");
        let world = g.generate_world();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "256 chunks took {elapsed:?} on {} rayon threads, bound 10 s",
            rayon::current_num_threads()
        );
        let positions: Vec<ChunkPos> = world.iter().map(|&(pos, _)| pos).collect();
        assert_eq!(positions, expected, "chunks in the bounds' (y, z, x) order");
        assert_eq!(g.bounds(), CellAabb::PHASE1_DEFAULT);

        // The world is a real one: unmodified revision-0 chunks that fill
        // a World, with every step of SAD §9 present somewhere.
        let mut w = rmc_world::world::World::new(
            std::sync::Arc::new(table.clone()),
            CellAabb::PHASE1_DEFAULT,
        );
        let mut seen = BTreeSet::new();
        for (pos, chunk) in &world {
            assert_eq!(chunk.revision, 0, "{pos:?}");
            assert!(!chunk.modified, "{pos:?}");
            for i in 0..CHUNK_VOLUME as u16 {
                seen.insert(chunk.storage.get(LocalPos::from_index(i)));
            }
            assert!(w.insert_chunk(*pos, chunk.clone()).unwrap().is_none());
        }
        for key in [
            "air",
            "topsoil",
            "dirt",
            "stone",
            "granite",
            "basalt",
            "coal_seam",
            "copper_ore",
            "iron_ore",
            "glowcrystal",
            "bedrock",
        ] {
            assert!(seen.contains(&id(&table, key)), "no {key} in the world");
        }

        // Parallel output equals sequential generation and does not depend
        // on the thread count or scheduling: the same chunks from a
        // 3-thread pool, and a hash-picked sample generated one by one.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        let again = pool.install(|| g.generate_world());
        assert!(again == world, "3-thread pool gave a different world");
        let reversed: Vec<ChunkPos> = expected.iter().rev().copied().collect();
        let backwards = g.generate_chunks(reversed.iter().copied());
        assert_eq!(backwards.len(), 256);
        for ((pos, chunk), want) in backwards.iter().zip(&reversed) {
            assert_eq!(pos, want, "generate_chunks keeps the order given");
            let i = expected.iter().position(|p| p == pos).unwrap();
            assert!(*chunk == world[i].1, "{pos:?} differs by order");
        }
        let sample: BTreeSet<usize> = (0..32u64)
            .map(|k| (cell_hash(45, 0x45, k as i32, 0, 0) % 256) as usize)
            .collect();
        assert!(sample.len() >= 16, "sample of {}", sample.len());
        for i in sample {
            let (pos, chunk) = &world[i];
            assert!(*chunk == g.generate(*pos), "{pos:?} differs from generate");
        }
        assert!(g.generate_chunks([]).is_empty());
    }
}
