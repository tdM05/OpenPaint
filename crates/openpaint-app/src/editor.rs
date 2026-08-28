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

use openpaint_core::{Brush, Canvas, Dab, Document, Mode, Page, PageRect, StrokeState};

use crate::history::{BoundsBuilder, CanvasRect};

/// Starting page size. A placeholder until New-document presets exist (§5a says
/// "300 DPI A4" and friends are presets computing pixel dimensions).
const PAGE_W: u32 = 2048;
const PAGE_H: u32 = 2048;

/// Default amount an Extend adds, in pixels.
///
/// A default, not a rule: §5a requires this be user-configurable, and drag-to-extend
/// later feeds the same call with whatever the drag produced.
pub const DEFAULT_EXTEND: u32 = 512;

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
    ///
    /// Carries the region the stroke touched so history can snapshot just that
    /// rectangle rather than the whole canvas. `None` when the stroke landed
    /// entirely off-canvas and there is nothing to record.
    End { bounds: Option<CanvasRect> },
}

/// Interim cap on total canvas pixels, while the canvas is a single texture.
///
/// The dimension limit alone is not enough protection: 8192x8192 at `Rgba16Float` is
/// **512 MiB** for the canvas plus 128 MiB for the stroke accumulation buffer, which
/// a Surface sharing system memory will simply fail to allocate — and a failed
/// allocation is a device error, i.e. another crash.
///
/// 16 Mpx keeps the canvas at ~128 MiB and the accumulation buffer at ~32 MiB, which
/// is defensible on integrated graphics. It permits e.g. 2048x8192 or 4096x4096.
/// The tiled resident cache (OPEN_QUESTIONS Q13) removes this entirely, because only
/// the visible working set is ever resident.
pub const MAX_CANVAS_PIXELS: u64 = 16 * 1024 * 1024;

/// Whether a page of this size fits the interim single-texture budget.
#[must_use]
pub fn fits_pixel_budget(w: u32, h: u32) -> bool {
    u64::from(w) * u64::from(h) <= MAX_CANVAS_PIXELS
}

/// Clamp a requested page size to what the GPU can actually allocate.
///
/// Returns `None` when no growth is possible at all, so the caller can say so
/// instead of attempting a resize that changes nothing.
///
/// This exists because the canvas is currently **one texture**, so the page can
/// never exceed `max_dimension` — 8192 with the default wgpu limits we deliberately
/// stay within (DECISIONS §2). Exceeding it used to panic inside
/// `Device::create_texture`, which is not an acceptable outcome for pressing a
/// button. The real answer is the tiled resident cache (OPEN_QUESTIONS Q13), which
/// removes the ceiling entirely; until then this is the honest boundary.
#[must_use]
pub fn clamp_page_size(
    current: (u32, u32),
    requested: (u32, u32),
    max_dimension: u32,
) -> Option<(u32, u32)> {
    let w = requested.0.min(max_dimension).max(1);
    let h = requested.1.min(max_dimension).max(1);
    if (w, h) == current {
        return None;
    }
    Some((w, h))
}

