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
use crate::dab::Dab;
use crate::modulation::{Input, Noise, Response, Source};

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
    /// What drives the radius, and how. See [`crate::modulation`].
    pub radius_response: Response,
    /// What drives the flow.
    ///
    /// **Flow rather than opacity, and that is forced by the model rather than chosen.** Opacity is
    /// a ceiling on the *whole stroke* (see [`crate::stroke`]): it is what stops overlapping dabs
    /// from building past it. A ceiling that changed halfway along a stroke would not be a ceiling.
    /// Flow is the per-dab quantity, so it is the one an input can honestly drive.
    pub flow_response: Response,
    /// What drives the edge hardness.
    pub hardness_response: Response,
    /// What drives the spacing between dabs.
    pub spacing_response: Response,
    /// Minor axis as a fraction of the major. One is a circle.
    pub roundness: f32,
    /// What drives the roundness.
    pub roundness_response: Response,
    /// Rotation of the major axis, in **turns**.
    ///
    /// Turns rather than degrees so that a full turn is 1.0, and therefore so that pointing this
    /// at `Source::Direction` with an identity curve makes the dab follow the stroke exactly —
    /// which is a chisel nib, and the reason modulation was generalised before this landed.
    pub angle: f32,
    /// What drives the angle.
    pub angle_response: Response,
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
            // Radius follows pressure exactly, which is what the brush did before any of this
            // existed. Not necessarily the *best* default -- a gentler curve suits most hands --
            // but changing how the existing brush feels is a separate decision from making it
            // adjustable, and only one of those is being taken.
            radius_response: Response::following(Source::Pressure),
            // The rest ignore their input until asked to, so nothing about the old brush changed.
            flow_response: Response::fixed(),
            hardness_response: Response::fixed(),
            spacing_response: Response::fixed(),
            // A circle, unrotated: exactly the dab that existed before shape did.
            roundness: 1.0,
            roundness_response: Response::fixed(),
            angle: 0.0,
            angle_response: Response::fixed(),
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
    /// When that sample arrived, so speed is a distance over a *time* rather than over a sample
    /// count. Sample rate varies with pen speed on real hardware, so the two are not the same.
    last_time_ms: f64,
    /// Distance accumulated since the previous stamped dab.
    residual: f32,
    /// Per-dab noise, for `Source::Random`.
    noise: Noise,
}

impl StrokeState {
    pub fn new() -> Self {
        Self {
            last: None,
            last_time_ms: 0.0,
            residual: 0.0,
            noise: Noise::default(),
        }
    }
}

impl Default for StrokeState {
    fn default() -> Self {
        Self::new()
    }
}

/// One input sample, as the caller has it.
///
/// Position, pressure, tilt and a timestamp — everything that comes off a pen. Velocity and heading
/// are *not* here: they are properties of the path between samples, and the brush is the thing
/// walking it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    /// Tilt from vertical, in radians.
    pub tilt: f32,
    pub time_ms: f64,
}

impl Sample {
    /// A sample with no tilt and no clock, for callers that have neither.
    #[must_use]
    pub fn at(x: f32, y: f32, pressure: f32) -> Self {
        Self {
            x,
            y,
            pressure,
            tilt: 0.0,
            time_ms: 0.0,
        }
    }
}

/// A seed for one stroke's noise, from where and when it started.
///
/// Position and clock mixed together: position alone repeats if you start two strokes at the same
/// place, and the clock alone is coarse enough that two strokes in the same millisecond would
/// match. Neither is a strong hash and neither needs to be — this only has to differ between
/// strokes a person could draw.
fn seed_from(at: Sample) -> u32 {
    let bits = at.x.to_bits() ^ at.y.to_bits().rotate_left(16);
    #[allow(clippy::cast_possible_truncation)]
    let clock = (at.time_ms * 1000.0) as u64 as u32;
    bits ^ clock.rotate_left(8)
}

