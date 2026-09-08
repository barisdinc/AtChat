//! The AtCHAT egui interface library. Four binaries share it:
//! `atchat-gui` (all-in-one), `atchat-channel`, `atchat-client`,
//! `atchat-monitor`.

pub mod app;
pub mod audio;
pub mod engine;

pub use app::{AppConfig, AtchatApp, Tab};
pub use engine::EngineHandle;

pub fn init_tracing(default_filter: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .try_init();
}

pub fn native_options(title: &str, size: [f32; 2]) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([760.0, 520.0])
            .with_title(title),
        ..Default::default()
    }
}
