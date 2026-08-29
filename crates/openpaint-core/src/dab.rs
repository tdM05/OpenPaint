//! The dab — one stamp of the brush tip, and the engine's central boundary.
//!
//! A stroke is not a line. It is a sequence of overlapping stamps placed along
//! the path at regular spacing, and each stamp is a [`Dab`]. Spacing (rather than
//! per-input-sample stamping) is what gives a stroke consistent density
//! regardless of how fast the pen moves or how irregularly samples arrive.
//!
//! # Why this type exists: the per-dab / per-pixel boundary
//!
//! Everything in the brush engine falls on one side of a line, and the two sides
//! have wildly different cost:
//!
//! | | dabs per stroke | pixels per stroke |
//! |---|---|---|
//! | small brush (r=8) | ~100 | ~32,000 |
//! | large soft brush (r=200) | ~20 | ~3,200,000 |
//!
//! Three to four orders of magnitude apart. So:
//!
//! - **Per-dab work is effectively free.** Deciding where a dab goes, how big it
//!   is, its angle, its flow — hundreds of times per stroke. This side can be
//!   dynamic, composable, data-driven, and user-authored at negligible cost.
//! - **Per-pixel work is the hot loop.** Computing coverage and blending runs
//!   millions of times per stroke, and will run in a fragment shader. This side
//!   must be a *fixed* compiled program: WGSL has no function pointers and
//!   WebGPU has no dynamic shader linking, so a dynamically composed per-pixel
//!   pipeline degrades into either shader-permutation explosion (compile hitches
//!   mid-stroke — fatal for feel) or an uber-shader whose register pressure makes
//!   a plain round brush run at the speed of the most complex brush. On the
//!   Surface-class target (DECISIONS §2) that is not affordable.
//!
//! `Dab` is the type that makes this boundary explicit, and it is deliberately
//! *uniform*: anything shaped `Dab -> Dab` composes cleanly and in any order.
//! That is the property that makes composition meaningful at all — the same
//! reason Blender modifiers compose (every one is `Mesh -> Mesh`) and the reason
//! a heterogeneous "any brush stage anywhere" pipeline does not.
//!
//! See DECISIONS §4a and §4c.

use crate::curve::Curve;

/// What decides a dab's coverage.
///
/// A dab's *geometry* — where it is, how big, how round, at what angle — is the same either way.
/// This is only the answer to "how covered is this point", and putting the choice here rather than
/// in two rasterizers is what keeps spacing, modulation, blending, tile touching and the GPU quad
/// from knowing that bitmap tips exist at all.
///
/// Two arms and not more, deliberately:
///
/// - [`Tip::Round`] — coverage from distance, through an edge profile (see §4o). Cheap, resolution
///   independent, and what every brush was before bitmap tips.
/// - [`Tip::Stamp`] — coverage read from an image. What makes a chalk, a bristle brush or a
///   screentone dot possible at all.
///
/// **Hardness applies to `Round` only.** A stamp already contains its own edge — that is most of
/// what it *is* — so a hardness control over the top would be a second, contradictory answer to the
/// same question. See [`Dab::coverage_at`].
#[derive(Clone, Debug, PartialEq)]
pub enum Tip {
    Round(Curve),
    /// Shared rather than owned: a stamp is hundreds of kilobytes and a `Brush` is cloned for every
    /// queued stroke, so the tip travels with the stroke by pointer.
    Stamp(std::sync::Arc<crate::Stamp>),
}

impl Default for Tip {
    fn default() -> Self {
        Self::Round(linear_falloff())
    }
}

impl Tip {
    /// Whether this is a bitmap tip.
    #[must_use]
    pub fn is_stamp(&self) -> bool {
        matches!(self, Self::Stamp(_))
    }

    /// The edge profile, when there is one to edit.
    ///
    /// `None` for a stamp, which is what the UI needs to know to stop offering an edge control that
    /// would do nothing.
    #[must_use]
    pub fn falloff(&self) -> Option<&Curve> {
        match self {
            Self::Round(curve) => Some(curve),
            Self::Stamp(_) => None,
        }
    }

    /// The edge profile, to edit.
    pub fn falloff_mut(&mut self) -> Option<&mut Curve> {
        match self {
            Self::Round(curve) => Some(curve),
            Self::Stamp(_) => None,
        }
    }

