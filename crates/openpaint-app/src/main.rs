//! OpenPaint desktop application shell.
//!
//! Phase 0 progress:
//!   step 1 — open a window (done).
//!   step 2 — stand up wgpu, clear surface (done).
//!   step 3 — tiled canvas + mouse drawing (done).
//!   step 4 — input abstraction: all drawing flows through PenEvent/PenSample,
//!            with the mouse as the first swappable backend (this).
//! Next: octotablet backend behind the same trait for real pen pressure/tilt.
//!
//! NOTE: console stays attached on Windows for now (GPU logs + errors). We add
//! `#![windows_subsystem = "windows"]` before a real release.

mod canvas_renderer;
mod gpu;
mod input;
mod input_mouse;
#[cfg(target_os = "windows")]
mod input_pen;

use std::error::Error;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use gpu::Gpu;
use input::{InputBackend, PenEvent};
use input_mouse::MouseBackend;

/// App state. The GPU context is created lazily in `resumed`, per the winit
/// 0.30 lifecycle contract.
struct OpenPaint {
    gpu: Option<Gpu>,
    /// The active input source. Boxed so the backend is swappable at runtime;
    /// the rest of the app only sees `PenEvent`s, never the concrete backend.
    input: Box<dyn InputBackend>,
    /// Scratch buffer reused each event to avoid per-event allocation.
    pen_events: Vec<PenEvent>,
}

impl Default for OpenPaint {
    fn default() -> Self {
        Self {
            gpu: None,
            input: Box::new(MouseBackend::new()),
            pen_events: Vec::new(),
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
                self.gpu = Some(gpu);
            }
            Err(err) => {
                eprintln!("failed to initialize GPU: {err}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
                // Drain any polled backend (e.g. octotablet) just before
                // rendering, so queued pen samples are applied this frame.
                self.pen_events.clear();
                self.input.poll(&mut self.pen_events);
                for pe in self.pen_events.drain(..) {
                    Self::handle_pen_event(gpu, &pe);
                }

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

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Polled backends (e.g. octotablet) have no window events to wake them,
        // so drive a redraw each loop iteration to keep `poll` running. The
        // mouse backend returns false here and stays idle until a real event,
        // keeping the app at 0% CPU when nothing is happening.
        if self.input.wants_continuous_poll() {
            if let Some(gpu) = self.gpu.as_ref() {
                gpu.window().request_redraw();
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = OpenPaint::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
