//! Seeded noise layers (SAD §9): `fastnoise-lite` OpenSimplex2 with FBm,
//! seeded from `(world seed, layer id)` and sampled at world meters.

use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};
use rmc_world::coords::CELL_SIZE_M;

use crate::params::NoiseParams;

/// Layer id of the surface heightmap.
pub const LAYER_SURFACE: u32 = 0x0001;
/// Layer id of soil layer `i` is `LAYER_SOIL + i`.
pub const LAYER_SOIL: u32 = 0x0100;
/// Layer id of the boundary under stratum `i` is `LAYER_STRATA + i`.
pub const LAYER_STRATA: u32 = 0x0200;
/// Layer id of the blob cave noise.
pub const LAYER_CAVE_BLOB: u32 = 0x0300;
/// Layer ids of the two worm tunnel noises.
pub const LAYER_CAVE_WORM_A: u32 = 0x0301;
pub const LAYER_CAVE_WORM_B: u32 = 0x0302;
/// Layer id of ore `i` is `LAYER_ORE + i`.
pub const LAYER_ORE: u32 = 0x0400;
/// Layer id of the crystal placement hash.
pub const LAYER_CRYSTAL: u32 = 0x0500;

/// The `fastnoise-lite` seed of a layer: xxh3 over the whole 64-bit world
/// seed and the layer id, truncated to 32 bits. Seeds that differ only in
/// their high bits still give different noise.
pub fn layer_seed(seed: u64, layer: u32) -> i32 {
    let mut buf = [0u8; 24];
    buf[..12].copy_from_slice(b"rmc.noise.v1");
    buf[12..20].copy_from_slice(&seed.to_le_bytes());
    buf[20..].copy_from_slice(&layer.to_le_bytes());
    xxhash_rust::xxh3::xxh3_64(&buf) as u32 as i32
}

/// A per-cell hash for choices that need no spatial coherence: xxh3 over
/// the whole 64-bit world seed, the layer id and the cell coordinates.
pub fn cell_hash(seed: u64, layer: u32, x: i32, y: i32, z: i32) -> u64 {
    let mut buf = [0u8; 36];
    buf[..12].copy_from_slice(b"rmc.cell.v1.");
    buf[12..20].copy_from_slice(&seed.to_le_bytes());
    buf[20..24].copy_from_slice(&layer.to_le_bytes());
    buf[24..28].copy_from_slice(&x.to_le_bytes());
    buf[28..32].copy_from_slice(&y.to_le_bytes());
    buf[32..].copy_from_slice(&z.to_le_bytes());
    xxhash_rust::xxh3::xxh3_64(&buf)
}

/// One seeded noise layer.
pub struct NoiseLayer(FastNoiseLite);

impl NoiseLayer {
    pub fn new(seed: u64, layer: u32, params: &NoiseParams) -> Self {
        let mut n = FastNoiseLite::with_seed(layer_seed(seed, layer));
        n.set_noise_type(Some(NoiseType::OpenSimplex2));
        n.set_frequency(Some(params.frequency));
        n.set_fractal_type(Some(FractalType::FBm));
        n.set_fractal_octaves(Some(i32::from(params.octaves)));
        Self(n)
    }

    /// Noise in about [-1, 1] at the min corner of cell column `(x, z)`.
    pub fn sample_2d(&self, x: i32, z: i32) -> f32 {
        self.0
            .get_noise_2d(x as f32 * CELL_SIZE_M, z as f32 * CELL_SIZE_M)
    }

    /// Noise in about [-1, 1] at the min corner of cell `(x, y, z)`.
    pub fn sample_3d(&self, x: i32, y: i32, z: i32) -> f32 {
        self.0.get_noise_3d(
            x as f32 * CELL_SIZE_M,
            y as f32 * CELL_SIZE_M,
            z as f32 * CELL_SIZE_M,
        )
    }

    /// `n` scaled to a whole offset in `-amplitude..=amplitude`.
    pub fn offset_2d(&self, x: i32, z: i32, amplitude: i32) -> i32 {
        let n = self.sample_2d(x, z);
        ((n * amplitude as f32).floor() as i32).clamp(-amplitude, amplitude)
    }

    /// A whole value in `min..=max`, spread evenly over the noise range.
    pub fn pick_2d(&self, x: i32, z: i32, min: i32, max: i32) -> i32 {
        let n = self.sample_2d(x, z);
        let span = max - min;
        let step = ((n + 1.0) * 0.5 * (span + 1) as f32).floor() as i32;
        min + step.clamp(0, span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_seeds_use_every_input_bit() {
        let base = layer_seed(42, LAYER_SURFACE);
        assert_eq!(base, layer_seed(42, LAYER_SURFACE));
        let mut seen = vec![base];
        for bit in 0..64 {
            seen.push(layer_seed(42 ^ (1 << bit), LAYER_SURFACE));
        }
        for bit in 0..32 {
            seen.push(layer_seed(42, LAYER_SURFACE ^ (1 << bit)));
        }
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "two inputs share a layer seed");
    }

    #[test]
    fn cell_hashes_use_every_input_bit() {
        let (seed, layer, x, y, z) = (42, LAYER_CRYSTAL, 3, -7, 1 << 20);
        let base = cell_hash(seed, layer, x, y, z);
        assert_eq!(base, cell_hash(seed, layer, x, y, z));
        let mut seen = vec![base];
        for bit in 0..64 {
            seen.push(cell_hash(seed ^ (1 << bit), layer, x, y, z));
        }
        for bit in 0..32 {
            let b = (1u32 << bit) as i32;
            seen.push(cell_hash(seed, layer ^ (1 << bit), x, y, z));
            seen.push(cell_hash(seed, layer, x ^ b, y, z));
            seen.push(cell_hash(seed, layer, x, y ^ b, z));
            seen.push(cell_hash(seed, layer, x, y, z ^ b));
        }
        // Swapped coordinates hash differently.
        seen.push(cell_hash(seed, layer, y, x, z));
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "two inputs share a cell hash");
    }

    #[test]
    fn offsets_and_picks_stay_in_range() {
        let params = NoiseParams {
            frequency: 0.3,
            octaves: 3,
        };
        let layer = NoiseLayer::new(7, LAYER_SURFACE, &params);
        let mut picks = [0u32; 3];
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        for x in -64..64 {
            for z in -64..64 {
                let o = layer.offset_2d(x, z, 5);
                assert!((-5..=5).contains(&o));
                lo = lo.min(o);
                hi = hi.max(o);
                assert_eq!(layer.offset_2d(x, z, 0), 0);
                let p = layer.pick_2d(x, z, 1, 3);
                picks[(p - 1) as usize] += 1;
                assert_eq!(layer.pick_2d(x, z, 4, 4), 4);
            }
        }
        assert!(lo <= -3 && hi >= 3, "offsets span only {lo}..={hi}");
        assert!(picks.iter().all(|&c| c > 1000), "picks uneven: {picks:?}");
    }
}
