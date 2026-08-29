//! Stamp-based round brush.
//!
//! This is the architectural seed of the real brush engine: a stroke is not a
//! polyline, it's a series of round *dabs* stamped along the path at a fixed
//! fraction of the brush diameter (the "spacing", ~25% in Photoshop). Each dab
//! has a falloff from center to edge controlled by hardness. Building it this way now
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
//! Flow and opacity are honored via [`crate::stroke`], which accumulates paint
//! per stroke so overlapping dabs build toward the opacity ceiling instead of
//! darkening on every overlap.
//!
//! Pressure drives size and flow through editable response curves ([`crate::curve`]), which is
//! what makes a pencil and an inking pen different tools rather than two sizes of one.
//!
//! Still to come (docs Q7a): dab *shape* -- elliptical, roundness, angle, following stroke
//! direction or tilt -- and textured tips, which is what a pencil or charcoal actually needs. Tilt
//! and velocity as curve inputs alongside pressure: the curve machinery is already general over
//! its input, so those are new sources rather than new mechanisms.

use crate::color::{opaque_linear_premul_to_srgb8, opaque_srgb8_to_linear_premul};
use crate::curve::Curve;
use crate::dab::Dab;

/// Brush parameters. Sizes are in canvas pixels.
#[derive(Clone)]
pub struct Brush {
    /// Dab radius at full pressure, in pixels.
    pub radius: f32,
    /// Edge hardness in `0.0..=1.0`: 1 = hard edge, 0 = fully soft. Matches
    /// Photoshop/CSP orientation -- see [`crate::dab::Dab::hardness`].
    pub hardness: f32,
    /// Dab spacing as a fraction of diameter (Photoshop default ≈ 0.25).
    pub spacing: f32,
    /// How much paint each dab deposits, `0.0..=1.0`.
    pub flow: f32,
    /// How pressure drives dab size. See [`crate::curve`].
    ///
    /// A flat curve is "pressure does not affect size", so there is no separate switch for that.
    pub size_response: Curve,
    /// How pressure drives flow — how much paint each dab lays down.
    ///
    /// **Flow rather than opacity, and that is forced by the model rather than chosen.** Opacity is
    /// a ceiling on the *whole stroke* (see [`crate::stroke`]): it is what stops overlapping dabs
    /// from building past it. A ceiling that changed halfway along a stroke would not be a ceiling.
    /// Flow is per dab, so it is the parameter pressure can honestly drive, and driving it gives the
    /// light-touch-faint-mark behaviour that pressure-to-opacity is usually reached for.
    pub flow_response: Curve,
    /// Ceiling the whole stroke may reach, `0.0..=1.0`.
    ///
    /// Per *stroke*, not per dab and not per layer: overlapping dabs build toward
    /// this and stop, but a second stroke builds on top. See [`crate::stroke`].
    pub opacity: f32,
    /// How far the line may trail the pen, in milliseconds. See [`crate::stabilizer`].
    ///
    /// Per brush rather than global, because it is a property of how a tool is used: inking wants
    /// a lot, sketching wants none, and an eraser wants whatever the hand doing the erasing wants.
    /// Same reasoning as each tool keeping its own radius.
    pub stabilization_ms: f32,
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
            flow: 1.0,
            // Identity, which is what the brush did before curves existed: size tracks pressure
            // exactly. Not necessarily the *best* default -- a slight curve suits most hands -- but
            // changing the feel of the existing brush is a separate decision from making it
            // adjustable, and only one of those is being made here.
            size_response: Curve::linear(),
            // Flat: flow ignores pressure until asked to. Full flow at every pressure is the
            // behaviour that was there before.
            flow_response: Curve::constant(1.0),
            opacity: 1.0,
            // Off by default, and that is a deliberate refusal to guess. Smoothing buys steadiness
            // with latency (see `stabilizer`), and how much an artist needs depends entirely on
            // their hand and their digitizer -- neither of which this project has measured on any
            // hardware yet. Picking a nonzero default would be inventing a constant, which
            // DECISIONS objects to; the slider states its own cost in milliseconds instead, so the
            // choice is made with the number visible. A default can be earned once real use says
            // what it should be.
            stabilization_ms: 0.0,
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

    /// The brush color as authored 8-bit sRGB, for display in a color picker.
    #[must_use]
    pub fn color_srgb8(&self) -> [u8; 3] {
        opaque_linear_premul_to_srgb8(self.color_linear_premul)
    }

