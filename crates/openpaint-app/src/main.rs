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
mod export;
mod history;
mod input;
mod input_mouse;
#[cfg(target_os = "windows")]
mod input_pen;
mod renderer;
mod stroke_layer;
#[cfg(test)]
mod test_gpu;
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
use openpaint_core::Anchor;
use renderer::Renderer;
use view::{View, ROTATE_STEP};

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
    /// Most recent notable outcome (export, resize refusal, history loss), shown in
    /// the panel so it isn't only visible in a console the user may not be watching.
    status_message: Option<String>,
    /// Set while we're inside one of our own event handlers, so a nested
    /// message pump can't re-enter our GPU/input work. See the module-level
    /// "Windows Ink reentrancy" note - without this, a re-entered frame can
    /// deadlock on a lock its own interrupted outer call is holding.
    in_dispatch: bool,
}

/// Which edge an Extend adds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtendDir {
    Down,
    Up,
    Left,
    Right,
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
            status_message: None,
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
                if self.ui_blocks_point(sample) || self.nav.is_active() {
                    return;
                }
                if let Some((cx, cy)) = self.to_canvas(sample) {
                    self.editor.stroke_begin(cx, cy, sample.pressure);
                    self.request_redraw();
                }
            }
            PenEvent::Move(samples) => {
                if !self.editor.is_drawing() || self.nav.is_active() {
                    return;
                }
                for sample in samples {
                    if let Some((cx, cy)) = self.to_canvas(sample) {
                        self.editor.stroke_to(cx, cy, sample.pressure);
                    }
                }
                self.request_redraw();
            }
            PenEvent::Up => {
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

    /// Grow the current page in one direction and keep everything consistent.
    ///
    /// The single place that knows a resize has three consequences: the page's
    /// dimensions change, the GPU textures must be re-created with content copied to
    /// the new offset, and stored page coordinates (undo rectangles and the dab
    /// positions kept for redo) must be shifted to match. Missing the third would
    /// silently corrupt history, which is why it goes through one function.
    fn extend_page(&mut self, dir: ExtendDir, amount: u32) {
        if amount == 0 {
            return;
        }
        let (w, h) = {
            let page = self.editor.document().active();
            (page.width(), page.height())
        };
        let (wanted_w, wanted_h, anchor) = match dir {
            ExtendDir::Down => (
                w.saturating_add(0),
                h.saturating_add(amount),
                Anchor::TOP_LEFT,
            ),
            ExtendDir::Up => (w, h.saturating_add(amount), Anchor::BOTTOM_LEFT),
            ExtendDir::Right => (w.saturating_add(amount), h, Anchor::TOP_LEFT),
            ExtendDir::Left => (w.saturating_add(amount), h, Anchor::TOP_RIGHT),
        };

        // The canvas is one texture, so the page cannot exceed what the device will
        // allocate. Requesting more used to panic inside wgpu; clamp and say so.
        let max = self
            .renderer
            .as_ref()
            .map_or(8192, Renderer::max_canvas_dimension);
        let Some((new_w, new_h)) = editor::clamp_page_size((w, h), (wanted_w, wanted_h), max)
        else {
            self.status_message = Some(format!(
                "Page is at the {max} px limit -- a tiled canvas is needed to go further"
            ));
            self.request_redraw();
            return;
        };
        if (new_w, new_h) != (wanted_w, wanted_h) {
            self.status_message = Some(format!("Extend clamped to the {max} px texture limit"));
        }

        // A size within the dimension limit can still be far too much memory to
        // allocate, and a failed allocation is a device error -- another crash, just
        // a different one.
        if !editor::fits_pixel_budget(new_w, new_h) {
            let mpx = editor::MAX_CANVAS_PIXELS / (1024 * 1024);
            self.status_message = Some(format!(
                "{new_w}x{new_h} exceeds the interim {mpx} Mpx single-texture budget;                  a tiled canvas is needed to go further"
            ));
            self.request_redraw();
            return;
        }

        let (dx, dy) = self.editor.resize_page(new_w, new_h, anchor);
        let history_kept = match self.renderer.as_mut() {
            Some(r) => r.resize_canvas(new_w, new_h, dx, dy),
            None => true,
        };
        if !history_kept {
            self.status_message = Some("Undo history cleared by the resize".to_owned());
        }

        // Show the new extent, so the user can see what they just added.
        self.view.request_fit();
        self.request_redraw();
    }

    /// Handle undo/redo shortcuts. Returns `true` if the event was consumed.
    ///
    /// Ctrl+Z undoes, Ctrl+Shift+Z and Ctrl+Y redo -- the bindings every art app
    /// shares. Hardcoded for now, like navigation (OPEN_QUESTIONS Q16).
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
                if matches!(c.as_str(), "s" | "S") {
                    self.export_png();
                    return true;
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
                let changed = if redo {
                    renderer.redo()
                } else {
                    renderer.undo()
                };
                if changed {
                    self.request_redraw();
                }
                true
            }
            _ => false,
        }
    }

    /// Export the canvas to a PNG beside the executable's working directory.
    ///
    /// Ctrl+S is "export" rather than "save" for now, deliberately: there is no
    /// native document format yet, and inventing one before the page model exists
    /// would guarantee a migration (OPEN_QUESTIONS Q6).
    fn export_png(&mut self) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let path = export::default_path();
        self.status_message = Some(match renderer.export_png(&path) {
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
    ///   - `[` / `]` rotate about the cursor, Ctrl+0 also resets rotation
    fn handle_navigation(&mut self, event: &WindowEvent) -> bool {
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

        let Some((w, h)) = self.renderer.as_ref().map(Renderer::size_px) else {
            return false;
        };

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.nav.cursor = Some((position.x, position.y));
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
                        PhysicalKey::Code(KeyCode::BracketLeft) => Some(-ROTATE_STEP),
                        PhysicalKey::Code(KeyCode::BracketRight) => Some(ROTATE_STEP),
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
                    // Digits by logical key: stable across layouts.
                    Key::Character(c) if pressed => match c.as_str() {
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
                        _ => false,
                    },
                    _ => false,
                }
            }

            _ => false,
        }
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

        // Execute any queued stroke work before the frame reads the canvas.
        // Borrowed, not cloned: this runs every frame a stroke is active, so a
        // copy here would be an allocation per frame on the interactive path.
        // `renderer` and `editor` are disjoint fields, so both borrows coexist.
        if editor.has_pending_stroke() {
            let (ops, dabs) = editor.pending_stroke();
            renderer.apply_stroke(ops, dabs);
            editor.clear_pending_stroke();
        }

        renderer.upload_canvas(editor.canvas_mut());
        let (w, h) = renderer.size_px();
        // Fitting needs both the surface size and the UI inset, so it is deferred
        // to here rather than done at construction.
        self.view.apply_pending_fit(w, h, editor.canvas());
        let placement = self.view.placement(w, h, editor.canvas());

        let mut ui_wants_repaint = false;
        let mut ui_inset_left = None;
        let mut extend_request = None;
        let history_status = renderer.history_status();
        let status_message = self.status_message.clone();
        let page_size = {
            let page = editor.document().active();
            (page.width(), page.height())
        };
        let window = renderer.window().clone();
        // Borrowed, not copied: a copy would mean any future UI control that edits
        // the view silently writes to a dead value. Disjoint field borrows make
        // this fine alongside the mutable borrows of `renderer` and `editor`.
        let view = &self.view;
        let result = renderer.render(placement, |gpu| {
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
                    },
                );
                ui_wants_repaint = out.wants_repaint;
                extend_request = out.extend;
                ui_inset_left = Some(ui.inset_left_px());
            }
        });

        // Keep the canvas centered in the area the panel leaves free. The first
        // fit necessarily ran before the UI existed, so learning the inset queues
        // another one -- which needs a frame to actually apply.
        let refit_queued = ui_inset_left.is_some_and(|inset| self.view.set_inset_left(inset));
        if ui_wants_repaint || refit_queued {
            self.request_redraw();
        }

        // Applied after the frame, not inside the overlay closure: resizing
        // re-creates the very GPU resources the frame is drawing with.
        if let Some((dir, amount)) = extend_request {
            self.extend_page(dir, amount);
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
