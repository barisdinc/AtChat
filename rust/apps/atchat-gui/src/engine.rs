//! Motor: ayrı bir thread'de tokio runtime çalıştırır. Dört mod:
//!   - `spawn_all_in_one` — in-proc kanal + in-proc istasyonlar + monitör tap
//!   - `spawn_channel_host` — in-proc kanal + TCP sunucu (başka process'ler bağlanır)
//!   - `spawn_client` — TCP ile kanala bağlanan istasyonlar (çoklu pencere)
//!   - `spawn_monitor` — TCP ile kanalı pasif dinleyen monitör
//!
//! GUI ↔ motor: komutlar `mpsc` ile içeri, durum `Arc<Mutex<EngineSnapshot>>`
//! ile dışarı, monitör örnekleri paylaşımlı halka tamponlarına.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use channel::{
    b64_to_samples, ChannelConfig, ChannelCore, ChannelEvent, ChannelSnapshot, Connector,
    InProcConnector, LinkRx, TcpConnector,
};
use netproto::ServerMsg;
use protocol::{ChatScope, Station, StationConfig, StationEvent, StationSnapshot};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

pub const GUI_RING_CAP: usize = 24_000; // ~3 sn @ 8 kHz
pub const AUDIO_RING_CAP: usize = 32_000;
const MONITOR_HOP: usize = 160; // 20 ms @ 8 kHz

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
    /// Otomatik sohbet: rastgele bir istasyon her `interval_ms`'de ALL'a yazar.
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
    /// Bu motorun bağlı olduğu adres (client/monitör modları).
    pub link_addr: Option<String>,
}

pub struct EngineHandle {
    cmd_tx: UnboundedSender<EngineCmd>,
    pub snapshot: Arc<Mutex<EngineSnapshot>>,
    pub gui_ring: Arc<Mutex<VecDeque<i16>>>,
    pub audio_ring: Arc<Mutex<VecDeque<i16>>>,
    pub audio_enabled: Arc<AtomicBool>,
}

struct Shared {
    cmd_tx: UnboundedSender<EngineCmd>,
    cmd_rx: UnboundedReceiver<EngineCmd>,
    snapshot: Arc<Mutex<EngineSnapshot>>,
    gui_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_enabled: Arc<AtomicBool>,
}

impl EngineHandle {
    fn new() -> (Self, Shared) {
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let snapshot = Arc::new(Mutex::new(EngineSnapshot::default()));
        let gui_ring = Arc::new(Mutex::new(VecDeque::new()));
        let audio_ring = Arc::new(Mutex::new(VecDeque::new()));
        let audio_enabled = Arc::new(AtomicBool::new(false));
        (
            Self {
                cmd_tx: cmd_tx.clone(),
                snapshot: Arc::clone(&snapshot),
                gui_ring: Arc::clone(&gui_ring),
                audio_ring: Arc::clone(&audio_ring),
                audio_enabled: Arc::clone(&audio_enabled),
            },
            Shared {
                cmd_tx,
                cmd_rx,
                snapshot,
                gui_ring,
                audio_ring,
                audio_enabled,
            },
        )
    }

