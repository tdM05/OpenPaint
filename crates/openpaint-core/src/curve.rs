//! Response curves — how an input like pressure drives a brush parameter.
//!
//! # Why a curve and not a number
//!
//! An inking pen and a pencil are not two sizes of the same round dab. What separates them is
//! mostly *how they answer pressure*: a pen holds its width until you lean on it and then opens up
//! quickly; a pencil responds from the lightest touch and keeps responding. Both are the same
//! rasterizer with the same parameters — only the mapping from pressure differs.
//!
//! That is why this is the first piece of brush depth (DECISIONS §4.2). It is also why brush
//! *presets* wait for it: presets over the parameters we had would have been six slightly different
//! round brushes.
//!
//! # Control points, evaluated monotonically
//!
//! A list of points, interpolated with monotone cubic Hermite (Fritsch–Carlson). Two properties
//! matter and neither is decoration:
//!
//! - **It passes through the points.** What the artist places is what they get, so a curve editor
//!   can be trusted.
//! - **It never overshoots.** Ordinary cubic interpolation dips below the lowest point and rises
//!   above the highest between widely spaced control points. On a size curve that means a stroke
//!   that briefly *inverts* its response — pressing harder making a thinner line — for no reason
//!   the artist can see. Monotone interpolation cannot do it.
//!
//! A straight line between two points is a special case of the same thing, so "no curve" needs no
//! separate representation and no flag.

/// A mapping from an input in `0..=1` to an output in `0..=1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    /// At least two points, sorted by x, all within `0..=1`.
    points: Vec<(f32, f32)>,
}

impl Default for Curve {
    fn default() -> Self {
        Self::linear()
    }
}

impl Curve {
    /// The identity: output follows input exactly.
    #[must_use]
    pub fn linear() -> Self {
        Self {
            points: vec![(0.0, 0.0), (1.0, 1.0)],
        }
    }

    /// A constant output, for a parameter pressure should not touch.
    ///
    /// The reason there is no "pressure affects size" checkbox: a flat curve *is* that switch
    /// turned off, and one representation cannot disagree with itself.
    #[must_use]
    pub fn constant(y: f32) -> Self {
        let y = y.clamp(0.0, 1.0);
        Self {
            points: vec![(0.0, y), (1.0, y)],
        }
    }

    /// Build from control points, or `None` if they do not describe a curve.
    ///
    /// Rejects fewer than two points and any x out of order. Sorting them silently would hide an
    /// editor bug rather than surface it, and a curve with one point has no defined shape.
    #[must_use]
    pub fn from_points(points: Vec<(f32, f32)>) -> Option<Self> {
        if points.len() < 2 {
            return None;
        }
        if points.windows(2).any(|w| w[1].0 <= w[0].0) {
            return None;
        }
        if points
            .iter()
            .any(|p| !(0.0..=1.0).contains(&p.0) || !(0.0..=1.0).contains(&p.1))
        {
            return None;
        }
        Some(Self { points })
    }

    /// The control points, for an editor to draw and drag.
    #[must_use]
    pub fn points(&self) -> &[(f32, f32)] {
        &self.points
    }

    /// Evaluate the curve.
    ///
    /// Inputs outside `0..=1` are clamped: pressure should already be normalised, and extrapolating
    /// a curve past its ends is how a brush ends up with a negative radius.
    #[must_use]
    pub fn at(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let p = &self.points;

        // Outside the authored range, hold the end value rather than extending the last slope.
        if x <= p[0].0 {
            return p[0].1;
        }
        if x >= p[p.len() - 1].0 {
            return p[p.len() - 1].1;
        }

        let i = p
            .windows(2)
            .position(|w| x >= w[0].0 && x <= w[1].0)
            .unwrap_or(0);
        let (x0, y0) = p[i];
        let (x1, y1) = p[i + 1];
        let h = x1 - x0;
        if h <= f32::EPSILON {
            return y0;
        }

        let (m0, m1) = self.tangents(i);
        let t = (x - x0) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        // Hermite basis.
        let value = (2.0 * t3 - 3.0 * t2 + 1.0) * y0
            + (t3 - 2.0 * t2 + t) * h * m0
            + (-2.0 * t3 + 3.0 * t2) * y1
            + (t3 - t2) * h * m1;
        value.clamp(0.0, 1.0)
    }

