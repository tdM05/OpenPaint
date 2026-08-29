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

mod autosave;
mod canvas_renderer;
mod crop;
mod editor;
mod export;
mod history;
mod input;
mod input_mouse;
#[cfg(target_os = "windows")]
mod input_pen;
mod perf;
mod renderer;
mod stroke_exec;
mod stroke_layer;
#[cfg(test)]
mod test_gpu;
mod tile_pool;
mod tile_store;
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

use crop::Crop;
use editor::Editor;
use input::{InputBackend, PenEvent, PenSample};
use input_mouse::MouseBackend;
use openpaint_core::{PageRect, Side};
use renderer::Renderer;
use view::{View, ROTATE_STEP};

/// How close, in screen pixels, a press must be to grab a crop handle.
///
/// Screen-relative on purpose: converted to page units by dividing by the zoom, so a
/// handle is equally easy to hit whether you are at 10% or 800%.
const CROP_GRAB_PX: f32 = 10.0;

/// Extension for the native document format.
const DOCUMENT_EXTENSION: &str = "openpaint";

/// Multiplier per brush-size keypress.
///
/// Multiplicative rather than a fixed increment, because size is perceived logarithmically: one
/// press at radius 4 should feel like one press at radius 40.
const BRUSH_STEP: f32 = 1.15;

/// Brush radius limits. The lower bound keeps a single-pixel brush reachable; the upper is past
/// any useful blocking-in brush on the largest page.
const MIN_BRUSH_RADIUS: f32 = 0.5;
const MAX_BRUSH_RADIUS: f32 = 512.0;

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
    /// Canvas navigation (pan/zoom) state. Kept in the shell rather than the
    /// view because it is *interaction* state, not camera state.
    nav: Nav,
    /// The crop tool, present only while it is active. Its presence *is* the tool
    /// state, so there is no separate mode enum to keep in sync.
    crop: Option<Crop>,
    /// The selection tool, present only while it is active — same pattern as `crop`, and
    /// deliberately not a variant of [`editor::Tool`]: that enum indexes an array of brushes, and a
    /// selection tool has no brush to index.
    select: Option<Select>,
    /// The document's selection, and the outline it draws.
    ///
    /// The outline is cached beside it because it is derived from the whole mask, which is far too
    /// much work to redo every frame and changes only when the selection does.
    selection: Option<ActiveSelection>,
    /// Most recent notable outcome (export, resize refusal, history loss), shown in
    /// the panel so it isn't only visible in a console the user may not be watching.
    status_message: Option<String>,
    /// Set while we're inside one of our own event handlers, so a nested
    /// message pump can't re-enter our GPU/input work. See the module-level
    /// "Windows Ink reentrancy" note - without this, a re-entered frame can
    /// deadlock on a lock its own interrupted outer call is holding.
    in_dispatch: bool,
    /// Where the open document lives, or `None` if it has never been saved.
    document_path: Option<std::path::PathBuf>,
    /// Whether there are edits the file does not have. Drives the title marker and the guard on
    /// anything that would throw them away.
    dirty: bool,
    /// A dialog waiting to be shown.
    ///
    /// **Not** shown from the event handler that asked for it. A modal dialog pumps the Windows
    /// message queue, which dispatches our pending events straight back into us -- the exact
    /// hazard described at the top of this file. So a request is parked here and serviced from
    /// [`ApplicationHandler::about_to_wait`], the one place winit guarantees no foreign frame is
    /// on the stack. Same rule as draining pen input, for the same reason.
    pending_dialog: Option<Dialog>,
    /// The unsaved-changes prompt, drawn in-app rather than as a native dialog.
    ///
    /// Native only where it is unavoidable -- a file picker cannot be reimplemented, a
    /// three-button question can. Every native modal runs its own message loop, which is this
    /// project's most expensive recurring bug (Q10c), so the fewer of them the better. Drawing it
    /// ourselves also means it cannot end up behind the window, which is precisely how the first
    /// version broke.
    pending_confirm: Option<Confirm>,
    /// An action to run once a save the user asked for has succeeded.
    after_save: Option<Dialog>,
    /// A native file dialog running on its own thread.
    file_dialog: Option<FileDialogTask>,
    /// Arrival time of the newest pen sample the next presented frame will show.
    ///
    /// Taken by the frame that presents it, so a frame drawn for some other reason cannot claim
    /// an input latency it had no input for.
    pending_sample_ms: Option<f64>,
    /// Rolling latency and frame-time measurements.
    perf: perf::Perf,
    /// Periodic recovery copies, and the abandoned one found at startup.
    autosave: autosave::Autosave,
    /// An abandoned recovery copy waiting for the user to accept or discard it.
    recovery: Option<autosave::Recoverable>,
    /// Path smoothing, applied between the pen and the brush.
    ///
    /// Lives here rather than in [`Editor`] because it conditions *input*, and input is the
    /// shell's job — the same reason `to_canvas` is here. It is stateful across a stroke, so it
    /// cannot be a free function.
    stabilizer: openpaint_core::Stabilizer,
}

/// Which file dialog is in flight, and where its answer will arrive.
struct FileDialogTask {
    kind: FileDialogKind,
    answer: std::sync::mpsc::Receiver<Option<std::path::PathBuf>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileDialogKind {
    Save,
    Open,
}

/// A pending "you have unsaved changes" question.
#[derive(Clone, Copy, Debug)]
struct Confirm {
    /// What to do if the user goes ahead.
    then: Dialog,
    /// What they are about to do, for the prompt.
    what: &'static str,
}

/// Something that needs a native dialog, deferred to a safe point.
///
/// `confirmed` distinguishes "check for unsaved edits first" from "the user has already answered
/// that question". The unsaved-changes prompt is drawn in-app, so answering it parks a second
/// request rather than continuing inline -- a native file picker still must not be opened from a
/// frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialog {
    /// Save, asking for a path only if there is not one yet.
    Save,
    /// Always ask for a path.
    SaveAs,
    Open {
        confirmed: bool,
    },
    New {
        confirmed: bool,
    },
    Quit {
        confirmed: bool,
    },
}

/// The selection, and the outline it draws.
///
/// Kept together because the outline is *derived* from the mask: separate fields could drift, and a
/// selection outline that disagrees with what a fill will do is worse than none.
struct ActiveSelection {
    mask: openpaint_core::Selection,
    outline: Vec<openpaint_core::selection::Segment>,
}

/// A selection gesture in progress.
///
/// Holds the shape being drawn, not the resulting mask: the mask is built once on release, because
/// rasterizing a growing polygon on every pen sample would cost the whole page per sample for a
/// preview the outline of the gesture already gives.
#[derive(Clone, Debug)]
enum Select {
    /// Freehand. Points in page space, in the order the pen visited them.
    Lasso { points: Vec<(f32, f32)> },
    /// Drag a rectangle. `from` is the anchor, `to` follows the pen.
    Rect {
        from: Option<(f32, f32)>,
        to: (f32, f32),
    },
}

impl Select {
    /// What the gesture would select, or `None` if it is not a shape yet.
    fn resolve(&self, page: PageRect) -> Option<openpaint_core::Selection> {
        match self {
            Self::Lasso { points } => {
                let sel = openpaint_core::Selection::from_polygon(points, page);
                (!sel.is_empty()).then_some(sel)
            }
            Self::Rect { from, to } => {
                let from = (*from)?;
                let rect = rect_between(from, *to);
                let sel = openpaint_core::Selection::from_rect(rect, page);
                (!sel.is_empty()).then_some(sel)
            }
        }
    }

    /// The gesture's own outline, drawn live while dragging.
    fn preview(&self) -> Vec<((f32, f32), (f32, f32))> {
        match self {
            Self::Lasso { points } => points
                .windows(2)
                .map(|w| (w[0], w[1]))
                .chain(
                    // The closing edge, shown from the start: a lasso is implicitly closed, and
                    // seeing where it will close is most of what makes one aimable.
                    (points.len() > 2).then(|| (*points.last().expect("non-empty"), points[0])),
                )
                .collect(),
            Self::Rect { from, to } => from.map_or_else(Vec::new, |from| {
                let (a, b) = (from, *to);
                vec![
                    ((a.0, a.1), (b.0, a.1)),
                    ((b.0, a.1), (b.0, b.1)),
                    ((b.0, b.1), (a.0, b.1)),
                    ((a.0, b.1), (a.0, a.1)),
                ]
            }),
        }
    }
}

/// The page rectangle spanned by two corners, in either order.
fn rect_between(a: (f32, f32), b: (f32, f32)) -> PageRect {
    let x0 = a.0.min(b.0).floor() as i32;
    let y0 = a.1.min(b.1).floor() as i32;
    let x1 = a.0.max(b.0).ceil() as i32;
    let y1 = a.1.max(b.1).ceil() as i32;
    PageRect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
}

/// In-progress canvas navigation.
///
/// Bindings are hardcoded here for now. DECISIONS section 6 wants input mapping to
/// be data (a remappable command table), but with only a handful of navigation
/// actions that would be speculative structure before the command set is known.
/// Tracked as OPEN_QUESTIONS Q16.
#[derive(Default)]
struct Nav {
    /// Cursor position in physical pixels, needed to anchor zoom and rotation.
    ///
    /// `None` until the pointer has actually been somewhere. Defaulting to (0, 0)
    /// would silently anchor the first zoom at the window's top-left corner --
    /// which is reachable in practice (wheel over the window without moving first,
    /// e.g. a trackpad gesture) and looks like the canvas leaping away.
    cursor: Option<(f64, f64)>,
    /// True while space is held, which arms pan-on-drag (Photoshop/CSP habit).
    space_held: bool,
    /// Latest modifier state, for shortcuts like Ctrl+Z.
    modifiers: winit::keyboard::ModifiersState,
    /// Where a pan drag last was, if one is in progress.
    panning_from: Option<(f64, f64)>,
}

impl Nav {
    /// Whether navigation is currently swallowing input, in which case a stroke
    /// must not also start.
    ///
    /// This matters more than it looks: mouse drags reach us as *pen* events via
    /// octotablet's emulated mouse tool, so without this a space-drag would pan
    /// and paint simultaneously.
    fn is_active(&self) -> bool {
        self.space_held || self.panning_from.is_some()
    }

