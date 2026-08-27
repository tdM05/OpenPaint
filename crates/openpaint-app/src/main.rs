//! OpenPaint desktop application shell.
//!
//! Phase 0 progress:
//!   step 1 — open a window (done).
//!   step 2 — stand up wgpu, clear surface (done).
//!   step 3 — tiled canvas + mouse drawing (this).
//! Next: Windows Ink pressure/tilt so strokes respond to the pen.
//!
//! NOTE: console stays attached on Windows for now (GPU logs + errors). We add
//! `#![windows_subsystem = "windows"]` before a real release.

mod canvas_renderer;
mod gpu;

use std::error::Error;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use gpu::Gpu;

/// App state. The GPU context is created lazily in `resumed`, per the winit
/// 0.30 lifecycle contract.
#[derive(Default)]
struct OpenPaint {
    gpu: Option<Gpu>,
    /// Latest known cursor position in window pixels.
    cursor: (f64, f64),
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

        match Gpu::new(window) {
            Ok(gpu) => {
                println!("{} — {}", openpaint_core::hello(), openpaint_core::VERSION);
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

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                gpu.resize(new_size);
                gpu.window().request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // Mouse has no pressure; feed full pressure for now.
                gpu.stroke_to(self.cursor.0, self.cursor.1, 1.0);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    match state {
                        ElementState::Pressed => {
                            gpu.stroke_begin(self.cursor.0, self.cursor.1, 1.0);
                        }
                        ElementState::Released => {
                            gpu.stroke_end();
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => match gpu.render() {
                Ok(()) => {}
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    gpu.reconfigure();
                }
                Err(wgpu::SurfaceError::OutOfMemory) => {
                    eprintln!("GPU out of memory — exiting");
                    event_loop.exit();
                }
                Err(wgpu::SurfaceError::Timeout) => {}
            },
            _ => {}
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
