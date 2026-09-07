//! `Link` — bir istasyonun kanala bağlantısı, **okuyucu ve yazıcı yarımları
//! ayrı**. `client.py` modeli: tek bir `receive_loop` okur, `send_frame`
//! (bir `tx_lock` ardında) yazar — bu ayrım o modele birebir oturur.
//!
//! İki gerçekleştirme:
//!   - [`InProcConnector`]: aynı süreç içindeki [`ChannelCore`]'a doğrudan
//!     bağlanır (GUI'deki tüm istasyonlar).
//!   - [`TcpConnector`]: `channel_server.py` / `atchat-channeld` ile
//!     tel-uyumlu satır-JSON (Python interop, dağıtık kurulum).

// `LinkTx`/`LinkRx` metotları bilerek `-> impl Future + Send + '_`: `Station`
// bu future'ları başka görevlerde kullanacağından generic `Send` sınırı şart.
// `async fn` sözdizimi bu sınırı ifade edemez.
#![allow(clippy::manual_async_fn)]

use std::future::Future;
use std::sync::Arc;

use base64::Engine as _;
use netproto::{ClientMsg, ServerMsg};
use tokio::io::{BufReader, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::core::{ChannelCore, ClientId};

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

pub fn samples_to_b64(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    BASE64.encode(&bytes)
}

pub fn b64_to_samples(b64: &str) -> anyhow::Result<Vec<i16>> {
    let bytes = BASE64.decode(b64.as_bytes())?;
    Ok(bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

// ------------------------------------------------------------------ //
// Trait'ler
// ------------------------------------------------------------------ //

/// İstemci -> sunucu yönü (`send_json`).
pub trait LinkTx: Send + 'static {
    fn send(&mut self, msg: ClientMsg) -> impl Future<Output = anyhow::Result<()>> + Send + '_;
    /// Bağlantıyı kapat (istasyonun `/drop`'u). InProc'ta kanal kaydını siler.
    fn close(&mut self) {}
}

/// Sunucu -> istemci yönü (`read_json`). `None` -> bağlantı koptu.
pub trait LinkRx: Send + 'static {
    fn recv(&mut self) -> impl Future<Output = Option<ServerMsg>> + Send + '_;
}

/// Yeni bir (tx, rx) çifti üretebilen bağlantı fabrikası — `/reconnect` için.
pub trait Connector: Send + Sync + 'static {
    type Tx: LinkTx;
    type Rx: LinkRx;
    fn connect(&self) -> impl Future<Output = anyhow::Result<(Self::Tx, Self::Rx)>> + Send + '_;
}

// ------------------------------------------------------------------ //
// InProc
// ------------------------------------------------------------------ //

pub struct InProcTx {
    core: Arc<ChannelCore>,
    id: ClientId,
}

pub struct InProcRx {
    rx: mpsc::Receiver<ServerMsg>,
}

impl LinkTx for InProcTx {
    fn send(&mut self, msg: ClientMsg) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            match msg {
                ClientMsg::Hello { .. } => {} // connect() sırasında yapıldı
                ClientMsg::TransmitAudio { audio_b64 } => {
                    let samples = b64_to_samples(&audio_b64)?;
                    self.core.transmit(self.id, samples);
                }
            }
            Ok(())
        }
    }

    fn close(&mut self) {
        self.core.deregister(self.id);
    }
}

impl LinkRx for InProcRx {
    fn recv(&mut self) -> impl Future<Output = Option<ServerMsg>> + Send + '_ {
        async move { self.rx.recv().await }
    }
}

/// Aynı süreçteki [`ChannelCore`]'a bağlanır.
pub struct InProcConnector {
    pub core: Arc<ChannelCore>,
    pub callsign: String,
}

impl InProcConnector {
    pub fn new(core: Arc<ChannelCore>, callsign: impl Into<String>) -> Self {
        Self {
            core,
            callsign: callsign.into(),
        }
    }
}

impl Connector for InProcConnector {
    type Tx = InProcTx;
    type Rx = InProcRx;

    fn connect(&self) -> impl Future<Output = anyhow::Result<(Self::Tx, Self::Rx)>> + Send + '_ {
        async move {
            let (id, rx) = self.core.register(&self.callsign);
            Ok((
                InProcTx {
                    core: Arc::clone(&self.core),
                    id,
                },
                InProcRx { rx },
            ))
        }
    }
}

// ------------------------------------------------------------------ //
// TCP
// ------------------------------------------------------------------ //

pub struct TcpTx {
    writer: BufWriter<OwnedWriteHalf>,
}

pub struct TcpRx {
    reader: BufReader<OwnedReadHalf>,
}

impl LinkTx for TcpTx {
    fn send(&mut self, msg: ClientMsg) -> impl Future<Output = anyhow::Result<()>> + Send + '_ {
        async move {
            netproto::write_json(&mut self.writer, &msg).await?;
            Ok(())
        }
    }
}

impl LinkRx for TcpRx {
    fn recv(&mut self) -> impl Future<Output = Option<ServerMsg>> + Send + '_ {
        async move {
            loop {
                match netproto::read_json(&mut self.reader).await {
                    Ok(Some(v)) => {
                        if let Ok(m) = serde_json::from_value::<ServerMsg>(v) {
                            return Some(m);
                        }
                        // Bilinmeyen sunucu mesajı -> yok say, okumaya devam.
                    }
                    _ => return None,
                }
            }
        }
    }
}

/// `atchat-channeld` / `channel_server.py`'ye TCP ile bağlanır.
pub struct TcpConnector {
    pub addr: String,
    pub callsign: String,
}

impl TcpConnector {
    pub fn new(addr: impl Into<String>, callsign: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            callsign: callsign.into(),
        }
    }

    /// Tek seferlik bağlantı (fabrikaya ihtiyaç duymayan testler için).
    pub async fn connect_once(addr: &str, callsign: &str) -> anyhow::Result<(TcpTx, TcpRx)> {
        let c = TcpConnector::new(addr, callsign);
        c.connect().await
    }
}

impl Connector for TcpConnector {
    type Tx = TcpTx;
    type Rx = TcpRx;

    fn connect(&self) -> impl Future<Output = anyhow::Result<(Self::Tx, Self::Rx)>> + Send + '_ {
        async move {
            let stream = TcpStream::connect(&self.addr).await?;
            stream.set_nodelay(true).ok();
            let (r, w) = stream.into_split();
            let mut writer = BufWriter::new(w);
            netproto::write_json(
                &mut writer,
                &ClientMsg::Hello {
                    callsign: self.callsign.clone(),
                },
            )
            .await?;
            Ok((
                TcpTx { writer },
                TcpRx {
                    reader: BufReader::new(r),
                },
            ))
        }
    }
}