    /// The bitmap, when there is one.
    #[must_use]
    pub fn stamp(&self) -> Option<&std::sync::Arc<crate::Stamp>> {
        match self {
            Self::Round(_) => None,
            Self::Stamp(stamp) => Some(stamp),
        }
    }
}

/// The narrowest a dab may be, as a fraction of its radius.
///
/// Not zero: roundness divides, and a dab with no width would cover nothing anywhere while costing
/// the same to rasterize. A fiftieth of the major axis is already a hairline at any usable size.
pub const MIN_ROUNDNESS: f32 = 0.02;

/// How many points the edge profile is sampled at when handed to the GPU.
///
/// The shader cannot evaluate a spline per fragment, so the curve arrives as a lookup table. Thirty
/// two samples across a ramp that is rarely more than a few dozen pixels wide, read with linear
/// interpolation between entries, is finer than the `f16` accumulation buffer can record — which is
/// the bar worth meeting, rather than any particular smoothness.
pub const FALLOFF_SAMPLES: usize = 32;

/// The default edge profile: a straight ramp from full coverage at the core to none at the rim.
///
/// Exactly what the falloff was before it was a curve, so making it adjustable changed no existing
/// brush.
#[must_use]
pub fn linear_falloff() -> Curve {
    Curve::from_points(vec![(0.0, 1.0), (1.0, 0.0)]).expect("a two-point ramp is a valid curve")
}

/// One stamp of the brush tip: pure data, no pixels, no canvas, no GPU.
///
/// Deliberately small and `Copy`. A stroke produces hundreds of these into a
/// reusable buffer, and they are also exactly what a GPU rasterizer will upload
/// as per-instance data — so keeping this plain and flat is not premature
/// optimization, it's the eventual wire format.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dab {
    /// Center in canvas pixels.
    pub x: f32,
    pub y: f32,
    /// Radius in canvas pixels, after modulation. The **major** axis of the dab.
    ///
    /// Major rather than mean, so narrowing a dab never makes it reach further than its radius —
    /// which is what lets the bounds and the GPU quad stay the same for every shape.
    pub radius: f32,
    /// Edge hardness in `0.0..=1.0`: **1 = hard edge, 0 = fully soft falloff**.
    ///
    /// Oriented to match Photoshop and CSP, where Hardness 100% is a crisp edge
    /// (DECISIONS §1a). Getting this backwards would invert every brush preset we
    /// ever ship, so there is a test pinning the convention.
    pub hardness: f32,
    /// How much paint this dab deposits, in `0.0..=1.0`.
    ///
    /// Kept separate from `color_linear_premul` because the accumulation model
    /// needs it separate: within one stroke, flow *accumulates* toward the
    /// stroke's opacity ceiling (see [`crate::stroke`]). Folding it into the
    /// color would make a single dab look right but make overlapping dabs wrong.
    ///
    /// Per-dab rather than per-stroke because pressure-to-flow is a standard
    /// mapping in Photoshop and CSP. Opacity, by contrast, is a *stroke*-level
    /// ceiling and so lives on the brush, not here.
    pub flow: f32,
    /// Minor axis as a fraction of the major, `0.0..=1.0`. One is a circle.
    ///
    /// With [`Dab::angle`], this is what makes a chisel nib: a stroke thick across its travel and
    /// thin along it, which is most of what inked line weight actually is.
    pub roundness: f32,
    /// Rotation of the major axis, in radians, clockwise on screen.
    ///
    /// Radians here and turns on the brush: the brush's angle is a *parameter* that modulation
    /// scales, and modulation works in `0..=1`, so turns are its natural unit. By the time a dab
    /// exists the value is geometry, and geometry is radians.
    pub angle: f32,
    /// Color, linear and premultiplied (see [`crate::color`]). Normally opaque
    /// (alpha 1.0); coverage and flow are applied when the dab is rasterized.
    pub color_linear_premul: [f32; 4],
}

