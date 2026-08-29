//! Bitmap brush tips.
//!
//! A [`Stamp`] is the shape of a single dab, as a grayscale image: how much ink lands at each point
//! of the mark. It is what turns a brush from "a disc with a soft edge" into a chalk, a bristle
//! brush, or a screentone dot — the difference between the tools an inker actually reaches for.
//!
//! # Where a stamp sits in the pipeline
//!
//! Exactly where the procedural edge profile sits, and instead of it. A dab's geometry — centre,
//! radius, roundness, angle — is unchanged; only the question "how covered is this point" is
//! answered by reading an image rather than by evaluating a curve of distance. See
//! [`crate::dab::Tip`], which is the choice between the two.
//!
//! That placement is the whole reason this is cheap: nothing about spacing, modulation, blending,
//! tile touching or the GPU quad changes, because none of them ever asked how coverage was
//! computed.
//!
//! # Coverage, not colour
//!
//! One byte per texel. A coloured tip would be a different feature — a *dual brush* or a stamped
//! image — and mixing the two would mean deciding what happens when a coloured tip meets a brush
//! colour, a question with no good answer. The tip says *where* ink lands; the brush says what
//! colour it is.

/// The largest tip we will accept, per side.
///
/// A guard rather than a preference. A tip is a texture the GPU holds for the whole stroke and the
/// CPU reads per pixel, and a photograph dropped in by mistake should be refused with a reason
/// rather than eating a gigabyte. Comfortably above any real brush tip, which are usually a few
/// hundred pixels square at most.
pub const MAX_STAMP_SIDE: u32 = 2048;

/// A bitmap brush tip: one byte of coverage per texel, row-major.
///
/// Immutable once built. Brushes share tips by [`std::sync::Arc`], because a stamp is a few hundred
/// kilobytes and a `Brush` is cloned for every queued stroke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

/// Why an image could not be used as a brush tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampError {
    /// Zero width or height. Nothing to stamp.
    Empty,
    /// Larger than [`MAX_STAMP_SIDE`] on one side.
    TooLarge { width: u32, height: u32 },
    /// The pixel data does not match the stated size.
    WrongLength { expected: usize, found: usize },
}

impl std::fmt::Display for StampError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "a brush tip cannot be empty"),
            Self::TooLarge { width, height } => write!(
                f,
                "a brush tip may be at most {MAX_STAMP_SIDE}px on a side; this one is {width}x{height}"
            ),
            Self::WrongLength { expected, found } => {
                write!(f, "expected {expected} bytes of coverage, found {found}")
            }
        }
    }
}

impl std::error::Error for StampError {}

impl Stamp {
    /// Build a tip from coverage values, `width * height` of them, row-major.
    ///
    /// # Errors
    /// See [`StampError`].
    pub fn new(width: u32, height: u32, coverage: Vec<u8>) -> Result<Self, StampError> {
        if width == 0 || height == 0 {
            return Err(StampError::Empty);
        }
        if width > MAX_STAMP_SIDE || height > MAX_STAMP_SIDE {
            return Err(StampError::TooLarge { width, height });
        }
        let expected = width as usize * height as usize;
        if coverage.len() != expected {
            return Err(StampError::WrongLength {
                expected,
                found: coverage.len(),
            });
        }
        Ok(Self {
            width,
            height,
            coverage,
        })
    }

