//! The channel physics (a port of `channel_server.py`) + the station-link
//! abstraction.
//!
//! Deliberate layering: the channel PHYSICS lives here (half-duplex access,
//! AWGN, multipath); the protocol LOGIC (master election, ARQ, chat) lives in
//! the `protocol` crate. Moving to a real SDR most likely changes only this crate.

pub mod config;
pub mod core;
pub mod link;
pub mod tcp_server;

pub use config::{apply_channel, ChannelConfig};
pub use core::{ChannelCore, ChannelEvent, ChannelSnapshot, ClientId};
pub use link::{
    b64_to_samples, samples_to_b64, Connector, InProcConnector, InProcRx, InProcTx, LinkRx, LinkTx,
    TcpConnector, TcpRx, TcpTx,
};
