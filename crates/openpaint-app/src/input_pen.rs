//! Pen input backend via `octotablet` (Windows Ink) — a polled [`InputBackend`].
//!
//! This is a Windows-specific *leaf*: it depends on octotablet, but the rest of
//! the app never sees octotablet types — this file translates them into our
//! [`PenEvent`]/[`PenSample`]. If octotablet is ever replaced (e.g. hand-rolled
//! Windows Ink for input prediction + coalesced samples), only this file
//! changes.
//!
//! octotablet is *polled*: [`InputBackend::poll`] drains `Manager::pump()` once
//! per loop iteration. Its events are grouped into frames — `Pose` carries the
//! current axes (position, pressure, tilt), `Down`/`Up` mark press state,
//! `Frame` ends a group. We map that onto our stroke model: a `Down` starts a
//! stroke, each `Pose` while pressed contributes a positioned+pressured sample,
//! `Up` ends it.
//!
//! # `pump()` is NOT reentrancy-safe — drain it only from the top of the loop
//!
//! octotablet's Windows Ink backend registers a RealTimeStylus *async* plugin.
//! Despite the name, RTS delivers async plugin callbacks **on the thread that
//! owns the RTS** — our UI thread — via a window it owns there. Those callbacks
//! take octotablet's internal state mutex and then call back into RTS/Ink over
//! COM. Those are out-of-process calls (`tpcps.dll` proxy → tablet service), and
//! a COM call from an STA pumps the message queue while it waits.
//!
//! Net effect: while octotablet holds its mutex, Windows can dispatch a pending
//! `WM_PAINT` into winit and re-enter our handlers. If we call `pump()` there,
//! it blocks forever on a mutex the same thread already holds — a hard deadlock
//! at 0% CPU on the first frame. This is what broke step 5's first build; the
//! fix lives in `main.rs` (drain from `about_to_wait` only, plus a reentrancy
//! guard). Do not move the `poll` call into the paint path.

use std::sync::Arc;

use octotablet::axis::AvailableAxes;
use octotablet::events::{Event, ToolEvent};
use octotablet::tool::Type as ToolType;
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
    /// How many pose samples we've logged since the last stroke started. We log
    /// the first handful so the Windows terminal shows real pen values
    /// (pressure/pos/tilt), then go quiet to avoid flooding.
    ///
    /// Reset on every `Down` deliberately: the Tablet PC stack reports the
    /// *mouse* as a tool too, so a run-global budget gets spent on mouse hover
    /// before the pen is ever touched — which hides the one number that matters
    /// (whether pressure actually varies). Per-stroke, every stroke is legible.
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
        PenSample::new(
            self.last_pos.0,
            self.last_pos.1,
            self.last_pressure,
            self.last_tilt,
        )
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
    /// A tool was enumerated or entered proximity — log what it can actually do.
    ///
    /// This is the diagnostic that tells apart the two reasons pressure can read
    /// as a constant 1.0, which are indistinguishable downstream because both
    /// arrive as a missing pressure axis:
    ///   - a real pen whose driver doesn't declare `NORMAL_PRESSURE` to Windows
    ///     Ink (or declares it with a degenerate range), and
    ///   - octotablet's *emulated* mouse tool, which it creates by default
    ///     (`Builder::emulate_tool_from_mouse` is `true`) and which has no
    ///     pressure axis at all.
    Describe {
        what: &'static str,
        tool_type: Option<ToolType>,
        axes: AvailableAxes,
        name: Option<String>,
    },
}

impl InputBackend for PenBackend {
    fn poll(&mut self, out: &mut Vec<PenEvent>) {
        // Pass 1: drain octotablet into owned actions. `events` borrows
        // `self.manager` for this whole loop, so we must NOT read `self` here.
        let mut actions: Vec<Action> = Vec::new();
        match self.manager.pump() {
            Ok(events) => {
                for event in events.into_iter() {
                    let Event::Tool {
                        tool,
                        event: tool_event,
                    } = event
                    else {
                        continue;
                    };
                    let describe = |what| Action::Describe {
                        what,
                        tool_type: tool.tool_type,
                        axes: tool.axes.available(),
                        name: tool.name.clone(),
                    };
                    match tool_event {
                        ToolEvent::Pose(pose) => actions.push(Action::Pose {
                            // Position is in *physical* pixels from the window's
                            // top-left, which is what `Gpu::to_canvas` wants (it
                            // divides by the wgpu surface size, also physical) and
                            // what winit's `CursorMoved` gives the mouse backend.
                            // Confirmed on a 150%-scaled display: values exceed
                            // the logical client height but fit the physical one.
                            pos: (pose.position[0] as f64, pose.position[1] as f64),
                            // Pressure is optional; treat a missing pressure axis
                            // as full so a pressureless tool still draws.
                            pressure: pose.pressure.get().unwrap_or(1.0),
                            tilt: pose.tilt.map(|t| (t[0], t[1])).unwrap_or((0.0, 0.0)),
                        }),
                        // Capability reports: `Added` is enumeration, `In` is the
                        // tool that's about to actually draw. Both are rare, so
                        // logging them costs nothing on the hot path.
                        ToolEvent::Added => actions.push(describe("added")),
                        ToolEvent::In { .. } => actions.push(describe("in range")),
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
                    } else {
                        out.push(PenEvent::Hover(self.sample()));
                    }
                }
                Action::Down => {
                    self.down = true;
                    // Fresh logging budget per stroke — see `logged_poses`.
                    self.logged_poses = 0;
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
                Action::Describe {
                    what,
                    tool_type,
                    axes,
                    name,
                } => {
                    let has_pressure = axes.contains(AvailableAxes::PRESSURE);
                    println!(
                        "pen: tool {what} — type={tool_type:?} name={name:?} axes={axes:?}\n      \
                         pressure axis: {}",
                        if has_pressure {
                            "YES"
                        } else {
                            "NO (pressure will read a constant 1.0)"
                        }
                    );
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
