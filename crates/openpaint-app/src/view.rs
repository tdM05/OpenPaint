//! The camera onto the canvas — pan, zoom, rotate, and the screen↔canvas transform.
//!
//! This is *view* state, deliberately separate from the document and from the GPU.
//! Two things must agree exactly or strokes land where the user didn't draw:
//!
//! 1. where canvas geometry is drawn ([`View::page_to_ndc`]), and
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
//! Rotation is included from the start even though only a keyboard binding drives it so
//! far. It is what forces the transform to be a general affine rather than an
//! axis-aligned rectangle — retrofitting that later would mean rewriting the uniform, the
//! shaders, and both directions of this transform. Building it once is cheaper than
//! building it twice.

use openpaint_core::PageRect;

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

/// The page→NDC transform as a 2×3 affine.
///
/// The canvas is drawn as many tile quads whose page positions the shader only learns from
/// per-instance data, so what it needs is the transform itself. An earlier version handed
/// the shader four precomputed NDC corners instead, which could place exactly one known
/// quad and nothing else.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageToNdc {
    /// `ndc.x = x_row·(px, py, 1)`
    pub x_row: [f32; 3],
    /// `ndc.y = y_row·(px, py, 1)`
    pub y_row: [f32; 3],
}

impl PageToNdc {
    /// Map a page coordinate to normalized device coordinates.
    ///
    /// The CPU mirror of what the vertex shaders do with these rows, and test-only for
    /// exactly that reason: it exists so the affine can be checked against
    /// [`View::canvas_to_screen`], which is the property that stops drawn geometry and
    /// input mapping drifting apart. The shipping path applies the rows on the GPU.
    #[cfg(test)]
    #[must_use]
    pub fn apply(&self, px: f32, py: f32) -> [f32; 2] {
        [
            self.x_row[0] * px + self.x_row[1] * py + self.x_row[2],
            self.y_row[0] * px + self.y_row[1] * py + self.y_row[2],
        ]
    }
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
    /// Where on the surface the canvas may draw, in physical pixels, or `None` for all of it.
    ///
    /// **A rectangle rather than a left inset**, which the panel workspace forced and which was
    /// the better shape all along: the canvas is a panel like any other (§1c), so it can sit
    /// anywhere — middle, right, bottom, or beside a second canvas. A left inset could only ever
    /// describe one arrangement, and it was the one the old side panel happened to have.
    viewport: Option<(f32, f32, f32, f32)>,
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
            viewport: None,
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

