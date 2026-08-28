//! The camera onto the canvas — pan, zoom, rotate, and the screen↔canvas transform.
//!
//! This is *view* state, deliberately separate from the document and from the GPU.
//! Two things must agree exactly or strokes land where the user didn't draw:
//!
//! 1. where the canvas quad is drawn ([`View::placement`]), and
//! 2. where an input sample maps to in canvas space ([`View::screen_to_canvas`]).
//!
//! They are the forward and inverse of one transform, defined once here, which is
//! what guarantees they cannot drift.
//!
//! # The transform
//!
//! ```text
//! screen = content_center + rotate(theta) * (canvas - center) * scale
//! canvas = center + rotate(-theta) * (screen - content_center) / scale
//! ```
//!
//! `center` is the canvas point shown at the middle of the visible area, `scale`
//! is screen pixels per canvas pixel, and `theta` is the canvas rotation.
//!
//! Rotation is included from the start even though only a keyboard binding drives
//! it so far. It is what forces the drawn quad to be four independent corners
//! rather than an axis-aligned rectangle — retrofitting that later would mean
//! rewriting the placement uniform, the shader, and both directions of this
//! transform. Building it once is cheaper than building it twice.

use openpaint_core::Canvas;

/// Fraction of the visible area the canvas is fitted into, so the sheet has a
/// little breathing room rather than touching the window edge.
const FIT_MARGIN: f32 = 0.94;

/// Zoom limits. The lower bound keeps a huge canvas reachable; the upper bound is
/// where individual pixels are comfortably inspectable.
const MIN_SCALE: f32 = 0.02;
const MAX_SCALE: f32 = 32.0;

/// Multiplier per wheel notch. ~1.1 feels controllable rather than jumpy.
const ZOOM_PER_NOTCH: f32 = 1.1;

/// Rotation step for keyboard rotate, in radians (15°, matching common art-app
/// increments).
pub const ROTATE_STEP: f32 = std::f32::consts::FRAC_PI_4 / 3.0;

/// Where the canvas quad sits, as four corners in normalized device coordinates.
///
/// Four corners rather than a min/max rectangle because a rotated canvas is not
/// axis-aligned. Order is top-left, top-right, bottom-left, bottom-right *in
/// canvas space* — so under rotation they are no longer visually top/left, and the
/// names refer to which canvas corner each one is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub tl: [f32; 2],
    pub tr: [f32; 2],
    pub bl: [f32; 2],
    pub br: [f32; 2],
}

#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Screen pixels per canvas pixel.
    scale: f32,
    /// Canvas rotation in radians, counter-clockwise.
    rotation: f32,
    /// Canvas point displayed at the center of the visible area.
    center: (f32, f32),
    /// Physical pixels along the left edge the UI occupies, so the canvas uses the
    /// area actually visible rather than hiding under the panel.
    inset_left_px: f32,
    /// Set when the view should re-fit on the next frame. Deferred rather than
    /// applied immediately because fitting needs the surface size *and* the UI
    /// inset, which aren't both known at construction.
    needs_fit: bool,
    /// While true, anything that changes the visible area (window resize, the UI
    /// panel appearing or changing width) re-fits the canvas.
    ///
    /// This exists because the first fit necessarily runs before the UI has
    /// reported its width, so it would otherwise centre the canvas on the whole
    /// window and stay there. Manual pan/zoom/rotate clears it, so resizing never
    /// throws away a view the user has deliberately set.
    auto_fit: bool,
}

impl Default for View {
    fn default() -> Self {
        Self {
            scale: 1.0,
            rotation: 0.0,
            center: (0.0, 0.0),
            inset_left_px: 0.0,
            needs_fit: true,
            auto_fit: true,
        }
    }
}

impl View {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn scale(&self) -> f32 {
        self.scale
    }

    #[must_use]
    pub fn rotation(&self) -> f32 {
        self.rotation
    }

