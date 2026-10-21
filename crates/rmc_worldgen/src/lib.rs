//! Bevy-free deterministic world generation.
#![deny(unsafe_code)]

pub mod r#gen;
pub mod magma;
pub mod noise;
pub mod params;
pub mod strata;
pub mod temperature;
pub mod water;

pub use r#gen::{Generator, generate_chunk};
pub use magma::MagmaInit;
pub use params::{GenError, GenParams};
pub use temperature::TemperatureInit;
pub use water::WaterTableInit;

/// The noise crate and version worldgen output depends on. The save header
/// records it, and a mismatch fails load (SAD §9, §10).
pub const NOISE_CRATE_VERSION: &str = "fastnoise-lite 1.1.1";
