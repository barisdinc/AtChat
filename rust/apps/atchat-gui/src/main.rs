//! AtCHAT tek pencere GUI (egui/eframe): Kanal | İstasyonlar | Monitör.
//! Tüm protokol/kanal/modem işi ayrı bir thread'deki tokio motorunda döner;
//! bu thread yalnız arayüzü çizer.

mod app;
mod audio;
mod engine;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,atchat_gui=info".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title("AtCHAT — Telsiz NET"),
        ..Default::default()
    };

    eframe::run_native(
        "AtCHAT",
        options,
        Box::new(|cc| Ok(Box::new(app::AtchatApp::new(cc)))),
    )
}