    /// Where to anchor zoom and rotation: the pointer if we've seen it, otherwise
    /// the centre of the surface.
    fn anchor(&self, surface_w: u32, surface_h: u32) -> (f64, f64) {
        self.cursor
            .unwrap_or((f64::from(surface_w) / 2.0, f64::from(surface_h) / 2.0))
    }
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
            nav: Nav::default(),
            crop: None,
            select: None,
            selection: None,
            status_message: None,
            in_dispatch: false,
            document_path: None,
            dirty: false,
            pending_dialog: None,
            pending_confirm: None,
            after_save: None,
            file_dialog: None,
            pending_sample_ms: None,
            perf: perf::Perf::default(),
            autosave: autosave::Autosave::new(),
            recovery: None,
            stabilizer: openpaint_core::Stabilizer::default(),
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
                // Before the guards below: a press that lands on the UI is still the pointer
                // telling us where it is, and the zoom anchor and the ring both want that.
                self.note_pointer(sample.x, sample.y);
                // Don't paint underneath the UI. This check is needed for the pen
                // specifically, because pen input never reaches egui and so egui's
                // own pointer capture cannot exclude it (OPEN_QUESTIONS Q14).
                if self.ui_blocks_point(sample.x, sample.y)
                    || self.nav.is_active()
                    || self.pending_confirm.is_some()
                    || self.recovery.is_some()
                {
                    return;
                }
                // Alt turns any press into a colour sample, which is how every art app does it and
                // is far more used than a tool-palette entry would be. It gets a place in the tool
                // palette too once there is one — the modifier is the workflow, the palette entry
                // is discoverability, and only one of them is reachable today.
                if self.nav.modifiers.alt_key() {
                    self.sample_color(sample);
                    return;
                }
                // The crop tool consumes input entirely: no painting while it is up.
                if self.crop.is_some() {
                    self.crop_press(sample);
                    return;
                }
                if self.select.is_some() {
                    self.select_press(sample);
                    return;
                }
                if let Some((cx, cy)) = self.to_canvas(sample) {
                    self.stroke_start(cx, cy, sample.pressure, sample.time_ms());
                    self.note_latency_input(sample);
                    self.request_redraw();
                }
            }
            PenEvent::Hover(sample) => {
                // The brush ring follows the pointer, so a hover is a real visible change and
                // does need a frame -- but only when the pointer actually moved. Pens report
                // poses continuously, including while resting perfectly still, so repainting on
                // every one would turn a pen left on the tablet into a permanent full-rate
                // repaint and defeat the whole demand-driven design.
                if self.note_pointer(sample.x, sample.y) {
                    self.request_redraw();
                }
            }
            PenEvent::Move(samples) => {
                if let Some(sample) = samples.last() {
                    self.note_pointer(sample.x, sample.y);
                }
                if self.crop.is_some() {
                    if let Some(sample) = samples.last() {
                        self.crop_drag(sample);
                    }
                    return;
                }
                if self.select.is_some() {
                    // Every sample, not just the last: a lasso *is* the path the pen took, so
                    // dropping the intermediate ones would cut corners off the shape.
                    for sample in samples {
                        self.select_drag(sample);
                    }
                    return;
                }
                if !self.editor.is_drawing() || self.nav.is_active() {
                    return;
                }
                for sample in samples {
                    if let Some((cx, cy)) = self.to_canvas(sample) {
                        self.stroke_continue(cx, cy, sample.pressure, sample.time_ms());
                    }
                }
                // The newest sample in the batch is the one at the tip of the stroke, which is
                // the pixel the artist is actually watching, so it is the one whose latency
                // matters. Older samples in the same batch are already historical.
                if let Some(sample) = samples.last() {
                    self.note_latency_input(sample);
                }
                self.request_redraw();
            }
            PenEvent::Up => {
                if let Some(crop) = self.crop.as_mut() {
                    crop.release();
                    return;
                }
                if self.select.is_some() {
                    self.select_release();
                    return;
                }
                // Queues the bake that commits the stroke; demand-driven painting
                // means it needs a frame requested or it simply never happens.
                self.stroke_finish();
                self.request_redraw();
            }
        }
    }

    /// Map a pen sample from window pixels to canvas pixels.
    fn to_canvas(&self, sample: &PenSample) -> Option<(f32, f32)> {
        let renderer = self.renderer.as_ref()?;
        let (w, h) = renderer.size_px();
        self.view
            .screen_to_canvas(sample.x, sample.y, w, h, self.editor.page_rect())
    }

    fn ui_blocks_point(&self, x: f64, y: f64) -> bool {
        self.ui.as_ref().is_some_and(|ui| ui.blocks_point(x, y))
    }

    /// Remember where the pointer is. Returns whether it moved far enough to be worth a frame.
    ///
    /// Half a physical pixel, because that is the threshold below which redrawing cannot change
    /// what is on screen. Nothing about it is tuned to feel right — it is the point at which the
    /// work is provably wasted.
    ///
    /// **The only place `nav.cursor` is written.** It has two readers that must agree — the zoom
    /// anchor and the brush ring — and it is reached from two directions, the pen seam's `Hover`
    /// and winit's `CursorMoved` for mouse drags. When the navigation handler wrote it directly as
    /// well, it always got there first for the mouse, so the threshold below saw an unchanged
    /// position every time and the ring never moved at all.
    fn note_pointer(&mut self, x: f64, y: f64) -> bool {
        let next = (x, y);
        let still = self
            .nav
            .cursor
            .is_some_and(|(x, y)| (x - next.0).abs() < 0.5 && (y - next.1).abs() < 0.5);
        if still {
            // Below the threshold, and deliberately *without* storing the position. The
            // comparison is against the last position we accepted, not the last we saw: storing
            // every sample would let a slow drift of sub-threshold steps carry the pointer
            // arbitrarily far while never once crossing the threshold, so the ring would sit
            // somewhere the pen no longer is.
            return false;
        }
        self.nav.cursor = Some(next);
        true
    }

    /// Begin a stroke from an input sample in canvas coordinates, through smoothing.
    ///
    /// These three methods exist as their own seam rather than inline in the event handlers so
    /// they can be tested without a window: the editor is headless, so a test can drive them and
    /// read the dabs back. Every bug this session lived in a layer that had no test.
    fn stroke_start(&mut self, cx: f32, cy: f32, pressure: f32, t_ms: f64) {
        // Say why nothing is going to happen. `Editor::stroke_begin` refuses this case on its own,
        // so the stroke is safe either way -- but a tool that silently does nothing reads as a bug,
        // and the artist has no way to guess which of two settings is in their way.
        if self.editor.paint_mode().is_none() {
            self.status_message = Some(
                "This layer's alpha is locked, so the eraser cannot remove anything.                  Unlock it, or paint instead."
                    .to_owned(),
            );
            self.request_redraw();
            return;
        }
        // Strength is read once, at the press. Dragging the slider mid-stroke must not change the
        // filter under a line already being drawn.
        self.stabilizer
            .set_lag_ms(self.editor.brush().stabilization_ms);
        let s = self.stabilizer.begin(cx, cy, pressure, t_ms);
        self.editor.stroke_begin(s.x, s.y, s.pressure);
    }

    /// Extend the stroke with another input sample.
    fn stroke_continue(&mut self, cx: f32, cy: f32, pressure: f32, t_ms: f64) {
        let s = self.stabilizer.push(cx, cy, pressure, t_ms);
        self.editor.stroke_to(s.x, s.y, s.pressure);
    }

    /// Let the smoothed line catch up to the pen with time alone. Returns whether it is still
    /// converging, and so whether the loop needs to keep waking.
    ///
    /// The bug this fixes: draw fast with heavy stabilization and stop, and the line stopped dead
    /// well short of the cursor, then jumped to it in a straight segment on the next twitch. The
    /// filter was only ever advanced by arriving samples, so with no samples it stopped converging,
    /// and the next one arrived carrying every millisecond of the pause.
    fn tick_stabilizer(&mut self) -> bool {
        // Unsmoothed strokes need no ticking at all: the line is never behind the pen, so an idle
        // backend can stay idle exactly as it did before this existed.
        if !self.editor.is_drawing() || !self.stabilizer.is_active() {
            return false;
        }
        if let Some(s) = self.stabilizer.advance(input::now_ms()) {
            self.editor.stroke_to(s.x, s.y, s.pressure);
            self.request_redraw();
        }
        // Keep ticking for the whole stroke, not only while `advance` reports movement. Once it has
        // converged there is nothing to draw, but the clock still has to be kept current: a gap
        // with no ticks is banked and spent on the next sample, which is the snap all over again.
        true
    }

    /// End the stroke, first carrying the line to where the pen actually lifted.
    ///
    /// Smoothing trails by design, so without the tail every stabilized stroke would stop short of
    /// its own endpoint — ruining tapers and leaving strokes that should meet apart. The tail is
    /// empty when smoothing is off.
    fn stroke_finish(&mut self) {
        for s in self.stabilizer.finish() {
            self.editor.stroke_to(s.x, s.y, s.pressure);
        }
        self.editor.stroke_end();
    }

    /// Pick up the colour under the pointer as the brush colour.
    ///
    /// Sets the *brush's* colour even when the eraser is active: the eraser has no colour, so
    /// sampling while erasing would otherwise be silently ignored — and someone reaching for the
    /// eyedropper is choosing what to paint with next.
    fn sample_color(&mut self, sample: &PenSample) {
        let Some((cx, cy)) = self.to_canvas(sample) else {
            return;
        };
        let layers = self.editor.layers().to_vec();
        #[allow(clippy::cast_possible_truncation)]
        let picked = match self.renderer.as_mut() {
            Some(r) => r.sample_page_pixel(cx.floor() as i32, cy.floor() as i32, &layers),
            None => return,
        };
        let srgb = openpaint_core::color::opaque_linear_premul_to_srgb8(picked);
        self.editor.paint_brush_mut().set_color_srgb8(srgb);
        self.status_message = Some(format!(
            "Picked #{:02X}{:02X}{:02X}",
            srgb[0], srgb[1], srgb[2]
        ));
        self.request_redraw();
    }

    /// Mark this sample as the one whose latency the next presented frame will measure.
    fn note_latency_input(&mut self, sample: &PenSample) {
        self.pending_sample_ms = Some(sample.time_ms());
    }

    /// The brush ring to draw at the pointer, if there should be one.
    ///
    /// Drawn by us rather than handed to the OS as a cursor bitmap, which is the obvious cheaper
    /// route and does not work: Windows caps cursor size well below the radii a paint brush
    /// reaches, so a large brush would silently stop matching its own cursor.
    ///
    /// The honest cost is that hovering now repaints at display rate, because there is no cached
    /// composite to draw an overlay over — DECISIONS §4e deferred that cache deliberately. Adding
    /// it is the fix if the frame-time readout says it matters, and now there is a readout to ask.
    fn brush_cursor(&self) -> Option<ui::BrushCursor> {
        // No ring while cropping: the pointer is dragging a rectangle, not painting, and a brush
        // circle would claim otherwise.
        if self.crop.is_some() {
            return None;
        }
        let (x, y) = self.nav.cursor?;
        if self.ui_blocks_point(x, y) {
            return None;
        }
        Some(ui::BrushCursor {
            centre: [x as f32, y as f32],
            // Brush radius is in page pixels and the ring is in screen pixels, so it tracks zoom.
            // That is the entire point: the ring has to say how big the mark will be *here*.
            radius: self.editor.brush().radius * self.view.scale(),
        })
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

    /// Page coordinates for a sample, **unclipped**.
    ///
    /// The crop tool needs points outside the page, since dragging an edge outward is how
    /// you extend rather than crop.
    fn to_page_unclipped(&self, sample: &PenSample) -> Option<(f32, f32)> {
        let renderer = self.renderer.as_ref()?;
        let (w, h) = renderer.size_px();
        Some(
            self.view
                .screen_to_canvas_unclipped((sample.x, sample.y), w, h),
        )
    }

    /// How far, in page units, a press may be from a handle to grab it.
    fn crop_tolerance(&self) -> f32 {
        CROP_GRAB_PX / self.view.scale().max(f32::MIN_POSITIVE)
    }

    fn crop_press(&mut self, sample: &PenSample) {
        let Some(p) = self.to_page_unclipped(sample) else {
            return;
        };
        let tolerance = self.crop_tolerance();
        if let Some(crop) = self.crop.as_mut() {
            if crop.press(p, tolerance) {
                self.request_redraw();
            }
        }
    }

    fn crop_drag(&mut self, sample: &PenSample) {
        let Some(p) = self.to_page_unclipped(sample) else {
            return;
        };
        let dragging = self.crop.as_ref().is_some_and(Crop::is_dragging);
        if !dragging {
            return;
        }
        if let Some(crop) = self.crop.as_mut() {
            crop.drag_to(p);
        }
        self.request_redraw();
    }

    /// Start, apply, or cancel the crop tool.
    ///
    /// Applying goes through the same `apply_page_rect` as everything else, so a crop is
    /// undoable and memory-guarded on exactly the same terms as an extend.
    fn crop_action(&mut self, action: ui::CropAction) {
        match action {
            ui::CropAction::Start => {
                // Any stroke still in flight would otherwise bake after the geometry
                // moved underneath it.
                self.editor.stroke_end();
                let page = self.editor.document().active().rect();
                self.crop = Some(Crop::new(page));
            }
            ui::CropAction::Cancel => {
                self.crop = None;
                self.status_message = Some("Crop cancelled".to_owned());
            }
            ui::CropAction::Apply => {
                if let Some(crop) = self.crop.take() {
                    let target = crop.rect();
                    if target == self.editor.document().active().rect() {
                        self.status_message = Some("Crop unchanged".to_owned());
                    } else {
                        self.apply_page_rect(target);
                    }
                }
            }
        }
        self.request_redraw();
    }

    /// The crop rectangle in screen space, for the panel to paint.
    /// Begin a selection gesture.
    fn select_press(&mut self, sample: &PenSample) {
        let Some(p) = self.to_page_unclipped(sample) else {
            return;
        };
        match self.select.as_mut() {
            Some(Select::Lasso { points }) => {
                points.clear();
                points.push(p);
            }
            Some(Select::Rect { from, to }) => {
                *from = Some(p);
                *to = p;
            }
            None => return,
        }
        self.request_redraw();
    }

    /// Extend it.
    fn select_drag(&mut self, sample: &PenSample) {
        let Some(p) = self.to_page_unclipped(sample) else {
            return;
        };
        match self.select.as_mut() {
            Some(Select::Lasso { points }) => {
                // Only when the pen has actually moved a pixel. A resting pen reports continuously
                // (see `note_pointer`), and a polygon with thousands of coincident vertices is
                // slower to rasterize and no more accurate.
                if points
                    .last()
                    .is_none_or(|last| (last.0 - p.0).abs() >= 1.0 || (last.1 - p.1).abs() >= 1.0)
                {
                    points.push(p);
                }
            }
            Some(Select::Rect { from, to }) => {
                if from.is_none() {
                    return;
                }
                *to = p;
            }
            None => return,
        }
        self.request_redraw();
    }

    /// Finish it, and make the mask.
    fn select_release(&mut self) {
        let page = self.editor.page_rect();
        let resolved = self.select.as_ref().and_then(|s| s.resolve(page));

        // Reset the gesture either way, so the next press starts a fresh shape rather than
        // extending the last one.
        match self.select.as_mut() {
            Some(Select::Lasso { points }) => points.clear(),
            Some(Select::Rect { from, .. }) => *from = None,
            None => {}
        }

        match resolved {
            Some(selection) => self.set_selection(Some(selection)),
            // A gesture that enclosed nothing -- a tap, or a lasso of three coincident points --
            // reads as "deselect", which is what a click on empty space means in every art app.
            None => self.set_selection(None),
        }
    }

    /// Replace the selection, recomputing the outline it draws.
    ///
    /// The single place the selection changes, so the outline can never be stale: it is derived
    /// from the mask, and deriving it anywhere else would let the two drift.
    fn set_selection(&mut self, selection: Option<openpaint_core::Selection>) {
        self.selection = selection.map(|mask| {
            let outline = mask.outline();
            ActiveSelection { mask, outline }
        });
        self.status_message = Some(match self.selection.as_ref() {
            Some(_) => "Selected".to_owned(),
            None => "Deselected".to_owned(),
        });
        self.request_redraw();
    }

    /// Act on a selection command from the panel or a shortcut.
    fn apply_select_action(&mut self, action: ui::SelectAction) {
        let page = self.editor.page_rect();
        match action {
            ui::SelectAction::Use(tool) => {
                let already = matches!(
                    (&self.select, tool),
                    (Some(Select::Lasso { .. }), ui::SelectTool::Lasso)
                        | (Some(Select::Rect { .. }), ui::SelectTool::Rect)
                );
                self.select = if already {
                    None
                } else {
                    // The crop tool also consumes input entirely, so the two cannot both be up.
                    self.crop = None;
                    Some(match tool {
                        ui::SelectTool::Lasso => Select::Lasso { points: Vec::new() },
                        ui::SelectTool::Rect => Select::Rect {
                            from: None,
                            to: (0.0, 0.0),
                        },
                    })
                };
                self.request_redraw();
            }
            ui::SelectAction::All => {
                self.set_selection(Some(openpaint_core::Selection::everything(page)))
            }
            ui::SelectAction::None => self.set_selection(None),
            ui::SelectAction::Invert => {
                // Inverting nothing selects everything, which is what every art app does and is
                // more useful than refusing.
                let inverted = self.selection.as_ref().map_or_else(
                    || openpaint_core::Selection::everything(page),
                    |s| s.mask.inverted(page),
                );
                self.set_selection((!inverted.is_empty()).then_some(inverted));
            }
        }
    }

    /// The selection and any in-progress gesture, in screen space, ready to draw.
    fn selection_overlay(&self) -> Vec<[[f32; 2]; 2]> {
        let Some(renderer) = self.renderer.as_ref() else {
            return Vec::new();
        };
        let (sw, sh) = renderer.size_px();
        let to_screen = |p: (f32, f32)| {
            let (sx, sy) = self.view.canvas_to_screen(p.0, p.1, sw, sh);
            [sx, sy]
        };

        // The committed selection, plus the gesture being drawn over it.
        let committed = self.selection.iter().flat_map(|s| s.outline.iter());
        let live = self.select.iter().flat_map(Select::preview);
        committed
            .copied()
            .chain(live)
            .map(|(a, b)| [to_screen(a), to_screen(b)])
            .collect()
    }

    fn crop_overlay(&self) -> Option<ui::CropOverlay> {
        let crop = self.crop.as_ref()?;
        let renderer = self.renderer.as_ref()?;
        let (sw, sh) = renderer.size_px();
        let rect = crop.rect();

        let to_screen = |x: f32, y: f32| {
            let (sx, sy) = self.view.canvas_to_screen(x, y, sw, sh);
            [sx, sy]
        };
        let (x0, y0) = (rect.x as f32, rect.y as f32);
        let (ex, ey) = rect.end();
        let (x1, y1) = (ex as f32, ey as f32);

        let mut handles = [[0.0; 2]; 8];
        for (slot, handle) in handles.iter_mut().zip(crop::Handle::EDGES_AND_CORNERS) {
            let (hx, hy) = handle.position(rect);
            *slot = to_screen(hx, hy);
        }

        Some(ui::CropOverlay {
            corners: [
                to_screen(x0, y0),
                to_screen(x1, y0),
                to_screen(x1, y1),
                to_screen(x0, y1),
            ],
            handles,
        })
    }

    /// Mirror a document change from undo/redo onto the page.
    ///
    /// The renderer has already moved its own state; the page's rectangle and its layer stack
    /// live in the editor, so both halves must be applied for them to agree. Nothing done here
    /// may be recorded in history -- it *is* the undo, not a new edit.
    fn apply_history_change(&mut self, change: renderer::HistoryChange) {
        // An undo moves the document away from what the file holds just as an edit does.
        if change != renderer::HistoryChange::None {
            self.mark_dirty();
        }
        match change {
            renderer::HistoryChange::None => {}
            renderer::HistoryChange::Pixels => self.request_redraw(),
            renderer::HistoryChange::Geometry { rect } => {
                self.editor.resize_page(rect);
                self.request_redraw();
            }
            renderer::HistoryChange::LayerRestored { index, layer } => {
                self.editor
                    .document_mut()
                    .active_mut()
                    .restore_layer(index, layer);
                self.request_redraw();
            }
            renderer::HistoryChange::LayerDeleted { index } => {
                self.editor.document_mut().active_mut().remove_layer(index);
                self.request_redraw();
            }
            renderer::HistoryChange::PageRestored { index, page } => {
                self.editor.document_mut().restore_page(index, page);
                self.follow_active_page();
                self.request_redraw();
            }
            renderer::HistoryChange::PageDeleted { index } => {
                self.editor.document_mut().remove_page(index);
                self.follow_active_page();
                self.request_redraw();
            }
        }
    }

    /// Grow the current page on one side.
    ///
    /// A convenience over [`OpenPaint::apply_page_rect`]: it only works out the target
    /// rectangle, so every resize shares the same guards and consequences.
    fn extend_page(&mut self, side: Side, amount: u32) {
        if amount == 0 {
            return;
        }
        let target = self
            .editor
            .document()
            .active()
            .rect()
            .extended(side, amount);
        self.apply_page_rect(target);
    }

    /// Move the current page to a new rectangle, keeping everything consistent.
    ///
    /// The single place that knows a resize has consequences beyond the page itself: the
    /// Extend, crop, and drag-to-resize all come through here.
    ///
    /// Nothing is destroyed: the page rectangle moves and the tiles stay where they are
    /// (DECISIONS §5c). Content coordinates are stable, so nothing needs compensating
    /// either — the rectangle moves around the drawing rather than the drawing moving
    /// inside it.
    fn apply_page_rect(&mut self, target: PageRect) {
        let old = self.editor.document().active().rect();

        let Some((new_w, new_h)) = editor::clamp_page_size((old.w, old.h), (target.w, target.h))
        else {
            self.status_message = Some(format!("Page is already {}x{}", old.w, old.h));
            self.request_redraw();
            return;
        };
        if (new_w, new_h) != (target.w, target.h) {
            self.status_message = Some(format!(
                "Clamped to {} px, the largest coordinates stay exact at",
                editor::MAX_PAGE_DIMENSION
            ));
        }

        let new = PageRect::new(target.x, target.y, new_w, new_h);
        let resize = openpaint_core::PageResize { old, new };

        self.editor.resize_page(new);
        if let Some(r) = self.renderer.as_mut() {
            r.resize_canvas(resize, true);
        }
        self.mark_dirty();

        // Deliberately does NOT re-fit. Photoshop keeps your zoom through a canvas
        // resize, and having the camera jump is disorienting when working zoomed in --
        // press 0 to fit.
        self.request_redraw();
    }

    /// Apply a page change from the panel.
    ///
    /// Deletion is the only one that touches pixels, and it destroys a whole stack at once --
    /// the largest thing a single click can throw away -- so it is recorded in history first and
    /// refused if there is no room (DECISIONS §5c).
    fn apply_page_action(&mut self, action: ui::PageAction) {
        self.mark_dirty();
        match action {
            ui::PageAction::Select(index) => {
                self.editor.stroke_end();
                if self.editor.document_mut().set_active(index) {
                    self.follow_active_page();
                }
            }
            ui::PageAction::Add => {
                self.editor.stroke_end();
                let index = self.editor.document_mut().add_page_like_active();
                self.follow_active_page();
                self.status_message = Some(format!("Added page {}", index + 1));
            }
            ui::PageAction::Delete(index) => self.delete_page(index),
            ui::PageAction::Move { from, to } => {
                self.editor.document_mut().move_page(from, to);
            }
        }
        self.request_redraw();
    }

    /// Point the renderer and the camera at whatever page is now active.
    ///
    /// Re-fits deliberately: a different page is very likely a different size, so keeping the
    /// zoom would leave the artist looking at the middle of nowhere. Same reasoning as opening a
    /// document, and unlike a *resize*, where keeping the zoom is what you want.
    fn follow_active_page(&mut self) {
        let rect = self.editor.page_rect();
        if let Some(r) = self.renderer.as_mut() {
            r.set_page(rect);
        }
        self.view.request_fit();
    }

    /// Delete a page, undoably.
    fn delete_page(&mut self, index: usize) {
        self.editor.stroke_end();
        let Some(page) = self.editor.document().page(index).cloned() else {
            return;
        };
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let Some(tiles) = renderer.adopt_page(&page) else {
            self.status_message =
                Some("No room to record the deletion, so the page was kept".to_owned());
            self.request_redraw();
            return;
        };
        if self.editor.document_mut().remove_page(index).is_none() {
            self.status_message = Some("A document needs at least one page".to_owned());
            self.apply_history_change(renderer::HistoryChange::PageRestored { index, page });
            return;
        }
        if let Some(r) = self.renderer.as_mut() {
            r.record_page_deletion(index, page, tiles);
        }
        self.follow_active_page();
        self.status_message = Some(format!(
            "Deleted page {} (Ctrl+Z to bring it back)",
            index + 1
        ));
        self.request_redraw();
    }

    /// Apply a layer change from the panel.
    ///
    /// Deletion is the only one that touches pixels, and it is recorded in history first: a
    /// layer holds work, so destroying one with no way back is the same mistake as a
    /// destructive crop (DECISIONS §5c). If there is no room to record it, the delete is
    /// refused rather than done.
    fn apply_layer_action(&mut self, action: ui::LayerAction) {
        self.mark_dirty();
        match action {
            ui::LayerAction::Select(index) => {
                // Ends any stroke first: a stroke belongs to the layer it started on.
                self.editor.stroke_end();
                self.editor.document_mut().active_mut().set_active(index);
            }
            ui::LayerAction::Add => {
                self.editor.stroke_end();
                let index = self.editor.document_mut().add_layer();
                self.status_message = Some(format!("Added layer {}", index + 1));
            }
            ui::LayerAction::Delete(index) => self.delete_layer(index),
            ui::LayerAction::Move { from, to } => {
                self.editor.document_mut().active_mut().move_layer(from, to);
            }
            ui::LayerAction::SetVisible { index, visible } => {
                if let Some(l) = self.editor.document_mut().active_mut().layer_mut(index) {
                    l.visible = visible;
                }
            }
            ui::LayerAction::SetLockAlpha { index, lock } => {
                // Not undoable, deliberately, and the same call as visibility: it changes no pixels.
                // Putting a switch in the undo stack would make Ctrl+Z toggle settings instead of
                // reversing artwork, which is the more surprising behaviour by far.
                if let Some(l) = self.editor.document_mut().active_mut().layer_mut(index) {
                    l.lock_alpha = lock;
                }
                self.mark_dirty();
            }
            ui::LayerAction::SetClipBelow { index, clip } => {
                // Not undoable, like visibility and alpha lock: it changes no pixels, and a switch
                // in the undo stack would make Ctrl+Z toggle settings rather than reverse artwork.
                if let Some(l) = self.editor.document_mut().active_mut().layer_mut(index) {
                    l.clip_below = clip;
                }
                self.mark_dirty();
            }
            ui::LayerAction::SetOpacity { index, opacity } => {
                if let Some(l) = self.editor.document_mut().active_mut().layer_mut(index) {
                    l.opacity = opacity;
                }
            }
            ui::LayerAction::SetBlend { index, blend } => {
                if let Some(l) = self.editor.document_mut().active_mut().layer_mut(index) {
                    l.blend = blend;
                }
            }
        }
        self.request_redraw();
    }

    /// Delete a layer, undoably.
    fn delete_layer(&mut self, index: usize) {
        self.editor.stroke_end();
        let Some(layer) = self.editor.document().active().layer(index).cloned() else {
            return;
        };
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let Some(tiles) = renderer.adopt_layer(tile_store::LayerId(layer.id())) else {
            self.status_message =
                Some("No room to record the deletion, so the layer was kept".to_owned());
            self.request_redraw();
            return;
        };
        if self
            .editor
            .document_mut()
            .active_mut()
            .remove_layer(index)
            .is_none()
        {
            // The last layer cannot go; put its tiles straight back.
            self.status_message = Some("A page needs at least one layer".to_owned());
            self.apply_history_change(renderer::HistoryChange::LayerRestored {
                index,
                layer: layer.clone(),
            });
            return;
        }
        let name = layer.name.clone();
        if let Some(r) = self.renderer.as_mut() {
            r.record_layer_deletion(index, layer, tiles);
        }
        self.status_message = Some(format!("Deleted {name} (Ctrl+Z to bring it back)"));
        self.request_redraw();
    }

    /// Resize the active tool's brush by a factor, and say what it became.
    ///
    /// The status line matters more than it looks: the pen cannot reach the panel (Q14), so
    /// without feedback the only way to know the new size is to draw with it.
    fn scale_brush(&mut self, factor: f32) -> bool {
        let brush = self.editor.brush_mut();
        brush.radius = (brush.radius * factor).clamp(MIN_BRUSH_RADIUS, MAX_BRUSH_RADIUS);
        let radius = brush.radius;
        let tool = self.editor.tool().label();
        self.status_message = Some(format!("{tool} size {:.1} px", radius * 2.0));
        self.request_redraw();
        true
    }

    /// Switch tool from the keyboard.
    fn pick_tool(&mut self, tool: editor::Tool) -> bool {
        self.editor.set_tool(tool);
        self.status_message = Some(tool.label().to_owned());
        self.request_redraw();
        true
    }

    /// Note that the document differs from its file.
    fn mark_dirty(&mut self) {
        if !self.dirty {
            self.dirty = true;
            self.update_title();
        }
    }

    /// Note that the document matches its file, and throw away the recovery copy.
    ///
    /// The two belong together: a recovery copy exists *only* while there is unsaved work, which is
    /// what makes one surviving to the next launch mean "that process died" rather than merely
    /// "that process ran". Every path that makes the document clean goes through here.
    fn mark_clean(&mut self) {
        self.dirty = false;
        self.autosave.discard();
        self.update_title();
    }

    /// Write a recovery copy if one is due.
    ///
    /// Skipped mid-stroke: a save reads every resident tile back off the GPU, which would both
    /// hitch the one thing that must never hitch and capture a stroke halfway through.
    fn maybe_autosave(&mut self) {
        if !self.autosave.is_due(self.dirty, self.editor.is_drawing()) {
            return;
        }
        let Some(path) = self.autosave.path().map(std::path::Path::to_path_buf) else {
            return;
        };

        // The document it belongs to, so recovery can hand back something that still knows where it
        // lives instead of an untitled orphan.
        let original = self
            .document_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let mut meta: Vec<(&str, &str)> = vec![(autosave::IS_RECOVERY, "1")];
        if let Some(original) = original.as_deref() {
            meta.push((autosave::ORIGINAL_PATH, original));
        }

        let started = Instant::now();
        let document = self.editor.document();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        match renderer.save_document(document, &path, &meta) {
            Ok(tiles) => self.autosave.record(started.elapsed(), tiles),
            Err(e) => {
                // Not a status message: autosave is background work the user did not ask for, and
                // interrupting them about it every minute would be worse than the failure. The
                // panel shows that it has not run, which is the honest signal.
                eprintln!("autosave failed: {e}");
                self.autosave.postpone();
            }
        }
    }

    /// Load an abandoned recovery copy as the live document.
    ///
    /// It comes back **dirty and pointed at the original file**, not at the copy: the work in it was
    /// never saved, so pretending otherwise would let the next Ctrl+S write into the recovery
    /// directory and leave the artist's real file untouched.
    fn recover(&mut self, recoverable: &autosave::Recoverable) {
        let from = recoverable.path.clone();
        self.load_from(&from);
        self.document_path = recoverable.original.clone();
        self.dirty = true;
        self.update_title();
        self.status_message = Some(match self.document_path.as_ref() {
            Some(p) => format!("Recovered unsaved changes to {}", p.display()),
            None => "Recovered an unsaved document -- save it somewhere".to_owned(),
        });
        // The work is live in this session now, and this session has its own copy to protect it,
        // so the abandoned one has done its job.
        let _ = std::fs::remove_file(&from);
        self.request_redraw();
    }

    /// Show the document's name and whether it has unsaved edits.
    ///
    /// In the title bar rather than the panel because it has to be true even when the panel is
    /// scrolled away, and because it is the one place every OS already trains people to look.
    fn update_title(&self) {
        let name = self.document_path.as_ref().map_or_else(
            || "Untitled".to_owned(),
            |p| {
                p.file_name()
                    .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
            },
        );
        let marker = if self.dirty { "*" } else { "" };
        if let Some(r) = self.renderer.as_ref() {
            r.window().set_title(&format!("{marker}{name} - OpenPaint"));
        }
    }

    /// Park a dialog request for `about_to_wait` to service. See [`Dialog`].
    fn request_dialog(&mut self, dialog: Dialog) {
        self.pending_dialog = Some(dialog);
        // A polled backend wakes on its own; an event-driven one needs a nudge, or the request
        // would sit here until the next unrelated event.
        self.request_redraw();
    }

    /// Show whatever dialog was parked, and act on it.
    ///
    /// Only ever called from `about_to_wait`.
    fn service_dialog(&mut self, event_loop: &ActiveEventLoop) {
        let Some(dialog) = self.pending_dialog.take() else {
            return;
        };
        match dialog {
            // When a path is already known the save happens now, so anything waiting on it can
            // run. When a dialog is needed, the wait continues in `poll_file_dialog`.
            Dialog::Save => {
                if self.document_path.is_some() {
                    self.save_to_current_path();
                    self.continue_after_save();
                } else {
                    self.save_as();
                }
            }
            Dialog::SaveAs => self.save_as(),
            Dialog::Open { confirmed } => {
                if confirmed || !self.dirty {
                    self.open_with_dialog();
                } else {
                    self.ask_confirm(Dialog::Open { confirmed: true }, "open another document");
                }
            }
            Dialog::New { confirmed } => {
                if confirmed || !self.dirty {
                    self.new_document();
                } else {
                    self.ask_confirm(Dialog::New { confirmed: true }, "start a new document");
                }
            }
            Dialog::Quit { confirmed } => {
                if confirmed || !self.dirty {
                    event_loop.exit();
                } else {
                    self.ask_confirm(Dialog::Quit { confirmed: true }, "quit");
                }
            }
        }
    }

    /// Put the unsaved-changes question on screen.
    fn ask_confirm(&mut self, then: Dialog, what: &'static str) {
        self.pending_confirm = Some(Confirm { then, what });
        self.request_redraw();
    }

    /// Act on the answer to the unsaved-changes question.
    fn answer_confirm(&mut self, choice: ui::ConfirmChoice) {
        let Some(confirm) = self.pending_confirm.take() else {
            return;
        };
        match choice {
            ui::ConfirmChoice::Cancel => {}
            ui::ConfirmChoice::Discard => {
                self.pending_dialog = Some(confirm.then);
            }
            ui::ConfirmChoice::SaveFirst => {
                // Saving may itself need a native path picker, so it goes through the deferred
                // path like everything else, and the original action waits for it to succeed.
                self.after_save = Some(confirm.then);
                self.pending_dialog = Some(Dialog::Save);
            }
        }
        self.request_redraw();
    }

    /// Run whatever was waiting on a save, but only if the save actually happened.
    ///
    /// A cancelled or failed save must not be treated as permission to discard the work.
    fn continue_after_save(&mut self) {
        let next = self.after_save.take();
        if self.dirty {
            return;
        }
        if let Some(next) = next {
            self.pending_dialog = Some(next);
            self.request_redraw();
        }
    }

    /// Write to the path the document already has.
    fn save_to_current_path(&mut self) {
        let Some(path) = self.document_path.clone() else {
            return;
        };
        let document = self.editor.document();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        match renderer.save_document(document, &path, &[]) {
            Ok(tiles) => {
                self.mark_clean();
                self.status_message = Some(format!("Saved {tiles} tiles to {}", path.display()));
            }
            Err(e) => self.status_message = Some(format!("Save failed: {e}")),
        }
        self.update_title();
        self.request_redraw();
    }

    /// Ask for a path on another thread, then save to it when the answer arrives.
    ///
    /// **On its own thread, deliberately.** Two problems it solves at once:
    ///
    /// 1. A Windows file dialog needs a single-threaded COM apartment, and our thread's
    ///    apartment is whatever RealTimeStylus initialised it to. In the wrong apartment the
    ///    dialog opens and then hangs forever on "Working on it..." while the shell tries to
    ///    enumerate a folder -- which is exactly what it did. A fresh thread gets the apartment
    ///    the dialog wants.
    /// 2. A modal dialog runs its own message loop, dispatching our pending messages back into
    ///    us. Keeping it off our thread means that cannot happen at all, rather than being
    ///    guarded against -- and that hazard is this project's most expensive recurring bug
    ///    (Q10c).
    ///
    /// The cost is that the answer arrives later, so the app keeps running rather than blocking.
    /// That is an improvement too: the canvas stays live behind the dialog.
    fn save_as(&mut self) {
        let suggested = self
            .document_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "untitled.openpaint".to_owned());
        self.spawn_file_dialog(FileDialogKind::Save, move |dialog| {
            dialog.set_file_name(suggested).save_file()
        });
    }

    /// Ask for a file on another thread, then open it when the answer arrives.
    fn open_with_dialog(&mut self) {
        self.spawn_file_dialog(FileDialogKind::Open, rfd::FileDialog::pick_file);
    }

    /// Run a file dialog on a fresh thread and remember where its answer will land.
    fn spawn_file_dialog(
        &mut self,
        kind: FileDialogKind,
        show: impl FnOnce(rfd::FileDialog) -> Option<std::path::PathBuf> + Send + 'static,
    ) {
        if self.file_dialog.is_some() {
            // One at a time. A second would be modal to nothing and confusing.
            return;
        }
        let Some(window) = self.renderer.as_ref().map(|r| r.window().clone()) else {
            return;
        };
        let (tx, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Owned by our window, so it cannot end up behind it. The `Arc<Window>` moves in
            // with it, which is also what keeps the handle valid for as long as the dialog is up.
            let dialog = rfd::FileDialog::new()
                .set_parent(window.as_ref())
                .add_filter("OpenPaint document", &[DOCUMENT_EXTENSION]);
            // The receiver is gone if the app closed meanwhile; nothing to do about that.
            let _ = tx.send(show(dialog));
        });
        self.file_dialog = Some(FileDialogTask { kind, answer });
        self.request_redraw();
    }

    /// Act on a file dialog that has finished, if one has.
    fn poll_file_dialog(&mut self) {
        let Some(task) = self.file_dialog.as_ref() else {
            return;
        };
        let answer = match task.answer.try_recv() {
            Ok(answer) => answer,
            // Still up. `Disconnected` means the thread died without answering, which is a
            // cancellation as far as we are concerned.
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let kind = task.kind;
        self.file_dialog = None;

        match (kind, answer) {
            (FileDialogKind::Save, Some(path)) => {
                // The dialog does not always append it, and a document without the extension
                // will not match the open dialog's filter later.
                let path = if path.extension().is_some() {
                    path
                } else {
                    path.with_extension(DOCUMENT_EXTENSION)
                };
                self.document_path = Some(path);
                self.save_to_current_path();
                self.continue_after_save();
            }
            (FileDialogKind::Open, Some(path)) => self.load_from(&path),
            // Cancelled. Anything that was waiting on a save does not happen.
            (_, None) => {
                self.after_save = None;
                self.request_redraw();
            }
        }
    }

    /// Replace the open document with the one in `path`.
    fn load_from(&mut self, path: &std::path::Path) {
        let loaded = match openpaint_file::load(path) {
            Ok(l) => l,
            Err(e) => {
                self.status_message = Some(format!("Open failed: {e}"));
                self.request_redraw();
                return;
            }
        };
        self.editor.stroke_end();
        self.crop = None;
        // A mask is in page coordinates, so it means nothing once the page it was drawn on is gone.
        self.set_selection(None);

        let tiles: Vec<_> = loaded.tiles.into_iter().collect();
        let pages = loaded.document.page_count();
        let rect = loaded.document.active().rect();
        self.editor.replace_document(loaded.document);
        if let Some(r) = self.renderer.as_mut() {
            r.load_document(rect, tiles);
        }
        // A fresh document deserves a fresh view; its page is very likely a different size.
        self.view.request_fit();
        self.document_path = Some(path.to_path_buf());
        self.mark_clean();
        self.status_message = Some(format!(
            "Opened {} ({pages} page{})",
            path.display(),
            if pages == 1 { "" } else { "s" }
        ));
        self.request_redraw();
    }

    /// Replace everything with an empty document.
    fn new_document(&mut self) {
        self.editor.stroke_end();
        self.crop = None;
        // A mask is in page coordinates, so it means nothing once the page it was drawn on is gone.
        self.set_selection(None);
        self.editor
            .replace_document(openpaint_core::Document::new(openpaint_core::Page::new(
                editor::PAGE_W,
                editor::PAGE_H,
            )));
        let rect = self.editor.page_rect();
        if let Some(r) = self.renderer.as_mut() {
            r.load_document(rect, Vec::new());
        }
        self.view.request_fit();
        self.document_path = None;
        self.mark_clean();
        self.status_message = Some("New document".to_owned());
        self.request_redraw();
    }

    /// Discard the tiles outside the page, reclaiming their memory.
    ///
    /// The only action that destroys pixels, which is why it is explicit and separate from
    /// crop (DECISIONS §5c).
    fn trim_to_page(&mut self) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        let (released, refused) = r.trim_to_page();
        self.mark_dirty();
        self.status_message = Some(match (released, refused) {
            (0, false) => "Nothing outside the page to trim".to_owned(),
            (0, true) => "No room to record the trim; nothing was discarded".to_owned(),
            (n, false) => format!("Trimmed {n} tiles outside the page (undoable)"),
            (n, true) => format!(
                "Trimmed {n} tiles; some were kept because there was no room to record them"
            ),
        });
        self.request_redraw();
    }

    /// Enter applies the crop, Escape cancels it. Only while the tool is up, so neither
    /// key is stolen from anything else.
    fn handle_crop_keys(&mut self, event: &WindowEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::{Key, NamedKey};

        if self.crop.is_none() {
            return false;
        }
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ElementState::Pressed {
            return false;
        }
        match key.logical_key {
            Key::Named(NamedKey::Enter) => {
                self.crop_action(ui::CropAction::Apply);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.crop_action(ui::CropAction::Cancel);
                true
            }
            _ => false,
        }
    }

    /// Handle undo/redo shortcuts. Returns `true` if the event was consumed.
    ///
    /// Ctrl+Z undoes, Ctrl+Shift+Z and Ctrl+Y redo; Ctrl+N, Ctrl+O, Ctrl+S and Ctrl+Shift+S are
    /// new/open/save/save-as; Ctrl+E exports a PNG; Ctrl+A, Ctrl+D and Ctrl+Shift+I select all,
    /// deselect and invert -- the bindings every art app shares. Hardcoded for now, like navigation
    /// (OPEN_QUESTIONS Q16).
    fn handle_history(&mut self, event: &WindowEvent) -> bool {
        use winit::event::ElementState;
        use winit::keyboard::Key;

        match event {
            WindowEvent::ModifiersChanged(mods) => {
                self.nav.modifiers = mods.state();
                false
            }
            WindowEvent::KeyboardInput { event: key, .. } => {
                if key.state != ElementState::Pressed || !self.nav.modifiers.control_key() {
                    return false;
                }
                let Key::Character(c) = &key.logical_key else {
                    return false;
                };
                match c.as_str() {
                    // Ctrl+S is save, as it should be; export moved to Ctrl+E.
                    "s" | "S" => {
                        self.request_dialog(if self.nav.modifiers.shift_key() {
                            Dialog::SaveAs
                        } else {
                            Dialog::Save
                        });
                        return true;
                    }
                    "o" | "O" => {
                        self.request_dialog(Dialog::Open { confirmed: false });
                        return true;
                    }
                    "n" | "N" => {
                        self.request_dialog(Dialog::New { confirmed: false });
                        return true;
                    }
                    "a" | "A" => {
                        self.apply_select_action(ui::SelectAction::All);
                        return true;
                    }
                    "d" | "D" => {
                        self.apply_select_action(ui::SelectAction::None);
                        return true;
                    }
                    // Ctrl+Shift+I, the binding every art app uses for invert-selection.
                    "i" | "I" if self.nav.modifiers.shift_key() => {
                        self.apply_select_action(ui::SelectAction::Invert);
                        return true;
                    }
                    "e" | "E" => {
                        self.export_png();
                        return true;
                    }
                    _ => {}
                }
                let redo = match c.as_str() {
                    "z" | "Z" => self.nav.modifiers.shift_key(),
                    "y" | "Y" => true,
                    _ => return false,
                };

                // Refuse mid-stroke. The in-progress stroke is not in history yet
                // and is still accumulating, so undoing here would revert the
                // *previous* stroke and then bake the current one on top of the
                // restored image -- a state the user never asked for and cannot
                // reason about.
                if self.editor.is_drawing() {
                    return true;
                }

                let Some(renderer) = self.renderer.as_mut() else {
                    return true;
                };
                let change = if redo {
                    renderer.redo()
                } else {
                    renderer.undo()
                };
                self.apply_history_change(change);
                true
            }
            _ => false,
        }
    }

    /// Export the flattened canvas to a PNG in the working directory.
    ///
    /// Ctrl+E, not Ctrl+S: saving the *document* is what Ctrl+S means now that there is a
    /// document format to save into.
    fn export_png(&mut self) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let path = export::default_path();
        self.status_message = Some(match renderer.export_png(self.editor.layers(), &path) {
            Ok(()) => {
                // Absolute, because a bare relative name leaves the user hunting
                // for the file -- the working directory is not obvious when the app
                // was launched from a script or an IDE.
                let shown = std::env::current_dir()
                    .map_or_else(|_| path.clone(), |dir| dir.join(&path))
                    .display()
                    .to_string();
                println!("exported {shown}");
                format!("Exported {shown}")
            }
            Err(e) => {
                eprintln!("export failed: {e}");
                format!("Export failed: {e}")
            }
        });
        self.request_redraw();
    }

    /// Handle canvas navigation: pan, zoom, rotate, fit. Returns `true` if the
    /// event was navigation and should go no further.
    ///
    /// Bindings follow Photoshop/CSP habits (DECISIONS section 1a):
    ///   - space + drag, or middle-drag, pans
    ///   - wheel zooms about the cursor
    ///   - Ctrl+0 fits the canvas, Ctrl+1 goes to 100%
    ///   - `[` / `]` resize the brush, Shift+`[` / Shift+`]` rotate about the cursor
    ///   - `b` / `e` choose brush or eraser
    fn handle_navigation(&mut self, event: &WindowEvent) -> bool {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

        let Some((w, h)) = self.renderer.as_ref().map(Renderer::size_px) else {
            return false;
        };

        match event {
            // The pointer is gone, so the brush ring must go with it -- a ring frozen at the edge
            // of the window says the brush is somewhere it is not. Forgetting the position also
            // returns the zoom anchor to the centre of the view, which is the right answer for a
            // wheel event that arrives with no pointer over us.
            WindowEvent::CursorLeft { .. } => {
                if self.nav.cursor.take().is_some() {
                    self.request_redraw();
                }
                // Not navigation, so it goes on to the rest of the handlers.
                false
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Through `note_pointer`, not by assignment, and here as well as on the pen seam's
                // `Hover`: a pan drag is swallowed below, so this is the only path that keeps the
                // pointer position current while one is in progress.
                if self.note_pointer(position.x, position.y) {
                    self.request_redraw();
                }
                if let Some(from) = self.nav.panning_from {
                    let dx = (position.x - from.0) as f32;
                    let dy = (position.y - from.1) as f32;
                    self.view.pan_by_screen(dx, dy);
                    self.nav.panning_from = Some((position.x, position.y));
                    self.request_redraw();
                    return true;
                }
                false
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let start_pan = matches!(button, MouseButton::Middle)
                    || (self.nav.space_held && matches!(button, MouseButton::Left));
                match (state, start_pan) {
                    (ElementState::Pressed, true) => {
                        self.nav.panning_from = Some(self.nav.anchor(w, h));
                        true
                    }
                    (ElementState::Released, _) if self.nav.panning_from.is_some() => {
                        self.nav.panning_from = None;
                        true
                    }
                    _ => false,
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // Over the panel, the wheel belongs to the panel: it is taller than the window
                // and has to be scrollable. egui does get first refusal on events before this
                // handler runs, but it only reports a scroll as consumed once it has a scrollable
                // area under the pointer *as of the last frame* -- so on the first notch after
                // the pointer arrives, the canvas would jump. Asking where the pointer is costs
                // nothing and does not depend on a frame having happened.
                if self
                    .nav
                    .cursor
                    .is_some_and(|(cx, cy)| self.ui_blocks_point(cx, cy))
                {
                    return false;
                }
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    // Trackpads report pixels; ~50px per notch feels close to a
                    // wheel click without being twitchy.
                    MouseScrollDelta::PixelDelta(p) => (p.y / 50.0) as f32,
                };
                if notches != 0.0 {
                    self.view
                        .zoom_by_notches(notches, self.nav.anchor(w, h), w, h);
                    self.request_redraw();
                }
                true
            }

            WindowEvent::KeyboardInput { event: key, .. } => {
                let pressed = key.state == ElementState::Pressed;

                // Rotation is matched on *physical* key position, not the logical
                // character. Two reasons: bracket keys don't always resolve to a
                // character (they arrive as `Unidentified` under synthetic input,
                // and layouts differ), and position-consistent bindings are what
                // you want for a held modifier-style action anyway.
                if pressed {
                    let step = match key.physical_key {
                        // Only with Shift: the bare brackets resize the brush, as they do in
                        // every art app, and that is the far more frequent action.
                        PhysicalKey::Code(KeyCode::BracketLeft)
                            if self.nav.modifiers.shift_key() =>
                        {
                            Some(-ROTATE_STEP)
                        }
                        PhysicalKey::Code(KeyCode::BracketRight)
                            if self.nav.modifiers.shift_key() =>
                        {
                            Some(ROTATE_STEP)
                        }
                        _ => None,
                    };
                    if let Some(step) = step {
                        self.view.rotate_by(step, self.nav.anchor(w, h), w, h);
                        self.request_redraw();
                        return true;
                    }
                }

                match &key.logical_key {
                    Key::Named(NamedKey::Space) => {
                        self.nav.space_held = pressed;
                        if !pressed {
                            self.nav.panning_from = None;
                        }
                        true
                    }
                    // By logical key, so these are stable across keyboard layouts. Modifiers
                    // are excluded so Ctrl+S and friends are not eaten here.
                    Key::Character(c) if pressed && !self.nav.modifiers.control_key() => {
                        match c.as_str() {
                            "0" => {
                                self.view.request_fit();
                                self.request_redraw();
                                true
                            }
                            "1" => {
                                self.view.set_scale_about(1.0, self.nav.anchor(w, h), w, h);
                                self.request_redraw();
                                true
                            }
                            // Multiplicative, because brush sizes are perceived that way: one
                            // step at radius 4 should feel like one step at radius 40, which a
                            // fixed increment does not.
                            "[" => self.scale_brush(1.0 / BRUSH_STEP),
                            "]" => self.scale_brush(BRUSH_STEP),
                            "b" | "B" => self.pick_tool(editor::Tool::Brush),
                            "e" | "E" => self.pick_tool(editor::Tool::Eraser),
                            _ => false,
                        }
                    }
                    _ => false,
                }
            }

            _ => false,
        }
    }

    /// Draw one frame: canvas first, then the UI on top of it.
    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        // Computed up front, while `self` can still be borrowed immutably: the renderer
        // is taken mutably just below.
        let crop_overlay = self.crop_overlay();
        let crop_rect = self.crop.as_ref().map(|c| {
            let r = c.rect();
            (r.x, r.y, r.w, r.h)
        });
        let brush_cursor = self.brush_cursor();
        let selection_overlay = self.selection_overlay();
        let select_tool = self.select.as_ref().map(|s| match s {
            Select::Lasso { .. } => ui::SelectTool::Lasso,
            Select::Rect { .. } => ui::SelectTool::Rect,
        });
        let has_selection = self.selection.is_some();
        let perf = self.perf.snapshot();
        let recovery_prompt = self.recovery.as_ref().map(autosave::Recoverable::describe);
        // Built here rather than in the panel so the panel stays a renderer of state. It reports
        // the *cost* as well as the time, because that number is what decides whether the 60 s
        // interval is affordable or whether saving has to become incremental.
        let autosave_status = match self.autosave.last() {
            Some((_, took, tiles)) => {
                format!("Autosave: {tiles} tiles in {} ms", took.as_millis().max(1))
            }
            None if !self.autosave.available() => {
                "Autosave: unavailable (no writable data directory)".to_owned()
            }
            None if self.dirty => "Autosave: due within a minute".to_owned(),
            None => "Autosave: nothing unsaved".to_owned(),
        };

        // Disjoint field borrows, so the overlay closure can touch the editor and
        // the UI while the renderer is mutably borrowed.
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let ui = self.ui.as_mut();
        let editor = &mut self.editor;

        // Execute any queued stroke work before the frame reads the canvas.
        // Borrowed, not cloned: this runs every frame a stroke is active, so a
        // copy here would be an allocation per frame on the interactive path.
        // `renderer` and `editor` are disjoint fields, so both borrows coexist.
        let mut edited = false;
        if editor.has_pending_stroke() {
            let active_layer = editor.active_layer_id();
            let (ops, dabs) = editor.pending_stroke();
            renderer.apply_stroke(ops, dabs, tile_store::LayerId(active_layer));
            edited = true;
            editor.clear_pending_stroke();

            // Running out of tiles or out of snapshot room is a real, reachable state on a
            // bounded pool, and the artist has to be told rather than left wondering why
            // paint stopped landing or undo went quiet.
            if renderer.take_exhausted() {
                self.status_message = Some(
                    "Canvas memory is full; part of that stroke was not painted.                      Trim to canvas, or undo."
                        .to_owned(),
                );
            } else if renderer.take_unrecordable() {
                self.status_message =
                    Some("That stroke was too large to record; it cannot be undone".to_owned());
            }
        }

        let (w, h) = renderer.size_px();
        // Fitting needs both the surface size and the UI inset, so it is deferred
        // to here rather than done at construction.
        self.view.apply_pending_fit(w, h, editor.page_rect());
        let xform = self.view.page_to_ndc(w, h);
        // What the viewport covers, in page coordinates. Residency is bounded, so the
        // *visible* set is the working set: without this the renderer would try to restore
        // the whole document from the CPU every frame.
        let visible = self.view.visible_rect(w, h);

        let mut ui_wants_repaint = false;
        let mut ui_inset_left = None;
        let mut extend_request = None;
        let mut crop_request = None;
        let mut trim_request = false;
        let mut layer_request = None;
        let mut tool_request = None;
        let mut confirm_request = None;
        let mut recovery_request = None;
        let mut select_request = None;
        let mut page_request = None;
        let history_status = renderer.history_status();
        let residency = renderer.residency();
        let (spilled, traffic) = renderer.spill_status();
        let status_message = self.status_message.clone();
        let page_size = {
            let page = editor.document().active();
            (page.width(), page.height())
        };
        // Cloned, not borrowed: `editor` is handed to the overlay closure mutably, and the
        // stack is a handful of small structs so the copy is immaterial next to the borrow
        // gymnastics avoiding it would need.
        let layers = editor.layers().to_vec();
        let active_index = editor.active_layer_index();
        let active_tool = editor.tool();
        let confirm_prompt = self.pending_confirm.map(|c| c.what);
        let page_count = editor.document().page_count();
        let active_page = editor.document().active_index();
        let window = renderer.window().clone();
        // Borrowed, not copied: a copy would mean any future UI control that edits
        // the view silently writes to a dead value. Disjoint field borrows make
        // this fine alongside the mutable borrows of `renderer` and `editor`.
        let view = &self.view;
        // Started here rather than at the top of `redraw`: what this measures is the cost of
        // producing a frame, and the work above it is bookkeeping that happens whether or not a
        // frame follows. `render` presents before it returns, so the interval ends at the present.
        let frame_start = Instant::now();
        let result = renderer.render(xform, visible, &layers, active_index, |gpu| {
            if let Some(ui) = ui {
                let out = ui.render(
                    &window,
                    gpu,
                    editor.brush_mut(),
                    view,
                    ui::Status {
                        history: history_status,
                        message: status_message.as_deref(),
                        page_size,
                        crop: crop_overlay.as_ref(),
                        crop_rect,
                        residency,
                        spilled,
                        traffic,
                        layers: &layers,
                        active_layer: active_index,
                        pages: (page_count, active_page),
                        tool: active_tool,
                        confirm: confirm_prompt,
                        brush_cursor,
                        perf,
                        recovery: recovery_prompt.as_deref(),
                        autosave: &autosave_status,
                        selection: &selection_overlay,
                        select_tool,
                        has_selection,
                    },
                );
                ui_wants_repaint = out.wants_repaint;
                extend_request = out.extend;
                crop_request = out.crop;
                trim_request = out.trim;
                layer_request = out.layer;
                page_request = out.page;
                tool_request = out.tool;
                confirm_request = out.confirm;
                recovery_request = out.recovery;
                select_request = out.select;
                ui_inset_left = Some(ui.inset_left_px());
            }
        });

        // Recorded whatever `render` returned: a frame that failed still consumed the time, and
        // hiding those would make the readout flatter than the app.
        self.perf
            .frame
            .push(frame_start.elapsed().as_secs_f32() * 1000.0);
        if let Some(sent) = self.pending_sample_ms.take() {
            self.perf
                .input
                .push((input::now_ms() - sent).max(0.0) as f32);
        }

        if edited {
            self.mark_dirty();
        }

        // Keep the canvas centered in the area the panel leaves free. The first
        // fit necessarily ran before the UI existed, so learning the inset queues
        // another one -- which needs a frame to actually apply.
        let refit_queued = ui_inset_left.is_some_and(|inset| self.view.set_inset_left(inset));
        if ui_wants_repaint || refit_queued {
            self.request_redraw();
        }

        // Applied after the frame, not inside the overlay closure: resizing
        // re-creates the very GPU resources the frame is drawing with.
        if let Some((side, amount)) = extend_request {
            self.extend_page(side, amount);
        }
        // Zoomed far out on a heavily painted document, the visible tiles can outnumber the
        // pool. Say so rather than quietly drawing part of the canvas.
        if self.renderer.as_ref().is_some_and(Renderer::pressured) {
            self.status_message = Some(
                "Too much of the canvas is visible at once to keep on the GPU; zoom in.".to_owned(),
            );
        }
        if let Some(choice) = confirm_request {
            self.answer_confirm(choice);
        }
        if let Some(action) = select_request {
            self.apply_select_action(action);
        }
        if let Some(choice) = recovery_request {
            // Taken either way: an offer answered is an offer gone, and leaving it set would put
            // the prompt straight back up on the next frame.
            if let Some(found) = self.recovery.take() {
                match choice {
                    ui::RecoveryChoice::Recover => self.recover(&found),
                    ui::RecoveryChoice::Discard => {
                        let _ = std::fs::remove_file(&found.path);
                        self.status_message = Some("Discarded the recovered work".to_owned());
                    }
                }
            }
            self.request_redraw();
        }
        if let Some(tool) = tool_request {
            self.editor.set_tool(tool);
            self.request_redraw();
        }
        if let Some(action) = page_request {
            self.apply_page_action(action);
        }
        if let Some(action) = layer_request {
            self.apply_layer_action(action);
        }
        if trim_request {
            self.trim_to_page();
        }
        if let Some(action) = crop_request {
            self.crop_action(action);
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
                if self.dirty {
                    self.request_dialog(Dialog::Quit { confirmed: false });
                } else {
                    event_loop.exit();
                }
                return;
            }
            WindowEvent::Resized(new_size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(*new_size);
                }
                // Re-fit if the user hasn't taken manual control of the view.
                self.view.surface_resized();
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

        // The unsaved-changes prompt claims Enter and Escape while it is up, and takes
        // precedence over the crop tool: it is a question that has to be answered.
        if self.pending_confirm.is_some() {
            if let WindowEvent::KeyboardInput { event: key, .. } = &event {
                use winit::event::ElementState;
                use winit::keyboard::{Key, NamedKey};
                if key.state == ElementState::Pressed {
                    match key.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            self.answer_confirm(ui::ConfirmChoice::SaveFirst);
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.answer_confirm(ui::ConfirmChoice::Cancel);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }

        // The crop tool claims Enter and Escape while it is up.
        if self.handle_crop_keys(&event) {
            return;
        }

        // Undo/redo before navigation, so Ctrl+Z is never eaten as a plain 'z'.
        if self.handle_history(&event) {
            return;
        }

        // Canvas navigation. Before the input backend, so a pan drag is not also
        // interpreted as a stroke.
        if self.handle_navigation(&event) {
            return;
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
        if std::env::var("OPENPAINT_NO_PEN").is_err() {
            if let Some(pen) = input_pen::PenBackend::try_new(window.clone()) {
                self.input = Box::new(pen);
            }
        }

        match Renderer::new(window.clone(), self.editor.page_rect()) {
            Ok(renderer) => {
                println!("{} - {}", openpaint_core::hello(), openpaint_core::VERSION);
                println!("input backend: {}", self.input.name());
                self.ui = Some(ui::Ui::new(
                    renderer.device(),
                    renderer.surface_format(),
                    &window,
                ));
                // A recovery copy that outlived its process means that process died with unsaved
                // work in it. Offered rather than loaded: the artist may well have moved on, and
                // silently replacing whatever they just opened would be worse than losing it.
                self.recovery = self.autosave.find_recoverable();
                if let Some(found) = self.recovery.as_ref() {
                    println!("recovery available: {}", found.path.display());
                }

                // Ask for the first frame explicitly. Redraws are demand-driven
                // (strokes, resizes, and UI activity request them), so nothing
                // else would paint the initial canvas.
                renderer.window().request_redraw();
                self.renderer = Some(renderer);
                // The window exists now, so the title can finally say which document is open.
                self.update_title();
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

    /// Clean exit: drop the recovery copy, so the next launch does not offer work that was never
    /// actually lost.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.autosave.discard();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // The safe point for a modal dialog, for the same reason it is the safe point for
        // draining input: winit only calls this from the top of its own loop, so no foreign
        // frame -- and no lock of ours -- is on the stack while the dialog pumps messages.
        if self.pending_dialog.is_some() && !self.in_dispatch {
            self.in_dispatch = true;
            self.service_dialog(event_loop);
            self.in_dispatch = false;
        }
        self.poll_file_dialog();

        // ---------------------------------------------------------------------------------------
        // NOTHING ABOVE THIS LINE MAY `return`, AND NOTHING BELOW IT MAY EITHER UNTIL THE INPUT
        // BACKEND HAS BEEN DRAINED.
        //
        // This is not style. Draining is the *only* route by which pen movement and, critically,
        // pen *release* reach the app, and `about_to_wait` is the only place it is safe to do
        // (see the module note). An early return here does not merely skip a frame: it strands
        // `Editor::drawing` set forever, because the `Up` that would clear it is sitting in the
        // backend's queue. Painting then stops permanently -- one dab on press and nothing ever
        // again, including for every later stroke.
        //
        // That is not hypothetical. A "keep ticking while a stroke converges" branch was added
        // above the drain, with a `return`, and did exactly this. Structure the function so the
        // mistake is not available: drain first, unconditionally, then decide about waking.
        // ---------------------------------------------------------------------------------------
        if !self.in_dispatch {
            self.in_dispatch = true;
            self.drain_input();
            self.in_dispatch = false;
        }

        // A stabilized stroke keeps moving after its last sample -- the line is still travelling
        // toward the pen, and that takes real time. So a stroke in flight needs the loop to keep
        // waking whatever the input backend wants, or the line stops wherever it had reached.
        // Demand-driven painting is intact: there genuinely is demand.
        let converging = self.tick_stabilizer();

        // Deliberately after the drain, like everything else here: it is safe work, but there is
        // no reason for it to sit above an obligation. It also reads tiles back off the GPU, so
        // `about_to_wait` -- with no foreign frame on the stack -- is where it belongs.
        self.maybe_autosave();

        // A file dialog answers on another thread, so nothing else would wake the loop to notice.
        let awaiting_dialog = self.file_dialog.is_some();

        if !converging && !awaiting_dialog && !self.input.wants_continuous_poll() {
            // Event-driven backends (mouse) with nothing in flight stay idle until a real window
            // event, keeping the app at 0% CPU when nothing is happening.
            return;
        }

        // Keep waking up. Note we deliberately do NOT request a redraw here:
        // doing so would leave a `WM_PAINT` permanently pending, which is
        // precisely what a nested pump would dispatch back into us. Painting is
        // demand-driven from strokes and resizes instead.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Hovering must not turn into an unbounded repaint loop.
    ///
    /// The brush ring made pointer movement a reason to redraw, which it had never been before,
    /// and a pen reports poses continuously — including while lying perfectly still on the
    /// tablet. Without a threshold, a pen left on the surface would drive a full canvas composite
    /// at pen report rate forever, which is exactly the kind of idle cost the demand-driven paint
    /// loop exists to avoid. On the integrated graphics of the §2 target that is not a rounding
    /// error.
    #[test]
    fn a_resting_pointer_does_not_ask_for_frames() {
        let mut app = OpenPaint::default();

        // The first sighting is a change: there was no ring on screen before it.
        assert!(app.note_pointer(100.0, 100.0));

        for _ in 0..64 {
            assert!(
                !app.note_pointer(100.0, 100.0),
                "an unchanged pose must be free"
            );
        }
        assert!(
            !app.note_pointer(100.3, 99.8),
            "jitter too small to move a pixel is not worth a frame"
        );
        assert!(app.note_pointer(101.0, 100.0), "real movement is");
    }

    /// A slow drift must still reach the threshold.
    ///
    /// Guards the reason `note_pointer` does not store sub-threshold positions. If it did, every
    /// step would reset the comparison and a pointer creeping along in third-of-a-pixel steps
    /// could cross the whole window without ever asking for a frame — leaving the ring parked at
    /// the start of the drift.
    #[test]
    fn a_slow_drift_is_not_invisible() {
        let mut app = OpenPaint::default();
        assert!(app.note_pointer(0.0, 0.0));

        let mut x = 0.0;
        let mut asked = 0;
        for _ in 0..30 {
            x += 0.3;
            if app.note_pointer(x, 0.0) {
                asked += 1;
            }
        }
        // 9 px of travel in 0.3 px steps: roughly one frame per half pixel crossed, and
        // emphatically not zero.
        assert!(
            asked >= 8,
            "9 px of drift should have asked for several frames, got {asked}"
        );
    }

    /// The pointer leaving the window takes the ring with it.
    #[test]
    fn losing_the_pointer_forgets_where_it_was() {
        let mut app = OpenPaint::default();
        app.note_pointer(10.0, 20.0);
        assert!(app.brush_cursor().is_some());

        app.nav.cursor = None;
        assert!(
            app.brush_cursor().is_none(),
            "a ring frozen at the window edge claims the brush is somewhere it is not"
        );
    }

    /// The crop tool owns the pointer while it is up, so no brush ring follows it.
    #[test]
    fn cropping_shows_no_brush_ring() {
        let mut app = OpenPaint::default();
        app.note_pointer(10.0, 20.0);
        app.crop = Some(crop::Crop::new(app.editor.page_rect()));
        assert!(app.brush_cursor().is_none());
    }

    /// Draw a straight rightward stroke at 1 px/ms and report the dabs it produced.
    ///
    /// One pixel per millisecond makes the numbers legible: a lag of *n* ms is a shortfall of
    /// *n* px, so the stabilizer's advertised cost can be read straight off the geometry.
    /// `finish` chooses whether the pen-lift convergence runs, which is what isolates the lag
    /// from the correction for it.
    #[cfg(test)]
    fn drag_right(lag_ms: f32, length: u32, finish: bool) -> Vec<openpaint_core::Dab> {
        let mut app = OpenPaint::default();
        app.editor.brush_mut().stabilization_ms = lag_ms;

        app.stroke_start(0.0, 100.0, 1.0, 0.0);
        for i in 1..=length {
            app.stroke_continue(i as f32, 100.0, 1.0, f64::from(i));
        }
        if finish {
            app.stroke_finish();
        }

        let (_, dabs) = app.editor.pending_stroke();
        dabs.to_vec()
    }

    /// Where the drawn line had reached, in page pixels.
    #[cfg(test)]
    fn reached(dabs: &[openpaint_core::Dab]) -> f32 {
        dabs.last().expect("the drag must produce dabs").x
    }

    /// With smoothing off, the line has to be exactly where the pen was.
    #[test]
    fn an_unstabilized_stroke_follows_the_pen_exactly() {
        let dabs = drag_right(0.0, 300, true);
        for dab in &dabs {
            assert!(
                (dab.y - 100.0).abs() < 1e-3,
                "smoothing is off, so nothing should have moved off the line: y = {}",
                dab.y
            );
        }
        // Within one dab spacing of the end: dabs land at spacing intervals, so the last one sits
        // slightly short by construction.
        assert!(
            reached(&dabs) > 295.0,
            "the line stopped at {} instead of near 300",
            reached(&dabs)
        );
    }

    /// The lag half of the feature, through the real code path.
    ///
    /// At full strength the time constant is 50 ms, and this drag runs at 1 px/ms, so the drawn
    /// line should sit ~50 px behind the pen when it lifts. Measured with the convergence tail
    /// suppressed, so it is the filter being observed rather than the correction for it.
    #[test]
    fn a_stabilized_stroke_trails_the_pen_while_drawing() {
        let smoothed = reached(&drag_right(50.0, 300, false));
        let raw = reached(&drag_right(0.0, 300, false));

        assert!(
            raw - smoothed > 40.0 && raw - smoothed < 60.0,
            "expected ~50 px of trail at 50 ms and 1 px/ms, got {} (raw reached {raw}, \
             smoothed {smoothed})",
            raw - smoothed
        );
    }

    /// The correction half, and the one that breaks silently.
    ///
    /// A one-pole filter never catches up, so without the pen-lift convergence every stabilized
    /// stroke would stop ~50 px short at this strength — which reads as the app losing the end of
    /// every line, taper and all.
    #[test]
    fn a_stabilized_stroke_still_ends_where_the_pen_did() {
        let finished = reached(&drag_right(50.0, 300, true));
        let unfinished = reached(&drag_right(50.0, 300, false));

        assert!(
            finished > 295.0,
            "the stroke ended at {finished} instead of near 300"
        );
        assert!(
            finished - unfinished > 40.0,
            "the convergence tail moved the end by only {}, so it is not doing its job",
            finished - unfinished
        );
    }

    /// The freeze bug, at the seam where it actually lived.
    ///
    /// The filter itself could always catch up on time alone; nothing ever asked it to. The event
    /// loop advanced a stroke only when a sample arrived, so stopping the pen stopped the line —
    /// short of the cursor, until the next twitch snapped it forward in a straight segment.
    ///
    /// Uses the real clock and real sleeps, because `tick_stabilizer` reads the clock itself, and a
    /// test that injected time would not have caught this: the wiring was the broken part.
    #[test]
    fn a_held_stroke_keeps_catching_up_without_new_samples() {
        let mut app = OpenPaint::default();
        app.editor.brush_mut().stabilization_ms = 100.0;

        // One fast flick, as a quick drag looks to the filter, then the pen stops dead.
        let t0 = input::now_ms();
        app.stroke_start(0.0, 100.0, 1.0, t0);
        app.stroke_continue(600.0, 100.0, 1.0, t0 + 8.0);
        let before = app.editor.pending_stroke().1.len();

        // Nothing but time passing from here. No samples, no events.
        for _ in 0..25 {
            std::thread::sleep(std::time::Duration::from_millis(8));
            assert!(
                app.tick_stabilizer(),
                "a stroke in progress must keep the loop awake, or the ticks stop arriving"
            );
        }
        let after = app.editor.pending_stroke().1.len();

        assert!(
            after > before + 50,
            "the line went from {before} dabs to {after} while the pen was held still; it \
             should have kept travelling toward the cursor"
        );
    }

    /// A lasso gesture resolves into a mask, and a tap does not.
    ///
    /// Exercises the gesture seam rather than the rasterizer: whether points reach the polygon at
    /// all, and whether a release with nothing enclosed reads as "deselect" rather than leaving a
    /// degenerate selection behind.
    #[test]
    fn a_lasso_gesture_becomes_a_selection() {
        let mut app = OpenPaint::default();
        app.apply_select_action(ui::SelectAction::Use(ui::SelectTool::Lasso));
        assert!(app.select.is_some(), "the tool should be armed");

        // Points are in *page* space here; `select_press`/`select_drag` map from screen, which
        // needs a renderer, so drive the gesture directly.
        if let Some(Select::Lasso { points }) = app.select.as_mut() {
            points.extend([
                (100.0, 100.0),
                (300.0, 100.0),
                (300.0, 300.0),
                (100.0, 300.0),
            ]);
        }
        app.select_release();

        let selection = app.selection.as_ref().expect("the lasso enclosed an area");
        assert_eq!(
            selection.mask.coverage_at(200, 200),
            255,
            "inside the lasso"
        );
        assert_eq!(selection.mask.coverage_at(50, 50), 0, "outside it");
        assert!(
            !selection.outline.is_empty(),
            "a selection with no outline cannot be seen"
        );
    }

    /// A tap deselects, which is what a click on empty space means in every art app.
    #[test]
    fn a_tap_clears_the_selection() {
        let mut app = OpenPaint::default();
        app.apply_select_action(ui::SelectAction::All);
        assert!(app.selection.is_some());

        app.apply_select_action(ui::SelectAction::Use(ui::SelectTool::Lasso));
        if let Some(Select::Lasso { points }) = app.select.as_mut() {
            points.push((10.0, 10.0));
        }
        app.select_release();
        assert!(
            app.selection.is_none(),
            "a gesture that enclosed nothing left a selection behind"
        );
    }

    /// The gesture resets between uses, or a second lasso would extend the first.
    #[test]
    fn each_gesture_starts_fresh() {
        let mut app = OpenPaint::default();
        app.apply_select_action(ui::SelectAction::Use(ui::SelectTool::Lasso));
        if let Some(Select::Lasso { points }) = app.select.as_mut() {
            points.extend([(100.0, 100.0), (200.0, 100.0), (200.0, 200.0)]);
        }
        app.select_release();

        match app.select.as_ref() {
            Some(Select::Lasso { points }) => assert!(
                points.is_empty(),
                "the previous lasso's points survived into the next gesture"
            ),
            other => panic!("the tool should still be armed, got {other:?}"),
        }
    }

    /// Invert with nothing selected selects everything, rather than refusing.
    #[test]
    fn inverting_nothing_selects_everything() {
        let mut app = OpenPaint::default();
        app.apply_select_action(ui::SelectAction::Invert);
        let selection = app.selection.as_ref().expect("invert produced nothing");
        assert_eq!(selection.mask.coverage_at(0, 0), 255);
    }

    /// Selecting a tool twice puts it away, and the crop tool cannot be up at the same time —
    /// both consume input entirely, so two at once means one of them silently loses.
    #[test]
    fn selection_and_crop_do_not_overlap() {
        let mut app = OpenPaint::default();
        app.crop = Some(crop::Crop::new(app.editor.page_rect()));

        app.apply_select_action(ui::SelectAction::Use(ui::SelectTool::Rect));
        assert!(
            app.crop.is_none(),
            "arming a selection left the crop tool up"
        );

        app.apply_select_action(ui::SelectAction::Use(ui::SelectTool::Rect));
        assert!(app.select.is_none(), "the tool did not toggle off");
    }

    /// The ring reports the size of the mark *on screen*, so it has to track zoom.
    ///
    /// Its whole purpose is answering "how big will this be here", and a ring fixed in page
    /// pixels would answer a question nobody asked.
    #[test]
    fn the_ring_tracks_zoom() {
        let mut app = OpenPaint::default();
        app.note_pointer(10.0, 20.0);
        app.editor.brush_mut().radius = 20.0;

        app.view.fit(1000, 1000, app.editor.page_rect());
        let fitted = app
            .brush_cursor()
            .expect("the pointer is over the canvas")
            .radius;

        app.view
            .set_scale_about(app.view.scale() * 2.0, (500.0, 500.0), 1000, 1000);
        let zoomed = app
            .brush_cursor()
            .expect("the pointer is over the canvas")
            .radius;

        assert!(
            (zoomed - fitted * 2.0).abs() < 0.01,
            "doubling the zoom should double the ring: {fitted} -> {zoomed}"
        );
    }
}
