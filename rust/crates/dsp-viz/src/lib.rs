//! Monitör görselleştirme DSP'si — GUI çatısından bağımsız, saf veri üretir
//! (`Vec<(f32,f32)>` zarf, `Vec<f32>` dB spektrum, `Vec<u8>` RGB waterfall).
//! Faz 5'te `atchat-gui` bunları egui doku/şekillerine çevirir.

mod colormap;
mod scope;
mod spectrum;
mod waterfall;

pub use colormap::Colormap;
pub use scope::ScopeBuf;
pub use spectrum::{db_to_unit, SpectrumAnalyzer, DB_FLOOR};
pub use waterfall::Waterfall;
