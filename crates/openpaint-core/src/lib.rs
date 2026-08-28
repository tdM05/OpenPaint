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
pub mod dab;
pub mod document;
pub mod page;
pub mod raster;
pub mod stroke;
pub mod tile;

pub use brush::{Brush, StrokeState};
pub use canvas::Canvas;
pub use dab::Dab;
pub use document::{Document, Mode};
pub use page::{Anchor, Page, PageResize};
pub use stroke::StrokePainter;

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
