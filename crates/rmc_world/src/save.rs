//! Save format v2 (SAD §10, SAD-P2 §3): a RON header naming everything the
//! world was built from, plus the saved chunks as
//! `zstd(postcard(Vec<SavedChunk>))`, sorted by position. A chunk is saved
//! when its cells or its ecology state (layers, plants) differ from worldgen
//! output.
//!
//! Loading generates the world from the seed (the caller supplies the
//! generator, since worldgen sits above this crate) and then overwrites it
//! with the saved chunks. Any header mismatch fails the load; a save of
//! another format version (Phase 1 saves are version 1) fails with a version
//! message.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chunk::{Chunk, ChunkStorage};
use crate::coords::{CellAabb, ChunkPos};
use crate::edit::ChunkDelta;
use crate::layers::{ChunkLayers, PlantState};
use crate::world::{DeltaError, World};

/// Version of the on-disk layout. Bump on any change to the header or chunk
/// encoding. Version 2 adds `tables_hash` and per-chunk ecology state.
pub const SAVE_FORMAT_VERSION: u32 = 2;
/// Header file name inside a save directory.
pub const HEADER_FILE: &str = "header.ron";
/// Chunk file name inside a save directory.
pub const CHUNKS_FILE: &str = "chunks.bin";

/// zstd level for `chunks.bin`. The default level: fast, and paletted chunks
/// compress well at it.
const ZSTD_LEVEL: i32 = 3;

/// What a saved world was built from. A save loads only into a world built
/// from the same values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveHeader {
    pub format_version: u32,
    pub seed: u64,
    pub bounds: CellAabb,
    /// [`MaterialTable::hash`](crate::materials::MaterialTable::hash).
    pub material_hash: u64,
    /// Hash of the Phase 2 data tables (`rmc_eco::EcoTables::hash`), which
    /// decide what the saved ecology state means.
    pub tables_hash: u64,
    /// Hash of the worldgen parameters (`assets/worldgen.ron`).
    pub gen_params_hash: u64,
    /// Version of the noise crate worldgen samples, since its output decides
    /// every unmodified chunk.
    pub noise_crate_version: String,
}

/// The one header field every format version shares, read before the rest
/// so a save of another version fails with a version message rather than a
/// parse error.
#[derive(Deserialize)]
struct VersionProbe {
    format_version: u32,
}

impl SaveHeader {
    /// A header at the current [`SAVE_FORMAT_VERSION`].
    pub fn new(
        seed: u64,
        bounds: CellAabb,
        material_hash: u64,
        tables_hash: u64,
        gen_params_hash: u64,
        noise_crate_version: impl Into<String>,
    ) -> Self {
        Self {
            format_version: SAVE_FORMAT_VERSION,
            seed,
            bounds,
            material_hash,
            tables_hash,
            gen_params_hash,
            noise_crate_version: noise_crate_version.into(),
        }
    }

    /// Fails with the first field (in declaration order) where `found`
    /// differs from `self`.
    fn check(&self, found: &SaveHeader) -> Result<(), SaveError> {
        same(
            HeaderField::FormatVersion,
            &self.format_version,
            &found.format_version,
        )?;
        same(HeaderField::Seed, &self.seed, &found.seed)?;
        same(HeaderField::Bounds, &self.bounds, &found.bounds)?;
        same(
            HeaderField::MaterialHash,
            &self.material_hash,
            &found.material_hash,
        )?;
        same(
            HeaderField::TablesHash,
            &self.tables_hash,
            &found.tables_hash,
        )?;
        same(
            HeaderField::GenParamsHash,
            &self.gen_params_hash,
            &found.gen_params_hash,
        )?;
        same(
            HeaderField::NoiseCrateVersion,
            &self.noise_crate_version,
            &found.noise_crate_version,
        )
    }

    /// Fails if `world` was not built with this header's bounds and
    /// material table.
    fn check_world(&self, world: &World) -> Result<(), SaveError> {
        if world.bounds() != self.bounds {
            return Err(SaveError::WorldMismatch(HeaderField::Bounds));
        }
        if world.materials().hash() != self.material_hash {
            return Err(SaveError::WorldMismatch(HeaderField::MaterialHash));
        }
        Ok(())
    }
}

/// [`SaveError::HeaderMismatch`] unless `expected == found`.
fn same<T: PartialEq + Debug>(
    field: HeaderField,
    expected: &T,
    found: &T,
) -> Result<(), SaveError> {
    if expected == found {
        Ok(())
    } else {
        Err(SaveError::HeaderMismatch {
            field,
            expected: format!("{expected:?}"),
            found: format!("{found:?}"),
        })
    }
}

/// A [`SaveHeader`] field, named in mismatch errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderField {
    FormatVersion,
    Seed,
    Bounds,
    MaterialHash,
    TablesHash,
    GenParamsHash,
    NoiseCrateVersion,
}

/// One saved chunk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedChunk {
    pub pos: ChunkPos,
    /// The chunk's revision. With `storage` `None` it is the generated
    /// chunk's, and loading leaves the generated revision in place.
    pub revision: u64,
    /// Compacted cells; `None` when the cells are as generated (the chunk is
    /// not `modified`) and only its ecology state is saved.
    pub storage: Option<ChunkStorage>,
    pub eco_revision: u64,
    /// Compacted.
    pub layers: ChunkLayers,
    pub plants: BTreeMap<u16, PlantState>,
}

