//! Bevy-free authoritative host: world, command queue, tick, transport trait.
#![deny(unsafe_code)]

pub mod host;
pub mod replica;
pub mod thread;
pub mod transport;

pub use host::{Host, HostConfig, HostSaveError, SaveConfig};
pub use replica::{Replica, ReplicaError};
pub use thread::{HostHandle, spawn_host_thread};
pub use transport::{ChannelTransport, ClientEndpoint, Disconnected, Recipient, Transport};
