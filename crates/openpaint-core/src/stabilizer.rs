//! Stroke stabilization — smoothing the input path without lying about where it ended.
//!
//! A pen reports a path that shakes. Hand tremor sits around 8–12 Hz, and cheap digitizers add
//! quantization noise on top, so a slowly drawn line comes out visibly wobbly even though the
//! artist's intent was smooth. Stabilization is the standard answer, and one of the most-used
//! settings in CSP.
//!
//! # A one-pole filter, defined in time
//!
//! The drawn point chases the pen:
//!
//! ```text
//! alpha = 1 - exp(-dt / tau)
//! point += (pen - point) * alpha
//! ```
//!
//! **`dt` is real elapsed time, not "one sample".** That is the whole reason [`PenSample`] carries
//! a clock. The naive version uses a fixed alpha per sample, and it is wrong in two ways an artist
//! feels immediately:
//!
//! - **It varies with hardware.** A 200 Hz tablet takes four times as many steps through the
//!   filter per unit of time as a 50 Hz one, so it converges four times faster. The same gesture
//!   would draw differently on two tablets, and the setting would have to be re-tuned per device.
//! - **It varies with drawing speed.** Slow movement produces more samples per unit distance,
//!   hence more smoothing exactly where the artist is being careful — the opposite of wanted.
//!
//! Defining the filter in time removes both. `tau` is a duration, and the amount of smoothing is a
//! property of the setting alone.
//!
//! # The setting *is* the lag
//!
//! A one-pole filter tracking a steady movement settles exactly `tau` behind it. So `tau` is not an
//! abstract strength that happens to cost latency — it is the latency, in milliseconds, exactly.
//!
//! The setting is therefore expressed in milliseconds rather than as a 0–1 strength. An abstract
//! strength would need a maximum to scale against, and any such maximum is a number somebody made
//! up; worse, it would hide the one fact the artist most needs, given that latency is this
//! project's top quality axis (DECISIONS §4.1). "24 ms of lag" is a price. "0.48" is a mystery.
//!
//! # Ending where the pen ended
//!
//! Because the filter trails, at the instant the pen lifts the drawn point is short of the pen's
//! real final position — by roughly the lag distance. Left alone, every stroke would stop early,
//! which ruins tapers and makes strokes fail to meet. [`Stabilizer::finish`] converges the
//! remaining distance instead of jumping it, so the approach decelerates the same way the rest of
//! the stroke did, and then lands exactly on the true endpoint.
//!
//! [`PenSample`]: https://docs.rs/  (the app's input type; see `openpaint-app/src/input.rs`)

/// The largest lag a control should offer, in milliseconds.
///
/// **A UI range, not a limit of the filter** — the maths works at any value. 200 ms is about twelve
/// frames at 60 Hz, well past the point where drawing feels like dragging a weight on a string, so
/// it covers even the heaviest inking use with room to spare. Raising it changes nothing except how
/// far a slider can travel, precisely because the setting is a real unit rather than a fraction of
/// this number.
pub const MAX_LAG_MS: f32 = 200.0;

/// How many convergence steps [`Stabilizer::finish`] will take before it simply lands.
///
/// Only a bound against a pathological `tau`; the loop normally exits on distance.
const MAX_TAIL_STEPS: usize = 64;

/// How close to the pen counts as arrived, in pixels.
///
/// Judged by distance *remaining*, deliberately, and not by how far the point moved on the last
/// step. A heavily smoothed filter takes its smallest steps exactly when it still has the furthest
/// to travel, so a step-size test stops it several pixels short — which looks like the line
/// refusing to quite reach a held pen. A quarter pixel is below what a dab renders, and
/// [`Stabilizer::finish`] closes even that exactly.
const ARRIVED_PX: f32 = 0.25;

/// Interval used for the convergence tail's steps.
///
/// The tail is emitted all at once, so this does not delay anything: it only sets how finely the
/// remaining distance is subdivided. Roughly half a frame, which is finer than dab spacing will
/// resolve anyway.
const TAIL_STEP_MS: f32 = 8.0;

