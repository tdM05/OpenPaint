//! The box a free transform draws, and what dragging its handles means.
//!
//! Every raster app with a transform draws the same thing — a rectangle around the content with
//! eight handles, a rotation zone just outside the corners, and an interior that moves. That shape
//! is not decoration: it is *how a transform is edited*. Without it a transform can only be typed
//! into a panel, which is the difference between adjusting a drawing and filling in a form.
//!
//! # Why the hit test runs in source space
//!
//! The box is the content rectangle *transformed*, so on screen it is a rotated, scaled
//! parallelogram, and hit-testing that directly means testing a point against eight rotated
//! rectangles. Instead the pointer is mapped **backwards** through the transform into the
//! untransformed rectangle's own space, where the test is the same axis-aligned one
//! [`crate::crop`] does. Rotation then costs nothing at all, and there is one piece of geometry
//! rather than two that have to agree.
//!
//! The grab tolerance is divided by the scale on the way in, so a handle stays the same size to
//! aim at whatever the content has been scaled to.
//!
//! # Why a drag is a pure function of where it started
//!
//! [`drag`] takes the transform as it was *at the press* and returns a whole new one. Nothing
//! accumulates: dragging out and back returns the exact transform you began with, and a drag
//! cannot creep by a fraction of a pixel per frame the way an incremental one does. Same reason
//! [`crate::crop::Crop`] keeps its `start`.
//!
//! # Why scaling anchors the opposite corner rather than moving the pivot
//!
//! Dragging the bottom-right corner has to hold the top-left one still — that is what every app
//! does and what the gesture means. The tempting way is to move [`Transform::pivot`] to the
//! anchor, but the pivot is also what *rotation* turns about, and rotation has to stay about the
//! centre of the box. So the pivot never moves and the anchor is held by solving for the
//! translation instead: one line of algebra rather than a second meaning for a field.

use openpaint_core::{Transform, MIN_SCALE};

/// How far outside the box the rotation ring reaches, as a multiple of the grab tolerance.
///
/// Generous, because a rotation zone that is hard to find reads as a broken tool.
const ROTATE_BAND: f32 = 3.0;

/// What a press on the transform box grabbed.
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
    /// The interior: moves the content.
    Inside,
    /// Just outside the box: turns it about its centre.
    Rotate,
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

    /// Whether this handle moves in both axes at once, which is what "lock the aspect ratio" is
    /// about. An edge handle drives one axis and has nothing to lock it to.
    #[must_use]
    pub fn is_corner(self) -> bool {
        matches!(
            self,
            Self::TopLeft | Self::TopRight | Self::BottomLeft | Self::BottomRight
        )
    }

    /// The handle diagonally across the box, which a drag holds still.
    ///
    /// An edge handle's opposite is the facing edge, so dragging the right edge holds the left one.
    fn opposite(self) -> Self {
        match self {
            Self::TopLeft => Self::BottomRight,
            Self::Top => Self::Bottom,
            Self::TopRight => Self::BottomLeft,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::BottomLeft => Self::TopRight,
            Self::Bottom => Self::Top,
            Self::BottomRight => Self::TopLeft,
            other => other,
        }
    }

    /// Where this handle sits on the untransformed rectangle.
    fn on(self, rect: Rect) -> (f32, f32) {
        let (cx, cy) = rect.centre();
        match self {
            Self::TopLeft => (rect.x0, rect.y0),
            Self::Top => (cx, rect.y0),
            Self::TopRight => (rect.x1, rect.y0),
            Self::Left => (rect.x0, cy),
            Self::Right => (rect.x1, cy),
            Self::BottomLeft => (rect.x0, rect.y1),
            Self::Bottom => (cx, rect.y1),
            Self::BottomRight => (rect.x1, rect.y1),
            Self::Inside | Self::Rotate => (cx, cy),
        }
    }
}

