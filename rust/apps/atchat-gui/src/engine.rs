//! Motor: ayrı bir thread'de tokio runtime çalıştırır; `ChannelCore` +
//! istasyonları barındırır. GUI ↔ motor: komutlar `mpsc` ile içeri,
//! durum `Arc<Mutex<EngineSnapshot>>` ile dışarı, monitör örnekleri
//! paylaşımlı halka tamponlarına.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use channel::{ChannelConfig, ChannelCore, ChannelEvent, ChannelSnapshot, InProcConnector};
use protocol::{ChatScope, Station, StationConfig, StationEvent, StationSnapshot};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

pub const GUI_RING_CAP: usize = 24_000; // ~3 sn @ 8 kHz
pub const AUDIO_RING_CAP: usize = 32_000;

#[derive(Debug, Clone)]
pub enum EngineCmd {
    AddStation {
        callsign: String,
        mode: netproto::Mode,
    },
    RemoveStation {
        callsign: String,
    },
    Chat {
        callsign: String,
        dst: String,
        text: String,
    },
    SendFile {
        callsign: String,
        path: PathBuf,
        dst: String,
    },
    Drop {
        callsign: String,
    },
    Reconnect {
        callsign: String,
    },
    SetChannel(ChannelConfig),
}

#[derive(Clone)]
pub struct ChatLine {
    pub from: String,
    pub text: String,
    pub private: bool,
    pub own: bool,
}

#[derive(Clone)]
pub struct StationView {
    pub snap: StationSnapshot,
    pub log: Vec<String>,
    pub chat: Vec<ChatLine>,
}

#[derive(Default, Clone)]
pub struct EngineSnapshot {
    pub channel: Option<ChannelSnapshot>,
    pub channel_log: Vec<String>,
    pub monitor_decodes: Vec<String>,
    pub stations: Vec<StationView>,
}

pub struct EngineHandle {
    cmd_tx: UnboundedSender<EngineCmd>,
    pub snapshot: Arc<Mutex<EngineSnapshot>>,
    pub gui_ring: Arc<Mutex<VecDeque<i16>>>,
    pub audio_ring: Arc<Mutex<VecDeque<i16>>>,
    pub audio_enabled: Arc<AtomicBool>,
}

impl EngineHandle {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let snapshot = Arc::new(Mutex::new(EngineSnapshot::default()));
        let gui_ring = Arc::new(Mutex::new(VecDeque::new()));
        let audio_ring = Arc::new(Mutex::new(VecDeque::new()));
        let audio_enabled = Arc::new(AtomicBool::new(false));

        let handle = Self {
            cmd_tx,
            snapshot: Arc::clone(&snapshot),
            gui_ring: Arc::clone(&gui_ring),
            audio_ring: Arc::clone(&audio_ring),
            audio_enabled: Arc::clone(&audio_enabled),
        };

