//! The eframe app: Channel | Stations | Monitor tabs.

use std::sync::{Arc, Mutex};

use dsp_viz::{db_to_unit, Colormap, ScopeBuf, SpectrumAnalyzer, Waterfall};
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use crate::audio::AudioOut;
use crate::engine::{EngineCmd, EngineHandle, EngineSnapshot};

const WF_W: usize = 480;
const WF_H: usize = 256;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Tab {
    Channel,
    Stations,
    Net,
    Monitor,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Channel => "Channel",
            Tab::Stations => "Stations",
            Tab::Net => "NET",
            Tab::Monitor => "Monitor",
        }
    }
}

/// Decides which binary shows which tabs.
pub struct AppConfig {
    pub title: String,
    pub tabs: Vec<Tab>,
}

pub struct AtchatApp {
    engine: EngineHandle,
    title: String,
    tabs: Vec<Tab>,
    tab: Tab,

    // --- DSP (GUI thread) ---
    scope: ScopeBuf,
    spectrum: SpectrumAnalyzer,
    waterfall: Waterfall,
    wf_tex: Option<egui::TextureHandle>,
    fft_size: usize,
    avg_alpha: f32,
    cmap: Colormap,
    wf_floor: f32,
    wf_ceil: f32,
    scope_ms: f32,
    /// true -> the spectrum + waterfall show only 0–2.76 kHz (the data band).
    freq_zoom: bool,

    // --- audio ---
    audio: Option<AudioOut>,
    audio_on: bool,
    audio_failed: bool,
    audio_vol: Arc<Mutex<f32>>,

    // --- station tab ui ---
    sel_station: Option<String>,
    new_call: String,
    new_mode: netproto::Mode,
    chat_input: String,
    chat_dst: String,

    // --- NET tab ui ---
    net_from: String,
    net_dst: String,
    net_msg: String,
    net_file_dst: String,
    auto_secs: f32,
    img_idx: usize,
    img_tex: Option<egui::TextureHandle>,
    img_key: Option<(usize, usize, usize)>,
    prev_img_count: usize,

    // --- channel tab ui ---
    snr_on: bool,
    snr_db: f32,
    mp_delay: f32,
    mp_gain: f32,
}

impl AtchatApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, engine: EngineHandle, cfg: AppConfig) -> Self {
        let fft_size = 1024;
        let mut spectrum = SpectrumAnalyzer::new(fft_size);
        spectrum.set_averaging(0.5);
        let tab = cfg.tabs.first().copied().unwrap_or(Tab::Stations);
        Self {
            engine,
            title: cfg.title,
            tabs: cfg.tabs,
            tab,
            scope: ScopeBuf::new(16_000),
            spectrum,
            waterfall: Waterfall::new(WF_W, WF_H),
            wf_tex: None,
            fft_size,
            avg_alpha: 0.5,
            cmap: Colormap::Turbo,
            wf_floor: -95.0,
            wf_ceil: -20.0,
            scope_ms: 250.0,
            freq_zoom: false,
            audio: None,
            audio_on: false,
            audio_failed: false,
            audio_vol: Arc::new(Mutex::new(0.6)),
            sel_station: None,
            new_call: "TA1ABC".into(),
            new_mode: netproto::Mode::Qpsk,
            chat_input: String::new(),
            chat_dst: "ALL".into(),
            net_from: String::new(),
            net_dst: "ALL".into(),
            net_msg: String::new(),
            net_file_dst: "ALL".into(),
            auto_secs: 4.0,
            img_idx: 0,
            img_tex: None,
            img_key: None,
            prev_img_count: 0,
            snr_on: false,
            snr_db: 15.0,
            mp_delay: 0.0,
            mp_gain: 0.0,
        }
    }

    fn apply_channel(&self) {
        self.engine
            .send(EngineCmd::SetChannel(channel::ChannelConfig {
                snr_db: self.snr_on.then_some(self.snr_db as f64),
                multipath_delay_ms: self.mp_delay as f64,
                multipath_gain: self.mp_gain as f64,
                ..Default::default()
            }));
    }

    /// The top displayed frequency (Hz). The full band is 4 kHz (Nyquist @ 8 kHz).
    fn view_hz(&self) -> f32 {
        if self.freq_zoom {
            2760.0
        } else {
            4000.0
        }
    }

    /// The bin count corresponding to `view_hz` (1..n_bins).
    fn view_bins(&self) -> usize {
        let n = self.spectrum.n_bins().max(1);
        ((self.view_hz() / 4000.0) * n as f32)
            .round()
            .clamp(1.0, n as f32) as usize
    }

    fn drain_monitor_samples(&mut self) {
        let chunk: Vec<i16> = {
            let mut r = self.engine.gui_ring.lock().unwrap();
            r.drain(..).collect()
        };
        if chunk.is_empty() {
            return;
        }
        self.scope.push_i16(&chunk);
        self.spectrum.feed_i16(&chunk);
        let db = self.spectrum.magnitudes_db();
        let vb = self.view_bins().min(db.len());
        self.waterfall
            .push_row_db(&db[..vb], self.wf_floor, self.wf_ceil);
    }
}

