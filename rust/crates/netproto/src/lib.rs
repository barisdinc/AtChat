//! `netproto.py` portu — AtCHAT ortak protokol sabitleri, çerçeve tipleri ve
//! satır-bazlı JSON tel çerçeveleme.
//!
//! Bu crate iki ayrı katmanı temsil eder:
//!   1. **Tel mesajları** (`ClientMsg` / `ServerMsg`): istemci ↔ kanal sunucusu
//!      arasında TCP üzerinden giden dış zarf. `channel_server.py` /
//!      `client.py` ile birebir alan adları (Python ↔ Rust interop).
//!   2. **Protokol çerçeveleri** (`Frame`): modüle edilen ses payload'ının
//!      İÇİNDEKİ JSON. `client.py`'deki `handle_frame` ile birebir.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

// --- PHY katmanı parametreleri (netproto.py) --------------------------------

/// Efektif bayt/sn (tasarım tablosu). `airtime()` için.
pub fn rate_for(mode: &str) -> f64 {
    match mode {
        "BPSK" => 125.0,
        "QPSK" => 250.0,
        "16QAM" => 500.0,
        _ => 250.0, // Python: RATE_TABLE["QPSK"]
    }
}

/// sn — her aktarımın sabit senkron+header maliyeti.
pub const PREAMBLE_OVERHEAD: f64 = 0.3;

/// Bir çerçevenin "havada kalma süresi" (saniye). `netproto.airtime`.
pub fn airtime(size_bytes: usize, mode: &str) -> f64 {
    PREAMBLE_OVERHEAD + size_bytes as f64 / rate_for(mode)
}

// --- Süper-çerçeve / NET zamanlama parametreleri ---------------------------

pub const BEACON_INTERVAL: f64 = 8.0;
pub const BEACON_TIMEOUT: f64 = BEACON_INTERVAL * 3.0; // 24 sn
pub const LOST_TIMEOUT: f64 = 30.0;
pub const REMOVE_TIMEOUT: f64 = 120.0;
pub const BLOCK_SIZE: usize = 220;

pub const SAMPLE_RATE: u32 = 8000;

// --- CRC -----------------------------------------------------------------

/// `zlib.crc32(data) & 0xFFFFFFFF` ile birebir (IEEE CRC-32).
pub fn crc32(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(data);
    h.finalize()
}

// --- base64 (netproto.b64e / b64d) -------------------------------------

use base64::Engine as _;
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

pub fn b64e(data: &[u8]) -> String {
    B64.encode(data)
}

pub fn b64d(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    B64.decode(s.as_bytes())
}

// --- Modülasyon modu ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[serde(rename = "BPSK")]
    Bpsk,
    #[serde(rename = "QPSK")]
    Qpsk,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Bpsk => "BPSK",
            Mode::Qpsk => "QPSK",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "BPSK" => Some(Mode::Bpsk),
            "QPSK" => Some(Mode::Qpsk),
            _ => None,
        }
    }
}

// --- Tel mesajları: istemci -> kanal sunucusu -----------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum ClientMsg {
    #[serde(rename = "HELLO")]
    Hello { callsign: String },
    #[serde(rename = "TRANSMIT_AUDIO")]
    TransmitAudio { audio_b64: String },
}

// --- Tel mesajları: kanal sunucusu -> istemci -----------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    #[serde(rename = "TX_GRANTED")]
    TxGranted { duration: f64 },
    #[serde(rename = "CHANNEL_BUSY")]
    ChannelBusy { retry_after: f64 },
    #[serde(rename = "RX_AUDIO")]
    RxAudio { audio_b64: String },
}

