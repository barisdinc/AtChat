//! `ChannelServer` portu — "kanalın fiziği". Protokol mantığı (master
//! seçimi, ARQ, sohbet) İÇERMEZ; yalnız (1) yarı çift yönlü erişimi zorunlu
//! kılar, (2) sesi (bozarak) tüm istasyonlara yayınlar.
//!
//! İki çıkış bus'ı:
//!   - **protokol bus'ı**: bir aktarım = tek `RX_AUDIO` (istasyon demod'u
//!     tam burst tamponu bekler — `client.py` ile aynı).
//!   - **monitör tap**: aynı bozulmuş örnekler ~20 ms'lik hop'larla gerçek
//!     zamanda akıtılır (boştayken sıfır). Scope / waterfall / cpal bunu
//!     tüketir; protokole etkisi yoktur.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{Duration, Instant, MissedTickBehavior};

use netproto::ServerMsg;

use crate::config::{apply_channel, ChannelConfig, SAMPLE_RATE};

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// Monitör tap hop boyutu: 160 örnek @ 8 kHz = 20 ms.
const MONITOR_CHUNK: usize = SAMPLE_RATE as usize / 50;

pub type ClientId = u64;

/// GUI'nin aktivite günlüğü için yayınlanan olaylar.
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    Joined {
        callsign: String,
        active: usize,
    },
    Left {
        callsign: String,
        active: usize,
    },
    TxGranted {
        src: String,
        n_samples: usize,
        duration: f64,
    },
    TxDenied {
        src: String,
        retry_after: f64,
    },
    Delivered {
        src: String,
        n_samples: usize,
    },
    /// Pasif "monitör" çözümü — teslim edilen (bozulmuş) burst demodüle edildi.
    /// `monitor.py`'nin metin günlüğünün karşılığı.
    Decoded {
        duration: f64,
        /// Çözülebildiyse `"TA1ABC -> ALL | CHAT"`, çözülemezse `None`.
        summary: Option<String>,
    },
}

/// GUI'nin her karede okuyabileceği anlık kanal durumu.
#[derive(Debug, Clone)]
pub struct ChannelSnapshot {
    pub busy: bool,
    pub busy_remaining: f64,
    pub current_tx: Option<String>,
    pub active_clients: usize,
    pub cfg: ChannelConfig,
}

struct Client {
    callsign: String,
    out: mpsc::Sender<ServerMsg>,
}

struct Inner {
    clients: HashMap<ClientId, Client>,
    busy_until: Instant,
    busy_src: Option<String>,
    cfg: ChannelConfig,
}

pub struct ChannelCore {
    inner: Mutex<Inner>,
    next_id: AtomicU64,
    /// Teslim edilen burst sayacı (TEST hook'u `corrupt_burst_nums` için).
    burst_count: AtomicU64,
    events: broadcast::Sender<ChannelEvent>,
    monitor: broadcast::Sender<Vec<i16>>,
    /// Monitör pacer'ının gerçek zamanda boşalttığı "havadaki örnekler".
    airwaves: Mutex<VecDeque<i16>>,
    /// Pasif monitör demodülatörü (`ChannelEvent::Decoded` için).
    decoder: modem::Modem,
}

