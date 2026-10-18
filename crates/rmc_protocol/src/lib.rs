//! Bevy-free client/server messages and their serialization.
#![deny(unsafe_code)]

pub mod codec;
pub mod msg;

pub use codec::{CodecError, decode_client, decode_server, encode_client, encode_server};
pub use msg::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