pub struct Editor {
    document: Document,
    brush: Brush,
    /// Dab spacing continuity across input samples, for the stroke in progress.
    stroke: StrokeState,
    /// Dabs emitted since the renderer last consumed them.
    dabs: Vec<Dab>,
    /// Stroke commands awaiting execution, indexing into `dabs`.
    ops: Vec<StrokeOp>,
    /// Area the in-progress stroke has touched, for history's snapshot.
    bounds: BoundsBuilder,
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
            document: Document::new(Page::new(PAGE_W, PAGE_H), Mode::Pages),
            brush: Brush::default(),
            stroke: StrokeState::new(),
            dabs: Vec::new(),
            ops: Vec::new(),
            bounds: BoundsBuilder::default(),
            drawing: false,
        }
    }

    /// The canvas of the page being edited.
    #[must_use]
    pub fn canvas(&self) -> &Canvas {
        self.document.active().canvas()
    }

    /// Mutable canvas access, for the renderer to drain dirty tiles.
    pub fn canvas_mut(&mut self) -> &mut Canvas {
        self.document.active_mut().canvas_mut()
    }

    #[must_use]
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Resize the page being edited, returning how far existing content moved.
    ///
    /// Callers must act on that offset: GPU textures copy their contents to it, and
    /// anything storing page coordinates (undo rectangles *and* the dab positions
    /// kept for redo) must be shifted by the same amount.
    pub fn resize_page(&mut self, rect: PageRect) -> (i32, i32) {
        // A resize during a stroke would leave the in-progress accumulation keyed to
        // stale coordinates, so end it first.
        self.stroke_end();
        self.document.active_mut().resize(rect)
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
        self.bounds.clear();
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
            let bounds = self.bounds.to_rect(self.canvas());
            self.ops.push(StrokeOp::End { bounds });
        }
        self.drawing = false;
        self.bounds.clear();
        self.stroke = StrokeState::new();
    }

    /// Record that dabs from `from` onward were just emitted.
    fn record_dabs(&mut self, from: usize) {
        let len = self.dabs.len() - from;
        if len == 0 {
            return;
        }
        for d in &self.dabs[from..] {
            self.bounds.add_dab(d);
        }
        self.ops.push(StrokeOp::Dabs { start: from, len });
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
        assert!(matches!(ops.last(), Some(StrokeOp::End { .. })));
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
    /// History snapshots only the region a stroke touched, so `End` must carry it
    /// and it must actually cover the dabs.
    #[test]
    fn end_reports_the_region_the_stroke_touched() {
        let mut e = Editor::new();
        e.brush_mut().radius = 10.0;
        e.stroke_begin(200.0, 300.0, 1.0);
        e.stroke_to(400.0, 300.0, 1.0);
        e.stroke_end();

        let (ops, _) = e.pending_stroke();
        match ops.last() {
            Some(StrokeOp::End { bounds: Some(r) }) => {
                // Page coordinates are signed now, so widen deliberately rather than
                // mixing i32 and u32 at the comparison.
                let right = r.x + r.w as i32;
                let bottom = r.y + r.h as i32;
                assert!(r.x <= 189, "left edge {} too far right", r.x);
                assert!(right >= 411, "right edge {right} too far left");
                assert!(r.y <= 289 && bottom >= 311, "vertical bounds {r:?}");
            }
            other => panic!("expected End with bounds, got {other:?}"),
        }
    }

    /// A stroke entirely off-canvas has nothing to snapshot, and must say so rather
    /// than producing a rectangle the GPU copy would reject.
    #[test]
    fn a_fully_off_canvas_stroke_reports_no_region() {
        let mut e = Editor::new();
        e.stroke_begin(-900.0, -900.0, 1.0);
        e.stroke_end();
        let (ops, _) = e.pending_stroke();
        assert!(matches!(ops.last(), Some(StrokeOp::End { bounds: None })));
    }

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
        let ends = ops
            .iter()
            .filter(|o| matches!(o, StrokeOp::End { .. }))
            .count();
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

    #[test]
    fn a_normal_extend_is_not_clamped() {
        assert_eq!(
            clamp_page_size((2048, 2048), (2048, 2560), 8192),
            Some((2048, 2560))
        );
    }

    /// The case that used to panic inside wgpu: a request past the texture limit.
    #[test]
    fn extending_past_the_limit_clamps_to_it() {
        assert_eq!(
            clamp_page_size((2048, 8000), (2048, 9216), 8192),
            Some((2048, 8192))
        );
    }

    /// Already at the ceiling: there is nothing to do, and the caller needs to know
    /// rather than resize to the same size and report success.
    #[test]
    fn no_growth_available_reports_none() {
        assert_eq!(clamp_page_size((2048, 8192), (2048, 9216), 8192), None);
    }

    #[test]
    fn width_and_height_clamp_independently() {
        assert_eq!(
            clamp_page_size((8192, 1000), (9000, 2000), 8192),
            Some((8192, 2000))
        );
    }

    #[test]
    fn a_zero_request_is_clamped_to_something_drawable() {
        assert_eq!(clamp_page_size((100, 100), (0, 0), 8192), Some((1, 1)));
    }

    /// The dimension limit alone would still permit 8192x8192, which is 512 MiB and
    /// would fail to allocate on the target hardware -- another crash, just a
    /// different one.
    #[test]
    fn the_pixel_budget_rejects_sizes_the_dimension_limit_allows() {
        assert!(!fits_pixel_budget(8192, 8192));
        assert!(fits_pixel_budget(2048, 2048), "the default page must fit");
        assert!(
            fits_pixel_budget(2048, 8192),
            "a tall webtoon-ish page should fit"
        );
        assert!(fits_pixel_budget(4096, 4096));
        assert!(!fits_pixel_budget(4096, 8192));
    }

    /// A4 at 300 DPI has to fit, or the print workflow is impossible.
    #[test]
    fn a4_at_300_dpi_fits_the_budget() {
        assert!(fits_pixel_budget(2480, 3508));
    }
}
