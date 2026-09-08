//! Kanal (channel_server.py karşılığı) — kanal fiziğini barındırır ve TCP'de
//! dinler. `atchat-client` / `atchat-monitor` (ve Python client.py/monitor.py)
//! buraya bağlanır. Tek örnek çalıştırın.

use atchat_gui::{init_tracing, native_options, AppConfig, AtchatApp, EngineHandle, Tab};
use clap::Parser;

#[derive(Parser)]
#[command(about = "AtCHAT kanal — TCP'de dinleyen kanal fiziği + GUI")]
struct Args {
    /// Dinlenecek TCP portu (127.0.0.1).
    #[arg(long, default_value_t = 6000)]
    port: u16,
}

fn main() -> eframe::Result<()> {
    init_tracing("warn,atchat_gui=info");
    let args = Args::parse();
    let port = args.port;
    eframe::run_native(
        "AtCHAT Kanal",
        native_options(&format!("AtCHAT Kanal :{port}"), [820.0, 640.0]),
        Box::new(move |cc| {
            let engine = EngineHandle::spawn_channel_host(cc.egui_ctx.clone(), port);
            Ok(Box::new(AtchatApp::new(
                cc,
                engine,
                AppConfig {
                    title: format!("AtCHAT Kanal :{port}"),
                    tabs: vec![Tab::Channel],
                },
            )))
        }),
    )
}
