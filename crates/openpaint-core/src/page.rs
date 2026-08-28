//! A page: one tiled canvas with its print metadata.
//!
//! Per `docs/DECISIONS.md` §5a, a page has **exact pixel dimensions** — nothing is
//! infinite. Growing it is an explicit [`Page::resize`], which is the single
//! primitive behind extend, crop, and (later) drag-to-resize.
//!
//! DPI lives here as *metadata*, never as something the engine reasons about:
//! pixels are the canvas, and "300 DPI A4" is a preset that computes 2480×3508.

use crate::canvas::Canvas;

/// Default DPI for a new page. Screen-ish; print presets set their own.
pub const DEFAULT_DPI: f32 = 72.0;

/// Where existing content sits when a page changes size.
///
/// The nine positions Photoshop's Canvas Size dialog offers, expressed as two
/// independent axes rather than nine variants — which keeps the offset arithmetic
/// to two small functions instead of a nine-arm match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Anchor {
    pub h: Horizontal,
    pub v: Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Horizontal {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Vertical {
    #[default]
    Top,
    Middle,
    Bottom,
}

impl Anchor {
    /// Content pinned to the top-left, i.e. what "extend down" and "extend right"
    /// need: existing pixels keep their coordinates.
    pub const TOP_LEFT: Self = Self {
        h: Horizontal::Left,
        v: Vertical::Top,
    };
    /// Content pinned to the bottom-left — what "extend up" needs.
    pub const BOTTOM_LEFT: Self = Self {
        h: Horizontal::Left,
        v: Vertical::Bottom,
    };
    /// Content pinned to the top-right — what "extend left" needs.
    pub const TOP_RIGHT: Self = Self {
        h: Horizontal::Right,
        v: Vertical::Top,
    };
    pub const CENTER: Self = Self {
        h: Horizontal::Center,
        v: Vertical::Middle,
    };

    /// How far existing content moves when the size changes, in pixels.
    ///
    /// Signed and may be negative when shrinking. Note the asymmetry this encodes:
    /// extending down/right yields `(0, 0)` — nothing moves, which is why those are
    /// the cheap directions — while extending up/left shifts every coordinate.
    #[must_use]
    pub fn offset(self, old_w: u32, old_h: u32, new_w: u32, new_h: u32) -> (i32, i32) {
        let dw = new_w as i64 - old_w as i64;
        let dh = new_h as i64 - old_h as i64;
        let dx = match self.h {
            Horizontal::Left => 0,
            Horizontal::Center => dw / 2,
            Horizontal::Right => dw,
        };
        let dy = match self.v {
            Vertical::Top => 0,
            Vertical::Middle => dh / 2,
            Vertical::Bottom => dh,
        };
        (dx as i32, dy as i32)
    }
}

pub struct Page {
    canvas: Canvas,
    /// Dots per inch, for print and export. Metadata only — see the module note.
    dpi: f32,
}

impl Page {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            canvas: Canvas::new(width, height),
            dpi: DEFAULT_DPI,
        }
    }

    #[must_use]
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn canvas_mut(&mut self) -> &mut Canvas {
        &mut self.canvas
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.canvas.width()
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.canvas.height()
    }

    #[must_use]
    pub fn dpi(&self) -> f32 {
        self.dpi
    }

    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi.max(1.0);
    }

    /// Change the page's size, keeping existing content at `anchor`.
    ///
    /// Returns how far content moved, which callers need: GPU textures must copy
    /// the old contents to the same offset, and stored page coordinates (undo
    /// rectangles) must be shifted by it.
    pub fn resize(&mut self, new_w: u32, new_h: u32, anchor: Anchor) -> (i32, i32) {
        self.canvas.resize(new_w, new_h, anchor)
    }

    /// Grow the page downward by `amount` pixels — the webtoon "Extend ↓".
    ///
    /// `amount` is a parameter rather than a constant on purpose (DECISIONS §5a):
    /// it is user-configurable, and later drag-to-extend feeds the same call.
    pub fn extend_down(&mut self, amount: u32) -> (i32, i32) {
        self.resize(self.width(), self.height() + amount, Anchor::TOP_LEFT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extending_down_or_right_moves_nothing() {
        // The cheap directions: existing coordinates are untouched.
        assert_eq!(Anchor::TOP_LEFT.offset(100, 100, 100, 400), (0, 0));
        assert_eq!(Anchor::TOP_LEFT.offset(100, 100, 400, 100), (0, 0));
    }

    #[test]
    fn extending_up_shifts_content_down() {
        assert_eq!(Anchor::BOTTOM_LEFT.offset(100, 100, 100, 400), (0, 300));
    }

    #[test]
    fn extending_left_shifts_content_right() {
        assert_eq!(Anchor::TOP_RIGHT.offset(100, 100, 400, 100), (300, 0));
    }

    #[test]
    fn centering_splits_the_difference() {
        assert_eq!(Anchor::CENTER.offset(100, 100, 300, 300), (100, 100));
    }

    /// Shrinking gives negative offsets, which is what a crop needs.
    #[test]
    fn shrinking_yields_negative_offsets() {
        assert_eq!(Anchor::BOTTOM_LEFT.offset(100, 400, 100, 100), (0, -300));
        assert_eq!(Anchor::CENTER.offset(300, 300, 100, 100), (-100, -100));
    }

    #[test]
    fn a_new_page_has_the_requested_size() {
        let p = Page::new(800, 1200);
        assert_eq!((p.width(), p.height()), (800, 1200));
        assert_eq!(p.dpi(), DEFAULT_DPI);
    }

    #[test]
    fn extend_down_grows_only_the_height() {
        let mut p = Page::new(800, 1000);
        let moved = p.extend_down(500);
        assert_eq!((p.width(), p.height()), (800, 1500));
        assert_eq!(moved, (0, 0), "extending down must not move content");
    }

    #[test]
    fn dpi_cannot_be_set_to_nonsense() {
        let mut p = Page::new(10, 10);
        p.set_dpi(0.0);
        assert!(p.dpi() >= 1.0);
    }
}
