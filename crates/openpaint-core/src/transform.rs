//! Affine transforms of pixels, and the resampling that makes them look right.
//!
//! Moving a selection needs no resampling at all — a whole-pixel offset is a copy, which is why
//! [`crate::Lifted::shifted`] exists and is exact. Rotating or scaling one does: a destination
//! pixel no longer lands on a source pixel, so its colour has to be *reconstructed* from the
//! neighbours it falls between. Everything difficult about a free transform is in that
//! reconstruction, and this module is it.
//!
//! # Why the filter matters more than it sounds
//!
//! Line art is the worst case for resampling and it is exactly what this app is for. Nearest
//! neighbour turns a rotated ink line into a staircase. Bilinear turns it into a blur. A cubic
//! filter is the first one that keeps an inked edge looking inked, which is why it is the default
//! here rather than the option — see [`Kernel`].
//!
//! Every interpolating cubic overshoots at a hard edge; the choice is how much. The default trades
//! a little sharpness for about half the ringing, which is the right way round for ink.
//!
//! # Premultiplied, which is not a detail
//!
//! Filtering happens on premultiplied colour, per §4b, and that is *why* the convention is worth
//! holding. Interpolating straight colour across an edge into transparency mixes in whatever
//! happens to be stored in the fully transparent texels, which shows up as a dark or bright halo
//! around every rotated selection. Premultiplied has no such value to leak: transparent is zero in
//! every channel, so it contributes nothing and the edge stays clean.
//!
//! # Minification widens the footprint
//!
//! Shrinking is the case a fixed-radius filter gets wrong: many source pixels fall into one
//! destination pixel, and reading only the four nearest of them is undersampling — which is
//! aliasing, and looks like sparkle and broken lines. So the kernel's footprint is scaled by the
//! minification factor, which is the standard fix and needs no mip pyramid. Magnification uses the
//! kernel at its natural width, since there is nothing to average.

/// An affine transform: rotate and scale about a pivot, then translate.
///
/// Stored as the parameters an artist manipulates rather than as a matrix, because the UI has to
/// show and edit them — "rotated 15 degrees, scaled to 120%" is a thing to display, and recovering
/// it from a matrix is both lossy and pointless when it was known all along.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// The point rotation and scaling happen about, in page pixels.
    ///
    /// Normally the centre of what is being transformed, so it grows and turns in place rather than
    /// swinging away from the origin.
    pub pivot: (f32, f32),
    /// Translation, in page pixels, applied after the rotation and scale.
    pub offset: (f32, f32),
    /// Scale on each axis. Negative flips.
    pub scale: (f32, f32),
    /// Rotation in radians, clockwise on screen — the same sense as [`crate::Dab::angle`].
    pub rotation: f32,
}

