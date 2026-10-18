//! Bevy-free world core: storage, materials, shapes, edits, queries, raycast, save format.
#![deny(unsafe_code)]

pub mod chunk;
pub mod coords;
pub mod damage;
pub mod edit;
pub mod materials;
pub mod raycast;
pub mod save;
pub mod shape;
pub mod world;

pub use raycast::{Hit, raycast};
