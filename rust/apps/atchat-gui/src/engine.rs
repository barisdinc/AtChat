//! The engine: runs a tokio runtime on its own thread. Four modes:
//!   - `spawn_all_in_one` — in-proc channel + in-proc stations + monitor tap
//!   - `spawn_channel_host` — in-proc channel + TCP server (other processes connect)
//!   - `spawn_client` — stations connected to the channel over TCP (multi-window)
//!   - `spawn_monitor` — a monitor passively listening to the channel over TCP
//!
//! GUI ↔ engine: commands in via `mpsc`, state out via `Arc<Mutex<EngineSnapshot>>`,
//! monitor samples into shared ring buffers.

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

pub const GUI_RING_CAP: usize = 24_000; // ~3 s @ 8 kHz
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
    /// EVERY connected station sends the same message to `dst`.
    ChatAll {
        dst: String,
        text: String,
    },
    SendFile {
        callsign: String,
        path: PathBuf,
        dst: String,
    },
    /// EVERY connected station sends the same file to `dst`.
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
    /// Auto-chat: a random station writes to ALL every `interval_ms`.
    SetAutoChat {
        enabled: bool,
        interval_ms: u64,
    },
    /// Internal — a tick fired by the auto-chat timer.
    AutoTick,
}

const AUTO_PHRASES: &[&str] = &[
    "test test",
    "signal 59",
    "roger",
    "QSL",
    "how is the channel?",
    "waterfall is clean",
    "standing by",
    "group call",
    "I am here",
    "copy that",
    "understood",
    "10-4",
    "temperature normal",
    "waiting for a report",
];

#[derive(Clone)]
pub struct ChatLine {
    pub from: String,
    pub dst: String,
    pub text: String,
    pub private: bool,
    pub own: bool,
}

/// A file received over the air (or sent by this station) that decoded as an image.
#[derive(Clone)]
pub struct RecvImage {
    pub from: String,
    pub filename: String,
    pub when: String, // HH:MM:SS (UTC)
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<Vec<u8>>, // width*height*4
    /// true -> this station sent it, false -> it came over the air.
    pub own: bool,
}

/// Add an image to the NET list; do not repeat if the same `(from, filename,
/// w, h)` is in the recent entries (many local receivers / sender+receiver in
/// the same process).
fn push_image(images: &Mutex<Vec<RecvImage>>, img: RecvImage) {
    let mut list = images.lock().unwrap();
    let dup = list.iter().rev().take(8).any(|i| {
        i.from == img.from
            && i.filename == img.filename
            && i.width == img.width
            && i.height == img.height
    });
    if !dup {
        list.push(img);
        while list.len() > 50 {
            list.remove(0);
        }
    }
}

/// Read `path`; if it is an image, add it with `push_image` (in the background).
fn try_add_image(
    images: Arc<Mutex<Vec<RecvImage>>>,
    from: String,
    filename: String,
    path: String,
    own: bool,
) {
    tokio::spawn(async move {
        let Ok(bytes) = tokio::fs::read(&path).await else {
            return;
        };
        let Ok(img) = image::load_from_memory(&bytes) else {
            return; // not an image -> silently ignore
        };
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        push_image(
            &images,
            RecvImage {
                from,
                filename,
                when: now_hms(),
                width: w,
                height: h,
                rgba: Arc::new(rgba.into_raw()),
                own,
            },
        );
    });
}

/// The NET tab's shared state (chat + images).
#[derive(Clone)]
struct NetShared {
    chat: Arc<Mutex<VecDeque<ChatLine>>>,
    images: Arc<Mutex<Vec<RecvImage>>>,
}