/// The smallest scale factor that still describes a shape rather than a line.
///
/// Not zero: the inverse transform divides by the scale, and a zero would map every destination
/// pixel to the same source point. Small enough that no real edit reaches it.
pub const MIN_SCALE: f32 = 1e-3;

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// Changes nothing.
    pub const IDENTITY: Self = Self {
        pivot: (0.0, 0.0),
        offset: (0.0, 0.0),
        scale: (1.0, 1.0),
        rotation: 0.0,
    };

    /// A pure translation, which is what a plain move is.
    #[must_use]
    pub fn translation(dx: f32, dy: f32) -> Self {
        Self {
            offset: (dx, dy),
            ..Self::IDENTITY
        }
    }

    /// Whether this leaves every pixel exactly where it was.
    ///
    /// Worth asking, because a transform that only translates by whole pixels can be done by
    /// copying rather than resampling — and resampling when you did not have to is a quality loss
    /// for nothing.
    #[must_use]
    pub fn is_a_plain_move(&self) -> bool {
        let (sx, sy) = self.scale;
        self.rotation == 0.0
            && (sx - 1.0).abs() < f32::EPSILON
            && (sy - 1.0).abs() < f32::EPSILON
            && self.offset.0.fract() == 0.0
            && self.offset.1.fract() == 0.0
    }

    /// The scale actually used, floored away from zero so the inverse exists.
    #[must_use]
    pub fn effective_scale(&self) -> (f32, f32) {
        let clamp = |s: f32| {
            if s.abs() < MIN_SCALE {
                MIN_SCALE.copysign(if s == 0.0 { 1.0 } else { s })
            } else {
                s
            }
        };
        (clamp(self.scale.0), clamp(self.scale.1))
    }

    /// Where a source point ends up.
    #[must_use]
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let (sx, sy) = self.effective_scale();
        let (dx, dy) = (x - self.pivot.0, y - self.pivot.1);
        let (sinr, cosr) = self.rotation.sin_cos();
        let (px, py) = (dx * sx, dy * sy);
        (
            self.pivot.0 + px * cosr - py * sinr + self.offset.0,
            self.pivot.1 + px * sinr + py * cosr + self.offset.1,
        )
    }

    /// Where a destination point came from.
    ///
    /// The direction resampling actually needs: reconstruction walks *destination* pixels and asks
    /// where each one was, because walking source pixels forward leaves holes wherever the
    /// transform magnifies.
    #[must_use]
    pub fn invert(&self, x: f32, y: f32) -> (f32, f32) {
        let (sx, sy) = self.effective_scale();
        let (dx, dy) = (
            x - self.pivot.0 - self.offset.0,
            y - self.pivot.1 - self.offset.1,
        );
        let (sinr, cosr) = self.rotation.sin_cos();
        // Undo the rotation, then the scale, in that order.
        let (rx, ry) = (dx * cosr + dy * sinr, -dx * sinr + dy * cosr);
        (self.pivot.0 + rx / sx, self.pivot.1 + ry / sy)
    }

    /// Where a source rectangle lands, as an integer bounding box `(min_x, min_y, max_x, max_y)`
    /// with the maxima exclusive.
    ///
    /// All four corners, not two: under rotation the axis-aligned bounds of the result are not the
    /// transform of the axis-aligned bounds of the input.
    #[must_use]
    pub fn bounds_of(
        &self,
        min_x: i32,
        min_y: i32,
        max_x: i32,
        max_y: i32,
    ) -> (i32, i32, i32, i32) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "page coordinates, far inside f32's exact integer range"
        )]
        let corners = [
            self.apply(min_x as f32, min_y as f32),
            self.apply(max_x as f32, min_y as f32),
            self.apply(min_x as f32, max_y as f32),
            self.apply(max_x as f32, max_y as f32),
        ];
        let xs = corners.map(|c| c.0);
        let ys = corners.map(|c| c.1);
        let lo_x = xs.iter().copied().fold(f32::INFINITY, f32::min);
        let hi_x = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let lo_y = ys.iter().copied().fold(f32::INFINITY, f32::min);
        let hi_y = ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        // Widened by the filter's reach, so the soft edge a cubic kernel produces is not clipped
        // off — which would leave a visibly hard edge on an otherwise smoothly resampled shape.
        let pad = 2.0;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "page coordinates, far inside i32"
        )]
        (
            (lo_x - pad).floor() as i32,
            (lo_y - pad).floor() as i32,
            (hi_x + pad).ceil() as i32,
            (hi_y + pad).ceil() as i32,
        )
    }
}

/// How a destination pixel is reconstructed from the source pixels around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kernel {
    /// Mitchell–Netravali (B = C = 1/3).
    ///
    /// The default. It is the point Mitchell and Netravali identified as the best compromise
    /// between blurring and ringing, and it is the right default here because ringing matters more
    /// in this app than in most: a dark halo beside a rotated ink line is exactly the artefact a
    /// comic page shows off.
    ///
    /// Its negative lobe is about **-0.035**, against Catmull–Rom's **-0.063** — so it rings
    /// roughly half as hard, not not at all. No interpolating cubic is free of it; the members of
    /// the family that are (B = 1, the cubic B-spline) are far too soft to ink with.
    #[default]
    Mitchell,
    /// Catmull–Rom (B = 0, C = 1/2).
    ///
    /// Sharper, and it *interpolates* rather than approximates — an unmoved pixel comes back
    /// exactly, which Mitchell cannot promise. It pays for that with about twice the ringing.
    /// Offered for work that is mostly flat colour, where there are few hard edges to ring.
    CatmullRom,
    /// Bilinear.
    ///
    /// Soft, but cheap and utterly predictable. Kept because a preview during a drag can afford to
    /// be worse than the committed result, and because it is the thing a cubic is compared against.
    Bilinear,
}

impl Kernel {
    /// Every kernel, in the order the UI lists them.
    pub const ALL: [Self; 3] = [Self::Mitchell, Self::CatmullRom, Self::Bilinear];

