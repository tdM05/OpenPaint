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
                {
                    return;
                }
                // The crop tool consumes input entirely: no painting while it is up.
                if self.crop.is_some() {
                    self.crop_press(sample);
                    return;
                }
                if let Some((cx, cy)) = self.to_canvas(sample) {
                    self.editor.stroke_begin(cx, cy, sample.pressure);
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
                if !self.editor.is_drawing() || self.nav.is_active() {
                    return;
                }
                for sample in samples {
                    if let Some((cx, cy)) = self.to_canvas(sample) {
                        self.editor.stroke_to(cx, cy, sample.pressure);
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
                // Queues the bake that commits the stroke; demand-driven painting
                // means it needs a frame requested or it simply never happens.
                self.editor.stroke_end();
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

    /// Mark this sample as the one whose latency the next presented frame will measure.
    fn note_latency_input(&mut self, sample: &PenSample) {
        self.pending_sample_ms = Some(sample.time_ms);
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
        match renderer.save_document(document, &path) {
            Ok(tiles) => {
                self.dirty = false;
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
        self.dirty = false;
        self.update_title();
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
        self.dirty = false;
        self.update_title();
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
    /// new/open/save/save-as; Ctrl+E exports a PNG -- the bindings every art app shares. Hardcoded for now, like navigation
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
        let perf = self.perf.snapshot();

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

        // A file dialog answers on another thread, so nothing else would wake this loop to
        // notice. Keep checking while one is open, whatever the input backend wants.
        if self.file_dialog.is_some() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL_INTERVAL));
            return;
        }

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
