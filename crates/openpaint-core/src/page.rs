//! A page: a rectangle, a stack of layers, and its print metadata.
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
//!
//! # The page owns the geometry; layers own nothing but their looks
//!
//! There is exactly one rectangle per page, not one per layer. Layers are always the same
//! size as the page they are on, so storing the rectangle per layer would be a set of values
//! that must agree and therefore a set that can disagree. Pixels live in the renderer's tile
//! store keyed by layer id (see [`crate::layer`]), so a page here is geometry plus an ordered
//! list of [`Layer`] properties.

use crate::layer::Layer;

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
    rect: PageRect,
    /// Bottom-first, so index 0 is the layer furthest back. Compositing walks it in order,
    /// and "bottom-first" matches the direction paint actually stacks up -- the UI is what
    /// reverses it for display, since artists read a layer list top-down.
    layers: Vec<Layer>,
    /// Index into `layers` of the layer being painted.
    active: usize,
    /// Never reused, so a tile keyed by a dead layer's id can never be mistaken for a live
    /// one's.
    next_layer_id: u32,
    /// Dots per inch, for print and export. Metadata only -- see the module note.
    dpi: f32,
}

impl Page {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            rect: PageRect::from_size(width, height),
            layers: vec![Layer::new(0, "Layer 1")],
            active: 0,
            next_layer_id: 1,
            dpi: DEFAULT_DPI,
        }
    }

    /// The page's rectangle in page coordinates.
    #[must_use]
    pub fn rect(&self) -> PageRect {
        self.rect
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.rect.w
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.rect.h
    }

    #[must_use]
    pub fn origin(&self) -> (i32, i32) {
        self.rect.origin()
    }

    #[must_use]
    pub fn dpi(&self) -> f32 {
        self.dpi
    }

    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi.max(1.0);
    }

    /// The stack, bottom layer first.
    #[must_use]
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    #[must_use]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Index of the layer being painted. Always valid.
    #[must_use]
    pub fn active_index(&self) -> usize {
        self.active
    }

    #[must_use]
    pub fn active_layer(&self) -> &Layer {
        &self.layers[self.active]
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        &mut self.layers[self.active]
    }

    #[must_use]
    pub fn layer(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    pub fn layer_mut(&mut self, index: usize) -> Option<&mut Layer> {
        self.layers.get_mut(index)
    }

    /// Select a layer to paint on. Returns `false` if the index does not exist, so a stale
    /// index from the UI cannot leave `active` pointing at nothing.
    pub fn set_active(&mut self, index: usize) -> bool {
        if index >= self.layers.len() {
            return false;
        }
        self.active = index;
        true
    }

    /// Insert a new empty layer directly above `above`, and select it.
    ///
    /// Returns its index. Inserting above the active layer rather than at the top is what
    /// every drawing app does, because the layer you just made is nearly always meant to sit
    /// on the one you were working on.
    pub fn insert_layer_above(&mut self, above: usize, name: impl Into<String>) -> usize {
        let id = self.next_layer_id;
        self.next_layer_id += 1;
        let at = (above + 1).min(self.layers.len());
        self.layers.insert(at, Layer::new(id, name));
        self.active = at;
        at
    }

    /// Add a layer above the active one, named for its position.
    pub fn add_layer(&mut self) -> usize {
        // Named from the id, not the count: naming from the count reuses names as soon as a
        // layer is deleted, and two layers called "Layer 2" is worse than a gap in the
        // numbering.
        let name = format!("Layer {}", self.next_layer_id + 1);
        self.insert_layer_above(self.active, name)
    }

    /// Remove a layer, returning its id so its tiles can be released.
    ///
    /// Refuses to remove the last one: a page with no layers has nowhere to paint, and every
    /// caller would have to handle that state.
    pub fn remove_layer(&mut self, index: usize) -> Option<u32> {
        if self.layers.len() <= 1 || index >= self.layers.len() {
            return None;
        }
        let id = self.layers.remove(index).id();
        // Keep `active` on a real layer, preferring the one that took the deleted one's place.
        self.active = self.active.min(self.layers.len() - 1);
        Some(id)
    }

    /// Put a previously removed layer back at `index`, keeping its id.
    ///
    /// For undoing a deletion, which is why it takes a whole [`Layer`] rather than making a new
    /// one: the id has to be the same, or the tiles history is about to restore would be keyed
    /// to a layer that no longer exists.
    pub fn restore_layer(&mut self, index: usize, layer: Layer) -> usize {
        // Ids are never reused, so a restored layer can be newer than `next_layer_id` only if
        // something else already handed out its id -- keep the counter ahead regardless.
        self.next_layer_id = self.next_layer_id.max(layer.id() + 1);
        let at = index.min(self.layers.len());
        self.layers.insert(at, layer);
        self.active = at;
        at
    }

    /// Move a layer to a new index, shifting the rest. Returns where it ended up.
    pub fn move_layer(&mut self, from: usize, to: usize) -> Option<usize> {
        if from >= self.layers.len() {
            return None;
        }
        let to = to.min(self.layers.len() - 1);
        if from == to {
            return Some(to);
        }
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
        // Follow the layer that moved, if it was the active one.
        if self.active == from {
            self.active = to;
        } else if from < self.active && to >= self.active {
            self.active -= 1;
        } else if from > self.active && to <= self.active {
            self.active += 1;
        }
        Some(to)
    }

    /// Move the page to a new rectangle, keeping existing content exactly where it is.
    ///
    /// Returns how far the **origin** moved. Nothing else needs adjusting: content
    /// coordinates are stable, so stored page coordinates stay valid, and layers have no
    /// geometry of their own to update.
    pub fn resize(&mut self, rect: PageRect) -> (i32, i32) {
        let shift = (rect.x - self.rect.x, rect.y - self.rect.y);
        self.rect = rect;
        shift
    }

    /// Grow the page on one side by `amount` pixels -- the webtoon "Extend ↓" and friends.
    ///
    /// `amount` is a parameter rather than a constant on purpose (DECISIONS §5a): it is
    /// user-configurable, and drag-to-extend feeds the same call.
    pub fn extend(&mut self, side: Side, amount: u32) -> (i32, i32) {
        self.resize(self.rect.extended(side, amount))
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

    /// A fresh page has somewhere to paint, because every caller would otherwise have to
    /// handle "no layers".
    #[test]
    fn a_new_page_starts_with_one_active_layer() {
        let p = Page::new(100, 100);
        assert_eq!(p.layer_count(), 1);
        assert_eq!(p.active_index(), 0);
        assert!(p.active_layer().visible);
    }

    /// A new layer goes *above* the one being worked on and becomes active, which is what
    /// every drawing app does and what makes "add a layer and keep drawing" work.
    #[test]
    fn a_new_layer_lands_above_the_active_one_and_takes_over() {
        let mut p = Page::new(100, 100);
        let bottom = p.active_layer().id();
        let at = p.add_layer();
        assert_eq!(at, 1);
        assert_eq!(p.active_index(), 1);
        assert_eq!(
            p.layers()[0].id(),
            bottom,
            "the old layer should stay below"
        );

        // Selecting the bottom and adding again inserts in the middle, not at the top.
        assert!(p.set_active(0));
        let at = p.add_layer();
        assert_eq!(at, 1);
        assert_eq!(p.layer_count(), 3);
    }

    /// Ids are never reused, so a tile still keyed by a deleted layer can never be picked up
    /// by a new one.
    #[test]
    fn layer_ids_are_never_reused() {
        let mut p = Page::new(100, 100);
        let mut seen = vec![p.active_layer().id()];
        for _ in 0..5 {
            p.add_layer();
            seen.push(p.active_layer().id());
        }
        p.remove_layer(2);
        p.add_layer();
        seen.push(p.active_layer().id());

        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seen.len(), "an id was reused: {seen:?}");
    }

    /// Deleting must hand back the id, or the layer's tiles would be stranded on the GPU with
    /// nothing referring to them.
    #[test]
    fn removing_a_layer_reports_its_id() {
        let mut p = Page::new(100, 100);
        p.add_layer();
        let doomed = p.layers()[1].id();
        assert_eq!(p.remove_layer(1), Some(doomed));
        assert_eq!(p.layer_count(), 1);
    }

    /// The last layer cannot go: a page with nothing to paint on is a state every caller
    /// would have to special-case.
    #[test]
    fn the_last_layer_cannot_be_removed() {
        let mut p = Page::new(100, 100);
        assert_eq!(p.remove_layer(0), None);
        assert_eq!(p.layer_count(), 1);
    }

    /// Deleting must leave `active` on a layer that exists, including when the active layer is
    /// the one deleted and it was the topmost.
    #[test]
    fn removing_keeps_the_selection_valid() {
        let mut p = Page::new(100, 100);
        p.add_layer();
        p.add_layer();
        assert_eq!(p.active_index(), 2);
        p.remove_layer(2);
        assert_eq!(p.active_index(), 1, "selection should not dangle");
        assert!(p.layer(p.active_index()).is_some());

        // Deleting below the selection shifts it down with the stack.
        let mut p = Page::new(100, 100);
        p.add_layer();
        p.add_layer();
        assert!(p.set_active(2));
        p.remove_layer(0);
        assert_eq!(p.active_index(), 1);
    }

    /// Reordering has to carry the selection with the layer that moved, or the artist keeps
    /// drawing on a different layer than the one they dragged.
    #[test]
    fn reordering_follows_the_active_layer() {
        let mut p = Page::new(100, 100);
        p.add_layer();
        p.add_layer();
        let ids: Vec<u32> = p.layers().iter().map(Layer::id).collect();

        assert!(p.set_active(0));
        assert_eq!(p.move_layer(0, 2), Some(2));
        assert_eq!(p.active_index(), 2, "selection did not follow");
        assert_eq!(p.layers()[2].id(), ids[0]);
        assert_eq!(p.layers()[0].id(), ids[1], "the rest should shift down");
    }

    /// Moving a layer past the selection has to shift the selection the other way, or it ends
    /// up pointing at a different layer than before the move.
    #[test]
    fn reordering_around_the_selection_keeps_pointing_at_the_same_layer() {
        let mut p = Page::new(100, 100);
        p.add_layer();
        p.add_layer();
        assert!(p.set_active(1));
        let selected = p.active_layer().id();

        // Move the bottom layer to the top: the selection slides down one.
        p.move_layer(0, 2);
        assert_eq!(p.active_layer().id(), selected, "selection changed layer");

        // And back the other way.
        p.move_layer(2, 0);
        assert_eq!(p.active_layer().id(), selected, "selection changed layer");
    }

    #[test]
    fn out_of_range_operations_are_refused_rather_than_panicking() {
        let mut p = Page::new(100, 100);
        assert!(!p.set_active(7));
        assert_eq!(p.active_index(), 0);
        assert_eq!(p.remove_layer(7), None);
        assert_eq!(p.move_layer(7, 0), None);
        assert!(p.layer(7).is_none());
    }

    /// Layers have no geometry of their own, so a resize is still just the page's rectangle
    /// moving -- no per-layer bookkeeping to fall out of step.
    #[test]
    fn resizing_does_not_disturb_the_stack() {
        let mut p = Page::new(100, 100);
        p.add_layer();
        let before: Vec<u32> = p.layers().iter().map(Layer::id).collect();
        let shift = p.resize(PageRect::new(-50, -20, 300, 300));
        assert_eq!(shift, (-50, -20));
        assert_eq!(p.rect(), PageRect::new(-50, -20, 300, 300));
        let after: Vec<u32> = p.layers().iter().map(Layer::id).collect();
        assert_eq!(before, after);
        assert_eq!(p.active_index(), 1);
    }
}