// --- Protokol çerçeveleri (modüle edilen payload içindeki JSON) ------------
//
// `client.py`'deki `handle_frame` ile birebir alan adları. Bilinmeyen bir
// çerçeve tipi `Unknown`'a düşer (Python tarafı da bilinmeyeni sessizce
// yok sayıyor).

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Frame {
    #[serde(rename = "JOIN_REQUEST")]
    JoinRequest { src: String, dst: String },

    #[serde(rename = "BEACON")]
    Beacon {
        src: String,
        dst: String,
        #[serde(default)]
        backup: Option<String>,
        #[serde(default)]
        roster: Vec<String>,
    },

    #[serde(rename = "CHAT")]
    Chat {
        src: String,
        dst: String,
        text: String,
    },

    #[serde(rename = "BULK_META")]
    BulkMeta {
        src: String,
        dst: String,
        transfer_id: String,
        filename: String,
        total_blocks: usize,
        total_size: usize,
    },

    #[serde(rename = "BULK_BLOCK")]
    BulkBlock {
        src: String,
        dst: String,
        transfer_id: String,
        seq: usize,
        data: String, // base64
        crc: u32,
    },

    #[serde(rename = "BULK_END")]
    BulkEnd {
        src: String,
        dst: String,
        transfer_id: String,
    },

    #[serde(rename = "BULK_STATUS")]
    BulkStatus {
        src: String,
        dst: String,
        transfer_id: String,
        missing: Vec<usize>,
    },

    #[serde(other)]
    Unknown,
}

impl Frame {
    /// `frame.get("src")` — Python kolaylığı.
    pub fn src(&self) -> Option<&str> {
        match self {
            Frame::JoinRequest { src, .. }
            | Frame::Beacon { src, .. }
            | Frame::Chat { src, .. }
            | Frame::BulkMeta { src, .. }
            | Frame::BulkBlock { src, .. }
            | Frame::BulkEnd { src, .. }
            | Frame::BulkStatus { src, .. } => Some(src),
            Frame::Unknown => None,
        }
    }

    /// `frame.get("dst", "ALL")`.
    pub fn dst(&self) -> &str {
        match self {
            Frame::JoinRequest { dst, .. }
            | Frame::Beacon { dst, .. }
            | Frame::Chat { dst, .. }
            | Frame::BulkMeta { dst, .. }
            | Frame::BulkBlock { dst, .. }
            | Frame::BulkEnd { dst, .. }
            | Frame::BulkStatus { dst, .. } => dst,
            Frame::Unknown => "ALL",
        }
    }

    pub fn to_json_bytes(&self) -> Vec<u8> {
        // `json.dumps(frame, ensure_ascii=False)` karşılığı.
        serde_json::to_vec(self).expect("frame serialize")
    }

    pub fn from_json_bytes(b: &[u8]) -> Option<Frame> {
        serde_json::from_slice(b).ok()
    }
}

// --- Satır bazlı JSON çerçeveleme (netproto.send_json / read_json) ---------

/// `send_json`: tek satır JSON + `\n`, ardından flush.
pub async fn write_json<W, T>(w: &mut W, obj: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut line = serde_json::to_vec(obj).map_err(io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await
}

/// `read_json`: bir satır oku. EOF'ta `Ok(None)`.
pub async fn read_json<R>(r: &mut R) -> io::Result<Option<Value>>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut line = String::new();
    let n = r.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let v = serde_json::from_str(line.trim_end()).map_err(io::Error::other)?;
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_zlib() {
        // zlib.crc32(b"123456789") == 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn airtime_matches_python() {
        assert!((airtime(250, "QPSK") - 1.3).abs() < 1e-9);
        assert!((airtime(125, "BPSK") - 1.3).abs() < 1e-9);
    }

    #[test]
    fn frame_roundtrip_field_names() {
        let f = Frame::Beacon {
            src: "TA1ABC".into(),
            dst: "ALL".into(),
            backup: Some("TA2DEF".into()),
            roster: vec!["TA1ABC".into(), "TA2DEF".into()],
        };
        let s = String::from_utf8(f.to_json_bytes()).unwrap();
        assert!(s.contains(r#""type":"BEACON""#));
        assert!(s.contains(r#""backup":"TA2DEF""#));
        let back = Frame::from_json_bytes(s.as_bytes()).unwrap();
        assert_eq!(back.src(), Some("TA1ABC"));
    }

    #[test]
    fn unknown_frame_does_not_error() {
        let v = br#"{"type":"SOMETHING_NEW","src":"X"}"#;
        assert!(matches!(Frame::from_json_bytes(v), Some(Frame::Unknown)));
    }

    #[test]
    fn wire_msg_tags() {
        let m = ClientMsg::Hello {
            callsign: "TA1ABC".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains(r#""cmd":"HELLO""#));
        let r: ServerMsg = serde_json::from_str(r#"{"type":"TX_GRANTED","duration":1.5}"#).unwrap();
        assert!(matches!(r, ServerMsg::TxGranted { duration } if (duration - 1.5).abs() < 1e-9));
    }
}