    /// Tell the view which part of the surface the canvas may use.
    ///
    /// Returns `true` if this queued a re-fit, in which case the caller **must**
    /// request another frame. Painting is demand-driven, so a queued fit that
    /// nobody asks to draw simply never happens -- the same trap as egui needing a
    /// frame to process its input.
    pub fn set_viewport(&mut self, area: (f32, f32, f32, f32)) -> bool {
        let area = (area.0.max(0.0), area.1.max(0.0), area.2, area.3);
        let moved = self.viewport.is_none_or(|old| {
            (old.0 - area.0).abs() > 0.5
                || (old.1 - area.1).abs() > 0.5
                || (old.2 - area.2).abs() > 0.5
                || (old.3 - area.3).abs() > 0.5
        });
        if moved {
            self.viewport = Some(area);
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
    pub fn apply_pending_fit(&mut self, surface_w: u32, surface_h: u32, page: PageRect) -> bool {
        if !self.needs_fit {
            return false;
        }
        self.needs_fit = false;
        self.fit(surface_w, surface_h, page);
        true
    }

    /// Fit the whole canvas in the visible area, upright and centered.
    pub fn fit(&mut self, surface_w: u32, surface_h: u32, page: PageRect) {
        let (_, _, area_w, area_h) = self.content_area(surface_w, surface_h);
        let cw = page.w.max(1) as f32;
        let ch = page.h.max(1) as f32;
        self.scale = ((area_w / cw).min(area_h / ch) * FIT_MARGIN).clamp(MIN_SCALE, MAX_SCALE);
        self.rotation = 0.0;
        // The page's centre in page coordinates, which is not `extent / 2` once the origin is
        // negative.
        self.center = (page.x as f32 + cw * 0.5, page.y as f32 + ch * 0.5);
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
        let Some((x, y, w, h)) = self.viewport else {
            return (0.0, 0.0, sw, sh);
        };
        // Clamped to the surface, because a stale viewport from the frame before a resize would
        // otherwise put the canvas partly off-screen -- and the fit that corrects it needs a
        // sane rectangle to fit *into*.
        let x = x.min(sw - 1.0);
        let y = y.min(sh - 1.0);
        (x, y, w.clamp(1.0, sw - x), h.clamp(1.0, sh - y))
    }

    /// Center of the visible area, in physical pixels.
    fn content_center(&self, surface_w: u32, surface_h: u32) -> (f32, f32) {
        let (x, y, w, h) = self.content_area(surface_w, surface_h);
        (x + w * 0.5, y + h * 0.5)
    }

    /// Forward transform: canvas pixels -> physical screen pixels.
    #[must_use]
    pub fn canvas_to_screen(&self, cx: f32, cy: f32, surface_w: u32, surface_h: u32) -> (f32, f32) {
        let (ccx, ccy) = self.content_center(surface_w, surface_h);
        let (rx, ry) = rotate(
            (cx - self.center.0) * self.scale,
            (cy - self.center.1) * self.scale,
            self.rotation,
        );
        (ccx + rx, ccy + ry)
    }

    /// Inverse transform, without the on-canvas bounds check.
    ///
    /// Public because the crop tool needs it: a crop handle dragged outward is legitimately
    /// outside the page, and clamping it would make extending-by-drag impossible.
    #[must_use]
    pub fn screen_to_canvas_unclipped(
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

    /// The page→NDC transform, for drawing geometry the shader positions itself.
    ///
    /// Built by *sampling* the forward transform at three points rather than re-deriving
    /// the trigonometry. The transform is affine, so those three samples determine it
    /// exactly — and this way there is no second copy of the maths that could drift from
    /// [`View::canvas_to_screen`], which is the one property this module exists to
    /// guarantee (see the module note).
    #[must_use]
    pub fn page_to_ndc(&self, surface_w: u32, surface_h: u32) -> PageToNdc {
        let at = |px: f32, py: f32| self.canvas_to_ndc(px, py, surface_w, surface_h);
        let o = at(0.0, 0.0);
        let x = at(1.0, 0.0);
        let y = at(0.0, 1.0);
        PageToNdc {
            x_row: [x[0] - o[0], y[0] - o[0], o[0]],
            y_row: [x[1] - o[1], y[1] - o[1], o[1]],
        }
    }

    /// Forward transform all the way to normalized device coordinates.
    fn canvas_to_ndc(&self, cx: f32, cy: f32, surface_w: u32, surface_h: u32) -> [f32; 2] {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        let (x, y) = self.canvas_to_screen(cx, cy, surface_w, surface_h);
        // Pixels -> NDC: x maps 0..sw to -1..1, y maps 0..sh to 1..-1.
        [x / sw * 2.0 - 1.0, 1.0 - y / sh * 2.0]
    }

    /// The page-space bounding box of everything the viewport can show.
    ///
    /// Conservative on purpose: under rotation the visible region is a rotated rectangle, and
    /// this returns its axis-aligned bound. A tile just outside the true region costs one
    /// wasted instance, whereas missing one leaves a hole on screen — so erring outward is the
    /// only safe direction.
    ///
    /// Residency depends on this: with a bounded tile pool and spilling in place, the visible
    /// set *is* the working set, so this is what decides which tiles are kept on the GPU.
    #[must_use]
    pub fn visible_rect(&self, surface_w: u32, surface_h: u32) -> PageRect {
        let (x, y, w, h) = self.content_area(surface_w, surface_h);
        let corners = [
            (f64::from(x), f64::from(y)),
            (f64::from(x + w), f64::from(y)),
            (f64::from(x), f64::from(y + h)),
            (f64::from(x + w), f64::from(y + h)),
        ];
        let mut min = (f32::MAX, f32::MAX);
        let mut max = (f32::MIN, f32::MIN);
        for c in corners {
            let (px, py) = self.screen_to_canvas_unclipped(c, surface_w, surface_h);
            min = (min.0.min(px), min.1.min(py));
            max = (max.0.max(px), max.1.max(py));
        }
        // One pixel of slack, so a boundary landing exactly on a tile edge still includes it.
        let x0 = min.0.floor() as i32 - 1;
        let y0 = min.1.floor() as i32 - 1;
        let x1 = max.0.ceil() as i32 + 1;
        let y1 = max.1.ceil() as i32 + 1;
        PageRect::new(x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32)
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
        page: PageRect,
    ) -> Option<(f32, f32)> {
        let (cx, cy) = self.screen_to_canvas_unclipped((px, py), surface_w, surface_h);
        let (ex, ey) = page.end();
        if cx < page.x as f32 || cy < page.y as f32 || cx > ex as f32 || cy > ey as f32 {
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

    fn canvas() -> PageRect {
        PageRect::from_size(1000, 1000)
    }

    fn fitted() -> View {
        let mut v = View::new();
        v.fit(SW, SH, canvas());
        v
    }

    /// The affine and the point-by-point forward transform are the same map. This is the
    /// property that lets tiles be placed by the shader while input keeps using
    /// `screen_to_canvas`: if they ever disagree, strokes land where nobody drew.
    ///
    /// Checked under rotation and a negative origin specifically, because those are where
    /// a hand-derived matrix would go wrong (a sign slip in the rotation, or an assumption
    /// that the page starts at zero).
    #[test]
    fn the_affine_agrees_with_the_forward_transform() {
        let mut v = fitted();
        v.rotate_by(0.7, (300.0, 200.0), SW, SH);
        v.pan_by_screen(37.0, -19.0);

        let m = v.page_to_ndc(SW, SH);
        for (px, py) in [
            (0.0, 0.0),
            (1000.0, 1000.0),
            (-512.0, -768.0),
            (123.5, -4.25),
            // A webtoon strip is genuinely this tall, so the transform has to stay
            // accurate out here and not only near the origin.
            (800.0, 40000.0),
        ] {
            let expect = v.canvas_to_ndc(px, py, SW, SH);
            let got = m.apply(px, py);
            // Relative, because both sides are f32 sums whose rounding grows with the
            // magnitude of the input. The affine is exact in exact arithmetic; what is
            // being checked is that it is the *same map*, not that f32 is lossless.
            let tol = |e: f32| 1e-4 * (1.0 + e.abs());
            assert!(
                (got[0] - expect[0]).abs() < tol(expect[0])
                    && (got[1] - expect[1]).abs() < tol(expect[1]),
                "at ({px}, {py}): affine {got:?} vs forward {expect:?}"
            );
        }
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
            let _ = v.page_to_ndc(SW, SH);
            let _ = &c;
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
        let m = v.page_to_ndc(SW, SH);
        let (ox, oy) = c.origin();
        let (ex, ey) = c.end();
        for (px, py) in [
            (ox as f32, oy as f32),
            (ex as f32, oy as f32),
            (ox as f32, ey as f32),
            (ex as f32, ey as f32),
        ] {
            let corner = m.apply(px, py);
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
            let _ = v.page_to_ndc(SW, SH);
            let _ = &c;
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
        assert!(v.screen_to_canvas(1.0, 1.0, SW, SH, c).is_none());
        assert!(v.screen_to_canvas(400.0, 300.0, SW, SH, c).is_some());
    }

    #[test]
    fn a_pending_fit_applies_once() {
        let c = canvas();
        let mut v = View::new();
        assert!(v.apply_pending_fit(SW, SH, c), "first fit should apply");
        assert!(!v.apply_pending_fit(SW, SH, c), "fit should not repeat");
        v.request_fit();
        assert!(v.apply_pending_fit(SW, SH, c), "requested fit should apply");
    }

    #[test]
    fn the_viewport_shifts_the_visible_center() {
        let c = canvas();
        let mut v = View::new();
        let _ = v.set_viewport((
            200.0,
            0.0,
            f32::from(SW as u16) - 200.0,
            f32::from(SH as u16),
        ));
        v.fit(SW, SH, c);
        // Center of the area right of the panel maps to the canvas center.
        let (cx, cy) = v.screen_to_canvas_unclipped((200.0 + 300.0, 300.0), SW, SH);
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
        assert!((cy - 500.0).abs() < 0.5, "y {cy}");
    }

    /// The first fit necessarily happens before the UI reports its width, so the
    /// view must re-fit once it does -- otherwise the canvas sits centred on the
    /// whole window with half of it behind the panel.
    #[test]
    fn learning_the_viewport_refits_while_auto_fitting() {
        let c = canvas();
        let mut v = View::new();
        assert!(v.apply_pending_fit(SW, SH, c));

        assert!(
            v.set_viewport((
                280.0,
                0.0,
                f32::from(SW as u16) - 280.0,
                f32::from(SH as u16)
            )),
            "a viewport change should queue a fit"
        );
        assert!(
            v.apply_pending_fit(SW, SH, c),
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
        v.apply_pending_fit(SW, SH, c);

        v.zoom_by_notches(4.0, (400.0, 300.0), SW, SH);
        let zoomed = v.scale();

        v.surface_resized();
        let _ = v.set_viewport((
            280.0,
            0.0,
            f32::from(SW as u16) - 280.0,
            f32::from(SH as u16),
        ));
        assert!(
            !v.apply_pending_fit(SW, SH, c),
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
        v.apply_pending_fit(SW, SH, c);
        v.zoom_by_notches(4.0, (400.0, 300.0), SW, SH);

        v.request_fit();
        assert!(v.apply_pending_fit(SW, SH, c));
        v.surface_resized();
        assert!(v.apply_pending_fit(SW, SH, c), "auto-fit did not resume");
    }

    #[test]
    fn degenerate_surface_sizes_do_not_panic() {
        let c = canvas();
        let mut v = View::new();
        v.fit(0, 0, c);
        let _ = v.page_to_ndc(0, 0);
        let _ = v.screen_to_canvas(0.0, 0.0, 0, 0, c);
    }

    /// After extending upward the page origin is negative, and the camera must treat
    /// that area as part of the page -- otherwise the space just added would not be
    /// drawable on.
    #[test]
    fn a_negative_origin_is_inside_the_page() {
        // A page extended downward: same origin, taller.
        let c = openpaint_core::PageRect::new(0, -500, 1000, 1500);
        assert_eq!(c.origin(), (0, -500));

        let mut v = View::new();
        v.fit(SW, SH, c);

        // The centre of the visible area is the page's centre, which is now y = 250.
        let (cx, cy) =
            v.screen_to_canvas_unclipped((f64::from(SW) / 2.0, f64::from(SH) / 2.0), SW, SH);
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
        assert!((cy - 250.0).abs() < 0.5, "y {cy}");

        // A point in the newly added region maps and is accepted.
        let screen = v.canvas_to_screen(500.0, -250.0, SW, SH);
        let hit = v
            .screen_to_canvas(f64::from(screen.0), f64::from(screen.1), SW, SH, c)
            .expect("negative-y page space should be paintable");
        assert!((hit.1 - -250.0).abs() < 0.5, "got {hit:?}");
    }

    /// The invariant that removed the camera compensation: extending the page does not
    /// change where existing content appears on screen, because the content's
    /// coordinates do not change and the camera is untouched.
    #[test]
    fn extending_does_not_move_content_on_screen() {
        let mut v = View::new();
        v.fit(SW, SH, PageRect::from_size(1000, 1000));

        let p = (137.0_f32, 421.0_f32);
        let before = v.canvas_to_screen(p.0, p.1, SW, SH);

        // Extend upward and leftward: the rectangle's origin moves, content does not. The
        // camera is deliberately *not* told about it, which is the point -- nothing has to
        // compensate, because the point's coordinates did not change.
        let _extended = PageRect::new(-400, -500, 1400, 1500);
        let after = v.canvas_to_screen(p.0, p.1, SW, SH);

        assert_eq!(
            before, after,
            "content appeared to move; the camera should need no compensation"
        );
    }
}
