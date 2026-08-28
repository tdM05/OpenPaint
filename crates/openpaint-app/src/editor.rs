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
//!
//! # Pixels live on the GPU
//!
//! Painting is no longer done here. The editor emits a small **stroke command
//! stream** ([`StrokeOp`]) which the renderer consumes at frame time and executes
//! on the GPU (`crate::stroke_layer`). Commands are buffered rather than executed
//! immediately because they need a command encoder, and a frame follows every
//! stroke update anyway — so batching costs nothing and avoids a submit per input
//! sample.
//!
//! Consequently the CPU [`Canvas`] no longer holds painted pixels; it carries the
//! document's dimensions, and its tile machinery is what the future tile cache and
//! readback will build on (OPEN_QUESTIONS Q13). `openpaint_core::stroke` and
//! `openpaint_core::raster` remain the CPU *reference* implementations, and
//! `tests/gpu_matches_cpu.rs` is what keeps the GPU honest against them.

use openpaint_core::{Brush, Canvas, Dab, StrokeState};

/// Fixed test-bed canvas size for the Phase 0 slice. The real document/page
/// model (growable and multi-page) arrives in Phase 2; see OPEN_QUESTIONS Q13.
const CANVAS_W: u32 = 2048;
const CANVAS_H: u32 = 2048;

/// One step of the stroke command stream the renderer executes on the GPU.
///
/// An ordered list rather than flags, because a single frame can legitimately
/// contain a whole stroke: a quick tap produces `Begin`, `Dabs`, `End` together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrokeOp {
    /// Start a stroke, clearing accumulation. Carries the paint to use, captured
    /// at stroke start so changing the colour mid-stroke can't produce a two-tone
    /// stroke.
    Begin {
        color_linear_premul: [f32; 4],
        opacity: f32,
    },
    /// Stamp dabs `[start, start + len)` of the accompanying dab buffer.
    Dabs { start: usize, len: usize },
    /// Commit the stroke into the canvas.
    End,
}

pub struct Editor {
    canvas: Canvas,
    brush: Brush,
    /// Dab spacing continuity across input samples, for the stroke in progress.
    stroke: StrokeState,
    /// Dabs emitted since the renderer last consumed them.
    dabs: Vec<Dab>,
    /// Stroke commands awaiting execution, indexing into `dabs`.
    ops: Vec<StrokeOp>,
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
            ops: Vec::new(),
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

    /// Stroke commands and their dabs, for the renderer to execute.
    #[must_use]
    pub fn pending_stroke(&self) -> (&[StrokeOp], &[Dab]) {
        (&self.ops, &self.dabs)
    }

    /// Drop the commands the renderer has just executed.
    pub fn clear_pending_stroke(&mut self) {
        self.ops.clear();
        self.dabs.clear();
    }

    /// Whether there is anything for the renderer to do.
    #[must_use]
    pub fn has_pending_stroke(&self) -> bool {
        !self.ops.is_empty()
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
        self.ops.push(StrokeOp::Begin {
            color_linear_premul: self.brush.color_linear_premul(),
            opacity: self.brush.opacity,
        });
        self.drawing = true;
        let from = self.dabs.len();
        self.brush
            .stroke_begin(&mut self.dabs, &mut self.stroke, cx, cy, pressure);
        self.record_dabs(from);
    }

    /// Continue the current stroke to a new canvas-space position.
    pub fn stroke_to(&mut self, cx: f32, cy: f32, pressure: f32) {
        if !self.drawing {
            return;
        }
        let from = self.dabs.len();
        self.brush
            .stroke_to(&mut self.dabs, &mut self.stroke, cx, cy, pressure);
        self.record_dabs(from);
    }

    /// End the current stroke, committing it to the canvas.
    pub fn stroke_end(&mut self) {
        if self.drawing {
            self.ops.push(StrokeOp::End);
        }
        self.drawing = false;
        self.stroke = StrokeState::new();
    }

