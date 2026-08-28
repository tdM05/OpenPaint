//! Editing state: the document being painted, the brush, and stroke lifecycle.
//!
//! This is the app's *model*. It knows nothing about wgpu, winit, or egui, and it
//! works entirely in **canvas coordinates** — mapping input from screen space is
//! the [`crate::view::View`]'s job, and drawing is the renderer's.
//!
//! Previously all of this lived inside the GPU context, which meant the renderer
//! was also the document owner and the stroke state machine. Separating it is
//! what allows pan/zoom (view state), pages (document state), and layers/undo to
//! land without piling onto one type.
//!
//! It stays in the app crate rather than `openpaint-core` because it is a
//! *session* concern — which document is open, which tool is active, what stroke
//! is in progress. The reusable engine pieces it orchestrates are all in core.

use openpaint_core::{Brush, Canvas, Dab, StrokePainter, StrokeState};

/// Fixed test-bed canvas size for the Phase 0 slice. The real document/page
/// model (growable and multi-page) arrives in Phase 2; see OPEN_QUESTIONS Q13.
const CANVAS_W: u32 = 2048;
const CANVAS_H: u32 = 2048;

pub struct Editor {
    canvas: Canvas,
    brush: Brush,
    /// Dab spacing continuity across input samples, for the stroke in progress.
    stroke: StrokeState,
    /// Dabs emitted by the brush this update, reused to avoid per-update
    /// allocation.
    dabs: Vec<Dab>,
    /// Per-stroke accumulation + pre-stroke tile snapshot, which is what makes
    /// flow build toward the opacity ceiling instead of darkening on every
    /// overlap. See `openpaint_core::stroke`.
    painter: StrokePainter,
    drawing: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            canvas: Canvas::new(CANVAS_W, CANVAS_H),
            brush: Brush::default(),
            stroke: StrokeState::new(),
            dabs: Vec::new(),
            painter: StrokePainter::new(),
            drawing: false,
        }
    }

    #[must_use]
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// Mutable canvas access, for the renderer to drain dirty tiles.
    pub fn canvas_mut(&mut self) -> &mut Canvas {
        &mut self.canvas
    }

    /// Mutable brush access, for the UI to edit settings.
    pub fn brush_mut(&mut self) -> &mut Brush {
        &mut self.brush
    }

    /// Whether a stroke is currently in progress.
    #[must_use]
    pub fn is_drawing(&self) -> bool {
        self.drawing
    }

    /// Begin a stroke at a canvas-space position.
    pub fn stroke_begin(&mut self, cx: f32, cy: f32, pressure: f32) {
        // A fresh stroke resets accumulation, so its opacity ceiling is
        // independent of the previous stroke's.
        self.painter.begin();
        self.brush
            .stroke_begin(&mut self.dabs, &mut self.stroke, cx, cy, pressure);
        self.drawing = true;
        self.flush_dabs();
    }

    /// Continue the current stroke to a new canvas-space position.
    pub fn stroke_to(&mut self, cx: f32, cy: f32, pressure: f32) {
        if !self.drawing {
            return;
        }
        self.brush
            .stroke_to(&mut self.dabs, &mut self.stroke, cx, cy, pressure);
        self.flush_dabs();
    }

    /// End the current stroke.
    pub fn stroke_end(&mut self) {
        self.drawing = false;
        self.stroke = StrokeState::new();
    }

    /// Accumulate whatever the brush emitted and re-composite the affected tiles.
    ///
    /// This is the per-dab / per-pixel seam (see `openpaint_core::dab`): the brush
    /// produced dabs without touching a pixel, and paint lands here. When dab
    /// rasterization moves to the GPU, this is the only call site that changes.
    fn flush_dabs(&mut self) {
        self.painter.add_dabs(&self.canvas, &self.dabs);
        self.dabs.clear();
        self.painter.composite(
            &mut self.canvas,
            self.brush.color_linear_premul(),
            self.brush.opacity,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_editor_is_idle_with_an_empty_canvas() {
        let e = Editor::new();
        assert!(!e.is_drawing());
        assert_eq!(e.canvas().tiles().count(), 0);
    }

    #[test]
    fn beginning_a_stroke_paints_and_sets_drawing() {
        let mut e = Editor::new();
        e.stroke_begin(100.0, 100.0, 1.0);
        assert!(e.is_drawing());
        assert_eq!(e.canvas().tiles().count(), 1);
    }

    /// `stroke_to` before `stroke_begin` must not paint. Otherwise a stray move
    /// event (a hover, or the tail of a previous stroke) would draw.
    #[test]
    fn stroke_to_without_begin_paints_nothing() {
        let mut e = Editor::new();
        e.stroke_to(100.0, 100.0, 1.0);
        assert!(!e.is_drawing());
        assert_eq!(e.canvas().tiles().count(), 0);
    }

    #[test]
    fn ending_a_stroke_clears_drawing_and_stops_painting() {
        let mut e = Editor::new();
        e.stroke_begin(100.0, 100.0, 1.0);
        e.stroke_end();
        assert!(!e.is_drawing());

        let before = e.canvas().tiles().count();
        e.stroke_to(900.0, 900.0, 1.0);
        assert_eq!(
            e.canvas().tiles().count(),
            before,
            "painted after stroke end"
        );
    }

    /// Each stroke gets its own opacity ceiling, so a second pass over the same
    /// area must go darker. This is the editor-level check on the behavior
    /// `openpaint_core::stroke` implements.
    ///
    /// Note the stroke has to actually *travel*: dabs are emitted by distance, so
    /// repeatedly calling `stroke_to` at one point emits nothing at all. Spacing is
    /// tightened here so many dabs overlap the sampled pixel.
    #[test]
    fn a_second_stroke_deepens_the_same_spot() {
        const CEILING: f32 = 0.5;

        fn configured() -> Editor {
            let mut e = Editor::new();
            let b = e.brush_mut();
            b.flow = 0.1;
            b.opacity = CEILING;
            b.hardness = 1.0; // hard edge, so coverage at the sample is exactly 1
            b.spacing = 0.05; // dense dabs, so they pile up on one pixel
            e
        }

        /// Paint deposited at (120, 100), derived from the color channel: the
        /// canvas is opaque paper, so its alpha says nothing.
        fn paint_at(e: &Editor) -> f32 {
            let paper = Canvas::paper_color()[0];
            1.0 - e.canvas().tile((0, 0)).expect("tile").texel(120, 100)[0] / paper
        }

        fn sweep(e: &mut Editor) {
            e.stroke_begin(100.5, 100.5, 1.0);
            e.stroke_to(140.5, 100.5, 1.0);
            e.stroke_end();
        }

        let mut e = configured();
        sweep(&mut e);
        let after_first = paint_at(&e);
        sweep(&mut e);
        let after_second = paint_at(&e);

        assert!(
            after_first > 0.05,
            "stroke deposited almost nothing ({after_first}); did any dabs land?"
        );
        assert!(
            after_first <= CEILING + 1e-3,
            "first stroke passed its ceiling: {after_first}"
        );
        assert!(
            after_second > after_first + 0.05,
            "second stroke did not deepen: {after_first} -> {after_second}"
        );
    }

    #[test]
    fn painting_off_canvas_is_harmless() {
        let mut e = Editor::new();
        e.stroke_begin(-500.0, -500.0, 1.0);
        assert_eq!(e.canvas().tiles().count(), 0);
    }
}
