//! Bevy client: replica, meshing, rendering, input, controller, HUD, debug UI.
#![deny(unsafe_code)]

pub mod net;

pub use net::{ClientMaterials, FromHost, HostConnection, HostLink, NetPlugin, ToHost};