impl eframe::App for AtchatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_monitor_samples();
        let snap = self.engine.snapshot.lock().unwrap().clone();

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(&self.title);
                ui.separator();
                for t in self.tabs.clone() {
                    ui.selectable_value(&mut self.tab, t, t.label());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(ch) = &snap.channel {
                        let (txt, col) = match &ch.current_tx {
                            Some(s) => (
                                format!("BUSY — {s} ({:.1}s)", ch.busy_remaining),
                                Color32::from_rgb(230, 150, 60),
                            ),
                            None => ("channel idle".to_string(), Color32::from_rgb(120, 190, 120)),
                        };
                        ui.colored_label(col, txt);
                        ui.label(format!("{} stations", ch.active_clients));
                    } else if let Some(addr) = &snap.link_addr {
                        ui.weak(format!("↔ {addr}"));
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Channel => self.ui_channel(ui, &snap),
            Tab::Stations => self.ui_stations(ui, &snap),
            Tab::Net => self.ui_net(ui, &snap),
            Tab::Monitor => self.ui_monitor(ui, ctx, &snap),
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

// ------------------------------------------------------------------ //
// Channel tab
// ------------------------------------------------------------------ //
impl AtchatApp {
    fn ui_channel(&mut self, ui: &mut egui::Ui, snap: &EngineSnapshot) {
        ui.heading("Channel physics");
        ui.add_space(4.0);

        let mut changed = false;
        egui::Grid::new("chan_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("AWGN noise");
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut self.snr_on, "on").changed();
                    ui.add_enabled(
                        self.snr_on,
                        egui::Slider::new(&mut self.snr_db, 6.0..=30.0).suffix(" dB"),
                    )
                    .changed()
                    .then(|| changed = true);
                });
                ui.end_row();

                ui.label("Multipath delay");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.mp_delay, 0.0..=20.0)
                            .suffix(" ms  (guard interval 8 ms)"),
                    )
                    .changed();
                ui.end_row();

                ui.label("Multipath gain");
                changed |= ui
                    .add(egui::Slider::new(&mut self.mp_gain, 0.0..=0.6))
                    .changed();
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Preset:");
            if ui.button("Clean").clicked() {
                self.snr_on = false;
                self.mp_delay = 0.0;
                self.mp_gain = 0.0;
                changed = true;
            }
            if ui.button("13 dB — ARQ").clicked() {
                self.snr_on = true;
                self.snr_db = 13.0;
                self.mp_delay = 0.0;
                self.mp_gain = 0.0;
                changed = true;
            }
            if ui.button("Multipath limit (15 ms)").clicked() {
                self.snr_on = false;
                self.mp_delay = 15.0;
                self.mp_gain = 0.25;
                changed = true;
            }
        });
        if changed {
            self.apply_channel();
        }

        ui.separator();
        if let Some(ch) = &snap.channel {
            ui.label(format!(
                "State: {}   |   active stations: {}",
                if ch.busy {
                    format!(
                        "BUSY ({}, {:.2}s left)",
                        ch.current_tx.clone().unwrap_or_default(),
                        ch.busy_remaining
                    )
                } else {
                    "idle".into()
                },
                ch.active_clients
            ));
        }

        ui.add_space(4.0);
        ui.label("On-air signal (scope):");
        self.draw_scope(ui);

        ui.add_space(4.0);
        ui.label("Event log:");
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_height(220.0)
            .show(ui, |ui| {
                for line in &snap.channel_log {
                    ui.monospace(line);
                }
            });
    }
}

