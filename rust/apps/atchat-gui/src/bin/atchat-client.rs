//! Client (the client.py counterpart) — station(s) that connect to a channel
//! over TCP. Run AS MANY instances as you like; each window manages its own stations.

use atchat_gui::{init_tracing, native_options, AppConfig, AtchatApp, EngineHandle, Tab};
use clap::Parser;

#[derive(Parser)]
#[command(about = "AtCHAT client — a station GUI that connects to a channel over TCP")]
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
        "AtCHAT Client",
        native_options(&format!("AtCHAT Client → {addr}"), [1120.0, 780.0]),
        Box::new(move |cc| {
            let engine = EngineHandle::spawn_client(cc.egui_ctx.clone(), addr.clone());
            Ok(Box::new(AtchatApp::new(
                cc,
                engine,
                AppConfig {
                    title: format!("Client → {addr}"),
                    tabs: vec![Tab::Stations, Tab::Net],
                },
            )))
        }),
    )
}
