//! Per-chunk ecology state (SAD-P2 §6): sparse cell layers, plant state, and
//! the cell bitsets the season tick keeps per chunk.
//!
//! A layer that is absent reads 0 everywhere, so only `Temperature` and
//! `Light` (materialized for every chunk at worldgen) and the layers a chunk
//! actually uses cost memory.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::coords::{CHUNK_VOLUME, LocalPos};
use crate::materials::MaterialDef;

/// Substance layers per chunk (SAD-P2 §3: ten `u8` layers).
pub const N_SUBSTANCES: usize = 10;
/// Gas kinds (SAD-P2 §2: steam, firedamp, chokedamp, smoke).
pub const N_GASES: usize = 4;
/// Sweep kinds with a per-chunk active set (SAD-P2 §6): water, one per gas,
/// chemistry transport, plant spread.
pub const N_SWEEPS: usize = 1 + N_GASES + 2;
/// Index of the water (and magma) sweep in [`Chunk::active`](crate::chunk::Chunk::active).
pub const SWEEP_WATER: usize = 0;
/// Index of gas 0's sweep; gas `g` uses `SWEEP_GAS + g`.
pub const SWEEP_GAS: usize = 1;
/// Index of the chemistry transport sweep.
pub const SWEEP_TRANSPORT: usize = SWEEP_GAS + N_GASES;
/// Index of the plant spread sweep.
pub const SWEEP_SPREAD: usize = SWEEP_TRANSPORT + 1;

/// `u64` words in a one-bit-per-cell chunk bitset.
const BITSET_WORDS: usize = CHUNK_VOLUME / 64;

/// Which per-cell quantity a layer holds.
///
/// The derived order is the storage and hash order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LayerId {
    /// 0.1 °C, `i16`. Dense: never dropped by [`ChunkLayers::compact`].
    Temperature,
    Moisture,
    Regolith,
    /// Index below [`N_SUBSTANCES`].
    Substance(u8),
    /// Index below [`N_GASES`].
    Gas(u8),
    /// Dense: never dropped by [`ChunkLayers::compact`].
    Light,
    Growth,
}

impl LayerId {
    /// Whether the layer stores `i16` values; every other layer stores `u8`.
    pub fn is_i16(self) -> bool {
        matches!(self, LayerId::Temperature)
    }

    /// Whether [`ChunkLayers::compact`] keeps the layer even when all zero.
    pub fn is_dense(self) -> bool {
        matches!(self, LayerId::Temperature | LayerId::Light)
    }

    /// Whether a substance or gas index is in range. Other ids always are.
    pub fn is_valid(self) -> bool {
        match self {
            LayerId::Substance(i) => usize::from(i) < N_SUBSTANCES,
            LayerId::Gas(i) => usize::from(i) < N_GASES,
            _ => true,
        }
    }

    /// Two bytes for the hash: variant, then the index (0 when none).
    fn tag(self) -> [u8; 2] {
        match self {
            LayerId::Temperature => [0, 0],
            LayerId::Moisture => [1, 0],
            LayerId::Regolith => [2, 0],
            LayerId::Substance(i) => [3, i],
            LayerId::Gas(i) => [4, i],
            LayerId::Light => [5, 0],
            LayerId::Growth => [6, 0],
        }
    }
}

/// Why a substance key list is not a valid table.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubstanceLayersError {
    #[error("{0} substances; expected 1..={N_SUBSTANCES}")]
    Count(usize),
    #[error("substance key {0:?} is empty or duplicate")]
    BadKey(String),
}

/// The substance keys of `substances.ron` in row order (SAD-P2 §7.6): row
/// `i` is stored in [`LayerId::Substance`]`(i)`. `rmc_eco` loads the table;
/// this is the part `rmc_world` needs to name a substance's layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubstanceLayers {
    keys: Vec<String>,
}

impl SubstanceLayers {
    /// From the keys in table order: 1..=[`N_SUBSTANCES`], unique and
    /// non-empty.
    pub fn new<I, S>(keys: I) -> Result<Self, SubstanceLayersError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let keys: Vec<String> = keys.into_iter().map(Into::into).collect();
        if keys.is_empty() || keys.len() > N_SUBSTANCES {
            return Err(SubstanceLayersError::Count(keys.len()));
        }
        for (i, k) in keys.iter().enumerate() {
            if k.is_empty() || keys[..i].contains(k) {
                return Err(SubstanceLayersError::BadKey(k.clone()));
            }
        }
        Ok(Self { keys })
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Never true: a table has at least one substance.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The layer of substance `key`, if the table has it.
    pub fn layer(&self, key: &str) -> Option<LayerId> {
        let i = self.keys.iter().position(|k| k == key)?;
        Some(LayerId::Substance(u8::try_from(i).ok()?))
    }