    fn spawn_with<F, Fut>(ctx: egui::Context, name: &str, run: F) -> Self
    where
        F: FnOnce(Shared, Box<dyn Fn() + Send>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()>,
    {
        let (handle, shared) = Self::new();
        let repaint: Box<dyn Fn() + Send> = Box::new(move || ctx.request_repaint());
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(run(shared, repaint));
            })
            .expect("engine thread");
        handle
    }

    pub fn spawn_all_in_one(ctx: egui::Context) -> Self {
        Self::spawn_with(ctx, "atchat-engine", engine_all_in_one)
    }

    pub fn spawn_channel_host(ctx: egui::Context, port: u16) -> Self {
        Self::spawn_with(ctx, "atchat-channel", move |s, r| {
            engine_channel_host(s, r, port)
        })
    }

    pub fn spawn_client(ctx: egui::Context, addr: String) -> Self {
        Self::spawn_with(ctx, "atchat-client", move |s, r| engine_client(s, r, addr))
    }

    pub fn spawn_monitor(ctx: egui::Context, addr: String) -> Self {
        Self::spawn_with(ctx, "atchat-monitor", move |s, r| {
            engine_monitor(s, r, addr)
        })
    }

    pub fn send(&self, cmd: EngineCmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

// ------------------------------------------------------------------ //
// İstasyon yönetimi (in-proc / TCP fark etmez — sadece Connector değişir)
// ------------------------------------------------------------------ //

struct StationSlot<C: Connector> {
    station: Station<C>,
    log: Arc<Mutex<VecDeque<String>>>,
    chat: Arc<Mutex<VecDeque<ChatLine>>>,
    _drain: JoinHandle<()>,
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

fn push_own_chat<C: Connector>(
    slot: &StationSlot<C>,
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

/// Gelen bir sohbeti birleşik akışa ekle — aynı mesaj birden çok yerel
/// istasyonca alınırsa (ve kendi gönderdiysem) tek satır kalsın diye
/// son satırlara karşı tekilleştirir.
fn push_recv_chat(net_chat: &Mutex<VecDeque<ChatLine>>, line: ChatLine) {
    let mut nc = net_chat.lock().unwrap();
    let dup = nc
        .iter()
        .rev()
        .take(16)
        .any(|l| l.from == line.from && l.text == line.text && l.dst == line.dst);
    if !dup {
        nc.push_back(line);
        while nc.len() > 400 {
            nc.pop_front();
        }
    }
}

async fn add_station<C: Connector>(
    slots: &mut BTreeMap<String, StationSlot<C>>,
    callsign: &str,
    mode: netproto::Mode,
    connector: C,
    net_chat: &Arc<Mutex<VecDeque<ChatLine>>>,
) {
    let c = callsign.trim().to_uppercase();
    if c.is_empty() || slots.contains_key(&c) {
        return;
    }
    match Station::start(connector, &c, mode, StationConfig::default()).await {
        Ok(station) => {
            let log = Arc::new(Mutex::new(VecDeque::new()));
            let chat = Arc::new(Mutex::new(VecDeque::new()));
            let mut ev = station.subscribe();
            let (l2, c2, nc2) = (Arc::clone(&log), Arc::clone(&chat), Arc::clone(net_chat));
            let drain = tokio::spawn(async move {
                loop {
                    match ev.recv().await {
                        Ok(StationEvent::Log(s)) => push_cap(&l2, s, 300),
                        Ok(StationEvent::Chat { from, scope, text }) => {
                            let private = scope == ChatScope::Private;
                            let line = ChatLine {
                                from,
                                dst: if private {
                                    "(özel)".into()
                                } else {
                                    "ALL".into()
                                },
                                text,
                                private,
                                own: false,
                            };
                            push_cap(&c2, line.clone(), 200);
                            push_recv_chat(&nc2, line);
                        }
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

fn station_views<C: Connector>(slots: &BTreeMap<String, StationSlot<C>>) -> Vec<StationView> {
    slots
        .values()
        .map(|sl| StationView {
            snap: sl.station.snapshot(),
            log: sl.log.lock().unwrap().iter().cloned().collect(),
            chat: sl.chat.lock().unwrap().iter().cloned().collect(),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn handle_station_cmd<C, F>(
    cmd: EngineCmd,
    slots: &mut BTreeMap<String, StationSlot<C>>,
    make_conn: &F,
    net_chat: &Arc<Mutex<VecDeque<ChatLine>>>,
    auto_chat: &mut Option<JoinHandle<()>>,
    cmd_tx: &UnboundedSender<EngineCmd>,
) where
    C: Connector,
    F: Fn(&str) -> C,
{
    match cmd {
        EngineCmd::AddStation { callsign, mode } => {
            let conn = make_conn(&callsign);
            add_station(slots, &callsign, mode, conn, net_chat).await;
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
            let live: Vec<&StationSlot<C>> = slots
                .values()
                .filter(|s| s.station.is_connected())
                .collect();
            if let Some(&sl) = live.choose(&mut rng) {
                let phrase = AUTO_PHRASES.choose(&mut rng).copied().unwrap_or("test");
                push_own_chat(sl, net_chat, sl.station.callsign(), "ALL", phrase);
            }
        }
        EngineCmd::SetChannel(_) => {} // yalnız kanalı barındıran motor işler
    }
}

// ------------------------------------------------------------------ //
// Ortak arka plan görevleri
// ------------------------------------------------------------------ //

fn spawn_channel_log_task(
    core: &Arc<ChannelCore>,
    ch_log: Arc<Mutex<VecDeque<String>>>,
    mon_dec: Arc<Mutex<VecDeque<String>>>,
) {
    let mut ev = core.subscribe_events();
    tokio::spawn(async move {
        loop {
            match ev.recv().await {
                Ok(ChannelEvent::Decoded { duration, summary }) => {
                    let line = match summary {
                        Some(s) => format!("{s} | {duration:.2}sn | çözüldü"),
                        None => format!("{duration:.2}sn | çözülemedi"),
                    };
                    push_cap(&mon_dec, line, 120);
                }
                Ok(other) => {
                    let s = fmt_channel_event(&other);
                    if !s.is_empty() {
                        push_cap(&ch_log, s, 200);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}

fn spawn_monitor_tap_task(
    core: &Arc<ChannelCore>,
    gui_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_enabled: Arc<AtomicBool>,
) {
    let mut mon = core.subscribe_monitor();
    tokio::spawn(async move {
        loop {
            match mon.recv().await {
                Ok(chunk) => {
                    fill_ring(&gui_ring, &chunk, GUI_RING_CAP);
                    if audio_enabled.load(Ordering::Relaxed) {
                        fill_ring(&audio_ring, &chunk, AUDIO_RING_CAP);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}

fn fmt_channel_event(e: &ChannelEvent) -> String {
    match e {
        ChannelEvent::Joined { callsign, active } => {
            format!("{callsign} bağlandı ({active} aktif)")
        }
        ChannelEvent::Left { callsign, active } => format!("{callsign} ayrıldı ({active} aktif)"),
        ChannelEvent::TxGranted {
            src,
            n_samples,
            duration,
        } => format!("{src} yayında | {n_samples} örnek | {duration:.2}sn"),
        ChannelEvent::TxDenied { src, retry_after } => {
            format!("{src} MEŞGUL | retry {retry_after:.2}sn")
        }
        ChannelEvent::Delivered { .. } | ChannelEvent::Decoded { .. } => String::new(),
    }
}

/// Bir burst'ü 20 ms'lik parçalarla gerçek zamanda halka tamponlarına akıtır.
async fn paced_feed(
    samples: Vec<i16>,
    gui_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_ring: Arc<Mutex<VecDeque<i16>>>,
    audio_enabled: Arc<AtomicBool>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut i = 0;
    while i < samples.len() {
        tick.tick().await;
        let end = (i + MONITOR_HOP).min(samples.len());
        fill_ring(&gui_ring, &samples[i..end], GUI_RING_CAP);
        if audio_enabled.load(Ordering::Relaxed) {
            fill_ring(&audio_ring, &samples[i..end], AUDIO_RING_CAP);
        }
        i = end;
    }
}

// ------------------------------------------------------------------ //
// Modlar
// ------------------------------------------------------------------ //

async fn engine_all_in_one(mut s: Shared, repaint: Box<dyn Fn() + Send>) {
    let core = ChannelCore::spawn(ChannelConfig::default());
    let ch_log = Arc::new(Mutex::new(VecDeque::new()));
    let mon_dec = Arc::new(Mutex::new(VecDeque::new()));
    spawn_channel_log_task(&core, Arc::clone(&ch_log), Arc::clone(&mon_dec));
    spawn_monitor_tap_task(
        &core,
        Arc::clone(&s.gui_ring),
        Arc::clone(&s.audio_ring),
        Arc::clone(&s.audio_enabled),
    );

    let mut slots: BTreeMap<String, StationSlot<InProcConnector>> = BTreeMap::new();
    let net_chat = Arc::new(Mutex::new(VecDeque::new()));
    let mut auto_chat: Option<JoinHandle<()>> = None;
    let core_for_make = Arc::clone(&core);
    let make = move |c: &str| InProcConnector::new(Arc::clone(&core_for_make), c);

    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let stations = station_views(&slots);
                {
                    let mut snap = s.snapshot.lock().unwrap();
                    snap.channel = Some(core.snapshot());
                    snap.channel_log = ch_log.lock().unwrap().iter().cloned().collect();
                    snap.monitor_decodes = mon_dec.lock().unwrap().iter().cloned().collect();
                    snap.stations = stations;
                    snap.net_chat = net_chat.lock().unwrap().iter().cloned().collect();
                    snap.auto_chat_on = auto_chat.is_some();
                }
                repaint();
            }
            Some(cmd) = s.cmd_rx.recv() => {
                match cmd {
                    EngineCmd::SetChannel(cfg) => core.set_config(cfg),
                    other => handle_station_cmd(other, &mut slots, &make, &net_chat, &mut auto_chat, &s.cmd_tx).await,
                }
            }
            else => break,
        }
    }
}

async fn engine_channel_host(mut s: Shared, repaint: Box<dyn Fn() + Send>, port: u16) {
    let core = ChannelCore::spawn(ChannelConfig::default());
    let ch_log = Arc::new(Mutex::new(VecDeque::new()));
    let mon_dec = Arc::new(Mutex::new(VecDeque::new()));
    spawn_channel_log_task(&core, Arc::clone(&ch_log), Arc::clone(&mon_dec));
    spawn_monitor_tap_task(
        &core,
        Arc::clone(&s.gui_ring),
        Arc::clone(&s.audio_ring),
        Arc::clone(&s.audio_enabled),
    );

    let addr = format!("127.0.0.1:{port}");
    match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => {
            push_cap(&ch_log, format!("dinleniyor: {addr}"), 200);
            tokio::spawn(channel::tcp_server::serve(listener, Arc::clone(&core)));
        }
        Err(e) => push_cap(&ch_log, format!("PORT AÇILAMADI {addr}: {e}"), 200),
    }
    s.snapshot.lock().unwrap().link_addr = Some(addr);

    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                {
                    let mut snap = s.snapshot.lock().unwrap();
                    snap.channel = Some(core.snapshot());
                    snap.channel_log = ch_log.lock().unwrap().iter().cloned().collect();
                    snap.monitor_decodes = mon_dec.lock().unwrap().iter().cloned().collect();
                }
                repaint();
            }
            Some(cmd) = s.cmd_rx.recv() => {
                if let EngineCmd::SetChannel(cfg) = cmd {
                    core.set_config(cfg);
                }
            }
            else => break,
        }
    }
}

async fn engine_client(mut s: Shared, repaint: Box<dyn Fn() + Send>, addr: String) {
    s.snapshot.lock().unwrap().link_addr = Some(addr.clone());
    let mut slots: BTreeMap<String, StationSlot<TcpConnector>> = BTreeMap::new();
    let net_chat = Arc::new(Mutex::new(VecDeque::new()));
    let mut auto_chat: Option<JoinHandle<()>> = None;
    let addr_for_make = addr.clone();
    let make = move |c: &str| TcpConnector::new(addr_for_make.clone(), c);

    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let stations = station_views(&slots);
                {
                    let mut snap = s.snapshot.lock().unwrap();
                    snap.stations = stations;
                    snap.net_chat = net_chat.lock().unwrap().iter().cloned().collect();
                    snap.auto_chat_on = auto_chat.is_some();
                }
                repaint();
            }
            Some(cmd) = s.cmd_rx.recv() => {
                handle_station_cmd(cmd, &mut slots, &make, &net_chat, &mut auto_chat, &s.cmd_tx).await;
            }
            else => break,
        }
    }
}

async fn engine_monitor(s: Shared, repaint: Box<dyn Fn() + Send>, addr: String) {
    s.snapshot.lock().unwrap().link_addr = Some(addr.clone());
    let mon_dec = Arc::new(Mutex::new(VecDeque::<String>::new()));

    {
        let (md, gr, ar, ae) = (
            Arc::clone(&mon_dec),
            Arc::clone(&s.gui_ring),
            Arc::clone(&s.audio_ring),
            Arc::clone(&s.audio_enabled),
        );
        let call = format!("MON-{:04X}", rand::random::<u16>());
        let modem = modem::Modem::new();
        tokio::spawn(async move {
            loop {
                match TcpConnector::connect_once(&addr, &call).await {
                    Ok((_tx, mut rx)) => {
                        push_cap(&md, format!("bağlandı: {addr}"), 120);
                        while let Some(msg) = rx.recv().await {
                            let ServerMsg::RxAudio { audio_b64 } = msg else {
                                continue;
                            };
                            let Ok(samples) = b64_to_samples(&audio_b64) else {
                                continue;
                            };
                            let dur = samples.len() as f32 / 8000.0;
                            let summary = modem.demodulate(&samples).and_then(|p| {
                                serde_json::from_slice::<serde_json::Value>(&p)
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
                            match summary {
                                Some(t) => {
                                    push_cap(&md, format!("{t} | {dur:.2}sn | çözüldü"), 120)
                                }
                                None => push_cap(&md, format!("{dur:.2}sn | çözülemedi"), 120),
                            }
                            tokio::spawn(paced_feed(
                                samples,
                                Arc::clone(&gr),
                                Arc::clone(&ar),
                                Arc::clone(&ae),
                            ));
                        }
                        push_cap(&md, "bağlantı koptu, yeniden deneniyor…".into(), 120);
                    }
                    Err(e) => push_cap(&md, format!("bağlanılamadı ({e}), yeniden…"), 120),
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        {
            let mut snap = s.snapshot.lock().unwrap();
            snap.monitor_decodes = mon_dec.lock().unwrap().iter().cloned().collect();
        }
        repaint();
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
        let h = EngineHandle::spawn_all_in_one(egui::Context::default());

        for c in ["TA1ABC", "TA2DEF"] {
            h.send(EngineCmd::AddStation {
                callsign: c.into(),
                mode: netproto::Mode::Qpsk,
            });
        }
        assert!(wait_until(&h, 10, |s| s.stations.len() == 2));

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
        assert!(wait_until(&h, 5, |s| !s.monitor_decodes.is_empty()));
        assert!(wait_until(&h, 3, |s| s
            .net_chat
            .iter()
            .any(|c| c.from == "TA1ABC" && c.text == "motor testi")));
    }

    #[test]
    fn chat_all_and_auto_chat() {
        let h = EngineHandle::spawn_all_in_one(egui::Context::default());
        for c in ["TA1ABC", "TA2DEF", "TA3GHI"] {
            h.send(EngineCmd::AddStation {
                callsign: c.into(),
                mode: netproto::Mode::Qpsk,
            });
        }
        assert!(wait_until(&h, 10, |s| s.stations.len() == 3));

        h.send(EngineCmd::ChatAll {
            dst: "ALL".into(),
            text: "toplu selam".into(),
        });
        assert!(wait_until(&h, 5, |s| {
            let senders: std::collections::BTreeSet<_> = s
                .net_chat
                .iter()
                .filter(|c| c.text == "toplu selam")
                .map(|c| c.from.as_str())
                .collect();
            senders.len() == 3
        }));

        h.send(EngineCmd::SetAutoChat {
            enabled: true,
            interval_ms: 800,
        });
        assert!(wait_until(&h, 3, |s| s.auto_chat_on));
        let n0 = h.snapshot.lock().unwrap().net_chat.len();
        assert!(wait_until(&h, 6, |s| s.net_chat.len() > n0));
        h.send(EngineCmd::SetAutoChat {
            enabled: false,
            interval_ms: 800,
        });
        assert!(wait_until(&h, 3, |s| !s.auto_chat_on));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn channel_host_and_two_tcp_clients() {
        // Kanalı barındıran motor + iki ayrı client motoru (TCP) -> sohbet.
        let port = 6400 + (std::process::id() % 200) as u16;
        let ch = EngineHandle::spawn_channel_host(egui::Context::default(), port);
        std::thread::sleep(Duration::from_millis(600));

        let addr = format!("127.0.0.1:{port}");
        let a = EngineHandle::spawn_client(egui::Context::default(), addr.clone());
        let b = EngineHandle::spawn_client(egui::Context::default(), addr.clone());
        a.send(EngineCmd::AddStation {
            callsign: "TA1ABC".into(),
            mode: netproto::Mode::Qpsk,
        });
        b.send(EngineCmd::AddStation {
            callsign: "TA2DEF".into(),
            mode: netproto::Mode::Qpsk,
        });
        assert!(wait_until(&a, 10, |s| s.stations.len() == 1));
        assert!(wait_until(&b, 10, |s| s.stations.len() == 1));

        a.send(EngineCmd::Chat {
            callsign: "TA1ABC".into(),
            dst: "ALL".into(),
            text: "tcp merhaba".into(),
        });
        assert!(
            wait_until(&b, 20, |s| s
                .stations
                .first()
                .map(|v| v.chat.iter().any(|c| c.text == "tcp merhaba" && !c.own))
                .unwrap_or(false)),
            "B (ayrı process benzeri) TCP kanaldan sohbeti almalıydı"
        );
        // NET akışında da görünmeli (kendi mesajı olmasa bile).
        assert!(
            wait_until(&b, 5, |s| s
                .net_chat
                .iter()
                .any(|c| c.from == "TA1ABC" && c.text == "tcp merhaba" && !c.own)),
            "uzak istasyonun mesajı B'nin NET akışında görünmeliydi"
        );
        // Kanal motoru trafiği görmüş olmalı.
        assert!(wait_until(&ch, 5, |s| !s.monitor_decodes.is_empty()
            || !s.channel_log.is_empty()));
    }
}