// ------------------------------------------------------------------ //
// Stations tab
// ------------------------------------------------------------------ //
impl AtchatApp {
    fn ui_stations(&mut self, ui: &mut egui::Ui, snap: &EngineSnapshot) {
        egui::SidePanel::left("stations_list")
            .resizable(false)
            .default_width(180.0)
            .show_inside(ui, |ui| {
                ui.heading("Stations");
                ui.separator();
                let mut remove: Option<String> = None;
                for sv in &snap.stations {
                    ui.horizontal(|ui| {
                        let sel = self.sel_station.as_deref() == Some(sv.snap.callsign.as_str());
                        let label = format!("{}  [{}]", sv.snap.callsign, sv.snap.role.as_str());
                        if ui.selectable_label(sel, label).clicked() {
                            self.sel_station = Some(sv.snap.callsign.clone());
                        }
                        if ui.small_button("✕").on_hover_text("remove").clicked() {
                            remove = Some(sv.snap.callsign.clone());
                        }
                    });
                }
                if let Some(c) = remove {
                    self.engine.send(EngineCmd::RemoveStation {
                        callsign: c.clone(),
                    });
                    if self.sel_station.as_deref() == Some(c.as_str()) {
                        self.sel_station = None;
                    }
                }
                ui.separator();
                ui.label("New station:");
                ui.text_edit_singleline(&mut self.new_call);
                egui::ComboBox::from_id_salt("newmode")
                    .selected_text(self.new_mode.as_str())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.new_mode, netproto::Mode::Qpsk, "QPSK");
                        ui.selectable_value(&mut self.new_mode, netproto::Mode::Bpsk, "BPSK");
                    });
                if ui.button("＋ Add").clicked() && !self.new_call.trim().is_empty() {
                    self.engine.send(EngineCmd::AddStation {
                        callsign: self.new_call.trim().to_uppercase(),
                        mode: self.new_mode,
                    });
                    self.new_call.clear();
                }
            });

        let Some(call) = self.sel_station.clone() else {
            ui.centered_and_justified(|ui| ui.label("Select a station on the left, or add one."));
            return;
        };
        let Some(sv) = snap.stations.iter().find(|s| s.snap.callsign == call) else {
            self.sel_station = None;
            return;
        };
        let s = &sv.snap;

        ui.horizontal(|ui| {
            ui.heading(&s.callsign);
            let (col, txt) = match s.role {
                protocol::Role::Master => (Color32::from_rgb(90, 190, 100), "MASTER"),
                protocol::Role::Backup => (Color32::from_rgb(230, 170, 60), "BACKUP"),
                protocol::Role::Listener => (Color32::GRAY, "LISTENER"),
            };
            ui.colored_label(col, egui::RichText::new(txt).strong());
            ui.label(format!(
                "master={}  backup={}  {}",
                s.master.clone().unwrap_or_else(|| "-".into()),
                s.backup.clone().unwrap_or_else(|| "-".into()),
                if s.connected { "connected" } else { "DOWN" },
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reconnect").clicked() {
                    self.engine.send(EngineCmd::Reconnect {
                        callsign: call.clone(),
                    });
                }
                if ui.button("Drop").clicked() {
                    self.engine.send(EngineCmd::Drop {
                        callsign: call.clone(),
                    });
                }
            });
        });

        ui.separator();
        ui.columns(2, |cols| {
            // left: roster + transfers
            cols[0].label(egui::RichText::new("Roster").strong());
            egui::Grid::new("roster")
                .striped(true)
                .show(&mut cols[0], |ui| {
                    for (c, st, age) in &s.roster {
                        ui.label(c);
                        let (col, t) = match st {
                            protocol::RosterStatus::Active => {
                                (Color32::from_rgb(120, 190, 120), "active")
                            }
                            protocol::RosterStatus::Lost => {
                                (Color32::from_rgb(220, 120, 120), "lost")
                            }
                        };
                        ui.colored_label(col, t);
                        ui.label(format!("{age:.0}s ago"));
                        ui.end_row();
                    }
                });
            cols[0].add_space(8.0);
            cols[0].label(egui::RichText::new("Transfers").strong());
            for t in s.transfers_out.iter() {
                let frac = if t.total > 0 {
                    t.have as f32 / t.total as f32
                } else {
                    0.0
                };
                cols[0].add(egui::ProgressBar::new(frac).text(format!(
                    "↑ {} -> {} {}/{}{}",
                    t.filename,
                    t.peer,
                    t.have,
                    t.total,
                    if t.arq_round > 0 {
                        format!(" (ARQ {})", t.arq_round)
                    } else {
                        String::new()
                    }
                )));
            }
            for t in s.transfers_in.iter() {
                let frac = if t.total > 0 {
                    t.have as f32 / t.total as f32
                } else {
                    0.0
                };
                cols[0].add(egui::ProgressBar::new(frac).text(format!(
                    "↓ {} <- {} {}/{}{}",
                    t.filename,
                    t.peer,
                    t.have,
                    t.total,
                    if t.complete { "  ✓" } else { "" }
                )));
            }
            if s.transfers_in.is_empty() && s.transfers_out.is_empty() {
                cols[0].weak("(no transfers)");
            }
            if cols[0].button("Send file/image…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    self.engine.send(EngineCmd::SendFile {
                        callsign: call.clone(),
                        path,
                        dst: self.chat_dst.clone(),
                    });
                }
            }

            // right: chat + log
            cols[1].label(egui::RichText::new("Chat").strong());
            egui::ScrollArea::vertical()
                .id_salt("chat")
                .stick_to_bottom(true)
                .max_height(220.0)
                .show(&mut cols[1], |ui| {
                    for line in &sv.chat {
                        let tag = if line.private { "private" } else { "all" };
                        let col = if line.own {
                            Color32::from_rgb(140, 180, 240)
                        } else {
                            Color32::from_rgb(210, 210, 210)
                        };
                        ui.colored_label(col, format!("[{tag}] {}: {}", line.from, line.text));
                    }
                });
            cols[1].horizontal(|ui| {
                egui::ComboBox::from_id_salt("chatdst")
                    .selected_text(&self.chat_dst)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.chat_dst, "ALL".to_string(), "ALL");
                        for (c, _, _) in &s.roster {
                            ui.selectable_value(&mut self.chat_dst, c.clone(), c);
                        }
                    });
                // Reserve the Send button on the right FIRST so it can never be
                // pushed off the row by the text field; the field then fills the
                // rest of the space to its left.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut send = ui.button("Send").clicked();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.chat_input)
                            .hint_text("message")
                            .desired_width(f32::INFINITY),
                    );
                    send |= resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if send && !self.chat_input.trim().is_empty() {
                        self.engine.send(EngineCmd::Chat {
                            callsign: call.clone(),
                            dst: self.chat_dst.clone(),
                            text: self.chat_input.trim().to_string(),
                        });
                        self.chat_input.clear();
                        resp.request_focus();
                    }
                });
            });
            cols[1].add_space(6.0);
            cols[1].label(egui::RichText::new("Log").strong());
            egui::ScrollArea::vertical()
                .id_salt("stlog")
                .stick_to_bottom(true)
                .max_height(160.0)
                .show(&mut cols[1], |ui| {
                    for l in &sv.log {
                        ui.monospace(l);
                    }
                });
        });
    }
}

