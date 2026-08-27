//! OpenPaint desktop application shell.
//!
//! Phase 0, step 1: prove the Linux-dev -> GitHub Actions -> Windows-download
//! -> run loop end to end. This opens a single window titled "OpenPaint".
//! GPU rendering (wgpu), the tiled canvas, and pen input come next, once the
//! build/download pipeline is confirmed working on the real tablet machine.
//!
//! NOTE: we intentionally keep the console attached on Windows for now so that
//! any startup errors are visible during early testing. Before a real release
//! we'll add `#![windows_subsystem = "windows"]` to suppress the console.

use std::error::Error;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Holds the app's live state. The window is created lazily in `resumed`,
/// which is the winit 0.30 lifecycle contract (and required for portability
/// to platforms that create windows only after the event loop is running).
#[derive(Default)]
struct OpenPaint {
    window: Option<Window>,
}

impl ApplicationHandler for OpenPaint {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let title = format!("OpenPaint  ({})", openpaint_core::VERSION);
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(1280.0, 800.0));

        match event_loop.create_window(attributes) {
            Ok(window) => {
                // Sanity signal that the core crate is linked and reachable.
                println!("{} — {}", openpaint_core::hello(), openpaint_core::VERSION);
                self.window = Some(window);
            }
            Err(err) => {
                eprintln!("failed to create window: {err}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                // Nothing to draw yet — wgpu renderer lands next step.
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    // Wait for events rather than busy-looping; correct for a paint app that
    // should stay idle until there's input or a redraw request.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = OpenPaint::default();
    event_loop.run_app(&mut app)?;
    Ok(())
}