/// One point on the smoothed path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Smoothed {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

/// Smooths an input path in time. One per stroke sequence; [`Stabilizer::begin`] resets it.
///
/// Pressure is filtered alongside position, with the same time constant. Pressure noise shows up
/// as width wobble along an otherwise clean line, so smoothing the path and not the pressure would
/// leave half the artifact in place.
#[derive(Clone, Debug, Default)]
pub struct Stabilizer {
    /// Time constant in milliseconds. Zero is a pass-through.
    tau_ms: f32,
    /// The smoothed point, and the timestamp it was last advanced to.
    at: Option<(Smoothed, f64)>,
    /// The latest raw sample, which the tail converges onto.
    target: Option<Smoothed>,
}

impl Stabilizer {
    /// Set how far the line may trail the pen, in milliseconds. Zero disables smoothing entirely.
    ///
    /// The argument is both the filter's time constant and, exactly, the latency it adds — see the
    /// module note. Negative values are clamped; there is no upper clamp, because there is no value
    /// at which the filter stops being correct, only values at which it stops being pleasant.
    pub fn set_lag_ms(&mut self, lag_ms: f32) {
        self.tau_ms = lag_ms.max(0.0);
    }

    /// Whether smoothing is doing anything at all.
    ///
    /// Lets a caller skip keeping a frame loop alive for a stroke that is a pass-through: with no
    /// smoothing the line is never behind the pen, so there is nothing for time to catch up.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.tau_ms > 0.0
    }

    /// Start a stroke. The first point is never moved — a stroke has to begin under the pen.
    pub fn begin(&mut self, x: f32, y: f32, pressure: f32, t_ms: f64) -> Smoothed {
        let first = Smoothed { x, y, pressure };
        self.at = Some((first, t_ms));
        self.target = Some(first);
        first
    }

    /// Advance the smoothed point toward a new sample and return where it now is.
    pub fn push(&mut self, x: f32, y: f32, pressure: f32, t_ms: f64) -> Smoothed {
        let raw = Smoothed { x, y, pressure };
        self.target = Some(raw);

        let Some((point, _)) = self.at else {
            return self.begin(x, y, pressure, t_ms);
        };
        if self.tau_ms <= 0.0 {
            self.at = Some((raw, t_ms));
            return raw;
        }
        self.step(t_ms).unwrap_or(point)
    }

    /// Let time pass without a new sample, and report the new point if it moved enough to draw.
    ///
    /// **A filter defined in time has to be driven by time.** Advancing only on samples looks
    /// correct while the pen is moving and breaks the moment it stops: the line freezes wherever
    /// it had got to, still short of the pen, and then the next sample carries the whole
    /// accumulated `dt`, so `alpha` is near 1 and the line snaps forward in one straight segment.
    /// That is a bug that only shows up at high strength and speed — where the trailing distance
    /// is large enough to see — which is exactly how it was found.
    ///
    /// `None` means "nothing worth a frame", which is the caller's signal to stop asking for them.
    /// The timestamp still advances in that case: skipping it would bank the idle time and hand it
    /// to the next real sample, which is the same snap by another route.
    pub fn advance(&mut self, t_ms: f64) -> Option<Smoothed> {
        if self.tau_ms <= 0.0 {
            return None;
        }
        let target = self.target?;
        let point = self.step(t_ms)?;
        ((target.x - point.x).hypot(target.y - point.y) >= ARRIVED_PX).then_some(point)
    }

    /// Move the point toward the target by however much time has passed. `None` only when there is
    /// no stroke.
    fn step(&mut self, t_ms: f64) -> Option<Smoothed> {
        let (mut point, last_t) = self.at?;
        let target = self.target?;

        // Clamped at zero rather than trusted: a backend that ever reported time going backwards
        // would otherwise produce a negative alpha and fling the point away from the pen.
        let dt = (t_ms - last_t).max(0.0) as f32;
        if dt > 0.0 && self.tau_ms > 0.0 {
            let alpha = 1.0 - (-dt / self.tau_ms).exp();
            point.x += (target.x - point.x) * alpha;
            point.y += (target.y - point.y) * alpha;
            point.pressure += (target.pressure - point.pressure) * alpha;
        }

        self.at = Some((point, t_ms));
        Some(point)
    }

    /// End the stroke, returning the points that carry it to where the pen actually finished.
    ///
    /// Empty when smoothing is off, because then the path was never behind.
    pub fn finish(&mut self) -> Vec<Smoothed> {
        let mut tail = Vec::new();
        if let (Some((mut point, _)), Some(target)) = (self.at, self.target) {
            if self.tau_ms > 0.0 {
                let alpha = 1.0 - (-TAIL_STEP_MS / self.tau_ms).exp();
                for _ in 0..MAX_TAIL_STEPS {
                    if (target.x - point.x).hypot(target.y - point.y) < 0.05 {
                        break;
                    }
                    point.x += (target.x - point.x) * alpha;
                    point.y += (target.y - point.y) * alpha;
                    point.pressure += (target.pressure - point.pressure) * alpha;
                    tail.push(point);
                }
                // Land exactly, rather than wherever the loop happened to stop. Stopping a
                // twentieth of a pixel short is invisible; stopping short because a bound was hit
                // is a stroke that misses what it was drawn to meet.
                tail.push(target);
            }
        }
        self.at = None;
        self.target = None;
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a straight ramp and report where the smoothed point ends up.
    fn ramp(lag_ms: f32, rate_ms: f64, duration_ms: f64) -> Smoothed {
        let mut s = Stabilizer::default();
        s.set_lag_ms(lag_ms);
        let mut out = s.begin(0.0, 0.0, 1.0, 0.0);
        let mut t = rate_ms;
        while t <= duration_ms {
            // 1 px per ms, so position and time are numerically the same and the lag reads
            // directly off the x coordinate.
            out = s.push(t as f32, 0.0, 1.0, t);
            t += rate_ms;
        }
        out
    }

    #[test]
    fn off_means_untouched() {
        let mut s = Stabilizer::default();
        s.begin(0.0, 0.0, 1.0, 0.0);
        let got = s.push(37.0, 11.0, 0.4, 16.0);
        assert_eq!(
            got,
            Smoothed {
                x: 37.0,
                y: 11.0,
                pressure: 0.4
            },
            "strength 0 must be a pass-through, not merely a weak filter"
        );
        assert!(
            s.finish().is_empty(),
            "nothing to catch up when nothing lags"
        );
    }

    #[test]
    fn the_first_point_is_never_moved() {
        let mut s = Stabilizer::default();
        s.set_lag_ms(50.0);
        let got = s.begin(12.0, 34.0, 0.7, 0.0);
        assert_eq!(
            got,
            Smoothed {
                x: 12.0,
                y: 34.0,
                pressure: 0.7
            },
            "a stroke has to start under the pen at any strength"
        );
    }

    /// The property the whole design rests on: smoothing is a function of *time*, so report rate
    /// cannot change the result.
    ///
    /// A filter with a fixed alpha per sample converges twice as far when the report rate
    /// doubles, so the same gesture would draw differently on two tablets and the setting would
    /// have to be re-tuned per device.
    ///
    /// Note what is *not* claimed: that any two rates agree exactly. Sampling a ramp with a
    /// zero-order hold makes the sampled target lead the true ramp by half the interval, so even a
    /// perfect time-defined filter shows a small rate-dependent offset — comparing 10 ms with 1 ms
    /// really does differ by ~4.5 px, and that is the input, not the filter. Comparing adjacent
    /// rates keeps that term under a pixel.
    #[test]
    fn smoothing_is_defined_in_time_not_in_samples() {
        let at_2ms = ramp(25.0, 2.0, 200.0).x;
        let at_1ms = ramp(25.0, 1.0, 200.0).x;
        assert!(
            (at_2ms - at_1ms).abs() < 1.0,
            "halving the report rate moved the result: {at_2ms} vs {at_1ms}"
        );

        // The same comparison for a fixed-alpha-per-sample filter, so the tolerance above cannot
        // be what is passing the test. Steady-state lag there is `step * (1 - a) / a`, and the
        // step per sample is proportional to the interval — so halving the rate halves the lag,
        // which is the hardware dependence this design exists to remove.
        let naive = |rate_ms: f64| {
            let alpha = 0.1_f32;
            let mut point = 0.0_f32;
            let mut t = rate_ms;
            while t <= 200.0 {
                point += (t as f32 - point) * alpha;
                t += rate_ms;
            }
            point
        };
        let naive_2ms = naive(2.0);
        let naive_1ms = naive(1.0);
        assert!(
            (naive_2ms - naive_1ms).abs() > 5.0,
            "the sample-defined filter was supposed to be rate-dependent, but got \
             {naive_2ms} vs {naive_1ms} — this test proves nothing if its own counter-example \
             does not move"
        );
    }

    /// The setting is stated in milliseconds of lag, so it has to *be* milliseconds of lag.
    ///
    /// This is the claim the UI repeats to the artist, and the reason the control is expressed in a
    /// real unit rather than an abstract strength. If the two ever drifted apart, the slider would
    /// be quoting a made-up number with a units label on it, which is worse than quoting nothing.
    #[test]
    fn the_setting_is_the_lag_in_milliseconds() {
        for lag in [10.0_f32, 25.0, 50.0, 120.0] {
            // 1 px per ms, run well past settling, so the shortfall in pixels *is* the lag in ms.
            let settled = ramp(lag, 1.0, lag as f64 * 12.0);
            let shortfall = lag as f64 * 12.0 - f64::from(settled.x);
            assert!(
                (shortfall - f64::from(lag)).abs() < 1.5,
                "set to {lag} ms, but the filter trails {shortfall} px at 1 px/ms"
            );
        }
    }

    #[test]
    fn more_lag_trails_further() {
        let mut previous = f32::MAX;
        for lag in [0.0_f32, 12.0, 25.0, 50.0, 100.0, MAX_LAG_MS] {
            let x = ramp(lag, 4.0, 400.0).x;
            assert!(
                x < previous,
                "{lag} ms did not trail further than the step below it"
            );
            previous = x;
        }
    }

    /// What the feature is for: a shaky path comes out straighter.
    #[test]
    fn jitter_is_reduced() {
        let mut s = Stabilizer::default();
        s.set_lag_ms(30.0);
        s.begin(0.0, 0.0, 1.0, 0.0);

        let mut raw_error = 0.0_f32;
        let mut smoothed_error = 0.0_f32;
        let mut count = 0.0_f32;
        for i in 1..=200 {
            let t = f64::from(i) * 4.0;
            // A straight line along x with the hand shaking +/-2 px across it.
            let jitter = if i % 2 == 0 { 2.0 } else { -2.0 };
            let got = s.push(i as f32, jitter, 1.0, t);
            raw_error += jitter.abs();
            smoothed_error += got.y.abs();
            count += 1.0;
        }

        let raw = raw_error / count;
        let smoothed = smoothed_error / count;
        assert!(
            smoothed < raw * 0.25,
            "smoothing barely helped: {raw} px of wobble became {smoothed} px"
        );
    }

    /// A trailing filter would otherwise stop every stroke short of where the pen lifted, which
    /// ruins tapers and leaves strokes failing to meet.
    #[test]
    fn the_stroke_ends_exactly_where_the_pen_did() {
        let mut s = Stabilizer::default();
        s.set_lag_ms(50.0);
        s.begin(0.0, 0.0, 1.0, 0.0);
        for i in 1..=50 {
            s.push(i as f32 * 3.0, 0.0, 1.0, f64::from(i) * 4.0);
        }
        let last_raw = 150.0_f32;

        let tail = s.finish();
        let end = *tail.last().expect("a lagging stroke must have a tail");
        assert_eq!(
            end.x, last_raw,
            "the stroke has to finish where the pen finished"
        );

        // And get there gradually: a single jump would draw a straight segment across whatever
        // curve the stroke was making.
        assert!(
            tail.len() > 4,
            "converged in {} steps, which is a jump rather than an approach",
            tail.len()
        );
        // Monotonic approach, never overshooting past the target.
        for pair in tail.windows(2) {
            assert!(
                pair[1].x >= pair[0].x && pair[1].x <= last_raw + 1e-3,
                "tail moved wrongly: {:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    /// The line must keep catching up while the pen is held still.
    ///
    /// The reported bug: draw fast with heavy smoothing and stop, and the line froze well short of
    /// the cursor. It froze because the filter was advanced only by arriving samples, and holding
    /// still produces none.
    #[test]
    fn a_held_pen_still_lets_the_line_catch_up() {
        let mut s = Stabilizer::default();
        s.set_lag_ms(100.0);
        s.begin(0.0, 0.0, 1.0, 0.0);
        // One big jump, as a fast drag looks to the filter, then the pen stops dead.
        let after_jump = s.push(500.0, 0.0, 1.0, 10.0);
        assert!(
            after_jump.x < 100.0,
            "the filter should still be far behind here, but is at {}",
            after_jump.x
        );

        let mut point = after_jump;
        let mut t = 10.0;
        let mut frames = 0;
        // Time passes with no new samples at all.
        while t < 1000.0 {
            t += 8.0;
            if let Some(p) = s.advance(t) {
                point = p;
                frames += 1;
            }
        }
        assert!(
            (500.0 - point.x) < 1.0,
            "after a second of holding still the line is still {} px short",
            500.0 - point.x
        );
        assert!(
            frames > 10,
            "it arrived in {frames} steps, which is a jump rather than catching up"
        );
    }

    /// A pause must not be banked and spent on the next sample.
    ///
    /// The other half of the same bug, and the subtler one: even with `advance` in place, if it
    /// declined to move the clock forward once converged, the idle time would accumulate and the
    /// next sample would arrive with a huge `dt` — `alpha` near 1, and the line teleports to the
    /// pen. Smoothing would silently switch itself off after every pause.
    #[test]
    fn a_pause_does_not_bank_time_for_the_next_sample() {
        let mut s = Stabilizer::default();
        s.set_lag_ms(50.0);
        s.begin(0.0, 0.0, 1.0, 0.0);

        // Hold still for a second. These all converge (there is nowhere to go), so `advance`
        // reports nothing -- but it must still be moving the clock.
        let mut t = 0.0;
        while t < 1000.0 {
            t += 8.0;
            s.advance(t);
        }

        // Now the pen jumps a long way, one sample, a normal frame later.
        t += 8.0;
        let after = s.push(500.0, 0.0, 1.0, t);
        assert!(
            after.x < 120.0,
            "one 8 ms sample at 50 ms of smoothing should move ~15% of the way, but it went to \
             {} -- the pause was banked and smoothing turned itself off",
            after.x
        );
    }

    /// Time running backwards is bad input, but it must not fling the point away from the pen.
    #[test]
    fn a_clock_going_backwards_does_not_explode() {
        let mut s = Stabilizer::default();
        s.set_lag_ms(25.0);
        s.begin(0.0, 0.0, 1.0, 100.0);
        let got = s.push(50.0, 0.0, 1.0, 40.0);
        assert!(
            (0.0..=50.0).contains(&got.x),
            "negative dt moved the point outside the segment: {}",
            got.x
        );
    }
}