        std::thread::Builder::new()
            .name("atchat-engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(engine_main(
                    cmd_rx,
                    snapshot,
                    gui_ring,
                    audio_ring,
                    audio_enabled,
                    move || ctx.request_repaint(),
                ));
            })
            .expect("engine thread");

        handle
    }

    pub fn send(&self, cmd: EngineCmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

// ------------------------------------------------------------------ //

struct StationSlot {
    station: Station<InProcConnector>,
    log: Arc<Mutex<VecDeque<String>>>,
    chat: Arc<Mutex<VecDeque<ChatLine>>>,
    _drain: tokio::task::JoinHandle<()>,
}

fn push_cap<T>(q: &Mutex<VecDeque<T>>, item: T, cap: usize) {
    let mut q = q.lock().unwrap();
    q.push_back(item);
    while q.len() > cap {
        q.pop_front();
    }
}

fn fill_ring(r: &Mutex<VecDeque<i16>>, chunk: &[i16], cap: usize) {
    let mut r = r.lock().unwrap();
    r.extend(chunk.iter().copied());
    let over = r.len().saturating_sub(cap);
    for _ in 0..over {
        r.pop_front();
    }
}

fn fmt_channel_event(e: &ChannelEvent) -> String {
    match e {
        ChannelEvent::Joined { callsign, active } => {
            format!("{callsign} bağlandı ({active} aktif)")
        }
        ChannelEvent::Left { callsign, active } => {
            format!("{callsign} ayrıldı ({active} aktif)")
        }
        ChannelEvent::TxGranted {
            src,
            n_samples,
            duration,
        } => {
            format!("{src} yayında | {n_samples} örnek | {duration:.2}sn")
        }
        ChannelEvent::TxDenied { src, retry_after } => {
            format!("{src} MEŞGUL | retry {retry_after:.2}sn")
        }
        ChannelEvent::Delivered { .. } => String::new(),
        ChannelEvent::Decoded { .. } => String::new(),
    }
}

async fn engine_main(
    mut cmd_rx: UnboundedReceiver<EngineCmd>,
    snapshot: Arc<Mutex<EngineSnapshot>>,
    gui_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_enabled: Arc<AtomicBool>,
    repaint: impl Fn() + Send + 'static,
) {
    let core = ChannelCore::spawn(ChannelConfig::default());
    let mut slots: BTreeMap<String, StationSlot> = BTreeMap::new();

    let ch_log = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let mon_dec = Arc::new(Mutex::new(VecDeque::<String>::new()));

    // Kanal olayları -> günlük + monitör çözüm şeridi.
    {
        let mut ev = core.subscribe_events();
        let (cl, md) = (Arc::clone(&ch_log), Arc::clone(&mon_dec));
        tokio::spawn(async move {
            loop {
                match ev.recv().await {
                    Ok(ChannelEvent::Decoded { duration, summary }) => {
                        let line = match summary {
                            Some(s) => format!("{s} | {duration:.2}sn | çözüldü"),
                            None => format!("{duration:.2}sn | çözülemedi"),
                        };
                        push_cap(&md, line, 120);
                    }
                    Ok(other) => {
                        let s = fmt_channel_event(&other);
                        if !s.is_empty() {
                            push_cap(&cl, s, 200);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }

    // Monitör tap -> GUI halkası (+ ses halkası açıksa).
    {
        let mut mon = core.subscribe_monitor();
        let (g, a, ae) = (
            Arc::clone(&gui_ring),
            Arc::clone(&audio_ring),
            Arc::clone(&audio_enabled),
        );
        tokio::spawn(async move {
            loop {
                match mon.recv().await {
                    Ok(chunk) => {
                        fill_ring(&g, &chunk, GUI_RING_CAP);
                        if ae.load(Ordering::Relaxed) {
                            fill_ring(&a, &chunk, AUDIO_RING_CAP);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }

    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let stations: Vec<StationView> = slots
                    .values()
                    .map(|sl| StationView {
                        snap: sl.station.snapshot(),
                        log: sl.log.lock().unwrap().iter().cloned().collect(),
                        chat: sl.chat.lock().unwrap().iter().cloned().collect(),
                    })
                    .collect();
                {
                    let mut s = snapshot.lock().unwrap();
                    s.channel = Some(core.snapshot());
                    s.channel_log = ch_log.lock().unwrap().iter().cloned().collect();
                    s.monitor_decodes = mon_dec.lock().unwrap().iter().cloned().collect();
                    s.stations = stations;
                }
                repaint();
            }
            Some(cmd) = cmd_rx.recv() => {
                handle_cmd(cmd, &core, &mut slots).await;
            }
            else => break,
        }
    }
}

async fn handle_cmd(
    cmd: EngineCmd,
    core: &Arc<ChannelCore>,
    slots: &mut BTreeMap<String, StationSlot>,
) {
    match cmd {
        EngineCmd::AddStation { callsign, mode } => {
            let c = callsign.trim().to_uppercase();
            if c.is_empty() || slots.contains_key(&c) {
                return;
            }
            let conn = InProcConnector::new(Arc::clone(core), c.clone());
            match Station::start(conn, &c, mode, StationConfig::default()).await {
                Ok(station) => {
                    let log = Arc::new(Mutex::new(VecDeque::new()));
                    let chat = Arc::new(Mutex::new(VecDeque::new()));
                    let mut ev = station.subscribe();
                    let (l2, c2) = (Arc::clone(&log), Arc::clone(&chat));
                    let drain = tokio::spawn(async move {
                        loop {
                            match ev.recv().await {
                                Ok(StationEvent::Log(s)) => push_cap(&l2, s, 300),
                                Ok(StationEvent::Chat { from, scope, text }) => push_cap(
                                    &c2,
                                    ChatLine {
                                        from,
                                        text,
                                        private: scope == ChatScope::Private,
                                        own: false,
                                    },
                                    200,
                                ),
                                Ok(_) => {}
                                Err(broadcast::error::RecvError::Lagged(_)) => {}
                                Err(_) => break,
                            }
                        }
                    });
                    slots.insert(
                        c,
                        StationSlot {
                            station,
                            log,
                            chat,
                            _drain: drain,
                        },
                    );
                }
                Err(e) => tracing::error!("istasyon eklenemedi: {e}"),
            }
        }
        EngineCmd::RemoveStation { callsign } => {
            slots.remove(&callsign.to_uppercase());
        }
        EngineCmd::Chat {
            callsign,
            dst,
            text,
        } => {
            if let Some(sl) = slots.get(&callsign.to_uppercase()) {
                push_cap(
                    &sl.chat,
                    ChatLine {
                        from: callsign.to_uppercase(),
                        text: text.clone(),
                        private: dst != "ALL",
                        own: true,
                    },
                    200,
                );
                sl.station.chat_bg(&text, &dst);
            }
        }
        EngineCmd::SendFile {
            callsign,
            path,
            dst,
        } => {
            if let Some(sl) = slots.get(&callsign.to_uppercase()) {
                sl.station.send_file(path, &dst);
            }
        }
        EngineCmd::Drop { callsign } => {
            if let Some(sl) = slots.get(&callsign.to_uppercase()) {
                sl.station.drop_bg();
            }
        }
        EngineCmd::Reconnect { callsign } => {
            if let Some(sl) = slots.get(&callsign.to_uppercase()) {
                sl.station.reconnect_bg();
            }
        }
        EngineCmd::SetChannel(cfg) => core.set_config(cfg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait_until<F: Fn(&EngineSnapshot) -> bool>(h: &EngineHandle, secs: u64, pred: F) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if pred(&h.snapshot.lock().unwrap()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn engine_adds_stations_and_relays_chat() {
        let h = EngineHandle::spawn(egui::Context::default());

        h.send(EngineCmd::AddStation {
            callsign: "TA1ABC".into(),
            mode: netproto::Mode::Qpsk,
        });
        h.send(EngineCmd::AddStation {
            callsign: "TA2DEF".into(),
            mode: netproto::Mode::Qpsk,
        });

        assert!(
            wait_until(&h, 10, |s| s.stations.len() == 2),
            "iki istasyon snapshot'ta görünmeliydi"
        );

        h.send(EngineCmd::Chat {
            callsign: "TA1ABC".into(),
            dst: "ALL".into(),
            text: "motor testi".into(),
        });

        assert!(
            wait_until(&h, 20, |s| {
                s.stations
                    .iter()
                    .find(|v| v.snap.callsign == "TA2DEF")
                    .map(|v| v.chat.iter().any(|c| c.text == "motor testi" && !c.own))
                    .unwrap_or(false)
            }),
            "TA2DEF sohbeti motor üzerinden almalıydı"
        );

        // Kendi gönderdiği satır TA1ABC'de 'own' olarak görünmeli.
        let own_ok = h.snapshot.lock().unwrap().stations.iter().any(|v| {
            v.snap.callsign == "TA1ABC" && v.chat.iter().any(|c| c.own && c.text == "motor testi")
        });
        assert!(own_ok, "gönderen kendi mesajını görmeli");

        // Monitör çözüm şeridi bir şeyler yakalamış olmalı.
        assert!(
            wait_until(&h, 5, |s| !s.monitor_decodes.is_empty()),
            "monitör çözüm şeridi boş kaldı"
        );
    }
}
