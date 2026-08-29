//! OpenPaint engine core.
//!
//! This crate is deliberately **UI-agnostic and platform-agnostic**: it knows
//! nothing about windows, buttons, any UI framework, or any OS. It houses the
//! document model, the tiled canvas, the brush engine, and (later) the GPU
//! compositor. Everything here compiles and runs identically on Windows, Linux,
//! and macOS — the only platform-specific code (stylus input) lives in the app
//! layer behind a trait.

pub mod brush;
pub mod canvas;
pub mod color;
pub mod curve;
pub mod dab;
pub mod document;
pub mod layer;
pub mod lifted;
pub mod modulation;
pub mod page;
pub mod raster;
pub mod region;
pub mod selection;
pub mod stabilizer;
pub mod stamp;
pub mod stroke;
pub mod text;
pub mod tile;

pub use brush::{Brush, StrokeState};
pub use canvas::Canvas;
pub use curve::Curve;
pub use dab::Dab;
pub use document::Document;
pub use layer::{Blend, Content, Layer};
pub use lifted::Lifted;
pub use modulation::{Input, Response, Source};
pub use page::{Page, PageRect, PageResize, Side};
pub use selection::Selection;
pub use stabilizer::{Smoothed, Stabilizer};
pub use stamp::Stamp;
pub use stroke::StrokePainter;
pub use text::{TextBlock, TextRenderer};

/// Crate version, surfaced so the app/UI can display it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Sanity signal that the core crate is linked and reachable.
pub fn hello() -> &'static str {
    "openpaint-core online"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_reports_online() {
        assert_eq!(hello(), "openpaint-core online");
    }
}
