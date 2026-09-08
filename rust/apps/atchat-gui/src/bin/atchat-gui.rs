//! All-in-one: the in-proc channel + stations + monitor in a single window.
//! For quick experiments. To run many stations in separate processes, use
//! `atchat-channel` + several `atchat-client` + `atchat-monitor`.

use atchat_gui::{init_tracing, native_options, AppConfig, AtchatApp, EngineHandle, Tab};

fn main() -> eframe::Result<()> {
    init_tracing("warn,atchat_gui=info");
    eframe::run_native(
        "AtCHAT",
        native_options("AtCHAT — Radio NET", [1180.0, 800.0]),
        Box::new(|cc| {
            let engine = EngineHandle::spawn_all_in_one(cc.egui_ctx.clone());
            Ok(Box::new(AtchatApp::new(
                cc,
                engine,
                AppConfig {
                    title: "AtCHAT".into(),
                    tabs: vec![Tab::Channel, Tab::Stations, Tab::Net, Tab::Monitor],
                },
            )))
        }),
    )
}
