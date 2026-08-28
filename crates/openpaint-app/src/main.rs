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
mod editor;
mod input;
mod input_mouse;
#[cfg(target_os = "windows")]
mod input_pen;
mod renderer;
mod ui;
mod view;

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use editor::Editor;
use input::{InputBackend, PenEvent, PenSample};
use input_mouse::MouseBackend;
use renderer::Renderer;
use view::View;

/// Fallback drain cadence for a polled input backend.
///
/// Windows Ink normally posts a thread message when new pen data lands, which
/// wakes the event loop on its own and gets us a drain immediately — so this is
/// a safety net for backends that queue silently, not the primary cadence. Kept
/// short so it can't add meaningful latency if it *is* what wakes us. Revisit
/// with real latency numbers in step 6.
const POLL_INTERVAL: Duration = Duration::from_millis(4);

/// The application shell: it owns the pieces and wires them together, and holds
/// no engine logic of its own.
///
/// Four separate concerns, deliberately not one object (see `renderer.rs`):
///   - [`Editor`] - the document, brush, and stroke state. No GPU, no UI.
///   - [`View`] - where the canvas sits on screen, and the screen/canvas
///     transform. This is where pan/zoom/rotate will land.
///   - [`Renderer`] - wgpu resources and frame presentation. No document, no UI.
///   - [`ui::Ui`] - the throwaway debug panel, reached only through the
///     renderer's overlay callback so the renderer stays UI-agnostic.
///
/// GPU and UI are created lazily in `resumed`, per the winit 0.30 lifecycle.
struct OpenPaint {
    editor: Editor,
    view: View,
    renderer: Option<Renderer>,
    ui: Option<ui::Ui>,
    /// The active input source. Boxed so the backend is swappable at runtime;
    /// the rest of the app only sees `PenEvent`s, never the concrete backend.
    input: Box<dyn InputBackend>,
    /// Scratch buffer reused each event to avoid per-event allocation.
    pen_events: Vec<PenEvent>,
    /// Set while we're inside one of our own event handlers, so a nested
    /// message pump can't re-enter our GPU/input work. See the module-level
    /// "Windows Ink reentrancy" note - without this, a re-entered frame can
    /// deadlock on a lock its own interrupted outer call is holding.
    in_dispatch: bool,
}

impl Default for OpenPaint {
    fn default() -> Self {
        Self {
            editor: Editor::new(),
            view: View::new(),
            renderer: None,
            ui: None,
            input: Box::new(MouseBackend::new()),
            pen_events: Vec::new(),
            in_dispatch: false,
        }
    }
}

impl OpenPaint {
    /// Apply a decoded pen event, mapping window coordinates to canvas space.
    ///
    /// The coordinate transform lives in [`View`] rather than here or in the
    /// editor, so the mapping used for input can never drift from the one used to
    /// draw the canvas.
    fn handle_pen_event(&mut self, event: &PenEvent) {
        match event {
            PenEvent::Down(sample) => {
                // Don't paint underneath the UI. This check is needed for the pen
                // specifically, because pen input never reaches egui and so egui's
                // own pointer capture cannot exclude it (OPEN_QUESTIONS Q14).
                if self.ui_blocks_point(sample) {
                    return;
                }
                if let Some((cx, cy)) = self.to_canvas(sample) {
                    self.editor.stroke_begin(cx, cy, sample.pressure);
                    self.request_redraw();
                }
            }
            PenEvent::Move(samples) => {
                if !self.editor.is_drawing() {
                    return;
                }
                for sample in samples {
                    if let Some((cx, cy)) = self.to_canvas(sample) {
                        self.editor.stroke_to(cx, cy, sample.pressure);
                    }
                }
                self.request_redraw();
            }
            PenEvent::Up => self.editor.stroke_end(),
        }
    }

    /// Map a pen sample from window pixels to canvas pixels.
    fn to_canvas(&self, sample: &PenSample) -> Option<(f32, f32)> {
        let renderer = self.renderer.as_ref()?;
        let (w, h) = renderer.size_px();
        self.view
            .screen_to_canvas(sample.x, sample.y, w, h, self.editor.canvas())
    }