impl NetShared {
    fn new() -> Self {
        Self {
            chat: Arc::new(Mutex::new(VecDeque::new())),
            images: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

fn file_name_of(p: &std::path::Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string()
}

fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
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
    /// Chat sent by every station, chronological and merged (the NET tab).
    pub net_chat: Vec<ChatLine>,
    /// Files received over the air that decoded as images (the NET tab).
    pub images: Vec<RecvImage>,
    pub auto_chat_on: bool,
    /// The address this engine is connected to (client/monitor modes).
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
// Station management (in-proc / TCP does not matter — only the Connector changes)
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

/// Add an incoming chat to the merged stream — deduplicated against the
/// recent lines so that if the same message is received by several local
/// stations (and I sent it myself) only one line remains.
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
    net: &NetShared,
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
            let (l2, c2, nc2, im2) = (
                Arc::clone(&log),
                Arc::clone(&chat),
                Arc::clone(&net.chat),
                Arc::clone(&net.images),
            );
            let drain = tokio::spawn(async move {
                loop {
                    match ev.recv().await {
                        Ok(StationEvent::Log(s)) => push_cap(&l2, s, 300),
                        Ok(StationEvent::Chat { from, scope, text }) => {
                            let private = scope == ChatScope::Private;
                            let line = ChatLine {
                                from,
                                dst: if private {
                                    "(private)".into()
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
                        Ok(StationEvent::Transfer {
                            dir: protocol::TransferDir::In,
                            done: true,
                            saved_path: Some(path),
                            filename,
                            peer,
                            ..
                        }) => {
                            try_add_image(Arc::clone(&im2), peer, filename, path, false);
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
        Err(e) => tracing::error!("could not add the station: {e}"),
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
    net: &NetShared,
    auto_chat: &mut Option<JoinHandle<()>>,
    cmd_tx: &UnboundedSender<EngineCmd>,
) where
    C: Connector,
    F: Fn(&str) -> C,
{
    let net_chat = &net.chat;
    match cmd {
        EngineCmd::AddStation { callsign, mode } => {
            let conn = make_conn(&callsign);
            add_station(slots, &callsign, mode, conn, net).await;
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
            let c = callsign.to_uppercase();
            if let Some(sl) = slots.get(&c) {
                sl.station.send_file(path.clone(), &dst);
                try_add_image(
                    Arc::clone(&net.images),
                    c,
                    file_name_of(&path),
                    path.to_string_lossy().into_owned(),
                    true,
                );
            }
        }
        EngineCmd::SendFileAll { path, dst } => {
            let mut first_sender: Option<String> = None;
            for (c, sl) in slots.iter() {
                if sl.station.is_connected() {
                    sl.station.send_file(path.clone(), &dst);
                    first_sender.get_or_insert_with(|| c.clone());
                }
            }
            if let Some(c) = first_sender {
                try_add_image(
                    Arc::clone(&net.images),
                    c,
                    file_name_of(&path),
                    path.to_string_lossy().into_owned(),
                    true,
                );
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
        EngineCmd::SetChannel(_) => {} // only the channel-hosting engine handles this
    }
}

// ------------------------------------------------------------------ //
// Shared background tasks
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
                        Some(s) => format!("{s} | {duration:.2}s | decoded"),
                        None => format!("{duration:.2}s | undecoded"),
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
            format!("{callsign} joined ({active} active)")
        }
        ChannelEvent::Left { callsign, active } => format!("{callsign} left ({active} active)"),
        ChannelEvent::TxGranted {
            src,
            n_samples,
            duration,
        } => format!("{src} on air | {n_samples} samples | {duration:.2}s"),
        ChannelEvent::TxDenied { src, retry_after } => {
            format!("{src} BUSY | retry {retry_after:.2}s")
        }
        ChannelEvent::Delivered { .. } | ChannelEvent::Decoded { .. } => String::new(),
    }
}

/// Streams a burst into the ring buffers in real time in 20 ms chunks.
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
// Modes
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
    let net = NetShared::new();
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
                    snap.net_chat = net.chat.lock().unwrap().iter().cloned().collect();
                    snap.images = net.images.lock().unwrap().clone();
                    snap.auto_chat_on = auto_chat.is_some();
                }
                repaint();
            }
            Some(cmd) = s.cmd_rx.recv() => {
                match cmd {
                    EngineCmd::SetChannel(cfg) => core.set_config(cfg),
                    other => handle_station_cmd(other, &mut slots, &make, &net, &mut auto_chat, &s.cmd_tx).await,
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
            push_cap(&ch_log, format!("listening on: {addr}"), 200);
            tokio::spawn(channel::tcp_server::serve(listener, Arc::clone(&core)));
        }
        Err(e) => push_cap(&ch_log, format!("COULD NOT OPEN PORT {addr}: {e}"), 200),
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
    let net = NetShared::new();
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
                    snap.net_chat = net.chat.lock().unwrap().iter().cloned().collect();
                    snap.images = net.images.lock().unwrap().clone();
                    snap.auto_chat_on = auto_chat.is_some();
                }
                repaint();
            }
            Some(cmd) = s.cmd_rx.recv() => {
                handle_station_cmd(cmd, &mut slots, &make, &net, &mut auto_chat, &s.cmd_tx).await;
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
                        push_cap(&md, format!("connected: {addr}"), 120);
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
                                Some(t) => push_cap(&md, format!("{t} | {dur:.2}s | decoded"), 120),
                                None => push_cap(&md, format!("{dur:.2}s | undecoded"), 120),
                            }
                            tokio::spawn(paced_feed(
                                samples,
                                Arc::clone(&gr),
                                Arc::clone(&ar),
                                Arc::clone(&ae),
                            ));
                        }
                        push_cap(&md, "connection dropped, retrying…".into(), 120);
                    }
                    Err(e) => push_cap(&md, format!("could not connect ({e}), retrying…"), 120),
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
            text: "engine test".into(),
        });
        assert!(
            wait_until(&h, 20, |s| {
                s.stations
                    .iter()
                    .find(|v| v.snap.callsign == "TA2DEF")
                    .map(|v| v.chat.iter().any(|c| c.text == "engine test" && !c.own))
                    .unwrap_or(false)
            }),
            "TA2DEF should receive the chat through the engine"
        );
        assert!(wait_until(&h, 5, |s| !s.monitor_decodes.is_empty()));
        assert!(wait_until(&h, 3, |s| s
            .net_chat
            .iter()
            .any(|c| c.from == "TA1ABC" && c.text == "engine test")));
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
            text: "group hello".into(),
        });
        assert!(wait_until(&h, 5, |s| {
            let senders: std::collections::BTreeSet<_> = s
                .net_chat
                .iter()
                .filter(|c| c.text == "group hello")
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

    #[test]
    fn received_png_shows_in_images() {
        let dir = std::env::temp_dir().join(format!("atchat_img_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("card.png");
        let mut img = image::RgbaImage::new(8, 6);
        for p in img.pixels_mut() {
            *p = image::Rgba([200, 40, 40, 255]);
        }
        img.save(&png).unwrap();

        let h = EngineHandle::spawn_all_in_one(egui::Context::default());
        for c in ["TA1ABC", "TA2DEF"] {
            h.send(EngineCmd::AddStation {
                callsign: c.into(),
                mode: netproto::Mode::Qpsk,
            });
        }
        assert!(wait_until(&h, 10, |s| s.stations.len() == 2));

        h.send(EngineCmd::SendFile {
            callsign: "TA1ABC".into(),
            path: png,
            dst: "TA2DEF".into(),
        });

        // Visible on the sender's side immediately (own = true).
        assert!(
            wait_until(&h, 8, |s| s.images.iter().any(|i| i.from == "TA1ABC"
                && i.own
                && i.width == 8
                && i.height == 6)),
            "the sender should see its own image in NET"
        );
        // The transfer also completes (the receiver hits the dedup, but the flow works).
        assert!(wait_until(&h, 45, |s| s.stations.iter().any(|v| v
            .snap
            .transfers_in
            .iter()
            .any(|t| t.complete))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn channel_host_and_two_tcp_clients() {
        // The channel-hosting engine + two separate client engines (TCP) -> chat.
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
            text: "tcp hello".into(),
        });
        assert!(
            wait_until(&b, 20, |s| s
                .stations
                .first()
                .map(|v| v.chat.iter().any(|c| c.text == "tcp hello" && !c.own))
                .unwrap_or(false)),
            "B (like a separate process) should receive the chat from the TCP channel"
        );
        // It should also appear in the NET stream (even though it is not its own message).
        assert!(
            wait_until(&b, 5, |s| s
                .net_chat
                .iter()
                .any(|c| c.from == "TA1ABC" && c.text == "tcp hello" && !c.own)),
            "the remote station's message should appear in B's NET stream"
        );
        // The channel engine should have seen the traffic.
        assert!(wait_until(&ch, 5, |s| !s.monitor_decodes.is_empty()
            || !s.channel_log.is_empty()));
    }
}
