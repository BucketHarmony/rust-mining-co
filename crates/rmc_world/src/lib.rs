//! Bevy-free world core: storage, materials, shapes, edits, queries, raycast, save format.
#![deny(unsafe_code)]

mod bench_smoke;
pub mod chop;
pub mod chunk;
pub mod columns;
pub mod coords;
pub mod damage;
pub mod displace;
pub mod edit;
pub mod fasthash;
pub mod inventory;
pub mod items;
pub mod layers;
pub mod ledger;
pub mod materials;
pub mod query;
pub mod raycast;
pub mod save;
pub mod shape;
pub mod tools;
pub mod tree;
pub mod world;

pub use chop::{TreeBox, TreeRules};
pub use displace::EcoRules;
pub use inventory::Inventory;
pub use items::{Bag, ItemTable};
pub use ledger::SeasonLedger;
pub use raycast::{Hit, TreeHit, raycast, raycast_solid, raycast_tree};
pub use tools::{PICKAXE, ToolProfile, ToolShape, ToolTable};
