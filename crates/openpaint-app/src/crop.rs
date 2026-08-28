//! The crop tool: a rectangle on the canvas with draggable edges and corners.
//!
//! Behaves like the crop in Windows Photos or PowerPoint — grab an edge or a corner and
//! drag, or grab the middle to move the whole rectangle. Applying it resizes the page to
//! that rectangle.
//!
//! # Dragging outward extends the page
//!
//! The rectangle is deliberately **not** clamped to the current page, so dragging an
//! edge outward grows the page instead of cropping it. That is Photoshop's behaviour
//! too, and it costs nothing here: `Page::resize` takes a rectangle and does not care
//! whether it is larger or smaller. One tool covers crop, extend, and reframe.
//!
//! # Why this is not built from egui widgets
//!
//! Pen input bypasses winit and never reaches egui (OPEN_QUESTIONS Q14), so handles made
//! of egui widgets would be mouse-only. Everything here works in **page coordinates**
//! and is driven from the app's own input path, which sees pen and mouse alike. egui is
//! used only to *paint* the outline.
//!
//! Geometry lives here, apart from any GPU or UI type, so the fiddly part — hit-testing
//! and the eight drag behaviours — is testable directly.

use openpaint_core::PageRect;

/// What a press grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
    /// The interior: moves the whole rectangle.
    Inside,
}