impl Dab {
    /// Coverage of this dab at a point, in `0.0..=1.0`.
    ///
    /// This is the falloff curve — the single most important function for making
    /// the brush feel like Photoshop's or CSP's soft round (docs Q7a). It lives
    /// here, on the dab, so that the CPU reference rasterizer and the GPU shader
    /// are demonstrably computing the *same* curve rather than two drifting
    /// copies of it.
    ///
    /// Currently a linear ramp from the hard core to the edge. Photoshop's is a
    /// specific tuned curve, not linear, so this is a placeholder that is
    /// correct-shaped but not yet correct-valued.
    #[must_use]
    pub fn coverage_at(&self, px: f32, py: f32, tip: &Tip) -> f32 {
        match tip {
            Tip::Round(falloff) => self.coverage_at_distance(self.distance_to(px, py), falloff),
            Tip::Stamp(stamp) => {
                let (u, v) = self.stamp_coords(px, py);
                // Outside the dab's own square the stamp does not reach, and clamping instead
                // would smear the image's edge texels across the rest of the page.
                if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                    return 0.0;
                }
                stamp.sample(u, v)
            }
        }
    }

    /// Where a page point falls within the stamp image, as `0..=1` on each axis.
    ///
    /// The same frame transform [`Dab::distance_to`] uses — rotated back by the dab's angle, minor
    /// axis stretched to the major — so a stamp squashes and turns with roundness and angle exactly
    /// as a round tip does. Without that, the two kinds of tip would answer differently to the same
    /// controls.
    ///
    /// The stamp is mapped across the dab's full diameter, so its outer edge sits on the radius and
    /// nothing about pixel bounds or tile touching changes.
    #[must_use]
    pub fn stamp_coords(&self, px: f32, py: f32) -> (f32, f32) {
        let dx = px - self.x;
        let dy = py - self.y;
        let (sin, cos) = self.angle.sin_cos();
        let rx = dx * cos + dy * sin;
        let ry = (-dx * sin + dy * cos) / self.roundness.clamp(MIN_ROUNDNESS, 1.0);
        let d = self.radius.max(f32::MIN_POSITIVE) * 2.0;
        (rx / d + 0.5, ry / d + 0.5)
    }

    /// Distance from the centre, measured in the dab's own frame.
    ///
    /// The point is rotated back by the dab's angle and the minor axis stretched to match the
    /// major, so an ellipse becomes a circle and the falloff below needs to know nothing about
    /// shape. Two things follow from doing it this way rather than with a separate elliptical
    /// falloff: hardness means the same thing at every roundness, and the result is still bounded
    /// by `radius`, so nothing that reasons about a dab's extent has to change.
    #[must_use]
    pub fn distance_to(&self, px: f32, py: f32) -> f32 {
        let dx = px - self.x;
        let dy = py - self.y;
        let (sin, cos) = self.angle.sin_cos();
        let rx = dx * cos + dy * sin;
        let ry = -dx * sin + dy * cos;
        // Floored so a fully flattened dab is a very thin ellipse rather than a division by zero.
        rx.hypot(ry / self.roundness.clamp(MIN_ROUNDNESS, 1.0))
    }

    /// Coverage as a function of distance from the dab center.
    ///
    /// `falloff` is the *edge profile*: it maps how far across the ramp a point is, from the edge
    /// of the solid core to the radius, onto how much coverage is left there.
    ///
    /// **Not modulation.** Its x axis is a distance within the dab, not a pen input, so it is a
    /// plain [`Curve`] rather than a [`crate::Response`] — nothing about pressure or velocity can
    /// or should reach it. Confusing the two would put a source dropdown on a control where no
    /// source means anything.
    ///
    /// It is a parameter of the *stroke* rather than of the dab, which is why it arrives as an
    /// argument: every dab in a stroke shares one, and a `Dab` is `Copy` GPU instance data with no
    /// room for a curve.
    #[must_use]
    pub fn coverage_at_distance(&self, dist: f32, falloff: &Curve) -> f32 {
        // Radius of the solid core, outside which coverage ramps to zero.
        // hardness=1 -> core fills the dab (hard edge, still AA'd at the rim);
        // hardness=0 -> no core, so falloff starts at the very center (softest).
        let inner = (self.radius * self.hardness).max(0.0);
        if dist <= inner {
            return 1.0;
        }
        if dist >= self.radius {
            return 0.0;
        }
        falloff.at((dist - inner) / (self.radius - inner))
    }

    /// Inclusive pixel bounds this dab can touch, as `(min_x, min_y, max_x, max_y)`.
    #[must_use]
    pub fn pixel_bounds(&self) -> (i32, i32, i32, i32) {
        (
            (self.x - self.radius).floor() as i32,
            (self.y - self.radius).floor() as i32,
            (self.x + self.radius).ceil() as i32,
            (self.y + self.radius).ceil() as i32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dab(radius: f32, hardness: f32) -> Dab {
        Dab {
            x: 0.0,
            y: 0.0,
            radius,
            hardness,
            flow: 1.0,
            roundness: 1.0,
            angle: 0.0,
            color_linear_premul: [0.0, 0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn coverage_is_full_at_center_and_zero_outside() {
        let d = dab(10.0, 0.5);
        assert_eq!(d.coverage_at_distance(0.0, &linear_falloff()), 1.0);
        assert_eq!(d.coverage_at_distance(10.0, &linear_falloff()), 0.0);
        assert_eq!(d.coverage_at_distance(50.0, &linear_falloff()), 0.0);
    }

    #[test]
    fn coverage_never_leaves_the_unit_range() {
        for hardness in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let d = dab(10.0, hardness);
            for i in 0..=200 {
                let c = d.coverage_at_distance(i as f32 * 0.1, &linear_falloff());
                assert!((0.0..=1.0).contains(&c), "hardness {hardness} dist {i}");
            }
        }
    }

    /// Pins the Photoshop/CSP convention: **hardness 1 is hard, 0 is soft.**
    /// This was inverted once; the slider felt backwards to use, which is exactly
    /// what a wrong orientation feels like.
    #[test]
    fn hardness_one_is_hard_and_zero_is_soft() {
        let hard = dab(10.0, 1.0);
        let soft = dab(10.0, 0.0);
        // Hard: still fully covered near the rim.
        assert_eq!(hard.coverage_at_distance(9.0, &linear_falloff()), 1.0);
        // Soft: already falling off close to the center.
        assert!(soft.coverage_at_distance(5.0, &linear_falloff()) < 0.6);
        assert!(soft.coverage_at_distance(9.0, &linear_falloff()) < 0.2);
    }

    /// Coverage at a given distance must rise with hardness, at every radius.
    #[test]
    fn higher_hardness_means_more_coverage_everywhere() {
        let mut prev = -1.0;
        for h in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let c = dab(10.0, h).coverage_at_distance(6.0, &linear_falloff());
            assert!(c >= prev, "hardness {h} gave less coverage than the last");
            prev = c;
        }
    }

    #[test]
    fn coverage_decreases_monotonically() {
        let d = dab(10.0, 0.6);
        let mut prev = f32::INFINITY;
        for i in 0..=100 {
            let c = d.coverage_at_distance(i as f32 * 0.1, &linear_falloff());
            assert!(c <= prev + 1e-6, "not monotonic at {i}");
            prev = c;
        }
    }

    /// A flattened dab is narrow across its minor axis and full width along its major.
    #[test]
    fn roundness_flattens_the_dab() {
        let d = Dab {
            roundness: 0.25,
            ..dab(20.0, 1.0)
        };
        // Unrotated, the major axis runs along x.
        assert!(
            d.coverage_at(19.0, 0.0, &Tip::default()) > 0.9,
            "along the major axis"
        );
        assert!(
            d.coverage_at(21.0, 0.0, &Tip::default()) < 0.01,
            "and stops at the radius"
        );
        assert!(
            d.coverage_at(0.0, 4.0, &Tip::default()) > 0.9,
            "across the minor axis"
        );
        assert!(
            d.coverage_at(0.0, 6.0, &Tip::default()) < 0.01,
            "the minor axis should end at a quarter of the radius"
        );
    }

    /// The angle turns the whole shape, so what was the long way round becomes the short way.
    #[test]
    fn the_angle_rotates_the_shape() {
        let flat = Dab {
            roundness: 0.25,
            ..dab(20.0, 1.0)
        };
        let turned = Dab {
            angle: std::f32::consts::FRAC_PI_2,
            ..flat
        };

        assert!(
            flat.coverage_at(15.0, 0.0, &Tip::default()) > 0.9
                && flat.coverage_at(0.0, 15.0, &Tip::default()) < 0.01
        );
        assert!(
            turned.coverage_at(0.0, 15.0, &Tip::default()) > 0.9
                && turned.coverage_at(15.0, 0.0, &Tip::default()) < 0.01,
            "a quarter turn should swap the axes"
        );
    }

    /// A round dab is unaffected by its angle, which is the property that lets angle default to
    /// zero and be ignored by every brush that does not want it.
    #[test]
    fn a_circle_does_not_care_about_its_angle() {
        let plain = dab(20.0, 0.5);
        let turned = Dab {
            angle: 1.234,
            ..plain
        };
        for (x, y) in [(5.0_f32, 0.0_f32), (0.0, 5.0), (9.0, 9.0), (14.0, 3.0)] {
            assert!(
                (plain.coverage_at(x, y, &Tip::default())
                    - turned.coverage_at(x, y, &Tip::default()))
                .abs()
                    < 1e-4,
                "rotating a circle changed it at ({x}, {y})"
            );
        }
    }

    /// Hardness has to mean the same thing whatever the shape, which is why the ellipse is handled
    /// by transforming the *point* rather than by a separate elliptical falloff.
    #[test]
    fn hardness_means_the_same_at_every_roundness() {
        let round = dab(20.0, 0.5);
        let flat = Dab {
            roundness: 0.25,
            ..round
        };
        // Halfway out along each dab's own major axis, the falloff should have progressed equally.
        assert!(
            (round.coverage_at(15.0, 0.0, &Tip::default())
                - flat.coverage_at(15.0, 0.0, &Tip::default()))
            .abs()
                < 1e-4,
            "the falloff differs along the major axis"
        );
        // And equally at the matching fraction across the minor.
        assert!(
            (round.coverage_at(0.0, 15.0, &Tip::default())
                - flat.coverage_at(0.0, 15.0 * 0.25, &Tip::default()))
            .abs()
                < 1e-4,
            "the falloff differs across the minor axis"
        );
    }

    /// However flat, a dab never reaches past its radius -- which is what lets the pixel bounds
    /// and the GPU quad stay the same for every shape.
    #[test]
    fn no_shape_reaches_past_the_radius() {
        for roundness in [1.0_f32, 0.5, 0.02] {
            for angle in [0.0_f32, 0.7, 2.5] {
                let d = Dab {
                    roundness,
                    angle,
                    ..dab(20.0, 1.0)
                };
                let (min_x, min_y, max_x, max_y) = d.pixel_bounds();
                for y in (min_y - 3)..=(max_y + 3) {
                    for x in (min_x - 3)..=(max_x + 3) {
                        let outside = x < min_x || y < min_y || x > max_x || y > max_y;
                        if outside {
                            assert_eq!(
                                d.coverage_at(x as f32, y as f32, &Tip::default()),
                                0.0,
                                "roundness {roundness} angle {angle} covered ({x}, {y}), \
                                 which is outside its own bounds"
                            );
                        }
                    }
                }
            }
        }
    }

    /// A tip is asked for coverage instead of the edge profile, not as well as it: the two answer
    /// the same question and there is no sensible way to combine them.
    #[test]
    fn a_stamped_tip_replaces_the_procedural_edge() {
        let solid = std::sync::Arc::new(crate::Stamp::new(1, 1, vec![255]).expect("valid"));
        let tip = Tip::Stamp(solid);
        // Hardness zero would make a round tip fade from the very centre. A stamp ignores it.
        let d = Dab {
            hardness: 0.0,
            ..dab(20.0, 0.0)
        };
        assert!(
            (d.coverage_at(10.0, 0.0, &tip) - 1.0).abs() < 1e-4,
            "a solid stamp should be solid regardless of hardness"
        );
        assert!(
            d.coverage_at(10.0, 0.0, &Tip::default()) < 0.9,
            "and a round tip at the same hardness should not be"
        );
    }

    /// The stamp is mapped across the dab's diameter, so nothing that reasons about a dab's extent
    /// has to learn about tips. Outside that square it draws nothing, rather than smearing the
    /// image's edge texels across the page.
    #[test]
    fn a_stamp_stays_inside_the_dab() {
        let solid = std::sync::Arc::new(crate::Stamp::new(1, 1, vec![255]).expect("valid"));
        let tip = Tip::Stamp(solid);
        let d = dab(20.0, 1.0);
        assert!(d.coverage_at(0.0, 0.0, &tip) > 0.9, "the centre is covered");
        assert_eq!(
            d.coverage_at(30.0, 0.0, &tip),
            0.0,
            "well outside the dab, nothing"
        );

        let (min_x, min_y, max_x, max_y) = d.pixel_bounds();
        for (x, y) in [
            (min_x - 1, 0),
            (max_x + 1, 0),
            (0, min_y - 1),
            (0, max_y + 1),
        ] {
            assert_eq!(
                d.coverage_at(x as f32, y as f32, &tip),
                0.0,
                "({x}, {y}) is outside the dab's own bounds"
            );
        }
    }

    /// The corners of the dab's square are inside the stamp but outside the disc, which is how a
    /// stamped tip can make a mark a round one never could.
    #[test]
    fn a_stamp_reaches_the_corners_a_disc_cannot() {
        let solid = std::sync::Arc::new(crate::Stamp::new(1, 1, vec![255]).expect("valid"));
        let d = dab(20.0, 1.0);
        // Just inside the square's corner: distance 19*sqrt(2) is well past the radius.
        let (x, y) = (19.0, 19.0);
        assert_eq!(
            d.coverage_at(x, y, &Tip::default()),
            0.0,
            "a disc does not reach its square's corner"
        );
        assert!(
            d.coverage_at(x, y, &Tip::Stamp(solid)) > 0.9,
            "a stamp does"
        );
    }

    /// Roundness and angle are dab geometry, so a stamp turns with them exactly as a disc does.
    #[test]
    fn stamp_coordinates_follow_the_dabs_frame() {
        let d = Dab {
            angle: std::f32::consts::FRAC_PI_2,
            ..dab(20.0, 1.0)
        };
        // A quarter turn should send a point on +x to the stamp's -v side.
        let (u, v) = d.stamp_coords(10.0, 0.0);
        assert!((u - 0.5).abs() < 1e-4, "u should be centred, got {u}");
        assert!(
            v < 0.5,
            "a quarter turn should move it up the image, got {v}"
        );

        let plain = dab(20.0, 1.0);
        let (u, v) = plain.stamp_coords(10.0, 0.0);
        assert!(u > 0.5 && (v - 0.5).abs() < 1e-4, "unrotated: {u}, {v}");
    }

    /// The centre of the dab is the centre of the image, whatever the shape.
    #[test]
    fn the_dab_centre_is_the_stamp_centre() {
        for roundness in [1.0_f32, 0.4] {
            for angle in [0.0_f32, 1.1] {
                let d = Dab {
                    roundness,
                    angle,
                    ..dab(20.0, 1.0)
                };
                let (u, v) = d.stamp_coords(d.x, d.y);
                assert!(
                    (u - 0.5).abs() < 1e-4 && (v - 0.5).abs() < 1e-4,
                    "roundness {roundness} angle {angle} gave ({u}, {v})"
                );
            }
        }
    }

    /// The two arms report themselves honestly, which is what lets the UI stop offering an edge
    /// control that would do nothing.
    #[test]
    fn a_tip_says_which_kind_it_is() {
        let round = Tip::default();
        assert!(!round.is_stamp());
        assert!(round.falloff().is_some());
        assert!(round.stamp().is_none());

        let stamp = Tip::Stamp(std::sync::Arc::new(
            crate::Stamp::new(1, 1, vec![255]).expect("valid"),
        ));
        assert!(stamp.is_stamp());
        assert!(
            stamp.falloff().is_none(),
            "a stamp has no edge profile to edit"
        );
        assert!(stamp.stamp().is_some());
    }

    #[test]
    fn coverage_at_matches_coverage_at_distance() {
        let d = dab(10.0, 0.5);
        // (3,4) is distance 5 from the origin.
        assert!(
            (d.coverage_at(3.0, 4.0, &Tip::default())
                - d.coverage_at_distance(5.0, &linear_falloff()))
            .abs()
                < 1e-6
        );
    }

    #[test]
    fn bounds_enclose_the_whole_dab() {
        let d = Dab {
            x: 100.5,
            y: 200.25,
            radius: 8.0,
            hardness: 0.5,
            flow: 1.0,
            roundness: 1.0,
            angle: 0.0,
            color_linear_premul: [0.0, 0.0, 0.0, 1.0],
        };
        let (min_x, min_y, max_x, max_y) = d.pixel_bounds();
        assert!((min_x as f32) <= d.x - d.radius);
        assert!((min_y as f32) <= d.y - d.radius);
        assert!((max_x as f32) >= d.x + d.radius);
        assert!((max_y as f32) >= d.y + d.radius);
    }
}
