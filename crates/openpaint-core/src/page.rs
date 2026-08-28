//! A page: one tiled canvas with its print metadata.
//!
//! Per `docs/DECISIONS.md` §5a, a page has **exact pixel dimensions** — nothing is
//! infinite. Resizing it is [`Page::resize`], which takes the target **rectangle**;
//! extend, crop, and drag-to-resize are all just different rectangles.
//!
//! # Why a rectangle rather than a size plus an anchor
//!
//! An earlier version took a size and an "anchor" saying which edges moved, because
//! page coordinates were re-based at zero and a rectangle could not be named. Once
//! coordinates became stable (see [`crate::canvas`]) the rectangle became the natural
//! thing to state, and it is strictly more expressive: it can trim 10 px off the left
//! and 30 off the right, which size-plus-anchor cannot say at all.
//!
//! It also collapses what used to be two easily-confused derived offsets into a
//! subtraction, and makes the inverse of a resize simply "swap the two rectangles".
//!
//! DPI lives here as *metadata*, never as something the engine reasons about: pixels
//! are the canvas, and "300 DPI A4" is a preset that computes 2480×3508.

use crate::canvas::Canvas;

/// Default DPI for a new page. Screen-ish; print presets set their own.
pub const DEFAULT_DPI: f32 = 72.0;

/// A rectangle in page coordinates. The corner may be negative, since extending
/// leftward or upward moves the origin (see [`crate::canvas`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl PageRect {
    #[must_use]
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self {
            x,
            y,
            w: w.max(1),
            h: h.max(1),
        }
    }

    /// A rectangle at the origin, for a fresh page.
    #[must_use]
    pub fn from_size(w: u32, h: u32) -> Self {
        Self::new(0, 0, w, h)
    }

    #[must_use]
    pub fn origin(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// One past the bottom-right corner.
    #[must_use]
    pub fn end(&self) -> (i32, i32) {
        (self.x + self.w as i32, self.y + self.h as i32)
    }

    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        let (ex, ey) = self.end();
        x >= self.x && y >= self.y && x < ex && y < ey
    }

    /// Whether this rectangle fully covers `other`.
    ///
    /// Used to decide whether a resize loses pixels, which is more accurate than
    /// comparing sizes: a rectangle can move without shrinking and still drop content.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        let (ex, ey) = self.end();
        let (ox, oy) = other.end();
        self.x <= other.x && self.y <= other.y && ex >= ox && ey >= oy
    }

    /// This rectangle grown by `amount` on the given side.
    #[must_use]
    pub fn extended(&self, side: Side, amount: u32) -> Self {
        let a = amount as i32;
        match side {
            Side::Top => Self::new(self.x, self.y - a, self.w, self.h + amount),
            Side::Bottom => Self::new(self.x, self.y, self.w, self.h + amount),
            Side::Left => Self::new(self.x - a, self.y, self.w + amount, self.h),
            Side::Right => Self::new(self.x, self.y, self.w + amount, self.h),
        }
    }
}

/// Which edge of a page an extend grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// A page resize, expressed as the rectangle before and after.
///
/// Both offsets are derived by subtraction, which is why there is no anchor and no
/// stored offset to disagree with the sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageResize {
    pub old: PageRect,
    pub new: PageRect,
}

impl PageResize {
    /// How the page rectangle's origin moves. Content does not move.
    #[must_use]
    pub fn origin_shift(&self) -> (i32, i32) {
        (self.new.x - self.old.x, self.new.y - self.old.y)
    }

    /// Where old content lands inside a **zero-based texture** of the new size.
    ///
    /// The GPU needs this because a texture always starts at (0, 0), so growing upward
    /// means copying the old contents lower down. It is the negation of the origin
    /// shift.
    #[must_use]
    pub fn content_offset(&self) -> (i32, i32) {
        (self.old.x - self.new.x, self.old.y - self.new.y)
    }

    /// The inverse resize, for undoing this one — simply the two rectangles swapped.
    #[must_use]
    pub fn inverted(&self) -> Self {
        Self {
            old: self.new,
            new: self.old,
        }
    }

