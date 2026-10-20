//! Save format (SAD §10): a RON header naming everything the world was built
//! from, plus the modified chunks as `zstd(postcard(Vec<(ChunkPos, revision,
//! ChunkStorage)>))`, sorted by position.
//!
//! Loading generates the world from the seed (the caller supplies the
//! generator, since worldgen sits above this crate) and then overwrites it
//! with the saved chunks. Any header mismatch fails the load.

use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chunk::ChunkStorage;
use crate::coords::{CellAabb, ChunkPos};
use crate::edit::ChunkDelta;
use crate::world::{DeltaError, World};

/// Version of the on-disk layout. Bump on any change to the header or chunk
/// encoding.
pub const SAVE_FORMAT_VERSION: u32 = 1;
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
    /// Hash of the worldgen parameters (`assets/worldgen.ron`).
    pub gen_params_hash: u64,
    /// Version of the noise crate worldgen samples, since its output decides
    /// every unmodified chunk.
    pub noise_crate_version: String,
}

impl SaveHeader {
    /// A header at the current [`SAVE_FORMAT_VERSION`].
    pub fn new(
        seed: u64,
        bounds: CellAabb,
        material_hash: u64,
        gen_params_hash: u64,
        noise_crate_version: impl Into<String>,
    ) -> Self {
        Self {
            format_version: SAVE_FORMAT_VERSION,
            seed,
            bounds,
            material_hash,
            gen_params_hash,
            noise_crate_version: noise_crate_version.into(),
        }
    }

    /// Fails with the first field (in declaration order) where `found`
    /// differs from `self`.
    fn check(&self, found: &SaveHeader) -> Result<(), SaveError> {
        fn same<T: PartialEq + Debug>(field: HeaderField, e: &T, f: &T) -> Result<(), SaveError> {
            if e == f {
                Ok(())
            } else {
                Err(SaveError::HeaderMismatch {
                    field,
                    expected: format!("{e:?}"),
                    found: format!("{f:?}"),
                })
            }
        }
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

/// A [`SaveHeader`] field, named in mismatch errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderField {
    FormatVersion,
    Seed,
    Bounds,
    MaterialHash,
    GenParamsHash,
    NoiseCrateVersion,
}

/// One saved chunk. Encodes exactly like the tuple `(ChunkPos, u64,
/// ChunkStorage)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedChunk {
    pub pos: ChunkPos,
    pub revision: u64,
    /// Compacted.
    pub storage: ChunkStorage,
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
    #[error("save header {field:?} mismatch: expected {expected}, save has {found}")]
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

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> SaveError + '_ {
    move |source| SaveError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The world's modified chunks in [`ChunkPos`] order, storage compacted.
/// This is what [`save`] writes; the host can take it on its own thread and
/// write it elsewhere with [`write`].
pub fn snapshot(world: &World) -> Vec<SavedChunk> {
    world
        .iter_chunks_sorted()
        .filter(|(_, chunk)| chunk.modified)
        .map(|(pos, chunk)| {
            let mut storage = chunk.storage.clone();
            storage.compact();
            SavedChunk {
                pos,
                revision: chunk.revision,
                storage,
            }
        })
        .collect()
}

/// Writes `world`'s modified chunks under `header` into `dir`, creating it
/// if needed and replacing any save there. Returns the number of chunks
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

/// Reads the header of the save in `dir`.
pub fn read_header(dir: &Path) -> Result<SaveHeader, SaveError> {
    let path = dir.join(HEADER_FILE);
    let text = fs::read_to_string(&path).map_err(io_err(&path))?;
    ron::from_str(&text).map_err(|source| SaveError::HeaderParse {
        path,
        source: Box::new(source),
    })
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
/// if the save is refused before that), the world must match the header's
/// bounds and material table, and every saved chunk replaces the generated
/// one through [`World::apply_delta`] as a `Full` delta: the storage must be
/// well formed, in bounds and use only known materials, and the chunk keeps
/// its saved revision and is marked modified. Damage starts empty.
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
    } in chunks
    {
        world
            .apply_delta(&ChunkDelta::Full {
                pos,
                revision,
                storage,
            })
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
    use crate::materials::{MaterialDef, MaterialId, MaterialTable};
    use crate::shape::Shape;

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const GEN_HASH: u64 = 0xC0FF_EE00_1234_5678;
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
        SaveHeader::new(seed, bounds(), table.hash(), GEN_HASH, NOISE)
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
            storage,
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
            .map(|c| (c.pos, c.revision, c.storage))
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
                .map(|c| (c.pos, c.revision, c.storage.clone()))
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
                &[
                    SavedChunk {
                        pos: b,
                        revision: 1,
                        storage: s.clone()
                    },
                    SavedChunk {
                        pos: a,
                        revision: 1,
                        storage: s.clone()
                    },
                ]
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
}
