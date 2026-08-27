//! Pen input backend via `octotablet` (Windows Ink) — a polled [`InputBackend`].
//!
//! This is a Windows-specific *leaf*: it depends on octotablet, but the rest of
//! the app never sees octotablet types — this file translates them into our
//! [`PenEvent`]/[`PenSample`]. If octotablet is ever replaced (e.g. hand-rolled
//! Windows Ink for input prediction + coalesced samples), only this file
//! changes.
//!
//! octotablet is *polled*: [`InputBackend::poll`] drains `Manager::pump()` once
//! per frame. Its events are grouped into frames — `Pose` carries the current
//! axes (position, pressure, tilt), `Down`/`Up` mark press state, `Frame` ends
//! a group. We map that onto our stroke model: a `Down` starts a stroke, each
//! `Pose` while pressed contributes a positioned+pressured sample, `Up` ends it.

use std::sync::Arc;

use octotablet::events::{Event, ToolEvent};
use octotablet::Manager;
use winit::window::Window;

use crate::input::{InputBackend, PenEvent, PenSample};

pub struct PenBackend {
    manager: Manager,
    /// Whether the tool is currently pressed (mid-stroke).
    down: bool,
    /// Most recent pose seen this frame, applied when a sample is emitted.
    last_pos: (f64, f64),
    last_pressure: f32,
    last_tilt: (f32, f32),
    /// How many pose samples we've logged. We log the first handful of each run
    /// so the Windows terminal shows real pen values (pressure/pos/tilt) for
    /// remote debugging, then goes quiet to avoid flooding.
    logged_poses: u32,
}

impl PenBackend {
    /// Try to construct the pen backend for a window. Returns `None` (with a
    /// logged reason) if the tablet system can't be reached, so the caller can
    /// fall back to the mouse backend instead of failing to start.
    pub fn try_new(window: Arc<Window>) -> Option<Self> {
        match octotablet::Builder::new().build_shared(&window) {
            Ok(manager) => {
                println!(
                    "pen: octotablet connected (backend: {:?})",
                    manager.backed()
                );
                Some(Self {
                    manager,
                    down: false,
                    last_pos: (0.0, 0.0),
                    last_pressure: 0.0,
                    last_tilt: (0.0, 0.0),
                    logged_poses: 0,
                })
            }
            Err(e) => {
                eprintln!("pen: octotablet unavailable ({e:?}); falling back to mouse");
                None
            }
        }
    }

    /// Build a `PenSample` from the current tracked pose.
    fn sample(&self) -> PenSample {
        PenSample {
            x: self.last_pos.0,
            y: self.last_pos.1,
            pressure: self.last_pressure,
            tilt: self.last_tilt,
            time_ms: 0.0,
        }
    }
}

/// The tool actions we care about, extracted from octotablet events as owned
/// data. Buffering these lets us drop the `manager.pump()` borrow before we
/// touch `self`, sidestepping a borrow conflict while preserving event order.
enum Action {
    Pose {
        pos: (f64, f64),
        pressure: f32,
        tilt: (f32, f32),
    },
    Down,
    Up,
}

impl InputBackend for PenBackend {
    fn poll(&mut self, out: &mut Vec<PenEvent>) {
        // Pass 1: drain octotablet into owned actions. `events` borrows
        // `self.manager` for this whole loop, so we must NOT read `self` here.
        let mut actions: Vec<Action> = Vec::new();
        match self.manager.pump() {
            Ok(events) => {
                for event in events.into_iter() {
                    let Event::Tool { event: tool, .. } = event else {
                        continue;
                    };
                    match tool {
                        ToolEvent::Pose(pose) => actions.push(Action::Pose {
                            // Position is in logical pixels from the window's
                            // top-left, matching our screen->canvas mapping.
                            pos: (pose.position[0] as f64, pose.position[1] as f64),
                            // Pressure is optional; treat a missing pressure axis
                            // as full so a pressureless tool still draws.
                            pressure: pose.pressure.get().unwrap_or(1.0),
                            tilt: pose.tilt.map(|t| (t[0], t[1])).unwrap_or((0.0, 0.0)),
                        }),
                        ToolEvent::Down => actions.push(Action::Down),
                        // Pen lifted, or left the sensing area mid-stroke: both
                        // end the stroke so we don't leave a dangling one.
                        ToolEvent::Up | ToolEvent::Out => actions.push(Action::Up),
                        _ => {}
                    }
                }
            }
            Err(e) => {
                eprintln!("pen: pump error: {e:?}");
                return;
            }
        }

        // Pass 2: `events` is dropped, so we can freely mutate/read `self`.
        for action in actions {
            match action {
                Action::Pose {
                    pos,
                    pressure,
                    tilt,
                } => {
                    self.last_pos = pos;
                    self.last_pressure = pressure;
                    self.last_tilt = tilt;
                    // Log the first several poses so the terminal shows real
                    // values for remote debugging, then stay quiet.
                    if self.logged_poses < 20 {
                        self.logged_poses += 1;
                        println!(
                            "pen pose: pos=({:.1},{:.1}) pressure={:.3} tilt=({:.3},{:.3}) down={}",
                            pos.0, pos.1, pressure, tilt.0, tilt.1, self.down
                        );
                    }
                    if self.down {
                        out.push(PenEvent::Move(vec![self.sample()]));
                    }
                }
                Action::Down => {
                    self.down = true;
                    println!(
                        "pen: DOWN at ({:.1},{:.1})",
                        self.last_pos.0, self.last_pos.1
                    );
                    out.push(PenEvent::Down(self.sample()));
                }
                Action::Up => {
                    if self.down {
                        self.down = false;
                        println!("pen: UP");
                        out.push(PenEvent::Up);
                    }
                }
            }
        }
    }

    fn wants_continuous_poll(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "octotablet (Windows Ink)"
    }
}
