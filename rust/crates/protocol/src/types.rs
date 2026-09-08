//! `client.py`'deki veri yapılarının portu + GUI'ye yayınlanan olaylar.

use std::collections::BTreeMap;
use std::path::PathBuf;

use netproto::Mode;
use tokio::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Listener,
    Master,
    Backup,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Listener => "LISTENER",
            Role::Master => "MASTER",
            Role::Backup => "BACKUP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterStatus {
    Active,
    Lost,
}

#[derive(Debug, Clone)]
pub struct RosterEntry {
    pub last_seen: Instant,
    pub status: RosterStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDir {
    In,
    Out,
}

/// Bu istasyonun GÖNDERDİĞİ bir bulk transfer.
#[derive(Debug, Clone)]
pub struct TransferOut {
    pub transfer_id: String,
    pub filename: String,
    pub dst: String,
    pub blocks: BTreeMap<usize, Vec<u8>>,
    pub mode: Mode,
    pub arq_round: usize,
    /// GUI ilerleme çubuğu için gönderilen blok sayacı (ARQ turlarında artar).
    pub sent: usize,
    pub done: bool,
}

/// Bu istasyonun ALDIĞI bir bulk transfer.
#[derive(Debug, Clone)]
pub struct TransferIn {
    pub transfer_id: String,
    pub filename: String,
    pub total_blocks: usize,
    pub src: String,
    pub dst: String,
    pub received: BTreeMap<usize, Vec<u8>>,
    pub complete: bool,
    pub saved_path: Option<String>,
}

impl TransferIn {
    pub fn missing_blocks(&self) -> Vec<usize> {
        (0..self.total_blocks)
            .filter(|s| !self.received.contains_key(s))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatScope {
    /// `dst == "ALL"`
    Broadcast,
    /// `dst == <bu istasyon>`
    Private,
}

/// GUI'nin dinlediği istasyon olayları. `client.py`'nin `print(...)`
/// çağrılarının yapılandırılmış karşılığı.
#[derive(Debug, Clone)]
pub enum StationEvent {
    /// `self.log(msg)` — serbest metin günlük satırı.
    Log(String),
    /// Gelen sohbet mesajı.
    Chat {
        from: String,
        scope: ChatScope,
        text: String,
    },
    RoleChanged(Role),
    /// Roster ya da transfer tablosu değişti — GUI snapshot'ı yeniden okumalı.
    StateChanged,
    /// Bir transfer ilerledi ya da tamamlandı.
    Transfer {
        id: String,
        dir: TransferDir,
        filename: String,
        /// Karşı istasyon (gelen için kaynak, giden için hedef).
        peer: String,
        have: usize,
        total: usize,
        done: bool,
        saved_path: Option<String>,
    },
}

/// Zamanlama parametreleri. Varsayılanlar `netproto.py` sabitleri (GUI bunu
/// kullanır — CLAUDE.md'deki "gerçek ~24 sn master seçimi"). Testler kısaltır.
#[derive(Debug, Clone)]
pub struct StationConfig {
    pub beacon_interval: Duration,
    pub beacon_timeout: Duration,
    pub lost_timeout: Duration,
    pub remove_timeout: Duration,
    /// `_send_blocks`: her kaç blokta bir kontrol penceresi (CLAUDE.md hata #3).
    pub control_window_every: usize,
    pub control_window_pause: Duration,
    /// Alınan dosyaların yazılacağı dizin (`client.py`: "received").
    pub received_dir: PathBuf,
}

impl Default for StationConfig {
    fn default() -> Self {
        Self {
            beacon_interval: Duration::from_secs_f64(netproto::BEACON_INTERVAL),
            beacon_timeout: Duration::from_secs_f64(netproto::BEACON_TIMEOUT),
            lost_timeout: Duration::from_secs_f64(netproto::LOST_TIMEOUT),
            remove_timeout: Duration::from_secs_f64(netproto::REMOVE_TIMEOUT),
            control_window_every: 3,
            control_window_pause: Duration::from_millis(1200),
            received_dir: PathBuf::from("received"),
        }
    }
}

/// GUI'nin her karede okuduğu anlık istasyon durumu.
#[derive(Debug, Clone)]
pub struct StationSnapshot {
    pub callsign: String,
    pub role: Role,
    pub master: Option<String>,
    pub backup: Option<String>,
    pub connected: bool,
    pub roster: Vec<(String, RosterStatus, f64)>, // (çağrı, durum, son görülme sn önce)
    pub transfers_in: Vec<TransferSnapshot>,
    pub transfers_out: Vec<TransferSnapshot>,
}

#[derive(Debug, Clone)]
pub struct TransferSnapshot {
    pub id: String,
    pub filename: String,
    pub peer: String,
    pub have: usize,
    pub total: usize,
    pub complete: bool,
    pub arq_round: usize,
}
