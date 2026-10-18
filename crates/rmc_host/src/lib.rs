//! Bevy-free authoritative host: world, command queue, tick, transport trait.
#![deny(unsafe_code)]

pub mod transport;

pub use transport::{ChannelTransport, ClientEndpoint, Disconnected, Recipient, Transport};
