//! Bevy-free world core: storage, materials, shapes, edits, queries, raycast, save format.
#![deny(unsafe_code)]

mod bench_smoke;
pub mod chunk;
pub mod columns;
pub mod coords;
pub mod damage;
pub mod displace;
pub mod edit;
pub mod fasthash;
pub mod inventory;
pub mod layers;
pub mod ledger;
pub mod materials;
pub mod query;
pub mod raycast;
pub mod save;
pub mod shape;
pub mod tools;
pub mod world;

pub use displace::EcoRules;
pub use inventory::Inventory;
pub use ledger::SeasonLedger;
pub use raycast::{Hit, raycast};
pub use tools::{ToolProfile, ToolShape, ToolTable};
