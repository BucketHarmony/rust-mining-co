//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod net;
pub mod replica;

pub use net::{ClientMaterials, FromHost, HostConnection, HostLink, NetPlugin, NetSystems, ToHost};
pub use replica::{Replica, ReplicaError, ReplicaPlugin};