    /// The substance key stored in `id`, if `id` is a substance layer of
    /// this table.
    pub fn key(&self, id: LayerId) -> Option<&str> {
        match id {
            LayerId::Substance(i) => self.keys.get(usize::from(i)).map(String::as_str),
            _ => None,
        }
    }

    /// Every `(layer, key)` in table order.
    pub fn iter(&self) -> impl Iterator<Item = (LayerId, &str)> + '_ {
        (0u8..)
            .zip(&self.keys)
            .map(|(i, k)| (LayerId::Substance(i), k.as_str()))
    }

    /// A material's `composition` as `(layer, amount)` in its listed order;
    /// `None` if it names a substance the table lacks.
    pub fn composition(&self, def: &MaterialDef) -> Option<Vec<(LayerId, u8)>> {
        def.composition
            .iter()
            .map(|(k, amount)| Some((self.layer(k)?, *amount)))
            .collect()
    }
}

/// The values of one layer for every cell of a chunk, in local index order.
#[derive(Clone, PartialEq, Eq)]
pub enum LayerData {
    U8(Box<[u8; CHUNK_VOLUME]>),
    I16(Box<[i16; CHUNK_VOLUME]>),
}

impl LayerData {
    /// An all-zero layer of the type `id` stores.
    fn zeroed(id: LayerId) -> Self {
        if id.is_i16() {
            LayerData::I16(
                vec![0; CHUNK_VOLUME]
                    .into_boxed_slice()
                    .try_into()
                    .expect("CHUNK_VOLUME entries"),
            )
        } else {
            LayerData::U8(
                vec![0; CHUNK_VOLUME]
                    .into_boxed_slice()
                    .try_into()
                    .expect("CHUNK_VOLUME entries"),
            )
        }
    }

    /// Whether every cell is 0.
    pub fn is_zero(&self) -> bool {
        match self {
            LayerData::U8(v) => v.iter().all(|&x| x == 0),
            LayerData::I16(v) => v.iter().all(|&x| x == 0),
        }
    }

    /// Bytes owned on the heap.
    pub fn heap_bytes(&self) -> usize {
        match self {
            LayerData::U8(_) => CHUNK_VOLUME,
            LayerData::I16(_) => CHUNK_VOLUME * std::mem::size_of::<i16>(),
        }
    }
}

impl fmt::Debug for LayerData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, nonzero) = match self {
            LayerData::U8(v) => ("U8", v.iter().filter(|&&x| x != 0).count()),
            LayerData::I16(v) => ("I16", v.iter().filter(|&&x| x != 0).count()),
        };
        write!(f, "{kind}({nonzero} nonzero cells)")
    }
}

/// The layers of one chunk. Absent layers read 0.
///
/// Equality and [`hash`](Self::hash) go by what the layers read: an absent
/// layer equals a present all-zero one.
#[derive(Clone, Debug, Default)]
pub struct ChunkLayers {
    map: BTreeMap<LayerId, LayerData>,
}

impl ChunkLayers {
    /// Value of the `u8` layer `id` at `local`; 0 if the layer is absent.
    /// Panics if `id` is an `i16` layer or out of range.
    pub fn get_u8(&self, id: LayerId, local: LocalPos) -> u8 {
        check_u8(id);
        match self.map.get(&id) {
            Some(LayerData::U8(v)) => v[usize::from(local.index())],
            Some(LayerData::I16(_)) => unreachable!("u8 layer {id:?} stored as i16"),
            None => 0,
        }
    }

    /// Value of the `i16` layer `id` at `local`; 0 if the layer is absent.
    /// Panics if `id` is a `u8` layer.
    pub fn get_i16(&self, id: LayerId, local: LocalPos) -> i16 {
        check_i16(id);
        match self.map.get(&id) {
            Some(LayerData::I16(v)) => v[usize::from(local.index())],
            Some(LayerData::U8(_)) => unreachable!("i16 layer {id:?} stored as u8"),
            None => 0,
        }
    }

    /// Writes `v` at `local`, materializing the layer if it is absent.
    /// Panics if `id` is an `i16` layer or out of range.
    pub fn set_u8(&mut self, id: LayerId, local: LocalPos, v: u8) {
        check_u8(id);
        match self.layer_mut(id) {
            LayerData::U8(data) => data[usize::from(local.index())] = v,
            LayerData::I16(_) => unreachable!("u8 layer {id:?} stored as i16"),
        }
    }

    /// Writes `v` at `local`, materializing the layer if it is absent.
    /// Panics if `id` is a `u8` layer.
    pub fn set_i16(&mut self, id: LayerId, local: LocalPos, v: i16) {
        check_i16(id);
        match self.layer_mut(id) {
            LayerData::I16(data) => data[usize::from(local.index())] = v,
            LayerData::U8(_) => unreachable!("i16 layer {id:?} stored as u8"),
        }
    }