    /// Tell the view how much of the left edge the UI covers.
    ///
    /// Returns `true` if this queued a re-fit, in which case the caller **must**
    /// request another frame. Painting is demand-driven, so a queued fit that
    /// nobody asks to draw simply never happens -- the same trap as egui needing a
    /// frame to process its input.
    pub fn set_inset_left(&mut self, px: f32) -> bool {
        let px = px.max(0.0);
        if (px - self.inset_left_px).abs() > 0.5 {
            self.inset_left_px = px;
            if self.auto_fit {
                self.needs_fit = true;
                return true;
            }
        }
        false
    }

    /// Note that the visible area changed size, re-fitting if still auto-fitting.
    pub fn surface_resized(&mut self) {
        if self.auto_fit {
            self.needs_fit = true;
        }
    }

    /// Request a fit-to-window on the next frame, and resume auto-fitting.
    pub fn request_fit(&mut self) {
        self.needs_fit = true;
        self.auto_fit = true;
    }

    /// Apply a pending fit request, if any. Returns `true` if the view changed.
    pub fn apply_pending_fit(&mut self, surface_w: u32, surface_h: u32, canvas: &Canvas) -> bool {
        if !self.needs_fit {
            return false;
        }
        self.needs_fit = false;
        self.fit(surface_w, surface_h, canvas);
        true
    }

    /// Fit the whole canvas in the visible area, upright and centered.
    pub fn fit(&mut self, surface_w: u32, surface_h: u32, canvas: &Canvas) {
        let (_, _, area_w, area_h) = self.content_area(surface_w, surface_h);
        let cw = canvas.width().max(1) as f32;
        let ch = canvas.height().max(1) as f32;
        self.scale = ((area_w / cw).min(area_h / ch) * FIT_MARGIN).clamp(MIN_SCALE, MAX_SCALE);
        self.rotation = 0.0;
        // The page's centre in page coordinates, which is not `extent / 2` once the
        // origin is negative.
        let (ox, oy) = canvas.origin();
        self.center = (ox as f32 + cw * 0.5, oy as f32 + ch * 0.5);
    }

