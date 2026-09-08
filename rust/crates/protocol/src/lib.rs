//! The NET station protocol logic (a port of `client.py`).
//!
//! Deliberate layering: the protocol LOGIC lives here (master election,
//! roster, chat, block-CRC-ARQ transfer, drop/reconnect); the channel PHYSICS
//! is in the `channel` crate, the MODULATION in the `modem` crate.

mod station;
mod types;

pub use station::{Station, StationShared};
pub use types::{
    ChatScope, Role, RosterEntry, RosterStatus, StationConfig, StationEvent, StationSnapshot,
    TransferDir, TransferIn, TransferOut, TransferSnapshot,
};