    /// Materializes `id` as all zeros if it is absent. Reads do not change.
    pub fn materialize(&mut self, id: LayerId) {
        assert!(id.is_valid(), "layer {id:?} index out of range");
        self.layer_mut(id);
    }

    fn layer_mut(&mut self, id: LayerId) -> &mut LayerData {
        self.map.entry(id).or_insert_with(|| LayerData::zeroed(id))
    }

    /// The values of `id` for the whole chunk, if materialized.
    pub fn data(&self, id: LayerId) -> Option<&LayerData> {
        self.map.get(&id)
    }

    /// Whether `id` is materialized.
    pub fn contains(&self, id: LayerId) -> bool {
        self.map.contains_key(&id)
    }

    /// Materialized layer ids in storage order.
    pub fn ids(&self) -> impl Iterator<Item = LayerId> + '_ {
        self.map.keys().copied()
    }

    /// Drops every all-zero layer except `Temperature` and `Light`. Reads do
    /// not change.
    pub fn compact(&mut self) {
        self.map.retain(|id, data| id.is_dense() || !data.is_zero());
    }

    /// Whether every layer reads 0 everywhere (absent layers included).
    pub fn is_zero(&self) -> bool {
        self.map.values().all(LayerData::is_zero)
    }

    /// xxh3 over the layers that are not all zero, in storage order: a
    /// prefix, then per layer its id tag and its values little-endian.
    pub fn hash(&self) -> u64 {
        const PREFIX: &[u8] = b"rmc.layers.v1";
        let mut buf = Vec::with_capacity(PREFIX.len() + self.heap_bytes() + self.map.len() * 2);
        buf.extend_from_slice(PREFIX);
        for (id, data) in self.map.iter().filter(|(_, d)| !d.is_zero()) {
            buf.extend_from_slice(&id.tag());
            match data {
                LayerData::U8(v) => buf.extend_from_slice(&v[..]),
                LayerData::I16(v) => v
                    .iter()
                    .for_each(|x| buf.extend_from_slice(&x.to_le_bytes())),
            }
        }
        xxhash_rust::xxh3::xxh3_64(&buf)
    }

    /// Bytes the materialized layers own on the heap, map nodes excluded.
    pub fn heap_bytes(&self) -> usize {
        self.map.values().map(LayerData::heap_bytes).sum()
    }
}

fn check_u8(id: LayerId) {
    assert!(!id.is_i16(), "{id:?} is an i16 layer");
    assert!(id.is_valid(), "layer {id:?} index out of range");
}

fn check_i16(id: LayerId) {
    assert!(id.is_i16(), "{id:?} is a u8 layer");
}

impl PartialEq for ChunkLayers {
    fn eq(&self, other: &Self) -> bool {
        let only_zero = |a: &Self, b: &Self| {
            a.map
                .iter()
                .filter(|(id, _)| !b.map.contains_key(id))
                .all(|(_, d)| d.is_zero())
        };
        self.map
            .iter()
            .filter_map(|(id, a)| other.map.get(id).map(|b| (a, b)))
            .all(|(a, b)| a == b)
            && only_zero(self, other)
            && only_zero(other, self)
    }
}

impl Eq for ChunkLayers {}

/// One layer's values in local index order, as the save encodes them.
#[derive(Serialize)]
enum LayerValuesRef<'a> {
    U8(&'a [u8]),
    I16(&'a [i16]),
}

/// The owned form of [`LayerValuesRef`], read back from a save.
#[derive(Deserialize)]
enum LayerValues {
    U8(Vec<u8>),
    I16(Vec<i16>),
}

/// Encodes the materialized layers in storage order as a sequence of
/// `(LayerId, values)`.
impl Serialize for ChunkLayers {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.map.iter().map(|(id, data)| {
            let values = match data {
                LayerData::U8(v) => LayerValuesRef::U8(&v[..]),
                LayerData::I16(v) => LayerValuesRef::I16(&v[..]),
            };
            (*id, values)
        }))
    }
}