impl ChannelCore {
    /// Kanalı kurar ve monitör pacer görevini başlatır. Bir tokio runtime
    /// içinden çağrılmalı.
    pub fn spawn(cfg: ChannelConfig) -> Arc<Self> {
        let (events, _) = broadcast::channel(512);
        let (monitor, _) = broadcast::channel(128);
        let core = Arc::new(Self {
            inner: Mutex::new(Inner {
                clients: HashMap::new(),
                busy_until: Instant::now(),
                busy_src: None,
                cfg,
            }),
            next_id: AtomicU64::new(1),
            burst_count: AtomicU64::new(0),
            events,
            monitor,
            airwaves: Mutex::new(VecDeque::new()),
            decoder: modem::Modem::new(),
        });

        let pacer = Arc::clone(&core);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let mut chunk = Vec::with_capacity(MONITOR_CHUNK);
                {
                    let mut aw = pacer.airwaves.lock().unwrap();
                    let take = MONITOR_CHUNK.min(aw.len());
                    for _ in 0..take {
                        chunk.push(aw.pop_front().unwrap());
                    }
                }
                chunk.resize(MONITOR_CHUNK, 0);
                let _ = pacer.monitor.send(chunk);
            }
        });

        core
    }

    // -- abonelikler ----------------------------------------------------

    pub fn subscribe_events(&self) -> broadcast::Receiver<ChannelEvent> {
        self.events.subscribe()
    }

    /// Sürekli 8 kHz i16 akışı (20 ms'lik `Vec<i16>` parçaları).
    pub fn subscribe_monitor(&self) -> broadcast::Receiver<Vec<i16>> {
        self.monitor.subscribe()
    }

    // -- istemci kaydı ------------------------------------------------

    pub fn register(&self, callsign: &str) -> (ClientId, mpsc::Receiver<ServerMsg>) {
        let (tx, rx) = mpsc::channel(512);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let active = {
            let mut inner = self.inner.lock().unwrap();
            inner.clients.insert(
                id,
                Client {
                    callsign: callsign.to_string(),
                    out: tx,
                },
            );
            inner.clients.len()
        };
        let _ = self.events.send(ChannelEvent::Joined {
            callsign: callsign.to_string(),
            active,
        });
        (id, rx)
    }

    pub fn deregister(&self, id: ClientId) {
        let removed = {
            let mut inner = self.inner.lock().unwrap();
            inner
                .clients
                .remove(&id)
                .map(|c| (c.callsign, inner.clients.len()))
        };
        if let Some((callsign, active)) = removed {
            let _ = self.events.send(ChannelEvent::Left { callsign, active });
        }
    }

    // -- ayar --------------------------------------------------------

    pub fn set_config(&self, cfg: ChannelConfig) {
        self.inner.lock().unwrap().cfg = cfg;
    }

    pub fn config(&self) -> ChannelConfig {
        self.inner.lock().unwrap().cfg.clone()
    }

    pub fn snapshot(&self) -> ChannelSnapshot {
        let inner = self.inner.lock().unwrap();
        let now = Instant::now();
        let busy = now < inner.busy_until;
        ChannelSnapshot {
            busy,
            busy_remaining: if busy {
                (inner.busy_until - now).as_secs_f64()
            } else {
                0.0
            },
            current_tx: if busy { inner.busy_src.clone() } else { None },
            active_clients: inner.clients.len(),
            cfg: inner.cfg.clone(),
        }
    }

    // -- aktarım ---------------------------------------------------

    /// `handle_transmit` + `deliver_after_delay` portu. Yarı çift yönlü
    /// erişimi zorunlu kılar; kabul edilirse `duration` sonra bozulmuş sesi
    /// TÜM istasyonlara (gönderen dahil) yayınlar.
    pub fn transmit(self: &Arc<Self>, id: ClientId, samples: Vec<i16>) {
        let n = samples.len();
        let duration = n as f64 / SAMPLE_RATE as f64;
        let now = Instant::now();

        let (src, cfg) = {
            let mut inner = self.inner.lock().unwrap();
            let src = match inner.clients.get(&id) {
                Some(c) => c.callsign.clone(),
                None => return,
            };
            if now < inner.busy_until {
                let retry_after = (inner.busy_until - now).as_secs_f64();
                if let Some(c) = inner.clients.get(&id) {
                    let _ = c.out.try_send(ServerMsg::ChannelBusy { retry_after });
                }
                let _ = self
                    .events
                    .send(ChannelEvent::TxDenied { src, retry_after });
                return;
            }
            inner.busy_until = now + Duration::from_secs_f64(duration);
            inner.busy_src = Some(src.clone());
            if let Some(c) = inner.clients.get(&id) {
                let _ = c.out.try_send(ServerMsg::TxGranted { duration });
            }
            (src, inner.cfg.clone())
        };

        let _ = self.events.send(ChannelEvent::TxGranted {
            src: src.clone(),
            n_samples: n,
            duration,
        });

        let burst_num = self.burst_count.fetch_add(1, Ordering::Relaxed) + 1;

        // Bozulmayı BİR kez hesapla; hem monitör hem protokol aynı sesi duysun.
        let distorted = {
            let mut rng = rand::thread_rng();
            let mut d = apply_channel(&samples, &cfg, &mut rng);
            if cfg.corrupt_burst_nums.contains(&burst_num) {
                // TEST hook'u: bu burst'ü tamamen sıfırla -> demod kesin başarısız.
                d.iter_mut().for_each(|s| *s = 0);
            }
            Arc::new(d)
        };

        // Monitör: hemen airwaves'e (pacer gerçek zamanda boşaltır).
        {
            let mut aw = self.airwaves.lock().unwrap();
            aw.extend(distorted.iter().copied());
        }

        // Protokol: airtime sonunda tek RX_AUDIO.
        let core = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(duration)).await;

            let mut bytes = Vec::with_capacity(distorted.len() * 2);
            for s in distorted.iter() {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            let msg = ServerMsg::RxAudio {
                audio_b64: BASE64.encode(&bytes),
            };

            let targets: Vec<mpsc::Sender<ServerMsg>> = {
                let inner = core.inner.lock().unwrap();
                inner.clients.values().map(|c| c.out.clone()).collect()
            };
            for t in targets {
                let _ = t.try_send(msg.clone());
            }

            // Pasif monitör çözümü (monitor.py karşılığı).
            let summary = core.decoder.demodulate(&distorted).and_then(|payload| {
                serde_json::from_slice::<serde_json::Value>(&payload)
                    .ok()
                    .map(|v| {
                        format!(
                            "{} -> {} | {}",
                            v.get("src").and_then(|x| x.as_str()).unwrap_or("?"),
                            v.get("dst").and_then(|x| x.as_str()).unwrap_or("ALL"),
                            v.get("type").and_then(|x| x.as_str()).unwrap_or("?"),
                        )
                    })
            });
            let _ = core
                .events
                .send(ChannelEvent::Decoded { duration, summary });

            let _ = core.events.send(ChannelEvent::Delivered {
                src,
                n_samples: distorted.len(),
            });
        });
    }
}
