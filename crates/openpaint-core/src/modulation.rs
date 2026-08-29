//! What drives a brush parameter, and how.
//!
//! # One pattern, not several features
//!
//! Every modulatable brush parameter is a [`Response`]: a [`Source`] to read, and a
//! [`crate::Curve`] mapping it to a multiplier of the parameter's own setting. Pressure driving
//! size is one instance of that pattern, not a feature in its own right — which matters, because
//! the alternative is a "follow stroke direction" flag on angle, a "pressure affects size"
//! checkbox, a "velocity taper" toggle, and no two of them behaving alike.
//!
//! This is what DECISIONS §4c committed to: per-dab modulation as a composable, serialisable,
//! user-authored thing, "each optionally driven by pressure/tilt/velocity through a curve". Built
//! before dab shape deliberately — angle wants to follow *direction or tilt*, so adding angle first
//! would have meant adding it with special cases and taking them out again.
//!
//! # The curve is always a multiplier
//!
//! Uniformly, for every parameter: the slider is what you get at full input, and the curve says
//! what fraction of it arrives. Uniform beats convenient — the alternative is a curve that scales
//! some parameters and replaces others, and then no artist can predict what a curve will do without
//! remembering which kind they are looking at.
//!
//! # One parameter is deliberately absent
//!
//! **Opacity is not modulatable, and that is a property of the model rather than an omission.** It
//! is the ceiling the whole stroke may reach ([`crate::stroke`]), which is what stops overlapping
//! dabs building past it. A ceiling that varied per dab would not be a ceiling, and the blotchy
//! overlap it prevents is exactly what the accumulation buffer exists to avoid. Flow is the per-dab
//! quantity, and it is modulatable.

use crate::curve::Curve;

/// The reference speed at which [`Source::Velocity`] reads 1.0, in page pixels per millisecond.
///
/// A scale, not a threshold: it says what "fast" means so the curve's x axis has units. Three
/// pixels per millisecond is a brisk flick across a page. Any disagreement is expressible in the
/// curve itself, which is why this being approximate costs nothing.
pub const VELOCITY_FULL: f32 = 3.0;

/// The tilt at which [`Source::Tilt`] reads 1.0, in radians from vertical.
///
/// Sixty degrees. Pens report further than that, but past it the reported angle gets noisy and the
/// nib is barely on the page, so mapping the useful range to the full curve is worth more than
/// reserving room at the end for readings nobody draws with.
pub const TILT_FULL: f32 = std::f32::consts::FRAC_PI_3;

/// Where a parameter looks for its input.
///
/// Every one yields `0.0..=1.0`, so any source can drive any parameter and a curve authored against
/// one reads sensibly against another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Source {
    /// How hard the pen is pressed.
    Pressure,
    /// How far the pen is tilted from vertical, against [`TILT_FULL`].
    Tilt,
    /// How fast the pen is travelling, against [`VELOCITY_FULL`].
    ///
    /// The source that gives tapered ink: a fast flick thins the line at both ends without the
    /// artist doing anything.
    Velocity,
    /// Which way the stroke is heading, as a fraction of a full turn.
    ///
    /// Meant for dab *angle*, where an identity curve makes the dab follow the stroke — which is
    /// what a chisel nib does, and most of what inked line weight is.
    Direction,
    /// A different value per dab.
    ///
    /// Reproducible under undo for free, and worth saying why: history stores the **dabs**, not the
    /// gesture, so the numbers are already drawn by the time anything is recorded. A redo replays
    /// what happened rather than rolling again.
    Random,
}

impl Source {
    /// Every source, for a UI to offer.
    pub const ALL: [Self; 5] = [
        Self::Pressure,
        Self::Tilt,
        Self::Velocity,
        Self::Direction,
        Self::Random,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pressure => "Pressure",
            Self::Tilt => "Tilt",
            Self::Velocity => "Velocity",
            Self::Direction => "Direction",
            Self::Random => "Random",
        }
    }

    /// Read this source out of one dab's worth of input.
    #[must_use]
    pub fn read(self, input: &Input) -> f32 {
        let raw = match self {
            Self::Pressure => input.pressure,
            Self::Tilt => input.tilt / TILT_FULL,
            Self::Velocity => input.velocity / VELOCITY_FULL,
            Self::Direction => input.direction,
            Self::Random => input.random,
        };
        raw.clamp(0.0, 1.0)
    }
}

/// How one parameter answers its input.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Response {
    pub source: Source,
    pub curve: Curve,
}

impl Response {
    /// A parameter that ignores its input entirely.
    ///
    /// A flat curve, not a `None` source — so "off" is a shape the same editor draws, and there is
    /// no separate switch that could disagree with the curve shown next to it.
    #[must_use]
    pub fn fixed() -> Self {
        Self {
            source: Source::Pressure,
            curve: Curve::constant(1.0),
        }
    }

    /// A parameter that follows its source exactly.
    #[must_use]
    pub fn following(source: Source) -> Self {
        Self {
            source,
            curve: Curve::linear(),
        }
    }

