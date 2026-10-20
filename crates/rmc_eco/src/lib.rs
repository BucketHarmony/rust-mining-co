//! Bevy-free Phase 2 ecology (SAD-P2 §4, §7): the season tick, its data
//! tables, active sets and invariant checker.
#![deny(unsafe_code)]

pub mod tables;

pub use tables::{EcoTableError, EcoTables};
