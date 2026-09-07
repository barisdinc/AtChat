//! NET istasyon protokol mantığı (`client.py` portu).
//!
//! Kasıtlı katman ayrımı: burada protokol MANTIĞI var (master seçimi, roster,
//! sohbet, blok+CRC+ARQ ile transfer, drop/reconnect); kanal FİZİĞİ `channel`
//! crate'inde, MODÜLASYON `modem` crate'inde.

mod station;
mod types;

pub use station::{Station, StationShared};
pub use types::{
    ChatScope, Role, RosterEntry, RosterStatus, StationConfig, StationEvent, StationSnapshot,
    TransferDir, TransferIn, TransferOut, TransferSnapshot,
};