/// Decodes what [`Serialize`] wrote. Rejects an id out of range, ids not
/// strictly ascending, values of the wrong type for the id, or a length
/// other than one value per cell.
impl<'de> Deserialize<'de> for ChunkLayers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let entries = Vec::<(LayerId, LayerValues)>::deserialize(deserializer)?;
        let mut map = BTreeMap::new();
        for (id, values) in entries {
            if !id.is_valid() {
                return Err(D::Error::custom(format!("layer {id:?} out of range")));
            }
            if map.last_key_value().is_some_and(|(last, _)| *last >= id) {
                return Err(D::Error::custom(format!("layer {id:?} out of order")));
            }
            let wrong_length =
                || D::Error::custom(format!("layer {id:?} is not one value per cell"));
            let data = match values {
                LayerValues::U8(v) if !id.is_i16() => LayerData::U8(
                    v.into_boxed_slice()
                        .try_into()
                        .map_err(|_| wrong_length())?,
                ),
                LayerValues::I16(v) if id.is_i16() => LayerData::I16(
                    v.into_boxed_slice()
                        .try_into()
                        .map_err(|_| wrong_length())?,
                ),
                _ => {
                    return Err(D::Error::custom(format!(
                        "layer {id:?} has the wrong value type"
                    )));
                }
            };
            map.insert(id, data);
        }
        Ok(Self { map })
    }
}

/// One plant cell's state (SAD-P2 §6, §7.8), keyed by local index in
/// [`Chunk::plants`](crate::chunk::Chunk::plants).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlantState {
    /// Row in `plants.ron`.
    pub species: u8,
    pub stress: u8,
    pub age_seasons: u8,
    /// Substance amounts taken up, by substance index.
    pub held: [u8; N_SUBSTANCES],
}

impl PlantState {
    /// Bytes for the hash: species, stress, age, then `held`.
    fn encode(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&[self.species, self.stress, self.age_seasons]);
        buf.extend_from_slice(&self.held);
    }
}

/// xxh3 over the plant map in key order: a prefix, then per plant its local
/// index (`u16` little-endian) and [`PlantState`] bytes.
pub fn plants_hash(plants: &BTreeMap<u16, PlantState>) -> u64 {
    const PREFIX: &[u8] = b"rmc.plants.v1";
    let mut buf = Vec::with_capacity(PREFIX.len() + plants.len() * (5 + N_SUBSTANCES));
    buf.extend_from_slice(PREFIX);
    for (index, plant) in plants {
        buf.extend_from_slice(&index.to_le_bytes());
        plant.encode(&mut buf);
    }
    xxhash_rust::xxh3::xxh3_64(&buf)
}

/// One bit per cell of a chunk, allocated on the first insert. Used for the
/// season-scoped freeze-thaw bits and the sweep active sets (SAD-P2 §6).
///
/// Equality goes by content: an unallocated set equals an allocated empty one.
#[derive(Clone, Default)]
pub struct CellSet {
    words: Option<Box<[u64; BITSET_WORDS]>>,
}

impl CellSet {
    pub fn contains(&self, local: LocalPos) -> bool {
        let (word, bit) = Self::slot(local);
        self.words.as_ref().is_some_and(|w| w[word] & bit != 0)
    }

    /// Adds `local`; returns whether it was absent.
    pub fn insert(&mut self, local: LocalPos) -> bool {
        let (word, bit) = Self::slot(local);
        let words = self.words.get_or_insert_with(|| {
            vec![0; BITSET_WORDS]
                .into_boxed_slice()
                .try_into()
                .expect("BITSET_WORDS entries")
        });
        let absent = words[word] & bit == 0;
        words[word] |= bit;
        absent
    }

    /// Removes `local`; returns whether it was present.
    pub fn remove(&mut self, local: LocalPos) -> bool {
        let (word, bit) = Self::slot(local);
        match self.words.as_mut() {
            Some(w) if w[word] & bit != 0 => {
                w[word] &= !bit;
                true
            }
            _ => false,
        }
    }

    /// Removes every cell and frees the storage.
    pub fn clear(&mut self) {
        self.words = None;
    }

    pub fn is_empty(&self) -> bool {
        self.words
            .as_ref()
            .is_none_or(|w| w.iter().all(|&x| x == 0))
    }

    pub fn len(&self) -> usize {
        self.words
            .as_ref()
            .map_or(0, |w| w.iter().map(|x| x.count_ones() as usize).sum())
    }

    /// Word `i` of the bitset (cells `64·i .. 64·i + 64`, bit `j` for cell
    /// `64·i + j`); 0 when unallocated. Panics if `i` is not below
    /// `CHUNK_VOLUME / 64`.
    pub fn word(&self, i: usize) -> u64 {
        assert!(i < BITSET_WORDS, "bitset word {i} out of range");
        self.words.as_ref().map_or(0, |w| w[i])
    }