    /// Effective dab radius for a given pressure in `0.0..=1.0`.
    ///
    /// Floored at half a pixel so the lightest touch still makes a mark rather than a degenerate
    /// dab: a brush that silently does nothing at low pressure reads as a broken pen.
    fn radius_for(&self, pressure: f32) -> f32 {
        (self.radius * self.size_response.at(pressure)).max(0.5)
    }

    /// Effective flow for a given pressure.
    fn flow_for(&self, pressure: f32) -> f32 {
        (self.flow * self.flow_response.at(pressure)).clamp(0.0, 1.0)
    }

    /// Build one dab at `(cx, cy)` for a given pressure.
    fn dab_at(&self, cx: f32, cy: f32, pressure: f32) -> Dab {
        Dab {
            x: cx,
            y: cy,
            radius: self.radius_for(pressure),
            hardness: self.hardness,
            flow: self.flow_for(pressure),
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
        out.push(self.dab_at(x, y, pressure));
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
            out.push(self.dab_at(px + ux * t, py + uy * t, pressure));
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

    /// The point of the whole feature: two brushes with the same numbers and different curves
    /// make genuinely different marks at the same pressure.
    ///
    /// A pencil answers from the lightest touch; an inking pen holds its width and then opens up.
    /// Both are this rasterizer with these parameters — only the mapping from pressure differs, and
    /// that is what makes them different *tools* rather than two sizes of one.
    #[test]
    fn the_size_curve_is_what_separates_a_pencil_from_a_pen() {
        let mut pencil = Brush {
            radius: 20.0,
            ..Brush::default()
        };
        let mut pen = pencil.clone();
        pencil.size_response =
            Curve::from_points(vec![(0.0, 0.0), (0.2, 0.55), (1.0, 1.0)]).expect("valid");
        pen.size_response =
            Curve::from_points(vec![(0.0, 0.0), (0.7, 0.25), (1.0, 1.0)]).expect("valid");

        let light = 0.2_f32;
        let pencil_dab = stroke(&pencil, 0.0, 0.0, light);
        let pen_dab = stroke(&pen, 0.0, 0.0, light);
        assert!(
            pencil_dab[0].radius > pen_dab[0].radius * 2.0,
            "at light pressure the pencil should already be wide: {} vs {}",
            pencil_dab[0].radius,
            pen_dab[0].radius
        );

        // And both reach full width when leaned on, or one is simply a smaller brush.
        let hard = 1.0_f32;
        for b in [&pencil, &pen] {
            let dab = stroke(b, 0.0, 0.0, hard);
            assert!(
                (dab[0].radius - 20.0).abs() < 0.1,
                "full pressure should give the full radius, got {}",
                dab[0].radius
            );
        }
    }

    /// Pressure drives *flow*, and the flow curve reaches the dabs.
    #[test]
    fn the_flow_curve_reaches_the_dabs() {
        let mut brush = Brush {
            radius: 10.0,
            flow: 1.0,
            ..Brush::default()
        };
        // Flat by default, so pressure does nothing until asked.
        assert!(
            (stroke(&brush, 0.0, 0.0, 0.2)[0].flow - 1.0).abs() < 1e-3,
            "the default flow curve should ignore pressure"
        );

        brush.flow_response = Curve::linear();
        let faint = stroke(&brush, 0.0, 0.0, 0.25);
        let firm = stroke(&brush, 0.0, 0.0, 1.0);
        assert!(
            (faint[0].flow - 0.25).abs() < 0.02,
            "a light touch should lay down little paint, got {}",
            faint[0].flow
        );
        assert!(
            (firm[0].flow - 1.0).abs() < 0.02,
            "and a firm one all of it, got {}",
            firm[0].flow
        );
    }

    /// A flat size curve is how "pressure does not affect size" is expressed, so it has to work.
    #[test]
    fn a_flat_size_curve_makes_pressure_irrelevant() {
        let brush = Brush {
            radius: 12.0,
            size_response: Curve::constant(1.0),
            ..Brush::default()
        };
        for p in [0.05_f32, 0.5, 1.0] {
            let dab = stroke(&brush, 0.0, 0.0, p);
            assert!(
                (dab[0].radius - 12.0).abs() < 0.01,
                "pressure {p} changed the radius to {}",
                dab[0].radius
            );
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
