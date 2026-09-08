//! Monitor (the monitor.py counterpart) — PASSIVELY listens to the channel
//! over TCP, shows the on-air waveform as scope / spectrum / waterfall, logs
//! the decoded frames and plays the audio to the speakers (with cpal). Does
//! not appear in the roster.

use atchat_gui::{init_tracing, native_options, AppConfig, AtchatApp, EngineHandle, Tab};
use clap::Parser;

#[derive(Parser)]
#[command(about = "AtCHAT monitor — a passive scope/spectrum/waterfall on the channel")]
struct Args {
    /// The channel address to connect to.
    #[arg(long, default_value = "127.0.0.1:6000")]
    connect: String,
}

fn main() -> eframe::Result<()> {
    init_tracing("warn,atchat_gui=info");
    let args = Args::parse();
    let addr = args.connect;
    eframe::run_native(
        "AtCHAT Monitor",
        native_options(&format!("AtCHAT Monitor → {addr}"), [980.0, 820.0]),
        Box::new(move |cc| {
            let engine = EngineHandle::spawn_monitor(cc.egui_ctx.clone(), addr.clone());
            Ok(Box::new(AtchatApp::new(
                cc,
                engine,
                AppConfig {
                    title: format!("Monitor → {addr}"),
                    tabs: vec![Tab::Monitor],
                },
            )))
        }),
    )
}