    fn ui_blocks_point(&self, sample: &PenSample) -> bool {
        self.ui
            .as_ref()
            .is_some_and(|ui| ui.blocks_point(sample.x, sample.y))
    }

    fn request_redraw(&self) {
        if let Some(renderer) = self.renderer.as_ref() {
            renderer.window().request_redraw();
        }
    }

    /// Apply a batch of pen events, reusing the scratch buffer.
    fn apply_pen_events(&mut self) {
        if self.pen_events.is_empty() {
            return;
        }
        let events = std::mem::take(&mut self.pen_events);
        for pe in &events {
            self.handle_pen_event(pe);
        }
        // Put the allocation back for next time.
        self.pen_events = events;
        self.pen_events.clear();
    }

    /// Drain the input backend and apply whatever it produced.
    ///
    /// Only ever called from `about_to_wait` - see the module-level reentrancy
    /// note for why touching the backend from a window/paint event deadlocks.
    fn drain_input(&mut self) {
        self.pen_events.clear();
        self.input.poll(&mut self.pen_events);
        self.apply_pen_events();
    }

    /// Draw one frame: canvas first, then the UI on top of it.
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        // Disjoint field borrows, so the overlay closure can touch the editor and
        // the UI while the renderer is mutably borrowed.
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let ui = self.ui.as_mut();
        let editor = &mut self.editor;

        renderer.upload_canvas(editor.canvas_mut());
        let (w, h) = renderer.size_px();
        let placement = self.view.placement(w, h, editor.canvas());

        let mut ui_wants_repaint = false;
        let mut ui_inset_left = None;
        let window = renderer.window().clone();
        let result = renderer.render(placement, |gpu| {
            if let Some(ui) = ui {
                ui_wants_repaint = ui.render(&window, gpu, editor.brush_mut());
                ui_inset_left = Some(ui.inset_left_px());
            }
        });

        // Keep the canvas centered in the area the panel leaves free.
        if let Some(inset) = ui_inset_left {
            self.view.set_inset_left(inset);
        }
        if ui_wants_repaint {
            self.request_redraw();
        }

        match result {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.reconfigure();
                }
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                eprintln!("GPU out of memory - exiting");
                event_loop.exit();
            }
            Err(wgpu::SurfaceError::Timeout) => {}
        }
    }

    /// The real body of [`ApplicationHandler::window_event`], wrapped by the
    /// reentrancy guard.
    fn dispatch_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        if self.renderer.is_none() {
            return;
        }

        // Window/lifecycle events the shell handles directly.
        match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(new_size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(*new_size);
                }
                self.request_redraw();
                return;
            }
            WindowEvent::RedrawRequested => {
                // Render only. Input is drained in `about_to_wait`; draining it
                // here is what deadlocked step 5 (module-level note).
                self.redraw(event_loop);
                return;
            }
            _ => {}
        }

        // The debug panel gets first refusal on anything left. If it took the
        // event, it must not also become a brush stroke.
        //
        // Always request a frame afterwards, consumed or not: egui processes
        // queued input during its own render, so with demand-driven painting it
        // cannot react - or even decide whether to consume the *next* event -
        // unless a frame follows.
        if let (Some(ui), Some(renderer)) = (self.ui.as_mut(), self.renderer.as_ref()) {
            let consumed = ui.on_window_event(renderer.window(), &event);
            renderer.window().request_redraw();
            if consumed {
                return;
            }
        }

        // Everything else is offered to the input backend, which turns native
        // events into our PenEvents. The shell knows nothing about mice or pens.
        self.pen_events.clear();
        self.input
            .process_window_event(&event, &mut self.pen_events);
        self.apply_pen_events();
    }
}

impl ApplicationHandler for OpenPaint {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
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

        match Renderer::new(window.clone(), self.editor.canvas()) {
            Ok(renderer) => {
                println!("{} - {}", openpaint_core::hello(), openpaint_core::VERSION);
                println!("input backend: {}", self.input.name());
                self.ui = Some(ui::Ui::new(
                    renderer.device(),
                    renderer.surface_format(),
                    &window,
                ));
                // Ask for the first frame explicitly. Redraws are demand-driven
                // (strokes, resizes, and UI activity request them), so nothing
                // else would paint the initial canvas.
                renderer.window().request_redraw();
                self.renderer = Some(renderer);
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
