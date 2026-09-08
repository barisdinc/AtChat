//! Client (client.py karşılığı) — bir kanala TCP ile bağlanan istasyon(lar).
//! İSTEDİĞİNİZ KADAR örnek çalıştırın; her pencere kendi istasyonlarını yönetir.

use atchat_gui::{init_tracing, native_options, AppConfig, AtchatApp, EngineHandle, Tab};
use clap::Parser;

#[derive(Parser)]
#[command(about = "AtCHAT client — kanala TCP ile bağlanan istasyon GUI'si")]
struct Args {
    /// Bağlanılacak kanal adresi.
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
