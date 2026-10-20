//! Bevy-free Phase 2 ecology (SAD-P2 §4, §7): the season tick, its data
//! tables, active sets and invariant checker.
#![deny(unsafe_code)]

pub mod activity;
pub mod invariants;
pub mod ledger;
pub mod reference;
pub mod runner;
pub mod tables;
pub mod temperature;
pub mod tick;

pub use activity::{Activity, ActivityReport, JacobiKind, N_JACOBI};
pub use invariants::{InvariantChecker, Totals, Violation, Violations};
pub use ledger::SeasonLedger;
pub use runner::{
    ChunkView, Exec, JacobiOut, JacobiPass, JacobiReport, SweepCtx, SweepPass, SweepReport,
};
pub use tables::{EcoTableError, EcoTables};
pub use temperature::{Profile, TemperaturePass};
pub use tick::{TickReport, season_tick};
