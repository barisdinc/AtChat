//! Monitor-visualisation DSP — independent of any GUI framework, it produces
//! pure data (`Vec<(f32,f32)>` envelope, `Vec<f32>` dB spectrum, `Vec<u8>` RGB
//! waterfall). In phase 5 `atchat-gui` turns these into egui textures/shapes.

mod colormap;
mod scope;
mod spectrum;
mod waterfall;

pub use colormap::Colormap;
pub use scope::ScopeBuf;
pub use spectrum::{db_to_unit, SpectrumAnalyzer, DB_FLOOR};
pub use waterfall::Waterfall;