    /// Whether this resize drops any content, and therefore needs pixels saved to be
    /// undone.
    #[must_use]
    pub fn loses_pixels(&self) -> bool {
        !self.new.covers(&self.old)
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

    /// The page's rectangle in page coordinates.
    #[must_use]
    pub fn rect(&self) -> PageRect {
        self.canvas.rect()
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
    pub fn origin(&self) -> (i32, i32) {
        self.canvas.origin()
    }

    #[must_use]
    pub fn dpi(&self) -> f32 {
        self.dpi
    }

    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi.max(1.0);
    }

    /// Move the page to a new rectangle, keeping existing content exactly where it is.
    ///
    /// Returns how far the **origin** moved, which the renderer needs in order to place
    /// the old texture contents inside the new one. Nothing else needs adjusting:
    /// content coordinates are stable, so stored page coordinates stay valid.
    pub fn resize(&mut self, rect: PageRect) -> (i32, i32) {
        self.canvas.resize(rect)
    }

    /// Grow the page downward by `amount` pixels — the webtoon "Extend ↓".
    ///
    /// `amount` is a parameter rather than a constant on purpose (DECISIONS §5a): it is
    /// user-configurable, and drag-to-extend feeds the same call.
    pub fn extend(&mut self, side: Side, amount: u32) -> (i32, i32) {
        self.resize(self.rect().extended(side, amount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extending_the_bottom_or_right_leaves_the_origin_alone() {
        let r = PageRect::from_size(100, 100);
        assert_eq!(r.extended(Side::Bottom, 300), PageRect::new(0, 0, 100, 400));
        assert_eq!(r.extended(Side::Right, 300), PageRect::new(0, 0, 400, 100));
    }

    /// Extending upward moves the origin *up* (negative y) and leaves content alone.
    #[test]
    fn extending_the_top_moves_the_origin_up() {
        let r = PageRect::from_size(100, 100);
        assert_eq!(r.extended(Side::Top, 300), PageRect::new(0, -300, 100, 400));
    }

    #[test]
    fn extending_the_left_moves_the_origin_left() {
        let r = PageRect::from_size(100, 100);
        assert_eq!(
            r.extended(Side::Left, 300),
            PageRect::new(-300, 0, 400, 100)
        );
    }

    /// The two derived offsets are exact opposites: the rectangle moving one way is the
    /// content moving the other way within it. Confusing them is the likeliest silent
    /// bug here, so it is pinned.
    #[test]
    fn the_two_offsets_are_opposites() {
        let r = PageResize {
            old: PageRect::from_size(100, 100),
            new: PageRect::new(0, -500, 100, 600),
        };
        assert_eq!(r.origin_shift(), (0, -500));
        assert_eq!(r.content_offset(), (0, 500));
    }

    #[test]
    fn inverting_swaps_the_rectangles_and_reverses_both_offsets() {
        let r = PageResize {
            old: PageRect::new(10, 20, 300, 300),
            new: PageRect::new(-40, -10, 800, 700),
        };
        let back = r.inverted();
        assert_eq!(back.old, r.new);
        assert_eq!(back.new, r.old);

        let (ox, oy) = r.origin_shift();
        assert_eq!(back.origin_shift(), (-ox, -oy));
        let (cx, cy) = r.content_offset();
        assert_eq!(back.content_offset(), (-cx, -cy));
    }

    /// Growing never loses pixels; that is what makes undoable extends free.
    #[test]
    fn growing_loses_nothing() {
        let grow = PageResize {
            old: PageRect::from_size(100, 100),
            new: PageRect::new(0, -400, 100, 500),
        };
        assert!(!grow.loses_pixels());
        assert!(
            grow.inverted().loses_pixels(),
            "undoing a grow drops pixels"
        );
    }

    /// A rectangle can *move* without shrinking and still drop content. Comparing
    /// sizes would have missed this; comparing coverage does not.
    #[test]
    fn a_same_size_rectangle_that_moved_loses_pixels() {
        let slid = PageResize {
            old: PageRect::new(0, 0, 100, 100),
            new: PageRect::new(50, 0, 100, 100),
        };
        assert!(
            slid.loses_pixels(),
            "sliding the window drops the left strip"
        );
    }

    #[test]
    fn covers_is_inclusive_of_an_identical_rectangle() {
        let r = PageRect::new(-5, -5, 20, 20);
        assert!(r.covers(&r));
    }

    #[test]
    fn a_new_page_has_the_requested_size_at_the_origin() {
        let p = Page::new(800, 1200);
        assert_eq!(p.rect(), PageRect::new(0, 0, 800, 1200));
        assert_eq!(p.dpi(), DEFAULT_DPI);
    }

    #[test]
    fn extending_down_grows_only_the_height() {
        let mut p = Page::new(800, 1000);
        let moved = p.extend(Side::Bottom, 500);
        assert_eq!(p.rect(), PageRect::new(0, 0, 800, 1500));
        assert_eq!(moved, (0, 0), "extending down must not move the origin");
    }

    #[test]
    fn extending_up_leaves_a_negative_origin() {
        let mut p = Page::new(800, 1000);
        let shift = p.extend(Side::Top, 500);
        assert_eq!(shift, (0, -500));
        assert_eq!(p.rect(), PageRect::new(0, -500, 800, 1500));
    }

    #[test]
    fn a_rect_is_never_degenerate() {
        assert_eq!(PageRect::new(0, 0, 0, 0), PageRect::new(0, 0, 1, 1));
    }

    #[test]
    fn dpi_cannot_be_set_to_nonsense() {
        let mut p = Page::new(10, 10);
        p.set_dpi(0.0);
        assert!(p.dpi() >= 1.0);
    }
}