// ------------------------------------------------------------------ //
// NET tab — manage every station from one place
// ------------------------------------------------------------------ //
impl AtchatApp {
    fn ui_net(&mut self, ui: &mut egui::Ui, snap: &EngineSnapshot) {
        let calls: Vec<String> = snap
            .stations
            .iter()
            .map(|s| s.snap.callsign.clone())
            .collect();
        if calls.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("Add a few stations from the Stations tab first.")
            });
            return;
        }
        if !calls.contains(&self.net_from) {
            self.net_from = calls[0].clone();
        }
        let dst_opts: Vec<String> = std::iter::once("ALL".to_string())
            .chain(calls.clone())
            .collect();

        ui.heading("NET — bulk control");
        ui.add_space(4.0);

        // --- bulk chat ---
        ui.group(|ui| {
            ui.label(egui::RichText::new("Chat").strong());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("net_from")
                    .selected_text(&self.net_from)
                    .show_ui(ui, |ui| {
                        for c in &calls {
                            ui.selectable_value(&mut self.net_from, c.clone(), c);
                        }
                    });
                ui.label("→");
                egui::ComboBox::from_id_salt("net_dst")
                    .selected_text(&self.net_dst)
                    .show_ui(ui, |ui| {
                        for c in &dst_opts {
                            ui.selectable_value(&mut self.net_dst, c.clone(), c);
                        }
                    });
                // Reserve the buttons on the right FIRST (right-to-left) so they
                // stay visible however narrow the window is; the text field fills
                // whatever space is left to their left.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let all_talk = ui
                        .button("All talk")
                        .on_hover_text("Every connected station sends this message")
                        .clicked();
                    let mut send = ui.button("Send").clicked();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.net_msg)
                            .hint_text("message")
                            .desired_width(f32::INFINITY),
                    );
                    send |= resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let has_text = !self.net_msg.trim().is_empty();
                    if send && has_text {
                        self.engine.send(EngineCmd::Chat {
                            callsign: self.net_from.clone(),
                            dst: self.net_dst.clone(),
                            text: self.net_msg.trim().to_string(),
                        });
                        self.net_msg.clear();
                        resp.request_focus();
                    } else if all_talk && has_text {
                        self.engine.send(EngineCmd::ChatAll {
                            dst: self.net_dst.clone(),
                            text: self.net_msg.trim().to_string(),
                        });
                        self.net_msg.clear();
                    }
                });
            });

            ui.horizontal(|ui| {
                let mut auto = snap.auto_chat_on;
                if ui.checkbox(&mut auto, "Auto-chat").changed() {
                    self.engine.send(EngineCmd::SetAutoChat {
                        enabled: auto,
                        interval_ms: (self.auto_secs * 1000.0) as u64,
                    });
                }
                if ui
                    .add(egui::Slider::new(&mut self.auto_secs, 1.0..=15.0).suffix(" s"))
                    .changed()
                    && snap.auto_chat_on
                {
                    self.engine.send(EngineCmd::SetAutoChat {
                        enabled: true,
                        interval_ms: (self.auto_secs * 1000.0) as u64,
                    });
                }
                ui.weak("(a random station → ALL; to see the waterfall live)");
            });
        });

        // --- bulk file ---
        ui.group(|ui| {
            ui.label(egui::RichText::new("File / image").strong());
            ui.horizontal(|ui| {
                ui.label("target:");
                egui::ComboBox::from_id_salt("net_file_dst")
                    .selected_text(&self.net_file_dst)
                    .show_ui(ui, |ui| {
                        for c in &dst_opts {
                            ui.selectable_value(&mut self.net_file_dst, c.clone(), c);
                        }
                    });
                if ui.button(format!("{} send…", self.net_from)).clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.engine.send(EngineCmd::SendFile {
                            callsign: self.net_from.clone(),
                            path,
                            dst: self.net_file_dst.clone(),
                        });
                    }
                }
                if ui
                    .button("All send…")
                    .on_hover_text("Every connected station sends the selected file")
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.engine.send(EngineCmd::SendFileAll {
                            path,
                            dst: self.net_file_dst.clone(),
                        });
                    }
                }
            });
        });

        self.ui_net_images(ui, snap);

        ui.add_space(4.0);
        ui.columns(2, |cols| {
            cols[0].label(egui::RichText::new("NET chat stream").strong());
            egui::ScrollArea::vertical()
                .id_salt("net_chat")
                .stick_to_bottom(true)
                .max_height(360.0)
                .show(&mut cols[0], |ui| {
                    for line in &snap.net_chat {
                        let arrow = if line.dst == "ALL" {
                            String::new()
                        } else {
                            format!(" → {}", line.dst)
                        };
                        ui.monospace(format!("{}{}: {}", line.from, arrow, line.text));
                    }
                });

            cols[1].label(egui::RichText::new("All active transfers").strong());
            egui::ScrollArea::vertical()
                .id_salt("net_xfers")
                .max_height(360.0)
                .show(&mut cols[1], |ui| {
                    let mut any = false;
                    for sv in &snap.stations {
                        for t in &sv.snap.transfers_out {
                            any = true;
                            let frac = if t.total > 0 {
                                t.have as f32 / t.total as f32
                            } else {
                                0.0
                            };
                            ui.add(egui::ProgressBar::new(frac).text(format!(
                                "{} ↑ {} → {} {}/{}{}",
                                sv.snap.callsign,
                                t.filename,
                                t.peer,
                                t.have,
                                t.total,
                                if t.arq_round > 0 {
                                    format!(" ARQ{}", t.arq_round)
                                } else {
                                    String::new()
                                }
                            )));
                        }
                        for t in &sv.snap.transfers_in {
                            any = true;
                            let frac = if t.total > 0 {
                                t.have as f32 / t.total as f32
                            } else {
                                0.0
                            };
                            ui.add(egui::ProgressBar::new(frac).text(format!(
                                "{} ↓ {} ← {} {}/{}{}",
                                sv.snap.callsign,
                                t.filename,
                                t.peer,
                                t.have,
                                t.total,
                                if t.complete { "  ✓" } else { "" }
                            )));
                        }
                    }
                    if !any {
                        ui.weak("(no active transfers)");
                    }
                });
        });
    }

    /// Images received over the air — an info line + ◀ ▶ to switch between them.
    fn ui_net_images(&mut self, ui: &mut egui::Ui, snap: &EngineSnapshot) {
        let n = snap.images.len();
        ui.add_space(6.0);
        ui.group(|ui| {
            ui.label(egui::RichText::new("Images").strong());
            if n == 0 {
                ui.weak(
                    "(no image received over the air — one appears here once a .png/.jpg is sent)",
                );
                self.img_tex = None;
                self.img_key = None;
                self.prev_img_count = 0;
                return;
            }

            // If a new image arrived and I am watching the end, jump to it automatically.
            if n > self.prev_img_count && self.img_idx + 1 >= self.prev_img_count.max(1) {
                self.img_idx = n - 1;
            }
            self.prev_img_count = n;
            self.img_idx = self.img_idx.min(n - 1);
            let img = &snap.images[self.img_idx];

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.img_idx > 0, egui::Button::new("◀"))
                    .clicked()
                {
                    self.img_idx -= 1;
                }
                ui.label(format!("{}/{}", self.img_idx + 1, n));
                if ui
                    .add_enabled(self.img_idx + 1 < n, egui::Button::new("▶"))
                    .clicked()
                {
                    self.img_idx += 1;
                }
                ui.separator();
                let (tag, col) = if img.own {
                    ("↑ sent", Color32::from_rgb(140, 180, 240))
                } else {
                    ("↓ received", Color32::from_rgb(140, 210, 150))
                };
                ui.colored_label(col, tag);
                ui.label(
                    egui::RichText::new(format!(
                        "{}  ·  {}  ·  {} UTC  ·  {}×{}",
                        img.from, img.filename, img.when, img.width, img.height
                    ))
                    .strong(),
                );
            });

            let key = (self.img_idx, n, std::sync::Arc::as_ptr(&img.rgba) as usize);
            if self.img_key != Some(key) {
                let ci =
                    egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.rgba);
                match &mut self.img_tex {
                    Some(t) => t.set(ci, egui::TextureOptions::LINEAR),
                    None => {
                        self.img_tex = Some(ui.ctx().load_texture(
                            "net_img",
                            ci,
                            egui::TextureOptions::LINEAR,
                        ))
                    }
                }
                self.img_key = Some(key);
            }

            if let Some(t) = &self.img_tex {
                let maxw = ui.available_width().clamp(64.0, 720.0);
                let maxh = 380.0_f32;
                let (iw, ih) = (img.width.max(1) as f32, img.height.max(1) as f32);
                let scale = (maxw / iw).min(maxh / ih).min(1.0);
                let size = egui::vec2(iw * scale, ih * scale);
                ui.image(egui::load::SizedTexture::new(t.id(), size));
            }
        });
    }
}