impl StrokeState {
    /// Assemble one dab's worth of input, drawing a fresh random value.
    fn input_for(&mut self, at: Sample, velocity: f32, direction: f32) -> Input {
        Input {
            pressure: at.pressure,
            tilt: at.tilt,
            velocity,
            direction,
            random: self.noise.draw(),
        }
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

    /// Effective dab radius for one dab's worth of input.
    ///
    /// Floored at half a pixel so the lightest touch still makes a mark rather than a degenerate
    /// dab: a brush that silently does nothing at low pressure reads as a broken pen.
    fn radius_for(&self, input: &Input) -> f32 {
        (self.radius * self.radius_response.factor(input)).max(0.5)
    }

    /// Effective spacing, as a fraction of diameter.
    ///
    /// Clamped to a sane fraction. What actually bounds the dab count is the floor on `step` in
    /// `stroke_to` — a zero spacing would otherwise ask for infinitely many dabs — so this clamp is
    /// belt to that braces, keeping the *value* meaningful rather than being the thing that stops
    /// the loop.
    fn spacing_for(&self, input: &Input) -> f32 {
        (self.spacing * self.spacing_response.factor(input)).clamp(0.01, 1.0)
    }

    /// Build one dab at `(cx, cy)` from its input.
    fn dab_at(&self, cx: f32, cy: f32, input: &Input) -> Dab {
        Dab {
            x: cx,
            y: cy,
            radius: self.radius_for(input),
            hardness: (self.hardness * self.hardness_response.factor(input)).clamp(0.0, 1.0),
            flow: (self.flow * self.flow_response.factor(input)).clamp(0.0, 1.0),
            roundness: (self.roundness * self.roundness_response.factor(input))
                .clamp(crate::dab::MIN_ROUNDNESS, 1.0),
            angle: (self.angle * self.angle_response.factor(input)) * std::f32::consts::TAU,
            color_linear_premul: self.color_linear_premul,
        }
    }

    /// Every modulatable parameter, for a UI to lay out in a loop.
    ///
    /// Returned as a list rather than named one at a time so the panel cannot fall behind the
    /// engine: adding a parameter here makes an editor for it appear, rather than needing a second
    /// edit somewhere that is easy to forget.
    pub fn responses_mut(&mut self) -> [(&'static str, &mut Response); 6] {
        [
            ("Size", &mut self.radius_response),
            ("Flow", &mut self.flow_response),
            ("Hardness", &mut self.hardness_response),
            ("Spacing", &mut self.spacing_response),
            ("Roundness", &mut self.roundness_response),
            ("Angle", &mut self.angle_response),
        ]
    }

    /// Begin a stroke: emit the initial dab at the first sample.
    pub fn stroke_begin(&self, out: &mut Vec<Dab>, state: &mut StrokeState, at: Sample) {
        // Seed from where and when the stroke began, so two strokes do not draw the same numbers.
        // The default seed is a constant, which made every stroke speckle *identically* -- the
        // patterning randomness exists to avoid. Reproducibility is not lost by this: history
        // stores the dabs, so a redo replays what was drawn rather than re-rolling.
        state.noise = Noise::new(seed_from(at));

        // A stroke begins from rest, so there is no speed and no heading yet. Reading either from
        // a single sample would invent one.
        let input = state.input_for(at, 0.0, 0.0);
        out.push(self.dab_at(at.x, at.y, &input));
        state.last = Some((at.x, at.y));
        state.last_time_ms = at.time_ms;
        state.residual = 0.0;
    }

    /// Continue a stroke to a new sample, emitting evenly spaced dabs along the
    /// segment from the previous sample so speed doesn't create gaps.
    pub fn stroke_to(&self, out: &mut Vec<Dab>, state: &mut StrokeState, at: Sample) {
        let Some((px, py)) = state.last else {
            self.stroke_begin(out, state, at);
            return;
        };

        let dx = at.x - px;
        let dy = at.y - py;
        let seg_len = dx.hypot(dy);
        if seg_len <= f32::EPSILON {
            return;
        }
        let (ux, uy) = (dx / seg_len, dy / seg_len);

        // Both are properties of the *path*, which is why the brush computes them rather than
        // asking the caller for them. Heading as a fraction of a turn, so a curve mapping it
        // straight through makes a dab follow the stroke.
        let dt = (at.time_ms - state.last_time_ms).max(0.0) as f32;
        let velocity = if dt > 0.0 { seg_len / dt } else { 0.0 };
        let direction = uy.atan2(ux).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;

        // Walk along the segment, emitting a dab every `step` pixels, carrying
        // leftover distance in `residual` so spacing is continuous across
        // segments.
        //
        // The input is re-drawn per dab, because `Source::Random` has to differ between them --
        // one draw for the whole segment would make a scattering brush emit rows of identical
        // dabs.
        let mut traveled = -state.residual;
        loop {
            let input = state.input_for(at, velocity, direction);
            let step = (self.radius_for(&input) * 2.0 * self.spacing_for(&input)).max(0.5);
            if traveled + step > seg_len {
                // Nothing more fits; `traveled` is where the last dab landed, and the remainder
                // carries into the next segment.
                break;
            }
            traveled += step;
            out.push(self.dab_at(px + ux * traveled, py + uy * traveled, &input));
        }
        state.residual = seg_len - traveled;
        state.last = Some((at.x, at.y));
        state.last_time_ms = at.time_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::Curve;
    use crate::modulation::Source;

    /// Emit a straight horizontal stroke and return the dabs it produced.
    fn stroke(brush: &Brush, from: f32, to: f32, pressure: f32) -> Vec<Dab> {
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        brush.stroke_begin(&mut dabs, &mut state, Sample::at(from, 0.0, pressure));
        brush.stroke_to(&mut dabs, &mut state, Sample::at(to, 0.0, pressure));
        dabs
    }

    #[test]
    fn beginning_a_stroke_emits_exactly_one_dab() {
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        Brush::default().stroke_begin(&mut dabs, &mut state, Sample::at(5.0, 7.0, 1.0));
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
        b.stroke_begin(&mut dabs, &mut state, Sample::at(0.0, 0.0, 1.0));
        // Ten 10px segments == one 100px segment, as far as spacing goes.
        for i in 1..=10 {
            b.stroke_to(&mut dabs, &mut state, Sample::at(i as f32 * 10.0, 0.0, 1.0));
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
        pencil.radius_response.curve =
            Curve::from_points(vec![(0.0, 0.0), (0.2, 0.55), (1.0, 1.0)]).expect("valid");
        pen.radius_response.curve =
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

        brush.flow_response = Response::following(Source::Pressure);
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
            radius_response: Response::fixed(),
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

    /// Velocity reaches the dabs, computed from the clock rather than from sample count.
    ///
    /// This is the source that gives tapered ink: a fast flick thins the line without the artist
    /// doing anything. Two identical strokes, differing only in how much time passed, must produce
    /// different marks -- and a brush that measured speed per *sample* instead of per millisecond
    /// would call these two identical, because they have the same number of samples.
    #[test]
    fn velocity_is_measured_against_the_clock() {
        let mut brush = Brush {
            radius: 20.0,
            ..Brush::default()
        };
        // Thinner the faster you go, which is what a taper is.
        brush.radius_response = Response {
            source: Source::Velocity,
            curve: Curve::from_points(vec![(0.0, 1.0), (1.0, 0.1)]).expect("valid"),
        };

        let run = |dt: f64| {
            let mut dabs = Vec::new();
            let mut state = StrokeState::new();
            brush.stroke_begin(&mut dabs, &mut state, Sample::at(0.0, 0.0, 1.0));
            brush.stroke_to(
                &mut dabs,
                &mut state,
                Sample {
                    x: 300.0,
                    y: 0.0,
                    pressure: 1.0,
                    tilt: 0.0,
                    time_ms: dt,
                },
            );
            // The last dab, well into the segment, where the speed is being felt.
            dabs.last().expect("dabs").radius
        };

        let slow = run(1000.0);
        let fast = run(50.0);
        assert!(
            fast < slow * 0.5,
            "the same distance covered faster should give a thinner line: {fast} vs {slow}"
        );
    }

    /// Tilt reaches the dabs.
    #[test]
    fn tilt_reaches_the_dabs() {
        let mut brush = Brush {
            radius: 20.0,
            ..Brush::default()
        };
        brush.radius_response = Response::following(Source::Tilt);

        let run = |tilt: f32| {
            let mut dabs = Vec::new();
            let mut state = StrokeState::new();
            brush.stroke_begin(
                &mut dabs,
                &mut state,
                Sample {
                    x: 0.0,
                    y: 0.0,
                    pressure: 1.0,
                    tilt,
                    time_ms: 0.0,
                },
            );
            dabs[0].radius
        };

        let upright = run(0.0);
        let leaning = run(crate::modulation::TILT_FULL);
        assert!(
            upright < 1.0,
            "an upright pen should be at the floor, got {upright}"
        );
        assert!(
            (leaning - 20.0).abs() < 0.1,
            "a fully leant pen should give the full radius, got {leaning}"
        );
    }

    /// Two strokes must not draw the same random numbers.
    ///
    /// The seed was a constant, so every stroke speckled identically -- draw the same line twice
    /// and get the same dabs, which is the patterning randomness exists to avoid. Found by being
    /// asked what the seed was.
    #[test]
    fn two_strokes_do_not_share_their_randomness() {
        let mut brush = Brush {
            radius: 20.0,
            ..Brush::default()
        };
        brush.radius_response = Response::following(Source::Random);

        let run = |x: f32, t: f64| {
            let mut dabs = Vec::new();
            let mut state = StrokeState::new();
            let start = Sample {
                x,
                y: 0.0,
                pressure: 1.0,
                tilt: 0.0,
                time_ms: t,
            };
            brush.stroke_begin(&mut dabs, &mut state, start);
            brush.stroke_to(
                &mut dabs,
                &mut state,
                Sample {
                    x: x + 300.0,
                    ..start
                },
            );
            dabs.iter().map(|d| d.radius).collect::<Vec<_>>()
        };

        // Different place, same instant.
        assert_ne!(
            run(0.0, 5.0),
            run(50.0, 5.0),
            "position did not reach the seed"
        );
        // Same place, different instant.
        assert_ne!(
            run(0.0, 5.0),
            run(0.0, 9.0),
            "the clock did not reach the seed"
        );
        // And the same stroke twice is still the same, which is what makes a brush reproducible.
        assert_eq!(run(0.0, 5.0), run(0.0, 5.0));
    }

    /// Randomness differs *between dabs*, not once per segment.
    ///
    /// One draw for a whole segment would make a scattering brush emit rows of identical dabs,
    /// which looks like a bug and is the easy mistake when the input is assembled outside the loop.
    #[test]
    fn randomness_varies_between_dabs_within_a_segment() {
        let mut brush = Brush {
            radius: 20.0,
            ..Brush::default()
        };
        brush.radius_response = Response::following(Source::Random);

        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        brush.stroke_begin(&mut dabs, &mut state, Sample::at(0.0, 0.0, 1.0));
        brush.stroke_to(&mut dabs, &mut state, Sample::at(400.0, 0.0, 1.0));

        assert!(dabs.len() > 8, "not enough dabs to compare: {}", dabs.len());
        let first = dabs[1].radius;
        assert!(
            dabs.iter().skip(1).any(|d| (d.radius - first).abs() > 0.5),
            "every dab in the segment got the same random value"
        );
    }

    /// Hardness answers its response too.
    ///
    /// Checked as a *change*, not merely as a value in range: a brush that ignored the response
    /// entirely would still emit a perfectly legal hardness, which is how this went untested at
    /// first.
    #[test]
    fn hardness_answers_its_response() {
        let mut brush = Brush {
            hardness: 1.0,
            ..Brush::default()
        };
        brush.hardness_response = Response::following(Source::Pressure);

        let run = |pressure: f32| {
            let mut dabs = Vec::new();
            let mut state = StrokeState::new();
            brush.stroke_begin(&mut dabs, &mut state, Sample::at(0.0, 0.0, pressure));
            dabs[0].hardness
        };

        let light = run(0.25);
        let firm = run(1.0);
        assert!(
            (light - 0.25).abs() < 0.02,
            "a light touch should soften the edge, got {light}"
        );
        assert!(
            (firm - 1.0).abs() < 0.02,
            "and a firm one should give the full hardness, got {firm}"
        );
    }

    /// Any source can drive any parameter, which is the point of the whole arrangement.
    #[test]
    fn any_source_can_drive_any_parameter() {
        for source in Source::ALL {
            let mut brush = Brush::default();
            for (_, response) in brush.responses_mut() {
                *response = Response::following(source);
            }
            let mut dabs = Vec::new();
            let mut state = StrokeState::new();
            brush.stroke_begin(&mut dabs, &mut state, Sample::at(0.0, 0.0, 0.5));
            brush.stroke_to(&mut dabs, &mut state, Sample::at(120.0, 40.0, 0.5));

            assert!(!dabs.is_empty(), "{source:?} produced no dabs at all");
            for d in &dabs {
                assert!(
                    d.radius >= 0.5,
                    "{source:?} gave a degenerate radius {}",
                    d.radius
                );
                assert!(
                    (0.0..=1.0).contains(&d.flow),
                    "{source:?} gave flow {} outside its range",
                    d.flow
                );
                assert!(
                    (0.0..=1.0).contains(&d.hardness),
                    "{source:?} gave hardness {} outside its range",
                    d.hardness
                );
            }
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
        b.stroke_begin(&mut dabs, &mut state, Sample::at(10.0, 10.0, 1.0));
        b.stroke_to(&mut dabs, &mut state, Sample::at(10.0, 10.0, 1.0));
        assert_eq!(dabs.len(), 1);
    }

    /// `stroke_to` without a preceding `stroke_begin` must not silently drop the
    /// input; it should behave as the start of a stroke.
    #[test]
    fn stroke_to_without_begin_starts_the_stroke() {
        let mut dabs = Vec::new();
        let mut state = StrokeState::new();
        Brush::default().stroke_to(&mut dabs, &mut state, Sample::at(3.0, 4.0, 1.0));
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
        b.stroke_begin(&mut dabs, &mut state, Sample::at(0.0, 0.0, 1.0));
        // (30, 40) is 50 units away, so 50/4 = 12 further dabs.
        b.stroke_to(&mut dabs, &mut state, Sample::at(30.0, 40.0, 1.0));
        assert_eq!(dabs.len(), 13);
        // Consecutive dabs must be one spacing step apart along the diagonal.
        for pair in dabs.windows(2) {
            let d = ((pair[1].x - pair[0].x).powi(2) + (pair[1].y - pair[0].y).powi(2)).sqrt();
            assert!((d - 4.0).abs() < 1e-3, "spacing {d} != 4.0");
        }
    }
}
