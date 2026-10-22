//! Bevy-free Phase 2 ecology (SAD-P2 §4, §7): the season tick, its data
//! tables, active sets and invariant checker.
#![deny(unsafe_code)]

pub mod activity;
pub mod chemistry;
pub mod gases;
pub mod invariants;
pub mod ledger;
pub mod light;
pub mod plants;
pub mod reference;
pub mod runner;
pub mod tables;
pub mod temperature;
pub mod tick;
pub mod transitions;
pub mod water;
pub mod weathering;

pub use activity::{Activity, ActivityReport, JacobiKind, N_JACOBI};
pub use chemistry::{Move, MoveLog, ReactionsPass, Transport, TransportReport};
pub use gases::GasPass;
pub use invariants::{InvariantChecker, Totals, Violation, Violations};
pub use ledger::SeasonLedger;
pub use light::{LightGrid, LightPass};
pub use plants::{GrowthPass, age_plants};
pub use runner::{
    ChunkView, Exec, JacobiOut, JacobiPass, JacobiReport, SweepCtx, SweepPass, SweepReport,
};
pub use tables::{EcoTableError, EcoTables, SeasonOverride, ToxKind};
pub use temperature::{Profile, TemperaturePass};
pub use tick::{Observer, PassId, TickReport, season_tick, season_tick_observed};
pub use transitions::TransitionsPass;
pub use water::{Condensation, Evaporation, Rain, WaterPass, rain_pattern};
pub use weathering::WeatheringPass;