/// Why a save or load failed. A failed load returns no world.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("save i/o on {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("save header {path} does not parse: {source}")]
    HeaderParse {
        path: PathBuf,
        source: Box<ron::error::SpannedError>,
    },
    #[error("save header does not encode: {0}")]
    HeaderEncode(Box<ron::Error>),
    #[error("{}", mismatch_message(*field, expected, found))]
    HeaderMismatch {
        field: HeaderField,
        expected: String,
        found: String,
    },
    #[error("world does not match the save header in {0:?}")]
    WorldMismatch(HeaderField),
    #[error("{CHUNKS_FILE} does not decompress: {0}")]
    Decompress(std::io::Error),
    #[error("{CHUNKS_FILE} does not decode: {0}")]
    ChunkDecode(postcard::Error),
    #[error("chunks do not encode: {0}")]
    ChunkEncode(postcard::Error),
    #[error("saved chunks are not strictly ascending at {0:?}")]
    UnsortedChunks(ChunkPos),
    #[error("saved chunk refused: {0}")]
    BadChunk(DeltaError),
}

/// The [`SaveError::HeaderMismatch`] message. A format version mismatch says
/// which version the save has and which this build reads.
fn mismatch_message(field: HeaderField, expected: &str, found: &str) -> String {
    match field {
        HeaderField::FormatVersion => format!(
            "save format version {found} is not supported: this build reads save format \
             version {expected} only"
        ),
        _ => format!("save header {field:?} mismatch: expected {expected}, save has {found}"),
    }
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> SaveError + '_ {
    move |source| SaveError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Whether `chunk` differs from worldgen output, so it is saved: its cells
/// changed, or its layers or plants did (every such change bumps
/// `eco_revision`).
fn is_saved(chunk: &Chunk) -> bool {
    chunk.modified || chunk.eco_revision != 0
}

/// The world's saved chunks (see [`is_saved`]) in [`ChunkPos`] order,
/// storage and layers compacted. This is what [`save`] writes; the host can
/// take it on its own thread and write it elsewhere with [`write`].
pub fn snapshot(world: &World) -> Vec<SavedChunk> {
    world
        .iter_chunks_sorted()
        .filter(|(_, chunk)| is_saved(chunk))
        .map(|(pos, chunk)| {
            let storage = chunk.modified.then(|| {
                let mut storage = chunk.storage.clone();
                storage.compact();
                storage
            });
            let mut layers = chunk.layers.clone();
            layers.compact();
            SavedChunk {
                pos,
                revision: chunk.revision,
                storage,
                eco_revision: chunk.eco_revision,
                layers,
                plants: chunk.plants.clone(),
            }
        })
        .collect()
}

/// Writes `world`'s saved chunks under `header` into `dir`, creating it if
/// needed and replacing any save there. Returns the number of chunks
/// written. Fails without writing if the world's bounds or material table
/// differ from the header.
pub fn save(dir: &Path, header: &SaveHeader, world: &World) -> Result<usize, SaveError> {
    header.check_world(world)?;
    let chunks = snapshot(world);
    write(dir, header, &chunks)?;
    Ok(chunks.len())
}

/// Writes a save directory from a [`snapshot`]. `chunks` must be strictly
/// ascending by position. Each file is written beside its final name and
/// renamed into place, chunks first, so an interrupted save leaves complete
/// files.
pub fn write(dir: &Path, header: &SaveHeader, chunks: &[SavedChunk]) -> Result<(), SaveError> {
    check_sorted(chunks)?;
    let header_text = ron::ser::to_string_pretty(header, ron::ser::PrettyConfig::default())
        .map_err(|e| SaveError::HeaderEncode(Box::new(e)))?;
    let encoded = postcard::to_stdvec(chunks).map_err(SaveError::ChunkEncode)?;
    let compressed = zstd::encode_all(encoded.as_slice(), ZSTD_LEVEL).map_err(io_err(dir))?;

    fs::create_dir_all(dir).map_err(io_err(dir))?;
    replace_file(&dir.join(CHUNKS_FILE), &compressed)?;
    replace_file(&dir.join(HEADER_FILE), header_text.as_bytes())?;
    tracing::info!(
        dir = %dir.display(),
        chunks = chunks.len(),
        bytes = compressed.len(),
        "world saved"
    );
    Ok(())
}

/// Writes `bytes` to `path` through a temporary file in the same directory.
fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes).map_err(io_err(&tmp))?;
    fs::rename(&tmp, path).map_err(io_err(path))
}

/// Reads the header of the save in `dir`. A header of another format
/// version (a Phase 1 save, say) fails with a
/// [`HeaderField::FormatVersion`] mismatch before the rest of it is parsed.
pub fn read_header(dir: &Path) -> Result<SaveHeader, SaveError> {
    let path = dir.join(HEADER_FILE);
    let text = fs::read_to_string(&path).map_err(io_err(&path))?;
    let parse_err = |source| SaveError::HeaderParse {
        path: path.clone(),
        source: Box::new(source),
    };
    let probe: VersionProbe = ron::from_str(&text).map_err(parse_err)?;
    same(
        HeaderField::FormatVersion,
        &SAVE_FORMAT_VERSION,
        &probe.format_version,
    )?;
    ron::from_str(&text).map_err(parse_err)
}

/// Reads the chunks of the save in `dir`, checking they are strictly
/// ascending. The storage is not validated here; [`load`] does that.
pub fn read_chunks(dir: &Path) -> Result<Vec<SavedChunk>, SaveError> {
    let path = dir.join(CHUNKS_FILE);
    let compressed = fs::read(&path).map_err(io_err(&path))?;
    let encoded = zstd::decode_all(compressed.as_slice()).map_err(SaveError::Decompress)?;
    let chunks: Vec<SavedChunk> = postcard::from_bytes(&encoded).map_err(SaveError::ChunkDecode)?;
    check_sorted(&chunks)?;
    Ok(chunks)
}

fn check_sorted(chunks: &[SavedChunk]) -> Result<(), SaveError> {
    match chunks.windows(2).find(|w| w[0].pos >= w[1].pos) {
        Some(w) => Err(SaveError::UnsortedChunks(w[1].pos)),
        None => Ok(()),
    }
}