    /// Build a tip from RGBA pixels, taking coverage from whichever channel carries the mark.
    ///
    /// **What looks like ink is ink.** Two conventions exist and both are common: a tip drawn as
    /// black-on-white with no transparency (Photoshop's own `.abr` tips work this way), and a tip
    /// drawn as opaque-on-transparent, which is what falls out of exporting from any paint app.
    /// Rather than making the artist know which they have, an image that uses its alpha channel is
    /// read from alpha, and one that is fully opaque is read from inverted luminance. A tip that is
    /// wrong under one rule would be obviously, uselessly wrong — an inverted mark — so guessing
    /// here is safe in a way guessing usually is not.
    ///
    /// Luminance is the Rec. 709 weighting rather than a flat average, so a coloured tip converts
    /// the way an eye would judge it.
    ///
    /// # Errors
    /// See [`StampError`].
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Self, StampError> {
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            return Err(StampError::WrongLength {
                expected,
                found: rgba.len(),
            });
        }
        let pixels = rgba.as_chunks::<4>().0;
        let uses_alpha = pixels.iter().any(|p| p[3] != 255);
        let coverage = pixels
            .iter()
            .map(|p| {
                if uses_alpha {
                    p[3]
                } else {
                    let luma = 0.2126 * f32::from(p[0])
                        + 0.7152 * f32::from(p[1])
                        + 0.0722 * f32::from(p[2]);
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "luma is a weighted average of u8s, so 0..=255"
                    )]
                    {
                        255 - luma.round() as u8
                    }
                }
            })
            .collect();
        Self::new(width, height, coverage)
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The raw coverage, for uploading to the GPU.
    #[must_use]
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// Sample the tip at normalised coordinates, bilinearly.
    ///
    /// `u` and `v` run 0 to 1 across the image, with `v` increasing downward — the same convention
    /// as the texture the GPU reads, because these two have to agree to within a hundredth and the
    /// cheapest way to guarantee that is to do the identical arithmetic.
    ///
    /// Specifically: texel centres sit at `(i + 0.5) / n`, and coordinates outside the image clamp
    /// to the edge. Both are what a `wgpu` linear sampler with `ClampToEdge` does, so this is not a
    /// convention chosen here but one matched to there.
    #[must_use]
    pub fn sample(&self, u: f32, v: f32) -> f32 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "stamp sides are bounded by MAX_STAMP_SIDE"
        )]
        let (w, h) = (self.width as f32, self.height as f32);
        // Into texel space, where whole numbers land between texels and .5 lands on a centre.
        let x = u.mul_add(w, -0.5);
        let y = v.mul_add(h, -0.5);
        let x0 = x.floor();
        let y0 = y.floor();
        let fx = x - x0;
        let fy = y - y0;

        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to the image before use"
        )]
        let (ix, iy) = (x0 as i32, y0 as i32);
        let c00 = self.texel(ix, iy);
        let c10 = self.texel(ix + 1, iy);
        let c01 = self.texel(ix, iy + 1);
        let c11 = self.texel(ix + 1, iy + 1);

        let top = c10.mul_add(fx, c00 * (1.0 - fx));
        let bottom = c11.mul_add(fx, c01 * (1.0 - fx));
        bottom.mul_add(fy, top * (1.0 - fy))
    }

    /// One texel as `0.0..=1.0`, clamped to the edge.
    fn texel(&self, x: i32, y: i32) -> f32 {
        #[expect(
            clippy::cast_possible_wrap,
            reason = "stamp sides are bounded by MAX_STAMP_SIDE"
        )]
        let (w, h) = (self.width as i32, self.height as i32);
        #[expect(clippy::cast_sign_loss, reason = "clamped to 0 on the line above")]
        let (cx, cy) = (x.clamp(0, w - 1) as usize, y.clamp(0, h - 1) as usize);
        f32::from(self.coverage[cy * self.width as usize + cx]) / 255.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 with a distinct value per texel, so a transposed or flipped read cannot pass.
    fn quad() -> Stamp {
        Stamp::new(2, 2, vec![0, 85, 170, 255]).expect("valid")
    }

    #[test]
    fn a_stamp_keeps_what_it_was_given() {
        let s = quad();
        assert_eq!((s.width(), s.height()), (2, 2));
        assert_eq!(s.coverage(), [0, 85, 170, 255]);
    }

    #[test]
    fn degenerate_stamps_are_refused() {
        assert_eq!(Stamp::new(0, 4, Vec::new()), Err(StampError::Empty));
        assert_eq!(Stamp::new(4, 0, Vec::new()), Err(StampError::Empty));
        assert_eq!(
            Stamp::new(2, 2, vec![0; 3]),
            Err(StampError::WrongLength {
                expected: 4,
                found: 3
            })
        );
        assert!(matches!(
            Stamp::new(MAX_STAMP_SIDE + 1, 1, vec![0; MAX_STAMP_SIDE as usize + 1]),
            Err(StampError::TooLarge { .. })
        ));
    }

    /// Texel centres, which is where sampling has to be exact — everything else is interpolation
    /// between these.
    #[test]
    fn sampling_a_texel_centre_reads_it_exactly() {
        let s = quad();
        assert!((s.sample(0.25, 0.25) - 0.0).abs() < 1e-4, "top-left");
        assert!(
            (s.sample(0.75, 0.25) - 85.0 / 255.0).abs() < 1e-4,
            "top-right"
        );
        assert!(
            (s.sample(0.25, 0.75) - 170.0 / 255.0).abs() < 1e-4,
            "bottom-left"
        );
        assert!((s.sample(0.75, 0.75) - 1.0).abs() < 1e-4, "bottom-right");
    }

    /// v increases downward, matching the texture the GPU reads. A flipped tip is the kind of bug
    /// that looks plausible until someone uses an asymmetric brush.
    #[test]
    fn v_increases_downward() {
        let s = Stamp::new(1, 2, vec![0, 255]).expect("valid");
        assert!(s.sample(0.5, 0.1) < 0.2, "the top row is the first row");
        assert!(s.sample(0.5, 0.9) > 0.8, "the bottom row is the last");
    }

    #[test]
    fn sampling_between_texels_interpolates() {
        let s = Stamp::new(2, 1, vec![0, 255]).expect("valid");
        let mid = s.sample(0.5, 0.5);
        assert!(
            (mid - 0.5).abs() < 0.02,
            "halfway between 0 and 255 should be about half, got {mid}"
        );
        assert!(
            s.sample(0.4, 0.5) < mid && s.sample(0.6, 0.5) > mid,
            "monotone"
        );
    }

    /// Outside the image, the edge repeats rather than wrapping. Wrapping would put the far side of
    /// a tip against its near side, which shows up as a seam around every dab.
    #[test]
    fn sampling_outside_clamps_to_the_edge() {
        let s = Stamp::new(2, 1, vec![0, 255]).expect("valid");
        assert!(
            (s.sample(-5.0, 0.5) - 0.0).abs() < 1e-4,
            "left of the image"
        );
        assert!((s.sample(5.0, 0.5) - 1.0).abs() < 1e-4, "right of it");
        assert!(
            (s.sample(-5.0, 0.5) - s.sample(0.0, 0.5)).abs() < 1e-4,
            "the edge should extend, not wrap"
        );
    }

    /// An exported-from-a-paint-app tip: opaque where the mark is.
    #[test]
    fn a_transparent_image_reads_its_alpha() {
        // Two pixels, both black, one opaque and one clear.
        let rgba = vec![0, 0, 0, 255, 0, 0, 0, 0];
        let s = Stamp::from_rgba(2, 1, &rgba).expect("valid");
        assert_eq!(s.coverage(), [255, 0], "the opaque pixel is the mark");
    }

    /// A Photoshop-style tip: black on white, fully opaque.
    #[test]
    fn an_opaque_image_reads_inverted_luminance() {
        // Black then white, both opaque.
        let rgba = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let s = Stamp::from_rgba(2, 1, &rgba).expect("valid");
        assert_eq!(
            s.coverage(),
            [255, 0],
            "black is the mark when there is no alpha to go on"
        );
    }

    /// The two rules must agree about which end is ink, or a tip would invert depending on how it
    /// happened to be saved.
    #[test]
    fn both_conventions_put_the_mark_in_the_same_place() {
        let with_alpha = Stamp::from_rgba(2, 1, &[0, 0, 0, 255, 0, 0, 0, 0]).expect("valid");
        let without = Stamp::from_rgba(2, 1, &[0, 0, 0, 255, 255, 255, 255, 255]).expect("valid");
        assert_eq!(with_alpha.coverage(), without.coverage());
    }

    /// Luminance is weighted, not averaged: a saturated green is much lighter than a saturated
    /// blue, and a flat average would make a coloured tip come out wrong.
    #[test]
    fn luminance_is_perceptual() {
        let green = Stamp::from_rgba(1, 1, &[0, 255, 0, 255]).expect("valid");
        let blue = Stamp::from_rgba(1, 1, &[0, 0, 255, 255]).expect("valid");
        assert!(
            green.coverage()[0] < blue.coverage()[0],
            "green should read as lighter (less ink) than blue, got {} and {}",
            green.coverage()[0],
            blue.coverage()[0]
        );
    }

    #[test]
    fn rgba_of_the_wrong_length_is_refused() {
        assert_eq!(
            Stamp::from_rgba(2, 2, &[0; 12]),
            Err(StampError::WrongLength {
                expected: 16,
                found: 12
            })
        );
    }

    /// Errors reach the user, so they have to read as sentences.
    #[test]
    fn stamp_errors_explain_themselves() {
        assert!(StampError::Empty.to_string().contains("empty"));
        let big = StampError::TooLarge {
            width: 9000,
            height: 12,
        };
        assert!(big.to_string().contains("9000"), "{big}");
        assert!(big.to_string().contains(&MAX_STAMP_SIDE.to_string()));
    }
}
