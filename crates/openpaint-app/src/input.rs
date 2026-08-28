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

use winit::event::WindowEvent;

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
    /// Milliseconds since an arbitrary start, for future prediction/
    /// stabilization. Zero when the backend has no clock (e.g. mouse for now).
    ///
    /// Also forward-looking (see `tilt`): stroke smoothing and input prediction
    /// need per-sample timing; reserving the field now avoids a later rework.
    #[allow(dead_code)]
    pub time_ms: f64,
}

impl PenSample {
    /// Convenience constructor for a basic sample (no tilt, no timestamp).
    pub fn at(x: f64, y: f64, pressure: f32) -> Self {
        Self {
            x,
            y,
            pressure,
            tilt: (0.0, 0.0),
            time_ms: 0.0,
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