/// The content rectangle the box is drawn around, in page pixels, before any transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// From the tight bounds of what was lifted.
    ///
    /// Tight, not tile-aligned: see [`openpaint_core::Selection::content_bounds`], which exists
    /// for this. A box snapped out to whole tiles would hang off the artwork and put the pivot
    /// somewhere the artist never selected.
    #[must_use]
    pub fn from_bounds((x0, y0, x1, y1): (i32, i32, i32, i32)) -> Self {
        Self {
            x0: x0 as f32,
            y0: y0 as f32,
            x1: x1 as f32,
            y1: y1 as f32,
        }
    }

    /// The centre, which is where a transform's pivot belongs.
    #[must_use]
    pub fn centre(&self) -> (f32, f32) {
        ((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }

    /// The four corners after `transform`, clockwise from the top left.
    ///
    /// Clockwise *in source space*: a flip turns that into anti-clockwise on screen, which is
    /// correct — the box is showing the content, and the content is flipped.
    #[must_use]
    pub fn corners(&self, transform: &Transform) -> [(f32, f32); 4] {
        [
            (self.x0, self.y0),
            (self.x1, self.y0),
            (self.x1, self.y1),
            (self.x0, self.y1),
        ]
        .map(|(x, y)| transform.apply(x, y))
    }

    /// Where the eight draggable handles are after `transform`, in [`Handle::EDGES_AND_CORNERS`]
    /// order.
    #[must_use]
    pub fn handles(&self, transform: &Transform) -> [(f32, f32); 8] {
        Handle::EDGES_AND_CORNERS.map(|h| {
            let (x, y) = h.on(*self);
            transform.apply(x, y)
        })
    }

    /// What a press at `p` (page pixels) grabbed, or `None` if it landed clear of the box.
    ///
    /// `tolerance` is in page units at the current zoom, exactly as [`crate::crop::Crop::hit_test`]
    /// takes it, so the caller divides a screen-pixel grab radius by the view scale once and both
    /// tools stay the same size to aim at.
    ///
    /// Corners beat edges, because at a corner both are in range and the corner is the more
    /// specific intent — the same order the crop tool settled on.
    #[must_use]
    pub fn hit_test(&self, p: (f32, f32), transform: &Transform, tolerance: f32) -> Option<Handle> {
        let (sx, sy) = transform.effective_scale();
        let (q0, q1) = transform.invert(p.0, p.1);

        // Back in source space, a page-space distance has been divided by the scale, so the
        // tolerance has to be too — otherwise a selection scaled to 400% would grow its grab zones
        // with it and swallow the interior.
        let (w, h) = (self.x1 - self.x0, self.y1 - self.y0);
        let cap = |t: f32, extent: f32| t.clamp(0.5, (extent * 0.25).max(0.5));
        let tx = cap(tolerance / sx.abs(), w);
        let ty = cap(tolerance / sy.abs(), h);

        let near_left = (q0 - self.x0).abs() <= tx;
        let near_right = (q0 - self.x1).abs() <= tx;
        let near_top = (q1 - self.y0).abs() <= ty;
        let near_bottom = (q1 - self.y1).abs() <= ty;
        let within_x = q0 >= self.x0 - tx && q0 <= self.x1 + tx;
        let within_y = q1 >= self.y0 - ty && q1 <= self.y1 + ty;

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
        if q0 > self.x0 && q0 < self.x1 && q1 > self.y0 && q1 < self.y1 {
            return Some(Handle::Inside);
        }
        // Outside the box but still near it: the rotation ring.
        let (band_x, band_y) = (tx * ROTATE_BAND, ty * ROTATE_BAND);
        if q0 >= self.x0 - band_x
            && q0 <= self.x1 + band_x
            && q1 >= self.y0 - band_y
            && q1 <= self.y1 + band_y
        {
            return Some(Handle::Rotate);
        }
        None
    }
}

/// The transform produced by dragging `handle` from `grab` to `to`, both in page pixels.
///
/// A pure function of the transform as it was at the press, so a drag never accumulates: that is
/// what makes dragging out and back land exactly where it began.
///
/// `lock_aspect` is honoured on corners only — an edge handle drives one axis, and there is nothing
/// to lock it to.
#[must_use]
pub fn drag(
    rect: Rect,
    handle: Handle,
    start: &Transform,
    grab: (f32, f32),
    to: (f32, f32),
    lock_aspect: bool,
) -> Transform {
    match handle {
        // Whole pixels, so a move still takes the lossless copy path (§5d). Rounding the *total*
        // rather than each step is why a slow drag cannot creep.
        Handle::Inside => Transform {
            offset: (
                start.offset.0 + (to.0 - grab.0).round(),
                start.offset.1 + (to.1 - grab.1).round(),
            ),
            ..*start
        },
        Handle::Rotate => {
            // The box turns about its own centre, which is `apply(pivot)` — the pivot is the source
            // centre, and everything after it in `apply` is the translation.
            let centre = (
                start.pivot.0 + start.offset.0,
                start.pivot.1 + start.offset.1,
            );
            let angle_to = |q: (f32, f32)| (q.1 - centre.1).atan2(q.0 - centre.0);
            Transform {
                rotation: start.rotation + angle_to(to) - angle_to(grab),
                ..*start
            }
        }
        _ => scale(rect, handle, start, to, lock_aspect),
    }
}

/// Drag a scale handle, holding the opposite one still.
///
/// Solved rather than approximated. Writing `apply` out for the anchor and for the dragged handle
/// and subtracting leaves the handle's offset from the anchor as `R · S · (h − a)`, where `R` and
/// `S` are the rotation and the scale and `h − a` is a vector in the untransformed rectangle.
/// Undoing the rotation on the pointer therefore reads the scale straight off:
///
/// ```text
/// d = R⁻¹ · (pointer − anchor)      scale = (d.x / (h−a).x, d.y / (h−a).y)
/// ```
///
/// An edge handle has one component of `h − a` equal to zero, which is exactly the axis it must not
/// touch, so one expression covers all eight without a special case.
fn scale(
    rect: Rect,
    handle: Handle,
    start: &Transform,
    to: (f32, f32),
    lock_aspect: bool,
) -> Transform {
    let anchor_src = handle.opposite().on(rect);
    let handle_src = handle.on(rect);
    let anchor_page = start.apply(anchor_src.0, anchor_src.1);

    let (sinr, cosr) = start.rotation.sin_cos();
    let (vx, vy) = (to.0 - anchor_page.0, to.1 - anchor_page.1);
    let d = (vx.mul_add(cosr, vy * sinr), (-vx).mul_add(sinr, vy * cosr));
    let e = (handle_src.0 - anchor_src.0, handle_src.1 - anchor_src.1);

    let (sx0, sy0) = start.effective_scale();
    let mut scale = (
        if e.0.abs() > f32::EPSILON {
            d.0 / e.0
        } else {
            sx0
        },
        if e.1.abs() > f32::EPSILON {
            d.1 / e.1
        } else {
            sy0
        },
    );

    if lock_aspect && handle.is_corner() {
        // One factor `k` applied to the scale it started at, chosen so the handle lands as close to
        // the pointer as it can while staying on the diagonal. That is a least-squares projection
        // of the drag onto the direction the corner is allowed to travel — the same answer as
        // "follow the diagonal", written so a drag perpendicular to it degrades gracefully instead
        // of picking one axis and ignoring the other.
        let v = (e.0 * sx0, e.1 * sy0);
        let denom = v.0.mul_add(v.0, v.1 * v.1);
        if denom > f32::EPSILON {
            let k = d.0.mul_add(v.0, d.1 * v.1) / denom;
            scale = (sx0 * k, sy0 * k);
        }
    }

    // Floored away from zero, keeping the sign: dragging a handle past its anchor flips the
    // content, which is what every app does and is worth keeping, but a scale of exactly zero has
    // no inverse and both the resampler and the hit test need one.
    let floor = |s: f32| {
        if s.abs() < MIN_SCALE {
            MIN_SCALE.copysign(if s == 0.0 { 1.0 } else { s })
        } else {
            s
        }
    };
    let scale = (floor(scale.0), floor(scale.1));

    // Hold the anchor where it was. `apply(anchor) = pivot + R·S·(anchor − pivot) + offset`, so the
    // offset that pins it is that equation rearranged. The pivot is left alone on purpose — it is
    // what rotation turns about, and rotation stays about the centre of the box.
    let (ax, ay) = (anchor_src.0 - start.pivot.0, anchor_src.1 - start.pivot.1);
    let (px, py) = (ax * scale.0, ay * scale.1);
    let placed = (
        start.pivot.0 + px.mul_add(cosr, -(py * sinr)),
        start.pivot.1 + px.mul_add(sinr, py * cosr),
    );
    Transform {
        scale,
        offset: (anchor_page.0 - placed.0, anchor_page.1 - placed.1),
        ..*start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100x100 box at the origin, pivoted at its centre the way a session builds one.
    fn box_and_transform() -> (Rect, Transform) {
        let rect = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        };
        let transform = Transform {
            pivot: rect.centre(),
            ..Transform::IDENTITY
        };
        (rect, transform)
    }

    fn close(a: f32, b: f32, what: &str) {
        assert!((a - b).abs() < 0.01, "{what}: expected {b}, got {a}");
    }

    /// The interior moves and the corners scale — the two things a press has to tell apart before
    /// anything else can be right.
    #[test]
    fn a_press_tells_the_interior_from_a_corner_from_the_ring() {
        let (rect, t) = box_and_transform();
        assert_eq!(rect.hit_test((50.0, 50.0), &t, 6.0), Some(Handle::Inside));
        assert_eq!(rect.hit_test((0.0, 0.0), &t, 6.0), Some(Handle::TopLeft));
        assert_eq!(rect.hit_test((100.0, 50.0), &t, 6.0), Some(Handle::Right));
        assert_eq!(rect.hit_test((50.0, 0.0), &t, 6.0), Some(Handle::Top));
        // Clear of the box but still near it: the rotation ring.
        assert_eq!(rect.hit_test((-14.0, -14.0), &t, 6.0), Some(Handle::Rotate));
        // Far away: nothing at all, so a stray press cannot fling the artwork.
        assert_eq!(rect.hit_test((-400.0, -400.0), &t, 6.0), None);
    }

    /// Hit-testing follows the box through a rotation, which is the whole reason the test runs in
    /// source space.
    ///
    /// A quarter turn sends the top-left corner to the top-right of the screen. A test written
    /// against the *screen* rectangle would report `TopLeft` there and scale the wrong corner —
    /// this is the case that separates "inverse-mapped" from "axis-aligned and hoping".
    #[test]
    fn a_rotated_box_is_still_grabbed_by_its_own_corners() {
        let (rect, mut t) = box_and_transform();
        t.rotation = std::f32::consts::FRAC_PI_2;

        // (0,0) rotated a quarter turn about (50,50) lands at (100, 0).
        let moved = t.apply(0.0, 0.0);
        close(moved.0, 100.0, "corner x");
        close(moved.1, 0.0, "corner y");
        assert_eq!(
            rect.hit_test(moved, &t, 6.0),
            Some(Handle::TopLeft),
            "the corner is the one it came from, not the one it now looks like"
        );
    }

    /// Dragging a corner holds the opposite one exactly still. That is what the gesture means, and
    /// getting it wrong is immediately visible as the artwork sliding away under the pointer.
    #[test]
    fn scaling_a_corner_pins_the_opposite_corner() {
        let (rect, t) = box_and_transform();
        // Drag the bottom-right corner out to double the box.
        let after = drag(
            rect,
            Handle::BottomRight,
            &t,
            (100.0, 100.0),
            (200.0, 200.0),
            false,
        );
        close(after.scale.0, 2.0, "scale x");
        close(after.scale.1, 2.0, "scale y");

        let anchor = after.apply(0.0, 0.0);
        close(anchor.0, 0.0, "anchor x");
        close(anchor.1, 0.0, "anchor y");

        let dragged = after.apply(100.0, 100.0);
        close(dragged.0, 200.0, "the dragged corner follows the pointer");
        close(dragged.1, 200.0, "the dragged corner follows the pointer");
    }

    /// An edge handle drives one axis and leaves the other alone. Without the zero-component check
    /// in `scale` this divides by zero and the other axis becomes NaN, which silently destroys the
    /// transform rather than merely looking wrong.
    #[test]
    fn an_edge_handle_scales_one_axis_only() {
        let (rect, t) = box_and_transform();
        let after = drag(rect, Handle::Right, &t, (100.0, 50.0), (150.0, 90.0), false);
        close(after.scale.0, 1.5, "the axis the edge drives");
        close(after.scale.1, 1.0, "the axis it must not touch");
        let anchor = after.apply(0.0, 50.0);
        close(anchor.0, 0.0, "the facing edge stays put");
    }

    /// The lock keeps the two axes equal, and does it by projecting the drag onto the diagonal
    /// rather than by copying one axis onto the other — so a drag that is mostly sideways still
    /// grows the box, instead of the y axis alone deciding.
    #[test]
    fn locking_the_aspect_keeps_the_axes_equal() {
        let (rect, t) = box_and_transform();
        let after = drag(
            rect,
            Handle::BottomRight,
            &t,
            (100.0, 100.0),
            (200.0, 120.0),
            true,
        );
        close(after.scale.0, after.scale.1, "the axes stay equal");
        assert!(
            after.scale.0 > 1.0,
            "a drag outward grows the box, got {}",
            after.scale.0
        );
    }

    /// Scaling under rotation still pins the anchor. This is the case that fails if the pointer is
    /// not un-rotated before the scale is read off it: the box shears away instead of scaling.
    #[test]
    fn scaling_a_rotated_box_still_pins_its_anchor() {
        let (rect, mut t) = box_and_transform();
        t.rotation = 0.6;
        let anchor_before = t.apply(0.0, 0.0);

        // Wherever the pointer goes, the anchor must not move.
        let to = t.apply(180.0, 220.0);
        let after = drag(
            rect,
            Handle::BottomRight,
            &t,
            t.apply(100.0, 100.0),
            to,
            false,
        );
        let anchor_after = after.apply(0.0, 0.0);
        close(anchor_after.0, anchor_before.0, "anchor x under rotation");
        close(anchor_after.1, anchor_before.1, "anchor y under rotation");

        let dragged = after.apply(100.0, 100.0);
        close(
            dragged.0,
            to.0,
            "the dragged corner still reaches the pointer",
        );
        close(
            dragged.1,
            to.1,
            "the dragged corner still reaches the pointer",
        );
    }

    /// Rotation is measured about the box's centre, not the page origin, and the drag is a
    /// *difference* of angles — so grabbing the ring anywhere turns by how far the pointer went
    /// round rather than snapping the box to the pointer.
    #[test]
    fn the_ring_turns_the_box_about_its_own_centre() {
        let (rect, t) = box_and_transform();
        // From due east of the centre to due south of it: a quarter turn clockwise on screen.
        let after = drag(
            rect,
            Handle::Rotate,
            &t,
            (150.0, 50.0),
            (50.0, 150.0),
            false,
        );
        close(
            after.rotation,
            std::f32::consts::FRAC_PI_2,
            "a quarter turn",
        );
        let centre = after.apply(50.0, 50.0);
        close(centre.0, 50.0, "the centre does not move");
        close(centre.1, 50.0, "the centre does not move");
    }

    /// A drag is a pure function of where it started, so going out and coming back is exact.
    ///
    /// Pins the property the `start` parameter exists for. An incremental implementation passes
    /// every other test here and fails this one, drifting a little on every frame of a long drag.
    #[test]
    fn a_drag_out_and_back_returns_the_transform_it_started_from() {
        let (rect, t) = box_and_transform();
        let out = drag(
            rect,
            Handle::BottomRight,
            &t,
            (100.0, 100.0),
            (400.0, 260.0),
            false,
        );
        assert_ne!(
            out.scale, t.scale,
            "the detour has to actually do something"
        );
        let back = drag(
            rect,
            Handle::BottomRight,
            &t,
            (100.0, 100.0),
            (100.0, 100.0),
            false,
        );
        close(back.scale.0, 1.0, "scale x came back");
        close(back.scale.1, 1.0, "scale y came back");
        close(back.offset.0, 0.0, "offset x came back");
        close(back.offset.1, 0.0, "offset y came back");
    }

    /// Moving stays on whole pixels, because a whole-pixel move is the one transform that does not
    /// resample (§5d). A fractional offset here would quietly make every drag lossy.
    #[test]
    fn moving_lands_on_whole_pixels() {
        let (rect, t) = box_and_transform();
        let after = drag(rect, Handle::Inside, &t, (10.0, 10.0), (43.4, 77.6), false);
        assert_eq!(after.offset, (33.0, 68.0));
        assert!(after.is_a_plain_move(), "a move must not need resampling");
    }

    /// Dragging a handle past its anchor flips the content rather than collapsing it, and the
    /// scale never reaches zero — which would leave the transform with no inverse and take the hit
    /// test down with it.
    #[test]
    fn dragging_past_the_anchor_flips_instead_of_collapsing() {
        let (rect, t) = box_and_transform();
        let after = drag(
            rect,
            Handle::BottomRight,
            &t,
            (100.0, 100.0),
            (-50.0, -50.0),
            false,
        );
        assert!(after.scale.0 < 0.0, "past the anchor is a flip");
        let collapsed = drag(
            rect,
            Handle::BottomRight,
            &t,
            (100.0, 100.0),
            (0.0, 0.0),
            false,
        );
        assert!(
            collapsed.scale.0.abs() >= MIN_SCALE,
            "a scale of zero has no inverse"
        );
    }

    /// Handles stay the same size to aim at however far the content has been scaled.
    ///
    /// Without dividing the tolerance by the scale, a selection scaled up 20x has grab zones 20x
    /// wider in page units — which at that size covers the whole box, so the interior can never be
    /// grabbed and the thing simply cannot be moved.
    #[test]
    fn the_grab_zone_does_not_grow_with_the_content() {
        let (rect, mut t) = box_and_transform();
        t.scale = (20.0, 20.0);
        let centre = t.apply(rect.centre().0, rect.centre().1);
        assert_eq!(
            rect.hit_test(centre, &t, 6.0),
            Some(Handle::Inside),
            "the middle of a scaled-up box is still its interior"
        );
        // Six screen pixels in from the scaled corner is still well outside the handle.
        let corner = t.apply(0.0, 0.0);
        assert_eq!(
            rect.hit_test((corner.0 + 40.0, corner.1 + 40.0), &t, 6.0),
            Some(Handle::Inside)
        );
    }
}