/// Loads the save in `dir`. Its header must equal `expected` field for
/// field. Then `generate` builds the world from the seed (it is not called
/// if the save is refused before that), and the world must match the
/// header's bounds and material table. Each saved chunk then overwrites the
/// generated one:
/// - saved cells go through [`World::apply_delta`] as a `Full` delta: the
///   storage must be well formed, in bounds and use only known materials,
///   and the chunk keeps its saved revision and is marked modified;
/// - its ecology state goes through [`World::apply_eco_snapshot`]: the
///   chunk takes the saved layers, plants and eco revision.
///
/// Damage starts empty.
pub fn load(
    dir: &Path,
    expected: &SaveHeader,
    generate: impl FnOnce() -> World,
) -> Result<World, SaveError> {
    let found = read_header(dir)?;
    expected.check(&found)?;
    let chunks = read_chunks(dir)?;
    let mut world = generate();
    expected.check_world(&world)?;
    let count = chunks.len();
    for SavedChunk {
        pos,
        revision,
        storage,
        eco_revision,
        layers,
        plants,
    } in chunks
    {
        if let Some(storage) = storage {
            world
                .apply_delta(&ChunkDelta::Full {
                    pos,
                    revision,
                    storage,
                })
                .map_err(SaveError::BadChunk)?;
        }
        world
            .apply_eco_snapshot(pos, eco_revision, layers, plants)
            .map_err(SaveError::BadChunk)?;
    }
    tracing::info!(dir = %dir.display(), chunks = count, "world loaded");
    Ok(world)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::chunk::Chunk;
    use crate::coords::{CHUNK_VOLUME, CellPos, LocalPos};
    use crate::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use crate::layers::LayerId;
    use crate::materials::{MaterialDef, MaterialId, MaterialTable};
    use crate::shape::Shape;

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const GEN_HASH: u64 = 0xC0FF_EE00_1234_5678;
    const TABLES_HASH: u64 = 0x7AB1_E500_8765_4321;
    const NOISE: &str = "1.1.1";

    fn phase1() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(PHASE1_PATH).expect("assets/materials.ron loads"))
    }

    fn id(table: &MaterialTable, key: &str) -> MaterialId {
        table
            .id(key)
            .unwrap_or_else(|| panic!("material {key} in table"))
    }

    /// 3 × 2 × 2 chunks, negative coordinates included.
    fn bounds() -> CellAabb {
        CellAabb::new(CellPos::new(-32, -32, -32), CellPos::new(63, 31, 31))
    }

    fn header(seed: u64, table: &MaterialTable) -> SaveHeader {
        SaveHeader::new(seed, bounds(), table.hash(), TABLES_HASH, GEN_HASH, NOISE)
    }

    /// Stand-in for worldgen: the lower layer is stone with seed-dependent
    /// granite and copper cells, the upper layer air with a dirt floor in
    /// one chunk. Every chunk unmodified at revision 0.
    fn generate(seed: u64, table: Arc<MaterialTable>) -> World {
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let copper = id(&table, "copper_ore");
        let dirt = id(&table, "dirt");
        let mut w = World::new(table, bounds());
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        for pos in bounds().chunks() {
            let mut storage = if pos.0.y < 0 {
                ChunkStorage::Uniform(stone)
            } else {
                ChunkStorage::Uniform(MaterialId::AIR)
            };
            if pos.0.y < 0 {
                for k in 0..200u32 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let index = (state % CHUNK_VOLUME as u64) as u16;
                    let m = if k % 5 == 0 { copper } else { granite };
                    storage.set(LocalPos::from_index(index), m);
                }
            } else if pos == ChunkPos::new(0, 0, 0) {
                for x in 0..32 {
                    for z in 0..32 {
                        storage.set(LocalPos::new(x, 0, z), dirt);
                    }
                }
            }
            w.insert_chunk(pos, Chunk::new(storage)).unwrap();
        }
        w
    }

    fn cmd(seq: u32, op: EditOp, shape: Shape) -> EditCommand {
        let issuer = PlayerId(1);
        EditCommand {
            id: CommandId { issuer, seq },
            issuer,
            op,
            shape,
        }
    }

    fn cell(x: i32, y: i32, z: i32) -> CellPos {
        CellPos::new(x, y, z)
    }

    fn apply(w: &mut World, c: &EditCommand) {
        let r = w.apply(c);
        assert_eq!(r.status, EditStatus::Applied, "{c:?}");
    }

    /// Edits that touch some chunks and leave others unmodified: a set across
    /// a chunk border, a fill big enough for a Full delta, broken granite,
    /// a remove, a paint, and pending damage that breaks nothing.
    fn edit(w: &mut World) {
        let t = w.materials().clone();
        let brass = id(&t, "brass_plate");
        let basalt = id(&t, "basalt");
        // Across x = 0 / x = -1 in the lower layer.
        apply(
            w,
            &cmd(
                1,
                EditOp::Set(brass),
                Shape::Box {
                    min: cell(-2, -5, -5),
                    max: cell(1, -4, -4),
                },
            ),
        );
        // 32 × 8 × 32 = 8192 cells in chunk (1, -1, 0): a Full delta.
        apply(
            w,
            &cmd(
                2,
                EditOp::Set(basalt),
                Shape::Box {
                    min: cell(32, -32, 0),
                    max: cell(63, -25, 31),
                },
            ),
        );
        // Break everything in a small box in chunk (0, -1, -1).
        for seq in 3..30 {
            w.apply(&cmd(
                seq,
                EditOp::Damage(u16::MAX),
                Shape::Sphere {
                    center: cell(10, -10, -10),
                    radius: 3,
                },
            ));
        }
        apply(w, &cmd(30, EditOp::Remove, Shape::Cell(cell(-20, -1, 5))));
        apply(
            w,
            &cmd(
                31,
                EditOp::Paint(basalt),
                Shape::Box {
                    min: cell(0, 0, 0),
                    max: cell(3, 0, 3),
                },
            ),
        );
        // Pending damage in chunk (-1, -1, -1): no break, no modification.
        apply(
            w,
            &cmd(32, EditOp::Damage(1), Shape::Cell(cell(-5, -5, -5))),
        );
        assert!(!w.damage().is_empty());
    }

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("rmc_save_{name}_{}_{n}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        out.sort();
        out
    }

    /// The saved record for a chunk at `revision` holding `storage`.
    fn raw_chunk(pos: ChunkPos, revision: u64, storage: ChunkStorage) -> SavedChunk {
        SavedChunk {
            pos,
            revision,
            storage: Some(storage),
            eco_revision: 0,
            layers: ChunkLayers::default(),
            plants: BTreeMap::new(),
        }
    }

    /// Writes `chunks.bin` bytes for `chunks` directly, bypassing the
    /// checks in [`write`].
    fn write_raw_chunks(dir: &Path, chunks: &[(ChunkPos, u64, ChunkStorage)]) {
        let chunks: Vec<SavedChunk> = chunks
            .iter()
            .map(|(pos, revision, storage)| raw_chunk(*pos, *revision, storage.clone()))
            .collect();
        let encoded = postcard::to_stdvec(&chunks).unwrap();
        let compressed = zstd::encode_all(encoded.as_slice(), 3).unwrap();
        fs::write(dir.join(CHUNKS_FILE), compressed).unwrap();
    }

    /// Decodes `chunks.bin` bytes by hand (zstd, then postcard) into
    /// position, revision and storage per saved chunk.
    fn decode_raw_chunks(raw: &[u8]) -> Vec<(ChunkPos, u64, ChunkStorage)> {
        let chunks: Vec<SavedChunk> =
            postcard::from_bytes(&zstd::decode_all(raw).unwrap()).unwrap();
        chunks
            .into_iter()
            .map(|c| (c.pos, c.revision, c.storage.expect("cells saved")))
            .collect()
    }

    fn assert_same_chunks(a: &World, b: &World) {
        for pos in bounds().chunks() {
            let (ca, cb) = (a.chunk(pos).unwrap(), b.chunk(pos).unwrap());
            assert_eq!(ca.hash(), cb.hash(), "{pos:?}");
            assert_eq!(ca.revision, cb.revision, "{pos:?}");
            assert_eq!(ca.modified, cb.modified, "{pos:?}");
        }
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn roundtrip_hash_identical() {
        let t = phase1();
        let dir = TempDir::new("roundtrip");
        let mut w = generate(7, t.clone());
        let unedited = w.hash();
        edit(&mut w);
        assert_ne!(w.hash(), unedited);

        let written = save(dir.path(), &header(7, &t), &w).unwrap();
        assert_eq!(
            written,
            w.iter_chunks_sorted().filter(|(_, c)| c.modified).count()
        );
        assert!((4..12).contains(&written), "{written}");

        let loaded = load(dir.path(), &header(7, &t), || generate(7, t.clone())).unwrap();
        assert_same_chunks(&w, &loaded);
        // Damage is not saved.
        assert!(loaded.damage().is_empty());
        // Loaded chunks hold the compacted storage.
        for (pos, c) in w.iter_chunks_sorted().filter(|(_, c)| c.modified) {
            let mut s = c.storage.clone();
            s.compact();
            assert_eq!(loaded.chunk(pos).unwrap().storage, s, "{pos:?}");
        }

        // The same edits on an independently built world, and a re-save of
        // the loaded world, give byte-identical files.
        let mut w2 = generate(7, t.clone());
        edit(&mut w2);
        let dir2 = TempDir::new("roundtrip2");
        save(dir2.path(), &header(7, &t), &w2).unwrap();
        let dir3 = TempDir::new("roundtrip3");
        save(dir3.path(), &header(7, &t), &loaded).unwrap();
        assert_eq!(files(dir.path()), files(dir2.path()));
        assert_eq!(files(dir.path()), files(dir3.path()));

        // Editing continues identically after load: revisions were restored,
        // so the same command yields the same deltas.
        let mut loaded = loaded;
        let next = cmd(
            100,
            EditOp::Set(id(&t, "glowcrystal")),
            Shape::Sphere {
                center: cell(0, -1, 0),
                radius: 4,
            },
        );
        assert_eq!(w.apply(&next), loaded.apply(&next));
        assert_same_chunks(&w, &loaded);

        // Saving again into the same directory replaces the save.
        save(dir.path(), &header(7, &t), &w).unwrap();
        let reloaded = load(dir.path(), &header(7, &t), || generate(7, t.clone())).unwrap();
        assert_same_chunks(&w, &reloaded);
        assert_eq!(
            files(dir.path())
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>(),
            [CHUNKS_FILE, HEADER_FILE]
        );

        // An unedited world saves no chunks and loads back as generated.
        let fresh = generate(9, t.clone());
        let dir4 = TempDir::new("roundtrip4");
        assert_eq!(save(dir4.path(), &header(9, &t), &fresh).unwrap(), 0);
        let loaded = load(dir4.path(), &header(9, &t), || generate(9, t.clone())).unwrap();
        assert_same_chunks(&fresh, &loaded);
    }

    #[test]
    fn only_modified_chunks_written() {
        let t = phase1();
        let granite = id(&t, "granite");
        let dir = TempDir::new("modified");
        let mut w = generate(3, t.clone());
        edit(&mut w);
        // A chunk changed and changed back is still modified, so still saved.
        let c = cell(40, 10, -20);
        assert_eq!(w.get_cell(c), Some(MaterialId::AIR));
        apply(&mut w, &cmd(40, EditOp::Set(granite), Shape::Cell(c)));
        apply(&mut w, &cmd(41, EditOp::Remove, Shape::Cell(c)));
        // A rejected command modifies nothing.
        let r = w.apply(&cmd(
            42,
            EditOp::Set(MaterialId(999)),
            Shape::Cell(cell(-10, 10, -10)),
        ));
        assert_ne!(r.status, EditStatus::Applied);

        let expected: Vec<(ChunkPos, u64)> = w
            .iter_chunks_sorted()
            .filter(|(_, c)| c.modified)
            .map(|(p, c)| (p, c.revision))
            .collect();
        let mut want_positions = vec![
            ChunkPos::new(-1, -1, -1), // brass (z = -5..-4 is chunk z = -1)
            ChunkPos::new(0, -1, -1),  // brass + broken sphere
            ChunkPos::new(1, -1, 0),   // basalt fill (Full)
            ChunkPos::new(-1, -1, 0),  // remove
            ChunkPos::new(0, 0, 0),    // paint
            ChunkPos::new(1, 0, -1),   // set and restored
        ];
        want_positions.sort();
        assert_eq!(
            expected.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            want_positions
        );

        assert_eq!(save(dir.path(), &header(3, &t), &w).unwrap(), 6);

        // Decode chunks.bin by hand: zstd around postcard.
        let raw = fs::read(dir.path().join(CHUNKS_FILE)).unwrap();
        assert_eq!(&raw[..4], &[0x28, 0xB5, 0x2F, 0xFD], "zstd frame magic");
        let decoded = decode_raw_chunks(&raw);
        assert_eq!(
            decoded.iter().map(|(p, r, _)| (*p, *r)).collect::<Vec<_>>(),
            expected
        );
        for (pos, _, storage) in &decoded {
            let mut s = w.chunk(*pos).unwrap().storage.clone();
            s.compact();
            assert_eq!(storage, &s, "{pos:?} saved compacted");
        }
        // The restored chunk compacts back to Uniform air.
        assert_eq!(
            decoded
                .iter()
                .find(|(p, ..)| *p == ChunkPos::new(1, 0, -1))
                .unwrap()
                .2,
            ChunkStorage::Uniform(MaterialId::AIR)
        );

        // read_chunks agrees.
        let read = read_chunks(dir.path()).unwrap();
        assert_eq!(
            read.iter()
                .map(|c| (c.pos, c.revision, c.storage.clone().expect("cells saved")))
                .collect::<Vec<_>>(),
            decoded
        );

        // The header is readable RON.
        let text = fs::read_to_string(dir.path().join(HEADER_FILE)).unwrap();
        assert!(text.contains("seed"), "{text}");
        assert_eq!(ron::from_str::<SaveHeader>(&text).unwrap(), header(3, &t));
        assert_eq!(read_header(dir.path()).unwrap(), header(3, &t));

        // Only the two files remain (no temporaries).
        assert_eq!(
            files(dir.path())
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>(),
            [CHUNKS_FILE, HEADER_FILE]
        );

        // An unedited world writes an empty list.
        let dir2 = TempDir::new("modified2");
        save(dir2.path(), &header(3, &t), &generate(3, t.clone())).unwrap();
        let raw = fs::read(dir2.path().join(CHUNKS_FILE)).unwrap();
        assert!(decode_raw_chunks(&raw).is_empty());

        // snapshot is what save writes.
        assert_eq!(snapshot(&w), read);
    }

    #[test]
    fn header_mismatch_fails() {
        let t = phase1();
        let dir = TempDir::new("mismatch");
        let mut w = generate(7, t.clone());
        edit(&mut w);
        save(dir.path(), &header(7, &t), &w).unwrap();

        let good = header(7, &t);
        let mut other_bounds = good.clone();
        other_bounds.bounds = CellAabb::new(CellPos::new(-32, -32, -32), CellPos::new(63, 31, 63));
        let cases: Vec<(HeaderField, SaveHeader)> = vec![
            (
                HeaderField::FormatVersion,
                SaveHeader {
                    format_version: SAVE_FORMAT_VERSION + 1,
                    ..good.clone()
                },
            ),
            (
                HeaderField::Seed,
                SaveHeader {
                    seed: 8,
                    ..good.clone()
                },
            ),
            (HeaderField::Bounds, other_bounds),
            (
                HeaderField::MaterialHash,
                SaveHeader {
                    material_hash: good.material_hash ^ 1,
                    ..good.clone()
                },
            ),
            (
                HeaderField::GenParamsHash,
                SaveHeader {
                    gen_params_hash: GEN_HASH + 1,
                    ..good.clone()
                },
            ),
            (
                HeaderField::NoiseCrateVersion,
                SaveHeader {
                    noise_crate_version: "1.1.2".into(),
                    ..good.clone()
                },
            ),
        ];
        for (field, expected) in &cases {
            let called = Cell::new(false);
            let r = load(dir.path(), expected, || {
                called.set(true);
                generate(7, t.clone())
            });
            match r {
                Err(SaveError::HeaderMismatch { field: f, .. }) => assert_eq!(f, *field),
                other => panic!(
                    "{field:?}: expected mismatch, got {:?}",
                    other.map(|w| w.hash())
                ),
            }
            assert!(
                !called.get(),
                "{field:?}: generator ran before the header check"
            );
        }
        // The good header still loads.
        let loaded = load(dir.path(), &good, || generate(7, t.clone())).unwrap();
        assert_eq!(loaded.hash(), w.hash());

        // A header on disk from another format version fails too.
        let path = dir.path().join(HEADER_FILE);
        let on_disk = SaveHeader {
            format_version: 0,
            ..good.clone()
        };
        fs::write(
            &path,
            ron::ser::to_string_pretty(&on_disk, Default::default()).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            load(dir.path(), &good, || generate(7, t.clone())),
            Err(SaveError::HeaderMismatch {
                field: HeaderField::FormatVersion,
                ..
            })
        ));

        // An unparsable header fails.
        fs::write(&path, "(not a header").unwrap();
        assert!(matches!(
            load(dir.path(), &good, || generate(7, t.clone())),
            Err(SaveError::HeaderParse { .. })
        ));

        // A missing save fails.
        let missing = TempDir::new("mismatch_missing");
        assert!(matches!(
            load(missing.path(), &good, || generate(7, t.clone())),
            Err(SaveError::Io { .. })
        ));

        // The generated world must match the header too.
        save(dir.path(), &good, &w).unwrap();
        let small = CellAabb::new(CellPos::new(-32, -32, -32), CellPos::new(31, 31, 31));
        assert!(matches!(
            load(dir.path(), &good, || World::new(t.clone(), small)),
            Err(SaveError::WorldMismatch(HeaderField::Bounds))
        ));
        let mut defs: Vec<MaterialDef> = t.iter().cloned().collect();
        defs[4].hardness += 1;
        let t2 = Arc::new(MaterialTable::from_defs(defs).unwrap());
        assert_ne!(t2.hash(), t.hash());
        assert!(matches!(
            load(dir.path(), &good, || generate(7, t2.clone())),
            Err(SaveError::WorldMismatch(HeaderField::MaterialHash))
        ));

        // Saving under a header that does not describe the world writes
        // nothing.
        let dir2 = TempDir::new("mismatch_save");
        assert!(matches!(
            save(dir2.path(), &header(7, &t2), &w),
            Err(SaveError::WorldMismatch(HeaderField::MaterialHash))
        ));
        assert!(!dir2.path().exists());
    }

    #[test]
    fn corrupt_chunks_fail() {
        let t = phase1();
        let stone = id(&t, "stone");
        let dir = TempDir::new("corrupt");
        let w = generate(5, t.clone());
        save(dir.path(), &header(5, &t), &w).unwrap();
        let load_it = || load(dir.path(), &header(5, &t), || generate(5, t.clone()));
        let a = ChunkPos::new(0, -1, 0);
        let b = ChunkPos::new(1, -1, 0);

        // Not zstd, or truncated.
        let path = dir.path().join(CHUNKS_FILE);
        fs::write(&path, b"plain bytes").unwrap();
        assert!(matches!(load_it(), Err(SaveError::Decompress(_))));
        write_raw_chunks(dir.path(), &[(a, 1, ChunkStorage::Uniform(stone))]);
        let raw = fs::read(&path).unwrap();
        fs::write(&path, &raw[..raw.len() - 3]).unwrap();
        assert!(matches!(load_it(), Err(SaveError::Decompress(_))));
        // Valid zstd around bytes that are not the chunk list.
        fs::write(&path, zstd::encode_all(&[0xFFu8; 3][..], 3).unwrap()).unwrap();
        assert!(matches!(load_it(), Err(SaveError::ChunkDecode(_))));

        // Unsorted or duplicate positions.
        let s = ChunkStorage::Uniform(stone);
        write_raw_chunks(dir.path(), &[(b, 1, s.clone()), (a, 1, s.clone())]);
        assert!(matches!(load_it(), Err(SaveError::UnsortedChunks(p)) if p == a));
        write_raw_chunks(dir.path(), &[(a, 1, s.clone()), (a, 2, s.clone())]);
        assert!(matches!(load_it(), Err(SaveError::UnsortedChunks(p)) if p == a));
        assert!(matches!(
            write(
                dir.path(),
                &header(5, &t),
                &[raw_chunk(b, 1, s.clone()), raw_chunk(a, 1, s.clone())]
            ),
            Err(SaveError::UnsortedChunks(_))
        ));

        // Out of bounds, malformed storage, unknown material.
        write_raw_chunks(dir.path(), &[(ChunkPos::new(5, 0, 0), 1, s.clone())]);
        assert!(matches!(
            load_it(),
            Err(SaveError::BadChunk(DeltaError::OutOfBounds(_)))
        ));
        let malformed = ChunkStorage::Paletted {
            palette: vec![stone],
            bits: 3,
            data: vec![0; 10],
        };
        write_raw_chunks(dir.path(), &[(a, 1, malformed)]);
        assert!(matches!(
            load_it(),
            Err(SaveError::BadChunk(DeltaError::MalformedStorage(p))) if p == a
        ));
        write_raw_chunks(
            dir.path(),
            &[(a, 1, ChunkStorage::Uniform(MaterialId(999)))],
        );
        assert!(matches!(
            load_it(),
            Err(SaveError::BadChunk(DeltaError::UnknownMaterial { .. }))
        ));

        // Missing chunks.bin.
        fs::remove_file(&path).unwrap();
        assert!(matches!(load_it(), Err(SaveError::Io { .. })));

        // A hand-written valid list loads, keeping its revision.
        write_raw_chunks(dir.path(), &[(a, 17, ChunkStorage::Uniform(stone))]);
        let loaded = load_it().unwrap();
        let c = loaded.chunk(a).unwrap();
        assert_eq!((c.revision, c.modified), (17, true));
        assert_eq!(c.storage, ChunkStorage::Uniform(stone));
    }
    /// Ecology state on two chunks, the way the season tick leaves it: the
    /// edited chunk (0, -1, -1) and the unedited chunk (-1, 0, 0), whose
    /// cells stay as generated. Each gets temperature, moisture, a
    /// substance, a gas, an all-zero materialized layer (which the save
    /// compacts away) and plants, and its eco revision bumps.
    fn eco_edit(w: &mut World) -> [ChunkPos; 2] {
        let edited = ChunkPos::new(0, -1, -1);
        let unedited = ChunkPos::new(-1, 0, 0);
        assert!(w.chunk(edited).unwrap().modified);
        assert!(!w.chunk(unedited).unwrap().modified);
        for (k, pos) in [edited, unedited].into_iter().enumerate() {
            let k = k as u8;
            let chunk = w.chunk(pos).unwrap();
            let mut layers = chunk.layers.clone();
            let mut plants = chunk.plants.clone();
            for i in 0..CHUNK_VOLUME as u16 {
                let local = LocalPos::from_index(i);
                layers.set_i16(LayerId::Temperature, local, 150 - (i / 1024) as i16 * 7);
                if i % 3 == 0 {
                    layers.set_u8(LayerId::Moisture, local, (i % 251) as u8 + k);
                }
            }
            layers.set_u8(LayerId::Substance(3), LocalPos::new(4, 5, 6), 77 + k);
            layers.set_u8(LayerId::Gas(1), LocalPos::new(31, 31, 31), 9);
            layers.materialize(LayerId::Regolith);
            plants.insert(
                LocalPos::new(1, 2, 3).index(),
                PlantState {
                    species: 1 + k,
                    stress: 20,
                    age_seasons: 3,
                    held: [0, 4, 0, 0, 0, 0, 0, 0, 9, 1],
                },
            );
            plants.insert(LocalPos::new(30, 0, 2).index(), PlantState::default());
            let eco_revision = chunk.eco_revision + 5 + u64::from(k);
            w.apply_eco_snapshot(pos, eco_revision, layers, plants)
                .unwrap();
        }
        [edited, unedited]
    }

    fn assert_same_eco(a: &World, b: &World) {
        for pos in bounds().chunks() {
            let (ca, cb) = (a.chunk(pos).unwrap(), b.chunk(pos).unwrap());
            assert_eq!(ca.eco_revision, cb.eco_revision, "{pos:?}");
            assert_eq!(ca.layers, cb.layers, "{pos:?}");
            assert_eq!(ca.plants, cb.plants, "{pos:?}");
        }
    }

    #[test]
    fn v2_roundtrip_with_layers_and_plants_hash_identical() {
        let t = phase1();
        let dir = TempDir::new("v2_roundtrip");
        let mut w = generate(7, t.clone());
        edit(&mut w);
        let materials_only = w.hash();
        let [edited, unedited] = eco_edit(&mut w);
        assert_ne!(w.hash(), materials_only, "the world hash covers ecology");

        let modified = w.iter_chunks_sorted().filter(|(_, c)| c.modified).count();
        let written = save(dir.path(), &header(7, &t), &w).unwrap();
        assert_eq!(
            written,
            modified + 1,
            "plus the unedited chunk with eco state"
        );

        // The header carries the tables hash at format version 2.
        let read = read_header(dir.path()).unwrap();
        assert_eq!(read.format_version, 2);
        assert_eq!(read.tables_hash, TABLES_HASH);

        // chunks.bin by hand: zstd around postcard of the v2 records.
        let raw = fs::read(dir.path().join(CHUNKS_FILE)).unwrap();
        let decoded: Vec<SavedChunk> =
            postcard::from_bytes(&zstd::decode_all(raw.as_slice()).unwrap()).unwrap();
        assert_eq!(decoded, read_chunks(dir.path()).unwrap());
        assert_eq!(decoded, snapshot(&w));
        let record = |pos| decoded.iter().find(|c| c.pos == pos).unwrap();
        // The unedited chunk saves its eco state but not its cells.
        let u = record(unedited);
        assert_eq!(u.storage, None);
        assert_eq!(u.eco_revision, w.chunk(unedited).unwrap().eco_revision);
        assert_eq!(u.plants.len(), 2);
        // Layers are compacted: the all-zero Regolith layer is dropped.
        for pos in [edited, unedited] {
            let ids: Vec<LayerId> = record(pos).layers.ids().collect();
            assert_eq!(
                ids,
                [
                    LayerId::Temperature,
                    LayerId::Moisture,
                    LayerId::Substance(3),
                    LayerId::Gas(1)
                ],
                "{pos:?}"
            );
        }
        assert!(record(edited).storage.is_some());

        // Load: cells, revisions, modified flags, layers, plants and eco
        // revisions all match, so the world hash is identical.
        let loaded = load(dir.path(), &header(7, &t), || generate(7, t.clone())).unwrap();
        assert_same_chunks(&w, &loaded);
        assert_same_eco(&w, &loaded);
        assert_eq!(loaded.hash(), w.hash());
        assert!(!loaded.chunk(unedited).unwrap().modified);
        let probe = LocalPos::new(4, 5, 6);
        assert_eq!(
            loaded
                .chunk(edited)
                .unwrap()
                .layers
                .get_u8(LayerId::Substance(3), probe),
            77
        );
        assert_eq!(
            loaded
                .chunk(unedited)
                .unwrap()
                .layers
                .get_i16(LayerId::Temperature, LocalPos::from_index(5000)),
            150 - 4 * 7
        );

        // Saving the loaded world, or the same edits on an independently
        // built world, gives byte-identical files.
        let dir2 = TempDir::new("v2_roundtrip2");
        save(dir2.path(), &header(7, &t), &loaded).unwrap();
        assert_eq!(files(dir.path()), files(dir2.path()));
        let mut w2 = generate(7, t.clone());
        edit(&mut w2);
        eco_edit(&mut w2);
        let dir3 = TempDir::new("v2_roundtrip3");
        save(dir3.path(), &header(7, &t), &w2).unwrap();
        assert_eq!(files(dir.path()), files(dir3.path()));

        // Saved layers replace the generated ones entirely: a layer the
        // generated chunk has but the save does not reads 0 after load.
        let dir4 = TempDir::new("v2_roundtrip4");
        save(dir4.path(), &header(7, &t), &w).unwrap();
        let loaded = load(dir4.path(), &header(7, &t), || {
            let mut g = generate(7, t.clone());
            let mut layers = ChunkLayers::default();
            layers.set_u8(LayerId::Growth, LocalPos::new(0, 0, 0), 200);
            g.apply_eco_snapshot(unedited, 0, layers, BTreeMap::new())
                .unwrap();
            g
        })
        .unwrap();
        assert_eq!(loaded.hash(), w.hash());

        // A saved plant outside the chunk, or eco state for a chunk the
        // generator did not make resident, is refused.
        let mut bad = decoded.clone();
        bad[0]
            .plants
            .insert(CHUNK_VOLUME as u16, PlantState::default());
        write(dir.path(), &header(7, &t), &bad).unwrap();
        assert!(matches!(
            load(dir.path(), &header(7, &t), || generate(7, t.clone())),
            Err(SaveError::BadChunk(DeltaError::BadIndex { .. }))
        ));
        write(dir.path(), &header(7, &t), &decoded).unwrap();
        assert!(matches!(
            load(dir.path(), &header(7, &t), || {
                let generated = generate(7, t.clone());
                let mut g = World::new(t.clone(), bounds());
                for pos in bounds().chunks().filter(|&p| p != unedited) {
                    g.insert_chunk(pos, generated.chunk(pos).unwrap().clone())
                        .unwrap();
                }
                g
            }),
            Err(SaveError::BadChunk(DeltaError::NotResident(p))) if p == unedited
        ));
    }

    #[test]
    fn v1_rejected_with_version_message() {
        /// The Phase 1 header: no `tables_hash`.
        #[derive(Serialize)]
        struct V1Header {
            format_version: u32,
            seed: u64,
            bounds: CellAabb,
            material_hash: u64,
            gen_params_hash: u64,
            noise_crate_version: String,
        }
        let t = phase1();
        let dir = TempDir::new("v1");
        fs::create_dir_all(dir.path()).unwrap();
        let v1 = V1Header {
            format_version: 1,
            seed: 7,
            bounds: bounds(),
            material_hash: t.hash(),
            gen_params_hash: GEN_HASH,
            noise_crate_version: NOISE.into(),
        };
        fs::write(
            dir.path().join(HEADER_FILE),
            ron::ser::to_string_pretty(&v1, Default::default()).unwrap(),
        )
        .unwrap();
        // A v1 chunk list: (pos, revision, storage) per chunk.
        let stone = id(&t, "stone");
        let v1_chunks = vec![(ChunkPos::new(0, -1, 0), 1u64, ChunkStorage::Uniform(stone))];
        let encoded = postcard::to_stdvec(&v1_chunks).unwrap();
        fs::write(
            dir.path().join(CHUNKS_FILE),
            zstd::encode_all(encoded.as_slice(), 3).unwrap(),
        )
        .unwrap();

        let check = |e: SaveError| {
            match &e {
                SaveError::HeaderMismatch {
                    field: HeaderField::FormatVersion,
                    expected,
                    found,
                } => assert_eq!((expected.as_str(), found.as_str()), ("2", "1")),
                other => panic!("expected a format version mismatch, got {other:?}"),
            }
            let message = e.to_string();
            assert!(
                message.contains("save format version 1 is not supported")
                    && message.contains("version 2"),
                "{message}"
            );
        };
        check(read_header(dir.path()).unwrap_err());
        let called = Cell::new(false);
        let r = load(dir.path(), &header(7, &t), || {
            called.set(true);
            generate(7, t.clone())
        });
        check(r.map(|w| w.hash()).unwrap_err());
        assert!(!called.get(), "generator ran for a v1 save");

        // Any other version is refused the same way; the current one parses.
        let mut text = fs::read_to_string(dir.path().join(HEADER_FILE)).unwrap();
        text = text.replace("format_version: 1", "format_version: 3");
        fs::write(dir.path().join(HEADER_FILE), &text).unwrap();
        assert!(matches!(
            read_header(dir.path()),
            Err(SaveError::HeaderMismatch {
                field: HeaderField::FormatVersion,
                ..
            })
        ));
        save(dir.path(), &header(7, &t), &generate(7, t.clone())).unwrap();
        assert_eq!(read_header(dir.path()).unwrap(), header(7, &t));
    }

    #[test]
    fn tables_hash_mismatch_fails() {
        let t = phase1();
        let dir = TempDir::new("tables_hash");
        let mut w = generate(7, t.clone());
        edit(&mut w);
        eco_edit(&mut w);
        save(dir.path(), &header(7, &t), &w).unwrap();

        let other = SaveHeader {
            tables_hash: TABLES_HASH ^ 1,
            ..header(7, &t)
        };
        let called = Cell::new(false);
        let r = load(dir.path(), &other, || {
            called.set(true);
            generate(7, t.clone())
        });
        match r.map(|w| w.hash()) {
            Err(
                e @ SaveError::HeaderMismatch {
                    field: HeaderField::TablesHash,
                    ..
                },
            ) => assert!(e.to_string().contains("TablesHash"), "{e}"),
            other => panic!("expected a tables hash mismatch, got {other:?}"),
        }
        assert!(!called.get(), "generator ran before the header check");

        // The matching tables hash loads.
        let loaded = load(dir.path(), &header(7, &t), || generate(7, t.clone())).unwrap();
        assert_eq!(loaded.hash(), w.hash());
    }
}
