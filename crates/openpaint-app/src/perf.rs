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
        }
    }
}

/// Mean and peak for each measurement, or `None` where nothing has been measured.
#[derive(Clone, Copy, Debug, Default)]
pub struct PerfSnapshot {
    pub input: Option<(f32, f32)>,
    pub frame: Option<(f32, f32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_measured_reports_nothing() {
        assert_eq!(Rolling::default().summary(), None);
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
