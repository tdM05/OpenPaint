//! OpenPaint desktop application shell.
//!
//! Phase 0 progress:
//!   step 1 — open a window (done).
//!   step 2 — stand up wgpu, clear surface (done).
//!   step 3 — tiled canvas + mouse drawing (done).
//!   step 4 — input abstraction: all drawing flows through PenEvent/PenSample,
//!            with the mouse as the first swappable backend.
//!   step 5 — octotablet backend (Windows Ink) behind the same trait (this).
//!
//! NOTE: console stays attached on Windows for now (GPU logs + errors). We add
//! `#![windows_subsystem = "windows"]` before a real release.
//!
//! # Windows Ink reentrancy — why polling happens in `about_to_wait`
//!
//! On Windows, RealTimeStylus (which octotablet wraps) delivers its plugin
//! callbacks **on our own UI thread**, from a window it owns on that thread.
//! Those callbacks hold octotablet's internal state lock while making
//! out-of-process COM calls to the tablet service, and a COM call from an STA
//! *pumps the message queue while it waits*. That nested pump dispatches our
//! pending `WM_PAINT` straight back into winit, re-entering our handlers while
//! octotablet's lock is still held.
//!
//! So if we drain the pen backend from inside a window/paint event, the second
//! (re-entered) `poll` tries to take a lock the interrupted outer call already
//! holds, on the same thread. Rust locks aren't reentrant: the app deadlocks on
//! its first frame, at 0% CPU. That is exactly what happened in step 5.
//!
//! Two rules keep this safe, and both matter:
//!   1. **Only drain input from [`ApplicationHandler::about_to_wait`]**, which
//!      winit calls solely from the top of its own loop (never from a window
//!      procedure), so no foreign frame can be holding the backend's lock.
//!   2. **Guard our handlers against reentrancy** (`in_dispatch`), so a nested
//!      pump can never re-enter our GPU or input work part-way through.

mod canvas_renderer;
mod gpu;
mod input;
mod input_mouse;
#[cfg(target_os = "windows")]
mod input_pen;

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use gpu::Gpu;
use input::{InputBackend, PenEvent};
use input_mouse::MouseBackend;

/// Fallback drain cadence for a polled input backend.
///
/// Windows Ink normally posts a thread message when new pen data lands, which
/// wakes the event loop on its own and gets us a drain immediately — so this is
/// a safety net for backends that queue silently, not the primary cadence. Kept
/// short so it can't add meaningful latency if it *is* what wakes us. Revisit
/// with real latency numbers in step 6.
const POLL_INTERVAL: Duration = Duration::from_millis(4);

/// App state. The GPU context is created lazily in `resumed`, per the winit
/// 0.30 lifecycle contract.
struct OpenPaint {
    gpu: Option<Gpu>,
    /// The active input source. Boxed so the backend is swappable at runtime;
    /// the rest of the app only sees `PenEvent`s, never the concrete backend.
    input: Box<dyn InputBackend>,
    /// Scratch buffer reused each event to avoid per-event allocation.
    pen_events: Vec<PenEvent>,
    /// Set while we're inside one of our own event handlers, so a nested
    /// message pump can't re-enter our GPU/input work. See the module-level
    /// "Windows Ink reentrancy" note — without this, a re-entered frame can
    /// deadlock on a lock its own interrupted outer call is holding.
    in_dispatch: bool,
}

impl Default for OpenPaint {
    fn default() -> Self {
        Self {
            gpu: None,
            input: Box::new(MouseBackend::new()),
            pen_events: Vec::new(),
            in_dispatch: false,
        }
    }
}

impl OpenPaint {
    /// Apply a decoded pen event to the canvas.
    fn handle_pen_event(gpu: &mut Gpu, event: &PenEvent) {
        match event {
            PenEvent::Down(sample) => gpu.stroke_begin(sample),
            PenEvent::Move(samples) => {
                for sample in samples {
                    gpu.stroke_to(sample);
                }
            }
            PenEvent::Up => gpu.stroke_end(),
        }
    }