    /// The name shown in the UI.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Mitchell => "Smooth (Mitchell)",
            Self::CatmullRom => "Sharp (Catmull-Rom)",
            Self::Bilinear => "Fast (bilinear)",
        }
    }

    /// How far the kernel reaches, in source pixels, before minification widens it.
    #[must_use]
    pub fn radius(self) -> f32 {
        match self {
            Self::Mitchell | Self::CatmullRom => 2.0,
            Self::Bilinear => 1.0,
        }
    }

    /// The kernel's weight at a distance of `x` source pixels.
    #[must_use]
    pub fn weight(self, x: f32) -> f32 {
        let x = x.abs();
        match self {
            Self::Bilinear => (1.0 - x).max(0.0),
            Self::Mitchell => cubic(x, 1.0 / 3.0, 1.0 / 3.0),
            Self::CatmullRom => cubic(x, 0.0, 0.5),
        }
    }
}

/// The Mitchell–Netravali family, parameterised by B and C.
///
/// One function for both cubics rather than two sets of hardcoded coefficients: the family is a
/// single formula, and writing it once means the two kernels cannot drift into disagreeing about
/// what a cubic is.
fn cubic(x: f32, b: f32, c: f32) -> f32 {
    let x2 = x * x;
    let x3 = x2 * x;
    if x < 1.0 {
        ((12.0 - 9.0 * b - 6.0 * c) * x3 + (-18.0 + 12.0 * b + 6.0 * c) * x2 + (6.0 - 2.0 * b))
            / 6.0
    } else if x < 2.0 {
        ((-b - 6.0 * c) * x3
            + (6.0 * b + 30.0 * c) * x2
            + (-12.0 * b - 48.0 * c) * x
            + (8.0 * b + 24.0 * c))
            / 6.0
    } else {
        0.0
    }
}