    /// Cells in ascending local index order.
    pub fn iter(&self) -> impl Iterator<Item = LocalPos> + '_ {
        self.words.iter().flat_map(|w| {
            w.iter().enumerate().flat_map(|(i, &word)| {
                let mut rest = word;
                std::iter::from_fn(move || {
                    if rest == 0 {
                        return None;
                    }
                    let bit = rest.trailing_zeros() as usize;
                    rest &= rest - 1;
                    Some(LocalPos::from_index((i * 64 + bit) as u16))
                })
            })
        })
    }

    /// Bytes owned on the heap.
    pub fn heap_bytes(&self) -> usize {
        self.words
            .as_ref()
            .map_or(0, |_| BITSET_WORDS * std::mem::size_of::<u64>())
    }

    fn slot(local: LocalPos) -> (usize, u64) {
        let i = usize::from(local.index());
        (i / 64, 1 << (i % 64))
    }
}

impl PartialEq for CellSet {
    fn eq(&self, other: &Self) -> bool {
        match (&self.words, &other.words) {
            (Some(a), Some(b)) => a == b,
            _ => self.is_empty() && other.is_empty(),
        }
    }
}

impl Eq for CellSet {}

impl fmt::Debug for CellSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CellSet({} cells)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use crate::materials::MaterialId;

    const U8_LAYERS: [LayerId; 6] = [
        LayerId::Moisture,
        LayerId::Regolith,
        LayerId::Substance(0),
        LayerId::Substance(N_SUBSTANCES as u8 - 1),
        LayerId::Gas(N_GASES as u8 - 1),
        LayerId::Light,
    ];

    fn sample_locals() -> [LocalPos; 4] {
        [
            LocalPos::new(0, 0, 0),
            LocalPos::new(31, 31, 31),
            LocalPos::new(5, 17, 9),
            LocalPos::new(31, 0, 0),
        ]
    }

    #[test]
    fn absent_layer_reads_zero() {
        let mut layers = ChunkLayers::default();
        for local in sample_locals() {
            assert_eq!(layers.get_i16(LayerId::Temperature, local), 0);
            for id in U8_LAYERS.into_iter().chain([LayerId::Growth]) {
                assert_eq!(layers.get_u8(id, local), 0, "{id:?}");
            }
        }
        assert_eq!(layers.ids().count(), 0);

        // Setting one cell materializes the layer; its other cells read 0
        // and other layers stay absent.
        let at = LocalPos::new(5, 17, 9);
        layers.set_u8(LayerId::Moisture, at, 200);
        layers.set_i16(LayerId::Temperature, at, -150);
        assert!(layers.contains(LayerId::Moisture));
        assert!(!layers.contains(LayerId::Regolith));
        assert_eq!(layers.get_u8(LayerId::Moisture, at), 200);
        assert_eq!(layers.get_i16(LayerId::Temperature, at), -150);
        assert_eq!(layers.get_u8(LayerId::Moisture, LocalPos::new(6, 17, 9)), 0);
        assert_eq!(layers.get_u8(LayerId::Regolith, at), 0);
        assert_eq!(
            layers.get_i16(LayerId::Temperature, LocalPos::new(5, 17, 10)),
            0
        );

        // i16 extremes survive.
        layers.set_i16(LayerId::Temperature, at, i16::MIN);
        assert_eq!(layers.get_i16(LayerId::Temperature, at), i16::MIN);
        layers.set_i16(LayerId::Temperature, at, i16::MAX);
        assert_eq!(layers.get_i16(LayerId::Temperature, at), i16::MAX);

        // A fresh chunk has no layers and no plants.
        let chunk = Chunk::uniform(MaterialId::AIR);
        assert_eq!(chunk.layers.ids().count(), 0);
        assert!(chunk.plants.is_empty());
        assert_eq!(chunk.eco_revision, 0);
        assert!(chunk.freeze_thaw.is_empty());
        assert!(chunk.active.iter().all(CellSet::is_empty));
        assert_eq!(chunk.layers.get_u8(LayerId::Light, at), 0);
    }

    #[test]
    #[should_panic(expected = "i16 layer")]
    fn u8_read_of_temperature_panics() {
        ChunkLayers::default().get_u8(LayerId::Temperature, LocalPos::new(0, 0, 0));
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn substance_index_out_of_range_panics() {
        ChunkLayers::default().set_u8(
            LayerId::Substance(N_SUBSTANCES as u8),
            LocalPos::new(0, 0, 0),
            1,
        );
    }

    #[test]
    fn compact_drops_zero_keeps_temperature_and_light() {
        let mut layers = ChunkLayers::default();
        let at = LocalPos::new(3, 4, 5);
        layers.materialize(LayerId::Temperature);
        layers.materialize(LayerId::Light);
        layers.materialize(LayerId::Regolith);
        // Written then cleared: materialized but all zero.
        layers.set_u8(LayerId::Moisture, at, 9);
        layers.set_u8(LayerId::Moisture, at, 0);
        layers.set_u8(LayerId::Gas(1), at, 0);
        // Nonzero: kept.
        layers.set_u8(LayerId::Substance(2), at, 7);
        layers.set_u8(LayerId::Growth, LocalPos::new(31, 31, 31), 1);
        let before = layers.clone();
        assert_eq!(layers.ids().count(), 7);

        layers.compact();
        assert_eq!(
            layers.ids().collect::<Vec<_>>(),
            vec![
                LayerId::Temperature,
                LayerId::Substance(2),
                LayerId::Light,
                LayerId::Growth
            ]
        );
        // Reads, equality and hash do not change.
        assert_eq!(layers, before);
        assert_eq!(layers.hash(), before.hash());
        assert_eq!(layers.get_u8(LayerId::Substance(2), at), 7);
        assert_eq!(layers.get_u8(LayerId::Moisture, at), 0);
        assert_eq!(layers.get_i16(LayerId::Temperature, at), 0);

        // Nonzero dense layers stay too, and compacting twice is a no-op.
        layers.set_i16(LayerId::Temperature, at, 120);
        layers.set_u8(LayerId::Light, at, 255);
        layers.compact();
        layers.compact();
        assert!(layers.contains(LayerId::Temperature));
        assert!(layers.contains(LayerId::Light));
        assert_eq!(layers.get_i16(LayerId::Temperature, at), 120);
        assert_eq!(layers.get_u8(LayerId::Light, at), 255);

        // A layer that becomes all zero again is dropped.
        layers.set_u8(LayerId::Substance(2), at, 0);
        layers.compact();
        assert!(!layers.contains(LayerId::Substance(2)));
    }

    #[test]
    fn chunk_hash_covers_layers_and_plants() {
        let stone = MaterialId(4);
        let base = Chunk::uniform(stone);
        // No ecology state: the Phase 1 storage hash, unchanged.
        assert_eq!(base.hash(), base.storage.hash());

        let at = LocalPos::new(1, 2, 3);
        let mut seen = vec![base.hash()];
        let mut assert_new = |c: &Chunk, what: &str| {
            let h = c.hash();
            assert!(!seen.contains(&h), "{what}: hash {h:#x} not new");
            seen.push(h);
        };

        let mut temp = base.clone();
        temp.layers.set_i16(LayerId::Temperature, at, 1);
        assert_new(&temp, "temperature");
        let mut temp2 = base.clone();
        temp2.layers.set_i16(LayerId::Temperature, at, -1);
        assert_new(&temp2, "temperature value");
        let mut moved = base.clone();
        moved
            .layers
            .set_i16(LayerId::Temperature, LocalPos::new(1, 2, 4), 1);
        assert_new(&moved, "temperature cell");
        for id in U8_LAYERS {
            let mut c = base.clone();
            c.layers.set_u8(id, at, 1);
            assert_new(&c, &format!("{id:?}"));
        }

        let plant = PlantState {
            species: 1,
            stress: 2,
            age_seasons: 3,
            held: [0; N_SUBSTANCES],
        };
        let mut planted = base.clone();
        planted.plants.insert(at.index(), plant);
        assert_new(&planted, "plant");
        let mut elsewhere = base.clone();
        elsewhere.plants.insert(at.index() + 1, plant);
        assert_new(&elsewhere, "plant cell");
        for (i, field) in ["species", "stress", "age", "held"].iter().enumerate() {
            let mut p = plant;
            match i {
                0 => p.species += 1,
                1 => p.stress += 1,
                2 => p.age_seasons += 1,
                _ => p.held[N_SUBSTANCES - 1] = 1,
            }
            let mut c = base.clone();
            c.plants.insert(at.index(), p);
            assert_new(&c, field);
        }

        // Storage still counts when layers are present.
        let mut other_cells = temp.clone();
        other_cells.storage.set(at, MaterialId::AIR);
        assert_new(&other_cells, "storage under layers");

        // All-zero layers hash as absent ones; compact does not move the hash.
        let mut zeroed = base.clone();
        zeroed.layers.materialize(LayerId::Temperature);
        zeroed.layers.set_u8(LayerId::Moisture, at, 0);
        assert_eq!(zeroed.hash(), base.hash());
        let mut compacted = temp.clone();
        compacted.layers.materialize(LayerId::Regolith);
        compacted.layers.compact();
        assert_eq!(compacted.hash(), temp.hash());

        // Revisions, `modified`, freeze-thaw and active sets are not hashed.
        let mut meta = temp.clone();
        meta.revision += 1;
        meta.eco_revision += 1;
        meta.modified = true;
        meta.freeze_thaw.insert(at);
        meta.active[N_SWEEPS - 1].insert(at);
        assert_eq!(meta.hash(), temp.hash());
    }

    #[test]
    fn layers_encode_round_trip_and_bad_layers_are_refused() {
        let mut layers = ChunkLayers::default();
        layers.set_i16(LayerId::Temperature, LocalPos::new(1, 2, 3), -40);
        layers.set_u8(LayerId::Gas(2), LocalPos::new(31, 0, 7), 200);
        layers.materialize(LayerId::Light);
        let bytes = postcard::to_stdvec(&layers).unwrap();
        let back: ChunkLayers = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, layers);
        assert_eq!(back.hash(), layers.hash());
        assert_eq!(
            back.ids().collect::<Vec<_>>(),
            layers.ids().collect::<Vec<_>>()
        );

        let decode = |entries: Vec<(LayerId, LayerValues)>| {
            let bytes = postcard::to_stdvec(&RawLayers(entries)).unwrap();
            postcard::from_bytes::<ChunkLayers>(&bytes)
        };
        let u8s = || LayerValues::U8(vec![0; CHUNK_VOLUME]);
        let i16s = || LayerValues::I16(vec![0; CHUNK_VOLUME]);
        assert!(decode(vec![(LayerId::Moisture, u8s())]).is_ok());
        // Wrong value type, wrong length, out of range, out of order.
        assert!(decode(vec![(LayerId::Moisture, i16s())]).is_err());
        assert!(decode(vec![(LayerId::Temperature, u8s())]).is_err());
        assert!(decode(vec![(LayerId::Moisture, LayerValues::U8(vec![0; 7]))]).is_err());
        assert!(decode(vec![(LayerId::Gas(N_GASES as u8), u8s())]).is_err());
        assert!(decode(vec![(LayerId::Moisture, u8s()), (LayerId::Moisture, u8s())]).is_err());
        assert!(decode(vec![(LayerId::Light, u8s()), (LayerId::Moisture, u8s())]).is_err());
    }

    /// Encodes like [`ChunkLayers`] from arbitrary entries.
    struct RawLayers(Vec<(LayerId, LayerValues)>);

    impl Serialize for RawLayers {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_seq(self.0.iter().map(|(id, v)| {
                let v = match v {
                    LayerValues::U8(v) => LayerValuesRef::U8(v),
                    LayerValues::I16(v) => LayerValuesRef::I16(v),
                };
                (*id, v)
            }))
        }
    }

    #[test]
    fn cell_set_insert_remove_iter() {
        let mut s = CellSet::default();
        assert!(s.is_empty());
        assert_eq!(s.heap_bytes(), 0);
        let cells = [
            LocalPos::new(31, 31, 31),
            LocalPos::new(0, 0, 0),
            LocalPos::new(1, 0, 2),
        ];
        for c in cells {
            assert!(s.insert(c));
            assert!(!s.insert(c));
        }
        assert_eq!(s.len(), 3);
        let mut sorted = cells.map(LocalPos::index);
        sorted.sort_unstable();
        assert_eq!(s.iter().map(LocalPos::index).collect::<Vec<_>>(), sorted);
        assert!(s.remove(cells[0]));
        assert!(!s.remove(cells[0]));
        assert!(!s.contains(cells[0]));
        assert!(s.contains(cells[1]));
        s.remove(cells[1]);
        s.remove(cells[2]);
        assert!(s.is_empty());
        assert_eq!(s, CellSet::default());
        s.insert(cells[2]);
        s.clear();
        assert_eq!(s.heap_bytes(), 0);
    }

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// The fields of a `substances.ron` row this crate reads; `rmc_eco`
    /// owns the full row.
    #[derive(Deserialize)]
    struct SubstanceRow {
        key: String,
        #[serde(default)]
        yield_material: Option<String>,
    }

    fn shipped_substance_rows() -> Vec<SubstanceRow> {
        let text = std::fs::read_to_string(format!("{ASSETS}/substances.ron"))
            .expect("assets/substances.ron reads");
        ron::from_str(&text).expect("assets/substances.ron parses")
    }

    #[test]
    fn substance_layers_index_by_table() {
        use crate::materials::{MaterialTable, Phase};

        // Row i of substances.ron is LayerId::Substance(i) (SAD-P2 §3, §7.6).
        let rows = shipped_substance_rows();
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["cu", "fe", "s", "acid", "lime", "salt", "n", "p", "k", "c"]
        );
        let table = SubstanceLayers::new(keys.iter().copied()).unwrap();
        assert_eq!(table.len(), N_SUBSTANCES);
        assert!(!table.is_empty());
        for (i, key) in keys.iter().enumerate() {
            let id = LayerId::Substance(i as u8);
            assert!(id.is_valid());
            assert!(!id.is_i16() && !id.is_dense());
            assert_eq!(table.layer(key), Some(id), "{key}");
            assert_eq!(table.key(id), Some(*key));
        }
        assert_eq!(
            table.iter().collect::<Vec<_>>(),
            keys.iter()
                .enumerate()
                .map(|(i, &k)| (LayerId::Substance(i as u8), k))
                .collect::<Vec<_>>()
        );
        assert_eq!(table.layer("gold"), None);
        assert_eq!(table.key(LayerId::Substance(N_SUBSTANCES as u8)), None);
        assert_eq!(table.key(LayerId::Moisture), None);
        assert_eq!(table.key(LayerId::Gas(0)), None);

        // The layers are separate stores: each substance written at a cell
        // reads back alone, and a chunk compacts the untouched ones away.
        let at = LocalPos::new(7, 30, 2);
        let mut layers = ChunkLayers::default();
        for (id, _) in table.iter() {
            let LayerId::Substance(i) = id else {
                unreachable!()
            };
            layers.set_u8(id, at, 10 + i);
        }
        let lime = table.layer("lime").unwrap();
        layers.set_u8(lime, at, 0);
        layers.compact();
        for (id, key) in table.iter() {
            let LayerId::Substance(i) = id else {
                unreachable!()
            };
            let want = if key == "lime" { 0 } else { 10 + i };
            assert_eq!(layers.get_u8(id, at), want, "{key}");
            assert_eq!(layers.get_u8(id, LocalPos::new(7, 30, 3)), 0, "{key}");
            assert_eq!(layers.contains(id), key != "lime", "{key}");
        }

        // Every composition in the shipped material table names a
        // substance layer, in its listed order.
        let materials = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        for d in materials.iter() {
            let got = table.composition(d).unwrap_or_else(|| {
                panic!("{} names an unknown substance: {:?}", d.key, d.composition)
            });
            let want: Vec<(LayerId, u8)> = d
                .composition
                .iter()
                .map(|(k, a)| (table.layer(k).unwrap(), *a))
                .collect();
            assert_eq!(got, want, "{}", d.key);
        }
        let comp = |key: &str| {
            table
                .composition(materials.get(materials.id(key).unwrap()).unwrap())
                .unwrap()
        };
        let (cu, fe, s) = (
            table.layer("cu").unwrap(),
            table.layer("fe").unwrap(),
            table.layer("s").unwrap(),
        );
        assert_eq!(comp("sulfide_ore"), [(cu, 60), (fe, 40), (s, 80)]);
        assert_eq!(comp("crushed_sulfide"), comp("sulfide_ore"));
        assert_eq!(comp("limestone"), [(lime, 120)]);

        // Each substance's yield material (SAD-P2 §5) is a placeable,
        // diggable solid that holds that substance and no other.
        let mut yields = 0;
        for (i, row) in rows.iter().enumerate() {
            let Some(key) = &row.yield_material else {
                continue;
            };
            yields += 1;
            let d = materials
                .get(materials.id(key).unwrap_or_else(|| panic!("no {key}")))
                .unwrap();
            assert_eq!(d.phase, Phase::Solid, "{key}");
            assert!(d.diggable && d.has_tag("placeable"), "{key}");
            assert!(d.yield_count > 0 && d.yield_material.is_none(), "{key}");
            let held = table.composition(d).unwrap();
            assert_eq!(held.len(), 1, "{key}");
            assert_eq!(held[0].0, LayerId::Substance(i as u8), "{key}");
            assert!(held[0].1 > 0, "{key}");
        }
        assert_eq!(yields, 3);

        // An unknown composition key resolves to nothing.
        let mut odd = materials
            .get(materials.id("limestone").unwrap())
            .unwrap()
            .clone();
        odd.composition.push(("gold".into(), 1));
        assert_eq!(table.composition(&odd), None);

        // A table is 1..=N_SUBSTANCES unique, non-empty keys.
        let none: [&str; 0] = [];
        assert_eq!(
            SubstanceLayers::new(none),
            Err(SubstanceLayersError::Count(0))
        );
        let eleven = (0..=N_SUBSTANCES).map(|i| format!("x{i}"));
        assert_eq!(
            SubstanceLayers::new(eleven),
            Err(SubstanceLayersError::Count(N_SUBSTANCES + 1))
        );
        assert_eq!(
            SubstanceLayers::new(["cu", "fe", "cu"]),
            Err(SubstanceLayersError::BadKey("cu".into()))
        );
        assert_eq!(
            SubstanceLayers::new(["cu", ""]),
            Err(SubstanceLayersError::BadKey(String::new()))
        );
        let one = SubstanceLayers::new(["c"]).unwrap();
        assert_eq!(one.layer("c"), Some(LayerId::Substance(0)));
        assert_eq!(one.key(LayerId::Substance(1)), None);
    }
}
