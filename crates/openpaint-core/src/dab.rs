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
    /// Radius in canvas pixels, after pressure has been applied.
    pub radius: f32,
    /// Edge softness in `0.0..=1.0`: 0 = hard edge, 1 = fully soft falloff.
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
        let dx = px - self.x;
        let dy = py - self.y;
        let dist = (dx * dx + dy * dy).sqrt();
        self.coverage_at_distance(dist)
    }

    /// Coverage as a function of distance from the dab center.
    #[must_use]
    pub fn coverage_at_distance(&self, dist: f32) -> f32 {
        // hardness=1 -> falloff starts at the very center (very soft);
        // hardness=0 -> solid until ~1px from the edge (hard, still AA'd).
        let inner = (self.radius * (1.0 - self.hardness)).max(0.0);
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

    /// A hard brush should be solid almost to its edge; a soft one should start
    /// falling off immediately. If these invert, hardness is backwards.
    #[test]
    fn hardness_controls_where_falloff_starts() {
        let hard = dab(10.0, 0.0);
        let soft = dab(10.0, 1.0);
        assert_eq!(hard.coverage_at_distance(9.0), 1.0);
        assert!(soft.coverage_at_distance(9.0) < 0.2);
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
            color_linear_premul: [0.0, 0.0, 0.0, 1.0],
        };
        let (min_x, min_y, max_x, max_y) = d.pixel_bounds();
        assert!((min_x as f32) <= d.x - d.radius);
        assert!((min_y as f32) <= d.y - d.radius);
        assert!((max_x as f32) >= d.x + d.radius);
        assert!((max_y as f32) >= d.y + d.radius);
    }
}
