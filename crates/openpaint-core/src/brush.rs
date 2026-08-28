//! Stamp-based round brush.
//!
//! This is the architectural seed of the real brush engine: a stroke is not a
//! polyline, it's a series of round *dabs* stamped along the path at a fixed
//! fraction of the brush diameter (the "spacing", ~25% in Photoshop). Each dab
//! has a soft falloff from center to edge (hardness). Building it this way now
//! means the path toward Photoshop/CSP-quality brushes is incremental tuning,
//! not a rewrite.
//!
//! # This module emits dabs; it does not touch pixels
//!
//! Per DECISIONS §4a, the brush decides *where dabs go and what they look like*
//! and nothing else. Turning dabs into pixels is [`crate::raster`] (reference)
//! and later a GPU rasterizer. See [`crate::dab`] for why that boundary is drawn
//! exactly here.
//!
//! The practical benefit is visible in the tests below: stroke behavior is now
//! verified by asserting on the *dabs produced* — exact count, exact positions,
//! exact spacing — with no canvas involved. Previously it could only be checked
//! indirectly, by observing that some pixels somewhere had changed.
//!
//! Phase 0 scope: constant color, pressure maps to dab radius. Still to come
//! (docs Q7a): **flow/opacity accumulation**, textures, a tuned falloff curve,
//! and modulation of parameters by pressure/tilt/velocity through curves.
//!
//! ⚠️ Dabs are still composited one at a time by the rasterizer, which is wrong
//! for overlapping dabs within a stroke: *flow* should accumulate per dab while
//! *opacity* caps the stroke's total contribution. That needs a per-stroke
//! accumulation buffer sitting between emission and rasterization — which is
//! exactly the seam this split creates, and the next piece of work.

use crate::color::opaque_srgb8_to_linear_premul;
use crate::dab::Dab;

/// Brush parameters. Sizes are in canvas pixels.
#[derive(Clone, Copy)]
pub struct Brush {
    /// Dab radius at full pressure, in pixels.
    pub radius: f32,
    /// Edge softness in `0.0..=1.0`: 0 = hard edge, 1 = fully soft falloff.
    pub hardness: f32,
    /// Dab spacing as a fraction of diameter (Photoshop default ≈ 0.25).
    pub spacing: f32,
    /// Brush color, linear and premultiplied (see [`crate::color`]).
    ///
    /// Stored converted rather than as authored sRGB so the per-pixel inner loop
    /// never pays for a transfer function, and so there is exactly one place the
    /// conversion can be got wrong: [`Brush::set_color_srgb8`].
    color_linear_premul: [f32; 4],
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            radius: 8.0,
            hardness: 0.5,
            spacing: 0.25,
            color_linear_premul: opaque_srgb8_to_linear_premul([20, 20, 24]),
        }
    }
}

/// Tracks stroke continuity so dabs are spaced correctly across successive
/// input samples (which arrive at irregular distances). Create one per stroke.
pub struct StrokeState {
    /// Last sample position, and how far we've traveled since the last dab.
    last: Option<(f32, f32)>,
    /// Distance accumulated since the previous stamped dab.
    residual: f32,
}

impl StrokeState {
    pub fn new() -> Self {
        Self {
            last: None,
            residual: 0.0,
        }
    }
}

impl Default for StrokeState {
    fn default() -> Self {
        Self::new()
    }
}

impl Brush {
    /// Set the brush color from an authored opaque sRGB value.
    pub fn set_color_srgb8(&mut self, rgb: [u8; 3]) {
        self.color_linear_premul = opaque_srgb8_to_linear_premul(rgb);
    }

    /// The brush color as linear premultiplied RGBA.
    #[must_use]
    pub fn color_linear_premul(&self) -> [f32; 4] {
        self.color_linear_premul
    }

    /// Effective dab radius for a given pressure in `0.0..=1.0`.
    fn radius_for(&self, pressure: f32) -> f32 {
        // Simple linear pressure→size for now. A configurable response curve
        // arrives with the real engine.
        (self.radius * pressure.clamp(0.0, 1.0)).max(0.5)
    }

    /// Build one dab at `(cx, cy)` with the given radius.
    fn dab_at(&self, cx: f32, cy: f32, radius: f32) -> Dab {
        Dab {
            x: cx,
            y: cy,
            radius,
            hardness: self.hardness,
            color_linear_premul: self.color_linear_premul,
        }
    }

    /// Begin a stroke: emit the initial dab at the first sample.
    pub fn stroke_begin(
        &self,
        out: &mut Vec<Dab>,
        state: &mut StrokeState,
        x: f32,
        y: f32,
        pressure: f32,
    ) {
        out.push(self.dab_at(x, y, self.radius_for(pressure)));
        state.last = Some((x, y));
        state.residual = 0.0;
    }

