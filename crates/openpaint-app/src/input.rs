//! Input abstraction — the durable foundation of pen handling.
//!
//! The engine and the rest of the app depend ONLY on the types in this module,
//! never on any specific input library or OS API. A concrete input source
//! (mouse now; octotablet / hand-rolled Windows Ink later) is just a backend
//! implementing [`InputBackend`] that translates its native events into our
//! [`PenEvent`]s. Swapping backends therefore touches only the backend file,
//! never the engine.
//!
//! `PenSample` is intentionally designed to hold everything the *ultimate*
//! (hand-rolled Windows Ink) path will need — pressure, tilt, timestamp — and
//! `PenEvent::Move` carries a *batch* of samples so high-frequency coalesced
//! input (pens report far faster than the display refreshes) has a home from
//! day one. Simpler backends (the mouse) just fill the basic fields and emit
//! one sample at a time; nothing above has to change when a richer backend
//! starts filling in the rest.

use std::sync::OnceLock;
use std::time::Instant;

use winit::event::WindowEvent;

/// Milliseconds on the input clock.
///
/// One shared epoch for every backend, so a timestamp from one is comparable with a timestamp
/// from another and with "now" at present time. Latency is then a subtraction rather than a
/// per-backend special case.
///
/// This is deliberately *our* clock, read when the sample reaches us, not the tablet's own. It
/// therefore cannot see the time the sample spent in the driver and the OS before arriving, and
/// any number derived from it has to be read as "our share", never as end to end. See the module
/// note in `perf.rs` for why that is still the measurement worth having.
pub fn now_ms() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// A single pen/pointer sample in window pixel coordinates.
#[derive(Clone, Copy, Debug)]
pub struct PenSample {
    /// Position in window pixels (x right, y down).
    pub x: f64,
    pub y: f64,
    /// Normalized pressure in `0.0..=1.0`. Mouse reports a constant 1.0.
    pub pressure: f32,
    /// Pen tilt, radians from vertical, as (x, y). Zero when unavailable.
    ///
    /// Read by tilt-driven brush modulation, which is what it was reserved for.
    pub tilt: (f32, f32),
    /// When this sample reached us, on the [`now_ms`] clock.
    ///
    /// **Private, and that is the point.** A private field cannot be named from another module,
    /// so a backend cannot write a struct literal at all — it has to come through [`new`] or
    /// [`at`], and the clock is not a parameter of either. The first version of this had the
    /// field public and set it in `at`, which the mouse backend uses; the pen backend builds its
    /// samples by hand and quietly kept `time_ms: 0.0`, so the whole latency readout measured
    /// time-since-launch on the one backend anybody actually draws with. Making it unforgettable
    /// is cheaper than remembering.
    ///
    /// Read by the latency measurement, and the input that stroke smoothing and prediction will
    /// need — those want to know how fast the pen was moving, which is distance over *time*, not
    /// distance over samples. Sample rate varies with pen speed on real hardware, so treating
    /// consecutive samples as evenly spaced would make speed-dependent behaviour subtly wrong.
    ///
    /// [`new`]: PenSample::new
    /// [`at`]: PenSample::at
    time_ms: f64,
}

impl PenSample {
    /// The only way to build a sample, stamped with its arrival time.
    pub fn new(x: f64, y: f64, pressure: f32, tilt: (f32, f32)) -> Self {
        Self {
            x,
            y,
            pressure,
            tilt,
            time_ms: now_ms(),
        }
    }

    /// Shorthand for a backend with no tilt to report.
    pub fn at(x: f64, y: f64, pressure: f32) -> Self {
        Self::new(x, y, pressure, (0.0, 0.0))
    }

    /// How far the pen is tilted from vertical, in radians.
    ///
    /// The magnitude of the two axes. Which *way* it leans is a separate quantity, wanted for dab
    /// angle rather than for scalar modulation, and it can be added when there is a parameter that
    /// asks for it.
    #[must_use]
    pub fn tilt_from_vertical(&self) -> f32 {
        self.tilt.0.hypot(self.tilt.1)
    }

