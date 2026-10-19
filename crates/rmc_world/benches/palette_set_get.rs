//! SAD §14 bench `palette_set_get`: chunk storage writes and reads.
//!
//! One iteration starts from a uniform chunk, writes 4096 cells spread over
//! the chunk with 20 distinct materials (promoting the storage through the
//! 1-, 2-, 4- and 8-bit palettes), then reads all 32768 cells back.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use rmc_world::chunk::ChunkStorage;
use rmc_world::coords::{CHUNK_VOLUME, LocalPos};
use rmc_world::materials::MaterialId;

/// Cells written per iteration.
const WRITES: usize = 4096;
/// Distinct materials written: more than 16, so the storage ends 8-bit
/// paletted.
const MATERIALS: u16 = 20;

/// Cell indices visited in a fixed scattered order (an odd stride is a
/// permutation of `0..CHUNK_VOLUME`).
fn write_order() -> Vec<LocalPos> {
    const STRIDE: usize = 7919;
    (0..WRITES)
        .map(|i| LocalPos::from_index(((i * STRIDE) % CHUNK_VOLUME) as u16))
        .collect()
}

fn palette_set_get(c: &mut Criterion) {
    let order = write_order();
    let base = ChunkStorage::Uniform(MaterialId(1));
    c.bench_function("palette_set_get", |b| {
        b.iter(|| {
            let mut storage = base.clone();
            for (i, &local) in order.iter().enumerate() {
                storage.set(local, MaterialId(1 + i as u16 % MATERIALS));
            }
            let mut sum = 0u64;
            for i in 0..CHUNK_VOLUME as u16 {
                sum += u64::from(storage.get(LocalPos::from_index(i)).0);
            }
            black_box((storage, sum))
        })
    });
}

criterion_group!(benches, palette_set_get);
criterion_main!(benches);