    /// The multiplier to apply to a parameter's setting for this dab.
    #[must_use]
    pub fn factor(&self, input: &Input) -> f32 {
        self.curve.at(self.source.read(input))
    }
}

/// Everything the sources can read, for one dab.
///
/// Assembled by the brush from consecutive samples, not by the caller: velocity and direction are
/// properties of the *path*, and only the thing walking the path knows them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input {
    /// Normalised pen pressure.
    pub pressure: f32,
    /// Tilt from vertical, in radians.
    pub tilt: f32,
    /// Speed in page pixels per millisecond.
    pub velocity: f32,
    /// Heading as a fraction of a turn, `0.0..1.0`.
    pub direction: f32,
    /// A fresh value per dab, `0.0..1.0`.
    pub random: f32,
}

/// A small deterministic generator, for [`Source::Random`].
///
/// Its own rather than a dependency: this needs a few bits of noise per dab, not statistical
/// quality, and a brush engine should not acquire a crate for it. Deterministic given its seed,
/// which matters less than it sounds — history stores dabs, so a redo never re-rolls anything.
#[derive(Clone, Copy, Debug)]
pub struct Noise(u32);

impl Noise {
    #[must_use]
    pub fn new(seed: u32) -> Self {
        // Never zero: xorshift is stuck there forever, which would make every dab identical.
        Self(seed | 1)
    }

    /// The next value in `0.0..1.0`.
    ///
    /// Named `draw` rather than `next`: this is not an iterator, and a method called `next` on a
    /// non-iterator reads as one at every call site.
    pub fn draw(&mut self) -> f32 {
        // xorshift32.
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1 << 24) as f32
    }
}

impl Default for Noise {
    fn default() -> Self {
        Self::new(0x2545_F491)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input {
            pressure: 0.5,
            tilt: TILT_FULL / 2.0,
            velocity: VELOCITY_FULL / 4.0,
            direction: 0.75,
            random: 0.3,
        }
    }

    /// Every source lands in 0..1, which is what lets any of them drive any parameter.
    #[test]
    fn every_source_is_normalised() {
        for s in Source::ALL {
            let v = s.read(&input());
            assert!((0.0..=1.0).contains(&v), "{s:?} produced {v}");
        }
        assert!((Source::Pressure.read(&input()) - 0.5).abs() < 1e-4);
        assert!((Source::Tilt.read(&input()) - 0.5).abs() < 1e-4);
        assert!((Source::Velocity.read(&input()) - 0.25).abs() < 1e-4);
        assert!((Source::Direction.read(&input()) - 0.75).abs() < 1e-4);
    }

    /// Readings past the reference are clamped rather than left to scale a parameter past its
    /// setting: a pen flicked hard should reach the brush's size, not exceed it.
    #[test]
    fn sources_clamp_rather_than_overshoot() {
        let wild = Input {
            pressure: 3.0,
            tilt: TILT_FULL * 4.0,
            velocity: VELOCITY_FULL * 10.0,
            direction: 1.0,
            random: 1.0,
        };
        for s in Source::ALL {
            assert!(
                (0.0..=1.0).contains(&s.read(&wild)),
                "{s:?} escaped the range"
            );
        }
    }

    #[test]
    fn fixed_ignores_everything() {
        let r = Response::fixed();
        for s in Source::ALL {
            let r = Response {
                source: s,
                curve: r.curve.clone(),
            };
            assert!((r.factor(&input()) - 1.0).abs() < 1e-4, "{s:?}");
        }
    }

    #[test]
    fn following_a_source_tracks_it() {
        let r = Response::following(Source::Pressure);
        assert!((r.factor(&input()) - 0.5).abs() < 1e-3);
        let r = Response::following(Source::Direction);
        assert!((r.factor(&input()) - 0.75).abs() < 1e-3);
    }

    /// Noise has to actually vary, and stay in range. A stuck generator would make `Random`
    /// silently equivalent to a constant.
    #[test]
    fn noise_varies_and_stays_in_range() {
        let mut n = Noise::new(12345);
        let values: Vec<f32> = (0..64).map(|_| n.draw()).collect();
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)));

        let first = values[0];
        assert!(
            values.iter().any(|v| (v - first).abs() > 0.1),
            "the generator is stuck at {first}"
        );
    }

    /// A zero seed must not lock the generator at zero, which xorshift does.
    #[test]
    fn a_zero_seed_still_produces_noise() {
        let mut n = Noise::new(0);
        let values: Vec<f32> = (0..8).map(|_| n.draw()).collect();
        assert!(
            values.iter().any(|v| *v > 0.0),
            "a zero seed produced only zeros"
        );
    }

    /// The same seed replays the same numbers, which is what makes a brush reproducible.
    #[test]
    fn noise_is_deterministic() {
        let a: Vec<f32> = (0..16).scan(Noise::new(7), |n, _| Some(n.draw())).collect();
        let b: Vec<f32> = (0..16).scan(Noise::new(7), |n, _| Some(n.draw())).collect();
        assert_eq!(a, b);
    }
}
