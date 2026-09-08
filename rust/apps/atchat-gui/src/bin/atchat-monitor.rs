//! Monitör (monitor.py karşılığı) — kanalı TCP ile PASİF dinler, havadaki
//! dalgayı scope / spectrum / waterfall olarak gösterir, çözülen çerçeveleri
//! loglar ve (cpal ile) sesi hoparlöre verir. Roster'da görünmez.

use atchat_gui::{init_tracing, native_options, AppConfig, AtchatApp, EngineHandle, Tab};
use clap::Parser;

#[derive(Parser)]
#[command(about = "AtCHAT monitör — kanalı pasif dinleyen scope/spectrum/waterfall")]
struct Args {
    /// Dinlenecek kanal adresi.
    #[arg(long, default_value = "127.0.0.1:6000")]
    connect: String,
}

fn main() -> eframe::Result<()> {
    init_tracing("warn,atchat_gui=info");
    let args = Args::parse();
    let addr = args.connect;
    eframe::run_native(
        "AtCHAT Monitör",
        native_options(&format!("AtCHAT Monitör → {addr}"), [980.0, 820.0]),
        Box::new(move |cc| {
            let engine = EngineHandle::spawn_monitor(cc.egui_ctx.clone(), addr.clone());
            Ok(Box::new(AtchatApp::new(
                cc,
                engine,
                AppConfig {
                    title: format!("Monitör → {addr}"),
                    tabs: vec![Tab::Monitor],
                },
            )))
        }),
    )
}