    /// When this sample arrived, on the [`now_ms`] clock.
    pub fn time_ms(&self) -> f64 {
        self.time_ms
    }
}

/// A stroke-lifecycle event carrying one or more samples.
///
/// `Move` holds a `Vec` so a backend can deliver a whole batch of coalesced
/// samples captured since the last frame in one event, in chronological order.
#[derive(Clone, Debug)]
pub enum PenEvent {
    /// Pen/button went down — the start of a stroke.
    Down(PenSample),
    /// Movement while down. Contains 1..N samples in time order.
    Move(Vec<PenSample>),
    /// Pen/button lifted — the end of a stroke.
    Up,
    /// The pointer moved without drawing.
    ///
    /// Its own event rather than a flag on `Move`, because the two mean different things to
    /// everything downstream: `Move` extends a stroke, `Hover` only says where the pointer is.
    ///
    /// Carried through the pen seam rather than read off winit's `CursorMoved`, because a pen
    /// hovering over a tablet is not guaranteed to produce mouse motion at all — whether Windows
    /// synthesises it depends on the driver and on what RealTimeStylus decides to consume. The
    /// backend that already knows the pen's pose is the one that can answer this reliably.
    Hover(PenSample),
}

/// A source of pen input. Implementors translate their native input into
/// [`PenEvent`]s, appending any produced events to `out`.
///
/// Two delivery models are supported because real backends differ:
/// - **Event-driven** (the mouse): input arrives as winit [`WindowEvent`]s →
///   [`InputBackend::process_window_event`].
/// - **Polled** (octotablet, and typically native tablet APIs): input lives in
///   the backend's own queue, drained once per frame → [`InputBackend::poll`].
///
/// A backend implements whichever it needs; both have default no-op impls.
pub trait InputBackend {
    /// Inspect a window event and append any resulting pen events to `out`.
    /// Default: ignore window events (for purely polled backends).
    fn process_window_event(&mut self, _event: &WindowEvent, _out: &mut Vec<PenEvent>) {}

    /// Drain any queued native input into `out`. Default: nothing to poll (for
    /// purely event-driven backends).
    ///
    /// Called only from the **top of the event loop** (winit's `about_to_wait`),
    /// never from inside a window or paint event. That is a hard requirement,
    /// not a convenience: native tablet APIs can re-enter our event handlers
    /// through a nested message pump, and draining them from inside such a
    /// re-entered handler deadlocks. See the "Windows Ink reentrancy" note in
    /// `main.rs` for the full mechanism.
    fn poll(&mut self, _out: &mut Vec<PenEvent>) {}

    /// Whether this backend keeps input in its own queue and so needs the loop
    /// to keep waking up to call [`poll`]. Event-driven backends (mouse) return
    /// `false` and stay idle until a window event; polled backends (octotablet)
    /// return `true`.
    ///
    /// [`poll`]: InputBackend::poll
    fn wants_continuous_poll(&self) -> bool {
        false
    }

    /// Human-readable name of the active backend, for logging.
    fn name(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sample carries a live clock, whichever constructor built it.
    ///
    /// Pins a bug that shipped: `time_ms` was public and only `at` stamped it, so the pen backend
    /// — which assembles samples field by field, and is the one anybody actually draws with —
    /// carried `0.0`, and the latency readout reported time since launch as if it were latency.
    /// The private field now makes a struct literal impossible; this guards the constructors,
    /// which are the remaining way to get it wrong.
    ///
    /// Asserts on the *difference*, not on either value: the epoch starts at the first call, so
    /// the very first sample legitimately reads near zero. A difference cannot be faked by a
    /// stopped clock.
    #[test]
    fn a_sample_is_stamped_when_it_is_made() {
        let first = PenSample::at(1.0, 2.0, 0.5);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = PenSample::new(3.0, 4.0, 0.5, (0.1, 0.2));

        let elapsed = second.time_ms() - first.time_ms();
        assert!(
            elapsed >= 1.0,
            "5 ms apart should show as at least 1 ms, got {elapsed}"
        );
    }
}