    /// Continue a stroke to a new sample, emitting evenly spaced dabs along the
    /// segment from the previous sample so speed doesn't create gaps.
    pub fn stroke_to(
        &self,
        out: &mut Vec<Dab>,
        state: &mut StrokeState,
        x: f32,
        y: f32,
        pressure: f32,
    ) {
        let Some((px, py)) = state.last else {
            self.stroke_begin(out, state, x, y, pressure);
            return;
        };

        let radius = self.radius_for(pressure);
        let step = (radius * 2.0 * self.spacing).max(0.5);

        let dx = x - px;
        let dy = y - py;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len <= f32::EPSILON {
            return;
        }
        let (ux, uy) = (dx / seg_len, dy / seg_len);

        // Walk along the segment, emitting a dab every `step` pixels, carrying
        // leftover distance in `residual` so spacing is continuous across
        // segments.
        let mut traveled = -state.residual;
        while traveled + step <= seg_len {
            traveled += step;
            let t = traveled;
            out.push(self.dab_at(px + ux * t, py + uy * t, radius));
        }
        state.residual = seg_len - traveled;
        state.last = Some((x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Emit a straight horizontal stroke and return the dabs it produced.
    fn stroke(brush: &Brush, from: f32, to: f32, pressure: f32) -> Vec<Dab> {
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        brush.stroke_begin(&mut dabs, &mut state, from, 0.0, pressure);
        brush.stroke_to(&mut dabs, &mut state, to, 0.0, pressure);
        dabs
    }

    #[test]
    fn beginning_a_stroke_emits_exactly_one_dab() {
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        Brush::default().stroke_begin(&mut dabs, &mut state, 5.0, 7.0, 1.0);
        assert_eq!(dabs.len(), 1);
        assert_eq!((dabs[0].x, dabs[0].y), (5.0, 7.0));
    }

    /// The core spacing contract, now directly checkable: radius 8 at spacing
    /// 0.25 means a dab every 4px, so 400px of travel is 100 dabs plus the
    /// initial one.
    #[test]
    fn dabs_are_spaced_at_a_quarter_of_the_diameter() {
        let b = Brush::default();
        let dabs = stroke(&b, 0.0, 400.0, 1.0);
        assert_eq!(b.radius * 2.0 * b.spacing, 4.0);
        assert_eq!(dabs.len(), 101);
        for (i, d) in dabs.iter().enumerate() {
            assert!(
                (d.x - i as f32 * 4.0).abs() < 1e-3,
                "dab {i} at {} not at {}",
                d.x,
                i as f32 * 4.0
            );
        }
    }

    /// Spacing must carry across segment boundaries, or slow strokes (many short
    /// segments) would bunch dabs up at every input sample.
    #[test]
    fn spacing_is_continuous_across_segments() {
        let b = Brush::default();
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        b.stroke_begin(&mut dabs, &mut state, 0.0, 0.0, 1.0);
        // Ten 10px segments == one 100px segment, as far as spacing goes.
        for i in 1..=10 {
            b.stroke_to(&mut dabs, &mut state, i as f32 * 10.0, 0.0, 1.0);
        }

        let one_segment = stroke(&b, 0.0, 100.0, 1.0);
        assert_eq!(dabs.len(), one_segment.len());
        for (a, b) in dabs.iter().zip(&one_segment) {
            assert!((a.x - b.x).abs() < 1e-3, "{} vs {}", a.x, b.x);
        }
    }

    #[test]
    fn pressure_scales_dab_radius() {
        let b = Brush::default();
        let full = stroke(&b, 0.0, 100.0, 1.0);
        let light = stroke(&b, 0.0, 100.0, 0.5);
        assert_eq!(full[0].radius, b.radius);
        assert_eq!(light[0].radius, b.radius * 0.5);
        // Lighter pressure means smaller dabs, hence tighter spacing, hence more
        // of them over the same distance.
        assert!(light.len() > full.len());
    }

    /// Zero pressure must still produce a drawable dab rather than a degenerate
    /// one, or a stroke started before the pen is fully down would vanish.
    #[test]
    fn zero_pressure_still_yields_a_positive_radius() {
        let dabs = stroke(&Brush::default(), 0.0, 10.0, 0.0);
        assert!(dabs.iter().all(|d| d.radius > 0.0));
    }

    #[test]
    fn a_zero_length_segment_emits_no_extra_dabs() {
        let b = Brush::default();
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        b.stroke_begin(&mut dabs, &mut state, 10.0, 10.0, 1.0);
        b.stroke_to(&mut dabs, &mut state, 10.0, 10.0, 1.0);
        assert_eq!(dabs.len(), 1);
    }

    /// `stroke_to` without a preceding `stroke_begin` must not silently drop the
    /// input; it should behave as the start of a stroke.
    #[test]
    fn stroke_to_without_begin_starts_the_stroke() {
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        Brush::default().stroke_to(&mut dabs, &mut state, 3.0, 4.0, 1.0);
        assert_eq!(dabs.len(), 1);
        assert_eq!((dabs[0].x, dabs[0].y), (3.0, 4.0));
    }

    #[test]
    fn dabs_carry_the_brush_color_and_hardness() {
        let mut b = Brush::default();
        b.set_color_srgb8([255, 0, 0]);
        let dabs = stroke(&b, 0.0, 20.0, 1.0);
        for d in &dabs {
            assert_eq!(d.hardness, b.hardness);
            assert_eq!(d.color_linear_premul, b.color_linear_premul());
        }
    }

    #[test]
    fn diagonal_strokes_are_spaced_along_the_path() {
        let b = Brush::default();
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        b.stroke_begin(&mut dabs, &mut state, 0.0, 0.0, 1.0);
        // (30, 40) is 50 units away, so 50/4 = 12 further dabs.
        b.stroke_to(&mut dabs, &mut state, 30.0, 40.0, 1.0);
        assert_eq!(dabs.len(), 13);
        // Consecutive dabs must be one spacing step apart along the diagonal.
        for pair in dabs.windows(2) {
            let d = ((pair[1].x - pair[0].x).powi(2) + (pair[1].y - pair[0].y).powi(2)).sqrt();
            assert!((d - 4.0).abs() < 1e-3, "spacing {d} != 4.0");
        }
    }
}
