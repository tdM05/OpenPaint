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

/// The narrowest a dab may be, as a fraction of its radius.
///
/// Not zero: roundness divides, and a dab with no width would cover nothing anywhere while costing
/// the same to rasterize. A fiftieth of the major axis is already a hairline at any usable size.
pub const MIN_ROUNDNESS: f32 = 0.02;

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
    pub fn coverage_at(&self, px: f32, py: f32) -> f32 {
        self.coverage_at_distance(self.distance_to(px, py))
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
    #[must_use]
    pub fn coverage_at_distance(&self, dist: f32) -> f32 {
        // Radius of the solid core, outside which coverage ramps to zero.
        // hardness=1 -> core fills the dab (hard edge, still AA'd at the rim);
        // hardness=0 -> no core, so falloff starts at the very center (softest).
        let inner = (self.radius * self.hardness).max(0.0);
        if dist <= inner {
            1.0
        } else if dist >= self.radius {
            0.0
        } else {
            1.0 - (dist - inner) / (self.radius - inner)
        }
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
        assert_eq!(d.coverage_at_distance(0.0), 1.0);
        assert_eq!(d.coverage_at_distance(10.0), 0.0);
        assert_eq!(d.coverage_at_distance(50.0), 0.0);
    }

    #[test]
    fn coverage_never_leaves_the_unit_range() {
        for hardness in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let d = dab(10.0, hardness);
            for i in 0..=200 {
                let c = d.coverage_at_distance(i as f32 * 0.1);
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
        assert_eq!(hard.coverage_at_distance(9.0), 1.0);
        // Soft: already falling off close to the center.
        assert!(soft.coverage_at_distance(5.0) < 0.6);
        assert!(soft.coverage_at_distance(9.0) < 0.2);
    }

    /// Coverage at a given distance must rise with hardness, at every radius.
    #[test]
    fn higher_hardness_means_more_coverage_everywhere() {
        let mut prev = -1.0;
        for h in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let c = dab(10.0, h).coverage_at_distance(6.0);
            assert!(c >= prev, "hardness {h} gave less coverage than the last");
            prev = c;
        }
    }

    #[test]
    fn coverage_decreases_monotonically() {
        let d = dab(10.0, 0.6);
        let mut prev = f32::INFINITY;
        for i in 0..=100 {
            let c = d.coverage_at_distance(i as f32 * 0.1);
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
        assert!(d.coverage_at(19.0, 0.0) > 0.9, "along the major axis");
        assert!(d.coverage_at(21.0, 0.0) < 0.01, "and stops at the radius");
        assert!(d.coverage_at(0.0, 4.0) > 0.9, "across the minor axis");
        assert!(
            d.coverage_at(0.0, 6.0) < 0.01,
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

        assert!(flat.coverage_at(15.0, 0.0) > 0.9 && flat.coverage_at(0.0, 15.0) < 0.01);
        assert!(
            turned.coverage_at(0.0, 15.0) > 0.9 && turned.coverage_at(15.0, 0.0) < 0.01,
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
                (plain.coverage_at(x, y) - turned.coverage_at(x, y)).abs() < 1e-4,
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
            (round.coverage_at(15.0, 0.0) - flat.coverage_at(15.0, 0.0)).abs() < 1e-4,
            "the falloff differs along the major axis"
        );
        // And equally at the matching fraction across the minor.
        assert!(
            (round.coverage_at(0.0, 15.0) - flat.coverage_at(0.0, 15.0 * 0.25)).abs() < 1e-4,
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
                                d.coverage_at(x as f32, y as f32),
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

    #[test]
    fn coverage_at_matches_coverage_at_distance() {
        let d = dab(10.0, 0.5);
        // (3,4) is distance 5 from the origin.
        assert!((d.coverage_at(3.0, 4.0) - d.coverage_at_distance(5.0)).abs() < 1e-6);
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