    /// Set zoom to an exact scale, keeping the canvas point under `anchor` fixed.
    pub fn set_scale_about(
        &mut self,
        scale: f32,
        anchor_px: (f64, f64),
        surface_w: u32,
        surface_h: u32,
    ) {
        let new_scale = scale.clamp(MIN_SCALE, MAX_SCALE);
        if (new_scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.auto_fit = false;
        // Keep the canvas point under the anchor stationary: find it before the
        // change, then move `center` so it maps back to the same screen point.
        let before = self.screen_to_canvas_unclipped(anchor_px, surface_w, surface_h);
        self.scale = new_scale;
        let after = self.screen_to_canvas_unclipped(anchor_px, surface_w, surface_h);
        self.center.0 += before.0 - after.0;
        self.center.1 += before.1 - after.1;
    }

    /// Zoom by wheel notches (positive zooms in), anchored at the cursor.
    pub fn zoom_by_notches(
        &mut self,
        notches: f32,
        anchor_px: (f64, f64),
        surface_w: u32,
        surface_h: u32,
    ) {
        let factor = ZOOM_PER_NOTCH.powf(notches);
        self.set_scale_about(self.scale * factor, anchor_px, surface_w, surface_h);
    }

    /// Pan by a screen-space delta in physical pixels.
    pub fn pan_by_screen(&mut self, dx: f32, dy: f32) {
        // Dragging right should move the canvas right, i.e. move the viewpoint
        // left, hence the negation. Rotation is undone so a drag always follows
        // the pointer rather than the canvas axes.
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.auto_fit = false;
        let (cx, cy) = rotate(-dx / self.scale, -dy / self.scale, -self.rotation);
        self.center.0 += cx;
        self.center.1 += cy;
    }

    /// Rotate by a delta in radians, keeping `anchor` fixed.
    pub fn rotate_by(
        &mut self,
        radians: f32,
        anchor_px: (f64, f64),
        surface_w: u32,
        surface_h: u32,
    ) {
        if radians == 0.0 {
            return;
        }
        self.auto_fit = false;
        let before = self.screen_to_canvas_unclipped(anchor_px, surface_w, surface_h);
        self.rotation = wrap_angle(self.rotation + radians);
        let after = self.screen_to_canvas_unclipped(anchor_px, surface_w, surface_h);
        self.center.0 += before.0 - after.0;
        self.center.1 += before.1 - after.1;
    }

    /// The visible area available to the canvas, in physical pixels, as
    /// `(left, top, width, height)`.
    fn content_area(&self, surface_w: u32, surface_h: u32) -> (f32, f32, f32, f32) {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        let left = self.inset_left_px.min(sw - 1.0);
        (left, 0.0, (sw - left).max(1.0), sh)
    }

    /// Center of the visible area, in physical pixels.
    fn content_center(&self, surface_w: u32, surface_h: u32) -> (f32, f32) {
        let (x, y, w, h) = self.content_area(surface_w, surface_h);
        (x + w * 0.5, y + h * 0.5)
    }

    /// Forward transform: canvas pixels -> physical screen pixels.
    fn canvas_to_screen(&self, cx: f32, cy: f32, surface_w: u32, surface_h: u32) -> (f32, f32) {
        let (ccx, ccy) = self.content_center(surface_w, surface_h);
        let (rx, ry) = rotate(
            (cx - self.center.0) * self.scale,
            (cy - self.center.1) * self.scale,
            self.rotation,
        );
        (ccx + rx, ccy + ry)
    }

    /// Inverse transform, without the on-canvas bounds check.
    fn screen_to_canvas_unclipped(
        &self,
        px: (f64, f64),
        surface_w: u32,
        surface_h: u32,
    ) -> (f32, f32) {
        let (ccx, ccy) = self.content_center(surface_w, surface_h);
        let (rx, ry) = rotate(px.0 as f32 - ccx, px.1 as f32 - ccy, -self.rotation);
        (
            self.center.0 + rx / self.scale,
            self.center.1 + ry / self.scale,
        )
    }

    /// Where to draw the canvas quad, in NDC.
    #[must_use]
    pub fn placement(&self, surface_w: u32, surface_h: u32, canvas: &Canvas) -> Placement {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        // Pixels -> NDC: x maps 0..sw to -1..1, y maps 0..sh to 1..-1.
        let to_ndc = |(x, y): (f32, f32)| [x / sw * 2.0 - 1.0, 1.0 - y / sh * 2.0];
        let corner = |cx: f32, cy: f32| to_ndc(self.canvas_to_screen(cx, cy, surface_w, surface_h));

        // The page's own corners, which start at its origin rather than at zero.
        let (ox, oy) = canvas.origin();
        let (ex, ey) = canvas.end();
        let (x0, y0) = (ox as f32, oy as f32);
        let (x1, y1) = (ex as f32, ey as f32);

        Placement {
            tl: corner(x0, y0),
            tr: corner(x1, y0),
            bl: corner(x0, y1),
            br: corner(x1, y1),
        }
    }

    /// Map a window position (physical pixels) to canvas pixels.
    ///
    /// Returns `None` when the point is off the canvas, so callers simply don't
    /// paint rather than clamping to an edge.
    #[must_use]
    pub fn screen_to_canvas(
        &self,
        px: f64,
        py: f64,
        surface_w: u32,
        surface_h: u32,
        canvas: &Canvas,
    ) -> Option<(f32, f32)> {
        let (cx, cy) = self.screen_to_canvas_unclipped((px, py), surface_w, surface_h);
        let (ox, oy) = canvas.origin();
        let (ex, ey) = canvas.end();
        if cx < ox as f32 || cy < oy as f32 || cx > ex as f32 || cy > ey as f32 {
            return None;
        }
        Some((cx, cy))
    }
}

/// Rotate a screen-space vector by `radians` counter-clockwise.
fn rotate(x: f32, y: f32, radians: f32) -> (f32, f32) {
    if radians == 0.0 {
        return (x, y);
    }
    let (s, c) = radians.sin_cos();
    (x * c - y * s, x * s + y * c)
}

/// Keep an angle in `-PI..=PI` so it can't drift unboundedly.
fn wrap_angle(radians: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let mut a = radians % tau;
    if a > std::f32::consts::PI {
        a -= tau;
    } else if a < -std::f32::consts::PI {
        a += tau;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    const SW: u32 = 800;
    const SH: u32 = 600;

    fn canvas() -> Canvas {
        Canvas::new(1000, 1000)
    }

    fn fitted() -> View {
        let mut v = View::new();
        v.fit(SW, SH, &canvas());
        v
    }

    /// Round-tripping a screen point through both directions must return it. This
    /// is the property that stops strokes landing away from the pen, and it must
    /// hold under every combination of pan, zoom, and rotation.
    #[test]
    fn screen_canvas_roundtrip_holds_under_all_transforms() {
        let c = canvas();
        for &(zoom, rot, pan) in &[
            (1.0_f32, 0.0_f32, (0.0_f32, 0.0_f32)),
            (0.35, 0.0, (0.0, 0.0)),
            (4.0, 0.0, (37.0, -19.0)),
            (1.0, ROTATE_STEP, (0.0, 0.0)),
            (2.5, -ROTATE_STEP * 2.0, (-80.0, 45.0)),
            (0.2, std::f32::consts::FRAC_PI_2, (10.0, 10.0)),
        ] {
            let mut v = fitted();
            v.set_scale_about(zoom, (400.0, 300.0), SW, SH);
            v.rotate_by(rot, (400.0, 300.0), SW, SH);
            v.pan_by_screen(pan.0, pan.1);

            for &(sx, sy) in &[(400.0_f64, 300.0_f64), (500.0, 320.0), (280.0, 480.0)] {
                let (cx, cy) = v.screen_to_canvas_unclipped((sx, sy), SW, SH);
                let (bx, by) = v.canvas_to_screen(cx, cy, SW, SH);
                assert!(
                    (bx - sx as f32).abs() < 0.01 && (by - sy as f32).abs() < 0.01,
                    "roundtrip failed at zoom {zoom} rot {rot} pan {pan:?}: \
                     ({sx},{sy}) -> ({cx},{cy}) -> ({bx},{by})"
                );
            }
            let _ = v.placement(SW, SH, &c);
        }
    }

    #[test]
    fn fit_centers_the_canvas_upright() {
        let v = fitted();
        assert_eq!(v.rotation(), 0.0);
        let (cx, cy) =
            v.screen_to_canvas_unclipped((f64::from(SW) / 2.0, f64::from(SH) / 2.0), SW, SH);
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
        assert!((cy - 500.0).abs() < 0.5, "y {cy}");
    }

    #[test]
    fn fit_shows_the_whole_canvas() {
        let c = canvas();
        let v = fitted();
        let p = v.placement(SW, SH, &c);
        for corner in [p.tl, p.tr, p.bl, p.br] {
            assert!(
                (-1.0..=1.0).contains(&corner[0]) && (-1.0..=1.0).contains(&corner[1]),
                "corner {corner:?} outside the viewport after fit"
            );
        }
    }

    /// Zoom must keep the point under the cursor stationary, or zooming feels like
    /// the canvas is sliding away.
    #[test]
    fn zoom_is_anchored_at_the_cursor() {
        let mut v = fitted();
        let anchor = (610.0_f64, 205.0_f64);
        let before = v.screen_to_canvas_unclipped(anchor, SW, SH);
        v.zoom_by_notches(5.0, anchor, SW, SH);
        let after = v.screen_to_canvas_unclipped(anchor, SW, SH);
        assert!(
            (before.0 - after.0).abs() < 0.01 && (before.1 - after.1).abs() < 0.01,
            "anchor drifted: {before:?} -> {after:?}"
        );
        assert!(v.scale() > fitted().scale(), "did not zoom in");
    }

    #[test]
    fn rotation_is_anchored_too() {
        let mut v = fitted();
        let anchor = (300.0_f64, 500.0_f64);
        let before = v.screen_to_canvas_unclipped(anchor, SW, SH);
        v.rotate_by(ROTATE_STEP * 3.0, anchor, SW, SH);
        let after = v.screen_to_canvas_unclipped(anchor, SW, SH);
        assert!(
            (before.0 - after.0).abs() < 0.01 && (before.1 - after.1).abs() < 0.01,
            "anchor drifted: {before:?} -> {after:?}"
        );
    }

    /// A drag must move the canvas with the pointer, at any rotation — otherwise
    /// panning a rotated canvas goes sideways.
    #[test]
    fn pan_follows_the_pointer_even_when_rotated() {
        let c = canvas();
        for rot in [0.0, ROTATE_STEP, std::f32::consts::FRAC_PI_2, -1.0] {
            let mut v = fitted();
            v.rotate_by(rot, (400.0, 300.0), SW, SH);

            // The canvas point at the cursor before the drag...
            let start = (400.0_f64, 300.0_f64);
            let grabbed = v.screen_to_canvas_unclipped(start, SW, SH);
            v.pan_by_screen(50.0, -30.0);
            // ...must now sit 50 right and 30 up on screen.
            let (sx, sy) = v.canvas_to_screen(grabbed.0, grabbed.1, SW, SH);
            assert!(
                (sx - 450.0).abs() < 0.05 && (sy - 270.0).abs() < 0.05,
                "rot {rot}: grabbed point went to ({sx},{sy}), expected (450,270)"
            );
            let _ = v.placement(SW, SH, &c);
        }
    }

    #[test]
    fn zoom_is_clamped() {
        let mut v = fitted();
        v.zoom_by_notches(10_000.0, (400.0, 300.0), SW, SH);
        assert!(v.scale() <= MAX_SCALE + 1e-3, "scale {}", v.scale());
        v.zoom_by_notches(-10_000.0, (400.0, 300.0), SW, SH);
        assert!(v.scale() >= MIN_SCALE - 1e-6, "scale {}", v.scale());
    }

    #[test]
    fn rotation_stays_bounded() {
        let mut v = fitted();
        for _ in 0..200 {
            v.rotate_by(ROTATE_STEP, (400.0, 300.0), SW, SH);
        }
        assert!(
            v.rotation().abs() <= std::f32::consts::PI + 1e-5,
            "rotation ran away: {}",
            v.rotation()
        );
    }

    #[test]
    fn points_off_the_canvas_are_rejected() {
        let c = canvas();
        let v = fitted();
        // Top-left of the window is backdrop when the canvas is fitted with margin.
        assert!(v.screen_to_canvas(1.0, 1.0, SW, SH, &c).is_none());
        assert!(v.screen_to_canvas(400.0, 300.0, SW, SH, &c).is_some());
    }

    #[test]
    fn a_pending_fit_applies_once() {
        let c = canvas();
        let mut v = View::new();
        assert!(v.apply_pending_fit(SW, SH, &c), "first fit should apply");
        assert!(!v.apply_pending_fit(SW, SH, &c), "fit should not repeat");
        v.request_fit();
        assert!(
            v.apply_pending_fit(SW, SH, &c),
            "requested fit should apply"
        );
    }

    #[test]
    fn the_ui_inset_shifts_the_visible_center() {
        let c = canvas();
        let mut v = View::new();
        let _ = v.set_inset_left(200.0);
        v.fit(SW, SH, &c);
        // Center of the area right of the panel maps to the canvas center.
        let (cx, cy) = v.screen_to_canvas_unclipped((200.0 + 300.0, 300.0), SW, SH);
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
        assert!((cy - 500.0).abs() < 0.5, "y {cy}");
    }

    /// The first fit necessarily happens before the UI reports its width, so the
    /// view must re-fit once it does -- otherwise the canvas sits centred on the
    /// whole window with half of it behind the panel.
    #[test]
    fn learning_the_ui_inset_refits_while_auto_fitting() {
        let c = canvas();
        let mut v = View::new();
        assert!(v.apply_pending_fit(SW, SH, &c));

        assert!(v.set_inset_left(280.0), "inset change should queue a fit");
        assert!(
            v.apply_pending_fit(SW, SH, &c),
            "inset change did not trigger a re-fit"
        );

        // Canvas centre should now sit in the middle of the area right of the panel.
        let visible_center_x = 280.0 + (f64::from(SW) - 280.0) / 2.0;
        let (cx, _) = v.screen_to_canvas_unclipped((visible_center_x, 300.0), SW, SH);
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
    }

    /// ...but once the user has navigated, a resize must not throw their view away.
    #[test]
    fn manual_navigation_stops_auto_fitting() {
        let c = canvas();
        let mut v = View::new();
        v.apply_pending_fit(SW, SH, &c);

        v.zoom_by_notches(4.0, (400.0, 300.0), SW, SH);
        let zoomed = v.scale();

        v.surface_resized();
        let _ = v.set_inset_left(280.0);
        assert!(
            !v.apply_pending_fit(SW, SH, &c),
            "auto-fit re-fitted after the user navigated"
        );
        assert!((v.scale() - zoomed).abs() < 1e-6, "zoom was reset");
    }

    /// An explicit fit request resumes auto-fitting, so the canvas keeps tracking
    /// window size again afterwards.
    #[test]
    fn requesting_fit_resumes_auto_fitting() {
        let c = canvas();
        let mut v = View::new();
        v.apply_pending_fit(SW, SH, &c);
        v.zoom_by_notches(4.0, (400.0, 300.0), SW, SH);

        v.request_fit();
        assert!(v.apply_pending_fit(SW, SH, &c));
        v.surface_resized();
        assert!(v.apply_pending_fit(SW, SH, &c), "auto-fit did not resume");
    }

    #[test]
    fn degenerate_surface_sizes_do_not_panic() {
        let c = canvas();
        let mut v = View::new();
        v.fit(0, 0, &c);
        let _ = v.placement(0, 0, &c);
        let _ = v.screen_to_canvas(0.0, 0.0, 0, 0, &c);
    }

    /// After extending upward the page origin is negative, and the camera must treat
    /// that area as part of the page -- otherwise the space just added would not be
    /// drawable on.
    #[test]
    fn a_negative_origin_is_inside_the_page() {
        let mut c = Canvas::new(1000, 1000);
        c.resize(openpaint_core::PageRect::new(0, -500, 1000, 1500));
        assert_eq!(c.origin(), (0, -500));

        let mut v = View::new();
        v.fit(SW, SH, &c);

        // The centre of the visible area is the page's centre, which is now y = 250.
        let (cx, cy) =
            v.screen_to_canvas_unclipped((f64::from(SW) / 2.0, f64::from(SH) / 2.0), SW, SH);
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
        assert!((cy - 250.0).abs() < 0.5, "y {cy}");

        // A point in the newly added region maps and is accepted.
        let screen = v.canvas_to_screen(500.0, -250.0, SW, SH);
        let hit = v
            .screen_to_canvas(f64::from(screen.0), f64::from(screen.1), SW, SH, &c)
            .expect("negative-y page space should be paintable");
        assert!((hit.1 - -250.0).abs() < 0.5, "got {hit:?}");
    }

    /// The invariant that removed the camera compensation: extending the page does not
    /// change where existing content appears on screen, because the content's
    /// coordinates do not change and the camera is untouched.
    #[test]
    fn extending_does_not_move_content_on_screen() {
        let mut c = Canvas::new(1000, 1000);
        let mut v = View::new();
        v.fit(SW, SH, &c);

        let p = (137.0_f32, 421.0_f32);
        let before = v.canvas_to_screen(p.0, p.1, SW, SH);

        // Extend upward and leftward: the rectangle's origin moves, content does not.
        c.resize(openpaint_core::PageRect::new(-400, -500, 1400, 1500));
        let after = v.canvas_to_screen(p.0, p.1, SW, SH);

        assert_eq!(
            before, after,
            "content appeared to move; the camera should need no compensation"
        );
    }
}
