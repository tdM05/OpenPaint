//! Rolling timing measurements.
//!
//! DECISIONS §4.1 names input latency the project's top quality axis, and until this module
//! existed nothing measured it — every judgement about how the app *felt* was a guess. The point
//! of these numbers is not that they make anything faster; it is that they make claims about
//! speed falsifiable, and tell a real regression apart from a bad mood.
//!
//! **What is measured, precisely**, because a latency figure without its boundaries is worse than
//! no figure at all:
//!
//! - **Stroke latency** runs from the moment a pen sample reaches *us* to the moment the frame
//!   containing it has been presented. It excludes the time the sample spent in the tablet, its
//!   driver and the OS on the way in, and it excludes the gap between `present` returning and the
//!   display actually lighting those pixels. Both are real, and neither is ours to fix.
//! - **Frame time** is how long producing that frame took on the CPU, including the wait for the
//!   surface.
//!
//! So these are a *lower bound* on what the artist experiences, covering the part we can act on.
//! Measuring the whole pen-to-photon path needs a high-speed camera. Measuring our share of it
//! needs only this, so this comes first.
//!
//! # Sample rate, which answers a different question
//!
//! Latency asks *how late* a sample is. Rate asks **how many of them we get at all**, and that is
//! the question Phase 0's step 6 left open: a pen reports around 200 times a second while a
//! display refreshes 60, so an input path that hands over one sample per frame is discarding two
//! thirds of the pen's motion. That does not show up as lag — it shows up as a fast curve coming
//! out faceted, which is easy to blame on the brush engine.
//!
//! Two numbers settle it. **Rate** is device-facing: near the tablet's own report rate means
//! nothing is being lost, near the frame rate means it is. **Step** is artist-facing: how far the
//! pen moved between consecutive samples, in page pixels, which is the distance the brush engine
//! then has to interpolate across and therefore the thing that actually becomes a flat spot.

/// A fixed-size window of millisecond timings.
///
/// Bounded, so a session left open all day cannot grow it. Reports mean *and* peak because the
/// mean hides the occasional long frame, and a single stutter mid-stroke is exactly the thing an
/// artist notices and an average does not.
#[derive(Clone, Debug)]
pub struct Rolling {
    samples: [f32; Self::LEN],
    /// Where the next sample goes.
    next: usize,
    /// How many entries are real, until the window has filled once.
    filled: usize,
}

impl Default for Rolling {
    fn default() -> Self {
        Self {
            samples: [0.0; Self::LEN],
            next: 0,
            filled: 0,
        }
    }
}

impl Rolling {
    /// Two seconds at 60 Hz: long enough to average out noise, short enough that the number still
    /// visibly reacts while you watch it.
    const LEN: usize = 120;

    pub fn push(&mut self, ms: f32) {
        self.samples[self.next] = ms;
        self.next = (self.next + 1) % Self::LEN;
        self.filled = (self.filled + 1).min(Self::LEN);
    }

    /// Mean and peak over the window, or `None` if nothing has been measured yet.
    ///
    /// Returned together because they are only ever shown together, and because computing them
    /// in one pass means they cannot disagree about which samples are live.
    pub fn summary(&self) -> Option<(f32, f32)> {
        if self.filled == 0 {
            return None;
        }
        let live = &self.samples[..self.filled];
        let sum: f32 = live.iter().sum();
        let peak = live.iter().copied().fold(0.0_f32, f32::max);
        Some((sum / self.filled as f32, peak))
    }
}

/// How often samples arrive, over a rolling window of them.
///
/// A rate, not a duration, so it cannot reuse [`Rolling`]: averaging the gaps between samples
/// gives the wrong answer when they arrive in bursts, which is exactly what a polled backend
/// produces. Several samples land within microseconds of each other and then nothing comes for a
/// frame — the mean gap says "very fast" and the truth is "sixty bursts a second". Counting over
/// elapsed wall-clock time is immune to that.
#[derive(Clone, Debug, Default)]
pub struct Rate {
    /// When the window opened, on the input clock.
    started_ms: Option<f64>,
    /// When the most recent sample arrived.
    latest_ms: f64,
    count: u32,
    /// The last completed window's rate, so the readout does not flicker to zero between windows.
    last: Option<f32>,
}

impl Rate {
    /// How much wall-clock time a window covers before it is reported and restarted.
    ///
    /// Long enough that a burst does not dominate it, short enough to react while you watch.
    const WINDOW_MS: f64 = 500.0;

    /// Note a sample that arrived at `time_ms` on the input clock.
    pub fn push(&mut self, time_ms: f64) {
        let started = *self.started_ms.get_or_insert(time_ms);
        self.latest_ms = time_ms;
        self.count += 1;
        let elapsed = time_ms - started;
        if elapsed >= Self::WINDOW_MS {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a rate in hertz, far inside f32"
            )]
            {
                self.last = Some((f64::from(self.count) / elapsed * 1000.0) as f32);
            }
            self.started_ms = Some(time_ms);
            self.count = 0;
        }
    }

    /// Samples per second over the last completed window, or `None` before one has closed.
    ///
    /// Goes stale rather than resetting when drawing stops: the number describes the last stroke,
    /// which is the one worth reading. A zero here would mean "the pen reports nothing", which is
    /// a different and much more alarming claim than "you are not drawing right now".
    #[must_use]
    pub fn per_second(&self) -> Option<f32> {
        self.last
    }
}