    /// Tangents at the ends of segment `i`, clamped so the curve cannot overshoot.
    ///
    /// Fritsch–Carlson: where a segment is flat both its tangents are zero, and elsewhere a tangent
    /// is limited to three times the neighbouring secant. That limit is the whole reason this is not
    /// a plain cubic spline — without it, a curve through widely spaced points bulges past them, and
    /// on a size curve that reads as pressing harder briefly making a *thinner* line.
    fn tangents(&self, i: usize) -> (f32, f32) {
        let p = &self.points;
        let secant = |j: usize| (p[j + 1].1 - p[j].1) / (p[j + 1].0 - p[j].0);

        let d = secant(i);
        let before = if i == 0 { d } else { secant(i - 1) };
        let after = if i + 2 >= p.len() { d } else { secant(i + 1) };

        let raw0 = if i == 0 { d } else { (before + d) / 2.0 };
        let raw1 = if i + 2 >= p.len() {
            d
        } else {
            (d + after) / 2.0
        };

        if d.abs() <= f32::EPSILON {
            // A flat segment must stay flat, or the curve wanders off it and comes back.
            return (0.0, 0.0);
        }
        let limit = 3.0 * d;
        let clamp_to = |m: f32| {
            if (m > 0.0) == (d > 0.0) {
                if d > 0.0 {
                    m.min(limit)
                } else {
                    m.max(limit)
                }
            } else {
                // A tangent pointing against the segment would make the curve reverse inside it.
                0.0
            }
        };
        (clamp_to(raw0), clamp_to(raw1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_the_identity() {
        let c = Curve::linear();
        for x in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
            assert!((c.at(x) - x).abs() < 1e-4, "at({x}) = {}", c.at(x));
        }
    }

    #[test]
    fn constant_ignores_its_input() {
        let c = Curve::constant(0.4);
        for x in [0.0_f32, 0.3, 1.0] {
            assert!((c.at(x) - 0.4).abs() < 1e-4);
        }
    }

    /// What the artist places is what they get, or a curve editor cannot be trusted.
    #[test]
    fn the_curve_passes_through_its_control_points() {
        let c = Curve::from_points(vec![(0.0, 0.1), (0.3, 0.8), (0.7, 0.2), (1.0, 1.0)])
            .expect("valid points");
        for (x, y) in c.points() {
            assert!(
                (c.at(*x) - y).abs() < 1e-3,
                "at({x}) = {} but the point says {y}",
                c.at(*x)
            );
        }
    }

    /// The reason for monotone interpolation rather than a plain spline.
    ///
    /// A plain cubic through these points bulges well past 1.0 between the last two and dips below
    /// 0.0 near the start. On a size curve that is a stroke that gets *thinner* as you press
    /// harder, over part of its range, for no visible reason.
    #[test]
    fn a_rising_curve_never_dips_or_overshoots() {
        let c = Curve::from_points(vec![(0.0, 0.0), (0.1, 0.02), (0.9, 0.98), (1.0, 1.0)])
            .expect("valid points");

        let mut previous = -1.0_f32;
        for i in 0..=200 {
            let x = i as f32 / 200.0;
            let y = c.at(x);
            assert!((0.0..=1.0).contains(&y), "at({x}) left the range: {y}");
            assert!(
                y >= previous - 1e-4,
                "at({x}) went backwards: {previous} -> {y}"
            );
            previous = y;
        }
    }

    /// A flat run has to stay flat, or a brush with a dead zone would wobble through it.
    #[test]
    fn a_flat_segment_stays_flat() {
        let c = Curve::from_points(vec![(0.0, 0.5), (0.5, 0.5), (1.0, 1.0)]).expect("valid");
        for i in 0..=50 {
            let x = i as f32 / 100.0;
            assert!(
                (c.at(x) - 0.5).abs() < 1e-3,
                "at({x}) = {} inside the flat run",
                c.at(x)
            );
        }
    }

    #[test]
    fn input_outside_the_range_is_held_not_extrapolated() {
        let c = Curve::from_points(vec![(0.0, 0.2), (1.0, 0.9)]).expect("valid");
        assert!((c.at(-5.0) - 0.2).abs() < 1e-4);
        assert!((c.at(5.0) - 0.9).abs() < 1e-4);
    }

    /// Bad control points are refused rather than quietly repaired: silently sorting them would
    /// hide an editor bug instead of surfacing it.
    #[test]
    fn malformed_points_are_refused() {
        assert!(Curve::from_points(vec![]).is_none(), "no points");
        assert!(
            Curve::from_points(vec![(0.0, 0.0)]).is_none(),
            "one point has no shape"
        );
        assert!(
            Curve::from_points(vec![(0.5, 0.0), (0.2, 1.0)]).is_none(),
            "out of order"
        );
        assert!(
            Curve::from_points(vec![(0.0, 0.0), (0.0, 1.0)]).is_none(),
            "two points at the same x is a vertical jump, not a curve"
        );
        assert!(
            Curve::from_points(vec![(0.0, 0.0), (1.0, 2.0)]).is_none(),
            "out of range"
        );
    }

    /// A curve that answers from the lightest touch, versus one that holds then opens up — the
    /// difference between a pencil and an inking pen, and the whole point of the feature.
    #[test]
    fn different_curves_give_genuinely_different_responses() {
        let pencil = Curve::from_points(vec![(0.0, 0.0), (0.2, 0.55), (1.0, 1.0)]).expect("valid");
        let pen = Curve::from_points(vec![(0.0, 0.0), (0.7, 0.25), (1.0, 1.0)]).expect("valid");

        let light = 0.2_f32;
        assert!(
            pencil.at(light) > pen.at(light) + 0.25,
            "at light pressure the pencil should already be responding ({} vs {})",
            pencil.at(light),
            pen.at(light)
        );
        // Both still reach full width when leaned on, or one of them is simply a smaller brush.
        assert!((pencil.at(1.0) - 1.0).abs() < 1e-3);
        assert!((pen.at(1.0) - 1.0).abs() < 1e-3);
    }
}