    /// Record that dabs from `from` onward were just emitted.
    fn record_dabs(&mut self, from: usize) {
        let len = self.dabs.len() - from;
        if len > 0 {
            self.ops.push(StrokeOp::Dabs { start: from, len });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Count the dabs a stroke command stream refers to.
    fn dab_count(ops: &[StrokeOp]) -> usize {
        ops.iter()
            .map(|op| match op {
                StrokeOp::Dabs { len, .. } => *len,
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn a_new_editor_is_idle_with_nothing_queued() {
        let e = Editor::new();
        assert!(!e.is_drawing());
        assert!(!e.has_pending_stroke());
        assert_eq!(e.canvas().tiles().count(), 0);
    }

    #[test]
    fn beginning_a_stroke_queues_begin_and_dabs() {
        let mut e = Editor::new();
        e.stroke_begin(100.0, 100.0, 1.0);
        assert!(e.is_drawing());

        let (ops, dabs) = e.pending_stroke();
        assert!(matches!(ops.first(), Some(StrokeOp::Begin { .. })));
        assert_eq!(dab_count(ops), 1, "one dab for the initial press");
        assert_eq!(dabs.len(), 1);
        assert_eq!((dabs[0].x, dabs[0].y), (100.0, 100.0));
    }

    /// The captured paint must be the brush state at stroke *start*, so changing
    /// colour mid-stroke can't produce a two-tone stroke.
    #[test]
    fn begin_captures_the_paint_in_use() {
        let mut e = Editor::new();
        e.brush_mut().opacity = 0.25;
        e.brush_mut().set_color_srgb8([255, 0, 0]);
        let expected = e.brush_mut().color_linear_premul();

        e.stroke_begin(10.0, 10.0, 1.0);
        let (ops, _) = e.pending_stroke();
        match ops.first() {
            Some(StrokeOp::Begin {
                color_linear_premul,
                opacity,
            }) => {
                assert_eq!(*color_linear_premul, expected);
                assert!((*opacity - 0.25).abs() < 1e-6);
            }
            other => panic!("expected Begin, got {other:?}"),
        }
    }

    /// `stroke_to` before `stroke_begin` must queue nothing. Otherwise a stray move
    /// event (a hover, or the tail of a previous stroke) would paint.
    #[test]
    fn stroke_to_without_begin_queues_nothing() {
        let mut e = Editor::new();
        e.stroke_to(100.0, 100.0, 1.0);
        assert!(!e.is_drawing());
        assert!(!e.has_pending_stroke());
    }

    /// A moving stroke must emit dabs along the path, and the ops must index the
    /// dab buffer correctly -- an off-by-one here would stamp the wrong dabs.
    #[test]
    fn stroke_ops_index_the_dab_buffer_correctly() {
        let mut e = Editor::new();
        e.stroke_begin(0.0, 0.0, 1.0);
        e.stroke_to(100.0, 0.0, 1.0);
        e.stroke_to(200.0, 0.0, 1.0);
        e.stroke_end();

        let (ops, dabs) = e.pending_stroke();
        assert!(matches!(ops.first(), Some(StrokeOp::Begin { .. })));
        assert!(matches!(ops.last(), Some(StrokeOp::End)));
        assert_eq!(
            dab_count(ops),
            dabs.len(),
            "ops must cover every dab exactly"
        );

        // Ranges must be contiguous and in order, starting at zero.
        let mut next = 0;
        for op in ops {
            if let StrokeOp::Dabs { start, len } = op {
                assert_eq!(*start, next, "gap or overlap in dab ranges");
                next += len;
            }
        }
        assert_eq!(next, dabs.len());
    }

    #[test]
    fn ending_a_stroke_queues_end_and_stops_emitting() {
        let mut e = Editor::new();
        e.stroke_begin(100.0, 100.0, 1.0);
        e.stroke_end();
        assert!(!e.is_drawing());

        let before = e.pending_stroke().1.len();
        e.stroke_to(900.0, 900.0, 1.0);
        assert_eq!(
            e.pending_stroke().1.len(),
            before,
            "emitted dabs after the stroke ended"
        );
    }

    /// Ending without a stroke in progress must not queue a stray commit, which
    /// would bake whatever the previous stroke left behind a second time.
    #[test]
    fn ending_without_a_stroke_queues_nothing() {
        let mut e = Editor::new();
        e.stroke_end();
        assert!(!e.has_pending_stroke());
    }

    #[test]
    fn two_strokes_produce_two_begins() {
        let mut e = Editor::new();
        e.stroke_begin(10.0, 10.0, 1.0);
        e.stroke_end();
        e.stroke_begin(20.0, 20.0, 1.0);
        e.stroke_end();

        let (ops, _) = e.pending_stroke();
        let begins = ops
            .iter()
            .filter(|o| matches!(o, StrokeOp::Begin { .. }))
            .count();
        let ends = ops.iter().filter(|o| matches!(o, StrokeOp::End)).count();
        assert_eq!(begins, 2, "each stroke needs its own accumulation reset");
        assert_eq!(ends, 2);
    }

    #[test]
    fn clearing_pending_work_empties_both_buffers() {
        let mut e = Editor::new();
        e.stroke_begin(10.0, 10.0, 1.0);
        e.stroke_to(50.0, 10.0, 1.0);
        assert!(e.has_pending_stroke());

        e.clear_pending_stroke();
        assert!(!e.has_pending_stroke());
        assert!(e.pending_stroke().1.is_empty());
    }

    /// Clipping to the canvas is the view's job, not the editor's, so off-canvas
    /// coordinates are emitted rather than dropped -- the GPU viewport discards
    /// them. This just pins that it doesn't panic or misbehave.
    #[test]
    fn off_canvas_coordinates_are_emitted_not_dropped() {
        let mut e = Editor::new();
        e.stroke_begin(-500.0, -500.0, 1.0);
        assert_eq!(e.pending_stroke().1.len(), 1);
    }
}