// ------------------------------------------------------------------ //
// Monitor tab
// ------------------------------------------------------------------ //
impl AtchatApp {
    fn ui_monitor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, snap: &EngineSnapshot) {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("fft")
                .selected_text(format!("FFT {}", self.fft_size))
                .show_ui(ui, |ui| {
                    for sz in [512usize, 1024, 2048] {
                        if ui
                            .selectable_value(&mut self.fft_size, sz, format!("{sz}"))
                            .clicked()
                        {
                            let mut sp = SpectrumAnalyzer::new(sz);
                            sp.set_averaging(self.avg_alpha);
                            self.spectrum = sp;
                        }
                    }
                });
            egui::ComboBox::from_id_salt("cmap")
                .selected_text(self.cmap.name())
                .show_ui(ui, |ui| {
                    for cm in Colormap::ALL {
                        ui.selectable_value(&mut self.cmap, cm, cm.name());
                    }
                });
            if ui
                .add(egui::Slider::new(&mut self.avg_alpha, 0.0..=0.95).text("average"))
                .changed()
            {
                self.spectrum.set_averaging(self.avg_alpha);
            }
            ui.add(egui::Slider::new(&mut self.wf_floor, -140.0..=-40.0).text("floor dB"));
            ui.add(egui::Slider::new(&mut self.wf_ceil, -60.0..=0.0).text("ceil dB"));
            ui.add(egui::Slider::new(&mut self.scope_ms, 20.0..=1000.0).text("scope ms"));
            if ui.button("Reset peak").clicked() {
                self.spectrum.reset_peak();
            }
            if ui
                .checkbox(&mut self.freq_zoom, "Data-band zoom")
                .on_hover_text("Limit the spectrum + waterfall to 0–2.76 kHz")
                .changed()
            {
                self.waterfall.clear();
            }

            if ui.checkbox(&mut self.audio_on, "Audio").changed() {
                if self.audio_on {
                    self.engine
                        .audio_enabled
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    match AudioOut::start(
                        Arc::clone(&self.engine.audio_ring),
                        Arc::clone(&self.audio_vol),
                    ) {
                        Ok(a) => {
                            self.audio = Some(a);
                            self.audio_failed = false;
                        }
                        Err(e) => {
                            tracing::warn!("could not start audio: {e}");
                            self.audio_failed = true;
                            self.audio_on = false;
                            self.engine
                                .audio_enabled
                                .store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                } else {
                    self.audio = None;
                    self.engine
                        .audio_enabled
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    self.engine.audio_ring.lock().unwrap().clear();
                }
            }
            if self.audio_on {
                let mut v = *self.audio_vol.lock().unwrap();
                if ui
                    .add(egui::Slider::new(&mut v, 0.0..=1.0).text("volume"))
                    .changed()
                {
                    *self.audio_vol.lock().unwrap() = v;
                }
            }
            if self.audio_failed {
                ui.colored_label(Color32::from_rgb(220, 120, 120), "no audio device");
            }
        });

        ui.separator();
        ui.label("Scope (time domain)");
        self.draw_scope(ui);
        ui.add_space(6.0);
        ui.label(format!(
            "Spectrum (0–{:.1} kHz, shaded = 312–2688 Hz data band)",
            self.view_hz() / 1000.0
        ));
        self.draw_spectrum(ui);
        ui.add_space(6.0);
        ui.label("Waterfall");
        self.draw_waterfall(ui, ctx);

        ui.add_space(6.0);
        ui.label("Decode strip (passive monitor):");
        egui::ScrollArea::vertical()
            .id_salt("decodes")
            .stick_to_bottom(true)
            .max_height(110.0)
            .show(ui, |ui| {
                for d in &snap.monitor_decodes {
                    ui.monospace(d);
                }
            });
    }

    fn draw_scope(&self, ui: &mut egui::Ui) {
        let (resp, painter) =
            ui.allocate_painter(Vec2::new(ui.available_width(), 110.0), egui::Sense::hover());
        let rect = resp.rect;
        painter.rect_filled(rect, 2.0, Color32::from_gray(16));
        painter.hline(
            rect.x_range(),
            rect.center().y,
            Stroke::new(1.0_f32, Color32::from_gray(50)),
        );

        let width = rect.width().max(1.0) as usize;
        let window = ((self.scope_ms / 1000.0) * 8000.0) as usize;
        let env = self.scope.envelope(window, width);
        let stroke = Stroke::new(1.0_f32, Color32::from_rgb(120, 220, 140));
        for (i, (mn, mx)) in env.iter().enumerate() {
            let x = rect.left() + i as f32;
            let y0 = rect.center().y - mx * rect.height() * 0.48;
            let y1 = rect.center().y - mn * rect.height() * 0.48;
            painter.line_segment([Pos2::new(x, y0), Pos2::new(x, y1)], stroke);
        }
    }

    fn draw_spectrum(&self, ui: &mut egui::Ui) {
        let (resp, painter) =
            ui.allocate_painter(Vec2::new(ui.available_width(), 160.0), egui::Sense::hover());
        let rect = resp.rect;
        painter.rect_filled(rect, 2.0, Color32::from_gray(16));

        let n = self.spectrum.n_bins();
        if n == 0 {
            return;
        }
        let view_hz = self.view_hz();
        let max_bin = self.view_bins().min(n);
        // data-band shading
        let bx = |hz: f32| rect.left() + (hz / view_hz).clamp(0.0, 1.0) * rect.width();
        painter.rect_filled(
            Rect::from_x_y_ranges(bx(312.0)..=bx(2688.0), rect.y_range()),
            0.0,
            Color32::from_rgba_unmultiplied(80, 120, 200, 28),
        );
        // vertical guides (500 Hz steps)
        let mut hz = 500.0_f32;
        while hz < view_hz - 1.0 {
            let x = bx(hz);
            painter.vline(
                x,
                rect.y_range(),
                Stroke::new(1.0_f32, Color32::from_gray(40)),
            );
            painter.text(
                Pos2::new(x + 2.0, rect.bottom() - 12.0),
                egui::Align2::LEFT_BOTTOM,
                if hz >= 1000.0 {
                    format!("{:.1}k", hz / 1000.0)
                } else {
                    format!("{hz:.0}")
                },
                egui::FontId::proportional(10.0),
                Color32::from_gray(120),
            );
            hz += 500.0;
        }

        let db = self.spectrum.magnitudes_db();
        let peak = self.spectrum.peak_db();
        let w = rect.width().max(1.0) as usize;
        let mut cur = Vec::with_capacity(w);
        let mut pk = Vec::with_capacity(w);
        for x in 0..w {
            let bin = (x * max_bin.saturating_sub(1)) / w.max(1);
            let yv = db_to_unit(db[bin], self.wf_floor, self.wf_ceil);
            let yp = db_to_unit(peak[bin], self.wf_floor, self.wf_ceil);
            let px = rect.left() + x as f32;
            cur.push(Pos2::new(px, rect.bottom() - yv * rect.height()));
            pk.push(Pos2::new(px, rect.bottom() - yp * rect.height()));
        }
        painter.add(egui::Shape::line(
            pk,
            Stroke::new(1.0_f32, Color32::from_rgb(200, 90, 90)),
        ));
        painter.add(egui::Shape::line(
            cur,
            Stroke::new(1.5_f32, Color32::from_rgb(120, 200, 240)),
        ));
    }

    fn draw_waterfall(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let rgb = self.waterfall.to_rgb(self.cmap, true);
        let img = egui::ColorImage::from_rgb([WF_W, WF_H], &rgb);
        match &mut self.wf_tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => {
                self.wf_tex =
                    Some(ctx.load_texture("waterfall", img, egui::TextureOptions::LINEAR));
            }
        }
        let (resp, painter) =
            ui.allocate_painter(Vec2::new(ui.available_width(), 256.0), egui::Sense::hover());
        if let Some(t) = &self.wf_tex {
            painter.image(
                t.id(),
                resp.rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }
}
