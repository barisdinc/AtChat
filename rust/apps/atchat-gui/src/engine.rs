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
    /// Bağlı TÜM istasyonlar aynı mesajı `dst`'e gönderir.
    ChatAll {
        dst: String,
        text: String,
    },
    SendFile {
        callsign: String,
        path: PathBuf,
        dst: String,
    },
    /// Bağlı TÜM istasyonlar aynı dosyayı `dst`'e gönderir.
    SendFileAll {
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
    /// Otomatik sohbet: kapalı, ya da her `interval_ms`'de rastgele bir
    /// istasyon ALL'a kısa bir mesaj atar.
    SetAutoChat {
        enabled: bool,
        interval_ms: u64,
    },
    /// İç kullanım — otomatik sohbet zamanlayıcısının tetiklediği tik.
    AutoTick,
}

const AUTO_PHRASES: &[&str] = &[
    "test test",
    "sinyal 59",
    "roger",
    "QSL",
    "kanal nasıl?",
    "waterfall temiz",
    "beklemede",
    "grup çağrısı",
    "buradayım",
    "kopyala",
    "anlaşıldı",
    "10-4",
    "sıcaklık normal",
    "rapor bekliyorum",
];

#[derive(Clone)]
pub struct ChatLine {
    pub from: String,
    pub dst: String,
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
    /// Tüm istasyonların gönderdiği sohbet, kronolojik ve birleşik (NET sekmesi).
    pub net_chat: Vec<ChatLine>,
    pub auto_chat_on: bool,
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
            cmd_tx: cmd_tx.clone(),
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
                    cmd_tx,
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
    cmd_tx: UnboundedSender<EngineCmd>,
    snapshot: Arc<Mutex<EngineSnapshot>>,
    gui_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_enabled: Arc<AtomicBool>,
    repaint: impl Fn() + Send + 'static,
) {
    let core = ChannelCore::spawn(ChannelConfig::default());
    let mut slots: BTreeMap<String, StationSlot> = BTreeMap::new();
    let net_chat = Arc::new(Mutex::new(VecDeque::<ChatLine>::new()));
    let mut auto_chat: Option<tokio::task::JoinHandle<()>> = None;

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
                    s.net_chat = net_chat.lock().unwrap().iter().cloned().collect();
                    s.auto_chat_on = auto_chat.is_some();
                }
                repaint();
            }
            Some(cmd) = cmd_rx.recv() => {
                handle_cmd(cmd, &core, &mut slots, &net_chat, &mut auto_chat, &cmd_tx).await;
            }
            else => break,
        }
    }
}

fn push_own_chat(
    slot: &StationSlot,
    net_chat: &Mutex<VecDeque<ChatLine>>,
    from: &str,
    dst: &str,
    text: &str,
) {
    let line = ChatLine {
        from: from.to_string(),
        dst: dst.to_string(),
        text: text.to_string(),
        private: dst != "ALL",
        own: true,
    };
    push_cap(&slot.chat, line.clone(), 200);
    push_cap(net_chat, line, 400);
    slot.station.chat_bg(text, dst);
}

async fn handle_cmd(
    cmd: EngineCmd,
    core: &Arc<ChannelCore>,
    slots: &mut BTreeMap<String, StationSlot>,
    net_chat: &Arc<Mutex<VecDeque<ChatLine>>>,
    auto_chat: &mut Option<tokio::task::JoinHandle<()>>,
    cmd_tx: &UnboundedSender<EngineCmd>,
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
                                        dst: if scope == ChatScope::Private {
                                            "(özel)".into()
                                        } else {
                                            "ALL".into()
                                        },
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
            let c = callsign.to_uppercase();
            if let Some(sl) = slots.get(&c) {
                push_own_chat(sl, net_chat, &c, &dst, &text);
            }
        }
        EngineCmd::ChatAll { dst, text } => {
            for (c, sl) in slots.iter() {
                if sl.station.is_connected() {
                    push_own_chat(sl, net_chat, c, &dst, &text);
                }
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
        EngineCmd::SendFileAll { path, dst } => {
            for sl in slots.values() {
                if sl.station.is_connected() {
                    sl.station.send_file(path.clone(), &dst);
                }
            }
        }
        EngineCmd::SetAutoChat {
            enabled,
            interval_ms,
        } => {
            if let Some(h) = auto_chat.take() {
                h.abort();
            }
            if enabled {
                let tx = cmd_tx.clone();
                let d = Duration::from_millis(interval_ms.max(500));
                *auto_chat = Some(tokio::spawn(async move {
                    let mut t = tokio::time::interval(d);
                    t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        t.tick().await;
                        if tx.send(EngineCmd::AutoTick).is_err() {
                            break;
                        }
                    }
                }));
            }
        }
        EngineCmd::AutoTick => {
            use rand::seq::SliceRandom;
            let mut rng = rand::thread_rng();
            let live: Vec<&StationSlot> = slots
                .values()
                .filter(|s| s.station.is_connected())
                .collect();
            if let Some(&sl) = live.choose(&mut rng) {
                let phrase = AUTO_PHRASES.choose(&mut rng).copied().unwrap_or("test");
                push_own_chat(sl, net_chat, sl.station.callsign(), "ALL", phrase);
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

        // net_chat: gönderen kendi mesajını birleşik akışta görmeli.
        assert!(
            wait_until(&h, 3, |s| s
                .net_chat
                .iter()
                .any(|c| c.from == "TA1ABC" && c.text == "motor testi")),
            "net_chat akışında mesaj yok"
        );
    }

    #[test]
    fn chat_all_and_auto_chat() {
        let h = EngineHandle::spawn(egui::Context::default());
        for c in ["TA1ABC", "TA2DEF", "TA3GHI"] {
            h.send(EngineCmd::AddStation {
                callsign: c.into(),
                mode: netproto::Mode::Qpsk,
            });
        }
        assert!(wait_until(&h, 10, |s| s.stations.len() == 3));

        // ChatAll -> her istasyon gönderir -> net_chat'te 3 farklı kaynak.
        h.send(EngineCmd::ChatAll {
            dst: "ALL".into(),
            text: "toplu selam".into(),
        });
        assert!(
            wait_until(&h, 5, |s| {
                let senders: std::collections::BTreeSet<_> = s
                    .net_chat
                    .iter()
                    .filter(|c| c.text == "toplu selam")
                    .map(|c| c.from.as_str())
                    .collect();
                senders.len() == 3
            }),
            "ChatAll üç istasyondan da net_chat'e düşmeliydi"
        );

        // Otomatik sohbet: açınca net_chat büyümeye devam etmeli.
        h.send(EngineCmd::SetAutoChat {
            enabled: true,
            interval_ms: 800,
        });
        assert!(wait_until(&h, 3, |s| s.auto_chat_on));
        let n0 = h.snapshot.lock().unwrap().net_chat.len();
        assert!(
            wait_until(&h, 6, |s| s.net_chat.len() > n0),
            "otomatik sohbet net_chat'i büyütmeliydi"
        );
        h.send(EngineCmd::SetAutoChat {
            enabled: false,
            interval_ms: 800,
        });
        assert!(wait_until(&h, 3, |s| !s.auto_chat_on));
    }
}