impl Handle {
    /// The eight edge and corner handles, in the order they are drawn.
    pub const EDGES_AND_CORNERS: [Self; 8] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// Where this handle sits on a rectangle, in page coordinates.
    #[must_use]
    pub fn position(self, rect: PageRect) -> (f32, f32) {
        let (x0, y0) = (rect.x as f32, rect.y as f32);
        let (x1, y1) = {
            let (ex, ey) = rect.end();
            (ex as f32, ey as f32)
        };
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        match self {
            Self::TopLeft => (x0, y0),
            Self::Top => (cx, y0),
            Self::TopRight => (x1, y0),
            Self::Left => (x0, cy),
            Self::Right => (x1, cy),
            Self::BottomLeft => (x0, y1),
            Self::Bottom => (cx, y1),
            Self::BottomRight => (x1, y1),
            Self::Inside => (cx, cy),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    handle: Handle,
    /// Where the press landed, in page coordinates.
    grab: (f32, f32),
    /// The rectangle as it was when the press landed, so the drag is always computed
    /// from the original rather than accumulating rounding error frame by frame.
    start: PageRect,
}

pub struct Crop {
    rect: PageRect,
    drag: Option<Drag>,
}

impl Crop {
    /// Start the tool covering `page`.
    #[must_use]
    pub fn new(page: PageRect) -> Self {
        Self {
            rect: page,
            drag: None,
        }
    }

    #[must_use]
    pub fn rect(&self) -> PageRect {
        self.rect
    }

    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// What a point would grab, or `None` if it is outside the rectangle entirely.
    ///
    /// `tolerance` is in **page** units, so the caller converts a screen-pixel grab
    /// radius by dividing by the zoom — which keeps handles the same physical size to
    /// aim at however far you are zoomed in.
    ///
    /// Corners win over edges, because at a corner both are within tolerance and the
    /// corner is the more specific intent.
    #[must_use]
    pub fn hit_test(&self, p: (f32, f32), tolerance: f32) -> Option<Handle> {
        let (x0, y0) = (self.rect.x as f32, self.rect.y as f32);
        let (ex, ey) = self.rect.end();
        let (x1, y1) = (ex as f32, ey as f32);

        // Tolerance is capped per axis at a quarter of that side. Without the cap, a
        // small rectangle -- or a view zoomed far out, which is the same thing in page
        // units -- makes the tolerance wider than the rectangle itself: every press then
        // reads as a corner, and neither the edges nor the interior can ever be grabbed.
        let cap = |extent: f32| tolerance.clamp(0.5, (extent * 0.25).max(0.5));
        let tx = cap(x1 - x0);
        let ty = cap(y1 - y0);

        let near_left = (p.0 - x0).abs() <= tx;
        let near_right = (p.0 - x1).abs() <= tx;
        let near_top = (p.1 - y0).abs() <= ty;
        let near_bottom = (p.1 - y1).abs() <= ty;
        let within_x = p.0 >= x0 - tx && p.0 <= x1 + tx;
        let within_y = p.1 >= y0 - ty && p.1 <= y1 + ty;

        // Corners first.
        if near_left && near_top {
            return Some(Handle::TopLeft);
        }
        if near_right && near_top {
            return Some(Handle::TopRight);
        }
        if near_left && near_bottom {
            return Some(Handle::BottomLeft);
        }
        if near_right && near_bottom {
            return Some(Handle::BottomRight);
        }
        // Then edges, which need to be within the perpendicular span.
        if near_left && within_y {
            return Some(Handle::Left);
        }
        if near_right && within_y {
            return Some(Handle::Right);
        }
        if near_top && within_x {
            return Some(Handle::Top);
        }
        if near_bottom && within_x {
            return Some(Handle::Bottom);
        }
        // Finally the interior.
        if p.0 > x0 && p.0 < x1 && p.1 > y0 && p.1 < y1 {
            return Some(Handle::Inside);
        }
        None
    }

    /// Begin a drag if `p` grabbed something. Returns whether it did.
    pub fn press(&mut self, p: (f32, f32), tolerance: f32) -> bool {
        match self.hit_test(p, tolerance) {
            Some(handle) => {
                self.drag = Some(Drag {
                    handle,
                    grab: p,
                    start: self.rect,
                });
                true
            }
            None => false,
        }
    }

    /// Update the rectangle for a drag in progress. No-op if nothing is being dragged.
    pub fn drag_to(&mut self, p: (f32, f32)) {
        let Some(d) = self.drag else {
            return;
        };
        let dx = (p.0 - d.grab.0).round() as i32;
        let dy = (p.1 - d.grab.1).round() as i32;

        let (mut x0, mut y0) = (d.start.x, d.start.y);
        let (mut x1, mut y1) = d.start.end();

        match d.handle {
            Handle::Inside => {
                x0 += dx;
                x1 += dx;
                y0 += dy;
                y1 += dy;
            }
            Handle::Left => x0 += dx,
            Handle::Right => x1 += dx,
            Handle::Top => y0 += dy,
            Handle::Bottom => y1 += dy,
            Handle::TopLeft => {
                x0 += dx;
                y0 += dy;
            }
            Handle::TopRight => {
                x1 += dx;
                y0 += dy;
            }
            Handle::BottomLeft => {
                x0 += dx;
                y1 += dy;
            }
            Handle::BottomRight => {
                x1 += dx;
                y1 += dy;
            }
        }

        // Never let an edge cross its opposite. The edge being dragged is the one that
        // gives way, so dragging past the far side parks against it rather than flipping
        // the rectangle inside out.
        match d.handle {
            Handle::Left | Handle::TopLeft | Handle::BottomLeft => x0 = x0.min(x1 - 1),
            Handle::Right | Handle::TopRight | Handle::BottomRight => x1 = x1.max(x0 + 1),
            _ => {}
        }
        match d.handle {
            Handle::Top | Handle::TopLeft | Handle::TopRight => y0 = y0.min(y1 - 1),
            Handle::Bottom | Handle::BottomLeft | Handle::BottomRight => y1 = y1.max(y0 + 1),
            _ => {}
        }

        self.rect = PageRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
    }

    /// End any drag in progress.
    pub fn release(&mut self) {
        self.drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100x100 rectangle at the origin, for readable expectations.
    fn crop() -> Crop {
        Crop::new(PageRect::new(0, 0, 100, 100))
    }

    #[test]
    fn every_handle_is_reachable_at_its_own_position() {
        let c = crop();
        for handle in Handle::EDGES_AND_CORNERS {
            let p = handle.position(c.rect());
            assert_eq!(
                c.hit_test(p, 4.0),
                Some(handle),
                "{handle:?} was not hit at its own position"
            );
        }
    }

    /// At a corner both edges are within tolerance, and the corner is the more specific
    /// intent -- otherwise corners would be impossible to grab.
    #[test]
    fn corners_win_over_edges() {
        let c = crop();
        assert_eq!(c.hit_test((1.0, 1.0), 4.0), Some(Handle::TopLeft));
        assert_eq!(c.hit_test((99.0, 1.0), 4.0), Some(Handle::TopRight));
    }

    #[test]
    fn the_interior_moves_the_whole_rectangle() {
        let c = crop();
        assert_eq!(c.hit_test((50.0, 50.0), 4.0), Some(Handle::Inside));
    }

    #[test]
    fn a_point_well_outside_grabs_nothing() {
        let c = crop();
        assert_eq!(c.hit_test((-40.0, -40.0), 4.0), None);
        assert_eq!(c.hit_test((500.0, 50.0), 4.0), None);
    }

    /// A press that grabbed nothing must not start a drag, or clicking away from the
    /// rectangle would move it.
    #[test]
    fn pressing_outside_does_not_start_a_drag() {
        let mut c = crop();
        assert!(!c.press((-40.0, -40.0), 4.0));
        assert!(!c.is_dragging());
        c.drag_to((0.0, 0.0));
        assert_eq!(c.rect(), PageRect::new(0, 0, 100, 100), "rectangle moved");
    }

    #[test]
    fn dragging_the_right_edge_changes_only_the_width() {
        let mut c = crop();
        assert!(c.press((100.0, 50.0), 4.0));
        c.drag_to((160.0, 50.0));
        assert_eq!(c.rect(), PageRect::new(0, 0, 160, 100));
    }

    #[test]
    fn dragging_the_left_edge_moves_the_origin() {
        let mut c = crop();
        assert!(c.press((0.0, 50.0), 4.0));
        c.drag_to((30.0, 50.0));
        assert_eq!(c.rect(), PageRect::new(30, 0, 70, 100));
    }

    #[test]
    fn dragging_a_corner_moves_two_edges() {
        let mut c = crop();
        assert!(c.press((100.0, 100.0), 4.0));
        c.drag_to((70.0, 60.0));
        assert_eq!(c.rect(), PageRect::new(0, 0, 70, 60));
    }

    #[test]
    fn dragging_the_interior_translates_without_resizing() {
        let mut c = crop();
        assert!(c.press((50.0, 50.0), 4.0));
        c.drag_to((70.0, 30.0));
        assert_eq!(c.rect(), PageRect::new(20, -20, 100, 100));
    }

    /// Dragging outward is allowed on purpose: it extends the page rather than cropping
    /// it, so one tool covers both (see the module note).
    #[test]
    fn dragging_outward_grows_past_the_original() {
        let mut c = crop();
        assert!(c.press((0.0, 0.0), 4.0));
        c.drag_to((-200.0, -300.0));
        assert_eq!(c.rect(), PageRect::new(-200, -300, 300, 400));
    }

    /// Dragging an edge past its opposite must park against it, not invert the
    /// rectangle -- a negative extent would be meaningless everywhere downstream.
    #[test]
    fn an_edge_cannot_cross_its_opposite() {
        let mut c = crop();
        assert!(c.press((100.0, 50.0), 4.0));
        c.drag_to((-500.0, 50.0));
        let r = c.rect();
        assert_eq!(r.x, 0);
        assert_eq!(r.w, 1, "width collapsed past 1: {r:?}");

        let mut c = crop();
        assert!(c.press((0.0, 0.0), 4.0));
        c.drag_to((900.0, 900.0));
        let r = c.rect();
        assert_eq!((r.w, r.h), (1, 1), "corner did not park: {r:?}");
    }

    /// A drag is computed from the rectangle as it was at press time, so repeated
    /// updates during one drag are not cumulative.
    #[test]
    fn dragging_is_measured_from_the_press_not_the_last_frame() {
        let mut c = crop();
        assert!(c.press((100.0, 50.0), 4.0));
        c.drag_to((120.0, 50.0));
        c.drag_to((140.0, 50.0));
        c.drag_to((110.0, 50.0));
        assert_eq!(c.rect(), PageRect::new(0, 0, 110, 100), "drag accumulated");
    }

    #[test]
    fn releasing_ends_the_drag() {
        let mut c = crop();
        c.press((100.0, 50.0), 4.0);
        c.release();
        assert!(!c.is_dragging());
        c.drag_to((300.0, 50.0));
        assert_eq!(c.rect().w, 100, "still dragging after release");
    }

    /// Zoomed far out, the caller's screen-pixel grab radius becomes enormous in page
    /// units. The interior and the edges must still be reachable, or the tool would be
    /// unusable at exactly the zoom where you most want to reframe a whole page.
    #[test]
    fn a_huge_tolerance_does_not_swallow_the_whole_rectangle() {
        let c = crop();
        // 10 screen px at 5% zoom.
        let tolerance = 200.0;
        assert_eq!(c.hit_test((50.0, 50.0), tolerance), Some(Handle::Inside));
        assert_eq!(c.hit_test((50.0, 0.0), tolerance), Some(Handle::Top));
        assert_eq!(c.hit_test((100.0, 50.0), tolerance), Some(Handle::Right));
        assert_eq!(c.hit_test((0.0, 0.0), tolerance), Some(Handle::TopLeft));
    }

    /// Handles must be grabbable on a page whose origin is negative, which is what a
    /// page looks like after extending upward or leftward.
    #[test]
    fn handles_work_on_a_negative_origin_rectangle() {
        let c = Crop::new(PageRect::new(-400, -500, 900, 1200));
        for handle in Handle::EDGES_AND_CORNERS {
            let p = handle.position(c.rect());
            assert_eq!(c.hit_test(p, 4.0), Some(handle), "{handle:?}");
        }
    }
}
