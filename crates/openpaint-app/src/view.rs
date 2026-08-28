//! How the canvas is positioned on screen — the screen↔canvas transform.
//!
//! Extracted deliberately, because this is *view* state and it was previously
//! entangled with GPU state. Two things must agree exactly or strokes land where
//! the user didn't draw:
//!
//! 1. where the canvas quad is drawn ([`View::placement`]), and
//! 2. where an input sample maps to in canvas space ([`View::screen_to_canvas`]).
//!
//! Keeping both on one type is what guarantees they can't drift, and it is where
//! pan / zoom / rotate will land — they are changes to *this* type, not to the
//! renderer or the editor.

use openpaint_core::Canvas;

/// Fraction of the available area the canvas is fitted into, so the sheet has a
/// little breathing room rather than touching the window edge.
const FIT_MARGIN: f32 = 0.94;

/// Where the canvas quad sits, in normalized device coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Top-left corner in NDC (x right, y up).
    pub min_ndc: [f32; 2],
    /// Bottom-right corner in NDC.
    pub max_ndc: [f32; 2],
}

/// The camera onto the canvas.
///
/// Currently fit-and-center with a left inset for the UI panel. Pan/zoom/rotate
/// extend this type; nothing else needs to change to accommodate them, which is
/// the point of it existing.
#[derive(Clone, Copy, Debug, Default)]
pub struct View {
    /// Physical pixels along the left edge that the UI occupies, so the canvas
    /// centers in the area actually visible rather than under the panel.
    inset_left_px: f32,
}

impl View {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Tell the view how much of the left edge the UI covers.
    pub fn set_inset_left(&mut self, px: f32) {
        self.inset_left_px = px.max(0.0);
    }

    /// The content area available to the canvas, in physical pixels, as
    /// `(left, top, width, height)`.
    fn content_area(&self, surface_w: u32, surface_h: u32) -> (f32, f32, f32, f32) {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        let left = self.inset_left_px.min(sw - 1.0);
        (left, 0.0, (sw - left).max(1.0), sh)
    }

    /// Scale and on-screen rect (physical px) the canvas is drawn at.
    fn fit(&self, surface_w: u32, surface_h: u32, canvas: &Canvas) -> (f32, f32, f32, f32, f32) {
        let (area_x, area_y, area_w, area_h) = self.content_area(surface_w, surface_h);
        let cw = canvas.width().max(1) as f32;
        let ch = canvas.height().max(1) as f32;

        let scale = (area_w / cw).min(area_h / ch) * FIT_MARGIN;
        let draw_w = cw * scale;
        let draw_h = ch * scale;
        // Centered within the content area.
        let left = area_x + (area_w - draw_w) * 0.5;
        let top = area_y + (area_h - draw_h) * 0.5;
        (scale, left, top, draw_w, draw_h)
    }

    /// Where to draw the canvas quad, in NDC.
    #[must_use]
    pub fn placement(&self, surface_w: u32, surface_h: u32, canvas: &Canvas) -> Placement {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        let (_, left, top, draw_w, draw_h) = self.fit(surface_w, surface_h, canvas);

        // Pixels -> NDC: x maps 0..sw to -1..1, y maps 0..sh to 1..-1.
        let to_ndc_x = |px: f32| px / sw * 2.0 - 1.0;
        let to_ndc_y = |py: f32| 1.0 - py / sh * 2.0;

        Placement {
            min_ndc: [to_ndc_x(left), to_ndc_y(top)],
            max_ndc: [to_ndc_x(left + draw_w), to_ndc_y(top + draw_h)],
        }
    }

    /// Map a window position (physical pixels) to canvas pixels.
    ///
    /// Returns `None` when the point is outside the drawn canvas, so callers can
    /// simply not paint rather than clamping to an edge.
    #[must_use]
    pub fn screen_to_canvas(
        &self,
        px: f64,
        py: f64,
        surface_w: u32,
        surface_h: u32,
        canvas: &Canvas,
    ) -> Option<(f32, f32)> {
        let (scale, left, top, draw_w, draw_h) = self.fit(surface_w, surface_h, canvas);
        let lx = px as f32 - left;
        let ly = py as f32 - top;
        if lx < 0.0 || ly < 0.0 || lx > draw_w || ly > draw_h {
            return None;
        }
        Some((lx / scale, ly / scale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Canvas {
        Canvas::new(1000, 1000)
    }

    /// The two halves of the transform must agree: the center of the drawn quad
    /// must map to the center of the canvas. If they drift, strokes land away
    /// from the pen.
    #[test]
    fn screen_center_maps_to_canvas_center() {
        let v = View::new();
        let c = canvas();
        let (sw, sh) = (800, 600);
        let (cx, cy) = v
            .screen_to_canvas(400.0, 300.0, sw, sh, &c)
            .expect("inside");
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
        assert!((cy - 500.0).abs() < 0.5, "y {cy}");
    }

    #[test]
    fn points_outside_the_canvas_are_rejected() {
        let v = View::new();
        let c = canvas();
        // Far left of a landscape window is backdrop, not canvas.
        assert!(v.screen_to_canvas(2.0, 300.0, 800, 600, &c).is_none());
        assert!(v.screen_to_canvas(-10.0, -10.0, 800, 600, &c).is_none());
    }

    #[test]
    fn placement_stays_within_ndc() {
        let v = View::new();
        let c = canvas();
        let p = v.placement(800, 600, &c);
        for value in [p.min_ndc[0], p.min_ndc[1], p.max_ndc[0], p.max_ndc[1]] {
            assert!((-1.0..=1.0).contains(&value), "{value} outside NDC");
        }
        // y is flipped: min (top) is above max (bottom).
        assert!(p.min_ndc[1] > p.max_ndc[1]);
        assert!(p.min_ndc[0] < p.max_ndc[0]);
    }

    /// A UI inset must shift the canvas right, and the transform must follow it,
    /// or clicks would be offset by the panel width.
    #[test]
    fn inset_shifts_the_canvas_and_the_mapping_together() {
        let c = canvas();
        let (sw, sh) = (800, 600);

        let plain = View::new();
        let mut inset = View::new();
        inset.set_inset_left(200.0);

        let a = plain.placement(sw, sh, &c);
        let b = inset.placement(sw, sh, &c);
        assert!(b.min_ndc[0] > a.min_ndc[0], "inset did not shift right");

        // The center of the *visible* area should still be the canvas center.
        let (cx, cy) = inset
            .screen_to_canvas(200.0 + 300.0, 300.0, sw, sh, &c)
            .expect("inside");
        assert!((cx - 500.0).abs() < 0.5, "x {cx}");
        assert!((cy - 500.0).abs() < 0.5, "y {cy}");
    }

    /// A non-square canvas must keep its aspect ratio.
    #[test]
    fn aspect_ratio_is_preserved() {
        let v = View::new();
        let tall = Canvas::new(500, 2000);
        let (scale, _, _, draw_w, draw_h) = v.fit(800, 600, &tall);
        assert!(scale > 0.0);
        assert!((draw_w / draw_h - 500.0 / 2000.0).abs() < 1e-4);
    }

    #[test]
    fn degenerate_surface_sizes_do_not_panic() {
        let v = View::new();
        let c = canvas();
        let _ = v.placement(0, 0, &c);
        let _ = v.screen_to_canvas(0.0, 0.0, 0, 0, &c);
    }
}
