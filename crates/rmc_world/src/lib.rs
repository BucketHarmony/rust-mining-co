//! Bevy-free world core: storage, materials, shapes, edits, queries, raycast, save format.
#![deny(unsafe_code)]

mod bench_smoke;
pub mod chunk;
pub mod coords;
pub mod damage;
pub mod edit;
pub mod inventory;
pub mod materials;
pub mod raycast;
pub mod save;
pub mod shape;
pub mod tools;
pub mod world;

pub use inventory::Inventory;
pub use raycast::{Hit, raycast};
pub use tools::{ToolProfile, ToolShape, ToolTable};