/// Resample a source into a destination through `transform`.
///
/// `source` reads a **premultiplied** source texel, returning transparent outside the image.
/// `plot` receives each destination pixel that has any coverage at all.
///
/// Walks destination pixels and inverse-maps each one, rather than walking source pixels forward:
/// the forward direction leaves holes wherever the transform magnifies, and no amount of care about
/// the filter fixes a hole.
pub fn resample(
    transform: &Transform,
    kernel: Kernel,
    bounds: (i32, i32, i32, i32),
    mut source: impl FnMut(i32, i32) -> [f32; 4],
    mut plot: impl FnMut(i32, i32, [f32; 4]),
) {
    let (min_x, min_y, max_x, max_y) = bounds;
    let (sx, sy) = transform.effective_scale();

    // Minification widens the footprint; magnification leaves it alone. Reading only the nearest
    // few source pixels while shrinking is undersampling, which reads as sparkle and broken lines.
    let spread_x = (1.0 / sx.abs()).max(1.0);
    let spread_y = (1.0 / sy.abs()).max(1.0);
    let radius = kernel.radius();
    let (rx, ry) = (radius * spread_x, radius * spread_y);

    for py in min_y..max_y {
        for px in min_x..max_x {
            #[expect(
                clippy::cast_precision_loss,
                reason = "page coordinates, far inside f32's exact integer range"
            )]
            let (u, v) = transform.invert(px as f32 + 0.5, py as f32 + 0.5);
            // Source pixel *centres* sit at integer + 0.5, so this is the centre-relative position
            // the kernel is measured from.
            let (cu, cv) = (u - 0.5, v - 0.5);

            #[expect(
                clippy::cast_possible_truncation,
                reason = "source coordinates, far inside i32"
            )]
            let (x0, x1) = ((cu - rx).ceil() as i32, (cu + rx).floor() as i32);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "source coordinates, far inside i32"
            )]
            let (y0, y1) = ((cv - ry).ceil() as i32, (cv + ry).floor() as i32);

            let mut acc = [0.0_f32; 4];
            let mut total = 0.0_f32;
            for sy_i in y0..=y1 {
                #[expect(clippy::cast_precision_loss, reason = "as above")]
                let wy = kernel.weight((sy_i as f32 - cv) / spread_y);
                if wy == 0.0 {
                    continue;
                }
                for sx_i in x0..=x1 {
                    #[expect(clippy::cast_precision_loss, reason = "as above")]
                    let wx = kernel.weight((sx_i as f32 - cu) / spread_x);
                    if wx == 0.0 {
                        continue;
                    }
                    let w = wx * wy;
                    let texel = source(sx_i, sy_i);
                    for c in 0..4 {
                        acc[c] += texel[c] * w;
                    }
                    total += w;
                }
            }

            if total <= 0.0 {
                continue;
            }
            // Normalised by the weight actually gathered, not by the kernel's nominal sum. A cubic
            // does not sum to exactly one at every subpixel phase, and leaving that unnormalised
            // shows up as a faint grid of brightness variation across the whole result.
            let inv = 1.0 / total;
            let mut out = [0.0_f32; 4];
            for c in 0..4 {
                // Clamped at zero because Mitchell and Catmull-Rom both have negative lobes, and a
                // negative premultiplied channel is not a colour.
                out[c] = (acc[c] * inv).max(0.0);
            }
            if out[3] <= 0.0 {
                continue;
            }
            // Premultiplied means no channel may exceed alpha. Overshoot past that is the same
            // negative lobe seen from the other side, and it would read as a bright fringe.
            let a = out[3].min(1.0);
            plot(px, py, [out[0].min(a), out[1].min(a), out[2].min(a), a]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_leaves_a_point_alone() {
        let t = Transform::IDENTITY;
        let (x, y) = t.apply(12.5, -3.25);
        assert!((x - 12.5).abs() < 1e-4 && (y + 3.25).abs() < 1e-4);
    }

    #[test]
    fn applying_and_inverting_are_opposites() {
        let t = Transform {
            pivot: (100.0, 50.0),
            offset: (-13.5, 7.25),
            scale: (1.7, 0.6),
            rotation: 0.9,
        };
        for (x, y) in [(0.0_f32, 0.0_f32), (100.0, 50.0), (250.5, -30.25)] {
            let (fx, fy) = t.apply(x, y);
            let (bx, by) = t.invert(fx, fy);
            assert!(
                (bx - x).abs() < 1e-2 && (by - y).abs() < 1e-2,
                "({x}, {y}) -> ({fx}, {fy}) -> ({bx}, {by})"
            );
        }
    }

    /// Rotation and scale happen about the pivot, so the pivot itself only ever translates. This is
    /// what makes a selection grow in place instead of swinging away from the origin.
    #[test]
    fn the_pivot_only_translates() {
        let t = Transform {
            pivot: (40.0, 60.0),
            offset: (5.0, -2.0),
            scale: (3.0, 0.2),
            rotation: 1.234,
        };
        let (x, y) = t.apply(40.0, 60.0);
        assert!(
            (x - 45.0).abs() < 1e-3 && (y - 58.0).abs() < 1e-3,
            "({x}, {y})"
        );
    }

    /// A quarter turn sends +x to +y on screen, matching the clockwise sense the dab angle uses.
    #[test]
    fn rotation_turns_the_same_way_as_a_dab() {
        let t = Transform {
            rotation: std::f32::consts::FRAC_PI_2,
            ..Transform::IDENTITY
        };
        let (x, y) = t.apply(10.0, 0.0);
        assert!(x.abs() < 1e-3 && (y - 10.0).abs() < 1e-3, "({x}, {y})");
    }

    /// A zero scale would make the inverse divide by zero and collapse the whole result onto one
    /// source point.
    #[test]
    fn a_zero_scale_is_floored_rather_than_dividing_by_zero() {
        let t = Transform {
            scale: (0.0, -0.0),
            ..Transform::IDENTITY
        };
        let (sx, sy) = t.effective_scale();
        assert!(sx.abs() >= MIN_SCALE && sy.abs() >= MIN_SCALE);
        let (x, y) = t.invert(10.0, 10.0);
        assert!(x.is_finite() && y.is_finite(), "({x}, {y})");
    }

    /// A negative scale is a flip, and has to stay one rather than being clamped into a positive.
    #[test]
    fn a_negative_scale_flips() {
        let t = Transform {
            scale: (-1.0, 1.0),
            ..Transform::IDENTITY
        };
        let (x, _) = t.apply(10.0, 0.0);
        assert!((x + 10.0).abs() < 1e-3, "should mirror, got {x}");
        assert!(t.effective_scale().0 < 0.0, "the sign must survive");
    }

    /// Under rotation the bounds of the result are not the transform of the bounds, so all four
    /// corners have to be considered.
    #[test]
    fn bounds_cover_a_rotated_square() {
        let t = Transform {
            pivot: (50.0, 50.0),
            rotation: std::f32::consts::FRAC_PI_4,
            ..Transform::IDENTITY
        };
        let (min_x, min_y, max_x, max_y) = t.bounds_of(0, 0, 100, 100);
        // A 100px square turned 45 degrees has a diagonal of about 141, centred on the pivot.
        assert!(min_x < -18 && max_x > 118, "x span {min_x}..{max_x}");
        assert!(min_y < -18 && max_y > 118, "y span {min_y}..{max_y}");
    }

    /// A whole-pixel move needs no resampling, and resampling when you did not have to is a quality
    /// loss for nothing.
    #[test]
    fn a_whole_pixel_move_is_recognised() {
        assert!(Transform::translation(12.0, -30.0).is_a_plain_move());
        assert!(!Transform::translation(12.5, 0.0).is_a_plain_move());
        assert!(!Transform {
            rotation: 0.01,
            ..Transform::translation(12.0, 0.0)
        }
        .is_a_plain_move());
        assert!(!Transform {
            scale: (1.5, 1.0),
            ..Transform::IDENTITY
        }
        .is_a_plain_move());
    }

    /// Every kernel has to be a partition of unity at whole-pixel offsets, or resampling would
    /// change the overall brightness of what it resamples.
    #[test]
    fn kernels_sum_to_one_across_a_pixel() {
        for kernel in Kernel::ALL {
            for phase in [0.0_f32, 0.25, 0.5, 0.75] {
                let sum: f32 = (-3..=3).map(|i| kernel.weight(i as f32 - phase)).sum();
                assert!(
                    (sum - 1.0).abs() < 1e-3,
                    "{:?} at phase {phase} sums to {sum}",
                    kernel
                );
            }
        }
    }

    /// Catmull-Rom interpolates: a pixel that did not move comes back exactly. Mitchell
    /// approximates and deliberately does not, which is the trade that buys it no ringing.
    #[test]
    fn catmull_rom_interpolates_and_mitchell_approximates() {
        assert!((Kernel::CatmullRom.weight(0.0) - 1.0).abs() < 1e-4);
        assert!(Kernel::CatmullRom.weight(1.0).abs() < 1e-4);
        assert!(
            Kernel::Mitchell.weight(0.0) < 1.0,
            "Mitchell should spread a pixel's own weight to its neighbours"
        );
        assert!(Kernel::Mitchell.weight(1.0) > 0.0);
    }

    /// Ringing is a negative lobe, and the default's is about half the sharp one's. Pinned as a
    /// ratio rather than as "none": no interpolating cubic has none, and claiming otherwise in a
    /// comment would be the kind of thing nobody checks.
    #[test]
    fn the_default_kernel_rings_about_half_as_hard() {
        let lobe = |k: Kernel| {
            (0..=200)
                .map(|i| k.weight(i as f32 / 100.0))
                .fold(0.0_f32, f32::min)
        };
        let mitchell = lobe(Kernel::Mitchell);
        let catmull = lobe(Kernel::CatmullRom);
        assert!(mitchell < 0.0 && catmull < 0.0, "both cubics overshoot");
        assert!(
            mitchell > catmull * 0.75,
            "Mitchell ({mitchell}) should ring markedly less than Catmull-Rom ({catmull})"
        );
        assert!(
            mitchell > -0.05,
            "and not much at all in absolute terms, got {mitchell}"
        );
        assert!(
            lobe(Kernel::Bilinear) >= -1e-6,
            "bilinear cannot overshoot at all"
        );
    }

    #[test]
    fn kernels_stop_at_their_radius() {
        for kernel in Kernel::ALL {
            let past = kernel.radius() + 0.01;
            assert_eq!(
                kernel.weight(past),
                0.0,
                "{kernel:?} reaches past its radius"
            );
            assert_eq!(kernel.weight(-past), 0.0, "{kernel:?}, on the other side");
        }
    }

    /// A flat field must resample to the same flat field, whatever the transform. Any brightness
    /// change here would show up as a seam wherever a transformed selection meets untouched art.
    #[test]
    fn resampling_a_flat_field_preserves_it() {
        let t = Transform {
            pivot: (16.0, 16.0),
            offset: (0.3, -0.7),
            scale: (1.3, 0.8),
            rotation: 0.4,
        };
        let mut worst = 0.0_f32;
        resample(
            &t,
            Kernel::Mitchell,
            (8, 8, 24, 24),
            // A wide flat field, so the sampled window is always fully inside it.
            |_, _| [0.5, 0.25, 0.125, 1.0],
            |_, _, texel| {
                for (got, want) in texel.iter().zip([0.5, 0.25, 0.125, 1.0]) {
                    worst = worst.max((got - want).abs());
                }
            },
        );
        assert!(worst < 1e-3, "flat field drifted by {worst}");
    }

    /// The identity has to be very nearly a copy, or every committed transform would soften the
    /// artwork slightly even when nothing was asked of it.
    #[test]
    fn the_identity_transform_barely_changes_anything() {
        let source = |x: i32, y: i32| {
            if (4..12).contains(&x) && (4..12).contains(&y) {
                [1.0, 0.0, 0.0, 1.0]
            } else {
                [0.0; 4]
            }
        };
        let mut out = std::collections::HashMap::new();
        resample(
            &Transform::IDENTITY,
            Kernel::CatmullRom,
            (0, 0, 16, 16),
            source,
            |x, y, texel| {
                out.insert((x, y), texel);
            },
        );
        // Interior pixels come back exactly, because Catmull-Rom interpolates.
        let centre = out.get(&(8, 8)).copied().unwrap_or_default();
        assert!(
            (centre[0] - 1.0).abs() < 1e-3 && (centre[3] - 1.0).abs() < 1e-3,
            "the centre changed: {centre:?}"
        );
        assert!(
            out.get(&(0, 0)).is_none_or(|t| t[3] < 1e-3),
            "empty space should stay empty"
        );
    }

    /// Shrinking must average the source rather than pick from it, or fine line art breaks up.
    ///
    /// A checkerboard is the obvious test and it is the wrong one: any four-by-four window of a
    /// checkerboard averages to grey, so an undersampled reduction looks correct. Sparse lines are
    /// the case that actually distinguishes them — reduced properly they give a uniform faint grey,
    /// and sampled they give black or white depending on where each destination pixel happens to
    /// land. So this measures the *spread* across the output, not its average.
    #[test]
    fn shrinking_averages_instead_of_undersampling() {
        const PERIOD: i32 = 8;
        let lines = |x: i32, y: i32| {
            if !(0..256).contains(&x) || !(0..256).contains(&y) {
                return [0.0; 4];
            }
            if x.rem_euclid(PERIOD) == 0 {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [0.0, 0.0, 0.0, 1.0]
            }
        };
        let t = Transform {
            pivot: (128.0, 128.0),
            scale: (1.0 / PERIOD as f32, 1.0 / PERIOD as f32),
            ..Transform::IDENTITY
        };

        let mut values = Vec::new();
        resample(
            &t,
            Kernel::Mitchell,
            (118, 124, 138, 132),
            lines,
            |_, _, t| {
                values.push(t[0]);
            },
        );
        assert!(values.len() > 40, "only {} pixels produced", values.len());

        let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        // One white line in every eight source pixels means about an eighth of a unit of ink per
        // destination pixel, wherever it lands. Undersampling gives 0 or 1 instead.
        assert!(
            hi - lo < 0.25,
            "an eightfold reduction of evenly spaced lines varied from {lo} to {hi}, so it \
             sampled rather than averaged"
        );
        assert!(
            (0.02..0.5).contains(&lo) && (0.02..0.5).contains(&hi),
            "the result should be a faint grey, got {lo}..{hi}"
        );
    }

    /// The negative lobe of a sharp kernel must not be allowed to produce a colour that is not a
    /// colour: a channel brighter than its own alpha reads as a bright fringe.
    #[test]
    fn overshoot_cannot_break_premultiplication() {
        // A hard edge, which is what makes a sharp kernel overshoot.
        let edge = |x: i32, _y: i32| {
            if x >= 8 {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [0.0; 4]
            }
        };
        let t = Transform {
            offset: (0.5, 0.0),
            ..Transform::IDENTITY
        };
        resample(
            &t,
            Kernel::CatmullRom,
            (0, 0, 16, 4),
            edge,
            |x, y, texel| {
                for c in 0..3 {
                    assert!(
                        texel[c] <= texel[3] + 1e-4,
                        "({x}, {y}) channel {c} is {} against alpha {}",
                        texel[c],
                        texel[3]
                    );
                    assert!(texel[c] >= 0.0, "({x}, {y}) channel {c} went negative");
                }
                assert!((0.0..=1.0).contains(&texel[3]));
            },
        );
    }

    /// Nothing in, nothing out — and specifically no plotted transparent pixels, which would
    /// allocate tiles for empty space.
    #[test]
    fn resampling_emptiness_plots_nothing() {
        let mut count = 0;
        resample(
            &Transform::IDENTITY,
            Kernel::Mitchell,
            (0, 0, 32, 32),
            |_, _| [0.0; 4],
            |_, _, _| count += 1,
        );
        assert_eq!(count, 0);
    }
}
