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

    /// How far the canvas **origin** moves when the size changes.
    ///
    /// This is the negative of [`Anchor::offset`]: content stays where it is and the
    /// rectangle moves around it. Extending leftward with a right anchor decreases the
    /// origin's x by the growth, so the left edge moves out and the right edge stays.
    #[must_use]
    pub fn origin_shift(self, old_w: u32, old_h: u32, new_w: u32, new_h: u32) -> (i32, i32) {
        let (dx, dy) = self.offset(old_w, old_h, new_w, new_h);
        (-dx, -dy)
    }

    /// How far content *would* move if the canvas were re-based at zero.
    ///
    /// Retained because it is the natural way to express an anchor, and
    /// [`Anchor::origin_shift`] is defined from it — but note that content does **not**
    /// move (see [`crate::canvas`]).
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

/// A described page resize, from which both offsets are **derived**.
///
/// Deriving them removes a class of bug: an offset carried alongside the sizes can
/// disagree with them, and the result is content placed wrongly with nothing raised.
///
/// Two offsets, easy to confuse, so they are named for what they move:
/// - [`PageResize::origin_shift`] — how the page rectangle moves in page coordinates.
///   Content does not move (see [`crate::canvas`]).
/// - [`PageResize::content_offset`] — where old content lands inside a **zero-based
///   texture** of the new size. The GPU needs this because a texture always starts at
///   (0, 0), so growing upward means copying the old contents lower down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageResize {
    pub old_w: u32,
    pub old_h: u32,
    pub new_w: u32,
    pub new_h: u32,
    pub anchor: Anchor,
}

impl PageResize {
    /// How the page rectangle's origin moves.
    #[must_use]
    pub fn origin_shift(&self) -> (i32, i32) {
        self.anchor
            .origin_shift(self.old_w, self.old_h, self.new_w, self.new_h)
    }

    /// Where old content lands inside a zero-based texture of the new size.
    #[must_use]
    pub fn content_offset(&self) -> (i32, i32) {
        self.anchor
            .offset(self.old_w, self.old_h, self.new_w, self.new_h)
    }

    /// The inverse resize, for undoing this one.
    ///
    /// Swapping the sizes and keeping the anchor suffices, because both offsets are
    /// computed from the size difference — so the same anchor yields exactly the
    /// opposite movement.
    #[must_use]
    pub fn inverted(&self) -> Self {
        Self {
            old_w: self.new_w,
            old_h: self.new_h,
            new_w: self.old_w,
            new_h: self.old_h,
            anchor: self.anchor,
        }
    }

    /// Whether this resize loses pixels, and therefore needs them saved to be undone.
    #[must_use]
    pub fn shrinks(&self) -> bool {
        self.new_w < self.old_w || self.new_h < self.old_h
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

    /// Top-left corner in page coordinates. Negative once the page has been extended
    /// leftward or upward; see [`crate::canvas`] for why coordinates are stable.
    #[must_use]
    pub fn origin(&self) -> (i32, i32) {
        self.canvas.origin()
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

    /// Change the page's size, keeping existing content exactly where it is.
    ///
    /// Returns how far the **origin** moved, which the renderer needs in order to place
    /// the old texture contents inside the new one. Nothing else needs adjusting:
    /// content coordinates are stable, so stored page coordinates stay valid.
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
    fn extending_down_or_right_leaves_the_origin_alone() {
        assert_eq!(Anchor::TOP_LEFT.origin_shift(100, 100, 100, 400), (0, 0));
        assert_eq!(Anchor::TOP_LEFT.origin_shift(100, 100, 400, 100), (0, 0));
    }

    /// Extending upward moves the origin *up* (negative y) and leaves content alone —
    /// the opposite of re-basing at zero, which would have moved every pixel down.
    #[test]
    fn extending_up_moves_the_origin_up() {
        assert_eq!(
            Anchor::BOTTOM_LEFT.origin_shift(100, 100, 100, 400),
            (0, -300)
        );
    }

    #[test]
    fn extending_left_moves_the_origin_left() {
        assert_eq!(
            Anchor::TOP_RIGHT.origin_shift(100, 100, 400, 100),
            (-300, 0)
        );
    }

    /// The origin shift is exactly the negative of the content offset. If these ever
    /// disagree, one of the two coordinate conventions has drifted.
    #[test]
    fn origin_shift_is_the_negated_content_offset() {
        for anchor in [
            Anchor::TOP_LEFT,
            Anchor::BOTTOM_LEFT,
            Anchor::TOP_RIGHT,
            Anchor::CENTER,
        ] {
            let (ox, oy) = anchor.origin_shift(300, 300, 800, 700);
            let (cx, cy) = anchor.offset(300, 300, 800, 700);
            assert_eq!((ox, oy), (-cx, -cy), "mismatch for {anchor:?}");
        }
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
        assert_eq!(moved, (0, 0), "extending down must not move the origin");
        assert_eq!(p.origin(), (0, 0));
    }

    /// Extending upward must leave the origin negative rather than moving content.
    #[test]
    fn extending_up_leaves_a_negative_origin() {
        let mut p = Page::new(800, 1000);
        let shift = p.resize(800, 1500, Anchor::BOTTOM_LEFT);
        assert_eq!(shift, (0, -500));
        assert_eq!(p.origin(), (0, -500));
        assert_eq!(p.height(), 1500);
    }

    #[test]
    fn dpi_cannot_be_set_to_nonsense() {
        let mut p = Page::new(10, 10);
        p.set_dpi(0.0);
        assert!(p.dpi() >= 1.0);
    }

    /// Inverting must reverse both offsets, since that is what undoing a resize relies
    /// on.
    #[test]
    fn inverting_a_resize_reverses_both_offsets() {
        for anchor in [
            Anchor::TOP_LEFT,
            Anchor::BOTTOM_LEFT,
            Anchor::TOP_RIGHT,
            Anchor::CENTER,
        ] {
            let forward = PageResize {
                old_w: 300,
                old_h: 300,
                new_w: 800,
                new_h: 700,
                anchor,
            };
            let back = forward.inverted();
            let (ox, oy) = forward.origin_shift();
            let (bx, by) = back.origin_shift();
            assert_eq!((bx, by), (-ox, -oy), "origin shift for {anchor:?}");

            let (cx, cy) = forward.content_offset();
            let (dx, dy) = back.content_offset();
            assert_eq!((dx, dy), (-cx, -cy), "content offset for {anchor:?}");
        }
    }

    /// The two offsets are opposites: the rectangle moving one way is the same as the
    /// content moving the other way within it.
    #[test]
    fn the_two_offsets_are_opposites() {
        let r = PageResize {
            old_w: 100,
            old_h: 100,
            new_w: 100,
            new_h: 600,
            anchor: Anchor::BOTTOM_LEFT,
        };
        assert_eq!(r.origin_shift(), (0, -500));
        assert_eq!(r.content_offset(), (0, 500));
    }

    #[test]
    fn only_a_shrink_needs_pixels_saved() {
        let grow = PageResize {
            old_w: 100,
            old_h: 100,
            new_w: 100,
            new_h: 400,
            anchor: Anchor::TOP_LEFT,
        };
        assert!(!grow.shrinks());
        assert!(grow.inverted().shrinks(), "undoing a grow is a shrink");

        // Growing one axis while shrinking the other still loses pixels.
        let mixed = PageResize {
            old_w: 400,
            old_h: 100,
            new_w: 100,
            new_h: 400,
            anchor: Anchor::TOP_LEFT,
        };
        assert!(mixed.shrinks());
    }
}
