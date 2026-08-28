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
    /// Intentionally part of the foundation now though nothing reads it yet:
    /// the real pen backend fills it, and tilt-driven brushes consume it later.
    /// Kept here so adding those is "use existing data", not a reshape.
    #[allow(dead_code)]
    pub tilt: (f32, f32),
    /// When this sample reached us, on the [`now_ms`] clock.
    ///
    /// Read by the latency measurement, and the input that stroke smoothing and prediction will
    /// need — those want to know how fast the pen was moving, which is distance over *time*, not
    /// distance over samples. Sample rate varies with pen speed on real hardware, so treating
    /// consecutive samples as evenly spaced would make speed-dependent behaviour subtly wrong.
    pub time_ms: f64,
}

impl PenSample {
    /// Convenience constructor for a basic sample (no tilt), stamped with the arrival time.
    pub fn at(x: f64, y: f64, pressure: f32) -> Self {
        Self {
            x,
            y,
            pressure,
            tilt: (0.0, 0.0),
            time_ms: now_ms(),
        }
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
