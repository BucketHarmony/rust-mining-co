//! Bevy-free authoritative host: world, command queue, tick, transport trait.
#![deny(unsafe_code)]

pub mod host;
pub mod transport;

pub use host::{Host, HostConfig};
pub use transport::{ChannelTransport, ClientEndpoint, Disconnected, Recipient, Transport};
