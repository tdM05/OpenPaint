//! OpenPaint engine core.
//!
//! This crate is deliberately **UI-agnostic**: it knows nothing about windows,
//! buttons, or any specific UI framework. It will house the document model,
//! the tiled canvas, the brush engine, and the GPU compositor.
//!
//! Right now it is an empty shell — the first real subsystem lands in Phase 0.

/// Crate version, surfaced so the app/UI can display it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Placeholder to give the pipeline something to build and test until the
/// first real subsystem exists. Will be removed once the tiled canvas lands.
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