/// The timings the readout shows.
#[derive(Clone, Debug, Default)]
pub struct Perf {
    /// Pen sample arrival to frame presented, recorded only on frames that carried a sample.
    ///
    /// Only those frames, deliberately: a frame drawn because the UI wanted a repaint has no
    /// input in it, and counting it would drag the average towards whatever the idle path costs
    /// rather than what drawing costs.
    pub input: Rolling,
    /// How long producing each frame took.
    pub frame: Rolling,
    /// How many pen samples arrive per second while drawing.
    pub rate: Rate,
    /// How far the pen moved between consecutive samples, in page pixels.
    ///
    /// The peak is the number that matters and the mean is context: one long step in a stroke is a
    /// flat spot in a curve, and an average over a slow stroke would hide it completely.
    pub step: Rolling,
}

impl Perf {
    /// Copy the numbers out.
    ///
    /// Needed because the UI is built inside a closure that already holds the renderer mutably,
    /// so it reads a snapshot rather than borrowing. The readout is therefore one frame old,
    /// which is invisible at these rates.
    pub fn snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            input: self.input.summary(),
            frame: self.frame.summary(),
            rate: self.rate.per_second(),
            step: self.step.summary(),
        }
    }
}

/// Mean and peak for each measurement, or `None` where nothing has been measured.
#[derive(Clone, Copy, Debug, Default)]
pub struct PerfSnapshot {
    pub input: Option<(f32, f32)>,
    pub frame: Option<(f32, f32)>,
    /// Pen samples per second, over the last completed window.
    pub rate: Option<f32>,
    /// Page pixels between consecutive samples: mean and peak.
    pub step: Option<(f32, f32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_measured_reports_nothing() {
        assert_eq!(Rolling::default().summary(), None);
        assert_eq!(Rate::default().per_second(), None);
    }

    /// Bursty arrival must not read as a high rate.
    ///
    /// **The whole reason this is not a `Rolling` of gaps.** A polled backend delivers several
    /// samples within microseconds and then nothing for a frame; averaging the gaps would call
    /// that thousands of samples a second, which is the opposite of the truth and would answer
    /// step 6 backwards. Sixty bursts of three is a hundred and eighty a second, and that is what
    /// this has to say.
    #[test]
    fn a_bursty_stream_reports_its_real_rate() {
        let mut rate = Rate::default();
        let mut t = 0.0_f64;
        // 60 bursts of 3, one burst every 16.67 ms: 180 samples per second.
        for _ in 0..60 {
            for _ in 0..3 {
                rate.push(t);
                t += 0.05;
            }
            t += 16.6;
        }
        let hz = rate.per_second().expect("a window closed");
        assert!(
            (150.0..=210.0).contains(&hz),
            "sixty bursts of three is about 180 a second, got {hz}"
        );
    }

    /// One sample per frame reads as the frame rate, which is the failure step 6 is looking for.
    #[test]
    fn one_sample_per_frame_reads_as_the_frame_rate() {
        let mut rate = Rate::default();
        let mut t = 0.0_f64;
        for _ in 0..120 {
            rate.push(t);
            t += 16.67;
        }
        let hz = rate.per_second().expect("a window closed");
        assert!(
            (50.0..=70.0).contains(&hz),
            "one a frame at 60 Hz is about 60 a second, got {hz}"
        );
    }

    /// The rate goes stale rather than resetting, because zero would be a different claim.
    #[test]
    fn the_rate_survives_the_pen_being_lifted() {
        let mut rate = Rate::default();
        let mut t = 0.0_f64;
        for _ in 0..200 {
            rate.push(t);
            t += 5.0;
        }
        let during = rate.per_second().expect("a window closed");
        // Nothing more arrives. The number still describes the stroke that happened.
        assert_eq!(rate.per_second(), Some(during));
    }

    #[test]
    fn a_partly_filled_window_averages_only_what_is_in_it() {
        let mut r = Rolling::default();
        r.push(10.0);
        r.push(20.0);
        // Not 30/120: the unwritten entries are absent, not zero. Averaging over the whole array
        // would make every early reading look impossibly fast.
        assert_eq!(r.summary(), Some((15.0, 20.0)));
    }

    #[test]
    fn old_samples_leave_the_window() {
        let mut r = Rolling::default();
        for _ in 0..Rolling::LEN {
            r.push(1.0);
        }
        assert_eq!(r.summary(), Some((1.0, 1.0)));
        // A full window's worth of new values must displace every old one, peak included --
        // otherwise a single early stall would haunt the readout for the rest of the session.
        for _ in 0..Rolling::LEN {
            r.push(3.0);
        }
        assert_eq!(r.summary(), Some((3.0, 3.0)));
    }

    #[test]
    fn the_peak_is_not_lost_in_the_mean() {
        let mut r = Rolling::default();
        for _ in 0..50 {
            r.push(2.0);
        }
        r.push(40.0);
        let (mean, peak) = r.summary().expect("samples were pushed");
        assert!(
            mean < 3.0,
            "one stall should barely move the mean, got {mean}"
        );
        assert_eq!(peak, 40.0, "but it must still be visible as the peak");
    }
}