    /// Drain the input backend and apply whatever it produced.
    ///
    /// Only ever called from `about_to_wait` — see the module-level reentrancy
    /// note for why touching the backend from a window/paint event deadlocks.
    fn drain_input(&mut self) {
        self.pen_events.clear();
        self.input.poll(&mut self.pen_events);
        if let Some(gpu) = self.gpu.as_mut() {
            for pe in self.pen_events.drain(..) {
                Self::handle_pen_event(gpu, &pe);
            }
        }
    }

    /// The real body of [`ApplicationHandler::window_event`], wrapped by the
    /// reentrancy guard.
    fn dispatch_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };

        // Window/lifecycle events the shell handles directly.
        match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(new_size) => {
                gpu.resize(*new_size);
                gpu.window().request_redraw();
                return;
            }
            WindowEvent::RedrawRequested => {
                // Render only. Input is drained in `about_to_wait`; draining it
                // here is what deadlocked step 5 (module-level note).
                match gpu.render() {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        gpu.reconfigure();
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        eprintln!("GPU out of memory — exiting");
                        event_loop.exit();
                    }
                    Err(wgpu::SurfaceError::Timeout) => {}
                }
                return;
            }
            _ => {}
        }

        // Everything else is offered to the input backend, which turns native
        // events into our PenEvents. The shell knows nothing about mice or pens.
        self.pen_events.clear();
        self.input
            .process_window_event(&event, &mut self.pen_events);
        for pe in self.pen_events.drain(..) {
            Self::handle_pen_event(gpu, &pe);
        }
    }
}

impl ApplicationHandler for OpenPaint {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        let title = format!("OpenPaint  ({})", openpaint_core::VERSION);
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(1280.0, 800.0));

        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(err) => {
                eprintln!("failed to create window: {err}");
                event_loop.exit();
                return;
            }
        };

        // Select the input backend now that we have a window. On Windows, try
        // the pen backend (octotablet / Windows Ink); if it can't connect, keep
        // the mouse backend from `Default`. Other platforms use the mouse until
        // their own backend exists. This is the one swap point — the engine is
        // untouched regardless of which backend wins.
        #[cfg(target_os = "windows")]
        if let Some(pen) = input_pen::PenBackend::try_new(window.clone()) {
            self.input = Box::new(pen);
        }

        match Gpu::new(window) {
            Ok(gpu) => {
                println!("{} — {}", openpaint_core::hello(), openpaint_core::VERSION);
                println!("input backend: {}", self.input.name());
                // Ask for the first frame explicitly. Redraws are demand-driven
                // now (strokes and resizes request them), so nothing else would
                // paint the initial canvas.
                gpu.window().request_redraw();
                self.gpu = Some(gpu);
            }
            Err(err) => {
                eprintln!("failed to initialize GPU: {err}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // A nested message pump (see the module-level Windows Ink note) can land
        // us back here mid-frame. Bail out rather than re-entering GPU work.
        if self.in_dispatch {
            return;
        }
        self.in_dispatch = true;
        self.dispatch_window_event(event_loop, event);
        self.in_dispatch = false;
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.input.wants_continuous_poll() {
            // Event-driven backends (mouse) stay idle until a real window event,
            // keeping the app at 0% CPU when nothing is happening.
            return;
        }

        // winit calls `about_to_wait` only from the top of its own loop, never
        // from a window procedure, so this is the one place it's safe to touch a
        // polled backend. The guard is belt-and-braces.
        if !self.in_dispatch {
            self.in_dispatch = true;
            self.drain_input();
            self.in_dispatch = false;
        }

        // Keep waking up to drain the backend. Note we deliberately do NOT
        // request a redraw here: doing so would leave a `WM_PAINT` permanently
        // pending, which is precisely what a nested pump would dispatch back
        // into us. Painting is demand-driven from strokes and resizes instead.
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL));
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = OpenPaint::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
