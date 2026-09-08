//! Channel (the channel_server.py counterpart) — hosts the channel physics
//! and listens on TCP. `atchat-client` / `atchat-monitor` (and the Python
//! client.py/monitor.py) connect here. Run a single instance.

use atchat_gui::{init_tracing, native_options, AppConfig, AtchatApp, EngineHandle, Tab};
use clap::Parser;

#[derive(Parser)]
#[command(about = "AtCHAT channel — the channel physics listening on TCP + a GUI")]
struct Args {
    /// The TCP port to listen on (127.0.0.1).
    #[arg(long, default_value_t = 6000)]
    port: u16,
}

fn main() -> eframe::Result<()> {
    init_tracing("warn,atchat_gui=info");
    let args = Args::parse();
    let port = args.port;
    eframe::run_native(
        "AtCHAT Channel",
        native_options(&format!("AtCHAT Channel :{port}"), [820.0, 640.0]),
        Box::new(move |cc| {
            let engine = EngineHandle::spawn_channel_host(cc.egui_ctx.clone(), port);
            Ok(Box::new(AtchatApp::new(
                cc,
                engine,
                AppConfig {
                    title: format!("AtCHAT Channel :{port}"),
                    tabs: vec![Tab::Channel],
                },
            )))
        }),
    )
}
