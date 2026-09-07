//! Kanal fiziği (`channel_server.py` portu) + istasyon bağlantı soyutlaması.
//!
//! Kasıtlı katman ayrımı: burada kanalın FİZİĞİ var (yarı çift yönlü erişim,
//! AWGN, multipath); protokol MANTIĞI (master seçimi, ARQ, sohbet) `protocol`
//! crate'inde. Gerçek SDR'a geçerken büyük ihtimalle yalnız bu crate değişir.

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
