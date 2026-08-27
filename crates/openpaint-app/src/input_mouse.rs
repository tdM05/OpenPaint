//! Mouse input backend — the first (and Linux-testable) [`InputBackend`].
//!
//! Translates winit mouse events into [`PenEvent`]s with constant full
//! pressure. Its only job is to prove the input seam works end to end before a
//! real pen backend (octotablet) is introduced behind the same trait.

use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::input::{InputBackend, PenEvent, PenSample};

#[derive(Default)]
pub struct MouseBackend {
    /// Latest cursor position in window pixels.
    cursor: (f64, f64),
    /// Whether the left button is currently held (i.e. mid-stroke).
    down: bool,
}

impl MouseBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl InputBackend for MouseBackend {
    fn process_window_event(&mut self, event: &WindowEvent, out: &mut Vec<PenEvent>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if self.down {
                    // Mouse gives one sample per event; emit a single-sample
                    // batch. A pen backend would emit many coalesced samples.
                    out.push(PenEvent::Move(vec![PenSample::at(
                        self.cursor.0,
                        self.cursor.1,
                        1.0,
                    )]));
                }
            }
            WindowEvent::MouseInput { state, button, .. } if *button == MouseButton::Left => {
                match state {
                    ElementState::Pressed => {
                        self.down = true;
                        out.push(PenEvent::Down(PenSample::at(
                            self.cursor.0,
                            self.cursor.1,
                            1.0,
                        )));
                    }
                    ElementState::Released => {
                        self.down = false;
                        out.push(PenEvent::Up);
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "mouse"
    }
}
